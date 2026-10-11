//! The person's side: keys on their way to the shell.

use super::{
    Batch, Ctx, Engine, Flow, Instant, Line, Mode, Out, PASTE_END, PROBE_KEYS, Phase, Probe,
    Prompt, Safety, Suggest, Tty,
};
use crate::keys::{self, Accept, Key, Token};
use crate::text;

/// What one pass over the buffered keys settled.
struct Pass<'a> {
    ctx: Ctx<'a>,
    /// The terminal and the shell are in a state where a probe is safe.
    ready: bool,
    batch: Batch,
    out: &'a mut Out,
}

impl<S: Suggest> Engine<S> {
    /// The person typed `bytes`.
    pub fn on_input(&mut self, now: Instant, bytes: &[u8], tty: &dyn Tty, out: &mut Out) {
        let Phase::Prompt(prompt) = &mut self.phase else {
            out.to_shell.extend_from_slice(bytes);
            return;
        };
        if prompt.disabled {
            out.to_shell.extend_from_slice(bytes);
            return;
        }
        if prompt.input.is_empty() {
            prompt.input_since = Some(now);
        }
        prompt.input.extend_from_slice(bytes);
        self.pump(now, tty, out);
    }

    /// Whether another program has the terminal; if so the quarantine
    /// begins, and when it is over and readline has it again, it ends.
    pub(super) fn check_front(&mut self, tty: &dyn Tty) {
        let Phase::Prompt(prompt) = &mut self.phase else {
            return;
        };
        if prompt.disabled {
            return;
        }
        let mode = tty.mode();
        if !tty.shell_in_front() || mode == Mode::Other {
            prompt.safety = Safety::Quarantined { foreign_seen: true };
            prompt.line = Line::Unknown;
        } else if prompt.safety == (Safety::Quarantined { foreign_seen: true })
            && mode == Mode::Readline
        {
            prompt.safety = Safety::Clear;
            prompt.line = Line::Unknown;
        }
    }

    /// Pass on the keys that have been waiting, and send a probe if one is
    /// due. Does nothing while a probe is out.
    pub(super) fn pump(&mut self, now: Instant, tty: &dyn Tty, out: &mut Out) {
        self.check_front(tty);
        let ready = tty.shell_in_front() && tty.mode() == Mode::Readline;
        let Self {
            phase,
            shown,
            submitted,
            ..
        } = self;
        let Phase::Prompt(prompt) = phase else {
            return;
        };
        if prompt.probe != Probe::Idle || prompt.input.is_empty() {
            return;
        }
        let mut pass = Pass {
            ctx: Ctx {
                shown: shown.as_deref(),
            },
            ready,
            batch: Batch::default(),
            out,
        };
        let flow = prompt.pass(&mut pass);
        let batch = pass.batch;
        if flow == Flow::Leave {
            *submitted = match &prompt.line {
                Line::Known { text, .. } => Some(text.clone()),
                Line::Unknown => None,
            };
            out.to_shell.append(&mut prompt.input);
            self.phase = Phase::Away;
            return;
        }
        let due = batch.any
            && batch.last_safe
            && ready
            && !prompt.quarantined()
            && !prompt.in_paste
            && !prompt.disabled;
        if due {
            out.to_shell.extend_from_slice(PROBE_KEYS);
            prompt.probe = Probe::Waiting { since: now };
        }
        if prompt.input.is_empty() {
            prompt.input_since = None;
        }
    }
}

impl Prompt {
    /// Go through the buffered keys, passing them on, until the buffer is
    /// used up, ends inside a key, reaches a key that has to wait for a
    /// probe, or reaches one that runs the line.
    fn pass(&mut self, pass: &mut Pass<'_>) -> Flow {
        let input = std::mem::take(&mut self.input);
        let mut at = 0;
        let mut flow = Flow::Next;
        // Every turn takes at least one byte, or ends the pass.
        for _ in 0..input.len() {
            if at >= input.len() {
                break;
            }
            if self.in_paste {
                at = self.paste(&input, at, pass);
                continue;
            }
            let rest = input.get(at..).unwrap_or_default();
            let Token::Key { key, len } = keys::next(rest) else {
                break;
            };
            let raw = rest.get(..len).unwrap_or_default();
            flow = self.key(key, raw, pass);
            if flow == Flow::Defer {
                break;
            }
            at = at.saturating_add(len);
            if flow == Flow::Leave {
                break;
            }
        }
        self.input = input;
        self.input.drain(..at.min(self.input.len()));
        flow
    }

    /// One key.
    fn key(&mut self, key: Key, raw: &[u8], pass: &mut Pass<'_>) -> Flow {
        match key {
            Key::Submit => {
                pass.out.to_shell.extend_from_slice(raw);
                return Flow::Leave;
            }
            Key::Accept(kind) => {
                if self.take_suggestion(kind, pass) {
                    pass.batch.any = true;
                    pass.batch.last_safe = true;
                    return Flow::Next;
                }
                // The line is not known, but keys before this one are about
                // to change it: find out first, then see.
                let unknown = matches!(self.line, Line::Unknown);
                if unknown && pass.batch.any && pass.batch.last_safe && pass.ready {
                    return Flow::Defer;
                }
            }
            _ => {}
        }
        pass.out.to_shell.extend_from_slice(raw);
        pass.batch.any = true;
        pass.batch.last_safe = key.probe_safe();
        self.line = Line::Unknown;
        match key {
            Key::Modal => self.quarantine(),
            Key::Cancel => self.safety = Safety::Clear,
            Key::PasteStart => self.in_paste = true,
            _ => {}
        }
        Flow::Next
    }

    /// Type the part of the suggestion `kind` takes, instead of the key.
    fn take_suggestion(&mut self, kind: Accept, pass: &mut Pass<'_>) -> bool {
        let Some(rest) = pass.ctx.shown else {
            return false;
        };
        let at_end = matches!(self.line, Line::Known { at_end: true, .. });
        if !at_end || self.quarantined() || self.in_paste {
            return false;
        }
        let end = match kind {
            Accept::All => rest.len(),
            Accept::Word => text::word_end(rest),
            Accept::Shell => text::shell_word_end(rest),
        };
        let part = rest.get(..end).unwrap_or_default();
        if part.is_empty() {
            return false;
        }
        pass.out.to_shell.extend_from_slice(part.as_bytes());
        self.line = Line::Unknown;
        true
    }

    /// Inside a paste: everything passes until the terminal ends it. Returns
    /// where the buffer is read from next.
    fn paste(&mut self, input: &[u8], at: usize, pass: &mut Pass<'_>) -> usize {
        let rest = input.get(at..).unwrap_or_default();
        let mut window = std::mem::take(&mut self.paste_tail);
        let before = window.len();
        window.extend_from_slice(rest);
        let found = window.windows(PASTE_END.len()).position(|w| w == PASTE_END);
        let used = match found {
            Some(start) => start
                .saturating_add(PASTE_END.len())
                .saturating_sub(before)
                .min(rest.len()),
            None => rest.len(),
        };
        pass.out
            .to_shell
            .extend_from_slice(rest.get(..used).unwrap_or_default());
        if found.is_some() {
            self.in_paste = false;
            pass.batch.any = true;
            pass.batch.last_safe = true;
            self.line = Line::Unknown;
        } else {
            let keep = window
                .len()
                .saturating_sub(PASTE_END.len().saturating_sub(1));
            self.paste_tail = window.split_off(keep);
        }
        at.saturating_add(used)
    }
}
