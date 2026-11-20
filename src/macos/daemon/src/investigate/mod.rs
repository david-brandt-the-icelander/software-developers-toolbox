// `zazzle investigate QUESTION --path P.. --symbol S..`: the local model resolves a repository question with bounded read-only lookups inside the scope, and its answer comes back as findings whose `path:line` citations zazzle has checked and quoted, plus what it could not establish.
pub mod cite;
mod seed;

use crate::cmd::{Ctx, Output};
use crate::llm::Reply;
use std::path::{Path, PathBuf};
use std::time::Instant;

pub const USAGE: &str = "usage: investigate QUESTION.. [--path P].. [--symbol S].. [--steps N] [--seconds N] [--run-id ID [--cancel]] [--context TEXT].. [--adaptive] [--agents A,B..] [--orchestrator M] [--orchestrator-effort E] [--benchmark-model M] [--benchmark-effort E] [--review-model M] [--review-effort E] [--no-review] [--prose] [--json]";
const DEFAULT_STEPS: usize = 6;
const MAX_STEPS: usize = 12;
// The whole call, waiting for the shared local model included, ends by this many seconds: under the 120 s tool
// timeout `zazzle setup` gives clients, so a client gets one result instead of timing out and polling.
const DEFAULT_SECONDS: u64 = 90;
const MAX_SECONDS: u64 = 600;
// Declarations of each starting symbol read before the first turn.
const SEED_DEFS: usize = 2;
// The ranked excerpts returned when no finding is verified: their token cap and the lines kept around a match.
const FALLBACK_TOKENS: usize = 3000;
const CONTEXT_LINES: usize = 2;
// Lines of a lookup with no matching line that the excerpts keep.
const HEAD_LINES: usize = 12;
const PER_TURN: usize = 3;
const LOOKUP_BUDGET: &str = "1500";
const LOOKUP_MAX: &str = "15";
const SEEN_TOKENS: usize = 12_000;
const REPLY_TOKENS: usize = 1500;

// What the model is told on its one answer-only turn, after a reason the run could not go on.
const FINAL: &str = "No more lookups will run. Reply now with ANSWER, then FINDING lines citing only lines shown above (path:A-B, or E1:A-B for lines A to B of excerpt E1), UNKNOWN lines for what the evidence does not show, and the CONFIDENCE line. Nothing else.";

// Words of a question too common to pick out its evidence.
const COMMON: &[&str] = &[
    "all", "and", "any", "are", "but", "can", "did", "for", "has", "how", "its", "not", "the", "was", "who",
    "why", "you", "about", "after", "before", "between", "call", "called", "calls", "code", "could", "does",
    "each", "explain", "file", "files", "find", "from", "happen", "happens", "have", "into", "make", "made",
    "only", "path", "should", "show", "than", "that", "their", "them", "then", "there", "they", "this",
    "through", "trace", "used", "uses", "using", "what", "when", "where", "which", "with", "work", "works",
    "would",
];
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
CONFIDENCE understanding=high|medium|low result=high|medium|low
and only when needed: ESCALATE the exact open question a stronger model should take over, CONTEXT lines it would need, BLOCKED access|decision what only the user can give.
Cite only lines you have seen, with the line numbers shown to you. Lookups that found something are numbered E1, E2 and so on; E2:A-B cites lines A to B of E2's file. Prefer a few exact findings over many vague ones. Stay inside the scope.";

// Something an investigation asks: the local model through ai-agent, or a script in tests.
pub trait Local {
    // Answers one turn, the system message and the transcript so far, by the deadline (waiting for the model included).
    fn ask(&self, system: &str, transcript: &str, deadline: Instant) -> Result<Reply, String>;
}

// The user's local model through daemon's shared worker, which stays loaded across the turns (and after the run until idle). Nothing is downloaded.
#[derive(Default)]
pub struct Model;

impl Local for Model {
    // Asks at low effort and temperature 0, with whatever model is loaded, else the default.
    fn ask(&self, system: &str, transcript: &str, deadline: Instant) -> Result<Reply, String> {
        crate::llm::ask_until(None, &request(system, transcript, "low"), deadline)
    }
}

// The local model request for one turn at an effort, temperature 0.
pub fn request(system: &str, transcript: &str, effort: &str) -> serde_json::Value {
    let user = format!("Reply with your next lookups, or ANSWER.\n\n---\n{transcript}");
    serde_json::json!({"system": system, "user": user, "max_tokens": REPLY_TOKENS, "temp": 0, "effort": effort})
}

// The parsed command line; `context` is what an earlier attempt established, shown to the model apart from the question.
pub struct Plan {
    pub question: String,
    pub scope: Vec<String>,
    pub symbols: Vec<String>,
    pub steps: usize,
    pub seconds: u64,
    pub context: Vec<String>,
}

// Parses the question words and the flags.
pub fn parse(argv: &[String]) -> Result<Plan, String> {
    let mut p = Plan {
        question: String::new(),
        scope: Vec::new(),
        symbols: Vec::new(),
        steps: DEFAULT_STEPS,
        seconds: DEFAULT_SECONDS,
        context: Vec::new(),
    };
    let (mut words, mut it) = (Vec::new(), argv.iter());
    while let Some(a) = it.next() {
        let mut value = || it.next().cloned().ok_or_else(|| format!("{a} needs a value; {USAGE}"));
        match a.as_str() {
            "--path" | "-p" => p.scope.push(value()?),
            "--symbol" | "-s" => p.symbols.push(value()?),
            "--context" => p.context.push(value()?),
            "--steps" => {
                let v = value()?;
                p.steps = v
                    .parse::<usize>()
                    .ok()
                    .filter(|n| (1..=MAX_STEPS).contains(n))
                    .ok_or_else(|| format!("--steps takes 1 to {MAX_STEPS}, got '{v}'"))?;
            }
            "--seconds" => {
                let v = value()?;
                p.seconds = v
                    .parse::<u64>()
                    .ok()
                    .filter(|n| (1..=MAX_SECONDS).contains(n))
                    .ok_or_else(|| format!("--seconds takes 1 to {MAX_SECONDS}, got '{v}'"))?;
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
        if !plan.context.is_empty() {
            t.push_str(
                "Established by an earlier attempt (build on it; cite lines only once you have seen them):\n",
            );
            t.push_str(&plan.context.iter().map(|c| format!("- {c}\n")).collect::<String>());
        }
        t.push_str(&match left {
            0 => "No lookups left: reply with ANSWER now.\n".to_string(),
            n => format!("Turns left for lookups: {n} (up to {PER_TURN} lookups each)\n"),
        });
        t.push_str("\nWhat you have seen:\n");
        if self.seen.is_empty() {
            t.push_str("(nothing yet)\n");
        }
        for (i, (label, text)) in self.seen.iter().enumerate() {
            t.push_str(&format!("\n### {}{label}\n{}\n", self.id(i), text.trim_end()));
        }
        t
    }

    // The excerpt id of `seen[i]` with a space after it (`E2 `) when that lookup found something, else nothing.
    fn id(&self, i: usize) -> String {
        self.evidence.iter().position(|&e| e == i).map(|n| format!("E{} ", n + 1)).unwrap_or_default()
    }

    // The file excerpt `En` read, for a citation `En:A-B`: the path of a READ or OUTLINE that found something.
    fn file_of(&self, id: &str) -> Option<String> {
        let n: usize = id.strip_prefix('E')?.parse().ok()?;
        let (label, _) = &self.seen[*self.evidence.get(n.checked_sub(1)?)?];
        let arg = label.strip_prefix("READ ").or_else(|| label.strip_prefix("OUTLINE "))?;
        Some(path_part(arg).0.to_string())
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
            let kind = if matches!(l, Lookup::Read(_)) { "read" } else { "outline" };
            match std::fs::canonicalize(lctx.cwd.join(file)) {
                Ok(canon) => format!("{kind} {}:{range}", canon.display()),
                // A file that does not exist fails the same way for any range and either kind: one attempt is enough.
                Err(_) => format!("missing {}", lctx.cwd.join(file).display()),
            }
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
fn step(
    lctx: &Ctx,
    scope: &[PathBuf],
    plan: &Plan,
    l: &Lookup,
    trail: &mut Trail,
    deadline: Instant,
) -> Result<bool, String> {
    let k = key(lctx, l);
    if let Some((_, why)) = trail.done.iter().find(|(d, _)| *d == k) {
        trail.skipped += 1;
        return Err(match why {
            None => format!("{} (its output is already above)", l.label()),
            Some(w) => format!("{} (it already failed: {w})", l.label()),
        });
    }
    let (text, status) = crate::budget::within(deadline, || look(lctx, scope, plan, l, trail));
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
        Lookup::Read(p) | Lookup::Outline(p) if !path_ok(p) => return (refusal(lctx, scope, plan, p), 0),
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

// Why a READ or OUTLINE of `p` is refused: outside the scope, or a file that does not exist there, with the real files and symbols in scope whose names resemble it.
fn refusal(lctx: &Ctx, scope: &[PathBuf], plan: &Plan, p: &str) -> String {
    let file = path_part(p).0;
    let within = plan.scope.join(", ");
    if std::fs::canonicalize(lctx.cwd.join(file)).is_ok() {
        return format!("refused: {p} is not an existing path inside the scope ({within})");
    }
    let stem = Path::new(file).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let c = seed::candidates(lctx.engine, &lctx.cwd, scope, &stem);
    match c.is_empty() {
        true => format!("refused: {file} does not exist inside the scope ({within}), and nothing there resembles it"),
        false => format!(
            "refused: {file} does not exist inside the scope ({within}). Real files and symbols there that resemble it:\n{}",
            c.join("\n")
        ),
    }
}

// The declarations a `refs` output lists under `defs` (`path:A-B kind name`), as `path:A-B`, at most SEED_DEFS.
fn defs(refs: &str) -> Vec<String> {
    refs.lines()
        .skip_while(|l| l.trim() != "defs")
        .skip(1)
        .take_while(|l| l.trim() != "refs" && !l.starts_with("--"))
        .filter_map(|l| l.split_whitespace().next())
        .filter(|w| path_part(w).1.contains('-'))
        .take(SEED_DEFS)
        .map(str::to_string)
        .collect()
}

// Resolves the starting symbols before any inference, none started after the deadline: their refs inside the scope and the bodies of their declarations are the first evidence; a symbol that is not there gets the real files and symbols in scope that resemble it. Returns the misses with their candidates.
fn seeds(
    lctx: &Ctx,
    scope: &[PathBuf],
    plan: &Plan,
    trail: &mut Trail,
    deadline: Instant,
) -> Vec<(String, Vec<String>)> {
    let mut missed = Vec::new();
    for s in &plan.symbols {
        if crate::budget::over(deadline) {
            break;
        }
        let found = step(lctx, scope, plan, &Lookup::Refs(s.clone()), trail, deadline);
        if found == Ok(true) {
            let refs = trail.seen.last().map(|(_, t)| t.clone()).unwrap_or_default();
            for d in defs(&refs).into_iter().filter(|_| !crate::budget::over(deadline)) {
                let _ = step(lctx, scope, plan, &Lookup::Read(d), trail, deadline);
            }
        }
        if found == Ok(false) && !crate::budget::over(deadline) {
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

// What one run of the lookup loop left: the plan, the scope, the trail of lookups, the answer and how the run ended; `nothing` when no starting symbol was in scope and the model was not asked.
pub struct Explored {
    pub plan: Plan,
    scope: Vec<PathBuf>,
    trail: Trail,
    answer: Option<cite::Answer>,
    st: Status,
    nothing: bool,
}

// Investigates with the given local model and returns the report, with the rules for the cited and read files as notes for the caller.
pub fn investigate(
    ctx: &Ctx,
    argv: &[String],
    local: &dyn Local,
    deadline: Instant,
) -> Result<Output, String> {
    finish(ctx, explore(ctx, argv, local, deadline)?).map(|(o, _)| o)
}

// The report of a run and its structured result.
pub fn finish(ctx: &Ctx, e: Explored) -> Result<(Output, crate::adaptive::work::Work), String> {
    let w = work(ctx, &e);
    Ok((if e.nothing { no_evidence(&e.plan) } else { report(ctx, e) }, w))
}

// Runs the lookup loop for a command line with the given model, up to its answer, a stop or `deadline` (the job's, never extended): no lookup or model turn starts after it, and the lookups it runs end with it.
pub fn explore(ctx: &Ctx, argv: &[String], local: &dyn Local, deadline: Instant) -> Result<Explored, String> {
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
    let missed = seeds(&lctx, &scope, &plan, &mut trail, deadline);
    let nothing = !plan.symbols.is_empty()
        && missed.len() == plan.symbols.len()
        && missed.iter().all(|(_, c)| c.is_empty());
    let mut st =
        Status { stop: String::new(), cut: false, turns: 0, answer_only: false, recovered: None, missed };
    if nothing {
        st.stop = "none of the starting symbols occurs inside the scope".into();
        return Ok(Explored { plan, scope, trail, answer: None, st, nothing });
    }
    let (mut answer, mut strayed, mut stalled, mut retry) = (None, false, false, None);
    for step_no in 1..=plan.steps {
        st.turns = step_no;
        if crate::budget::over(deadline) {
            st.turns -= 1;
            st.stop = stopped(&plan, "");
            break;
        }
        let left = if step_no == plan.steps || trail.size() > SEEN_TOKENS { 0 } else { plan.steps - step_no };
        let reply = match local.ask(&system(&trail), &trail.transcript(&plan, left), deadline) {
            Ok(r) => r,
            Err(e) if trail.seen.is_empty() => return Err(e),
            Err(e) => {
                st.stop = format!("the local model failed: {e}");
                break;
            }
        };
        st.cut |= reply.truncated;
        if empty(&reply) && !trail.evidence.is_empty() {
            st.stop =
                format!("its answer was empty after {} lookups that found evidence", trail.evidence.len());
            retry = Some("Your answer was empty, but the lookups above found evidence.");
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
            (st.stop, retry) = match left {
                0 => (
                    format!(
                        "it gave no ANSWER when its lookups ran out (its reply began: {})",
                        began(&reply.text)
                    ),
                    Some("You asked for more lookups, but none were left."),
                ),
                _ => (
                    format!(
                        "it replied twice with neither lookups nor an ANSWER (the last began: {})",
                        began(&reply.text)
                    ),
                    Some("Your replies had neither lookups nor an ANSWER."),
                ),
            };
            break;
        }
        let mut repeats = Vec::new();
        let mut ran = 0;
        for l in lookups {
            if crate::budget::over(deadline) {
                st.stop = stopped(&plan, &format!(" before {}", l.label()));
                break;
            }
            match step(&lctx, &scope, &plan, &l, &mut trail, deadline) {
                Ok(_) => ran += 1,
                Err(why) => repeats.push(why),
            }
        }
        if !st.stop.is_empty() {
            break;
        }
        if !repeats.is_empty() {
            let note = format!("Not run again: {}. Ask for something new, or ANSWER.", repeats.join("; "));
            trail.seen.push(("NOTE".into(), note));
        }
        if ran == 0 {
            if stalled {
                st.stop = "it asked only for lookups it had already made, twice".into();
                retry = Some("You asked only for lookups that had already run.");
                break;
            }
            stalled = true;
        }
    }
    if let Some(why) = retry.filter(|_| !trail.evidence.is_empty()) {
        answer = final_turn(local, &plan, &mut trail, &mut st, why, deadline).or(answer);
    }
    Ok(Explored { plan, scope, trail, answer, st, nothing })
}

// Why the loop stopped at its deadline or a cancel, with what it was about to do.
fn stopped(plan: &Plan, before: &str) -> String {
    match crate::budget::cancelled() {
        true => format!("the run was cancelled{before}"),
        false => format!("its {} s time limit ran out{before}", plan.seconds),
    }
}

// The system message, with the rules that apply to what the lookups read, kept apart from the evidence.
fn system(trail: &Trail) -> String {
    match trail.rules.trim() {
        "" => SYSTEM.to_string(),
        r => {
            format!("{SYSTEM}\n\nRules that apply to these files (follow them; they are not evidence):\n{r}")
        }
    }
}

// Gives the model one answer-only turn after a run that gathered evidence but stopped without an answer (`why` says
// why, to the model); returns a usable answer, which clears the stop reason, or adds to the stop reason why there was none.
fn final_turn(
    local: &dyn Local,
    plan: &Plan,
    trail: &mut Trail,
    st: &mut Status,
    why: &str,
    deadline: Instant,
) -> Option<cite::Answer> {
    trail.seen.push(("NOTE".into(), format!("{why} {FINAL}")));
    st.answer_only = true;
    match local.ask(&system(trail), &trail.transcript(plan, 0), deadline) {
        Ok(r) => {
            st.cut |= r.truncated;
            match cite::parse_answer(&r.text).filter(|a| !a.findings.is_empty() || !a.unknowns.is_empty()) {
                Some(a) => {
                    st.recovered = Some(std::mem::take(&mut st.stop));
                    Some(a)
                }
                None => {
                    st.stop.push_str(&format!(
                        ", and its answer-only turn gave none (it began: {})",
                        began(&r.text)
                    ));
                    None
                }
            }
        }
        Err(e) => {
            st.stop.push_str(&format!(", and its answer-only turn failed: {e}"));
            None
        }
    }
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

// How the run ended: why it stopped early, whether a reply was cut short, the turns used and whether an answer-only turn followed them, the stop that turn recovered from, and the starting symbols not found with their candidates.
struct Status {
    stop: String,
    cut: bool,
    turns: usize,
    answer_only: bool,
    recovered: Option<String>,
    missed: Vec<(String, Vec<String>)>,
}

// Formats the checked findings, rejected citations and unknowns, with the caller's rules for the files involved as notes; when no finding is verified, ranked excerpts of the evidence and the lookups that found nothing come back instead.
fn report(ctx: &Ctx, e: Explored) -> Output {
    let Explored { plan, scope, trail, answer, st, .. } = e;
    let (plan, trail) = (&plan, &trail);
    let answer = answer.unwrap_or_default();
    let (checked, rejects) = checked(ctx, &scope, trail, &answer);
    let (mut found, mut rejected, mut files) = (String::new(), String::new(), Vec::new());
    let n = checked.len();
    for (i, f) in checked.iter().enumerate() {
        let unread = if trail.seen.iter().any(|(l, t)| l.contains(&f.path) || t.contains(&f.path)) {
            ""
        } else {
            " (not among the lookups)"
        };
        found.push_str(&format!("{}. {}\n   {}{unread}\n{}", i + 1, f.claim, f.cite, f.excerpt));
        files.push(ctx.cwd.join(&f.path));
    }
    for r in &rejects {
        rejected.push_str(&format!("- {r}\n"));
    }
    let fallback = n == 0 && !trail.evidence.is_empty();
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
    out.push_str(&self_report(&answer));
    if fallback {
        out.push_str(&excerpts(trail, plan));
        out.push_str(&gaps(trail));
    }
    let state = match (st.stop.is_empty(), st.cut, fallback) {
        (false, _, true) => {
            format!("PARTIAL: {}; ranked excerpts of its evidence are above, verbatim", st.stop)
        }
        (true, _, true) => {
            "PARTIAL: no finding could be verified; ranked excerpts of its evidence are above, verbatim"
                .to_string()
        }
        (true, false, _) => match &st.recovered {
            Some(why) => format!("complete, on an answer-only turn after {why}"),
            None => "complete".to_string(),
        },
        (true, true, _) => "INCOMPLETE: a reply was cut short by the token limit".to_string(),
        (false, _, false) => format!("INCOMPLETE: {}", st.stop),
    };
    let skipped = match trail.skipped {
        0 => String::new(),
        n => format!(", {n} repeated lookups not run again"),
    };
    let extra = if st.answer_only { " and an answer-only turn" } else { "" };
    out.push_str(&format!(
        "-- local model, {} of {} turns{extra}, {} lookups ({}){skipped}; {state}. Excerpts are read from the files; claims are the model's.",
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

// Checks an answer's citations, resolving excerpt ids to their files: the verified findings and, for each rejected
// one, `path:A-B claim: why`.
fn checked(ctx: &Ctx, scope: &[PathBuf], trail: &Trail, answer: &cite::Answer) -> (Vec<Found>, Vec<String>) {
    let (mut found, mut rejected) = (Vec::new(), Vec::new());
    for c in &answer.findings {
        let c = &cite::Claim { path: trail.file_of(&c.path).unwrap_or_else(|| c.path.clone()), ..c.clone() };
        let (f, r) = verify(&ctx.cwd, scope, c);
        found.extend(f);
        rejected.extend(r);
    }
    (found, rejected)
}

// One finding whose citation zazzle checked: the claim, the file, `path:A-B` and the cited lines.
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    pub claim: String,
    pub path: String,
    pub cite: String,
    pub excerpt: String,
}

// Checks one claim against its file: a Found, or the rejection line `path:A-B claim: why`.
pub fn verify(cwd: &Path, scope: &[PathBuf], c: &cite::Claim) -> (Option<Found>, Option<String>) {
    let tail = if c.end > c.start { format!("-{}", c.end) } else { String::new() };
    let cite = format!("{}:{}{tail}", c.path, c.start);
    match cite::check(cwd, scope, c) {
        cite::Checked::Verified { excerpt } => {
            (Some(Found { claim: c.text.clone(), path: c.path.clone(), cite, excerpt }), None)
        }
        cite::Checked::Rejected(why) => (None, Some(format!("{cite} {}: {why}", c.text))),
    }
}

// The answer's report on itself as lines for the report: its confidence, the question it would escalate with its
// context, and what it needs from the user; nothing for what it did not say.
fn self_report(a: &cite::Answer) -> String {
    let mut out = String::new();
    if !a.understanding.is_empty() || !a.confidence.is_empty() {
        let or = |s: &str| if s.is_empty() { "not given".to_string() } else { s.to_string() };
        out.push_str(&format!(
            "confidence: understanding {}, result {}\n",
            or(&a.understanding),
            or(&a.confidence)
        ));
    }
    if let Some(q) = &a.escalate {
        let ctx = if a.context.is_empty() {
            String::new()
        } else {
            format!(" (context: {})", a.context.join("; "))
        };
        out.push_str(&format!("escalate: {q}{ctx}\n"));
    }
    if let Some(b) = &a.blocked {
        out.push_str(&format!("needs the user: {b}\n"));
    }
    out
}

// The checked, structured result of a run, for the adaptive mode: verified findings, rejected citations, gaps (the
// unknowns, the stop and the lookups that found nothing) and the model's report on itself; `status` is complete
// only for an answer with nothing stopping it, partial when something was verified, else failed.
pub fn work(ctx: &Ctx, e: &Explored) -> crate::adaptive::work::Work {
    let answer = e.answer.clone().unwrap_or_default();
    let (findings, rejected) = checked(ctx, &e.scope, &e.trail, &answer);
    let mut gaps = answer.unknowns.clone();
    if !e.st.stop.is_empty() {
        gaps.push(format!("the run stopped early: {}", e.st.stop));
    }
    let failed = e.trail.lookups.iter().zip(&e.trail.done);
    gaps.extend(failed.filter_map(|(l, (_, why))| why.as_ref().map(|w| format!("{l} found nothing: {w}"))));
    let status = match (e.answer.is_some() && e.st.stop.is_empty() && !e.st.cut, findings.is_empty()) {
        (true, _) if !answer.status.is_empty() => answer.status.clone(),
        (true, _) => "complete".into(),
        (false, false) => "partial".into(),
        (false, true) => "failed".into(),
    };
    crate::adaptive::work::Work {
        status,
        understanding: answer.understanding,
        confidence: answer.confidence,
        findings,
        rejected,
        gaps,
        escalate: answer.escalate,
        context: answer.context,
        blocked: answer.blocked,
        turns: e.st.turns + usize::from(e.st.answer_only),
    }
}

// The lowercase words that pick out a question's evidence: the starting symbols and their words, and the question's
// words of three letters or more that are not common.
fn terms(plan: &Plan) -> Vec<String> {
    let mut t: Vec<String> = plan.symbols.iter().map(|s| s.to_lowercase()).collect();
    let words = plan.symbols.iter().flat_map(|s| seed::words(s)).chain(
        plan.question
            .split(|c: char| !c.is_alphanumeric() && c != '_')
            .flat_map(|w| std::iter::once(w.to_lowercase()).chain(seed::words(w))),
    );
    for w in words {
        if w.len() >= 3 && !COMMON.contains(&w.as_str()) && !t.contains(&w) {
            t.push(w);
        }
    }
    t
}

// The evidence as ranked verbatim excerpts within FALLBACK_TOKENS: from each lookup that found something, its first
// line and the lines naming a term (with CONTEXT_LINES around them), or its first HEAD_LINES when none does; lookups
// with more matching lines first, each line of a file shown once; the lookups left out are named.
fn excerpts(trail: &Trail, plan: &Plan) -> String {
    let terms = terms(plan);
    let matches = |l: &str| {
        let l = l.to_lowercase();
        terms.iter().any(|t| l.contains(t.as_str()))
    };
    // The session's `[r:…]` markers mean nothing outside it; the first line left names the file and is always kept,
    // but a match there (a file named after a term) does not count.
    let body = |text: &str| -> Vec<String> {
        text.lines().filter(|l| !(l.starts_with("[r:") && l.ends_with(']'))).map(str::to_string).collect()
    };
    let mut ranked: Vec<(usize, usize)> = trail
        .evidence
        .iter()
        .enumerate()
        .map(|(id, &i)| (body(&trail.seen[i].1).iter().skip(1).filter(|l| matches(l)).count(), id))
        .collect();
    ranked.sort_by_key(|&(hits, id)| (std::cmp::Reverse(hits), id));
    let mut out = String::from("evidence (no finding verified; ranked excerpts of the lookups, verbatim):\n");
    let (mut shown, mut used, mut left) = (std::collections::HashSet::new(), 0, Vec::new());
    for (hits, id) in ranked {
        let (label, text) = &trail.seen[trail.evidence[id]];
        let file = label.split_once(' ').map(|(_, a)| path_part(a).0).unwrap_or_default();
        let lines = body(text);
        let hit = |j: usize| j > 0 && lines.get(j).is_some_and(|l| matches(l));
        let keep = |k: usize| match hits {
            0 => k < HEAD_LINES,
            _ => k == 0 || (k.saturating_sub(CONTEXT_LINES)..=k + CONTEXT_LINES).any(hit),
        };
        let (mut body, mut last) = (String::new(), None);
        for (k, line) in lines.iter().enumerate().filter(|&(k, _)| keep(k)) {
            if !shown.insert((file.to_string(), line.clone())) {
                continue;
            }
            if last.is_some_and(|p| p + 1 < k) {
                body.push_str("…\n");
            }
            body.push_str(&format!("{line}\n"));
            last = Some(k);
        }
        let cost = crate::tokens::estimate(&body);
        if body.is_empty() || (used + cost > FALLBACK_TOKENS && used > 0) {
            left.extend((!body.is_empty()).then(|| format!("E{} {label}", id + 1)));
            continue;
        }
        used += cost;
        out.push_str(&format!("### E{} {label}\n{body}", id + 1));
    }
    if !left.is_empty() {
        out.push_str(&format!("(over ~{FALLBACK_TOKENS} tokens, not shown: {})\n", left.join("; ")));
    }
    out
}

// The lookups that found nothing, with why: what the caller need not try again.
fn gaps(trail: &Trail) -> String {
    let failed: Vec<String> = trail
        .lookups
        .iter()
        .zip(&trail.done)
        .filter_map(|(l, (_, why))| why.as_ref().map(|w| format!("- {l}: {w}\n")))
        .collect();
    match failed.is_empty() {
        true => String::new(),
        false => format!("lookups that found nothing:\n{}", failed.concat()),
    }
}

// The MCP tool for investigate.
pub(crate) const TOOLS: &[crate::mcp::Tool] = &[crate::mcp::Tool {
    name: "zazzle_investigate",
    description: "Hand a repository question needing several files to the user's local model: it follows read-only lookups inside paths from the symbols, and returns findings whose path:line citations zazzle checked and quoted, plus unknowns. adaptive splits it into parts for the cheapest agent and effort that will do each, escalating unsettled parts; review_model judges the results, benchmark_model compares them with one agent alone. For one known symbol or a short lookup, use zazzle_read or zazzle_refs directly.",
    schema,
    map: tool_args,
}];

// Schema for zazzle_investigate.
fn schema() -> serde_json::Value {
    crate::mcp::object(
        serde_json::json!({
            "question": {"type": "string", "description": "Required unless run_id names an earlier run to get back"},
            "run_id": {"type": "string", "description": "The run's id: the same id again returns the kept result (waiting while it runs) without asking any model again; with no question, looks the run up"},
            "cancel": {"type": "boolean", "description": "With run_id alone: stop that run now and return what it had gathered"},
            "paths": {"type": "array", "items": {"type": "string"}, "description": "Folders or files to stay inside (default: the working directory)"},
            "symbols": {"type": "array", "items": {"type": "string"}, "description": "Names to start from"},
            "steps": {"type": "integer", "description": "Model turns, 1 to 12 (default 6)"},
            "seconds": {"type": "integer", "description": "Time limit for the whole job, 1 to 600 (default 90): planning, waiting for and starting models, lookups, escalation, a benchmark and a review all end by it, and a partial result is kept"},
            "adaptive": {"type": "boolean", "description": "Plan the question into parts, give each the cheapest agent and effort that will do it, and escalate a part that is not settled"},
            "agents": {"type": "array", "items": {"type": "string"}, "description": "The adaptive ladder, cheapest first: MODEL, local:MODEL, claude:MODEL or codex:MODEL, each optionally @EFFORT (default: the installed local models)"},
            "orchestrator": {"type": "string", "description": "Adaptive only: the agent that plans, combines and summarizes, instead of the ladder: MODEL, local:MODEL, claude:MODEL or codex:MODEL, optionally @EFFORT; the report lists each step, its agent and tokens as JSON"},
            "orchestrator_effort": {"type": "string"},
            "benchmark_model": {"type": "string", "description": "Also answer the question with this agent alone and compare (default: the top of the ladder); a judge compares the two on quality, wall-clock time and tokens, by default claude:opus at high effort"},
            "benchmark_effort": {"type": "string"},
            "review_model": {"type": "string", "description": "Have this agent judge the results and each finding"},
            "review_effort": {"type": "string"},
            "no_review": {"type": "boolean", "description": "With a benchmark: leave out the judge it gets by default (claude:opus at high effort when installed)"},
            "prose": {"type": "boolean", "description": "Adaptive only: the cheapest agent adds a short summary that keeps the citations"}
        }),
        &[],
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
    crate::mcp::push_num(&mut a, v, "seconds", "--seconds")?;
    crate::mcp::push_flag(&mut a, v, "adaptive", "--adaptive")?;
    crate::mcp::push_flag(&mut a, v, "prose", "--prose")?;
    crate::mcp::push_flag(&mut a, v, "no_review", "--no-review")?;
    let agents = crate::mcp::get_list(v, "agents")?;
    if !agents.is_empty() {
        a.extend(["--agents".to_string(), agents.join(",")]);
    }
    for key in [
        "orchestrator",
        "orchestrator_effort",
        "benchmark_model",
        "benchmark_effort",
        "review_model",
        "review_effort",
    ] {
        if let Some(x) = crate::mcp::get_str(v, key)? {
            a.extend([format!("--{}", key.replace('_', "-")), x]);
        }
    }
    let run = crate::mcp::get_str(v, "run_id")?;
    if let Some(id) = &run {
        a.extend(["--run-id".to_string(), id.clone()]);
    }
    if v["cancel"] == true {
        return match (&run, a.len(), v.get("question")) {
            (Some(_), 3, None) => Ok([a, vec!["--cancel".to_string()]].concat()),
            _ => Err("cancel takes run_id and nothing else".into()),
        };
    }
    match crate::mcp::get_str(v, "question")? {
        Some(q) => a.push(q),
        None if run.is_some() && a.len() == 3 => {}
        None => return Err("question is required unless run_id alone names an earlier run".into()),
    }
    Ok(a)
}

// Runs `investigate` as a run the caller can recover or cancel (`--run-id ID --cancel` stops it now and returns what it gathered): with the run store, the job gets a run id (`--run-id ID`, else a new one, said on the caller's standard error before the wait), its model calls' usage is kept as each ends, and its result is kept before it is returned; the same id again returns that result (waiting for it while it runs) without asking any model again, and `--run-id ID` alone looks it up.
pub fn run(ctx: &Ctx, argv: &[String]) -> Result<Output, String> {
    let accepted = Instant::now();
    let (given, argv) = take_run_id(argv)?;
    let (cancel, argv): (Vec<String>, Vec<String>) = argv.into_iter().partition(|a| a == "--cancel");
    let Some(root) = ctx.engine.runs.clone() else {
        return match given {
            Some(_) => Err("--run-id needs the run store (~/.zazzle/runs, or ZAZZLE_RUNS_DIR)".into()),
            None => job(ctx, &argv, accepted, &None),
        };
    };
    if !cancel.is_empty() {
        return match (given, argv.is_empty()) {
            (Some(id), true) => crate::runs::cancel(&root, &id),
            _ => Err("--cancel takes the run to stop with --run-id ID and nothing else".into()),
        };
    }
    if argv.is_empty() && given.is_none() {
        return Err(USAGE.into());
    }
    let id = given.unwrap_or_else(crate::runs::new_id);
    let until = accepted + std::time::Duration::from_secs(seconds_of(&argv));
    match crate::runs::begin(&root, &id, &argv, &ctx.cwd, until)? {
        crate::runs::Begun::Earlier(res) => res.map(|mut o| {
            o.notes.push_str(&format!(
                "\n-- the kept result of investigate run {id}; no model was asked again\n"
            ));
            o
        }),
        crate::runs::Begun::New(j) => {
            announce(ctx, &id);
            let stop = j.stop();
            let res = crate::budget::stopping(&stop, || job(ctx, &argv, accepted, &Some(j.usage()))).map(
                |mut o| {
                    if stop.cancelled() {
                        o.notes.push_str(&format!(
                            "\n-- run {id} was cancelled; this is what it had gathered\n"
                        ));
                    }
                    o
                },
            );
            if let Err(e) = j.finish(&res) {
                eprintln!("investigate: keeping run {id}: {e}");
            }
            res
        }
    }
}

// Runs the job: the adaptive mode when any of its flags is given, else the user's local model, each call's usage appended to `sink`.
fn job(
    ctx: &Ctx,
    argv: &[String],
    accepted: Instant,
    sink: &crate::adaptive::Sink,
) -> Result<Output, String> {
    use crate::adaptive::{agent::Rung, calls, OnRung};
    if crate::adaptive::wanted(argv) {
        return crate::adaptive::run_at(ctx, argv, &calls::Real, accepted, sink);
    }
    let plan = parse(argv)?;
    let (log, rung) = (calls::Log::to(sink.clone()), Rung::parse("", Some("low"))?);
    let local = OnRung { b: &calls::Real, rung: &rung, log: &log, role: "investigate" };
    investigate(ctx, argv, &local, crate::adaptive::job_end(accepted, plan.seconds))
}

// Splits `--run-id ID` (or `--run-id=ID`) off a command line.
fn take_run_id(argv: &[String]) -> Result<(Option<String>, Vec<String>), String> {
    let (mut id, mut rest, mut it) = (None, Vec::new(), argv.iter());
    while let Some(a) = it.next() {
        match a.strip_prefix("--run-id") {
            Some("") => id = Some(it.next().ok_or("--run-id takes an id")?.clone()),
            Some(v) if v.starts_with('=') => id = Some(v[1..].to_string()),
            _ => rest.push(a.clone()),
        }
    }
    Ok((id, rest))
}

// The time limit a command line asks for, in seconds (the default when it names none it can use).
fn seconds_of(argv: &[String]) -> u64 {
    argv.iter()
        .position(|a| a == "--seconds")
        .and_then(|i| argv.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_SECONDS)
}

// Tells a command-line caller the run id before the wait, on its standard error, so a wait cut short can be recovered.
fn announce(ctx: &Ctx, id: &str) {
    use std::io::Write;
    if let Some(c) = ctx.caller.borrow().as_ref() {
        if let Ok(fd) = c.stdio[2].try_clone() {
            let _ = std::fs::File::from(fd).write_all(
                format!(
                    "investigate: run {id} (if this wait is cut off: zazzle investigate --run-id {id})\n"
                )
                .as_bytes(),
            );
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests;
