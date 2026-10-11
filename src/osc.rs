//! The shell's private marks, taken out of its output.
//!
//! The shell writes three kinds of mark, each an operating system command
//! the terminal would ignore anyway, but which it never gets to see:
//!
//! ```text
//! ESC ] 7777 ; NONCE ; P ; FLAGS BEL            a prompt is about to be drawn
//! ESC ] 7777 ; NONCE ; R BEL                    a command is about to run
//! ESC ] 7777 ; NONCE ; L ; POINT ; LENGTH ; Q BEL   the line being edited
//! ```
//!
//! NONCE is a random token only this session's shell was given, so a file
//! that holds marks from an earlier session, shown with `cat`, changes
//! nothing. A sequence that starts like a mark and turns out not to be one is
//! handed on exactly as it came.
//!
//! The filter works on whatever chunks the pseudo-terminal delivers. A chunk
//! may end anywhere, so the few bytes that might be the start of a mark are
//! held back until the next chunk says what they are; [`Filter::flush`] gives
//! them up when no next chunk comes.

use memchr::{memchr, memchr2};

/// Longest payload kept. A line longer than this is dropped, and the probe
/// that asked for it counts as failed.
const MAX_PAYLOAD: usize = 1 << 20;

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;

/// What the shell said.
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum Mark {
    /// A prompt is about to be drawn. The flags are single letters: `v` for
    /// the vi editing mode.
    Prompt { flags: Vec<u8> },
    /// A command is about to run.
    Run,
    /// The line being edited, still in the shell's quoting: the cursor
    /// position, the length (both in the shell's own units), and the quoted
    /// text, separated by semicolons.
    Report { payload: Vec<u8> },
    /// Something that started as a mark and was not a whole one.
    Garbled,
}

/// A mark, and where in the clean output it came.
#[derive(Debug, PartialEq, Eq)]
pub struct Found {
    /// The length of the clean output when the mark ended.
    pub at: usize,
    pub mark: Mark,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Between marks.
    Ground,
    /// Matched this many bytes of the header.
    Header(usize),
    /// Inside a mark, after its header.
    Body,
    /// Inside a mark, just after an ESC that may be the start of ST.
    BodyEscape,
}

/// Takes the marks out of a stream.
#[derive(Debug)]
pub struct Filter {
    header: Vec<u8>,
    state: State,
    payload: Vec<u8>,
    dropped: bool,
}

impl Filter {
    /// A filter for the marks that carry `nonce`.
    #[must_use]
    pub fn new(nonce: &str) -> Self {
        let mut header = b"\x1b]7777;".to_vec();
        header.extend_from_slice(nonce.as_bytes());
        header.push(b';');
        Self {
            header,
            state: State::Ground,
            payload: Vec::new(),
            dropped: false,
        }
    }

    /// Whether bytes are being held back, waiting to learn what they are.
    #[must_use]
    pub fn holding(&self) -> bool {
        matches!(self.state, State::Header(held) if held > 0)
    }

    /// Hand on, as they are, the bytes being held back.
    pub fn flush(&mut self, clean: &mut Vec<u8>) {
        if let State::Header(held) = self.state {
            if let Some(bytes) = self.header.get(..held) {
                clean.extend_from_slice(bytes);
            }
            self.state = State::Ground;
        }
    }

    /// Take `input`: what is not a mark goes to `clean`, what is goes to
    /// `found`.
    pub fn feed(&mut self, input: &[u8], clean: &mut Vec<u8>, found: &mut Vec<Found>) {
        let mut at = 0;
        // Every step takes a byte, or changes the state so that the next one
        // does: at most two steps for each byte of input.
        for _ in 0..input.len().saturating_mul(4).saturating_add(4) {
            if at >= input.len() {
                break;
            }
            at = match self.state {
                State::Ground => Self::ground(&mut self.state, input, at, clean),
                State::Header(held) => self.header_step(input, at, held, clean),
                State::Body | State::BodyEscape => self.body(input, at, clean.len(), found),
            };
        }
    }

    /// Outside a mark: everything up to the next ESC is clean.
    fn ground(state: &mut State, input: &[u8], at: usize, clean: &mut Vec<u8>) -> usize {
        let rest = input.get(at..).unwrap_or_default();
        match memchr(ESC, rest) {
            None => {
                clean.extend_from_slice(rest);
                input.len()
            }
            Some(offset) => {
                let escape = at.saturating_add(offset);
                clean.extend_from_slice(input.get(at..escape).unwrap_or_default());
                *state = State::Header(0);
                escape
            }
        }
    }

    /// Matching the header, byte by byte.
    fn header_step(&mut self, input: &[u8], at: usize, held: usize, clean: &mut Vec<u8>) -> usize {
        let mut matched = held;
        let mut next = at;
        while let Some(&byte) = input.get(next) {
            if self.header.get(matched) != Some(&byte) {
                // Not a mark. What matched so far was only ordinary output.
                if let Some(bytes) = self.header.get(..matched) {
                    clean.extend_from_slice(bytes);
                }
                self.state = State::Ground;
                return next;
            }
            matched = matched.saturating_add(1);
            next = next.saturating_add(1);
            if matched == self.header.len() {
                self.state = State::Body;
                self.payload.clear();
                self.dropped = false;
                return next;
            }
        }
        self.state = State::Header(matched);
        next
    }

    /// Inside a mark: everything up to BEL, or ESC and a backslash, is its
    /// payload.
    fn body(&mut self, input: &[u8], at: usize, clean_len: usize, found: &mut Vec<Found>) -> usize {
        if self.state == State::BodyEscape {
            return self.after_escape(input, at, clean_len, found);
        }
        let rest = input.get(at..).unwrap_or_default();
        let Some(offset) = memchr2(BEL, ESC, rest) else {
            self.keep(rest);
            return input.len();
        };
        self.keep(rest.get(..offset).unwrap_or_default());
        let end = at.saturating_add(offset);
        if rest.get(offset) == Some(&BEL) {
            self.finish(clean_len, found);
        } else {
            self.state = State::BodyEscape;
        }
        end.saturating_add(1)
    }

    /// The byte after an ESC inside a mark: a backslash ends it.
    fn after_escape(
        &mut self,
        input: &[u8],
        at: usize,
        clean_len: usize,
        found: &mut Vec<Found>,
    ) -> usize {
        if input.get(at) == Some(&b'\\') {
            self.finish(clean_len, found);
            return at.saturating_add(1);
        }
        // A raw ESC never occurs inside a real mark.
        self.state = State::Ground;
        found.push(Found {
            at: clean_len,
            mark: Mark::Garbled,
        });
        at
    }

    fn keep(&mut self, bytes: &[u8]) {
        if self.payload.len().saturating_add(bytes.len()) > MAX_PAYLOAD {
            self.dropped = true;
        } else if !self.dropped {
            self.payload.extend_from_slice(bytes);
        }
    }

    fn finish(&mut self, clean_len: usize, found: &mut Vec<Found>) {
        self.state = State::Ground;
        let mark = if self.dropped {
            Mark::Garbled
        } else {
            parse(&self.payload)
        };
        found.push(Found {
            at: clean_len,
            mark,
        });
    }
}

/// The meaning of a payload: its first byte says which mark it is.
fn parse(payload: &[u8]) -> Mark {
    let fields = payload.get(2..).unwrap_or_default();
    match payload.first() {
        Some(b'P') => Mark::Prompt {
            flags: fields.to_vec(),
        },
        Some(b'R') => Mark::Run,
        Some(b'L') => Mark::Report {
            payload: fields.to_vec(),
        },
        _ => Mark::Garbled,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONCE: &str = "00ff00ff00ff00ff";

    fn mark(body: &str) -> Vec<u8> {
        format!("\x1b]7777;{NONCE};{body}\x07").into_bytes()
    }

    /// Feed `input` in the given pieces; what is clean, what was found.
    fn run(pieces: &[&[u8]]) -> (Vec<u8>, Vec<Found>) {
        let mut filter = Filter::new(NONCE);
        let mut clean = Vec::new();
        let mut found = Vec::new();
        for piece in pieces {
            let before = clean.len();
            let mut part = Vec::new();
            let mut marks = Vec::new();
            filter.feed(piece, &mut part, &mut marks);
            clean.extend_from_slice(&part);
            found.extend(marks.into_iter().map(|f| Found {
                at: f.at.saturating_add(before),
                mark: f.mark,
            }));
        }
        filter.flush(&mut clean);
        (clean, found)
    }

    #[test]
    fn ordinary_output_is_untouched() {
        let (clean, found) = run(&[b"hello \x1b[1mworld\x1b[0m\r\n"]);
        assert_eq!(clean, b"hello \x1b[1mworld\x1b[0m\r\n");
        assert_eq!(found.len(), 0);
    }

    #[test]
    fn a_mark_is_taken_out_and_placed() {
        let mut input = b"ab".to_vec();
        input.extend(mark("P;v"));
        input.extend_from_slice(b"cd");
        let (clean, found) = run(&[&input]);
        assert_eq!(clean, b"abcd");
        assert_eq!(
            found,
            vec![Found {
                at: 2,
                mark: Mark::Prompt {
                    flags: b"v".to_vec()
                }
            }]
        );
    }

    #[test]
    fn every_kind_is_read() {
        let mut input = mark("R");
        input.extend(mark("L;3;3;abc"));
        input.extend(mark("P;"));
        let (clean, found) = run(&[&input]);
        assert_eq!(clean.len(), 0);
        let kinds: Vec<Mark> = found.into_iter().map(|f| f.mark).collect();
        assert_eq!(
            kinds,
            vec![
                Mark::Run,
                Mark::Report {
                    payload: b"3;3;abc".to_vec()
                },
                Mark::Prompt { flags: Vec::new() }
            ]
        );
    }

    #[test]
    fn a_mark_split_anywhere_is_still_found() {
        let mut input = b"x".to_vec();
        input.extend(mark("L;1;1;a"));
        input.extend_from_slice(b"y");
        for cut in 0..=input.len() {
            let (head, tail) = input.split_at(cut);
            let (clean, found) = run(&[head, tail]);
            assert_eq!(clean, b"xy", "cut at {cut}");
            assert_eq!(found.len(), 1, "cut at {cut}");
        }
    }

    #[test]
    fn a_mark_cut_into_single_bytes_is_still_found() {
        let mut input = b"x".to_vec();
        input.extend(mark("R"));
        input.extend_from_slice(b"y");
        let pieces: Vec<&[u8]> = input.chunks(1).collect();
        let (clean, found) = run(&pieces);
        assert_eq!(clean, b"xy");
        assert_eq!(found.len(), 1);
    }

    #[test]
    fn a_mark_with_another_nonce_is_ordinary_output() {
        let other = b"\x1b]7777;1111111111111111;R\x07".to_vec();
        let (clean, found) = run(&[&other]);
        assert_eq!(clean, other);
        assert_eq!(found.len(), 0);
    }

    #[test]
    fn other_sequences_that_start_alike_are_untouched() {
        let input = b"\x1b]0;title\x07\x1b]77\x1b[31mred\x1b]7778;x\x07".to_vec();
        for cut in 0..=input.len() {
            let (head, tail) = input.split_at(cut);
            let (clean, found) = run(&[head, tail]);
            assert_eq!(clean, input, "cut at {cut}");
            assert_eq!(found.len(), 0);
        }
    }

    #[test]
    fn the_string_terminator_ends_a_mark_too() {
        let input = format!("a\x1b]7777;{NONCE};R\x1b\\b").into_bytes();
        let (clean, found) = run(&[&input]);
        assert_eq!(clean, b"ab");
        assert_eq!(found.len(), 1);
    }

    #[test]
    fn held_bytes_are_given_up_on_flush() {
        let mut filter = Filter::new(NONCE);
        let mut clean = Vec::new();
        let mut found = Vec::new();
        filter.feed(b"ok\x1b]77", &mut clean, &mut found);
        assert_eq!(clean, b"ok");
        assert!(filter.holding());
        filter.flush(&mut clean);
        assert_eq!(clean, b"ok\x1b]77");
        assert!(!filter.holding());
    }

    #[test]
    fn an_escape_inside_a_mark_makes_it_garbled_and_is_not_lost() {
        let input = format!("\x1b]7777;{NONCE};L;1;1;a\x1b[0mz").into_bytes();
        let (clean, found) = run(&[&input]);
        assert_eq!(found.len(), 1);
        assert_eq!(found.first().map(|f| &f.mark), Some(&Mark::Garbled));
        assert_eq!(clean, b"[0mz");
    }

    #[test]
    fn a_payload_past_the_limit_is_dropped_but_the_stream_goes_on() {
        let mut input = format!("\x1b]7777;{NONCE};L;0;0;").into_bytes();
        input.resize(input.len() + MAX_PAYLOAD + 10, b'a');
        input.push(BEL);
        input.extend_from_slice(b"after");
        let (clean, found) = run(&[&input]);
        assert_eq!(clean, b"after");
        assert_eq!(found.first().map(|f| &f.mark), Some(&Mark::Garbled));
    }
}
