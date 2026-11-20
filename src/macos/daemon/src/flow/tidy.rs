// `zazzle flow tidy`: formatting and linting are the tools' job, run by zazzle rather than by an expensive execution agent: each format and lint check that applies to the change runs its fix command, the checks run again, and only a lint failure that still needs an edit goes to the cheap local agent, kept to the change's files, before one last check.
use super::edit::{self, FILES};
use super::policy::{Check, Policy};
use super::repo;
use super::verify::{self, Outcome};
use crate::adaptive::agent::Rung;
use crate::adaptive::calls::{Backend, Log};
use std::path::Path;
use std::time::{Duration, Instant};

// The format and lint checks that apply to the change against `base` (all of them without a base).
fn applicable<'a>(top: &Path, p: &'a Policy, base: Option<&str>) -> Result<Vec<&'a Check>, String> {
    let changed = match base {
        Some(b) => repo::changed(top, b, &repo::worktree_tree(top)?)?,
        None => Vec::new(),
    };
    let checks = if base.is_some() { p.applicable(&changed) } else { p.checks.iter().collect() };
    Ok(checks.into_iter().filter(|c| c.kind == "format" || c.kind == "lint").collect())
}

// Runs a check's own command, or its fix command, logging under `tag`.
fn run(top: &Path, c: &Check, fix: bool, tag: &str, limit: Duration) -> Result<Outcome, String> {
    let cmd = if fix { c.fix.clone().unwrap_or_default() } else { c.run.clone() };
    let one = Check { run: cmd, ..c.clone() };
    let safe: String = c.name.chars().map(|ch| if ch.is_alphanumeric() { ch } else { '-' }).collect();
    Ok(verify::run_check(top, &one, &repo::state(top, "logs")?.join(format!("tidy-{tag}-{safe}.log")), limit))
}

// Runs the fix commands of the format checks that apply, for after an agent's edit; returns a line on what ran, empty when nothing applies.
pub fn format(top: &Path, p: &Policy, base: Option<&str>, limit: Duration) -> Result<String, String> {
    let mut ran = Vec::new();
    for c in applicable(top, p, base)?.into_iter().filter(|c| c.kind == "format" && c.fix.is_some()) {
        let o = run(top, c, true, "format", limit)?;
        ran.push(format!("{} {}", c.name, o.status));
    }
    Ok(if ran.is_empty() { String::new() } else { format!("formatted by zazzle: {}\n", ran.join(", ")) })
}

// Fixes what the tools can, then hands lint failures that remain to `fixer` (None: report them), and returns the report and whether every format and lint check passes.
#[allow(clippy::too_many_arguments)]
pub fn tidy(
    top: &Path,
    p: &Policy,
    base: Option<&str>,
    b: &dyn Backend,
    log: &Log,
    fixer: Option<&Rung>,
    limit: Duration,
) -> Result<(String, bool), String> {
    let checks = applicable(top, p, base)?;
    if checks.is_empty() {
        return Ok(("tidy: no format or lint check applies (add kind \"format\" or \"lint\" checks with a \"fix\" command to .zazzle/flow.json)\n".into(), true));
    }
    let mut out = String::from("tidy:\n");
    for c in checks.iter().filter(|c| c.fix.is_some()) {
        let o = run(top, c, true, "fix", limit)?;
        out.push_str(&format!("  fixed with `{}`: {}\n", c.fix.as_deref().unwrap_or_default(), o.status));
    }
    let mut failing = Vec::new();
    for c in &checks {
        let o = run(top, c, false, "check", limit)?;
        out.push_str(&format!(
            "  {} {} ({}): {}\n",
            if o.status == "passed" { "✓" } else { "✗" },
            c.name,
            c.kind,
            o.summary()
        ));
        if o.status != "passed" {
            failing.push((*c, o));
        }
    }
    let Some(r) = fixer.filter(|_| !failing.is_empty()) else {
        let ok = failing.is_empty();
        out.push_str(if ok {
            "-- format and lint clean, by the tools\n"
        } else {
            "-- format or lint still failing; nothing was asked to fix it\n"
        });
        return Ok((out, ok));
    };
    let changed = match base {
        Some(bb) => repo::changed(top, bb, &repo::worktree_tree(top)?)?,
        None => Vec::new(),
    };
    let report: String = failing
        .iter()
        .map(|(c, o)| {
            format!(
                "`{}`:\n{}\n",
                c.run,
                std::fs::read_to_string(&o.log)
                    .unwrap_or_default()
                    .lines()
                    .take(60)
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        })
        .collect();
    let prompt = format!("Fix only these lint findings, with the smallest edits; do not reformat or change anything else (zazzle runs the formatter and the linter after you). Change only: {}.\n{report}\n{}", if changed.is_empty() { "the files named in the findings".to_string() } else { changed.join(", ") }, if r.hosted() { "Edit the files, then reply with STATUS." } else { FILES });
    let allowed = |x: &str| !p.is_doc(x) && (changed.is_empty() || changed.iter().any(|c| c == x));
    let sections = [("evidence", report.as_str())];
    match edit::edit(top, b, log, r, "lint fix", &prompt, &sections, &allowed, Instant::now() + limit) {
        Ok(e) if !e.reverted.is_empty() => out.push_str(&format!(
            "  {} changed paths outside the change, put back: {}\n",
            r.label(),
            e.reverted.join(", ")
        )),
        Ok(_) => {}
        Err(e) => out.push_str(&format!("  {} failed: {e}\n", r.label())),
    }
    out.push_str(&format(top, p, base, limit)?);
    let mut ok = true;
    for (c, _) in &failing {
        let o = run(top, c, false, "recheck", limit)?;
        ok &= o.status == "passed";
        out.push_str(&format!("  after {}: {} {}\n", r.label(), c.name, o.summary()));
    }
    out.push_str(if ok {
        "-- format and lint clean; the leftover lint was fixed by the local agent\n"
    } else {
        "-- lint still failing after the local agent: these findings need the executor\n"
    });
    Ok((out, ok))
}
