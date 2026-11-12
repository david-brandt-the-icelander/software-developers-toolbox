// The ls command: a compact tree with recursive file counts and per-file line counts.
use super::{Ctx, Output};
use crate::args::{self, flag, opt, Flag};
use crate::util::{display, resolve};
use crate::walk::{self, WalkOpts};
use std::collections::BTreeMap;
use std::path::Path;

const SPEC: &[Flag] = &[opt("depth", 'd'), opt("max", 'm'), flag("no-ignore", 'u')];
const USAGE: &str = "usage: ls [DIR] [-d N] [-m N] [-u]";
const DEFAULT_DEPTH: usize = 2;
const DEFAULT_MAX: usize = 150;

// A directory in the tree: subdirectories, files and the recursive file count.
#[derive(Default)]
struct Node {
    dirs: BTreeMap<String, Node>,
    files: Vec<String>,
    count: usize,
}

// Inserts a file path, given as components, into the tree.
fn insert(node: &mut Node, parts: &[&str]) {
    node.count += 1;
    match parts {
        [name] => node.files.push((*name).to_string()),
        [dir, rest @ ..] => insert(node.dirs.entry((*dir).to_string()).or_default(), rest),
        [] => {}
    }
}

// Summarises one file as a line count, "bin" or a size.
fn summary(ctx: &Ctx, path: &Path) -> String {
    match ctx.engine.meta(path) {
        Ok(m) if m.binary => format!("bin {}B", m.size),
        Ok(m) => m.lines().to_string(),
        Err(_) => "?".into(),
    }
}

// Printing state: output text, entry budget and what was dropped.
struct Printer<'a> {
    ctx: &'a Ctx<'a>,
    out: String,
    left: usize,
    dropped: usize,
    depth: usize,
}

impl Printer<'_> {
    // Prints one entry line if the budget allows, else counts it as dropped.
    fn line(&mut self, indent: usize, text: String) -> bool {
        if self.left == 0 {
            self.dropped += 1;
            return false;
        }
        self.left -= 1;
        self.out.push_str(&format!("{}{text}\n", "  ".repeat(indent)));
        true
    }

    // Prints a directory's children down to the depth limit.
    fn tree(&mut self, node: &Node, dir: &Path, level: usize) {
        for (name, child) in &node.dirs {
            let files = if child.count == 1 { "file" } else { "files" };
            if !self.line(level, format!("{name}/ {} {files}", child.count)) {
                continue;
            }
            if level < self.depth {
                self.tree(child, &dir.join(name), level + 1);
            }
        }
        if level > self.depth {
            return;
        }
        for name in &node.files {
            let s = summary(self.ctx, &dir.join(name));
            self.line(level, format!("{name} {s}"));
        }
    }
}

// Runs the ls command.
pub fn run(ctx: &Ctx, argv: &[String]) -> Result<Output, String> {
    let p = args::parse(SPEC, argv).map_err(|e| format!("{e}; {USAGE}"))?;
    if p.positional.len() > 1 {
        return Err(USAGE.into());
    }
    let root = p.positional.first().map_or_else(|| ctx.cwd.clone(), |a| resolve(a, &ctx.cwd));
    let depth = p.number("depth")?.unwrap_or(DEFAULT_DEPTH);
    let max = p.number("max")?.unwrap_or(DEFAULT_MAX).max(1);
    let md = std::fs::metadata(&root)
        .map_err(|e| format!("{}: {}", display(&root, &ctx.cwd), walk::reason(&e)))?;
    if md.is_file() {
        let name = display(&root, &ctx.cwd);
        return Ok(Output::ok(format!("{name} {}", summary(ctx, &root))));
    }
    let files = walk::collect(
        std::slice::from_ref(&root),
        &WalkOpts { no_ignore: p.has("no-ignore"), globs: vec![] },
    )?;
    let mut top = Node::default();
    for f in &files {
        let rel = f.strip_prefix(&root).unwrap_or(f);
        let parts: Vec<String> =
            rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
        insert(&mut top, &parts.iter().map(String::as_str).collect::<Vec<_>>());
    }
    let word = if top.count == 1 { "file" } else { "files" };
    let mut pr = Printer {
        ctx,
        out: format!("{}/ {} {word}\n", display(&root, &ctx.cwd), top.count),
        left: max,
        dropped: 0,
        depth,
    };
    pr.tree(&top, &root, 1);
    if pr.dropped > 0 {
        pr.out.push_str(&format!("… {} more entries (raise -m, or list a subdirectory)\n", pr.dropped));
    }
    pr.out.pop();
    Ok(Output::ok(pr.out))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use crate::testutil::{run_cmd, TempDir};

    // Builds a small project tree.
    fn tree() -> TempDir {
        let t = TempDir::new();
        t.write("src/engine/mod.rs", b"a\nb\nc\n");
        t.write("src/engine/deep/x.rs", b"x\n");
        t.write("src/main.rs", b"fn main() {}\n");
        t.write("img.bin", b"\0\0\0");
        t.write("README.md", b"# hi\n");
        t
    }

    // Checks the default tree output with counts and line numbers.
    #[test]
    fn test_ls_default_tree() {
        let t = tree();
        let o = run_cmd(t.path(), &["ls"]);
        let want = format!(
            "{}/ 5 files\n  src/ 3 files\n    engine/ 2 files\n    main.rs 1\n  README.md 1\n  img.bin bin 3B",
            "."
        );
        assert_eq!(o.text, want);
        assert!(o.idable);
    }

    // Checks the depth limit hides deeper entries but keeps their counts.
    #[test]
    fn test_ls_depth() {
        let t = tree();
        let o = run_cmd(t.path(), &["ls", "-d", "3"]);
        assert!(o.text.contains("      mod.rs 3") && o.text.contains("      deep/ 1 file"));
        let shallow = run_cmd(t.path(), &["ls", "-d", "0"]).text;
        assert!(shallow.contains("src/ 3 files") && !shallow.contains("engine"));
    }

    // Checks the entry cap and its note.
    #[test]
    fn test_ls_cap() {
        let t = tree();
        let o = run_cmd(t.path(), &["ls", "-m", "2"]);
        assert!(o.text.ends_with("… 3 more entries (raise -m, or list a subdirectory)"), "{}", o.text);
    }

    // Checks listing a subdirectory, a file and errors.
    #[test]
    fn test_ls_subdir_file_and_errors() {
        let t = tree();
        assert!(run_cmd(t.path(), &["ls", "src/engine"])
            .text
            .starts_with("src/engine/ 2 files\n  deep/ 1 file\n    x.rs 1\n  mod.rs 3"));
        assert_eq!(run_cmd(t.path(), &["ls", "README.md"]).text, "README.md 1");
        assert_eq!(run_cmd(t.path(), &["ls", "nope"]).text, "error: nope: no such file or directory");
        assert!(run_cmd(t.path(), &["ls", "a", "b"]).text.starts_with("error: usage: ls"));
        assert!(run_cmd(t.path(), &["ls", "--x"]).text.contains("unknown flag"));
    }

    // Checks the singular wording and an unreadable file marker.
    #[test]
    fn test_ls_singular_and_unreadable() {
        use std::os::unix::fs::PermissionsExt;
        let t = TempDir::new();
        let p = t.write("only.txt", b"x\n");
        assert!(run_cmd(t.path(), &["ls"]).text.starts_with("./ 1 file\n"));
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o0)).expect("chmod");
        let readable = std::fs::read(&p).is_ok();
        let o = run_cmd(t.path(), &["ls"]).text;
        assert!(o.ends_with(if readable { "only.txt 1" } else { "only.txt ?" }));
    }

    // Checks the no-ignore flag lists ignored files.
    #[test]
    fn test_ls_no_ignore() {
        let t = TempDir::new();
        t.write(".gitignore", b"*.log\n");
        t.write("a.log", b"x\n");
        assert!(!run_cmd(t.path(), &["ls"]).text.contains("a.log"));
        assert!(run_cmd(t.path(), &["ls", "-u"]).text.contains("a.log"));
    }

    // Checks a directory entry dropped by the cap is not descended into.
    #[test]
    fn test_ls_cap_drops_directories() {
        let t = TempDir::new();
        t.write("a/x.txt", b"1\n");
        t.write("b/y.txt", b"1\n");
        let o = run_cmd(t.path(), &["ls", "-m", "1"]).text;
        assert!(
            o.contains("a/ 1 file")
                && !o.contains("b/ 1 file")
                && o.ends_with("… 2 more entries (raise -m, or list a subdirectory)")
        );
    }
}
