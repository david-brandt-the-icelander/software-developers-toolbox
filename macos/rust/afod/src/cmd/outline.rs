// The outline command: a file's symbols with line ranges, so a large file can be read in pieces.
use super::{Ctx, Output};
use crate::args::{self, opt, Flag};
use crate::outline::{self, Symbol};
use crate::util::{display, resolve};
use crate::walk::{self, WalkOpts};
use std::path::PathBuf;

const SPEC: &[Flag] = &[opt("depth", 'd'), opt("max", 'm')];
const USAGE: &str = "usage: outline PATH.. [-d N] [-m N]";
const DEFAULT_DEPTH: usize = 3;
const DEFAULT_MAX: usize = 200;

// Expands path arguments into files: directories contribute their outlinable files.
fn targets(ctx: &Ctx, args: &[String]) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    for a in args {
        let path = resolve(a, &ctx.cwd);
        let md = std::fs::metadata(&path)
            .map_err(|e| format!("{}: {}", display(&path, &ctx.cwd), walk::reason(&e)))?;
        if md.is_dir() {
            let all = walk::collect(std::slice::from_ref(&path), &WalkOpts::default(), &ctx.engine.walks)?;
            files.extend(all.into_iter().filter(|f| outline::worth_outlining(f)));
        } else {
            files.push(path);
        }
    }
    Ok(files)
}

// Renders one symbol line, indented by depth.
fn symbol_line(s: &Symbol) -> String {
    format!("{}{}-{} {} {}", "  ".repeat(s.depth), s.start, s.end, s.kind, s.name)
}

// Runs the outline command.
pub fn run(ctx: &Ctx, argv: &[String]) -> Result<Output, String> {
    let p = args::parse(SPEC, argv).map_err(|e| format!("{e}; {USAGE}"))?;
    if p.positional.is_empty() {
        return Err(USAGE.into());
    }
    let depth = p.number("depth")?.unwrap_or(DEFAULT_DEPTH);
    let mut left = p.number("max")?.unwrap_or(DEFAULT_MAX).max(1);
    let (mut out, mut dropped, mut skipped_files) = (String::new(), 0, 0);
    for path in targets(ctx, &p.positional)? {
        if left == 0 {
            skipped_files += 1;
            continue;
        }
        let shown = display(&path, &ctx.cwd);
        let loaded = ctx.engine.load(&path).map_err(|e| format!("{shown}: {e}"))?;
        if loaded.meta.binary {
            out.push_str(&format!("{shown}: binary, {} bytes\n", loaded.meta.size));
            continue;
        }
        let syms = ctx.engine.outline(&path, &loaded);
        let visible: Vec<&Symbol> = syms.iter().filter(|s| s.depth <= depth).collect();
        let note = if visible.is_empty() { " (no symbols)" } else { "" };
        out.push_str(&format!("{shown} {} lines{note}\n", loaded.meta.lines()));
        for s in visible {
            if left == 0 {
                dropped += 1;
                continue;
            }
            left -= 1;
            out.push_str(&symbol_line(s));
            out.push('\n');
        }
    }
    if dropped > 0 {
        out.push_str(&format!("… {dropped} more symbols (raise -m, lower -d, or outline a single file)\n"));
    }
    if skipped_files > 0 {
        out.push_str(&format!(
            "… {skipped_files} more files not outlined (raise -m, or outline a subdirectory)\n"
        ));
    }
    out.pop();
    Ok(Output::ok(out))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use crate::testutil::{run_cmd, TempDir};

    // Builds a project with Rust, Markdown and plain text files.
    fn project() -> TempDir {
        let t = TempDir::new();
        t.write("src/a.rs", b"struct A;\n\nimpl A {\n    fn new() -> A {\n        A\n    }\n}\n");
        t.write("docs/readme.md", b"# Title\n\n## Part\n");
        t.write("notes.txt", b"nothing here\n");
        t.write("bin.dat", b"\0\0");
        t
    }

    // Checks a file outline with nesting and ranges.
    #[test]
    fn test_outline_file() {
        let t = project();
        let o = run_cmd(t.path(), &["outline", "src/a.rs"]);
        assert_eq!(o.text, "src/a.rs 7 lines\n1-1 struct A\n3-7 impl A\n  4-6 fn new");
        assert!(o.idable);
    }

    // Checks the depth limit hides nested symbols.
    #[test]
    fn test_outline_depth_limit() {
        let t = project();
        let o = run_cmd(t.path(), &["outline", "src/a.rs", "-d", "0"]);
        assert_eq!(o.text, "src/a.rs 7 lines\n1-1 struct A\n3-7 impl A");
    }

    // Checks directories list outlinable files only.
    #[test]
    fn test_outline_directory() {
        let t = project();
        let o = run_cmd(t.path(), &["outline", "."]).text;
        assert!(o.contains("docs/readme.md 3 lines\n1-3 h1 Title\n  3-3 h2 Part"));
        assert!(o.contains("src/a.rs 7 lines") && !o.contains("notes.txt"));
    }

    // Checks the symbol cap and its note.
    #[test]
    fn test_outline_cap() {
        let t = project();
        let o = run_cmd(t.path(), &["outline", "src/a.rs", "-m", "1"]).text;
        assert!(o.ends_with("… 2 more symbols (raise -m, lower -d, or outline a single file)"));
    }

    // Checks files without symbols and binary files.
    #[test]
    fn test_outline_no_symbols_and_binary() {
        let t = project();
        assert_eq!(run_cmd(t.path(), &["outline", "notes.txt"]).text, "notes.txt 1 lines (no symbols)");
        assert_eq!(run_cmd(t.path(), &["outline", "bin.dat"]).text, "bin.dat: binary, 2 bytes");
    }

    // Checks usage and path errors.
    #[test]
    fn test_outline_errors() {
        let t = project();
        assert!(run_cmd(t.path(), &["outline"]).text.starts_with("error: usage: outline"));
        assert!(run_cmd(t.path(), &["outline", "--zzz"]).text.contains("unknown flag"));
        assert_eq!(
            run_cmd(t.path(), &["outline", "nope.rs"]).text,
            "error: nope.rs: no such file or directory"
        );
    }

    // Checks a file that cannot be loaded is reported with its name.
    #[test]
    fn test_outline_unloadable_file() {
        let t = project();
        let sock = t.path().join("fifo");
        let c = std::ffi::CString::new(sock.to_str().expect("utf8")).expect("cstring");
        // SAFETY: c is a valid NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        let o = run_cmd(t.path(), &["outline", "fifo"]).text;
        assert_eq!(o, "error: fifo: not a regular file");
    }

    // Checks files after the cap is reached are counted but not read.
    #[test]
    fn test_outline_stops_reading_files_at_the_cap() {
        let t = TempDir::new();
        t.write("a.rs", b"fn a() {}\n");
        t.write("b.rs", b"fn b() {}\n");
        t.write("c.rs", b"fn c() {}\n");
        let o = run_cmd(t.path(), &["outline", ".", "-m", "1"]).text;
        assert_eq!(
            o,
            "a.rs 1 lines\n1-1 fn a\n… 2 more files not outlined (raise -m, or outline a subdirectory)"
        );
    }
}
