// Git for the repository workflow: the repository's top, branch, head and remote, the tree of the working tree as it stands (untracked files included, the index untouched), diffs against a base, and the private state folder inside the git directory where receipts, logs and review ledgers live, never committed.
use std::path::{Path, PathBuf};
use std::process::Command;

// Runs git in `dir` with extra environment and returns its trimmed standard output, or its last error line.
pub fn git_env(dir: &Path, args: &[&str], env: &[(&str, &Path)]) -> Result<String, String> {
    let mut c = Command::new("git");
    c.arg("-C").arg(dir).args(args).stdin(std::process::Stdio::null());
    for (k, v) in env {
        c.env(k, v);
    }
    let out = c.output().map_err(|e| format!("running git: {e}"))?;
    match out.status.success() {
        true => Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string()),
        false => {
            let err = String::from_utf8_lossy(&out.stderr);
            let lines: Vec<&str> = err
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .filter(|l| !l.starts_with("hint:"))
                .collect();
            let why =
                lines.iter().find(|l| l.contains("[rejected]")).or(lines.last()).copied().unwrap_or("failed");
            Err(format!("git {}: {why}", args.first().unwrap_or(&"")))
        }
    }
}

// Runs git in `dir`.
pub fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    git_env(dir, args, &[])
}

// The repository's top folder.
pub fn top(dir: &Path) -> Result<PathBuf, String> {
    git(dir, &["rev-parse", "--show-toplevel"])
        .map(PathBuf::from)
        .map_err(|_| format!("{} is not inside a git repository", dir.display()))
}

// The checked-out branch, or an error for a detached head.
pub fn branch(dir: &Path) -> Result<String, String> {
    match git(dir, &["symbolic-ref", "--quiet", "--short", "HEAD"]) {
        Ok(b) => Ok(b),
        Err(_) => Err("HEAD is detached: check out a branch first".into()),
    }
}

// The commit HEAD names, or None in a repository with no commit.
pub fn head(dir: &Path) -> Option<String> {
    git(dir, &["rev-parse", "--verify", "--quiet", "HEAD"]).ok()
}

// The private state folder `<git dir>/zazzle/<sub>`, created on demand.
pub fn state(dir: &Path, sub: &str) -> Result<PathBuf, String> {
    let gd = git(dir, &["rev-parse", "--absolute-git-dir"])?;
    let p = PathBuf::from(gd).join("zazzle").join(sub);
    std::fs::create_dir_all(&p).map_err(|e| format!("{}: {e}", p.display()))?;
    Ok(p)
}

// The tree of the working tree as it stands, untracked (not ignored) files included, written through a temporary index so the real index is untouched; the copy keeps the real index's modification time, so git still re-reads files changed in the same second (a fresh time would make it trust their old stat data).
pub fn worktree_tree(dir: &Path) -> Result<String, String> {
    let tmp =
        state(dir, "tmp")?.join(format!("index-{}-{:?}", std::process::id(), std::thread::current().id()));
    let real = PathBuf::from(git(dir, &["rev-parse", "--absolute-git-dir"])?).join("index");
    let _ = std::fs::copy(&real, &tmp).and_then(|_| {
        let m = std::fs::metadata(&real)?.modified()?;
        std::fs::File::options().write(true).open(&tmp)?.set_modified(m)
    });
    let env = [("GIT_INDEX_FILE", tmp.as_path())];
    let tree = git_env(dir, &["add", "-A"], &env).and_then(|_| git_env(dir, &["write-tree"], &env));
    let _ = std::fs::remove_file(&tmp);
    tree
}

// The tree a commit (or HEAD) has.
pub fn tree_of(dir: &Path, rev: &str) -> Result<String, String> {
    git(dir, &["rev-parse", &format!("{rev}^{{tree}}")])
}

// The paths that differ between two tree-ishes.
pub fn changed(dir: &Path, from: &str, to: &str) -> Result<Vec<String>, String> {
    Ok(git(dir, &["diff", "--name-only", from, to])?.lines().map(str::to_string).collect())
}

// The diff between two tree-ishes, with a few lines of context.
pub fn diff(dir: &Path, from: &str, to: &str) -> Result<String, String> {
    git(dir, &["diff", "-U3", from, to])
}

// Where a feature branch forks from `target`: the merge base with origin/TARGET when the remote has it, else with TARGET, else TARGET itself.
pub fn base(dir: &Path, target: &str) -> Result<String, String> {
    for r in [format!("origin/{target}"), target.to_string()] {
        if let Ok(b) = git(dir, &["merge-base", "HEAD", &r]) {
            return Ok(b);
        }
    }
    git(dir, &["rev-parse", "--verify", target])
        .map_err(|_| format!("no branch `{target}` here or on origin to compare with"))
}

// Restores `paths` to their content in `tree`, deleting those `tree` does not have.
pub fn restore(dir: &Path, tree: &str, paths: &[String]) -> Result<(), String> {
    for p in paths {
        let there = git(dir, &["cat-file", "-e", &format!("{tree}:{p}")]).is_ok();
        match there {
            true => git(dir, &["restore", "--source", tree, "--worktree", "--", p]).map(|_| ())?,
            false => {
                let _ = std::fs::remove_file(dir.join(p));
            }
        }
    }
    Ok(())
}

// Tells whether a repository path lies under one of the given prefixes (a file or a folder, with or without a trailing slash).
pub fn under(path: &str, prefixes: &[String]) -> bool {
    prefixes.iter().any(|p| {
        let p = p.trim_end_matches('/');
        p.is_empty() || p == "." || path == p || path.starts_with(&format!("{p}/"))
    })
}
