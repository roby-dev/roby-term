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

/// Warp-style command input bar with cursor editing, history, and passthrough toggle.
#[derive(Debug, Clone)]
pub struct InputBar {
    buffer: String,
    /// Byte offset in `buffer` (always on a UTF-8 char boundary).
    cursor: usize,
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
            history: VecDeque::with_capacity(64),
            history_index: None,
            draft_before_history: String::new(),
            passthrough: false,
        }
    }

    pub fn text(&self) -> &str {
        &self.buffer
    }

    /// Cursor position in UTF-8 characters (for rendering).
    pub fn cursor_char_index(&self) -> usize {
        self.buffer[..self.cursor].chars().count()
    }

    pub fn tokens(&self) -> Vec<Token> {
        tokenize_command(&self.buffer)
    }

    pub fn insert_str(&mut self, s: &str) {
        self.buffer.insert_str(self.cursor, s);
        self.cursor += s.len();
        self.history_index = None;
    }

    pub fn insert_char(&mut self, ch: char) {
        self.buffer.insert(self.cursor, ch);
        self.cursor += ch.len_utf8();
        self.history_index = None;
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let prev = self.prev_char_boundary();
        self.buffer.drain(prev..self.cursor);
        self.cursor = prev;
        self.history_index = None;
    }

    pub fn delete(&mut self) {
        if self.cursor >= self.buffer.len() {
            return;
        }
        let next = self.next_char_boundary();
        self.buffer.drain(self.cursor..next);
        self.history_index = None;
    }

    pub fn move_left(&mut self) {
        if self.cursor > 0 {
            self.cursor = self.prev_char_boundary();
        }
    }

    pub fn move_right(&mut self) {
        if self.cursor < self.buffer.len() {
            self.cursor = self.next_char_boundary();
        }
    }

    pub fn move_home(&mut self) {
        self.cursor = 0;
    }

    pub fn move_end(&mut self) {
        self.cursor = self.buffer.len();
    }

    pub fn clear(&mut self) {
        self.buffer.clear();
        self.cursor = 0;
        self.history_index = None;
    }

    /// Submits the current buffer, saves non-empty commands to history, and clears the bar.
    pub fn submit(&mut self) -> String {
        let cmd = std::mem::take(&mut self.buffer);
        self.cursor = 0;
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
}
