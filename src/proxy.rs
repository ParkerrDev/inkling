//! The proxy: a pseudo-terminal in the middle, and a loop that carries bytes
//! between the person's terminal and the shell.
//!
//! Everything passes in both directions as it comes. The terminal is put in
//! raw mode so that no key is interpreted on the way in; the pseudo-terminal
//! takes over the job of a terminal for the shell, with the real one's
//! settings and size. A resize reaches the shell's size, and so its programs.
//! The loop knows nothing of suggestions: the [`Engine`] decides what to do
//! with each chunk, and this carries it out.

use std::ffi::OsString;
use std::io::{self, Read};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
use rustix::io::Errno;
use rustix::process::Pid;
use rustix::stdio::{stdin, stdout};
use rustix::termios::{
    LocalModes, OptionalActions, Termios, Winsize, isatty, tcgetattr, tcgetpgrp, tcgetwinsize,
    tcsetattr, tcsetwinsize,
};
use signal_hook::consts::{SIGCHLD, SIGHUP, SIGINT, SIGQUIT, SIGTERM, SIGWINCH};

use crate::brain::Brain;
use crate::engine::{Engine, Event, Out, Tty};
use crate::error::{Error, Result};
use crate::history;
use crate::pty::{self, Pty};
use crate::render::Style;
use crate::session;

/// How much is read from either side at a time.
const CHUNK: usize = 64 * 1024;
/// How much input may wait for the shell before the person's keys are left
/// unread.
const QUEUE_LIMIT: usize = 1 << 20;
/// How long the shell is given to leave after the terminal is gone: this
/// many turns of this long, two seconds in all.
const REAP_TURNS: u32 = 400;
const REAP_STEP: Duration = Duration::from_millis(5);

/// What to run, and how.
#[derive(Debug)]
pub struct Settings {
    /// The program and its arguments.
    pub command: Vec<OsString>,
    /// What the program is told its name is, if not the program's own.
    pub arg0: Option<OsString>,
    pub style: Style,
    pub env: history::Env,
}

/// Run `settings.command` in a pseudo-terminal until it exits, with
/// suggestions. Returns its exit status, as a shell reports it.
///
/// # Errors
///
/// When the terminal is not one, or the system will not make the
/// pseudo-terminal or start the program.
pub fn run(settings: &Settings) -> Result<i32> {
    let (settings_of_terminal, size) = capture_terminal()?;
    let pty = pty::open(size, &settings_of_terminal)?;
    let nonce = nonce()?;
    let child = spawn(settings, &pty, &nonce)?;
    let Pty { master, slave } = pty;
    // The shell has the other end now; this one is not needed, and keeping
    // it would keep the pseudo-terminal open after the shell is gone.
    drop(slave);
    let nonblocking =
        fcntl_getfl(&master).map_err(|e| Error::io("read master flags", e))? | OFlags::NONBLOCK;
    fcntl_setfl(&master, nonblocking).map_err(|e| Error::io("set master flags", e))?;
    let signals = Signals::install()?;
    // The shell starts while the history is read.
    let brain = Brain::load(&settings.env);
    let columns = usize::from(size.ws_col);
    let engine = Engine::new(&nonce, columns, settings.style.clone(), brain);
    let _raw = RawMode::enter(&settings_of_terminal)?;
    let mut proxy = Proxy::new(master, child, engine, signals);
    proxy.run()
}

/// The real terminal's settings and size.
fn capture_terminal() -> Result<(Termios, Winsize)> {
    if !isatty(stdin()) {
        return Err(Error::NotATerminal("standard input"));
    }
    if !isatty(stdout()) {
        return Err(Error::NotATerminal("standard output"));
    }
    let settings = tcgetattr(stdin()).map_err(|e| Error::io("read the terminal's settings", e))?;
    let mut size = tcgetwinsize(stdin()).map_err(|e| Error::io("read the terminal's size", e))?;
    if size.ws_col == 0 || size.ws_row == 0 {
        size.ws_col = 80;
        size.ws_row = 24;
    }
    Ok((settings, size))
}

const HEX: &[u8; 16] = b"0123456789abcdef";

/// Sixteen hex digits nobody else knows.
fn nonce() -> Result<String> {
    let mut bytes = [0_u8; 8];
    rustix::rand::getrandom(&mut bytes, rustix::rand::GetRandomFlags::empty())
        .map_err(|e| Error::io("get random bytes", e))?;
    let digit =
        |nibble: u8| char::from(HEX.get(usize::from(nibble & 0xf)).copied().unwrap_or(b'0'));
    let mut text = String::with_capacity(16);
    for byte in bytes {
        text.push(digit(byte >> 4));
        text.push(digit(byte));
    }
    Ok(text)
}

/// Start the program on the pseudo-terminal, by way of the hidden session
/// command of this same program.
fn spawn(settings: &Settings, pty: &Pty, nonce: &str) -> Result<Child> {
    let exe = std::env::current_exe().map_err(|e| Error::io("find this program", e))?;
    let copy = |what| {
        pty.slave
            .try_clone()
            .map(Stdio::from)
            .map_err(|e| Error::io(what, e))
    };
    let mut command = Command::new(exe);
    command.arg(session::COMMAND);
    if let Some(name) = &settings.arg0 {
        command.arg("--arg0").arg(name);
    }
    command
        .arg("--")
        .args(&settings.command)
        .env(session::NONCE_VARIABLE, nonce)
        .env_remove(session::SHELL_VARIABLE)
        .stdin(copy("share the pseudo-terminal")?)
        .stdout(copy("share the pseudo-terminal")?)
        .stderr(copy("share the pseudo-terminal")?);
    command.spawn().map_err(|source| Error::Spawn {
        command: settings
            .command
            .first()
            .map(|c| c.to_string_lossy().into_owned())
            .unwrap_or_default(),
        source,
    })
}

/// The person's terminal in raw mode for as long as this lives.
struct RawMode {
    saved: Termios,
}

impl RawMode {
    fn enter(saved: &Termios) -> Result<Self> {
        let mut raw = saved.clone();
        raw.make_raw();
        tcsetattr(stdin(), OptionalActions::Now, &raw)
            .map_err(|e| Error::io("put the terminal in raw mode", e))?;
        Ok(Self {
            saved: saved.clone(),
        })
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        // Nothing can be done about a terminal that will not take its
        // settings back, and no one to tell.
        let _restored = tcsetattr(stdin(), OptionalActions::Drain, &self.saved);
    }
}

/// The signals this cares about, each as a byte on a pipe that can be polled.
struct Signals {
    winch: UnixStream,
    child: UnixStream,
    end: UnixStream,
}

impl Signals {
    fn install() -> Result<Self> {
        let pipe = |signals: &[i32]| -> io::Result<UnixStream> {
            let (read, write) = UnixStream::pair()?;
            read.set_nonblocking(true)?;
            write.set_nonblocking(true)?;
            for signal in signals {
                signal_hook::low_level::pipe::register(*signal, write.try_clone()?)?;
            }
            Ok(read)
        };
        let failed = |e| Error::io("watch for signals", e);
        Ok(Self {
            winch: pipe(&[SIGWINCH]).map_err(failed)?,
            child: pipe(&[SIGCHLD]).map_err(failed)?,
            end: pipe(&[SIGTERM, SIGHUP, SIGINT, SIGQUIT]).map_err(failed)?,
        })
    }
}

/// Read what is waiting on a signal pipe, to clear it.
fn drain(mut pipe: &UnixStream) {
    let mut sink = [0_u8; 64];
    // Signals come a byte at a time; a few reads empty it.
    for _ in 0..16 {
        match pipe.read(&mut sink) {
            Ok(n) if n == sink.len() => {}
            _ => break,
        }
    }
}

/// What the terminal looks like, as the engine asks.
struct Facts<'a> {
    master: BorrowedFd<'a>,
    shell: Pid,
}

impl Tty for Facts<'_> {
    fn shell_in_front(&self) -> bool {
        tcgetpgrp(self.master).is_ok_and(|group| group == self.shell)
    }

    fn readline_mode(&self) -> bool {
        tcgetattr(self.master).is_ok_and(|modes| {
            !modes
                .local_modes
                .intersects(LocalModes::ICANON | LocalModes::ECHO)
        })
    }
}

/// What woke the loop.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Which {
    Stdin,
    MasterIn,
    MasterOut,
    Winch,
    Child,
    End,
}

/// Why the loop ended.
enum Over {
    /// The shell has gone, or its terminal has.
    ShellGone,
    /// The person's terminal has gone, or a signal asked to end.
    Hangup,
}

struct Proxy {
    master: OwnedFd,
    child: Child,
    shell: Pid,
    engine: Engine<Brain>,
    signals: Signals,
    out: Out,
    to_shell: Vec<u8>,
    input: Vec<u8>,
    output: Vec<u8>,
    cwd: PathBuf,
}

impl Proxy {
    fn new(master: OwnedFd, child: Child, engine: Engine<Brain>, signals: Signals) -> Self {
        let shell = Pid::from_child(&child);
        Self {
            master,
            shell,
            cwd: PathBuf::from(format!("/proc/{}/cwd", shell.as_raw_pid())),
            child,
            engine,
            signals,
            out: Out::default(),
            to_shell: Vec::new(),
            input: vec![0; CHUNK],
            output: vec![0; CHUNK],
        }
    }

    /// Carry bytes until the shell is gone. Returns its exit status.
    fn run(&mut self) -> Result<i32> {
        let over = self.serve()?;
        self.drain_output()?;
        self.engine.finish(&mut self.out);
        self.apply()?;
        Ok(self.reap(&over))
    }

    fn serve(&mut self) -> Result<Over> {
        // This loop is meant to run until the shell is gone; each way out
        // is a return.
        loop {
            let now = Instant::now();
            let timeout = self
                .engine
                .deadline()
                .map(|at| at.saturating_duration_since(now));
            let ready = self.wait(timeout)?;
            let now = Instant::now();
            if ready.contains(&Which::End) {
                return Ok(Over::Hangup);
            }
            if ready.contains(&Which::Winch) {
                self.resize()?;
            }
            if ready.contains(&Which::MasterIn) && !self.read_shell(now)? {
                return Ok(Over::ShellGone);
            }
            if ready.contains(&Which::Stdin) && !self.read_terminal(now)? {
                return Ok(Over::Hangup);
            }
            if ready.contains(&Which::MasterOut) {
                self.flush_to_shell()?;
            }
            if ready.contains(&Which::Child) && self.child_gone()? {
                return Ok(Over::ShellGone);
            }
            self.tick(now)?;
        }
    }

    /// Wait for something to happen, or for `timeout`.
    fn wait(&self, timeout: Option<Duration>) -> Result<Vec<Which>> {
        let mut fds: Vec<PollFd<'_>> = Vec::with_capacity(5);
        let mut which: Vec<Which> = Vec::with_capacity(5);
        let wants_input = !self.engine.holds_input() && self.to_shell.len() < QUEUE_LIMIT;
        if wants_input {
            fds.push(PollFd::from_borrowed_fd(stdin(), PollFlags::IN));
            which.push(Which::Stdin);
        }
        let mut master_events = PollFlags::IN;
        if !self.to_shell.is_empty() {
            master_events |= PollFlags::OUT;
        }
        fds.push(PollFd::new(&self.master, master_events));
        which.push(Which::MasterIn);
        for (kind, pipe) in [
            (Which::Winch, &self.signals.winch),
            (Which::Child, &self.signals.child),
            (Which::End, &self.signals.end),
        ] {
            fds.push(PollFd::new(pipe, PollFlags::IN));
            which.push(kind);
        }
        let timespec = timeout.map(|d| Timespec {
            tv_sec: i64::try_from(d.as_secs()).unwrap_or(i64::MAX),
            tv_nsec: i64::from(d.subsec_nanos()),
        });
        match poll(&mut fds, timespec.as_ref()) {
            Ok(_) | Err(Errno::INTR) => {}
            Err(source) => return Err(Error::io("wait for something to do", source)),
        }
        let mut ready = Vec::with_capacity(5);
        for (fd, kind) in fds.iter().zip(which) {
            let events = fd.revents();
            if kind == Which::MasterIn {
                if events.contains(PollFlags::OUT) {
                    ready.push(Which::MasterOut);
                }
                if events.intersects(PollFlags::IN | PollFlags::HUP | PollFlags::ERR) {
                    ready.push(Which::MasterIn);
                }
            } else if !events.is_empty() {
                ready.push(kind);
            }
        }
        Ok(ready)
    }

    /// The shell wrote. Returns false once it has closed its end.
    fn read_shell(&mut self, now: Instant) -> Result<bool> {
        let count = match rustix::io::read(&self.master, self.output.as_mut_slice()) {
            Ok(0) | Err(Errno::IO) => return Ok(false),
            Ok(count) => count,
            Err(Errno::AGAIN | Errno::INTR) => return Ok(true),
            Err(source) => return Err(Error::io("read from the shell", source)),
        };
        let facts = Facts {
            master: self.master.as_fd(),
            shell: self.shell,
        };
        let bytes = self.output.get(..count).unwrap_or_default();
        self.engine.on_output(now, bytes, &facts, &mut self.out);
        self.apply()?;
        Ok(true)
    }

    /// The person typed. Returns false once the terminal has closed.
    fn read_terminal(&mut self, now: Instant) -> Result<bool> {
        let count = match rustix::io::read(stdin(), self.input.as_mut_slice()) {
            Ok(0) | Err(Errno::IO) => return Ok(false),
            Ok(count) => count,
            Err(Errno::AGAIN | Errno::INTR) => return Ok(true),
            Err(source) => return Err(Error::io("read from the terminal", source)),
        };
        let facts = Facts {
            master: self.master.as_fd(),
            shell: self.shell,
        };
        let bytes = self.input.get(..count).unwrap_or_default();
        self.engine.on_input(now, bytes, &facts, &mut self.out);
        self.apply()?;
        Ok(true)
    }

    fn tick(&mut self, now: Instant) -> Result<()> {
        if self.engine.deadline().is_none_or(|at| at > now) {
            return Ok(());
        }
        let facts = Facts {
            master: self.master.as_fd(),
            shell: self.shell,
        };
        self.engine.on_tick(now, &facts, &mut self.out);
        self.apply()
    }

    /// The terminal changed size: the shell's does too.
    fn resize(&mut self) -> Result<()> {
        drain(&self.signals.winch);
        let Ok(mut size) = tcgetwinsize(stdin()) else {
            return Ok(());
        };
        if size.ws_col == 0 || size.ws_row == 0 {
            size.ws_col = 80;
            size.ws_row = 24;
        }
        // The kernel tells the shell's programs, by signal, that it changed.
        tcsetwinsize(&self.master, size)
            .map_err(|e| Error::io("resize the shell's terminal", e))?;
        self.engine.resize(usize::from(size.ws_col), &mut self.out);
        self.apply()
    }

    /// Whether the shell has exited.
    fn child_gone(&mut self) -> Result<bool> {
        drain(&self.signals.child);
        let status = self
            .child
            .try_wait()
            .map_err(|e| Error::io("check on the shell", e))?;
        Ok(status.is_some())
    }

    /// Carry out what the engine decided.
    fn apply(&mut self) -> Result<()> {
        write_all(stdout(), &self.out.to_terminal)?;
        self.to_shell.extend_from_slice(&self.out.to_shell);
        let events = std::mem::take(&mut self.out.events);
        self.out.clear();
        self.flush_to_shell()?;
        for event in events {
            self.event(event);
        }
        Ok(())
    }

    fn event(&mut self, event: Event) {
        let cwd = std::fs::read_link(&self.cwd).ok();
        let brain = self.engine.suggester_mut();
        match event {
            Event::PromptStarted => brain.prompt(cwd.as_deref()),
            Event::CommandStarted { line } => brain.ran(line.as_deref(), cwd.as_deref()),
        }
    }

    /// Write what is queued for the shell, as much as it takes.
    fn flush_to_shell(&mut self) -> Result<()> {
        // Each turn takes some bytes or ends.
        for _ in 0..self.to_shell.len() {
            if self.to_shell.is_empty() {
                break;
            }
            match rustix::io::write(&self.master, &self.to_shell) {
                Ok(0) | Err(Errno::AGAIN) => break,
                Ok(count) => {
                    self.to_shell.drain(..count.min(self.to_shell.len()));
                }
                Err(Errno::INTR) => {}
                // The shell has gone; there is nobody to type to.
                Err(Errno::IO) => {
                    self.to_shell.clear();
                    break;
                }
                Err(source) => return Err(Error::io("write to the shell", source)),
            }
        }
        Ok(())
    }

    /// What the shell wrote before it went, which is still to be shown.
    fn drain_output(&mut self) -> Result<()> {
        let now = Instant::now();
        // Bounded by how much a pseudo-terminal can hold.
        for _ in 0..256 {
            if !self.read_shell_once(now)? {
                break;
            }
        }
        Ok(())
    }

    fn read_shell_once(&mut self, now: Instant) -> Result<bool> {
        match rustix::io::read(&self.master, self.output.as_mut_slice()) {
            Ok(0) | Err(Errno::IO | Errno::AGAIN) => Ok(false),
            Ok(count) => {
                let facts = Facts {
                    master: self.master.as_fd(),
                    shell: self.shell,
                };
                let bytes = self.output.get(..count).unwrap_or_default();
                self.engine.on_output(now, bytes, &facts, &mut self.out);
                self.apply()?;
                Ok(true)
            }
            Err(Errno::INTR) => Ok(true),
            Err(source) => Err(Error::io("read from the shell", source)),
        }
    }

    /// Wait for the shell to be gone, hanging up on it if the terminal is.
    fn reap(&mut self, over: &Over) -> i32 {
        if matches!(over, Over::Hangup) {
            // Closing the master (when this ends) hangs the terminal up on
            // the shell; give it a moment to take the hint.
            std::thread::sleep(Duration::from_millis(50));
        }
        let mut status = None;
        for _ in 0..REAP_TURNS {
            match self.child.try_wait() {
                Ok(Some(done)) => {
                    status = Some(done);
                    break;
                }
                Ok(None) => std::thread::sleep(REAP_STEP),
                Err(_) => break,
            }
        }
        if let Some(status) = status {
            return status
                .code()
                .or_else(|| status.signal().map(|signal| signal.saturating_add(128)))
                .unwrap_or(1);
        }
        // It did not leave. Make it.
        let _killed = self.child.kill();
        let _reaped = self.child.wait();
        1
    }
}

/// Write all of `bytes`, waiting for the descriptor when it is full.
fn write_all(fd: BorrowedFd<'_>, mut bytes: &[u8]) -> Result<()> {
    // Each turn writes some bytes or waits; a descriptor that takes none
    // for this long is not coming back.
    for _ in 0..bytes.len().saturating_add(1 << 20) {
        if bytes.is_empty() {
            return Ok(());
        }
        match rustix::io::write(fd, bytes) {
            Ok(count) => bytes = bytes.get(count..).unwrap_or_default(),
            Err(Errno::INTR) => {}
            Err(Errno::AGAIN) => {
                let mut fds = [PollFd::from_borrowed_fd(fd, PollFlags::OUT)];
                poll(&mut fds, None).map_err(|e| Error::io("wait for the terminal", e))?;
            }
            Err(source) => return Err(Error::io("write to the terminal", source)),
        }
    }
    Err(Error::io(
        "write to the terminal",
        io::Error::from(io::ErrorKind::WriteZero),
    ))
}
