//! The commands.

use std::ffi::{OsStr, OsString};
use std::os::unix::process::CommandExt;

use anyhow::{Context, Result, bail};

use inkling::history;
use inkling::init;
use inkling::proxy::{self, Settings};
use inkling::render::Style;
use inkling::session;

use crate::out;

const USAGE: &str = "\
inkling: fish-like inline suggestions for bash

usage:
  inkling init bash          print the shell integration
                             (put eval \"$(inkling init bash)\" in ~/.bashrc)
  inkling doctor             say what was found: the history, the shell, the terminal
  inkling run [COMMAND...]   run a command (default: your shell) with suggestions
  inkling version

INKLING=0 in the environment turns it off for that session.
";

/// Run the command in `words`. Returns the exit status.
pub fn run(words: &[OsString]) -> Result<i32> {
    let words: Vec<&OsStr> = words.iter().map(OsString::as_os_str).collect();
    let name = words.first().and_then(|word| word.to_str());
    match (name, words.get(1..).unwrap_or_default()) {
        (None, []) | (Some("help" | "--help" | "-h"), _) => {
            out::text(USAGE)?;
            Ok(0)
        }
        (Some("version" | "--version" | "-V"), []) => {
            out::line(format_args!("inkling {}", env!("CARGO_PKG_VERSION")))?;
            Ok(0)
        }
        (Some("init"), [shell]) => init_shell(shell),
        (Some("doctor"), []) => doctor(),
        (Some("run"), rest) => run_command(rest),
        (Some(session::COMMAND), rest) => start_session(rest),
        _ => bail!("unknown command; try `inkling help`"),
    }
}

fn init_shell(shell: &OsStr) -> Result<i32> {
    if shell != "bash" {
        bail!("only bash is supported so far, not {}", shell.display());
    }
    out::text(init::bash())?;
    Ok(0)
}

fn doctor() -> Result<i32> {
    out::line("doctor is not written yet")?;
    Ok(0)
}

/// Pull `--arg0 NAME` off the front, then an optional `--`.
fn split_name(mut rest: &[&OsStr]) -> (Option<OsString>, Vec<OsString>) {
    let mut name = None;
    if let [flag, given, tail @ ..] = rest
        && *flag == "--arg0"
    {
        name = Some((*given).to_owned());
        rest = tail;
    }
    if let [dashes, tail @ ..] = rest
        && *dashes == "--"
    {
        rest = tail;
    }
    (name, rest.iter().map(|word| (*word).to_owned()).collect())
}

fn run_command(rest: &[&OsStr]) -> Result<i32> {
    let (name, mut command) = split_name(rest);
    if command.is_empty() {
        command.push(std::env::var_os("SHELL").unwrap_or_else(|| "bash".into()));
    }
    if std::env::var_os("INKLING").is_some_and(|value| value == "0") {
        // Turned off: the program runs as if inkling were not there.
        return replace_with(name.as_deref(), &command);
    }
    let style = std::env::var("INKLING_STYLE")
        .ok()
        .and_then(|spec| Style::parse(&spec))
        .unwrap_or_else(Style::dim);
    let settings = Settings {
        command,
        arg0: name,
        style,
        env: history::Env::from_process(),
    };
    proxy::run(&settings).context("run in the proxy")
}

/// The hidden command: become the session leader and then the program.
fn start_session(rest: &[&OsStr]) -> Result<i32> {
    let (name, command) = split_name(rest);
    let Some((program, tail)) = command.split_first() else {
        bail!("no command to run");
    };
    Err(session::enter(program, tail, name.as_deref()).into())
}

/// Become the program, with nothing in between.
fn replace_with(name: Option<&OsStr>, command: &[OsString]) -> Result<i32> {
    let Some((program, tail)) = command.split_first() else {
        bail!("no command to run");
    };
    let mut process = std::process::Command::new(program);
    process.args(tail);
    if let Some(name) = name {
        process.arg0(name);
    }
    let failure = process.exec();
    Err(anyhow::Error::from(failure).context(format!("start {}", program.display())))
}
