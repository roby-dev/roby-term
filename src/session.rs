use crate::input::InputBar;
use crate::shell::ShellProfile;
use crate::terminal::TerminalGrid;
use crate::theme::Rgb;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::thread;

pub type WakeupFn = Arc<dyn Fn() + Send + Sync>;

/// A single terminal session backed by an OS PTY (ConPTY on Windows, POSIX PTY on macOS/Linux).
pub struct Session {
    pub id: usize,
    pub title: String,
    pub shell: ShellProfile,
    pub grid: TerminalGrid,
    pub input: InputBar,
    pub scroll_offset: usize,
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

        let mut cmd = CommandBuilder::new(&shell.program);
        for arg in &shell.args {
            cmd.arg(arg);
        }
        if let Ok(cwd) = std::env::current_dir() {
            cmd.cwd(cwd);
        }
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

        let title = format!("{} #{}", shell.name, id);
        let grid = TerminalGrid::new(cols, rows, default_fg, default_bg);

        Ok(Self {
            id,
            title,
            shell,
            grid,
            input: InputBar::new(),
            scroll_offset: 0,
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

    /// Submits a command line from the Warp-style input bar to the PTY.
    pub fn submit_command(&mut self, cmd: &str) {
        self.scroll_offset = 0;
        let mut line = String::with_capacity(cmd.len() + 1);
        line.push_str(cmd);
        line.push('\r');
        self.write_bytes(line.as_bytes());
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
        self.input.passthrough || self.grid.is_alt_screen()
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

/// Manages multiple concurrent terminal sessions and tracks the active selection.
pub struct SessionManager {
    sessions: Vec<Session>,
    active: usize,
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
            next_id: 1,
            cols: cols.max(2),
            rows: rows.max(2),
            default_fg,
            default_bg,
            wakeup,
        }
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
        if idx < self.sessions.len() && idx != self.active {
            self.active = idx;
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
        true
    }

    pub fn close(&mut self, idx: usize) -> bool {
        if idx >= self.sessions.len() {
            return false;
        }
        self.sessions.remove(idx);
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

    /// Polls all sessions for new PTY output. Returns `true` if any session updated.
    pub fn poll_all(&mut self) -> bool {
        let mut any = false;
        for session in &mut self.sessions {
            if session.poll_output() {
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
}
