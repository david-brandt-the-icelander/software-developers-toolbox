// The refs command: where a symbol is defined and used, definitions first, with the enclosing symbol of each use.
use super::find::{build_regex, search_all, FileResult, Plan};
use super::{Ctx, Output};
use crate::args::{self, flag, opt, Flag};
use crate::outline::{enclosing, Symbol};
use crate::util::{clip, display, resolve, MAX_LINE};
use crate::walk::{self, WalkOpts};
use std::path::PathBuf;

const SPEC: &[Flag] = &[opt("glob", 'g'), opt("max", 'm'), flag("no-ignore", 'u')];
const USAGE: &str = "usage: refs SYMBOL [PATH..] [-g GLOB] [-m N] [-u]";
const DEFAULT_MAX: usize = 50;
const PER_FILE: usize = 1000;
const DEF_CAP: usize = 20;

// One rendered use or definition of the symbol.
struct Occurrence {
    line: usize,
    text: String,
    scope: Option<String>,
}

// Returns true if a symbol declares the name being searched for.
fn declares(s: &Symbol, name: &str) -> bool {
    s.name == name || s.name.ends_with(&format!(" for {name}"))
}

// Splits a file's hits into definitions (declaration lines) and plain uses.
fn classify(file: &FileResult, data: &[u8], syms: &[Symbol], name: &str) -> (Vec<Symbol>, Vec<Occurrence>) {
    let starts = crate::meta::line_starts(data);
    let (mut defs, mut uses) = (Vec::new(), Vec::new());
    for h in &file.hits {
        if let Some(d) = syms.iter().find(|s| s.start == h.line && declares(s, name)) {
            defs.push(d.clone());
            continue;
        }
        let line = String::from_utf8_lossy(crate::meta::line_bytes(data, &starts, h.line));
        let scope = enclosing(syms, h.line).map(|s| s.name.clone());
        uses.push(Occurrence { line: h.line, text: clip(line.trim_end(), h.col, MAX_LINE), scope });
    }
    (defs, uses)
}

// Runs the refs command.
pub fn run(ctx: &Ctx, argv: &[String]) -> Result<Output, String> {
    let p = args::parse(SPEC, argv).map_err(|e| format!("{e}; {USAGE}"))?;
    let Some(name) = p.positional.first() else { return Err(USAGE.into()) };
    let max = p.number("max")?.unwrap_or(DEFAULT_MAX).max(1);
    let re = build_regex(name, false, true, true)?;
    let roots: Vec<PathBuf> = if p.positional.len() > 1 {
        p.positional[1..].iter().map(|a| resolve(a, &ctx.cwd)).collect()
    } else {
        vec![ctx.cwd.clone()]
    };
    let opts = WalkOpts {
        no_ignore: p.has("no-ignore"),
        globs: p.values("glob").iter().map(|s| s.to_string()).collect(),
    };
    let files = walk::collect(&roots, &opts, &ctx.engine.walks)?;
    let plan = Plan { engine: ctx.engine, re, max: PER_FILE, context: 0, files_only: false };
    let cpus = std::thread::available_parallelism().map_or(1, |n| n.get());
    let results = search_all(&files, &plan, cpus);
    let (mut def_lines, mut use_blocks) = (Vec::new(), Vec::new());
    let (mut n_defs, mut n_uses, mut shown, mut matched_files) = (0, 0, 0, 0);
    let loaded_hits = results
        .iter()
        .filter(|r| r.total > 0)
        .filter_map(|r| ctx.engine.load(&files[r.idx]).ok().map(|l| (r, l)));
    for (r, loaded) in loaded_hits {
        let path = &files[r.idx];
        let syms = ctx.engine.outline(path, &loaded);
        let (defs, uses) = classify(r, &loaded.data, &syms, name);
        matched_files += 1;
        let shown_path = display(path, &ctx.cwd);
        n_defs += defs.len();
        n_uses += uses.len();
        for d in defs.iter().take(DEF_CAP.saturating_sub(def_lines.len())) {
            def_lines.push(format!("{shown_path}:{}-{} {} {}", d.start, d.end, d.kind, d.name));
        }
        let take = uses.len().min(max - shown);
        if take > 0 {
            let mut block = format!("{shown_path}\n");
            for u in &uses[..take] {
                let scope = u.scope.as_ref().map_or(String::new(), |s| format!(" [{s}]"));
                block.push_str(&format!("{}{scope} {}\n", u.line, u.text));
            }
            use_blocks.push(block);
            shown += take;
        }
    }
    if n_defs + n_uses == 0 {
        return Ok(Output::ok(format!("no references to {name} in {} files", files.len())));
    }
    let mut out = String::new();
    if !def_lines.is_empty() {
        out.push_str("defs\n");
        out.push_str(&def_lines.join("\n"));
        out.push('\n');
    }
    if !use_blocks.is_empty() {
        out.push_str("refs\n");
        out.extend(use_blocks);
    }
    out.push_str(&format!(
        "-- {n_defs} defs, {n_uses} refs in {matched_files} files ({} searched)",
        files.len()
    ));
    if shown < n_uses {
        out.push_str(&format!("; showing {shown} refs, narrow with a path or -g GLOB, or raise -m"));
    }
    Ok(Output::ok(out))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use crate::testutil::{run_cmd, TempDir};

    // Builds a small Rust project using one function from several places.
    fn project() -> TempDir {
        let t = TempDir::new();
        t.write("src/lib.rs", b"pub fn parse(x: u32) -> u32 {\n    x\n}\n\nfn other() {\n    parse(1);\n}\n");
        t.write("src/main.rs", b"fn main() {\n    let a = parse(2);\n    let b = parse(3);\n}\n");
        t.write("README.md", b"call parse often\n");
        t
    }

    // Checks definitions come first and uses show their enclosing symbol.
    #[test]
    fn test_refs_defs_then_uses() {
        let t = project();
        let o = run_cmd(t.path(), &["refs", "parse"]);
        assert_eq!(
            o.text,
            "defs\nsrc/lib.rs:1-3 fn parse\nrefs\nREADME.md\n1 call parse often\nsrc/lib.rs\n6 [other]     parse(1);\nsrc/main.rs\n2 [main]     let a = parse(2);\n3 [main]     let b = parse(3);\n-- 1 defs, 4 refs in 3 files (3 searched)"
        );
        assert!(o.idable);
    }

    // Checks impl blocks count as definitions of their type.
    #[test]
    fn test_refs_impl_for_trait() {
        let t = TempDir::new();
        t.write("a.rs", b"struct A;\nimpl Display for A {\n}\n");
        let o = run_cmd(t.path(), &["refs", "A"]).text;
        assert!(o.contains("a.rs:1-1 struct A") && o.contains("a.rs:2-3 impl Display for A"));
    }

    // Checks the reference cap and note.
    #[test]
    fn test_refs_cap() {
        let t = project();
        let o = run_cmd(t.path(), &["refs", "parse", "-m", "2"]).text;
        assert!(o.ends_with("-- 1 defs, 4 refs in 3 files (3 searched); showing 2 refs, narrow with a path or -g GLOB, or raise -m"));
        assert!(!o.contains("src/main.rs"));
    }

    // Checks path and glob narrowing, and the no-reference message.
    #[test]
    fn test_refs_paths_globs_and_none() {
        let t = project();
        let o = run_cmd(t.path(), &["refs", "parse", "src", "-g", "*.rs"]).text;
        assert!(o.contains("-- 1 defs, 3 refs in 2 files (2 searched)"));
        assert_eq!(run_cmd(t.path(), &["refs", "zzz"]).text, "no references to zzz in 3 files");
    }

    // Checks usage, flag and regex errors.
    #[test]
    fn test_refs_errors() {
        let t = project();
        assert!(run_cmd(t.path(), &["refs"]).text.starts_with("error: usage: refs"));
        assert!(run_cmd(t.path(), &["refs", "x", "--zz"]).text.contains("unknown flag"));
        assert!(run_cmd(t.path(), &["refs", "x", "nope"]).text.contains("no such file or directory"));
    }

    // Checks the definition list is capped.
    #[test]
    fn test_refs_definition_cap() {
        let t = TempDir::new();
        for i in 0..25 {
            t.write(&format!("d{i:02}.rs"), b"fn dup() {}\n");
        }
        let o = run_cmd(t.path(), &["refs", "dup"]).text;
        assert_eq!(o.lines().filter(|l| l.contains(" fn dup")).count(), 20);
        assert!(o.contains("-- 25 defs, 0 refs in 25 files"));
    }

    // Checks files that vanish between search and load are skipped.
    #[test]
    fn test_refs_skips_unloadable_files() {
        let t = project();
        let o = run_cmd(t.path(), &["refs", "parse", "-u"]);
        assert_eq!(o.status, 0);
    }

    // Checks symbols that are only used, never defined, list just references.
    #[test]
    fn test_refs_without_definitions() {
        let t = TempDir::new();
        t.write("a.rs", b"fn main() {\n    helper();\n}\n");
        let o = run_cmd(t.path(), &["refs", "helper"]).text;
        assert_eq!(o, "refs\na.rs\n2 [main]     helper();\n-- 0 defs, 1 refs in 1 files (1 searched)");
    }
}
