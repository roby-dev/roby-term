use my_warp::acp::{Agent, Client, Event};
use std::{path::Path, time::Duration};

#[test]
fn missing_agent_is_reported_before_creating_a_session() {
    let result = Client::spawn("velt-nonexistent-acp-agent", &[], Path::new("."), || {});
    assert_eq!(result.err().unwrap().kind(), std::io::ErrorKind::NotFound);
}

#[test]
#[ignore = "requires an installed Antigravity CLI with ACP support"]
fn initializes_real_antigravity_over_stdio() {
    let cwd = std::env::current_dir().unwrap();
    let (program, args) = Agent::Antigravity.command();
    let mut client = Client::spawn(program, args, &cwd, || {}).unwrap();
    client.initialize().unwrap();
    match client.recv_timeout(Duration::from_secs(10)).unwrap() {
        Event::Message(value) => assert_eq!(value["result"]["protocolVersion"], 1),
        other => panic!("unexpected ACP event: {other:?}"),
    }
}
