//! Reading what `printf %q` wrote.
//!
//! The shell reports the line being edited in the shell's own quoting, made
//! by one builtin call, because the encodings that need a loop in bash cost
//! far more than the whole rest of the probe: base64 written with builtins
//! alone takes about 5.5 microseconds a character (0.7 ms for a 120 character
//! line), `%q` about 14 microseconds for the same line. `%q` is as safe: it
//! never writes a raw control byte, so the report cannot end early or be
//! mistaken for something the terminal acts on.
//!
//! `%q` has three shapes, and this reads all three. Nothing to quote at all
//! is `''`. Plain text with problem characters is written with a backslash
//! before each. Text with a control character in it is written whole in
//! ANSI-C form, `$'...'`, with `\n`, `\t`, `\E`, octal escapes and so on.
//! Bytes from 0x80 up are copied as they are (a multibyte character) or
//! written as octal escapes (a byte that is not part of one).

use std::fmt;

/// Why a quoted line could not be read back.
#[derive(Debug, PartialEq, Eq)]
pub enum QuoteError {
    /// A backslash at the very end.
    DanglingBackslash,
    /// An escape this reader does not know.
    UnknownEscape(u8),
    /// `$'` without its closing quote.
    Unterminated,
}

impl fmt::Display for QuoteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DanglingBackslash => write!(f, "a backslash ends the text"),
            Self::UnknownEscape(byte) => write!(f, "unknown escape \\{}", char::from(*byte)),
            Self::Unterminated => write!(f, "the quoted text is not closed"),
        }
    }
}

impl std::error::Error for QuoteError {}

/// The bytes that `printf %q` quoted into `input`.
///
/// # Errors
///
/// When `input` is not something `%q` writes.
pub fn unquote(input: &[u8]) -> Result<Vec<u8>, QuoteError> {
    if input == b"''" {
        return Ok(Vec::new());
    }
    match input.strip_prefix(b"$'") {
        Some(rest) => match rest.strip_suffix(b"'") {
            Some(body) => ansi_c(body),
            None => Err(QuoteError::Unterminated),
        },
        None => backslashed(input),
    }
}

/// Plain text where a backslash means "the next byte, as it is".
fn backslashed(input: &[u8]) -> Result<Vec<u8>, QuoteError> {
    let mut out = Vec::with_capacity(input.len());
    let mut bytes = input.iter().copied();
    while let Some(byte) = bytes.next() {
        if byte == b'\\' {
            out.push(bytes.next().ok_or(QuoteError::DanglingBackslash)?);
        } else {
            out.push(byte);
        }
    }
    Ok(out)
}

/// The inside of `$'...'`.
fn ansi_c(body: &[u8]) -> Result<Vec<u8>, QuoteError> {
    let mut out = Vec::with_capacity(body.len());
    let mut at = 0;
    while let Some(&byte) = body.get(at) {
        at = at.saturating_add(1);
        if byte != b'\\' {
            out.push(byte);
            continue;
        }
        let escape = *body.get(at).ok_or(QuoteError::DanglingBackslash)?;
        at = at.saturating_add(1);
        match simple_escape(escape) {
            Some(plain) => out.push(plain),
            None => at = numeric_escape(body, at, escape, &mut out)?,
        }
    }
    Ok(out)
}

/// The escapes that stand for one fixed byte.
fn simple_escape(escape: u8) -> Option<u8> {
    Some(match escape {
        b'a' => 0x07,
        b'b' => 0x08,
        b'e' | b'E' => 0x1b,
        b'f' => 0x0c,
        b'n' => b'\n',
        b'r' => b'\r',
        b't' => b'\t',
        b'v' => 0x0b,
        b'\\' | b'\'' | b'"' | b'?' => escape,
        _ => return None,
    })
}

/// The escapes with digits after them: `\NNN` octal and `\xHH` hexadecimal.
/// Returns where reading goes on.
fn numeric_escape(
    body: &[u8],
    at: usize,
    escape: u8,
    out: &mut Vec<u8>,
) -> Result<usize, QuoteError> {
    let (radix, most): (u32, usize) = match escape {
        b'0'..=b'7' => (8, 2),
        b'x' => (16, 2),
        other => return Err(QuoteError::UnknownEscape(other)),
    };
    let mut value = if radix == 8 {
        u32::from(escape.saturating_sub(b'0'))
    } else {
        0
    };
    let mut taken = 0;
    let mut next = at;
    while taken < most {
        let Some(digit) = body.get(next).and_then(|b| char::from(*b).to_digit(radix)) else {
            break;
        };
        value = value.saturating_mul(radix).saturating_add(digit);
        next = next.saturating_add(1);
        taken = taken.saturating_add(1);
    }
    if radix == 16 && taken == 0 {
        return Err(QuoteError::UnknownEscape(escape));
    }
    out.push(u8::try_from(value & 0xff).unwrap_or(0));
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_is_two_quotes() {
        assert_eq!(unquote(b"''"), Ok(Vec::new()));
    }

    #[test]
    fn plain_text_is_itself() {
        assert_eq!(unquote(b"git"), Ok(b"git".to_vec()));
    }

    #[test]
    fn a_backslash_gives_the_next_byte_as_it_is() {
        assert_eq!(
            unquote(b"ls\\ -la\\;\\ echo\\ \\'hi\\'"),
            Ok(b"ls -la; echo 'hi'".to_vec())
        );
    }

    #[test]
    fn a_backslash_at_the_end_is_an_error() {
        assert_eq!(unquote(b"ab\\"), Err(QuoteError::DanglingBackslash));
    }

    #[test]
    fn ansi_c_text_has_its_escapes_read() {
        assert_eq!(
            unquote(b"$'a\\Eb\\ac\\td\\ne\\\\f\\'g'"),
            Ok(b"a\x1bb\x07c\td\ne\\f'g".to_vec())
        );
    }

    #[test]
    fn octal_escapes_are_bytes() {
        assert_eq!(
            unquote(b"$'\\303\\251\\001\\177'"),
            Ok(vec![0xc3, 0xa9, 0x01, 0x7f])
        );
        assert_eq!(unquote(b"$'\\0A'"), Ok(vec![0, b'A']));
    }

    #[test]
    fn hexadecimal_escapes_are_bytes() {
        assert_eq!(unquote(b"$'\\x1b\\x7'"), Ok(vec![0x1b, 0x07]));
        assert_eq!(unquote(b"$'\\xZ'"), Err(QuoteError::UnknownEscape(b'x')));
    }

    #[test]
    fn multibyte_text_is_copied() {
        let text = "echo \u{e9}\u{65e5}";
        let mut quoted = b"$'".to_vec();
        quoted.extend_from_slice(text.as_bytes());
        quoted.extend_from_slice(b"\\t'");
        assert_eq!(unquote(&quoted), Ok(format!("{text}\t").into_bytes()));
    }

    #[test]
    fn an_unclosed_ansi_c_text_is_an_error() {
        assert_eq!(unquote(b"$'abc"), Err(QuoteError::Unterminated));
    }

    #[test]
    fn an_unknown_escape_is_an_error() {
        assert_eq!(unquote(b"$'\\q'"), Err(QuoteError::UnknownEscape(b'q')));
    }
}
