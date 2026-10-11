//! The shell's history file.
//!
//! One command to a line, oldest first. The file only gains lines until the
//! shell that owns it exits and rewrites it, so what is new is read from
//! where the last read stopped, and a file that has become shorter is read
//! again from the top. Lines that are the timestamps bash writes when
//! `HISTTIMEFORMAT` is set (`#` and digits) are not commands, and a line that
//! is not text is skipped, not an error.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::index::Index;

/// How much of the end of the file is read at most.
const MAX_BYTES: u64 = 32 << 20;

/// A history file being followed.
#[derive(Debug)]
pub struct HistFile {
    path: PathBuf,
    /// Where the last read stopped.
    offset: u64,
    /// The recency to give the next line: later lines are more recent.
    next: i64,
}

impl HistFile {
    /// A history file at `path`, not yet read.
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            offset: 0,
            next: 1,
        }
    }

    /// Where the file is.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Read the whole file (its last part, if it is huge) into `index`.
    ///
    /// # Errors
    ///
    /// When the file cannot be read.
    pub fn load(&mut self, index: &mut Index) -> Result<usize> {
        self.offset = 0;
        self.next = 1;
        self.read_from(index)
    }

    /// Read what has been added. Returns how many lines were taken in.
    ///
    /// # Errors
    ///
    /// When the file cannot be read.
    pub fn refresh(&mut self, index: &mut Index) -> Result<usize> {
        let length = std::fs::metadata(&self.path)
            .map_err(|source| Error::io("history file", source))?
            .len();
        if length == self.offset {
            return Ok(0);
        }
        if length < self.offset {
            // Rewritten shorter: start again.
            index.clear();
            return self.load(index);
        }
        self.read_from(index)
    }

    fn read_from(&mut self, index: &mut Index) -> Result<usize> {
        let mut file =
            File::open(&self.path).map_err(|source| Error::io("history file", source))?;
        let length = file
            .metadata()
            .map_err(|source| Error::io("history file", source))?
            .len();
        let skip_to = self.offset.max(length.saturating_sub(MAX_BYTES));
        file.seek(SeekFrom::Start(skip_to))
            .map_err(|source| Error::io("history file", source))?;
        let mut bytes = Vec::new();
        file.take(length.saturating_sub(skip_to))
            .read_to_end(&mut bytes)
            .map_err(|source| Error::io("history file", source))?;
        // A line still being written has no end yet; leave it for next time.
        let complete = bytes
            .iter()
            .rposition(|b| *b == b'\n')
            .map_or(0, |at| at.saturating_add(1));
        let mut lines = bytes.get(..complete).unwrap_or_default();
        if skip_to > self.offset && self.offset == 0 && skip_to > 0 {
            // Started in the middle of the file: the first line is a stump.
            let stump = lines
                .iter()
                .position(|b| *b == b'\n')
                .map_or(0, |at| at.saturating_add(1));
            lines = lines.get(stump..).unwrap_or_default();
        }
        self.offset = skip_to.saturating_add(u64::try_from(complete).unwrap_or(0));
        Ok(self.take_lines(lines, index))
    }

    fn take_lines(&mut self, lines: &[u8], index: &mut Index) -> usize {
        let mut taken: usize = 0;
        for line in lines.split(|b| *b == b'\n') {
            let Ok(text) = std::str::from_utf8(line) else {
                continue;
            };
            if is_timestamp(text) {
                continue;
            }
            if index.push(text, "", self.next).is_ok() {
                self.next = self.next.saturating_add(1);
                taken = taken.saturating_add(1);
            }
        }
        taken
    }
}

/// The comment line bash writes before a command when it records times.
fn is_timestamp(line: &str) -> bool {
    line.strip_prefix('#')
        .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write(path: &Path, text: &[u8]) {
        std::fs::write(path, text).unwrap();
    }

    fn append(path: &Path, text: &[u8]) {
        let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        file.write_all(text).unwrap();
    }

    #[test]
    fn lines_are_commands_and_later_is_more_recent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hist");
        write(&path, b"git add\ngit commit\nls\n");
        let mut source = HistFile::new(path);
        let mut index = Index::new();
        assert_eq!(source.load(&mut index).unwrap(), 3);
        index.commit();
        assert_eq!(index.suggest("git "), Some("git commit"));
    }

    #[test]
    fn timestamps_and_text_that_is_not_text_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hist");
        write(
            &path,
            b"#1700000000\necho a\n#1700000001\necho \xff\xfe bad\necho b\n",
        );
        let mut source = HistFile::new(path);
        let mut index = Index::new();
        assert_eq!(source.load(&mut index).unwrap(), 2);
        index.commit();
        assert_eq!(index.suggest("echo "), Some("echo b"));
        assert_eq!(index.len(), 2);
    }

    #[test]
    fn what_is_added_is_read_from_where_it_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hist");
        write(&path, b"one\n");
        let mut source = HistFile::new(path.clone());
        let mut index = Index::new();
        source.load(&mut index).unwrap();
        index.commit();
        assert_eq!(source.refresh(&mut index).unwrap(), 0);
        append(&path, b"onward\ntwo\n");
        assert_eq!(source.refresh(&mut index).unwrap(), 2);
        index.commit();
        assert_eq!(index.suggest("on"), Some("onward"));
        assert_eq!(index.len(), 3);
    }

    #[test]
    fn a_line_still_being_written_waits() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hist");
        write(&path, b"done\npart");
        let mut source = HistFile::new(path.clone());
        let mut index = Index::new();
        assert_eq!(source.load(&mut index).unwrap(), 1);
        append(&path, b"ial\n");
        assert_eq!(source.refresh(&mut index).unwrap(), 1);
        index.commit();
        assert_eq!(index.suggest("par"), Some("partial"));
    }

    #[test]
    fn a_file_rewritten_shorter_is_read_again() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hist");
        write(&path, b"first command\nsecond command\n");
        let mut source = HistFile::new(path.clone());
        let mut index = Index::new();
        source.load(&mut index).unwrap();
        index.commit();
        write(&path, b"fresh\n");
        source.refresh(&mut index).unwrap();
        index.commit();
        assert_eq!(index.suggest("fir"), None);
        assert_eq!(index.suggest("fre"), Some("fresh"));
    }

    #[test]
    fn a_missing_file_is_an_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let mut source = HistFile::new(dir.path().join("nothing"));
        assert!(source.load(&mut Index::new()).is_err());
    }
}
