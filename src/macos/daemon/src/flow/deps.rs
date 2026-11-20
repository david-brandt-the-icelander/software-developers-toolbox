// `zazzle flow deps`: dependency updates are the package managers' job, run by zazzle: the `upgrade` checks (cargo update, npm update, go get -u=patch, poetry update) run on a clean feature branch, the version changes are read from the lockfiles, a major bump goes to the strongest agent for review, the audits and checks run, and the update is committed with a message listing the bumps only when everything passes; otherwise every change is put back.
use super::policy::{Check, Policy};
use super::repo;
use super::verify;
use crate::adaptive::agent::Rung;
use crate::adaptive::calls::{Ask, Backend, Log};
use std::path::Path;
use std::time::{Duration, Instant};

// What a dependency update was asked: a branch to create first (None: the current one), the strong reviewer for major bumps (None: no review), and each step's time limit.
pub struct DepsAsk {
    pub branch: Option<String>,
    pub reviewer: Option<Rung>,
    pub limit: Duration,
}

// One dependency's change: its name, the old version (empty: added) and the new one (empty: removed).
#[derive(Debug, Clone, PartialEq)]
pub struct Bump {
    pub name: String,
    pub from: String,
    pub to: String,
}

impl Bump {
    // Tells whether the bump changes the major version (or the minor one below 1.0), where breaking changes are allowed.
    pub fn major(&self) -> bool {
        let part = |v: &str, i: usize| {
            v.trim_start_matches('v').split(['.', '-', '+']).nth(i).and_then(|x| x.parse::<u64>().ok())
        };
        if self.from.is_empty() || self.to.is_empty() {
            return false;
        }
        match (part(&self.from, 0), part(&self.to, 0)) {
            (Some(0), Some(0)) => part(&self.from, 1) != part(&self.to, 1),
            (a, b) => a != b,
        }
    }

    // The bump as `name 1.0 → 1.1`.
    pub fn line(&self) -> String {
        match (self.from.is_empty(), self.to.is_empty()) {
            (true, _) => format!("{} added at {}", self.name, self.to),
            (_, true) => format!("{} {} removed", self.name, self.from),
            _ => format!("{} {} → {}", self.name, self.from, self.to),
        }
    }
}

// The name and version of every package a lockfile or manifest pins: Cargo.lock's [[package]] entries, package-lock.json's packages, go.mod's requirements.
pub fn pins(path: &str, text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if path.ends_with("Cargo.lock") {
        let mut name = String::new();
        for l in text.lines() {
            if let Some(n) = l.strip_prefix("name = \"") {
                name = n.trim_end_matches('"').to_string();
            } else if let Some(v) = l.strip_prefix("version = \"") {
                out.push((name.clone(), v.trim_end_matches('"').to_string()));
            }
        }
    } else if path.ends_with("package-lock.json") {
        let v: serde_json::Value = serde_json::from_str(text).unwrap_or_default();
        for (k, p) in v["packages"].as_object().into_iter().flatten().filter(|(k, _)| !k.is_empty()) {
            if let Some(ver) = p["version"].as_str() {
                out.push((k.rsplit("node_modules/").next().unwrap_or(k).to_string(), ver.to_string()));
            }
        }
    } else if path.ends_with("go.mod") {
        for l in text.lines().map(str::trim) {
            let l = l.strip_prefix("require ").unwrap_or(l);
            let mut w = l.split_whitespace();
            if let (Some(m), Some(v)) = (w.next(), w.next()) {
                if m.contains('/') && v.starts_with('v') {
                    out.push((m.to_string(), v.to_string()));
                }
            }
        }
    }
    out
}

// The bumps between two versions of a file: changed, added and removed pins, by name.
pub fn bumps(path: &str, before: &str, after: &str) -> Vec<Bump> {
    let (a, b) = (pins(path, before), pins(path, after));
    let mut out = Vec::new();
    for (n, v) in &b {
        match a.iter().find(|(m, _)| m == n) {
            Some((_, old)) if old != v => {
                out.push(Bump { name: n.clone(), from: old.clone(), to: v.clone() })
            }
            None => out.push(Bump { name: n.clone(), from: String::new(), to: v.clone() }),
            _ => {}
        }
    }
    for (n, v) in a.iter().filter(|(n, _)| !b.iter().any(|(m, _)| m == n)) {
        out.push(Bump { name: n.clone(), from: v.clone(), to: String::new() });
    }
    out
}

// Puts the update back and returns the report with the reason.
fn undo(
    top: &Path,
    before: &str,
    changed: &[String],
    mut out: String,
    why: &str,
) -> Result<(String, bool), String> {
    repo::restore(top, before, changed)?;
    out.push_str(&format!("-- {why}; the update was put back\n"));
    Ok((out, false))
}

// Runs the updaters, then the review of major bumps, the audits and the checks, and commits the update only when they all pass; returns the report and whether it was committed (or nothing needed updating).
pub fn deps(
    top: &Path,
    p: &Policy,
    a: &DepsAsk,
    b: &dyn Backend,
    log: &Log,
) -> Result<(String, bool), String> {
    let ups: Vec<&Check> = p.checks.iter().filter(|c| c.kind == "upgrade").collect();
    if ups.is_empty() {
        return Ok((
            "deps: no updater is configured or found (add kind \"upgrade\" checks to .zazzle/flow.json)\n"
                .into(),
            true,
        ));
    }
    if !p.opted_in || !p.effects.iter().any(|e| e == "commit") {
        return Err(format!(
            "dependency updates are committed: opt in with {} and allow `commit`",
            super::policy::FILE
        ));
    }
    if !repo::git(top, &["status", "--porcelain"])?.is_empty() {
        return Err("the working tree has changes: commit or stash them first".into());
    }
    if let Some(name) = &a.branch {
        repo::git(top, &["checkout", "-q", "-b", name])?;
    }
    let branch = repo::branch(top)?;
    if p.is_protected(&branch) {
        return Err(format!(
            "{branch} is a {} branch: run on a feature branch, or pass --branch NAME",
            p.level(&branch)
        ));
    }
    let before = repo::worktree_tree(top)?;
    let mut out = format!("deps on {branch}:\n");
    for c in &ups {
        let o = verify::run_check(
            top,
            c,
            &repo::state(top, "logs")?.join(format!("deps-{}.log", verify::safe(&c.name))),
            a.limit,
        );
        out.push_str(&format!(
            "  {} {}: {}\n",
            if o.status == "passed" { "✓" } else { "✗" },
            c.name,
            o.summary()
        ));
    }
    let changed = repo::changed(top, &before, &repo::worktree_tree(top)?)?;
    if changed.is_empty() {
        out.push_str("-- every dependency is already at its newest allowed version\n");
        return Ok((out, true));
    }
    let mut all = Vec::new();
    for f in &changed {
        let old = repo::git(top, &["show", &format!("{before}:{f}")]).unwrap_or_default();
        let new = std::fs::read_to_string(top.join(f)).unwrap_or_default();
        let bs = bumps(f, &old, &new);
        if bs.is_empty() {
            out.push_str(&format!("  {f} changed\n"));
        }
        all.extend(bs);
    }
    for bump in &all {
        out.push_str(&format!("  {}{}\n", bump.line(), if bump.major() { " (major)" } else { "" }));
    }
    let majors: Vec<&Bump> = all.iter().filter(|x| x.major()).collect();
    if let (false, Some(r)) = (majors.is_empty(), &a.reviewer) {
        let list: String = majors.iter().map(|x| format!("- {}\n", x.line())).collect();
        let system = format!("You review major dependency upgrades before they are committed. For each, judge from its release notes and this repository's use of it (you may read the files; change nothing) whether it is safe as it is. {} Be concise; reply only with lines:\nPROBLEM package | what breaks or must change | what to do\nOK package | why it is safe", super::TOOLS_NOTE);
        let sections = [("brief", system.as_str()), ("task", list.as_str())];
        let reply = log.text(
            b,
            r,
            &Ask::new("deps review", &system, &list, &sections),
            top,
            Instant::now() + a.limit,
        );
        let reply = match reply {
            Ok(t) => t,
            Err(e) => {
                return undo(
                    top,
                    &before,
                    &changed,
                    out,
                    &format!("the review of the major bumps failed: {e}"),
                )
            }
        };
        let problems: Vec<&str> = reply
            .lines()
            .map(str::trim)
            .filter(|l| crate::investigate::cite::strip_word(l, "PROBLEM").is_some())
            .collect();
        out.push_str(&format!("major bumps reviewed by {}\n", r.label()));
        if !problems.is_empty() {
            out.push_str(&problems.iter().map(|l| format!("  {l}\n")).collect::<String>());
            return undo(
                top,
                &before,
                &changed,
                out,
                "the review found problems with the major bumps; upgrading them is your decision",
            );
        }
    }
    let (audit, audit_ok) = super::audit::audit(top, p, Some(&before), false, a.limit)?;
    out.push_str(&audit);
    if !audit_ok {
        return undo(top, &before, &changed, out, "the updated dependencies have advisories");
    }
    let r = verify::verify(top, p, Some(&before), &[], a.limit)?;
    out.push_str(&r.text());
    if !r.checks.is_empty() && !r.green() {
        return undo(top, &before, &changed, out, "the checks fail with the updated dependencies");
    }
    let subject = match all.len() {
        0 => "Update dependencies".to_string(),
        1 => format!("Update {}", all[0].line()),
        n => format!("Update {n} dependencies"),
    };
    let body: String = all.iter().map(|x| format!("- {}\n", x.line())).collect();
    let mut add = vec!["add", "-A", "--"];
    add.extend(changed.iter().map(String::as_str));
    repo::git(top, &add)?;
    repo::git(top, &["commit", "-q", "-m", &subject, "-m", body.trim_end()])?;
    out.push_str(&format!("commit: {} ({subject})\n-- dependencies updated; deliver it with `zazzle flow deliver` or open a pull request\n", verify::short(&repo::head(top).unwrap_or_default())));
    Ok((out, true))
}
