//! The commands.

use std::ffi::{OsStr, OsString};
use std::os::unix::process::CommandExt;

use anyhow::{Context, Result, bail};

use inkling::doctor;
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
  inkling replay [--tune]    replay your history: how much typing the suggestions
                             save, against fish's order (prints numbers only)
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
        (Some("replay"), []) => replay(false, None),
        (Some("replay"), [flag]) if *flag == "--tune" => replay(true, None),
        (Some("replay"), [flag, spec]) if *flag == "--weights" => replay(false, spec.to_str()),
        (Some("run"), rest) => run_command(rest),
        (Some(session::COMMAND), rest) => start_session(rest),
        _ => bail!("unknown command; try `inkling help`"),
    }
}

fn init_shell(shell: &OsStr) -> Result<i32> {
    if shell != "bash" {
        bail!("only bash is supported so far, not {}", shell.display());
    }
    // The program names itself in the script by its full path.
    let exe = std::env::current_exe().context("find this program")?;
    out::text(&init::bash_for(&exe.to_string_lossy()))?;
    Ok(0)
}

fn doctor() -> Result<i32> {
    let report = doctor::examine(&history::Env::from_process());
    out::text(&report.to_string())?;
    Ok(i32::from(!report.all_well()))
}

/// Learn this share of the history, in hundredths, before typing the rest.
const LEARN: usize = 70;
/// With `--tune`, the weights are tuned on the history from this share on,
/// up to `LEARN`, and judged on what comes after.
const TUNE_FROM: usize = 10;

fn share_of(len: usize, hundredths: usize) -> usize {
    len.saturating_mul(hundredths).checked_div(100).unwrap_or(0)
}

/// Weights from `spec`, `name=value` pairs split by commas, over the
/// defaults: `fade`, `fade_here`, `project`, `directory`, `follows`, `failed`.
fn weights_from(spec: &str) -> Result<inkling::index::Weights> {
    let mut weights = inkling::index::Weights::default();
    for pair in spec.split(',').filter(|pair| !pair.is_empty()) {
        let Some((name, value)) = pair.split_once('=') else {
            bail!("{pair:?} is not name=value");
        };
        let value: f64 = value
            .parse()
            .with_context(|| format!("{value:?} is not a number"))?;
        let slot = match name {
            "fade" => &mut weights.fade,
            "fade_here" => &mut weights.fade_here,
            "project" => &mut weights.project,
            "directory" => &mut weights.directory,
            "follows" => &mut weights.follows,
            "failed" => &mut weights.failed,
            _ => bail!("no weight called {name:?}"),
        };
        *slot = value;
    }
    Ok(weights)
}

fn replay(tune: bool, spec: Option<&str>) -> Result<i32> {
    use inkling::index::{Order, Weights};
    let env = history::Env::from_process();
    let Some(source) = history::Source::discover(&env) else {
        bail!("no history found (neither Atuin's database nor the history file)");
    };
    let past = source.past().context("read the history")?;
    let mut look = inkling::paths::Look::default();
    let (learn, end) = (share_of(past.len(), LEARN), past.len());
    out::line(format_args!(
        "inkling replay: {} commands from {} ({})",
        past.len(),
        source.kind(),
        source.path().display()
    ))?;
    let mut weights = spec.map_or_else(|| Ok(Weights::default()), weights_from)?;
    if tune {
        let range = (share_of(past.len(), TUNE_FROM), learn);
        out::line(format_args!(
            "tuning on {} commands; this takes a while",
            range.1.saturating_sub(range.0)
        ))?;
        weights = inkling::replay::tune(&past, weights, range, &mut look).0;
        out::line(format_args!("tuned: {weights:?}"))?;
    }
    let orders = [Order::Recent, Order::RecentHereFirst, Order::Likely];
    let replayed = inkling::replay::replay(&past, weights, (learn, end), &orders, &mut look);
    report(&replayed.orders, &replayed.knowing)?;
    Ok(0)
}

fn report(scores: &[inkling::replay::Score], knowing: &inkling::replay::Score) -> Result<()> {
    let names = [
        "fish's order (most recent)",
        "inkling 0.1 (recent, this directory first)",
        "inkling (likely, by directory, project, sequence)",
    ];
    if let Some(first) = scores.first() {
        out::line(format_args!(
            "learned the first {LEARN}%, then typed the rest: {} commands, {} characters\n",
            first.commands, first.chars
        ))?;
    }
    out::line(format_args!(
        "{:52} {:>7} {:>22}",
        "", "typing", "the very command after"
    ))?;
    out::line(format_args!(
        "{:52} {:>7} {:>8}{:>7}{:>7}",
        "", "saved", "1 char", "2", "3"
    ))?;
    let knowing_name = "knowing every command run before (the most)";
    for (name, score) in names
        .iter()
        .chain([&knowing_name])
        .zip(scores.iter().chain([knowing]))
    {
        let per_cent = |share: f64| format!("{:.1}%", share * 100.0);
        out::line(format_args!(
            "{name:52} {:>7} {:>8}{:>7}{:>7}",
            per_cent(score.saved()),
            per_cent(score.right_after(1)),
            per_cent(score.right_after(2)),
            per_cent(score.right_after(3)),
        ))?;
    }
    Ok(())
}

/// What comes before the command in `run` and in the hidden session command.
struct Options {
    arg0: Option<OsString>,
    command: Vec<OsString>,
}

/// Read `--arg0 NAME`, then an optional `--`; the rest is the command.
fn options(mut rest: &[&OsStr]) -> Options {
    let mut found = Options {
        arg0: None,
        command: Vec::new(),
    };
    if let [flag, value, tail @ ..] = rest
        && *flag == "--arg0"
    {
        found.arg0 = Some((*value).to_owned());
        rest = tail;
    }
    if let [dashes, tail @ ..] = rest
        && *dashes == "--"
    {
        rest = tail;
    }
    found.command = rest.iter().map(|word| (*word).to_owned()).collect();
    found
}

fn run_command(rest: &[&OsStr]) -> Result<i32> {
    let Options { arg0, mut command } = options(rest);
    if command.is_empty() {
        command.push(std::env::var_os("SHELL").unwrap_or_else(|| "bash".into()));
    }
    if std::env::var_os("INKLING").is_some_and(|value| value == "0") {
        // Turned off: the program runs as if inkling were not there.
        return replace_with(arg0.as_deref(), &command);
    }
    let style = std::env::var("INKLING_STYLE")
        .ok()
        .and_then(|spec| Style::parse(&spec))
        .unwrap_or_else(Style::dim);
    let settings = Settings {
        command,
        arg0,
        environ: std::env::var("INKLING_ENVIRON").ok(),
        trace: std::env::var_os("INKLING_TRACE").map(Into::into),
        style,
        env: history::Env::from_process(),
    };
    match proxy::run(&settings) {
        // A terminal has to come up with a shell in it whatever becomes of
        // the suggestions: one that closes as it opens cannot even be used
        // to find out why. The command is run as if inkling were turned
        // off, and told so, so that its startup file does not try again.
        Err(inkling::Error::NotStarted(why)) => {
            out::note(format_args!("inkling: {why}; running without suggestions"))?;
            replace_plain(settings.arg0.as_deref(), &settings.command)
        }
        other => other.context("run in the proxy"),
    }
}

/// Become the program with inkling turned off for it.
fn replace_plain(name: Option<&OsStr>, command: &[OsString]) -> Result<i32> {
    let Some((program, tail)) = command.split_first() else {
        bail!("no command to run");
    };
    let mut process = std::process::Command::new(program);
    process.args(tail).env("INKLING", "0");
    if let Some(name) = name {
        process.arg0(name);
    }
    let failure = process.exec();
    Err(anyhow::Error::from(failure).context(format!("start {}", program.display())))
}

/// The hidden command: become the session leader and then the program.
fn start_session(rest: &[&OsStr]) -> Result<i32> {
    let Options { arg0, command, .. } = options(rest);
    let Some((program, tail)) = command.split_first() else {
        bail!("no command to run");
    };
    Err(session::enter(program, tail, arg0.as_deref()).into())
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
