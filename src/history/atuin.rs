//! Atuin's database, read and never written.
//!
//! The database is opened read-only, with `query_only` on as a second lock on
//! the door. It is in write-ahead mode, so reading it while Atuin writes
//! waits for nothing. What is read is the command, the directory, when, and
//! how it ended; deleted rows are left out, and so are commands that were not
//! found (exit status 127), which are typos and not worth suggesting again
//! (fish does not keep them either).

use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};

use crate::error::{Error, Result};
use crate::index::Index;

/// The most rows read in one go.
const MAX_ROWS: i64 = 1_000_000;
/// The exit status of a command that was not found.
const NOT_FOUND: i64 = 127;

/// An open Atuin database.
#[derive(Debug)]
pub struct Atuin {
    path: PathBuf,
    conn: Connection,
    /// Every row up to this one has been read.
    seen: i64,
    /// What `PRAGMA data_version` said last: it changes when another
    /// connection has written.
    version: i64,
}

impl Atuin {
    /// Open the database at `path` for reading.
    ///
    /// # Errors
    ///
    /// When it cannot be opened or has no history table.
    pub fn open(path: &Path) -> Result<Self> {
        let failed = |source| Error::Database {
            path: path.to_path_buf(),
            source,
        };
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(failed)?;
        conn.busy_timeout(Duration::from_millis(100))
            .map_err(failed)?;
        conn.pragma_update(None, "query_only", true)
            .map_err(failed)?;
        // The columns this reads, so that a database that lacks them is found
        // out now and not at the first prompt.
        conn.prepare(
            "SELECT rowid, command, cwd, timestamp, exit, deleted_at FROM history LIMIT 0",
        )
        .map_err(failed)?;
        let version = Self::data_version(&conn).map_err(failed)?;
        Ok(Self {
            path: path.to_path_buf(),
            conn,
            seen: 0,
            version,
        })
    }

    /// Where the database is.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn data_version(conn: &Connection) -> rusqlite::Result<i64> {
        conn.query_row("PRAGMA data_version", [], |row| row.get(0))
    }

    fn failed(&self, source: rusqlite::Error) -> Error {
        Error::Database {
            path: self.path.clone(),
            source,
        }
    }

    /// Read every command into `index`. Returns how many runs were read.
    ///
    /// # Errors
    ///
    /// When the database cannot be read.
    pub fn load(&mut self, index: &mut Index) -> Result<usize> {
        let newest: i64 = self
            .conn
            .query_row("SELECT coalesce(max(rowid), 0) FROM history", [], |row| {
                row.get(0)
            })
            .map_err(|source| self.failed(source))?;
        // Older than the cap allows are left out.
        self.seen = newest.saturating_sub(MAX_ROWS).max(0);
        self.read_new(index)
    }

    /// Read what has been written since the last read. Returns how many runs.
    ///
    /// # Errors
    ///
    /// When the database cannot be read.
    pub fn refresh(&mut self, index: &mut Index) -> Result<usize> {
        let version = Self::data_version(&self.conn).map_err(|source| self.failed(source))?;
        if version == self.version {
            return Ok(0);
        }
        self.version = version;
        self.read_new(index)
    }

    fn read_new(&mut self, index: &mut Index) -> Result<usize> {
        let mut statement = self
            .conn
            .prepare_cached(
                "SELECT rowid, command, cwd, timestamp, exit FROM history \
                 WHERE rowid > ?1 AND deleted_at IS NULL ORDER BY rowid LIMIT ?2",
            )
            .map_err(|source| self.failed(source))?;
        let mut rows = statement
            .query([self.seen, MAX_ROWS])
            .map_err(|source| self.failed(source))?;
        let mut read: usize = 0;
        let mut last = self.seen;
        for _ in 0..MAX_ROWS {
            let Some(row) = rows.next().map_err(|source| self.failed(source))? else {
                break;
            };
            read = read.saturating_add(1);
            last = row.get(0).unwrap_or(last);
            if row.get::<_, i64>(4).unwrap_or(0) == NOT_FOUND {
                continue;
            }
            // Text that is not text, or a row without its fields, is not a
            // command to suggest; the rest of the table is still good.
            let (Some(command), Some(cwd), Ok(when)) = (
                row.get_ref(1).ok().and_then(|value| value.as_str().ok()),
                row.get_ref(2).ok().and_then(|value| value.as_str().ok()),
                row.get::<_, i64>(3),
            ) else {
                continue;
            };
            let _ = index.push(command, cwd, when);
        }
        drop(rows);
        self.seen = last;
        Ok(read)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Atuin's own table (the columns that matter and the ones that do
    /// not), in write-ahead mode as Atuin keeps it.
    fn database(path: &Path) -> Connection {
        let conn = Connection::open(path).unwrap();
        conn.pragma_update(None, "journal_mode", "wal").unwrap();
        conn.execute_batch(
            "CREATE TABLE history (
                id text primary key, timestamp integer not null, duration integer not null,
                exit integer not null, command text not null, cwd text not null,
                session text not null, hostname text not null, deleted_at integer,
                author text, intent text, shell text, author_kind text);
             CREATE INDEX idx_history_timestamp ON history(timestamp);",
        )
        .unwrap();
        conn
    }

    fn add(conn: &Connection, n: i64, command: &str, cwd: &str, exit: i64) {
        conn.execute(
            "INSERT INTO history (id, timestamp, duration, exit, command, cwd, session, hostname)
             VALUES (?1, ?2, 1, ?3, ?4, ?5, 's', 'h')",
            rusqlite::params![format!("id{n}"), n, exit, command, cwd],
        )
        .unwrap();
    }

    #[test]
    fn it_reads_what_is_there() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.db");
        let conn = database(&path);
        add(&conn, 1, "git status", "/p", 0);
        add(&conn, 2, "git stash", "/p", 0);
        add(&conn, 3, "cargo build", "/q", 1);
        let mut atuin = Atuin::open(&path).unwrap();
        let mut index = Index::new();
        assert_eq!(atuin.load(&mut index).unwrap(), 3);
        index.commit();
        assert_eq!(index.suggest("git s"), Some("git stash"));
        assert_eq!(index.suggest("cargo"), Some("cargo build"));
        index.set_cwd("/p");
        assert_eq!(index.suggest("g"), Some("git stash"));
    }

    #[test]
    fn deleted_rows_and_commands_not_found_are_left_out() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.db");
        let conn = database(&path);
        add(&conn, 1, "ls -la", "/p", 0);
        add(&conn, 2, "lss", "/p", 127);
        add(&conn, 3, "ls secret", "/p", 0);
        conn.execute("UPDATE history SET deleted_at = 9 WHERE id = 'id3'", [])
            .unwrap();
        let mut atuin = Atuin::open(&path).unwrap();
        let mut index = Index::new();
        atuin.load(&mut index).unwrap();
        index.commit();
        assert_eq!(index.suggest("ls"), Some("ls -la"));
        assert_eq!(index.len(), 1);
    }

    #[test]
    fn text_that_is_not_text_is_skipped_and_the_rest_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.db");
        let conn = database(&path);
        add(&conn, 1, "echo before", "/p", 0);
        conn.execute(
            "INSERT INTO history (id, timestamp, duration, exit, command, cwd, session, hostname)
             VALUES ('bad', 2, 1, 0, CAST(x'6563686f20fffe' AS TEXT), '/p', 's', 'h')",
            [],
        )
        .unwrap();
        add(&conn, 3, "echo after", "/p", 0);
        let mut atuin = Atuin::open(&path).unwrap();
        let mut index = Index::new();
        atuin.load(&mut index).unwrap();
        index.commit();
        assert_eq!(index.len(), 2);
        assert_eq!(index.suggest("echo "), Some("echo after"));
    }

    #[test]
    fn a_refresh_reads_only_what_is_new() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.db");
        let conn = database(&path);
        add(&conn, 1, "one", "/p", 0);
        let mut atuin = Atuin::open(&path).unwrap();
        let mut index = Index::new();
        atuin.load(&mut index).unwrap();
        assert_eq!(atuin.refresh(&mut index).unwrap(), 0);
        add(&conn, 2, "onward", "/p", 0);
        add(&conn, 3, "other", "/p", 0);
        assert_eq!(atuin.refresh(&mut index).unwrap(), 2);
        index.commit();
        assert_eq!(index.suggest("on"), Some("onward"));
        assert_eq!(atuin.refresh(&mut index).unwrap(), 0);
    }

    #[test]
    fn a_database_without_the_table_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("other.db");
        Connection::open(&path)
            .unwrap()
            .execute_batch("CREATE TABLE other (x)")
            .unwrap();
        assert!(Atuin::open(&path).is_err());
    }

    #[test]
    fn it_cannot_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.db");
        let conn = database(&path);
        add(&conn, 1, "one", "/p", 0);
        let atuin = Atuin::open(&path).unwrap();
        assert!(atuin.conn.execute("DELETE FROM history", []).is_err());
        let count: i64 = conn
            .query_row("SELECT count(*) FROM history", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }
}
