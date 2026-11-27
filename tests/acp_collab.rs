use my_warp::acp::{AcpSessionState, Agent, Chat, ConnectionState, MAX_PROMPT_BYTES};
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::{self, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

/// Echoes every received prompt text back as `<name> got: <text>`.
const ECHO_MOCK: &str = r###"
use std::io::{self, BufRead};
fn frame(body: &str) { println!(r#"{{"jsonrpc":"2.0",{body}}}"#); }
fn main() {
    let name = std::env::args().nth(1).unwrap();
    let mut id = 0;
    for line in io::stdin().lock().lines() {
        let line = line.unwrap();
        if line.contains(r#""outcome""#) {
            std::fs::write(format!("{name}.reply"), &line).unwrap();
            continue;
        }
        id += 1;
        if line.contains(r#""method":"initialize""#) {
            frame(&format!(r#""id":{id},"result":{{"protocolVersion":1}}"#));
        } else if line.contains(r#""method":"session/new""#) {
            frame(&format!(r#""id":{id},"result":{{"sessionId":"{name}"}}"#));
        } else if line.contains(r#""method":"session/prompt""#) {
            let start = line.find(r#""text":""#).unwrap() + 8;
            let end = line.find(r#"","type":"text""#).unwrap();
            let text = &line[start..end];
            frame(&format!(r#""method":"session/update","params":{{"sessionId":"{name}","update":{{"sessionUpdate":"agent_message_chunk","content":{{"type":"text","text":"{name} got: {text}"}}}}}}"#));
            let permission = format!(r#""id":900,"method":"session/request_permission","params":{{"sessionId":"{name}","toolCall":{{"title":"rm"}},"options":[{{"optionId":"yes","kind":"allow_once"}}]}}"#);
            if text.ends_with("ask-before-end") {
                frame(&permission);
            }
            frame(&format!(r#""id":{id},"result":{{"stopReason":"end_turn"}}"#));
            if text.ends_with("ask-after-end") {
                frame(&permission);
            }
        }
    }
}
"###;

struct Mock(PathBuf);

impl Mock {
    fn build(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("velt-collab-{}-{tag}", process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir(&dir).unwrap();
        let mock = Self(dir);
        let mut rustc = Command::new("rustc")
            .current_dir(&mock.0)
            .args(["--crate-name", "acp_collab_mock", "-", "-o"])
            .arg(mock.binary())
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        write!(rustc.stdin.take().unwrap(), "{ECHO_MOCK}").unwrap();
        assert!(rustc.wait().unwrap().success());
        mock
    }

    fn binary(&self) -> PathBuf {
        self.0.join(format!("mock{}", std::env::consts::EXE_SUFFIX))
    }

    fn launcher<'a>(
        &'a self,
        started: &'a mut Vec<Agent>,
    ) -> impl FnOnce(&mut Chat, Agent) -> io::Result<()> + 'a {
        move |chat, agent| {
            started.push(agent);
            let name = format!("{agent:?}");
            chat.start_command(self.binary().to_str().unwrap(), &[&name], &self.0, || {})
        }
    }

    fn send(&self, state: &mut AcpSessionState, started: &mut Vec<Agent>, text: &str) -> Result<(), String> {
        state.input.set_text(text);
        state.submit_routed(None, self.launcher(started))
    }
}

impl Drop for Mock {
    fn drop(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while fs::remove_dir_all(&self.0).is_err() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
    }
}

fn settle(state: &mut AcpSessionState) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while state.pending_prompt.is_some() {
        assert!(Instant::now() < deadline, "timed out: {:?}", state.chat.connection());
        if !state.poll().1 {
            thread::sleep(Duration::from_millis(5));
        }
    }
}

fn open_state() -> AcpSessionState {
    let mut state = AcpSessionState::default();
    state.open = true;
    state
}

#[test]
fn mentions_route_prompts_and_share_only_unseen_turns() {
    let mock = Mock::build("share");
    let mut state = open_state();
    let mut started = Vec::new();

    mock.send(&mut state, &mut started, "@claude build it").unwrap();
    assert_eq!(state.agent, Agent::Claude);
    assert_eq!(state.input.text(), "");
    settle(&mut state);
    assert_eq!(state.turns[0].agent, Agent::Claude);
    assert_eq!(state.turns[0].prompt, "build it");
    assert_eq!(state.turns[0].response, "Claude got: build it");

    mock.send(&mut state, &mut started, "@CODEX review that").unwrap();
    settle(&mut state);
    let review = &state.turns[1].response;
    assert_eq!(state.turns[1].agent, Agent::Codex);
    assert!(review.starts_with("Codex got: [Shared conversation]"), "{review}");
    assert!(review.contains("### User to Claude Code\nbuild it"), "{review}");
    assert!(review.contains("### Claude Code\nClaude got: build it"), "{review}");
    assert!(review.ends_with("[Your turn]\nreview that"), "{review}");

    mock.send(&mut state, &mut started, "@claude apply the review").unwrap();
    settle(&mut state);
    let fix = &state.turns[2].response;
    let first_entry = fix.split("### ").nth(1).unwrap();
    assert!(first_entry.starts_with("User to Codex\nreview that"), "{fix}");
    assert!(fix.ends_with("[Your turn]\napply the review"), "{fix}");
    assert_eq!(started, [Agent::Claude, Agent::Codex]);

    mock.send(&mut state, &mut started, "and again").unwrap();
    settle(&mut state);
    assert_eq!(state.turns[3].agent, Agent::Claude);
    assert_eq!(state.turns[3].response, "Claude got: and again");
}

#[test]
fn mention_alone_switches_and_unknown_mention_stays_text() {
    let mock = Mock::build("plain");
    let mut state = open_state();
    let mut started = Vec::new();

    mock.send(&mut state, &mut started, "@agy").unwrap();
    assert_eq!(state.agent, Agent::Antigravity);
    mock.send(&mut state, &mut started, "@codex").unwrap();
    assert_eq!(state.agent, Agent::Codex);
    assert_eq!(state.input.text(), "");
    assert!(state.pending_prompt.is_none());

    let deadline = Instant::now() + Duration::from_secs(5);
    while state.chat.connection() != ConnectionState::Active {
        assert!(Instant::now() < deadline);
        state.poll();
        thread::sleep(Duration::from_millis(5));
    }
    mock.send(&mut state, &mut started, "@types/node is missing").unwrap();
    settle(&mut state);
    assert_eq!(state.turns[0].agent, Agent::Codex);
    assert_eq!(state.turns[0].prompt, "@types/node is missing");
}

#[test]
fn one_turn_at_a_time_and_failed_launch_keeps_the_input() {
    let mock = Mock::build("busy");
    let mut state = open_state();
    let mut started = Vec::new();

    state.input.set_text("too early");
    assert!(state.submit_routed(None, mock.launcher(&mut started)).is_err());
    assert_eq!(state.input.text(), "too early");

    mock.send(&mut state, &mut started, "@claude first").unwrap();
    assert!(mock.send(&mut state, &mut started, "@codex second").is_err());
    assert_eq!(state.agent, Agent::Claude);
    assert_eq!(state.input.text(), "@codex second");
    settle(&mut state);

    let long = format!("@codex {}", "x".repeat(MAX_PROMPT_BYTES + 1));
    assert!(mock.send(&mut state, &mut started, &long).is_err());
    assert_eq!(state.agent, Agent::Claude);

    state.input.set_text("@codex ping");
    state
        .submit_routed(None, |chat, _| {
            chat.start_command(mock.0.join("missing").to_str().unwrap(), &[], &mock.0, || {})
        })
        .unwrap();
    settle(&mut state);
    assert!(matches!(state.chat.connection(), ConnectionState::Error(_)));
    assert_eq!(state.input.text(), "ping");
    assert_eq!(state.turns.len(), 1);
}

#[test]
fn context_is_attached_and_closing_stops_every_agent() {
    let mock = Mock::build("close");
    let mut state = open_state();
    let mut started = Vec::new();
    mock.send(&mut state, &mut started, "@claude one").unwrap();
    settle(&mut state);

    state.include_context = true;
    state.input.set_text("@codex two");
    state
        .submit_routed(
            Some((Path::new("repo"), &["last line".to_string()])),
            mock.launcher(&mut started),
        )
        .unwrap();
    settle(&mut state);
    assert!(state.turns[1].response.contains("[Terminal context]"));
    assert!(state.turns[1].response.contains("last line"));
    assert_eq!(state.turns[1].prompt, "two");

    state.deactivate();
    state.open = true;
    mock.send(&mut state, &mut started, "@claude three").unwrap();
    settle(&mut state);
    assert_eq!(started, [Agent::Claude, Agent::Codex, Agent::Claude]);
    assert!(state.turns[2].response.contains("### User to Claude Code\none"));
}

#[test]
fn permission_requests_outside_a_running_turn_are_cancelled() {
    for (tag, prompt) in [("late", "ask-after-end"), ("open", "ask-before-end")] {
        let mock = Mock::build(tag);
        let mut state = open_state();
        let mut started = Vec::new();
        mock.send(&mut state, &mut started, &format!("@claude {prompt}")).unwrap();
        settle(&mut state);

        let reply = mock.0.join("Claude.reply");
        let deadline = Instant::now() + Duration::from_secs(5);
        while !fs::read_to_string(&reply).is_ok_and(|line| line.ends_with('}')) {
            assert!(Instant::now() < deadline, "{tag}: no permission reply");
            state.poll();
            thread::sleep(Duration::from_millis(5));
        }
        let line = fs::read_to_string(&reply).unwrap();
        assert!(line.contains(r#""outcome":"cancelled""#), "{tag}: {line}");
        assert!(state.chat.pending_permission().is_none());

        state.auto_approve = true;
        assert!(!state.drain_auto_permissions());
    }
}

#[test]
fn switching_away_from_a_queued_prompt_keeps_the_text() {
    let mock = Mock::build("queued");
    let mut state = open_state();
    let mut started = Vec::new();
    mock.send(&mut state, &mut started, "@claude important work").unwrap();
    state.switch_agent(Agent::Codex);
    assert_eq!(state.input.text(), "important work");
    assert!(state.pending_prompt.is_none());
    assert!(state.turns.is_empty());
}
