use my_warp::acp::{AcpSessionState, Agent, ConnectionState};
use std::{
    fs,
    io::{self, Write},
    path::PathBuf,
    process::{self, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

/// Asks permission for the tool call and option list given as arguments, then
/// reports the client's reply as its answer.
const MOCK: &str = r###"
use std::io::{self, BufRead};
fn frame(body: &str) { println!(r#"{{"jsonrpc":"2.0",{body}}}"#); }
fn main() {
    let mut args = std::env::args().skip(1);
    let (tool_call, options) = (args.next().unwrap(), args.next().unwrap());
    let mut id = 0;
    for line in io::stdin().lock().lines() {
        let line = line.unwrap();
        if line.contains(r#""outcome""#) {
            let answer = if line.contains(r#""optionId":"once""#) { "allowed" } else { "other" };
            frame(&format!(r#""method":"session/update","params":{{"sessionId":"s","update":{{"sessionUpdate":"agent_message_chunk","content":{{"type":"text","text":"{answer}"}}}}}}"#));
            frame(&format!(r#""id":{id},"result":{{"stopReason":"end_turn"}}"#));
            continue;
        }
        id += 1;
        if line.contains(r#""method":"initialize""#) {
            frame(&format!(r#""id":{id},"result":{{"protocolVersion":1}}"#));
        } else if line.contains(r#""method":"session/new""#) {
            frame(&format!(r#""id":{id},"result":{{"sessionId":"s"}}"#));
        } else if line.contains(r#""method":"session/prompt""#) {
            frame(&format!(r#""id":900,"method":"session/request_permission","params":{{"sessionId":"s","toolCall":{tool_call},"options":{options}}}"#));
        }
    }
}
"###;

const ONCE: &str = r#"[{"optionId":"once","kind":"allow_once"},{"optionId":"no","kind":"reject_once"}]"#;
const ALWAYS: &str = r#"[{"optionId":"always","kind":"allow_always"},{"optionId":"no","kind":"reject_once"}]"#;

struct Mock(PathBuf);

impl Mock {
    fn build() -> Self {
        let dir = std::env::temp_dir().join(format!("velt-memperm-{}", process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir(&dir).unwrap();
        let mock = Self(dir);
        let mut rustc = Command::new("rustc")
            .current_dir(&mock.0)
            .args(["--crate-name", "acp_memory_mock", "-", "-o"])
            .arg(mock.binary())
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        write!(rustc.stdin.take().unwrap(), "{MOCK}").unwrap();
        assert!(rustc.wait().unwrap().success());
        mock
    }

    fn binary(&self) -> PathBuf {
        self.0.join(format!("mock{}", std::env::consts::EXE_SUFFIX))
    }

    /// Returns whether the request was answered without the user.
    fn auto_approved(&self, agent: Agent, tool_call: &str, options: &str) -> bool {
        let mut state = AcpSessionState::default();
        state.open = true;
        state.agent = agent;
        state
            .chat
            .start_command(self.binary().to_str().unwrap(), &[tool_call, options], &self.0, || {})
            .unwrap();
        pump(&mut state, |s| s.chat.connection() == ConnectionState::Active);
        state.input.set_text("go");
        state.submit_prompt().unwrap();
        pump(&mut state, |s| !s.chat.is_prompt_pending() || s.chat.pending_permission().is_some());
        if state.chat.pending_permission().is_some() {
            return false;
        }
        assert_eq!(state.turns[0].response, "allowed");
        true
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

fn pump(state: &mut AcpSessionState, ready: impl Fn(&AcpSessionState) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready(state) {
        assert!(Instant::now() < deadline, "timed out: {:?}", state.chat.connection());
        if !state.poll().1 {
            thread::sleep(Duration::from_millis(5));
        }
    }
}

fn call(server: &str, tool: &str, arguments: &str) -> String {
    format!(r#"{{"title":"MCP Tool {tool}","rawInput":{{"Arguments":{arguments},"ServerName":"{server}","ToolName":"{tool}"}}}}"#)
}

#[test]
fn only_antigravity_engram_reads_and_saves_skip_the_permission_card() {
    let mock = Mock::build();
    let agy = Agent::Antigravity;
    let plugin = "gentle-ai-engram_engram";

    for tool in [
        "mem_current_project", "mem_search", "mem_suggest_topic_key", "mem_save",
        "mem_save_prompt", "mem_session_summary",
    ] {
        assert!(mock.auto_approved(agy, &call(plugin, tool, "{}"), ONCE), "{tool}");
    }
    assert!(mock.auto_approved(agy, &call("engram", "mem_search", r#"{"all_projects":false}"#), ONCE));
    let here = mock.0.file_name().unwrap().to_str().unwrap().to_owned();
    for (tool, arguments) in [
        ("mem_context", format!(r#"{{"project":"{here}"}}"#)),
        ("mem_search", format!(r#"{{"query":"x","project":"{here}","scope":"project","limit":5}}"#)),
        ("mem_save", format!(r#"{{"title":"t","content":"c","type":"decision","project":"{here}"}}"#)),
    ] {
        assert!(mock.auto_approved(agy, &call(plugin, tool, &arguments), ONCE), "{tool} {arguments}");
    }

    for tool in [
        "mem_delete", "mem_update", "mem_judge", "mem_search_prompts", "mem_save_extra",
        "mem_context", "mem_get_observation",
    ] {
        assert!(!mock.auto_approved(agy, &call(plugin, tool, "{}"), ONCE), "{tool}");
    }
    let save = call(plugin, "mem_save", "{}");
    for (name, request) in [
        ("other server", call("other", "mem_save", "{}")),
        ("all projects", call(plugin, "mem_search", r#"{"all_projects":true}"#)),
        ("all projects as text", call(plugin, "mem_search", r#"{"all_projects":"true"}"#)),
        ("shell", r#"{"title":"git status","rawInput":{"CommandLine":"git status"}}"#.to_string()),
        ("disguised shell", save.replace(r#""ServerName""#, r#""CommandLine":"whoami","ServerName""#)),
        ("title mismatch", save.replace("MCP Tool mem_save", "rm -rf")),
        ("other project search", call(plugin, "mem_search", r#"{"query":"x","project":"other"}"#)),
        ("other project context", call(plugin, "mem_context", r#"{"project":"other"}"#)),
        ("other project save", call(plugin, "mem_save", r#"{"project":"other"}"#)),
        ("other project summary", call(plugin, "mem_session_summary", r#"{"project":"other"}"#)),
        ("other directory prompt", call(plugin, "mem_save_prompt", r#"{"directory":"C:/other"}"#)),
        ("global scope search", call(plugin, "mem_search", r#"{"query":"x","scope":"global"}"#)),
        ("global scope save", call(plugin, "mem_save", r#"{"scope":"global"}"#)),
        ("foreign session", call(plugin, "mem_save", r#"{"session_id":"s9"}"#)),
        ("unknown argument", call(plugin, "mem_search", r#"{"org":"acme"}"#)),
        ("arguments as text", call(plugin, "mem_search", r#""{\"all_projects\":true}""#)),
        ("arguments as list", call(plugin, "mem_search", "[]")),
    ] {
        assert!(!mock.auto_approved(agy, &request, ONCE), "{name}");
    }
    assert!(!mock.auto_approved(agy, &save, ALWAYS));
    assert!(!mock.auto_approved(Agent::Codex, &save, ONCE));
    assert!(!mock.auto_approved(Agent::Claude, &save, ONCE));
}
