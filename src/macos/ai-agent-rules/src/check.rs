// `check [PATH]`: finds broken `@file` imports, `paths:` globs that match no file and bad skill front matter in the rule and skill files that apply at PATH and in the folders below it.
use super::{children, front, glob, holds_rules, imports, json_str, trace, Found, Kind, Traced, Why};
use std::fs;
use std::path::{Path, PathBuf};

// One problem in a rule or skill file: where it is, its kind (import, paths or skill) and what is wrong.
#[derive(Debug, PartialEq)]
pub struct Problem {
    pub file: PathBuf,
    pub line: usize,
    pub kind: &'static str,
    pub message: String,
}

// Returns the nearest folder at or above `dir` that holds `.git`, or `dir` itself when none does.
fn repo_root(dir: &Path) -> PathBuf {
    dir.ancestors().find(|a| a.join(".git").exists()).unwrap_or(dir).to_path_buf()
}

// Tells whether a path is a real folder, not a symlink to one.
fn real_dir(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.is_dir())
}

// Collects the folders below `dir` that hold rule or skill places, skipping hidden folders, node_modules and symlinks.
fn rule_dirs(dir: &Path, out: &mut Vec<PathBuf>) {
    for path in children(dir) {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if !real_dir(&path) || name.starts_with('.') || name == "node_modules" {
            continue;
        }
        if holds_rules(&path) {
            out.push(path.clone());
        }
        rule_dirs(&path, out);
    }
}

// Tells whether a file or folder under `dir` matches a glob relative to `base`, skipping .git and not following symlinked folders.
fn any_match(base: &Path, dir: &Path, glob: &str) -> bool {
    children(dir).iter().filter(|p| !p.ends_with(".git")).any(|p| {
        let rel = p.strip_prefix(base).unwrap_or(p).to_string_lossy().into_owned();
        glob::matches(glob, &rel, false) || (real_dir(p) && any_match(base, p, glob))
    })
}

// Returns the 1-based number of the first line that passes the test, or 1.
fn line_of(text: &str, test: impl Fn(&str) -> bool) -> usize {
    text.lines().position(test).map_or(1, |i| i + 1)
}

// Returns a problem in a file.
fn problem(f: &Found, line: usize, kind: &'static str, message: String) -> Problem {
    Problem { file: f.path.clone(), line, kind, message }
}

// Returns the `@path` imports of a rule or imported file that point to no file; words without `.` or `/` are taken for mentions, not imports.
fn import_problems(f: &Found, home: Option<&Path>) -> Vec<Problem> {
    let text = f.text.as_deref().unwrap_or("");
    let dir = f.path.parent().unwrap_or(Path::new("/"));
    imports::tokens(text)
        .into_iter()
        .filter(|(_, t)| t.contains(['.', '/']) && imports::resolve(t, dir, home).is_none())
        .map(|(line, t)| problem(f, line, "import", format!("@{t} points to no file")))
        .collect()
}

// Returns the `paths:` globs of a scoped rule that match nothing under the folder holding its rules.
fn glob_problems(f: &Found, base: &Path) -> Vec<Problem> {
    let text = f.text.as_deref().unwrap_or("");
    let globs = front::list(text, "paths").unwrap_or_default();
    globs
        .iter()
        .filter(|g| !any_match(base, base, g))
        .map(|g| {
            let line = line_of(text, |l| l.contains(g.as_str()));
            problem(f, line, "paths", format!("paths: `{g}` matches no file under {}", base.display()))
        })
        .collect()
}

// Returns what is wrong with a skill's front matter: no `name` or `description`, or a `name` that is not its folder's.
fn skill_problems(f: &Found) -> Vec<Problem> {
    let text = f.text.as_deref().unwrap_or("");
    let folder = f.path.parent().and_then(Path::file_name).unwrap_or_default().to_string_lossy();
    let field = |key| front::field(text, key).filter(|v| !v.trim().is_empty());
    let mut out = Vec::new();
    match field("name") {
        None => out.push(problem(f, 1, "skill", "front matter has no `name`".to_string())),
        Some(n) if n != folder => {
            let line = line_of(text, |l| l.starts_with("name:"));
            out.push(problem(f, line, "skill", format!("name `{n}` does not match its folder `{folder}`")));
        }
        Some(_) => {}
    }
    if field("description").is_none() {
        out.push(problem(f, 1, "skill", "front matter has no `description`".to_string()));
    }
    out
}

// Returns the problems of one applying file; `paths:` globs are checked only for rules kept in the repository.
fn file_problems(x: &Traced, home: Option<&Path>, root: &Path) -> Vec<Problem> {
    match &x.why {
        _ if x.found.kind == Kind::Skill => skill_problems(&x.found),
        Why::Glob(base, _) if base.starts_with(root) => {
            let mut out = import_problems(&x.found, home);
            out.extend(glob_problems(&x.found, base));
            out
        }
        _ => import_problems(&x.found, home),
    }
}

// Returns how many files apply at a path or in the folders below it, and their problems, looking no higher than `top`.
pub fn problems(path: &Path, home: Option<&Path>, top: &Path) -> Result<(usize, Vec<Problem>), String> {
    let (t, mut all) = trace(path, home, &[], top)?;
    let root = repo_root(&t.dir);
    let mut dirs = Vec::new();
    if t.is_dir {
        rule_dirs(&t.dir, &mut dirs);
    }
    for d in dirs {
        all.extend(trace(&d, home, &[], top).map(|r| r.1).unwrap_or_default());
    }
    let home = home.and_then(|h| fs::canonicalize(h).ok());
    let mut seen: Vec<PathBuf> = Vec::new();
    let mut out = Vec::new();
    for x in all.iter().filter(|x| x.why.applies()) {
        let c = fs::canonicalize(&x.found.path).unwrap_or_else(|_| x.found.path.clone());
        if seen.contains(&c) {
            continue;
        }
        seen.push(c);
        out.extend(file_problems(x, home.as_deref(), &root));
    }
    Ok((seen.len(), out))
}

// Formats problems as a JSON array of {file, line, kind, message}.
fn json(found: &[Problem]) -> String {
    let items: Vec<String> = found
        .iter()
        .map(|p| {
            let file = json_str(&p.file.to_string_lossy());
            let msg = json_str(&p.message);
            format!("{{\"file\":{file},\"line\":{},\"kind\":\"{}\",\"message\":{msg}}}", p.line, p.kind)
        })
        .collect();
    format!("[{}]\n", items.join(",\n"))
}

// Runs check and returns one `FILE:LINE: message` line per problem (or JSON) and exit 1 when there are problems, 0 when clean, 66 when PATH is missing.
pub fn run(path: &Path, home: Option<&Path>, top: &Path, as_json: bool) -> (String, i32) {
    let (files, found) = match problems(path, home, top) {
        Ok(r) => r,
        Err(e) => return (format!("{e}\n"), 66),
    };
    let code = i32::from(!found.is_empty());
    if as_json {
        return (json(&found), code);
    }
    if found.is_empty() {
        return (format!("no problems in {files} files\n"), 0);
    }
    let lines = found.iter().map(|p| format!("{}:{}: {}\n", p.file.display(), p.line, p.message));
    (lines.collect(), code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{put, run_in, temp, write};

    // Lists the problems as `NAME:LINE: message` with names relative to root.
    fn found(root: &Path, path: &Path, home: Option<&Path>) -> Vec<String> {
        let (_, ps) = problems(path, home, root).expect("checks");
        ps.iter()
            .map(|p| {
                format!(
                    "{}:{}: {}",
                    p.file.strip_prefix(root).unwrap_or(&p.file).display(),
                    p.line,
                    p.message
                )
            })
            .collect()
    }

    // A tree whose imports, globs and skills are all sound has no problems.
    #[test]
    fn test_check_clean_tree() {
        let root = temp("chk-clean");
        write(&root.join("AGENTS.md"), "Read @docs/a.md, ask @someone, not `@x.md`.\n```\n@y.md\n```\n");
        put(&root.join("docs/a.md"));
        write(&root.join(".claude/rules/d.md"), "---\npaths:\n  - \"docs/*.md\"\n  - empty/**\n---\n");
        fs::create_dir_all(root.join("empty")).expect("folder");
        write(&root.join("agents/skills/s/SKILL.md"), "---\nname: s\ndescription: Does s.\n---\n");
        assert!(found(&root, &root, None).is_empty());
        let dir = root.to_str().expect("utf8");
        assert_eq!(run_in(&root, &["check", dir]), ("no problems in 4 files\n".to_string(), 0));
        assert_eq!(run_in(&root, &["check", dir, "--json"]), ("[]\n".to_string(), 0));
    }

    // Broken imports, dead globs and bad skill front matter are each reported at their line, also in folders below.
    #[test]
    fn test_check_reports_problems() {
        let root = temp("chk-bad");
        write(&root.join("AGENTS.md"), "intro\nsee @missing.md and @~/home.md\n");
        write(&root.join(".claude/rules/r.md"), "---\npaths:\n  - src/**\n  - gone/*.rs\n---\n");
        put(&root.join("src/a.rs"));
        write(&root.join("agents/skills/s/SKILL.md"), "no front matter\n");
        write(&root.join("agents/skills/t/SKILL.md"), "---\ndescription: d\nname: other\n---\n");
        write(&root.join("agents/skills/u/SKILL.md"), "---\nname:\ndescription: d\n---\n");
        write(&root.join("sub/deep/AGENTS.md"), "@../nope.md\n");
        write(&root.join(".hidden/AGENTS.md"), "@bad.md\n");
        write(&root.join("node_modules/p/AGENTS.md"), "@bad.md\n");
        std::os::unix::fs::symlink(root.join("sub"), root.join("again")).expect("symlink");
        let want = [
            "AGENTS.md:2: @missing.md points to no file",
            "AGENTS.md:2: @~/home.md points to no file",
            ".claude/rules/r.md:4: paths: `gone/*.rs` matches no file under ",
            "agents/skills/s/SKILL.md:1: front matter has no `name`",
            "agents/skills/s/SKILL.md:1: front matter has no `description`",
            "agents/skills/t/SKILL.md:3: name `other` does not match its folder `t`",
            "agents/skills/u/SKILL.md:1: front matter has no `name`",
            "sub/deep/AGENTS.md:1: @../nope.md points to no file",
        ];
        let got = found(&root, &root, None);
        assert_eq!(got.len(), want.len(), "{got:#?}");
        for (g, w) in got.iter().zip(want) {
            assert!(g.starts_with(w), "{g} vs {w}");
        }
        let (text, code) = run_in(&root, &["check", root.to_str().expect("utf8")]);
        assert_eq!(code, 1);
        assert!(text.starts_with(&format!(
            "{}:2: @missing.md points to no file\n",
            root.join("AGENTS.md").display()
        )));
        let (json, code) = run_in(&root, &["check", root.to_str().expect("utf8"), "--json"]);
        assert_eq!(code, 1);
        assert!(json.starts_with(&format!(
            "[{{\"file\":\"{}\",\"line\":2,\"kind\":\"import\",\"message\":\"@missing.md points to no file\"}},\n",
            root.join("AGENTS.md").display()
        )));
        assert!(json.contains("\"kind\":\"paths\"") && json.contains("\"kind\":\"skill\""));
    }

    // Globs of rules kept outside the repository (the home folder) are not checked, and a file PATH checks only what applies to it.
    #[test]
    fn test_check_home_rules_and_file_path() {
        let root = temp("chk-home");
        let home = root.join("home");
        write(&home.join(".claude/rules/h.md"), "---\npaths: \"**/nowhere\"\n---\n@lost.md\n");
        fs::create_dir_all(root.join("repo/.git")).expect("folder");
        write(&root.join("repo/sub/AGENTS.md"), "@x.md\n");
        write(&root.join("repo/AGENTS.md"), "@y.md\n");
        put(&root.join("repo/file.txt"));
        let got = found(&root, &root.join("repo"), Some(&home));
        let want = [
            "home/.claude/rules/h.md:4: @lost.md points to no file",
            "repo/AGENTS.md:1: @y.md points to no file",
            "repo/sub/AGENTS.md:1: @x.md points to no file",
        ];
        assert_eq!(got, want);
        assert_eq!(found(&root, &root.join("repo/file.txt"), Some(&home)), want[1..2]);
        assert_eq!(repo_root(&root.join("repo/sub")), root.join("repo"));
    }

    // A missing PATH is exit 66, and check takes the current folder when no PATH is given.
    #[test]
    fn test_check_missing_and_default_path() {
        let root = temp("chk-missing");
        let (text, code) = run_in(&root, &["check", root.join("nope").to_str().expect("utf8")]);
        assert!(code == 66 && text.contains("nope"));
        assert_eq!(run_in(&root, &["check"]).1, 0);
    }
}
