use my_warp::acp::{format_prompt_with_context, Chat, ConnectionState, MAX_PROMPT_BYTES};
use std::{
    fs, io::{self, Write}, path::Path, path::PathBuf, process::{self, Command, Stdio}, thread,
    time::{Duration, Instant},
};

const BRIDGE_MOCK: &str = r###"
use std::io::{self, BufRead};
fn receive(input: &mut impl BufRead, parts: &[&str]) {
    let mut line = String::new();
    input.read_line(&mut line).unwrap();
    assert!(parts.iter().all(|part| line.contains(part)), "unexpected frame: {}", line);
}
fn frame(body: &str) { println!(r#"{{"jsonrpc":"2.0",{body}}}"#); }
fn main() {
    let mode = std::env::args().nth(1).unwrap();
    let mut input = io::stdin().lock();
    receive(&mut input, &[r#""id":1"#, r#""method":"initialize""#]);
    frame(r#""id":1,"result":{"protocolVersion":1}"#);
    receive(&mut input, &[r#""id":2"#, r#""method":"session/new""#]);
    frame(r#""id":2,"result":{"sessionId":"s1"}"#);

    receive(&mut input, &[r#""id":3"#, r#""method":"session/prompt""#, r#""sessionId":"s1""#]);
    // Foreign session permission request must be cancelled immediately.
    frame(r#""id":91,"method":"session/request_permission","params":{"sessionId":"foreign","toolCall":{"title":"Run rm"},"options":[{"optionId":"allow_once","kind":"allow_once"}]}"#);
    receive(&mut input, &[r#""id":91"#, r#""outcome":"cancelled""#]);

    // Matching session permission request waits for explicit approve or reject.
    if mode == "approve" {
        frame(r#""id":92,"method":"session/request_permission","params":{"sessionId":"s1","toolCall":{"title":"Run cargo test","rawInput":{"command":"cargo test"}},"options":[{"optionId":"opt-allow","kind":"allow_once"},{"optionId":"opt-reject","kind":"reject_once"}]}"#);
        receive(&mut input, &[r#""id":92"#, r#""outcome":"selected""#, r#""optionId":"opt-allow""#]);
        frame(r#""method":"session/update","params":{"sessionId":"s1","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"approved ok"}}}"#);
    } else if mode == "reject" {
        frame(r#""id":93,"method":"session/request_permission","params":{"sessionId":"s1","toolCall":{"title":"Write file","rawInput":{"path":"foo.txt"}},"options":[{"optionId":"opt-allow","kind":"allow_once"},{"optionId":"opt-deny","kind":"reject_once"}]}"#);
        receive(&mut input, &[r#""id":93"#, r#""outcome":"selected""#, r#""optionId":"opt-deny""#]);
        frame(r#""method":"session/update","params":{"sessionId":"s1","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"rejected ok"}}}"#);
    } else {
        frame(r#""id":94,"method":"session/request_permission","params":{"sessionId":"s1","toolCall":{"kind":"execute"},"options":[{"optionId":"opt-allow","kind":"allow_once"}]}"#);
        receive(&mut input, &[r#""id":94"#, r#""outcome":"cancelled""#]);
        frame(r#""method":"session/update","params":{"sessionId":"s1","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"cancelled ok"}}}"#);
    }
    frame(r#""id":3,"result":{"stopReason":"end_turn"}"#);
    let _ = input.read_line(&mut String::new());
}
"###;

struct Mock(PathBuf);
impl Mock {
    fn build() -> Self {
        let temp = std::env::temp_dir();
        let dir = (0..).find_map(|n| {
            let path = temp.join(format!("velt-bridge-{}-{n}", process::id()));
            match fs::create_dir(&path) {
                Ok(()) => Some(Self(path)),
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => None,
                Err(err) => panic!("create ACP bridge mock directory: {err}"),
            }
        }).unwrap();
        let mut rustc = Command::new("rustc")
            .current_dir(&dir.0).args(["--crate-name", "acp_bridge_mock", "-", "-o"])
            .arg(dir.binary()).stdin(Stdio::piped()).spawn().unwrap();
        write!(rustc.stdin.take().unwrap(), "{BRIDGE_MOCK}").unwrap();
        assert!(rustc.wait().unwrap().success());
        dir
    }

    fn binary(&self) -> PathBuf {
        self.0.join(format!("mock{}", std::env::consts::EXE_SUFFIX))
    }

    fn start(&self, chat: &mut Chat, mode: &str) {
        chat.start_command(self.binary().to_str().unwrap(), &[mode], &self.0, || {}).unwrap();
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
fn queues_matching_permissions_and_sends_explicit_approve_or_reject() {
    let mock = Mock::build();

    let mut chat = Chat::default();
    mock.start(&mut chat, "approve");
    until(&mut chat, |c| c.connection() == ConnectionState::Active);
    chat.prompt("run tests").unwrap();
    until(&mut chat, |c| c.pending_permission().is_some());
    let perm = chat.pending_permission().unwrap();
    assert_eq!(perm.title, "Run cargo test");
    assert_eq!(perm.detail.as_deref(), Some("cargo test"));
    assert_eq!(perm.allow_option_id.as_deref(), Some("opt-allow"));
    assert_eq!(perm.reject_option_id.as_deref(), Some("opt-reject"));
    assert!(chat.approve_permission().unwrap());
    assert!(chat.pending_permission().is_none());
    until(&mut chat, |c| !c.is_prompt_pending());
    assert_eq!(chat.connection(), ConnectionState::Active);
    assert_eq!(chat.response(), "approved ok");

    let mut chat = Chat::default();
    mock.start(&mut chat, "reject");
    until(&mut chat, |c| c.connection() == ConnectionState::Active);
    chat.prompt("write file").unwrap();
    until(&mut chat, |c| c.pending_permission().is_some());
    assert_eq!(chat.pending_permission().unwrap().detail.as_deref(), Some("foo.txt"));
    assert!(chat.reject_permission().unwrap());
    until(&mut chat, |c| !c.is_prompt_pending());
    assert_eq!(chat.connection(), ConnectionState::Active);
    assert_eq!(chat.response(), "rejected ok");

    let mut chat = Chat::default();
    mock.start(&mut chat, "cancel-fallback");
    until(&mut chat, |c| c.connection() == ConnectionState::Active);
    chat.prompt("exec").unwrap();
    until(&mut chat, |c| c.pending_permission().is_some());
    assert!(chat.reject_permission().unwrap());
    until(&mut chat, |c| !c.is_prompt_pending());
    assert_eq!(chat.connection(), ConnectionState::Active);
    assert_eq!(chat.response(), "cancelled ok");
}

#[test]
fn context_formatter_includes_cwd_and_drops_oldest_lines_within_limit() {
    let lines: Vec<String> = (0..250).map(|i| format!("line-{i:03} {}", "x".repeat(80))).collect();
    let formatted = format_prompt_with_context("check failure", Path::new("D:\\repo\\velt"), &lines);
    assert!(formatted.len() <= MAX_PROMPT_BYTES);
    assert!(formatted.contains("cwd: D:\\repo\\velt"));
    assert!(formatted.contains("line-249"));
    assert!(!formatted.contains("line-000"));
    assert!(formatted.ends_with("\ncheck failure"));
}
