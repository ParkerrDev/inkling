//! Reading the keys on their way to the shell.
//!
//! The proxy forwards every byte it is given. Alongside, it reads the stream
//! as keys, because what may be done next depends on what was just pressed:
//!
//! * A probe can be sent only when the shell is certain to be in its plain
//!   editing state, and the keys that leave it in any other (a search, a
//!   prefix, a quoted insert, a program that has taken the terminal) are told
//!   apart from the ones that do not.
//! * A probe is a command of its own to readline, so it ends a run of
//!   commands that depend on the one before: Tab pressed twice cycles through
//!   the completions, two kills in a row are one piece of text for yanking.
//!   No probe is sent after a key that begins or continues such a run.
//! * A few keys take the suggestion.
//!
//! The classes are chosen to err toward doing nothing: a key not known to be
//! plain is never followed by a probe.

/// What an accepting key takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Accept {
    /// The whole suggestion.
    All,
    /// One word of it, as Alt-F would move over one.
    Word,
    /// One whitespace-delimited word of it, and the blanks around it.
    Shell,
}

/// What kind of key it was.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    /// A character is inserted.
    Text,
    /// A character before or after the cursor is deleted.
    Edit,
    /// The cursor moves.
    Move,
    /// A key that moves the cursor when there is nothing to take, and takes
    /// the suggestion when there is.
    Accept(Accept),
    /// The line is accepted, or may be: the shell runs it.
    Submit,
    /// Something happens to the line that cannot be told from here, and the
    /// shell is in its plain state afterwards. Tab, the kills, the history.
    Neutral,
    /// The shell may be left somewhere other than its plain state, or a
    /// program may have the terminal: a search, a prefix key, a quoted
    /// insert, an unknown escape sequence, the arrows that Atuin takes.
    Modal,
    /// Ctrl-G: whatever was going on is abandoned.
    Cancel,
    /// A key the terminal turns into a signal for the shell: interrupt,
    /// suspend or quit. Not safe to probe after until the shell has drawn a
    /// new prompt, whatever comes in between.
    Signal,
    /// The terminal begins a paste.
    PasteStart,
    /// The terminal ends a paste.
    PasteEnd,
}

impl Key {
    /// Whether the shell is certain to be in its plain editing state after
    /// this key, with nothing that depends on it being the last one.
    #[must_use]
    pub fn probe_safe(self) -> bool {
        matches!(
            self,
            Self::Text | Self::Edit | Self::Move | Self::Accept(_) | Self::PasteEnd
        )
    }
}

/// The first key of a buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Token {
    /// A key, and how many bytes it was.
    Key { key: Key, len: usize },
    /// The buffer ends inside a key; more bytes may complete it.
    Incomplete,
}

const fn key(key: Key, len: usize) -> Token {
    Token::Key { key, len }
}

/// The longest control string (a reply to a query, say) read as one key.
const MAX_STRING: usize = 4096;
/// The longest control sequence read as one key.
const MAX_SEQUENCE: usize = 32;

/// The first key in `buf`.
#[must_use]
pub fn next(buf: &[u8]) -> Token {
    let Some(&first) = buf.first() else {
        return Token::Incomplete;
    };
    match first {
        0x1b => escape(buf),
        0x20..=0x7e => key(Key::Text, 1),
        0x08 | 0x7f => key(Key::Edit, 1),
        0x00..=0x1f => key(control(first), 1),
        _ => multibyte(buf),
    }
}

/// The keys that are one control byte.
fn control(byte: u8) -> Key {
    match byte {
        // Ctrl-A, B, F: beginning of line, back, forward.
        0x01 | 0x02 | 0x06 => Key::Move,
        // Ctrl-E: end of line.
        0x05 => Key::Accept(Accept::All),
        0x07 => Key::Cancel,
        // Ctrl-J, Ctrl-M, and Ctrl-O (operate and get next).
        0x0a | 0x0d | 0x0f => Key::Submit,
        // Set mark, quoted insert, searches, Ctrl-T (taken by fzf), Ctrl-V,
        // the prefix Ctrl-X, and character search.
        0x00 | 0x11 | 0x12 | 0x13 | 0x14 | 0x16 | 0x18 | 0x1d => Key::Modal,
        // The keys the terminal turns into a signal (interrupt, suspend,
        // quit). The shell handles one after the keys that followed it have
        // been read, and a command that runs in that moment is cancelled with
        // the line, so nothing is probed until the shell is at a new prompt.
        0x03 | 0x1a | 0x1c => Key::Signal,
        _ => Key::Neutral,
    }
}

/// A character above ASCII, in UTF-8.
fn multibyte(buf: &[u8]) -> Token {
    let first = buf.first().copied().unwrap_or(0);
    let need = match first {
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        _ => return key(Key::Neutral, 1),
    };
    let have = buf.get(..need).unwrap_or(buf);
    let continuations_fine = have
        .get(1..)
        .unwrap_or_default()
        .iter()
        .all(|b| b & 0xc0 == 0x80);
    if !continuations_fine {
        return key(Key::Neutral, 1);
    }
    if have.len() < need {
        return Token::Incomplete;
    }
    match std::str::from_utf8(have)
        .ok()
        .and_then(|s| s.chars().next())
    {
        // The C1 controls are not text.
        Some(c) if u32::from(c) >= 0xa0 => key(Key::Text, need),
        _ => key(Key::Neutral, 1),
    }
}

/// Everything that begins with ESC.
fn escape(buf: &[u8]) -> Token {
    let Some(&second) = buf.get(1) else {
        return Token::Incomplete;
    };
    match second {
        b'[' => csi(buf, 2, false),
        b'O' => ss3(buf),
        b']' | b'P' | b'_' | b'^' | b'X' => control_string(buf),
        0x1b => match buf.get(2) {
            None => Token::Incomplete,
            Some(b'[') => csi(buf, 3, true),
            Some(_) => key(Key::Modal, 2),
        },
        // Alt-B and Alt-F: a word back, a word forward.
        b'b' | b'B' => key(Key::Move, 2),
        b'f' | b'F' => key(Key::Accept(Accept::Word), 2),
        // Alt-D, Alt-Backspace: kill a word.
        b'd' | b'D' | 0x7f | 0x08 => key(Key::Neutral, 2),
        // Anything else may be bound to a program that takes the terminal,
        // or begin an argument.
        _ => key(Key::Modal, 2),
    }
}

/// The modifier of `ESC [ 1 ; m X`: 3 is Alt, 5 is Ctrl.
fn modifier(params: &[u8]) -> usize {
    params.split(|b| *b == b';').nth(1).map_or(0, number)
}

fn number(digits: &[u8]) -> usize {
    digits
        .iter()
        .take_while(|b| b.is_ascii_digit())
        .fold(0usize, |acc, b| {
            acc.saturating_mul(10)
                .saturating_add(usize::from(b.saturating_sub(b'0')))
        })
}

/// `ESC [` and what follows, up to its final byte. `start` is where the
/// parameters begin; `alt` is set when ESC came before it.
fn csi(buf: &[u8], start: usize, alt: bool) -> Token {
    let rest = buf.get(start..).unwrap_or_default();
    let Some(end) = rest.iter().position(|b| (0x40..=0x7e).contains(b)) else {
        let sane = rest.len() < MAX_SEQUENCE && rest.iter().all(|b| (0x20..=0x3f).contains(b));
        return if sane {
            Token::Incomplete
        } else {
            key(Key::Neutral, start)
        };
    };
    let len = start.saturating_add(end).saturating_add(1);
    let params = rest.get(..end).unwrap_or_default();
    let last = rest.get(end).copied().unwrap_or(0);
    // Private-mode and intermediate sequences are replies and reports.
    let plain = params.iter().all(|b| b.is_ascii_digit() || *b == b';');
    let key_kind = if plain {
        csi_key(params, last, alt)
    } else {
        Key::Neutral
    };
    key(key_kind, len)
}

fn csi_key(params: &[u8], last: u8, alt: bool) -> Key {
    let modified = modifier(params);
    let first = number(params);
    match last {
        b'A' | b'B' => Key::Modal,
        b'C' if alt => Key::Accept(Accept::Shell),
        b'C' => match modified {
            0 => Key::Accept(Accept::All),
            5 => Key::Accept(Accept::Word),
            3 => Key::Accept(Accept::Shell),
            _ => Key::Move,
        },
        b'F' if modified == 0 => Key::Accept(Accept::All),
        b'D' | b'H' | b'F' => Key::Move,
        _ if alt => Key::Modal,
        b'~' => match first {
            1 | 7 => Key::Move,
            4 | 8 => Key::Accept(Accept::All),
            3 => Key::Edit,
            5 | 6 => Key::Modal,
            200 => Key::PasteStart,
            201 => Key::PasteEnd,
            _ => Key::Neutral,
        },
        _ => Key::Neutral,
    }
}

/// `ESC O X`, the keys of application cursor mode.
fn ss3(buf: &[u8]) -> Token {
    let Some(&last) = buf.get(2) else {
        return Token::Incomplete;
    };
    let kind = match last {
        b'A' | b'B' => Key::Modal,
        b'C' | b'F' => Key::Accept(Accept::All),
        b'D' | b'H' => Key::Move,
        b'M' => Key::Submit,
        _ => Key::Neutral,
    };
    key(kind, 3)
}

/// A string that ends with BEL or ST: a reply to a query, as far as keys go.
fn control_string(buf: &[u8]) -> Token {
    let body = buf.get(2..).unwrap_or_default();
    let mut at = 0;
    while let Some(&byte) = body.get(at) {
        if byte == 0x07 {
            return key(Key::Neutral, at.saturating_add(3));
        }
        if byte == 0x1b {
            return match body.get(at.saturating_add(1)) {
                Some(b'\\') => key(Key::Neutral, at.saturating_add(4)),
                Some(_) => key(Key::Neutral, at.saturating_add(2)),
                None => Token::Incomplete,
            };
        }
        at = at.saturating_add(1);
        if at > MAX_STRING {
            return key(Key::Neutral, 2);
        }
    }
    Token::Incomplete
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(bytes: &[u8]) -> Token {
        next(bytes)
    }

    fn is(bytes: &[u8], kind: Key) {
        assert_eq!(
            one(bytes),
            Token::Key {
                key: kind,
                len: bytes.len()
            },
            "{bytes:?}"
        );
    }

    #[test]
    fn text_is_text() {
        is(b"a", Key::Text);
        is(b" ", Key::Text);
        is(b"~", Key::Text);
        is("\u{e9}".as_bytes(), Key::Text);
        is("\u{65e5}".as_bytes(), Key::Text);
        is("\u{1f600}".as_bytes(), Key::Text);
    }

    #[test]
    fn a_character_cut_short_is_incomplete() {
        assert_eq!(one(&[0xe6, 0x97]), Token::Incomplete);
        assert_eq!(one(&[0xe6]), Token::Incomplete);
        assert_eq!(
            one(&[0xe6, 0x41]),
            Token::Key {
                key: Key::Neutral,
                len: 1
            }
        );
        assert_eq!(
            one(&[0xff]),
            Token::Key {
                key: Key::Neutral,
                len: 1
            }
        );
        // A C1 control written in UTF-8 is not text.
        assert_eq!(
            one(&[0xc2, 0x85]),
            Token::Key {
                key: Key::Neutral,
                len: 1
            }
        );
    }

    #[test]
    fn the_plain_editing_keys_are_safe() {
        is(&[0x7f], Key::Edit);
        is(&[0x08], Key::Edit);
        is(&[0x01], Key::Move);
        is(&[0x02], Key::Move);
        is(&[0x06], Key::Move);
        is(b"\x1b[D", Key::Move);
        is(b"\x1bOD", Key::Move);
        is(b"\x1b[H", Key::Move);
        is(b"\x1b[1~", Key::Move);
        is(b"\x1b[3~", Key::Edit);
        is(b"\x1bb", Key::Move);
        is(b"\x1b[1;5D", Key::Move);
        for kind in [Key::Text, Key::Edit, Key::Move, Key::Accept(Accept::All)] {
            assert!(kind.probe_safe());
        }
    }

    #[test]
    fn the_keys_that_take_the_suggestion() {
        is(b"\x1b[C", Key::Accept(Accept::All));
        is(b"\x1bOC", Key::Accept(Accept::All));
        is(b"\x1b[F", Key::Accept(Accept::All));
        is(b"\x1bOF", Key::Accept(Accept::All));
        is(b"\x1b[4~", Key::Accept(Accept::All));
        is(b"\x1b[8~", Key::Accept(Accept::All));
        is(&[0x05], Key::Accept(Accept::All));
        is(b"\x1bf", Key::Accept(Accept::Word));
        is(b"\x1b[1;5C", Key::Accept(Accept::Word));
        is(b"\x1b[1;3C", Key::Accept(Accept::Shell));
        is(b"\x1b\x1b[C", Key::Accept(Accept::Shell));
    }

    #[test]
    fn modified_end_does_not_take_it() {
        is(b"\x1b[1;5F", Key::Move);
        is(b"\x1b[1;2C", Key::Move);
    }

    #[test]
    fn the_keys_that_run_the_line() {
        is(b"\r", Key::Submit);
        is(b"\n", Key::Submit);
        is(&[0x0f], Key::Submit);
        is(b"\x1bOM", Key::Submit);
    }

    #[test]
    fn the_keys_with_a_run_of_their_own_are_not_safe() {
        for byte in [0x09u8, 0x0b, 0x0c, 0x0e, 0x10, 0x15, 0x17, 0x19, 0x04] {
            is(&[byte], Key::Neutral);
            assert!(!Key::Neutral.probe_safe());
        }
        is(b"\x1b[Z", Key::Neutral);
        is(b"\x1bd", Key::Neutral);
        is(b"\x1b\x7f", Key::Neutral);
    }

    #[test]
    fn the_keys_that_may_leave_the_plain_state_are_modal() {
        for byte in [0x00u8, 0x11, 0x12, 0x13, 0x14, 0x16, 0x18, 0x1d] {
            is(&[byte], Key::Modal);
        }
        // And the ones that become a signal.
        for byte in [0x03u8, 0x1a, 0x1c] {
            is(&[byte], Key::Signal);
        }
        is(b"\x1b[A", Key::Modal);
        is(b"\x1b[B", Key::Modal);
        is(b"\x1bOA", Key::Modal);
        is(b"\x1b[5~", Key::Modal);
        is(b"\x1b.", Key::Modal);
        is(b"\x1b5", Key::Modal);
        is(b"\x1bc", Key::Modal);
        assert_eq!(
            one(b"\x1b\x1bx"),
            Token::Key {
                key: Key::Modal,
                len: 2
            }
        );
    }

    #[test]
    fn a_lone_escape_waits_to_see_what_follows() {
        assert_eq!(one(b"\x1b"), Token::Incomplete);
        assert_eq!(one(b"\x1b["), Token::Incomplete);
        assert_eq!(one(b"\x1b[1;"), Token::Incomplete);
        assert_eq!(one(b"\x1bO"), Token::Incomplete);
        assert_eq!(one(b"\x1b\x1b"), Token::Incomplete);
    }

    #[test]
    fn paste_markers_are_found() {
        is(b"\x1b[200~", Key::PasteStart);
        is(b"\x1b[201~", Key::PasteEnd);
    }

    #[test]
    fn replies_from_the_terminal_are_one_neutral_key() {
        is(b"\x1b[12;40R", Key::Neutral);
        is(b"\x1b[?62;4c", Key::Neutral);
        is(b"\x1b[0n", Key::Neutral);
        is(b"\x1b]11;rgb:0000/0000/0000\x07", Key::Neutral);
        is(b"\x1b]11;rgb:0000/0000/0000\x1b\\", Key::Neutral);
        is(b"\x1bP>|kitty(0.30)\x1b\\", Key::Neutral);
        assert_eq!(one(b"\x1b]11;rgb:00"), Token::Incomplete);
    }

    #[test]
    fn a_sequence_longer_than_any_real_one_is_not_waited_for() {
        let mut long = b"\x1b[".to_vec();
        long.extend(std::iter::repeat_n(b'1', 40));
        assert_eq!(
            one(&long),
            Token::Key {
                key: Key::Neutral,
                len: 2
            }
        );
    }

    #[test]
    fn ctrl_g_cancels() {
        is(&[0x07], Key::Cancel);
    }
}
