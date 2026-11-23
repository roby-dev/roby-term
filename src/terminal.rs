use crate::theme::{ansi_256_color, Rgb};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Command;
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
    /// Total lines evicted from the front of `scrollback` (keeps absolute line IDs monotonic).
    evicted_lines: usize,
    cursor_row: usize,
    cursor_col: usize,
    saved_cursor: (usize, usize),
    cur_fg: Rgb,
    cur_bg: Rgb,
    cur_bold: bool,
    cur_underline: bool,
    cur_inverse: bool,
    alt_screen: bool,
    mouse_tracking: bool,
    last_printed_char: char,
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
            evicted_lines: 0,
            cursor_row: 0,
            cursor_col: 0,
            saved_cursor: (0, 0),
            cur_fg: default_fg,
            cur_bg: default_bg,
            cur_bold: false,
            cur_underline: false,
            cur_inverse: false,
            alt_screen: false,
            mouse_tracking: false,
            last_printed_char: ' ',
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

    /// Returns true when the running process has activated a TUI protocol (alt-screen or mouse tracking).
    pub fn is_tui_active(&self) -> bool {
        self.alt_screen || self.mouse_tracking
    }

    pub fn scrollback_len(&self) -> usize {
        self.scrollback.len()
    }

    /// Monotonic absolute line index of the current cursor row.
    pub fn current_abs_line(&self) -> usize {
        self.evicted_lines + self.scrollback.len() + self.cursor_row
    }

    /// Monotonic absolute line index corresponding to `screen_row` at `scroll_offset`.
    pub fn screen_row_to_abs_line(&self, screen_row: usize, scroll_offset: usize) -> usize {
        if self.alt_screen {
            return screen_row;
        }
        let clamped = scroll_offset.min(self.scrollback.len());
        self.evicted_lines + self.scrollback.len().saturating_sub(clamped) + screen_row
    }

    /// Returns the row slice for a monotonic `abs_line`, if still in scrollback or screen.
    pub fn row_at_abs_line(&self, abs_line: usize) -> Option<&[Cell]> {
        if abs_line < self.evicted_lines {
            return None;
        }
        let rel = abs_line - self.evicted_lines;
        if rel < self.scrollback.len() {
            self.scrollback.get(rel).map(|r| r.as_slice())
        } else {
            self.cells.get(rel - self.scrollback.len()).map(|r| r.as_slice())
        }
    }

    /// Computes a `scroll_offset` that brings `abs_line` cleanly into view.
    pub fn scroll_offset_for_abs_line(&self, abs_line: usize) -> usize {
        if self.alt_screen {
            return 0;
        }
        let total_sb = self.scrollback.len();
        let rel = abs_line.saturating_sub(self.evicted_lines);
        let desired_top = rel.saturating_sub(self.rows / 3);
        total_sb.saturating_sub(desired_top).min(total_sb)
    }

    /// Extracts text from a mouse drag selection `(row, col)` in screen coordinates.
    pub fn extract_selection(
        &self,
        a: (usize, usize),
        b: (usize, usize),
        scroll_offset: usize,
    ) -> String {
        let (start, end) = if a <= b { (a, b) } else { (b, a) };
        let mut lines = Vec::new();
        for r in start.0..=end.0 {
            let Some(cells) = self.visible_row(r, scroll_offset) else {
                continue;
            };
            let c0 = if r == start.0 { start.1.min(cells.len()) } else { 0 };
            let c1 = if r == end.0 {
                (end.1 + 1).min(cells.len())
            } else {
                cells.len()
            };
            if c0 <= c1 {
                let s: String = cells[c0..c1].iter().map(|c| c.ch).collect();
                lines.push(s.trim_end().to_string());
            }
        }
        lines.join("\n")
    }

    /// Extracts trimmed text across an inclusive range of monotonic absolute lines (for Warp blocks).
    pub fn extract_abs_range(&self, start_abs: usize, end_abs: usize) -> String {
        let mut lines = Vec::new();
        for abs in start_abs..=end_abs {
            if let Some(cells) = self.row_at_abs_line(abs) {
                let s: String = cells.iter().map(|c| c.ch).collect();
                lines.push(s.trim_end().to_string());
            }
        }
        while lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        lines.join("\n")
    }

    /// Dumps the full scrollback + visible screen as plain text for local session persistence.
    pub fn dump_transcript(&self) -> String {
        let mut lines = Vec::with_capacity(self.scrollback.len() + self.cells.len());
        for row in &self.scrollback {
            let s: String = row.iter().map(|c| c.ch).collect();
            lines.push(s.trim_end().to_string());
        }
        for row in &self.cells {
            let s: String = row.iter().map(|c| c.ch).collect();
            lines.push(s.trim_end().to_string());
        }
        while lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        lines.join("\n")
    }

    /// Searches scrollback + active screen case-insensitively for `query`.
    pub fn search_matches(&self, query: &str) -> Vec<SearchMatch> {
        let q = query.trim();
        if q.is_empty() {
            return Vec::new();
        }
        let q_lower: Vec<char> = q.to_lowercase().chars().collect();
        let q_len = q_lower.len();
        let mut matches = Vec::new();

        let total_lines = self.scrollback.len() + self.cells.len();
        for rel in 0..total_lines {
            let row = if rel < self.scrollback.len() {
                &self.scrollback[rel]
            } else {
                &self.cells[rel - self.scrollback.len()]
            };
            if row.len() < q_len {
                continue;
            }
            let row_chars: Vec<char> = row
                .iter()
                .map(|c| c.ch.to_lowercase().next().unwrap_or(c.ch))
                .collect();
            let mut col = 0;
            while col + q_len <= row_chars.len() {
                if row_chars[col..col + q_len] == q_lower[..] {
                    matches.push(SearchMatch {
                        abs_line: self.evicted_lines + rel,
                        col_start: col,
                        col_end: col + q_len,
                    });
                    col += q_len;
                } else {
                    col += 1;
                }
            }
        }
        matches
    }

    /// Updates default terminal colors live (e.g. from Settings) and refreshes default-colored cells.
    pub fn set_default_colors(&mut self, fg: Rgb, bg: Rgb) {
        let old_fg = self.default_fg;
        let old_bg = self.default_bg;
        self.default_fg = fg;
        self.default_bg = bg;
        if self.cur_fg == old_fg {
            self.cur_fg = fg;
        }
        if self.cur_bg == old_bg {
            self.cur_bg = bg;
        }
        for row in &mut self.cells {
            for cell in row {
                if cell.fg == old_fg {
                    cell.fg = fg;
                }
                if cell.bg == old_bg {
                    cell.bg = bg;
                }
            }
        }
        for row in &mut self.scrollback {
            for cell in row {
                if cell.fg == old_fg {
                    cell.fg = fg;
                }
                if cell.bg == old_bg {
                    cell.bg = bg;
                }
            }
        }
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
            self.evicted_lines += 1;
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
        self.last_printed_char = c;
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

    fn esc_dispatch(&mut self, _intermediates: &[u8], _ignore: bool, byte: u8) {
        match byte {
            b'7' => self.saved_cursor = (self.cursor_row, self.cursor_col),
            b'8' => {
                self.cursor_row = self.saved_cursor.0.min(self.rows - 1);
                self.cursor_col = self.saved_cursor.1.min(self.cols - 1);
            }
            b'M' => {
                // Reverse Index (RI)
                if self.cursor_row > 0 {
                    self.cursor_row -= 1;
                } else {
                    self.cells.pop();
                    self.cells.insert(0, self.blank_row());
                }
            }
            _ => {}
        }
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
            for sub in params.iter() {
                let mode = sub.first().copied().unwrap_or(0) as usize;
                match mode {
                    1049 | 47 | 1047 => match action {
                        'h' => self.enter_alt_screen(),
                        'l' => self.leave_alt_screen(),
                        _ => {}
                    },
                    1000 | 1002 | 1003 | 1006 => match action {
                        'h' => self.mouse_tracking = true,
                        'l' => self.mouse_tracking = false,
                        _ => {}
                    },
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
            'E' => {
                // CNL — Cursor Next Line
                self.cursor_row = (self.cursor_row + param(0, 1)).min(self.rows - 1);
                self.cursor_col = 0;
            }
            'F' => {
                // CPL — Cursor Previous Line
                self.cursor_row = self.cursor_row.saturating_sub(param(0, 1));
                self.cursor_col = 0;
            }
            'G' | '`' => self.cursor_col = param(0, 1).saturating_sub(1).min(self.cols - 1),
            'd' => {
                // VPA — Vertical Position Absolute
                self.cursor_row = param(0, 1).saturating_sub(1).min(self.rows - 1);
            }
            'H' | 'f' => {
                let r = param(0, 1).saturating_sub(1).min(self.rows - 1);
                let c = param(1, 1).saturating_sub(1).min(self.cols - 1);
                self.cursor_row = r;
                self.cursor_col = c;
            }
            'X' => {
                // ECH — Erase Characters (used heavily by ConPTY & TUI menus to clear gaps)
                let count = param(0, 1);
                let blank = Cell::blank(self.default_fg, self.cur_bg);
                if let Some(row) = self.cells.get_mut(self.cursor_row) {
                    let end = (self.cursor_col + count).min(self.cols);
                    for c in self.cursor_col..end {
                        row[c] = blank;
                    }
                }
            }
            'P' => {
                // DCH — Delete Characters
                let count = param(0, 1);
                let blank = Cell::blank(self.default_fg, self.cur_bg);
                if let Some(row) = self.cells.get_mut(self.cursor_row) {
                    let col = self.cursor_col.min(self.cols);
                    let del = count.min(self.cols.saturating_sub(col));
                    if del > 0 {
                        row.drain(col..col + del);
                        row.resize(self.cols, blank);
                    }
                }
            }
            '@' => {
                // ICH — Insert Characters
                let count = param(0, 1);
                let blank = Cell::blank(self.default_fg, self.cur_bg);
                if let Some(row) = self.cells.get_mut(self.cursor_row) {
                    let col = self.cursor_col.min(self.cols);
                    let ins = count.min(self.cols.saturating_sub(col));
                    for _ in 0..ins {
                        row.pop();
                        row.insert(col, blank);
                    }
                }
            }
            'L' => {
                // IL — Insert Lines
                let count = param(0, 1).min(self.rows.saturating_sub(self.cursor_row));
                for _ in 0..count {
                    self.cells.pop();
                    self.cells.insert(self.cursor_row, self.blank_row());
                }
            }
            'M' => {
                // DL — Delete Lines
                let count = param(0, 1).min(self.rows.saturating_sub(self.cursor_row));
                for _ in 0..count {
                    self.cells.remove(self.cursor_row);
                    self.cells.push(self.blank_row());
                }
            }
            'S' => {
                // SU — Scroll Up
                let count = param(0, 1).min(self.rows);
                for _ in 0..count {
                    let top = self.cells.remove(0);
                    self.push_scrollback(top);
                    self.cells.push(self.blank_row());
                }
            }
            'T' => {
                // SD — Scroll Down
                let count = param(0, 1).min(self.rows);
                for _ in 0..count {
                    self.cells.pop();
                    self.cells.insert(0, self.blank_row());
                }
            }
            'b' => {
                // REP — Repeat preceding graphic character
                let count = param(0, 1).min(self.cols);
                let ch = self.last_printed_char;
                for _ in 0..count {
                    self.print(ch);
                }
            }
            'J' => {
                let blank = Cell::blank(self.default_fg, self.cur_bg);
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
                let blank = Cell::blank(self.default_fg, self.cur_bg);
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

/// A case-insensitive search match inside the terminal scrollback or screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchMatch {
    pub abs_line: usize,
    pub col_start: usize,
    pub col_end: usize,
}

/// Clickable link target detected in terminal output (local path or HTTP/HTTPS URL).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkTarget {
    Path { resolved: PathBuf, is_file: bool },
    Url(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedLink {
    pub col_start: usize,
    pub col_end: usize,
    pub target: LinkTarget,
}

/// Detects either an HTTP/HTTPS URL or an existing filesystem path under `col`.
pub fn detect_link_at_col(row: &[Cell], col: usize, cwd: &Path) -> Option<DetectedLink> {
    if col >= row.len() {
        return None;
    }
    let is_delim = |c: char| {
        c.is_whitespace()
            || matches!(
                c,
                '"' | '\'' | '<' | '>' | '|' | '(' | ')' | '[' | ']' | '{' | '}'
            )
    };
    if is_delim(row[col].ch) {
        return None;
    }

    let mut start = col;
    while start > 0 && !is_delim(row[start - 1].ch) {
        start -= 1;
    }
    let mut end = col + 1;
    while end < row.len() && !is_delim(row[end].ch) {
        end += 1;
    }

    let raw: String = row[start..end].iter().map(|c| c.ch).collect();
    let trimmed = raw.trim_end_matches([',', ';', '.']);

    // 1. Check HTTP / HTTPS URL first
    if trimmed.starts_with("https://") || trimmed.starts_with("http://") {
        if trimmed.len() > 8 {
            let effective_len = trimmed.chars().count();
            return Some(DetectedLink {
                col_start: start,
                col_end: (start + effective_len).min(end),
                target: LinkTarget::Url(trimmed.to_string()),
            });
        }
    }

    // 2. Fall back to existing filesystem path
    detect_path_at_col(row, col, cwd).map(|p| DetectedLink {
        col_start: p.col_start,
        col_end: p.col_end,
        target: LinkTarget::Path {
            resolved: p.resolved,
            is_file: p.is_file,
        },
    })
}

/// Opens a detected link: Explorer/Finder for paths, default browser for URLs.
pub fn open_link(target: &LinkTarget) {
    match target {
        LinkTarget::Path { resolved, .. } => open_in_file_explorer(resolved),
        LinkTarget::Url(url) => open_url_in_browser(url),
    }
}

pub fn open_url_in_browser(url: &str) {
    #[cfg(target_os = "windows")]
    {
        let _ = Command::new("cmd").args(["/C", "start", "", url]).spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = Command::new("open").arg(url).spawn();
    }
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    {
        let _ = Command::new("xdg-open").arg(url).spawn();
    }
}

/// A clickable filesystem path detected on a terminal row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedPath {
    pub col_start: usize,
    pub col_end: usize,
    pub resolved: PathBuf,
    pub is_file: bool,
}

/// Detects an existing file or directory path under `col` on a terminal row.
pub fn detect_path_at_col(row: &[Cell], col: usize, cwd: &Path) -> Option<DetectedPath> {
    if col >= row.len() {
        return None;
    }
    let is_delim = |c: char| {
        c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | '|' | '(' | ')' | '[' | ']' | '{' | '}')
    };
    if is_delim(row[col].ch) {
        return None;
    }

    let mut start = col;
    while start > 0 && !is_delim(row[start - 1].ch) {
        start -= 1;
    }
    let mut end = col + 1;
    while end < row.len() && !is_delim(row[end].ch) {
        end += 1;
    }

    let raw: String = row[start..end].iter().map(|c| c.ch).collect();
    let trimmed_end = raw.trim_end_matches([',', ';', '.']);
    let mut candidate_str = trimmed_end;

    // Strip optional `:line` or `:line:col` suffix (e.g. `src/main.rs:42:10`)
    if let Some((path_part, rest)) = strip_line_col_suffix(candidate_str) {
        if !rest.is_empty() {
            candidate_str = path_part;
        }
    }

    if !looks_like_fs_path(candidate_str) && !looks_like_local_filename(candidate_str) {
        return None;
    }

    let resolved = resolve_candidate_path(candidate_str, cwd)?;
    if !resolved.exists() {
        return None;
    }

    let effective_len = candidate_str.chars().count();
    Some(DetectedPath {
        col_start: start,
        col_end: (start + effective_len).min(end),
        is_file: resolved.is_file(),
        resolved,
    })
}

fn looks_like_fs_path(s: &str) -> bool {
    if s.len() < 2 {
        return false;
    }
    let b = s.as_bytes();
    // Windows drive path: C:\..., D:\wauth, D:/foo
    if b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/') {
        return true;
    }
    // Relative / home / Unix paths
    s.starts_with("./")
        || s.starts_with("../")
        || s.starts_with(".\\")
        || s.starts_with("..\\")
        || s.starts_with("~/")
        || s.starts_with("~\\")
        || s.starts_with('/')
        || s.contains('\\')
        || s.contains('/')
}

fn looks_like_local_filename(s: &str) -> bool {
    s.len() >= 3
        && s.contains('.')
        && !s.starts_with('.')
        && !s.ends_with('.')
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

fn strip_line_col_suffix(s: &str) -> Option<(&str, &str)> {
    // Avoid stripping Windows drive colon `D:\...`
    let min_colon_idx = if s.len() >= 2 && s.as_bytes()[1] == b':' { 2 } else { 0 };
    let rest_slice = &s[min_colon_idx..];
    let rel_colon = rest_slice.find(':')?;
    let idx = min_colon_idx + rel_colon;
    let suffix = &s[idx + 1..];
    if !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit() || b == b':') {
        Some((&s[..idx], suffix))
    } else {
        None
    }
}

fn resolve_candidate_path(candidate: &str, cwd: &Path) -> Option<PathBuf> {
    if let Some(rest) = candidate
        .strip_prefix("~/")
        .or_else(|| candidate.strip_prefix("~\\"))
    {
        let home = std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(PathBuf::from)?;
        return Some(home.join(rest));
    }
    let p = PathBuf::from(candidate);
    if p.is_absolute() {
        Some(p)
    } else {
        Some(cwd.join(p))
    }
}

/// Opens a directory in Windows Explorer / macOS Finder, or reveals a file inside its folder.
pub fn open_in_file_explorer(path: &Path) {
    #[cfg(target_os = "windows")]
    {
        if path.is_file() {
            let _ = Command::new("explorer.exe")
                .arg(format!("/select,{}", path.display()))
                .spawn();
        } else {
            let _ = Command::new("explorer.exe").arg(path).spawn();
        }
    }
    #[cfg(target_os = "macos")]
    {
        if path.is_file() {
            let _ = Command::new("open").arg("-R").arg(path).spawn();
        } else {
            let _ = Command::new("open").arg(path).spawn();
        }
    }
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    {
        let target = if path.is_file() {
            path.parent().unwrap_or(path)
        } else {
            path
        };
        let _ = Command::new("xdg-open").arg(target).spawn();
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
        assert!(!grid.is_tui_active());
        assert_eq!(row_text(&grid, 0), "main");

        // Inline TUI mouse tracking (e.g. modern CLI agents)
        grid.feed(b"\x1b[?1000h\x1b[?1006h");
        assert!(grid.is_tui_active());
        grid.feed(b"\x1b[?1000l\x1b[?1006l");
        assert!(!grid.is_tui_active());
    }

    #[test]
    fn detects_existing_clickable_path_in_terminal_row() {
        let dir = std::env::temp_dir().join("my_warp_path_detect_test");
        let file = dir.join("hello.txt");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&file, "hi").unwrap();

        let fg = Rgb::new(255, 255, 255);
        let bg = Rgb::new(0, 0, 0);
        let mut grid = TerminalGrid::new(120, 4, fg, bg);
        let line = format!("Opened {} successfully", dir.display());
        grid.feed(line.as_bytes());

        let row = grid.visible_row(0, 0).unwrap();
        let detected = detect_path_at_col(row, 8, &dir).expect("should detect dir path");
        assert_eq!(detected.resolved, dir);
        assert!(!detected.is_file);
        assert!(detect_path_at_col(row, 0, &dir).is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn detects_clickable_http_and_https_urls() {
        let fg = Rgb::new(255, 255, 255);
        let bg = Rgb::new(0, 0, 0);
        let mut grid = TerminalGrid::new(100, 4, fg, bg);
        grid.feed(b"Server listening at http://localhost:3000/api, ready!");
        let row = grid.visible_row(0, 0).unwrap();
        let link = detect_link_at_col(row, 25, Path::new(".")).expect("should detect URL");
        assert_eq!(
            link.target,
            LinkTarget::Url("http://localhost:3000/api".to_string())
        );
    }

    #[test]
    fn searches_scrollback_and_extracts_selection() {
        let fg = Rgb::new(255, 255, 255);
        let bg = Rgb::new(0, 0, 0);
        let mut grid = TerminalGrid::new(40, 4, fg, bg);
        grid.feed(b"alpha beta\r\ngamma BETA\r\ndelta");
        let hits = grid.search_matches("beta");
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].col_start, 6);
        assert_eq!(hits[1].col_start, 6);

        let sel = grid.extract_selection((0, 0), (1, 4), 0);
        assert_eq!(sel, "alpha beta\ngamma");
    }

    #[test]
    fn handles_ech_dch_ich_il_dl_and_rep_for_clean_tui_redraws() {
        let fg = Rgb::new(255, 255, 255);
        let bg = Rgb::new(0, 0, 0);
        let mut grid = TerminalGrid::new(40, 5, fg, bg);

        // Simulate old line overwritten by TUI menu using CSI <n> X (ECH) + EL (\x1b[K)
        grid.feed(b"old stale text that should disappear");
        grid.feed(b"\r/usage\x1b[14X\x1b[21GView quota\x1b[K");
        assert_eq!(row_text(&grid, 0), "/usage              View quota");

        // DCH (`P`) deletes characters and shifts left
        grid.feed(b"\x1b[2;1Habcdef\x1b[2;3H\x1b[2P");
        assert_eq!(row_text(&grid, 1), "abef");

        // ICH (`@`) inserts blanks and shifts right
        grid.feed(b"\x1b[2;3H\x1b[2@");
        assert_eq!(row_text(&grid, 1), "ab  ef");

        // REP (`b`) repeats last printed character
        grid.feed(b"\x1b[3;1H-\x1b[4b");
        assert_eq!(row_text(&grid, 2), "-----");
    }
}
