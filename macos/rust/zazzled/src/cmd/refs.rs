// The refs command: where a symbol is defined and used, definitions first, with the enclosing symbol of each use; hits in comments and strings are dropped.
use super::find::{build_regex, code_only, rank_in, search_all, FileResult, Hit, Plan, NARROW};
use super::{Ctx, Output};
use crate::args::{self, flag, opt, Flag};
use crate::engine::Engine;
use crate::outline::{enclosing, noncode_ranges, Symbol};
use crate::pool::{cpus, par_map};
use crate::util::{clip, display, locate, MAX_LINE};
use crate::walk::{self, WalkOpts};
use std::path::{Path, PathBuf};

pub(super) const SPEC: &[Flag] =
    &[opt("glob", 'g'), opt("max", 'm'), opt("also", '\0'), flag("no-ignore", 'u'), flag("code-only", '\0')];
const USAGE: &str =
    "usage: refs SYMBOL|Type.Member [PATH..] [--also SYMBOL].. [-g GLOB] [-m N] [-u] [--code-only]";
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

// One file's definitions and uses of the symbol, with its display rank.
struct FileRefs {
    path: String,
    rank: (bool, usize),
    defs: Vec<Symbol>,
    uses: Vec<Occurrence>,
}

// Splits a file's hits into definitions (declaration lines) and plain uses.
fn classify(
    hits: &[&Hit],
    data: &[u8],
    starts: &[u32],
    syms: &[Symbol],
    name: &str,
) -> (Vec<Symbol>, Vec<Occurrence>) {
    let (mut defs, mut uses) = (Vec::new(), Vec::new());
    for h in hits {
        if let Some(d) = syms.iter().find(|s| s.start == h.line && declares(s, name)) {
            defs.push(d.clone());
            continue;
        }
        let line = String::from_utf8_lossy(crate::meta::line_bytes(data, starts, h.line));
        let scope = enclosing(syms, h.line).map(|s| s.name.clone());
        uses.push(Occurrence { line: h.line, text: clip(line.trim_end(), h.col, MAX_LINE), scope });
    }
    (defs, uses)
}

// Drops the hits whose line names the symbol only inside comments or string literals.
fn code_hits<'a>(path: &Path, data: &[u8], starts: &[u32], name: &str, file: &'a FileResult) -> Vec<&'a Hit> {
    let skip = noncode_ranges(path, data);
    let inside = |off: usize| {
        let i = skip.partition_point(|r| r.1 <= off);
        skip.get(i).is_some_and(|r| r.0 <= off)
    };
    let code_on = |h: &Hit| {
        let Some(&start) = starts.get(h.line - 1) else { return true };
        let start = start as usize;
        let end = starts.get(h.line).map_or(data.len(), |e| *e as usize);
        let line = &data[start..end];
        let hits = (0..line.len()).filter(|i| line[*i..].starts_with(name.as_bytes()));
        hits.map(|i| start + i).chain(std::iter::once(start + h.col)).any(|off| !inside(off))
    };
    file.hits.iter().filter(|h| code_on(h)).collect()
}

// Splits a qualified name, Type.Member or a::b, into the member searched for and the type a file must name.
fn split_qualified(query: &str) -> (&str, Option<&str>) {
    let segs: Vec<&str> = query.split(['.', ':']).filter(|s| !s.is_empty()).collect();
    match segs.as_slice() {
        [.., ty, member] => (member, Some(ty)),
        _ => (query, None),
    }
}

// Loads and outlines one matching file and splits its hits into definitions and uses; None if it can no longer be read, does not name the qualifying type, or has none left.
fn file_refs(
    engine: &Engine,
    path: &Path,
    file: &FileResult,
    name: &str,
    qual: Option<&str>,
    cwd: &Path,
) -> Option<FileRefs> {
    let loaded = engine.load(path).ok()?;
    if let Some(q) = qual {
        if !loaded.data.windows(q.len()).any(|w| w == q.as_bytes()) {
            return None;
        }
    }
    let hits = code_hits(path, &loaded.data, &loaded.meta.starts, name, file);
    if hits.is_empty() {
        return None;
    }
    let syms = engine.outline(path, &loaded);
    let (defs, uses) = classify(&hits, &loaded.data, &loaded.meta.starts, &syms, name);
    Some(FileRefs { path: display(path, cwd), rank: rank_in(path, cwd), defs, uses })
}

// Keeps the exact answer's definitions and reference groups whose file the scope keeps, and restates the totals for what is left.
fn in_scope(text: &str, keep: impl Fn(&str) -> bool) -> String {
    let (mut out, mut in_refs, mut take) = (Vec::new(), false, true);
    let (mut defs, mut refs, mut files) = (0, 0, 0);
    for line in text.lines() {
        if line == "defs" || line == "refs" {
            in_refs = line == "refs";
            out.push(line.to_string());
        } else if line.starts_with("-- exact") {
            out.push(format!("-- exact (Roslyn), within the paths and globs given: {defs} defs, {refs} refs in {files} files"));
        } else if !in_refs {
            if keep(line.split(':').next().unwrap_or("")) {
                defs += 1;
                out.push(line.to_string());
            }
        } else if line.split(' ').next().is_some_and(|w| w.parse::<usize>().is_ok()) {
            if take {
                refs += 1;
                out.push(line.to_string());
            }
        } else {
            take = keep(line);
            files += usize::from(take);
            if take {
                out.push(line.to_string());
            }
        }
    }
    out.join("\n")
}

// Runs the refs command; each --also name gets its own answer under a `== NAME` line, with the same paths and options.
pub fn run(ctx: &Ctx, argv: &[String]) -> Result<Output, String> {
    let p = args::parse(SPEC, argv).map_err(|e| format!("{e}; {USAGE}"))?;
    let Some(first) = p.positional.first() else { return Err(USAGE.into()) };
    let also = p.values("also");
    if also.is_empty() {
        return run_one(ctx, argv);
    }
    let mut rest: Vec<String> = Vec::new();
    let mut it = argv.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--also" => {
                it.next();
            }
            s if s.starts_with("--also=") => {}
            s if s == first => {}
            _ => rest.push(a.clone()),
        }
    }
    let mut parts = Vec::new();
    for name in std::iter::once(first.as_str()).chain(also) {
        let line: Vec<String> = std::iter::once(name.to_string()).chain(rest.iter().cloned()).collect();
        parts.push(format!("== {name}\n{}", run_one(ctx, &line)?.text));
    }
    Ok(Output::ok(parts.join("\n")))
}

// Answers refs for one name.
fn run_one(ctx: &Ctx, argv: &[String]) -> Result<Output, String> {
    let p = args::parse(SPEC, argv).map_err(|e| format!("{e}; {USAGE}"))?;
    let Some(query) = p.positional.first() else { return Err(USAGE.into()) };
    if query.contains(['|', '(', '[', '^', '$', '\\']) {
        return Err(format!(
            "refs takes a literal name, not a pattern: `{query}`; pass more names with --also"
        ));
    }
    let (name, qual) = split_qualified(query);
    let max = p.number("max")?.unwrap_or(DEFAULT_MAX).max(1);
    let re = build_regex(name, false, true, true)?;
    let roots: Vec<PathBuf> = if p.positional.len() > 1 {
        p.positional[1..].iter().map(|a| locate(a, &ctx.cwd)).collect()
    } else {
        vec![ctx.cwd.clone()]
    };
    let opts = WalkOpts {
        no_ignore: p.has("no-ignore"),
        globs: p.values("glob").iter().map(|s| s.to_string()).collect(),
    };
    let files = code_only(walk::collect(&roots, &opts, &ctx.engine.walks)?, &ctx.cwd, p.has("code-only"));
    let is_cs = |f: &PathBuf| f.extension().is_some_and(|e| e.eq_ignore_ascii_case("cs"));
    let plan = Plan { engine: ctx.engine, re, max: PER_FILE, context: 0, files_only: false };
    let mut note = String::new();
    if files.iter().any(is_cs) {
        match super::structure::exact_refs(ctx, query) {
            Ok(mut o) => {
                let scoped = p.positional.len() > 1 || !opts.globs.is_empty();
                if scoped {
                    let keep: std::collections::HashSet<String> =
                        files.iter().map(|f| display(f, &ctx.cwd)).collect();
                    o.text = in_scope(&o.text, |path| keep.contains(path));
                }
                let others: Vec<PathBuf> = files.iter().filter(|f| !is_cs(f)).cloned().collect();
                let n = search_all(&others, &plan, cpus()).iter().filter(|r| r.total > 0).count();
                if n > 0 {
                    o.text.push_str(&format!(
                        "\n-- text matches in {n} other files: zazzle refs {query} -g '!*.cs'"
                    ));
                }
                return Ok(o);
            }
            Err(e) if e.contains("no C# declaration") => {}
            Err(e) => {
                note =
                    format!("\n-- text match; exact C# refs unavailable: {}", e.lines().next().unwrap_or(""))
            }
        }
    }
    let results = search_all(&files, &plan, cpus());
    let matched: Vec<&FileResult> = results.iter().filter(|r| r.total > 0).collect();
    let (engine, cwd) = (ctx.engine, &ctx.cwd);
    let mut found: Vec<FileRefs> =
        par_map(&matched, cpus(), |_, r| file_refs(engine, &files[r.idx], r, name, qual, cwd))
            .into_iter()
            .flatten()
            .collect();
    found.sort_by_key(|f| (f.defs.is_empty(), f.rank));
    let (mut def_lines, mut use_blocks) = (Vec::new(), Vec::new());
    let (mut n_defs, mut n_uses, mut shown) = (0, 0, 0);
    let matched_files = found.len();
    for f in &found {
        n_defs += f.defs.len();
        n_uses += f.uses.len();
        for d in f.defs.iter().take(DEF_CAP.saturating_sub(def_lines.len())) {
            def_lines.push(format!("{}:{}-{} {} {}", f.path, d.start, d.end, d.kind, d.name));
        }
        let take = f.uses.len().min(max - shown);
        if take > 0 {
            let mut block = format!("{}\n", f.path);
            for u in &f.uses[..take] {
                let scope = u.scope.as_ref().map_or(String::new(), |s| format!(" [{s}]"));
                block.push_str(&format!("{}{scope} {}\n", u.line, u.text));
            }
            use_blocks.push(block);
            shown += take;
        }
    }
    if n_defs + n_uses == 0 {
        return Ok(Output::ok(match qual {
            Some(q) => {
                format!("no references to {name} in the files naming {q} ({} searched){note}", files.len())
            }
            None => format!("no references to {name} in {} files{note}", files.len()),
        }));
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
        "-- {n_defs} defs, {n_uses} refs in {matched_files} files ({} searched{})",
        files.len(),
        qual.map_or(String::new(), |q| format!(", {name} in files naming {q}"))
    ));
    if shown < n_uses {
        out.push_str(&format!("; showing {shown} refs, {NARROW}"));
    }
    out.push_str(&note);
    Ok(Output::ok(out))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::in_scope;
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
            "defs\nsrc/lib.rs:1-3 fn parse\nrefs\nsrc/lib.rs\n6 [other]     parse(1);\nsrc/main.rs\n2 [main]     let a = parse(2);\n3 [main]     let b = parse(3);\nREADME.md\n1 call parse often\n-- 1 defs, 4 refs in 3 files (3 searched)"
        );
        assert!(o.idable);
    }

    // Checks Type.Member finds the member's uses through instances, in files that name the type, and --code-only drops docs.
    #[test]
    fn test_refs_qualified_and_code_only() {
        let t = TempDir::new();
        t.write("src/svc.rs", b"struct Billing;\nimpl Billing {\n    fn charge(&self) {}\n}\n");
        t.write("src/use.rs", b"fn x(b: &Billing) {\n    b.charge();\n}\n");
        t.write("src/other.rs", b"fn y() {\n    charge();\n}\n");
        let o = run_cmd(t.path(), &["refs", "Billing.charge"]).text;
        assert!(o.contains("src/svc.rs:3-3 fn charge") && o.contains("src/use.rs\n2 [x]"), "{o}");
        assert!(!o.contains("other.rs") && o.contains(", charge in files naming Billing)"), "{o}");
        assert_eq!(
            run_cmd(t.path(), &["refs", "Nobody.charge"]).text,
            "no references to charge in the files naming Nobody (3 searched)"
        );
        let p = project();
        let code = run_cmd(p.path(), &["refs", "parse", "--code-only"]).text;
        assert!(!code.contains("README.md") && code.ends_with("(2 searched)"), "{code}");
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
        assert!(o.ends_with("-- 1 defs, 4 refs in 3 files (3 searched); showing 2 refs, narrow with a path, -g GLOB or --code-only"));
        assert!(!o.contains("README.md"));
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

    // Checks C# mentions in comments and strings are not references, and a file with only those is left out.
    #[test]
    fn test_refs_csharp_skips_comments_and_strings() {
        let t = TempDir::new();
        t.write(
            "W.cs",
            b"// Widget is documented here\npublic class Widget\n{\n    string Label = \"Widget label\";\n    void Use(Widget w) { }\n}\n",
        );
        t.write("Doc.cs", b"/* Widget */\nclass Other { string s = \"Widget\"; }\n");
        t.write("notes.txt", b"Widget\n");
        let o = run_cmd(t.path(), &["refs", "Widget"]).text;
        assert_eq!(
            o,
            "defs\nW.cs:2-6 class Widget\nrefs\nW.cs\n5 [Use]     void Use(Widget w) { }\nnotes.txt\n1 Widget\n-- 1 defs, 2 refs in 2 files (3 searched)\n-- text match; exact C# refs unavailable: not inside a git work tree, and no git repositories below this folder"
        );
    }

    // Checks comments and strings are dropped in every language: from the parse tree for Rust, by a lexical scan for Go, shell and Lua.
    #[test]
    fn test_refs_skip_comments_and_strings_everywhere() {
        let t = TempDir::new();
        t.write("a.rs", b"// widget here\nfn widget() {}\nfn m() { let s = \"widget\"; widget(); }\n");
        t.write(
            "b.go",
            b"// widget\n/* widget\n widget */\nfunc m() { s := \"a \\\" widget\"; c := 'w'; widget() }\n",
        );
        t.write("c.sh", b"# widget\necho 'widget' \"widget\"\nwidget\n");
        t.write("d.lua", b"-- widget\nwidget()\n");
        let o = run_cmd(t.path(), &["refs", "widget"]).text;
        for gone in ["1 // widget", "2 /*", "3  widget */", "1 # widget", "2 echo", "1 -- widget"] {
            assert!(!o.contains(gone), "{gone}: {o}");
        }
        assert!(
            o.contains("3 [m] fn m()")
                && o.contains("4 ")
                && o.contains("c.sh\n3 widget")
                && o.contains("d.lua\n2 widget()"),
            "{o}"
        );
    }

    // Checks a glob or a path keeps refs on text, without asking the indexer.
    #[test]
    fn test_refs_csharp_narrowed_asks_exact_first() {
        let t = TempDir::new();
        t.write("W.cs", b"class Widget { }\n");
        let o = run_cmd(t.path(), &["refs", "Widget", "-g", "*.cs"]).text;
        assert!(
            o.contains("-- 1 defs, 0 refs in 1 files (1 searched)\n-- text match; exact C# refs unavailable"),
            "{o}"
        );
        let p = run_cmd(t.path(), &["refs", "Widget", "."]).text;
        assert!(p.contains("exact C# refs unavailable"), "{p}");
    }

    // Checks a scope keeps only the exact answer's definitions and reference groups in its files, and restates the totals.
    #[test]
    fn test_in_scope() {
        let text = "defs\napi/E.cs:3 method Api.E.Run\nsim/S.cs:9 method Sim.S.Run\nrefs\napi/C.cs\n4 e.Run();\n5 e.Run();\nsim/T.cs\n7 s.Run();\n-- exact (Roslyn): 2 defs, 3 refs in 2 files";
        let o = in_scope(text, |p| p.starts_with("api/"));
        assert_eq!(o, "defs\napi/E.cs:3 method Api.E.Run\nrefs\napi/C.cs\n4 e.Run();\n5 e.Run();\n-- exact (Roslyn), within the paths and globs given: 1 defs, 2 refs in 1 files");
    }

    // Checks several names are answered together under their own headings, and a pattern is refused.
    #[test]
    fn test_refs_batch_and_literal_names() {
        let t = TempDir::new();
        t.write("a.rs", b"fn alpha() {}\nfn beta() { alpha(); }\nfn main() { beta(); }\n");
        let o = run_cmd(t.path(), &["refs", "alpha", "--also", "beta", "."]).text;
        let (a, b) = (o.find("== alpha\n").expect("alpha"), o.find("== beta\n").expect("beta"));
        assert!(a < b && o[b..].contains("3 [main]"), "{o}");
        let eq = run_cmd(t.path(), &["refs", "alpha", "--also=beta"]).text;
        assert!(eq.contains("== beta\n"), "{eq}");
        let worse = run_cmd(t.path(), &["refs", "alpha", "--also", "b|c"]);
        assert!(worse.status != 0 && worse.text.contains("literal name"), "{}", worse.text);
        assert!(run_cmd(t.path(), &["refs", "--also", "beta"]).status != 0);
        let bad = run_cmd(t.path(), &["refs", "alpha|beta"]);
        assert!(bad.status != 0 && bad.text.contains("literal name"), "{}", bad.text);
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
