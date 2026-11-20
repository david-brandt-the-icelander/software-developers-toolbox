// `zazzle flow audit`: dependency audits are the tools' job, run by zazzle rather than read by an execution agent: the `audit` checks (cargo-audit, npm audit, govulncheck, pip-audit, dotnet's vulnerable-package list) run when a manifest or lockfile changed, or all of them on request; the report names each advisory found with the line that names it, and `--fix` runs an audit's fix command and checks again.
use super::policy::{Check, Policy};
use super::repo;
use super::verify::{self, Outcome};
use std::path::Path;
use std::time::Duration;

// The advisory id prefixes the audits print.
const IDS: &[&str] = &["RUSTSEC-", "GHSA-", "CVE-", "GO-", "PYSEC-"];

// The advisories in an audit's output, once each, with the first line that names it (trimmed to 160 characters).
pub fn advisories(out: &str) -> Vec<(String, String)> {
    let mut found: Vec<(String, String)> = Vec::new();
    for l in out.lines() {
        for w in l.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-')) {
            let id_like =
                IDS.iter().any(|p| w.starts_with(p)) && w.chars().filter(char::is_ascii_digit).count() >= 4;
            if id_like && !found.iter().any(|(i, _)| i == w) {
                found.push((w.to_string(), l.trim().chars().take(160).collect()));
            }
        }
    }
    found
}

// Runs one audit, logging under the check's name.
fn run(top: &Path, c: &Check, fix: bool, limit: Duration) -> Result<(Outcome, String), String> {
    let one = Check { run: if fix { c.fix.clone().unwrap_or_default() } else { c.run.clone() }, ..c.clone() };
    let safe = verify::safe(&c.name);
    let log = repo::state(top, "logs")?.join(format!("audit-{}{safe}.log", if fix { "fix-" } else { "" }));
    let o = verify::run_check(top, &one, &log, limit);
    let text = std::fs::read_to_string(&log).unwrap_or_default();
    Ok((o, text))
}

// Runs the audits that apply (to the manifests changed against `base`, or all without one), optionally their fixes, and returns the report and whether every audit passed.
pub fn audit(
    top: &Path,
    p: &Policy,
    base: Option<&str>,
    fix: bool,
    limit: Duration,
) -> Result<(String, bool), String> {
    let audits: Vec<&Check> = match base {
        Some(b) => p
            .applicable(&repo::changed(top, b, &repo::worktree_tree(top)?)?)
            .into_iter()
            .filter(|c| c.kind == "audit")
            .collect(),
        None => p.checks.iter().filter(|c| c.kind == "audit").collect(),
    };
    if audits.is_empty() {
        let why = if p.checks.iter().any(|c| c.kind == "audit") {
            "no dependency manifest or lockfile changed (run with --all to audit anyway)"
        } else {
            "no audit check is configured or found (add kind \"audit\" checks to .zazzle/flow.json)"
        };
        return Ok((format!("audit: {why}\n"), true));
    }
    let mut out = String::from("dependency audit, run by zazzle:\n");
    let mut ok = true;
    for c in audits {
        let (mut o, mut text) = run(top, c, false, limit)?;
        if o.status == "failed" && fix && c.fix.is_some() {
            let (f, _) = run(top, c, true, limit)?;
            out.push_str(&format!("  ran `{}`: {}\n", c.fix.as_deref().unwrap_or_default(), f.status));
            (o, text) = run(top, c, false, limit)?;
        }
        let ids = advisories(&text);
        out.push_str(&format!(
            "  {} {}: {}{}\n",
            if o.status == "passed" { "✓" } else { "✗" },
            c.name,
            o.summary(),
            if ids.is_empty() {
                String::new()
            } else {
                format!(", {} advisor{}", ids.len(), if ids.len() == 1 { "y" } else { "ies" })
            }
        ));
        for (id, line) in &ids {
            out.push_str(&format!("      {id}: {line}\n"));
        }
        if o.status != "passed" {
            out.push_str(&format!("      log: {}\n", o.log));
        }
        ok &= o.status == "passed";
    }
    out.push_str(match (ok, fix) {
        (true, _) => "-- no known vulnerable dependencies\n",
        (false, true) => "-- advisories remain after the fixes; upgrading or replacing those dependencies is your decision\n",
        (false, false) => "-- advisories found; `--fix` runs the audits' fix commands where they have one\n",
    });
    Ok((out, ok))
}
