use my_warp::acp::{Client, Event};
use serde_json::{json, Value};
use std::{
    fs,
    io::{self, Read},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{self, Command},
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc::{self, RecvTimeoutError},
    },
    thread,
    time::{Duration, Instant},
};

const TIMEOUT: Duration = Duration::from_secs(5);

struct Mock(PathBuf);

impl Mock {
    fn build() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let mock = loop {
            let path = std::env::temp_dir().join(format!(
                "velt-acp-mock-{}-{}",
                process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed),
            ));
            match fs::create_dir(&path) {
                Ok(()) => break Self(path),
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(err) => panic!("create mock directory {}: {err}", path.display()),
            }
        };
        // Use the Rust toolchain running the tests, not an agent CLI or shell.
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/acp_mock.rs");
        let output = Command::new("rustc")
            .current_dir(&mock.0)
            .args(["--edition=2021", "-o"])
            .arg(mock.binary())
            .arg(source)
            .output()
            .expect("run rustc for the ACP mock");
        assert!(
            output.status.success(),
            "mock compilation failed ({}):\n{}{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        mock
    }

    fn binary(&self) -> PathBuf {
        self.0.join(format!("acp_mock{}", std::env::consts::EXE_SUFFIX))
    }

    fn spawn(&self, scenario: &str) -> Client {
        Client::spawn(self.binary().to_str().unwrap(), &[scenario], &self.0, || {})
            .expect("spawn the mock ACP agent")
    }
}

impl Drop for Mock {
    fn drop(&mut self) {
        // Windows may still have the executable mapped while the reaper runs.
        let deadline = Instant::now() + TIMEOUT;
        loop {
            match fs::remove_dir_all(&self.0) {
                Ok(()) => break,
                Err(err) if err.kind() == io::ErrorKind::NotFound => break,
                Err(err)
                    if cfg!(windows)
                        && Instant::now() < deadline
                        && matches!(
                            err.kind(),
                            io::ErrorKind::PermissionDenied | io::ErrorKind::DirectoryNotEmpty
                        ) =>
                {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(err) => {
                    let message = format!("remove mock directory {}: {err}", self.0.display());
                    if thread::panicking() {
                        eprintln!("{message}");
                        break;
                    }
                    panic!("{message}");
                }
            }
        }
    }
}

fn receive(client: &Client) -> Event {
    client.recv_timeout(TIMEOUT).expect("receive an ACP event")
}

fn expect_message(client: &Client, expected: Value) {
    match receive(client) {
        Event::Message(message) => assert_eq!(message, expected),
        other => panic!("expected ACP message {expected}, got {other:?}"),
    }
}

fn request(id: u64, method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

fn expect_result(client: &Client, id: u64, mut result: Value, observed_request: Value) {
    result["_meta"] = json!({"mockRequest": observed_request});
    expect_message(client, json!({"jsonrpc": "2.0", "id": id, "result": result}));
}

fn initialize(client: &mut Client) {
    let id = client.initialize().expect("enqueue initialize");
    expect_result(
        client,
        id,
        json!({"protocolVersion": 1}),
        request(
            id,
            "initialize",
            json!({
                "protocolVersion": 1,
                "clientCapabilities": {},
                "clientInfo": {"name": "velt", "version": env!("CARGO_PKG_VERSION")},
            }),
        ),
    );
}

fn session_params(cwd: &Path) -> Value {
    json!({"cwd": cwd, "mcpServers": []})
}

fn expect_chunk(client: &Client, text: &str) {
    expect_message(
        client,
        json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "mock-session",
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": {"type": "text", "text": text},
                },
            },
        }),
    );
}

fn expect_closed(client: &Client) {
    let event = receive(client);
    assert!(matches!(event, Event::Closed), "expected EOF, got {event:?}");
}

#[test]
fn initializes_and_streams_interleaved_updates() {
    let mock = Mock::build();
    let mut client = mock.spawn("lifecycle");
    initialize(&mut client);

    let id = client.new_session(&mock.0).expect("enqueue session/new");
    expect_result(
        &client,
        id,
        json!({"sessionId": "mock-session"}),
        request(id, "session/new", session_params(&mock.0)),
    );

    let text = "Explain \"velocidad\"\nlínea 2";
    let prompt_id = client.prompt("mock-session", text).expect("enqueue prompt");
    expect_chunk(&client, "Hello, ");

    let resume_id = client.request("mock/resume", json!({})).expect("release mock");
    expect_result(
        &client,
        resume_id,
        json!({"resumed": true}),
        request(resume_id, "mock/resume", json!({})),
    );
    expect_chunk(&client, "world!\n");
    expect_result(
        &client,
        prompt_id,
        json!({"stopReason": "end_turn"}),
        request(
            prompt_id,
            "session/prompt",
            json!({"sessionId": "mock-session", "prompt": [{"type": "text", "text": text}]}),
        ),
    );
}

fn drop_with_deadline(client: Client) {
    let (done, returned) = mpsc::channel();
    std::thread::spawn(move || {
        drop(client);
        let _ = done.send(());
    });
    returned.recv_timeout(TIMEOUT).expect("dropping the ACP client must return within the deadline");
}

#[test]
fn drop_disconnects_a_full_event_queue() {
    let mock = Mock::build();
    let (wake, notifications) = mpsc::channel();
    let mut client = Client::spawn(mock.binary().to_str().unwrap(), &["flood"], &mock.0, move || {
        let _ = wake.send(());
    })
    .expect("spawn the mock ACP agent");
    client.initialize().expect("start mock");

    // The callback runs once per enqueue. Never drain the event receiver.
    for _ in 0..64 {
        notifications.recv_timeout(TIMEOUT).expect("fill event queue");
    }
    drop_with_deadline(client);
    assert_eq!(
        notifications.recv_timeout(TIMEOUT),
        Err(RecvTimeoutError::Disconnected),
    );
}

#[test]
fn drop_terminates_the_owned_descendant() {
    let mock = Mock::build();
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock readiness socket");
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let mut client = Client::spawn(
        mock.binary().to_str().unwrap(),
        &["descendant", &address],
        &mock.0,
        || {},
    )
    .expect("spawn the mock ACP agent with a descendant");
    client.initialize().expect("start mock");

    let mut descendant = accept_mock(&listener);
    let mut ready = [0];
    descendant
        .read_exact(&mut ready)
        .expect("descendant is ready");
    assert_eq!(ready, [b'!']);

    drop_with_deadline(client);
    // No other process has the peer socket. It closes on termination; on test
    // failure the mock also exits when this end is dropped, with no PID lookup.
    match descendant.read(&mut ready) {
        Ok(0) => {}
        // Windows can reset the socket when its owner is terminated.
        Err(err) if err.kind() == io::ErrorKind::ConnectionReset => {}
        other => panic!("descendant did not exit after client drop: {other:?}"),
    }
}

fn accept_mock(listener: &TcpListener) -> TcpStream {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false).unwrap();
                stream.set_read_timeout(Some(TIMEOUT)).unwrap();
                return stream;
            }
            Err(err) if err.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(err) => panic!("accept ready mock connection: {err}"),
        }
    }
}

#[test]
fn delivers_authentication_error_with_request_id_before_eof() {
    let mock = Mock::build();
    let mut client = mock.spawn("auth-required");
    initialize(&mut client);

    let id = client.new_session(&mock.0).expect("enqueue session/new");
    expect_message(
        &client,
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": -32000,
                "message": "Authentication required",
                "data": {"mockRequest": request(id, "session/new", session_params(&mock.0))},
            },
        }),
    );
    expect_closed(&client);
}

#[test]
fn rejects_malformed_wrong_version_and_unterminated_frames() {
    let mock = Mock::build();
    for (scenario, expected) in [
        ("malformed", "Invalid JSON-RPC 2.0 message"),
        ("wrong-version", "Invalid JSON-RPC 2.0 message"),
        ("unterminated", "Invalid ACP frame length or framing"),
    ] {
        let mut client = mock.spawn(scenario);
        client.initialize().expect("enqueue initialize");
        match receive(&client) {
            Event::Error(message) => assert_eq!(message, expected, "scenario: {scenario}"),
            other => panic!("{scenario}: expected transport error, got {other:?}"),
        }
    }
}

#[test]
fn reports_eof_when_agent_exits_before_initialize_response() {
    let mock = Mock::build();
    let mut client = mock.spawn("early-exit");
    client.initialize().expect("enqueue initialize");
    expect_closed(&client);
}
