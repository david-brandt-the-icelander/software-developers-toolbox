// The read operation: capped, ranged file reads with session-aware unchanged and diff replies.
use crate::diff;
use crate::fsio::{self, Ident};
use crate::req::ReadArgs;
use crate::state::{Res, State};
use memchr::{memchr, memchr_iter};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Arc;

const MAX_LINES: usize = 400;
const MAX_BYTES: usize = 16 * 1024;
const MAX_LINE_BYTES: usize = 2000;

// The lines selected for a reply, with what was left out.
struct Slice {
    chunks: Vec<Value>,
    next: Vec<[usize; 2]>,
    omitted: usize,
    cut_lines: usize,
}

// Count the lines in a buffer, counting a final unterminated line.
pub fn line_count(bytes: &[u8]) -> usize {
    match bytes.last() {
        None => 0,
        Some(&last) => memchr_iter(b'\n', bytes).count() + usize::from(last != b'\n'),
    }
}

// Return the byte offset where a 1-based line starts, or the length when past the end.
fn line_start(bytes: &[u8], line: usize) -> usize {
    if line <= 1 {
        return 0;
    }
    memchr_iter(b'\n', bytes).nth(line - 2).map_or(bytes.len(), |i| i + 1)
}

// Return a line as text, cutting very long lines and counting each cut.
fn clip_line(line: &[u8], cuts: &mut usize) -> String {
    if line.len() <= MAX_LINE_BYTES {
        return String::from_utf8_lossy(line).into_owned();
    }
    *cuts += 1;
    let head = String::from_utf8_lossy(&line[..MAX_LINE_BYTES]);
    format!("{head} …(+{} bytes)", line.len() - MAX_LINE_BYTES)
}

// Select lines from the spans until a line or byte cap is reached.
fn slice_lines(bytes: &[u8], spans: &[(usize, usize)], total: usize, caps: (usize, usize)) -> Slice {
    let mut out = Slice { chunks: vec![], next: vec![], omitted: 0, cut_lines: 0 };
    let (mut n_lines, mut n_bytes) = (0usize, 0usize);
    for (i, &(a, b)) in spans.iter().enumerate() {
        if a > total {
            continue;
        }
        let b = b.min(total);
        let mut pos = line_start(bytes, a);
        let (mut text, mut ln) = (String::new(), a);
        while ln <= b {
            let end = memchr(b'\n', &bytes[pos..]).map_or(bytes.len(), |k| pos + k);
            let shown = clip_line(&bytes[pos..end], &mut out.cut_lines);
            if n_lines >= caps.0 || (n_lines > 0 && n_bytes + shown.len() + 1 > caps.1) {
                if !text.is_empty() {
                    out.chunks.push(json!({"start": a, "end": ln - 1, "text": text}));
                }
                out.next.push([ln, b]);
                out.omitted = line_start(bytes, b + 1) - pos;
                for &(a2, b2) in &spans[i + 1..] {
                    let b2 = b2.min(total);
                    if a2 <= b2 {
                        out.next.push([a2, b2]);
                        out.omitted += line_start(bytes, b2 + 1) - line_start(bytes, a2);
                    }
                }
                return out;
            }
            text.push_str(&shown);
            text.push('\n');
            n_lines += 1;
            n_bytes += shown.len() + 1;
            pos = (end + 1).min(bytes.len());
            ln += 1;
        }
        out.chunks.push(json!({"start": a, "end": b, "text": text}));
    }
    out
}

// Load a file for reading with a path-prefixed error.
fn load(path: &Path, shown: &str) -> Res<(Ident, Vec<u8>)> {
    fsio::load(path).map_err(|e| format!("{shown}: {e}"))
}

// Reply with a diff against the version this session last received, when that is much smaller.
fn try_diff(st: &mut State, shown: &str, etag: &str, bytes: &Arc<Vec<u8>>, key: &str) -> Option<Value> {
    let base_etag = st.complete_etag(shown).filter(|e| e != etag)?;
    let base = st.content(&base_etag)?;
    let hunks = diff::hunks(&base, bytes)?;
    let size: usize = hunks.iter().map(|h| h["ins"].as_str().map_or(0, str::len) + 40).sum();
    if size * 2 >= bytes.len() {
        return None;
    }
    st.put_content(etag, Arc::clone(bytes));
    let reference = st.mark_sent(shown, etag, key, true);
    Some(json!({
        "path": shown, "etag": etag, "base_etag": base_etag, "changed": true,
        "total_lines": line_count(bytes), "hunks": hunks, "ref": reference
    }))
}

// Run a read request.
pub fn run(st: &mut State, a: ReadArgs) -> Res<Value> {
    let path = Path::new(&a.path);
    let md = std::fs::metadata(path).map_err(|e| format!("{}: {e}", a.path))?;
    if !md.is_file() {
        return Err(format!("{}: not a regular file (use list for directories)", a.path));
    }
    let caps = (a.max_lines.unwrap_or(MAX_LINES).max(1), a.max_bytes.unwrap_or(MAX_BYTES).max(1));
    let key = format!("{:?}|{}|{}", a.ranges, caps.0, caps.1);
    let mut loaded = None;
    let mut etag = match st.trusted_etag(path, &Ident::of(&md)) {
        Some(e) => e,
        None => {
            let (id, bytes) = load(path, &a.path)?;
            let e = st.record(path, id, &bytes);
            loaded = Some(bytes);
            e
        }
    };
    if a.if_none_match.as_deref() == Some(etag.as_str()) {
        return Ok(json!({"path": a.path, "etag": etag, "unchanged": true}));
    }
    if !a.fresh {
        if let Some(r) = st.already_sent(&a.path, &etag, &key) {
            return Ok(json!({"path": a.path, "etag": etag, "unchanged": true, "ref": r}));
        }
    }
    let bytes = match loaded {
        Some(b) => b,
        None => {
            let (id, b) = load(path, &a.path)?;
            etag = st.record(path, id, &b);
            b
        }
    };
    if fsio::is_binary(&bytes) {
        return Ok(json!({"path": a.path, "etag": etag, "binary": true, "size": bytes.len()}));
    }
    let bytes = Arc::new(bytes);
    if a.ranges.is_empty() && !a.fresh {
        if let Some(v) = try_diff(st, &a.path, &etag, &bytes, &key) {
            return Ok(v);
        }
    }
    let total = line_count(&bytes);
    let spans: Vec<(usize, usize)> = match a.ranges.is_empty() {
        true => vec![(1, total.max(1))],
        false => a.ranges.iter().map(|r| (r[0], r[1])).collect(),
    };
    if let Some(&(s, e)) = spans.iter().find(|&&(s, e)| s == 0 || e < s) {
        return Err(format!("bad range [{s},{e}]: lines are 1-based and start must be <= end"));
    }
    let slice = slice_lines(&bytes, &spans, total, caps);
    let complete = a.ranges.is_empty() && slice.next.is_empty();
    st.put_content(&etag, Arc::clone(&bytes));
    let reference = st.mark_sent(&a.path, &etag, &key, complete);
    let mut out = json!({
        "path": a.path, "etag": etag, "total_lines": total, "chunks": slice.chunks, "ref": reference
    });
    if !slice.next.is_empty() {
        out["truncated"] = json!(true);
        out["next_ranges"] = json!(slice.next);
        out["bytes_omitted"] = json!(slice.omitted);
    }
    if slice.cut_lines > 0 {
        out["long_lines_cut"] = json!(slice.cut_lines);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Lines are counted with or without a final newline.
    #[test]
    fn test_line_count_variants() {
        assert_eq!(line_count(b""), 0);
        assert_eq!(line_count(b"a\nb\n"), 2);
        assert_eq!(line_count(b"a\nb"), 2);
    }

    // The line cap truncates and reports where to continue.
    #[test]
    fn test_slice_lines_caps() {
        let data = b"1\n2\n3\n4\n5\n";
        let s = slice_lines(data, &[(1, 5)], 5, (2, 1000));
        assert_eq!(s.chunks[0]["text"], "1\n2\n");
        assert_eq!(s.next, vec![[3, 5]]);
        assert_eq!(s.omitted, 6);
    }

    // A range past the end of the file yields nothing and no truncation.
    #[test]
    fn test_slice_lines_past_end() {
        let s = slice_lines(b"a\n", &[(5, 9)], 1, (10, 1000));
        assert!(s.chunks.is_empty());
        assert!(s.next.is_empty());
    }

    // Overlong lines are cut and counted.
    #[test]
    fn test_clip_line_long() {
        let mut cuts = 0;
        let text = clip_line(&vec![b'x'; 2500], &mut cuts);
        assert_eq!(cuts, 1);
        assert!(text.ends_with("(+500 bytes)"));
    }

    // Build read arguments from JSON.
    fn args(v: serde_json::Value) -> ReadArgs {
        serde_json::from_value(v).unwrap()
    }

    // Write a file and return its path text with the directory guard.
    fn file(body: &[u8]) -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f.txt");
        std::fs::write(&p, body).unwrap();
        (dir, p.to_string_lossy().into_owned())
    }

    // The byte cap stops a chunk but a single oversized line is still shown.
    #[test]
    fn test_slice_lines_byte_cap() {
        let data = b"aaaa\naaaa\naaaa\naaaa\n";
        let s = slice_lines(data, &[(1, 4)], 4, (100, 12));
        assert_eq!((s.chunks[0]["end"].as_u64(), s.next.clone()), (Some(2), vec![[3, 4]]));
        let one = slice_lines(data, &[(1, 4)], 4, (100, 1));
        assert_eq!((one.chunks[0]["end"].as_u64(), one.next), (Some(1), vec![[2, 4]]));
    }

    // Stopping exactly at a span boundary emits no empty chunk and keeps later spans, skipping those past the end.
    #[test]
    fn test_slice_lines_span_boundaries() {
        let data = b"1\n2\n3\n4\n";
        let s = slice_lines(data, &[(1, 2), (3, 4)], 4, (2, 1000));
        assert_eq!((s.chunks.len(), s.next), (1, vec![[3, 4]]));
        let t = slice_lines(data, &[(1, 4), (100, 200)], 4, (1, 1000));
        assert_eq!((t.next, t.omitted), (vec![[2, 4]], 6));
        let u = slice_lines(data, &[(1, 2), (3, 3), (100, 200)], 4, (2, 1000));
        assert_eq!(u.next, vec![[3, 3]]);
        let v = slice_lines(data, &[(1, 4), (2, 3)], 4, (1, 1000));
        assert_eq!(v.next, vec![[2, 4], [2, 3]]);
    }

    // Reads fail on directories and missing files, and reject bad ranges.
    #[test]
    fn test_run_errors() {
        let (dir, path) = file(b"a\nb\n");
        let mut st = State::new();
        let on_dir = run(&mut st, args(json!({"path": dir.path()}))).unwrap_err();
        assert!(on_dir.contains("not a regular file"));
        assert!(run(&mut st, args(json!({"path": "/no/such/file"}))).is_err());
        assert!(run(&mut st, args(json!({"path": path, "ranges": [[3, 2]]}))).is_err());
    }

    // Binary files, long lines and byte caps are reported in the reply.
    #[test]
    fn test_run_binary_long_and_caps() {
        let (_d, bin) = file(b"ab\0cd");
        let mut st = State::new();
        assert_eq!(run(&mut st, args(json!({"path": bin}))).unwrap()["binary"], true);
        let long = format!("{}\nshort\n", "x".repeat(MAX_LINE_BYTES + 50));
        let (_d2, path) = file(long.as_bytes());
        let out = run(&mut st, args(json!({"path": path}))).unwrap();
        assert_eq!(out["long_lines_cut"], 1);
        let (_d3, many) = file("0123456789\n".repeat(100).as_bytes());
        let capped = run(&mut st, args(json!({"path": many, "max_bytes": 50}))).unwrap();
        assert_eq!(capped["truncated"], true);
        assert!(capped["bytes_omitted"].as_u64().unwrap() > 900);
    }

    // Once a file is old enough to trust its stamp, repeats are answered without reloading it.
    #[test]
    fn test_run_trusted_etag_path() {
        let (_d, path) = file("line\n".repeat(500).as_bytes());
        std::thread::sleep(std::time::Duration::from_millis(2200));
        let mut st = State::new();
        let first = run(&mut st, args(json!({"path": path, "max_lines": 100}))).unwrap();
        assert_eq!(first["truncated"], true);
        let same = run(&mut st, args(json!({"path": path, "max_lines": 100}))).unwrap();
        assert_eq!(same["unchanged"], true);
        let other = run(&mut st, args(json!({"path": path, "max_lines": 50}))).unwrap();
        assert!(other.get("chunks").is_some());
        let tag = run(&mut st, args(json!({"path": path, "if_none_match": first["etag"]}))).unwrap();
        assert_eq!(tag["unchanged"], true);
    }

    // A diff that is not smaller than the file falls back to full content.
    #[test]
    fn test_run_large_diff_falls_back() {
        let (_d, path) = file(b"one\ntwo\nthree\n");
        let mut st = State::new();
        run(&mut st, args(json!({"path": path}))).unwrap();
        std::fs::write(&path, "completely\ndifferent\ncontent\n").unwrap();
        let out = run(&mut st, args(json!({"path": path}))).unwrap();
        assert!(out.get("chunks").is_some());
        assert!(out.get("hunks").is_none());
    }
}
