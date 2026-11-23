use crate::builtin::try_run_builtin;
use crate::complete::{detect_git_branch, maybe_update_cwd, strip_verbatim_prefix};
use crate::input::InputBar;
use crate::shell::{default_shell, ShellProfile};
use crate::terminal::TerminalGrid;
use crate::theme::Rgb;
use crate::workspace::{SavedSession, WorkspaceSnapshot};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::thread;

pub type WakeupFn = Arc<dyn Fn() + Send + Sync>;

const MAX_BLOCKS_PER_SESSION: usize = 200;

/// Warp-style logical block grouping a submitted command with its terminal output line range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandBlock {
    pub id: usize,
    pub command: String,
    pub start_abs_line: usize,
    pub end_abs_line: Option<usize>,
}

/// A single terminal session backed by an OS PTY (ConPTY on Windows, POSIX PTY on macOS/Linux).
pub struct Session {
    pub id: usize,
    pub title: String,
    pub shell: ShellProfile,
    pub cwd: PathBuf,
    pub git_branch: Option<String>,
    pub grid: TerminalGrid,
    pub input: InputBar,
    pub blocks: Vec<CommandBlock>,
    next_block_id: usize,
    pub scroll_offset: usize,
    pub has_unread_output: bool,
    pub alive: bool,
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    rx: Receiver<Vec<u8>>,
}

impl Session {
    pub fn spawn(
        id: usize,
        shell: ShellProfile,
        cols: usize,
        rows: usize,
        default_fg: Rgb,
        default_bg: Rgb,
        wakeup: Option<WakeupFn>,
    ) -> Result<Self, String> {
        Self::spawn_with_state(id, shell, cols, rows, default_fg, default_bg, wakeup, None, None, None)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn spawn_with_state(
        id: usize,
        shell: ShellProfile,
        cols: usize,
        rows: usize,
        default_fg: Rgb,
        default_bg: Rgb,
        wakeup: Option<WakeupFn>,
        initial_cwd: Option<PathBuf>,
        custom_title: Option<String>,
        restored_transcript: Option<&str>,
    ) -> Result<Self, String> {
        let pty_system = native_pty_system();
        let size = PtySize {
            rows: rows.max(2) as u16,
            cols: cols.max(2) as u16,
            pixel_width: 0,
            pixel_height: 0,
        };
        let pair = pty_system
            .openpty(size)
            .map_err(|e| format!("openpty failed: {e}"))?;

        let cwd = initial_cwd
            .map(strip_verbatim_prefix)
            .filter(|p| p.is_dir())
            .or_else(|| std::env::current_dir().ok().map(strip_verbatim_prefix))
            .unwrap_or_else(|| PathBuf::from("."));

        let mut cmd = CommandBuilder::new(&shell.program);
        for arg in &shell.args {
            cmd.arg(arg);
        }
        cmd.cwd(&cwd);
        cmd.env("TERM", "xterm-256color");

        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| format!("spawn {} failed: {e}", shell.program))?;
        drop(pair.slave);

        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| format!("clone PTY reader failed: {e}"))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| format!("take PTY writer failed: {e}"))?;

        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        thread::Builder::new()
            .name(format!("pty-read-{id}"))
            .spawn(move || {
                let mut buf = [0u8; 4096];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            if tx.send(buf[..n].to_vec()).is_err() {
                                break;
                            }
                            if let Some(ref wake) = wakeup {
                                wake();
                            }
                        }
                        Err(_) => break,
                    }
                }
                if let Some(ref wake) = wakeup {
                    wake();
                }
            })
            .map_err(|e| format!("spawn reader thread failed: {e}"))?;

        let title = custom_title
            .filter(|t| !t.trim().is_empty())
            .unwrap_or_else(|| format!("{} #{}", shell.name, id));
        let git_branch = detect_git_branch(&cwd);
        let mut grid = TerminalGrid::new(cols, rows, default_fg, default_bg);

        if let Some(transcript) = restored_transcript {
            if !transcript.trim().is_empty() {
                grid.feed(b"\x1b[38;2;134;144;168m");
                for line in transcript.lines() {
                    grid.feed(line.as_bytes());
                    grid.feed(b"\r\n");
                }
                grid.feed(b"--- session restored ---\x1b[0m\r\n");
            }
        }

        Ok(Self {
            id,
            title,
            shell,
            cwd,
            git_branch,
            grid,
            input: InputBar::new(),
            blocks: Vec::new(),
            next_block_id: 1,
            scroll_offset: 0,
            has_unread_output: false,
            alive: true,
            master: pair.master,
            writer,
            child,
            rx,
        })
    }

    /// Drains all pending PTY bytes into the terminal grid. Returns `true` if anything changed.
    pub fn poll_output(&mut self) -> bool {
        let mut changed = false;
        loop {
            match self.rx.try_recv() {
                Ok(chunk) => {
                    self.grid.feed(&chunk);
                    changed = true;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if self.alive {
                        self.alive = false;
                        changed = true;
                    }
                    break;
                }
            }
        }
        changed
    }

    /// Writes raw bytes to the PTY stdin.
    pub fn write_bytes(&mut self, data: &[u8]) {
        if !self.alive {
            return;
        }
        let _ = self.writer.write_all(data);
        let _ = self.writer.flush();
    }

    /// Submits a command line from the Warp-style input bar to the PTY (or runs a native built-in like `ls`).
    pub fn submit_command(&mut self, cmd: &str, accent: Rgb) {
        self.scroll_offset = 0;
        let cur_abs = self.grid.current_abs_line();
        if let Some(prev) = self.blocks.last_mut() {
            if prev.end_abs_line.is_none() {
                prev.end_abs_line = Some(cur_abs.saturating_sub(1).max(prev.start_abs_line));
            }
        }
        let trimmed = cmd.trim();
        if !trimmed.is_empty() {
            if self.blocks.len() >= MAX_BLOCKS_PER_SESSION {
                self.blocks.remove(0);
            }
            let id = self.next_block_id;
            self.next_block_id += 1;
            self.blocks.push(CommandBlock {
                id,
                command: trimmed.to_string(),
                start_abs_line: cur_abs,
                end_abs_line: None,
            });
        }

        // Fast cross-platform native `ls` / `ll` / `la` rendered directly into the terminal grid
        if let Some(output) = try_run_builtin(cmd, &self.cwd, accent) {
            self.grid.feed(output.as_bytes());
            let end_abs = self.grid.current_abs_line();
            if let Some(last) = self.blocks.last_mut() {
                last.end_abs_line = Some(end_abs);
            }
            return;
        }

        if let Some(next_cwd) = maybe_update_cwd(&self.cwd, cmd) {
            self.cwd = next_cwd;
        }
        self.git_branch = detect_git_branch(&self.cwd);
        if is_interactive_command(cmd) {
            self.input.passthrough = true;
        }
        let mut line = String::with_capacity(cmd.len() + 1);
        line.push_str(cmd);
        line.push('\r');
        self.write_bytes(line.as_bytes());
    }

    /// Extracts the full text of a command block (from its start line through its end or current line).
    pub fn block_output_text(&self, block_idx: usize) -> Option<String> {
        let block = self.blocks.get(block_idx)?;
        let end = block
            .end_abs_line
            .unwrap_or_else(|| self.grid.current_abs_line());
        Some(self.grid.extract_abs_range(block.start_abs_line, end))
    }

    pub fn resize(&mut self, cols: usize, rows: usize) {
        let cols = cols.max(2);
        let rows = rows.max(2);
        if cols == self.grid.cols() && rows == self.grid.rows() {
            return;
        }
        self.grid.resize(cols, rows);
        let _ = self.master.resize(PtySize {
            rows: rows as u16,
            cols: cols as u16,
            pixel_width: 0,
            pixel_height: 0,
        });
    }

    pub fn scroll_up(&mut self, lines: usize) {
        let max_scroll = self.grid.scrollback_len();
        self.scroll_offset = (self.scroll_offset + lines).min(max_scroll);
    }

    pub fn scroll_down(&mut self, lines: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(lines);
    }

    pub fn effective_passthrough(&self) -> bool {
        self.input.passthrough || self.grid.is_tui_active()
    }
}

/// Detects interactive CLI agents, TUIs, editors, remote shells, and bare REPLs
/// so Passthrough mode activates automatically when they are launched.
pub fn is_interactive_command(raw_cmd: &str) -> bool {
    let trimmed = raw_cmd.trim();
    if trimmed.is_empty() {
        return false;
    }
    let parts: Vec<&str> = trimmed.split_whitespace().collect();
    let Some(&first) = parts.first() else {
        return false;
    };
    let rest = &parts[1..];

    // Help/version invocations exit immediately; don't switch to passthrough.
    if rest
        .iter()
        .any(|a| matches!(*a, "--help" | "-h" | "--version" | "-v" | "-V" | "help" | "version"))
    {
        return false;
    }

    let bin = first
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(first)
        .trim_end_matches(".exe")
        .trim_end_matches(".cmd")
        .trim_end_matches(".bat")
        .to_ascii_lowercase();

    // Always-interactive AI agents, TUIs, editors, and remote shells
    if matches!(
        bin.as_str(),
        "agy"
            | "claude"
            | "codex"
            | "opencode"
            | "aider"
            | "gemini"
            | "vim"
            | "nvim"
            | "vi"
            | "nano"
            | "hx"
            | "helix"
            | "micro"
            | "htop"
            | "btop"
            | "top"
            | "less"
            | "more"
            | "man"
            | "lazygit"
            | "lazydocker"
            | "fzf"
            | "tig"
            | "tmux"
            | "zellij"
            | "k9s"
            | "ssh"
            | "sftp"
            | "psql"
            | "mysql"
            | "sqlite3"
            | "mongosh"
            | "redis-cli"
    ) {
        return true;
    }

    // Bare REPLs (interactive only when invoked with no script arguments)
    if matches!(bin.as_str(), "python" | "python3" | "py" | "node" | "irb" | "iex") && rest.is_empty()
    {
        return true;
    }

    // `docker exec -it ...` / `docker run -it ...` / `kubectl exec -it ...`
    if matches!(bin.as_str(), "docker" | "podman" | "kubectl")
        && rest.iter().any(|a| *a == "-it" || *a == "-ti")
    {
        return true;
    }

    false
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

/// Manages multiple concurrent terminal sessions, active selection, and optional side-by-side split pane.
pub struct SessionManager {
    sessions: Vec<Session>,
    active: usize,
    /// Optional second session shown side-by-side in split view: `(left_idx, right_idx)`.
    split_pair: Option<(usize, usize)>,
    next_id: usize,
    cols: usize,
    rows: usize,
    default_fg: Rgb,
    default_bg: Rgb,
    wakeup: Option<WakeupFn>,
}

impl SessionManager {
    pub fn new(
        cols: usize,
        rows: usize,
        default_fg: Rgb,
        default_bg: Rgb,
        wakeup: Option<WakeupFn>,
    ) -> Self {
        Self {
            sessions: Vec::new(),
            active: 0,
            split_pair: None,
            next_id: 1,
            cols: cols.max(2),
            rows: rows.max(2),
            default_fg,
            default_bg,
            wakeup,
        }
    }

    pub fn is_split(&self) -> bool {
        self.split_pair.is_some()
    }

    pub fn split_panes(&self) -> Option<(usize, usize)> {
        self.split_pair
    }

    /// Toggles side-by-side split view. When opening a split, spawns a new session with `shell`
    /// on the right pane; when already split, closes the split layout (keeping both sessions open).
    pub fn toggle_split(&mut self, shell: ShellProfile) -> Result<bool, String> {
        if self.split_pair.is_some() {
            self.split_pair = None;
            return Ok(false);
        }
        if self.sessions.is_empty() {
            return Ok(false);
        }
        let left = self.active;
        let right = self.spawn_session(shell)?;
        self.split_pair = Some((left, right));
        self.active = right;
        Ok(true)
    }

    pub fn get(&self, idx: usize) -> Option<&Session> {
        self.sessions.get(idx)
    }

    pub fn get_mut(&mut self, idx: usize) -> Option<&mut Session> {
        self.sessions.get_mut(idx)
    }

    pub fn spawn_session(&mut self, shell: ShellProfile) -> Result<usize, String> {
        let id = self.next_id;
        let session = Session::spawn(
            id,
            shell,
            self.cols,
            self.rows,
            self.default_fg,
            self.default_bg,
            self.wakeup.clone(),
        )?;
        self.next_id += 1;
        self.sessions.push(session);
        self.active = self.sessions.len() - 1;
        Ok(self.active)
    }

    pub fn sessions(&self) -> &[Session] {
        &self.sessions
    }

    pub fn active_index(&self) -> usize {
        self.active
    }

    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    pub fn active(&self) -> Option<&Session> {
        self.sessions.get(self.active)
    }

    pub fn active_mut(&mut self) -> Option<&mut Session> {
        self.sessions.get_mut(self.active)
    }

    pub fn select(&mut self, idx: usize) -> bool {
        if idx < self.sessions.len() {
            if let Some(sess) = self.sessions.get_mut(idx) {
                sess.has_unread_output = false;
            }
            if idx != self.active {
                self.active = idx;
                return true;
            }
        }
        false
    }

    pub fn rename(&mut self, idx: usize, new_title: &str) -> bool {
        let trimmed = new_title.trim();
        if trimmed.is_empty() {
            return false;
        }
        if let Some(sess) = self.sessions.get_mut(idx) {
            sess.title = trimmed.to_string();
            true
        } else {
            false
        }
    }

    pub fn select_next(&mut self) -> bool {
        if self.sessions.len() <= 1 {
            return false;
        }
        self.active = (self.active + 1) % self.sessions.len();
        if let Some(sess) = self.sessions.get_mut(self.active) {
            sess.has_unread_output = false;
        }
        true
    }

    pub fn close(&mut self, idx: usize) -> bool {
        if idx >= self.sessions.len() {
            return false;
        }
        self.sessions.remove(idx);
        // Update or collapse split pair indices safely
        if let Some((l, r)) = self.split_pair {
            if idx == l || idx == r {
                self.split_pair = None;
            } else {
                let nl = if idx < l { l - 1 } else { l };
                let nr = if idx < r { r - 1 } else { r };
                self.split_pair = Some((nl, nr));
            }
        }
        if self.sessions.is_empty() {
            self.active = 0;
        } else if self.active >= self.sessions.len() {
            self.active = self.sessions.len() - 1;
        } else if idx < self.active {
            self.active -= 1;
        }
        true
    }

    pub fn close_active(&mut self) -> bool {
        if self.sessions.is_empty() {
            return false;
        }
        self.close(self.active)
    }

    pub fn resize_all(&mut self, cols: usize, rows: usize) {
        self.cols = cols.max(2);
        self.rows = rows.max(2);
        for session in &mut self.sessions {
            session.resize(self.cols, self.rows);
        }
    }

    pub fn update_colors(&mut self, fg: Rgb, bg: Rgb) {
        self.default_fg = fg;
        self.default_bg = bg;
        for session in &mut self.sessions {
            session.grid.set_default_colors(fg, bg);
        }
    }

    /// Builds a serializable snapshot of all open sessions and their transcripts.
    pub fn to_snapshot(&self) -> WorkspaceSnapshot {
        WorkspaceSnapshot {
            active: self.active,
            split_pair: self.split_pair,
            sessions: self
                .sessions
                .iter()
                .map(|s| SavedSession {
                    id: s.id,
                    title: s.title.clone(),
                    shell_program: s.shell.program.clone(),
                    cwd: s.cwd.clone(),
                    transcript: s.grid.dump_transcript(),
                })
                .collect(),
        }
    }

    /// Restores all sessions (titles, shells, cwds, transcripts, active & split state) from a snapshot.
    pub fn restore_snapshot(
        &mut self,
        snapshot: &WorkspaceSnapshot,
        available_shells: &[ShellProfile],
        recent_history: &[String],
    ) -> usize {
        let mut restored = 0;
        let fallback = default_shell(available_shells);
        for saved in &snapshot.sessions {
            let shell = available_shells
                .iter()
                .find(|s| s.program.eq_ignore_ascii_case(&saved.shell_program))
                .cloned()
                .unwrap_or_else(|| fallback.clone());
            let id = saved.id.max(self.next_id);
            if let Ok(mut session) = Session::spawn_with_state(
                id,
                shell,
                self.cols,
                self.rows,
                self.default_fg,
                self.default_bg,
                self.wakeup.clone(),
                Some(saved.cwd.clone()),
                Some(saved.title.clone()),
                Some(&saved.transcript),
            ) {
                session.input.seed_history(recent_history.to_vec());
                self.next_id = id + 1;
                self.sessions.push(session);
                restored += 1;
            }
        }
        if restored > 0 {
            self.active = snapshot.active.min(self.sessions.len() - 1);
            if let Some((l, r)) = snapshot.split_pair {
                if l < self.sessions.len() && r < self.sessions.len() && l != r {
                    self.split_pair = Some((l, r));
                }
            }
        }
        restored
    }

    /// Polls all sessions for new PTY output. Returns `true` if any session updated.
    pub fn poll_all(&mut self) -> bool {
        let active = self.active;
        let mut any = false;
        for (idx, session) in self.sessions.iter_mut().enumerate() {
            if session.poll_output() {
                if idx != active {
                    session.has_unread_output = true;
                }
                any = true;
            }
        }
        any
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::{default_shell, discover_shells};

    #[test]
    fn spawns_switches_and_closes_sessions() {
        let shells = discover_shells();
        let shell = default_shell(&shells);
        let mut mgr = SessionManager::new(
            80,
            24,
            Rgb::new(220, 220, 220),
            Rgb::new(17, 19, 26),
            None,
        );

        let idx0 = mgr.spawn_session(shell.clone()).expect("spawn session 1");
        assert_eq!(idx0, 0);
        assert_eq!(mgr.sessions().len(), 1);
        assert_eq!(mgr.active_index(), 0);

        let idx1 = mgr.spawn_session(shell).expect("spawn session 2");
        assert_eq!(idx1, 1);
        assert_eq!(mgr.sessions().len(), 2);
        assert_eq!(mgr.active_index(), 1);

        assert!(mgr.select(0));
        assert_eq!(mgr.active_index(), 0);

        assert!(mgr.close(0));
        assert_eq!(mgr.sessions().len(), 1);
        assert_eq!(mgr.active_index(), 0);

        assert!(mgr.close_active());
        assert!(mgr.is_empty());
    }

    #[test]
    fn detects_interactive_agents_tuis_and_bare_repls() {
        assert!(is_interactive_command("agy"));
        assert!(is_interactive_command("claude"));
        assert!(is_interactive_command("nvim src/main.rs"));
        assert!(is_interactive_command("python"));
        assert!(is_interactive_command("docker exec -it api sh"));

        assert!(!is_interactive_command("agy --version"));
        assert!(!is_interactive_command("python script.py"));
        assert!(!is_interactive_command("git status -sb"));
    }
}
