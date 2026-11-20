// `zazzle flow deliver`: stages exactly the paths given, commits, pushes and opens or updates the pull request, each step safe to repeat: a commit already made, a push already done or a pull request already open is found and reused, so a retry after a partial failure makes no duplicate; the receipt says which verification, if any, covers the committed tree.
use super::policy::Policy;
use super::repo;
use super::verify;
use std::path::{Path, PathBuf};
use std::process::Command;

// What delivery was asked to do: the paths, the commit message, whether to push and open a pull request (draft or not), its title and body file, and the base.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Ask {
    pub paths: Vec<String>,
    pub message: String,
    pub push: bool,
    pub pr: bool,
    pub draft: bool,
    pub title: Option<String>,
    pub body: Option<PathBuf>,
    pub base: Option<String>,
    pub require_verified: bool,
}

// The gh program: $ZAZZLE_GH, else gh.
fn gh() -> PathBuf {
    std::env::var_os("ZAZZLE_GH").map_or_else(|| PathBuf::from("gh"), PathBuf::from)
}

// Runs gh in `dir` and returns its standard output, or its last error line.
pub fn run_gh(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new(gh())
        .args(args)
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("running gh: {e} (install the GitHub CLI or set $ZAZZLE_GH)"))?;
    match out.status.success() {
        true => Ok(String::from_utf8_lossy(&out.stdout).trim().to_string()),
        false => {
            let err = String::from_utf8_lossy(&out.stderr);
            Err(format!(
                "gh {}: {}",
                args.iter().take(2).copied().collect::<Vec<_>>().join(" "),
                err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("failed").trim()
            ))
        }
    }
}

// Delivers the change and returns the receipt lines: commit, branch, push, pull request and verification; refuses before any effect the policy does not allow or a branch it must not commit to.
pub fn deliver(top: &Path, p: &Policy, a: &Ask) -> Result<String, String> {
    let branch = repo::branch(top)?;
    let allowed = |e: &str| p.effects.iter().any(|x| x == e);
    if !p.opted_in {
        return Err(format!(
            "this repository has not opted in to zazzle delivery: add {} (see `zazzle flow init`)",
            super::policy::FILE
        ));
    }
    for (want, e) in [(true, "commit"), (a.push, "push"), (a.pr, "pr")] {
        if want && !allowed(e) {
            return Err(format!(
                "{} does not allow `{e}` (effects: {})",
                super::policy::FILE,
                p.effects.join(", ")
            ));
        }
    }
    if p.is_protected(&branch) {
        return Err(format!("{branch} is a {} branch: deliver from a feature branch", p.level(&branch)));
    }
    if a.paths.is_empty() {
        return Err("name the paths to commit with --paths (nothing is staged by default)".into());
    }
    if a.message.trim().is_empty() {
        return Err("give the commit message with -m".into());
    }
    let base = a.base.clone().unwrap_or_else(|| p.target.clone());
    let remote = repo::git(top, &["remote", "get-url", "origin"]).ok();
    if (a.push || a.pr) && remote.is_none() {
        return Err("there is no `origin` remote to push to".into());
    }
    let staged_before: Vec<String> =
        repo::git(top, &["diff", "--cached", "--name-only"])?.lines().map(str::to_string).collect();
    if let Some(other) = staged_before.iter().find(|f| !repo::under(f, &a.paths)) {
        return Err(format!("{other} is staged but not among --paths: unstage it or name it"));
    }
    let mut args = vec!["add", "-A", "--"];
    args.extend(a.paths.iter().map(String::as_str));
    repo::git(top, &args)?;
    let staged = repo::git(top, &["diff", "--cached", "--name-only"])?;
    let mut out = String::new();
    let commit = match staged.is_empty() {
        true => {
            let last = repo::git(top, &["log", "-1", "--format=%B"]).unwrap_or_default();
            if last.trim() != a.message.trim() {
                return Err("nothing to commit in --paths, and the last commit has another message".into());
            }
            let c = repo::head(top).unwrap_or_default();
            out.push_str(&format!("commit: {} (already made; not repeated)\n", verify::short(&c)));
            c
        }
        false => {
            let tree = repo::git(top, &["write-tree"])?;
            if a.require_verified && !verify::receipt_for(top, &tree).is_some_and(|r| r.green()) {
                return Err(format!(
                    "no green verification for tree {}: run `zazzle flow verify` first",
                    verify::short(&tree)
                ));
            }
            repo::git(top, &["commit", "-q", "-m", &a.message])?;
            let c = repo::head(top).unwrap_or_default();
            out.push_str(&format!("commit: {} ({} files)\n", verify::short(&c), staged.lines().count()));
            c
        }
    };
    out.push_str(&format!("branch: {branch} (base {base})\n"));
    let tree = repo::tree_of(top, &commit)?;
    out.push_str(&format!(
        "verification: {}\n",
        match verify::receipt_for(top, &tree) {
            Some(r) => r.line(),
            None => format!("none for tree {}: the commit is unverified", verify::short(&tree)),
        }
    ));
    if a.push {
        out.push_str(&format!("push: {}\n", push(top, &branch)?));
    }
    if a.pr {
        out.push_str(&pull_request(top, a, &branch, &base)?);
    }
    Ok(out)
}

// Pushes the branch, retrying network failures; a remote that has commits the branch lacks is reported, never forced.
pub fn push(top: &Path, branch: &str) -> Result<String, String> {
    let ahead = repo::git(top, &["rev-list", "--count", &format!("origin/{branch}..HEAD")]).ok();
    if ahead.as_deref() == Some("0") {
        return Ok("up to date (nothing to push)".into());
    }
    let mut last = String::new();
    for i in 0..4 {
        match repo::git(top, &["push", "-q", "-u", "origin", branch]) {
            Ok(_) => return Ok(format!("pushed to origin/{branch}")),
            Err(e) if e.contains("rejected") => {
                return Err(format!("origin/{branch} has commits this branch lacks: fetch and merge them, then deliver again ({e})"))
            }
            Err(e) => {
                last = e;
                std::thread::sleep(std::time::Duration::from_millis(200 << i));
            }
        }
    }
    Err(format!("push failed after 4 tries: {last}"))
}

// Opens the pull request, or finds the open one for the branch and updates its body when one is given.
fn pull_request(top: &Path, a: &Ask, branch: &str, base: &str) -> Result<String, String> {
    let list = run_gh(
        top,
        &["pr", "list", "--head", branch, "--state", "open", "--json", "number,url,isDraft,baseRefName"],
    )?;
    let open: serde_json::Value = serde_json::from_str(if list.is_empty() { "[]" } else { &list })
        .map_err(|e| format!("gh pr list: {e}"))?;
    let body = a.body.as_ref().map(|b| b.display().to_string());
    if let Some(pr) = open.as_array().and_then(|v| v.first()) {
        let n = pr["number"].to_string();
        if let Some(b) = &body {
            run_gh(top, &["pr", "edit", &n, "--body-file", b])?;
        }
        let other = pr["baseRefName"]
            .as_str()
            .filter(|b| *b != base)
            .map(|b| format!("; it targets {b}, not {base}: change that only if you mean to"))
            .unwrap_or_default();
        return Ok(format!(
            "pr: {} #{n} (already open{}{}){other}\n",
            pr["url"].as_str().unwrap_or_default(),
            if pr["isDraft"] == true { ", draft" } else { "" },
            if body.is_some() { ", body updated" } else { "" }
        ));
    }
    let title = a.title.clone().unwrap_or_else(|| a.message.lines().next().unwrap_or_default().to_string());
    let mut args = vec!["pr", "create", "--base", base, "--head", branch, "--title", &title];
    let fallback = "Delivered with zazzle flow.".to_string();
    match &body {
        Some(b) => args.extend(["--body-file", b.as_str()]),
        None => args.extend(["--body", fallback.as_str()]),
    }
    if a.draft {
        args.push("--draft");
    }
    let url = run_gh(top, &args)?;
    Ok(format!(
        "pr: {} (opened{})\n",
        url.lines().last().unwrap_or_default(),
        if a.draft { ", draft" } else { "" }
    ))
}
