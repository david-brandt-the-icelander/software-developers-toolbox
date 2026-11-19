// The list and stat operations: ignore-aware directory listings over the daemon's walk, and single-path metadata.
use crate::engine::Ctx;
use crate::fsio;
use crate::host::{display, hidden, resolve};
use crate::req::{ListArgs, StatArgs};
use crate::state::{Res, State};
use serde_json::{json, Value};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

const MAX_ENTRIES: usize = 500;

// Returns the paths to list below a directory: files matching the globs at any depth, or files and directories down to the depth.
fn rows(ctx: &Ctx, a: &ListArgs, root: &Path) -> Res<Vec<(PathBuf, bool)>> {
    let roots = [root.to_path_buf()];
    let depth = a.depth.or(if a.glob.is_empty() { Some(1) } else { None });
    let mut rows: Vec<(PathBuf, bool)> =
        ctx.host.files(&roots, &a.glob, a.no_ignore)?.into_iter().map(|f| (f, false)).collect();
    if a.glob.is_empty() {
        rows.extend(ctx.host.dirs(root, a.no_ignore)?.into_iter().map(|d| (d, true)));
    }
    let deep = |p: &Path| p.strip_prefix(root).map_or(0, |r| r.components().count());
    rows.retain(|(p, _)| depth.is_none_or(|d| deep(p) <= d) && (a.hidden || !hidden(p, &roots)));
    Ok(rows)
}

// Builds the reply: one page of sorted entries, with sizes and times in long mode.
fn compute(ctx: &Ctx, a: &ListArgs) -> Res<Value> {
    let shown = a.path.clone().unwrap_or_else(|| ".".to_string());
    let root = resolve(ctx.cwd, &shown);
    let md = std::fs::metadata(&root).map_err(|e| format!("{shown}: {e}"))?;
    if !md.is_dir() {
        return Err(format!("{shown}: not a directory (use stat)"));
    }
    let mut rows: Vec<(String, PathBuf, bool)> =
        rows(ctx, a, &root)?.into_iter().map(|(p, dir)| (display(&p, ctx.cwd), p, dir)).collect();
    rows.sort_by(|x, y| x.0.cmp(&y.0));
    let max = a.max_entries.unwrap_or(MAX_ENTRIES);
    let total = rows.len();
    let shown: Vec<Value> =
        rows.iter().skip(a.cursor).take(max).map(|(name, p, dir)| render(name, p, *dir, a.long)).collect();
    let end = a.cursor + shown.len();
    let mut value = json!({"entries": shown, "total": total});
    if end < total {
        value["truncated"] = json!(true);
        value["next_cursor"] = json!(end);
    }
    Ok(value)
}

// Renders an entry as a path (directories end in /), or as [path, size, mtime] in long mode.
fn render(name: &str, path: &Path, dir: bool, long: bool) -> Value {
    let name = if dir { format!("{name}/") } else { name.to_string() };
    if !long {
        return json!(name);
    }
    let (size, mtime) = std::fs::symlink_metadata(path).map_or((0, 0), |m| (m.len(), m.mtime()));
    json!([name, size, mtime])
}

// Run a list request, answering unchanged when this session already has the same listing.
pub fn list(st: &mut State, ctx: &Ctx, a: ListArgs) -> Res<Value> {
    let value = compute(ctx, &a)?;
    let fresh = a.fresh;
    let keyed = ListArgs { fresh: false, ..a };
    Ok(st.serve(&format!("list|{}|{keyed:?}", ctx.cwd.display()), fresh, value))
}

// Run a stat request.
pub fn stat(ctx: &Ctx, a: StatArgs) -> Res<Value> {
    let full = resolve(ctx.cwd, &a.path);
    let path = full.as_path();
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
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::host::testing::Plain;
    use crate::state::{Sent, Shared};

    // Runs stat with absolute paths.
    fn st(path: &Path, hash: bool) -> Value {
        stat(
            &Ctx { cwd: Path::new("/"), host: &Plain },
            StatArgs { path: path.to_string_lossy().into_owned(), hash },
        )
        .unwrap()
    }

    // Stat reports absence, kind and a content etag.
    #[test]
    fn test_stat_kinds() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("f.txt");
        std::fs::write(&f, "hi").unwrap();
        assert_eq!(st(&dir.path().join("nope"), false)["exists"], false);
        let file = st(&f, true);
        assert_eq!((file["kind"].as_str(), file["size"].as_u64()), (Some("file"), Some(2)));
        assert_eq!(file["etag"], fsio::etag(b"hi"));
        assert_eq!(st(dir.path(), true)["kind"], "dir");
    }

    // Symlinks and device files get their own kinds, and hashing skips non-regular files.
    #[test]
    fn test_stat_symlink_and_other() {
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("l");
        std::os::unix::fs::symlink("/nonexistent", &link).unwrap();
        assert_eq!(st(&link, false)["kind"], "symlink");
        assert_eq!(st(Path::new("/dev/null"), false)["kind"], "other");
        let fifo = dir.path().join("fifo");
        let c = std::ffi::CString::new(fifo.to_string_lossy().as_bytes()).unwrap();
        // SAFETY: the path is a valid NUL-terminated string and mkfifo only creates a file.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        assert!(st(&fifo, true).get("etag").is_none());
    }

    // A file that cannot be read fails a hashing stat.
    #[test]
    fn test_stat_hash_unreadable() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("secret");
        std::fs::write(&f, "x").unwrap();
        std::fs::set_permissions(&f, std::os::unix::fs::PermissionsExt::from_mode(0o000)).unwrap();
        let ctx = Ctx { cwd: Path::new("/"), host: &Plain };
        let r = stat(&ctx, StatArgs { path: f.to_string_lossy().into_owned(), hash: true });
        assert!(r.is_err() || std::fs::read(&f).is_ok(), "root can read anything");
    }

    // Runs a list request in a directory with a fresh session.
    fn list_in(cwd: &Path, v: Value) -> Res<Value> {
        let (mut sh, mut sent) = (Shared::default(), Sent::default());
        let a: ListArgs = serde_json::from_value(v).unwrap();
        list(&mut State { shared: &mut sh, sent: &mut sent }, &Ctx { cwd, host: &Plain }, a)
    }

    // Create a small project tree with a hidden file.
    fn tree() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        for f in ["a.rs", "b.txt", "sub/c.rs", ".hidden"] {
            std::fs::write(dir.path().join(f), "x").unwrap();
        }
        dir
    }

    // A glob lists matching files at any depth and no directories.
    #[test]
    fn test_list_glob_walks_deep() {
        let dir = tree();
        let out = list_in(dir.path(), json!({"glob": ["*.rs"]})).unwrap();
        assert_eq!(out["entries"], json!(["a.rs", "sub/c.rs"]));
        let shallow = list_in(dir.path(), json!({"glob": ["*.rs"], "depth": 1})).unwrap();
        assert_eq!(shallow["entries"], json!(["a.rs"]));
    }

    // Plain listings are one level deep with directories marked, long mode adds sizes, hidden adds dot files.
    #[test]
    fn test_list_default_depth_long_and_hidden() {
        let dir = tree();
        let out = list_in(dir.path(), json!({"path": "."})).unwrap();
        assert_eq!(out["entries"], json!(["a.rs", "b.txt", "sub/"]));
        let long = list_in(dir.path(), json!({"long": true})).unwrap();
        assert_eq!(long["entries"][0][1], 1);
        let all = list_in(dir.path(), json!({"hidden": true, "depth": 2})).unwrap();
        assert_eq!(all["total"], 5);
        let sub = list_in(dir.path(), json!({"path": "sub"})).unwrap();
        assert_eq!(sub["entries"], json!(["sub/c.rs"]));
    }

    // Paging reports where to continue and the last page has no cursor.
    #[test]
    fn test_list_paging() {
        let dir = tree();
        let first = list_in(dir.path(), json!({"max_entries": 2})).unwrap();
        assert_eq!((first["truncated"].as_bool(), first["next_cursor"].as_u64()), (Some(true), Some(2)));
        let rest = list_in(dir.path(), json!({"max_entries": 2, "cursor": 2})).unwrap();
        assert_eq!(rest["entries"].as_array().unwrap().len(), 1);
        assert!(rest.get("truncated").is_none());
    }

    // A missing root and a file root are errors, and a vanished entry lists with zero size.
    #[test]
    fn test_list_errors_and_render() {
        let dir = tree();
        assert!(list_in(dir.path(), json!({"path": "nope"})).is_err());
        assert!(list_in(dir.path(), json!({"path": "a.rs"})).unwrap_err().contains("not a directory"));
        assert_eq!(render("g", &dir.path().join("gone"), true, true), json!(["g/", 0, 0]));
    }
}
