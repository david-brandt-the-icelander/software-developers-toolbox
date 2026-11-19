// Hands text to the local model (zazzle-llm) from inside zazzle, so an agent reads the model's short answer instead of the whole text: post-processing a command's output (`--llm`), `agent prompt` over MCP, and choosing rules for `brief`.
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const MAX_INPUT: usize = 400_000;
const DEFAULT_TIMEOUT: u64 = 300;

// The local model's answer and whether its token limit cut it short.
#[derive(Debug, PartialEq)]
pub struct Reply {
    pub text: String,
    pub truncated: bool,
}

// What a finished zazzle-llm run produced: its exit code, standard output and standard error.
struct Ran {
    code: i32,
    out: String,
    err: String,
}

// Returns how long one local model call may take: ZAZZLE_LLM_TIMEOUT seconds, default 300.
fn timeout() -> Duration {
    let secs = std::env::var("ZAZZLE_LLM_TIMEOUT").ok().and_then(|v| v.parse().ok()).filter(|&n| n > 0);
    Duration::from_secs(secs.unwrap_or(DEFAULT_TIMEOUT))
}

// Reads a pipe to its end on its own thread.
fn drain(mut r: impl Read + Send + 'static) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = r.read_to_end(&mut buf);
        String::from_utf8_lossy(&buf).into_owned()
    })
}

// Runs a program with arguments, writing `input` to its standard input, and kills it past the timeout.
fn run(program: &Path, args: &[String], input: &str, limit: Duration) -> Result<Ran, String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("starting {}: {e}", program.display()))?;
    let mut stdin = child.stdin.take().ok_or("no stdin pipe")?;
    let bytes = input.as_bytes().to_vec();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&bytes);
    });
    let out = drain(child.stdout.take().ok_or("no stdout pipe")?);
    let err = drain(child.stderr.take().ok_or("no stderr pipe")?);
    let start = Instant::now();
    let status = loop {
        if let Some(st) = child.try_wait().map_err(|e| e.to_string())? {
            break st;
        }
        if start.elapsed() > limit {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "the local model took longer than {}s (ZAZZLE_LLM_TIMEOUT)",
                limit.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let _ = writer.join();
    let (out, err) = (out.join().unwrap_or_default(), err.join().unwrap_or_default());
    Ok(Ran { code: status.code().unwrap_or(-1), out, err })
}

// Asks the local model at a given program: the instruction, then the input (if any), with extra zazzle-llm flags (effort, system, grammar); never downloads a model.
pub fn ask_with(
    program: &Path,
    instruction: &str,
    input: &str,
    flags: &[&str],
    limit: Duration,
) -> Result<Reply, String> {
    if input.len() > MAX_INPUT {
        return Err(format!(
            "{} bytes is more than the local model takes ({MAX_INPUT}); narrow the command first",
            input.len()
        ));
    }
    static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|p| p.into_inner());
    let mut args: Vec<String> = vec!["prompt".into(), "--no-download".into()];
    args.extend(flags.iter().map(|f| f.to_string()));
    if input.is_empty() {
        args.push(instruction.into());
    } else {
        args.extend([format!("{instruction}\n\n---\n"), "-".into()]);
    }
    let ran = run(program, &args, input, limit)?;
    let reason = || {
        let last = ran.err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
        let last = last.strip_prefix("error: ").unwrap_or(last);
        if last.is_empty() {
            format!("zazzle-llm exited {}", ran.code)
        } else {
            last.to_string()
        }
    };
    match ran.code {
        0 | 3 => Ok(Reply { text: ran.out.trim().to_string(), truncated: ran.code == 3 }),
        _ => Err(format!("local model: {}", reason())),
    }
}

// Returns zazzle-llm: $ZAZZLE_LLM, else beside zazzled or from the tool store (downloading the program, never a model).
fn program() -> Result<PathBuf, String> {
    if let Some(p) = std::env::var_os("ZAZZLE_LLM") {
        return Ok(PathBuf::from(p));
    }
    let exe = std::env::current_exe().ok();
    crate::tools::path(
        "zazzle-llm",
        exe.as_deref().and_then(Path::parent),
        crate::tools::store(),
        &mut |_| {},
    )
}

// Asks the local model with zazzle-llm, at the default model.
pub fn ask(instruction: &str, input: &str, flags: &[&str]) -> Result<Reply, String> {
    ask_with(&program()?, instruction, input, flags, timeout())
}

// Formats a post-processed answer with a line saying how much it saved.
pub fn mark(input: &str, reply: &Reply) -> String {
    let (a, b) = (crate::tokens::estimate(input), crate::tokens::estimate(&reply.text));
    let cut = if reply.truncated { "; the answer was cut short" } else { "" };
    format!(
        "{}\n[post-processed by the local model: ~{a} → ~{b} tokens{cut}; run without --llm for the exact output]",
        reply.text
    )
}

// The system message for post-processing: the result only, so the agent reads no reasoning.
const POST_SYSTEM: &str = "Do exactly what the instruction asks with the text after ---. Reply with only the result: no reasoning, no preamble, no notes.";

// Runs a command's output through the local model with an instruction, at low effort.
pub fn post(input: &str, instruction: &str) -> Result<String, String> {
    if input.trim().is_empty() {
        return Ok(input.to_string());
    }
    ask(instruction, input, &["--effort", "low", "--temp", "0", "--system", POST_SYSTEM])
        .map(|r| mark(input, &r))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::testutil::TempDir;
    use std::os::unix::fs::PermissionsExt;

    // Writes an executable shell script standing in for zazzle-llm.
    fn fake(t: &TempDir, body: &str) -> PathBuf {
        let p = t.path().join("zazzle-llm");
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).expect("write");
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        p
    }

    // Checks the arguments and input reach zazzle-llm and its answer comes back, also when cut short.
    #[test]
    fn test_llm_ask_with() {
        let t = TempDir::new();
        let echo = fake(&t, "printf '%s|' \"$@\"; cat; exit 0");
        let r =
            ask_with(&echo, "Sum up", "DATA", &["--effort", "low"], Duration::from_secs(10)).expect("ask");
        assert_eq!(
            r,
            Reply {
                text: "prompt|--no-download|--effort|low|Sum up\n\n---\n|-|DATA".into(),
                truncated: false
            }
        );
        let bare = ask_with(&echo, "Hi", "", &[], Duration::from_secs(10)).expect("bare");
        assert_eq!(bare.text, "prompt|--no-download|Hi|");
        let cut = fake(&t, "echo partial; exit 3");
        assert!(
            ask_with(&cut, "x", "y", &["--effort", "low"], Duration::from_secs(10)).expect("cut").truncated
        );
    }

    // Checks failures name the reason: the model's error line, a bare exit code, a timeout, too much input and a missing program.
    #[test]
    fn test_llm_ask_failures() {
        let t = TempDir::new();
        let d = Duration::from_secs(10);
        let missing = fake(&t, "echo 'error: qwen3.5-4b is not downloaded' >&2; exit 4");
        assert_eq!(
            ask_with(&missing, "x", "y", &["--effort", "low"], d),
            Err("local model: qwen3.5-4b is not downloaded".into())
        );
        let silent = fake(&t, "exit 2");
        assert_eq!(
            ask_with(&silent, "x", "y", &["--effort", "low"], d),
            Err("local model: zazzle-llm exited 2".into())
        );
        let slow = fake(&t, "sleep 5");
        assert!(ask_with(&slow, "x", "y", &["--effort", "low"], Duration::from_millis(200))
            .expect_err("slow")
            .contains("longer than"));
        let big = "x".repeat(MAX_INPUT + 1);
        assert!(ask_with(&silent, "x", &big, &["--effort", "low"], d)
            .expect_err("big")
            .contains("narrow the command"));
        assert!(ask_with(&t.path().join("none"), "x", "y", &["--effort", "low"], d)
            .expect_err("none")
            .contains("starting"));
    }

    // Checks the saving line, the cut-short note, empty output passing through and the timeout setting.
    #[test]
    fn test_llm_mark_and_post() {
        let long = "word ".repeat(400);
        let m = mark(&long, &Reply { text: "short".into(), truncated: true });
        assert!(m.starts_with("short\n[post-processed by the local model: ~") && m.contains("cut short"));
        assert_eq!(post("  \n", "x"), Ok("  \n".into()));
        let _g = crate::testutil::env_lock();
        std::env::set_var("ZAZZLE_LLM_TIMEOUT", "7");
        assert_eq!(timeout(), Duration::from_secs(7));
        std::env::remove_var("ZAZZLE_LLM_TIMEOUT");
        assert_eq!(timeout(), Duration::from_secs(DEFAULT_TIMEOUT));
    }
}
