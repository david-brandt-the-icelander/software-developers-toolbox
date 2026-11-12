// Line diffs between two versions of a file, and the diff_since operation.
use crate::fsio;
use crate::req::DiffArgs;
use crate::state::{Res, State};
use serde_json::{json, Value};
use similar::{DiffTag, TextDiff};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

const DIFF_TIMEOUT_MS: u64 = 250;

// Return hunks that turn `old` into `new`, or None when either side is not UTF-8.
pub fn hunks(old: &[u8], new: &[u8]) -> Option<Vec<Value>> {
    let (old, new) = (std::str::from_utf8(old).ok()?, std::str::from_utf8(new).ok()?);
    let diff = TextDiff::configure().timeout(Duration::from_millis(DIFF_TIMEOUT_MS)).diff_lines(old, new);
    let new_lines = diff.new_slices();
    let mut out = Vec::new();
    for op in diff.ops().iter().filter(|op| op.tag() != DiffTag::Equal) {
        let (o, n) = (op.old_range(), op.new_range());
        let mut hunk = json!({"at": o.start + 1, "del": o.len(), "new_at": n.start + 1});
        if !n.is_empty() {
            hunk["ins"] = Value::String(new_lines[n].concat());
        }
        out.push(hunk);
    }
    Some(out)
}

// Run a diff_since request against the stored version with the given etag.
pub fn since(st: &mut State, a: DiffArgs) -> Res<Value> {
    let base = st.content(&a.etag).ok_or("base_unavailable: that version is not held; use read")?;
    let path = Path::new(&a.path);
    let (id, bytes) = fsio::load(path).map_err(|e| format!("{}: {e}", a.path))?;
    let etag = st.record(path, id, &bytes);
    if etag == a.etag {
        return Ok(json!({"path": a.path, "etag": etag, "unchanged": true}));
    }
    let hunks = hunks(&base, &bytes).ok_or("cannot diff non-UTF-8 content; use read")?;
    let total = bytes.iter().filter(|&&b| b == b'\n').count();
    st.put_content(&etag, Arc::new(bytes));
    let reference = st.mark_sent(&a.path, &etag, "diff", true);
    Ok(
        json!({"path": a.path, "base_etag": a.etag, "etag": etag, "newlines": total, "hunks": hunks, "ref": reference}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // A replaced middle line yields one hunk with the new text.
    #[test]
    fn test_hunks_replace_line() {
        let h = hunks(b"a\nb\nc\n", b"a\nX\nc\n").unwrap();
        assert_eq!(h.len(), 1);
        assert_eq!(h[0]["at"], 2);
        assert_eq!(h[0]["del"], 1);
        assert_eq!(h[0]["ins"], "X\n");
    }

    // Identical content gives no hunks and binary content gives None.
    #[test]
    fn test_hunks_equal_and_binary() {
        assert!(hunks(b"a\n", b"a\n").unwrap().is_empty());
        assert!(hunks(&[0xff, 0xfe], b"a").is_none());
    }

    // A pure deletion has no inserted text.
    #[test]
    fn test_hunks_pure_deletion() {
        let h = hunks(b"a\nb\n", b"a\n").unwrap();
        assert_eq!((h[0]["del"].as_u64(), h[0].get("ins")), (Some(1), None));
    }

    // diff_since reports unchanged, a diff and each failure.
    #[test]
    fn test_since_cases() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f.txt");
        std::fs::write(&p, "a\nb\n").unwrap();
        let path = p.to_string_lossy().into_owned();
        let mut st = State::new();
        let (id, bytes) = fsio::load(&p).unwrap();
        let etag = st.record(&p, id, &bytes);
        st.put_content(&etag, Arc::new(bytes));
        let ask =
            |st: &mut State, etag: &str| since(st, DiffArgs { path: path.clone(), etag: etag.to_string() });
        assert_eq!(ask(&mut st, &etag).unwrap()["unchanged"], true);
        std::fs::write(&p, "a\nB\n").unwrap();
        assert_eq!(ask(&mut st, &etag).unwrap()["hunks"][0]["ins"], "B\n");
        assert!(ask(&mut st, "b3:unknown").unwrap_err().contains("base_unavailable"));
        std::fs::write(&p, [0xff, 0xfe, b'\n']).unwrap();
        assert!(ask(&mut st, &etag).unwrap_err().contains("non-UTF-8"));
        std::fs::remove_file(&p).unwrap();
        assert!(ask(&mut st, &etag).unwrap_err().contains("f.txt"));
    }
}
