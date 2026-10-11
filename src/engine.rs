//! What to do with each byte that passes.
//!
//! The engine is the whole of inkling's judgement and none of its plumbing.
//! It is handed what the person typed and what the shell wrote, with the time
//! each came at and a few facts about the terminal, and it answers with the
//! bytes to send each way and events for the caller to act on. It reads no
//! clock, no file and no socket, so every rule here can be tested by feeding
//! it a conversation.
//!
//! # The rules
//!
//! * Outside a prompt everything passes untouched, both ways.
//! * At a prompt the keys are read as they pass. After a key that leaves the
//!   shell's editing in its plain state (see [`crate::keys`]) a probe goes to
//!   the shell in the same write, which makes readline report the line and
//!   the cursor. While the probe is out, nothing more is read from the
//!   person, and what the shell writes back is held, because readline clears
//!   the line and draws it again around the report, and the terminal should
//!   see that as one update.
//! * With the report in, and the redraw settled, the held bytes go to the
//!   terminal and, if the cursor is at the end of a line that starts a command
//!   in the history, the rest of that command is drawn dim after the cursor.
//! * Before the next output reaches the terminal, the suggestion is erased.
//! * A key that takes the suggestion is not sent; the missing characters are,
//!   as if typed.
//! * A probe is never sent when it might reach anything but readline: after
//!   a key that is not known to leave the shell plain, while another program
//!   holds the terminal, inside a paste, or in the same write as a key that
//!   runs the line.

use std::time::{Duration, Instant};

use crate::column::Column;
use crate::init::PROBE_KEYS;
use crate::osc::{self, Mark};
use crate::quote;
use crate::render::{self, Style};
use crate::text;

/// How long after the report to wait for the rest of readline's redraw.
const SETTLE: Duration = Duration::from_micros(300);
/// The longest the redraw is waited for, however it trickles in.
const SETTLE_LIMIT: Duration = Duration::from_millis(4);
/// How long a probe may go unanswered before it is given up on.
const REPORT_TIMEOUT: Duration = Duration::from_millis(80);
/// How long the start of a key is kept back waiting for its end.
const PARTIAL_HOLD: Duration = Duration::from_millis(15);
/// How long the start of a mark is kept back waiting for its end.
const MARK_HOLD: Duration = Duration::from_millis(5);
/// The marker that ends a bracketed paste.
const PASTE_END: &[u8] = b"\x1b[201~";

/// Where suggestions come from.
pub trait Suggest {
    /// The command to suggest for `line`: one that starts with it and is
    /// longer.
    fn suggest(&self, line: &str) -> Option<&str>;
}

impl Suggest for crate::index::Index {
    fn suggest(&self, line: &str) -> Option<&str> {
        Self::suggest(self, line)
    }
}

/// What the engine needs to know about the terminal at the moment, which
/// only the operating system can say.
pub trait Tty {
    /// Whether the shell itself, and not a program it started, is in the
    /// foreground of the terminal.
    fn shell_in_front(&self) -> bool;
    /// Whether the terminal is set the way readline sets it to read a line:
    /// no echo, no line editing by the terminal.
    fn readline_mode(&self) -> bool;
}

/// What happened that the caller may want to act on.
#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    /// A prompt is being drawn: the directory may have changed, and there
    /// may be new history.
    PromptStarted,
    /// A command is about to run. If the line it was is known for certain,
    /// here it is.
    CommandStarted { line: Option<String> },
}

/// What the engine wants done.
#[derive(Debug, Default)]
pub struct Out {
    /// Bytes for the terminal, in order.
    pub to_terminal: Vec<u8>,
    /// Bytes for the shell, in order.
    pub to_shell: Vec<u8>,
    pub events: Vec<Event>,
}

impl Out {
    /// Empty it for the next call.
    pub fn clear(&mut self) {
        self.to_terminal.clear();
        self.to_shell.clear();
        self.events.clear();
    }
}

/// What is known of the line being edited.
#[derive(Debug, PartialEq, Eq)]
enum Line {
    /// Not known: something has been done to it since it was last reported.
    Unknown,
    /// As reported, with nothing done since.
    Known { text: String, at_end: bool },
}

/// Where a probe is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Probe {
    /// None is out.
    Idle,
    /// Sent at this time; no report yet.
    Waiting { since: Instant },
    /// Reported; waiting for readline's redraw to finish arriving.
    Settling { until: Instant, limit: Instant },
}

/// Whether the shell is known to be somewhere a probe is safe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Safety {
    Clear,
    /// The shell may be in a state that a probe would disturb: a search, a
    /// prefix key, a program that has the terminal. Nothing is probed until
    /// something sets it right.
    Quarantined {
        /// Another program has had the terminal since the quarantine began;
        /// when it gives the terminal back, the quarantine ends.
        foreign_seen: bool,
    },
}

/// The state while at a prompt.
#[derive(Debug)]
struct Prompt {
    /// Nothing is done at this prompt (vi mode).
    disabled: bool,
    /// Whether a probe may be sent at all.
    safety: Safety,
    in_paste: bool,
    line: Line,
    probe: Probe,
    /// Input received and not yet passed on: the start of a key, or keys
    /// kept for after the probe.
    input: Vec<u8>,
    input_since: Option<Instant>,
    /// The last bytes of a paste, to find its end across reads.
    paste_tail: Vec<u8>,
}

impl Prompt {
    fn quarantined(&self) -> bool {
        self.safety != Safety::Clear
    }

    fn quarantine(&mut self) {
        if self.safety == Safety::Clear {
            self.safety = Safety::Quarantined {
                foreign_seen: false,
            };
        }
    }

    fn new(flags: &[u8]) -> Self {
        Self {
            disabled: flags.contains(&b'v'),
            safety: Safety::Clear,
            in_paste: false,
            line: Line::Unknown,
            probe: Probe::Idle,
            input: Vec::new(),
            input_since: None,
            paste_tail: Vec::new(),
        }
    }
}

#[derive(Debug)]
enum Phase {
    /// No prompt: a command is running, or none has been seen.
    Away,
    Prompt(Box<Prompt>),
}

/// What happened to the keys in one pass.
#[derive(Default)]
struct Batch {
    /// Something was passed on that the shell will act on.
    any: bool,
    /// And the last of it leaves readline in its plain state.
    last_safe: bool,
}

/// What the keys in a pass may use.
struct Ctx<'a> {
    /// The rest of the suggestion on screen, if there is one.
    shown: Option<&'a str>,
}

/// How the keys of a pass go on.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Flow {
    Next,
    /// Leave this key for after the probe.
    Defer,
    /// The line is run: what remains is not for this prompt.
    Leave,
}

/// The engine.
#[derive(Debug)]
pub struct Engine<S> {
    suggester: S,
    style: Style,
    filter: osc::Filter,
    column: Column,
    phase: Phase,
    clean: Vec<u8>,
    found: Vec<osc::Found>,
    /// The shell's output kept back while a probe is out.
    held: Vec<u8>,
    /// The part of a suggestion that is on screen, whole.
    shown: Option<String>,
    /// When output last ended inside the start of a mark.
    mark_since: Option<Instant>,
    /// The line, if it was known for certain, when the keys that ran it passed.
    submitted: Option<String>,
}

impl<S: Suggest> Engine<S> {
    /// An engine for a terminal `columns` wide, listening for the marks that
    /// carry `nonce`.
    #[must_use]
    pub fn new(nonce: &str, columns: usize, style: Style, suggester: S) -> Self {
        Self {
            suggester,
            style,
            filter: osc::Filter::new(nonce),
            column: Column::new(columns),
            phase: Phase::Away,
            clean: Vec::new(),
            found: Vec::new(),
            held: Vec::new(),
            shown: None,
            mark_since: None,
            submitted: None,
        }
    }

    /// The source of suggestions, to be refreshed.
    pub fn suggester_mut(&mut self) -> &mut S {
        &mut self.suggester
    }

    /// The source of suggestions.
    #[must_use]
    pub fn suggester(&self) -> &S {
        &self.suggester
    }

    /// Whether the caller should leave the person's input unread for now.
    #[must_use]
    pub fn holds_input(&self) -> bool {
        match &self.phase {
            Phase::Prompt(prompt) => prompt.probe != Probe::Idle,
            Phase::Away => false,
        }
    }

    /// When the engine next needs a call to [`Engine::on_tick`], if it does.
    #[must_use]
    pub fn deadline(&self) -> Option<Instant> {
        let mut soonest: Option<Instant> = None;
        let mut consider = |at: Instant| soonest = Some(soonest.map_or(at, |s| s.min(at)));
        if let Phase::Prompt(prompt) = &self.phase {
            match prompt.probe {
                Probe::Idle => {}
                Probe::Waiting { since } => consider(output::after(since, REPORT_TIMEOUT)),
                Probe::Settling { until, .. } => consider(until),
            }
            if let Some(since) = prompt.input_since
                && !prompt.input.is_empty()
            {
                consider(output::after(since, PARTIAL_HOLD));
            }
        }
        if let Some(since) = self.mark_since {
            consider(output::after(since, MARK_HOLD));
        }
        soonest
    }

    /// The terminal is `columns` wide now.
    pub fn resize(&mut self, columns: usize, out: &mut Out) {
        self.column.resize(columns);
        self.erase(out);
        if let Phase::Prompt(prompt) = &mut self.phase {
            prompt.line = Line::Unknown;
        }
    }

    /// The shell is gone: let go of everything held.
    pub fn finish(&mut self, out: &mut Out) {
        let mut rest = Vec::new();
        self.filter.flush(&mut rest);
        self.mark_since = None;
        self.release_held(out);
        self.release(&rest, out);
        self.phase = Phase::Away;
    }

    fn erase(&mut self, out: &mut Out) {
        if self.shown.take().is_some() {
            out.to_terminal.extend_from_slice(render::ERASE);
        }
    }

    /// Send `bytes` to the terminal, erasing the suggestion first.
    fn release(&mut self, bytes: &[u8], out: &mut Out) {
        if bytes.is_empty() {
            return;
        }
        self.erase(out);
        out.to_terminal.extend_from_slice(bytes);
    }

    fn release_held(&mut self, out: &mut Out) {
        let held = std::mem::take(&mut self.held);
        self.release(&held, out);
        self.held = held;
        self.held.clear();
    }
}

mod input;
mod output;

#[cfg(test)]
mod tests;
