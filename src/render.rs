//! The bytes that draw and erase a suggestion.
//!
//! A suggestion is drawn after the cursor and the cursor is put back, so the
//! shell never knows. The cursor and its attributes are saved with `ESC 7`
//! and brought back with `ESC 8`; in between, automatic wrapping is turned
//! off so that a column miscounted by one cannot scroll the screen, and the
//! text is written in a dim style. Erasing is "clear to the end of the row":
//! the suggestion is the last thing on its row, because it is only drawn when
//! the cursor is at the end of the line.

/// Clear from the cursor to the end of the row.
pub const ERASE: &[u8] = b"\x1b[K";

/// How a suggestion looks: the parameters of an SGR sequence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Style {
    params: String,
}

impl Style {
    /// Dim, which takes the colour of the text around it.
    #[must_use]
    pub fn dim() -> Self {
        Self {
            params: "2".to_owned(),
        }
    }

    /// A style from the digits and semicolons of an SGR sequence, such as
    /// `2` or `38;5;244`. Anything else is refused: this ends up in the
    /// middle of an escape sequence.
    #[must_use]
    pub fn parse(spec: &str) -> Option<Self> {
        let fine = !spec.is_empty()
            && spec.len() <= 40
            && spec.bytes().all(|b| b.is_ascii_digit() || b == b';');
        fine.then(|| Self {
            params: spec.to_owned(),
        })
    }
}

/// Append to `out` the bytes that draw `text` after the cursor and leave the
/// cursor where it was.
pub fn draw(text: &str, style: &Style, out: &mut Vec<u8>) {
    out.extend_from_slice(b"\x1b7\x1b[?7l\x1b[");
    out.extend_from_slice(style.params.as_bytes());
    out.push(b'm');
    out.extend_from_slice(text.as_bytes());
    out.extend_from_slice(b"\x1b[22m\x1b[?7h\x1b8");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_suggestion_is_drawn_between_a_save_and_a_restore() {
        let mut out = Vec::new();
        draw("tus", &Style::dim(), &mut out);
        assert_eq!(out, b"\x1b7\x1b[?7l\x1b[2mtus\x1b[22m\x1b[?7h\x1b8");
    }

    #[test]
    fn styles_are_digits_and_semicolons() {
        assert!(Style::parse("2").is_some());
        assert!(Style::parse("38;5;244").is_some());
        assert!(Style::parse("").is_none());
        assert!(Style::parse("2m\x1b[31").is_none());
        assert!(Style::parse(&"1;".repeat(30)).is_none());
    }
}
