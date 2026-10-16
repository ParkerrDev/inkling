//! Where the commands come from.
//!
//! Atuin's database when there is one: it knows the directory every command
//! ran in and how it ended. Otherwise the shell's own history file.
//!
//! A source is read whole when the session starts and again, for what is new,
//! at each prompt. The index it fills is what is asked while typing, so no
//! keystroke ever waits on a file or a database.

pub mod atuin;
pub mod file;

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::index::Index;

/// What decides where history is, read once.
#[derive(Clone, Debug, Default)]
pub struct Env {
    pub home: Option<PathBuf>,
    pub xdg_data_home: Option<PathBuf>,
    /// Atuin's `ATUIN_DB_PATH`.
    pub atuin_db: Option<PathBuf>,
    /// The shell's history file: the one the shell told inkling about, else
    /// `HISTFILE`.
    pub histfile: Option<PathBuf>,
}

fn absolute(value: Option<OsString>) -> Option<PathBuf> {
    let path = PathBuf::from(value?);
    path.is_absolute().then_some(path)
}

impl Env {
    /// The process's own environment.
    #[must_use]
    pub fn from_process() -> Self {
        let var = |name: &str| std::env::var_os(name);
        Self {
            home: absolute(var("HOME")),
            xdg_data_home: absolute(var("XDG_DATA_HOME")),
            atuin_db: absolute(var("ATUIN_DB_PATH")),
            histfile: absolute(var("INKLING_HISTFILE").filter(|v| !v.is_empty()))
                .or_else(|| absolute(var("HISTFILE"))),
        }
    }

    /// Atuin's database, if the environment points at one.
    #[must_use]
    pub fn atuin_path(&self) -> Option<PathBuf> {
        if let Some(path) = &self.atuin_db {
            return Some(path.clone());
        }
        let data = self
            .xdg_data_home
            .clone()
            .or_else(|| self.home.as_ref().map(|home| home.join(".local/share")))?;
        Some(data.join("atuin/history.db"))
    }

    /// The history file bash writes.
    #[must_use]
    pub fn histfile_path(&self) -> Option<PathBuf> {
        self.histfile
            .clone()
            .or_else(|| self.home.as_ref().map(|home| home.join(".bash_history")))
    }
}

/// One command from the past, owned, as the replay reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Past {
    pub command: String,
    pub cwd: String,
    pub session: String,
    pub ok: bool,
}

/// A source of commands.
#[derive(Debug)]
pub enum Source {
    Atuin(atuin::Atuin),
    File(file::HistFile),
}

impl Source {
    /// The best source there is: Atuin's database if it exists and opens,
    /// else the history file if it exists.
    #[must_use]
    pub fn discover(env: &Env) -> Option<Self> {
        if let Some(path) = env.atuin_path()
            && path.is_file()
            && let Ok(atuin) = atuin::Atuin::open(&path)
        {
            return Some(Self::Atuin(atuin));
        }
        let path = env.histfile_path()?;
        path.is_file()
            .then(|| Self::File(file::HistFile::new(path)))
    }

    /// A word for what this is.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Atuin(_) => "atuin",
            Self::File(_) => "history file",
        }
    }

    /// Where it is.
    #[must_use]
    pub fn path(&self) -> &Path {
        match self {
            Self::Atuin(source) => source.path(),
            Self::File(source) => source.path(),
        }
    }

    /// Read everything into `index`. Returns how many runs were read.
    ///
    /// # Errors
    ///
    /// When the source cannot be read.
    pub fn load(&mut self, index: &mut Index) -> Result<usize> {
        let read = match self {
            Self::Atuin(source) => source.load(index)?,
            Self::File(source) => source.load(index)?,
        };
        index.commit();
        Ok(read)
    }

    /// Every command, oldest first, for a replay.
    ///
    /// # Errors
    ///
    /// When the source cannot be read.
    pub fn past(&self) -> Result<Vec<Past>> {
        match self {
            Self::Atuin(source) => source.past(),
            Self::File(source) => source.past(),
        }
    }

    /// Read what is new since the last time. Returns how many runs.
    ///
    /// # Errors
    ///
    /// When the source cannot be read.
    pub fn refresh(&mut self, index: &mut Index) -> Result<usize> {
        let read = match self {
            Self::Atuin(source) => source.refresh(index)?,
            Self::File(source) => source.refresh(index)?,
        };
        index.commit();
        Ok(read)
    }
}
