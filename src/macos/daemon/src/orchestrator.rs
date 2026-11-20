// The orchestrator: the one agent, by default the local model, that does zazzle's own housekeeping (planning, combining, summaries, commit and docs writing, lint fixes, test drafts, merge resolution, CI triage, review briefs, skill ranking, `--llm` post-processing), set by flags, the flow policy or the environment to an external model and effort, with each call recorded apart from the execution agents'.
use crate::adaptive::agent::{self, Rung};
use crate::adaptive::record;
use serde_json::json;
use std::time::{Duration, Instant};

// The environment variables that name the orchestrator and its effort for every command that has no flag.
pub const ENV_AGENT: &str = "ZAZZLE_ORCHESTRATOR";
pub const ENV_EFFORT: &str = "ZAZZLE_ORCHESTRATOR_EFFORT";

// What the orchestrator answered: the text, whether the local token limit cut it short, its label (None for the local default), and what a hosted agent reported.
#[derive(Debug, Default, PartialEq)]
pub struct Answer {
    pub text: String,
    pub truncated: bool,
    pub who: Option<String>,
    pub input: usize,
    pub cached: usize,
    pub tokens: usize,
    pub cost: Option<f64>,
}

// Builds the orchestrator from a spec (`[local:|claude:|codex:]MODEL[@EFFORT]`) and an effort that wins over `@EFFORT`; none without a spec.
pub fn pick(spec: Option<&str>, effort: Option<&str>) -> Result<Option<Rung>, String> {
    match spec.map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => Rung::parse(s, effort).map(Some),
        None if effort.is_some() => Err("an orchestrator effort needs an orchestrator MODEL".into()),
        None => Ok(None),
    }
}

// An environment variable's value when it is set and not blank; tests may override it for their own thread.
fn var(name: &str) -> Option<String> {
    #[cfg(test)]
    if let Some(v) =
        tests::OVERRIDE.with(|o| o.borrow().iter().find(|(k, _)| k == name).map(|(_, v)| v.clone()))
    {
        return v;
    }
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

// The orchestrator the environment names, if any.
pub fn from_env() -> Result<Option<Rung>, String> {
    pick(var(ENV_AGENT).as_deref(), var(ENV_EFFORT).as_deref())
}

// Adds `--orchestrator` and `--orchestrator-effort` from the environment to a command line that names neither, for the commands that take them.
pub fn with_env(argv: &[String]) -> Vec<String> {
    let mut out = argv.to_vec();
    for (flag, var_of) in [("--orchestrator", ENV_AGENT), ("--orchestrator-effort", ENV_EFFORT)] {
        let given = argv.iter().any(|a| a == flag);
        match var(var_of) {
            Some(v) if !given => out.extend([flag.to_string(), v]),
            _ => {}
        }
    }
    out
}

// Asks the orchestrator one question: the real one when the environment names it (a hosted agent in an empty folder, with the reply form in words, or a local model, with the grammar), else the local default at low effort as before; records the call.
pub fn ask(
    kind: &str,
    system: &str,
    user: &str,
    form: Option<(&str, &str)>,
    wait: Duration,
) -> Result<Answer, String> {
    let (started, deadline) = (Instant::now(), Instant::now() + wait);
    let orch = from_env();
    let res = match &orch {
        Ok(r) => ask_with(r.as_ref(), system, user, form, deadline),
        Err(e) => Err(e.clone()),
    };
    let r = orch.ok().flatten();
    let label = r.as_ref().map(Rung::label);
    let a = res.as_ref().ok();
    record::write(&json!({
        "v": 1,
        "kind": kind,
        "ts_ms": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis()),
        "agent": label.clone().unwrap_or_else(|| "local default".into()),
        "orchestrator": label.is_some(),
        "hosted": r.as_ref().is_some_and(Rung::hosted),
        "sent_tokens": crate::tokens::estimate(&format!("{system}{user}")),
        "input_tokens": a.map_or(0, |a| a.input),
        "cached_tokens": a.map_or(0, |a| a.cached),
        "output_tokens": a.map_or(0, |a| a.tokens),
        "cost_usd": a.and_then(|a| a.cost),
        "seconds": (started.elapsed().as_secs_f64() * 10.0).round() / 10.0,
        "ok": res.is_ok(),
    }));
    res
}

// Asks a given orchestrator (None: the local default model at low effort).
fn ask_with(
    r: Option<&Rung>,
    system: &str,
    user: &str,
    form: Option<(&str, &str)>,
    deadline: Instant,
) -> Result<Answer, String> {
    let who = r.map(|r| r.label());
    if let Some(r) = r.filter(|r| r.hosted()) {
        let hint = form.map(|(_, h)| format!("\n\n{h}")).unwrap_or_default();
        let prompt = format!("{system}\n\n{user}{hint}");
        let dir = std::env::temp_dir().join(format!("zazzle-orchestrator-{}", std::process::id()));
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let said = agent::ask_hosted(r, &agent::program(r.kind)?, &prompt, &dir, deadline, false);
        let _ = std::fs::remove_dir_all(&dir);
        let s = said?;
        return Ok(Answer {
            text: s.text,
            truncated: false,
            who,
            input: s.input,
            cached: s.cached,
            tokens: s.tokens,
            cost: s.cost,
        });
    }
    let effort = r.map_or("low", |r| r.effort.as_str());
    let mut req = json!({"user": user, "effort": effort, "temp": 0});
    if !system.is_empty() {
        req["system"] = system.into();
    }
    if let Some((grammar, _)) = form {
        req["grammar"] = grammar.into();
    }
    let model = r.map(|r| r.model.as_str()).filter(|m| !m.is_empty());
    let reply = crate::llm::ask_until(model, &req, deadline)?;
    Ok(Answer {
        text: reply.text,
        truncated: reply.truncated,
        who,
        tokens: reply.tokens,
        ..Answer::default()
    })
}

// A one-line note on who answered and, for a hosted orchestrator, what it reported, for the footer of a post-processed or ranked result.
pub fn note(a: &Answer) -> String {
    let Some(who) = &a.who else { return "the local model".into() };
    let cost = a.cost.map(|c| format!(", ${c:.4}")).unwrap_or_default();
    match a.input + a.cached + a.tokens {
        0 => format!("the orchestrator {who}"),
        _ => format!(
            "the orchestrator {who} (reported {} in, {} cached, {} out{cost})",
            a.input, a.cached, a.tokens
        ),
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::testutil::{env_lock, fake_worker, TempDir};
    use std::cell::RefCell;

    thread_local! {
        pub static OVERRIDE: RefCell<Vec<(String, Option<String>)>> = const { RefCell::new(Vec::new()) };
    }

    // Sets the orchestrator variables for this thread only.
    fn named(agent: Option<&str>, effort: Option<&str>) {
        OVERRIDE.with(|o| {
            *o.borrow_mut() = vec![
                (ENV_AGENT.into(), agent.map(String::from)),
                (ENV_EFFORT.into(), effort.map(String::from)),
            ]
        });
    }

    // Writes an executable stand-in for claude that logs its arguments and prompt and prints a JSON result.
    fn claude(t: &TempDir, result: &str) -> std::path::PathBuf {
        let body = format!("#!/bin/sh\necho \"$@\" > \"$0.args\"\ncat > \"$0.prompt\"\necho '{{\"result\":\"{result}\",\"usage\":{{\"input_tokens\":30,\"cache_read_input_tokens\":20,\"output_tokens\":4}},\"total_cost_usd\":0.01}}'\n");
        let p = t.write("claude", body.as_bytes());
        std::fs::set_permissions(&p, std::os::unix::fs::PermissionsExt::from_mode(0o755)).expect("chmod");
        p
    }

    // Checks specs and efforts become an orchestrator, and an effort alone or a bad one is refused.
    #[test]
    fn test_pick() {
        assert_eq!(pick(None, None).expect("none"), None);
        assert_eq!(pick(Some(" "), None).expect("blank"), None);
        assert_eq!(
            pick(Some("claude:opus@high"), None).expect("spec").expect("some").label(),
            "claude:opus high"
        );
        assert_eq!(
            pick(Some("claude:opus@high"), Some("max")).expect("flag wins").expect("some").label(),
            "claude:opus max"
        );
        assert!(pick(None, Some("high")).unwrap_err().contains("needs an orchestrator MODEL"));
        assert!(pick(Some("claude:opus"), Some("huge")).unwrap_err().contains("takes effort"));
    }

    // Checks the environment fills in the flags a command line lacks and never overrides one it has.
    #[test]
    fn test_with_env() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        named(None, None);
        assert_eq!(with_env(&a(&["q"])), ["q"]);
        named(Some("claude:opus"), Some("high"));
        assert_eq!(
            with_env(&a(&["q"])),
            ["q", "--orchestrator", "claude:opus", "--orchestrator-effort", "high"]
        );
        assert_eq!(
            with_env(&a(&["q", "--orchestrator", "codex:gpt"])),
            ["q", "--orchestrator", "codex:gpt", "--orchestrator-effort", "high"]
        );
        named(Some("claude:opus"), None);
        assert_eq!(from_env().expect("env").expect("some").label(), "claude:opus medium");
        named(None, None);
    }

    // Checks a hosted orchestrator answers --llm post-processing and brief ranking in an empty folder with the reply form in words, and what it reported is shown and recorded; and the local default and a local orchestrator still use the worker.
    #[test]
    fn test_ask_hosted_and_local() {
        let _g = env_lock();
        let t = TempDir::new();
        let (log, program) = (t.path().join("o.jsonl"), claude(&t, "1:h 2:n"));
        std::env::set_var("ZAZZLE_CLAUDE", &program);
        std::env::set_var("ZAZZLE_ORCHESTRATION_LOG", &log);
        named(Some("claude:opus"), Some("high"));
        let long = "line of output\n".repeat(200);
        let post = crate::llm::post(&long, "Keep the errors").expect("post");
        assert!(
            post.starts_with("1:h 2:n\n[post-processed by the orchestrator claude:opus high (reported 50 in, 20 cached, 4 out, $0.0100)"),
            "{post}"
        );
        let args = std::fs::read_to_string(format!("{}.args", program.display())).expect("args");
        assert!(args.contains("--model opus") && args.contains("--effort high"), "{args}");
        let prompt = std::fs::read_to_string(format!("{}.prompt", program.display())).expect("prompt");
        assert!(prompt.contains("Keep the errors") && prompt.contains("line of output"), "{prompt}");
        let ranked = crate::brief::ask_local("Rank them", "[1] a\n[2] b", "root ::= x").expect("rank");
        assert_eq!(ranked, "1:h 2:n");
        let prompt = std::fs::read_to_string(format!("{}.prompt", program.display())).expect("prompt");
        assert!(prompt.contains("like `1:h 2:n 3:m`"), "{prompt}");
        std::env::set_var("ZAZZLE_CLAUDE", t.path().join("missing"));
        assert!(crate::llm::post(&long, "x").unwrap_err().contains("starting"));
        let lines: Vec<serde_json::Value> = std::fs::read_to_string(&log)
            .expect("log")
            .lines()
            .map(|l| serde_json::from_str(l).expect("json"))
            .collect();
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert_eq!(
            (lines[0]["kind"].as_str(), lines[0]["agent"].as_str(), lines[0]["hosted"].as_bool()),
            (Some("post"), Some("claude:opus high"), Some(true))
        );
        assert_eq!(
            (lines[0]["input_tokens"].as_u64(), lines[0]["output_tokens"].as_u64(), lines[0]["ok"].as_bool()),
            (Some(50), Some(4), Some(true))
        );
        assert_eq!((lines[1]["kind"].as_str(), lines[2]["ok"].as_bool()), (Some("brief rank"), Some(false)));
        assert!(!lines[0].to_string().contains("Keep the errors"), "no text is logged");

        let worker = fake_worker(&t, "worker", &[("CONFIDENCE", "ok")]);
        let ai = t.write("ai-agent", format!("#!/bin/sh\nexec {} \"$@\"\n", worker.display()).as_bytes());
        std::fs::set_permissions(&ai, std::os::unix::fs::PermissionsExt::from_mode(0o755)).expect("chmod");
        std::env::set_var("ZAZZLE_LLM", &ai);
        named(Some("qwen-a@high"), None);
        let local =
            ask("probe", "sys", "CONFIDENCE please", Some(("root ::= x", "form")), Duration::from_secs(20))
                .expect("local");
        assert_eq!((local.text.as_str(), local.who.as_deref()), ("ok", Some("qwen-a high")));
        named(Some("bad:spec"), None);
        assert!(ask("probe", "", "x", None, Duration::from_secs(1))
            .unwrap_err()
            .contains("unknown agent kind"));
        named(None, None);
        assert_eq!(note(&local), "the orchestrator qwen-a high");
        assert_eq!(note(&Answer::default()), "the local model");
        for k in ["ZAZZLE_CLAUDE", "ZAZZLE_ORCHESTRATION_LOG", "ZAZZLE_LLM"] {
            std::env::remove_var(k);
        }
    }
}
