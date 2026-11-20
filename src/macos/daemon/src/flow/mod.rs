// `zazzle flow` and `zazzle review`: the repository workflow zazzle runs for an agent, opt-in per repository (`.zazzle/flow.json`): preview what applies, verify with tree-pinned receipts, write the commit message, changelog entry or pull request text, deliver (stage, commit, push, pull request) safely repeatable, write a failing test first and then the fix, wait for CI once, and review from several viewpoints with fixes before handing back. They run as their own process, in the caller's environment.
pub mod audit;
pub mod ci;
pub mod coverage;
pub mod deliver;
pub mod docs;
pub mod edit;
pub mod generate;
pub mod headers;
pub mod policy;
pub mod repo;
pub mod review;
pub mod testfirst;
pub mod tidy;
pub mod update;
pub mod verify;
pub mod write;

use crate::adaptive::agent::{self, Rung};
use crate::adaptive::calls::{self, Backend, Log};
use policy::Policy;
use std::path::Path;
use std::time::{Duration, Instant};

pub const USAGE: &str = "usage: zazzle flow preview|init [--write]|verify|tidy|coverage|audit|generate|headers|update|merge|write commit|changelog|pr|docs|deliver|test-first|ci .. (zazzle flow help)";
// The help text.
pub const HELP: &str = "zazzle flow: the repository workflow, opt-in with .zazzle/flow.json
  preview [--base B]                 what applies here: checks, target branch and level, effects, agents, conflicts in the guidance
  init [--write]                     a starter .zazzle/flow.json from what zazzle finds (printed; --write saves it)
  verify [--only NAME].. [--all] [--timeout S] [--json]
                                     run the checks that apply to the change; the receipt is pinned to the tree
  write commit|changelog|pr --intent TEXT [--compat TEXT] [--writer AGENT] [--apply] [--base B]
                                     a cheap writer drafts it from the diff, the intent, the compatibility decision and the receipt
  write docs --intent TEXT [--paths P..] [--writer AGENT] [--base B]
                                     the cheap writer updates the READMEs and .dev records the change makes stale, checked
  deliver --paths P.. -m MESSAGE [--push] [--pr] [--draft] [--title T] [--body-file F] [--base B] [--require-verified]
                                     stage exactly those paths, commit, push, open or update the pull request; safe to repeat
  test-first --behavior TEXT --tests P.. --reference FILE --run CMD --expect TEXT [--prod P..] [--test-agent A] [--fix-agent A|--no-fix]
                                     A writes a test that fails for the stated reason, B makes it pass, zazzle verifies
  tidy [--all] [--base B] [--fixer AGENT] [--no-agent]
                                     run the formatters' and linters' fix commands, re-check, and give lint left over to the local agent
  coverage [--all] [--base B] [--author AGENT --tests P..]
                                     measure coverage and list the changed files' uncovered lines and branches with tests
                                     to model on; with --author a cheap agent writes tests for them, kept only if they pass
  audit [--all] [--fix] [--base B]   run the dependency audits that apply (a manifest or lockfile changed; --all: every one)
  generate [--all] [--check] [--base B]
                                     run the generators whose inputs changed; --check reports stale outputs and puts them back
  headers [--all] [--fix] [--base B] check the license header in the changed (or all) source files; --fix adds it
  update [--target B] [--resolver AGENT] [--reviewer AGENT] [--no-agent] [--no-review] [--no-verify] [--push]
                                     merge the target in: lockfiles and generated files regenerated, other conflicts to the local agent,
                                     a complex merge reviewed by the strong reviewer; committed only if it passes
  merge [--pr N] [--method squash|merge|rebase]
                                     merge the pull request when the policy allows it, its checks are green and no finding is open
  ci [--pr N] [--timeout S] [--triager AGENT] [--no-triage]
                                     wait for the pull request's checks on its head and report once; failed jobs' logs
                                     are cut to what matters and triaged by the local model
zazzle review [--into BRANCH] [--level feature|development|staging|production] [--base B] [--viewpoints V,..] [--intent TEXT]
              [--reviewers A,..] [--final A] [--fix-agent A] [--writer A] [--read-only] [--rounds N] [--bench] [--json]
                                     multi-viewpoint review at the level of the branch the change goes into (--into; default: this branch's,
                                     feature for a feature branch); confirmed findings are fixed, verified and re-reviewed unless --read-only
AGENT is MODEL (local), claude:MODEL or codex:MODEL, optionally @EFFORT; --seconds S limits each agent call (default 300).";

// What every execution agent is told it does not do: zazzle's tools and its cheap writer do those after it.
pub const TOOLS_NOTE: &str = "Do not format, lint, build, run tests, measure coverage or audit dependencies, do not run code generators, edit generated files or add license headers, do not merge branches or pull requests, and do not write documentation, changelogs, commit messages or pull request text, or wait for CI: zazzle runs those tools and its writer after you and sends back only what needs your judgment.";

// Agent calls' default time limit.
const AGENT_SECS: u64 = 300;

// Reads the flags of a subcommand: values of repeatable flags gathered, switches set, the rest positional.
struct Args {
    values: Vec<(String, String)>,
    switches: Vec<String>,
    words: Vec<String>,
}

impl Args {
    // Splits argv given which flags are switches.
    fn parse(argv: &[String], switches: &[&str]) -> Result<Args, String> {
        let mut a = Args { values: Vec::new(), switches: Vec::new(), words: Vec::new() };
        let mut it = argv.iter();
        while let Some(x) = it.next() {
            match x.as_str() {
                s if switches.contains(&s) => a.switches.push(s.to_string()),
                f if f.starts_with("--") || f == "-m" => {
                    let v = it.next().ok_or_else(|| format!("{f} needs a value"))?;
                    a.values.push((f.to_string(), v.clone()));
                }
                w => a.words.push(w.to_string()),
            }
        }
        Ok(a)
    }

    // The last value of a flag.
    fn one(&self, f: &str) -> Option<String> {
        self.values.iter().rev().find(|(k, _)| k == f).map(|(_, v)| v.clone())
    }

    // Every value of a flag, comma lists split.
    fn all(&self, f: &str) -> Vec<String> {
        self.values
            .iter()
            .filter(|(k, _)| k == f)
            .flat_map(|(_, v)| v.split(',').map(|s| s.trim().to_string()))
            .filter(|s| !s.is_empty())
            .collect()
    }

    // Whether a switch was given.
    fn on(&self, s: &str) -> bool {
        self.switches.iter().any(|x| x == s)
    }

    // Fails on a flag the subcommand does not take.
    fn only(&self, known: &[&str]) -> Result<(), String> {
        match self.values.iter().find(|(k, _)| !known.contains(&k.as_str())) {
            Some((k, _)) => Err(format!("unknown flag {k}; {USAGE}")),
            None => Ok(()),
        }
    }
}

// The agents a command uses: the ladder (cheapest first) from --agents, the policy or the installed local models, each spec one rung at its effort.
fn ladder(p: &Policy, a: &Args, b: &dyn Backend) -> Result<Vec<Rung>, String> {
    let specs = match a.all("--agents") {
        s if !s.is_empty() => s,
        _ => match p.agent_list("ladder") {
            s if !s.is_empty() => s,
            _ => b.installed(),
        },
    };
    let mut out: Vec<Rung> = specs.iter().map(|s| Rung::parse(s, None)).collect::<Result<_, _>>()?;
    if out.is_empty() {
        out.push(Rung::parse("", None)?);
    }
    Ok(out)
}

// One agent from a flag, else the policy's key, else the fallback.
fn one_agent(a: &Args, flag: &str, p: &Policy, key: &str, fallback: &Rung) -> Result<Rung, String> {
    match a.one(flag).or_else(|| p.agent(key)) {
        Some(s) => Rung::parse(&s, None),
        None => Ok(fallback.clone()),
    }
}

// The time limit of each agent call.
fn limit(a: &Args, flag: &str, default: u64) -> Result<Duration, String> {
    match a.one(flag) {
        Some(v) => v
            .parse::<u64>()
            .ok()
            .filter(|n| *n > 0)
            .map(Duration::from_secs)
            .ok_or_else(|| format!("{flag} takes a number of seconds, not `{v}`")),
        None => Ok(Duration::from_secs(default)),
    }
}

// Runs `flow ARGS` in `cwd` with the real agents and returns its text and exit status.
pub fn main(cwd: &Path, argv: &[String]) -> (String, i32) {
    run(cwd, argv, &calls::Real)
}

// Runs `flow ARGS` (or `review ARGS` as `flow review ARGS`) with the given agents.
pub fn run(cwd: &Path, argv: &[String], b: &dyn Backend) -> (String, i32) {
    match dispatch(cwd, argv, b) {
        Ok((text, code)) => (text, code),
        Err(e) => (format!("error: {e}"), 2),
    }
}

// Runs one subcommand.
fn dispatch(cwd: &Path, argv: &[String], b: &dyn Backend) -> Result<(String, i32), String> {
    let Some((cmd, rest)) = argv.split_first() else { return Err(USAGE.into()) };
    if matches!(cmd.as_str(), "help" | "--help" | "-h") || rest.iter().any(|a| a == "--help" || a == "-h") {
        return Ok((HELP.into(), 0));
    }
    let top = repo::top(cwd)?;
    let p = policy::load(&top)?;
    let log = Log::default();
    match cmd.as_str() {
        "preview" => {
            let a = Args::parse(rest, &[])?;
            a.only(&["--base"])?;
            Ok((preview(&top, &p, a.one("--base"), b)?, 0))
        }
        "init" => {
            let a = Args::parse(rest, &["--write"])?;
            a.only(&[])?;
            init(&top, &p, a.on("--write"))
        }
        "verify" => {
            let a = Args::parse(rest, &["--all", "--json"])?;
            a.only(&["--only", "--timeout", "--base"])?;
            let base = match (a.on("--all"), a.one("--base")) {
                (true, _) => None,
                (false, Some(bb)) => Some(bb),
                (false, None) => repo::base(&top, &p.target).ok(),
            };
            let r = verify::verify(
                &top,
                &p,
                base.as_deref(),
                &a.all("--only"),
                limit(&a, "--timeout", verify::CHECK_SECS)?,
            )?;
            let text = if a.on("--json") {
                serde_json::to_string_pretty(&r.json()).unwrap_or_default()
            } else {
                r.text()
            };
            Ok((text, if r.green() { 0 } else { 1 }))
        }
        "write" if rest.first().is_some_and(|w| w == "docs") => {
            let a = Args::parse(&rest[1..], &[])?;
            a.only(&["--intent", "--writer", "--base", "--seconds", "--paths", "--agents"])?;
            let ladder = ladder(&p, &a, b)?;
            let writer = one_agent(&a, "--writer", &p, "writer", &ladder[0])?;
            let base = match a.one("--base") {
                Some(bb) => bb,
                None => repo::base(&top, &p.target).unwrap_or_else(|_| "HEAD".into()),
            };
            let intent = a.one("--intent").unwrap_or_default();
            let started = Instant::now();
            let (mut out, ok) = docs::update(
                &top,
                &p,
                &intent,
                &base,
                &a.all("--paths"),
                b,
                &writer,
                &log,
                limit(&a, "--seconds", AGENT_SECS)?,
            )?;
            out.push_str(&calls::phases(&log.calls(), started.elapsed().as_secs_f64()));
            Ok((out, if ok { 0 } else { 1 }))
        }
        "write" => {
            let a = Args::parse(rest, &["--apply"])?;
            a.only(&["--intent", "--compat", "--writer", "--base", "--seconds"])?;
            let kind = match a.words.first().map(String::as_str) {
                Some("commit") => write::Kind::Commit,
                Some("changelog") => write::Kind::Changelog,
                Some("pr") => write::Kind::Pr,
                _ => {
                    return Err(
                        "usage: zazzle flow write commit|changelog|pr|docs --intent TEXT [--compat TEXT]"
                            .into(),
                    )
                }
            };
            let ladder = ladder(&p, &a, b)?;
            let writer = one_agent(&a, "--writer", &p, "writer", &ladder[0])?;
            let base = match a.one("--base") {
                Some(bb) => bb,
                None => repo::base(&top, &p.target).unwrap_or_else(|_| "HEAD".into()),
            };
            let intent = a.one("--intent").unwrap_or_default();
            let compat = a.one("--compat");
            let f = write::Facts { kind, intent: &intent, compat: compat.as_deref(), base: &base };
            let deadline = Instant::now() + limit(&a, "--seconds", AGENT_SECS)?;
            let (draft, left) = write::write(&top, &p, &f, b, &writer, &log, deadline)?;
            let mut out = draft.clone();
            if !left.is_empty() {
                out.push_str(&format!(
                    "\n-- not valid after a second draft by {}: {}\n",
                    writer.label(),
                    left.join("; ")
                ));
                return Ok((out, 1));
            }
            if a.on("--apply") {
                out = apply(&top, &p, kind, &draft)?;
            }
            Ok((out, 0))
        }
        "tidy" => {
            let a = Args::parse(rest, &["--all", "--no-agent"])?;
            a.only(&["--base", "--fixer", "--agents", "--seconds"])?;
            let ladder = ladder(&p, &a, b)?;
            let fixer = one_agent(&a, "--fixer", &p, "lint", &ladder[0])?;
            let base = match (a.on("--all"), a.one("--base")) {
                (true, _) => None,
                (false, Some(bb)) => Some(bb),
                (false, None) => repo::base(&top, &p.target).ok(),
            };
            let started = Instant::now();
            let fixer = (!a.on("--no-agent")).then_some(&fixer);
            let (mut out, ok) =
                tidy::tidy(&top, &p, base.as_deref(), b, &log, fixer, limit(&a, "--seconds", AGENT_SECS)?)?;
            out.push_str(&calls::phases(&log.calls(), started.elapsed().as_secs_f64()));
            Ok((out, if ok { 0 } else { 1 }))
        }
        "deliver" => {
            let a = Args::parse(rest, &["--push", "--pr", "--draft", "--require-verified"])?;
            a.only(&["--paths", "-m", "--title", "--body-file", "--base"])?;
            let ask = deliver::Ask {
                paths: a.all("--paths").into_iter().chain(a.words.iter().cloned()).collect(),
                message: a.one("-m").unwrap_or_default(),
                push: a.on("--push") || a.on("--pr"),
                pr: a.on("--pr"),
                draft: a.on("--draft"),
                title: a.one("--title"),
                body: a.one("--body-file").map(|f| top.join(f)),
                base: a.one("--base"),
                require_verified: a.on("--require-verified"),
            };
            deliver::deliver(&top, &p, &ask).map(|t| (t, 0))
        }
        "test-first" => {
            let a = Args::parse(rest, &["--no-fix"])?;
            a.only(&[
                "--behavior",
                "--tests",
                "--reference",
                "--run",
                "--expect",
                "--prod",
                "--test-agent",
                "--fix-agent",
                "--agents",
                "--seconds",
            ])?;
            let ladder = ladder(&p, &a, b)?;
            let need = |f: &str| a.one(f).ok_or_else(|| format!("test-first needs {f}"));
            let spec = testfirst::Spec {
                behavior: need("--behavior")?,
                tests: a.all("--tests"),
                reference: need("--reference")?,
                run: need("--run")?,
                expect: need("--expect")?,
                prod: a.all("--prod"),
                test_agent: one_agent(&a, "--test-agent", &p, "tests", &ladder[0])?,
                fix_agent: match a.on("--no-fix") {
                    true => None,
                    false => Some(one_agent(&a, "--fix-agent", &p, "fix", ladder.last().expect("one rung"))?),
                },
                limit: limit(&a, "--seconds", AGENT_SECS)?,
            };
            let started = Instant::now();
            let mut out = testfirst::run(&top, &p, &spec, b, &log)?;
            out.push_str(&calls::phases(&log.calls(), started.elapsed().as_secs_f64()));
            let code = if out.contains("-- done:") || out.contains("-- test accepted") { 0 } else { 1 };
            Ok((out, code))
        }
        "ci" => {
            let a = Args::parse(rest, &["--no-triage"])?;
            a.only(&["--pr", "--timeout", "--triager", "--agents", "--seconds"])?;
            let (mut text, green, failed) =
                ci::wait(&top, &a.one("--pr").unwrap_or_default(), limit(&a, "--timeout", 3600)?)?;
            if !failed.is_empty() && !a.on("--no-triage") {
                let ladder = ladder(&p, &a, b)?;
                let r = one_agent(&a, "--triager", &p, "triage", &ladder[0])?;
                let changed = repo::base(&top, &p.target)
                    .ok()
                    .and_then(|bb| repo::changed(&top, &bb, "HEAD").ok())
                    .unwrap_or_default();
                text.push_str(&ci::triage(
                    &top,
                    &failed,
                    &changed,
                    b,
                    &log,
                    &r,
                    limit(&a, "--seconds", AGENT_SECS)?,
                ));
            }
            Ok((text, if green { 0 } else { 1 }))
        }
        "audit" => {
            let a = Args::parse(rest, &["--all", "--fix"])?;
            a.only(&["--base", "--timeout"])?;
            let base = match (a.on("--all"), a.one("--base")) {
                (true, _) => None,
                (false, Some(bb)) => Some(bb),
                (false, None) => repo::base(&top, &p.target).ok(),
            };
            let (out, ok) = audit::audit(
                &top,
                &p,
                base.as_deref(),
                a.on("--fix"),
                limit(&a, "--timeout", verify::CHECK_SECS)?,
            )?;
            Ok((out, if ok { 0 } else { 1 }))
        }
        "update" => {
            let a = Args::parse(rest, &["--no-verify", "--push", "--no-agent", "--no-review"])?;
            a.only(&["--target", "--resolver", "--reviewer", "--agents", "--seconds"])?;
            let ladder = ladder(&p, &a, b)?;
            let resolver = one_agent(&a, "--resolver", &p, "resolver", &ladder[0])?;
            let reviewer = one_agent(&a, "--reviewer", &p, "merge_review", ladder.last().expect("one rung"))?;
            let ask = update::Ask {
                target: a.one("--target"),
                resolver: (!a.on("--no-agent")).then_some(resolver),
                reviewer: (!a.on("--no-review")).then_some(reviewer),
                no_verify: a.on("--no-verify"),
                push: a.on("--push"),
                limit: limit(&a, "--seconds", AGENT_SECS)?,
            };
            let started = Instant::now();
            let (mut out, ok) = update::update(&top, &p, &ask, b, &log)?;
            out.push_str(&calls::phases(&log.calls(), started.elapsed().as_secs_f64()));
            Ok((out, if ok { 0 } else { 1 }))
        }
        "generate" => {
            let a = Args::parse(rest, &["--all", "--check"])?;
            a.only(&["--base", "--timeout"])?;
            let base = match (a.on("--all"), a.one("--base")) {
                (true, _) => None,
                (false, Some(bb)) => Some(bb),
                (false, None) => repo::base(&top, &p.target).ok(),
            };
            let (out, ok) = generate::generate(
                &top,
                &p,
                base.as_deref(),
                a.on("--check"),
                limit(&a, "--timeout", verify::CHECK_SECS)?,
            )?;
            Ok((out, if ok { 0 } else { 1 }))
        }
        "headers" => {
            let a = Args::parse(rest, &["--all", "--fix"])?;
            a.only(&["--base"])?;
            let base = match (a.on("--all"), a.one("--base")) {
                (true, _) => None,
                (false, Some(bb)) => Some(bb),
                (false, None) => repo::base(&top, &p.target).ok(),
            };
            let (out, ok) = headers::headers(&top, &p, base.as_deref(), a.on("--fix"))?;
            Ok((out, if ok { 0 } else { 1 }))
        }
        "merge" => {
            let a = Args::parse(rest, &[])?;
            a.only(&["--pr", "--method", "--timeout"])?;
            let method = a.one("--method").unwrap_or_else(|| "squash".into());
            deliver::merge_pr(
                &top,
                &p,
                &a.one("--pr").unwrap_or_default(),
                &method,
                limit(&a, "--timeout", 3600)?,
            )
            .map(|t| (t, 0))
        }
        "coverage" => {
            let a = Args::parse(rest, &["--all"])?;
            a.only(&["--base", "--author", "--tests", "--agents", "--seconds"])?;
            let base = match (a.on("--all"), a.one("--base")) {
                (true, _) => None,
                (false, Some(bb)) => Some(bb),
                (false, None) => repo::base(&top, &p.target).ok(),
            };
            let author = match a.one("--author") {
                Some(s) => Some(Rung::parse(&s, None)?),
                None => None,
            };
            let started = Instant::now();
            let (mut out, ok) = coverage::run(
                &top,
                &p,
                base.as_deref(),
                author.as_ref(),
                &a.all("--tests"),
                b,
                &log,
                limit(&a, "--seconds", AGENT_SECS)?,
            )?;
            out.push_str(&calls::phases(&log.calls(), started.elapsed().as_secs_f64()));
            Ok((out, if ok { 0 } else { 1 }))
        }
        "review" => {
            let a = Args::parse(rest, &["--read-only", "--bench", "--json"])?;
            a.only(&[
                "--level",
                "--into",
                "--base",
                "--viewpoints",
                "--intent",
                "--reviewers",
                "--final",
                "--fix-agent",
                "--writer",
                "--rounds",
                "--agents",
                "--seconds",
            ])?;
            let ladder = ladder(&p, &a, b)?;
            let reviewers: Vec<Rung> = match a.all("--reviewers") {
                r if !r.is_empty() => r.iter().map(|s| Rung::parse(s, None)).collect::<Result<_, _>>()?,
                _ => match p.agent_list("reviewers") {
                    r if !r.is_empty() => r.iter().map(|s| Rung::parse(s, None)).collect::<Result<_, _>>()?,
                    _ => ladder.clone(),
                },
            };
            let top_rung = ladder.last().expect("one rung").clone();
            let rounds = match a.one("--rounds") {
                Some(n) => n
                    .parse::<usize>()
                    .ok()
                    .filter(|n| *n <= 5)
                    .ok_or_else(|| format!("--rounds takes 0 to 5, not `{n}`"))?,
                None => 2,
            };
            let o = review::Opts {
                base: a.one("--base"),
                into: a.one("--into"),
                level: a.one("--level"),
                viewpoints: a.all("--viewpoints"),
                intent: a
                    .one("--intent")
                    .unwrap_or_else(|| "not given: infer it from the diff and say so".into()),
                reviewers,
                planner: ladder[0].clone(),
                last: one_agent(&a, "--final", &p, "final", &top_rung)?,
                fixer: one_agent(&a, "--fix-agent", &p, "fix", &top_rung)?,
                writer: one_agent(&a, "--writer", &p, "writer", &ladder[0])?,
                read_only: a.on("--read-only"),
                rounds,
                limit: limit(&a, "--seconds", AGENT_SECS)?,
                bench: a.on("--bench"),
            };
            let out = review::run(&top, &p, &o, b, &log, a.on("--json"))?;
            let code = if out.contains("still open") || out.contains("checks not green") { 1 } else { 0 };
            Ok((out, code))
        }
        _ => Err(USAGE.into()),
    }
}

// Applies a validated draft: a changelog entry goes under Unreleased (and the changelog script checks it), a pull request body is saved for `deliver --body-file`, a commit message is saved for `deliver -m "$(cat FILE)"`.
fn apply(top: &Path, p: &Policy, kind: write::Kind, draft: &str) -> Result<String, String> {
    match kind {
        write::Kind::Changelog => {
            let file = p.changelog.clone().unwrap_or_else(|| "CHANGELOG.md".into());
            let old = std::fs::read_to_string(top.join(&file)).unwrap_or_else(|_| "# Changelog\n".into());
            std::fs::write(top.join(&file), write::insert(&old, draft))
                .map_err(|e| format!("{file}: {e}"))?;
            let mut out = format!(
                "{draft}\n-- added under Unreleased in {file}; commit it with the change it describes\n"
            );
            if let Some(script) = &p.changelog_script {
                let c = policy::Check {
                    name: "changelog".into(),
                    run: script.clone(),
                    dir: ".".into(),
                    kind: "script".into(),
                    paths: Vec::new(),
                    fix: None,
                    report: None,
                    outputs: Vec::new(),
                };
                let o = verify::run_check(
                    top,
                    &c,
                    &repo::state(top, "logs")?.join("changelog.log"),
                    Duration::from_secs(300),
                );
                out.push_str(&format!("changelog check `{script}`: {}\n", o.summary()));
            }
            Ok(out)
        }
        _ => {
            let name = if kind == write::Kind::Pr { "pr-body.md" } else { "commit-message.txt" };
            let path = repo::state(top, "drafts")?.join(name);
            std::fs::write(&path, draft).map_err(|e| format!("{}: {e}", path.display()))?;
            Ok(format!("{draft}\n-- saved to {}\n", path.display()))
        }
    }
}

// The read-only preview: what flow would do here and on whose authority.
fn preview(top: &Path, p: &Policy, base: Option<String>, b: &dyn Backend) -> Result<String, String> {
    let branch = repo::branch(top).unwrap_or_else(|e| e);
    let mut out = format!("repository: {}\n", top.display());
    out.push_str(&format!(
        "opted in: {}\n",
        if p.opted_in {
            format!("yes ({})", policy::FILE)
        } else {
            format!("no: verify and review work; deliver needs {} (zazzle flow init)", policy::FILE)
        }
    ));
    out.push_str(&format!(
        "branch: {branch} ({} level{})\n",
        p.level(&branch),
        if p.is_protected(&branch) { ", protected: deliver refuses it" } else { "" }
    ));
    out.push_str(&format!(
        "pull requests target: {} (from {}; reviewed at the {} level)\n",
        p.target,
        p.target_from,
        p.level(&p.target)
    ));
    let base = base.or_else(|| repo::base(top, &p.target).ok());
    let tree = repo::worktree_tree(top)?;
    let changed = match &base {
        Some(bb) => repo::changed(top, bb, &tree).unwrap_or_default(),
        None => Vec::new(),
    };
    out.push_str(&format!(
        "change: {} file(s) against {}\n",
        changed.len(),
        base.as_deref().map_or("no base".to_string(), |bb| verify::short(bb).to_string())
    ));
    out.push_str("checks:\n");
    if p.checks.is_empty() {
        out.push_str("  none found: add them to .zazzle/flow.json\n");
    }
    let applicable = p.applicable(&changed);
    for c in &p.checks {
        let on = applicable.iter().any(|x| x.name == c.name);
        out.push_str(&format!(
            "  {} {} ({}): `{}` in {}\n",
            if on { "+" } else { "-" },
            c.name,
            c.kind,
            c.run,
            c.dir
        ));
    }
    out.push_str(&format!(
        "verification of the tree as it stands: {}\n",
        verify::receipt_for(top, &tree)
            .map_or_else(|| format!("none for tree {}", verify::short(&tree)), |r| r.line())
    ));
    out.push_str(&format!(
        "effects deliver may have: {}\n",
        if p.effects.is_empty() { "none (not opted in)".to_string() } else { p.effects.join(", ") }
    ));
    out.push_str(&format!(
        "changelog: {}; pull request template: {}\n",
        p.changelog.as_deref().unwrap_or("none"),
        p.pr_template.as_deref().unwrap_or("none")
    ));
    let installed = b.installed();
    out.push_str(&format!(
        "local models: {}\n",
        if installed.is_empty() {
            "none installed (zazzle agent llm)".to_string()
        } else {
            installed.join(", ")
        }
    ));
    let hosted: Vec<String> = [agent::Kind::Claude, agent::Kind::Codex]
        .iter()
        .filter_map(|k| agent::program(*k).ok().map(|p| p.display().to_string()))
        .collect();
    out.push_str(&format!(
        "hosted agents found: {}\n",
        if hosted.is_empty() { "none".to_string() } else { hosted.join(", ") }
    ));
    if !p.agents.is_null() {
        out.push_str(&format!("configured agents: {}\n", p.agents));
    }
    for c in &p.conflicts {
        out.push_str(&format!("conflict: {c}\n"));
    }
    out.push_str("-- preview only: nothing was run or changed\n");
    Ok(out)
}

// The starter opt-in file: the discovered checks, the target branch and the usual levels.
fn init(top: &Path, p: &Policy, save: bool) -> Result<(String, i32), String> {
    let checks: Vec<serde_json::Value> = p.checks.iter().map(|c| serde_json::json!({"name": c.name, "run": c.run, "dir": c.dir, "kind": c.kind, "paths": c.paths})).collect();
    let v = serde_json::json!({
        "checks": checks,
        "branches": {"target": p.target, "levels": {"main": "production"}, "protected": [p.target]},
        "changelog": {"path": p.changelog.clone().unwrap_or_else(|| "CHANGELOG.md".into())},
        "effects": ["commit", "push", "pr"],
        "agents": {"ladder": [], "writer": null, "reviewers": [], "final": null, "fix": null, "tests": null},
    });
    let text = serde_json::to_string_pretty(&v).unwrap_or_default() + "\n";
    let mut out = text.clone();
    for c in &p.conflicts {
        out.push_str(&format!("conflict to settle in the file: {c}\n"));
    }
    if !save {
        out.push_str(&format!("-- not saved: run with --write to save it as {}\n", policy::FILE));
        return Ok((out, 0));
    }
    let path = top.join(policy::FILE);
    if path.exists() {
        return Err(format!("{} already exists; edit it instead", policy::FILE));
    }
    std::fs::create_dir_all(path.parent().expect("has a folder")).map_err(|e| format!("{e}"))?;
    std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    out.push_str(&format!("-- saved {}; review it, then commit it\n", policy::FILE));
    Ok((out, 0))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests;
