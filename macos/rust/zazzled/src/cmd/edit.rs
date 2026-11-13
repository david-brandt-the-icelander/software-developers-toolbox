// The edit command: safe text and line edits, confined to the working directory and written atomically.
use super::{Ctx, Output};
use crate::args::{self, flag, opt, Flag};
use crate::meta::{anchor_str, line_anchor, Meta};
use crate::util::{display, hex8, resolve};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

const SPEC: &[Flag] = &[
    opt("old", '\0'),
    opt("new", '\0'),
    flag("all", '\0'),
    flag("create", '\0'),
    opt("lines", '\0'),
    opt("after", '\0'),
    opt("expect", '\0'),
    opt("at", '\0'),
];
const USAGE: &str = "usage: edit PATH --old TEXT --new TEXT [--all] --expect SHA | PATH --lines A-B --new TEXT (--expect SHA | --at A#xyz-B#xyz) | PATH --after N --new TEXT --expect SHA | PATH --create --new TEXT; SHA and anchors come from read --source; TEXT '-' reads stdin";
const MAX_LISTED: usize = 5;

// One end of a line range: the line number and its optional anchor.
type SpanEnd = (usize, Option<String>);

static TEMP_COUNTER: AtomicUsize = AtomicUsize::new(0);

// Checks a path stays inside the working directory after resolving symlinks, and returns its real location.
fn confine(cwd: &Path, path: &Path) -> Result<PathBuf, String> {
    let root = cwd.canonicalize().map_err(|e| format!("{}: {}", cwd.display(), crate::walk::reason(&e)))?;
    let mut base = path.to_path_buf();
    let mut tail = Vec::new();
    while !base.exists() {
        let Some(name) = base.file_name().map(|n| n.to_os_string()) else {
            return Err("path with .. through a missing directory".into());
        };
        tail.push(name);
        base.pop();
    }
    let mut real =
        base.canonicalize().map_err(|e| format!("{}: {}", base.display(), crate::walk::reason(&e)))?;
    real.extend(tail.iter().rev());
    if real.starts_with(&root) {
        return Ok(real);
    }
    Err(format!("refusing to write outside {} (run from a parent directory)", root.display()))
}

// Converts text to the file's line-ending style.
fn to_eol(text: &str, crlf: bool) -> String {
    let lf = text.replace("\r\n", "\n");
    if crlf {
        lf.replace('\n', "\r\n")
    } else {
        lf
    }
}

// Returns the 1-based line containing a byte offset.
fn line_at(starts: &[u32], off: usize) -> usize {
    starts.partition_point(|s| (*s as usize) <= off)
}

// Writes a file's new content atomically, or in place when the file has several hard links.
fn write_atomic(path: &Path, data: &[u8]) -> Result<(), String> {
    let io = |e: std::io::Error| format!("{}: {}", path.display(), crate::walk::reason(&e));
    let md = std::fs::metadata(path).map_err(io)?;
    if md.nlink() > 1 {
        return std::fs::write(path, data).map_err(io);
    }
    let tmp = path.with_file_name(format!(
        ".zazzle-edit-{}-{}",
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    let result = (|| {
        let mut f = File::create(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
        std::fs::set_permissions(&tmp, md.permissions())?;
        std::fs::rename(&tmp, path)
    })();
    result.inspect_err(|_| drop(std::fs::remove_file(&tmp))).map_err(io)
}

// Resolves a value, reading standard input when it is a dash.
fn value_of(ctx: &Ctx, v: &str) -> Result<String, String> {
    if v != "-" {
        return Ok(v.to_string());
    }
    String::from_utf8(ctx.read_stdin()?).map_err(|_| "standard input is not valid UTF-8".to_string())
}

// A file loaded for editing, with everything needed to rewrite and verify it.
struct Target {
    path: PathBuf,
    shown: String,
    data: Vec<u8>,
    meta: std::sync::Arc<Meta>,
}

// Loads the file to edit, refusing binary files.
fn open_target(ctx: &Ctx, arg: &str) -> Result<Target, String> {
    let wanted = resolve(arg, &ctx.cwd);
    let shown = display(&wanted, &ctx.cwd);
    let path = confine(&ctx.cwd, &wanted)?;
    let loaded = ctx.engine.load(&path).map_err(|e| format!("{shown}: {e}"))?;
    if loaded.meta.binary {
        return Err(format!("{shown}: binary file"));
    }
    Ok(Target { path, shown, data: loaded.data, meta: loaded.meta })
}

// Writes new content for a target after checking it was not changed since it was loaded.
fn commit(ctx: &Ctx, t: &Target, data: Vec<u8>) -> Result<String, String> {
    let now = std::fs::read(&t.path).map_err(|e| format!("{}: {}", t.shown, crate::walk::reason(&e)))?;
    if *blake3::hash(&now).as_bytes() != t.meta.hash {
        return Err(format!("{}: file changed while editing; read it again", t.shown));
    }
    write_atomic(&t.path, &data)?;
    ctx.engine.forget(&t.path);
    Ok(hex8(blake3::hash(&data).as_bytes()))
}

// Suggests nearby lines when old text is missing.
fn hint(old: &str, data: &[u8], meta: &Meta) -> String {
    let first = old.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    let found: Vec<String> = (1..=meta.lines())
        .filter(|n| {
            !first.is_empty()
                && String::from_utf8_lossy(crate::meta::line_bytes(data, &meta.starts, *n)).contains(first)
        })
        .take(3)
        .map(|n| n.to_string())
        .collect();
    let short: String = first.chars().take(50).collect();
    if found.is_empty() {
        format!("no line contains '{short}'")
    } else {
        format!("lines containing '{short}': {}", found.join(", "))
    }
}

// Replaces exact text, which must be unique unless all occurrences are wanted, in a file whose current hash the caller gives.
fn replace_text(
    ctx: &Ctx,
    arg: &str,
    old: &str,
    new: &str,
    all: bool,
    expect: Option<&str>,
) -> Result<Output, String> {
    if old.is_empty() {
        return Err("--old must not be empty".into());
    }
    let t = open_target(ctx, arg)?;
    check_precondition(&t, expect, &[])?;
    let (old_b, new_b) = (to_eol(old, t.meta.crlf), to_eol(new, t.meta.crlf));
    let found: Vec<usize> = memchr::memmem::find_iter(&t.data, old_b.as_bytes()).collect();
    if found.is_empty() {
        return Err(format!("{}: old text not found; {}", t.shown, hint(old, &t.data, &t.meta)));
    }
    let lines: Vec<String> = found.iter().map(|o| line_at(&t.meta.starts, *o).to_string()).collect();
    if found.len() > 1 && !all {
        let listed = lines.iter().take(MAX_LISTED).cloned().collect::<Vec<_>>().join(", ");
        return Err(format!(
            "{}: old text matches {} places (lines {listed}); add context or use --all",
            t.shown,
            found.len()
        ));
    }
    let mut out = Vec::with_capacity(t.data.len());
    let mut at = 0;
    for off in &found {
        out.extend_from_slice(&t.data[at..*off]);
        out.extend_from_slice(new_b.as_bytes());
        at = off + old_b.len();
    }
    out.extend_from_slice(&t.data[at..]);
    let sha = commit(ctx, &t, out)?;
    let listed = lines.iter().take(MAX_LISTED).cloned().collect::<Vec<_>>().join(", ");
    Ok(Output::plain(format!(
        "ok {}: replaced {} occurrence(s) at line(s) {listed}; sha {sha}",
        t.shown,
        found.len()
    )))
}

// Parses "A", "A-B" or an anchored "A#xyz-B#xyz" into line numbers and optional anchors.
fn parse_span(spec: &str) -> Result<(SpanEnd, SpanEnd), String> {
    let bad = || format!("bad line range '{spec}'; use A-B or A#xyz-B#xyz");
    let end = |s: &str| -> Result<SpanEnd, String> {
        match s.split_once('#') {
            Some((n, a)) => Ok((n.parse().map_err(|_| bad())?, Some(a.to_string()))),
            None => Ok((s.parse().map_err(|_| bad())?, None)),
        }
    };
    let (a, b) = spec.split_once('-').unwrap_or((spec, spec));
    Ok((end(a)?, end(b)?))
}

// Checks the caller read this version of the file, by whole-file hash or line anchors from read --source; refusals never reveal the current values.
fn check_precondition(
    t: &Target,
    expect: Option<&str>,
    anchors: &[(usize, &Option<String>)],
) -> Result<(), String> {
    let anchored = anchors.iter().any(|(_, a)| a.is_some());
    if expect.is_none() && !anchored {
        return Err(
            "edits need --expect SHA (from read --source) or --at A#xyz-B#xyz (from read --source --anchors)"
                .into(),
        );
    }
    if let Some(want) = expect {
        let now = hex8(&t.meta.hash);
        if want.len() < 4 || !now.starts_with(want) {
            return Err(format!(
                "{}: the file is not the version you read; read it again with --source",
                t.shown
            ));
        }
    }
    for (n, want) in anchors {
        let now = anchor_str(line_anchor(&t.data, &t.meta.starts, *n));
        if want.as_deref().is_some_and(|w| w != now) {
            return Err(format!("{}: line {n} is not what you read; read it again with --source", t.shown));
        }
    }
    Ok(())
}

// Builds replacement lines for a block: lines joined by the file's line ending, plus a terminator when requested.
fn block(new: &str, crlf: bool, terminate: bool) -> Vec<u8> {
    let eol = if crlf { "\r\n" } else { "\n" };
    let body = new.replace("\r\n", "\n");
    let body = body.strip_suffix('\n').unwrap_or(&body);
    if new.is_empty() {
        return Vec::new();
    }
    let mut out = body.split('\n').collect::<Vec<_>>().join(eol);
    if terminate {
        out.push_str(eol);
    }
    out.into_bytes()
}

// Replaces a range of lines after verifying the file is unchanged.
fn replace_lines(
    ctx: &Ctx,
    arg: &str,
    span: &str,
    new: &str,
    expect: Option<&str>,
) -> Result<Output, String> {
    let ((a, a_anchor), (b, b_anchor)) = parse_span(span)?;
    let t = open_target(ctx, arg)?;
    let nl = t.meta.lines();
    if a < 1 || b < a || b > nl {
        return Err(format!("{}: lines {a}-{b} are outside the file ({nl} lines)", t.shown));
    }
    check_precondition(&t, expect, &[(a, &a_anchor), (b, &b_anchor)])?;
    let start = t.meta.starts[a - 1] as usize;
    let end = t.meta.starts.get(b).map_or(t.data.len(), |s| *s as usize);
    let terminated = t.data[start..end].ends_with(b"\n");
    let replacement = block(new, t.meta.crlf, terminated);
    let added = if new.is_empty() { 0 } else { new.trim_end_matches('\n').split('\n').count() };
    let mut out = t.data[..start].to_vec();
    out.extend_from_slice(&replacement);
    out.extend_from_slice(&t.data[end..]);
    let sha = commit(ctx, &t, out)?;
    let what = if added == 0 {
        format!("deleted lines {a}-{b}")
    } else {
        format!("lines {a}-{b} replaced by {added} (now {a}-{})", a + added - 1)
    };
    Ok(Output::plain(format!("ok {}: {what}; sha {sha}", t.shown)))
}

// Inserts lines after a line number (0 inserts at the start) after verifying the file is unchanged.
fn insert_after(
    ctx: &Ctx,
    arg: &str,
    after: usize,
    new: &str,
    expect: Option<&str>,
) -> Result<Output, String> {
    let t = open_target(ctx, arg)?;
    let nl = t.meta.lines();
    if after > nl {
        return Err(format!("{}: line {after} is past the end ({nl} lines)", t.shown));
    }
    check_precondition(&t, expect, &[])?;
    let at = if after == 0 { 0 } else { t.meta.starts.get(after).map_or(t.data.len(), |s| *s as usize) };
    let unterminated_tail = at == t.data.len() && at > 0 && !t.data.ends_with(b"\n");
    let eol: &[u8] = if t.meta.crlf { b"\r\n" } else { b"\n" };
    let mut insert = if unterminated_tail { eol.to_vec() } else { Vec::new() };
    insert.extend(block(new, t.meta.crlf, !unterminated_tail));
    let mut out = t.data[..at].to_vec();
    out.extend_from_slice(&insert);
    out.extend_from_slice(&t.data[at..]);
    let added = new.trim_end_matches('\n').split('\n').count();
    let sha = commit(ctx, &t, out)?;
    Ok(Output::plain(format!(
        "ok {}: inserted {added} line(s) after {after} (now {}-{}); sha {sha}",
        t.shown,
        after + 1,
        after + added
    )))
}

// Creates a new file inside the working directory.
fn create_file(ctx: &Ctx, arg: &str, new: &str) -> Result<Output, String> {
    let wanted = resolve(arg, &ctx.cwd);
    let shown = display(&wanted, &ctx.cwd);
    let path = confine(&ctx.cwd, &wanted)?;
    let io = |e: std::io::Error| format!("{shown}: {}", crate::walk::reason(&e));
    let mut data = new.as_bytes().to_vec();
    if !data.is_empty() && !data.ends_with(b"\n") {
        data.push(b'\n');
    }
    std::fs::create_dir_all(path.parent().expect("confined paths have a parent")).map_err(io)?;
    let mut f = OpenOptions::new().write(true).create_new(true).open(&path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            format!("{shown} exists; edit it with --old/--lines")
        } else {
            io(e)
        }
    })?;
    f.write_all(&data).map_err(io)?;
    let lines = data.iter().filter(|b| **b == b'\n').count();
    Ok(Output::plain(format!(
        "ok {shown}: created, {lines} lines; sha {}",
        hex8(blake3::hash(&data).as_bytes())
    )))
}

// Runs the edit command.
pub fn run(ctx: &Ctx, argv: &[String]) -> Result<Output, String> {
    let p = args::parse(SPEC, argv).map_err(|e| format!("{e}; {USAGE}"))?;
    let [path] = p.positional.as_slice() else { return Err(USAGE.into()) };
    let new = p.value("new").map(|v| value_of(ctx, v)).transpose()?;
    let new = |what: &str| new.clone().ok_or_else(|| format!("{what} needs --new TEXT"));
    let _guard = ctx.engine.edit_guard();
    if p.has("create") {
        return create_file(ctx, path, &new("--create")?);
    }
    if let Some(old) = p.value("old") {
        return replace_text(
            ctx,
            path,
            &value_of(ctx, old)?,
            &new("--old")?,
            p.has("all"),
            p.value("expect"),
        );
    }
    if let Some(span) = p.value("at").or(p.value("lines")) {
        return replace_lines(ctx, path, span, &new("--lines")?, p.value("expect"));
    }
    if let Some(n) = p.number("after")? {
        return insert_after(ctx, path, n, &new("--after")?, p.value("expect"));
    }
    Err(USAGE.into())
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::meta::{anchor_str, line_anchor, Meta};
    use crate::testutil::{run_cmd, run_cmd_with, TempDir};

    // Adds --expect with the file's current hash to a text edit that lacks one, as a caller who read the source would.
    fn signed(cwd: &std::path::Path, args: &[&str]) -> Vec<String> {
        let mut out: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let path = args.iter().skip_while(|a| **a != "edit").nth(1).copied().unwrap_or("");
        if args.contains(&"--old") && !args.contains(&"--expect") {
            if let Ok(data) = std::fs::read(cwd.join(path)) {
                out.extend(["--expect".to_string(), hex8(&Meta::compute(&data).hash)]);
            }
        }
        out
    }

    // Runs a command line in a directory with signed text edits.
    fn run_signed(cwd: &std::path::Path, args: &[&str]) -> crate::cmd::Output {
        let args = signed(cwd, args);
        run_cmd(cwd, &args.iter().map(String::as_str).collect::<Vec<_>>())
    }

    // Runs edit in a directory and returns the output text; text edits carry the current hash.
    fn edit(t: &TempDir, args: &[&str]) -> String {
        let mut full = vec!["edit"];
        full.extend_from_slice(args);
        run_signed(t.path(), &full).text
    }

    // Reads a file in the temp directory as text.
    fn cat(t: &TempDir, rel: &str) -> String {
        std::fs::read_to_string(t.path().join(rel)).expect("read")
    }

    // Returns the short hash of a file's current content.
    fn sha(t: &TempDir, rel: &str) -> String {
        hex8(&Meta::compute(&std::fs::read(t.path().join(rel)).expect("read")).hash)
    }

    // Checks unique text replacement and its summary.
    #[test]
    fn test_replace_unique_text() {
        let t = TempDir::new();
        t.write("a.txt", b"one\ntwo\nthree\n");
        let out = edit(&t, &["a.txt", "--old", "two", "--new", "2"]);
        assert!(out.starts_with("ok a.txt: replaced 1 occurrence(s) at line(s) 2; sha "), "{out}");
        assert_eq!(cat(&t, "a.txt"), "one\n2\nthree\n");
    }

    // Checks missing and ambiguous text errors with hints, and --all.
    #[test]
    fn test_replace_text_errors_and_all() {
        let t = TempDir::new();
        t.write("a.txt", b"x = 1\ny = 2\nx = 3\n");
        assert_eq!(
            edit(&t, &["a.txt", "--old", "x", "--new", "z"]),
            "error: a.txt: old text matches 2 places (lines 1, 3); add context or use --all"
        );
        assert_eq!(
            edit(&t, &["a.txt", "--old", "q = 9", "--new", "z"]),
            "error: a.txt: old text not found; no line contains 'q = 9'"
        );
        assert!(edit(&t, &["a.txt", "--old", "y=2", "--new", "z"]).contains("no line contains 'y=2'"));
        let all = edit(&t, &["a.txt", "--old", "x", "--new", "z", "--all"]);
        assert!(all.starts_with("ok a.txt: replaced 2 occurrence(s) at line(s) 1, 3"), "{all}");
        assert_eq!(cat(&t, "a.txt"), "z = 1\ny = 2\nz = 3\n");
        assert_eq!(edit(&t, &["a.txt", "--old", "", "--new", "z"]), "error: --old must not be empty");
    }

    // Checks the hint names nearby lines when part of the old text exists.
    #[test]
    fn test_replace_text_hint_with_candidates() {
        let t = TempDir::new();
        t.write("a.txt", b"alpha\nbeta gamma\n");
        let out = edit(&t, &["a.txt", "--old", "beta\nDELTA", "--new", "z"]);
        assert_eq!(out, "error: a.txt: old text not found; lines containing 'beta': 2");
    }

    // Checks line replacement with a whole-file hash precondition.
    #[test]
    fn test_replace_lines_with_expect() {
        let t = TempDir::new();
        t.write("a.txt", b"one\ntwo\nthree\nfour\n");
        let s = sha(&t, "a.txt");
        let out = edit(&t, &["a.txt", "--lines", "2-3", "--new", "A\nB\nC", "--expect", &s]);
        assert!(out.starts_with("ok a.txt: lines 2-3 replaced by 3 (now 2-4); sha "), "{out}");
        assert_eq!(cat(&t, "a.txt"), "one\nA\nB\nC\nfour\n");
    }

    // Checks line replacement guarded by anchors, and anchor mismatch.
    #[test]
    fn test_replace_lines_with_anchors() {
        let t = TempDir::new();
        t.write("a.txt", b"one\ntwo\nthree\n");
        let data = b"one\ntwo\nthree\n";
        let m = Meta::compute(data);
        let (a2, a3) =
            (anchor_str(line_anchor(data, &m.starts, 2)), anchor_str(line_anchor(data, &m.starts, 3)));
        let span = format!("2#{a2}-3#{a3}");
        let bad = format!("2#zzz-3#{a3}");
        let err = edit(&t, &["a.txt", "--at", &bad, "--new", "X"]);
        assert_eq!(err, "error: a.txt: line 2 is not what you read; read it again with --source");
        assert!(!err.contains(&a2));
        assert!(edit(&t, &["a.txt", "--at", &span, "--new", "X"])
            .starts_with("ok a.txt: lines 2-3 replaced by 1 (now 2-2)"));
        assert_eq!(cat(&t, "a.txt"), "one\nX\n");
        t.write("b.txt", b"p\nq\n");
        let one = anchor_str(line_anchor(b"p\nq\n", &Meta::compute(b"p\nq\n").starts, 1));
        assert!(edit(&t, &["b.txt", "--at", &format!("1#{one}"), "--new", "P"])
            .starts_with("ok b.txt: lines 1-1 replaced"));
    }

    // Checks deleting lines with an empty replacement.
    #[test]
    fn test_delete_lines() {
        let t = TempDir::new();
        t.write("a.txt", b"one\ntwo\nthree\n");
        let s = sha(&t, "a.txt");
        let out = edit(&t, &["a.txt", "--lines", "2", "--new", "", "--expect", &s]);
        assert!(out.starts_with("ok a.txt: deleted lines 2-2; sha "), "{out}");
        assert_eq!(cat(&t, "a.txt"), "one\nthree\n");
    }

    // Checks line edit validation errors.
    #[test]
    fn test_line_edit_errors() {
        let t = TempDir::new();
        t.write("a.txt", b"one\ntwo\n");
        let s = sha(&t, "a.txt");
        assert!(edit(&t, &["a.txt", "--lines", "1-1", "--new", "x"])
            .starts_with("error: edits need --expect SHA (from read --source)"));
        assert_eq!(
            edit(&t, &["a.txt", "--lines", "1-9", "--new", "x", "--expect", &s]),
            "error: a.txt: lines 1-9 are outside the file (2 lines)"
        );
        assert_eq!(
            edit(&t, &["a.txt", "--lines", "0-1", "--new", "x", "--expect", &s]),
            "error: a.txt: lines 0-1 are outside the file (2 lines)"
        );
        assert_eq!(
            edit(&t, &["a.txt", "--lines", "2-1", "--new", "x", "--expect", &s]),
            "error: a.txt: lines 2-1 are outside the file (2 lines)"
        );
        let stale = edit(&t, &["a.txt", "--lines", "1-1", "--new", "x", "--expect", "deadbeef"]);
        assert_eq!(stale, "error: a.txt: the file is not the version you read; read it again with --source");
        assert!(!stale.contains(&s));
        assert!(edit(&t, &["a.txt", "--lines", "1-1", "--new", "x", "--expect", "de"])
            .contains("not the version you read"));
        assert!(edit(&t, &["a.txt", "--lines", "a-b", "--new", "x", "--expect", &s])
            .starts_with("error: bad line range 'a-b'"));
        assert!(edit(&t, &["a.txt", "--lines", "1-b", "--new", "x", "--expect", &s])
            .starts_with("error: bad line range"));
        assert_eq!(cat(&t, "a.txt"), "one\ntwo\n");
    }

    // Checks inserting after a line, at the start and at the end with and without a final newline.
    #[test]
    fn test_insert_after() {
        let t = TempDir::new();
        t.write("a.txt", b"one\ntwo\n");
        let s = sha(&t, "a.txt");
        let out = edit(&t, &["a.txt", "--after", "1", "--new", "mid\n", "--expect", &s]);
        assert!(out.starts_with("ok a.txt: inserted 1 line(s) after 1 (now 2-2); sha "), "{out}");
        assert_eq!(cat(&t, "a.txt"), "one\nmid\ntwo\n");
        let s = sha(&t, "a.txt");
        edit(&t, &["a.txt", "--after", "0", "--new", "top", "--expect", &s]);
        assert_eq!(cat(&t, "a.txt"), "top\none\nmid\ntwo\n");
        let s = sha(&t, "a.txt");
        edit(&t, &["a.txt", "--after", "4", "--new", "end1\nend2", "--expect", &s]);
        assert_eq!(cat(&t, "a.txt"), "top\none\nmid\ntwo\nend1\nend2\n");
        t.write("b.txt", b"x\ny");
        let s = sha(&t, "b.txt");
        edit(&t, &["b.txt", "--after", "2", "--new", "z", "--expect", &s]);
        assert_eq!(cat(&t, "b.txt"), "x\ny\nz");
        assert_eq!(
            edit(&t, &["b.txt", "--after", "9", "--new", "z", "--expect", "abcd"]),
            "error: b.txt: line 9 is past the end (3 lines)"
        );
        assert!(edit(&t, &["b.txt", "--after", "1", "--new", "z"]).starts_with("error: edits need --expect"));
    }

    // Checks CRLF files keep their line endings.
    #[test]
    fn test_crlf_is_preserved() {
        let t = TempDir::new();
        t.write("a.txt", b"one\r\ntwo\r\nthree\r\n");
        assert!(edit(&t, &["a.txt", "--old", "two\nthree", "--new", "2\n3"]).starts_with("ok"));
        assert_eq!(cat(&t, "a.txt"), "one\r\n2\r\n3\r\n");
        let s = sha(&t, "a.txt");
        edit(&t, &["a.txt", "--lines", "1", "--new", "uno", "--expect", &s]);
        assert_eq!(cat(&t, "a.txt"), "uno\r\n2\r\n3\r\n");
        let s = sha(&t, "a.txt");
        edit(&t, &["a.txt", "--after", "3", "--new", "four", "--expect", &s]);
        assert_eq!(cat(&t, "a.txt"), "uno\r\n2\r\n3\r\nfour\r\n");
    }

    // Checks stdin as the source of new text.
    #[test]
    fn test_new_text_from_stdin() {
        let t = TempDir::new();
        t.write("a.txt", b"old\n");
        let e = crate::engine::Engine::new();
        let ctx = Ctx {
            engine: &e,
            cwd: t.path().to_path_buf(),
            session: String::new(),
            stdin: std::cell::RefCell::new(Some(Box::new(&b"from stdin"[..]))),
        };
        let args = signed(t.path(), &["edit", "a.txt", "--old", "old", "--new", "-"]);
        assert!(crate::cmd::run(&ctx, &args).text.starts_with("ok a.txt"));
        assert_eq!(cat(&t, "a.txt"), "from stdin\n");
        let ctx2 = Ctx { stdin: std::cell::RefCell::new(Some(Box::new(&b"\xff"[..]))), ..ctx };
        let args = signed(t.path(), &["edit", "a.txt", "--old", "from", "--new", "-"]);
        assert_eq!(crate::cmd::run(&ctx2, &args).text, "error: standard input is not valid UTF-8");
        assert_eq!(edit(&t, &["a.txt", "--old", "from", "--new", "-"]), "error: no standard input available");
    }

    // Checks creating files, parents and the existing-file refusal.
    #[test]
    fn test_create_file() {
        let t = TempDir::new();
        let out = edit(&t, &["sub/dir/new.txt", "--create", "--new", "hello"]);
        assert!(out.starts_with("ok sub/dir/new.txt: created, 1 lines; sha "), "{out}");
        assert_eq!(cat(&t, "sub/dir/new.txt"), "hello\n");
        assert_eq!(
            edit(&t, &["sub/dir/new.txt", "--create", "--new", "again"]),
            "error: sub/dir/new.txt exists; edit it with --old/--lines"
        );
        assert!(
            edit(&t, &["empty.txt", "--create", "--new", ""]).starts_with("ok empty.txt: created, 0 lines")
        );
        assert_eq!(edit(&t, &["x.txt", "--create"]), "error: --create needs --new TEXT");
        t.write("blocker", b"x");
        assert!(edit(&t, &["blocker/inside.txt", "--create", "--new", "x"]).starts_with("error: "));
    }

    // Checks paths outside the working directory are refused, including via symlink and "..".
    #[test]
    fn test_confinement() {
        let outer = TempDir::new();
        outer.write("secret.txt", b"s\n");
        let work = outer.path().join("work");
        std::fs::create_dir_all(&work).expect("mkdir");
        std::fs::write(work.join("in.txt"), b"in\n").expect("write");
        std::os::unix::fs::symlink(outer.path().join("secret.txt"), work.join("link.txt")).expect("symlink");
        let run = |args: &[&str]| run_signed(&work, args).text;
        let refused = |s: String| s.starts_with("error: refusing to write outside ");
        assert!(refused(run(&["edit", "../secret.txt", "--old", "s", "--new", "x"])));
        assert!(refused(run(&["edit", "link.txt", "--old", "s", "--new", "x"])));
        assert!(refused(run(&[
            "edit",
            &outer.path().join("secret.txt").display().to_string(),
            "--old",
            "s",
            "--new",
            "x"
        ])));
        assert!(refused(run(&["edit", "../new.txt", "--create", "--new", "x"])));
        assert_eq!(
            run(&["edit", "gone/../../x.txt", "--create", "--new", "x"]),
            "error: path with .. through a missing directory"
        );
        assert_eq!(std::fs::read_to_string(outer.path().join("secret.txt")).expect("read"), "s\n");
        assert!(run(&["edit", "in.txt", "--old", "in", "--new", "out"]).starts_with("ok in.txt"));
        std::fs::create_dir(work.join("sub")).expect("mkdir sub");
        assert!(run(&["edit", "./sub/../in.txt", "--old", "out", "--new", "in"]).starts_with("ok"));
    }

    // Checks a symlink inside the directory to a file inside is edited in place on the real file.
    #[test]
    fn test_symlink_inside_is_followed() {
        let t = TempDir::new();
        t.write("real.txt", b"v1\n");
        std::os::unix::fs::symlink("real.txt", t.path().join("alias.txt")).expect("symlink");
        assert!(edit(&t, &["alias.txt", "--old", "v1", "--new", "v2"]).starts_with("ok"));
        assert_eq!(cat(&t, "real.txt"), "v2\n");
        assert!(t.path().join("alias.txt").is_symlink());
    }

    // Checks hard-linked files are rewritten in place so the links keep working.
    #[test]
    fn test_hard_links_are_kept() {
        let t = TempDir::new();
        t.write("a.txt", b"v1\n");
        std::fs::hard_link(t.path().join("a.txt"), t.path().join("b.txt")).expect("link");
        assert!(edit(&t, &["a.txt", "--old", "v1", "--new", "v2"]).starts_with("ok"));
        assert_eq!(cat(&t, "b.txt"), "v2\n");
    }

    // Checks file permissions survive an edit.
    #[test]
    fn test_permissions_are_kept() {
        use std::os::unix::fs::PermissionsExt;
        let t = TempDir::new();
        let p = t.write("run.sh", b"echo 1\n");
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        edit(&t, &["run.sh", "--old", "1", "--new", "2"]);
        assert_eq!(std::fs::metadata(&p).expect("meta").permissions().mode() & 0o777, 0o755);
        assert!(std::fs::read_dir(t.path()).expect("dir").all(|e| !e
            .expect("entry")
            .file_name()
            .to_string_lossy()
            .starts_with(".zazzle-edit")));
    }

    // Checks binary files, missing files and directories are refused.
    #[test]
    fn test_target_errors() {
        let t = TempDir::new();
        t.write("b.bin", b"a\0b");
        assert_eq!(edit(&t, &["b.bin", "--old", "a", "--new", "x"]), "error: b.bin: binary file");
        assert_eq!(
            edit(&t, &["nope.txt", "--old", "a", "--new", "x"]),
            "error: nope.txt: no such file or directory"
        );
        std::fs::create_dir(t.path().join("d")).expect("mkdir");
        assert_eq!(edit(&t, &["d", "--old", "a", "--new", "x"]), "error: d: not a regular file");
    }

    // Checks usage errors and missing --new.
    #[test]
    fn test_usage_errors() {
        let t = TempDir::new();
        t.write("a.txt", b"x\n");
        assert!(edit(&t, &[]).starts_with("error: usage: edit PATH"));
        assert!(edit(&t, &["a.txt"]).starts_with("error: usage: edit PATH"));
        assert!(edit(&t, &["a.txt", "b.txt", "--old", "x", "--new", "y"]).starts_with("error: usage:"));
        assert_eq!(edit(&t, &["a.txt", "--old", "x"]), "error: --old needs --new TEXT");
        assert_eq!(edit(&t, &["a.txt", "--lines", "1"]), "error: --lines needs --new TEXT");
        assert_eq!(edit(&t, &["a.txt", "--after", "1"]), "error: --after needs --new TEXT");
        assert!(edit(&t, &["a.txt", "--bogus"]).contains("unknown flag"));
        assert!(edit(&t, &["a.txt", "--after", "x", "--new", "y"]).contains("needs a number"));
    }

    // Checks a file that changes between loading and writing is rejected.
    #[test]
    fn test_concurrent_change_is_detected() {
        let t = TempDir::new();
        let p = t.write("a.txt", b"x\n");
        let e = crate::engine::Engine::new();
        let ctx = Ctx {
            engine: &e,
            cwd: t.path().to_path_buf(),
            session: String::new(),
            stdin: std::cell::RefCell::new(None),
        };
        let target = open_target(&ctx, "a.txt").expect("open");
        std::fs::write(&p, b"changed\n").expect("concurrent write");
        let err = commit(&ctx, &target, b"mine\n".to_vec()).unwrap_err();
        assert_eq!(err, "a.txt: file changed while editing; read it again");
        std::fs::remove_file(&p).expect("rm");
        assert_eq!(
            commit(&ctx, &target, b"mine\n".to_vec()).unwrap_err(),
            "a.txt: no such file or directory"
        );
    }

    // Checks write failures are reported and clean up their temporary file.
    #[test]
    fn test_write_failure_cleans_up() {
        use std::os::unix::fs::PermissionsExt;
        let t = TempDir::new();
        t.write("d/a.txt", b"x\n");
        let dir = t.path().join("d");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).expect("chmod");
        let writable = std::fs::File::create(dir.join("probe")).is_ok();
        let out = edit(&t, &["d/a.txt", "--old", "x", "--new", "y"]);
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).expect("chmod back");
        if !writable {
            assert!(out.starts_with("error: "), "{out}");
        }
        assert!(std::fs::read_dir(&dir).expect("dir").all(|e| !e
            .expect("entry")
            .file_name()
            .to_string_lossy()
            .starts_with(".zazzle-edit")));
        let engine = crate::engine::Engine::new();
        assert!(
            run_cmd_with(&engine, t.path(), "", &["edit", "d/a.txt", "--old", "q", "--new", "z"]).status != 0
        );
    }

    // Checks editing invalidates the cached view of the file.
    #[test]
    fn test_edit_updates_cache() {
        let t = TempDir::new();
        t.write("a.txt", b"one\n");
        let e = crate::engine::Engine::new();
        let r = |args: &[&str]| {
            let args = signed(t.path(), args);
            run_cmd_with(&e, t.path(), "", &args.iter().map(String::as_str).collect::<Vec<_>>()).text
        };
        assert!(r(&["read", "a.txt"]).contains("1 lines"));
        r(&["edit", "a.txt", "--old", "one", "--new", "one\ntwo"]);
        assert!(r(&["read", "a.txt"]).contains("2 lines"));
    }

    // Checks a text edit without the hash, or with a stale one, is refused without revealing the current hash.
    #[test]
    fn test_replace_text_needs_current_hash() {
        let t = TempDir::new();
        t.write("a.txt", b"one\ntwo\n");
        let now = sha(&t, "a.txt");
        let bare = run_cmd(t.path(), &["edit", "a.txt", "--old", "two", "--new", "2"]).text;
        assert!(bare.starts_with("error: edits need --expect SHA (from read --source)"), "{bare}");
        let stale =
            run_cmd(t.path(), &["edit", "a.txt", "--old", "two", "--new", "2", "--expect", "0000"]).text;
        assert_eq!(stale, "error: a.txt: the file is not the version you read; read it again with --source");
        assert!(!stale.contains(&now));
        assert_eq!(cat(&t, "a.txt"), "one\ntwo\n");
    }

    // Checks a whitespace-only old text gets an empty hint.
    #[test]
    fn test_replace_text_whitespace_only_hint() {
        let t = TempDir::new();
        t.write("a.txt", b"a\nb\n");
        assert_eq!(
            edit(&t, &["a.txt", "--old", "   ", "--new", "x"]),
            "error: a.txt: old text not found; no line contains ''"
        );
    }

    // Checks inserting into an empty file and creating a file whose text already ends in a newline.
    #[test]
    fn test_insert_into_empty_and_create_with_newline() {
        let t = TempDir::new();
        t.write("e.txt", b"");
        let s = sha(&t, "e.txt");
        assert!(edit(&t, &["e.txt", "--after", "0", "--new", "first", "--expect", &s])
            .starts_with("ok e.txt: inserted 1 line(s) after 0"));
        assert_eq!(cat(&t, "e.txt"), "first\n");
        assert!(edit(&t, &["n.txt", "--create", "--new", "ends\n"]).starts_with("ok n.txt: created, 1 lines"));
        assert_eq!(cat(&t, "n.txt"), "ends\n");
    }

    // Checks an unusable file name is reported as an ordinary error, not as an existing file.
    #[test]
    fn test_create_with_invalid_name() {
        let t = TempDir::new();
        let long = "x".repeat(300);
        let out = edit(&t, &[&long, "--create", "--new", "x"]);
        assert!(out.starts_with("error: ") && !out.contains("exists"), "{out}");
    }
}
