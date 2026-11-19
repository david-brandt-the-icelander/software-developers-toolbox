// Lists the largest folders and files under a path by allocated size, read-only and without leaving its volume.
use std::collections::HashSet;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use serde_json::{json, Value};

use crate::util::{self, die, human_i};

// Options for the top command.
pub struct TopOpts {
    pub path: String,
    pub count: usize,
    pub depth: usize,
    pub json: bool,
}

// One folder or file at most `depth` levels below the root, with its allocated size.
struct Row {
    path: PathBuf,
    size: i64,
    dir: bool,
    depth: usize,
}

// What one walker found: rows within the depth, files counted, unreadable paths and other volumes left alone.
struct Walk<'a> {
    dev: u64,
    depth: usize,
    seen: &'a Mutex<HashSet<(u64, u64)>>,
    rows: Vec<Row>,
    files: u64,
    skipped: Vec<(PathBuf, String)>,
    other_volumes: Vec<PathBuf>,
}

impl<'a> Walk<'a> {
    // Start an empty walk of the volume `dev`, sharing the set of hard-linked files already counted.
    fn new(dev: u64, depth: usize, seen: &'a Mutex<HashSet<(u64, u64)>>) -> Self {
        Walk { dev, depth, seen, rows: Vec::new(), files: 0, skipped: Vec::new(), other_volumes: Vec::new() }
    }

    // Return the allocated bytes of one entry `level` levels below the root (its whole tree for a folder), recording
    // it as a row when within the depth; a hard-linked file counts once and another volume counts nothing.
    fn visit(&mut self, path: PathBuf, level: usize) -> i64 {
        let md = match fs::symlink_metadata(&path) {
            Ok(md) => md,
            Err(e) => {
                self.skipped.push((path, e.to_string()));
                return 0;
            }
        };
        let size = if md.is_dir() {
            if md.dev() != self.dev {
                self.other_volumes.push(path);
                return 0;
            }
            util::alloc(&md) + self.children(&path, level)
        } else {
            let first =
                md.nlink() < 2 || self.seen.lock().map_or(true, |mut s| s.insert((md.dev(), md.ino())));
            if !first {
                return 0;
            }
            self.files += 1;
            util::alloc(&md)
        };
        if level <= self.depth {
            self.rows.push(Row { path, size, dir: md.is_dir(), depth: level });
        }
        size
    }

    // Return the allocated bytes of everything inside a folder, noting it when it cannot be read.
    fn children(&mut self, dir: &Path, level: usize) -> i64 {
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(e) => {
                self.skipped.push((dir.to_path_buf(), e.to_string()));
                return 0;
            }
        };
        entries.flatten().map(|e| self.visit(e.path(), level + 1)).sum()
    }
}

// The finished walk of a root: its total, rows biggest first, and what was left out.
struct Found {
    root: PathBuf,
    total: i64,
    files: u64,
    rows: Vec<Row>,
    skipped: Vec<(PathBuf, String)>,
    other_volumes: Vec<PathBuf>,
}

// Walk a folder's entries on several threads and merge what they found, biggest rows first.
fn walk_root(root: &Path, depth: usize) -> Result<Found, String> {
    let md = fs::metadata(root).map_err(|e| format!("{}: {e}", root.display()))?;
    if !md.is_dir() {
        return Err(format!("{} is not a folder", root.display()));
    }
    let entries: Vec<PathBuf> = fs::read_dir(root)
        .map_err(|e| format!("{}: {e}", root.display()))?
        .flatten()
        .map(|e| e.path())
        .collect();
    let seen = Mutex::new(HashSet::new());
    let next = AtomicUsize::new(0);
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).min(entries.len().max(1));
    let walks: Vec<(i64, Walk)> = std::thread::scope(|s| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                s.spawn(|| {
                    let mut w = Walk::new(md.dev(), depth, &seen);
                    let mut total = 0;
                    while let Some(path) = entries.get(next.fetch_add(1, Ordering::Relaxed)) {
                        total += w.visit(path.clone(), 1);
                    }
                    (total, w)
                })
            })
            .collect();
        workers.into_iter().map(|h| h.join().expect("a walker thread panicked")).collect()
    });
    let mut found = Found {
        root: root.to_path_buf(),
        total: util::alloc(&md),
        files: 0,
        rows: Vec::new(),
        skipped: Vec::new(),
        other_volumes: Vec::new(),
    };
    for (total, w) in walks {
        found.total += total;
        found.files += w.files;
        found.rows.extend(w.rows);
        found.skipped.extend(w.skipped);
        found.other_volumes.extend(w.other_volumes);
    }
    found.rows.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.path.cmp(&b.path)));
    found.skipped.sort();
    found.other_volumes.sort();
    Ok(found)
}

// Show a row's path relative to the root, with a slash after folders.
fn shown(root: &Path, row: &Row) -> String {
    let rel = row.path.strip_prefix(root).unwrap_or(&row.path).display();
    if row.dir {
        format!("{rel}/")
    } else {
        rel.to_string()
    }
}

// Render the result as aligned lines: a header, one row per entry and notes on what was left out.
fn text(f: &Found, count: usize, depth: usize) -> String {
    let mut out = format!(
        "{} allocated under {} ({} files, depth {depth})\n",
        human_i(f.total),
        f.root.display(),
        f.files
    );
    for row in f.rows.iter().take(count) {
        out.push_str(&format!("{:>10}  {}\n", human_i(row.size), shown(&f.root, row)));
    }
    if let Some((path, err)) = f.skipped.first() {
        out.push_str(&format!(
            "skipped {} it could not read, e.g. {}: {err}\n",
            f.skipped.len(),
            path.display()
        ));
    }
    for path in &f.other_volumes {
        out.push_str(&format!("not counted (another volume): {}\n", path.display()));
    }
    out
}

// Render the result as a JSON object.
fn to_json(f: &Found, count: usize, depth: usize) -> Value {
    let rows: Vec<Value> = f
        .rows
        .iter()
        .take(count)
        .map(|r| json!({"path": r.path, "size": r.size, "kind": if r.dir { "dir" } else { "file" }, "depth": r.depth}))
        .collect();
    let skipped: Vec<Value> = f.skipped.iter().map(|(p, e)| json!({"path": p, "error": e})).collect();
    json!({
        "tool": "free-disk-space", "version": env!("CARGO_PKG_VERSION"), "root": f.root, "total": f.total,
        "files": f.files, "depth": depth, "entries": rows, "skipped": skipped, "other_volumes": f.other_volumes,
    })
}

// List the largest folders and files under a path.
pub fn cmd_top(o: &TopOpts) {
    let root = PathBuf::from(util::realpath(&o.path));
    let found = walk_root(&root, o.depth).unwrap_or_else(|e| die(&e));
    if o.json {
        println!("{}", to_json(&found, o.count, o.depth));
    } else {
        print!("{}", text(&found, o.count, o.depth));
    }
}

// Unit tests for the largest-entries listing.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{chmod, is_root, Sandbox};

    // Build a tree: a big folder two levels deep, a small file, a hard link pair and a symlink.
    fn tree(sb: &Sandbox) -> PathBuf {
        sb.write("t/big/deep/blob", &vec![7u8; 256 * 1024]);
        sb.write("t/small.txt", &[1u8; 5000]);
        let once = sb.write("t/linked/one", &vec![2u8; 64 * 1024]);
        fs::hard_link(&once, sb.path("t/linked/two")).unwrap();
        std::os::unix::fs::symlink("/etc", sb.path("t/link")).unwrap();
        sb.path("t")
    }

    // Check rows are biggest first, limited by depth, count hard links once and do not follow symlinks.
    #[test]
    fn test_walk_root_sizes_and_depth() {
        let sb = Sandbox::new();
        let root = tree(&sb);
        let f = walk_root(&root, 2).unwrap();
        assert_eq!(f.files, 4);
        assert_eq!(shown(&root, &f.rows[0]), "big/");
        assert!(f.rows.iter().all(|r| r.depth <= 2));
        assert!(f.rows.iter().any(|r| shown(&root, r) == "big/deep/"));
        assert!(!f.rows.iter().any(|r| shown(&root, r) == "big/deep/blob"));
        let linked = f.rows.iter().find(|r| shown(&root, r) == "linked/").unwrap();
        assert!(linked.size < 2 * 64 * 1024, "a hard link pair counts once");
        assert!(f.total >= f.rows[0].size);
        let deep = walk_root(&root, 3).unwrap();
        assert!(deep.rows.iter().any(|r| shown(&root, r) == "big/deep/blob"));
        assert!(walk_root(&sb.path("t/small.txt"), 1).err().unwrap().ends_with("is not a folder"));
        assert!(walk_root(&sb.path("missing"), 1).is_err());
        let empty = walk_root(&sb.dir("empty"), 1).unwrap();
        assert!(empty.rows.is_empty());
        assert_eq!(empty.files, 0);
    }

    // Check the text and JSON renderings, with notes on unreadable folders and other volumes.
    #[test]
    fn test_renderings() {
        let sb = Sandbox::new();
        let root = tree(&sb);
        let mut f = walk_root(&root, 3).unwrap();
        let out = text(&f, 2, 3);
        assert_eq!(out.lines().count(), 3, "{out}");
        assert!(out.lines().nth(1).unwrap().ends_with("  big/"));
        f.skipped.push((sb.path("t/locked"), "Permission denied".into()));
        f.other_volumes.push(sb.path("t/mnt"));
        let out = text(&f, 1, 3);
        assert!(out.contains("skipped 1 it could not read, e.g. "));
        assert!(out.contains("not counted (another volume)"));
        let j = to_json(&f, 2, 3);
        assert_eq!(j["tool"], "free-disk-space");
        assert_eq!(j["entries"].as_array().unwrap().len(), 2);
        assert_eq!(j["entries"][0]["kind"], "dir");
        let all = to_json(&f, 100, 3);
        assert!(all["entries"].as_array().unwrap().iter().any(|e| e["kind"] == "file"));
        assert_eq!(j["skipped"][0]["error"], "Permission denied");
        let file = f.rows.iter().find(|r| !r.dir).unwrap();
        assert!(!shown(&root, file).ends_with('/'));
    }

    // Check an unreadable folder is skipped with a note, a vanished entry is noted and another volume is not entered.
    #[test]
    fn test_walk_skips_and_volumes() {
        let sb = Sandbox::new();
        let root = tree(&sb);
        let seen = Mutex::new(HashSet::new());
        let mut w = Walk::new(fs::metadata(&root).unwrap().dev(), 3, &seen);
        assert_eq!(w.visit(sb.path("t/vanished"), 1), 0);
        assert_eq!(w.skipped.len(), 1);
        let mut other = Walk::new(u64::MAX, 3, &seen);
        assert_eq!(other.visit(sb.path("t/big"), 1), 0);
        assert_eq!(other.other_volumes, vec![sb.path("t/big")]);
        sb.write("t/locked/secret", b"x");
        chmod(&sb.path("t/locked"), 0o000);
        let f = walk_root(&root, 2).unwrap();
        chmod(&sb.path("t/locked"), 0o755);
        if !is_root() {
            assert_eq!(f.skipped.len(), 1);
            assert!(text(&f, 5, 2).contains("could not read"));
        }
    }

    // Check the command prints both forms and dies on a path that is not a folder.
    #[test]
    fn test_cmd_top() {
        let sb = Sandbox::new();
        let root = tree(&sb).to_string_lossy().into_owned();
        cmd_top(&TopOpts { path: root.clone(), count: 3, depth: 2, json: false });
        cmd_top(&TopOpts { path: root, count: 3, depth: 2, json: true });
        let file = sb.path("t/small.txt").to_string_lossy().into_owned();
        let bad =
            std::panic::catch_unwind(|| cmd_top(&TopOpts { path: file, count: 1, depth: 1, json: false }));
        assert!(bad.is_err());
    }
}
