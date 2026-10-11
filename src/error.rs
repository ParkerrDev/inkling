//! What can go wrong, in one place.
//!
//! The library answers in these; the program at the edge turns them into a
//! message with `anyhow`.

use std::io;
use std::path::PathBuf;

/// Why something inkling does could not be done.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A system call, or a file, said no.
    #[error("{what}: {source}")]
    Io {
        what: &'static str,
        #[source]
        source: io::Error,
    },
    /// The history database could not be read.
    #[error("history database {path}: {source}")]
    Database {
        path: PathBuf,
        #[source]
        source: rusqlite::Error,
    },
    /// The command to run in the proxy was not given.
    #[error("no command to run")]
    NoCommand,
    /// A command could not be started.
    #[error("cannot start {command}: {source}")]
    Spawn {
        command: String,
        #[source]
        source: io::Error,
    },
    /// The terminal is not a terminal.
    #[error("{0} is not a terminal")]
    NotATerminal(&'static str),
}

impl Error {
    /// Wrap an I/O failure with what was being attempted.
    #[must_use]
    pub fn io(what: &'static str, source: impl Into<io::Error>) -> Self {
        Self::Io {
            what,
            source: source.into(),
        }
    }
}

/// The library's result type.
pub type Result<T> = std::result::Result<T, Error>;
