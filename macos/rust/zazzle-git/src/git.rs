// Runs git and other commands: captured with a timeout, interactive with the terminal, or replacing this process.

use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

// How long a non-interactive git command may take.
pub const GIT_TIMEOUT: Duration = Duration::from_secs(30);

// Reads a pipe to the end on a thread.
pub fn drain(mut pipe: impl Read + Send + 'static) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = pipe.read_to_end(&mut bytes);
        String::from_utf8_lossy(&bytes).into_owned()
    })
}

// Waits for a child up to the timeout, killing it when the time runs out.
fn wait_timeout(child: &mut std::process::Child, timeout: Duration) -> Result<ExitStatus, String> {
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            return Ok(status);
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err("timed out".to_string());
        }
        thread::sleep(Duration::from_millis(5));
    }
}

// Runs a command with the input on stdin and returns its stdout, or its stderr (or the reason) when it fails.
pub fn run_with_input(cmd: &[String], input: &str, timeout: Duration) -> Result<String, String> {
    let (prog, args) = cmd.split_first().ok_or("empty command")?;
    let mut child = Command::new(prog)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{prog}: {e}"))?;
    let mut stdin = child.stdin.take().expect("stdin is piped");
    let data = input.to_string();
    let writer = thread::spawn(move || {
        let _ = stdin.write_all(data.as_bytes());
    });
    let out = drain(child.stdout.take().expect("stdout is piped"));
    let err = drain(child.stderr.take().expect("stderr is piped"));
    let status = wait_timeout(&mut child, timeout);
    let _ = writer.join();
    let out = out.join().unwrap_or_default();
    let err = err.join().unwrap_or_default();
    match status? {
        s if s.success() => Ok(out),
        _ => Err(err.trim().to_string()),
    }
}

// Runs git with the arguments and returns its stdout, or its error message.
pub fn git(args: &[&str]) -> Result<String, String> {
    let mut cmd = vec!["git".to_string()];
    cmd.extend(args.iter().map(|s| s.to_string()));
    run_with_input(&cmd, "", GIT_TIMEOUT)
}

// Runs git attached to the terminal and returns its exit code; no timeout, because hooks and signing may ask the user.
pub fn git_interactive(args: &[String]) -> i32 {
    match Command::new("git").args(args).status() {
        Ok(s) => s.code().unwrap_or(1),
        Err(e) => {
            eprintln!("zazzle git: cannot run git: {e}");
            1
        }
    }
}

// Replaces this process with git and the arguments; returns the exit code to use only if git cannot be started.
pub fn exec_git(args: &[String]) -> i32 {
    let e = Command::new("git").args(args).exec();
    eprintln!("zazzle git: cannot run git: {e}");
    127
}

#[cfg(test)]
mod tests {
    use super::*;

    // Turns words into a command line.
    fn cmd(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    // Checks output, input, failure, a missing program and the timeout.
    #[test]
    fn test_run_with_input() {
        assert_eq!(run_with_input(&cmd(&["cat"]), "hi", GIT_TIMEOUT), Ok("hi".into()));
        assert_eq!(
            run_with_input(&cmd(&["sh", "-c", "echo bad >&2; exit 3"]), "", GIT_TIMEOUT),
            Err("bad".into())
        );
        assert!(run_with_input(&cmd(&["/nonexistent/prog"]), "", GIT_TIMEOUT).is_err());
        assert!(run_with_input(&[], "", GIT_TIMEOUT).is_err());
        let slow = run_with_input(&cmd(&["sleep", "5"]), "", Duration::from_millis(50));
        assert_eq!(slow, Err("timed out".into()));
    }

    // Checks running git captured and attached to the terminal.
    #[test]
    fn test_git_runs() {
        assert!(git(&["--version"]).is_ok_and(|v| v.starts_with("git version")));
        assert!(git(&["no-such-command"]).is_err());
        assert_eq!(git_interactive(&cmd(&["--version"])), 0);
        assert_ne!(git_interactive(&cmd(&["no-such-command"])), 0);
    }
}
