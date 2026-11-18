// The diff command: per-file stats first, then hunks labelled with their enclosing symbol, ignored and generated files skipped, under a token budget with a "read next" footer.
use super::{Ctx, Output};
use crate::args::{self, flag, opt, Flag};
use crate::tokens;
use std::path::Path;
use std::process::Command;

const SPEC: &[Flag] = &[flag("names", '\0'), opt("budget", 'b'), opt("context", 'C')];
const USAGE: &str = "usage: diff [BASE[..HEAD|...HEAD]] [PATH..] [--names] [--context N] [--budget T]";
const DEFAULT_BUDGET: usize = 3000;
// Files diff leaves out unless named: lock files and generated code.
const GENERATED: &[&str] = &[
    "Cargo.lock",
    "package-lock.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "poetry.lock",
    "Gemfile.lock",
    "composer.lock",
    "go.sum",
    "*.min.js",
    "*.min.css",
    "*.map",
    "*.g.cs",
    "*.designer.cs",
    "*.Designer.cs",
    "*.pb.go",
    "*_pb2.py",
    "*.snap",
];

// One changed file: its path and how many lines it adds and removes (None for binary).
struct Stat {
    path: String,
    added: Option<usize>,
    removed: Option<usize>,
}

// Runs git in the working directory and returns its output, or its first error line.
fn git(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(err.lines().next().unwrap_or("git failed").trim_start_matches("fatal: ").to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

// True when an argument names a revision or a range rather than a path.
fn is_range(cwd: &Path, arg: &str) -> bool {
    let first = arg.split("..").next().unwrap_or(arg).trim_end_matches('.');
    !cwd.join(arg).exists()
        && git(
            cwd,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{}^{{commit}}", if first.is_empty() { "HEAD" } else { first }),
            ],
        )
        .is_ok()
}

// True for a file diff skips: one `.ignore` matches, or a lock file or generated code.
fn skipped(root: &Path, path: &str, ignore: &ignore::gitignore::Gitignore) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    let generated = GENERATED.iter().any(|g| match g.strip_prefix('*') {
        Some(ext) => name.ends_with(ext),
        None => name == *g,
    });
    generated || ignore.matched_path_or_any_parents(root.join(path), false).is_ignore()
}

// Reads `git diff --numstat` lines.
fn stats(text: &str) -> Vec<Stat> {
    text.lines()
        .filter_map(|l| {
            let mut f = l.splitn(3, '\t');
            let (a, r, p) = (f.next()?, f.next()?, f.next()?);
            let path = p.rsplit_once(" => ").map_or(p, |(_, new)| new).trim_end_matches('}').to_string();
            Some(Stat { path, added: a.parse().ok(), removed: r.parse().ok() })
        })
        .collect()
}

// Formats a file's stat line.
fn stat_line(s: &Stat) -> String {
    match (s.added, s.removed) {
        (Some(a), Some(r)) => format!("{} +{a} -{r}", s.path),
        _ => format!("{} binary", s.path),
    }
}

// Returns the outline of a file as the diff's new side has it: the working tree, or HEAD of a range.
fn symbols(ctx: &Ctx, path: &str, head: Option<&str>) -> Vec<crate::outline::Symbol> {
    let data = match head {
        Some(rev) => git(&ctx.cwd, &["show", &format!("{rev}:./{path}")]).map(String::into_bytes).ok(),
        None => std::fs::read(ctx.cwd.join(path)).ok(),
    };
    let Some(data) = data else { return Vec::new() };
    let meta = crate::meta::Meta::compute(&data);
    crate::outline::outline(Path::new(path), &data, &meta.starts)
}

// Rewrites a file's hunk headers to name the symbol that encloses each hunk's first new line.
fn label_hunks(patch: &str, syms: &[crate::outline::Symbol]) -> String {
    let mut out = String::new();
    for line in patch.lines() {
        if let Some(rest) = line.strip_prefix("@@ ") {
            let (range, _) = rest.split_once(" @@").unwrap_or((rest, ""));
            let start = range
                .split_whitespace()
                .find_map(|r| r.strip_prefix('+'))
                .and_then(|r| r.split(',').next()?.parse::<usize>().ok())
                .unwrap_or(0);
            let label = crate::outline::enclosing(syms, start + 3)
                .or_else(|| crate::outline::enclosing(syms, start))
                .map_or(String::new(), |s| format!(" {} {}", s.kind, s.name));
            out.push_str(&format!("@@ {range} @@{label}\n"));
        } else if !line.starts_with("diff --git")
            && !line.starts_with("index ")
            && !line.starts_with("--- ")
            && !line.starts_with("+++ ")
        {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

// Runs the diff command.
pub fn run(ctx: &Ctx, argv: &[String]) -> Result<Output, String> {
    let p = args::parse(SPEC, argv).map_err(|e| format!("{e}; {USAGE}"))?;
    let root = git(&ctx.cwd, &["rev-parse", "--show-toplevel"])
        .map_err(|e| format!("{e}; diff needs a git work tree"))?;
    let root = Path::new(root.trim());
    let (range, paths): (Vec<String>, Vec<String>) = match p.positional.split_first() {
        Some((r, rest)) if is_range(&ctx.cwd, r) => (vec![r.clone()], rest.to_vec()),
        _ => (vec!["HEAD".into()], p.positional.clone()),
    };
    let head = range[0].split_once("..").map(|(_, h)| h.trim_start_matches('.')).filter(|h| !h.is_empty());
    let shown_range =
        if p.positional.first() == range.first() { range[0].clone() } else { "HEAD (working tree)".into() };
    let mut base_args = vec!["-c", "core.quotepath=off", "diff", "--no-color", "--no-ext-diff", "--relative"];
    base_args.extend(range.iter().map(String::as_str));
    let named: Vec<&str> = paths.iter().map(String::as_str).collect();
    let with = |extra: &[&str], files: &[&str]| {
        let mut a = base_args.clone();
        a.extend_from_slice(extra);
        a.push("--");
        a.extend_from_slice(files);
        git(&ctx.cwd, &a)
    };
    let (ignore, _) = ignore::gitignore::Gitignore::new(root.join(".ignore"));
    let all = stats(&with(&["--numstat"], &named)?);
    let (shown, left_out): (Vec<Stat>, Vec<Stat>) =
        all.into_iter().partition(|s| !named.is_empty() || !skipped(root, &s.path, &ignore));
    if shown.is_empty() && left_out.is_empty() {
        return Ok(Output::plain(format!("no changes ({shown_range})")));
    }
    let (plus, minus) =
        shown.iter().fold((0, 0), |(a, r), s| (a + s.added.unwrap_or(0), r + s.removed.unwrap_or(0)));
    let mut out = format!("diff {shown_range}: {} files, +{plus} -{minus}\n", shown.len());
    shown.iter().for_each(|s| out.push_str(&format!("{}\n", stat_line(s))));
    if !left_out.is_empty() {
        let names: Vec<&str> = left_out.iter().map(|s| s.path.as_str()).collect();
        out.push_str(&format!("skipped (generated or .ignore'd; name one to see it): {}\n", names.join(" ")));
    }
    if p.has("names") {
        return Ok(Output::ok(out.trim_end().to_string()));
    }
    let budget = p.number("budget")?.unwrap_or(DEFAULT_BUDGET);
    let context = format!("-U{}", p.number("context")?.unwrap_or(3));
    let header = tokens::estimate(&out);
    let mut spent = header;
    let mut unread = Vec::new();
    for s in shown.iter().filter(|s| s.added.is_some()) {
        let patch = label_hunks(&with(&[&context], &[&s.path])?, &symbols(ctx, &s.path, head));
        let cost = tokens::estimate(&patch);
        if spent + cost > budget && spent > header {
            unread.push(s.path.as_str());
            continue;
        }
        out.push_str(&format!("\n{}\n{patch}", s.path));
        spent += cost;
    }
    if !unread.is_empty() {
        let cmd = if p.positional.first() == range.first() {
            format!("zazzle diff {} ", range[0])
        } else {
            "zazzle diff ".into()
        };
        out.push_str(&format!(
            "\n-- read next ({} files over the {budget}-token budget): {cmd}{}",
            unread.len(),
            unread.join(" ")
        ));
    }
    Ok(Output::ok(out.trim_end().to_string()))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use crate::testutil::{git_commit, run_cmd, TempDir};

    // Builds a repository with a committed Rust file and a lock file, then changes both.
    fn changed() -> TempDir {
        let t = TempDir::new();
        t.write("src/a.rs", b"fn one() {\n    1\n}\n\nfn two() {\n    2\n}\n");
        t.write("Cargo.lock", b"a\n");
        t.write("notes/n.md", b"x\n");
        t.write(".ignore", b"notes/\n");
        git_commit(t.path(), "one");
        t.write("src/a.rs", b"fn one() {\n    1\n}\n\nfn two() {\n    22\n}\n");
        t.write("Cargo.lock", b"b\n");
        t.write("notes/n.md", b"y\n");
        t
    }

    // Checks the stats come first, hunks name their symbol, lock and .ignore'd files are skipped, and --names stops after the stats.
    #[test]
    fn test_diff_working_tree() {
        let t = changed();
        let o = run_cmd(t.path(), &["diff"]).text;
        assert!(o.starts_with("diff HEAD (working tree): 1 files, +1 -1\nsrc/a.rs +1 -1\nskipped (generated or .ignore'd; name one to see it): Cargo.lock notes/n.md\n"), "{o}");
        assert!(
            o.contains("\nsrc/a.rs\n@@ -3,5 +3,5 @@ fn two\n }\n \n fn two() {\n-    2\n+    22\n"),
            "{o}"
        );
        let names = run_cmd(t.path(), &["diff", "--names"]).text;
        assert!(!names.contains("@@") && names.contains("src/a.rs +1 -1"), "{names}");
        let lock = run_cmd(t.path(), &["diff", "Cargo.lock"]).text;
        assert!(lock.starts_with("diff HEAD (working tree): 1 files, +1 -1\nCargo.lock +1 -1\n"), "{lock}");
    }

    // Checks a commit range, the budget's read-next footer, a clean tree, and the errors.
    #[test]
    fn test_diff_range_budget_and_errors() {
        let t = changed();
        git_commit(t.path(), "two");
        let o = run_cmd(t.path(), &["diff", "HEAD~1..HEAD"]).text;
        assert!(o.starts_with("diff HEAD~1..HEAD: 1 files") && o.contains("@@ fn two"), "{o}");
        t.write("src/b.rs", b"fn b() {}\n");
        t.write("src/a.rs", b"fn one() {}\n");
        git_commit(t.path(), "three");
        let small = run_cmd(t.path(), &["diff", "HEAD~1", "--budget", "30"]).text;
        assert!(
            small.contains("-- read next (1 files over the 30-token budget): zazzle diff HEAD~1 src/b.rs"),
            "{small}"
        );
        let dots = run_cmd(t.path(), &["diff", "..HEAD"]).text;
        assert!(dots.starts_with("no changes") || dots.starts_with("diff ..HEAD"), "{dots}");
        std::fs::remove_file(t.path().join("src/b.rs")).expect("rm");
        t.write("src/a.rs", &b"fn c() {}\n".repeat(40));
        let gone = run_cmd(t.path(), &["diff", "--budget", "30"]).text;
        assert!(
            gone.contains("src/b.rs +0 -1")
                && gone.contains("-- read next (1 files over the 30-token budget): zazzle diff "),
            "{gone}"
        );
        git_commit(t.path(), "four");
        assert_eq!(run_cmd(t.path(), &["diff"]).text, "no changes (HEAD (working tree))");
        assert!(run_cmd(t.path(), &["diff", "--zz"]).text.contains("unknown flag"));
        let none = TempDir::new();
        assert!(run_cmd(none.path(), &["diff"]).text.contains("diff needs a git work tree"));
    }
}
