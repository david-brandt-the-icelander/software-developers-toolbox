// The structure commands answered by zazzle-csharp-indexer's daemon: overview, find-symbol, changed-since, git-log, git-blame and report-gap.
use super::{Ctx, Output};
use crate::args::{self, flag, opt, Flag};
use crate::indexer::{localize, work_tree, Indexer};
use crate::util::resolve;
use serde_json::{json, Map, Value};
use std::path::PathBuf;

const SYMBOL_SPEC: &[Flag] = &[flag("exact", 'x'), opt("kind", 'k'), opt("limit", 'm'), opt("offset", 'o')];
const LOG_SPEC: &[Flag] = &[opt("n", 'n'), opt("grep", '\0')];
const BLAME_SPEC: &[Flag] = &[opt("start", '\0'), opt("end", '\0')];
const OVERVIEW_USAGE: &str = "usage: overview";
const SYMBOL_USAGE: &str = "usage: find-symbol NAME [--exact] [--kind K] [--limit N] [--offset N]";
const CHANGED_USAGE: &str = "usage: changed-since [CURSOR]";
const LOG_USAGE: &str = "usage: git-log [PATH] [-n N] [--grep TEXT]";
const BLAME_USAGE: &str = "usage: git-blame PATH [--start N] [--end N]";
const GAP_USAGE: &str = "usage: report-gap TEXT";

// Where a structure command runs: the indexer, the git work tree of the working directory, and that directory.
struct Target {
    indexer: Indexer,
    root: PathBuf,
    cwd: PathBuf,
}

impl Target {
    // Finds the indexer and the work tree that holds the working directory.
    fn locate(ctx: &Ctx) -> Result<Target, String> {
        let root = work_tree(&ctx.cwd).ok_or("not inside a git work tree")?.to_path_buf();
        Ok(Target { indexer: Indexer::locate()?, root, cwd: ctx.cwd.clone() })
    }

    // Turns a path argument into a path relative to the work tree, as the indexer wants it.
    fn relative(&self, arg: &str) -> Result<String, String> {
        let full = resolve(arg, &self.cwd);
        let rel = full.strip_prefix(&self.root).map_err(|_| format!("{arg}: outside the git work tree"))?;
        Ok(rel.to_string_lossy().into_owned())
    }

    // Runs one indexer tool and rewrites its root:path references for the working directory.
    fn ask(&self, tool: &str, args: Map<String, Value>) -> Result<Output, String> {
        let text = self.indexer.ask(&self.root, tool, Value::Object(args))?;
        Ok(Output::ok(localize(&text, &self.root, &self.cwd)))
    }
}

// Puts a numeric flag's value into the tool arguments under a key when it was given.
fn put_number(p: &args::Parsed, flag: &str, key: &str, out: &mut Map<String, Value>) -> Result<(), String> {
    if let Some(n) = p.number(flag)? {
        out.insert(key.into(), json!(n));
    }
    Ok(())
}

// Runs overview: a map of the work tree's languages, directories, entry points and largest files.
pub fn overview(ctx: &Ctx, argv: &[String]) -> Result<Output, String> {
    if !argv.is_empty() {
        return Err(OVERVIEW_USAGE.into());
    }
    Target::locate(ctx)?.ask("overview", Map::new())
}

// Runs find-symbol: declarations by name from the index.
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
    Target::locate(ctx)?.ask("find_symbol", a)
}

// Runs changed-since: files added, modified or deleted since a cursor, and a new cursor.
pub fn changed_since(ctx: &Ctx, argv: &[String]) -> Result<Output, String> {
    let p = args::parse(&[], argv).map_err(|e| format!("{e}; {CHANGED_USAGE}"))?;
    let mut a = Map::new();
    match p.positional.as_slice() {
        [] => {}
        [cursor] => {
            a.insert("cursor".into(), json!(cursor));
        }
        _ => return Err(CHANGED_USAGE.into()),
    }
    Target::locate(ctx)?.ask("changed_since", a)
}

// Runs git-log: recent commits, optionally for one path or with a message filter.
pub fn git_log(ctx: &Ctx, argv: &[String]) -> Result<Output, String> {
    let p = args::parse(LOG_SPEC, argv).map_err(|e| format!("{e}; {LOG_USAGE}"))?;
    let t = Target::locate(ctx)?;
    let mut a = Map::new();
    match p.positional.as_slice() {
        [] => {}
        [path] => {
            a.insert("path".into(), json!(t.relative(path)?));
        }
        _ => return Err(LOG_USAGE.into()),
    }
    put_number(&p, "n", "n", &mut a)?;
    if let Some(g) = p.value("grep") {
        a.insert("grep".into(), json!(g));
    }
    t.ask("git_log", a)
}

// Runs git-blame: who last changed each line of a range of a file.
pub fn git_blame(ctx: &Ctx, argv: &[String]) -> Result<Output, String> {
    let p = args::parse(BLAME_SPEC, argv).map_err(|e| format!("{e}; {BLAME_USAGE}"))?;
    let [path] = p.positional.as_slice() else { return Err(BLAME_USAGE.into()) };
    let t = Target::locate(ctx)?;
    let mut a = Map::new();
    a.insert("path".into(), json!(t.relative(path)?));
    put_number(&p, "start", "start_line", &mut a)?;
    put_number(&p, "end", "end_line", &mut a)?;
    t.ask("git_blame", a)
}

// Runs report-gap: records in the indexer's usage log what the agent needed and could not get.
pub fn report_gap(ctx: &Ctx, argv: &[String]) -> Result<Output, String> {
    let p = args::parse(&[], argv).map_err(|e| format!("{e}; {GAP_USAGE}"))?;
    let what = p.positional.join(" ");
    if what.trim().is_empty() {
        return Err(GAP_USAGE.into());
    }
    let t = Target::locate(ctx)?;
    let text = t.indexer.ask(&t.root, "report_gap", json!({"what": what}))?;
    Ok(Output::plain(text))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use crate::indexer::tests::{echoing, FakeIndexer};
    use crate::testutil::{env_lock, run_cmd, TempDir};
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
            "\"find_symbol\" {\"exact\":true,\"kind\":\"method\",\"limit\":5,\"name\":\"Spin\",\"offset\":2}"
        );
        assert_eq!(run(&["find-symbol", "Spin"]), "\"find_symbol\" {\"name\":\"Spin\"}");
        assert_eq!(run(&["changed-since"]), "\"changed_since\" {}");
        assert_eq!(run(&["changed-since", "e;r=1"]), "\"changed_since\" {\"cursor\":\"e;r=1\"}");
        assert_eq!(run(&["git-log"]), "\"git_log\" {}");
        assert_eq!(
            run(&["git-log", "src", "-n", "3", "--grep", "fix"]),
            format!("\"git_log\" {{\"grep\":\"fix\",\"n\":3,\"path\":\"{name}:src\"}}")
        );
        let sub = run_cmd(&t.path().join("src"), &["git-blame", "C.cs", "--start", "2", "--end", "4"]).text;
        assert_eq!(
            sub,
            format!("\"git_blame\" {{\"end_line\":4,\"path\":\"{name}:src/C.cs\",\"start_line\":2}}")
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
        assert_eq!(run(&["overview", "x"]), "error: usage: overview");
        assert!(run(&["find-symbol"]).starts_with("error: usage: find-symbol NAME"));
        assert!(run(&["find-symbol", "a", "--zz"]).contains("unknown flag --zz; known:"));
        assert!(run(&["find-symbol", "a", "-m", "x"]).contains("--limit needs a number"));
        assert!(run(&["changed-since", "a", "b"]).starts_with("error: usage: changed-since"));
        assert!(run(&["git-log", "a", "b"]).starts_with("error: usage: git-log"));
        assert!(run(&["git-log", "--zz"]).contains("unknown flag"));
        assert_eq!(run(&["git-log", "/etc/hosts"]), "error: /etc/hosts: outside the git work tree");
        assert!(run(&["git-blame"]).starts_with("error: usage: git-blame PATH"));
        assert!(run(&["git-blame", "a", "--zz"]).contains("unknown flag"));
        assert!(run(&["git-blame", "a", "--start", "x"]).contains("--start needs a number"));
        assert!(run(&["git-blame", "a", "--end", "x"]).contains("--end needs a number"));
        assert_eq!(run(&["git-blame", "/etc/hosts"]), "error: /etc/hosts: outside the git work tree");
        assert!(run(&["git-log", "-n", "x"]).contains("--n needs a number"));
        assert_eq!(run(&["report-gap", " "]), "error: usage: report-gap TEXT");
        assert!(run(&["report-gap", "-x"]).contains("unknown flag -x"));
        assert!(run(&["changed-since", "--zz"]).contains("unknown flag --zz"));
        let loose = TempDir::new();
        assert_eq!(run_cmd(loose.path(), &["overview"]).text, "error: not inside a git work tree");
        assert_eq!(run_cmd(loose.path(), &["report-gap", "x"]).text, "error: not inside a git work tree");
        std::env::set_var("ZAZZLE_INDEXER", t.path().join("missing"));
        assert!(run(&["overview"]).starts_with("error: zazzle-csharp-indexer --version: "));
        restore(saved);
    }
}
