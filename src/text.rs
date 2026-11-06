//! Small questions about text: how wide it is on a screen, where its next
//! word ends, whether it is fit to be typed for the person, and whether one
//! text starts with another but for case.
//!
//! # Case
//!
//! A letter typed in lower case fits a command that has it in either case;
//! one typed in upper case fits only itself. So "cd ~/dev/g" fits
//! "cd ~/Dev/GitHub", and "cd ~/D" does not fit "cd ~/dotfiles": a capital
//! is never typed by accident, and a lower case letter often is, by someone
//! who does not reach for Shift. Characters are compared one at a time,
//! folded to their lower case where that is a single character, so a text
//! and its folded form have as many characters.

use std::cmp::Ordering;

use unicode_width::UnicodeWidthChar;

/// How many characters of a text [`capitals`] looks at.
pub const CAPITALS: usize = 64;

/// `c` as it is compared when case does not matter: its lower case, where
/// that is one character, else itself.
#[must_use]
pub fn fold(c: char) -> char {
    if c.is_ascii() {
        return c.to_ascii_lowercase();
    }
    let mut lower = c.to_lowercase();
    match (lower.next(), lower.next()) {
        (Some(one), None) => one,
        _ => c,
    }
}

/// Whether `c` has a lower case of its own, which [`fold`] gives.
#[must_use]
pub fn is_capital(c: char) -> bool {
    fold(c) != c
}

/// Whether the character `have` fits the character `typed`: they are the
/// same, or `typed` is in lower case and `have` is that letter in upper case.
#[must_use]
pub fn fits(have: char, typed: char) -> bool {
    have == typed || (!is_capital(typed) && fold(have) == typed)
}

/// The high bit of each byte of a word.
const HIGH: u64 = 0x8080_8080_8080_8080;

/// A word of ASCII bytes with each capital put in lower case: a byte gets
/// 0x20 added where it is at least `A` and not past `Z`. No byte carries
/// into the next, since none is above 0x7f.
fn lower_word(word: u64) -> u64 {
    let from_a = word.wrapping_add(0x3f3f_3f3f_3f3f_3f3f);
    let past_z = word.wrapping_add(0x2525_2525_2525_2525);
    word | ((from_a & !past_z & HIGH) >> 2)
}

/// How many bytes `a` and `b` agree on, case aside, while both are ASCII,
/// and how they are ordered at the first ASCII byte where they do not.
/// Eight bytes at a time first, the order of big-endian words being that
/// of their bytes. Stops at the end of either or at the first byte that is
/// not ASCII in one of them, so what is left starts on a character in each.
fn ascii_folded(a: &[u8], b: &[u8]) -> (usize, Option<Ordering>) {
    let mut same: usize = 0;
    let (words, _) = a.as_chunks::<8>();
    let (others, _) = b.as_chunks::<8>();
    for (&p, &q) in words.iter().zip(others) {
        let (p, q) = (u64::from_be_bytes(p), u64::from_be_bytes(q));
        if (p | q) & HIGH != 0 {
            break;
        }
        let (p, q) = (lower_word(p), lower_word(q));
        if p != q {
            return (same, Some(p.cmp(&q)));
        }
        same = same.saturating_add(8);
    }
    let rest = a.get(same..).unwrap_or_default();
    let others = b.get(same..).unwrap_or_default();
    for (p, q) in rest.iter().zip(others) {
        if !p.is_ascii() || !q.is_ascii() {
            break;
        }
        match p.to_ascii_lowercase().cmp(&q.to_ascii_lowercase()) {
            Ordering::Equal => same = same.saturating_add(1),
            unequal => return (same, Some(unequal)),
        }
    }
    (same, None)
}

/// `a` and `b` compared by their folded characters alone.
#[must_use]
pub fn fold_cmp(a: &str, b: &str) -> Ordering {
    let (same, order) = ascii_folded(a.as_bytes(), b.as_bytes());
    if let Some(order) = order {
        return order;
    }
    let (Some(a), Some(b)) = (a.get(same..), b.get(same..)) else {
        return a.cmp(b);
    };
    a.chars().map(fold).cmp(b.chars().map(fold))
}

/// The order commands are kept in: by their folded characters, and those
/// that are the same but for case by their own. The commands that start
/// with a text, case aside, are then one run of the order.
#[must_use]
pub fn key_cmp(a: &str, b: &str) -> Ordering {
    fold_cmp(a, b).then_with(|| a.cmp(b))
}

/// Whether `text` starts with `prefix`, case aside.
#[must_use]
pub fn starts_folded(text: &str, prefix: &str) -> bool {
    let (same, order) = ascii_folded(text.as_bytes(), prefix.as_bytes());
    if order.is_some() {
        return false;
    }
    if same >= prefix.len() {
        return true;
    }
    let (Some(text), Some(prefix)) = (text.get(same..), prefix.get(same..)) else {
        return false;
    };
    let mut have = text.chars();
    prefix
        .chars()
        .all(|typed| have.next().is_some_and(|c| fold(c) == fold(typed)))
}

/// How a text starts with what was typed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Start {
    /// Exactly.
    Exact,
    /// Where letters were typed in lower case, it has some in upper case.
    Loose,
    /// Not at all.
    No,
}

/// How `text` starts with `typed` (see [`Start`]).
#[must_use]
pub fn start(text: &str, typed: &str) -> Start {
    if text.starts_with(typed) {
        return Start::Exact;
    }
    let mut have = text.chars();
    let loose = typed
        .chars()
        .all(|t| have.next().is_some_and(|c| fits(c, t)));
    if loose { Start::Loose } else { Start::No }
}

/// Which of the first [`CAPITALS`] characters of `text` are capitals, one
/// bit each, the first character the lowest bit.
#[must_use]
pub fn capitals(text: &str) -> u64 {
    let mut mask: u64 = 0;
    let mut bit: u64 = 1;
    for c in text.chars().take(CAPITALS) {
        if is_capital(c) {
            mask |= bit;
        }
        bit = bit.wrapping_shl(1);
    }
    mask
}

/// Whether `text` has more than `chars` characters.
#[must_use]
pub fn longer(text: &str, chars: usize) -> bool {
    text.len() > chars && (text.is_ascii() || text.chars().nth(chars).is_some())
}

/// What taking a suggestion types: first the case of the line put right,
/// then the rest of the command.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Fix {
    /// How many characters at the end of the line are taken back.
    pub erase: usize,
    /// What they are in the command.
    pub retype: String,
    /// The rest of the command, after the line.
    pub rest: String,
}

/// How `command` goes on from `line`, which it starts with as [`start`]
/// has it; none if it does not.
#[must_use]
pub fn fix(line: &str, command: &str) -> Option<Fix> {
    if let Some(rest) = command.strip_prefix(line) {
        return Some(Fix {
            rest: rest.to_owned(),
            ..Fix::default()
        });
    }
    let mut have = command.char_indices();
    // Where the first character that differs is: in the line's characters,
    // and in the command's bytes.
    let mut unlike: Option<(usize, usize)> = None;
    let mut chars: usize = 0;
    for t in line.chars() {
        let (at, c) = have.next()?;
        if !fits(c, t) {
            return None;
        }
        if c != t && unlike.is_none() {
            unlike = Some((chars, at));
        }
        chars = chars.saturating_add(1);
    }
    let end = have.next().map_or(command.len(), |(at, _)| at);
    let (before, from) = unlike?;
    Some(Fix {
        erase: chars.saturating_sub(before),
        retype: command.get(from..end)?.to_owned(),
        rest: command.get(end..)?.to_owned(),
    })
}

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
    fn a_letter_typed_in_lower_case_fits_either_case() {
        assert_eq!(start("cd ~/Dev/GitHub", "cd ~/dev"), Start::Loose);
        assert_eq!(start("cd ~/Dev/GitHub", "cd ~/Dev"), Start::Exact);
        assert_eq!(start("cd ~/Dev/GitHub", "cd ~/Dev/g"), Start::Loose);
        assert_eq!(start("cd ~/dotfiles", "cd ~/D"), Start::No);
        assert_eq!(start("ls", "ls -la"), Start::No);
        assert_eq!(start("\u{c9}cole", "\u{e9}c"), Start::Loose);
        assert_eq!(start("\u{e9}cole", "\u{c9}c"), Start::No);
        assert!(fits('A', 'a') && fits('a', 'a') && !fits('a', 'A'));
        assert!(fits('-', '-') && !fits('_', '-'));
        // The lower case of a capital I with a dot is two characters: it is
        // left as it is, and fits only itself.
        assert_eq!(fold('\u{130}'), '\u{130}');
        assert!(!fits('\u{130}', 'i'));
    }

    #[test]
    fn the_quick_comparisons_agree_with_comparing_a_character_at_a_time() {
        let alphabet: Vec<char> = "@AZ[`az{ _-/9eE\u{e9}\u{c9}\u{212a}k\u{3a3}\u{3c3}"
            .chars()
            .collect();
        let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = |below: usize| {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            usize::try_from(seed >> 33).unwrap() % below
        };
        for _ in 0..20_000 {
            let len = next(24);
            let a: String = (0..len).map(|_| alphabet[next(alphabet.len())]).collect();
            let mut b: Vec<char> = a.chars().collect();
            match next(4) {
                0 => {
                    b = (0..next(24))
                        .map(|_| alphabet[next(alphabet.len())])
                        .collect();
                }
                1 if !b.is_empty() => {
                    let at = next(b.len());
                    b[at] = alphabet[next(alphabet.len())];
                }
                2 => b.truncate(next(len.saturating_add(1))),
                _ => b.extend((0..next(12)).map(|_| alphabet[next(alphabet.len())])),
            }
            let b: String = b.into_iter().collect();
            let slow = a.chars().map(fold).cmp(b.chars().map(fold));
            assert_eq!(fold_cmp(&a, &b), slow, "{a:?} {b:?}");
            let mut have = a.chars();
            let starts = b
                .chars()
                .all(|t| have.next().is_some_and(|c| fold(c) == fold(t)));
            assert_eq!(starts_folded(&a, &b), starts, "{a:?} {b:?}");
        }
    }

    #[test]
    fn the_order_keeps_texts_that_differ_only_in_case_together() {
        let mut texts = vec![
            "git status",
            "Git log",
            "gIT add",
            "gh pr",
            "Makefile",
            "make",
        ];
        texts.sort_by(|a, b| key_cmp(a, b));
        assert_eq!(
            texts,
            [
                "gh pr",
                "gIT add",
                "Git log",
                "git status",
                "make",
                "Makefile"
            ]
        );
        assert_eq!(key_cmp("a", "A"), Ordering::Greater);
        assert_eq!(fold_cmp("a", "A"), Ordering::Equal);
        assert_eq!(fold_cmp("\u{c9}t\u{e9}", "\u{e9}T\u{c9}"), Ordering::Equal);
        assert_eq!(fold_cmp("ab\u{e9}", "abz"), Ordering::Greater);
        assert_eq!(fold_cmp("ab", "abc"), Ordering::Less);
        assert!(starts_folded("Git Status", "git s"));
        assert!(starts_folded("\u{c9}cole", "\u{e9}co"));
        assert!(!starts_folded("git", "git s"));
        assert!(!starts_folded("gi\u{e9}", "gie"));
    }

    #[test]
    fn capitals_are_marked_by_place() {
        assert_eq!(capitals("cd ~/Dev/GitHub"), (1 << 5) | (1 << 9) | (1 << 12));
        assert_eq!(capitals("ls"), 0);
        assert_eq!(capitals(&format!("{}A", "a".repeat(CAPITALS))), 0);
        assert!(longer("abc", 2) && !longer("abc", 3) && longer("\u{e9}\u{e9}", 1));
        assert!(!longer("\u{e9}", 1));
    }

    #[test]
    fn a_fix_puts_the_case_right_from_the_first_letter_that_differs() {
        assert_eq!(
            fix("cd ~/dev/g", "cd ~/Dev/GitHub"),
            Some(Fix {
                erase: 5,
                retype: "Dev/G".to_owned(),
                rest: "itHub".to_owned(),
            })
        );
        assert_eq!(
            fix("git s", "git status"),
            Some(Fix {
                rest: "tatus".to_owned(),
                ..Fix::default()
            })
        );
        assert_eq!(fix("git x", "git status"), None);
        assert_eq!(fix("git status", "git"), None);
        assert_eq!(fix("cd ~/D", "cd ~/dotfiles"), None);
        let wide = fix("\u{e9}c", "\u{c9}cole").unwrap();
        assert_eq!(
            (wide.erase, wide.retype.as_str(), wide.rest.as_str()),
            (2, "\u{c9}c", "ole")
        );
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
