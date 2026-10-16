//! The shell's side: what it writes, and the probe's answer.

use super::{
    Duration, Engine, Event, Instant, Line, MARK_HOLD, Mark, Out, PARTIAL_HOLD, Phase, Probe,
    Prompt, REPORT_TIMEOUT, SETTLE, SETTLE_LIMIT, Suggest, Tty, quote, render,
};
use crate::keys::{self, Token};

/// `at` and `by` added, or `at` if the sum does not exist.
pub(super) fn after(at: Instant, by: Duration) -> Instant {
    at.checked_add(by).unwrap_or(at)
}

impl<S: Suggest> Engine<S> {
    /// The shell wrote `bytes`.
    pub fn on_output(&mut self, now: Instant, bytes: &[u8], tty: &dyn Tty, out: &mut Out) {
        let mut clean = std::mem::take(&mut self.clean);
        let mut found = std::mem::take(&mut self.found);
        clean.clear();
        found.clear();
        self.filter.feed(bytes, &mut clean, &mut found);
        self.mark_since = if self.filter.holding() {
            self.mark_since.or(Some(now))
        } else {
            None
        };
        let mut at = 0;
        for item in found.drain(..) {
            let end = item.at.min(clean.len());
            self.emit(clean.get(at..end).unwrap_or_default(), out);
            at = end;
            self.mark(now, item.mark, out);
        }
        self.emit(clean.get(at..).unwrap_or_default(), out);
        self.clean = clean;
        self.found = found;
        self.more_output(now);
        if self.redraw_done() {
            self.finish_probe(now, tty, out);
        }
        self.check_front(tty);
        if std::mem::take(&mut self.pump_due) {
            self.pump(now, tty, out);
        }
    }

    /// Time has passed with nothing else to do.
    pub fn on_tick(&mut self, now: Instant, tty: &dyn Tty, out: &mut Out) {
        if let Some(since) = self.mark_since
            && now >= after(since, MARK_HOLD)
        {
            // It never became a mark: it was something else.
            let mut rest = Vec::new();
            self.filter.flush(&mut rest);
            self.mark_since = None;
            self.emit(&rest, out);
        }
        self.check_front(tty);
        let Phase::Prompt(prompt) = &mut self.phase else {
            return;
        };
        match prompt.probe {
            Probe::Settling { until, limit, .. } if now >= until || now >= limit => {
                self.finish_probe(now, tty, out);
            }
            Probe::Waiting { since } if now >= after(since, REPORT_TIMEOUT) => {
                self.give_up(now, tty, out);
            }
            Probe::Idle => self.stalled_key(now, out),
            _ => {}
        }
    }

    /// Output carries on after the report: wait for the end of it.
    fn more_output(&mut self, now: Instant) {
        if let Phase::Prompt(prompt) = &mut self.phase
            && let Probe::Settling {
                limit, report_at, ..
            } = prompt.probe
        {
            prompt.probe = Probe::Settling {
                until: after(now, SETTLE).min(limit),
                limit,
                report_at,
            };
        }
    }

    fn probing(&self) -> bool {
        matches!(&self.phase, Phase::Prompt(prompt) if prompt.probe != Probe::Idle)
    }

    /// Clean output: held if a probe is out, else on its way.
    fn emit(&mut self, bytes: &[u8], out: &mut Out) {
        if bytes.is_empty() {
            return;
        }
        // At a prompt every byte is followed. While a command runs only the
        // end of its output is, so that the column is known when the next
        // prompt is drawn and its first key can be drawn ahead.
        if matches!(self.phase, Phase::Prompt(_)) {
            self.column.feed(bytes);
        } else {
            self.column.follow(bytes);
        }
        if self.probing() && self.held.len().saturating_add(bytes.len()) > super::HELD_LIMIT {
            self.abandon_probe(out);
        }
        if self.probing() {
            self.held.extend_from_slice(bytes);
        } else {
            self.release(bytes, out);
        }
    }

    /// Stop holding output back: let go of it, and take the probe for lost.
    fn abandon_probe(&mut self, out: &mut Out) {
        self.release_held(out);
        if let Phase::Prompt(prompt) = &mut self.phase {
            prompt.probe = Probe::Idle;
            prompt.quarantine();
            prompt.line = Line::Unknown;
        }
        self.pump_due = true;
    }

    fn mark(&mut self, now: Instant, mark: Mark, out: &mut Out) {
        match mark {
            Mark::Prompt { flags } => {
                self.release_held(out);
                if let Phase::Prompt(old) = &mut self.phase {
                    // Keys kept back at the old prompt were meant for the
                    // shell, which is now at a new one (after Ctrl-C, say).
                    self.typeahead |= !old.input.is_empty();
                    out.to_shell.append(&mut old.input);
                }
                // With nothing typed ahead for it, a prompt's line is empty.
                let empty = !std::mem::take(&mut self.typeahead);
                self.phase = Phase::Prompt(Box::new(Prompt::new(&flags, empty)));
                self.submitted = None;
                out.events.push(Event::PromptStarted);
            }
            Mark::Run => self.command_started(out),
            Mark::Report { payload } => self.reported(now, &payload),
            Mark::Garbled => {
                if self.probing() {
                    self.release_held(out);
                    if let Phase::Prompt(prompt) = &mut self.phase {
                        prompt.probe = Probe::Idle;
                        prompt.quarantine();
                        prompt.line = Line::Unknown;
                    }
                }
            }
        }
    }

    fn command_started(&mut self, out: &mut Out) {
        self.release_held(out);
        self.erase(out);
        let line = match &mut self.phase {
            Phase::Prompt(prompt) => {
                // Keys kept back were meant for the shell.
                self.typeahead |= !prompt.input.is_empty();
                out.to_shell.append(&mut prompt.input);
                match &prompt.line {
                    Line::Known { text, .. } => Some(text.clone()),
                    Line::Unknown => None,
                }
            }
            Phase::Away => self.submitted.take(),
        };
        self.phase = Phase::Away;
        out.events.push(Event::CommandStarted { line });
    }

    /// The probe's answer.
    fn reported(&mut self, now: Instant, payload: &[u8]) {
        let Phase::Prompt(prompt) = &mut self.phase else {
            return;
        };
        if !matches!(prompt.probe, Probe::Waiting { .. }) {
            return;
        }
        match parse_report(payload) {
            Ok(line) => prompt.line = line,
            Err(Unreadable::NotText) => prompt.line = Line::Unknown,
            Err(Unreadable::Malformed) => {
                prompt.line = Line::Unknown;
                prompt.quarantine();
            }
        }
        prompt.probe = Probe::Settling {
            until: after(now, SETTLE),
            limit: after(now, SETTLE_LIMIT),
            report_at: self.held.len(),
        };
    }

    /// With the line known and the cursor at its end, readline's redraw ends
    /// with the line itself, so when the held output ends that way there is
    /// nothing more to wait for. Where this cannot tell (an empty line, a
    /// cursor inside the line, a line that wraps), the wait is a short time.
    fn redraw_done(&self) -> bool {
        let Phase::Prompt(prompt) = &self.phase else {
            return false;
        };
        let Probe::Settling { report_at, .. } = prompt.probe else {
            return false;
        };
        let Line::Known { text, at_end: true } = &prompt.line else {
            return false;
        };
        !text.is_empty()
            && !text.chars().any(char::is_control)
            && self
                .held
                .get(report_at..)
                .is_some_and(|redraw| redraw.ends_with(text.as_bytes()))
    }

    /// The report is in and the redraw has settled: show what was held, draw
    /// the suggestion, and go on with the keys that waited.
    fn finish_probe(&mut self, now: Instant, tty: &dyn Tty, out: &mut Out) {
        debug_assert!(self.probing(), "only a probe that is out is finished");
        self.release_held(out);
        if let Phase::Prompt(prompt) = &mut self.phase {
            prompt.probe = Probe::Idle;
        }
        if let Some((whole, visible)) = self.choose() {
            self.erase(out);
            render::draw(&visible, &self.style, &mut out.to_terminal);
            self.shown = Some(whole);
        }
        self.pump(now, tty, out);
    }

    /// No answer came.
    fn give_up(&mut self, now: Instant, tty: &dyn Tty, out: &mut Out) {
        self.release_held(out);
        if let Phase::Prompt(prompt) = &mut self.phase {
            prompt.probe = Probe::Idle;
            prompt.quarantine();
            prompt.line = Line::Unknown;
        }
        self.pump(now, tty, out);
    }

    /// What to suggest for the line as it stands: the whole rest of the
    /// command, and as much of it as fits on the row.
    fn choose(&self) -> Option<(String, String)> {
        let Phase::Prompt(prompt) = &self.phase else {
            return None;
        };
        if prompt.disabled || prompt.quarantined() {
            return None;
        }
        let Line::Known { text, at_end: true } = &prompt.line else {
            return None;
        };
        self.suggestion_for(text, self.column.room()?)
    }

    /// The start of a key has waited long enough: it is not going to be
    /// finished. It goes on as it is, and nothing is known after it.
    fn stalled_key(&mut self, now: Instant, out: &mut Out) {
        let Phase::Prompt(prompt) = &mut self.phase else {
            return;
        };
        let Some(since) = prompt.input_since else {
            return;
        };
        let stalled = !prompt.input.is_empty()
            && now >= after(since, PARTIAL_HOLD)
            && matches!(keys::next(&prompt.input), Token::Incomplete);
        if stalled {
            out.to_shell.append(&mut prompt.input);
            prompt.input_since = None;
            prompt.line = Line::Unknown;
            prompt.quarantine();
        }
    }
}

/// Why a report could not be used.
enum Unreadable {
    /// The line is not text; the report itself was in order.
    NotText,
    /// The report was not in the form the shell writes.
    Malformed,
}

/// `POINT;LENGTH;QUOTED`.
fn parse_report(payload: &[u8]) -> Result<Line, Unreadable> {
    let mut fields = payload.splitn(3, |b| *b == b';');
    let number = |field: Option<&[u8]>| {
        field
            .and_then(|digits| std::str::from_utf8(digits).ok())
            .and_then(|digits| digits.parse::<usize>().ok())
            .ok_or(Unreadable::Malformed)
    };
    let point = number(fields.next())?;
    let length = number(fields.next())?;
    let quoted = fields.next().ok_or(Unreadable::Malformed)?;
    let bytes = quote::unquote(quoted).map_err(|_| Unreadable::Malformed)?;
    let text = String::from_utf8(bytes).map_err(|_| Unreadable::NotText)?;
    Ok(Line::Known {
        text,
        at_end: point == length,
    })
}
