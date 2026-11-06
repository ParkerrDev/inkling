//! The history, ready to be asked "what comes after this?" in microseconds.
//!
//! Every distinct command is kept once, in a list sorted by its text with
//! case set aside ([`text::key_cmp`]), so the commands that start with what
//! has been typed, in any case, are one run of that list, found by two
//! binary searches. Of those, the ones that fit are the ones that have every
//! capital that was typed ([`text::start`]). Beside it, for each command,
//! what it is worth in the whole history and which of its first characters
//! are capitals, so that most of a run is judged without reading its text;
//! and for each directory, a scope of the commands run there, searchable the
//! same way. The project the shell is in
//! gets a scope too, made from the directories under it. And for each
//! command, what was run next in the same session, and how often.
//!
//! [`Index::ranked`] puts the commands that fit in order by the mixture
//! described in [`rank`]. Only the best few of the whole history's run need
//! a score worked out, since a command that the project, the directory and
//! the last command know nothing of scores by its worth alone.
//!
//! Commands come in by [`Index::push`] and become visible at
//! [`Index::commit`], which takes a batch in one pass: loading a hundred
//! thousand runs is a sort, not a hundred thousand insertions.

pub mod rank;
mod scope;
mod worth;

use std::collections::{HashMap, HashSet};

pub use rank::{Best, Weights};
use scope::Scope;
use worth::NONE;

use crate::text::{self, Start, is_typeable};

/// A command longer than this is not kept: it would not fit on a row anyway,
/// and a few huge pasted lines should not decide how much memory this uses.
const MAX_COMMAND_BYTES: usize = 2048;
/// The most commands run that are kept. The oldest are the ones left out.
const MAX_ROWS: usize = 2_000_000;
/// The most bytes of command text kept.
const MAX_ARENA: usize = 256 << 20;
/// The most of a run of the whole history looked through for its best.
const MAX_SCAN: usize = 1 << 18;
/// The most of a run in a directory or a project that gets a score.
const MAX_LOCAL: usize = 4096;
/// The most different next commands remembered after one command.
const MAX_FOLLOWS: usize = 64;
/// The most sessions followed at once while reading.
const MAX_SESSIONS: usize = 1 << 16;

/// One run of a command, as a source reports it.
#[derive(Clone, Copy, Debug)]
pub struct Run<'a> {
    pub command: &'a str,
    /// The directory it ran in, or empty if that is not known.
    pub cwd: &'a str,
    /// It went well, or nobody knows (only a failure counts against it).
    pub ok: bool,
    /// The session it ran in: one shell, from start to exit.
    pub session: &'a str,
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

/// How the commands that fit are put in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Order {
    /// By the chance of being meant (see [`rank`]).
    Likely,
    /// The most recent, as fish has it: in the case typed if any fits so,
    /// else in any case.
    Recent,
    /// The most recent, but one run in this directory before all others, as
    /// inkling had it before.
    RecentHereFirst,
}

/// Where a command's text lies in the arena.
#[derive(Clone, Copy, Debug)]
struct Span {
    start: u32,
    len: u32,
}

/// A run waiting to be committed.
#[derive(Debug)]
struct Pending {
    text: Span,
    dir: u32,
    seq: u32,
    ok: bool,
    session: u32,
}

/// What came after one command, and how often.
#[derive(Debug, Default)]
struct Follows {
    next: Vec<(u32, u32)>,
    total: u32,
}

impl Follows {
    fn add(&mut self, id: u32) {
        self.total = self.total.saturating_add(1);
        if let Some(entry) = self.next.iter_mut().find(|(next, _)| *next == id) {
            entry.1 = entry.1.saturating_add(1);
        } else if self.next.len() < MAX_FOLLOWS {
            self.next.push((id, 1));
        }
    }

    fn share(&self, id: u32) -> f64 {
        let count = self
            .next
            .iter()
            .find(|(next, _)| *next == id)
            .map_or(0, |entry| entry.1);
        if self.total == 0 {
            return 0.0;
        }
        f64::from(count) / f64::from(self.total)
    }
}

/// The scope of the project the shell is in.
#[derive(Debug)]
struct Project {
    root: String,
    scope: Scope,
}

/// Whether directory `dir` is `root` or under it.
fn under(dir: &str, root: &str) -> bool {
    dir.strip_prefix(root)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/') || root.ends_with('/'))
}

fn slice(arena: &str, span: Span) -> &str {
    let start = usize::try_from(span.start).unwrap_or(0);
    let end = start.saturating_add(usize::try_from(span.len).unwrap_or(0));
    arena.get(start..end).unwrap_or_default()
}

fn text_of<'a>(arena: &'a str, spans: &[Span], id: u32) -> &'a str {
    let span = usize::try_from(id)
        .ok()
        .and_then(|at| spans.get(at))
        .copied()
        .unwrap_or(Span { start: 0, len: 0 });
    slice(arena, span)
}

/// The history, searchable by prefix and put in order by what is likely.
#[derive(Debug)]
pub struct Index {
    weights: Weights,
    /// The text of every distinct command, one after another.
    arena: String,
    /// Where each command's text is, by id.
    spans: Vec<Span>,
    /// Command ids, ordered by the text.
    sorted: Vec<u32>,
    /// Where each id is in `sorted`.
    pos: Vec<u32>,
    /// The latest run of each command, in the order of `sorted`.
    last: Vec<u32>,
    /// What each command is worth in the whole history, in that order.
    anchor: Vec<f64>,
    /// Which of each command's first characters are capitals, in that order
    /// (see [`text::capitals`]).
    cases: Vec<u64>,
    /// The whole history's runs together.
    mass: f64,
    pending: Vec<Pending>,
    pending_arena: String,
    dir_ids: HashMap<String, u32>,
    dir_names: Vec<String>,
    dirs: Vec<Scope>,
    session_ids: HashMap<String, u32>,
    /// The last command of each session, while reading.
    session_last: HashMap<u32, u32>,
    follows: HashMap<u32, Follows>,
    /// How many runs have been taken in: the clock runs fade by.
    seq: u32,
    cwd: String,
    project: Option<Project>,
    /// The command run last in this shell.
    prev: Option<u32>,
}

impl Default for Index {
    fn default() -> Self {
        Self::with_weights(Weights::default())
    }
}

impl Index {
    /// An empty index.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// An empty index that orders by `weights`.
    #[must_use]
    pub fn with_weights(weights: Weights) -> Self {
        Self {
            weights,
            arena: String::new(),
            spans: Vec::new(),
            sorted: Vec::new(),
            pos: Vec::new(),
            last: Vec::new(),
            anchor: Vec::new(),
            cases: Vec::new(),
            mass: NONE,
            pending: Vec::new(),
            pending_arena: String::new(),
            dir_ids: HashMap::new(),
            dir_names: Vec::new(),
            dirs: Vec::new(),
            session_ids: HashMap::new(),
            session_last: HashMap::new(),
            follows: HashMap::new(),
            seq: 0,
            cwd: String::new(),
            project: None,
            prev: None,
        }
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
        usize::try_from(self.seq).unwrap_or(usize::MAX)
    }

    /// Forget everything.
    pub fn clear(&mut self) {
        *self = Self::with_weights(self.weights);
    }

    fn text(&self, id: u32) -> &str {
        text_of(&self.arena, &self.spans, id)
    }

    /// Take in a run of a command. It is not searchable until
    /// [`Index::commit`].
    ///
    /// # Errors
    ///
    /// Says why the command was left out.
    pub fn push(&mut self, run: &Run<'_>) -> Result<(), Refused> {
        let command = run.command;
        if command.starts_with(char::is_whitespace) {
            return Err(Refused::Hidden);
        }
        if !is_typeable(command) {
            return Err(Refused::NotText);
        }
        if command.len() > MAX_COMMAND_BYTES {
            return Err(Refused::TooLong);
        }
        let full = self.rows() >= MAX_ROWS
            || self.arena.len().saturating_add(self.pending_arena.len()) > MAX_ARENA;
        if full {
            return Err(Refused::Full);
        }
        let start = u32::try_from(self.pending_arena.len()).map_err(|_| Refused::Full)?;
        let len = u32::try_from(command.len()).map_err(|_| Refused::TooLong)?;
        let dir = self.dir_id(run.cwd)?;
        let session = self.session_id(run.session);
        self.seq = self.seq.checked_add(1).ok_or(Refused::Full)?;
        self.pending_arena.push_str(command);
        self.pending.push(Pending {
            text: Span { start, len },
            dir,
            seq: self.seq,
            ok: run.ok,
            session,
        });
        Ok(())
    }

    fn dir_id(&mut self, cwd: &str) -> Result<u32, Refused> {
        if let Some(&id) = self.dir_ids.get(cwd) {
            return Ok(id);
        }
        let id = u32::try_from(self.dirs.len()).map_err(|_| Refused::Full)?;
        self.dir_ids.insert(cwd.to_owned(), id);
        self.dir_names.push(cwd.to_owned());
        self.dirs.push(Scope::default());
        Ok(id)
    }

    fn session_id(&mut self, session: &str) -> u32 {
        if let Some(&id) = self.session_ids.get(session) {
            return id;
        }
        if self.session_ids.len() >= MAX_SESSIONS {
            // Old sessions are over; what they ran last is forgotten.
            self.session_ids.clear();
            self.session_last.clear();
        }
        let id = u32::try_from(self.session_ids.len()).unwrap_or(u32::MAX);
        self.session_ids.insert(session.to_owned(), id);
        id
    }

    /// Make everything pushed so far searchable.
    pub fn commit(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        let rows = std::mem::take(&mut self.pending);
        let arena = std::mem::take(&mut self.pending_arena);
        let ids = self.assign(&rows, &arena);
        let mut touched = HashSet::new();
        for (row, &id) in rows.iter().zip(&ids) {
            self.record(row, id);
            touched.insert(row.dir);
        }
        self.settle(&touched);
        debug_assert_eq!(
            self.sorted.len(),
            self.anchor.len(),
            "the lists stay in step"
        );
        debug_assert_eq!(
            self.sorted.len(),
            self.pos.len(),
            "every command has a place"
        );
    }

    /// The id of each row's command, new ones given ids and put in place.
    fn assign(&mut self, rows: &[Pending], arena: &str) -> Vec<u32> {
        let mut ids = Vec::with_capacity(rows.len());
        let mut fresh: Vec<u32> = Vec::new();
        let mut new_texts: HashMap<&str, u32> = HashMap::new();
        for row in rows {
            let text = slice(arena, row.text);
            let at = self
                .sorted
                .partition_point(|&id| text::key_cmp(self.text(id), text).is_lt());
            let known = self
                .sorted
                .get(at)
                .copied()
                .filter(|&id| self.text(id) == text);
            let id = if let Some(id) = known.or_else(|| new_texts.get(text).copied()) {
                id
            } else {
                {
                    let id = u32::try_from(self.spans.len()).unwrap_or(u32::MAX);
                    let start = u32::try_from(self.arena.len()).unwrap_or(u32::MAX);
                    self.arena.push_str(text);
                    self.spans.push(Span {
                        start,
                        len: row.text.len,
                    });
                    new_texts.insert(text, id);
                    fresh.push(id);
                    id
                }
            };
            ids.push(id);
        }
        self.merge(fresh);
        ids
    }

    /// Put the commands in `fresh` (in the arena, not yet in the sorted
    /// list) into place, with nothing known of their runs yet.
    fn merge(&mut self, mut fresh: Vec<u32>) {
        if fresh.is_empty() {
            return;
        }
        fresh.sort_by(|a, b| text::key_cmp(self.text(*a), self.text(*b)));
        let total = self.sorted.len().saturating_add(fresh.len());
        let (mut sorted, mut last, mut anchor, mut cases) = (
            Vec::with_capacity(total),
            Vec::with_capacity(total),
            Vec::with_capacity(total),
            Vec::with_capacity(total),
        );
        let mut old = (0..self.sorted.len()).peekable();
        let mut new = fresh.iter().peekable();
        // Each turn takes one command from one of the two lists.
        for _ in 0..total {
            let old_id = old.peek().and_then(|&at| self.sorted.get(at)).copied();
            let take_old = match (old_id, new.peek()) {
                (Some(a), Some(b)) => text::key_cmp(self.text(a), self.text(**b)).is_le(),
                (Some(_), None) => true,
                (None, Some(_)) => false,
                (None, None) => break,
            };
            if take_old {
                let Some(at) = old.next() else { break };
                sorted.push(self.sorted.get(at).copied().unwrap_or(0));
                last.push(self.last.get(at).copied().unwrap_or(0));
                anchor.push(self.anchor.get(at).copied().unwrap_or(NONE));
                cases.push(self.cases.get(at).copied().unwrap_or(0));
            } else if let Some(&id) = new.next() {
                sorted.push(id);
                last.push(0);
                anchor.push(NONE);
                cases.push(text::capitals(self.text(id)));
            }
        }
        (self.sorted, self.last, self.anchor, self.cases) = (sorted, last, anchor, cases);
        self.pos = vec![0; self.spans.len()];
        for (at, &id) in self.sorted.iter().enumerate() {
            if let Some(slot) = usize::try_from(id).ok().and_then(|id| self.pos.get_mut(id)) {
                *slot = u32::try_from(at).unwrap_or(u32::MAX);
            }
        }
    }

    /// Count one run of command `id` everywhere it counts.
    fn record(&mut self, row: &Pending, id: u32) {
        let weights = self.weights;
        let ln_weight = if row.ok { 0.0 } else { weights.failed.ln() };
        let term = worth::term(row.seq, ln_weight, weights.fade);
        if let Some(at) = self.place(id) {
            if let Some(anchor) = self.anchor.get_mut(at) {
                *anchor = worth::log_add(*anchor, term);
            }
            if let Some(last) = self.last.get_mut(at) {
                *last = (*last).max(row.seq);
            }
        }
        self.mass = worth::log_add(self.mass, term);
        let dir = usize::try_from(row.dir).ok();
        if let Some(scope) = dir.and_then(|dir| self.dirs.get_mut(dir)) {
            scope.add(id, row.seq, ln_weight, weights.fade_here);
        }
        let dir_name = dir.and_then(|dir| self.dir_names.get(dir));
        if let (Some(project), Some(name)) = (&mut self.project, dir_name)
            && under(name, &project.root)
        {
            project.scope.add(id, row.seq, ln_weight, weights.fade_here);
        }
        if let Some(&before) = self.session_last.get(&row.session) {
            self.follows.entry(before).or_default().add(id);
        }
        self.session_last.insert(row.session, id);
    }

    /// Where command `id` is in the sorted list.
    fn place(&self, id: u32) -> Option<usize> {
        let at = self.pos.get(usize::try_from(id).ok()?)?;
        usize::try_from(*at).ok()
    }

    /// Put the commands new to the directories in `touched`, and to the
    /// project, into their places.
    fn settle(&mut self, touched: &HashSet<u32>) {
        let Self {
            arena,
            spans,
            dirs,
            project,
            ..
        } = self;
        let text = |id: u32| text_of(arena, spans, id);
        for &dir in touched {
            if let Some(scope) = usize::try_from(dir).ok().and_then(|dir| dirs.get_mut(dir)) {
                scope.settle(text);
            }
        }
        if let Some(project) = project {
            project.scope.settle(text);
        }
    }

    /// The shell is now in `cwd`, in the project whose root is `root` if it
    /// is in one.
    pub fn set_context(&mut self, cwd: &str, root: Option<&str>) {
        cwd.clone_into(&mut self.cwd);
        let same = match (&self.project, root) {
            (Some(project), Some(root)) => project.root == root,
            (None, None) => true,
            _ => false,
        };
        if same {
            return;
        }
        self.project = root.map(|root| self.project_scope(root));
    }

    /// The scope of the project at `root`: every directory under it.
    fn project_scope(&self, root: &str) -> Project {
        let mut scope = Scope::default();
        for (name, &dir) in &self.dir_ids {
            if !under(name, root) {
                continue;
            }
            if let Some(theirs) = usize::try_from(dir).ok().and_then(|dir| self.dirs.get(dir)) {
                scope.absorb(theirs);
            }
        }
        scope.settle(|id| self.text(id));
        Project {
            root: root.to_owned(),
            scope,
        }
    }

    /// The command just run in this shell was `line`, if it is known.
    pub fn set_prev(&mut self, line: Option<&str>) {
        self.prev = line.and_then(|line| self.id_of(line));
    }

    /// Whether command `text` has been run (and committed).
    #[must_use]
    pub fn knows(&self, text: &str) -> bool {
        self.id_of(text).is_some()
    }

    fn id_of(&self, text: &str) -> Option<u32> {
        let at = self
            .sorted
            .partition_point(|&id| text::key_cmp(self.text(id), text).is_lt());
        self.sorted
            .get(at)
            .copied()
            .filter(|&id| self.text(id) == text)
    }

    fn here(&self) -> Option<&Scope> {
        let dir = self.dir_ids.get(&self.cwd)?;
        self.dirs.get(usize::try_from(*dir).ok()?)
    }

    /// The chance that command `id` is the one meant, as [`rank`] has it.
    fn score(&self, id: u32) -> f64 {
        let w = &self.weights;
        let everywhere = self
            .place(id)
            .and_then(|at| self.anchor.get(at))
            .map_or(0.0, |&anchor| worth::share(anchor, self.mass));
        let project = self.project.as_ref().map_or(0.0, |p| p.scope.share(id));
        let directory = self.here().map_or(0.0, |scope| scope.share(id));
        let follows = self
            .prev
            .and_then(|prev| self.follows.get(&prev))
            .map_or(0.0, |follows| follows.share(id));
        w.everywhere * everywhere
            + w.project * project
            + w.directory * directory
            + w.follows * follows
    }

    /// The commands that start with `line`, case aside, in the sorted list:
    /// from, to.
    fn run(&self, line: &str) -> (usize, usize) {
        let low = self
            .sorted
            .partition_point(|&id| text::fold_cmp(self.text(id), line).is_lt());
        let tail = self.sorted.get(low..).unwrap_or_default();
        let len = tail.partition_point(|&id| text::starts_folded(self.text(id), line));
        (low, low.saturating_add(len))
    }

    /// What a command that fits as `start` counts for, if it fits.
    fn weight(&self, start: Start) -> Option<f64> {
        match start {
            Start::Exact => Some(1.0),
            Start::Loose => Some(self.weights.case),
            Start::No => None,
        }
    }

    /// Offer command `id` to `best` with its score, if it fits `query`.
    fn consider(&self, best: &mut Best, id: u32, query: &Query<'_>) {
        if let Some(weight) = self.weight(query.fit(self.text(id))) {
            best.offer(id, self.score(id) * weight);
        }
    }

    /// The best few commands that fit `line` and are longer, in `order`,
    /// best first.
    #[must_use]
    pub fn ranked(&self, line: &str, order: Order) -> Best {
        if line.is_empty() {
            return Best::default();
        }
        let query = Query::new(line);
        match order {
            Order::Likely => self.likely(&query),
            Order::Recent => self.recent(&query, false),
            Order::RecentHereFirst => self.recent(&query, true),
        }
    }

    fn likely(&self, query: &Query<'_>) -> Best {
        let (from, to) = self.run(query.line);
        // The whole history's best by worth alone: any command that no
        // other estimate knows scores by that, so one of these, or one the
        // others know, is the best of all. What can be told without the
        // text is asked first: whether the worth is enough, then whether
        // the capitals are.
        let mut worthiest = Best::default();
        let stretch = self.anchor.get(from..to).unwrap_or_default();
        let masks = self.cases.get(from..to).unwrap_or_default();
        for (offset, (&anchor, &mask)) in stretch.iter().zip(masks).enumerate().take(MAX_SCAN) {
            if !worthiest.would_take(anchor) {
                continue;
            }
            let Some(guess) = self.weight(query.guess(mask)) else {
                continue;
            };
            let id = self.sorted.get(from.saturating_add(offset)).copied();
            if let (true, Some(id)) = (worthiest.would_take(anchor + guess.ln()), id)
                && let Some(weight) = self.weight(query.fit(self.text(id)))
            {
                worthiest.offer(id, anchor + weight.ln());
            }
        }
        let mut best = Best::default();
        for id in worthiest.ids() {
            self.consider(&mut best, id, query);
        }
        let text = |id: u32| self.text(id);
        let scopes = [self.here(), self.project.as_ref().map(|p| &p.scope)];
        for scope in scopes.into_iter().flatten() {
            for &id in scope.starting(query.line, text).iter().take(MAX_LOCAL) {
                self.consider(&mut best, id, query);
            }
        }
        let follows = self.prev.and_then(|prev| self.follows.get(&prev));
        for &(id, _) in follows.map(|f| f.next.as_slice()).unwrap_or_default() {
            self.consider(&mut best, id, query);
        }
        best
    }

    /// The most recent first, in the two ways it has been done before.
    ///
    /// inkling's first rule (`here_first`) minded case, and put a command run in
    /// this directory before all others. fish takes the most recent command
    /// that starts with the line as typed and, only when history has none,
    /// the most recent that starts with it in any case at all, a capital
    /// typed or not (`reader.rs`, `get_autosuggestion_performer`).
    fn recent(&self, query: &Query<'_>, here_first: bool) -> Best {
        if here_first && let Some(best) = self.recent_here(query) {
            return best;
        }
        let (from, to) = self.run(query.line);
        let stretch = self.last.get(from..to).unwrap_or_default();
        let masks = self.cases.get(from..to).unwrap_or_default();
        let (mut exact, mut any) = (Best::default(), Best::default());
        for (offset, (&last, &mask)) in stretch.iter().zip(masks).enumerate().take(MAX_SCAN) {
            let score = f64::from(last);
            let for_exact = exact.would_take(score) && query.guess(mask) == Start::Exact;
            let for_any = !here_first && exact.is_empty() && any.would_take(score);
            if !for_exact && !for_any {
                continue;
            }
            let Some(id) = self.sorted.get(from.saturating_add(offset)).copied() else {
                continue;
            };
            let command = self.text(id);
            if !text::longer(command, query.chars) {
                continue;
            }
            if for_exact && text::start(command, query.line) == Start::Exact {
                exact.offer(id, score);
            } else if for_any {
                any.offer(id, score);
            }
        }
        if exact.is_empty() { any } else { exact }
    }

    /// The most recent of the commands run here that start with the line
    /// as typed, if there are any.
    fn recent_here(&self, query: &Query<'_>) -> Option<Best> {
        let scope = self.here()?;
        let text = |id: u32| self.text(id);
        let mut best = Best::default();
        for &id in scope.starting(query.line, text).iter().take(MAX_LOCAL) {
            if let (Start::Exact, Some(stat)) = (query.fit(self.text(id)), scope.stat(id)) {
                best.offer(id, f64::from(stat.last));
            }
        }
        (!best.is_empty()).then_some(best)
    }

    /// The text of command `id`.
    #[must_use]
    pub fn command(&self, id: u32) -> &str {
        self.text(id)
    }

    /// The command to suggest for `line`: the likeliest that fits it and is
    /// longer.
    #[must_use]
    pub fn suggest(&self, line: &str) -> Option<&str> {
        self.ranked(line, Order::Likely)
            .first()
            .map(|id| self.text(id))
    }

    /// What a program's name is worth as the first word of a line: the share
    /// of the whole history held by the command that is just the name and by
    /// those that start with it and a blank, in its own case.
    #[must_use]
    pub fn word_worth(&self, word: &str) -> f64 {
        let share = |at: usize| {
            self.anchor
                .get(at)
                .map_or(0.0, |&anchor| worth::share(anchor, self.mass))
        };
        let mut total = self
            .id_of(word)
            .and_then(|id| self.place(id))
            .map_or(0.0, share);
        let prefix = format!("{word} ");
        let query = Query::new(&prefix);
        let (from, to) = self.run(&prefix);
        for at in (from..to).take(MAX_SCAN) {
            let mask = self.cases.get(at).copied().unwrap_or(0);
            let id = self.sorted.get(at).copied();
            let exact = query.guess(mask) == Start::Exact
                && id.is_some_and(|id| self.text(id).starts_with(&prefix));
            if exact {
                total += share(at);
            }
        }
        total
    }
}

/// What was typed, as the commands are measured against it.
struct Query<'a> {
    line: &'a str,
    /// How many characters it has.
    chars: usize,
    /// Which of its first characters are capitals ([`text::capitals`]).
    capitals: u64,
    /// The bits of the characters that [`text::capitals`] looks at.
    span: u64,
}

impl<'a> Query<'a> {
    fn new(line: &'a str) -> Self {
        let chars = line.chars().count();
        let looked = u32::try_from(chars.min(text::CAPITALS)).unwrap_or(u32::MAX);
        let span = 1_u64
            .checked_shl(looked)
            .map_or(u64::MAX, |bit| bit.wrapping_sub(1));
        Self {
            line,
            chars,
            capitals: text::capitals(line),
            span,
        }
    }

    /// How a command in the run of `line` fits, as far as its capitals,
    /// `mask`, tell: `No` and `Loose` for certain, `Exact` perhaps.
    fn guess(&self, mask: u64) -> Start {
        let have = mask & self.span;
        if have & self.capitals != self.capitals {
            Start::No
        } else if have == self.capitals {
            Start::Exact
        } else {
            Start::Loose
        }
    }

    /// How `command` fits: as [`text::start`] has it, and only if it is
    /// longer than what was typed.
    fn fit(&self, command: &str) -> Start {
        if text::longer(command, self.chars) {
            text::start(command, self.line)
        } else {
            Start::No
        }
    }
}

#[cfg(test)]
mod tests;
