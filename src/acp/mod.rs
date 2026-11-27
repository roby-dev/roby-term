//! ACP subprocess transport. Never route protocol frames through a shell or PTY.
//!
//! Dropping a client disconnects its queues and signals its reaper to stop the agent.
//! Windows launches native executables in a job and fails launch if job membership
//! cannot be established. On Unix, shutdown covers only descendants still in the
//! agent's process group. Crash cleanup is not part of the transport contract.
#[cfg_attr(unix, path = "process_unix.rs")]
#[cfg_attr(windows, path = "process_windows.rs")]
mod process;
mod chat;
mod collab;
pub mod markdown;

pub use chat::{Chat, ConnectionState, PendingPermission, MAX_PROMPT_BYTES, MAX_RESPONSE_BYTES};
pub use collab::{parse_mention, MAX_HANDOFF_BYTES};
pub use markdown::{parse_markdown, MdLine, MdLineKind};

pub const DEFAULT_CONTEXT_LINES: usize = 40;

use crate::input::InputBar;
use serde_json::{json, Value};
use std::{
    io::{self, BufRead, BufReader, Read, Write},
    path::Path,
    sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender},
    thread,
    time::Duration,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Agent {
    #[default]
    Antigravity,
    Codex,
    Claude,
}

impl Agent {
    pub const ALL: [Self; 3] = [Self::Antigravity, Self::Codex, Self::Claude];

    pub fn label(self) -> &'static str {
        match self {
            Self::Antigravity => "Antigravity",
            Self::Codex => "Codex",
            Self::Claude => "Claude Code",
        }
    }

    pub fn command(self) -> (&'static str, &'static [&'static str]) {
        match self {
            Self::Antigravity => ("agy", &["acp"]),
            Self::Codex => ("codex-acp", &[]),
            Self::Claude => ("claude-agent-acp", &[]),
        }
    }

    pub fn resolve_command(self) -> (String, Vec<String>) {
        #[cfg(windows)]
        if let Some(path) = std::env::var_os("PATH") {
            let dirs: Vec<_> = std::env::split_paths(&path).collect();
            if let Some(resolved) = resolve_adapter_in_paths(self, &dirs) {
                return resolved;
            }
        }
        let (program, args) = self.command();
        (program.to_owned(), args.iter().map(|s| (*s).to_owned()).collect())
    }

    /// npm packages that ship this agent's adapter, in preference order.
    #[cfg(any(windows, test))]
    fn npm_packages(self) -> &'static [[&'static str; 2]] {
        match self {
            Self::Antigravity => &[],
            Self::Codex => &[
                ["@agentclientprotocol", "codex-acp"],
                ["@zed-industries", "codex-acp"],
            ],
            Self::Claude => &[["@agentclientprotocol", "claude-agent-acp"]],
        }
    }
}

/// Windows cannot launch npm command shims without a shell, so adapters resolve
/// to a native executable or to `node.exe` plus the package entry script.
#[cfg(any(windows, test))]
fn resolve_adapter_in_paths(agent: Agent, dirs: &[std::path::PathBuf]) -> Option<(String, Vec<String>)> {
    let packages = agent.npm_packages();
    if packages.is_empty() {
        return None;
    }
    let exe_name = format!("{}.exe", agent.command().0);
    for dir in dirs {
        let exe = dir.join(&exe_name);
        if exe.is_file() {
            return Some((exe.to_string_lossy().into_owned(), Vec::new()));
        }
    }
    for dir in dirs {
        for [scope, name] in packages {
            let script = dir.join("node_modules").join(scope).join(name).join("dist").join("index.js");
            if script.is_file() {
                let node = std::iter::once(dir)
                    .chain(dirs.iter())
                    .map(|d| d.join("node.exe"))
                    .find(|p| p.is_file())
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "node.exe".to_owned());
                return Some((node, vec![script.to_string_lossy().into_owned()]));
            }
        }
    }
    None
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpTurn {
    /// Position in the shared conversation, increasing across all agents.
    pub seq: u64,
    pub agent: Agent,
    pub prompt: String,
    pub response: String,
    pub truncated: bool,
}

const MAX_ACP_TURNS: usize = 50;

/// A connected agent that is not currently addressed.
struct AgentSlot {
    agent: Agent,
    chat: Chat,
    synced: Option<u64>,
}

/// Per-session ACP panel state. Creating it starts no threads or subprocesses.
/// `agent` and `chat` are the addressed agent; the others wait in `parked`.
#[derive(Default)]
pub struct AcpSessionState {
    pub open: bool,
    pub agent: Agent,
    pub include_context: bool,
    pub auto_approve: bool,
    pub chat: Chat,
    pub input: InputBar,
    pub turns: Vec<AcpTurn>,
    pub pending_prompt: Option<String>,
    pub scroll: usize,
    parked: Vec<AgentSlot>,
    /// Last turn delivered to the addressed agent's live session.
    synced: Option<u64>,
    /// Prompt waiting for the addressed agent to finish connecting.
    queued: Option<String>,
    last_seq: u64,
}

pub fn format_prompt_with_context(prompt: &str, cwd: &Path, clean_lines: &[String]) -> String {
    let header = format!("[Terminal context]\ncwd: {}\n", cwd.display());
    let footer = format!("\n{prompt}");
    if header.len() + footer.len() > MAX_PROMPT_BYTES {
        return prompt.to_owned();
    }
    if !clean_lines.is_empty() {
        for start in 0..clean_lines.len() {
            let body = clean_lines[start..].join("\n");
            let candidate = format!("{header}last_lines:\n```\n{body}\n```{footer}");
            if candidate.len() <= MAX_PROMPT_BYTES {
                return candidate;
            }
        }
    }
    format!("{header}{footer}")
}

impl AcpSessionState {
    pub fn status_label(&self) -> &'static str {
        match self.chat.connection() {
            ConnectionState::Disconnected => "Desconectado",
            ConnectionState::Connecting => "Conectando",
            ConnectionState::Active => "Activo",
            ConnectionState::Error(_) => "Error",
        }
    }

    pub fn error_message(&self) -> Option<&str> {
        match self.chat.connection() {
            ConnectionState::Error(msg) => Some(msg),
            _ => None,
        }
    }

    /// Advances ACP events without doing work when the panel is closed and disconnected.
    /// Returns `(changed, has_more)`.
    pub fn poll(&mut self) -> (bool, bool) {
        if !self.open && matches!(self.chat.connection(), ConnectionState::Disconnected) {
            return (false, false);
        }
        let prev_status = self.status_label();
        let prev_err = self.error_message().map(str::to_owned);
        let prev_resp_len = self.chat.response().len();
        let was_pending = self.chat.is_prompt_pending();
        let prev_perm_count = self.chat.pending_permission_count();
        let prev_perm_title = self.chat.pending_permission().map(|p| p.title.clone());

        let mut has_more = self.chat.poll();
        for slot in &mut self.parked {
            has_more |= slot.chat.poll();
        }
        let queue_changed = self.flush_queued();
        let auto_approved = self.drain_auto_permissions();

        if was_pending && !self.chat.is_prompt_pending() {
            if let Some(prompt) = self.pending_prompt.take() {
                let response = self.chat.response().to_owned();
                if !response.is_empty() || matches!(self.chat.connection(), ConnectionState::Active) {
                    if self.turns.len() >= MAX_ACP_TURNS {
                        self.turns.remove(0);
                    }
                    self.last_seq += 1;
                    self.turns.push(AcpTurn {
                        seq: self.last_seq,
                        agent: self.agent,
                        prompt,
                        response,
                        truncated: self.chat.response_truncated(),
                    });
                    self.scroll = 0;
                }
            }
        }

        let changed = prev_status != self.status_label()
            || prev_err.as_deref() != self.error_message()
            || prev_resp_len != self.chat.response().len()
            || was_pending != self.chat.is_prompt_pending()
            || auto_approved
            || queue_changed
            || prev_perm_count != self.chat.pending_permission_count()
            || prev_perm_title.as_deref() != self.chat.pending_permission().map(|p| p.title.as_str());
        (changed, has_more)
    }

    /// Allows queued requests that need no decision: everything allowable when
    /// auto-approve is on, otherwise only Antigravity's Engram reads and saves.
    pub fn drain_auto_permissions(&mut self) -> bool {
        let memory_only = self.agent == Agent::Antigravity;
        let mut approved_any = false;
        while self.chat.pending_permission().is_some_and(|p| {
            p.allow_option_id.is_some() && (self.auto_approve || (memory_only && p.memory_access))
        }) {
            match self.chat.approve_permission() {
                Ok(true) => approved_any = true,
                _ => break,
            }
        }
        approved_any
    }

    pub fn submit_prompt(&mut self) -> Result<(), String> {
        let text = self.input.text().trim().to_owned();
        if text.is_empty() {
            return Ok(());
        }
        self.dispatch(text, None, false)
    }

    pub fn submit_prompt_with_context(&mut self, cwd: &Path, clean_lines: &[String]) -> Result<(), String> {
        let raw = self.input.text().trim().to_owned();
        if raw.is_empty() {
            return Ok(());
        }
        self.dispatch(raw, Some((cwd, clean_lines)), false)
    }

    /// Submits the input, honoring a leading `@agent` mention. `launch` starts the
    /// mentioned agent when it is not connected; the prompt is sent once it is active.
    pub fn submit_routed(
        &mut self,
        context: Option<(&Path, &[String])>,
        launch: impl FnOnce(&mut Chat, Agent) -> io::Result<()>,
    ) -> Result<(), String> {
        let text = self.input.text().trim().to_owned();
        if text.is_empty() {
            return Ok(());
        }
        let Some((agent, body)) = parse_mention(&text) else {
            return self.dispatch(text, context, false);
        };
        if self.is_busy() {
            return Err(format!("{} is still working", self.agent.label()));
        }
        if body.len() > MAX_PROMPT_BYTES {
            return Err(format!("ACP prompt must be at most {MAX_PROMPT_BYTES} bytes"));
        }
        self.switch_agent(agent);
        self.ensure_started(launch).map_err(|err| format!("ACP launch failed: {err}"))?;
        if body.is_empty() {
            self.input.clear();
            return Ok(());
        }
        self.dispatch(body.to_owned(), context, true)
    }

    pub fn is_busy(&self) -> bool {
        self.pending_prompt.is_some() || self.chat.is_prompt_pending()
    }

    /// Addresses another agent, keeping idle connections alive. Switching away from
    /// a running turn stops that agent; a prompt not yet sent returns to the input.
    pub fn switch_agent(&mut self, agent: Agent) {
        if agent == self.agent {
            return;
        }
        if self.is_busy() {
            self.chat.stop();
            self.synced = None;
            if self.queued.take().is_some() {
                self.restore_pending_prompt();
            }
            self.pending_prompt = None;
        }
        let next = match self.parked.iter().position(|slot| slot.agent == agent) {
            Some(idx) => self.parked.swap_remove(idx),
            None => AgentSlot { agent, chat: Chat::default(), synced: None },
        };
        let previous = AgentSlot {
            agent: std::mem::replace(&mut self.agent, next.agent),
            chat: std::mem::replace(&mut self.chat, next.chat),
            synced: std::mem::replace(&mut self.synced, next.synced),
        };
        self.parked.push(previous);
    }

    /// Starts the addressed agent unless it is already connecting or active.
    pub fn ensure_started(
        &mut self,
        launch: impl FnOnce(&mut Chat, Agent) -> io::Result<()>,
    ) -> io::Result<()> {
        if matches!(
            self.chat.connection(),
            ConnectionState::Disconnected | ConnectionState::Error(_)
        ) {
            self.synced = None;
            launch(&mut self.chat, self.agent)?;
        }
        Ok(())
    }

    fn dispatch(
        &mut self,
        raw: String,
        context: Option<(&Path, &[String])>,
        may_queue: bool,
    ) -> Result<(), String> {
        let payload = match context {
            Some((cwd, lines)) if self.include_context && raw.len() <= MAX_PROMPT_BYTES => {
                format_prompt_with_context(&raw, cwd, lines)
            }
            _ => raw.clone(),
        };
        if may_queue && self.chat.connection() == ConnectionState::Connecting {
            self.queued = Some(payload);
        } else {
            self.send(&payload)?;
        }
        self.pending_prompt = Some(raw);
        self.input.clear();
        self.scroll = 0;
        Ok(())
    }

    fn send(&mut self, payload: &str) -> Result<(), String> {
        let mut history = collab::format_handoff(&self.turns, self.agent, self.synced);
        if self.synced.is_none() {
            history.insert_str(0, collab::MEMORY_NOTE);
        }
        self.chat
            .prompt_with_history(&history, payload)
            .map_err(|err| err.to_string())?;
        self.synced = Some(self.last_seq);
        Ok(())
    }

    /// Sends the queued prompt once the agent is active, or returns it to the
    /// input when the connection fails.
    fn flush_queued(&mut self) -> bool {
        let ready = match self.chat.connection() {
            ConnectionState::Connecting => return false,
            ConnectionState::Active => true,
            ConnectionState::Disconnected | ConnectionState::Error(_) => false,
        };
        let Some(payload) = self.queued.take() else {
            return false;
        };
        if !ready || self.send(&payload).is_err() {
            self.restore_pending_prompt();
        }
        true
    }

    fn restore_pending_prompt(&mut self) {
        if let Some(raw) = self.pending_prompt.take() {
            if self.input.text().is_empty() {
                self.input.set_text(&raw);
            }
        }
    }

    pub fn approve_permission(&mut self) -> Result<(), String> {
        self.chat.approve_permission().map(|_| ()).map_err(|err| err.to_string())
    }

    pub fn reject_permission(&mut self) -> Result<(), String> {
        self.chat.reject_permission().map(|_| ()).map_err(|err| err.to_string())
    }

    pub fn deactivate(&mut self) {
        self.open = false;
        self.pending_prompt = None;
        self.queued = None;
        self.synced = None;
        self.parked.clear();
        self.chat.stop();
    }
}

#[derive(Debug)]
pub enum Event {
    Message(Value),
    Closed,
    Error(String),
}

pub struct Client {
    // Drop the private event receiver first: it releases backpressured senders.
    events: Receiver<Event>,
    _shutdown: Sender<()>,
    tx: SyncSender<Value>,
    next_id: u64,
}

impl Client {
    /// `wake` should post a non-blocking notification for the event consumer.
    pub fn spawn(
        program: &str,
        args: &[&str],
        cwd: &Path,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> io::Result<Self> {
        let (shutdown, cancelled) = mpsc::channel();
        let (started, pipes) = mpsc::sync_channel(1);
        let program = program.to_owned();
        let args: Vec<_> = args.iter().map(|arg| (*arg).to_owned()).collect();
        let cwd = cwd.to_owned();
        // Start the reaper before launching the agent so that even failed
        // client setup won't leave a process with nobody to wait for it.
        thread::Builder::new()
            .name("acp-reaper".into())
            .spawn(move || match process::spawn(&program, &args, &cwd) {
                Ok((stdin, stdout, stderr, stop)) => {
                    if started.send(Ok((stdin, stdout, stderr))).is_ok() {
                        let _ = cancelled.recv();
                    }
                    stop();
                }
                Err(err) => {
                    let _ = started.send(Err(err));
                }
            })?;
        let (mut stdin, stdout, mut stderr) = pipes
            .recv()
            .map_err(|_| io::Error::other("ACP reaper stopped during launch"))??;
        let mut stdout = BufReader::new(stdout);
        let (tx, writes) = mpsc::sync_channel::<Value>(32);
        let (emit, events) = mpsc::sync_channel(64);
        let wake = std::sync::Arc::new(wake);
        let write_error = emit.clone();
        let write_wake = wake.clone();
        thread::Builder::new().spawn(move || {
            for frame in writes {
                if let Err(err) = serde_json::to_writer(&mut stdin, &frame)
                    .map_err(io::Error::from)
                    .and_then(|_| stdin.write_all(b"\n"))
                    .and_then(|_| stdin.flush())
                {
                    if write_error.send(Event::Error(err.to_string())).is_ok() {
                        write_wake();
                    }
                    break;
                }
            }
        })?;
        thread::Builder::new().spawn(move || {
            let _ = io::copy(&mut stderr, &mut io::sink());
        })?;
        thread::Builder::new().spawn(move || {
            const MAX_FRAME: usize = 4 * 1024 * 1024;
            let mut frame = Vec::new();
            loop {
                frame.clear();
                let event = match (&mut stdout)
                    .take(MAX_FRAME as u64 + 1)
                    .read_until(b'\n', &mut frame)
                {
                    Ok(0) => Event::Closed,
                    Ok(n) if n > MAX_FRAME || frame.last() != Some(&b'\n') => {
                        Event::Error("Invalid ACP frame length or framing".into())
                    }
                    Ok(_) => match serde_json::from_slice::<Value>(&frame) {
                        Ok(v) if v.get("jsonrpc").and_then(Value::as_str) == Some("2.0") => {
                            Event::Message(v)
                        }
                        _ => Event::Error("Invalid JSON-RPC 2.0 message".into()),
                    },
                    Err(err) => Event::Error(err.to_string()),
                };
                let done = !matches!(event, Event::Message(_));
                if emit.send(event).is_err() {
                    break;
                }
                wake();
                if done {
                    break;
                }
            }
        })?;
        Ok(Self {
            events,
            _shutdown: shutdown,
            tx,
            next_id: 1,
        })
    }

    /// Receive the next event, or pass `Duration::ZERO` to poll without blocking.
    pub fn recv_timeout(&self, timeout: Duration) -> Result<Event, RecvTimeoutError> {
        self.events.recv_timeout(timeout)
    }

    fn send(&self, frame: Value) -> io::Result<()> {
        self.tx.try_send(frame).map_err(|err| match err {
            mpsc::TrySendError::Full(_) => {
                io::Error::new(io::ErrorKind::WouldBlock, "Agent input queue is full")
            }
            mpsc::TrySendError::Disconnected(_) => {
                io::Error::new(io::ErrorKind::BrokenPipe, "Agent input closed")
            }
        })
    }
    pub fn request(&mut self, method: &str, params: Value) -> io::Result<u64> {
        let id = self.next_id;
        self.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))?;
        self.next_id += 1;
        Ok(id)
    }
    fn respond(&self, id: &Value, response: Result<Value, Value>) -> io::Result<()> {
        self.send(match response {
            Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
            Err(error) => json!({"jsonrpc":"2.0","id":id,"error":error}),
        })
    }
    pub fn initialize(&mut self) -> io::Result<u64> {
        self.request("initialize", json!({"protocolVersion":1,"clientCapabilities":{},"clientInfo":{"name":"velt","version":env!("CARGO_PKG_VERSION")}}))
    }
    pub fn new_session(&mut self, cwd: &Path) -> io::Result<u64> {
        self.request("session/new", json!({"cwd":cwd,"mcpServers":[]}))
    }
    pub fn prompt(&mut self, session: &str, text: &str) -> io::Result<u64> {
        self.request(
            "session/prompt",
            json!({"sessionId":session,"prompt":[{"type":"text","text":text}]}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{resolve_adapter_in_paths, Agent};
    use std::{fs, process, time::{SystemTime, UNIX_EPOCH}};

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("velt-acp-res-{}-{}-{nanos}", process::id(), tag));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn resolves_native_codex_exe_before_npm_script() {
        let dir = temp_dir("exe");
        let script_dir = dir
            .join("node_modules")
            .join("@agentclientprotocol")
            .join("codex-acp")
            .join("dist");
        fs::create_dir_all(&script_dir).unwrap();
        fs::write(script_dir.join("index.js"), "").unwrap();
        let exe = dir.join("codex-acp.exe");
        fs::write(&exe, "").unwrap();

        let got = resolve_adapter_in_paths(Agent::Codex, &[dir.clone()]).unwrap();
        assert_eq!(got.0, exe.to_string_lossy());
        assert!(got.1.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolves_npm_codex_acp_script_with_node_exe() {
        let dir = temp_dir("npm");
        let script_dir = dir
            .join("node_modules")
            .join("@agentclientprotocol")
            .join("codex-acp")
            .join("dist");
        fs::create_dir_all(&script_dir).unwrap();
        let script = script_dir.join("index.js");
        fs::write(&script, "").unwrap();
        let node = dir.join("node.exe");
        fs::write(&node, "").unwrap();

        let got = resolve_adapter_in_paths(Agent::Codex, &[dir.clone()]).unwrap();
        assert_eq!(got.0, node.to_string_lossy());
        assert_eq!(got.1, vec![script.to_string_lossy().into_owned()]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolves_npm_claude_agent_acp_script_with_node_exe() {
        let dir = temp_dir("claude");
        let script_dir = dir
            .join("node_modules")
            .join("@agentclientprotocol")
            .join("claude-agent-acp")
            .join("dist");
        fs::create_dir_all(&script_dir).unwrap();
        let script = script_dir.join("index.js");
        fs::write(&script, "").unwrap();
        let node = dir.join("node.exe");
        fs::write(&node, "").unwrap();

        let got = resolve_adapter_in_paths(Agent::Claude, &[dir.clone()]).unwrap();
        assert_eq!(got.0, node.to_string_lossy());
        assert_eq!(got.1, vec![script.to_string_lossy().into_owned()]);
        assert!(resolve_adapter_in_paths(Agent::Codex, &[dir.clone()]).is_none());
        assert!(resolve_adapter_in_paths(Agent::Antigravity, &[dir.clone()]).is_none());
        assert_eq!(Agent::Claude.command().0, "claude-agent-acp");
        assert_eq!(Agent::ALL.len(), 3);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn returns_none_when_codex_acp_is_absent() {
        let dir = temp_dir("none");
        assert!(resolve_adapter_in_paths(Agent::Codex, &[dir.clone()]).is_none());
        let _ = fs::remove_dir_all(&dir);
    }
}
