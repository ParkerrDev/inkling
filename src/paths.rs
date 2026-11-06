//! What the filesystem says: the project a directory is in, whether the
//! paths in a command are there to be used from here, and the names that can
//! finish a word when the history has nothing.
//!
//! Everything asked is remembered until the next prompt, when the directory
//! may have changed and so may the files; nothing here is asked more than a
//! few times for a keystroke.
//!
//! A name is finished from those that start with the word exactly, if any
//! do, else from those that fit it with capitals for letters typed in lower
//! case (see [`crate::text`]), and the word is then put in their case. So
//! are the directories before it, when they are not there as typed but one
//! is there that is the same but for case: "ls ~/dev/gi" becomes
//! "ls ~/Dev/GitHub/".

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::text::{self, Start};

/// The most paths whose existence is remembered at once.
const MAX_REMEMBERED: usize = 512;
/// The most directories whose entries are remembered at once.
const MAX_LISTED: usize = 16;
/// The most entries read from one directory.
const MAX_ENTRIES: usize = 4096;
/// The most program names gathered from the search path.
const MAX_PROGRAMS: usize = 1 << 16;
/// The most directories walked up looking for a project's root.
const MAX_DEPTH: usize = 64;
/// How long the programs on the search path are taken to stay the same.
const PROGRAMS_FRESH: Duration = Duration::from_secs(60);
/// What marks the root of a project.
const ROOT_MARKS: &[&str] = &[".git", ".hg", ".jj"];
/// Commands whose first argument is a directory.
const TAKES_A_DIRECTORY: &[&str] = &["cd", "pushd", "rmdir"];
/// Words that leave the next one in a command's place.
const PREFIXES: &[&str] = &[
    "sudo", "doas", "time", "nohup", "exec", "command", "builtin",
];

/// One word of a command, as the shell would split it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Word {
    /// With the quotes taken off.
    text: String,
    /// Nothing in it that the shell would expand.
    plain: bool,
    /// An operator: `|`, `&&`, `;`, `>` and the like.
    operator: bool,
}

/// The words of `command`. Quotes are taken off; a word with anything the
/// shell expands (a variable, a glob, a command substitution) is not plain.
fn words(command: &str) -> Vec<Word> {
    let mut out = Vec::new();
    let mut word = Word {
        text: String::new(),
        plain: true,
        operator: false,
    };
    let mut started = false;
    let mut chars = command.chars().peekable();
    // Each turn takes one character, or a quoted stretch.
    for _ in 0..command.len() {
        let Some(c) = chars.next() else { break };
        match c {
            ' ' | '\t' => end_word(&mut out, &mut word, &mut started),
            '|' | '&' | ';' | '<' | '>' => {
                end_word(&mut out, &mut word, &mut started);
                let mut op = String::from(c);
                while let Some(&next) = chars.peek().filter(|n| "|&;<>".contains(**n)) {
                    op.push(next);
                    chars.next();
                }
                out.push(Word {
                    text: op,
                    plain: true,
                    operator: true,
                });
            }
            '\'' => {
                started = true;
                word.text.extend(chars.by_ref().take_while(|&q| q != '\''));
            }
            '"' => {
                started = true;
                for q in chars.by_ref() {
                    if q == '"' {
                        break;
                    }
                    word.plain &= !matches!(q, '$' | '`' | '\\');
                    word.text.push(q);
                }
            }
            '\\' => {
                started = true;
                if let Some(next) = chars.next() {
                    word.text.push(next);
                }
            }
            _ => {
                started = true;
                word.plain &= !matches!(c, '$' | '`' | '*' | '?' | '[' | '{' | '(' | ')' | '!');
                word.text.push(c);
            }
        }
    }
    end_word(&mut out, &mut word, &mut started);
    out
}

fn end_word(out: &mut Vec<Word>, word: &mut Word, started: &mut bool) {
    if *started {
        out.push(std::mem::replace(
            word,
            Word {
                text: String::new(),
                plain: true,
                operator: false,
            },
        ));
    }
    *started = false;
}

/// Whether a word is a path by its shape alone: it starts at the root, at
/// home, or at this directory or its parent. A word that merely has a slash
/// in it is as often a branch, a repository or an image as a path.
fn path_shaped(word: &str) -> bool {
    ["./", "../", "~/", "/"]
        .iter()
        .any(|start| word.starts_with(start))
        && !word.starts_with("//")
}

/// Whether `name` needs nothing to be typed as one word at a bash prompt.
fn bare(name: &str) -> bool {
    name.chars()
        .all(|c| c.is_alphanumeric() || "._+-,/@%^=:".contains(c))
}

/// `name` as it is typed: a backslash before each character the shell
/// would take for something else. Control characters cannot be typed.
fn escaped(name: &str) -> Option<String> {
    if bare(name) {
        return Some(name.to_owned());
    }
    let mut out = String::with_capacity(name.len().saturating_add(4));
    for c in name.chars() {
        if c.is_control() {
            return None;
        }
        if !(c.is_alphanumeric() || "._+-,/@%^=:".contains(c)) {
            out.push('\\');
        }
        out.push(c);
    }
    Some(out)
}

/// Of `items`, whose names are given by `name`, those that start with `word`
/// exactly and are longer, if any are; else those that fit it with capitals
/// for its lower case letters.
fn fitting<'a, T: 'a>(
    items: impl Iterator<Item = &'a T>,
    word: &str,
    name: impl Fn(&T) -> &str,
) -> Vec<&'a T> {
    let chars = word.chars().count();
    let (mut exact, mut loose) = (Vec::new(), Vec::new());
    for item in items {
        let name = name(item);
        if !text::longer(name, chars) {
            continue;
        }
        match text::start(name, word) {
            Start::Exact => exact.push(item),
            Start::Loose if exact.is_empty() && loose.len() < MAX_ENTRIES => loose.push(item),
            _ => {}
        }
        if exact.len() >= MAX_ENTRIES {
            break;
        }
    }
    if exact.is_empty() { loose } else { exact }
}

/// The longest start that every one of `names` has.
fn common_start<'a>(names: impl Iterator<Item = &'a str>) -> String {
    let mut common: Option<&str> = None;
    for name in names.take(MAX_ENTRIES) {
        common = Some(match common {
            None => name,
            Some(so_far) => {
                let end = so_far
                    .char_indices()
                    .zip(name.chars())
                    .find(|((_, a), b)| a != b)
                    .map_or(so_far.len().min(name.len()), |((at, _), _)| at);
                so_far.get(..end).unwrap_or_default()
            }
        });
    }
    common.unwrap_or_default().to_owned()
}

/// An entry of a directory.
#[derive(Debug, Clone)]
struct Entry {
    name: String,
    dir: bool,
}

impl Entry {
    /// The name as it is finished: a directory's with a slash after it.
    fn finished(&self) -> String {
        format!("{}{}", self.name, if self.dir { "/" } else { "" })
    }
}

/// The most names that fit a word whose times are compared to choose one.
const MAX_COMPARED: usize = 64;

/// What the filesystem says, remembered until the next prompt.
#[derive(Debug, Default)]
pub struct Look {
    cwd: PathBuf,
    home: Option<PathBuf>,
    search: Vec<PathBuf>,
    found: HashMap<PathBuf, Option<bool>>,
    listed: HashMap<PathBuf, Vec<Entry>>,
    roots: HashMap<PathBuf, Option<String>>,
    programs: Vec<String>,
    programs_at: Option<Instant>,
}

impl Look {
    /// For a user whose home is `home`, finding programs in `search`.
    #[must_use]
    pub fn new(home: Option<PathBuf>, search: Vec<PathBuf>) -> Self {
        Self {
            home,
            search,
            ..Self::default()
        }
    }

    /// A new prompt, in `cwd`: what was seen of the files is seen again.
    pub fn prompt(&mut self, cwd: &Path) {
        cwd.clone_into(&mut self.cwd);
        self.found.clear();
        self.listed.clear();
        // Read now, while a prompt is drawn, so that no key waits for it.
        self.list(cwd);
        self.gather_programs();
    }

    /// The root of the project `dir` is in: the nearest directory, `dir`
    /// itself or one above it, that holds a repository.
    pub fn project_root(&mut self, dir: &Path) -> Option<String> {
        if let Some(known) = self.roots.get(dir) {
            return known.clone();
        }
        let mut at = Some(dir);
        let mut root = None;
        for _ in 0..MAX_DEPTH {
            let Some(here) = at else { break };
            if ROOT_MARKS.iter().any(|mark| here.join(mark).exists()) {
                root = here.to_str().map(str::to_owned);
                break;
            }
            at = here.parent();
        }
        if self.roots.len() >= MAX_REMEMBERED {
            self.roots.clear();
        }
        self.roots.insert(dir.to_path_buf(), root.clone());
        root
    }

    fn resolve(&self, word: &str) -> Option<PathBuf> {
        if word == "~" {
            return self.home.clone();
        }
        if let Some(rest) = word.strip_prefix("~/") {
            return self.home.as_ref().map(|home| home.join(rest));
        }
        if word.starts_with('~') {
            // Another user's home: not looked into.
            return None;
        }
        Some(self.cwd.join(word))
    }

    /// Whether `path` is there: `Some(true)` for a directory.
    fn kind(&mut self, path: PathBuf) -> Option<bool> {
        if let Some(known) = self.found.get(&path) {
            return *known;
        }
        let kind = std::fs::metadata(&path).ok().map(|meta| meta.is_dir());
        if self.found.len() >= MAX_REMEMBERED {
            self.found.clear();
        }
        self.found.insert(path, kind);
        kind
    }

    /// Whether `command` can be used from here: every word in it shaped like
    /// a path names something that is there, and what `cd` is given is a
    /// directory. Words the shell would expand are not judged.
    pub fn usable(&mut self, command: &str) -> bool {
        let mut in_place = true;
        let mut directory_next = false;
        let mut target = false;
        for word in words(command) {
            if word.operator {
                in_place = matches!(word.text.as_str(), "|" | "||" | "&&" | ";" | "&");
                target = word.text.ends_with('>');
                continue;
            }
            if std::mem::take(&mut target) || !word.plain {
                // What output goes to need not exist yet.
                directory_next = false;
                continue;
            }
            if in_place {
                in_place = PREFIXES.contains(&word.text.as_str());
                directory_next = TAKES_A_DIRECTORY.contains(&word.text.as_str());
                continue;
            }
            let wants_directory =
                std::mem::take(&mut directory_next) && !word.text.starts_with('-');
            if wants_directory || path_shaped(&word.text) {
                let Some(path) = self.resolve(&word.text) else {
                    continue;
                };
                match self.kind(path) {
                    None => return false,
                    Some(false) if wants_directory => return false,
                    Some(_) => {}
                }
            }
        }
        true
    }

    /// What finishes the last word of `line` when the history has nothing
    /// for it: a program's name in a command's place, else a name in the
    /// directory the word points into. `worth` says what a program is worth
    /// to this person. Returns the line with its last word finished, in the
    /// case of the names it was finished with.
    pub fn finish(&mut self, line: &str, worth: &dyn Fn(&str) -> f64) -> Option<String> {
        let (word, command, in_place) = last_word(line)?;
        let finished = if in_place {
            self.finish_program(word, worth)?
        } else {
            let only_dirs = command.is_some_and(|c| TAKES_A_DIRECTORY.contains(&c));
            self.finish_path(word, only_dirs)?
        };
        // The word is the end of the line.
        let before = line.get(..line.len().checked_sub(word.len())?)?;
        Some(format!("{before}{finished}"))
    }

    /// The program's name that `word` finishes as, whole.
    fn finish_program(&mut self, word: &str, worth: &dyn Fn(&str) -> f64) -> Option<String> {
        if word.is_empty() || word.contains('/') {
            return None;
        }
        self.gather_programs();
        let low = self
            .programs
            .partition_point(|name| text::fold_cmp(name, word).is_lt());
        let run = self
            .programs
            .get(low..)
            .unwrap_or_default()
            .iter()
            .take_while(|name| text::starts_folded(name, word));
        let fits = fitting(run, word, String::as_str);
        let best = fits
            .iter()
            .map(|name| (worth(name), *name))
            .filter(|(value, _)| *value > 0.0)
            .max_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, name)| name.clone());
        let chosen = best.or_else(|| match fits.as_slice() {
            [only] => Some((*only).clone()),
            _ => Some(common_start(fits.iter().map(|name| name.as_str()))),
        })?;
        text::longer(&chosen, word.chars().count()).then_some(chosen)
    }

    /// The path that `word` finishes as, whole, as it is typed.
    fn finish_path(&mut self, word: &str, only_dirs: bool) -> Option<String> {
        let base = word.rsplit_once('/').map_or(word, |(_, base)| base);
        let lead = word.get(..word.len().checked_sub(base.len())?)?;
        let (dir, lead) = self.directory_of(lead)?;
        self.list(&dir);
        let entries = self.listed.get(&dir)?;
        let hidden = base.starts_with('.');
        let shown = entries
            .iter()
            .filter(|e| hidden || !e.name.starts_with('.'))
            .filter(|e| !only_dirs || e.dir);
        let fits = fitting(shown, base, |e| e.name.as_str());
        let chosen = match fits.as_slice() {
            [] => return None,
            [only] => only.finished(),
            many => {
                let common = common_start(many.iter().map(|e| e.name.as_str()));
                if text::longer(&common, base.chars().count()) {
                    common
                } else {
                    newest(&dir, many)?.finished()
                }
            }
        };
        let finished = format!("{lead}{}", escaped(&chosen)?);
        text::longer(&finished, word.chars().count()).then_some(finished)
    }

    /// The directory that `lead`, a word up to its last slash, points into,
    /// and `lead` with its names in their own case if they were not there
    /// as typed.
    fn directory_of(&mut self, lead: &str) -> Option<(PathBuf, String)> {
        let Some(dir_word) = lead.strip_suffix('/') else {
            return Some((self.resolve(".")?, String::new()));
        };
        if dir_word.is_empty() {
            return Some((PathBuf::from("/"), lead.to_owned()));
        }
        let dir = self.resolve(dir_word)?;
        if self.kind(dir.clone()) == Some(true) {
            return Some((dir, lead.to_owned()));
        }
        let fixed = self.put_right(dir_word)?;
        Some((self.resolve(&fixed)?, format!("{fixed}/")))
    }

    /// `dir`, a directory as typed, with each name in it that is not there
    /// put in the case of the one directory beside it that is the same but
    /// for that, as [`text::start`] has it; none if there is not just one.
    fn put_right(&mut self, dir: &str) -> Option<String> {
        let mut parts = dir.split('/');
        let (mut at, mut fixed) = match parts.clone().next()? {
            "" => (PathBuf::from("/"), Vec::from([String::new()])),
            "~" => (self.home.clone()?, Vec::from(["~".to_owned()])),
            first if first.starts_with('~') => return None,
            _ => (self.cwd.clone(), Vec::new()),
        };
        if !fixed.is_empty() {
            parts.next();
        }
        for part in parts.take(MAX_DEPTH) {
            let as_typed =
                matches!(part, "" | "." | "..") || self.kind(at.join(part)) == Some(true);
            let name = if as_typed {
                part.to_owned()
            } else {
                self.same_but_for_case(&at, part)?
            };
            at.push(&name);
            fixed.push(name);
        }
        Some(fixed.join("/"))
    }

    /// The one directory in `dir` whose name is `part` but for capitals
    /// where `part` has lower case letters.
    fn same_but_for_case(&mut self, dir: &Path, part: &str) -> Option<String> {
        self.list(dir);
        let chars = part.chars().count();
        let mut fits = self.listed.get(dir)?.iter().filter(|e| {
            e.dir && !text::longer(&e.name, chars) && text::start(&e.name, part) != Start::No
        });
        match (fits.next(), fits.next()) {
            (Some(one), None) => Some(one.name.clone()),
            _ => None,
        }
    }

    /// Read the entries of `dir`, once until the next prompt. What kind each
    /// is comes with the listing; only a symbolic link is followed.
    fn list(&mut self, dir: &Path) {
        if self.listed.contains_key(dir) {
            return;
        }
        let mut entries = Vec::new();
        if let Ok(read) = std::fs::read_dir(dir) {
            for entry in read.take(MAX_ENTRIES).flatten() {
                let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                let dir = match entry.file_type() {
                    Ok(kind) if kind.is_symlink() => entry.path().is_dir(),
                    Ok(kind) => kind.is_dir(),
                    Err(_) => false,
                };
                entries.push(Entry { name, dir });
            }
        }
        if self.listed.len() >= MAX_LISTED {
            self.listed.clear();
        }
        self.listed.insert(dir.to_path_buf(), entries);
    }

    /// The programs on the search path, gathered at most once a minute.
    fn gather_programs(&mut self) {
        let fresh = self
            .programs_at
            .is_some_and(|at| at.elapsed() < PROGRAMS_FRESH);
        if fresh {
            return;
        }
        let mut names = Vec::new();
        for dir in &self.search {
            let Ok(read) = std::fs::read_dir(dir) else {
                continue;
            };
            for entry in read.take(MAX_PROGRAMS).flatten() {
                if let Some(name) = entry.file_name().to_str()
                    && names.len() < MAX_PROGRAMS
                {
                    names.push(name.to_owned());
                }
            }
        }
        names.sort_unstable_by(|a, b| text::key_cmp(a, b));
        names.dedup();
        self.programs = names;
        self.programs_at = Some(Instant::now());
    }
}

/// Of `many` entries of `dir`, the one changed last; none when there are too
/// many to be worth asking about.
fn newest<'a>(dir: &Path, many: &[&'a Entry]) -> Option<&'a Entry> {
    if many.len() > MAX_COMPARED {
        return None;
    }
    many.iter()
        .max_by_key(|e| {
            std::fs::metadata(dir.join(&e.name))
                .and_then(|m| m.modified())
                .ok()
        })
        .copied()
}

/// The operators after which a command comes.
const OPERATORS: &[&str] = &["|", "||", "&&", ";", "&"];

/// The last word of `line`, the command it belongs to, and whether it is in
/// a command's place itself. None where the line is not simple enough to
/// tell: quotes, escapes, expansions, or a blank at the end.
fn last_word(line: &str) -> Option<(&str, Option<&str>, bool)> {
    if line.ends_with(' ') || line.contains(['\'', '"', '\\', '$', '`', '\t']) {
        return None;
    }
    let tokens: Vec<&str> = line.split(' ').filter(|t| !t.is_empty()).collect();
    let (&word, before) = tokens.split_last()?;
    let mut command = None;
    let mut expecting = true;
    for &token in before.iter().take(MAX_ENTRIES) {
        if OPERATORS.contains(&token) {
            expecting = true;
            command = None;
        } else if expecting && !PREFIXES.contains(&token) {
            command = Some(token);
            expecting = false;
        }
    }
    Some((word, command, expecting))
}

#[cfg(test)]
mod tests;
