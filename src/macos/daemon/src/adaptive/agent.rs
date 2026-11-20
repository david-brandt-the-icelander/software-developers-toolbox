// The agents the adaptive mode routes work to: local models through ai-agent and the hosted coding agents `claude` and `codex` run as read-only command lines, each at an effort, ordered cheapest first into a ladder.
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

// Which program answers for an agent.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    Local,
    Claude,
    Codex,
}

// One agent at one effort: what the planner assigns a part to and what escalation climbs; an empty local model is
// whichever model is loaded, else the default.
#[derive(Debug, Clone, PartialEq)]
pub struct Rung {
    pub kind: Kind,
    pub model: String,
    pub effort: String,
}

// What a hosted agent said: its final text, and the output, input and cached tokens, rounds and cost its command line reported; `counted` is false when it reported no token counts, which are then unknown, not zero.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Said {
    pub text: String,
    pub tokens: usize,
    pub input: usize,
    pub cached: usize,
    pub rounds: usize,
    pub cost: Option<f64>,
    pub counted: bool,
}

impl Kind {
    // The efforts the agent takes, lowest first.
    pub fn efforts(self) -> &'static [&'static str] {
        match self {
            Kind::Local => &["low", "medium", "high"],
            Kind::Claude => &["low", "medium", "high", "xhigh", "max"],
            Kind::Codex => &["minimal", "low", "medium", "high", "xhigh"],
        }
    }

    // The effort used when none is named.
    pub fn default_effort(self) -> &'static str {
        match self {
            Kind::Local => "low",
            _ => "medium",
        }
    }
}

impl Rung {
    // Parses `[local:|claude:|codex:]MODEL[@EFFORT]`; `effort` (from a flag) wins over `@EFFORT`, and the kind's default applies when neither is given.
    pub fn parse(spec: &str, effort: Option<&str>) -> Result<Rung, String> {
        let (body, at) = match spec.rsplit_once('@') {
            Some((b, e)) => (b, Some(e)),
            None => (spec, None),
        };
        let (kind, model) = match body.split_once(':') {
            Some(("claude", m)) => (Kind::Claude, m),
            Some(("codex", m)) => (Kind::Codex, m),
            Some(("local", m)) => (Kind::Local, m),
            Some((k, _)) => {
                return Err(format!("unknown agent kind `{k}:` in `{spec}` (local:, claude: or codex:)"))
            }
            None => (Kind::Local, body),
        };
        if kind != Kind::Local && model.is_empty() {
            return Err(format!("`{spec}` names no model"));
        }
        let effort = effort.or(at).unwrap_or(kind.default_effort());
        if !kind.efforts().contains(&effort) {
            return Err(format!("`{spec}` takes effort {}, not `{effort}`", kind.efforts().join(", ")));
        }
        Ok(Rung { kind, model: model.to_string(), effort: effort.to_string() })
    }

    // Names the rung for reports: `qwen3.5-4b low`, `claude:sonnet high`, `local default low`.
    pub fn label(&self) -> String {
        let name = match (self.kind, self.model.as_str()) {
            (Kind::Local, "") => "local default",
            (Kind::Local, m) => m,
            (Kind::Claude, m) => return format!("claude:{m} {}", self.effort),
            (Kind::Codex, m) => return format!("codex:{m} {}", self.effort),
        };
        format!("{name} {}", self.effort)
    }

    // The agent alone, for a JSON report: `qwen3.5-4b`, `claude:sonnet`.
    pub fn agent(&self) -> String {
        self.label().rsplit_once(' ').map(|(a, _)| a.to_string()).unwrap_or_default()
    }

    // Tells whether the agent reads files itself rather than through zazzle's lookups.
    pub fn hosted(&self) -> bool {
        self.kind != Kind::Local
    }
}

// Builds the ladder, cheapest first, from agent specs in the order given: a spec with an effort is one rung, one
// without is two, at low and then high.
pub fn ladder(specs: &[String]) -> Result<Vec<Rung>, String> {
    let mut out = Vec::new();
    for spec in specs.iter().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        match spec.contains('@') {
            true => out.push(Rung::parse(spec, None)?),
            false => {
                out.push(Rung::parse(spec, Some("low"))?);
                out.push(Rung::parse(spec, Some("high"))?);
            }
        }
    }
    Ok(out)
}

// Reads `ai-agent status --json`: the installed models that passed their check, smallest first.
pub fn installed_models(status: &serde_json::Value) -> Vec<String> {
    let mut models: Vec<(u64, String)> = status["models"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| matches!(m["check"].as_str(), Some("size" | "sha256")))
        .filter_map(|m| Some((m["bytes"].as_u64()?, m["name"].as_str()?.to_string())))
        .collect();
    models.sort();
    models.into_iter().map(|(_, n)| n).collect()
}

// Finds a hosted agent's program: $ZAZZLE_CLAUDE or $ZAZZLE_CODEX, else on PATH or in the usual install folders.
pub fn program(kind: Kind) -> Result<PathBuf, String> {
    let (var, name) = match kind {
        Kind::Claude => ("ZAZZLE_CLAUDE", "claude"),
        _ => ("ZAZZLE_CODEX", "codex"),
    };
    if let Some(p) = std::env::var_os(var) {
        return Ok(PathBuf::from(p));
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let mut dirs: Vec<PathBuf> =
        std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    dirs.extend(["/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from));
    dirs.extend([".local/bin", ".claude/local", ".npm-global/bin"].map(|d| home.join(d)));
    dirs.into_iter()
        .map(|d| d.join(name))
        .find(|p| p.is_file())
        .ok_or_else(|| format!("{name} is not installed (not on PATH; set ${var} to its path)"))
}

// The command line that asks a hosted agent one question, the prompt on standard input: read-only, or with `write` allowed to edit files in its working directory (never to run commands); codex writes its last message to `out`.
pub fn command(r: &Rung, program: &Path, out: &Path, write: bool) -> Command {
    let mut c = Command::new(program);
    let (tools, sandbox) = match write {
        true => ("Read,Grep,Glob,Edit,Write", "workspace-write"),
        false => ("Read,Grep,Glob", "read-only"),
    };
    match r.kind {
        Kind::Claude => c
            .args(["-p", "--model", &r.model, "--effort", &r.effort, "--permission-mode", "dontAsk"])
            .args(["--allowedTools", tools, "--output-format", "json"]),
        _ => c
            .args(["exec", "--model", &r.model, "-c", &format!("model_reasoning_effort=\"{}\"", r.effort)])
            .args(["--sandbox", sandbox, "--skip-git-repo-check", "--json", "--output-last-message"])
            .arg(out)
            .arg("-"),
    };
    c
}

// Reads what a hosted agent printed: claude's JSON result, or codex's JSON event lines with its last message from `out_text`.
pub fn said(kind: Kind, stdout: &str, out_text: &str) -> Result<Said, String> {
    if kind == Kind::Claude {
        let v: serde_json::Value =
            serde_json::from_str(stdout.trim()).map_err(|e| format!("claude printed no JSON result: {e}"))?;
        let text = v["result"].as_str().unwrap_or_default().to_string();
        if v["is_error"] == true {
            return Err(format!("claude failed: {}", one_line(&text)));
        }
        let n = |k: &str| v["usage"][k].as_u64().unwrap_or_default() as usize;
        let input = n("input_tokens") + n("cache_read_input_tokens") + n("cache_creation_input_tokens");
        let rounds = v["num_turns"].as_u64().unwrap_or(1) as usize;
        let cost = v["total_cost_usd"].as_f64();
        return Ok(Said {
            text,
            tokens: n("output_tokens"),
            input,
            cached: n("cache_read_input_tokens"),
            rounds,
            cost,
            counted: v["usage"].is_object(),
        });
    }
    let (mut said, mut last) = (Said::default(), String::new());
    for v in stdout.lines().filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok()) {
        let n = |k: &str| v["usage"][k].as_u64().unwrap_or_default() as usize;
        said.tokens += n("output_tokens");
        said.input += n("input_tokens");
        said.cached += n("cached_input_tokens");
        said.counted |= v["usage"].is_object();
        said.rounds += usize::from(v["type"] == "turn.completed");
        if v["item"]["type"] == "agent_message" {
            last = v["item"]["text"].as_str().unwrap_or_default().to_string();
        }
    }
    said.text = if out_text.trim().is_empty() { last } else { out_text.to_string() };
    match said.text.trim().is_empty() {
        true => Err("codex gave no final message".into()),
        false => Ok(said),
    }
}

// Runs a hosted agent in `cwd` with the prompt on its standard input, stopping it at the deadline; `write` lets it edit files there.
pub fn ask_hosted(
    r: &Rung,
    program: &Path,
    prompt: &str,
    cwd: &Path,
    deadline: Instant,
    write: bool,
) -> Result<Said, String> {
    let dir = std::env::temp_dir().join(format!(
        "zazzle-adaptive-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let out = dir.join("last-message");
    let _ = std::fs::remove_file(&out);
    let mut cmd = command(r, program, &out, write);
    cmd.current_dir(cwd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = retry_busy(|| cmd.spawn()).map_err(|e| format!("starting {}: {e}", program.display()))?;
    let mut stdin = child.stdin.take().expect("piped");
    let input = prompt.to_string();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(input.as_bytes());
    });
    let drain = |mut r: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut s = String::new();
            let _ = r.read_to_string(&mut s);
            s
        })
    };
    let (so, se) = (
        drain(Box::new(child.stdout.take().expect("piped"))),
        drain(Box::new(child.stderr.take().expect("piped"))),
    );
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) if crate::budget::over(deadline) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{} while {} worked", crate::budget::why_over(), r.label()));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => return Err(format!("waiting for {}: {e}", r.label())),
        }
    };
    let _ = writer.join();
    let (stdout, stderr) = (so.join().unwrap_or_default(), se.join().unwrap_or_default());
    let out_text = std::fs::read_to_string(&out).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&dir);
    if !status.success() {
        let last = stderr.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("no message");
        return Err(format!("{} failed ({status}): {}", r.label(), one_line(last)));
    }
    said(r.kind, &stdout, &out_text)
}

// Runs a spawn again, up to ten times 20 ms apart, while it fails because the program file is still open for writing elsewhere (ETXTBSY, as right after a program is installed or written).
pub fn retry_busy<T>(mut f: impl FnMut() -> std::io::Result<T>) -> std::io::Result<T> {
    let mut tries = 0;
    loop {
        match f() {
            Err(e) if e.raw_os_error() == Some(26) && tries < 10 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(20));
            }
            r => return r,
        }
    }
}

// Shortens text to one line of at most 200 characters.
pub fn one_line(s: &str) -> String {
    let one = s.split_whitespace().collect::<Vec<_>>().join(" ");
    match one.chars().count() > 200 {
        true => format!("{}…", one.chars().take(200).collect::<String>()),
        false => one,
    }
}
