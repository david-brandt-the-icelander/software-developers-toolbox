// `--commit SHA` on repository commands: answers only from repositories checked out at that commit, narrows searches over a folder of repositories to them, and names the commit each answer came from.
use super::Ctx;
use crate::args::{self, opt, Flag};
use crate::indexer::{root_name, work_tree};
use crate::util::{display, locate};
use std::path::{Path, PathBuf};
use std::process::Command;

const REPO: Flag = opt("repo", 'r');

// What a command's arguments name: the flags it takes, the first positional that is a path, and whether only that one is.
struct Shape {
    spec: &'static [Flag],
    first_path: usize,
    one_path: bool,
    narrow: Narrow,
}

// How a search over a folder of repositories is narrowed to those at the commit: by listing them as paths, by --repo, or not at all.
#[derive(PartialEq)]
enum Narrow {
    Paths,
    Repo,
    No,
}

// Returns the argument shape of a repository command, or None for commands --commit does not apply to.
fn shape(cmd: &str) -> Option<Shape> {
    let s = |spec, first_path, one_path, narrow| Some(Shape { spec, first_path, one_path, narrow });
    match cmd {
        "find" | "grep" => s(super::find::SPEC, 1, false, Narrow::Paths),
        "refs" => s(super::refs::SPEC, 1, false, Narrow::Paths),
        "ls" => s(super::ls::SPEC, 0, false, Narrow::Paths),
        "outline" => s(super::outline::SPEC, 0, false, Narrow::Paths),
        "read" | "cat" => s(super::read::SPEC, 0, false, Narrow::No),
        "edit" => s(super::edit::SPEC, 0, true, Narrow::No),
        "overview" | "changed-since" => s(&[REPO], usize::MAX, false, Narrow::Repo),
        "find-symbol" => s(super::structure::SYMBOL_SPEC, usize::MAX, false, Narrow::Repo),
        "git-log" => s(super::structure::LOG_SPEC, 0, true, Narrow::Repo),
        "git-blame" => s(super::structure::BLAME_SPEC, 0, true, Narrow::No),
        _ => None,
    }
}

// Runs git in a repository and returns its trimmed output, or None when it fails.
fn git(repo: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").arg("-C").arg(repo).args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

// Returns the commit a work tree has checked out.
fn head(repo: &Path) -> Option<String> {
    git(repo, &["rev-parse", "HEAD"])
}

// True when the commit asked for is the one checked out: `HEAD` always is, else a hex prefix of at least four digits.
fn same(head: &str, want: &str) -> bool {
    let w = want.to_ascii_lowercase();
    want == "HEAD" || w.len() >= 4 && w.chars().all(|c| c.is_ascii_hexdigit()) && head.starts_with(&w)
}

// Explains why a repository cannot answer for a commit, and where that commit is, if it can tell.
fn elsewhere(repo: &Path, head: &str, want: &str, shown: &str) -> String {
    let mut msg = format!("{shown} is in {} at commit {head}, not {want}", repo.display());
    let list = git(repo, &["worktree", "list", "--porcelain"]).unwrap_or_default();
    let mut path = None;
    for line in list.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            path = Some(p.to_string());
        } else if let (Some(h), Some(p)) = (line.strip_prefix("HEAD "), &path) {
            if same(h, want) {
                return format!(
                    "{msg}; that commit is checked out in the worktree {p}: ask with paths there"
                );
            }
        }
    }
    msg.push_str(match git(repo, &["cat-file", "-e", &format!("{want}^{{commit}}")]) {
        Some(_) => "; the repository has it, but no worktree has it checked out (`git worktree add DIR COMMIT` makes one)",
        None => "; the repository does not have that commit",
    });
    msg
}

// Describes the commit a repository answered from, and whether the files (or the whole tree) differ from it.
fn footer(repo: &Path, head: &str, files: &[PathBuf], cwd: &Path) -> String {
    let rel: Vec<String> = files
        .iter()
        .filter_map(|f| f.strip_prefix(repo).ok())
        .map(|f| f.to_string_lossy().into_owned())
        .collect();
    let mut args = vec!["status", "--porcelain", "--untracked-files=no", "--"];
    args.extend(rel.iter().map(String::as_str));
    let dirty = git(repo, &args).is_some_and(|s| !s.is_empty());
    let name = if repo.starts_with(cwd) && repo != cwd { display(repo, cwd) } else { root_name(repo) };
    let note = match (dirty, rel.is_empty()) {
        (false, _) => "",
        (true, true) => ", with uncommitted changes in the work tree",
        (true, false) => ", with uncommitted changes in these files",
    };
    format!("[commit {head} {name}{note}]")
}

// Checks the repositories a command names are at the commit, narrows a search over a folder of repositories to those that are, and returns the arguments to run and the commit lines to add to the answer.
pub fn apply(ctx: &Ctx, cmd: &str, args: &[String], want: &str) -> Result<(Vec<String>, String), String> {
    let sh = shape(cmd).ok_or_else(|| format!("--commit applies to repository commands (find, refs, ls, outline, read, edit, overview, find-symbol, changed-since, git-log, git-blame), not {cmd}"))?;
    let p = args::parse(sh.spec, args)?;
    let mut named: Vec<String> = p.positional.iter().skip(sh.first_path).cloned().collect();
    if sh.one_path {
        named.truncate(1);
    }
    named.extend(p.value("repo").map(String::from));
    let mut out = args.to_vec();
    let mut lines = Vec::new();
    let mut used: Vec<(PathBuf, String, Vec<PathBuf>)> = Vec::new();
    let mut groups: Vec<(Option<String>, Vec<PathBuf>)> = Vec::new();
    for arg in &named {
        let path_arg =
            if matches!(cmd, "read" | "cat") { super::read::split_target(arg).0 } else { arg.as_str() };
        let path = locate(path_arg, &ctx.cwd);
        match work_tree(&path) {
            Some(t) => {
                let h =
                    head(t).ok_or_else(|| format!("{arg}: git cannot read the commit of {}", t.display()))?;
                if !same(&h, want) {
                    return Err(elsewhere(t, &h, want, arg));
                }
                match used.iter_mut().find(|(r, _, _)| r == t) {
                    Some(u) => u.2.push(path.clone()),
                    None => used.push((t.to_path_buf(), h, vec![path.clone()])),
                }
            }
            None => groups.push((Some(arg.clone()), super::structure::repos_below(&path))),
        }
    }
    if named.is_empty() {
        match work_tree(&ctx.cwd) {
            Some(t) => {
                let h = head(t).ok_or_else(|| format!("git cannot read the commit of {}", t.display()))?;
                if !same(&h, want) {
                    return Err(elsewhere(t, &h, want, "the working directory"));
                }
                used.push((t.to_path_buf(), h, Vec::new()));
            }
            None => groups.push((None, super::structure::repos_below(&ctx.cwd))),
        }
    }
    for (arg, repos) in groups {
        let heads: Vec<(PathBuf, String)> =
            repos.iter().filter_map(|r| Some((r.clone(), head(r)?))).collect();
        let at: Vec<&(PathBuf, String)> = heads.iter().filter(|(_, h)| same(h, want)).collect();
        if at.is_empty() {
            let seen: Vec<String> = heads
                .iter()
                .map(|(r, h)| format!("{} at {}", display(r, &ctx.cwd), &h[..h.len().min(12)]))
                .collect();
            return Err(format!(
                "no repository here is at commit {want} ({})",
                if seen.is_empty() { "none found".into() } else { seen.join(", ") }
            ));
        }
        if at.len() < heads.len() {
            let names: Vec<String> = at.iter().map(|(r, _)| display(r, &ctx.cwd)).collect();
            match (&sh.narrow, &arg) {
                (Narrow::Paths, Some(a)) => {
                    let i =
                        out.iter().rposition(|x| x == a).expect("a named argument is among the arguments");
                    out.splice(i..=i, names);
                }
                (Narrow::Paths, None) => out.extend(names),
                (Narrow::Repo, None) if names.len() == 1 => {
                    out.extend(["--repo".to_string(), names[0].clone()])
                }
                _ => {
                    return Err(format!(
                    "{} of the repositories here are at commit {want} ({}); name one with --repo or a path",
                    names.len(),
                    names.join(", ")
                ))
                }
            }
        }
        used.extend(at.into_iter().map(|(r, h)| (r.clone(), h.clone(), Vec::new())));
    }
    for (repo, h, files) in &used {
        lines.push(footer(repo, h, files, &ctx.cwd));
    }
    Ok((out, lines.join("\n")))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::testutil::{run_cmd, TempDir};

    // Makes a git repository with one committed file and returns its commit.
    fn repo(dir: &Path, file: &str, text: &str) -> String {
        std::fs::create_dir_all(dir).expect("mkdir");
        std::fs::write(dir.join(file), text).expect("write");
        let g = |a: &[&str]| Command::new("git").arg("-C").arg(dir).args(a).output().expect("git");
        g(&["init", "-q"]);
        g(&["add", "."]);
        g(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "c"]);
        head(dir).expect("head")
    }

    // Checks matching commits, refusals naming the checked-out commit, narrowing over a folder of repositories, and the commit lines.
    #[test]
    fn test_commit_pins() {
        let t = TempDir::new();
        let w = t.path().canonicalize().expect("canon");
        let a = repo(&w.join("api"), "a.txt", "alpha needle\n");
        let b = repo(&w.join("web"), "b.txt", "beta needle\n");
        let run = |dir: &Path, args: &[&str]| run_cmd(dir, args).text;
        let short = &a[..10];
        let read = run(&w.join("api"), &["read", "a.txt", "--commit", short]);
        assert!(read.ends_with(&format!("[commit {a} api]")), "{read}");
        let bad = run(&w.join("api"), &["read", "a.txt", "--commit", &b[..10]]);
        assert!(
            bad.starts_with(&format!(
                "error: a.txt is in {} at commit {a}, not {}; the repository does not have that commit",
                w.join("api").display(),
                &b[..10]
            )),
            "{bad}"
        );
        let found = run(&w, &["find", "needle", "--commit", short]);
        assert!(
            found.contains("a.txt")
                && !found.contains("b.txt")
                && found.ends_with(&format!("[commit {a} api]")),
            "{found}"
        );
        let both = run(&w, &["find", "needle", ".", "--commit", "HEAD"]);
        assert!(
            both.contains("a.txt") && both.contains("b.txt") && both.contains(&format!("[commit {b} web]")),
            "{both}"
        );
        let narrowed = run(&w, &["ls", ".", "--commit", &b[..8]]);
        assert!(narrowed.contains("b.txt") && !narrowed.contains("a.txt"), "{narrowed}");
        assert!(run(&w, &["find", "needle", "--commit", "abcd1234"])
            .starts_with("error: no repository here is at commit abcd1234 (api at "));
        let one = run(&w, &["overview", "--commit", &a[..10]]);
        assert!(!one.contains("at commit"), "only api is at it, so overview gets --repo api: {one}");
        Command::new("git")
            .args(["clone", "-q"])
            .arg(w.join("api"))
            .arg(w.join("api2"))
            .output()
            .expect("clone");
        let two = run(&w, &["find", "needle", "--commit", short]);
        assert!(two.contains("api/a.txt") && two.contains("api2/a.txt") && !two.contains("b.txt"), "{two}");
        assert!(run(&w, &["overview", "--commit", short])
            .starts_with(&format!("error: 2 of the repositories here are at commit {short} (api, api2)")));
        std::fs::remove_dir_all(w.join("api2")).expect("rm");
        std::fs::write(w.join("api/a.txt"), "changed\n").expect("write");
        assert!(run(&w.join("api"), &["read", "a.txt", "--commit", short])
            .ends_with(", with uncommitted changes in these files]"));
        assert!(run(&w.join("api"), &["ls", "--commit", short])
            .ends_with(", with uncommitted changes in the work tree]"));
        assert!(run(&w, &["status", "--commit", short])
            .starts_with("error: --commit applies to repository commands"));
        assert!(run(&w.join("api"), &["read", "a.txt", "--commit", "zz"]).contains("not zz"));
        Command::new("git")
            .arg("-C")
            .arg(w.join("api"))
            .args(["worktree", "add", "-q", "--detach"])
            .arg(w.join("api-old"))
            .arg("HEAD")
            .output()
            .expect("worktree");
        std::fs::write(w.join("api/a.txt"), "alpha needle\nmore\n").expect("write");
        Command::new("git")
            .arg("-C")
            .arg(w.join("api"))
            .args(["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qam", "two"])
            .output()
            .expect("commit");
        let moved = run(&w.join("api"), &["read", "a.txt", "--commit", short]);
        assert!(
            moved.contains(&format!(
                "that commit is checked out in the worktree {}",
                w.join("api-old").display()
            )),
            "{moved}"
        );
        let first = head(&w.join("web")).expect("head");
        Command::new("git")
            .arg("-C")
            .arg(w.join("web"))
            .args(["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "e"])
            .output()
            .expect("commit");
        assert!(run(&w.join("web"), &["read", "b.txt", "--commit", &first[..10]])
            .contains("the repository has it, but no worktree has it checked out"));
        assert_eq!(run(&w, &["read", "--commit"]), "error: --commit needs a commit sha (or HEAD)");
        assert!(run(&w, &["find", "x", "--zz", "--commit", "HEAD"]).starts_with("error: unknown flag"));
    }
}
