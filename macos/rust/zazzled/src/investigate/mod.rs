// `zazzle investigate QUESTION --path P.. --symbol S..`: the local model resolves a repository question with bounded read-only lookups inside the scope, and its answer comes back as findings whose `path:line` citations zazzle has checked and quoted, plus what it could not establish.
mod cite;

use crate::cmd::{Ctx, Output};
use crate::llm::Reply;
use std::path::PathBuf;

pub const USAGE: &str = "usage: investigate QUESTION.. [--path P].. [--symbol S].. [--steps N]";
const DEFAULT_STEPS: usize = 6;
const MAX_STEPS: usize = 12;
const PER_TURN: usize = 3;
const LOOKUP_BUDGET: &str = "1500";
const LOOKUP_MAX: &str = "15";
const SEEN_TOKENS: usize = 12_000;
const REPLY_TOKENS: &str = "1500";

// The system message: what the model may do each turn and how to answer.
const SYSTEM: &str = "You investigate a code repository to answer one question, using read-only lookups. Each turn, reply with either lookups or the answer, nothing else.
Lookups: up to 3 lines, each one of
READ path            (or READ path:A-B for lines A to B)
REFS name            where a symbol is defined and used
OUTLINE path         the symbols of a file with their line ranges
FIND text            lines containing the text
Answer: a line ANSWER, then one line per fact:
FINDING path:A-B one sentence that the cited lines show
UNKNOWN something the question needs that you could not establish
Cite only lines you have seen, with the line numbers shown to you. Prefer a few exact findings over many vague ones. Stay inside the scope.";

// Something an investigation asks: the local model through zazzle-llm, or a script in tests.
pub trait Local {
    // Answers one turn: the system message and the transcript so far.
    fn ask(&self, system: &str, transcript: &str) -> Result<Reply, String>;
}

// The user's local model, asked through zazzle-llm without downloading anything.
pub struct Model;

impl Local for Model {
    // Asks at low effort and temperature 0, with the transcript on standard input.
    fn ask(&self, system: &str, transcript: &str) -> Result<Reply, String> {
        let flags = ["--effort", "low", "--temp", "0", "--max-tokens", REPLY_TOKENS, "--system", system];
        crate::llm::ask("Reply with your next lookups, or ANSWER.", transcript, &flags)
    }
}

// The parsed command line.
struct Plan {
    question: String,
    scope: Vec<String>,
    symbols: Vec<String>,
    steps: usize,
}

// Parses the question words and the flags.
fn parse(argv: &[String]) -> Result<Plan, String> {
    let mut p =
        Plan { question: String::new(), scope: Vec::new(), symbols: Vec::new(), steps: DEFAULT_STEPS };
    let (mut words, mut it) = (Vec::new(), argv.iter());
    while let Some(a) = it.next() {
        let mut value = || it.next().cloned().ok_or_else(|| format!("{a} needs a value; {USAGE}"));
        match a.as_str() {
            "--path" | "-p" => p.scope.push(value()?),
            "--symbol" | "-s" => p.symbols.push(value()?),
            "--steps" => {
                let v = value()?;
                p.steps = v
                    .parse::<usize>()
                    .ok()
                    .filter(|n| (1..=MAX_STEPS).contains(n))
                    .ok_or_else(|| format!("--steps takes 1 to {MAX_STEPS}, got '{v}'"))?;
            }
            f if f.starts_with("--") => return Err(format!("unknown flag {f}; {USAGE}")),
            w => words.push(w),
        }
    }
    p.question = words.join(" ");
    if p.question.trim().is_empty() {
        return Err(USAGE.into());
    }
    if p.scope.is_empty() {
        p.scope.push(".".into());
    }
    Ok(p)
}

// One lookup the model asked for.
#[derive(Debug, PartialEq)]
enum Lookup {
    Read(String),
    Refs(String),
    Outline(String),
    Find(String),
}

impl Lookup {
    // Shows the lookup the way the model wrote it.
    fn label(&self) -> String {
        match self {
            Lookup::Read(a) => format!("READ {a}"),
            Lookup::Refs(a) => format!("REFS {a}"),
            Lookup::Outline(a) => format!("OUTLINE {a}"),
            Lookup::Find(a) => format!("FIND {a}"),
        }
    }
}

// Reads the lookups in a reply, at most PER_TURN.
fn parse_lookups(reply: &str) -> Vec<Lookup> {
    let mut out = Vec::new();
    for l in reply.lines().map(str::trim) {
        let l = l.trim_start_matches(['-', '*', '`']).trim_end_matches('`').trim();
        let arg = |w: &str| cite::strip_word(l, w).filter(|r| !r.is_empty()).map(str::to_string);
        if let Some(found) = arg("READ")
            .map(Lookup::Read)
            .or_else(|| arg("REFS").map(Lookup::Refs))
            .or_else(|| arg("OUTLINE").map(Lookup::Outline))
            .or_else(|| arg("FIND").map(Lookup::Find))
        {
            out.push(found);
        }
    }
    out.truncate(PER_TURN);
    out
}

// What the investigation has seen and done.
struct Trail {
    seen: Vec<(String, String)>,
    rules: String,
    lookups: Vec<String>,
}

impl Trail {
    // Renders the transcript for the next turn.
    fn transcript(&self, plan: &Plan, left: usize) -> String {
        let mut t = format!("Question: {}\nScope: {}\n", plan.question, plan.scope.join(", "));
        if !plan.symbols.is_empty() {
            t.push_str(&format!("Starting symbols: {}\n", plan.symbols.join(", ")));
        }
        t.push_str(&match left {
            0 => "No lookups left: reply with ANSWER now.\n".to_string(),
            n => format!("Turns left for lookups: {n} (up to {PER_TURN} lookups each)\n"),
        });
        t.push_str("\nWhat you have seen:\n");
        if self.seen.is_empty() {
            t.push_str("(nothing yet)\n");
        }
        for (label, text) in &self.seen {
            t.push_str(&format!("\n### {label}\n{}\n", text.trim_end()));
        }
        t
    }

    // Tokens of everything seen so far.
    fn size(&self) -> usize {
        self.seen.iter().map(|(_, t)| crate::tokens::estimate(t)).sum()
    }
}

// Runs one lookup through zazzle's own commands, confined to the scope; the rules that apply to what it read go to the trail's rules, not into the evidence.
fn look(lctx: &Ctx, scope: &[PathBuf], plan: &Plan, l: &Lookup, trail: &mut Trail) -> String {
    let path_ok = |p: &str| {
        let file = p
            .rsplit_once(':')
            .filter(|(_, r)| r.chars().all(|c| c.is_ascii_digit() || c == '-'))
            .map_or(p, |(f, _)| f);
        std::fs::canonicalize(lctx.cwd.join(file)).is_ok_and(|f| cite::in_scope(&f, scope))
    };
    let args: Vec<String> = match l {
        Lookup::Read(p) | Lookup::Outline(p) if !path_ok(p) => {
            return format!(
                "refused: {p} is not an existing path inside the scope ({})",
                plan.scope.join(", ")
            )
        }
        Lookup::Read(p) => vec!["read".into(), p.clone(), "--budget".into(), LOOKUP_BUDGET.into()],
        Lookup::Outline(p) => vec!["outline".into(), p.clone()],
        Lookup::Refs(n) => ["refs", n, "-m", LOOKUP_MAX, "--"]
            .iter()
            .map(|s| s.to_string())
            .chain(plan.scope.iter().cloned())
            .collect(),
        Lookup::Find(t) => ["find", "-F", "-m", LOOKUP_MAX, "--", t]
            .iter()
            .map(|s| s.to_string())
            .chain(plan.scope.iter().cloned())
            .collect(),
    };
    let mut args = args;
    args.insert(1, "--disable-session-deduping".into());
    let o = crate::cmd::run(lctx, &args);
    trail.rules.push_str(&o.notes);
    o.text
}

// Investigates with the given local model and returns the report, with the rules for the cited and read files as notes for the caller.
pub fn investigate(ctx: &Ctx, argv: &[String], local: &dyn Local) -> Result<Output, String> {
    let plan = parse(argv)?;
    let mut scope = Vec::new();
    for s in &plan.scope {
        scope.push(
            std::fs::canonicalize(ctx.cwd.join(s))
                .map_err(|_| format!("--path {s}: no such file or folder"))?,
        );
    }
    let base = if ctx.session.is_empty() { "investigate" } else { ctx.session.as_str() };
    let lctx = Ctx {
        engine: ctx.engine,
        cwd: ctx.cwd.clone(),
        session: format!("{base}>local"),
        roots: ctx.roots.clone(),
        stdin: std::cell::RefCell::new(None),
        caller: std::cell::RefCell::new(None),
    };
    let mut trail = Trail { seen: Vec::new(), rules: String::new(), lookups: Vec::new() };
    for s in &plan.symbols {
        let l = Lookup::Refs(s.clone());
        let text = look(&lctx, &scope, &plan, &l, &mut trail);
        trail.lookups.push(l.label());
        trail.seen.push((l.label(), text));
    }
    let (mut answer, mut stop, mut cut, mut turns, mut strayed) = (None, String::new(), false, 0, false);
    for step in 1..=plan.steps {
        turns = step;
        let left = if step == plan.steps || trail.size() > SEEN_TOKENS { 0 } else { plan.steps - step };
        let system = match trail.rules.trim() {
            "" => SYSTEM.to_string(),
            r => format!(
                "{SYSTEM}\n\nRules that apply to these files (follow them; they are not evidence):\n{r}"
            ),
        };
        let reply = match local.ask(&system, &trail.transcript(&plan, left)) {
            Ok(r) => r,
            Err(e) if trail.seen.is_empty() => return Err(e),
            Err(e) => {
                stop = format!("the local model failed: {e}");
                break;
            }
        };
        cut |= reply.truncated;
        if let Some(a) = cite::parse_answer(&reply.text) {
            answer = Some(a);
            break;
        }
        let lookups = parse_lookups(&reply.text);
        if lookups.is_empty() && left > 0 && !strayed {
            strayed = true;
            let note = format!("Your last reply had neither lookups nor ANSWER (it began: {}). Reply only with lookup lines or with ANSWER and FINDING/UNKNOWN lines.", began(&reply.text));
            trail.seen.push(("NOTE".into(), note));
            continue;
        }
        if left == 0 || lookups.is_empty() {
            stop = match left {
                0 => format!(
                    "it gave no ANSWER when its lookups ran out (its reply began: {})",
                    began(&reply.text)
                ),
                _ => format!(
                    "it replied twice with neither lookups nor an ANSWER (the last began: {})",
                    began(&reply.text)
                ),
            };
            break;
        }
        for l in lookups {
            let text = look(&lctx, &scope, &plan, &l, &mut trail);
            trail.lookups.push(l.label());
            trail.seen.push((l.label(), text));
        }
    }
    Ok(report(ctx, &plan, &scope, &trail, answer, Status { stop, cut, turns }))
}

// Quotes the start of a reply on one line, for a note to the model or the report.
fn began(reply: &str) -> String {
    let one: String = reply.split_whitespace().collect::<Vec<_>>().join(" ");
    let cut: String = one.chars().take(160).collect();
    match (one.is_empty(), cut.len() < one.len()) {
        (true, _) => "an empty reply".into(),
        (false, true) => format!("\"{cut}…\""),
        (false, false) => format!("\"{cut}\""),
    }
}

// How the run ended: why it stopped early, whether a reply was cut short, and the turns used.
struct Status {
    stop: String,
    cut: bool,
    turns: usize,
}

// Formats the checked findings, rejected citations and unknowns, with the caller's rules for the files involved as notes.
fn report(
    ctx: &Ctx,
    plan: &Plan,
    scope: &[PathBuf],
    trail: &Trail,
    answer: Option<cite::Answer>,
    st: Status,
) -> Output {
    let answer = answer.unwrap_or_default();
    let (mut found, mut rejected, mut files) = (String::new(), String::new(), Vec::new());
    let mut n = 0;
    for c in &answer.findings {
        let cited = format!(
            "{}:{}{}",
            c.path,
            c.start,
            if c.end > c.start { format!("-{}", c.end) } else { String::new() }
        );
        match cite::check(&ctx.cwd, scope, c) {
            cite::Checked::Verified { excerpt } => {
                n += 1;
                let unread = if trail.seen.iter().any(|(l, t)| l.contains(&c.path) || t.contains(&c.path)) {
                    ""
                } else {
                    " (not among the lookups)"
                };
                found.push_str(&format!("{n}. {}\n   {cited}{unread}\n{excerpt}", c.text));
                files.push(ctx.cwd.join(&c.path));
            }
            cite::Checked::Rejected(why) => rejected.push_str(&format!("- {cited} {}: {why}\n", c.text)),
        }
    }
    let mut out = format!("investigate: {}\nscope: {}\n", plan.question, plan.scope.join(", "));
    out.push_str(&format!(
        "findings ({n} verified):\n{}",
        if found.is_empty() { "(none)\n".into() } else { found }
    ));
    if !rejected.is_empty() {
        out.push_str(&format!("rejected citations:\n{rejected}"));
    }
    let mut unknowns = answer.unknowns.clone();
    if !st.stop.is_empty() {
        unknowns.push(format!("the investigation stopped early: {}", st.stop));
    }
    if !unknowns.is_empty() {
        out.push_str(&format!(
            "unknowns:\n{}",
            unknowns.iter().map(|u| format!("- {u}\n")).collect::<String>()
        ));
    }
    let state = match (st.stop.is_empty(), st.cut) {
        (true, false) => "complete".to_string(),
        (true, true) => "INCOMPLETE: a reply was cut short by the token limit".to_string(),
        (false, _) => format!("INCOMPLETE: {}", st.stop),
    };
    out.push_str(&format!(
        "-- local model, {} of {} turns, {} lookups ({}); {state}. Excerpts are read from the files; claims are the model's.",
        st.turns,
        plan.steps,
        trail.lookups.len(),
        trail.lookups.join("; ")
    ));
    let mut o = Output::plain(out);
    if !ctx.session.is_empty() {
        let has = |k: &str| ctx.engine.sessions.has_note(&ctx.session, k);
        let mark = |k: &str| ctx.engine.sessions.mark_note(&ctx.session, k);
        let l = crate::rules::Ledger { has: &has, mark: &mark };
        let purposes = ctx.engine.sessions.purposes(&ctx.session);
        o.notes = crate::nested::for_paths(&ctx.cwd, &files, &purposes, &l);
    }
    o
}

// The MCP tool for investigate.
pub(crate) const TOOLS: &[crate::mcp::Tool] = &[crate::mcp::Tool {
    name: "zazzle_investigate",
    description: "Hand a repository question needing several files to the user's local model: it follows read-only lookups inside paths from the symbols, and returns findings whose path:line citations zazzle checked and quoted, plus unknowns. For one known symbol or a short lookup, use zazzle_read or zazzle_refs directly.",
    schema,
    map: tool_args,
}];

// Schema for zazzle_investigate.
fn schema() -> serde_json::Value {
    crate::mcp::object(
        serde_json::json!({
            "question": {"type": "string"},
            "paths": {"type": "array", "items": {"type": "string"}, "description": "Folders or files to stay inside (default: the working directory)"},
            "symbols": {"type": "array", "items": {"type": "string"}, "description": "Names to start from"},
            "steps": {"type": "integer", "description": "Model turns, 1 to 12 (default 6)"}
        }),
        &["question"],
    )
}

// Turns zazzle_investigate arguments into a command line.
fn tool_args(v: &serde_json::Value) -> Result<Vec<String>, String> {
    let mut a = vec!["investigate".to_string()];
    for p in crate::mcp::get_list(v, "paths")? {
        a.extend(["--path".to_string(), p]);
    }
    for s in crate::mcp::get_list(v, "symbols")? {
        a.extend(["--symbol".to_string(), s]);
    }
    crate::mcp::push_num(&mut a, v, "steps", "--steps")?;
    a.push(crate::mcp::need_str(v, "question")?);
    Ok(a)
}

// Runs `investigate` with the user's local model.
pub fn run(ctx: &Ctx, argv: &[String]) -> Result<Output, String> {
    investigate(ctx, argv, &Model)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests;
