// Delivers rule, import and skill text to a session once: get-rules and the instructions appended to reads and edits share one ledger keyed by canonical file, line range and content, so neither route repeats what the other sent.
use std::path::{Path, PathBuf};
use zazzle_agent_rules_list::{body, list_rules, purpose, show, Found};

// The MCP tool for get-rules.
pub(crate) const TOOLS: &[crate::mcp::Tool] = &[crate::mcp::Tool {
    name: "zazzle_get_rules",
    description: "The rule, import and skill text that applies to one or more paths, minus what this session was already sent (by get-rules or after reads); a new purpose adds only its own sections.",
    schema,
    map: args,
}];

// Schema for zazzle_get_rules.
fn schema() -> serde_json::Value {
    crate::mcp::object(
        serde_json::json!({
            "paths": {"type": "array", "items": {"type": "string"}, "description": "Files or folders the work touches"},
            "purpose": {"type": "array", "items": {"type": "string"}, "description": "Purposes whose marked sections to add, e.g. review"},
            "name_only": {"type": "boolean"}, "skill_bodies": {"type": "boolean"},
            "again": {"type": "boolean", "description": "Send everything, even what this session has"}
        }),
        &["paths"],
    )
}

// Turns zazzle_get_rules arguments into a command line.
fn args(v: &serde_json::Value) -> Result<Vec<String>, String> {
    let mut a = vec!["get-rules".to_string()];
    a.extend(crate::mcp::get_list(v, "paths")?);
    for p in crate::mcp::get_list(v, "purpose")? {
        a.extend(["--purpose".to_string(), p]);
    }
    for (k, f) in [("name_only", "--name-only"), ("skill_bodies", "--skill-bodies"), ("again", "--again")] {
        crate::mcp::push_flag(&mut a, v, k, f)?;
    }
    Ok(a)
}

pub const USAGE: &str = "usage: get-rules PATH.. [--purpose NAME].. [--name-only] [--skill-bodies] [--again]";

// What a session has been sent: a way to ask and a way to record, so tests and the daemon share the logic.
pub struct Ledger<'a> {
    pub has: &'a dyn Fn(&str) -> bool,
    pub mark: &'a dyn Fn(&str),
}

// The key of a whole file's current content.
pub fn file_key(canon: &Path, text: &str) -> String {
    format!("rule:{}:{}", canon.display(), &blake3::hash(text.as_bytes()).to_hex()[..12])
}

// The key of one line range of a file's current content.
pub fn range_key(canon: &Path, text: &str, (a, b): (usize, usize)) -> String {
    format!("{}:{a}-{b}", file_key(canon, text))
}

// Returns a file's canonical path, so a symlink or import alias of a file shares its key.
pub fn canonical(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

// Returns the part of a found file the recipient has not been sent (the whole file, or its unsent line ranges) and records it as sent; None when nothing is new.
pub fn fresh(f: &Found, l: &Ledger) -> Option<Found> {
    let text = f.text.as_deref()?;
    let canon = canonical(&f.path);
    let whole = file_key(&canon, text);
    if (l.has)(&whole) {
        return None;
    }
    if f.lines.is_empty() {
        (l.mark)(&whole);
        return Some(Found { kind: f.kind, path: f.path.clone(), lines: Vec::new(), text: f.text.clone() });
    }
    let lines: Vec<(usize, usize)> =
        f.lines.iter().copied().filter(|r| !(l.has)(&range_key(&canon, text, *r))).collect();
    lines.iter().for_each(|r| (l.mark)(&range_key(&canon, text, *r)));
    (!lines.is_empty()).then(|| Found { kind: f.kind, path: f.path.clone(), lines, text: f.text.clone() })
}

// Formats the found files the recipient has not been sent, records them, and names the ones left out; without a ledger every file is formatted.
pub fn deliver(found: &[Found], skill_bodies: bool, ledger: Option<&Ledger>) -> (String, Vec<PathBuf>) {
    let (mut out, mut skipped) = (String::new(), Vec::new());
    for f in found {
        match ledger {
            Some(l) if f.text.is_some() => match fresh(f, l) {
                Some(part) => out.push_str(&body(&part, skill_bodies)),
                None => skipped.push(f.path.clone()),
            },
            _ => out.push_str(&body(f, skill_bodies)),
        }
    }
    (out, skipped)
}

// The parsed get-rules command line.
struct Args {
    paths: Vec<String>,
    wanted: Vec<String>,
    name_only: bool,
    skill_bodies: bool,
    again: bool,
}

// Parses get-rules arguments: several paths, repeatable purposes and the flags.
fn parse(argv: &[String]) -> Result<Args, String> {
    let mut a =
        Args { paths: Vec::new(), wanted: Vec::new(), name_only: false, skill_bodies: false, again: false };
    let mut it = argv.iter();
    while let Some(x) = it.next() {
        match x.as_str() {
            "--purpose" => {
                let v = it.next().ok_or("--purpose needs a NAME")?;
                if !purpose::valid(v) {
                    return Err(format!("not a purpose name: `{v}`"));
                }
                a.wanted.push(v.clone());
            }
            "--name-only" => a.name_only = true,
            "--skill-bodies" => a.skill_bodies = true,
            "--again" => a.again = true,
            s if s.starts_with('-') => return Err(format!("unknown option {s}; {USAGE}")),
            s => a.paths.push(s.to_string()),
        }
    }
    if a.paths.is_empty() {
        return Err(USAGE.into());
    }
    Ok(a)
}

// Lists the union of the files that apply to several paths, each file and range once, in the order first met.
fn union(paths: &[PathBuf], wanted: &[String], home: Option<&Path>) -> Result<Vec<Found>, String> {
    let mut out: Vec<Found> = Vec::new();
    for p in paths {
        for f in list_rules(p, home, wanted, Path::new("/"))? {
            let c = canonical(&f.path);
            match out.iter_mut().find(|g| canonical(&g.path) == c) {
                Some(g) if g.lines.is_empty() => {}
                Some(g) if f.lines.is_empty() => g.lines.clear(),
                Some(g) => {
                    for r in f.lines {
                        if !g.lines.contains(&r) {
                            g.lines.push(r);
                        }
                    }
                }
                None => out.push(f),
            }
        }
    }
    Ok(out)
}

// Returns the purposes a get-rules command line asks for, which become the session's active purposes; None when it does not parse or names none.
pub fn purposes(argv: &[String]) -> Option<Vec<String>> {
    parse(argv).ok().map(|a| a.wanted).filter(|w| !w.is_empty())
}

// Runs get-rules for a session: the rules, imports and skills that apply to the paths and that the session was not yet sent, then a line naming what it already has.
pub fn get_rules(
    argv: &[String],
    cwd: &Path,
    home: Option<&Path>,
    ledger: Option<&Ledger>,
) -> Result<String, String> {
    let a = parse(argv)?;
    let paths: Vec<PathBuf> = a.paths.iter().map(|p| cwd.join(p)).collect();
    let found = union(&paths, &a.wanted, home)?;
    if a.name_only {
        return Ok(found.iter().map(show).collect::<String>().trim_end().to_string());
    }
    let (text, skipped) = deliver(&found, a.skill_bodies, if a.again { None } else { ledger });
    let mut out = text.trim_end().to_string();
    if !skipped.is_empty() {
        let names: Vec<String> =
            skipped.iter().map(|p| p.strip_prefix(cwd).unwrap_or(p).display().to_string()).collect();
        let sep = if out.is_empty() { "" } else { "\n\n" };
        out.push_str(&format!(
            "{sep}-- already delivered this session, unchanged, not repeated: {} (--again resends)",
            names.join(" ")
        ));
    }
    if out.is_empty() {
        out = "no rule, import or skill files apply".into();
    }
    Ok(out)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::testutil::TempDir;
    use std::cell::RefCell;

    // A ledger backed by a list, as a session would be.
    fn ledger(seen: &RefCell<Vec<String>>) -> (impl Fn(&str) -> bool + '_, impl Fn(&str) + '_) {
        (
            move |k: &str| seen.borrow().iter().any(|s| s == k),
            move |k: &str| seen.borrow_mut().push(k.to_string()),
        )
    }

    // Builds a repository with a root rule file, a sub-folder rule and a purpose-marked file.
    fn repo() -> (TempDir, PathBuf) {
        let t = TempDir::new();
        let root = t.path().canonicalize().expect("root");
        std::fs::create_dir_all(root.join(".git")).expect("git");
        t.write("AGENTS.md", b"root rule: never change published contracts\n");
        t.write("api/AGENTS.md", b"api rule\n");
        t.write("api/a.cs", b"class A {}\n");
        t.write("web/b.ts", b"b\n");
        t.write(
            "web/AGENTS.md",
            b"<!-- #zazzle-purpose-begin:build -->\ncommon web rule\n<!-- #zazzle-purpose-end:build -->\n<!-- #zazzle-purpose-begin:review -->\nreview only\n<!-- #zazzle-purpose-end:review -->\n",
        );
        (t, root)
    }

    // Checks repeated lookups send each rule once, a second path adds only its own rules, and --again resends.
    #[test]
    fn test_get_rules_once_per_session() {
        let (_t, root) = repo();
        let seen = RefCell::new(Vec::new());
        let (has, mark) = ledger(&seen);
        let l = Ledger { has: &has, mark: &mark };
        let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let first = get_rules(&args(&["api/a.cs"]), &root, None, Some(&l)).expect("first");
        assert!(first.contains("root rule") && first.contains("api rule"), "{first}");
        let again = get_rules(&args(&["api/a.cs"]), &root, None, Some(&l)).expect("again");
        assert_eq!(again, "-- already delivered this session, unchanged, not repeated: AGENTS.md api/AGENTS.md (--again resends)");
        let both = get_rules(&args(&["api/a.cs", "web/b.ts"]), &root, None, Some(&l)).expect("both");
        assert!(
            both.contains("common web rule")
                && !both.contains("root rule")
                && both.matches("==> rule").count() == 1,
            "{both}"
        );
        let forced = get_rules(&args(&["api/a.cs", "--again"]), &root, None, Some(&l)).expect("forced");
        assert!(forced.contains("root rule") && forced.contains("api rule"), "{forced}");
        let names = get_rules(&args(&["api/a.cs", "--name-only"]), &root, None, Some(&l)).expect("names");
        assert!(names.lines().all(|n| n.starts_with("rule ")), "{names}");
        assert!(get_rules(&args(&[]), &root, None, None).unwrap_err().contains("usage"));
        assert!(get_rules(&args(&["x", "--purpose", "-bad"]), &root, None, None)
            .unwrap_err()
            .contains("not a purpose"));
        assert!(get_rules(&args(&["x", "--zz"]), &root, None, None).unwrap_err().contains("unknown option"));
    }

    // Checks a changed file is sent again, a new purpose adds only its missing segment, and a read's appendix skips what get-rules sent and the reverse.
    #[test]
    fn test_changes_purposes_and_both_routes() {
        let (t, root) = repo();
        let seen = RefCell::new(Vec::new());
        let (has, mark) = ledger(&seen);
        let l = Ledger { has: &has, mark: &mark };
        let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        get_rules(&args(&["web/b.ts", "--purpose", "build"]), &root, None, Some(&l)).expect("build");
        let review =
            get_rules(&args(&["web/b.ts", "--purpose", "review"]), &root, None, Some(&l)).expect("review");
        assert!(review.contains("review only") && !review.contains("common web rule"), "{review}");
        t.write("AGENTS.md", b"root rule changed\n");
        let changed = get_rules(&args(&["web/b.ts"]), &root, None, Some(&l)).expect("changed");
        assert!(changed.contains("root rule changed"), "{changed}");
        let (auto, _) = (crate::nested::for_paths(&root, &[root.join("web/b.ts")], &[], &l), ());
        assert!(!auto.contains("common web rule"), "the appendix skips what get-rules sent: {auto}");
        let seen2 = RefCell::new(Vec::new());
        let (has2, mark2) = ledger(&seen2);
        let l2 = Ledger { has: &has2, mark: &mark2 };
        let auto2 = crate::nested::for_paths(&root, &[root.join("api/a.cs")], &[], &l2);
        assert!(auto2.contains("api rule") && !auto2.contains("root rule"), "{auto2}");
        let after = get_rules(&args(&["api/a.cs"]), &root, None, Some(&l2)).expect("after");
        assert!(
            after.contains("root rule") && !after.contains("api rule\n"),
            "get-rules skips what the appendix sent: {after}"
        );
    }

    // Checks an import alias of an already listed file and a symlinked rule file are not sent twice.
    #[test]
    fn test_aliases_once() {
        let (t, root) = repo();
        t.write("api/CLAUDE.md", b"@AGENTS.md\n");
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("AGENTS.md"), root.join("api/.claude-link.md")).expect("link");
        let seen = RefCell::new(Vec::new());
        let (has, mark) = ledger(&seen);
        let l = Ledger { has: &has, mark: &mark };
        let out = get_rules(&["api/a.cs".to_string()], &root, None, Some(&l)).expect("rules");
        assert_eq!(out.matches("api rule").count(), 1, "{out}");
        let (plain, skipped) = deliver(&union(&[root.join("api/a.cs")], &[], None).expect("u"), false, None);
        assert!(plain.contains("api rule") && skipped.is_empty());
        let missing = Found {
            kind: zazzle_agent_rules_list::Kind::Rule,
            path: root.join("gone.md"),
            lines: vec![],
            text: None,
        };
        assert!(deliver(&[missing], false, Some(&l)).0.contains("error"));
    }

    // Checks the daemon route: a read's appendix and get-rules share the session ledger, `agent get-rules` is the same command, a reset sends again, and MCP arguments map to it.
    #[test]
    fn test_daemon_route() {
        let (_t, root) = repo();
        let e = crate::engine::Engine::new();
        let run = |s: &str, a: &[&str]| {
            let o = crate::testutil::run_cmd_with(&e, &root, s, a);
            o.text + &o.notes
        };
        assert!(run("s1", &["read", "api/a.cs"]).contains("api rule"));
        let rules = run("s1", &["agent", "get-rules", "api/a.cs"]);
        assert!(rules.contains("root rule") && rules.contains("not repeated: api/AGENTS.md"), "{rules}");
        assert!(!run("s1", &["get-rules", "api/a.cs"]).contains("==> rule"));
        run("", &["forget-sessions", "s1"]);
        assert!(run("s1", &["get-rules", "api/a.cs"]).contains("api rule"), "a reset sends again");
        assert!(run("", &["get-rules", "api/a.cs"]).contains("api rule"), "no session, no ledger");
        assert!(run("s1", &["agent", "get-rules", "--help"]).contains("get-rules"));
        assert!(run("s1", &["agent", "--version"]).starts_with("zazzle-agent "));
        assert!(run("s1", &["agent", "--help"]).contains("get-rules"));
        assert!(run("s1", &["agent", "nope"]).contains("unknown command"));
        let v = serde_json::json!({"paths": ["a", "b"], "purpose": ["review"], "again": true});
        assert_eq!(args(&v).expect("args"), ["get-rules", "a", "b", "--purpose", "review", "--again"]);
        assert_eq!(schema()["required"][0], "paths");
    }

    // Checks the union merges ranges and whole files of one file, and a range already sent is not sent again.
    #[test]
    fn test_union_and_ranges() {
        let f = |lines: Vec<(usize, usize)>| Found {
            kind: zazzle_agent_rules_list::Kind::Rule,
            path: PathBuf::from("/nonexistent/R.md"),
            lines,
            text: Some("a\nb\nc\nd\n".into()),
        };
        let seen = RefCell::new(Vec::new());
        let (has, mark) = ledger(&seen);
        let l = Ledger { has: &has, mark: &mark };
        let (first, _) = deliver(&[f(vec![(1, 2)])], false, Some(&l));
        assert!(first.contains(":1-2 <=="), "{first}");
        let (again, skipped) = deliver(&[f(vec![(1, 2)])], false, Some(&l));
        assert!(again.is_empty() && skipped.len() == 1);
        let (more, _) = deliver(&[f(vec![(1, 2), (3, 4)])], false, Some(&l));
        assert!(more.contains(":3-4 <==") && !more.contains(":1-2"), "{more}");
        let (_t, root) = repo();
        let u = union(&[root.join("web/b.ts"), root.join("web")], &["review".into()], None).expect("u");
        assert_eq!(u.iter().filter(|g| g.path.ends_with("web/AGENTS.md")).count(), 1);
        let empty = TempDir::new();
        let none = get_rules(&["x".to_string()], &empty.path().canonicalize().expect("c"), None, None);
        assert!(none.map_or(true, |o| o.contains("apply") || o.contains("==>")));
        let w = union(&[root.join("web/b.ts")], &["build".into(), "review".into()], None).expect("w");
        assert!(w.iter().any(|g| g.lines.len() == 2), "{w:?}");
    }
}
