//! A small terminal screen: what a person looking at the window would see.
//!
//! It follows the sequences that shells, prompts, readline, `cat` and a
//! full-screen editor use: printable text of any width, cursor motions, erase,
//! insert and delete, save and restore, the attributes that matter here
//! (dim, bold, reverse), automatic wrapping and scrolling, and the alternate
//! screen. It answers the queries programs make of a terminal, so they do
//! not wait for a reply that never comes.

use unicode_width::UnicodeWidthChar;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub dim: bool,
    pub bold: bool,
    /// The second half of a wide character.
    pub tail: bool,
}

impl Cell {
    const BLANK: Self = Self {
        ch: ' ',
        dim: false,
        bold: false,
        tail: false,
    };
}

#[derive(Clone, Copy, Default)]
struct Pen {
    dim: bool,
    bold: bool,
}

#[derive(Clone, Copy)]
struct Saved {
    x: usize,
    y: usize,
    pen: Pen,
    pending: bool,
}

type Grid = Vec<Vec<Cell>>;

enum Parse {
    Ground,
    Escape,
    Intermediate,
    Csi {
        private: bool,
        params: Vec<usize>,
        current: Option<usize>,
    },
    Text {
        escape: bool,
    },
}

pub struct Screen {
    rows: usize,
    cols: usize,
    main: Grid,
    alt: Grid,
    on_alt: bool,
    x: usize,
    y: usize,
    pending: bool,
    pen: Pen,
    saved: Option<Saved>,
    saved_main: Option<Saved>,
    modes: Modes,
    scrollback: Vec<String>,
    parse: Parse,
    utf8: Vec<u8>,
    /// What the terminal answers to queries; the caller writes it back.
    pub replies: Vec<u8>,
}

struct Modes {
    autowrap: bool,
    bracketed_paste: bool,
}

/// A numeric parameter, with 1 for one left out or given as 0.
fn param(params: &[usize], index: usize) -> usize {
    match params.get(index).copied() {
        None | Some(0) => 1,
        Some(value) => value,
    }
}

fn blank_grid(rows: usize, cols: usize) -> Grid {
    vec![vec![Cell::BLANK; cols]; rows]
}

impl Screen {
    pub fn new(rows: usize, cols: usize) -> Self {
        Self {
            rows,
            cols,
            main: blank_grid(rows, cols),
            alt: blank_grid(rows, cols),
            on_alt: false,
            x: 0,
            y: 0,
            pending: false,
            pen: Pen::default(),
            saved: None,
            saved_main: None,
            modes: Modes {
                autowrap: true,
                bracketed_paste: false,
            },
            scrollback: Vec::new(),
            parse: Parse::Ground,
            utf8: Vec::new(),
            replies: Vec::new(),
        }
    }

    pub fn resize(&mut self, rows: usize, cols: usize) {
        for grid in [&mut self.main, &mut self.alt] {
            grid.resize_with(rows, || vec![Cell::BLANK; cols]);
            for row in grid.iter_mut() {
                row.resize(cols, Cell::BLANK);
            }
        }
        self.rows = rows;
        self.cols = cols;
        self.x = self.x.min(cols.saturating_sub(1));
        self.y = self.y.min(rows.saturating_sub(1));
    }

    fn grid(&mut self) -> &mut Grid {
        if self.on_alt {
            &mut self.alt
        } else {
            &mut self.main
        }
    }

    fn grid_ref(&self) -> &Grid {
        if self.on_alt { &self.alt } else { &self.main }
    }

    pub fn cursor(&self) -> (usize, usize) {
        (self.x, self.y)
    }

    pub fn on_alternate_screen(&self) -> bool {
        self.on_alt
    }

    /// The text of a row, without the blanks at its end.
    pub fn row(&self, y: usize) -> String {
        let text: String = self
            .grid_ref()
            .get(y)
            .map(|row| row.iter().filter(|c| !c.tail).map(|c| c.ch).collect())
            .unwrap_or_default();
        text.trim_end().to_owned()
    }

    /// The text of a row that is not dim: the row without a suggestion on it.
    pub fn solid_row(&self, y: usize) -> String {
        let text: String = self
            .grid_ref()
            .get(y)
            .map(|row| {
                row.iter()
                    .filter(|c| !c.dim && !c.tail)
                    .map(|c| c.ch)
                    .collect()
            })
            .unwrap_or_default();
        text.trim_end().to_owned()
    }

    /// The text of the dim cells of a row, in order.
    pub fn dim_text(&self, y: usize) -> String {
        self.grid_ref()
            .get(y)
            .map(|row| {
                row.iter()
                    .filter(|c| c.dim && !c.tail)
                    .map(|c| c.ch)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Whether any cell on the screen is dim.
    pub fn any_dim(&self) -> bool {
        self.grid_ref()
            .iter()
            .flatten()
            .any(|c| c.dim && c.ch != ' ')
    }

    /// Every row of the visible screen, trailing blank rows dropped.
    pub fn text(&self) -> String {
        let mut rows: Vec<String> = (0..self.rows).map(|y| self.row(y)).collect();
        while rows.last().is_some_and(String::is_empty) {
            rows.pop();
        }
        rows.join("\n")
    }

    /// Everything that has scrolled off, then the screen.
    pub fn all_text(&self) -> String {
        let mut text = self.scrollback.join("\n");
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&self.text());
        text
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.byte(byte);
        }
    }

    fn byte(&mut self, byte: u8) {
        match std::mem::replace(&mut self.parse, Parse::Ground) {
            Parse::Ground => self.ground(byte),
            Parse::Escape => self.escape(byte),
            Parse::Intermediate => {}
            Parse::Csi {
                private,
                params,
                current,
            } => self.csi_byte(byte, private, params, current),
            Parse::Text { escape } => {
                let ends =
                    byte == 0x07 || byte == 0x18 || byte == 0x1a || (escape && byte == b'\\');
                if !ends {
                    self.parse = Parse::Text {
                        escape: byte == 0x1b,
                    };
                }
            }
        }
    }

    fn ground(&mut self, byte: u8) {
        if !self.utf8.is_empty() || byte >= 0x80 {
            self.utf8.push(byte);
            match std::str::from_utf8(&self.utf8) {
                Ok(text) => {
                    if let Some(c) = text.chars().next() {
                        self.print(c);
                    }
                    self.utf8.clear();
                }
                Err(error) if error.error_len().is_some() => self.utf8.clear(),
                Err(_) => {}
            }
            return;
        }
        match byte {
            0x1b => self.parse = Parse::Escape,
            b'\r' => {
                self.x = 0;
                self.pending = false;
            }
            b'\n' | 0x0b | 0x0c => self.line_feed(),
            0x08 => {
                self.x = self.x.saturating_sub(1);
                self.pending = false;
            }
            b'\t' => {
                self.x = (self.x / 8)
                    .saturating_add(1)
                    .saturating_mul(8)
                    .min(self.cols.saturating_sub(1));
                self.pending = false;
            }
            0x00..=0x1f | 0x7f => {}
            _ => self.print(char::from(byte)),
        }
    }

    fn line_feed(&mut self) {
        if self.y.saturating_add(1) >= self.rows {
            self.scroll_up(1);
        } else {
            self.y = self.y.saturating_add(1);
        }
    }

    fn scroll_up(&mut self, n: usize) {
        for _ in 0..n {
            let on_alt = self.on_alt;
            let cols = self.cols;
            let top = self.grid().remove(0);
            if !on_alt {
                let line: String = top.iter().filter(|c| !c.tail).map(|c| c.ch).collect();
                self.scrollback.push(line.trim_end().to_owned());
            }
            self.grid().push(vec![Cell::BLANK; cols]);
        }
    }

    fn scroll_down(&mut self, n: usize) {
        for _ in 0..n {
            let cols = self.cols;
            self.grid().pop();
            self.grid().insert(0, vec![Cell::BLANK; cols]);
        }
    }

    fn print(&mut self, c: char) {
        let width = c.width().unwrap_or(0);
        if width == 0 {
            return;
        }
        if self.pending {
            if self.modes.autowrap {
                self.x = 0;
                self.line_feed();
            }
            self.pending = false;
        }
        if self.x.saturating_add(width) > self.cols {
            if self.modes.autowrap {
                self.x = 0;
                self.line_feed();
            } else {
                self.x = self.cols.saturating_sub(width);
            }
        }
        let (x, y, pen) = (self.x, self.y, self.pen);
        if let Some(row) = self.grid().get_mut(y) {
            if let Some(cell) = row.get_mut(x) {
                *cell = Cell {
                    ch: c,
                    dim: pen.dim,
                    bold: pen.bold,
                    tail: false,
                };
            }
            if width == 2
                && let Some(cell) = row.get_mut(x.saturating_add(1))
            {
                *cell = Cell {
                    ch: ' ',
                    dim: pen.dim,
                    bold: pen.bold,
                    tail: true,
                };
            }
        }
        self.x = self.x.saturating_add(width);
        if self.x >= self.cols {
            self.x = self.cols.saturating_sub(1);
            self.pending = true;
        }
    }

    fn escape(&mut self, byte: u8) {
        match byte {
            b'[' => {
                self.parse = Parse::Csi {
                    private: false,
                    params: Vec::new(),
                    current: None,
                };
            }
            b']' | b'P' | b'X' | b'^' | b'_' => self.parse = Parse::Text { escape: false },
            b'7' => self.saved = Some(self.save()),
            b'8' => {
                if let Some(saved) = self.saved {
                    self.restore(saved);
                }
            }
            b'c' => *self = Self::new(self.rows, self.cols),
            b'D' => self.line_feed(),
            b'E' => {
                self.x = 0;
                self.line_feed();
            }
            b'M' => {
                if self.y == 0 {
                    self.scroll_down(1);
                } else {
                    self.y = self.y.saturating_sub(1);
                }
            }
            0x20..=0x2f => self.parse = Parse::Intermediate,
            _ => {}
        }
    }

    fn save(&self) -> Saved {
        Saved {
            x: self.x,
            y: self.y,
            pen: self.pen,
            pending: self.pending,
        }
    }

    fn restore(&mut self, saved: Saved) {
        self.x = saved.x;
        self.y = saved.y;
        self.pen = saved.pen;
        self.pending = saved.pending;
    }

    fn csi_byte(
        &mut self,
        byte: u8,
        private: bool,
        mut params: Vec<usize>,
        mut current: Option<usize>,
    ) {
        match byte {
            b'0'..=b'9' => {
                let digit = usize::from(byte.saturating_sub(b'0'));
                current = Some(
                    current
                        .unwrap_or(0)
                        .saturating_mul(10)
                        .saturating_add(digit),
                );
            }
            b';' | b':' => {
                params.push(current.unwrap_or(0));
                current = None;
            }
            b'?' | b'<' | b'=' | b'>' => {
                self.parse = Parse::Csi {
                    private: true,
                    params,
                    current,
                };
                return;
            }
            0x20..=0x2f => {
                // An intermediate byte: the final byte that follows is not
                // one of the plain commands.
                self.parse = Parse::Csi {
                    private,
                    params,
                    current,
                };
                return;
            }
            0x40..=0x7e => {
                if let Some(value) = current {
                    params.push(value);
                }
                self.csi(byte, private, &params);
                return;
            }
            _ => return,
        }
        self.parse = Parse::Csi {
            private,
            params,
            current,
        };
    }

    fn csi(&mut self, last: u8, private: bool, params: &[usize]) {
        if private {
            self.private_mode(last, params);
            return;
        }
        if !self.motion(last, params) {
            self.edit(last, params);
        }
        if !matches!(
            last,
            b'm' | b's' | b'n' | b'c' | b'K' | b'J' | b'X' | b'P' | b'@'
        ) {
            self.pending = false;
        }
    }

    /// The sequences that move the cursor. Returns whether `last` was one.
    fn motion(&mut self, last: u8, params: &[usize]) -> bool {
        let n = |index: usize| param(params, index);
        let last_row = self.rows.saturating_sub(1);
        let last_col = self.cols.saturating_sub(1);
        match last {
            b'A' => self.y = self.y.saturating_sub(n(0)),
            b'B' | b'e' => self.y = self.y.saturating_add(n(0)).min(last_row),
            b'C' | b'a' => self.x = self.x.saturating_add(n(0)).min(last_col),
            b'D' => self.x = self.x.saturating_sub(n(0)),
            b'E' => {
                self.y = self.y.saturating_add(n(0)).min(last_row);
                self.x = 0;
            }
            b'F' => {
                self.y = self.y.saturating_sub(n(0));
                self.x = 0;
            }
            b'G' | b'`' => self.x = n(0).saturating_sub(1).min(last_col),
            b'd' => self.y = n(0).saturating_sub(1).min(last_row),
            b'H' | b'f' => {
                self.y = n(0).saturating_sub(1).min(last_row);
                self.x = n(1).saturating_sub(1).min(last_col);
            }
            _ => return false,
        }
        true
    }

    /// Erasing, inserting, deleting, attributes, saving and the queries.
    fn edit(&mut self, last: u8, params: &[usize]) {
        let n = |index: usize| param(params, index);
        match last {
            b'J' => self.erase_display(params.first().copied().unwrap_or(0)),
            b'K' => self.erase_line(params.first().copied().unwrap_or(0)),
            b'L' => self.insert_lines(n(0)),
            b'M' => self.delete_lines(n(0)),
            b'P' => self.delete_chars(n(0)),
            b'@' => self.insert_chars(n(0)),
            b'X' => self.erase_chars(n(0)),
            b'S' => self.scroll_up(n(0)),
            b'T' => self.scroll_down(n(0)),
            b'm' => self.sgr(params),
            b's' => self.saved = Some(self.save()),
            b'u' => {
                if let Some(saved) = self.saved {
                    self.restore(saved);
                }
            }
            b'n' if params.first() == Some(&6) => {
                let reply = format!(
                    "\x1b[{};{}R",
                    self.y.saturating_add(1),
                    self.x.saturating_add(1)
                );
                self.replies.extend_from_slice(reply.as_bytes());
            }
            b'n' if params.first() == Some(&5) => self.replies.extend_from_slice(b"\x1b[0n"),
            b'c' => self.replies.extend_from_slice(b"\x1b[?62;c"),
            _ => {}
        }
    }

    fn private_mode(&mut self, last: u8, params: &[usize]) {
        let on = last == b'h';
        if last == b'c' {
            // The secondary device attributes, asked with `>`.
            self.replies.extend_from_slice(b"\x1b[>1;1;0c");
        }
        if last != b'h' && last != b'l' {
            return;
        }
        for &mode in params {
            match mode {
                7 => self.modes.autowrap = on,
                2004 => self.modes.bracketed_paste = on,
                1049 | 1047 | 47 => self.alternate_screen(on, mode == 1049),
                _ => {}
            }
        }
    }

    fn alternate_screen(&mut self, on: bool, save_cursor: bool) {
        if on == self.on_alt {
            return;
        }
        if on {
            if save_cursor {
                self.saved_main = Some(self.save());
            }
            self.alt = blank_grid(self.rows, self.cols);
            self.on_alt = true;
        } else {
            self.on_alt = false;
            if let Some(saved) = self.saved_main.take() {
                self.restore(saved);
            }
        }
    }

    fn sgr(&mut self, params: &[usize]) {
        if params.is_empty() {
            self.pen = Pen::default();
        }
        let mut skip: usize = 0;
        for (index, &code) in params.iter().enumerate() {
            if skip > 0 {
                skip = skip.saturating_sub(1);
                continue;
            }
            match code {
                0 => self.pen = Pen::default(),
                1 => self.pen.bold = true,
                2 => self.pen.dim = true,
                22 => {
                    self.pen.bold = false;
                    self.pen.dim = false;
                }
                38 | 48 => {
                    skip = if params.get(index.saturating_add(1)) == Some(&5) {
                        2
                    } else {
                        4
                    };
                }
                _ => {}
            }
        }
    }

    fn erase_line(&mut self, mode: usize) {
        let (x, y, cols) = (self.x, self.y, self.cols);
        let blank = Cell::BLANK;
        if let Some(row) = self.grid().get_mut(y) {
            let range = match mode {
                0 => x..cols,
                1 => 0..x.saturating_add(1).min(cols),
                _ => 0..cols,
            };
            for cell in row.get_mut(range).into_iter().flatten() {
                *cell = blank;
            }
        }
    }

    fn erase_display(&mut self, mode: usize) {
        let (y, rows) = (self.y, self.rows);
        let range = match mode {
            0 => y.saturating_add(1)..rows,
            1 => 0..y,
            _ => 0..rows,
        };
        let blank_row = vec![Cell::BLANK; self.cols];
        for index in range {
            if let Some(row) = self.grid().get_mut(index) {
                row.clone_from(&blank_row);
            }
        }
        match mode {
            0 => self.erase_line(0),
            1 => self.erase_line(1),
            _ => {}
        }
    }

    fn delete_chars(&mut self, n: usize) {
        let (x, y, cols) = (self.x, self.y, self.cols);
        if let Some(row) = self.grid().get_mut(y) {
            for _ in 0..n.min(cols.saturating_sub(x)) {
                row.remove(x);
                row.push(Cell::BLANK);
            }
        }
    }

    fn insert_chars(&mut self, n: usize) {
        let (x, y, cols) = (self.x, self.y, self.cols);
        if let Some(row) = self.grid().get_mut(y) {
            for _ in 0..n.min(cols.saturating_sub(x)) {
                row.insert(x, Cell::BLANK);
                row.pop();
            }
        }
    }

    fn erase_chars(&mut self, n: usize) {
        let (x, y, cols) = (self.x, self.y, self.cols);
        if let Some(row) = self.grid().get_mut(y) {
            for cell in row
                .get_mut(x..x.saturating_add(n).min(cols))
                .into_iter()
                .flatten()
            {
                *cell = Cell::BLANK;
            }
        }
    }

    fn insert_lines(&mut self, n: usize) {
        let (y, rows, cols) = (self.y, self.rows, self.cols);
        for _ in 0..n.min(rows.saturating_sub(y)) {
            self.grid().pop();
            self.grid().insert(y, vec![Cell::BLANK; cols]);
        }
        self.x = 0;
    }

    fn delete_lines(&mut self, n: usize) {
        let (y, rows, cols) = (self.y, self.rows, self.cols);
        for _ in 0..n.min(rows.saturating_sub(y)) {
            self.grid().remove(y);
            self.grid().push(vec![Cell::BLANK; cols]);
        }
        self.x = 0;
    }
}
