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
        if !text.is_empty() {
            out.chunks.push(json!({"start": a, "end": b, "text": text}));
        }
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
        assert!(s.chunks.is_empty() && s.next.is_empty());
    }

    // Overlong lines are cut and counted.
    #[test]
    fn test_clip_line_long() {
        let mut cuts = 0;
        let text = clip_line(&vec![b'x'; 2500], &mut cuts);
        assert_eq!(cuts, 1);
        assert!(text.ends_with("(+500 bytes)"));
    }
}
