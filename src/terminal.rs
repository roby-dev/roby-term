use crate::theme::{ansi_256_color, Rgb};
use std::collections::VecDeque;
use vte::{Params, Parser, Perform};

/// Maximum scrollback lines kept in RAM per session (ring buffer).
pub const DEFAULT_MAX_SCROLLBACK: usize = 1500;

/// Compact terminal cell (~10 bytes) to keep per-session RAM minimal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub fg: Rgb,
    pub bg: Rgb,
    pub bold: bool,
    pub underline: bool,
    pub inverse: bool,
}

impl Cell {
    pub const fn blank(fg: Rgb, bg: Rgb) -> Self {
        Self {
            ch: ' ',
            fg,
            bg,
            bold: false,
            underline: false,
            inverse: false,
        }
    }
}

/// Compact VT/ANSI terminal emulator backed by `vte` and a bounded ring buffer.
pub struct TerminalGrid {
    cols: usize,
    rows: usize,
    default_fg: Rgb,
    default_bg: Rgb,
    cells: Vec<Vec<Cell>>,
    scrollback: VecDeque<Vec<Cell>>,
    max_scrollback: usize,
    cursor_row: usize,
    cursor_col: usize,
    saved_cursor: (usize, usize),
    cur_fg: Rgb,
    cur_bg: Rgb,
    cur_bold: bool,
    cur_underline: bool,
    cur_inverse: bool,
    alt_screen: bool,
    saved_main_cells: Option<Vec<Vec<Cell>>>,
    parser: Parser,
}

impl TerminalGrid {
    pub fn new(cols: usize, rows: usize, default_fg: Rgb, default_bg: Rgb) -> Self {
        Self::with_scrollback(cols, rows, default_fg, default_bg, DEFAULT_MAX_SCROLLBACK)
    }

    pub fn with_scrollback(
        cols: usize,
        rows: usize,
        default_fg: Rgb,
        default_bg: Rgb,
        max_scrollback: usize,
    ) -> Self {
        let cols = cols.max(2);
        let rows = rows.max(2);
        let blank = Cell::blank(default_fg, default_bg);
        let cells = vec![vec![blank; cols]; rows];
        Self {
            cols,
            rows,
            default_fg,
            default_bg,
            cells,
            scrollback: VecDeque::with_capacity(max_scrollback.min(256)),
            max_scrollback,
            cursor_row: 0,
            cursor_col: 0,
            saved_cursor: (0, 0),
            cur_fg: default_fg,
            cur_bg: default_bg,
            cur_bold: false,
            cur_underline: false,
            cur_inverse: false,
            alt_screen: false,
            saved_main_cells: None,
            parser: Parser::new(),
        }
    }

    pub fn cols(&self) -> usize {
        self.cols
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    pub fn cursor(&self) -> (usize, usize) {
        (self.cursor_row, self.cursor_col)
    }

    pub fn is_alt_screen(&self) -> bool {
        self.alt_screen
    }

    pub fn scrollback_len(&self) -> usize {
        self.scrollback.len()
    }

    /// Feeds raw PTY output bytes through the VT parser into the grid.
    pub fn feed(&mut self, bytes: &[u8]) {
        // Temporarily swap out parser to satisfy borrow checker without extra allocation.
        let mut parser = std::mem::replace(&mut self.parser, Parser::new());
        parser.advance(self, bytes);
        self.parser = parser;
    }

    pub fn resize(&mut self, new_cols: usize, new_rows: usize) {
        let new_cols = new_cols.max(2);
        let new_rows = new_rows.max(2);
        if new_cols == self.cols && new_rows == self.rows {
            return;
        }
        let blank = Cell::blank(self.default_fg, self.default_bg);
        for row in &mut self.cells {
            row.resize(new_cols, blank);
        }
        while self.cells.len() > new_rows {
            if self.cursor_row >= new_rows {
                let top = self.cells.remove(0);
                self.push_scrollback(top);
                self.cursor_row = self.cursor_row.saturating_sub(1);
            } else {
                self.cells.pop();
            }
        }
        while self.cells.len() < new_rows {
            self.cells.push(vec![blank; new_cols]);
        }
        self.cols = new_cols;
        self.rows = new_rows;
        self.cursor_row = self.cursor_row.min(self.rows - 1);
        self.cursor_col = self.cursor_col.min(self.cols - 1);
    }

    /// Returns the visible row at `screen_row` given a `scroll_offset` (0 = live bottom).
    pub fn visible_row(&self, screen_row: usize, scroll_offset: usize) -> Option<&[Cell]> {
        if scroll_offset == 0 || self.alt_screen {
            return self.cells.get(screen_row).map(|r| r.as_slice());
        }
        let clamped_offset = scroll_offset.min(self.scrollback.len());
        let total_sb = self.scrollback.len();
        let virtual_idx = total_sb.saturating_sub(clamped_offset) + screen_row;
        if virtual_idx < total_sb {
            self.scrollback.get(virtual_idx).map(|r| r.as_slice())
        } else {
            self.cells.get(virtual_idx - total_sb).map(|r| r.as_slice())
        }
    }

    fn blank_row(&self) -> Vec<Cell> {
        vec![Cell::blank(self.default_fg, self.default_bg); self.cols]
    }

    fn push_scrollback(&mut self, row: Vec<Cell>) {
        if self.alt_screen || self.max_scrollback == 0 {
            return;
        }
        if self.scrollback.len() >= self.max_scrollback {
            self.scrollback.pop_front();
        }
        self.scrollback.push_back(row);
    }

    fn newline(&mut self) {
        if self.cursor_row + 1 < self.rows {
            self.cursor_row += 1;
        } else {
            let top = self.cells.remove(0);
            self.push_scrollback(top);
            self.cells.push(self.blank_row());
        }
    }

    fn reset_sgr(&mut self) {
        self.cur_fg = self.default_fg;
        self.cur_bg = self.default_bg;
        self.cur_bold = false;
        self.cur_underline = false;
        self.cur_inverse = false;
    }

    fn apply_sgr(&mut self, params: &Params) {
        let mut iter = params.iter();
        let mut any = false;
        while let Some(sub) = iter.next() {
            any = true;
            let code = sub.first().copied().unwrap_or(0);
            match code {
                0 => self.reset_sgr(),
                1 => self.cur_bold = true,
                4 => self.cur_underline = true,
                7 => self.cur_inverse = true,
                22 => self.cur_bold = false,
                24 => self.cur_underline = false,
                27 => self.cur_inverse = false,
                30..=37 => self.cur_fg = ansi_256_color((code - 30) as u8),
                39 => self.cur_fg = self.default_fg,
                40..=47 => self.cur_bg = ansi_256_color((code - 40) as u8),
                49 => self.cur_bg = self.default_bg,
                90..=97 => self.cur_fg = ansi_256_color((code - 90 + 8) as u8),
                100..=107 => self.cur_bg = ansi_256_color((code - 100 + 8) as u8),
                38 | 48 => {
                    let is_fg = code == 38;
                    if let Some(mode_sub) = iter.next() {
                        match mode_sub.first().copied().unwrap_or(0) {
                            5 => {
                                if let Some(idx_sub) = iter.next() {
                                    let idx = idx_sub.first().copied().unwrap_or(0) as u8;
                                    let color = ansi_256_color(idx);
                                    if is_fg {
                                        self.cur_fg = color;
                                    } else {
                                        self.cur_bg = color;
                                    }
                                }
                            }
                            2 => {
                                let r = iter.next().and_then(|s| s.first().copied()).unwrap_or(0) as u8;
                                let g = iter.next().and_then(|s| s.first().copied()).unwrap_or(0) as u8;
                                let b = iter.next().and_then(|s| s.first().copied()).unwrap_or(0) as u8;
                                let color = Rgb::new(r, g, b);
                                if is_fg {
                                    self.cur_fg = color;
                                } else {
                                    self.cur_bg = color;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
        if !any {
            self.reset_sgr();
        }
    }

    fn enter_alt_screen(&mut self) {
        if !self.alt_screen {
            let blank = vec![self.blank_row(); self.rows];
            self.saved_main_cells = Some(std::mem::replace(&mut self.cells, blank));
            self.saved_cursor = (self.cursor_row, self.cursor_col);
            self.cursor_row = 0;
            self.cursor_col = 0;
            self.alt_screen = true;
        }
    }

    fn leave_alt_screen(&mut self) {
        if self.alt_screen {
            if let Some(main_cells) = self.saved_main_cells.take() {
                self.cells = main_cells;
            }
            self.cursor_row = self.saved_cursor.0.min(self.rows - 1);
            self.cursor_col = self.saved_cursor.1.min(self.cols - 1);
            self.alt_screen = false;
        }
    }
}

impl Perform for TerminalGrid {
    fn print(&mut self, c: char) {
        if self.cursor_col >= self.cols {
            self.cursor_col = 0;
            self.newline();
        }
        let cell = Cell {
            ch: c,
            fg: self.cur_fg,
            bg: self.cur_bg,
            bold: self.cur_bold,
            underline: self.cur_underline,
            inverse: self.cur_inverse,
        };
        if let Some(row) = self.cells.get_mut(self.cursor_row) {
            if let Some(slot) = row.get_mut(self.cursor_col) {
                *slot = cell;
            }
        }
        self.cursor_col += 1;
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            b'\r' => self.cursor_col = 0,
            b'\n' | 0x0b | 0x0c => self.newline(),
            0x08 => self.cursor_col = self.cursor_col.saturating_sub(1),
            b'\t' => {
                let next_tab = ((self.cursor_col / 8) + 1) * 8;
                self.cursor_col = next_tab.min(self.cols - 1);
            }
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], _ignore: bool, action: char) {
        let param = |idx: usize, default: usize| -> usize {
            params
                .iter()
                .nth(idx)
                .and_then(|p| p.first().copied())
                .map(|v| if v == 0 { default } else { v as usize })
                .unwrap_or(default)
        };
        let raw_param = |idx: usize| -> usize {
            params
                .iter()
                .nth(idx)
                .and_then(|p| p.first().copied())
                .unwrap_or(0) as usize
        };

        let is_private = intermediates.contains(&b'?');
        if is_private {
            let mode = raw_param(0);
            if mode == 1049 || mode == 47 || mode == 1047 {
                match action {
                    'h' => self.enter_alt_screen(),
                    'l' => self.leave_alt_screen(),
                    _ => {}
                }
            }
            return;
        }

        match action {
            'm' => self.apply_sgr(params),
            'A' => self.cursor_row = self.cursor_row.saturating_sub(param(0, 1)),
            'B' => self.cursor_row = (self.cursor_row + param(0, 1)).min(self.rows - 1),
            'C' => self.cursor_col = (self.cursor_col + param(0, 1)).min(self.cols - 1),
            'D' => self.cursor_col = self.cursor_col.saturating_sub(param(0, 1)),
            'G' => self.cursor_col = param(0, 1).saturating_sub(1).min(self.cols - 1),
            'H' | 'f' => {
                let r = param(0, 1).saturating_sub(1).min(self.rows - 1);
                let c = param(1, 1).saturating_sub(1).min(self.cols - 1);
                self.cursor_row = r;
                self.cursor_col = c;
            }
            'J' => {
                let blank = Cell::blank(self.default_fg, self.default_bg);
                match raw_param(0) {
                    0 => {
                        if let Some(row) = self.cells.get_mut(self.cursor_row) {
                            for c in self.cursor_col..self.cols {
                                row[c] = blank;
                            }
                        }
                        for r in (self.cursor_row + 1)..self.rows {
                            self.cells[r].fill(blank);
                        }
                    }
                    1 => {
                        for r in 0..self.cursor_row {
                            self.cells[r].fill(blank);
                        }
                        if let Some(row) = self.cells.get_mut(self.cursor_row) {
                            for c in 0..=self.cursor_col.min(self.cols - 1) {
                                row[c] = blank;
                            }
                        }
                    }
                    2 | 3 => {
                        for row in &mut self.cells {
                            row.fill(blank);
                        }
                    }
                    _ => {}
                }
            }
            'K' => {
                let blank = Cell::blank(self.default_fg, self.default_bg);
                if let Some(row) = self.cells.get_mut(self.cursor_row) {
                    match raw_param(0) {
                        0 => {
                            for c in self.cursor_col..self.cols {
                                row[c] = blank;
                            }
                        }
                        1 => {
                            for c in 0..=self.cursor_col.min(self.cols - 1) {
                                row[c] = blank;
                            }
                        }
                        2 => row.fill(blank),
                        _ => {}
                    }
                }
            }
            's' => self.saved_cursor = (self.cursor_row, self.cursor_col),
            'u' => {
                self.cursor_row = self.saved_cursor.0.min(self.rows - 1);
                self.cursor_col = self.saved_cursor.1.min(self.cols - 1);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row_text(grid: &TerminalGrid, row: usize) -> String {
        grid.visible_row(row, 0)
            .unwrap()
            .iter()
            .map(|c| c.ch)
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    #[test]
    fn writes_text_and_parses_ansi_truecolor() {
        let fg = Rgb::new(220, 220, 220);
        let bg = Rgb::new(10, 10, 10);
        let mut grid = TerminalGrid::new(40, 10, fg, bg);
        grid.feed(b"\x1b[38;2;255;100;50mHello\x1b[0m World");
        assert_eq!(row_text(&grid, 0), "Hello World");
        let row = grid.visible_row(0, 0).unwrap();
        assert_eq!(row[0].fg, Rgb::new(255, 100, 50));
        assert_eq!(row[6].fg, fg);
    }

    #[test]
    fn scrollback_is_strictly_bounded_for_low_ram() {
        let mut grid = TerminalGrid::with_scrollback(20, 4, Rgb::new(255, 255, 255), Rgb::new(0, 0, 0), 10);
        for i in 0..50 {
            grid.feed(format!("line {i}\r\n").as_bytes());
        }
        assert_eq!(grid.scrollback_len(), 10);
    }

    #[test]
    fn detects_alt_screen_enter_and_leave() {
        let mut grid = TerminalGrid::new(20, 5, Rgb::new(255, 255, 255), Rgb::new(0, 0, 0));
        grid.feed(b"main");
        assert!(!grid.is_alt_screen());
        grid.feed(b"\x1b[?1049hvim");
        assert!(grid.is_alt_screen());
        assert_eq!(row_text(&grid, 0), "vim");
        grid.feed(b"\x1b[?1049l");
        assert!(!grid.is_alt_screen());
        assert_eq!(row_text(&grid, 0), "main");
    }
}
