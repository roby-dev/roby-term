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
    pub dim: bool,
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
            dim: false,
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
    cur_dim: bool,
    cur_underline: bool,
    cur_inverse: bool,
    alt_screen: bool,
    mouse_tracking: bool,
    sgr_mouse: bool,
    bracketed_paste: bool,
    sync_output: bool,
    kitty_keyboard_stack: Vec<u8>,
    modify_other_keys: u8,
    pending_responses: Vec<Vec<u8>>,
    reported_cwd: Option<std::path::PathBuf>,
    screen_cleared: bool,
    scroll_top: usize,
    scroll_bottom: usize,
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
            cur_dim: false,
            cur_underline: false,
            cur_inverse: false,
            alt_screen: false,
            mouse_tracking: false,
            sgr_mouse: false,
            bracketed_paste: false,
            sync_output: false,
            kitty_keyboard_stack: Vec::new(),
            modify_other_keys: 0,
            pending_responses: Vec::new(),
            reported_cwd: None,
            screen_cleared: false,
            scroll_top: 0,
            scroll_bottom: rows - 1,
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

    /// Returns true when the running process has activated a TUI protocol
    /// (alt-screen, mouse tracking, or Kitty keyboard protocol).
    pub fn is_tui_active(&self) -> bool {
        self.alt_screen || self.mouse_tracking || self.kitty_keyboard_enabled()
    }

    pub fn mouse_tracking_enabled(&self) -> bool {
        self.mouse_tracking
    }

    pub fn sgr_mouse_enabled(&self) -> bool {
        self.sgr_mouse
    }

    pub fn bracketed_paste_enabled(&self) -> bool {
        self.bracketed_paste
    }

    /// Returns true while the running app is inside a synchronized output frame (`CSI ? 2026 h`).
    pub fn is_sync_output(&self) -> bool {
        self.sync_output
    }

    /// Returns the active Kitty keyboard protocol flags (0 when not active).
    pub fn kitty_keyboard_flags(&self) -> u8 {
        self.kitty_keyboard_stack.last().copied().unwrap_or(0)
    }

    /// Returns true when the running app enabled the Kitty keyboard protocol (`CSI > flags u`).
    pub fn kitty_keyboard_enabled(&self) -> bool {
        !self.kitty_keyboard_stack.is_empty()
    }

    /// Returns the active xterm `modifyOtherKeys` level (`CSI > 4 ; level m`), 0 when disabled.
    pub fn modify_other_keys_level(&self) -> u8 {
        self.modify_other_keys
    }

    /// Drains any pending terminal query responses (e.g. Kitty keyboard `CSI ? u`, DA1, DSR)
    /// that must be written back to the PTY.
    pub fn take_responses(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.pending_responses)
    }

    /// Drains the latest working directory announced by the shell via
    /// `OSC 9;9;<path>` (ConEmu / Windows Terminal) or `OSC 7;file://host/<path>`.
    pub fn take_reported_cwd(&mut self) -> Option<std::path::PathBuf> {
        self.reported_cwd.take()
    }

    /// Returns `true` once after the screen (`CSI 2 J`) or scrollback (`CSI 3 J`) was cleared,
    /// e.g. by `cls` / `clear`, so command blocks anchored to wiped lines can be dropped.
    pub fn take_screen_cleared(&mut self) -> bool {
        std::mem::take(&mut self.screen_cleared)
    }

    pub fn scrollback_len(&self) -> usize {
        self.scrollback.len()
    }

    /// Commits all non-empty rows from the active screen into scrollback and clears the screen.
    /// Prevents Windows ConPTY cursor-addressing from overwriting local built-in output (e.g. `ls`).
    pub fn commit_screen_to_scrollback(&mut self) {
        if self.alt_screen {
            return;
        }
        let last_non_empty = self
            .cells
            .iter()
            .rposition(|row| row.iter().any(|c| c.ch != ' ' && c.ch != '\0'));
        if let Some(last_idx) = last_non_empty {
            let blank = self.blank_row();
            for r in 0..=last_idx {
                let row = std::mem::replace(&mut self.cells[r], blank.clone());
                self.push_scrollback(row);
            }
            for r in (last_idx + 1)..self.rows {
                self.cells[r] = blank.clone();
            }
        }
        self.cursor_row = 0;
        self.cursor_col = 0;
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
                let s: String = cells[c0..c1]
                    .iter()
                    .filter(|c| c.ch != '\0')
                    .map(|c| c.ch)
                    .collect();
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
                let s: String = cells
                    .iter()
                    .filter(|c| c.ch != '\0')
                    .map(|c| c.ch)
                    .collect();
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
            let s: String = row
                .iter()
                .filter(|c| c.ch != '\0')
                .map(|c| c.ch)
                .collect();
            lines.push(s.trim_end().to_string());
        }
        for row in &self.cells {
            let s: String = row
                .iter()
                .filter(|c| c.ch != '\0')
                .map(|c| c.ch)
                .collect();
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
            let non_nul: Vec<(usize, char)> = row
                .iter()
                .enumerate()
                .filter(|(_, c)| c.ch != '\0')
                .map(|(idx, c)| (idx, c.ch.to_lowercase().next().unwrap_or(c.ch)))
                .collect();
            if non_nul.len() < q_len {
                continue;
            }
            let mut i = 0;
            while i + q_len <= non_nul.len() {
                if (0..q_len).all(|k| non_nul[i + k].1 == q_lower[k]) {
                    let col_start = non_nul[i].0;
                    let last_col = non_nul[i + q_len - 1].0;
                    let col_end = if last_col + 1 < row.len() && row[last_col + 1].ch == '\0' {
                        last_col + 2
                    } else {
                        last_col + 1
                    };
                    matches.push(SearchMatch {
                        abs_line: self.evicted_lines + rel,
                        col_start,
                        col_end,
                    });
                    i += q_len;
                } else {
                    i += 1;
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
        self.scroll_top = 0;
        self.scroll_bottom = self.rows - 1;
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

    fn scroll_region_up(&mut self, count: usize) {
        let top = self.scroll_top.min(self.rows - 1);
        let bottom = self.scroll_bottom.min(self.rows - 1);
        if top >= bottom {
            return;
        }
        let n = count.min(bottom - top + 1);
        for _ in 0..n {
            let removed = self.cells.remove(top);
            if top == 0 && bottom == self.rows - 1 {
                self.push_scrollback(removed);
            }
            self.cells.insert(bottom, self.blank_row());
        }
    }

    fn scroll_region_down(&mut self, count: usize) {
        let top = self.scroll_top.min(self.rows - 1);
        let bottom = self.scroll_bottom.min(self.rows - 1);
        if top >= bottom {
            return;
        }
        let n = count.min(bottom - top + 1);
        for _ in 0..n {
            self.cells.remove(bottom);
            self.cells.insert(top, self.blank_row());
        }
    }

    fn newline(&mut self) {
        let bottom = self.scroll_bottom.min(self.rows - 1);
        if self.cursor_row == bottom {
            self.scroll_region_up(1);
        } else if self.cursor_row + 1 < self.rows {
            self.cursor_row += 1;
        }
    }

    fn reset_sgr(&mut self) {
        self.cur_fg = self.default_fg;
        self.cur_bg = self.default_bg;
        self.cur_bold = false;
        self.cur_dim = false;
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
                2 => self.cur_dim = true,
                4 => self.cur_underline = true,
                7 => self.cur_inverse = true,
                22 => {
                    self.cur_bold = false;
                    self.cur_dim = false;
                }
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
                    // Support both colon-separated (`38:2::R:G:B`, `48:5:N`) and semicolon-separated SGR
                    let parsed = if sub.len() > 1 {
                        match sub[1] {
                            5 if sub.len() >= 3 => Some(ansi_256_color(sub[2] as u8)),
                            2 if sub.len() >= 6 => {
                                Some(Rgb::new(sub[3] as u8, sub[4] as u8, sub[5] as u8))
                            }
                            2 if sub.len() == 5 => {
                                Some(Rgb::new(sub[2] as u8, sub[3] as u8, sub[4] as u8))
                            }
                            _ => None,
                        }
                    } else if let Some(mode_sub) = iter.next() {
                        match mode_sub.first().copied().unwrap_or(0) {
                            5 => iter
                                .next()
                                .map(|idx_sub| ansi_256_color(idx_sub.first().copied().unwrap_or(0) as u8)),
                            2 => {
                                let r = iter.next().and_then(|s| s.first().copied()).unwrap_or(0) as u8;
                                let g = iter.next().and_then(|s| s.first().copied()).unwrap_or(0) as u8;
                                let b = iter.next().and_then(|s| s.first().copied()).unwrap_or(0) as u8;
                                Some(Rgb::new(r, g, b))
                            }
                            _ => None,
                        }
                    } else {
                        None
                    };
                    if let Some(color) = parsed {
                        if is_fg {
                            self.cur_fg = color;
                        } else {
                            self.cur_bg = color;
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
            self.scroll_top = 0;
            self.scroll_bottom = self.rows - 1;
            self.alt_screen = true;
        }
    }

    fn leave_alt_screen(&mut self) {
        if self.alt_screen {
            self.alt_screen = false;
            let mut saved_row = self.saved_cursor.0;
            if let Some(mut main_cells) = self.saved_main_cells.take() {
                let blank = Cell::blank(self.default_fg, self.default_bg);
                for row in &mut main_cells {
                    row.resize(self.cols, blank);
                }
                while main_cells.len() > self.rows {
                    if saved_row >= self.rows {
                        let top = main_cells.remove(0);
                        self.push_scrollback(top);
                        saved_row = saved_row.saturating_sub(1);
                    } else {
                        main_cells.pop();
                    }
                }
                while main_cells.len() < self.rows {
                    main_cells.push(vec![blank; self.cols]);
                }
                self.cells = main_cells;
            }
            self.cursor_row = saved_row.min(self.rows - 1);
            self.cursor_col = self.saved_cursor.1.min(self.cols - 1);
            self.scroll_top = 0;
            self.scroll_bottom = self.rows - 1;
            // Clear stale TUI protocol flags when returning to the normal shell
            self.mouse_tracking = false;
            self.sgr_mouse = false;
            self.bracketed_paste = false;
            self.sync_output = false;
            self.kitty_keyboard_stack.clear();
            self.modify_other_keys = 0;
        }
    }
}

/// Returns the terminal column width of a Unicode scalar value (0 = combining/invisible,
/// 2 = wide emoji/CJK, 1 = normal). Keeps cursor columns in sync with modern inline TUIs.
fn char_display_width(c: char) -> usize {
    let cp = c as u32;
    if cp < 0x20 || cp == 0x7F || cp == 0x00AD {
        return 0;
    }
    if matches!(
        cp,
        0x0300..=0x036F
            | 0x1AB0..=0x1AFF
            | 0x1DC0..=0x1DFF
            | 0x200B..=0x200F
            | 0x202A..=0x202E
            | 0x2060..=0x206F
            | 0x20D0..=0x20FF
            | 0xFE00..=0xFE0F
            | 0xFE20..=0xFE2F
            | 0xFEFF
            | 0xE0100..=0xE01EF
    ) {
        return 0;
    }
    if matches!(
        cp,
        0x1100..=0x115F
            | 0x231A..=0x231B
            | 0x2329..=0x232A
            | 0x23E9..=0x23F3
            | 0x23F8..=0x23FA
            | 0x25FD..=0x25FE
            | 0x2614..=0x2615
            | 0x2648..=0x2653
            | 0x267F
            | 0x2693
            | 0x26A1
            | 0x26AA..=0x26AB
            | 0x26BD..=0x26BE
            | 0x26C4..=0x26C5
            | 0x26CE
            | 0x26D4
            | 0x26EA
            | 0x26F2..=0x26F3
            | 0x26F5
            | 0x26FA
            | 0x26FD
            | 0x2705
            | 0x270A..=0x270B
            | 0x2728
            | 0x274C
            | 0x274E
            | 0x2753..=0x2755
            | 0x2757
            | 0x2795..=0x2797
            | 0x27B0
            | 0x27BF
            | 0x2B1B..=0x2B1C
            | 0x2B50
            | 0x2B55
            | 0x2E80..=0x303E
            | 0x3040..=0xA4CF
            | 0xAC00..=0xD7A3
            | 0xF900..=0xFAFF
            | 0xFE10..=0xFE19
            | 0xFE30..=0xFE6F
            | 0xFF00..=0xFF60
            | 0xFFE0..=0xFFE6
            | 0x1F300..=0x1FAFF
            | 0x20000..=0x3FFFD
    ) {
        return 2;
    }
    1
}

impl Perform for TerminalGrid {
    fn print(&mut self, c: char) {
        let width = char_display_width(c);
        if width == 0 {
            return;
        }
        self.last_printed_char = c;
        if width == 2 && self.cursor_col + 1 >= self.cols {
            if self.cursor_col < self.cols {
                let blank = Cell::blank(self.default_fg, self.cur_bg);
                if let Some(row) = self.cells.get_mut(self.cursor_row) {
                    row[self.cursor_col] = blank;
                }
            }
            self.cursor_col = 0;
            self.newline();
        } else if self.cursor_col >= self.cols {
            self.cursor_col = 0;
            self.newline();
        }
        let cell = Cell {
            ch: c,
            fg: self.cur_fg,
            bg: self.cur_bg,
            bold: self.cur_bold,
            dim: self.cur_dim,
            underline: self.cur_underline,
            inverse: self.cur_inverse,
        };
        let blank = Cell::blank(self.default_fg, self.default_bg);
        if let Some(row) = self.cells.get_mut(self.cursor_row) {
            let col = self.cursor_col;
            if col < row.len() {
                // Clean up partner cell if overwriting half of a wide-character pair
                if row[col].ch == '\0' && col > 0 {
                    row[col - 1] = blank;
                }
                if col + 1 < row.len() && row[col + 1].ch == '\0' {
                    row[col + 1] = blank;
                }
                row[col] = cell;
            }
            if width == 2 && col + 1 < row.len() {
                if col + 2 < row.len() && row[col + 2].ch == '\0' {
                    row[col + 2] = blank;
                }
                row[col + 1] = Cell {
                    ch: '\0',
                    fg: self.cur_fg,
                    bg: self.cur_bg,
                    bold: self.cur_bold,
                    dim: self.cur_dim,
                    underline: self.cur_underline,
                    inverse: self.cur_inverse,
                };
            }
        }
        self.cursor_col += width;
    }

    fn esc_dispatch(&mut self, _intermediates: &[u8], _ignore: bool, byte: u8) {
        match byte {
            b'7' => self.saved_cursor = (self.cursor_row, self.cursor_col),
            b'8' => {
                self.cursor_row = self.saved_cursor.0.min(self.rows - 1);
                self.cursor_col = self.saved_cursor.1.min(self.cols - 1);
            }
            b'M' => {
                // Reverse Index (RI) respecting DECSTBM scroll region
                let top = self.scroll_top.min(self.rows - 1);
                if self.cursor_row == top {
                    self.scroll_region_down(1);
                } else if self.cursor_row > 0 {
                    self.cursor_row -= 1;
                }
            }
            _ => {}
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        if let Some(cwd) = parse_cwd_osc(params) {
            self.reported_cwd = Some(cwd);
            return;
        }
        if params.len() >= 2 && params[1] == b"?" {
            match params[0] {
                b"10" => {
                    let c = self.default_fg;
                    let resp = format!(
                        "\x1b]10;rgb:{0:02x}{0:02x}/{1:02x}{1:02x}/{2:02x}{2:02x}\x1b\\",
                        c.r, c.g, c.b
                    );
                    self.pending_responses.push(resp.into_bytes());
                }
                b"11" => {
                    let c = self.default_bg;
                    let resp = format!(
                        "\x1b]11;rgb:{0:02x}{0:02x}/{1:02x}{1:02x}/{2:02x}{2:02x}\x1b\\",
                        c.r, c.g, c.b
                    );
                    self.pending_responses.push(resp.into_bytes());
                }
                _ => {}
            }
        }
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            b'\r' => self.cursor_col = 0,
            b'\n' | 0x0b | 0x0c => self.newline(),
            0x08 => {
                // In VT100 pending-wrap state (`cursor_col == cols`), clamp to last column first
                self.cursor_col = self.cursor_col.min(self.cols - 1).saturating_sub(1);
            }
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
            if action == 'u' {
                // Kitty keyboard protocol query (`CSI ? u`): reply with active flags
                let resp = format!("\x1b[?{}u", self.kitty_keyboard_flags());
                self.pending_responses.push(resp.into_bytes());
                return;
            }
            for sub in params.iter() {
                let mode = sub.first().copied().unwrap_or(0) as usize;
                match mode {
                    1049 | 47 | 1047 => match action {
                        'h' => self.enter_alt_screen(),
                        'l' => self.leave_alt_screen(),
                        _ => {}
                    },
                    1000 | 1002 | 1003 => match action {
                        'h' => self.mouse_tracking = true,
                        'l' => self.mouse_tracking = false,
                        _ => {}
                    },
                    1006 => match action {
                        'h' => self.sgr_mouse = true,
                        'l' => self.sgr_mouse = false,
                        _ => {}
                    },
                    2004 => match action {
                        'h' => self.bracketed_paste = true,
                        'l' => self.bracketed_paste = false,
                        _ => {}
                    },
                    2026 => match action {
                        'h' => self.sync_output = true,
                        'l' => self.sync_output = false,
                        _ => {}
                    },
                    _ => {}
                }
            }
            return;
        }

        // Kitty keyboard protocol push/pop/set (`CSI > flags u` / `CSI < count u` / `CSI = flags ; mode u`)
        if action == 'u' && intermediates.contains(&b'>') {
            let flags = raw_param(0).min(u8::MAX as usize) as u8;
            if self.kitty_keyboard_stack.len() < 16 {
                self.kitty_keyboard_stack.push(flags);
            }
            return;
        }
        if action == 'u' && intermediates.contains(&b'<') {
            let pop = param(0, 1);
            for _ in 0..pop {
                if self.kitty_keyboard_stack.pop().is_none() {
                    break;
                }
            }
            return;
        }
        if action == 'u' && intermediates.contains(&b'=') {
            let flags = raw_param(0).min(u8::MAX as usize) as u8;
            let mode = param(1, 1);
            if let Some(top) = self.kitty_keyboard_stack.last_mut() {
                match mode {
                    2 => *top |= flags,
                    3 => *top &= !flags,
                    _ => *top = flags,
                }
            } else if flags != 0 {
                self.kitty_keyboard_stack.push(flags);
            }
            return;
        }
        // xterm modifyOtherKeys (`CSI > 4 ; level m` / `CSI > 4 m`)
        if action == 'm' && intermediates.contains(&b'>') {
            if raw_param(0) == 4 {
                self.modify_other_keys = if params.len() >= 2 {
                    raw_param(1).min(u8::MAX as usize) as u8
                } else {
                    0
                };
            }
            return;
        }
        if !intermediates.is_empty() {
            return;
        }

        match action {
            'm' => self.apply_sgr(params),
            'c' => {
                // DA1 — Primary Device Attributes (used as sentinel after Kitty keyboard `CSI ? u` probes)
                if raw_param(0) == 0 {
                    self.pending_responses.push(b"\x1b[?62;22c".to_vec());
                }
            }
            'n' => {
                // DSR — Device Status Report (`CSI 6 n` -> cursor position)
                if raw_param(0) == 6 {
                    let col = self.cursor_col.min(self.cols - 1);
                    let resp = format!("\x1b[{};{}R", self.cursor_row + 1, col + 1);
                    self.pending_responses.push(resp.into_bytes());
                }
            }
            'A' => self.cursor_row = self.cursor_row.saturating_sub(param(0, 1)),
            'B' => self.cursor_row = (self.cursor_row + param(0, 1)).min(self.rows - 1),
            'C' => self.cursor_col = (self.cursor_col + param(0, 1)).min(self.cols - 1),
            'D' => {
                self.cursor_col = self
                    .cursor_col
                    .min(self.cols - 1)
                    .saturating_sub(param(0, 1));
            }
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
                    let col = self.cursor_col.min(self.cols - 1);
                    let end = (col + count).min(self.cols);
                    for c in col..end {
                        row[c] = blank;
                    }
                }
            }
            'P' => {
                // DCH — Delete Characters
                let count = param(0, 1);
                let blank = Cell::blank(self.default_fg, self.cur_bg);
                if let Some(row) = self.cells.get_mut(self.cursor_row) {
                    let col = self.cursor_col.min(self.cols - 1);
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
                    let col = self.cursor_col.min(self.cols - 1);
                    let ins = count.min(self.cols.saturating_sub(col));
                    for _ in 0..ins {
                        row.pop();
                        row.insert(col, blank);
                    }
                }
            }
            'L' => {
                // IL — Insert Lines within scroll region
                let bottom = self.scroll_bottom.min(self.rows - 1);
                if self.cursor_row >= self.scroll_top && self.cursor_row <= bottom {
                    let count = param(0, 1).min(bottom - self.cursor_row + 1);
                    for _ in 0..count {
                        self.cells.remove(bottom);
                        self.cells.insert(self.cursor_row, self.blank_row());
                    }
                }
            }
            'M' => {
                // DL — Delete Lines within scroll region
                let bottom = self.scroll_bottom.min(self.rows - 1);
                if self.cursor_row >= self.scroll_top && self.cursor_row <= bottom {
                    let count = param(0, 1).min(bottom - self.cursor_row + 1);
                    for _ in 0..count {
                        self.cells.remove(self.cursor_row);
                        self.cells.insert(bottom, self.blank_row());
                    }
                }
            }
            'S' => {
                // SU — Scroll Up within scroll region
                self.scroll_region_up(param(0, 1));
            }
            'T' => {
                // SD — Scroll Down within scroll region
                self.scroll_region_down(param(0, 1));
            }
            'r' => {
                // DECSTBM — Set Top and Bottom Margins (scroll region)
                let top = param(0, 1).saturating_sub(1).min(self.rows - 1);
                let bottom = param(1, self.rows).saturating_sub(1).min(self.rows - 1);
                if top < bottom {
                    self.scroll_top = top;
                    self.scroll_bottom = bottom;
                } else {
                    self.scroll_top = 0;
                    self.scroll_bottom = self.rows - 1;
                }
                self.cursor_row = 0;
                self.cursor_col = 0;
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
                        let col = self.cursor_col.min(self.cols - 1);
                        if let Some(row) = self.cells.get_mut(self.cursor_row) {
                            for c in col..self.cols {
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
                    2 => {
                        for row in &mut self.cells {
                            row.fill(blank);
                        }
                        self.screen_cleared = true;
                    }
                    3 => {
                        // Keep absolute line numbers monotonic: cleared scrollback counts as evicted.
                        self.evicted_lines += self.scrollback.len();
                        self.scrollback.clear();
                        self.screen_cleared = true;
                    }
                    _ => {}
                }
            }
            'K' => {
                let blank = Cell::blank(self.default_fg, self.cur_bg);
                if let Some(row) = self.cells.get_mut(self.cursor_row) {
                    match raw_param(0) {
                        0 => {
                            let col = self.cursor_col.min(self.cols - 1);
                            for c in col..self.cols {
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
                '"' | '\'' | '`' | '<' | '>' | '|' | '(' | ')' | '[' | ']' | '{' | '}' | '│' | '┃'
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

    let raw: String = row[start..end]
        .iter()
        .filter(|c| c.ch != '\0')
        .map(|c| c.ch)
        .collect();
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

/// Returns true when `url` is a valid HTTP(S) URL with no control or whitespace characters.
pub fn is_safe_browser_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    (lower.starts_with("https://") || lower.starts_with("http://"))
        && !url.chars().any(|c| c.is_control() || c.is_whitespace())
}

pub fn open_url_in_browser(url: &str) {
    if !is_safe_browser_url(url) {
        return;
    }
    #[cfg(target_os = "windows")]
    {
        use std::ffi::OsStr;
        use std::os::windows::ffi::OsStrExt;
        use std::ptr;

        #[link(name = "shell32")]
        extern "system" {
            fn ShellExecuteW(
                hwnd: *mut core::ffi::c_void,
                lp_operation: *const u16,
                lp_file: *const u16,
                lp_parameters: *const u16,
                lp_directory: *const u16,
                n_show_cmd: i32,
            ) -> isize;
        }

        let op: Vec<u16> = OsStr::new("open").encode_wide().chain(Some(0)).collect();
        let target: Vec<u16> = OsStr::new(url).encode_wide().chain(Some(0)).collect();
        unsafe {
            ShellExecuteW(
                ptr::null_mut(),
                op.as_ptr(),
                target.as_ptr(),
                ptr::null(),
                ptr::null(),
                1, // SW_SHOWNORMAL
            );
        }
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
        c.is_whitespace()
            || matches!(
                c,
                '"' | '\'' | '`' | '<' | '>' | '|' | '(' | ')' | '[' | ']' | '{' | '}' | '│' | '┃'
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

    let raw: String = row[start..end]
        .iter()
        .filter(|c| c.ch != '\0')
        .map(|c| c.ch)
        .collect();
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

/// Extracts a shell-reported working directory from `OSC 9;9;<path>` or `OSC 7;file://host/<path>`.
fn parse_cwd_osc(params: &[&[u8]]) -> Option<std::path::PathBuf> {
    let join_rest = |rest: &[&[u8]]| -> String {
        rest.iter()
            .map(|p| String::from_utf8_lossy(p).into_owned())
            .collect::<Vec<_>>()
            .join(";")
    };
    let raw = match params {
        [b"9", b"9", rest @ ..] if !rest.is_empty() => {
            join_rest(rest).trim().trim_matches('"').to_string()
        }
        [b"7", rest @ ..] if !rest.is_empty() => {
            let uri = join_rest(rest);
            let after_scheme = uri.strip_prefix("file://")?;
            let path_start = after_scheme.find('/')?;
            let mut path = percent_decode(&after_scheme[path_start..]);
            // `/D:/dir` -> `D:/dir` on Windows-style URIs
            let b = path.as_bytes();
            if b.len() >= 3 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b':' {
                path.remove(0);
            }
            path
        }
        _ => return None,
    };
    if raw.is_empty() {
        None
    } else {
        Some(std::path::PathBuf::from(raw))
    }
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(v) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clear_scrollback_keeps_abs_lines_monotonic_and_flags_screen_clear() {
        let fg = Rgb::new(220, 220, 220);
        let bg = Rgb::new(0, 0, 0);
        let mut grid = TerminalGrid::new(20, 3, fg, bg);
        assert!(!grid.take_screen_cleared());
        grid.feed(b"a\r\nb\r\nc\r\nd\r\ne");
        let before = grid.current_abs_line();
        assert_eq!(before, 4);

        // ConPTY `cls`: home + per-line erase + clear scrollback
        grid.feed(b"\x1b[H\x1b[K\r\n\x1b[K\r\n\x1b[K\x1b[H\x1b[3J");
        assert!(grid.take_screen_cleared());
        assert!(!grid.take_screen_cleared(), "flag is drained once");
        assert!(
            grid.current_abs_line() >= 2,
            "cursor at row 0 must stay at/after the old screen top, got {}",
            grid.current_abs_line()
        );

        grid.feed(b"\x1b[2J");
        assert!(grid.take_screen_cleared());
    }

    #[test]
    fn captures_shell_reported_cwd_from_osc_9_9_and_osc_7() {
        let fg = Rgb::new(220, 220, 220);
        let bg = Rgb::new(0, 0, 0);
        let mut grid = TerminalGrid::new(40, 3, fg, bg);
        assert_eq!(grid.take_reported_cwd(), None);

        grid.feed(b"\x1b]9;9;D:\\FUENTES\\remediacion-fuerzabruta\x1b\\PS remediacion-fuerzabruta> ");
        assert_eq!(
            grid.take_reported_cwd(),
            Some(std::path::PathBuf::from(r"D:\FUENTES\remediacion-fuerzabruta"))
        );
        assert_eq!(grid.take_reported_cwd(), None, "reported cwd is drained once");
        assert!(row_text(&grid, 0).starts_with("PS remediacion-fuerzabruta>"), "OSC must not print");

        grid.feed(b"\x1b]9;9;\"C:\\Program Files\"\x07");
        assert_eq!(grid.take_reported_cwd(), Some(std::path::PathBuf::from(r"C:\Program Files")));

        grid.feed(b"\x1b]7;file://host/home/me/my%20dir\x07");
        assert_eq!(grid.take_reported_cwd(), Some(std::path::PathBuf::from("/home/me/my dir")));

        grid.feed(b"\x1b]7;file:///D:/FUENTES\x07");
        assert_eq!(grid.take_reported_cwd(), Some(std::path::PathBuf::from("D:/FUENTES")));

        // Other OSC 9 notifications are ignored
        grid.feed(b"\x1b]9;4;1;50\x07");
        assert_eq!(grid.take_reported_cwd(), None);
    }

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

        // Kitty keyboard protocol query/push/pop (`CSI ? u`, `CSI > 1 u`, `CSI < 1 u`)
        assert!(!grid.kitty_keyboard_enabled());
        grid.feed(b"\x1b[?u\x1b[c");
        assert_eq!(
            grid.take_responses(),
            vec![b"\x1b[?0u".to_vec(), b"\x1b[?62;22c".to_vec()]
        );
        grid.feed(b"\x1b[>1u");
        assert!(grid.kitty_keyboard_enabled());
        assert_eq!(grid.kitty_keyboard_flags(), 1);
        grid.feed(b"\x1b[?u");
        assert_eq!(grid.take_responses(), vec![b"\x1b[?1u".to_vec()]);
        grid.feed(b"\x1b[<1u");
        assert!(!grid.kitty_keyboard_enabled());

        // xterm modifyOtherKeys (`CSI > 4 ; 2 m` / `CSI > 4 m`)
        assert_eq!(grid.modify_other_keys_level(), 0);
        grid.feed(b"\x1b[>4;2m");
        assert_eq!(grid.modify_other_keys_level(), 2);
        grid.feed(b"\x1b[>4m");
        assert_eq!(grid.modify_other_keys_level(), 0);

        // Synchronized output (`?2026h` / `?2026l`)
        assert!(!grid.is_sync_output());
        grid.feed(b"\x1b[?2026h");
        assert!(grid.is_sync_output());
        grid.feed(b"\x1b[?2026l");
        assert!(!grid.is_sync_output());

        // Zero-width variation selector does not advance cursor; wide emoji advances 2 cells
        let mut g_uni = TerminalGrid::new(10, 2, fg, bg);
        g_uni.feed("a\u{fe0f}\u{2728}b".as_bytes());
        assert_eq!(g_uni.cursor(), (0, 4)); // 'a'(1) + FE0F(0) + '✨'(2) + 'b'(1) = 4

        // Bracketed paste (`?2004h` / `?2004l`)
        assert!(!grid.bracketed_paste_enabled());
        grid.feed(b"\x1b[?2004h");
        assert!(grid.bracketed_paste_enabled());
        grid.feed(b"\x1b[?2004l");
        assert!(!grid.bracketed_paste_enabled());

        // DECSTBM scroll region (`CSI 2;4 r`) preserves pinned header & footer rows
        let mut g2 = TerminalGrid::new(20, 5, fg, bg);
        g2.feed(b"HEADER\r\nline1\r\nline2\r\nline3\r\nFOOTER");
        g2.feed(b"\x1b[2;4r\x1b[4;1H\r\nline4");
        assert_eq!(row_text(&g2, 0), "HEADER");
        assert_eq!(row_text(&g2, 1), "line2");
        assert_eq!(row_text(&g2, 3), "line4");
        assert_eq!(row_text(&g2, 4), "FOOTER");

        // commit_screen_to_scrollback preserves built-in output intact and clears screen for PTY
        g2.commit_screen_to_scrollback();
        assert_eq!(g2.scrollback_len(), 5);
        assert_eq!(row_text(&g2, 0), "");
    }

    #[test]
    fn leave_alt_screen_after_resize_normalizes_main_grid_without_panic() {
        let fg = Rgb::new(220, 220, 220);
        let bg = Rgb::new(10, 10, 10);
        let mut grid = TerminalGrid::new(20, 5, fg, bg);
        grid.feed(b"main line 1\r\nmain line 2");

        // Enter alt screen, grow window, leave alt screen, then clear/scroll
        grid.feed(b"\x1b[?1049h");
        grid.resize(40, 12);
        grid.feed(b"\x1b[?1049l");

        assert_eq!(grid.cols(), 40);
        assert_eq!(grid.rows(), 12);
        grid.feed(b"\x1b[2J\x1b[12;1Hbottom\r\nscrolled");
        assert!(grid.visible_row(11, 0).is_some());

        // Enter alt screen again, shrink window, leave alt screen
        grid.feed(b"\x1b[?1049h");
        grid.resize(10, 3);
        grid.feed(b"\x1b[?1049l");
        assert_eq!(grid.cols(), 10);
        assert_eq!(grid.rows(), 3);
        grid.feed(b"\x1b[K");
    }

    #[test]
    fn is_safe_browser_url_allows_http_query_and_rejects_unsafe_schemes() {
        assert!(is_safe_browser_url("https://example.com/?a=1&b=2"));
        assert!(is_safe_browser_url("http://localhost:3000/api?x=1&y=2"));
        assert!(!is_safe_browser_url("file:///C:/Windows/System32/calc.exe"));
        assert!(!is_safe_browser_url("javascript:alert(1)"));
        assert!(!is_safe_browser_url("calc.exe"));
        assert!(!is_safe_browser_url("https://example.com/\r\ncalc.exe"));
        assert!(!is_safe_browser_url("https://example.com/ with space"));
    }

    #[test]
    fn wide_char_continuation_cells_are_stripped_from_selection_and_transcript() {
        let fg = Rgb::new(220, 220, 220);
        let bg = Rgb::new(10, 10, 10);
        let mut grid = TerminalGrid::new(20, 3, fg, bg);
        grid.feed("hi ✨ world".as_bytes());

        let selected = grid.extract_selection((0, 0), (0, 12), 0);
        assert_eq!(selected, "hi ✨ world");
        assert!(!selected.contains('\0'));

        let abs = grid.extract_abs_range(0, 0);
        assert_eq!(abs, "hi ✨ world");
        assert!(!abs.contains('\0'));

        let transcript = grid.dump_transcript();
        assert_eq!(transcript, "hi ✨ world");
        assert!(!transcript.contains('\0'));
    }

    #[test]
    fn sgr_mouse_mode_1006_does_not_leave_mouse_tracking_stuck() {
        let fg = Rgb::new(220, 220, 220);
        let bg = Rgb::new(10, 10, 10);
        let mut grid = TerminalGrid::new(20, 5, fg, bg);

        grid.feed(b"\x1b[?1006h");
        assert!(grid.sgr_mouse_enabled());
        assert!(!grid.mouse_tracking_enabled());
        grid.feed(b"\x1b[?1006l");
        assert!(!grid.sgr_mouse_enabled());
        assert!(!grid.mouse_tracking_enabled());
        assert!(!grid.is_tui_active());
    }

    #[test]
    fn leave_alt_screen_shrink_preserves_scrolled_main_lines_in_scrollback() {
        let fg = Rgb::new(220, 220, 220);
        let bg = Rgb::new(10, 10, 10);
        let mut grid = TerminalGrid::new(20, 5, fg, bg);
        grid.feed(b"line0\r\nline1\r\nline2\r\nline3\r\nline4");
        assert_eq!(grid.scrollback_len(), 0);

        grid.feed(b"\x1b[?1049h");
        grid.resize(20, 3);
        grid.feed(b"\x1b[?1049l");

        assert_eq!(grid.scrollback_len(), 2);
        assert!(grid.dump_transcript().starts_with("line0\nline1\n"));
    }

    #[test]
    fn search_matches_handles_wide_chars_and_full_column_span() {
        let fg = Rgb::new(220, 220, 220);
        let bg = Rgb::new(10, 10, 10);
        let mut grid = TerminalGrid::new(20, 2, fg, bg);
        grid.feed("hi ✨ world".as_bytes());

        let m = grid.search_matches("✨ world");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].col_start, 3);
        assert_eq!(m[0].col_end, 11); // '✨'(2 cols) + " world"(6 cols) = 8 cols -> 3..11
    }

    #[test]
    fn vt_csi_3j_clears_scrollback_and_pending_wrap_clamps_dsr_and_erase() {
        let fg = Rgb::new(220, 220, 220);
        let bg = Rgb::new(10, 10, 10);
        let mut grid = TerminalGrid::new(4, 2, fg, bg);
        grid.feed(b"aaaa\r\nbbbb\r\ncccc");
        assert!(grid.scrollback_len() > 0);

        // CSI 3 J clears scrollback without wiping active screen
        grid.feed(b"\x1b[3J");
        assert_eq!(grid.scrollback_len(), 0);
        assert_eq!(grid.extract_selection((1, 0), (1, 3), 0), "cccc");

        // Pending wrap: after printing 4 chars in 4-col grid, cursor_col == 4
        // DSR CSI 6 n must report col 4 (not 5) and CSI 0 K must erase last cell
        let mut g2 = TerminalGrid::new(4, 2, fg, bg);
        g2.feed(b"abcd\x1b[6n");
        let resp = g2.take_responses();
        assert_eq!(resp, vec![b"\x1b[1;4R".to_vec()]);
        g2.feed(b"\x1b[0K");
        assert_eq!(g2.extract_selection((0, 0), (0, 3), 0), "abc");
    }

    #[test]
    fn responds_to_osc_10_and_11_color_queries_and_parses_colon_sgr_and_dim() {
        let fg = Rgb::new(0xE0, 0xE2, 0xEC);
        let bg = Rgb::new(0x0C, 0x0E, 0x14);
        let mut grid = TerminalGrid::new(20, 3, fg, bg);

        // OSC 10 & OSC 11 queries (used by Codex / Ratatui to theme input box)
        grid.feed(b"\x1b]10;?\x07\x1b]11;?\x1b\\");
        let resps = grid.take_responses();
        assert_eq!(resps.len(), 2);
        assert_eq!(resps[0], b"\x1b]10;rgb:e0e0/e2e2/ecec\x1b\\");
        assert_eq!(resps[1], b"\x1b]11;rgb:0c0c/0e0e/1414\x1b\\");

        // Colon-separated truecolor background (`48:2::30:34:48`) + SGR 2 dim
        grid.feed(b"\x1b[2;48:2::30:34:48mX\x1b[22;49mY");
        let row = grid.visible_row(0, 0).unwrap();
        assert_eq!(row[0].ch, 'X');
        assert_eq!(row[0].bg, Rgb::new(30, 34, 48));
        assert!(row[0].dim);
        assert_eq!(row[1].ch, 'Y');
        assert_eq!(row[1].bg, bg);
        assert!(!row[1].dim);
    }
}
