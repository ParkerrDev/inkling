//! What the engine asks, and what keeps it informed: the history index, the
//! source it is filled from, and what the filesystem says.
//!
//! A suggestion is the likeliest command in the history that starts with the
//! line and can be used from here (its paths are there). When the history
//! has nothing of the kind, the last word is finished instead: a program's
//! name in a command's place, or a name in the directory the word points
//! into.

use std::borrow::Cow;
use std::cell::RefCell;
use std::path::{Path, PathBuf};

use crate::engine::Suggest;
use crate::history::{Env, Source};
use crate::index::{Index, Order, Run};
use crate::paths::Look;

/// The session name for commands learned from this shell itself.
const THIS_SESSION: &str = "inkling: this shell";

/// The history, loaded and kept up to date, and a look at the files.
#[derive(Debug)]
pub struct Brain {
    index: Index,
    source: Option<Source>,
    /// Remembers what it saw until the next prompt; asked while suggesting,
    /// which only reads the history.
    look: RefCell<Look>,
}

/// The directories programs are looked for in: the search path this was
/// started with, and the places a startup file usually adds to it later.
fn search_path(env: &Env) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default();
    if let Some(home) = &env.home {
        for extra in [".local/bin", ".cargo/bin", ".local/share/mise/shims", "bin"] {
            dirs.push(home.join(extra));
        }
    }
    dirs.dedup();
    dirs
}

impl Brain {
    /// Find the best history there is and read it.
    #[must_use]
    pub fn load(env: &Env) -> Self {
        let mut index = Index::new();
        let mut source = Source::discover(env);
        if let Some(found) = &mut source
            && found.load(&mut index).is_err()
        {
            source = None;
        }
        Self {
            index,
            source,
            look: RefCell::new(Look::new(env.home.clone(), search_path(env))),
        }
    }

    /// An empty brain with no source, which learns only what it sees.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            index: Index::new(),
            source: None,
            look: RefCell::new(Look::default()),
        }
    }

    /// The source of the commands, if there is one.
    #[must_use]
    pub fn source(&self) -> Option<&Source> {
        self.source.as_ref()
    }

    /// The index.
    #[must_use]
    pub fn index(&self) -> &Index {
        &self.index
    }

    /// A prompt is being drawn in `cwd`: commands from there and from its
    /// project come first, anything new in the history is read, and the
    /// files are looked at afresh.
    pub fn prompt(&mut self, cwd: Option<&Path>) {
        if let Some(source) = &mut self.source
            && source.refresh(&mut self.index).is_err()
        {
            // The source went away or broke: keep what is known.
            self.source = None;
        }
        let Some(cwd) = cwd else { return };
        let look = self.look.get_mut();
        look.prompt(cwd);
        let root = look.project_root(cwd);
        if let Some(dir) = cwd.to_str() {
            self.index.set_context(dir, root.as_deref());
        }
    }

    /// A command was run: `line`, if the line was known for certain. It is
    /// what comes before the next one. Atuin records its own runs; for a
    /// history file this is the only way a command run in this session is
    /// suggested before the shell exits and writes its history.
    pub fn ran(&mut self, line: Option<&str>, cwd: Option<&Path>) {
        let learns = !matches!(&self.source, Some(Source::Atuin(_)));
        if let (true, Some(line)) = (learns, line) {
            let run = Run {
                command: line,
                cwd: cwd.and_then(Path::to_str).unwrap_or(""),
                ok: true,
                session: THIS_SESSION,
            };
            if self.index.push(&run).is_ok() {
                self.index.commit();
            }
        }
        self.index.set_prev(line);
    }

    /// The likeliest command in the history for `line` that can be used
    /// from here.
    fn likeliest(&self, line: &str) -> Option<&str> {
        let best = self.index.ranked(line, Order::Likely);
        let mut look = self.look.try_borrow_mut().ok();
        best.ids()
            .map(|id| self.index.command(id))
            .find(|command| look.as_deref_mut().is_none_or(|look| look.usable(command)))
    }
}

impl Suggest for Brain {
    fn suggest(&self, line: &str) -> Option<Cow<'_, str>> {
        if let Some(command) = self.likeliest(line) {
            return Some(Cow::Borrowed(command));
        }
        let mut look = self.look.try_borrow_mut().ok()?;
        let finished = look.finish(line, &|word| self.index.word_worth(word))?;
        Some(Cow::Owned(finished))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_whose_paths_are_gone_gives_way_to_one_that_is_usable() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("there")).unwrap();
        let mut brain = Brain::empty();
        brain.ran(Some("cd there"), Some(dir.path()));
        brain.ran(Some("cd gone"), Some(dir.path()));
        brain.prompt(Some(dir.path()));
        assert_eq!(brain.suggest("cd ").as_deref(), Some("cd there"));
    }

    #[test]
    fn with_nothing_in_the_history_the_word_is_finished_from_the_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Makefile.am"), b"").unwrap();
        let mut brain = Brain::empty();
        brain.prompt(Some(dir.path()));
        assert_eq!(
            brain.suggest("vim Make").as_deref(),
            Some("vim Makefile.am")
        );
        assert_eq!(brain.suggest("vim Nothing"), None);
    }

    #[test]
    fn what_was_just_run_is_known_to_come_before() {
        let dir = tempfile::tempdir().unwrap();
        let mut brain = Brain::empty();
        for _ in 0..3 {
            for line in ["make", "make install", "make clean"] {
                brain.ran(Some(line), Some(dir.path()));
            }
        }
        brain.prompt(Some(dir.path()));
        brain.ran(Some("make"), Some(dir.path()));
        assert_eq!(brain.suggest("make ").as_deref(), Some("make install"));
    }
}
