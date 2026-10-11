//! Which column the cursor is in.
//!
//! A suggestion is cut to what fits on the row, so it matters how much of the
//! row is left. The terminal knows, and can be asked, but a question is a
//! round trip that must come back through the keyboard's own stream. The
//! cursor's column can instead be followed from what is written to the
//! terminal. The model here follows the small vocabulary readline and a
//! prompt use: printable characters of any width, carriage return,
//! backspace, tab, the cursor motions, and the sequences that move nothing.
//!
//! When something is written that moves the cursor in a way this does not
//! follow, the column is unknown until the next carriage return, which puts
//! it back at 0 whatever came before. Unknown means no suggestion is drawn:
//! the model is allowed to be unsure, never to guess.

use crate::text::width;

/// Tab stops are every eight columns.
const TAB: usize = 8;

/// The most numeric parameters of a control sequence that are kept.
const PARAMS: usize = 4;

#[derive(Clone, Copy, Debug, Default)]
struct Csi {
    params: [usize; PARAMS],
    count: usize,
    current: usize,
    started: bool,
    private: bool,
    intermediate: bool,
}

impl Csi {
    fn param(&mut self, digit: u8) {
        self.started = true;
        self.current = self
            .current
            .saturating_mul(10)
            .saturating_add(usize::from(digit.saturating_sub(b'0')));
    }

    fn separate(&mut self) {
        if let Some(slot) = self.params.get_mut(self.count) {
            *slot = self.current;
        }
        self.count = self.count.saturating_add(1);
        self.current = 0;
        self.started = false;
    }

    /// The parameter at `index`, or `default` if it was left out or is zero.
    fn get(&self, index: usize, default: usize) -> usize {
        let value = if index == self.count {
            self.current
        } else {
            self.params.get(index).copied().unwrap_or(0)
        };
        if value == 0 { default } else { value }
    }
}

#[derive(Clone, Copy, Debug)]
enum Parse {
    Ground,
    Escape,
    /// After ESC and an intermediate byte such as `(`: one more byte to skip.
    EscapeIntermediate,
    Csi(Csi),
    /// A string (OSC, DCS, APC, PM, SOS) that ends with BEL or ST.
    Text,
    TextEscape,
}

/// Follows the cursor's column.
#[derive(Debug)]
pub struct Column {
    cols: usize,
    at: usize,
    /// The cursor is on the last column with a character just written there:
    /// the next printable one wraps first.
    pending: bool,
    known: bool,
    saved: Option<(usize, bool)>,
    parse: Parse,
    utf8: [u8; 4],
    utf8_len: usize,
    utf8_need: usize,
}

impl Column {
    /// A model for a terminal `columns` wide, with the cursor not yet known.
    #[must_use]
    pub fn new(columns: usize) -> Self {
        Self {
            cols: columns,
            at: 0,
            pending: false,
            known: false,
            saved: None,
            parse: Parse::Ground,
            utf8: [0; 4],
            utf8_len: 0,
            utf8_need: 0,
        }
    }

    /// Where the cursor is is not known any more, as at the start of a prompt
    /// after output that was not followed.
    pub fn forget(&mut self) {
        self.known = false;
        self.pending = false;
        self.parse = Parse::Ground;
        self.utf8_need = 0;
    }

    /// The terminal changed size; the text on it has been laid out again, so
    /// the cursor is not where it was.
    pub fn resize(&mut self, columns: usize) {
        self.cols = columns;
        self.known = false;
        self.pending = false;
    }

    /// How many columns after the cursor are free on its row, keeping the
    /// last column free so that nothing drawn can set up a wrap. `None` if
    /// the cursor's place is not known.
    #[must_use]
    pub fn room(&self) -> Option<usize> {
        if !self.known || self.cols < 2 || self.pending {
            return None;
        }
        self.cols
            .saturating_sub(1)
            .checked_sub(self.at)
            .filter(|room| *room > 0)
    }

    /// The column, if known.
    #[must_use]
    pub fn position(&self) -> Option<usize> {
        self.known.then_some(self.at)
    }

    /// Follow `bytes` that were written to the terminal.
    pub fn feed(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.byte(byte);
        }
    }

    fn byte(&mut self, byte: u8) {
        match self.parse {
            Parse::Ground => self.ground(byte),
            Parse::Escape => self.escape(byte),
            Parse::EscapeIntermediate => self.parse = Parse::Ground,
            Parse::Csi(csi) => self.csi_byte(csi, byte),
            Parse::Text => match byte {
                0x07 | 0x18 | 0x1a => self.parse = Parse::Ground,
                0x1b => self.parse = Parse::TextEscape,
                _ => {}
            },
            Parse::TextEscape => {
                if byte == b'\\' {
                    self.parse = Parse::Ground;
                } else {
                    // ESC that is not the start of ST ends the string and
                    // starts a sequence of its own.
                    self.escape(byte);
                }
            }
        }
    }

    fn ground(&mut self, byte: u8) {
        if self.utf8_need > 0 {
            self.continue_utf8(byte);
            return;
        }
        match byte {
            0x1b => self.parse = Parse::Escape,
            b'\r' => self.carriage_return(),
            0x08 => self.back(1),
            b'\t' => self.tab(),
            0x00..=0x1f | 0x7f => {}
            0xc2..=0xdf => self.start_utf8(byte, 2),
            0xe0..=0xef => self.start_utf8(byte, 3),
            0xf0..=0xf4 => self.start_utf8(byte, 4),
            // Printable ASCII, and a stray continuation byte or an
            // impossible lead byte, each of which takes a column.
            0x20..=0x7e | 0x80..=0xc1 | 0xf5..=0xff => self.print(1),
        }
    }

    fn start_utf8(&mut self, byte: u8, need: usize) {
        self.utf8 = [byte, 0, 0, 0];
        self.utf8_len = 1;
        self.utf8_need = need;
    }

    fn continue_utf8(&mut self, byte: u8) {
        if byte & 0xc0 != 0x80 {
            // Not a continuation: the character before it was cut short.
            self.utf8_need = 0;
            self.print(1);
            self.ground(byte);
            return;
        }
        if let Some(slot) = self.utf8.get_mut(self.utf8_len) {
            *slot = byte;
        }
        self.utf8_len = self.utf8_len.saturating_add(1);
        if self.utf8_len >= self.utf8_need {
            self.utf8_need = 0;
            let bytes = self.utf8.get(..self.utf8_len).unwrap_or_default();
            let columns = std::str::from_utf8(bytes)
                .ok()
                .and_then(|text| text.chars().next())
                .map_or(1, width);
            self.print(columns);
        }
    }

    fn escape(&mut self, byte: u8) {
        self.parse = Parse::Ground;
        match byte {
            b'[' => self.parse = Parse::Csi(Csi::default()),
            b']' | b'P' | b'X' | b'^' | b'_' => self.parse = Parse::Text,
            b'7' => self.saved = Some((self.at, self.pending)),
            b'8' => {
                if let Some((column, pending)) = self.saved {
                    self.at = column;
                    self.pending = pending;
                }
            }
            b'E' | b'c' => self.carriage_return(),
            0x20..=0x2f => self.parse = Parse::EscapeIntermediate,
            // IND, RI, HTS, keypad modes and the rest move nothing sideways.
            _ => {}
        }
    }

    fn csi_byte(&mut self, mut csi: Csi, byte: u8) {
        match byte {
            b'0'..=b'9' => csi.param(byte),
            b';' | b':' => csi.separate(),
            b'?' | b'<' | b'=' | b'>' => csi.private = true,
            0x20..=0x2f => csi.intermediate = true,
            0x40..=0x7e => {
                self.parse = Parse::Ground;
                self.csi_final(&csi, byte);
                return;
            }
            // A control character inside a sequence, or garbage: give up on
            // the sequence and do not trust the column.
            _ => {
                self.parse = Parse::Ground;
                self.known = false;
                return;
            }
        }
        self.parse = Parse::Csi(csi);
    }

    fn csi_final(&mut self, csi: &Csi, byte: u8) {
        if csi.private || csi.intermediate {
            // Modes, cursor shape and the like. None of them moves the
            // cursor along the row.
            return;
        }
        let n = csi.get(0, 1);
        match byte {
            b'C' | b'a' => self.forward(n),
            b'D' => self.back(n),
            b'G' | b'`' => self.set(n.saturating_sub(1)),
            b'H' | b'f' => self.set(csi.get(1, 1).saturating_sub(1)),
            b'E' | b'F' | b'L' | b'M' | b'r' => self.carriage_return(),
            b's' => self.saved = Some((self.at, self.pending)),
            b'u' => {
                if let Some((column, pending)) = self.saved {
                    self.at = column;
                    self.pending = pending;
                }
            }
            // Repeat, forward and backward tab: not followed.
            b'b' | b'I' | b'Z' => self.known = false,
            // Moves up and down, erases, deletes and inserts, colours, and
            // reports: the column stays.
            _ => {}
        }
    }

    fn carriage_return(&mut self) {
        self.at = 0;
        self.pending = false;
        self.known = true;
    }

    fn set(&mut self, column: usize) {
        self.at = column.min(self.cols.saturating_sub(1));
        self.pending = false;
        self.known = true;
    }

    fn forward(&mut self, n: usize) {
        self.at = self.at.saturating_add(n).min(self.cols.saturating_sub(1));
        self.pending = false;
    }

    fn back(&mut self, n: usize) {
        self.at = self.at.saturating_sub(n);
        self.pending = false;
    }

    fn tab(&mut self) {
        let next = (self.at / TAB).saturating_add(1).saturating_mul(TAB);
        self.at = next.min(self.cols.saturating_sub(1));
        self.pending = false;
    }

    fn print(&mut self, columns: usize) {
        if columns == 0 {
            return;
        }
        if self.pending {
            self.at = 0;
            self.pending = false;
        }
        if self.at.saturating_add(columns) > self.cols {
            // A wide character that does not fit goes to the next row.
            self.at = 0;
        }
        self.at = self.at.saturating_add(columns);
        if self.at >= self.cols {
            self.at = self.cols.saturating_sub(1);
            self.pending = true;
        }
        debug_assert!(self.at < self.cols.max(1), "the cursor is on the row");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn after(columns: usize, text: &str) -> Column {
        let mut model = Column::new(columns);
        model.feed(b"\r");
        model.feed(text.as_bytes());
        model
    }

    #[test]
    fn it_is_unknown_until_a_carriage_return() {
        let mut model = Column::new(80);
        assert_eq!(model.position(), None);
        model.feed(b"abc");
        assert_eq!(model.position(), None);
        model.feed(b"\rab");
        assert_eq!(model.position(), Some(2));
    }

    #[test]
    fn a_prompt_with_colours_and_a_title_is_followed() {
        let prompt = "\x1b]0;title\x07\x1b[1;36m~/dev\x1b[0m \x1b[1;36m\u{276f}\x1b[0m ";
        let model = after(80, prompt);
        assert_eq!(model.position(), Some(8));
        assert_eq!(model.room(), Some(71));
    }

    #[test]
    fn readlines_redraw_is_followed() {
        // Clear the line, go home, the prompt, the line, then back two.
        let model = after(80, "\x1b[K\rP> ls -la\x08\x08");
        assert_eq!(model.position(), Some(7));
    }

    #[test]
    fn wide_characters_take_two_columns() {
        assert_eq!(after(80, "\u{65e5}\u{672c}a").position(), Some(5));
    }

    #[test]
    fn combining_marks_take_none() {
        assert_eq!(after(80, "e\u{301}x").position(), Some(2));
    }

    #[test]
    fn writing_the_last_column_waits_to_wrap() {
        let model = after(10, "0123456789");
        assert_eq!(model.position(), Some(9));
        assert_eq!(model.room(), None);
        let model = after(10, "01234567890");
        assert_eq!(model.position(), Some(1));
    }

    #[test]
    fn a_wide_character_that_does_not_fit_wraps() {
        let model = after(10, "012345678\u{65e5}");
        assert_eq!(model.position(), Some(2));
    }

    #[test]
    fn motions_are_followed() {
        assert_eq!(after(80, "abcdef\x1b[3D").position(), Some(3));
        assert_eq!(after(80, "ab\x1b[5C").position(), Some(7));
        assert_eq!(after(80, "abcdef\x1b[10G").position(), Some(9));
        assert_eq!(after(80, "abcdef\x1b[3;20H").position(), Some(19));
        assert_eq!(after(80, "ab\tc").position(), Some(9));
        assert_eq!(after(80, "abcdef\x1b[2A\x1b[K").position(), Some(6));
    }

    #[test]
    fn save_and_restore_bring_the_column_back() {
        assert_eq!(after(80, "ab\x1b7cdefg\x1b8").position(), Some(2));
        assert_eq!(after(80, "ab\x1b[scdefg\x1b[u").position(), Some(2));
    }

    #[test]
    fn what_is_not_followed_makes_it_unknown_until_the_next_return() {
        let mut model = after(80, "ab\x1b[3b");
        assert_eq!(model.position(), None);
        assert_eq!(model.room(), None);
        model.feed(b"\rxyz");
        assert_eq!(model.position(), Some(3));
    }

    #[test]
    fn a_resize_makes_it_unknown() {
        let mut model = after(80, "abc");
        model.resize(40);
        assert_eq!(model.position(), None);
    }

    #[test]
    fn sequences_split_across_feeds_are_followed() {
        let mut model = Column::new(80);
        model.feed(b"\rabc\x1b");
        model.feed(b"[2");
        model.feed(b"D");
        assert_eq!(model.position(), Some(1));
        model.feed(b"\xe6\x97");
        model.feed(b"\xa5");
        assert_eq!(model.position(), Some(3));
    }

    #[test]
    fn a_string_sequence_hides_its_text() {
        let model = after(80, "\x1b]8;;http://example.com\x1b\\link\x1b]8;;\x1b\\");
        assert_eq!(model.position(), Some(4));
    }
}
