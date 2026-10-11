//! A pseudo-terminal of our own, for the shell to live in.

use std::os::fd::OwnedFd;

use rustix::fs::{Mode, OFlags};
use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
use rustix::termios::{OptionalActions, Termios, Winsize, tcsetattr, tcsetwinsize};

use crate::error::{Error, Result};

/// Both ends of a pseudo-terminal.
#[derive(Debug)]
pub struct Pty {
    /// Our end: what is written here is typed, what is read is the shell's
    /// output.
    pub master: OwnedFd,
    /// The shell's end, to be its standard input, output and error.
    pub slave: OwnedFd,
}

/// Open a pseudo-terminal, set up like the real one: the same size and the
/// same settings, so that the shell finds the terminal as the person has it.
///
/// # Errors
///
/// When the system will not make one.
pub fn open(size: Winsize, settings: &Termios) -> Result<Pty> {
    let sys = |what| move |source: rustix::io::Errno| Error::io(what, source);
    let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC)
        .map_err(sys("open a pseudo-terminal"))?;
    grantpt(&master).map_err(sys("grant the pseudo-terminal"))?;
    unlockpt(&master).map_err(sys("unlock the pseudo-terminal"))?;
    let name = ptsname(&master, Vec::new()).map_err(sys("name the pseudo-terminal"))?;
    let slave = rustix::fs::open(
        name.as_c_str(),
        OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(sys("open the shell's end of the pseudo-terminal"))?;
    tcsetattr(&slave, OptionalActions::Now, settings).map_err(sys("set up the pseudo-terminal"))?;
    tcsetwinsize(&slave, size).map_err(sys("size the pseudo-terminal"))?;
    Ok(Pty { master, slave })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustix::termios::{LocalModes, tcgetattr, tcgetwinsize};

    #[test]
    fn a_pty_has_the_size_and_settings_it_was_given() {
        let size = Winsize {
            ws_row: 31,
            ws_col: 97,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let mut settings = tcgetattr(open(size, &blank()).unwrap().slave).unwrap();
        settings.local_modes.remove(LocalModes::ECHO);
        let pty = open(size, &settings).unwrap();
        let got = tcgetwinsize(&pty.master).unwrap();
        assert_eq!((got.ws_row, got.ws_col), (31, 97));
        let modes = tcgetattr(&pty.master).unwrap();
        assert!(!modes.local_modes.contains(LocalModes::ECHO));
    }

    /// Whatever a fresh pty starts with.
    fn blank() -> Termios {
        let size = Winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).unwrap();
        grantpt(&master).unwrap();
        unlockpt(&master).unwrap();
        let name = ptsname(&master, Vec::new()).unwrap();
        let slave = rustix::fs::open(
            name.as_c_str(),
            OFlags::RDWR | OFlags::NOCTTY,
            Mode::empty(),
        )
        .unwrap();
        tcsetwinsize(&slave, size).unwrap();
        tcgetattr(&slave).unwrap()
    }
}
