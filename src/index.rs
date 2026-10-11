//! The history, ready to be asked "what comes after this?" in microseconds.
//!
//! Every distinct command is kept once, in a list sorted by its text, beside
//! a list of when it was last run. The commands that start with what has been
//! typed are then one run of that list, found by two binary searches, and the
//! most recent of them is the largest number in a stretch of the other list.
//! Nothing is allocated or copied to ask.
//!
//! As fish does, the most recent command wins, but one that was run in the
//! directory the shell is in comes before all that were not. So there is a
//! second list of the same shape for the current directory, built once when
//! the directory changes.
//!
//! Commands come in by [`Index::push`] and become visible at
//! [`Index::commit`], which merges a batch in one pass: loading a hundred
//! thousand commands is a sort, not a hundred thousand insertions.

use std::cmp::Ordering;
use std::collections::HashMap;

use crate::text::is_typeable;

/// A command longer than this is not kept: it would not fit on a row anyway,
/// and a few huge pasted lines should not decide how much memory this uses.
const MAX_COMMAND_BYTES: usize = 2048;
/// The most commands run that are kept. The oldest are the ones left out.
const MAX_ROWS: usize = 2_000_000;
/// The most bytes of command text kept.
const MAX_ARENA: usize = 256 << 20;

/// Where a command's text lies in the arena.
#[derive(Clone, Copy, Debug)]
struct Span {
    start: u32,
    len: u32,
}

/// One command run, waiting to be committed.
#[derive(Debug)]
struct Row {
    text: Span,
    dir: u32,
    when: i64,
}

/// A command run in a directory, once committed.
#[derive(Clone, Copy, Debug)]
struct DirRow {
    id: u32,
    when: i64,
}

/// The view of the history for one directory.
#[derive(Debug, Default)]
struct Local {
    dir: Option<u32>,
    /// How many rows the directory had when this was built.
    built_from: usize,
    ids: Vec<u32>,
    when: Vec<i64>,
}

/// Why a command was not taken in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    /// Empty, or with a character in it that would be a key of its own.
    NotText,
    /// Starts with a blank: the person asked for it not to be remembered.
    Hidden,
    /// Too long.
    TooLong,
    /// The index is full.
    Full,
}

/// The history, searchable by prefix.
#[derive(Debug, Default)]
pub struct Index {
    /// The text of every distinct command, one after another.
    arena: String,
    spans: Vec<Span>,
    /// Command ids, ordered by the text.
    sorted: Vec<u32>,
    /// When each command in `sorted` was last run, in the same order.
    recent: Vec<i64>,
    pending: Vec<Row>,
    pending_arena: String,
    dir_ids: HashMap<String, u32>,
    by_dir: Vec<Vec<DirRow>>,
    local: Local,
    cwd: String,
    rows: usize,
}

impl Index {
    /// An empty index.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// How many distinct commands are searchable.
    #[must_use]
    pub fn len(&self) -> usize {
        self.sorted.len()
    }

    /// Whether nothing is searchable.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sorted.is_empty()
    }

    /// How many runs of commands are known, counting repeats.
    #[must_use]
    pub fn rows(&self) -> usize {
        self.rows
    }

    /// Forget everything.
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Take in a command that was run in `cwd` at time `when` (any unit, as
    /// long as later is larger). It is not searchable until [`Index::commit`].
    ///
    /// # Errors
    ///
    /// Says why the command was left out.
    pub fn push(&mut self, command: &str, cwd: &str, when: i64) -> Result<(), Refused> {
        if command.starts_with(char::is_whitespace) {
            return Err(Refused::Hidden);
        }
        if !is_typeable(command) {
            return Err(Refused::NotText);
        }
        if command.len() > MAX_COMMAND_BYTES {
            return Err(Refused::TooLong);
        }
        if self.rows >= MAX_ROWS
            || self.arena.len().saturating_add(self.pending_arena.len()) > MAX_ARENA
        {
            return Err(Refused::Full);
        }
        let start = u32::try_from(self.pending_arena.len()).map_err(|_| Refused::Full)?;
        let len = u32::try_from(command.len()).map_err(|_| Refused::TooLong)?;
        let dir = self.dir_id(cwd)?;
        self.pending_arena.push_str(command);
        self.pending.push(Row {
            text: Span { start, len },
            dir,
            when,
        });
        self.rows = self.rows.saturating_add(1);
        Ok(())
    }

    fn dir_id(&mut self, cwd: &str) -> Result<u32, Refused> {
        if let Some(&id) = self.dir_ids.get(cwd) {
            return Ok(id);
        }
        let id = u32::try_from(self.by_dir.len()).map_err(|_| Refused::Full)?;
        self.dir_ids.insert(cwd.to_owned(), id);
        self.by_dir.push(Vec::new());
        Ok(id)
    }

    fn text(&self, id: u32) -> &str {
        let span = usize::try_from(id)
            .ok()
            .and_then(|at| self.spans.get(at))
            .copied()
            .unwrap_or(Span { start: 0, len: 0 });
        slice(&self.arena, span)
    }

    /// Make everything pushed so far searchable.
    pub fn commit(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        let mut rows = std::mem::take(&mut self.pending);
        // The text of the pending rows is borrowed from here, apart from
        // `self`, which is changed as the rows are taken in.
        let arena = std::mem::take(&mut self.pending_arena);
        rows.sort_by(|a, b| slice(&arena, a.text).cmp(slice(&arena, b.text)));
        let mut fresh: Vec<(u32, i64)> = Vec::new();
        let mut at = 0;
        // Each pass takes one run of rows with the same text; every pass
        // takes at least one row.
        for _ in 0..rows.len() {
            let Some(first) = rows.get(at) else { break };
            let text = slice(&arena, first.text);
            let rest = rows.get(at..).unwrap_or_default();
            let run = rest
                .iter()
                .take_while(|row| slice(&arena, row.text) == text)
                .count();
            let group = rest.get(..run).unwrap_or_default();
            let latest = group.iter().map(|row| row.when).max().unwrap_or(0);
            let (id, is_new) = self.id_for(text, latest);
            if is_new {
                fresh.push((id, latest));
            }
            for row in group {
                if let Some(rows_of_dir) = usize::try_from(row.dir)
                    .ok()
                    .and_then(|dir| self.by_dir.get_mut(dir))
                {
                    rows_of_dir.push(DirRow { id, when: row.when });
                }
            }
            at = at.saturating_add(run);
        }
        self.merge(fresh);
        self.refresh_local();
    }

    /// The id of the command with this text, adding it if it is new, and its
    /// last run brought up to `when`. Returns whether it was new.
    fn id_for(&mut self, text: &str, when: i64) -> (u32, bool) {
        let at = self.sorted.partition_point(|&id| self.text(id) < text);
        if let Some(&id) = self.sorted.get(at)
            && self.text(id) == text
        {
            if let Some(recent) = self.recent.get_mut(at) {
                *recent = (*recent).max(when);
            }
            return (id, false);
        }
        let start = u32::try_from(self.arena.len()).unwrap_or(u32::MAX);
        let len = u32::try_from(text.len()).unwrap_or(0);
        let id = u32::try_from(self.spans.len()).unwrap_or(u32::MAX);
        self.arena.push_str(text);
        self.spans.push(Span { start, len });
        (id, true)
    }

    /// Put the commands in `fresh` (which are in the arena but not in the
    /// sorted list) into place.
    fn merge(&mut self, mut fresh: Vec<(u32, i64)>) {
        if fresh.is_empty() {
            return;
        }
        fresh.sort_by(|a, b| self.text(a.0).cmp(self.text(b.0)));
        let mut sorted = Vec::with_capacity(self.sorted.len().saturating_add(fresh.len()));
        let mut recent = Vec::with_capacity(sorted.capacity());
        let mut old = self.sorted.iter().zip(self.recent.iter()).peekable();
        let mut new = fresh.iter().peekable();
        loop {
            let take_old = match (old.peek(), new.peek()) {
                (Some((old_id, _)), Some((new_id, _))) => {
                    self.text(**old_id).cmp(self.text(*new_id)) != Ordering::Greater
                }
                (Some(_), None) => true,
                (None, Some(_)) => false,
                (None, None) => break,
            };
            if take_old {
                if let Some((id, when)) = old.next() {
                    sorted.push(*id);
                    recent.push(*when);
                }
            } else if let Some((id, when)) = new.next() {
                sorted.push(*id);
                recent.push(*when);
            }
        }
        self.sorted = sorted;
        self.recent = recent;
    }

    /// The shell is now in `cwd`; commands run there come first.
    pub fn set_cwd(&mut self, cwd: &str) {
        cwd.clone_into(&mut self.cwd);
        self.refresh_local();
    }

    /// Bring the view of the current directory up to date: built if the
    /// directory changed or has gained commands, dropped if it has none.
    fn refresh_local(&mut self) {
        let Some(dir) = self.dir_ids.get(&self.cwd).copied() else {
            self.local = Local::default();
            return;
        };
        let Some(rows) = usize::try_from(dir).ok().and_then(|at| self.by_dir.get(at)) else {
            return;
        };
        if self.local.dir == Some(dir) && self.local.built_from == rows.len() {
            return;
        }
        let mut pairs: Vec<DirRow> = rows.clone();
        let built_from = pairs.len();
        pairs.sort_by(|a, b| {
            self.text(a.id)
                .cmp(self.text(b.id))
                .then(b.when.cmp(&a.when))
        });
        pairs.dedup_by_key(|row| row.id);
        self.local = Local {
            dir: Some(dir),
            built_from,
            ids: pairs.iter().map(|row| row.id).collect(),
            when: pairs.iter().map(|row| row.when).collect(),
        };
    }

    /// The command to suggest for `line`: the most recent one that starts
    /// with it and is longer, among those run in the current directory if
    /// there are any, else among all.
    #[must_use]
    pub fn suggest(&self, line: &str) -> Option<&str> {
        if line.is_empty() {
            return None;
        }
        self.best(&self.local.ids, &self.local.when, line)
            .or_else(|| self.best(&self.sorted, &self.recent, line))
    }

    fn best(&self, ids: &[u32], when: &[i64], line: &str) -> Option<&str> {
        let low = ids.partition_point(|&id| self.text(id) < line);
        let tail = ids.get(low..)?;
        let run = tail.partition_point(|&id| self.text(id).starts_with(line));
        let mut from = low;
        // The command that is exactly what was typed has nothing to add.
        if run > 0 && ids.get(low).is_some_and(|&id| self.text(id) == line) {
            from = from.saturating_add(1);
        }
        let end = low.saturating_add(run);
        let stretch = when.get(from..end)?;
        let (offset, _) = stretch.iter().enumerate().max_by_key(|&(_, when)| *when)?;
        let id = *ids.get(from.saturating_add(offset))?;
        Some(self.text(id))
    }
}

fn slice(arena: &str, span: Span) -> &str {
    let start = usize::try_from(span.start).unwrap_or(0);
    let end = start.saturating_add(usize::try_from(span.len).unwrap_or(0));
    arena.get(start..end).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index(history: &[(&str, &str, i64)]) -> Index {
        let mut index = Index::new();
        for &(command, cwd, when) in history {
            index.push(command, cwd, when).unwrap();
        }
        index.commit();
        index
    }

    #[test]
    fn the_most_recent_command_with_the_prefix_wins() {
        let index = index(&[
            ("git status", "/a", 1),
            ("git stash", "/a", 3),
            ("git switch main", "/a", 2),
        ]);
        assert_eq!(index.suggest("git s"), Some("git stash"));
        assert_eq!(index.suggest("git sw"), Some("git switch main"));
        assert_eq!(index.suggest("git sta"), Some("git stash"));
    }

    #[test]
    fn nothing_matches_nothing() {
        let index = index(&[("git status", "/a", 1)]);
        assert_eq!(index.suggest("ls"), None);
        assert_eq!(index.suggest("git status -sb"), None);
    }

    #[test]
    fn an_empty_line_is_never_suggested_for() {
        let index = index(&[("ls", "/a", 1)]);
        assert_eq!(index.suggest(""), None);
    }

    #[test]
    fn a_command_exactly_as_typed_has_nothing_to_add() {
        let index = index(&[("make", "/a", 1)]);
        assert_eq!(index.suggest("make"), None);
        let index = self::index(&[("make", "/a", 5), ("make test", "/a", 1)]);
        assert_eq!(index.suggest("make"), Some("make test"));
    }

    #[test]
    fn a_repeated_command_counts_by_its_last_run() {
        let index = index(&[
            ("cargo build", "/a", 1),
            ("cargo test", "/a", 2),
            ("cargo build", "/a", 3),
        ]);
        assert_eq!(index.suggest("cargo "), Some("cargo build"));
        assert_eq!(index.len(), 2);
        assert_eq!(index.rows(), 3);
    }

    #[test]
    fn commands_run_in_this_directory_come_first() {
        let mut index = index(&[
            ("make here", "/work/here", 1),
            ("make elsewhere", "/work/other", 9),
        ]);
        assert_eq!(index.suggest("make "), Some("make elsewhere"));
        index.set_cwd("/work/here");
        assert_eq!(index.suggest("make "), Some("make here"));
        index.set_cwd("/work/other");
        assert_eq!(index.suggest("make "), Some("make elsewhere"));
    }

    #[test]
    fn with_nothing_here_it_falls_back_to_everything() {
        let mut index = index(&[("make here", "/work/here", 1), ("ls -la", "/work/other", 9)]);
        index.set_cwd("/work/here");
        assert_eq!(index.suggest("ls"), Some("ls -la"));
        index.set_cwd("/nowhere");
        assert_eq!(index.suggest("make"), Some("make here"));
    }

    #[test]
    fn within_this_directory_the_most_recent_wins() {
        let mut index = index(&[
            ("git add a", "/p", 1),
            ("git add b", "/p", 7),
            ("git add c", "/q", 9),
        ]);
        index.set_cwd("/p");
        assert_eq!(index.suggest("git add"), Some("git add b"));
    }

    #[test]
    fn later_commits_are_merged_and_seen() {
        let mut index = index(&[("echo a", "/a", 1), ("echo c", "/a", 3)]);
        index.set_cwd("/a");
        index.push("echo b", "/a", 4).unwrap();
        index.push("ls", "/b", 5).unwrap();
        assert_eq!(index.suggest("echo "), Some("echo c"));
        index.commit();
        assert_eq!(index.suggest("echo "), Some("echo b"));
        assert_eq!(index.suggest("l"), Some("ls"));
        assert_eq!(index.len(), 4);
    }

    #[test]
    fn a_repeat_in_a_later_commit_moves_the_command_forward() {
        let mut index = index(&[("echo a", "/a", 1), ("echo b", "/a", 2)]);
        assert_eq!(index.suggest("echo "), Some("echo b"));
        index.push("echo a", "/a", 3).unwrap();
        index.commit();
        assert_eq!(index.suggest("echo "), Some("echo a"));
        assert_eq!(index.len(), 2);
    }

    #[test]
    fn a_directory_view_is_rebuilt_when_it_gains_a_command() {
        let mut index = index(&[("make one", "/p", 1), ("make two", "/q", 5)]);
        index.set_cwd("/p");
        index.push("make three", "/p", 6).unwrap();
        index.commit();
        assert_eq!(index.suggest("make "), Some("make three"));
    }

    #[test]
    fn commands_that_cannot_be_typed_are_refused() {
        let mut index = Index::new();
        assert_eq!(index.push("", "/", 1), Err(Refused::NotText));
        assert_eq!(index.push("a\nb", "/", 1), Err(Refused::NotText));
        assert_eq!(index.push("a\tb", "/", 1), Err(Refused::NotText));
        assert_eq!(index.push(" secret", "/", 1), Err(Refused::Hidden));
        assert_eq!(index.push(&"x".repeat(5000), "/", 1), Err(Refused::TooLong));
        assert_eq!(index.push("fine", "/", 1), Ok(()));
    }

    #[test]
    fn prefixes_with_multibyte_text_work() {
        let index = index(&[("echo \u{65e5}\u{672c}", "/a", 1)]);
        assert_eq!(
            index.suggest("echo \u{65e5}"),
            Some("echo \u{65e5}\u{672c}")
        );
    }

    #[test]
    fn a_prefix_that_is_a_prefix_of_nothing_is_none_even_between_commands() {
        let index = index(&[("aa", "/", 1), ("cc", "/", 2)]);
        assert_eq!(index.suggest("b"), None);
    }

    #[test]
    fn clear_forgets_everything() {
        let mut index = index(&[("ls -la", "/", 1)]);
        index.clear();
        assert!(index.is_empty());
        assert_eq!(index.suggest("ls"), None);
    }

    #[test]
    fn a_lookup_in_a_large_history_is_fast() {
        let mut index = Index::new();
        for n in 0..100_000_i64 {
            index
                .push(&format!("cmd{} --flag {}", n % 30_000, n), "/d", n)
                .unwrap();
        }
        index.commit();
        let started = std::time::Instant::now();
        for n in 0..1000 {
            let _ = index.suggest(&format!("cmd{}", n % 100));
        }
        let each = started.elapsed() / 1000;
        assert!(
            each < std::time::Duration::from_millis(2),
            "{each:?} a lookup"
        );
    }
}
