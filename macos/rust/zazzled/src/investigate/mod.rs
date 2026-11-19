// `zazzle investigate QUESTION --path P.. --symbol S..`: the local model resolves a repository question with bounded read-only lookups inside the scope, and its answer comes back as findings whose `path:line` citations zazzle has checked and quoted, plus what it could not establish.
mod cite;
mod seed;

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
const REPLY_TOKENS: usize = 1500;
const EVIDENCE_TOKENS: usize = 6000;

// What the model is told when it answers with nothing after lookups that found something.
const REPAIR: &str = "Your answer was empty, but the lookups above found evidence. Reply now with ANSWER, then FINDING path:A-B lines citing only lines shown above, and UNKNOWN lines for what the evidence does not show. No lookups.";

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

// The user's local model through zazzled's shared worker, which stays loaded across the turns (and after the run until idle). Nothing is downloaded.
#[derive(Default)]
pub struct Model;

impl Local for Model {
    // Asks at low effort and temperature 0, with whatever model is loaded, else the default.
    fn ask(&self, system: &str, transcript: &str) -> Result<Reply, String> {
        let user = format!("Reply with your next lookups, or ANSWER.\n\n---\n{transcript}");
        let req = serde_json::json!({"system": system, "user": user, "max_tokens": REPLY_TOKENS, "temp": 0, "effort": "low"});
        crate::llm::ask(None, &req)
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
    // Each lookup run, by its normalized key, with why it failed (None when it found something).
    done: Vec<(String, Option<String>)>,
    // Indexes into `seen` of the lookups that found something.
    evidence: Vec<usize>,
    skipped: usize,
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

// Splits `path:A-B` into the path and the range, when the part after the last colon is a line range.
fn path_part(p: &str) -> (&str, &str) {
    p.rsplit_once(':').filter(|(_, r)| r.chars().all(|c| c.is_ascii_digit() || c == '-')).unwrap_or((p, ""))
}

// The key two lookups share when they would return the same thing: the canonical file and range for reads and outlines, the words otherwise.
fn key(lctx: &Ctx, l: &Lookup) -> String {
    match l {
        Lookup::Read(p) | Lookup::Outline(p) => {
            let (file, range) = path_part(p);
            let canon = std::fs::canonicalize(lctx.cwd.join(file)).unwrap_or_else(|_| lctx.cwd.join(file));
            let kind = if matches!(l, Lookup::Read(_)) { "read" } else { "outline" };
            format!("{kind} {}:{range}", canon.display())
        }
        _ => l.label().split_whitespace().collect::<Vec<_>>().join(" "),
    }
}

// Tells why a lookup's output found nothing (a refusal, an error, no references or no matches), or None when it found something.
fn failure(text: &str, status: u8) -> Option<String> {
    let first =
        text.lines().map(str::trim).find(|l| !(l.starts_with("[r:") && l.ends_with(']'))).unwrap_or("");
    let empty = first.starts_with("refused:")
        || first.starts_with("no references to ")
        || first.starts_with("no matches in ")
        || first.is_empty();
    (status != 0 || empty).then(|| first.chars().take(160).collect())
}

// Runs a lookup unless the same one already ran: records it and its output, and returns whether it found something; a repeat returns the reason it is not run again.
fn step(lctx: &Ctx, scope: &[PathBuf], plan: &Plan, l: &Lookup, trail: &mut Trail) -> Result<bool, String> {
    let k = key(lctx, l);
    if let Some((_, why)) = trail.done.iter().find(|(d, _)| *d == k) {
        trail.skipped += 1;
        return Err(match why {
            None => format!("{} (its output is already above)", l.label()),
            Some(w) => format!("{} (it already failed: {w})", l.label()),
        });
    }
    let (text, status) = look(lctx, scope, plan, l, trail);
    let why = failure(&text, status);
    let ok = why.is_none();
    trail.done.push((k, why));
    trail.lookups.push(l.label());
    if ok {
        trail.evidence.push(trail.seen.len());
    }
    trail.seen.push((l.label(), text));
    Ok(ok)
}

// Runs one lookup through zazzle's own commands, confined to the scope, returning its text and status; the rules that apply to what it read go to the trail's rules, not into the evidence.
fn look(lctx: &Ctx, scope: &[PathBuf], plan: &Plan, l: &Lookup, trail: &mut Trail) -> (String, u8) {
    let path_ok = |p: &str| {
        std::fs::canonicalize(lctx.cwd.join(path_part(p).0)).is_ok_and(|f| cite::in_scope(&f, scope))
    };
    let args: Vec<String> = match l {
        Lookup::Read(p) | Lookup::Outline(p) if !path_ok(p) => {
            return (
                format!("refused: {p} is not an existing path inside the scope ({})", plan.scope.join(", ")),
                0,
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
    (o.text, o.status)
}

// Resolves the starting symbols before any inference: their refs inside the scope are the first evidence; a symbol that is not there gets the real files and symbols in scope that resemble it. Returns the misses with their candidates.
fn seeds(lctx: &Ctx, scope: &[PathBuf], plan: &Plan, trail: &mut Trail) -> Vec<(String, Vec<String>)> {
    let mut missed = Vec::new();
    for s in &plan.symbols {
        if step(lctx, scope, plan, &Lookup::Refs(s.clone()), trail) == Ok(false) {
            let c = seed::candidates(lctx.engine, &lctx.cwd, scope, s);
            let text = match c.is_empty() {
                true => format!("{s} is not inside the scope, and nothing there resembles it."),
                false => format!(
                    "{s} is not inside the scope. Real files and symbols there that resemble it:\n{}",
                    c.join("\n")
                ),
            };
            trail.seen.push((format!("SEED {s}"), text));
            missed.push((s.clone(), c));
        }
    }
    missed
}

// The report when no starting symbol exists in the scope and nothing resembles any of them: the model is not asked.
fn no_evidence(plan: &Plan) -> Output {
    Output::plain(format!(
        "investigate: {}\nscope: {}\nno evidence: none of the starting symbols ({}) occurs inside the scope, and no file or symbol there resembles them.\n-- the local model was not asked. Check the names (zazzle find-symbol NAME, zazzle refs NAME) or widen --path.",
        plan.question,
        plan.scope.join(", "),
        plan.symbols.join(", ")
    ))
}

// Tells whether a reply holds no answer at all: empty, or an ANSWER with no findings and no unknowns.
fn empty(r: &Reply) -> bool {
    match cite::parse_answer(&r.text) {
        Some(a) => a.findings.is_empty() && a.unknowns.is_empty(),
        None => r.text.trim().is_empty(),
    }
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
    let mut trail = Trail {
        seen: Vec::new(),
        rules: String::new(),
        lookups: Vec::new(),
        done: Vec::new(),
        evidence: Vec::new(),
        skipped: 0,
    };
    let missed = seeds(&lctx, &scope, &plan, &mut trail);
    if !plan.symbols.is_empty()
        && missed.len() == plan.symbols.len()
        && missed.iter().all(|(_, c)| c.is_empty())
    {
        return Ok(no_evidence(&plan));
    }
    let mut st = Status { stop: String::new(), cut: false, turns: 0, partial: false, missed };
    let (mut answer, mut strayed, mut stalled) = (None, false, false);
    for step_no in 1..=plan.steps {
        st.turns = step_no;
        let left = if step_no == plan.steps || trail.size() > SEEN_TOKENS { 0 } else { plan.steps - step_no };
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
                st.stop = format!("the local model failed: {e}");
                break;
            }
        };
        st.cut |= reply.truncated;
        if empty(&reply) && !trail.evidence.is_empty() {
            trail.seen.push(("NOTE".into(), REPAIR.into()));
            st.turns += 1;
            match local.ask(&system, &trail.transcript(&plan, 0)) {
                Ok(r) if !empty(&r) && cite::parse_answer(&r.text).is_some() => {
                    st.cut |= r.truncated;
                    answer = cite::parse_answer(&r.text);
                }
                Ok(r) => {
                    st.partial = true;
                    st.stop = format!(
                        "the local model gave no answer after {} lookups that found evidence, and none after one repair turn (it began: {})",
                        trail.evidence.len(),
                        began(&r.text)
                    );
                }
                Err(e) => {
                    st.partial = true;
                    st.stop =
                        format!("the local model gave an empty answer, and the repair turn failed: {e}");
                }
            }
            break;
        }
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
            st.stop = match left {
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
        let mut repeats = Vec::new();
        let mut ran = 0;
        for l in lookups {
            match step(&lctx, &scope, &plan, &l, &mut trail) {
                Ok(_) => ran += 1,
                Err(why) => repeats.push(why),
            }
        }
        if !repeats.is_empty() {
            let note = format!("Not run again: {}. Ask for something new, or ANSWER.", repeats.join("; "));
            trail.seen.push(("NOTE".into(), note));
        }
        if ran == 0 {
            if stalled {
                st.stop = "it asked only for lookups it had already made, twice".into();
                break;
            }
            stalled = true;
        }
    }
    Ok(report(ctx, &plan, &scope, &trail, answer, st))
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

// How the run ended: why it stopped early, whether a reply was cut short, the turns used, whether only the evidence can be returned, and the starting symbols not found with their candidates.
struct Status {
    stop: String,
    cut: bool,
    turns: usize,
    partial: bool,
    missed: Vec<(String, Vec<String>)>,
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
    for (name, c) in &st.missed {
        let alts = if c.is_empty() { "nothing in scope resembles it".to_string() } else { c.join("; ") };
        out.push_str(&format!("starting symbol not in scope: {name} (candidates: {alts})\n"));
    }
    if st.partial {
        out.push_str(&evidence(trail));
    }
    let state = match (st.stop.is_empty(), st.cut, st.partial) {
        (_, _, true) => format!("PARTIAL: {}; the evidence it gathered is above, verbatim", st.stop),
        (true, false, _) => "complete".to_string(),
        (true, true, _) => "INCOMPLETE: a reply was cut short by the token limit".to_string(),
        (false, _, _) => format!("INCOMPLETE: {}", st.stop),
    };
    let skipped = match trail.skipped {
        0 => String::new(),
        n => format!(", {n} repeated lookups not run again"),
    };
    out.push_str(&format!(
        "-- local model, {} of {} turns, {} lookups ({}){skipped}; {state}. Excerpts are read from the files; claims are the model's.",
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

// The lookups that found something, verbatim, up to a token cap at lookup boundaries; the rest are named.
fn evidence(trail: &Trail) -> String {
    let (mut out, mut used, mut left) =
        (String::from("evidence gathered (lookup output, verbatim):\n"), 0, Vec::new());
    for &i in &trail.evidence {
        let (label, text) = &trail.seen[i];
        let cost = crate::tokens::estimate(text);
        if used + cost > EVIDENCE_TOKENS && used > 0 {
            left.push(label.clone());
            continue;
        }
        used += cost;
        out.push_str(&format!("### {label}\n{}\n", text.trim_end()));
    }
    if !left.is_empty() {
        out.push_str(&format!("(over ~{EVIDENCE_TOKENS} tokens, not shown: {})\n", left.join("; ")));
    }
    out
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
