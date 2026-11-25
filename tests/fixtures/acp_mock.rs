use std::{
    io::{self, BufRead, Read, Write},
    net::TcpStream,
    process::{Command, ExitCode, Stdio},
    thread,
    time::Duration,
};

fn request(input: &mut impl BufRead) -> io::Result<String> {
    let mut line = String::new();
    input.read_line(&mut line)?;
    if !line.ends_with('\n') {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "expected a newline-delimited request",
        ));
    }
    line.pop();
    Ok(line)
}

fn frame(output: &mut impl Write, value: &str) -> io::Result<()> {
    writeln!(output, "{value}")?;
    output.flush()
}

fn result(output: &mut impl Write, id: u64, fields: &str, observed: &str) -> io::Result<()> {
    // Embed the actual request so the parent can check it as JSON, not raw bytes.
    let body = format!(r#"{{{fields},"_meta":{{"mockRequest":{observed}}}}}"#);
    frame(
        output,
        &format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{body}}}"#),
    )
}

fn main() -> io::Result<ExitCode> {
    let mut args = std::env::args().skip(1);
    let scenario = args.next().expect("mock scenario");
    if scenario == "descendant-worker" {
        let mut stream = TcpStream::connect(args.next().expect("readiness address"))?;
        stream.set_read_timeout(Some(Duration::from_secs(30)))?;
        stream.write_all(b"!")?;
        // Stay alive until killed. If the test fails, closing its socket also
        // lets the mock exit and avoids leaving behind an unowned process.
        let _ = stream.read(&mut [0]);
        return Ok(ExitCode::SUCCESS);
    }
    let mut input = io::stdin().lock();
    let mut output = io::stdout().lock();
    let init = request(&mut input)?;

    match scenario.as_str() {
        "descendant" => {
            let mut child = Command::new(std::env::current_exe()?)
                .arg("descendant-worker")
                .arg(args.next().expect("readiness address"))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()?;
            return child.wait().map(|status| {
                if status.success() {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::FAILURE
                }
            });
        }
        "flood" => {
            for _ in 0..128 {
                frame(&mut output, r#"{"jsonrpc":"2.0","method":"mock/update"}"#)?;
            }
        }
        "malformed" => frame(&mut output, "{not JSON}")?,
        "wrong-version" => frame(&mut output, r#"{"jsonrpc":"1.0","id":1,"result":{}}"#)?,
        "unterminated" => {
            output.write_all(br#"{"jsonrpc":"2.0","id":1,"result":{}}"#)?;
            output.flush()?;
        }
        "early-exit" => {
            eprintln!("mock ACP agent exited before initialization");
            return Ok(ExitCode::from(17));
        }
        "lifecycle" | "auth-required" => {
            result(&mut output, 1, r#""protocolVersion":1"#, &init)?;
            let new_session = request(&mut input)?;
            if scenario == "auth-required" {
                let data = format!(r#"{{"mockRequest":{new_session}}}"#);
                let error = format!(
                    r#"{{"code":-32000,"message":"Authentication required","data":{data}}}"#
                );
                frame(
                    &mut output,
                    &format!(r#"{{"jsonrpc":"2.0","id":2,"error":{error}}}"#),
                )?;
                return Ok(ExitCode::SUCCESS);
            }

            result(&mut output, 2, r#""sessionId":"mock-session""#, &new_session)?;
            let prompt = request(&mut input)?;
            frame(
                &mut output,
                r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"mock-session","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"Hello, "}}}}"#,
            )?;
            // The conversation cannot finish until the parent receives the first chunk.
            let resume = request(&mut input)?;
            result(&mut output, 4, r#""resumed":true"#, &resume)?;
            frame(
                &mut output,
                r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"mock-session","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"world!\n"}}}}"#,
            )?;
            result(&mut output, 3, r#""stopReason":"end_turn""#, &prompt)?;

            // Keep stdout open even if stdin closes. The fallback bounds a leaked
            // mock's lifetime and cannot be mistaken for a clean client shutdown.
            thread::sleep(Duration::from_secs(30));
            frame(&mut output, r#"{"jsonrpc":"2.0","method":"mock/timeout"}"#)?;
        }
        _ => panic!("unknown mock scenario: {scenario}"),
    }
    Ok(ExitCode::SUCCESS)
}
