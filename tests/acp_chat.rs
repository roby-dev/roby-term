use my_warp::acp::{Chat, ConnectionState, MAX_PROMPT_BYTES, MAX_RESPONSE_BYTES};
use std::{
    fs, io::{self, Write}, path::PathBuf, process::{self, Command, Stdio}, thread,
    time::{Duration, Instant},
};

// Native mock: the transport must not require a shell or a real agent CLI.
const MOCK: &str = r###"
use std::{io::{self, BufRead}, path::Path, thread, time::{Duration, Instant}};
fn receive(input: &mut impl BufRead, parts: &[&str]) {
    let mut line = String::new();
    input.read_line(&mut line).unwrap();
    assert!(parts.iter().all(|part| line.contains(part)), "unexpected frame: {line}");
}
fn frame(body: &str) { println!(r#"{{"jsonrpc":"2.0",{body}}}"#); }
fn chunk(session: &str, kind: &str, text: &str) {
    frame(&format!(r#""method":"session/update","params":{{"sessionId":"{session}","update":{{"sessionUpdate":"{kind}","content":{{"type":"text","text":"{text}"}}}}}}"#));
}
fn main() {
    let mode = std::env::args().nth(1).unwrap();
    let mut input = io::stdin().lock();
    receive(&mut input, &[r#""id":1"#, r#""method":"initialize"#, r#""clientCapabilities":{}"#]);
    if mode == "eof" { return; }
    frame(r#""id":1,"result":{"protocolVersion":1}"#);
    receive(&mut input, &[r#""id":2"#, r#""method":"session/new"#, r#""cwd":"#, r#""mcpServers":[]"#]);
    if mode == "auth" {
        frame(r#""id":2,"error":{"code":-32000,"message":"Authentication required"}"#);
        return;
    }
    frame(&format!(r#""id":2,"result":{{"sessionId":"{mode}"}}"#));
    receive(&mut input, &[r#""id":3"#, r#""method":"session/prompt"#, &format!(r#""sessionId":"{mode}""#), r#""prompt":[{"text":"question","type":"text"}]"#]);
    frame(r#""id":99,"error":{"code":-32000,"message":"wrong request"}"#);
    chunk("foreign", "agent_message_chunk", "ignore this");
    chunk(&mode, "agent_thought_chunk", "ignore this too");
    // Even a method with the same numeric ID as the prompt is not its response.
    frame(r#""id":3,"method":"session/request_permission","params":{}"#);
    receive(&mut input, &[r#""id":3"#, r#""result":{"outcome":{"outcome":"cancelled"}}"#]);
    frame(r#""id":"tool","method":"fs/read_text_file","params":{}"#);
    receive(&mut input, &[r#""id":"tool"#, r#""error":{"code":-32601,"message":"Method not found"}"#]);
    chunk(&mode, "agent_message_chunk", "**hello**");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !Path::new(&mode).exists() && Instant::now() < deadline { thread::sleep(Duration::from_millis(5)); }
    assert!(Path::new(&mode).exists(), "test never observed streaming");
    chunk(&mode, "agent_message_chunk", &format!("{}💜", "x".repeat(LIMIT - 10)));
    chunk(&mode, "agent_message_chunk", "later");
    frame(r#""id":3,"result":{"stopReason":"end_turn"}"#);
    receive(&mut input, &[r#""id":4"#, r#""method":"session/prompt"#]);
    chunk(&mode, "agent_message_chunk", "second");
    frame(r#""id":4,"result":{"stopReason":"end_turn"}"#);
    let _ = input.read_line(&mut String::new());
}
"###;

struct Mock(PathBuf);
impl Mock {
    fn build() -> Self {
        let temp = std::env::temp_dir();
        let dir = (0..).find_map(|n| {
            let path = temp.join(format!("velt-chat-{}-{n}", process::id()));
            match fs::create_dir(&path) {
                Ok(()) => Some(Self(path)),
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => None,
                Err(err) => panic!("create ACP mock directory: {err}"),
            }
        }).unwrap();
        let mut rustc = Command::new("rustc")
            .current_dir(&dir.0).args(["--crate-name", "acp_chat_mock", "-", "-o"])
            .arg(dir.binary()).stdin(Stdio::piped()).spawn().unwrap();
        write!(rustc.stdin.take().unwrap(), "const LIMIT: usize = {MAX_RESPONSE_BYTES};\n{MOCK}").unwrap();
        assert!(rustc.wait().unwrap().success());
        dir
    }

    fn binary(&self) -> PathBuf {
        self.0.join(format!("mock{}", std::env::consts::EXE_SUFFIX))
    }

    fn start(&self, chat: &mut Chat, mode: &str) {
        chat.start_command(self.binary().to_str().unwrap(), &[mode], &self.0, || {}).unwrap();
        assert_eq!(chat.connection(), ConnectionState::Connecting);
    }
}

impl Drop for Mock {
    fn drop(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match fs::remove_dir_all(&self.0) {
                Ok(()) => break,
                Err(err) if cfg!(windows) && Instant::now() < deadline &&
                    matches!(err.kind(), io::ErrorKind::PermissionDenied | io::ErrorKind::DirectoryNotEmpty) =>
                    thread::sleep(Duration::from_millis(10)),
                Err(err) if thread::panicking() => { eprintln!("remove {}: {err}", self.0.display()); break; }
                Err(err) => panic!("remove {}: {err}", self.0.display()),
            }
        }
    }
}

fn until(chat: &mut Chat, ready: impl Fn(&Chat) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready(chat) {
        assert!(Instant::now() < deadline, "timed out: {:?}", chat.connection());
        if !chat.poll() { thread::sleep(Duration::from_millis(5)); }
    }
}

#[test]
fn streams_only_the_active_turn_and_keeps_sessions_independent() {
    let mock = Mock::build();
    let (mut left, mut right) = (Chat::default(), Chat::default());
    assert_eq!(left.connection(), ConnectionState::Disconnected);
    assert_eq!(left.prompt("question").unwrap_err().kind(), io::ErrorKind::NotConnected);
    assert!(!left.poll());
    mock.start(&mut left, "left");
    mock.start(&mut right, "right");
    for chat in [&mut left, &mut right] {
        until(chat, |c| c.connection() == ConnectionState::Active);
        for invalid in ["  ", &"q".repeat(MAX_PROMPT_BYTES + 1)] {
            assert_eq!(chat.prompt(invalid).unwrap_err().kind(), io::ErrorKind::InvalidInput);
            assert!(!chat.is_prompt_pending());
            assert_eq!(chat.response(), "");
        }
        chat.prompt("question").unwrap();
        assert_eq!(chat.prompt("another").unwrap_err().kind(), io::ErrorKind::WouldBlock);
        assert!(chat.is_prompt_pending());
    }
    until(&mut left, |c| !c.response().is_empty());
    assert_eq!(left.response(), "**hello**");
    assert!(left.is_prompt_pending());
    assert_eq!(right.response(), "");
    fs::write(mock.0.join("left"), []).unwrap();
    until(&mut left, |c| !c.is_prompt_pending());
    assert_eq!(left.response(), format!("**hello**{}", "x".repeat(MAX_RESPONSE_BYTES - 10)));
    assert!(left.response_truncated());
    left.prompt("next").unwrap();
    assert_eq!(left.response(), "");
    assert!(!left.response_truncated());
    until(&mut left, |c| !c.is_prompt_pending());
    assert_eq!(left.response(), "second");
    left.stop();
    assert_eq!(left.connection(), ConnectionState::Disconnected);
    assert_eq!(left.response(), "");
    assert!(!left.poll());
    until(&mut right, |c| !c.response().is_empty());
    assert_eq!(right.response(), "**hello**");
    assert!(right.is_prompt_pending());
}

#[test]
fn reports_launch_authentication_and_connection_errors() {
    let mock = Mock::build();
    let mut chat = Chat::default();
    chat.start_command(mock.0.join("missing-agent").to_str().unwrap(), &[], &mock.0, || {}).unwrap();
    until(&mut chat, |c| matches!(c.connection(), ConnectionState::Error(_)));
    let ConnectionState::Error(message) = chat.connection() else { unreachable!() };
    assert!(message.starts_with("ACP executable not found: ") && message.ends_with("missing-agent"));
    for (mode, message) in [
        ("auth", "ACP authentication required. Sign in with the agent CLI and try again."),
        ("eof", "ACP agent disconnected"),
    ] {
        mock.start(&mut chat, mode);
        until(&mut chat, |c| matches!(c.connection(), ConnectionState::Error(_)));
        assert_eq!(chat.connection(), ConnectionState::Error(message));
        assert!(!chat.is_prompt_pending());
        assert!(!chat.poll());
    }
    mock.start(&mut chat, "stopped-before-poll");
    chat.stop();
    assert_eq!(chat.connection(), ConnectionState::Disconnected);
    assert!(!chat.poll());
}
