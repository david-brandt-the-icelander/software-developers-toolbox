// `zazzle investigate .. --adaptive`: the cheapest planner sure to be overkill for the question splits it into parts and gives each the cheapest agent and effort that will do it; every executor answers in short structured lines that say how sure it is and whether to escalate, zazzle checks their citations and hands an unsettled part, with what it established, to the next rung; `--review-*` has another agent judge the results and `--benchmark-*` compares them with the question answered by one agent alone.
pub mod agent;
pub mod calls;
pub mod work;

use crate::cmd::{Ctx, Output};
use crate::investigate::{self, Plan};
use crate::llm::Reply;
use agent::Rung;
use calls::{Ask, Backend, Call, Log};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use work::Work;

// The flags that select the adaptive mode, its review or its benchmark.
const FLAGS: &[&str] = &[
    "--adaptive",
    "--agents",
    "--benchmark-model",
    "--benchmark-effort",
    "--review-model",
    "--review-effort",
    "--prose",
    "--json",
];
// The most parts a plan has, the most escalations of one part and the most planners tried.
const MAX_PARTS: usize = 4;
const MAX_ESCALATIONS: usize = 3;
const PLANNERS: usize = 2;
// The lines every executor answers in; the same words the local lookup loop's answer uses.
const LINES: &str = "STATUS complete|partial|failed
FINDING path:A-B one sentence that the cited lines show (one per fact, real line numbers)
GAP what you could not establish
CONFIDENCE understanding=high|medium|low result=high|medium|low (your understanding of the task, the support for your results)
ESCALATE the exact open question a stronger agent should take over (only if one should)
CONTEXT what that agent would need (only with ESCALATE)
BLOCKED access|decision what only the user can give (only if something is)";

// Where a job appends each model call's usage as it ends, if anywhere.
pub type Sink = Option<PathBuf>;

// One rung's local model as the lookup loop's model, each turn recorded under a role.
pub struct OnRung<'a> {
    pub b: &'a dyn Backend,
    pub rung: &'a Rung,
    pub log: &'a Log,
    pub role: &'a str,
}

impl investigate::Local for OnRung<'_> {
    // Asks the rung's model at the rung's effort.
    fn ask(&self, system: &str, transcript: &str, deadline: Instant) -> Result<Reply, String> {
        let req = investigate::request(system, transcript, &self.rung.effort);
        self.log.local(
            self.b,
            self.rung,
            self.role,
            &req,
            &[("brief", system), ("evidence", transcript)],
            deadline,
        )
    }
}

// The adaptive mode's own flags, with the investigate flags passed through in `rest`.
#[derive(Debug, Default, PartialEq)]
struct Opts {
    adaptive: bool,
    agents: Vec<String>,
    bench: Option<(Option<String>, Option<String>)>,
    review: Option<(Option<String>, Option<String>)>,
    prose: bool,
    json: bool,
    rest: Vec<String>,
}

// Tells whether a command line asks for the adaptive mode, a review or a benchmark.
pub fn wanted(argv: &[String]) -> bool {
    argv.iter().any(|a| FLAGS.contains(&a.as_str()))
}

// Separates the adaptive mode's flags from the investigate flags.
fn options(argv: &[String]) -> Result<Opts, String> {
    let mut o = Opts::default();
    let mut it = argv.iter();
    while let Some(a) = it.next() {
        let mut value =
            || it.next().cloned().ok_or_else(|| format!("{a} needs a value; {}", investigate::USAGE));
        match a.as_str() {
            "--adaptive" => o.adaptive = true,
            "--prose" => o.prose = true,
            "--json" => o.json = true,
            "--agents" => {
                o.agents.extend(value()?.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()))
            }
            "--benchmark-model" => o.bench.get_or_insert((None, None)).0 = Some(value()?),
            "--benchmark-effort" => o.bench.get_or_insert((None, None)).1 = Some(value()?),
            "--review-model" => o.review.get_or_insert((None, None)).0 = Some(value()?),
            "--review-effort" => o.review.get_or_insert((None, None)).1 = Some(value()?),
            _ => o.rest.push(a.clone()),
        }
    }
    if !o.adaptive && !o.agents.is_empty() {
        return Err("--agents sets the adaptive mode's ladder: add --adaptive".into());
    }
    if o.prose && !o.adaptive {
        return Err("--prose summarizes the adaptive mode's result: add --adaptive".into());
    }
    Ok(o)
}

// A model and effort from flags: the model given, else the top of the ladder; the effort given, else the model's default.
fn chosen(pick: &(Option<String>, Option<String>), ladder: &[Rung]) -> Result<Rung, String> {
    let top = ladder.last().cloned().unwrap_or(Rung::parse("", None)?);
    match &pick.0 {
        Some(m) => Rung::parse(m, pick.1.as_deref()),
        None => match pick.1.as_deref() {
            Some(e) if !top.kind.efforts().contains(&e) => {
                Err(format!("{} takes effort {}, not `{e}`", top.agent(), top.kind.efforts().join(", ")))
            }
            e => Ok(Rung { effort: e.unwrap_or(&top.effort).to_string(), ..top }),
        },
    }
}

// One part of a plan: its question, starting symbols and the ladder rung it starts on.
#[derive(Debug, Clone, PartialEq)]
struct Part {
    question: String,
    symbols: Vec<String>,
    rung: usize,
}

// One attempt at a part: the rung, its checked result and why it was escalated (None for the last attempt that settled it or the attempt left standing).
struct Attempt {
    rung: Rung,
    work: Work,
    why: Option<String>,
}

// A run's outcome: its checked result, its text report, its calls and how long it took.
struct Run {
    label: String,
    work: Work,
    text: String,
    calls: Vec<Call>,
    secs: f64,
    json: Value,
}

// Runs the adaptive mode, a review or a benchmark with the real agents.
pub fn run(ctx: &Ctx, argv: &[String]) -> Result<Output, String> {
    run_with(ctx, argv, &calls::Real)
}

// Runs the command line with the given agents under one deadline for the whole job: the main run (adaptive, or the plain local investigation) with a hosted benchmark beside it, then a local benchmark and a review, each stage ending by its share of the time left.
pub fn run_with(ctx: &Ctx, argv: &[String], b: &dyn Backend) -> Result<Output, String> {
    run_at(ctx, argv, b, Instant::now(), &None)
}

// Runs the command line for a job accepted at `accepted`, appending each model call's usage to `sink` as it ends.
pub fn run_at(
    ctx: &Ctx,
    argv: &[String],
    b: &dyn Backend,
    accepted: Instant,
    sink: &Sink,
) -> Result<Output, String> {
    let o = options(argv)?;
    let plan = investigate::parse(&o.rest)?;
    let mut scope = Vec::new();
    for s in &plan.scope {
        scope.push(
            std::fs::canonicalize(ctx.cwd.join(s))
                .map_err(|_| format!("--path {s}: no such file or folder"))?,
        );
    }
    let specs = match o.agents.is_empty() {
        true => b.installed(),
        false => o.agents.clone(),
    };
    let mut ladder = agent::ladder(&specs)?;
    if ladder.is_empty() {
        ladder = vec![Rung::parse("", Some("low"))?, Rung::parse("", Some("high"))?];
    }
    let bench = o.bench.as_ref().map(|p| chosen(p, &ladder)).transpose()?;
    let reviewer = o.review.as_ref().map(|p| chosen(p, &ladder)).transpose()?;
    let end = job_end(accepted, plan.seconds);
    let stages = 1 + u32::from(bench.as_ref().is_some_and(|r| !r.hosted())) + u32::from(reviewer.is_some());
    let main_end = share(end, stages);
    let (main, bench_run) = std::thread::scope(|s| {
        let hosted = bench.as_ref().filter(|r| r.hosted()).map(|r| {
            let (cwd, scope, plan, job) = (&ctx.cwd, &scope, &plan, crate::budget::current());
            s.spawn(move || crate::budget::carry(job, || pure_hosted(b, r, plan, cwd, scope, main_end, sink)))
        });
        let main = match o.adaptive {
            true => adaptive(ctx, b, &plan, &scope, &ladder, o.prose, main_end, sink),
            false => plain(ctx, b, &o.rest, main_end, sink),
        };
        let bench_run = match (hosted, &bench) {
            (Some(h), _) => Some(h.join().expect("the benchmark thread")),
            (None, Some(r)) => Some(pure_local(ctx, b, r, &o.rest, share(end, stages - 1), sink)),
            (None, None) => None,
        };
        (main, bench_run)
    });
    let main = main?;
    let review = reviewer.map(|r| review(b, &r, &plan, &ctx.cwd, &main, bench_run.as_ref(), end, sink));
    Ok(Output::plain(match o.json {
        true => serde_json::to_string_pretty(&joined(&plan, &main, bench_run.as_ref(), review.as_ref()))
            .unwrap_or_default(),
        false => text(&main, bench_run.as_ref(), review.as_ref()),
    }))
}

// The plain local investigation, as zazzle runs it without --adaptive.
fn plain(ctx: &Ctx, b: &dyn Backend, rest: &[String], deadline: Instant, sink: &Sink) -> Result<Run, String> {
    let (t, log, rung) = (Instant::now(), Log::to(sink.clone()), Rung::parse("", None)?);
    let e = investigate::explore(
        ctx,
        rest,
        &OnRung { b, rung: &rung, log: &log, role: "investigate" },
        deadline,
    )?;
    let (out, work) = investigate::finish(ctx, e)?;
    let json = work.json();
    Ok(Run {
        label: "zazzle".into(),
        work,
        text: out.text,
        calls: log.calls(),
        secs: t.elapsed().as_secs_f64(),
        json,
    })
}

// The question answered by one hosted agent alone, its citations checked like any executor's.
fn pure_hosted(
    b: &dyn Backend,
    r: &Rung,
    plan: &Plan,
    cwd: &Path,
    scope: &[PathBuf],
    deadline: Instant,
    sink: &Sink,
) -> Run {
    let (t, log) = (Instant::now(), Log::to(sink.clone()));
    let prompt = executor_prompt(&plan.question, &plan.scope, &plan.symbols, &[]);
    let work =
        match log.hosted(b, r, &Ask::new("benchmark", "", &prompt, &[("task", &prompt)]), cwd, deadline) {
            Ok(s) => Work::from_reply(cwd, scope, &s.text),
            Err(e) => Work::failed(&e),
        };
    let text = work.render();
    Run {
        label: r.label(),
        json: work.json(),
        work,
        text,
        calls: log.calls(),
        secs: t.elapsed().as_secs_f64(),
    }
}

// The question answered by one local model and effort alone, through the lookup loop.
fn pure_local(ctx: &Ctx, b: &dyn Backend, r: &Rung, rest: &[String], deadline: Instant, sink: &Sink) -> Run {
    let (t, log) = (Instant::now(), Log::to(sink.clone()));
    let work =
        match investigate::explore(ctx, rest, &OnRung { b, rung: r, log: &log, role: "benchmark" }, deadline)
        {
            Ok(e) => investigate::work(ctx, &e),
            Err(e) => Work::failed(&e),
        };
    let text = work.render();
    Run {
        label: r.label(),
        json: work.json(),
        work,
        text,
        calls: log.calls(),
        secs: t.elapsed().as_secs_f64(),
    }
}

// When the work of a job accepted at `start` with `seconds` must end: its deadline less a reserve (a twentieth, at most 2 s) for writing and keeping the report with what was gathered.
pub fn job_end(start: Instant, seconds: u64) -> Instant {
    let total = Duration::from_secs(seconds);
    start + total - (total / 20).min(Duration::from_secs(2))
}

// The end of the next of `stages` stages that run one after another before `end`: an even share of the time left, so time a stage does not use goes to the ones after it.
fn share(end: Instant, stages: u32) -> Instant {
    let now = Instant::now();
    now + end.saturating_duration_since(now) / stages.max(1)
}

// Rates how hard a question is to plan, 0 to 5, with the reasons; 0 needs no planner.
fn assess(plan: &Plan) -> (usize, Vec<&'static str>) {
    let q = plan.question.to_lowercase();
    let judgement =
        ["should", "why", "design", "compatib", "contract", "safe", "trade", "version", "migrat", "regress"];
    let checks = [
        (plan.question.split_whitespace().count() > 18, "a long question"),
        (
            q.matches('?').count() > 1
                || q.contains(';')
                || q.contains(" and how ")
                || q.contains(" and why "),
            "several questions",
        ),
        (plan.scope == ["."], "the whole repository"),
        (plan.symbols.is_empty(), "no starting symbol"),
        (plan.symbols.len() > 2, "many starting symbols"),
        (judgement.iter().any(|w| q.contains(w)), "it asks for judgement"),
    ];
    let why: Vec<&str> = checks.iter().filter(|(on, _)| *on).map(|(_, w)| *w).collect();
    (why.len().min(5), why)
}

// The adaptive run: assess, plan with a planner one rung above the difficulty (or skip planning for a simple question), run the parts at the same time (each with its own read session, so one part's reads never hide a file from another), each with escalation, combine the parts and, with --prose, have the cheapest rung write a summary.
#[allow(clippy::too_many_arguments)]
fn adaptive(
    ctx: &Ctx,
    b: &dyn Backend,
    plan: &Plan,
    scope: &[PathBuf],
    ladder: &[Rung],
    prose: bool,
    deadline: Instant,
    sink: &Sink,
) -> Result<Run, String> {
    let (t, log) = (Instant::now(), Log::to(sink.clone()));
    let (score, why) = assess(plan);
    let mut head = format!(
        "investigate (adaptive): {}\nscope: {}\nladder: {}\ndifficulty: {score} of 5 ({})\n",
        plan.question,
        plan.scope.join(", "),
        ladder
            .iter()
            .enumerate()
            .map(|(i, r)| format!("{} {}", i + 1, r.label()))
            .collect::<Vec<_>>()
            .join(", "),
        if why.is_empty() { "a short question about named symbols".to_string() } else { why.join(", ") }
    );
    let whole = Part { question: plan.question.clone(), symbols: plan.symbols.clone(), rung: 0 };
    let (parts, planner) = match score == 0 || ladder.len() == 1 {
        true => {
            head.push_str(&format!("plan: one part on {}, no planner call\n", ladder[0].label()));
            (vec![whole], None)
        }
        false => {
            let (parts, planner, note) =
                make_plan(b, &log, plan, &ctx.cwd, ladder, (score + 1).min(ladder.len() - 1), deadline);
            head.push_str(&note);
            match parts.is_empty() {
                true => (vec![whole], planner),
                false => (parts, planner),
            }
        }
    };
    if planner.is_some() {
        for (i, p) in parts.iter().enumerate() {
            head.push_str(&format!("  P{} {}: {}\n", i + 1, ladder[p.rung].label(), p.question));
        }
    }
    let (engine, cwd, roots) = (ctx.engine, &ctx.cwd, &ctx.roots);
    let base = if ctx.session.is_empty() { "investigate" } else { ctx.session.as_str() };
    let runs = calls::all(&parts, |i, p| {
        let own = Ctx {
            engine,
            cwd: cwd.clone(),
            session: format!("{base}>P{}", i + 1),
            roots: roots.clone(),
            stdin: std::cell::RefCell::new(None),
            caller: std::cell::RefCell::new(None),
        };
        part(&own, b, &log, plan, scope, ladder, p, i + 1, deadline)
    });
    let mut out = head;
    out.push_str("parts:\n");
    for (i, attempts) in runs.iter().enumerate() {
        out.push_str(&format!("  P{} {}\n", i + 1, story(attempts)));
    }
    let finals: Vec<Work> =
        runs.iter().map(|a| a.last().map(|x| x.work.clone()).unwrap_or_default()).collect();
    let (work, by) = match finals.len() {
        1 => (finals[0].clone(), String::new()),
        _ => {
            combine(b, &log, plan, &ctx.cwd, scope, &finals, planner.as_ref().unwrap_or(&ladder[0]), deadline)
        }
    };
    out.push_str(&format!("answer{by}:\n{}", work.render()));
    if prose && !work.findings.is_empty() {
        out.push_str(&summary(b, &log, plan, &ctx.cwd, &ladder[0], &work, deadline));
    }
    let calls = log.calls();
    let state = match work.trigger() {
        None => work.status.clone(),
        Some(t) => format!("{} ({t}; nothing higher settled it)", work.status),
    };
    out.push_str(&format!(
        "-- adaptive: {state}. Excerpts are read from the files; claims are the models'.\n"
    ));
    let json = json!({
        "difficulty": score,
        "planner": planner.as_ref().map(|r| r.label()),
        "parts": parts.iter().zip(&runs).map(|(p, a)| json!({
            "question": p.question,
            "attempts": a.iter().map(|x| json!({"agent": x.rung.agent(), "effort": x.rung.effort, "escalated_because": x.why, "result": x.work.json()})).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "result": work.json(),
    });
    Ok(Run { label: "zazzle adaptive".into(), work, text: out, calls, secs: t.elapsed().as_secs_f64(), json })
}

// Asks the planner for parts, moving to the next rung when it fails, gives no parts, rates its understanding low or asks to escalate; returns the parts, the planner that made them and the report lines.
fn make_plan(
    b: &dyn Backend,
    log: &Log,
    plan: &Plan,
    cwd: &Path,
    ladder: &[Rung],
    first: usize,
    deadline: Instant,
) -> (Vec<Part>, Option<Rung>, String) {
    let menu: String = ladder
        .iter()
        .enumerate()
        .map(|(i, r)| {
            format!(
                "{}. {} ({})\n",
                i + 1,
                r.label(),
                if r.hosted() {
                    "hosted agent, reads files itself"
                } else {
                    "local model, sees only what lookups show"
                }
            )
        })
        .collect();
    let system = format!("You plan how to answer a question about a code repository. Agents, cheapest first:\n{menu}Split the question into at most {MAX_PARTS} parts that can be answered separately (one part when it cannot be split) and give each the cheapest agent that will do it well: source lookups and mapping tests to code suit cheap agents; judgement over the whole task suits stronger ones. You combine the parts' results yourself afterwards. Do not answer the question. Be concise; reply only with lines:\nPART agent-number | the part's question | starting symbols, comma-separated (may be empty)\nCONFIDENCE understanding=high|medium|low result=high|medium|low\nESCALATE why a stronger planner is needed (only if one is)");
    let prompt = format!(
        "Question: {}\nScope: {}\nStarting symbols: {}",
        plan.question,
        plan.scope.join(", "),
        plan.symbols.join(", ")
    );
    let mut note = String::new();
    let tries: Vec<usize> = (first..ladder.len()).take(PLANNERS).collect();
    for (k, &idx) in tries.iter().enumerate() {
        let r = &ladder[idx];
        let reply = match log.text(
            b,
            r,
            &Ask::new("planner", &system, &prompt, &[("brief", &system), ("task", &prompt)]),
            cwd,
            deadline,
        ) {
            Ok(t) => t,
            Err(e) => {
                note.push_str(&format!("planner {} failed: {e}\n", r.label()));
                continue;
            }
        };
        let parts = parse_parts(&reply, ladder.len(), &plan.symbols);
        let a = investigate::cite::parse_answer(&reply).unwrap_or_default();
        let why = match () {
            _ if parts.is_empty() => Some("it gave no PART lines".to_string()),
            _ if a.understanding == "low" => Some("it rated its understanding low".to_string()),
            _ if a.escalate.is_some() => {
                Some(format!("it asked for a stronger planner: {}", a.escalate.clone().unwrap_or_default()))
            }
            _ => None,
        };
        match (why, k + 1 == tries.len()) {
            (Some(w), false) => note.push_str(&format!("planner {} escalated: {w}\n", r.label())),
            (Some(w), true) if parts.is_empty() => {
                note.push_str(&format!(
                    "planner {} gave no plan ({w}); one part on {}\n",
                    r.label(),
                    ladder[0].label()
                ));
                return (Vec::new(), Some(r.clone()), note);
            }
            (w, _) => {
                let kept = w.map(|w| format!(", kept although {w}")).unwrap_or_default();
                note.push_str(&format!("plan: {} parts by {}{kept}\n", parts.len(), r.label()));
                return (parts, Some(r.clone()), note);
            }
        }
    }
    note.push_str(&format!("plan: one part on {} (no planner answered)\n", ladder[0].label()));
    (Vec::new(), None, note)
}

// Reads `PART n | question | symbols` lines, clamping the rung to the ladder and taking the run's symbols when a part names none.
fn parse_parts(reply: &str, rungs: usize, symbols: &[String]) -> Vec<Part> {
    let mut out = Vec::new();
    for l in reply.lines() {
        let Some(rest) = investigate::cite::strip_word(l.trim(), "PART") else { continue };
        let mut fields = rest.split('|').map(str::trim);
        let head = fields.next().unwrap_or_default();
        let (n, q) = head.split_once(char::is_whitespace).unwrap_or((head, ""));
        let Ok(n) = n.trim_end_matches(['.', ':']).parse::<usize>() else { continue };
        let question = match q.trim() {
            "" => fields.next().unwrap_or_default().to_string(),
            q => q.to_string(),
        };
        if question.is_empty() {
            continue;
        }
        let named: Vec<String> = fields
            .next()
            .unwrap_or_default()
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        let symbols = if named.is_empty() { symbols.to_vec() } else { named };
        out.push(Part { question, symbols, rung: n.clamp(1, rungs) - 1 });
    }
    out.truncate(MAX_PARTS);
    out
}

// The prompt a hosted executor gets: the required lines, the question, the scope, the starting symbols and what earlier attempts established.
fn executor_prompt(question: &str, scope: &[String], symbols: &[String], context: &[String]) -> String {
    let mut p = format!("Answer one question about the repository in the current directory. Read the files you need; change nothing. Be concise: reply only with these lines, no prose:\n{LINES}\n\nQuestion: {question}\nScope (stay inside): {}\n", scope.join(", "));
    if !symbols.is_empty() {
        p.push_str(&format!("Starting symbols: {}\n", symbols.join(", ")));
    }
    if !context.is_empty() {
        p.push_str("Established by an earlier attempt (build on it; do not repeat it):\n");
        p.push_str(&context.iter().map(|c| format!("- {c}\n")).collect::<String>());
    }
    p
}

// Runs one part from its rung, escalating to the next rung on a trigger (keeping what was established) until it settles, is blocked on the user, reaches the top or runs out of time.
#[allow(clippy::too_many_arguments)]
fn part(
    ctx: &Ctx,
    b: &dyn Backend,
    log: &Log,
    plan: &Plan,
    scope: &[PathBuf],
    ladder: &[Rung],
    p: &Part,
    n: usize,
    deadline: Instant,
) -> Vec<Attempt> {
    let (mut idx, mut question, mut context, mut attempts) =
        (p.rung, p.question.clone(), Vec::<String>::new(), Vec::new());
    loop {
        let r = &ladder[idx];
        let role = if attempts.is_empty() { format!("part P{n}") } else { format!("escalation P{n}") };
        let work = match r.hosted() {
            _ if crate::budget::over(deadline) => {
                Work::failed(&format!("{} before it started", crate::budget::why_over()))
            }
            true => match log.hosted(
                b,
                r,
                &Ask::new(
                    &role,
                    "",
                    &executor_prompt(&question, &plan.scope, &p.symbols, &context),
                    &[("task", &question), ("context", &context.join("\n"))],
                ),
                &ctx.cwd,
                deadline,
            ) {
                Ok(s) => Work::from_reply(&ctx.cwd, scope, &s.text),
                Err(e) => Work::failed(&e),
            },
            false => {
                let mut argv = vec![question.clone()];
                argv.extend(plan.scope.iter().flat_map(|s| ["--path".to_string(), s.clone()]));
                argv.extend(p.symbols.iter().flat_map(|s| ["--symbol".to_string(), s.clone()]));
                argv.extend(["--steps".to_string(), plan.steps.to_string()]);
                argv.extend(["--seconds".to_string(), plan.seconds.to_string()]);
                argv.extend(context.iter().flat_map(|c| ["--context".to_string(), c.clone()]));
                match investigate::explore(ctx, &argv, &OnRung { b, rung: r, log, role: &role }, deadline) {
                    Ok(e) => investigate::work(ctx, &e),
                    Err(e) => Work::failed(&e),
                }
            }
        };
        let trigger = work.trigger();
        let stuck = match &trigger {
            None => None,
            Some(_) if idx + 1 >= ladder.len() => Some("it was the top of the ladder"),
            Some(_) if attempts.len() >= MAX_ESCALATIONS => {
                Some("the part was escalated as often as allowed")
            }
            Some(_) if crate::budget::over(deadline) => Some(crate::budget::why_over()),
            Some(_) => None,
        };
        match (trigger, stuck) {
            (Some(t), None) => {
                context = work.handoff(&r.label(), &t);
                question = work.escalate.clone().unwrap_or(question);
                attempts.push(Attempt { rung: r.clone(), work, why: Some(t) });
                idx += 1;
            }
            (t, stuck) => {
                let why = t.map(|t| format!("{t}; not escalated: {}", stuck.unwrap_or_default()));
                attempts.push(Attempt { rung: r.clone(), work, why });
                return attempts;
            }
        }
    }
}

// One line on how a part went: each rung it ran on and why it moved up, then how the last attempt ended.
fn story(attempts: &[Attempt]) -> String {
    let mut s = String::new();
    for (i, a) in attempts.iter().enumerate() {
        match (&a.why, i + 1 == attempts.len()) {
            (Some(w), false) => s.push_str(&format!("{} ({w}) → ", a.rung.label())),
            (Some(w), true) => s.push_str(&format!("{}: {}; {w}", a.rung.label(), a.work.summary())),
            (None, _) => s.push_str(&format!("{}: {}", a.rung.label(), a.work.summary())),
        }
    }
    s
}

// Combines the parts' results with the planner: its claims must cite lines a part verified, else they are rejected; when it fails or verifies nothing, the parts' findings are merged without it.
#[allow(clippy::too_many_arguments)]
fn combine(
    b: &dyn Backend,
    log: &Log,
    plan: &Plan,
    cwd: &Path,
    scope: &[PathBuf],
    parts: &[Work],
    r: &Rung,
    deadline: Instant,
) -> (Work, String) {
    let listing: String =
        parts.iter().enumerate().map(|(i, w)| format!("Part {}:\n{}", i + 1, w.listing("F", 0))).collect();
    let system = format!("You combine the checked results of the parts of one task into its answer. Use only the findings listed, citing them with their path:A-B exactly. Be concise; reply only with lines:\nANSWER\n{LINES}");
    let prompt = format!("Question: {}\n\n{listing}", plan.question);
    let merged = merge(parts);
    let reply = match log.text(
        b,
        r,
        &Ask::new("combine", &system, &prompt, &[("brief", &system), ("findings", &prompt)]),
        cwd,
        deadline,
    ) {
        Ok(t) => t,
        Err(e) => return (merged, format!(" (the parts merged; combining with {} failed: {e})", r.label())),
    };
    let mut w = Work::from_reply(cwd, scope, &reply);
    let known: Vec<&investigate::Found> = parts.iter().flat_map(|p| &p.findings).collect();
    let (kept, strays): (Vec<_>, Vec<_>) = w
        .findings
        .drain(..)
        .partition(|f| known.iter().any(|k| k.path == f.path && overlaps(&k.cite, &f.cite)));
    w.findings = kept;
    w.rejected.extend(strays.iter().map(|f| format!("{} {}: no part verified these lines", f.cite, f.claim)));
    match w.findings.is_empty() {
        true => (merged, format!(" (the parts merged; {} verified nothing from them)", r.label())),
        false => {
            w.gaps.extend(merged.gaps.into_iter().filter(|g| g.starts_with("the run stopped")));
            (w, format!(" (combined by {})", r.label()))
        }
    }
}

// Tells whether two citations `path:A-B` of one file share a line.
fn overlaps(a: &str, b: &str) -> bool {
    let range = |c: &str| {
        let (_, r) = c.rsplit_once(':')?;
        let (x, y) = r.split_once('-').unwrap_or((r, r));
        Some((x.parse::<usize>().ok()?, y.parse::<usize>().ok()?))
    };
    matches!((range(a), range(b)), (Some((a1, a2)), Some((b1, b2))) if a1 <= b2 && b1 <= a2)
}

// Merges the parts' results without a model: findings once per citation, all gaps, the lowest levels, and complete only when every part was.
fn merge(parts: &[Work]) -> Work {
    let mut w = Work {
        status: "complete".into(),
        understanding: "high".into(),
        confidence: "high".into(),
        ..Work::default()
    };
    for p in parts {
        for f in &p.findings {
            if !w.findings.iter().any(|g| g.cite == f.cite) {
                w.findings.push(f.clone());
            }
        }
        w.rejected.extend(p.rejected.iter().cloned());
        w.gaps.extend(p.gaps.iter().cloned());
        w.understanding = work::lowest(&w.understanding, &p.understanding);
        w.confidence = work::lowest(&w.confidence, &p.confidence);
        w.blocked = w.blocked.clone().or(p.blocked.clone());
        w.escalate = w.escalate.clone().or(p.escalate.clone());
        w.turns += p.turns;
        if p.status != "complete" {
            w.status = "partial".into();
        }
    }
    if w.findings.is_empty() {
        w.status = "failed".into();
    }
    w
}

// Has the cheapest rung write a short summary of the checked result that keeps its [n] markers and its uncertainty.
fn summary(
    b: &dyn Backend,
    log: &Log,
    plan: &Plan,
    cwd: &Path,
    r: &Rung,
    w: &Work,
    deadline: Instant,
) -> String {
    let facts: String = w
        .findings
        .iter()
        .enumerate()
        .map(|(i, f)| format!("[{}] {} ({})\n", i + 1, f.claim, f.cite))
        .collect();
    let gaps: String = w.gaps.iter().map(|g| format!("- {g}\n")).collect();
    let system = "Write two to five plain sentences that answer the question from the checked findings given. Keep each [n] marker after the claim it supports, say what is uncertain or missing, and add nothing the findings do not show.";
    let prompt = format!("Question: {}\nFindings:\n{facts}Not established:\n{gaps}", plan.question);
    match log.text(
        b,
        r,
        &Ask::new("summary", system, &prompt, &[("brief", system), ("findings", &prompt)]),
        cwd,
        deadline,
    ) {
        Ok(t) => format!("summary (by {}):\n{}\n", r.label(), t.trim()),
        Err(e) => format!("summary: {} failed: {e}\n", r.label()),
    }
}

// A reviewer's judgement: each result's quality, each finding's verdict, what each lacks, which is better, its own levels, or why it failed.
#[derive(Debug, Default, PartialEq)]
struct Review {
    rung: String,
    quality: Vec<(String, String, String)>,
    findings: Vec<(String, String, String)>,
    missing: Vec<(String, String)>,
    compared: Vec<(String, String, String)>,
    uncompared: Vec<&'static str>,
    better: Option<(String, String)>,
    understanding: String,
    confidence: String,
    error: Option<String>,
}

// Has the reviewer judge the main result (A) and the benchmark's (B) when there is one: quality, each finding and what is missing, and with a benchmark a comparison on each of quality, wall-clock time and tokens (from what zazzle measured) and which is better overall.
#[allow(clippy::too_many_arguments)]
fn review(
    b: &dyn Backend,
    r: &Rung,
    plan: &Plan,
    cwd: &Path,
    main: &Run,
    bench: Option<&Run>,
    deadline: Instant,
    sink: &Sink,
) -> Review {
    let log = Log::to(sink.clone());
    let reads = if r.hosted() { " You may read the files to check them; change nothing." } else { "" };
    let both = bench.map_or("", |_| "MISSING B ...\nQUALITY B ...\nFINDING B1 ...\nCOMPARE quality A|B|tie one-line reason (which answers the question more correctly and completely, checked against the files)\nCOMPARE time A|B|tie one-line reason (from the wall-clock seconds measured below)\nCOMPARE tokens A|B|tie one-line reason (from the tokens and cost measured below)\nBETTER A|B|tie one-line reason weighing quality, time and tokens together\n");
    let system = format!("You review the results of answering a question about the repository in the current directory.{reads} Be concise; reply only with lines:\nQUALITY A good|fair|poor one-line reason\nFINDING A1 supported|unsupported|unclear one-line reason (one per finding)\nMISSING A what the question needs that the result lacks\n{both}CONFIDENCE understanding=high|medium|low result=high|medium|low");
    let mut prompt = format!("Question: {}\n\nResult A:\n{}", plan.question, main.work.listing("A", 6));
    if let Some(bn) = bench {
        prompt.push_str(&format!("\nResult B:\n{}", bn.work.listing("B", 6)));
        prompt.push_str(&format!(
            "\nMeasured by zazzle, not estimated:\nA ({}): {}\nB ({}): {}\n",
            main.label,
            measured(main),
            bn.label,
            measured(bn)
        ));
    }
    match log.text(
        b,
        r,
        &Ask::new("review", &system, &prompt, &[("brief", &system), ("findings", &prompt)]),
        cwd,
        deadline,
    ) {
        Ok(t) => parse_review(&r.label(), &t, bench.is_some()),
        Err(e) => Review { rung: r.label(), error: Some(e), ..Review::default() },
    }
}

// What zazzle measured of a run for the reviewer: wall-clock seconds, calls, tokens (with any left unknown) and cost.
fn measured(run: &Run) -> String {
    let u = calls::usage(&run.calls, run.secs);
    let unknown = match u["uncounted_calls"].as_u64().unwrap_or(0) {
        0 => String::new(),
        n => format!(" (unknown for {n} call(s))"),
    };
    let cost = u["cost_usd"].as_f64().map_or("not reported".to_string(), |c| format!("${c:.4}"));
    format!(
        "{} s wall clock, {} calls, {} input, {} cached and {} output tokens{unknown}, cost {cost}",
        u["seconds"],
        u["calls"],
        u["hosted"]["input_tokens"],
        u["hosted"]["cached_tokens"],
        u["output_tokens"]
    )
}

// The axes a benchmark's reviewer must compare the two results on.
const AXES: [&str; 3] = ["quality", "time", "tokens"];

// Reads a review's lines; with a benchmark, notes each of quality, time and tokens the reviewer did not compare.
fn parse_review(rung: &str, reply: &str, with_bench: bool) -> Review {
    let mut v = Review { rung: rung.into(), ..Review::default() };
    let split = |s: &str| {
        let mut w = s.splitn(3, char::is_whitespace);
        (
            w.next().unwrap_or_default().to_string(),
            w.next().unwrap_or_default().to_lowercase(),
            w.next().unwrap_or_default().trim().to_string(),
        )
    };
    for l in reply.lines().map(str::trim) {
        let word = |w: &str| investigate::cite::strip_word(l, w);
        if let Some(rest) = word("QUALITY") {
            v.quality.push(split(rest));
        } else if let Some(rest) = word("FINDING") {
            v.findings.push(split(rest));
        } else if let Some(rest) = word("MISSING") {
            let (tag, text) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
            v.missing.push((tag.to_string(), text.trim().to_string()));
        } else if let Some(rest) = word("COMPARE") {
            v.compared.push(split(rest));
        } else if let Some(rest) = word("BETTER") {
            let (tag, text) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
            v.better = Some((tag.to_string(), text.trim().to_string()));
        }
    }
    let a = investigate::cite::parse_answer(reply).unwrap_or_default();
    (v.understanding, v.confidence) = (a.understanding, a.confidence);
    if with_bench {
        v.uncompared = AXES
            .into_iter()
            .filter(|x| !v.compared.iter().any(|(axis, ..)| axis.eq_ignore_ascii_case(x)))
            .collect();
    }
    if v.quality.is_empty() && v.findings.is_empty() {
        v.error = Some(format!(
            "the review had none of the required lines (it began: {})",
            agent::one_line(&reply.chars().take(160).collect::<String>())
        ));
    }
    v
}

// Renders the review for the text report.
fn review_text(v: &Review) -> String {
    let mut out = format!("review (by {}):\n", v.rung);
    if let Some(e) = &v.error {
        return format!("{out}- failed: {e}\n");
    }
    for (tag, q, why) in &v.quality {
        out.push_str(&format!("- {tag} quality {q}: {why}\n"));
    }
    for (id, verdict, why) in &v.findings {
        out.push_str(&format!("- finding {id} {verdict}: {why}\n"));
    }
    for (tag, what) in &v.missing {
        out.push_str(&format!("- {tag} misses: {what}\n"));
    }
    for (axis, tag, why) in &v.compared {
        out.push_str(&format!("- {axis}: {}: {why}\n", tag.to_uppercase()));
    }
    if !v.uncompared.is_empty() {
        out.push_str(&format!("- not compared by the reviewer: {}\n", v.uncompared.join(", ")));
    }
    if let Some((tag, why)) = &v.better {
        out.push_str(&format!("- better: {tag}: {why}\n"));
    }
    out
}

// The benchmark's side-by-side table: state, findings, rejections, gaps, usage and the files both cited.
fn compare(main: &Run, bench: &Run) -> String {
    let (a, b) = (calls::usage(&main.calls, main.secs), calls::usage(&bench.calls, bench.secs));
    let files =
        |w: &Work| w.findings.iter().map(|f| f.path.clone()).collect::<std::collections::BTreeSet<_>>();
    let (fa, fb) = (files(&main.work), files(&bench.work));
    let row = |k: &str, x: String, y: String| format!("  {k:<14} {x:<22} {y}\n");
    let mut out = format!("benchmark: {} against {} alone\n", main.label, bench.label);
    out.push_str(&row("", main.label.clone(), bench.label.clone()));
    out.push_str(&row("state", main.work.status.clone(), bench.work.status.clone()));
    out.push_str(&row(
        "verified",
        main.work.findings.len().to_string(),
        bench.work.findings.len().to_string(),
    ));
    out.push_str(&row(
        "rejected",
        main.work.rejected.len().to_string(),
        bench.work.rejected.len().to_string(),
    ));
    out.push_str(&row("gaps", main.work.gaps.len().to_string(), bench.work.gaps.len().to_string()));
    out.push_str(&row("calls", a["calls"].to_string(), b["calls"].to_string()));
    out.push_str(&row("wall seconds", a["seconds"].to_string(), b["seconds"].to_string()));
    out.push_str(&row(
        "input tokens",
        a["hosted"]["input_tokens"].to_string(),
        b["hosted"]["input_tokens"].to_string(),
    ));
    out.push_str(&row(
        "cached tokens",
        a["hosted"]["cached_tokens"].to_string(),
        b["hosted"]["cached_tokens"].to_string(),
    ));
    out.push_str(&row("output tokens", a["output_tokens"].to_string(), b["output_tokens"].to_string()));
    let cost = |v: &Value| v["cost_usd"].as_f64().map_or("-".to_string(), |c| format!("${c:.4}"));
    out.push_str(&row("cost", cost(&a), cost(&b)));
    out.push_str(&format!(
        "  cited files in common: {} of {}\n",
        fa.intersection(&fb).count(),
        fa.union(&fb).count()
    ));
    out.push_str(&format!("{} alone:\n{}", bench.label, bench.text));
    out
}

// The text report: the main run with its usage, then the benchmark and the review.
fn text(main: &Run, bench: Option<&Run>, review: Option<&Review>) -> String {
    let mut out = main.text.trim_end().to_string();
    out.push_str(&format!("\nusage: {}\n", calls::usage_line(&main.calls, main.secs)));
    out.push_str(&calls::phases(&main.calls, main.secs));
    if let Some(b) = bench {
        out.push_str(&compare(main, b));
        out.push_str(&format!("usage ({}): {}\n", b.label, calls::usage_line(&b.calls, b.secs)));
        out.push_str(&calls::phases(&b.calls, b.secs));
    }
    if let Some(v) = review {
        out.push_str(&review_text(v));
    }
    out
}

// The JSON report: the main result (with the plan and parts for an adaptive run), its usage, the benchmark and the review.
fn joined(plan: &Plan, main: &Run, bench: Option<&Run>, review: Option<&Review>) -> Value {
    let mut v = match main.json.get("result") {
        Some(r) => r.clone(),
        None => main.json.clone(),
    };
    v["question"] = plan.question.clone().into();
    if main.json.get("result").is_some() {
        for k in ["difficulty", "planner", "parts"] {
            v[k] = main.json[k].clone();
        }
    }
    v["usage"] = calls::usage(&main.calls, main.secs);
    if let Some(b) = bench {
        v["benchmark"] = json!({"agent": b.label, "result": b.json, "usage": calls::usage(&b.calls, b.secs)});
    }
    if let Some(r) = review {
        let triple =
            |x: &[(String, String, String)]| x.iter().map(|(a, b, c)| json!([a, b, c])).collect::<Vec<_>>();
        v["review"] = json!({
            "agent": r.rung, "error": r.error, "quality": triple(&r.quality), "findings": triple(&r.findings),
            "missing": r.missing.iter().map(|(a, b)| json!([a, b])).collect::<Vec<_>>(),
            "compared": triple(&r.compared), "not_compared": r.uncompared,
            "better": r.better.as_ref().map(|(a, b)| json!([a, b])),
            "task_understanding": r.understanding, "result_confidence": r.confidence,
        });
    }
    v
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests;
