// `zazzle flow update`: keeping a branch current is zazzle's job: it merges the target into the branch (never rebasing or forcing), regenerates conflicted lockfiles from their manifests with the package manager, has the local agent resolve other textual conflicts within each file, and commits the merge only when no conflict marker is left and the applicable checks pass; otherwise it aborts the merge and reports what needs the orchestrator.
use super::edit::{self, FILES};
use super::policy::{Check, Policy};
use super::repo;
use super::verify;
use crate::adaptive::agent::Rung;
use crate::adaptive::calls::{Backend, Log};
use std::path::Path;
use std::time::{Duration, Instant};

// What an update was asked: the branch to merge in (None: the policy's target), the agent for textual conflicts (None: report them), the strong agent that reviews a complex merge before it is committed (None: no review), whether to skip the checks, push afterwards, and each step's time limit.
pub struct Ask {
    pub target: Option<String>,
    pub resolver: Option<Rung>,
    pub reviewer: Option<Rung>,
    pub no_verify: bool,
    pub push: bool,
    pub limit: Duration,
}

// Tells whether text still has a conflict marker line.
pub fn has_markers(text: &str) -> bool {
    text.lines().any(|l| l.starts_with("<<<<<<< ") || l.starts_with(">>>>>>> ") || l == "=======")
}

// Ends the merge in progress: `git merge --abort`, then `git reset --merge` while a merge or a staged change is still there, a few times over a second in case another git process holds the index; an error says what is left.
pub fn end_merge(top: &Path) -> Result<(), String> {
    let pending = || {
        repo::git(top, &["rev-parse", "-q", "--verify", "MERGE_HEAD"]).is_ok()
            || repo::git(top, &["diff", "--cached", "--quiet"]).is_err()
    };
    let mut last = repo::git(top, &["merge", "--abort"]).err().unwrap_or_default();
    for _ in 0..5 {
        if !pending() {
            return Ok(());
        }
        last = repo::git(top, &["reset", "-q", "--merge"]).err().unwrap_or_default();
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    match pending() {
        true => Err(format!(
            "the merge could not be undone ({last}): run `git merge --abort` and check `git status`"
        )),
        false => Ok(()),
    }
}

// Aborts the merge in progress and returns the report with the reason.
fn abort(top: &Path, mut out: String, why: &str) -> Result<(String, bool), String> {
    end_merge(top)?;
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
    let out = format!("update: merging {from} ({behind} commit(s)) into {branch}\n");
    let clean = repo::git(top, &["merge", "--no-edit", "--no-commit", "--no-ff", &from]).is_ok();
    let r = settle(top, p, a, b, log, &from, &branch, clean, out);
    match r {
        Ok(done) => Ok(done),
        Err(e) => match end_merge(top) {
            Ok(()) => Err(format!("{e}; the merge was aborted and the branch is as it was")),
            Err(left) => Err(format!("{e}; {left}")),
        },
    }
}

// Settles a merge in progress: regenerates and resolves its conflicts, checks it, has a complex one reviewed, and commits it, or aborts it with the reason.
#[allow(clippy::too_many_arguments)]
fn settle(
    top: &Path,
    p: &Policy,
    a: &Ask,
    b: &dyn Backend,
    log: &Log,
    from: &str,
    branch: &str,
    clean: bool,
    mut out: String,
) -> Result<(String, bool), String> {
    let from = from.to_string();
    let conflicted: Vec<String> =
        repo::git(top, &["diff", "--name-only", "--diff-filter=U"])?.lines().map(str::to_string).collect();
    if !clean && conflicted.is_empty() {
        return abort(top, out, "git could not merge, and no file is in conflict");
    }
    let mut left = Vec::new();
    let mut resolved = Vec::new();
    for f in &conflicted {
        if p.is_generated(f) {
            repo::git(top, &["checkout", "--theirs", "--", f])?;
            match super::generate::regenerate(top, p, f, a.limit).expect("a generator writes it")? {
                o if o.status == "passed" => {
                    repo::git(top, &["add", "--", f])?;
                    out.push_str(&format!("  {f}: regenerated by its generator\n"));
                }
                o => left.push(format!("{f}: its generator {}", o.summary())),
            }
            continue;
        }
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
                    resolved.push(f.clone());
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
        if !r.checks.is_empty() && !r.green() {
            return abort(top, out, "the merged tree does not pass the checks");
        }
    }
    if let Some(why) = complex(top, &from, &resolved)? {
        match &a.reviewer {
            Some(r) => {
                out.push_str(&format!("a complex merge ({why}): reviewed by {}\n", r.label()));
                let problems = review_merge(top, &from, &resolved, &why, r, b, log, a.limit)?;
                if !problems.is_empty() {
                    out.push_str(&problems.iter().map(|l| format!("  {l}\n")).collect::<String>());
                    return abort(
                        top,
                        out,
                        "the merge review found problems; deciding them is the orchestrator's",
                    );
                }
            }
            None => {
                out.push_str(&format!("a complex merge ({why}), committed without a review (--no-review)\n"))
            }
        }
    }
    repo::git(top, &["commit", "-q", "--no-edit"])?;
    out.push_str(&format!(
        "commit: {} (merge of {from})\n",
        verify::short(&repo::head(top).unwrap_or_default())
    ));
    if a.push {
        out.push_str(&format!("push: {}\n", super::deliver::push(top, branch)?));
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

// Why a merge is complex enough for a strong reviewer, or None: conflicts the local agent resolved, three or more files changed on both sides, or a security or compatibility boundary in what the merge brings.
pub fn complex(top: &Path, from: &str, resolved: &[String]) -> Result<Option<String>, String> {
    let mut why = Vec::new();
    if !resolved.is_empty() {
        why.push(format!("the local agent resolved {}", resolved.join(", ")));
    }
    let mb = repo::git(top, &["merge-base", "HEAD", from])?;
    let ours = repo::changed(top, &mb, "HEAD")?;
    let both: Vec<String> = repo::changed(top, &mb, from)?.into_iter().filter(|f| ours.contains(f)).collect();
    if both.len() >= 3 {
        why.push(format!("{} files changed on both sides", both.len()));
    }
    let staged = repo::git(top, &["diff", "--cached", "HEAD"])?;
    let paths: Vec<String> =
        repo::git(top, &["diff", "--cached", "--name-only", "HEAD"])?.lines().map(str::to_string).collect();
    let (_, boundary) = super::review::signals(&staged, &paths);
    if !boundary.is_empty() {
        why.push(format!("it touches a {} boundary", boundary.join(" and ")));
    }
    Ok((!why.is_empty()).then(|| why.join("; ")))
}

// Has a strong agent review a merge before it is committed: the files both sides changed and how they were combined; returns its problem lines (empty: it found none).
#[allow(clippy::too_many_arguments)]
fn review_merge(
    top: &Path,
    from: &str,
    resolved: &[String],
    why: &str,
    r: &Rung,
    b: &dyn Backend,
    log: &Log,
    limit: Duration,
) -> Result<Vec<String>, String> {
    let mb = repo::git(top, &["merge-base", "HEAD", from])?;
    let ours = repo::changed(top, &mb, "HEAD")?;
    let both: Vec<String> = repo::changed(top, &mb, from)?.into_iter().filter(|f| ours.contains(f)).collect();
    let mut diff = String::new();
    for f in both.iter().chain(resolved).take(8) {
        diff.push_str(&repo::git(top, &["diff", "--cached", "HEAD", "--", f]).unwrap_or_default());
        diff.push('\n');
    }
    let system = format!("You review a merge of {from} into this branch before it is committed ({why}). Check that the combined code keeps what both sides meant and that nothing either side relied on was lost. You may read the files; change nothing. {} Be concise; reply only with lines:\nPROBLEM path:A-B | the issue | the fix\nOK what you checked (when there is no problem)", super::TOOLS_NOTE);
    let prompt = format!("Files changed on both sides: {}\nResolved by the local agent: {}\nWhat the merge changes on this branch:\n{}", if both.is_empty() { "none".to_string() } else { both.join(", ") }, if resolved.is_empty() { "none".to_string() } else { resolved.join(", ") }, diff.chars().take(40_000).collect::<String>());
    let sections = [("brief", system.as_str()), ("diff", diff.as_str())];
    let reply = log.text(
        b,
        r,
        &crate::adaptive::calls::Ask::new("merge review", &system, &prompt, &sections),
        top,
        Instant::now() + limit,
    )?;
    Ok(reply
        .lines()
        .map(str::trim)
        .filter(|l| crate::investigate::cite::strip_word(l, "PROBLEM").is_some())
        .map(str::to_string)
        .collect())
}
