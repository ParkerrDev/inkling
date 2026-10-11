//! The shell's side of the pseudo-terminal.
//!
//! A program needs a controlling terminal to do job control (Ctrl-Z, Ctrl-C
//! reaching the right program, `fg`). Making the pseudo-terminal the shell's
//! needs two calls between the fork and the exec, and the standard library
//! offers no safe place to put them. So the proxy starts itself again,
//! hidden, as `inkling __session -- COMMAND...`, with the pseudo-terminal as
//! its standard input, output and error; that process makes the calls and
//! becomes the command. The command keeps the process id the proxy started,
//! which is how the proxy knows the shell's process group.

use std::ffi::{OsStr, OsString};
use std::os::unix::process::CommandExt;
use std::process::Command;

use rustix::process::{getpid, ioctl_tiocsctty, setsid};
use rustix::stdio::stdin;

use crate::error::Error;

/// The hidden command the proxy starts itself with.
pub const COMMAND: &str = "__session";

/// What the shell is told about its place: the token on its marks and the
/// process id of the shell the proxy is following.
pub const NONCE_VARIABLE: &str = "INKLING_NONCE";
pub const SHELL_VARIABLE: &str = "INKLING_SHELL_PID";

/// Become the session leader of the terminal on standard input and then
/// `program`. Comes back only if that fails, with why.
#[must_use]
pub fn enter(program: &OsStr, rest: &[OsString], name: Option<&OsStr>) -> Error {
    if let Err(source) = setsid() {
        return Error::io("start a session", source);
    }
    if let Err(source) = ioctl_tiocsctty(stdin()) {
        return Error::io("take the terminal", source);
    }
    let mut command = Command::new(program);
    command.args(rest);
    if let Some(name) = name {
        command.arg0(name);
    }
    command.env(SHELL_VARIABLE, getpid().as_raw_nonzero().to_string());
    Error::Spawn {
        command: program.to_string_lossy().into_owned(),
        source: command.exec(),
    }
}
