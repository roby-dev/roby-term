use std::collections::VecDeque;
use std::ops::Range;

const MAX_HISTORY: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Command,
    Flag,
    StringLiteral,
    Path,
    EnvVar,
    Operator,
    Argument,
    Whitespace,
    Comment,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub range: Range<usize>,
    pub kind: TokenKind,
}

/// Tokenizes a shell command line for live Warp-style syntax coloring.
pub fn tokenize_command(input: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let bytes = input.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut expect_command = true;

    while i < len {
        let b = bytes[i];

        // Whitespace
        if b.is_ascii_whitespace() {
            let start = i;
            while i < len && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            tokens.push(Token {
                range: start..i,
                kind: TokenKind::Whitespace,
            });
            continue;
        }

        // Comment (# to end of line)
        if b == b'#' && (i == 0 || bytes[i - 1].is_ascii_whitespace()) {
            tokens.push(Token {
                range: i..len,
                kind: TokenKind::Comment,
            });
            break;
        }

        // Quoted string literal
        if b == b'"' || b == b'\'' {
            let quote = b;
            let start = i;
            i += 1;
            while i < len {
                if bytes[i] == b'\\' && quote == b'"' && i + 1 < len {
                    i += 2;
                    continue;
                }
                if bytes[i] == quote {
                    i += 1;
                    break;
                }
                i += 1;
            }
            tokens.push(Token {
                range: start..i,
                kind: TokenKind::StringLiteral,
            });
            expect_command = false;
            continue;
        }

        // Shell operators: |, ||, &&, ;, >, >>, <
        if matches!(b, b'|' | b'&' | b';' | b'>' | b'<') {
            let start = i;
            if i + 1 < len && ((b == b'|' && bytes[i + 1] == b'|')
                || (b == b'&' && bytes[i + 1] == b'&')
                || (b == b'>' && bytes[i + 1] == b'>'))
            {
                i += 2;
            } else {
                i += 1;
            }
            let resets_cmd = matches!(b, b'|' | b'&' | b';');
            tokens.push(Token {
                range: start..i,
                kind: TokenKind::Operator,
            });
            if resets_cmd {
                expect_command = true;
            }
            continue;
        }

        // Word token
        let start = i;
        while i < len
            && !bytes[i].is_ascii_whitespace()
            && !matches!(bytes[i], b'|' | b'&' | b';' | b'>' | b'<' | b'"' | b'\'')
        {
            i += 1;
        }
        let word = &input[start..i];
        let kind = if expect_command {
            expect_command = false;
            TokenKind::Command
        } else {
            classify_word(word)
        };
        tokens.push(Token {
            range: start..i,
            kind,
        });
    }

    tokens
}

fn classify_word(word: &str) -> TokenKind {
    // Environment variables: $HOME, $env:PATH, %USERPROFILE%
    if (word.starts_with('$') && word.len() > 1)
        || (word.starts_with('%') && word.ends_with('%') && word.len() > 2)
    {
        return TokenKind::EnvVar;
    }

    // Flags / parameters: --release, -la, /s (short Windows CMD flag)
    if (word.starts_with('-') && word.len() > 1)
        || (word.starts_with('/') && word.len() == 2 && word.as_bytes()[1].is_ascii_alphanumeric())
    {
        return TokenKind::Flag;
    }

    // Paths: ./foo, ../bar, ~/docs, C:\Users, /etc/hosts, src/main.rs
    if word.starts_with("./")
        || word.starts_with("../")
        || word.starts_with(".\\")
        || word.starts_with("..\\")
        || word.starts_with("~/")
        || word.starts_with('/')
        || word.contains('\\')
        || word.contains('/')
        || (word.len() >= 2 && word.as_bytes()[0].is_ascii_alphabetic() && word.as_bytes()[1] == b':')
    {
        return TokenKind::Path;
    }

    TokenKind::Argument
}

/// Command input bar with cursor editing, Shift/Ctrl+A text selection, history, and passthrough toggle.
#[derive(Debug, Clone)]
pub struct InputBar {
    buffer: String,
    /// Byte offset in `buffer` (always on a UTF-8 char boundary).
    cursor: usize,
    /// Optional byte offset where a Shift/Ctrl+A selection started.
    selection_anchor: Option<usize>,
    history: VecDeque<String>,
    history_index: Option<usize>,
    draft_before_history: String,
    /// When true, keystrokes go directly to the PTY (for interactive TUIs like vim/htop).
    pub passthrough: bool,
}

impl Default for InputBar {
    fn default() -> Self {
        Self::new()
    }
}

impl InputBar {
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
            cursor: 0,
            selection_anchor: None,
            history: VecDeque::with_capacity(64),
            history_index: None,
            draft_before_history: String::new(),
            passthrough: false,
        }
    }

    pub fn text(&self) -> &str {
        &self.buffer
    }

    pub fn is_cursor_at_end(&self) -> bool {
        self.cursor == self.buffer.len() && self.selected_range().is_none()
    }

    pub fn set_text(&mut self, text: impl Into<String>) {
        self.buffer = text.into();
        self.cursor = self.buffer.len();
        self.selection_anchor = None;
        self.history_index = None;
    }

    pub fn selected_range(&self) -> Option<(usize, usize)> {
        let anchor = self.selection_anchor?;
        if anchor == self.cursor {
            None
        } else if anchor < self.cursor {
            Some((anchor, self.cursor))
        } else {
            Some((self.cursor, anchor))
        }
    }

    pub fn selected_char_range(&self) -> Option<(usize, usize)> {
        let (a, b) = self.selected_range()?;
        let ca = self.buffer[..a].chars().count();
        let cb = self.buffer[..b].chars().count();
        Some((ca, cb))
    }

    pub fn selected_text(&self) -> Option<&str> {
        let (a, b) = self.selected_range()?;
        Some(&self.buffer[a..b])
    }

    pub fn select_all(&mut self) {
        if !self.buffer.is_empty() {
            self.selection_anchor = Some(0);
            self.cursor = self.buffer.len();
        }
    }

    pub fn clear_selection(&mut self) {
        self.selection_anchor = None;
    }

    pub fn delete_selection(&mut self) -> bool {
        if let Some((a, b)) = self.selected_range() {
            self.buffer.drain(a..b);
            self.cursor = a;
            self.selection_anchor = None;
            self.history_index = None;
            true
        } else {
            self.selection_anchor = None;
            false
        }
    }

    pub fn seed_history(&mut self, items: Vec<String>) {
        for item in items {
            let trimmed = item.trim();
            if !trimmed.is_empty() && self.history.back().map(|s| s.as_str()) != Some(trimmed) {
                if self.history.len() >= MAX_HISTORY {
                    self.history.pop_front();
                }
                self.history.push_back(trimmed.to_string());
            }
        }
    }

    /// Cursor position in UTF-8 characters (for rendering).
    pub fn cursor_char_index(&self) -> usize {
        self.buffer[..self.cursor].chars().count()
    }

    pub fn tokens(&self) -> Vec<Token> {
        tokenize_command(&self.buffer)
    }

    pub fn insert_str(&mut self, s: &str) {
        self.delete_selection();
        self.buffer.insert_str(self.cursor, s);
        self.cursor += s.len();
        self.history_index = None;
    }

    pub fn insert_char(&mut self, ch: char) {
        self.delete_selection();
        self.buffer.insert(self.cursor, ch);
        self.cursor += ch.len_utf8();
        self.history_index = None;
    }

    pub fn backspace(&mut self) {
        if self.delete_selection() {
            return;
        }
        if self.cursor == 0 {
            return;
        }
        let prev = self.prev_char_boundary();
        self.buffer.drain(prev..self.cursor);
        self.cursor = prev;
        self.history_index = None;
    }

    pub fn delete(&mut self) {
        if self.delete_selection() {
            return;
        }
        if self.cursor >= self.buffer.len() {
            return;
        }
        let next = self.next_char_boundary();
        self.buffer.drain(self.cursor..next);
        self.history_index = None;
    }

    pub fn move_left(&mut self) {
        self.move_left_with_select(false);
    }

    pub fn move_left_with_select(&mut self, select: bool) {
        if select {
            if self.selection_anchor.is_none() {
                self.selection_anchor = Some(self.cursor);
            }
            if self.cursor > 0 {
                self.cursor = self.prev_char_boundary();
            }
        } else if let Some((a, _)) = self.selected_range() {
            self.cursor = a;
            self.selection_anchor = None;
        } else {
            self.selection_anchor = None;
            if self.cursor > 0 {
                self.cursor = self.prev_char_boundary();
            }
        }
    }

    pub fn move_right(&mut self) {
        self.move_right_with_select(false);
    }

    pub fn move_right_with_select(&mut self, select: bool) {
        if select {
            if self.selection_anchor.is_none() {
                self.selection_anchor = Some(self.cursor);
            }
            if self.cursor < self.buffer.len() {
                self.cursor = self.next_char_boundary();
            }
        } else if let Some((_, b)) = self.selected_range() {
            self.cursor = b;
            self.selection_anchor = None;
        } else {
            self.selection_anchor = None;
            if self.cursor < self.buffer.len() {
                self.cursor = self.next_char_boundary();
            }
        }
    }

    pub fn move_home(&mut self) {
        self.move_home_with_select(false);
    }

    pub fn move_home_with_select(&mut self, select: bool) {
        if select {
            if self.selection_anchor.is_none() {
                self.selection_anchor = Some(self.cursor);
            }
        } else {
            self.selection_anchor = None;
        }
        self.cursor = 0;
    }

    pub fn move_end(&mut self) {
        self.move_end_with_select(false);
    }

    pub fn move_end_with_select(&mut self, select: bool) {
        if select {
            if self.selection_anchor.is_none() {
                self.selection_anchor = Some(self.cursor);
            }
        } else {
            self.selection_anchor = None;
        }
        self.cursor = self.buffer.len();
    }

    pub fn clear(&mut self) {
        self.buffer.clear();
        self.cursor = 0;
        self.selection_anchor = None;
        self.history_index = None;
    }

    /// Submits the current buffer, saves non-empty commands to history, and clears the bar.
    pub fn submit(&mut self) -> String {
        let cmd = std::mem::take(&mut self.buffer);
        self.cursor = 0;
        self.selection_anchor = None;
        self.history_index = None;
        self.draft_before_history.clear();

        let trimmed = cmd.trim();
        if !trimmed.is_empty() && self.history.back().map(|s| s.as_str()) != Some(trimmed) {
            if self.history.len() >= MAX_HISTORY {
                self.history.pop_front();
            }
            self.history.push_back(trimmed.to_string());
        }
        cmd
    }

    pub fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let next_idx = match self.history_index {
            None => {
                self.draft_before_history = self.buffer.clone();
                self.history.len() - 1
            }
            Some(0) => 0,
            Some(idx) => idx - 1,
        };
        self.history_index = Some(next_idx);
        self.buffer = self.history[next_idx].clone();
        self.cursor = self.buffer.len();
        self.selection_anchor = None;
    }

    pub fn history_next(&mut self) {
        let Some(idx) = self.history_index else {
            return;
        };
        if idx + 1 < self.history.len() {
            let next_idx = idx + 1;
            self.history_index = Some(next_idx);
            self.buffer = self.history[next_idx].clone();
        } else {
            self.history_index = None;
            self.buffer = std::mem::take(&mut self.draft_before_history);
        }
        self.cursor = self.buffer.len();
        self.selection_anchor = None;
    }

    fn prev_char_boundary(&self) -> usize {
        self.buffer[..self.cursor]
            .char_indices()
            .next_back()
            .map(|(idx, _)| idx)
            .unwrap_or(0)
    }

    fn next_char_boundary(&self) -> usize {
        self.buffer[self.cursor..]
            .chars()
            .next()
            .map(|c| self.cursor + c.len_utf8())
            .unwrap_or(self.buffer.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizes_command_flags_paths_strings_env_and_pipes() {
        let line = r#"git commit -m "init" ./src/main.rs $HOME | rg --color=always foo"#;
        let tokens = tokenize_command(line);
        let non_ws: Vec<(&str, TokenKind)> = tokens
            .iter()
            .filter(|t| t.kind != TokenKind::Whitespace)
            .map(|t| (&line[t.range.clone()], t.kind))
            .collect();

        assert_eq!(
            non_ws,
            vec![
                ("git", TokenKind::Command),
                ("commit", TokenKind::Argument),
                ("-m", TokenKind::Flag),
                ("\"init\"", TokenKind::StringLiteral),
                ("./src/main.rs", TokenKind::Path),
                ("$HOME", TokenKind::EnvVar),
                ("|", TokenKind::Operator),
                ("rg", TokenKind::Command),
                ("--color=always", TokenKind::Flag),
                ("foo", TokenKind::Argument),
            ]
        );
    }

    #[test]
    fn input_bar_editing_and_history_roundtrip() {
        let mut bar = InputBar::new();
        bar.insert_str("cargo test --release");
        assert_eq!(bar.submit(), "cargo test --release");
        assert_eq!(bar.text(), "");

        bar.insert_str("ls -la");
        assert_eq!(bar.submit(), "ls -la");

        bar.insert_str("draft");
        bar.history_prev();
        assert_eq!(bar.text(), "ls -la");
        bar.history_prev();
        assert_eq!(bar.text(), "cargo test --release");
        bar.history_next();
        assert_eq!(bar.text(), "ls -la");
        bar.history_next();
        assert_eq!(bar.text(), "draft");

        bar.move_left();
        bar.backspace();
        assert_eq!(bar.text(), "drat");
    }

    #[test]
    fn shift_home_end_and_ctrl_a_select_and_replace_text() {
        let mut bar = InputBar::new();
        bar.set_text("PowerShell 7 #1");

        // Shift+Home selects whole line from cursor at end, Backspace clears it
        bar.move_home_with_select(true);
        assert_eq!(bar.selected_text(), Some("PowerShell 7 #1"));
        bar.backspace();
        assert_eq!(bar.text(), "");

        // Type new name, Shift+Left selects tail, typing replaces only selection
        bar.insert_str("Backend API");
        bar.move_left_with_select(true);
        bar.move_left_with_select(true);
        bar.move_left_with_select(true);
        assert_eq!(bar.selected_text(), Some("API"));
        bar.insert_str("Core");
        assert_eq!(bar.text(), "Backend Core");

        // Ctrl+A selects all and typing overwrites
        bar.select_all();
        assert_eq!(bar.selected_text(), Some("Backend Core"));
        bar.insert_str("DB");
        assert_eq!(bar.text(), "DB");
    }

    #[test]
    fn history_navigation_clears_selection_anchor_without_panic() {
        let mut bar = InputBar::new();
        bar.set_text("ls");
        let _ = bar.submit();

        // Type a much longer draft and select part of it
        bar.set_text("this is a very long draft command");
        bar.move_left_with_select(true);
        bar.move_left_with_select(true);
        assert!(bar.selected_range().is_some());

        // Recalling shorter history entry must clear selection_anchor
        bar.history_prev();
        assert_eq!(bar.text(), "ls");
        assert_eq!(bar.selected_range(), None);
        assert_eq!(bar.selected_char_range(), None);

        // Navigating forward with active selection must also clear selection_anchor
        bar.select_all();
        assert!(bar.selected_range().is_some());
        bar.history_next();
        assert_eq!(bar.text(), "this is a very long draft command");
        assert_eq!(bar.selected_range(), None);
        assert_eq!(bar.selected_char_range(), None);
        bar.backspace();
        assert_eq!(bar.text(), "this is a very long draft comman");
    }
}
