// The agents every delegated call goes through and the record of each call: local models through daemon's shared worker, hosted agents through their command lines, each call logged with its phase, what was sent (by section), tokens, rounds and time, so a report can say what still went to the expensive hosted agents.
use super::agent::{self, Rung, Said};
use crate::llm::Reply;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Mutex;
use std::time::Instant;

// Tokens the local model may write for a short answer: a plan, a combination, a summary, a review or a brief.
pub const SHORT_TOKENS: usize = 700;

// Something that answers for the agents: local models through the shared worker, hosted agents through their command lines, or scripts in tests.
pub trait Backend: Sync {
    // Asks a local model (empty: the loaded one, else the default) one request by the deadline.
    fn local(&self, model: &str, req: &Value, deadline: Instant) -> Result<Reply, String>;
    // Asks a hosted agent one prompt, working in `cwd`, by the deadline; `write` lets it edit files there.
    fn hosted(
        &self,
        rung: &Rung,
        prompt: &str,
        cwd: &Path,
        deadline: Instant,
        write: bool,
    ) -> Result<Said, String>;
    // The installed local models, smallest first.
    fn installed(&self) -> Vec<String>;
    // Appends one run's record to the orchestration log; the default drops it.
    fn record(&self, _rec: &Value) {}
}

// The real agents: daemon's shared local worker, and `claude` or `codex` found on this machine.
pub struct Real;

impl Backend for Real {
    // Asks through the shared worker, never downloading a model.
    fn local(&self, model: &str, req: &Value, deadline: Instant) -> Result<Reply, String> {
        crate::llm::ask_until(Some(model).filter(|m| !m.is_empty()), req, deadline)
    }

    // Runs the hosted agent's command line, read-only unless `write`.
    fn hosted(
        &self,
        rung: &Rung,
        prompt: &str,
        cwd: &Path,
        deadline: Instant,
        write: bool,
    ) -> Result<Said, String> {
        agent::ask_hosted(rung, &agent::program(rung.kind)?, prompt, cwd, deadline, write)
    }

    // Reads `ai-agent status --json`.
    fn installed(&self) -> Vec<String> {
        let out = crate::llm::program().ok().and_then(|program| {
            std::process::Command::new(program)
                .args(["status", "--json"])
                .stdin(std::process::Stdio::null())
                .output()
                .ok()
        });
        let v = out.and_then(|o| serde_json::from_slice::<Value>(&o.stdout).ok()).unwrap_or_default();
        agent::installed_models(&v)
    }

    // Appends the record to `~/.zazzle/orchestration.jsonl` unless that log is disabled.
    fn record(&self, rec: &Value) {
        super::record::write(rec);
    }
}

// One model call: its role (its first word is the phase), the rung, whether it was hosted, the estimated tokens of
// each section sent, the input, cached and output tokens and rounds the agent reported, the cost, the seconds queued
// for the local worker and spent in all, and whether it answered.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Call {
    pub role: String,
    pub rung: String,
    pub hosted: bool,
    pub sent: Vec<(String, usize)>,
    pub input: usize,
    pub cached: usize,
    pub tokens: usize,
    pub rounds: usize,
    pub cost: Option<f64>,
    pub queued: f64,
    pub secs: f64,
    pub ok: bool,
}

impl Call {
    // The phase: the role's first word.
    pub fn phase(&self) -> &str {
        self.role.split_whitespace().next().unwrap_or_default()
    }

    // Tells whether the call is the orchestrator's work (planning, combining, summarizing) rather than an executor's.
    pub fn orchestrates(&self) -> bool {
        matches!(self.phase(), "planner" | "combine" | "summary")
    }
}

// One question for an agent: its role, the system message (local) or preamble (hosted), the prompt, the named sections of what is sent for the report, whether it may edit files, and its output limit for local models.
pub struct Ask<'a> {
    pub role: &'a str,
    pub system: &'a str,
    pub prompt: &'a str,
    pub sections: &'a [(&'a str, &'a str)],
    pub write: bool,
    pub max_tokens: usize,
}

impl<'a> Ask<'a> {
    // A read-only question with a short answer.
    pub fn new(
        role: &'a str,
        system: &'a str,
        prompt: &'a str,
        sections: &'a [(&'a str, &'a str)],
    ) -> Ask<'a> {
        Ask { role, system, prompt, sections, write: false, max_tokens: SHORT_TOKENS }
    }
}

// The calls one run made, shared by the threads that make them.
#[derive(Default)]
pub struct Log(Mutex<Vec<Call>>);

// The estimated tokens of each section, or of the whole prompt as `prompt` when no section is named.
fn sizes(sections: &[(&str, &str)], whole: &str) -> Vec<(String, usize)> {
    match sections.is_empty() {
        true => vec![("prompt".into(), crate::tokens::estimate(whole))],
        false => sections.iter().map(|(k, v)| (k.to_string(), crate::tokens::estimate(v))).collect(),
    }
}

impl Log {
    // Times one local request and records it with its sections.
    pub fn local(
        &self,
        b: &dyn Backend,
        r: &Rung,
        role: &str,
        req: &Value,
        sections: &[(&str, &str)],
        deadline: Instant,
    ) -> Result<Reply, String> {
        let t = Instant::now();
        let res = b.local(&r.model, req, deadline);
        let whole = format!(
            "{}{}",
            req["system"].as_str().unwrap_or_default(),
            req["user"].as_str().unwrap_or_default()
        );
        let (tokens, queued) = res.as_ref().map_or((0, 0.0), |x| (x.tokens, x.queued_ms as f64 / 1000.0));
        let c = Call {
            role: role.into(),
            rung: r.label(),
            sent: sizes(sections, &whole),
            tokens,
            queued,
            ok: res.is_ok(),
            ..Call::default()
        };
        self.push(c, t);
        res
    }

    // Times one hosted call and records it with what was sent and what the agent reported.
    pub fn hosted(
        &self,
        b: &dyn Backend,
        r: &Rung,
        a: &Ask,
        cwd: &Path,
        deadline: Instant,
    ) -> Result<Said, String> {
        let t = Instant::now();
        let prompt = match a.system.is_empty() {
            true => a.prompt.to_string(),
            false => format!("{}\n\n{}", a.system, a.prompt),
        };
        let res = b.hosted(r, &prompt, cwd, deadline, a.write);
        let s = res.as_ref().cloned().unwrap_or_default();
        let c = Call {
            role: a.role.into(),
            rung: r.label(),
            hosted: true,
            sent: sizes(a.sections, &prompt),
            input: s.input,
            cached: s.cached,
            tokens: s.tokens,
            rounds: s.rounds,
            cost: s.cost,
            ok: res.is_ok(),
            ..Call::default()
        };
        self.push(c, t);
        res
    }

    // Asks either kind of agent and returns its text: hosted agents get the system message ahead of the prompt.
    pub fn text(
        &self,
        b: &dyn Backend,
        r: &Rung,
        a: &Ask,
        cwd: &Path,
        deadline: Instant,
    ) -> Result<String, String> {
        match r.hosted() {
            true => self.hosted(b, r, a, cwd, deadline).map(|s| s.text),
            false => {
                let req = json!({"system": a.system, "user": a.prompt, "max_tokens": a.max_tokens, "temp": 0, "effort": r.effort});
                self.local(b, r, a.role, &req, a.sections, deadline).map(|x| x.text)
            }
        }
    }

    // Records one call with its elapsed time.
    fn push(&self, mut c: Call, t: Instant) {
        c.secs = t.elapsed().as_secs_f64();
        self.0.lock().unwrap_or_else(|p| p.into_inner()).push(c);
    }

    // The calls so far.
    pub fn calls(&self) -> Vec<Call> {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }
}

// Rounds to tenths for a report; adding 0.0 turns the -0.0 an empty float sum starts from into 0.0.
fn tenth(x: f64) -> f64 {
    (x * 10.0).round() / 10.0 + 0.0
}

// The usage of a set of calls over `secs`: totals, calls per role, and the hosted and local sides apart, each phase with what it sent to hosted agents by section.
pub fn usage(calls: &[Call], secs: f64) -> Value {
    let mut phases: Vec<String> = Vec::new();
    for c in calls {
        if !phases.iter().any(|p| p == c.phase()) {
            phases.push(c.phase().to_string());
        }
    }
    let side = |cs: Vec<&Call>| {
        let mut sent: Vec<(String, usize)> = Vec::new();
        for (k, n) in cs.iter().flat_map(|c| &c.sent) {
            match sent.iter_mut().find(|(s, _)| s == k) {
                Some((_, m)) => *m += n,
                None => sent.push((k.clone(), *n)),
            }
        }
        let costs: Vec<f64> = cs.iter().filter_map(|c| c.cost).collect();
        json!({
            "calls": cs.len(),
            "agents": cs.iter().map(|c| c.rung.clone()).fold(Vec::<String>::new(), |mut v, r| { if !v.contains(&r) { v.push(r) } v }),
            "sent_tokens": sent.iter().map(|(k, n)| json!({"section": k, "tokens": n})).collect::<Vec<_>>(),
            "input_tokens": cs.iter().map(|c| c.input).sum::<usize>(),
            "cached_tokens": cs.iter().map(|c| c.cached).sum::<usize>(),
            "output_tokens": cs.iter().map(|c| c.tokens).sum::<usize>(),
            "rounds": cs.iter().map(|c| c.rounds).sum::<usize>(),
            "cost_usd": if costs.is_empty() { Value::Null } else { json!(costs.iter().sum::<f64>()) },
            "queued_seconds": tenth(cs.iter().map(|c| c.queued).sum()),
            "seconds": tenth(cs.iter().map(|c| c.secs).sum()),
        })
    };
    let pick = |hosted: bool, phase: Option<&str>| {
        calls
            .iter()
            .filter(|c| c.hosted == hosted && phase.is_none_or(|p| c.phase() == p))
            .collect::<Vec<_>>()
    };
    let by =
        |orchestrator: bool| calls.iter().filter(|c| c.orchestrates() == orchestrator).collect::<Vec<_>>();
    json!({
        "calls": calls.len(),
        "failed_calls": calls.iter().filter(|c| !c.ok).count(),
        "by_role": phases.iter().map(|p| json!({"role": p, "calls": calls.iter().filter(|c| c.phase() == p).count()})).collect::<Vec<_>>(),
        "seconds": tenth(secs),
        "output_tokens": calls.iter().map(|c| c.tokens).sum::<usize>(),
        "cost_usd": side(pick(true, None))["cost_usd"].clone(),
        "hosted": side(pick(true, None)),
        "local": side(pick(false, None)),
        "orchestrator": side(by(true)),
        "executors": side(by(false)),
        "phases": phases.iter().map(|p| json!({"phase": p, "hosted": side(pick(true, Some(p))), "local": side(pick(false, Some(p)))})).collect::<Vec<_>>(),
    })
}

// One line of usage: calls per role, seconds, output tokens and reported cost.
pub fn usage_line(calls: &[Call], secs: f64) -> String {
    let u = usage(calls, secs);
    let roles: Vec<String> = u["by_role"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|x| format!("{} {}", x["role"].as_str().unwrap_or_default(), x["calls"]))
        .collect();
    let cost = u["cost_usd"].as_f64().map(|c| format!(", ${c:.4} reported")).unwrap_or_default();
    let split = match u["orchestrator"]["calls"].as_u64() > Some(0) {
        true => format!(
            "; orchestrator {} output tokens, executors {}",
            u["orchestrator"]["output_tokens"], u["executors"]["output_tokens"]
        ),
        false => String::new(),
    };
    format!(
        "{} calls ({}), {} s, {} output tokens{cost}{split}",
        u["calls"],
        roles.join(", "),
        u["seconds"],
        u["output_tokens"]
    )
}

// One side of the orchestrator and executors split: its agents, calls, reported input, cached and output tokens, rounds, cost and seconds.
fn side_line(name: &str, s: &Value) -> String {
    let agents: Vec<&str> = s["agents"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    let cost = s["cost_usd"].as_f64().map(|c| format!(", ${c:.4}")).unwrap_or_default();
    format!(
        "  {name}: {} call(s) to {}; {} in, {} cached, {} out, {} rounds{cost}, {} s\n",
        s["calls"],
        agents.join(", "),
        s["input_tokens"],
        s["cached_tokens"],
        s["output_tokens"],
        s["rounds"],
        s["seconds"]
    )
}

// The phase table: per phase, what went to hosted agents (agents, calls, tokens sent by section, reported input, cached and output tokens, rounds, cost) and what ran locally (calls, queue and run seconds); local calls carry no hosted token charge.
pub fn phases(calls: &[Call], secs: f64) -> String {
    let u = usage(calls, secs);
    let mut out = String::from("phases (hosted calls are charged; local ones cost only time):\n");
    let sent = |v: &Value| {
        let parts: Vec<String> = v["sent_tokens"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|s| format!("{} {}", s["section"].as_str().unwrap_or_default(), s["tokens"]))
            .collect();
        parts.join(", ")
    };
    for p in u["phases"].as_array().into_iter().flatten() {
        let (h, l) = (&p["hosted"], &p["local"]);
        let mut line = format!("  {}:", p["phase"].as_str().unwrap_or_default());
        if h["calls"].as_u64() > Some(0) {
            let agents: Vec<&str> =
                h["agents"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
            let cost = h["cost_usd"].as_f64().map(|c| format!(", ${c:.4}")).unwrap_or_default();
            line.push_str(&format!(
                " hosted {} call(s) to {} — sent ~{} tokens ({}); reported {} in, {} cached, {} out, {} rounds{cost};",
                h["calls"], agents.join(", "), h["sent_tokens"].as_array().into_iter().flatten().map(|s| s["tokens"].as_u64().unwrap_or(0)).sum::<u64>(), sent(h), h["input_tokens"], h["cached_tokens"], h["output_tokens"], h["rounds"]
            ));
        }
        if l["calls"].as_u64() > Some(0) {
            line.push_str(&format!(
                " local {} call(s), {} s queued, {} s in all, {} out;",
                l["calls"], l["queued_seconds"], l["seconds"], l["output_tokens"]
            ));
        }
        out.push_str(line.trim_end_matches(';'));
        out.push('\n');
    }
    let (h, l) = (&u["hosted"], &u["local"]);
    out.push_str(&format!(
        "  total: {} s; hosted {} calls, {} in, {} cached, {} out; local {} calls, {} s queued, {} s in all\n",
        u["seconds"], h["calls"], h["input_tokens"], h["cached_tokens"], h["output_tokens"], l["calls"], l["queued_seconds"], l["seconds"]
    ));
    if u["orchestrator"]["calls"].as_u64() > Some(0) {
        out.push_str("  tokens kept apart:\n");
        out.push_str(&side_line("orchestrator", &u["orchestrator"]));
        out.push_str(&side_line("executors", &u["executors"]));
    }
    out
}
