//! ACP subprocess transport. Never route protocol frames through a shell or PTY.
//!
//! Dropping a client disconnects its queues and signals its reaper to stop the agent.
//! Windows launches native executables in a job and fails launch if job membership
//! cannot be established. On Unix, shutdown covers only descendants still in the
//! agent's process group. Crash cleanup is not part of the transport contract.
#[cfg_attr(unix, path = "process_unix.rs")]
#[cfg_attr(windows, path = "process_windows.rs")]
mod process;

use serde_json::{json, Value};
use std::{
    io::{self, BufRead, BufReader, Read, Write},
    path::Path,
    sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender},
    thread,
    time::Duration,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Agent {
    Antigravity,
    Codex,
}

impl Agent {
    pub fn command(self) -> (&'static str, &'static [&'static str]) {
        match self {
            Self::Antigravity => ("agy", &["acp"]),
            Self::Codex => ("codex-acp", &[]),
        }
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
