//! What inkling finds on this machine, and whether it will work.
//!
//! The doctor looks at the environment (the terminal, the shell, the
//! history), reads the history the way the proxy would and times it, and
//! checks the part that matters most by trying it: it starts a bash of its
//! own on a pseudo-terminal, with the integration, sends the probe, and times
//! how long readline takes to answer. Nothing of the person's is changed; the
//! only file made is a small script in the temporary directory, removed
//! after.

use std::fmt;
use std::os::fd::OwnedFd;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::io::Errno;
use rustix::stdio::{stdin, stdout};
use rustix::termios::{Winsize, isatty, tcgetattr, tcgetwinsize};

use crate::brain::Brain;
use crate::error::{Error, Result};
use crate::history::Env;
use crate::init::{self, PROBE_KEYS};
use crate::osc::{self, Mark};
use crate::pty;
use crate::quote;
use crate::session;

/// The markers install.sh puts around the block in `~/.bashrc`.
pub const BLOCK_START: &str = "# >>> inkling >>>";
pub const BLOCK_END: &str = "# <<< inkling <<<";

/// Everything the doctor found.
#[derive(Debug)]
pub struct Report {
    pub version: &'static str,
    pub executable: Option<PathBuf>,
    /// The process id of the proxy this was run under, if it was.
    pub proxy: Option<u32>,
    pub switched_off: bool,
    pub terminal: TerminalReport,
    pub bashrc: Bashrc,
    pub history: HistoryReport,
    pub shell: std::result::Result<ShellReport, String>,
}

#[derive(Debug)]
pub struct TerminalReport {
    pub name: Option<String>,
    pub is_terminal: bool,
    pub size: Option<(u16, u16)>,
}

/// Whether `~/.bashrc` starts inkling.
#[derive(Debug, PartialEq, Eq)]
pub enum Bashrc {
    Missing,
    WithoutBlock,
    WithBlock,
}

#[derive(Debug)]
pub struct HistoryReport {
    pub atuin: PathBuf,
    pub file: PathBuf,
    /// What will be used, and what it holds.
    pub used: Option<Used>,
    pub problem: Option<String>,
}

#[derive(Debug)]
pub struct Used {
    pub kind: &'static str,
    pub path: PathBuf,
    pub commands: usize,
    pub runs: usize,
    pub loaded_in: Duration,
    pub lookup: Duration,
}

#[derive(Debug)]
pub struct ShellReport {
    pub bash: String,
    pub round_trip: Duration,
}

/// Look.
#[must_use]
pub fn examine(env: &Env) -> Report {
    Report {
        version: env!("CARGO_PKG_VERSION"),
        executable: std::env::current_exe().ok(),
        proxy: find_proxy(),
        switched_off: std::env::var_os("INKLING").is_some_and(|v| v == "0"),
        terminal: terminal(),
        bashrc: bashrc(env),
        history: history(env),
        shell: shell_check().map_err(|error| error.to_string()),
    }
}

fn terminal() -> TerminalReport {
    let size = tcgetwinsize(stdout())
        .ok()
        .map(|Winsize { ws_row, ws_col, .. }| (ws_row, ws_col));
    TerminalReport {
        name: std::env::var("TERM").ok(),
        is_terminal: isatty(stdin()) && isatty(stdout()),
        size,
    }
}

fn bashrc(env: &Env) -> Bashrc {
    let Some(path) = env.home.as_ref().map(|home| home.join(".bashrc")) else {
        return Bashrc::Missing;
    };
    match std::fs::read_to_string(path) {
        Err(_) => Bashrc::Missing,
        Ok(text) if text.contains(BLOCK_START) => Bashrc::WithBlock,
        Ok(_) => Bashrc::WithoutBlock,
    }
}

/// The proxy among this process's ancestors, if there is one.
fn find_proxy() -> Option<u32> {
    let mut pid = std::process::id();
    // A process tree is not deeper than this.
    for _ in 0..64 {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        // "pid (name) state parent ...": the name may hold anything, so the
        // fields are counted from the last closing parenthesis.
        let parent: u32 = stat
            .rsplit_once(')')?
            .1
            .split_whitespace()
            .nth(1)?
            .parse()
            .ok()?;
        if parent <= 1 {
            return None;
        }
        if is_proxy(parent) {
            return Some(parent);
        }
        pid = parent;
    }
    None
}

fn is_proxy(pid: u32) -> bool {
    let Ok(cmdline) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
        return false;
    };
    let mut words = cmdline.split(|b| *b == 0);
    let program = words.next().unwrap_or_default();
    let name = program.rsplit(|b| *b == b'/').next().unwrap_or_default();
    name == b"inkling" && words.next() == Some(b"run".as_slice())
}

fn history(env: &Env) -> HistoryReport {
    let atuin = env.atuin_path().unwrap_or_default();
    let file = env.histfile_path().unwrap_or_default();
    let started = Instant::now();
    let brain = Brain::load(env);
    let loaded_in = started.elapsed();
    let Some(source) = brain.source() else {
        let problem = if atuin.is_file() {
            "Atuin's database is there but could not be read".to_owned()
        } else {
            "neither Atuin's database nor the history file exists".to_owned()
        };
        return HistoryReport {
            atuin,
            file,
            used: None,
            problem: Some(problem),
        };
    };
    let used = Used {
        kind: source.kind(),
        path: source.path().to_path_buf(),
        commands: brain.index().len(),
        runs: brain.index().rows(),
        loaded_in,
        lookup: lookup_time(&brain),
    };
    HistoryReport {
        atuin,
        file,
        used: Some(used),
        problem: None,
    }
}

/// How long a lookup takes, typically: the median of lookups of short
/// prefixes.
fn lookup_time(brain: &Brain) -> Duration {
    let prefixes = [
        "g", "gi", "git ", "ls", "cd ", "c", "cargo ", "sudo ", "echo ", "e",
    ];
    let mut times = Vec::with_capacity(prefixes.len().saturating_mul(100));
    for _ in 0..100 {
        for prefix in prefixes {
            let started = Instant::now();
            let found = brain.index().suggest(prefix);
            let took = started.elapsed();
            // Keeps the lookup from being optimised away.
            if found.is_some_and(str::is_empty) {
                return took;
            }
            times.push(took);
        }
    }
    times.sort_unstable();
    times.get(times.len() / 2).copied().unwrap_or_default()
}

/// Try the shell's half on a bash of our own.
fn shell_check() -> Result<ShellReport> {
    let nonce = "d0c70d0c70d0c70d";
    let script = std::env::temp_dir().join(format!("inkling-doctor-{}.sh", std::process::id()));
    let body = format!(
        "PS1='$ '\nunset PROMPT_COMMAND\nprintf 'bash %s\\n' \"$BASH_VERSION\" >&2\n{}",
        init::bash()
    );
    std::fs::write(&script, body).map_err(|e| Error::io("write the doctor's script", e))?;
    let outcome = run_bash(&script, nonce);
    // The script is the doctor's own; whether it can be removed is of no
    // consequence to the answer.
    let _removed = std::fs::remove_file(&script);
    outcome
}

fn run_bash(script: &std::path::Path, nonce: &str) -> Result<ShellReport> {
    let size = Winsize {
        ws_row: 24,
        ws_col: 80,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let pty = pty::open(size, &fresh_settings()?)?;
    let exe = std::env::current_exe().map_err(|e| Error::io("find this program", e))?;
    let copy = || {
        pty.slave
            .try_clone()
            .map(Stdio::from)
            .map_err(|e| Error::io("share the pseudo-terminal", e))
    };
    let mut command = Command::new(exe);
    command
        .arg(session::COMMAND)
        .arg("--")
        .args(["bash", "--noprofile", "--rcfile"])
        .arg(script)
        .arg("-i")
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", std::env::var_os("HOME").unwrap_or_default())
        .env("TERM", "xterm-256color")
        .env(session::NONCE_VARIABLE, nonce)
        .stdin(copy()?)
        .stdout(copy()?)
        .stderr(copy()?);
    let mut child = command.spawn().map_err(|source| Error::Spawn {
        command: "bash".to_owned(),
        source,
    })?;
    let outcome = converse(&pty.master, nonce);
    // The doctor's own bash is done with; whether it needed telling twice is
    // of no consequence to the answer.
    let _killed = child.kill();
    let _reaped = child.wait();
    outcome
}

/// Whatever settings a new pseudo-terminal has, to make another like it.
fn fresh_settings() -> Result<rustix::termios::Termios> {
    use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
    let sys = |what| move |source: Errno| Error::io(what, source);
    let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).map_err(sys("open a pty"))?;
    grantpt(&master).map_err(sys("grant a pty"))?;
    unlockpt(&master).map_err(sys("unlock a pty"))?;
    let name = ptsname(&master, Vec::new()).map_err(sys("name a pty"))?;
    let slave = rustix::fs::open(
        name.as_c_str(),
        rustix::fs::OFlags::RDWR | rustix::fs::OFlags::NOCTTY,
        rustix::fs::Mode::empty(),
    )
    .map_err(sys("open a pty"))?;
    tcgetattr(&slave).map_err(sys("read terminal settings"))
}

/// Wait for the prompt, send a key and the probe, and time the report.
fn converse(master: &OwnedFd, nonce: &str) -> Result<ShellReport> {
    let mut filter = osc::Filter::new(nonce);
    let mut seen = Seen::default();
    let limit = Duration::from_secs(10);
    pump(master, &mut filter, &mut seen, limit, |s| s.prompt)?;
    let mut keys = b"x".to_vec();
    keys.extend_from_slice(PROBE_KEYS);
    let started = Instant::now();
    write_all(master, &keys)?;
    pump(master, &mut filter, &mut seen, limit, |s| {
        s.report.is_some()
    })?;
    let round_trip = started.elapsed();
    let line = seen
        .report
        .as_deref()
        .and_then(parse_report)
        .ok_or_else(|| Error::io("read the probe's report", std::io::ErrorKind::InvalidData))?;
    if line != (b"x".to_vec(), 1, 1) {
        return Err(Error::io(
            "read the probe's report",
            std::io::Error::other(format!("the shell reported {line:?} for the line x")),
        ));
    }
    Ok(ShellReport {
        bash: seen.bash.unwrap_or_default(),
        round_trip,
    })
}

#[derive(Default)]
struct Seen {
    prompt: bool,
    report: Option<Vec<u8>>,
    bash: Option<String>,
    text: Vec<u8>,
}

/// Read from the shell until `done` says so.
fn pump(
    master: &OwnedFd,
    filter: &mut osc::Filter,
    seen: &mut Seen,
    limit: Duration,
    done: impl Fn(&Seen) -> bool,
) -> Result<()> {
    let started = Instant::now();
    let mut buffer = vec![0_u8; 4096];
    while !done(seen) {
        let Some(left) = limit.checked_sub(started.elapsed()) else {
            return Err(Error::io(
                "wait for the shell",
                std::io::Error::from(std::io::ErrorKind::TimedOut),
            ));
        };
        let mut fds = [PollFd::new(master, PollFlags::IN)];
        let timeout = Timespec {
            tv_sec: i64::try_from(left.as_secs()).unwrap_or(i64::MAX),
            tv_nsec: i64::from(left.subsec_nanos()),
        };
        match poll(&mut fds, Some(&timeout)) {
            Ok(_) | Err(Errno::INTR) => {}
            Err(source) => return Err(Error::io("wait for the shell", source)),
        }
        match rustix::io::read(master, buffer.as_mut_slice()) {
            Ok(count) => take(filter, buffer.get(..count).unwrap_or_default(), seen),
            Err(Errno::AGAIN | Errno::INTR) => {}
            Err(source) => return Err(Error::io("read from the shell", source)),
        }
    }
    Ok(())
}

fn take(filter: &mut osc::Filter, bytes: &[u8], seen: &mut Seen) {
    let mut clean = Vec::new();
    let mut found = Vec::new();
    filter.feed(bytes, &mut clean, &mut found);
    for item in found {
        match item.mark {
            Mark::Prompt { .. } => seen.prompt = true,
            Mark::Report { payload } => seen.report = Some(payload),
            Mark::Run | Mark::Garbled => {}
        }
    }
    seen.text.extend_from_slice(&clean);
    if seen.bash.is_none() {
        let text = String::from_utf8_lossy(&seen.text);
        seen.bash = text
            .lines()
            .find_map(|line| line.trim().strip_prefix("bash ").map(str::to_owned));
    }
}

/// The line, cursor and length of a report.
fn parse_report(payload: &[u8]) -> Option<(Vec<u8>, usize, usize)> {
    let mut fields = payload.splitn(3, |b| *b == b';');
    let number = |field: Option<&[u8]>| std::str::from_utf8(field?).ok()?.parse::<usize>().ok();
    let point = number(fields.next())?;
    let length = number(fields.next())?;
    let line = quote::unquote(fields.next()?).ok()?;
    Some((line, point, length))
}

fn write_all(master: &OwnedFd, mut bytes: &[u8]) -> Result<()> {
    for _ in 0..1000 {
        if bytes.is_empty() {
            return Ok(());
        }
        match rustix::io::write(master, bytes) {
            Ok(count) => bytes = bytes.get(count..).unwrap_or_default(),
            Err(Errno::AGAIN | Errno::INTR) => {}
            Err(source) => return Err(Error::io("type to the shell", source)),
        }
    }
    Err(Error::io(
        "type to the shell",
        std::io::Error::from(std::io::ErrorKind::TimedOut),
    ))
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "inkling {}", self.version)?;
        if let Some(path) = &self.executable {
            write!(f, " ({})", path.display())?;
        }
        writeln!(f)?;
        self.session(f)?;
        self.history(f)?;
        self.shell_line(f)?;
        self.verdict(f)
    }
}

impl Report {
    /// Whether everything needed is in place.
    #[must_use]
    pub fn all_well(&self) -> bool {
        self.shell.is_ok() && self.history.used.is_some() && self.bashrc != Bashrc::WithoutBlock
    }

    fn session(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let wrapped = match self.proxy {
            Some(pid) => format!("yes, under the proxy {pid}"),
            None => "no".to_owned(),
        };
        writeln!(f, "this shell is wrapped:   {wrapped}")?;
        writeln!(
            f,
            "INKLING=0:               {}",
            if self.switched_off {
                "yes, it is off"
            } else {
                "no"
            }
        )?;
        let term = self.terminal.name.as_deref().unwrap_or("not set");
        let size = self
            .terminal
            .size
            .map_or_else(String::new, |(rows, cols)| format!(", {cols}x{rows}"));
        let tty = if self.terminal.is_terminal {
            ""
        } else {
            ", not a terminal here"
        };
        writeln!(f, "terminal:                TERM={term}{size}{tty}")?;
        let rc = match self.bashrc {
            Bashrc::WithBlock => "starts inkling",
            Bashrc::WithoutBlock => "does not start inkling (run install.sh)",
            Bashrc::Missing => "not found",
        };
        writeln!(f, "~/.bashrc:               {rc}")
    }

    fn history(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let history = &self.history;
        match &history.used {
            Some(used) => {
                writeln!(
                    f,
                    "history:                 {} at {}",
                    used.kind,
                    used.path.display()
                )?;
                writeln!(
                    f,
                    "                         {} distinct commands from {} runs, read in {} ms",
                    used.commands,
                    used.runs,
                    used.loaded_in.as_millis()
                )?;
                writeln!(
                    f,
                    "                         a lookup takes {:.1} us",
                    used.lookup.as_secs_f64() * 1e6
                )
            }
            None => writeln!(
                f,
                "history:                 none: {}",
                history.problem.as_deref().unwrap_or("nothing found")
            ),
        }?;
        writeln!(
            f,
            "                         looked for {} and {}",
            history.atuin.display(),
            history.file.display()
        )
    }

    fn shell_line(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.shell {
            Ok(shell) => writeln!(
                f,
                "shell integration:       works with bash {}; the probe answers in {:.2} ms",
                shell.bash,
                shell.round_trip.as_secs_f64() * 1e3
            ),
            Err(why) => writeln!(f, "shell integration:       does not work: {why}"),
        }
    }

    fn verdict(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut problems = Vec::new();
        if self.shell.is_err() {
            problems.push("the shell integration does not work with this bash");
        }
        if self.history.used.is_none() {
            problems.push("there is no history to suggest from");
        }
        if self.bashrc == Bashrc::WithoutBlock {
            problems.push("~/.bashrc does not start inkling");
        }
        if problems.is_empty() {
            writeln!(f, "\neverything needed is in place")
        } else {
            writeln!(f, "\nwhat is wrong:")?;
            for problem in problems {
                writeln!(f, "  {problem}")?;
            }
            Ok(())
        }
    }
}
