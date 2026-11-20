// `zazzle flow test-first`: agent A writes a real regression test inside the allowed test paths, modeled on a reference test; zazzle runs the focused command and accepts the test only when it compiles and fails for the stated reason (not a placeholder); agent B then changes production code only, the accepted test read-only, until the same command passes; zazzle runs the broader checks and keeps the test patch and both receipts.
use super::edit::{self, FILES};
use super::policy::{Check, Policy};
use super::repo;
use super::verify::{self, Outcome};
use crate::adaptive::agent::Rung;
use crate::adaptive::calls::{Backend, Log};
use std::path::Path;
use std::time::{Duration, Instant};

// Output that means the test did not compile or load, so its failure proves nothing about behavior.
const BROKEN: &[&str] = &[
    "error[E",
    "error CS",
    "error TS",
    "SyntaxError",
    "cannot find symbol",
    "could not compile",
    "Build FAILED",
    "ModuleNotFoundError",
    "ImportError",
    "undefined: ",
];
// Added test lines that make a test fail without asserting anything.
const PLACEHOLDERS: &[&str] = &[
    "NotImplementedException",
    "todo!(",
    "unimplemented!(",
    "TODO",
    "Assert.Fail(",
    "pytest.fail(",
    "t.Fatal(\"not implemented",
    "throw new Error(\"not implemented",
];

// What the orchestrator settled: the behavior to prove, the allowed test paths, the reference test, the focused command, the text its failure must show, the production paths B may change (empty: anything outside the tests), the two agents, and each agent call's time limit.
pub struct Spec {
    pub behavior: String,
    pub tests: Vec<String>,
    pub reference: String,
    pub run: String,
    pub expect: String,
    pub prod: Vec<String>,
    pub test_agent: Rung,
    pub fix_agent: Option<Rung>,
    pub limit: Duration,
}

// Runs the focused command on the working tree, returning its outcome and output.
fn focused(top: &Path, s: &Spec, name: &str) -> Result<(Outcome, String), String> {
    let log = repo::state(top, "logs")?.join(format!("test-first-{name}.log"));
    let c = Check {
        name: name.into(),
        run: s.run.clone(),
        dir: ".".into(),
        kind: "test".into(),
        paths: Vec::new(),
        fix: None,
        report: None,
        outputs: Vec::new(),
    };
    let o = verify::run_check(top, &c, &log, s.limit);
    let out = std::fs::read_to_string(&log).unwrap_or_default();
    Ok((o, out))
}

// Why a run of the new test does not prove the behavior, or None: it passed, did not compile or load, failed for another reason, or the test is a placeholder.
pub fn not_red(o: &Outcome, out: &str, patch: &str, expect: &str) -> Option<String> {
    if o.status == "missing" {
        return Some(format!("the command could not run: {}", o.failures.join("; ")));
    }
    if o.code == Some(0) && o.failed == 0 {
        return Some(
            "the test passed before any production change, so it does not prove the behavior".into(),
        );
    }
    if let Some(l) = out.lines().find(|l| BROKEN.iter().any(|b| l.contains(b))) {
        return Some(format!("the test did not compile or load: {}", l.trim()));
    }
    let added: Vec<&str> = patch.lines().filter(|l| l.starts_with('+') && !l.starts_with("+++")).collect();
    if let Some(l) = added.iter().find(|l| PLACEHOLDERS.iter().any(|p| l.contains(p))) {
        return Some(format!(
            "the test is a placeholder, not a regression test: {}",
            l.trim_start_matches('+').trim()
        ));
    }
    if !out.contains(expect) {
        let tail: Vec<&str> = out.lines().rev().filter(|l| !l.trim().is_empty()).take(3).collect();
        return Some(format!(
            "it failed, but not for the expected reason (`{expect}` is not in its output; it ended: {})",
            tail.into_iter().rev().map(str::trim).collect::<Vec<_>>().join(" / ")
        ));
    }
    None
}

// Runs the workflow and returns its receipt; an Err is a setup problem, a refused step comes back in the receipt.
pub fn run(top: &Path, p: &Policy, s: &Spec, b: &dyn Backend, log: &Log) -> Result<String, String> {
    let reference = std::fs::read_to_string(top.join(&s.reference))
        .map_err(|_| format!("--reference {}: no such file", s.reference))?;
    if s.tests.is_empty() {
        return Err("name the paths the test may be written in with --tests".into());
    }
    let base = repo::worktree_tree(top)?;
    let in_tests = |path: &str| repo::under(path, &s.tests);
    let existing: String = s
        .tests
        .iter()
        .filter(|t| top.join(t).is_file())
        .map(|t| {
            format!("Current {t}:\n```\n{}```\n", std::fs::read_to_string(top.join(t)).unwrap_or_default())
        })
        .collect();
    let mut out = format!("test-first: {}\nbase tree {}\n", s.behavior, verify::short(&base));
    let mut prompt = format!(
        "Write one regression test that proves this behavior: {}\nRun now, it must fail with: {}\nWrite only inside: {}. Do not change production code.\nThe test must compile and fail for that behavioral reason; a placeholder that throws or is marked TODO is not a regression test. Make meaningful assertions. {}\nFocused command: {}\nModel it on this reference test ({}):\n```\n{reference}```\n{existing}{}",
        super::TOOLS_NOTE,
        s.behavior, s.expect, s.tests.join(", "), s.run, s.reference, if s.test_agent.hosted() { "Reply with the STATUS and CONFIDENCE lines when done." } else { FILES }
    );
    let mut red = None;
    for round in 0..2 {
        let deadline = Instant::now() + s.limit;
        let sections = [
            ("task", s.behavior.as_str()),
            ("reference", reference.as_str()),
            ("context", existing.as_str()),
        ];
        let e = edit::edit(
            top,
            b,
            log,
            &s.test_agent,
            if round == 0 { "test author" } else { "test retry" },
            &prompt,
            &sections,
            &in_tests,
            deadline,
        );
        let e = match e {
            Ok(e) => e,
            Err(err) => {
                out.push_str(&format!("A ({}) failed: {err}\n", s.test_agent.label()));
                continue;
            }
        };
        if !e.reverted.is_empty() {
            out.push_str(&format!("A changed paths outside --tests, put back: {}\n", e.reverted.join(", ")));
        }
        let patch = filter_patch(&repo::diff(top, &base, &repo::worktree_tree(top)?)?, &s.tests);
        let why = match e.changed.is_empty() && patch.is_empty() {
            true => Some("it wrote no test inside --tests".to_string()),
            false => {
                let (o, text) = focused(top, s, "red")?;
                not_red(&o, &text, &patch, &s.expect).map_or_else(
                    || {
                        red = Some((o, patch.clone()));
                        None
                    },
                    Some,
                )
            }
        };
        match why {
            None => break,
            Some(w) => {
                out.push_str(&format!("A ({}) round {}: {w}\n", s.test_agent.label(), round + 1));
                prompt.push_str(&format!(
                    "\n\nYour test did not prove the behavior: {w}. Fix the test (only inside {}).",
                    s.tests.join(", ")
                ));
            }
        }
    }
    let Some((red_run, patch)) = red else {
        out.push_str("-- NOT ACCEPTED: no test failed for the expected reason; the orchestrator decides the next step (settle the behavior, pick a stronger test author, or give a better reference)\n");
        return Ok(out);
    };
    let accepted = repo::worktree_tree(top)?;
    let patch_file = repo::state(top, "test-first")?.join(format!("{}.patch", verify::short(&accepted)));
    std::fs::write(&patch_file, &patch).map_err(|e| format!("{}: {e}", patch_file.display()))?;
    out.push_str(&format!(
        "red: `{}` failed as expected ({}); test patch {}\n",
        s.run,
        red_run.summary(),
        patch_file.display()
    ));
    let Some(fixer) = &s.fix_agent else {
        out.push_str("-- test accepted; no fix agent given, so production is unchanged\n");
        return Ok(out);
    };
    let in_prod = |path: &str| {
        !in_tests(path) && !p.is_generated(path) && (s.prod.is_empty() || repo::under(path, &s.prod))
    };
    let red_text = std::fs::read_to_string(&red_run.log).unwrap_or_default();
    let tail: String =
        red_text.lines().rev().take(30).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
    let mut fix_prompt = format!(
        "Make this failing regression test pass by changing production code only.\nBehavior: {}\nFocused command: {}\nIts failure now:\n{tail}\nThe test (read-only; do not change it — if its assertion is wrong, say so in a GAP line instead):\n{patch}\nYou may change: {}.\n{}\n{}",
        s.behavior, s.run, if s.prod.is_empty() { "anything outside the test paths".to_string() } else { s.prod.join(", ") }, super::TOOLS_NOTE,
        if fixer.hosted() { "Reply with the STATUS and CONFIDENCE lines when done." } else { FILES }
    );
    let mut green = None;
    for round in 0..2 {
        let deadline = Instant::now() + s.limit;
        let sections = [("task", s.behavior.as_str()), ("evidence", tail.as_str()), ("diff", patch.as_str())];
        match edit::edit(
            top,
            b,
            log,
            fixer,
            if round == 0 { "implement" } else { "implement retry" },
            &fix_prompt,
            &sections,
            &in_prod,
            deadline,
        ) {
            Ok(e) => {
                let (tests, other): (Vec<String>, Vec<String>) =
                    e.reverted.into_iter().partition(|p| in_tests(p));
                if !tests.is_empty() {
                    out.push_str(&format!("B changed the test ({}); it is put back: revising the assertion is the orchestrator's decision\n", tests.join(", ")));
                }
                if !other.is_empty() {
                    out.push_str(&format!(
                        "B changed paths outside its scope, put back: {}\n",
                        other.join(", ")
                    ));
                }
            }
            Err(err) => out.push_str(&format!("B ({}) failed: {err}\n", fixer.label())),
        }
        let (o, text) = focused(top, s, "green")?;
        if o.status == "passed" {
            green = Some(o);
            break;
        }
        out.push_str(&format!(
            "B ({}) round {}: `{}` still fails: {}\n",
            fixer.label(),
            round + 1,
            s.run,
            o.summary()
        ));
        fix_prompt.push_str(&format!(
            "\n\nIt still fails:\n{}",
            text.lines().rev().take(20).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n")
        ));
    }
    let Some(g) = green else {
        out.push_str("-- NOT GREEN: the accepted test still fails; the test stays, the production change is left for the orchestrator\n");
        return Ok(out);
    };
    out.push_str(&format!("green: `{}` passes ({})\n", s.run, g.summary()));
    out.push_str(&super::tidy::format(top, p, Some(&base), s.limit)?);
    let r = verify::verify(top, p, Some(&base), &[], s.limit)?;
    out.push_str(&r.text());
    out.push_str(&format!("-- {}: the test failed for the stated reason before the change and passes after it, the assertion unchanged\n", if r.green() { "done" } else { "done, but the broader checks are not green" }));
    Ok(out)
}

// Keeps the parts of a diff that touch the given paths.
pub fn filter_patch(diff: &str, paths: &[String]) -> String {
    keep_patch(diff, &|f| repo::under(f, paths))
}

// Keeps the parts of a diff whose file `keep` accepts.
pub fn keep_patch(diff: &str, keep_file: &dyn Fn(&str) -> bool) -> String {
    let mut out = String::new();
    let mut keep = false;
    for l in diff.lines() {
        if let Some(rest) = l.strip_prefix("diff --git a/") {
            let path = rest.split(" b/").next().unwrap_or_default();
            keep = keep_file(path);
        }
        if keep {
            out.push_str(l);
            out.push('\n');
        }
    }
    out
}
