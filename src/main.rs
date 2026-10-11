//! inkling: fish-like inline suggestions for bash.
//!
//! This is the program: a thin face over the core in `lib.rs`. The proxy
//! that wraps the shell, the shell integration it prints, and the doctor.
//! None of it writes to standard output or standard error itself: all of it
//! goes through `out`.

mod cli;
mod out;

use std::ffi::OsString;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    match cli::run(&args) {
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        // Whoever was reading has gone. There is nothing to add, and no one
        // to tell.
        Err(error) if matches!(error.downcast_ref(), Some(out::Error::Closed)) => ExitCode::SUCCESS,
        Err(error) => {
            // If even this cannot be said, the exit status still says it.
            let _ = out::note(format_args!("inkling: {error:#}"));
            ExitCode::FAILURE
        }
    }
}
