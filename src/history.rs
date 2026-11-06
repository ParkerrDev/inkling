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
    /// Atuin's `ATUIN_HOME`.
    pub atuin_home: Option<PathBuf>,
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
            atuin_home: absolute(var("ATUIN_HOME")),
            histfile: absolute(var("INKLING_HISTFILE").filter(|v| !v.is_empty()))
                .or_else(|| absolute(var("HISTFILE"))),
        }
    }

    /// Atuin's database, where Atuin 18 looks for it: `ATUIN_DB_PATH`;
    /// else in `ATUIN_HOME`; else in `~/.atuin/data` if that is there (a
    /// `~/.atuin` with only Atuin's logs in it does not count); else where
    /// Atuin kept it before (`XDG_DATA_HOME`, `~/.local/share`) if that is
    /// there; else in `~/.atuin/data`, where a new one is made.
    #[must_use]
    pub fn atuin_path(&self) -> Option<PathBuf> {
        if let Some(path) = &self.atuin_db {
            return Some(path.clone());
        }
        if let Some(home) = &self.atuin_home {
            return Some(home.join("data/history.db"));
        }
        let home = self.home.as_ref()?;
        let own = home.join(".atuin/data");
        let shared = home.join(".local/share");
        let data = self.xdg_data_home.as_ref().unwrap_or(&shared);
        let was = [&shared, data].iter().any(|dir| dir.join("atuin").is_dir());
        if was && !own.is_dir() {
            return Some(data.join("atuin/history.db"));
        }
        Some(own.join("history.db"))
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

#[cfg(test)]
mod tests {
    use super::Env;
    use std::path::{Path, PathBuf};

    fn env(home: &Path) -> Env {
        Env {
            home: Some(home.to_path_buf()),
            ..Env::default()
        }
    }

    fn mkdir(path: PathBuf) {
        std::fs::create_dir_all(path).unwrap();
    }

    #[test]
    fn atuins_database_is_found_where_atuin_18_keeps_it() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let new = home.join(".atuin/data/history.db");
        assert_eq!(env(home).atuin_path(), Some(new.clone()), "a new home");
        mkdir(home.join(".local/share/atuin"));
        let old = home.join(".local/share/atuin/history.db");
        assert_eq!(env(home).atuin_path(), Some(old), "kept from before");
        let xdg = Env {
            xdg_data_home: Some(home.join("xd")),
            ..env(home)
        };
        let moved = home.join("xd/atuin/history.db");
        assert_eq!(xdg.atuin_path(), Some(moved), "kept from before, moved");
        mkdir(home.join(".atuin/logs"));
        let kept = env(home).atuin_path();
        assert_eq!(kept, Some(home.join(".local/share/atuin/history.db")));
        mkdir(home.join(".atuin/data"));
        assert_eq!(env(home).atuin_path(), Some(new), "its own over before");
    }

    #[test]
    fn atuins_own_variables_come_first() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        mkdir(home.join(".local/share/atuin"));
        let given = Env {
            atuin_home: Some(home.join("ah")),
            ..env(home)
        };
        let data = home.join("ah/data/history.db");
        assert_eq!(given.atuin_path(), Some(data));
        let db = Env {
            atuin_db: Some(home.join("db")),
            ..given
        };
        assert_eq!(db.atuin_path(), Some(home.join("db")));
    }

    #[test]
    fn a_data_home_where_atuin_kept_it_before_is_found_alone() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        mkdir(home.join("xd/atuin"));
        let xdg = Env {
            xdg_data_home: Some(home.join("xd")),
            ..env(home)
        };
        assert_eq!(xdg.atuin_path(), Some(home.join("xd/atuin/history.db")));
    }
}
