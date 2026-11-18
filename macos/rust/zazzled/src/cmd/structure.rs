// The structure commands answered by zazzle-csharp-indexer's daemon: overview, find-symbol, changed-since, git-log, git-blame and report-gap.
use super::{Ctx, Output};
use crate::args::{self, flag, opt, Flag};
use crate::indexer::{localize, root_name, work_tree, Indexer, Root};
use crate::util::{display, locate};
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

const REPO: Flag = opt("repo", 'r');
pub(super) const SYMBOL_SPEC: &[Flag] =
    &[flag("exact", 'x'), opt("kind", 'k'), opt("limit", 'm'), opt("offset", 'o'), REPO];
pub(super) const LOG_SPEC: &[Flag] = &[opt("n", 'n'), opt("grep", '\0'), REPO];
pub(super) const BLAME_SPEC: &[Flag] = &[opt("start", '\0'), opt("end", '\0')];
const OVERVIEW_USAGE: &str = "usage: overview [--repo DIR]";
const SYMBOL_USAGE: &str =
    "usage: find-symbol NAME [--exact] [--kind K] [--limit N] [--offset N] [--repo DIR]";
const CHANGED_USAGE: &str = "usage: changed-since [CURSOR] [--repo DIR]";
const LOG_USAGE: &str = "usage: git-log [PATH] [-n N] [--grep TEXT] [--repo DIR]";
const BLAME_USAGE: &str = "usage: git-blame PATH [--start N] [--end N]";
const GAP_USAGE: &str = "usage: report-gap TEXT [--repo DIR]";
// How deep below a folder that is not a work tree to look for repositories, and how many to search at most.
const REPO_DEPTH: usize = 3;
const MAX_REPOS: usize = 24;
// Folders never searched for repositories: build output and package caches.
const SKIP_DIRS: &[&str] = &["node_modules", "bin", "obj", "target"];

// The repositories a command works on: --repo's, the working directory's, or those below a folder that is not one; `found` counts all below.
struct Repos {
    roots: Vec<Root>,
    found: usize,
    below: bool,
}

// Returns the single root for a work tree.
fn single(root: &Path) -> Root {
    (root_name(root), root.to_path_buf())
}

// Finds the git work trees below a folder, outermost only, skipping hidden and build folders, sorted by path.
pub fn repos_below(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut level = vec![dir.to_path_buf()];
    for _ in 0..REPO_DEPTH {
        let mut next = Vec::new();
        for d in level {
            let Ok(entries) = std::fs::read_dir(&d) else { continue };
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if !e.file_type().is_ok_and(|t| t.is_dir())
                    || name.starts_with('.')
                    || SKIP_DIRS.contains(&name.as_str())
                {
                    continue;
                }
                if e.path().join(".git").exists() {
                    found.push(e.path());
                } else {
                    next.push(e.path());
                }
            }
        }
        level = next;
    }
    found.sort();
    found
}

// Picks the repositories: --repo's work tree, else the working directory's, else up to MAX_REPOS below it named by their path from it.
fn repos(ctx: &Ctx, repo: Option<&str>) -> Result<Repos, String> {
    if let Some(r) = repo {
        let root = work_tree(&locate(r, &ctx.cwd))
            .map(single)
            .ok_or_else(|| format!("{r}: not inside a git work tree"))?;
        return Ok(Repos { roots: vec![root], found: 1, below: false });
    }
    if let Some(root) = work_tree(&ctx.cwd) {
        return Ok(Repos { roots: vec![single(root)], found: 1, below: false });
    }
    let all = repos_below(&ctx.cwd);
    if all.is_empty() {
        return Err("not inside a git work tree, and no git repositories below this folder".into());
    }
    let name = |p: &PathBuf| display(p, &ctx.cwd).replace([':', ';', '='], "_");
    let roots = all.iter().take(MAX_REPOS).map(|p| (name(p), p.clone())).collect();
    Ok(Repos { roots, found: all.len(), below: true })
}

// Where a structure command runs: the indexer, the repositories, and the working directory.
struct Target {
    indexer: Indexer,
    repos: Repos,
    cwd: PathBuf,
    session: String,
}

impl Target {
    // Finds the indexer and the repositories a command works on.
    fn locate(ctx: &Ctx, repo: Option<&str>) -> Result<Target, String> {
        let repos = repos(ctx, repo)?;
        Ok(Target { indexer: indexer()?, repos, cwd: ctx.cwd.clone(), session: ctx.session.clone() })
    }

    // Finds the indexer and the work tree holding a path argument, wherever the working directory is; returns the path relative to it.
    fn for_path(ctx: &Ctx, arg: &str) -> Result<(Target, String), String> {
        let full = locate(arg, &ctx.cwd);
        let root =
            work_tree(&full).ok_or_else(|| format!("{arg}: not inside a git work tree"))?.to_path_buf();
        let rel = full.strip_prefix(&root).expect("work tree holds the path").to_string_lossy().into_owned();
        let repos = Repos { roots: vec![single(&root)], found: 1, below: false };
        Ok((Target { indexer: indexer()?, repos, cwd: ctx.cwd.clone(), session: ctx.session.clone() }, rel))
    }

    // Runs one indexer tool on every repository and rewrites its root:path references for the working directory.
    fn ask(&self, tool: &str, args: Map<String, Value>) -> Result<Output, String> {
        let text = self
            .indexer
            .ask(&self.repos.roots, tool, Value::Object(args), &self.session)
            .map_err(|e| unavailable(&e))?;
        let mut text = localize(&text, &self.repos.roots, &self.cwd);
        if self.repos.found > self.repos.roots.len() {
            text.push_str(&format!(
                "\n[searched {} of {} git repositories below this folder; give --repo for another]",
                self.repos.roots.len(),
                self.repos.found
            ));
        }
        Ok(Output::ok(text))
    }
}

// Asks the indexer for exact C# references to a name (Roslyn's SymbolFinder over the repository under the working directory).
pub fn exact_refs(ctx: &Ctx, query: &str) -> Result<Output, String> {
    let mut a = Map::new();
    a.insert("name".into(), json!(query));
    Target::locate(ctx, None)?.ask("refs", a)
}

// What a structure command says when the indexer cannot answer: why, in one line, and what still works.
fn unavailable(why: &str) -> String {
    format!("C# indexer unavailable: {why}. find, read, ls, outline and refs work without it.")
}

// Finds the indexer, or says in one line why there is none and what still works.
fn indexer() -> Result<Indexer, String> {
    Indexer::locate().map_err(|e| unavailable(&e))
}

// Puts a numeric flag's value into the tool arguments under a key when it was given.
fn put_number(p: &args::Parsed, flag: &str, key: &str, out: &mut Map<String, Value>) -> Result<(), String> {
    if let Some(n) = p.number(flag)? {
        out.insert(key.into(), json!(n));
    }
    Ok(())
}

// Runs overview: a map of the work tree's languages, directories, entry points and largest files; in a folder holding several repositories, a list of them.
pub fn overview(ctx: &Ctx, argv: &[String]) -> Result<Output, String> {
    let p = args::parse(&[REPO], argv).map_err(|e| format!("{e}; {OVERVIEW_USAGE}"))?;
    if !p.positional.is_empty() {
        return Err(OVERVIEW_USAGE.into());
    }
    let r = repos(ctx, p.value("repo"))?;
    if r.below && r.found > 1 {
        let list: Vec<String> =
            repos_below(&ctx.cwd).iter().map(|d| format!("  {}", display(d, &ctx.cwd))).collect();
        return Ok(Output::ok(format!(
            "{} git repositories below this folder (not itself a work tree):\n{}\ngive --repo DIR for one's overview; find-symbol and changed-since search the first {MAX_REPOS} together.",
            r.found,
            list.join("\n")
        )));
    }
    Target { indexer: indexer()?, repos: r, cwd: ctx.cwd.clone(), session: ctx.session.clone() }
        .ask("overview", Map::new())
}

// Runs find-symbol: declarations by name from the index, across every repository below a folder that is not one.
pub fn find_symbol(ctx: &Ctx, argv: &[String]) -> Result<Output, String> {
    let p = args::parse(SYMBOL_SPEC, argv).map_err(|e| format!("{e}; {SYMBOL_USAGE}"))?;
    let [name] = p.positional.as_slice() else { return Err(SYMBOL_USAGE.into()) };
    let mut a = Map::new();
    a.insert("name".into(), json!(name));
    if p.has("exact") {
        a.insert("exact".into(), json!(true));
    }
    if let Some(k) = p.value("kind") {
        a.insert("kind".into(), json!(k));
    }
    put_number(&p, "limit", "limit", &mut a)?;
    put_number(&p, "offset", "offset", &mut a)?;
    Target::locate(ctx, p.value("repo"))?.ask("find_symbol", a)
}

// Finds where a symbol is declared through the index: the `path:line kind name | signature` lines, paths relative to the working directory.
pub fn declarations(ctx: &Ctx, name: &str) -> Result<Vec<String>, String> {
    let mut a = Map::new();
    a.insert("name".into(), json!(name));
    a.insert("exact".into(), json!(true));
    a.insert("limit".into(), json!(50));
    let out = Target::locate(ctx, None)?.ask("find_symbol", a)?;
    Ok(out.text.lines().filter(|l| !l.starts_with('[')).map(String::from).collect())
}

// Runs changed-since: files added, modified or deleted since a cursor, and a new cursor, across every repository below a folder that is not one.
pub fn changed_since(ctx: &Ctx, argv: &[String]) -> Result<Output, String> {
    let p = args::parse(&[REPO], argv).map_err(|e| format!("{e}; {CHANGED_USAGE}"))?;
    let mut a = Map::new();
    match p.positional.as_slice() {
        [] => {}
        [cursor] => {
            a.insert("cursor".into(), json!(cursor));
        }
        _ => return Err(CHANGED_USAGE.into()),
    }
    let mut out = Target::locate(ctx, p.value("repo"))?.ask("changed_since", a)?;
    out.idable = false;
    Ok(out)
}

// Runs git-log: recent commits of the repository holding the path (or --repo, or the working directory), with an optional message filter.
pub fn git_log(ctx: &Ctx, argv: &[String]) -> Result<Output, String> {
    let p = args::parse(LOG_SPEC, argv).map_err(|e| format!("{e}; {LOG_USAGE}"))?;
    let mut a = Map::new();
    let t = match p.positional.as_slice() {
        [] => {
            let t = Target::locate(ctx, p.value("repo"))?;
            if t.repos.below && t.repos.found > 1 {
                let names: Vec<&str> = t.repos.roots.iter().map(|(n, _)| n.as_str()).collect();
                return Err(format!(
                    "several git repositories below this folder: give a path or --repo ({})",
                    names.join(", ")
                ));
            }
            t
        }
        [path] => {
            if !crate::util::locate(path, &ctx.cwd).exists() {
                return Err(format!(
                    "{path}: no such file or directory{}",
                    crate::util::similar(path, &ctx.cwd, &ctx.engine.walks)
                ));
            }
            let (t, rel) = Target::for_path(ctx, path)?;
            a.insert("path".into(), json!(rel));
            t
        }
        _ => return Err(LOG_USAGE.into()),
    };
    put_number(&p, "n", "n", &mut a)?;
    if let Some(g) = p.value("grep") {
        a.insert("grep".into(), json!(g));
    }
    let mut out = t.ask("git_log", a)?;
    if out.text.trim().is_empty() {
        out.text = match (p.positional.first(), p.value("grep")) {
            (_, Some(g)) => format!("no commits whose message matches '{g}'"),
            (Some(path), None) => format!("no commits touch {path}"),
            (None, None) => "no commits".to_string(),
        };
    }
    Ok(out)
}

// Runs git-blame: who last changed each line of a range of a file, in whichever repository holds it.
pub fn git_blame(ctx: &Ctx, argv: &[String]) -> Result<Output, String> {
    let p = args::parse(BLAME_SPEC, argv).map_err(|e| format!("{e}; {BLAME_USAGE}"))?;
    let [path] = p.positional.as_slice() else { return Err(BLAME_USAGE.into()) };
    let (t, rel) = Target::for_path(ctx, path)?;
    let mut a = Map::new();
    a.insert("path".into(), json!(rel));
    put_number(&p, "start", "start_line", &mut a)?;
    put_number(&p, "end", "end_line", &mut a)?;
    t.ask("git_blame", a)
}

// Runs report-gap: records in the indexer's usage log what the agent needed and could not get.
pub fn report_gap(ctx: &Ctx, argv: &[String]) -> Result<Output, String> {
    let p = args::parse(&[REPO], argv).map_err(|e| format!("{e}; {GAP_USAGE}"))?;
    let what = p.positional.join(" ");
    if what.trim().is_empty() {
        return Err(GAP_USAGE.into());
    }
    crate::activity::record(json!({"src": "gap", "what": what, "session": ctx.session, "cwd": ctx.cwd}));
    let indexed = Target::locate(ctx, p.value("repo"))
        .and_then(|t| t.indexer.ask(&t.repos.roots[..1], "report_gap", json!({"what": what}), &t.session));
    Ok(Output::plain(match indexed {
        Ok(text) => text,
        Err(_) => "recorded in zazzle's activity log (the indexer's own log is unavailable)".into(),
    }))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use crate::indexer::tests::{echoing, FakeIndexer};
    use crate::testutil::{env_lock, run_cmd, TempDir};
    use serde_json::{json, Value};
    use std::ffi::OsString;

    // Points the bridge at a fake indexer that echoes each call, and makes a git work tree with a subdirectory.
    fn setup(fake: &FakeIndexer) -> (TempDir, String, Vec<(&'static str, Option<OsString>)>) {
        let saved = ["ZAZZLE_INDEXER", "ZAZZLE_INDEXER_STATE_DIR"]
            .iter()
            .map(|k| (*k, std::env::var_os(k)))
            .collect();
        std::env::set_var("ZAZZLE_INDEXER", &fake.exe);
        std::env::set_var("ZAZZLE_INDEXER_STATE_DIR", fake.dir.path());
        let t = TempDir::new();
        std::fs::create_dir_all(t.path().join(".git")).expect("git dir");
        t.write("src/C.cs", b"class C {}\n");
        let name = t.path().file_name().expect("name").to_string_lossy().into_owned();
        (t, name, saved)
    }

    // Puts back the environment a test changed.
    fn restore(saved: Vec<(&'static str, Option<OsString>)>) {
        for (k, v) in saved {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
    }

    // Checks changed-since is sent in full every time in a session, since the same answer later means something new.
    #[test]
    fn test_changed_since_is_never_deduplicated() {
        let _g = env_lock();
        let fake = FakeIndexer::start(echoing);
        let (t, _name, saved) = setup(&fake);
        let e = crate::engine::Engine::new();
        let first = crate::testutil::run_cmd_with(&e, t.path(), "s", &["changed-since"]).text;
        let second = crate::testutil::run_cmd_with(&e, t.path(), "s", &["changed-since"]).text;
        assert_eq!((first.as_str(), second.as_str()), ("\"changed_since\" {}", "\"changed_since\" {}"));
        restore(saved);
    }

    // Checks refs answers from the indexer's Roslyn tool when a C# file matches, counts other text matches, and falls back to text when the name has no C# declaration.
    #[test]
    fn test_refs_asks_roslyn_first() {
        let _g = env_lock();
        let fake = FakeIndexer::start(echoing);
        let (t, _name, saved) = setup(&fake);
        t.write("notes.md", b"C is a class\n");
        let o = run_cmd(t.path(), &["refs", "C"]).text;
        assert_eq!(
            o,
            "\"refs\" {\"name\":\"C\"}\n-- text matches in 1 other files: zazzle refs C -g '!*.cs'"
        );
        let missing = FakeIndexer::start(|_, _| {
            (
                json!({"ok": true}),
                Some(
                    json!({"id": 1, "result": {"content": [{"type": "text", "text": "no C# declaration of C"}], "isError": true}}),
                ),
            )
        });
        std::env::set_var("ZAZZLE_INDEXER", &missing.exe);
        std::env::set_var("ZAZZLE_INDEXER_STATE_DIR", missing.dir.path());
        let text = run_cmd(t.path(), &["refs", "C"]).text;
        assert!(text.contains("defs\nsrc/C.cs:1-1 class C") && !text.contains("unavailable"), "{text}");
        assert!(run_cmd(t.path(), &["refs", "C", "src"]).text.contains("defs\nsrc/C.cs"));
        restore(saved);
    }

    // Checks each command calls its indexer tool with the arguments it was given, paths relative to the work tree.
    #[test]
    fn test_structure_commands_call_the_indexer() {
        let _g = env_lock();
        let fake = FakeIndexer::start(echoing);
        let (t, name, saved) = setup(&fake);
        let run = |args: &[&str]| run_cmd(t.path(), args).text;
        assert_eq!(run(&["overview"]), "\"overview\" {}");
        assert_eq!(
            run(&["find-symbol", "Spin", "--exact", "-k", "method", "-m", "5", "--offset", "2"]),
            "\"find_symbol\" {\"name\":\"Spin\",\"exact\":true,\"kind\":\"method\",\"limit\":5,\"offset\":2}"
        );
        assert_eq!(run(&["find-symbol", "Spin"]), "\"find_symbol\" {\"name\":\"Spin\"}");
        assert_eq!(run(&["changed-since"]), "\"changed_since\" {}");
        assert_eq!(run(&["changed-since", "e;r=1"]), "\"changed_since\" {\"cursor\":\"e;r=1\"}");
        assert_eq!(run(&["git-log"]), "\"git_log\" {}");
        assert_eq!(
            run(&["git-log", "src", "-n", "3", "--grep", "fix"]),
            format!("\"git_log\" {{\"path\":\"{name}:src\",\"n\":3,\"grep\":\"fix\"}}")
        );
        assert!(run(&["git-log", "srcx/C.cs"]).starts_with("error: srcx/C.cs: no such file or directory"));
        let sub = run_cmd(&t.path().join("src"), &["git-blame", "C.cs", "--start", "2", "--end", "4"]).text;
        assert_eq!(
            sub,
            format!("\"git_blame\" {{\"path\":\"{name}:src/C.cs\",\"start_line\":2,\"end_line\":4}}")
        );
        let gap = run_cmd(t.path(), &["report-gap", "--", "no", "callers", "of", "X"]);
        assert_eq!((gap.text.as_str(), gap.idable), ("\"report_gap\" {\"what\":\"no callers of X\"}", false));
        restore(saved);
    }

    // Checks usage errors, paths outside the work tree, and a working directory outside any work tree.
    #[test]
    fn test_structure_command_errors() {
        let _g = env_lock();
        let fake = FakeIndexer::start(echoing);
        let (t, _name, saved) = setup(&fake);
        let run = |args: &[&str]| run_cmd(t.path(), args).text;
        assert_eq!(run(&["overview", "x"]), "error: usage: overview [--repo DIR]");
        assert!(run(&["overview", "--zz"]).contains("unknown flag --zz"));
        assert!(run(&["find-symbol"]).starts_with("error: usage: find-symbol NAME"));
        assert!(run(&["find-symbol", "a", "--zz"]).contains("unknown flag --zz; known:"));
        assert!(run(&["find-symbol", "a", "-m", "x"]).contains("--limit needs a number"));
        assert!(run(&["changed-since", "a", "b"]).starts_with("error: usage: changed-since"));
        assert!(run(&["git-log", "a", "b"]).starts_with("error: usage: git-log"));
        assert!(run(&["git-log", "--zz"]).contains("unknown flag"));
        assert_eq!(run(&["git-log", "/etc/hosts"]), "error: /etc/hosts: not inside a git work tree");
        assert!(run(&["git-blame"]).starts_with("error: usage: git-blame PATH"));
        assert!(run(&["git-blame", "a", "--zz"]).contains("unknown flag"));
        assert!(run(&["git-blame", "a", "--start", "x"]).contains("--start needs a number"));
        assert!(run(&["git-blame", "a", "--end", "x"]).contains("--end needs a number"));
        assert_eq!(run(&["git-blame", "/etc/hosts"]), "error: /etc/hosts: not inside a git work tree");
        assert!(run(&["git-log", "-n", "x"]).contains("--n needs a number"));
        assert_eq!(run(&["report-gap", " "]), "error: usage: report-gap TEXT [--repo DIR]");
        assert!(run(&["report-gap", "-x"]).contains("unknown flag -x"));
        assert!(run(&["changed-since", "--zz"]).contains("unknown flag --zz"));
        let loose = TempDir::new();
        let none = "error: not inside a git work tree, and no git repositories below this folder";
        assert_eq!(run_cmd(loose.path(), &["overview"]).text, none);
        assert_eq!(
            run_cmd(loose.path(), &["report-gap", "x"]).text,
            "recorded in zazzle's activity log (the indexer's own log is unavailable)"
        );
        assert_eq!(run(&["find-symbol", "a", "--repo", "/"]), "error: /: not inside a git work tree");
        std::env::set_var("ZAZZLE_INDEXER", t.path().join("missing"));
        let down = run(&["overview"]);
        assert!(
            down.starts_with("error: C# indexer unavailable: zazzle-csharp-indexer --version: ")
                && down.ends_with("find, read, ls, outline and refs work without it."),
            "{down}"
        );
        restore(saved);
    }

    // A daemon that answers each call with its tool, the roots it was given and its arguments, then one `root:F.cs:1 C` line per root.
    fn rooted(hello: &Value, req: &Value) -> (Value, Option<Value>) {
        let names: Vec<String> = hello["roots"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|r| r.as_str()?.split('=').next().map(str::to_string))
            .collect();
        let lines: Vec<String> = names.iter().map(|n| format!("{n}:F.cs:1 C")).collect();
        let text = format!(
            "{} {} {}\n{}",
            req.pointer("/params/name").unwrap_or(&Value::Null),
            names.join(","),
            req.pointer("/params/arguments").unwrap_or(&Value::Null),
            lines.join("\n")
        );
        (
            json!({"ok": true}),
            Some(json!({"jsonrpc": "2.0", "id": 1, "result": {"content": [{"type": "text", "text": text}]}})),
        )
    }

    // Checks a folder holding several repositories: overview lists them, find-symbol and changed-since search them all, --repo and paths pick one, and git-log asks which.
    #[test]
    fn test_folder_of_repos() {
        let _g = env_lock();
        let fake = FakeIndexer::start(rooted);
        let (_t, _name, saved) = setup(&fake);
        let top = TempDir::new();
        for d in
            ["a/.git", "group/b/.git", "x/y/z/w/.git", ".hidden/c/.git", "node_modules/d/.git", "a/sub/.git"]
        {
            std::fs::create_dir_all(top.path().join(d)).expect("mkdir");
        }
        top.write("a/src/A.cs", b"class A {}\n");
        top.write("notes.txt", b"x\n");
        let run = |args: &[&str]| run_cmd(top.path(), args).text;
        assert_eq!(
            run(&["overview"]),
            "2 git repositories below this folder (not itself a work tree):\n  a\n  group/b\ngive --repo DIR for one's overview; find-symbol and changed-since search the first 24 together."
        );
        assert_eq!(run(&["overview", "--repo", "a/src"]), "\"overview\" a {}\na/F.cs:1 C");
        assert_eq!(
            run(&["find-symbol", "S"]),
            "\"find_symbol\" a,group/b {\"name\":\"S\"}\na/F.cs:1 C\ngroup/b/F.cs:1 C"
        );
        assert_eq!(
            run(&["find-symbol", "S", "--repo", "group/b"]),
            "\"find_symbol\" b {\"name\":\"S\"}\ngroup/b/F.cs:1 C"
        );
        assert!(run(&["changed-since"]).starts_with("\"changed_since\" a,group/b {}"));
        assert_eq!(
            run(&["git-log"]),
            "error: several git repositories below this folder: give a path or --repo (a, group/b)"
        );
        assert!(run(&["git-log", "--repo", "a"]).starts_with("\"git_log\" a {}"));
        assert!(run(&["git-log", "a/src"]).starts_with("\"git_log\" a {\"path\":\"a:src\"}"));
        assert!(run(&["git-blame", "group/b/F.cs"]).starts_with("\"git_blame\" b {\"path\":\"b:F.cs\"}"));
        assert!(run(&["report-gap", "x"]).starts_with("\"report_gap\" a {"));
        let one = TempDir::new();
        std::fs::create_dir_all(one.path().join("only/.git")).expect("mkdir");
        assert!(run_cmd(one.path(), &["overview"]).text.starts_with("\"overview\" only {}"));
        assert!(run_cmd(one.path(), &["git-log"]).text.starts_with("\"git_log\" only {}"));
        let many = TempDir::new();
        for n in 0..26 {
            std::fs::create_dir_all(many.path().join(format!("r{n:02}/.git"))).expect("mkdir");
        }
        let found = run_cmd(many.path(), &["find-symbol", "S"]).text;
        assert!(
            found.ends_with(
                "\n[searched 24 of 26 git repositories below this folder; give --repo for another]"
            ),
            "{found}"
        );
        restore(saved);
    }

    // Answers every call with no text, as git_log does when nothing matches.
    fn silent(_hello: &Value, _req: &Value) -> (Value, Option<Value>) {
        (
            json!({"ok": true}),
            Some(json!({"jsonrpc": "2.0", "id": 1, "result": {"content": [{"type": "text", "text": ""}]}})),
        )
    }

    // Checks an empty git-log answer says why it is empty instead of answering with nothing.
    #[test]
    fn test_git_log_empty() {
        let _g = env_lock();
        let fake = FakeIndexer::start(silent);
        let (t, _name, saved) = setup(&fake);
        let run = |args: &[&str]| run_cmd(t.path(), args).text;
        assert_eq!(run(&["git-log"]), "no commits");
        assert_eq!(run(&["git-log", "src"]), "no commits touch src");
        assert_eq!(run(&["git-log", "--grep", "fix"]), "no commits whose message matches 'fix'");
        restore(saved);
    }
}
