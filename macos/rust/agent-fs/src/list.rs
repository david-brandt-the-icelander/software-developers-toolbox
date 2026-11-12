// The list and stat operations: ignore-aware directory listings and single-path metadata.
use crate::fsio::{self, Ident};
use crate::req::{ListArgs, StatArgs};
use crate::state::{Dep, Res, State};
use crate::walk;
use serde_json::{json, Value};
use std::os::unix::fs::MetadataExt;
use std::path::Path;

const MAX_ENTRIES: usize = 500;

// One listed path with its type, size and modification time.
struct Row {
    path: String,
    dir: bool,
    size: u64,
    mtime: i64,
}

// Walk the root and build the reply with its dependencies.
fn compute(a: &ListArgs) -> Res<(Value, Vec<Dep>)> {
    let root = a.path.clone().unwrap_or_else(|| ".".to_string());
    let depth = a.depth.or(if a.glob.is_empty() { Some(1) } else { None });
    let walker = walk::builder(&[root], &a.glob, a.hidden, a.no_ignore, depth)?;
    let (mut deps, mut rows) = (vec![], vec![]);
    for entry in walker.build().flatten() {
        let Ok(md) = entry.metadata() else { continue };
        deps.push((entry.path().to_path_buf(), Some(Ident::of(&md))));
        if md.is_dir() {
            deps.extend(walk::ignore_file_deps(entry.path()));
        }
        if entry.depth() == 0 || (md.is_dir() && !a.glob.is_empty()) {
            continue;
        }
        rows.push(Row {
            path: walk::display(entry.path()),
            dir: md.is_dir(),
            size: md.len(),
            mtime: md.mtime(),
        });
    }
    rows.sort_by(|x, y| x.path.cmp(&y.path));
    let max = a.max_entries.unwrap_or(MAX_ENTRIES);
    let total = rows.len();
    let shown: Vec<Value> = rows.iter().skip(a.cursor).take(max).map(|r| render(r, a.long)).collect();
    let end = a.cursor + shown.len();
    let mut value = json!({"entries": shown, "total": total});
    if end < total {
        value["truncated"] = json!(true);
        value["next_cursor"] = json!(end);
    }
    Ok((value, deps))
}

// Render a row as a path string, or as [path, size, mtime] in long mode.
fn render(row: &Row, long: bool) -> Value {
    let name = if row.dir { format!("{}/", row.path) } else { row.path.clone() };
    if long {
        json!([name, row.size, row.mtime])
    } else {
        json!(name)
    }
}

// Run a list request through the session cache.
pub fn list(st: &mut State, a: ListArgs) -> Res<Value> {
    let mut keyed = a.clone();
    keyed.fresh = false;
    st.serve_cached(&format!("list|{keyed:?}"), a.fresh, || compute(&a))
}

// Run a stat request.
pub fn stat(a: StatArgs) -> Res<Value> {
    let path = Path::new(&a.path);
    let Ok(md) = std::fs::symlink_metadata(path) else {
        return Ok(json!({"path": a.path, "exists": false}));
    };
    let kind = match md.file_type() {
        t if t.is_symlink() => "symlink",
        t if t.is_dir() => "dir",
        t if t.is_file() => "file",
        _ => "other",
    };
    let mut out =
        json!({"path": a.path, "exists": true, "kind": kind, "size": md.len(), "mtime": md.mtime()});
    if a.hash && md.is_file() {
        let (_, bytes) = fsio::load(path).map_err(|e| format!("{}: {e}", a.path))?;
        out["etag"] = json!(fsio::etag(&bytes));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Stat reports absence, kind and a content etag.
    #[test]
    fn test_stat_kinds() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("f.txt");
        std::fs::write(&f, "hi").unwrap();
        let p = |p: &Path| p.to_string_lossy().into_owned();
        let missing = stat(StatArgs { path: p(&dir.path().join("nope")), hash: false }).unwrap();
        assert_eq!(missing["exists"], false);
        let file = stat(StatArgs { path: p(&f), hash: true }).unwrap();
        assert_eq!((file["kind"].as_str(), file["size"].as_u64()), (Some("file"), Some(2)));
        assert_eq!(file["etag"], fsio::etag(b"hi"));
        assert_eq!(stat(StatArgs { path: p(dir.path()), hash: true }).unwrap()["kind"], "dir");
    }

    // Directories get a trailing slash in plain listings.
    #[test]
    fn test_render_dir_slash() {
        let row = Row { path: "src".into(), dir: true, size: 0, mtime: 0 };
        assert_eq!(render(&row, false), "src/");
        assert_eq!(render(&row, true)[1], 0);
    }
}
