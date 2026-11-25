//! Optional chat for one Velt session, independent of the terminal and renderer.
use super::{Agent, Client, Event};
use serde_json::{json, Value};
use std::{
    io,
    path::{Path, PathBuf},
    sync::{mpsc, Arc},
    thread,
    time::Duration,
};

pub const MAX_PROMPT_BYTES: usize = 16 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 64 * 1024;
const MAX_ERROR_BYTES: usize = 512;
const POLL_BUDGET: usize = 32;

#[derive(Debug, PartialEq, Eq)]
pub enum ConnectionState<'a> {
    Disconnected,
    Connecting,
    Active,
    Error(&'a str),
}

#[derive(Default)]
enum Link {
    #[default]
    Off,
    Launching {
        ready: mpsc::Receiver<Result<Client, String>>,
        cwd: PathBuf,
        _pending: Arc<()>,
    },
    Open(Client, Phase),
    Failed(String),
}

enum Phase {
    Initialize(u64, PathBuf),
    SessionNew(u64),
    Active { session: String, prompt: Option<u64> },
}

#[derive(Default)]
struct Response {
    text: String,
    truncated: bool,
}

/// One connection and the latest agent reply. Creating an inactive chat starts no work.
#[derive(Default)]
pub struct Chat {
    link: Link,
    response: Response,
}

impl Chat {
    pub fn connection(&self) -> ConnectionState<'_> {
        match &self.link {
            Link::Off => ConnectionState::Disconnected,
            Link::Open(_, Phase::Active { .. }) => ConnectionState::Active,
            Link::Failed(error) => ConnectionState::Error(error),
            _ => ConnectionState::Connecting,
        }
    }

    /// Start the selected adapter. `wake` must post a non-blocking notice to call `poll`.
    pub fn start(&mut self, agent: Agent, cwd: &Path, wake: impl Fn() + Send + Sync + 'static) -> io::Result<()> {
        let (program, args) = agent.command();
        self.start_command(program, args, cwd, wake)
    }

    /// Like `start`, with a native executable and literal arguments (never a shell).
    /// The launch result is delivered via `poll`. Stop an active connection before switching agents.
    pub fn start_command(&mut self, program: &str, args: &[&str], cwd: &Path, wake: impl Fn() + Send + Sync + 'static) -> io::Result<()> {
        if matches!(self.link, Link::Launching { .. } | Link::Open(..)) {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, "ACP chat already started"));
        }
        let (send, ready) = mpsc::sync_channel(1);
        let (program, launch_cwd) = (program.to_owned(), cwd.to_owned());
        let args: Vec<_> = args.iter().map(|arg| (*arg).to_owned()).collect();
        let pending = Arc::new(());
        let launch_pending = Arc::downgrade(&pending);
        thread::Builder::new().name("acp-chat-launch".into()).spawn(move || {
            if launch_pending.strong_count() == 0 {
                return;
            }
            let wake = Arc::new(wake);
            let transport_wake = Arc::clone(&wake);
            let args: Vec<_> = args.iter().map(String::as_str).collect();
            let client = Client::spawn(&program, &args, &launch_cwd, move || transport_wake())
                .map_err(|err| match err.kind() {
                    io::ErrorKind::NotFound => format!("ACP executable not found: {}", prefix(&program, MAX_ERROR_BYTES)),
                    _ => format!("Could not start ACP agent: {err}"),
                });
            if send.send(client).is_ok() {
                wake();
            }
        })?;
        self.link = Link::Launching { ready, cwd: cwd.to_owned(), _pending: pending };
        self.response = Response::default();
        Ok(())
    }

    /// Clear the turn and disconnect without waiting for process cleanup. A launch
    /// already in flight is discarded on return; any late wake has no effect.
    pub fn stop(&mut self) {
        self.link = Link::Off;
        self.response = Response::default();
    }

    /// Enqueue one nonblank prompt while active and idle. A rejected submission
    /// never replaces the previous reply. Completion is reported through `poll`.
    pub fn prompt(&mut self, text: &str) -> io::Result<()> {
        let Link::Open(client, Phase::Active { session, prompt }) = &mut self.link else {
            return Err(io::Error::new(io::ErrorKind::NotConnected, "ACP chat is not active"));
        };
        if prompt.is_some() {
            return Err(io::Error::new(io::ErrorKind::WouldBlock, "ACP prompt is already pending"));
        }
        if text.len() > MAX_PROMPT_BYTES || text.trim().is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "ACP prompt must be nonblank and at most 16384 bytes"));
        }
        *prompt = Some(client.prompt(session, text)?);
        self.response = Response::default();
        Ok(())
    }

    pub fn is_prompt_pending(&self) -> bool {
        matches!(self.link, Link::Open(_, Phase::Active { prompt: Some(_), .. }))
    }

    /// Raw Markdown for the latest submitted turn, retained as a UTF-8 prefix.
    pub fn response(&self) -> &str {
        &self.response.text
    }

    pub fn response_truncated(&self) -> bool {
        self.response.truncated
    }

    /// Advance launch and handle up to 32 queued items without blocking. `true`
    /// means the budget was exhausted: schedule another poll before going idle.
    pub fn poll(&mut self) -> bool {
        for _ in 0..POLL_BUDGET {
            match self.poll_one() {
                Ok(true) => {}
                Ok(false) => return false,
                Err(error) => {
                    self.link = Link::Failed(prefix(&error, MAX_ERROR_BYTES).to_owned());
                    return false;
                }
            }
        }
        true
    }

    fn poll_one(&mut self) -> Result<bool, String> {
        match &mut self.link {
            Link::Launching { ready, cwd, .. } => match ready.try_recv() {
                Ok(client) => {
                    let mut client = client?;
                    let id = client.initialize().map_err(send_error)?;
                    self.link = Link::Open(client, Phase::Initialize(id, std::mem::take(cwd)));
                }
                Err(mpsc::TryRecvError::Empty) => return Ok(false),
                Err(mpsc::TryRecvError::Disconnected) => return Err("ACP launch worker stopped".into()),
            },
            Link::Open(client, phase) => match client.recv_timeout(Duration::ZERO) {
                Ok(Event::Message(frame)) => phase.handle(client, &mut self.response, &frame)?,
                Ok(Event::Error(error)) => return Err(error),
                Ok(Event::Closed) | Err(mpsc::RecvTimeoutError::Disconnected) => return Err("ACP agent disconnected".into()),
                Err(mpsc::RecvTimeoutError::Timeout) => return Ok(false),
            },
            _ => return Ok(false),
        }
        Ok(true)
    }
}

impl Phase {
    fn handle(&mut self, client: &mut Client, response: &mut Response, frame: &Value) -> Result<(), String> {
        if let Some(method) = frame.get("method").and_then(Value::as_str) {
            if let Some(id) = frame.get("id") {
                if id.is_string() || id.is_number() || id.is_null() {
                    // T2 offers no filesystem/terminal bridge, even for a matching session.
                    let reply = match method {
                        "session/request_permission" => Ok(json!({"outcome":{"outcome":"cancelled"}})),
                        _ => Err(json!({"code":-32601,"message":"Method not found"})),
                    };
                    client.respond(id, reply).map_err(send_error)?;
                }
            } else if method == "session/update" {
                let params = &frame["params"];
                let update = &params["update"];
                if let Self::Active { session, prompt: Some(_) } = self {
                    if params["sessionId"].as_str() == Some(session.as_str())
                        && update["sessionUpdate"] == "agent_message_chunk"
                        && update["content"]["type"] == "text"
                    {
                        if let Some(text) = update["content"]["text"].as_str() {
                            let fitted = prefix(text, MAX_RESPONSE_BYTES - response.text.len());
                            if !response.truncated {
                                response.text.push_str(fitted);
                                response.truncated = fitted.len() != text.len();
                            }
                        }
                    }
                }
            }
            return Ok(());
        }

        let (pending, method) = match self {
            Self::Initialize(id, _) => (*id, "initialize"),
            Self::SessionNew(id) => (*id, "session/new"),
            Self::Active { prompt: Some(id), .. } => (*id, "session/prompt"),
            Self::Active { prompt: None, .. } => return Ok(()),
        };
        if frame["id"].as_u64() != Some(pending) {
            return Ok(());
        }
        if let Some(error) = frame.get("error") {
            if error["code"].as_i64() == Some(-32000) {
                return Err("ACP authentication required. Sign in with the agent CLI and try again.".into());
            }
            let detail = error["message"].as_str().unwrap_or("unknown agent error");
            return Err(format!("ACP {method} failed: {}", prefix(detail, MAX_ERROR_BYTES)));
        }
        let result = frame.get("result").filter(|result| result.is_object())
            .ok_or_else(|| format!("Invalid ACP {method} response"))?;
        match self {
            Self::Initialize(_, cwd) => {
                if result["protocolVersion"].as_u64() != Some(1) {
                    return Err("Unsupported ACP protocol version (expected 1)".into());
                }
                *self = Self::SessionNew(client.new_session(cwd).map_err(send_error)?);
            }
            Self::SessionNew(_) => {
                let session = result["sessionId"].as_str().filter(|id| !id.is_empty())
                    .ok_or("ACP session/new did not return a session ID")?;
                *self = Self::Active { session: session.to_owned(), prompt: None };
            }
            Self::Active { prompt, .. } => *prompt = None,
        }
        Ok(())
    }
}

fn prefix(text: &str, bytes: usize) -> &str {
    &text[..text.floor_char_boundary(bytes)]
}

fn send_error(err: io::Error) -> String {
    format!("Could not send ACP message: {err}")
}
