// Asks the local model (zazzle-llm) from inside zazzled through one shared worker kept loaded between calls and stopped when idle: post-processing (`--llm`), `brief`, `investigate`, `agent prompt` over MCP and the per-terminal chats.
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const MAX_INPUT: usize = 400_000;
const DEFAULT_TIMEOUT: u64 = 300;
const DEFAULT_IDLE: u64 = 600;

// The local model's answer and whether its token limit cut it short.
#[derive(Debug, PartialEq)]
pub struct Reply {
    pub text: String,
    pub truncated: bool,
}

// Returns how long one local model call may take: ZAZZLE_LLM_TIMEOUT seconds, default 300.
fn timeout() -> Duration {
    secs("ZAZZLE_LLM_TIMEOUT", DEFAULT_TIMEOUT)
}

// Returns how long an unused worker stays loaded: ZAZZLE_LLM_IDLE seconds, default 600.
pub fn idle() -> Duration {
    secs("ZAZZLE_LLM_IDLE", DEFAULT_IDLE)
}

// Reads a positive number of seconds from the environment, or the default.
fn secs(var: &str, default: u64) -> Duration {
    let n = std::env::var(var).ok().and_then(|v| v.parse().ok()).filter(|&n| n > 0);
    Duration::from_secs(n.unwrap_or(default))
}

// Takes the last line a program printed to standard error, without `error: `, as the reason it failed.
fn last_reason(err: &str) -> Option<String> {
    let last = err.lines().rev().find(|l| !l.trim().is_empty())?.trim();
    Some(last.strip_prefix("error: ").unwrap_or(last).to_string())
}

// A zazzle-llm worker keeping one model loaded across prompts; its ready line names the model.
pub struct Session {
    child: std::process::Child,
    input: std::process::ChildStdin,
    lines: std::sync::mpsc::Receiver<String>,
    err: std::sync::Arc<Mutex<String>>,
    err_done: Option<std::thread::JoinHandle<()>>,
    limit: Duration,
    model: String,
}

impl Session {
    // Starts `PROGRAM worker --no-download FLAGS` and waits for its ready line, each wait bounded by `limit`.
    pub fn start_with(program: &Path, flags: &[&str], limit: Duration) -> Result<Session, String> {
        let mut child = Command::new(program)
            .args(["worker", "--no-download"])
            .args(flags)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("starting {}: {e}", program.display()))?;
        let (tx, lines) = std::sync::mpsc::channel();
        let out = child.stdout.take().ok_or("no stdout pipe")?;
        std::thread::spawn(move || {
            use std::io::BufRead;
            for l in std::io::BufReader::new(out).lines().map_while(Result::ok) {
                if tx.send(l).is_err() {
                    break;
                }
            }
        });
        let err = std::sync::Arc::new(Mutex::new(String::new()));
        let (sink, mut e) = (std::sync::Arc::clone(&err), child.stderr.take().ok_or("no stderr pipe")?);
        let err_done = std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n @ 1..) = e.read(&mut buf) {
                sink.lock().unwrap_or_else(|p| p.into_inner()).push_str(&String::from_utf8_lossy(&buf[..n]));
            }
        });
        let input = child.stdin.take().ok_or("no stdin pipe")?;
        let mut s =
            Session { child, input, lines, err, err_done: Some(err_done), limit, model: String::new() };
        let ready: serde_json::Value = serde_json::from_str(&s.line()?).unwrap_or_default();
        match ready["ready"].as_str() {
            Some(m) => {
                s.model = m.to_string();
                Ok(s)
            }
            None => Err(s.failed("the local model did not start")),
        }
    }

    // Waits for the worker's next line, killing it past the time limit.
    fn line(&mut self) -> Result<String, String> {
        match self.lines.recv_timeout(self.limit) {
            Ok(l) => Ok(l),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                let _ = self.child.kill();
                Err(format!(
                    "the local model took longer than {}s (ZAZZLE_LLM_TIMEOUT)",
                    self.limit.as_secs()
                ))
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                Err(self.failed("the local model stopped"))
            }
        }
    }

    // Names why the worker failed: the last line it printed to standard error, else `what`.
    fn failed(&mut self, what: &str) -> String {
        let _ = self.child.wait();
        let deadline = Instant::now() + Duration::from_secs(1);
        while self.err_done.as_ref().is_some_and(|h| !h.is_finished()) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        let err = self.err.lock().unwrap_or_else(|p| p.into_inner()).clone();
        format!("local model: {}", last_reason(&err).unwrap_or_else(|| what.to_string()))
    }

    // Sends one prompt (`{"user", "system"?, "max_tokens"?, "temp"?, ...}`) and returns the answer.
    pub fn ask(&mut self, req: &serde_json::Value) -> Result<Reply, String> {
        if req.to_string().len() > MAX_INPUT {
            return Err(format!("the prompt is more than the local model takes ({MAX_INPUT} bytes)"));
        }
        if writeln!(self.input, "{req}").and_then(|()| self.input.flush()).is_err() {
            return Err(self.failed("the local model stopped"));
        }
        let v: serde_json::Value =
            serde_json::from_str(&self.line()?).map_err(|e| format!("local model: unreadable reply: {e}"))?;
        match v["error"].as_str() {
            Some(e) => Err(format!("local model: {e}")),
            None => Ok(Reply {
                text: v["text"].as_str().unwrap_or_default().trim().to_string(),
                truncated: v["truncated"] == true,
            }),
        }
    }
}

impl Session {
    // Tells whether the worker process is still running.
    fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

impl Drop for Session {
    // Stops the worker, freeing the model's memory.
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
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

// The worker zazzled keeps loaded, the program it runs and when it last answered.
struct Live {
    session: Session,
    program: PathBuf,
    last: Instant,
}

// The one worker kept between calls (one model in memory at a time); its lock also makes calls take turns.
static POOL: Mutex<Option<Live>> = Mutex::new(None);

// Asks with the shared worker, starting it (or replacing it for another model) as needed: `model` None takes the loaded model, else the default; a worker that died or timed out is dropped.
pub fn ask_in(
    program: &Path,
    model: Option<&str>,
    req: &serde_json::Value,
    limit: Duration,
) -> Result<Reply, String> {
    let mut pool = POOL.lock().unwrap_or_else(|p| p.into_inner());
    let fits = |l: &Live| {
        l.program == program && model.is_none_or(|m| l.session.model == m || l.session.model.starts_with(m))
    };
    if !pool.as_ref().is_some_and(fits) {
        *pool = None;
        let mut flags = vec!["--effort", "low"];
        flags.extend(model.iter().flat_map(|m| ["--model", m]));
        let session = Session::start_with(program, &flags, limit)?;
        *pool = Some(Live { session, program: program.to_path_buf(), last: Instant::now() });
    }
    let live = pool.as_mut().expect("started above");
    let r = live.session.ask(req);
    live.last = Instant::now();
    if r.is_err() && !live.session.alive() {
        *pool = None;
    }
    r
}

// Stops the shared worker when it has been unused for longer than `idle`; returns whether it did.
pub fn reap(idle: Duration) -> bool {
    let mut pool = POOL.lock().unwrap_or_else(|p| p.into_inner());
    let stale = pool.as_ref().is_some_and(|l| l.last.elapsed() > idle);
    if stale {
        *pool = None;
    }
    stale
}

// Starts, once, the thread that stops the worker after ZAZZLE_LLM_IDLE seconds unused.
fn start_reaper() {
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        std::thread::spawn(|| loop {
            let idle = idle();
            std::thread::sleep((idle / 4).clamp(Duration::from_secs(1), Duration::from_secs(30)));
            reap(idle);
        });
    });
}

// Runs `zazzle-llm fetch [--model M]`: downloads the model unless it is here (progress on this terminal's standard error) and returns its full name.
pub fn fetch(model: Option<&str>) -> Result<String, String> {
    let program = program()?;
    let out = Command::new(&program)
        .arg("fetch")
        .stdin(Stdio::null())
        .args(model.iter().flat_map(|m| ["--model", m]))
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| format!("starting {}: {e}", program.display()))?;
    let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
    match out.status.success() && !name.is_empty() {
        true => Ok(name),
        false => Err(format!("zazzle-llm could not fetch {}", model.unwrap_or("the default model"))),
    }
}

// Asks the local model through the shared worker (zazzle-llm from ZAZZLE_LLM, beside zazzled or the tool store; never downloading a model).
pub fn ask(model: Option<&str>, req: &serde_json::Value) -> Result<Reply, String> {
    start_reaper();
    ask_in(&program()?, model, req, timeout())
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

// Outputs shorter than this many tokens skip the local model: reading them costs less than the model's answer and its latency.
pub const SHORT: usize = 400;

// The system message for post-processing: the result only, so the agent reads no reasoning.
const POST_SYSTEM: &str = "Do exactly what the instruction asks with the text after ---. Reply with only the result: no reasoning, no preamble, no notes.";

// Runs a command's output through the local model with an instruction, at low effort.
pub fn post(input: &str, instruction: &str) -> Result<String, String> {
    if input.trim().is_empty() {
        return Ok(input.to_string());
    }
    let tokens = crate::tokens::estimate(input);
    if tokens < SHORT {
        return Ok(format!(
            "{}\n[kept as is: ~{tokens} tokens is short enough to read directly; --llm is for larger outputs]",
            input.trim_end()
        ));
    }
    if input.len() > MAX_INPUT {
        return Err(format!(
            "{} bytes is more than the local model takes ({MAX_INPUT}); narrow the command first",
            input.len()
        ));
    }
    let user = format!("{instruction}\n\n---\n{input}");
    let req = serde_json::json!({"system": POST_SYSTEM, "user": user, "effort": "low", "temp": 0});
    ask(None, &req).map(|r| mark(input, &r))
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

    // Checks the shared worker: it starts once for many calls, `model` None reuses whatever is loaded, another model or program replaces it, a dead worker is dropped, and idle ones are reaped.
    #[test]
    fn test_llm_pool() {
        let _g = crate::testutil::env_lock();
        let t = TempDir::new();
        let starts = t.path().join("starts");
        let worker = fake(&t, &format!("echo \"$*\" >> {}\nm=qwen3.5-4b; case \"$*\" in *gemma*) m=gemma-4-12b;; esac\necho \"{{\\\"ready\\\":\\\"$m\\\"}}\"\nwhile read l; do case \"$l\" in *die*) exit 1;; *oops*) echo \"{{\\\"error\\\":\\\"bad\\\"}}\";; *) echo \"{{\\\"text\\\":\\\"$m\\\"}}\";; esac; done\n", starts.display()));
        let d = Duration::from_secs(10);
        let q = serde_json::json!({"user": "x"});
        assert_eq!(ask_in(&worker, None, &q, d).expect("first").text, "qwen3.5-4b");
        assert_eq!(ask_in(&worker, Some("qwen3.5"), &q, d).expect("prefix").text, "qwen3.5-4b");
        assert_eq!(ask_in(&worker, Some("gemma-4-12b"), &q, d).expect("switch").text, "gemma-4-12b");
        assert_eq!(ask_in(&worker, None, &q, d).expect("any").text, "gemma-4-12b");
        let log = std::fs::read_to_string(&starts).expect("starts");
        assert_eq!(
            log.lines().collect::<Vec<_>>(),
            ["worker --no-download --effort low", "worker --no-download --effort low --model gemma-4-12b"]
        );
        assert_eq!(
            ask_in(&worker, None, &serde_json::json!({"user": "oops"}), d),
            Err("local model: bad".into())
        );
        assert!(ask_in(&worker, None, &serde_json::json!({"user": "die"}), d).is_err());
        assert_eq!(
            ask_in(&worker, None, &q, d).expect("restarted").text,
            "qwen3.5-4b",
            "a dead worker is replaced"
        );
        let (t2, t3, t4) = (TempDir::new(), TempDir::new(), TempDir::new());
        let other = fake(&t2, "echo '{\"ready\":\"m\"}'\nwhile read l; do echo '{\"text\":\"other\"}'; done");
        assert_eq!(ask_in(&other, None, &q, d).expect("program").text, "other");
        assert!(!reap(Duration::from_secs(60)) && reap(Duration::ZERO) && !reap(Duration::ZERO));
        assert!(ask_in(&t.path().join("none"), None, &q, d).expect_err("none").contains("starting"));
        std::env::set_var("ZAZZLE_LLM", &other);
        assert_eq!(ask(None, &q).expect("env").text, "other");
        std::env::set_var("ZAZZLE_LLM", fake(&t4, "exit 2"));
        assert_eq!(fetch(None), Err("zazzle-llm could not fetch the default model".into()));
        let named = fake(&t3, "echo \"${3:-qwen3.5-4b}\"");
        std::env::set_var("ZAZZLE_LLM", &named);
        assert_eq!((fetch(None), fetch(Some("gemma"))), (Ok("qwen3.5-4b".into()), Ok("gemma".into())));
        std::env::set_var("ZAZZLE_LLM_IDLE", "5");
        assert_eq!(idle(), Duration::from_secs(5));
        std::env::remove_var("ZAZZLE_LLM_IDLE");
        std::env::remove_var("ZAZZLE_LLM");
        let _ = program();
        reap(Duration::ZERO);
    }

    // Checks the saving line, the cut-short note, empty output passing through and the timeout setting.
    #[test]
    fn test_llm_mark_and_post() {
        let long = "word ".repeat(400);
        let m = mark(&long, &Reply { text: "short".into(), truncated: true });
        assert!(m.starts_with("short\n[post-processed by the local model: ~") && m.contains("cut short"));
        assert_eq!(post("  \n", "x"), Ok("  \n".into()));
        let huge = "word ".repeat(MAX_INPUT / 4);
        assert!(post(&huge, "x").expect_err("big").contains("narrow the command"));
        let _g = crate::testutil::env_lock();
        std::env::set_var("ZAZZLE_LLM_TIMEOUT", "7");
        assert_eq!(timeout(), Duration::from_secs(7));
        std::env::remove_var("ZAZZLE_LLM_TIMEOUT");
        assert_eq!(timeout(), Duration::from_secs(DEFAULT_TIMEOUT));
    }

    // Checks a worker session: it waits for the ready line, answers several prompts from one process, passes errors and the cut-short flag, and reports a worker that will not start or that stalls.
    #[test]
    fn test_session() {
        let t = TempDir::new();
        let starts = t.path().join("starts");
        let worker = fake(&t, &format!("echo start >> {}\necho '{{\"ready\":\"m\"}}'\nwhile read l; do case \"$l\" in *fail*) echo '{{\"error\":\"bad prompt\"}}';; *long*) echo '{{\"text\":\" half \",\"truncated\":true}}';; *) echo '{{\"text\":\"ok\",\"truncated\":false}}';; esac; done\n", starts.display()));
        let d = Duration::from_secs(10);
        let mut s = Session::start_with(&worker, &["--effort", "low"], d).expect("start");
        assert_eq!(
            s.ask(&serde_json::json!({"user": "a"})),
            Ok(Reply { text: "ok".into(), truncated: false })
        );
        assert_eq!(
            s.ask(&serde_json::json!({"user": "long"})),
            Ok(Reply { text: "half".into(), truncated: true })
        );
        assert_eq!(
            s.ask(&serde_json::json!({"user": "fail"})).err().as_deref(),
            Some("local model: bad prompt")
        );
        assert!(s
            .ask(&serde_json::json!({"user": "x".repeat(MAX_INPUT)}))
            .expect_err("big")
            .contains("more than"));
        drop(s);
        assert_eq!(
            std::fs::read_to_string(&starts).expect("starts").lines().count(),
            1,
            "one process for every prompt"
        );
        let absent = fake(
            &t,
            "echo 'error: qwen is not downloaded: run `zazzle agent llm --model qwen` first' >&2\nexit 4\n",
        );
        let e = Session::start_with(&absent, &[], d).err().expect("absent");
        assert!(e.starts_with("local model: qwen is not downloaded"), "{e}");
        let silent = fake(&t, "exit 2\n");
        assert_eq!(
            Session::start_with(&silent, &[], d).err().as_deref(),
            Some("local model: the local model stopped")
        );
        let stall = fake(&t, "echo '{\"ready\":\"m\"}'\nsleep 30\n");
        let mut s = Session::start_with(&stall, &[], Duration::from_millis(300)).expect("ready");
        assert!(s.ask(&serde_json::json!({"user": "a"})).expect_err("stall").contains("took longer than"));
        drop(s);
        let junk = fake(&t, "echo not-json\n");
        assert!(Session::start_with(&junk, &[], d).err().expect("junk").contains("did not start"));
        let dead = fake(&t, "echo '{\"ready\":\"m\"}'\nexit 0\n");
        let mut s = Session::start_with(&dead, &[], d).expect("ready");
        std::thread::sleep(Duration::from_millis(100));
        assert!(s.ask(&serde_json::json!({"user": "a"})).expect_err("dead").starts_with("local model:"));
        drop(s);
        let bad = fake(&t, "echo '{\"ready\":\"m\"}'\nread l; echo garbage\n");
        let mut s = Session::start_with(&bad, &[], d).expect("ready");
        assert!(s.ask(&serde_json::json!({"user": "a"})).expect_err("garbage").contains("unreadable reply"));
        drop(s);
        assert!(Session::start_with(&t.path().join("none"), &[], d)
            .err()
            .expect("none")
            .contains("starting"));
        assert_eq!(last_reason("x\nerror: y\n\n"), Some("y".into()));
    }
}
