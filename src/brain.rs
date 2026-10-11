//! What the engine asks, and what keeps it informed: the history index and
//! the source it is filled from.

use std::path::Path;

use crate::engine::Suggest;
use crate::history::{Env, Source};
use crate::index::Index;

/// The history, loaded and kept up to date.
#[derive(Debug)]
pub struct Brain {
    index: Index,
    source: Option<Source>,
    /// The recency for the next command learned in this session, which is
    /// later than anything read from a file.
    session: i64,
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
            session: i64::MAX / 2,
        }
    }

    /// An empty brain with no source, which learns only what it sees.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            index: Index::new(),
            source: None,
            session: i64::MAX / 2,
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

    /// A prompt is being drawn in `cwd`: commands from there come first, and
    /// anything new in the history is read.
    pub fn prompt(&mut self, cwd: Option<&Path>) {
        if let Some(source) = &mut self.source
            && source.refresh(&mut self.index).is_err()
        {
            // The source went away or broke: keep what is known.
            self.source = None;
        }
        if let Some(cwd) = cwd.and_then(Path::to_str) {
            self.index.set_cwd(cwd);
        }
    }

    /// A command was run: `line`, if the line was known for certain. Atuin
    /// records its own; for the others this is the only way a command run in
    /// this session is suggested before the shell exits and writes its
    /// history.
    pub fn ran(&mut self, line: Option<&str>, cwd: Option<&Path>) {
        let learns = !matches!(&self.source, Some(Source::Atuin(_)));
        let (true, Some(line)) = (learns, line) else {
            return;
        };
        self.session = self.session.saturating_add(1);
        let cwd = cwd.and_then(Path::to_str).unwrap_or("");
        if self.index.push(line, cwd, self.session).is_ok() {
            self.index.commit();
        }
    }
}

impl Suggest for Brain {
    fn suggest(&self, line: &str) -> Option<&str> {
        self.index.suggest(line)
    }
}
