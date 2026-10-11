//! Small questions about text: how wide it is on a screen, where its next
//! word ends, whether it is fit to be typed for the person.

use unicode_width::UnicodeWidthChar;

/// The columns a character takes on a terminal. Control characters take none.
#[must_use]
pub fn width(c: char) -> usize {
    c.width().unwrap_or(0)
}

/// The columns a string takes.
#[must_use]
pub fn str_width(text: &str) -> usize {
    text.chars().map(width).fold(0, usize::saturating_add)
}

/// Whether `text` can be offered as a suggestion and typed for the person:
/// not empty, and with nothing in it that would be a key of its own when
/// typed (a newline would run the line, a tab would complete it, an escape
/// would start a sequence).
#[must_use]
pub fn is_typeable(text: &str) -> bool {
    !text.trim().is_empty() && !text.chars().any(char::is_control)
}

/// The longest start of `text` that fits in `columns` columns. When `text`
/// is cut, the last column is given to an ellipsis so the person can see
/// there is more; the whole of `text` is still what accepting inserts.
///
/// Returns what to draw, or `None` if not even one column fits.
#[must_use]
pub fn fit(text: &str, columns: usize) -> Option<String> {
    if columns == 0 {
        return None;
    }
    if str_width(text) <= columns {
        return Some(text.to_owned());
    }
    // Room for the ellipsis, which is one column wide.
    let room = columns.saturating_sub(1);
    let mut used: usize = 0;
    let mut kept = String::new();
    for c in text.chars() {
        let next = used.saturating_add(width(c));
        if next > room {
            break;
        }
        used = next;
        kept.push(c);
    }
    kept.push('\u{2026}');
    Some(kept)
}

/// Whether `c` belongs to a word as the shell's own word motions see it:
/// letters and digits.
fn is_word(c: char) -> bool {
    c.is_alphanumeric()
}

/// Where the next word of `text` ends, as a byte offset: past anything that
/// is not part of a word, then past the word. The same reach as Alt-F has in
/// readline.
#[must_use]
pub fn word_end(text: &str) -> usize {
    let mut end = 0;
    let mut in_word = false;
    for (at, c) in text.char_indices() {
        if is_word(c) {
            in_word = true;
        } else if in_word {
            return at;
        }
        end = at.saturating_add(c.len_utf8());
    }
    end
}

/// Where the next whitespace-delimited word of `text` ends, taking the blanks
/// on both sides of it, as a byte offset.
#[must_use]
pub fn shell_word_end(text: &str) -> usize {
    #[derive(Clone, Copy)]
    enum Phase {
        Leading,
        Word,
        Trailing,
    }
    let mut phase = Phase::Leading;
    let mut end = 0;
    for (at, c) in text.char_indices() {
        phase = match (phase, c.is_whitespace()) {
            (Phase::Leading, true) => Phase::Leading,
            (Phase::Leading | Phase::Word, false) => Phase::Word,
            (Phase::Word | Phase::Trailing, true) => Phase::Trailing,
            (Phase::Trailing, false) => return at,
        };
        end = at.saturating_add(c.len_utf8());
    }
    end
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widths_follow_the_terminal() {
        assert_eq!(str_width("abc"), 3);
        assert_eq!(str_width("\u{65e5}\u{672c}"), 4);
        assert_eq!(str_width("e\u{301}"), 1);
        assert_eq!(str_width(""), 0);
    }

    #[test]
    fn only_plain_text_is_typeable() {
        assert!(is_typeable("git status -sb"));
        assert!(is_typeable("echo \u{e9}\u{65e5}"));
        assert!(!is_typeable(""));
        assert!(!is_typeable("   "));
        assert!(!is_typeable("a\nb"));
        assert!(!is_typeable("a\tb"));
        assert!(!is_typeable("a\x1bb"));
        assert!(!is_typeable("a\x7fb"));
    }

    #[test]
    fn text_that_fits_is_whole() {
        assert_eq!(fit("abc", 3).as_deref(), Some("abc"));
        assert_eq!(fit("abc", 10).as_deref(), Some("abc"));
    }

    #[test]
    fn text_that_does_not_fit_is_cut_with_an_ellipsis() {
        assert_eq!(fit("abcdef", 4).as_deref(), Some("abc\u{2026}"));
        assert_eq!(fit("abcdef", 1).as_deref(), Some("\u{2026}"));
        assert_eq!(fit("abcdef", 0), None);
    }

    #[test]
    fn a_wide_character_is_never_split() {
        // Three columns of room, two for text: the first wide one fits, the
        // second would run past.
        assert_eq!(
            fit("\u{65e5}\u{672c}x", 3).as_deref(),
            Some("\u{65e5}\u{2026}")
        );
    }

    #[test]
    fn words_end_where_readline_ends_them() {
        assert_eq!(word_end("ommit -m"), 5);
        assert_eq!(word_end(" -m \"fix\""), 3);
        assert_eq!(word_end("/usr/local"), 4);
        assert_eq!(word_end("a"), 1);
        assert_eq!(word_end(" -- "), 4);
        assert_eq!(word_end(""), 0);
    }

    #[test]
    fn shell_words_take_their_blanks() {
        assert_eq!(shell_word_end("commit -m x"), 7);
        assert_eq!(shell_word_end("  commit  -m"), 10);
        assert_eq!(shell_word_end("last"), 4);
        assert_eq!(shell_word_end("   "), 3);
        assert_eq!(shell_word_end(""), 0);
    }
}
