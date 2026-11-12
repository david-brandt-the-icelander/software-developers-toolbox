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
    for (entry, md) in walker.build().flatten().filter_map(|e| e.metadata().ok().map(|m| (e, m))) {
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

    // Build list arguments from JSON.
    fn args(v: serde_json::Value) -> ListArgs {
        serde_json::from_value(v).unwrap()
    }

    // Create a small project tree.
    fn tree() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        for f in ["a.rs", "b.txt", "sub/c.rs"] {
            std::fs::write(dir.path().join(f), "x").unwrap();
        }
        dir
    }

    // A glob lists matching files at any depth and no directories.
    #[test]
    fn test_list_glob_walks_deep() {
        let dir = tree();
        let root = dir.path().to_string_lossy().into_owned();
        let out = list(&mut State::new(), args(json!({"path": root, "glob": ["*.rs"]}))).unwrap();
        let names: Vec<&str> =
            out["entries"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
        assert_eq!(names.len(), 2);
        assert!(names.iter().all(|n| n.ends_with(".rs")));
    }

    // Plain listings are one level deep with directories marked, and long mode adds sizes.
    #[test]
    fn test_list_default_depth_and_long() {
        let dir = tree();
        let root = dir.path().to_string_lossy().into_owned();
        let out = list(&mut State::new(), args(json!({"path": root}))).unwrap();
        assert_eq!(out["total"], 3);
        assert!(out["entries"].as_array().unwrap().iter().any(|v| v.as_str().unwrap().ends_with("sub/")));
        let long = list(&mut State::new(), args(json!({"path": root, "long": true}))).unwrap();
        assert_eq!(long["entries"][0][1], 1);
    }

    // Paging reports where to continue and the last page has no cursor.
    #[test]
    fn test_list_paging() {
        let dir = tree();
        let root = dir.path().to_string_lossy().into_owned();
        let first = list(&mut State::new(), args(json!({"path": root, "max_entries": 2}))).unwrap();
        assert_eq!((first["truncated"].as_bool(), first["next_cursor"].as_u64()), (Some(true), Some(2)));
        let rest =
            list(&mut State::new(), args(json!({"path": root, "max_entries": 2, "cursor": 2}))).unwrap();
        assert_eq!(rest["entries"].as_array().unwrap().len(), 1);
        assert!(rest.get("truncated").is_none());
    }

    // A missing root is an error.
    #[test]
    fn test_list_missing_root() {
        assert!(list(&mut State::new(), args(json!({"path": "/no/such/dir"}))).is_err());
    }

    // Symlinks and device files get their own kinds.
    #[test]
    fn test_stat_symlink_and_other() {
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("l");
        std::os::unix::fs::symlink("/nonexistent", &link).unwrap();
        let kind = |p: &str| stat(StatArgs { path: p.to_string(), hash: false }).unwrap()["kind"].clone();
        assert_eq!(kind(&link.to_string_lossy()), "symlink");
        assert_eq!(kind("/dev/null"), "other");
    }

    // Hashing adds an etag for regular files and not for other kinds.
    #[test]
    fn test_stat_hash_only_regular_files() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("f");
        std::fs::write(&f, "x").unwrap();
        let ok = stat(StatArgs { path: f.to_string_lossy().into_owned(), hash: true }).unwrap();
        assert!(ok["etag"].is_string());
        let fifo = dir.path().join("fifo");
        let c = std::ffi::CString::new(fifo.to_string_lossy().as_bytes()).unwrap();
        // SAFETY: the path is a valid NUL-terminated string and mkfifo only creates a file.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        let other = stat(StatArgs { path: fifo.to_string_lossy().into_owned(), hash: true }).unwrap();
        assert!(other.get("etag").is_none());
    }
}
