//! A terminal for the tests to be the person at: it starts a program on a
//! pseudo-terminal, types to it, and keeps a [`Screen`] of what comes back.

use std::os::fd::OwnedFd;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::fs::{Mode, OFlags};
use rustix::io::Errno;
use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
use rustix::termios::{Winsize, tcsetwinsize};

use super::screen::Screen;

pub const ROWS: u16 = 24;
pub const COLS: u16 = 80;

pub struct Terminal {
    master: OwnedFd,
    child: Child,
    pub screen: Screen,
    /// Everything the program has written, as bytes.
    pub raw: Vec<u8>,
}

/// What to start, and in what world.
pub struct Launch<'a> {
    pub argv: &'a [String],
    pub env: Vec<(String, String)>,
    pub dir: &'a Path,
    pub size: (u16, u16),
}

fn winsize(rows: u16, cols: u16) -> Winsize {
    Winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    }
}

impl Terminal {
    /// Start `argv` with the pseudo-terminal as its controlling terminal, by
    /// way of the program's own hidden session command.
    pub fn start(launch: &Launch<'_>) -> Self {
        let master =
            openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC).unwrap();
        grantpt(&master).unwrap();
        unlockpt(&master).unwrap();
        let name = ptsname(&master, Vec::new()).unwrap();
        let slave = rustix::fs::open(
            name.as_c_str(),
            OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .unwrap();
        let (rows, cols) = launch.size;
        tcsetwinsize(&slave, winsize(rows, cols)).unwrap();
        let copy = || Stdio::from(slave.try_clone().unwrap());
        let mut command = Command::new(env!("CARGO_BIN_EXE_inkling"));
        command
            .arg("__session")
            .arg("--")
            .args(launch.argv)
            .env_clear()
            .envs(launch.env.iter().map(|(k, v)| (k, v)))
            .current_dir(launch.dir)
            .stdin(copy())
            .stdout(copy())
            .stderr(copy());
        let child = command.spawn().unwrap();
        drop(slave);
        let flags = rustix::fs::fcntl_getfl(&master).unwrap();
        rustix::fs::fcntl_setfl(&master, flags | OFlags::NONBLOCK).unwrap();
        Self {
            master,
            child,
            screen: Screen::new(usize::from(rows), usize::from(cols)),
            raw: Vec::new(),
        }
    }

    pub fn send(&mut self, bytes: &[u8]) {
        let mut rest = bytes;
        let started = Instant::now();
        while !rest.is_empty() {
            match rustix::io::write(&self.master, rest) {
                Ok(count) => rest = rest.get(count..).unwrap_or_default(),
                Err(Errno::AGAIN | Errno::INTR) => {
                    // The program is not reading yet; keep up with what it
                    // writes while waiting, so neither side blocks the other.
                    self.pump(Duration::from_millis(2));
                    assert!(
                        started.elapsed() < Duration::from_secs(20),
                        "could not type"
                    );
                }
                Err(error) => panic!("typing failed: {error}"),
            }
        }
    }

    /// Read what has been written, waiting up to `wait` for something.
    /// Returns whether anything came.
    pub fn pump(&mut self, wait: Duration) -> bool {
        let mut fds = [PollFd::new(&self.master, PollFlags::IN)];
        let timeout = Timespec {
            tv_sec: i64::try_from(wait.as_secs()).unwrap(),
            tv_nsec: i64::from(wait.subsec_nanos()),
        };
        let _ = poll(&mut fds, Some(&timeout));
        let mut any = false;
        let mut buffer = vec![0_u8; 64 * 1024];
        loop {
            match rustix::io::read(&self.master, buffer.as_mut_slice()) {
                Ok(0) | Err(Errno::IO | Errno::AGAIN) => break,
                Ok(count) => {
                    any = true;
                    let bytes = buffer.get(..count).unwrap();
                    self.raw.extend_from_slice(bytes);
                    self.screen.feed(bytes);
                    let replies = std::mem::take(&mut self.screen.replies);
                    if !replies.is_empty() {
                        self.send(&replies);
                    }
                }
                Err(Errno::INTR) => {}
                Err(error) => panic!("reading failed: {error}"),
            }
        }
        any
    }

    /// Pump until nothing has come for `quiet`, or `limit` has passed.
    pub fn settle(&mut self, quiet: Duration) {
        let started = Instant::now();
        let mut last = Instant::now();
        while started.elapsed() < Duration::from_secs(10) {
            if self.pump(Duration::from_millis(5)) {
                last = Instant::now();
            } else if last.elapsed() >= quiet {
                return;
            }
        }
    }

    /// Pump until `condition` holds of the screen. Returns whether it did
    /// within `limit`.
    pub fn wait_for(
        &mut self,
        limit: Duration,
        mut condition: impl FnMut(&Screen) -> bool,
    ) -> bool {
        let started = Instant::now();
        loop {
            self.pump(Duration::from_millis(5));
            if condition(&self.screen) {
                return true;
            }
            if started.elapsed() >= limit {
                return false;
            }
        }
    }

    /// Wait for text to be on the screen.
    pub fn wait_text(&mut self, text: &str) -> bool {
        self.wait_for(Duration::from_secs(10), |screen| {
            screen.text().contains(text)
        })
    }

    /// Type, and let everything that follows settle.
    pub fn type_and_settle(&mut self, bytes: &[u8]) {
        self.send(bytes);
        self.settle(Duration::from_millis(80));
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        tcsetwinsize(&self.master, winsize(rows, cols)).unwrap();
        self.screen.resize(usize::from(rows), usize::from(cols));
    }

    /// Wait for the program to exit.
    pub fn exit_status(&mut self, limit: Duration) -> Option<ExitStatus> {
        let started = Instant::now();
        loop {
            self.pump(Duration::from_millis(5));
            if let Some(status) = self.child.try_wait().unwrap() {
                self.pump(Duration::from_millis(20));
                return Some(status);
            }
            if started.elapsed() >= limit {
                return None;
            }
        }
    }

    /// Run a command line and wait for the prompt to come back.
    pub fn command(&mut self, line: &str) {
        let mut bytes = line.as_bytes().to_vec();
        bytes.push(b'\r');
        self.send(&bytes);
        self.wait_prompt();
    }

    /// Wait for a bare prompt on the cursor's row, and for things to quiet.
    pub fn wait_prompt(&mut self) {
        let came = self.wait_for(Duration::from_secs(15), |screen| {
            screen.row(screen.cursor().1) == "$"
        });
        assert!(came, "no prompt came back:\n{}", self.screen.all_text());
        self.settle(Duration::from_millis(60));
    }

    /// The row the cursor is on, as text.
    pub fn cursor_row(&self) -> String {
        self.screen.row(self.screen.cursor().1)
    }

    /// The dim text on the cursor's row.
    pub fn dim_on_cursor_row(&self) -> String {
        self.screen.dim_text(self.screen.cursor().1)
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
