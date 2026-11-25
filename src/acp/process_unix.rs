use std::{
    io,
    os::unix::process::CommandExt,
    path::Path,
    process::{ChildStderr, ChildStdin, ChildStdout, Command, Stdio},
};

type PipesAndStop = (ChildStdin, ChildStdout, ChildStderr, Box<dyn FnOnce()>);

pub(super) fn spawn(program: &str, args: &[String], cwd: &Path) -> io::Result<PipesAndStop> {
    let mut child = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .process_group(0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let pipes = (
        child.stdin.take().unwrap(),
        child.stdout.take().unwrap(),
        child.stderr.take().unwrap(),
    );
    let stop = move || {
        // We created this group before exec. Signal while the leader is still
        // unreaped so that its process/group ID cannot have been recycled.
        unsafe { libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL) };
        let _ = child.kill();
        let _ = child.wait();
    };
    Ok((pipes.0, pipes.1, pipes.2, Box::new(stop)))
}
