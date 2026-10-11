//! The one way out: everything this program says goes through here.
//!
//! The library never prints. The commands hand what they have to say to
//! these few functions, which write it with `writeln!` on a locked handle,
//! so that a write that fails is an error to deal with. `println!` would
//! stop the program instead.
//!
//! A reader that has gone away (`inkling doctor | head -1`) is not a
//! failure, and there is no one left to tell of it: it is [`Error::Closed`],
//! which `main` takes as the end of the run.

use std::fmt::Display;
use std::io::{self, Write};

/// Why something could not be said.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Whoever was reading has closed the pipe.
    #[error("the output was closed")]
    Closed,
    #[error("failed printing to {stream}: {why}")]
    Failed {
        stream: &'static str,
        why: io::Error,
    },
}

fn checked(stream: &'static str, written: io::Result<()>) -> Result<(), Error> {
    match written {
        Ok(()) => Ok(()),
        Err(why) if why.kind() == io::ErrorKind::BrokenPipe => Err(Error::Closed),
        Err(why) => Err(Error::Failed { stream, why }),
    }
}

fn write_line(mut to: impl Write, stream: &'static str, text: impl Display) -> Result<(), Error> {
    checked(stream, writeln!(to, "{text}"))
}

fn write_text(mut to: impl Write, stream: &'static str, text: &str) -> Result<(), Error> {
    checked(stream, write!(to, "{text}"))
}

/// A line on standard output: an answer.
pub fn line(text: impl Display) -> Result<(), Error> {
    write_line(io::stdout().lock(), "stdout", text)
}

/// Text on standard output as it stands, line ends and all.
pub fn text(text: &str) -> Result<(), Error> {
    write_text(io::stdout().lock(), "stdout", text)
}

/// A line on standard error: what there is to say about an answer, or in
/// place of one.
pub fn note(text: impl Display) -> Result<(), Error> {
    write_line(io::stderr().lock(), "stderr", text)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pipe nobody reads from any more, or a disk with no room left.
    struct Fails(io::ErrorKind);

    impl Write for Fails {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(self.0.into())
        }
        fn flush(&mut self) -> io::Result<()> {
            Err(self.0.into())
        }
    }

    #[test]
    fn what_is_said_arrives_as_it_was_said() {
        let mut said = Vec::new();
        write_line(&mut said, "stdout", format_args!("{} apps", 2)).unwrap();
        write_text(&mut said, "stdout", "as it\nstands\n").unwrap();
        assert_eq!(said, b"2 apps\nas it\nstands\n");
    }

    #[test]
    fn a_closed_pipe_is_not_a_failure() {
        let result = write_line(Fails(io::ErrorKind::BrokenPipe), "stdout", "x");
        assert!(matches!(result, Err(Error::Closed)));
    }

    #[test]
    fn any_other_failure_is_one() {
        let result = write_line(Fails(io::ErrorKind::StorageFull), "stdout", "x");
        assert!(matches!(
            result,
            Err(Error::Failed {
                stream: "stdout",
                ..
            })
        ));
    }
}
