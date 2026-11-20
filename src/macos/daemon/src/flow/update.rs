// `zazzle flow update`: keeping a branch current is zazzle's job: it merges the target into the branch (never rebasing or forcing), regenerates conflicted lockfiles from their manifests with the package manager, has the local agent resolve other textual conflicts within each file, and commits the merge only when no conflict marker is left and the applicable checks pass; otherwise it aborts the merge and reports what needs the orchestrator.
use super::edit::{self, FILES};
use super::policy::{Check, Policy};
use super::repo;
use super::verify;
use crate::adaptive::agent::Rung;
use crate::adaptive::calls::{Backend, Log};
use std::path::Path;
use std::time::{Duration, Instant};

// What an update was asked: the branch to merge in (None: the policy's target), the agent for textual conflicts (None: report them), whether to skip the checks, push afterwards, and each step's time limit.
pub struct Ask {
    pub target: Option<String>,
    pub resolver: Option<Rung>,
    pub no_verify: bool,
    pub push: bool,
    pub limit: Duration,
}

// Tells whether text still has a conflict marker line.
pub fn has_markers(text: &str) -> bool {
    text.lines().any(|l| l.starts_with("<<<<<<< ") || l.starts_with(">>>>>>> ") || l == "=======")
}

// Aborts the merge in progress and returns the report with the reason.
fn abort(top: &Path, mut out: String, why: &str) -> Result<(String, bool), String> {
    repo::git(top, &["merge", "--abort"])?;
    out.push_str(&format!("-- merge aborted: {why}; the branch is as it was\n"));
    Ok((out, false))
}

// Merges the target into the current branch, resolving what tools and the local agent can, and returns the report and whether the branch is current and committed.
pub fn update(top: &Path, p: &Policy, a: &Ask, b: &dyn Backend, log: &Log) -> Result<(String, bool), String> {
    let branch = repo::branch(top)?;
    if p.is_protected(&branch) {
        return Err(format!("{branch} is a {} branch: update a feature branch", p.level(&branch)));
    }
    if !repo::git(top, &["status", "--porcelain", "--untracked-files=no"])?.is_empty() {
        return Err("the working tree has uncommitted changes: commit or stash them first".into());
    }
    let target = a.target.clone().unwrap_or_else(|| p.target.clone());
    let remote = repo::git(top, &["remote", "get-url", "origin"]).is_ok();
    if remote {
        repo::git(top, &["fetch", "-q", "origin", &target])?;
    }
    let from = if remote { format!("origin/{target}") } else { target.clone() };
    repo::git(top, &["rev-parse", "--verify", &from]).map_err(|_| format!("no branch `{from}` to merge"))?;
    if repo::git(top, &["merge-base", "--is-ancestor", &from, "HEAD"]).is_ok() {
        return Ok((format!("update: {branch} already has every commit of {from}\n"), true));
    }
    let behind = repo::git(top, &["rev-list", "--count", &format!("HEAD..{from}")])?;
    let mut out = format!("update: merging {from} ({behind} commit(s)) into {branch}\n");
    let clean = repo::git(top, &["merge", "--no-edit", "--no-commit", "--no-ff", &from]).is_ok();
    let conflicted: Vec<String> =
        repo::git(top, &["diff", "--name-only", "--diff-filter=U"])?.lines().map(str::to_string).collect();
    if !clean && conflicted.is_empty() {
        return abort(top, out, "git could not merge, and no file is in conflict");
    }
    let mut left = Vec::new();
    for f in &conflicted {
        match p.regenerate(f) {
            Some(cmd) => {
                repo::git(top, &["checkout", "--theirs", "--", f])?;
                let dir = Path::new(f)
                    .parent()
                    .map(|d| d.to_string_lossy().to_string())
                    .filter(|d| !d.is_empty())
                    .unwrap_or_else(|| ".".into());
                let c = Check {
                    name: "lockfile".into(),
                    run: cmd.clone(),
                    dir,
                    kind: "script".into(),
                    ..Check::default()
                };
                let o = verify::run_check(
                    top,
                    &c,
                    &repo::state(top, "logs")?.join("update-lockfile.log"),
                    a.limit,
                );
                match o.status.as_str() {
                    "passed" => {
                        repo::git(top, &["add", "--", f])?;
                        out.push_str(&format!("  {f}: regenerated with `{cmd}`\n"));
                    }
                    _ => left.push(format!("{f}: `{cmd}` {}", o.summary())),
                }
            }
            None => match resolve(top, f, &from, a, b, log) {
                Ok(how) => {
                    repo::git(top, &["add", "--", f])?;
                    out.push_str(&format!("  {f}: {how}\n"));
                }
                Err(why) => left.push(format!("{f}: {why}")),
            },
        }
    }
    if !left.is_empty() {
        out.push_str(&format!(
            "not resolved:\n{}",
            left.iter().map(|l| format!("  {l}\n")).collect::<String>()
        ));
        return abort(
            top,
            out,
            "these conflicts need the orchestrator (a semantic conflict, or no resolver)",
        );
    }
    if !a.no_verify {
        out.push_str(&super::tidy::format(top, p, Some(&from), a.limit)?);
        let r = verify::verify(top, p, Some(&from), &[], a.limit)?;
        out.push_str(&r.text());
        if !r.green() {
            return abort(top, out, "the merged tree does not pass the checks");
        }
    }
    repo::git(top, &["commit", "-q", "--no-edit"])?;
    out.push_str(&format!(
        "commit: {} (merge of {from})\n",
        verify::short(&repo::head(top).unwrap_or_default())
    ));
    if a.push {
        out.push_str(&format!("push: {}\n", super::deliver::push(top, &branch)?));
    }
    out.push_str("-- the branch is current\n");
    Ok((out, true))
}

// Has the resolver merge one file's conflict, keeping both sides' intent; returns how, or why not.
fn resolve(top: &Path, f: &str, from: &str, a: &Ask, b: &dyn Backend, log: &Log) -> Result<String, String> {
    let r = a.resolver.as_ref().ok_or("a textual conflict, and no resolver was given")?;
    let text = std::fs::read_to_string(top.join(f)).map_err(|e| format!("{e}"))?;
    let subjects =
        |range: &str| repo::git(top, &["log", "-5", "--format=- %s", range, "--", f]).unwrap_or_default();
    let ours = subjects(&format!("{from}..HEAD"));
    let theirs = subjects(&format!("HEAD..{from}"));
    let prompt = format!(
        "Resolve the merge conflict in {f}: keep what both sides meant, remove every conflict marker, and change nothing else. If the two sides contradict each other, do not choose: reply with a GAP line saying what conflicts. {}\nThis branch's commits to it:\n{ours}\nThe target's commits to it:\n{theirs}\nThe file now:\n```\n{text}```\n{}",
        super::TOOLS_NOTE,
        if r.hosted() { "Edit the file, then reply with STATUS." } else { FILES }
    );
    let sections = [("task", f), ("evidence", text.as_str())];
    let e = edit::edit(
        top,
        b,
        log,
        r,
        "resolve",
        &prompt,
        &sections,
        &|p: &str| p == f,
        Instant::now() + a.limit,
    )?;
    let gaps = crate::investigate::cite::parse_answer(&e.reply).map(|x| x.unknowns).unwrap_or_default();
    let now = std::fs::read_to_string(top.join(f)).unwrap_or_default();
    match (gaps.first(), has_markers(&now)) {
        (Some(g), _) => Err(format!("{} says the sides contradict: {g}", r.label())),
        (None, true) => Err(format!("{} left conflict markers", r.label())),
        (None, false) => Ok(format!("resolved by {}", r.label())),
    }
}
