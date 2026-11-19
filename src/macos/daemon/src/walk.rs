// Directory walking: ignore-file aware scans, glob filters and a stat-validated listing cache.
use ignore::overrides::{Override, OverrideBuilder};
use ignore::{Match, WalkBuilder, WalkState};
use std::collections::HashMap;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_LISTINGS: usize = 8;
const VALIDATE_THREADS: usize = 4;
const VALIDATE_MIN_CHUNK: usize = 128;
const RACY_NS: i64 = 2_000_000_000;
const IGNORE_FILES: [&str; 2] = [".gitignore", ".ignore"];

// Options controlling which files are returned.
#[derive(Default)]
pub struct WalkOpts {
    pub no_ignore: bool,
    pub globs: Vec<String>,
}

// What stat says about a directory or ignore file: modification time and size, or None if absent.
type Stamp = Option<(i64, u64)>;

// A scanned tree: its files and the stamps that prove the list is still current.
pub struct Listing {
    files: Vec<PathBuf>,
    dirs: Vec<(PathBuf, Stamp)>,
    ignores: Vec<(PathBuf, Stamp)>,
    racy: bool,
}

// In-memory cache of listings keyed by root and ignore mode, validated by stat on every use.
#[derive(Default)]
pub struct WalkCache {
    map: Mutex<HashMap<(PathBuf, bool), Arc<Listing>>>,
    pub scans: AtomicU64,
    pub reuses: AtomicU64,
}

// Gives a short human reason for an I/O error.
pub fn reason(e: &std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::NotFound => "no such file or directory".into(),
        std::io::ErrorKind::PermissionDenied => "permission denied".into(),
        _ => e.to_string(),
    }
}

// Returns the stamp of a path: its mtime in nanoseconds and size, or None if it cannot be read.
fn stamp(path: &Path) -> Stamp {
    let md = std::fs::metadata(path).ok()?;
    Some((md.mtime() * 1_000_000_000 + md.mtime_nsec(), md.size()))
}

// Returns the current time in nanoseconds since the epoch.
fn now_ns() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos() as i64)
}

// Lists the ignore files that can affect a directory: its own and, for roots, its ancestors' and git exclude.
fn ignore_paths(dir: &Path, with_exclude: bool) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = IGNORE_FILES.iter().map(|n| dir.join(n)).collect();
    if with_exclude {
        v.push(dir.join(".git/info/exclude"));
    }
    v
}

// Stamps the ignore files that matter: those present inside the tree (a new one changes its directory's mtime)
// and every candidate above the root, present or not.
fn ignore_stamps(root: &Path, dirs: &[(PathBuf, Stamp)]) -> Vec<(PathBuf, Stamp)> {
    let inside =
        dirs.iter().flat_map(|(d, _)| ignore_paths(d, false)).filter_map(|p| stamp(&p).map(|s| (p, Some(s))));
    let above = root.ancestors().flat_map(|a| ignore_paths(a, true)).map(|p| {
        let s = stamp(&p);
        (p, s)
    });
    inside.chain(above).collect()
}

// One item found by the parallel scan.
enum Found {
    File(PathBuf),
    Dir(PathBuf, Stamp),
}

// Classifies a walked entry as a file or a stamped directory; symlinks and special files are dropped.
fn classify(entry: ignore::DirEntry) -> Option<Found> {
    let t = entry.file_type()?;
    if t.is_file() {
        return Some(Found::File(entry.into_path()));
    }
    if t.is_dir() {
        let st = stamp(entry.path());
        return Some(Found::Dir(entry.into_path(), st));
    }
    None
}

// Scans a directory tree in parallel, honouring ignore files and skipping .git.
fn scan(root: &Path, no_ignore: bool) -> Listing {
    let mut b = WalkBuilder::new(root);
    b.hidden(false).require_git(false).git_global(false).filter_entry(|e| e.file_name() != ".git");
    if no_ignore {
        b.ignore(false).git_ignore(false).git_exclude(false).parents(false);
    }
    let (tx, rx) = std::sync::mpsc::channel();
    b.build_parallel().run(|| {
        let tx = tx.clone();
        Box::new(move |res| {
            res.into_iter().filter_map(classify).for_each(|f| {
                let _ = tx.send(f);
            });
            WalkState::Continue
        })
    });
    drop(tx);
    let (mut files, mut dirs) = (Vec::new(), Vec::new());
    for f in rx {
        match f {
            Found::File(p) => files.push(p),
            Found::Dir(p, s) => dirs.push((p, s)),
        }
    }
    files.sort();
    let ignores = if no_ignore { Vec::new() } else { ignore_stamps(root, &dirs) };
    let now = now_ns();
    let racy = dirs.iter().chain(ignores.iter()).any(|(_, s)| s.is_some_and(|(m, _)| now - m < RACY_NS));
    Listing { files, dirs, ignores, racy }
}

impl Listing {
    // Returns true if every directory and ignore file still has the stamp recorded at scan time.
    fn valid(&self) -> bool {
        if self.racy {
            return false;
        }
        let all: Vec<&(PathBuf, Stamp)> = self.dirs.iter().chain(self.ignores.iter()).collect();
        let chunk = all.len().div_ceil(VALIDATE_THREADS).max(VALIDATE_MIN_CHUNK);
        let parts: Vec<&[&(PathBuf, Stamp)]> = all.chunks(chunk).collect();
        crate::pool::par_map(&parts, VALIDATE_THREADS, |_, part| part.iter().all(|(p, st)| stamp(p) == *st))
            .into_iter()
            .all(|ok| ok)
    }
}

impl WalkCache {
    // Returns the listing for a root, rescanning only when the cached one is missing or stale.
    fn listing(&self, root: &Path, no_ignore: bool) -> Arc<Listing> {
        let key = (root.to_path_buf(), no_ignore);
        let hit = self.map.lock().expect("walk cache lock").get(&key).filter(|l| l.valid()).cloned();
        if let Some(l) = hit {
            self.reuses.fetch_add(1, Ordering::Relaxed);
            return l;
        }
        self.scans.fetch_add(1, Ordering::Relaxed);
        let fresh = Arc::new(scan(root, no_ignore));
        let mut map = self.map.lock().expect("walk cache lock");
        if map.len() >= MAX_LISTINGS {
            map.clear();
        }
        map.insert(key, Arc::clone(&fresh));
        fresh
    }
}

// Builds the glob override for a root, or None when there are no globs.
fn glob_filter(root: &Path, globs: &[String]) -> Result<Option<Override>, String> {
    if globs.is_empty() {
        return Ok(None);
    }
    let mut ob = OverrideBuilder::new(root);
    for g in globs {
        ob.add(g).map_err(|e| format!("bad glob '{g}': {e}"))?;
    }
    ob.build().map(Some).map_err(|e| format!("bad glob: {e}"))
}

// Collects regular files under the roots, sorted, with ignore rules and globs applied.
pub fn collect(roots: &[PathBuf], opts: &WalkOpts, cache: &WalkCache) -> Result<Vec<PathBuf>, String> {
    let mut out = Vec::new();
    for root in roots {
        let meta =
            std::fs::symlink_metadata(root).map_err(|e| format!("{}: {}", root.display(), reason(&e)))?;
        if meta.is_file() || (meta.file_type().is_symlink() && root.is_file()) {
            out.push(root.clone());
            continue;
        }
        let filter = glob_filter(root, &opts.globs)?;
        let listing = cache.listing(root, opts.no_ignore);
        out.extend(
            listing
                .files
                .iter()
                .filter(|f| filter.as_ref().is_none_or(|o| !matches!(o.matched(f, false), Match::Ignore(_))))
                .cloned(),
        );
    }
    if roots.len() > 1 {
        out.sort();
        out.dedup();
    }
    Ok(out)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::testutil::{age, TempDir};

    // Builds a small tree with an ignored file and a .git directory, all backdated so caching applies.
    fn tree() -> TempDir {
        let t = TempDir::new();
        t.write("a.rs", b"x");
        t.write("sub/b.md", b"y");
        t.write(".hidden/c.txt", b"z");
        t.write("skip.log", b"l");
        t.write(".gitignore", b"*.log\n");
        t.write(".git/config", b"g");
        std::os::unix::fs::symlink("a.rs", t.path().join("ln")).expect("symlink");
        age_all(t.path());
        t
    }

    // Backdates every file and directory under a path.
    fn age_all(root: &Path) {
        for e in std::fs::read_dir(root).expect("read_dir").flatten() {
            if e.path().is_dir() {
                age_all(&e.path());
            }
            age(&e.path());
        }
        age(root);
    }

    // Lists file names relative to a root for assertions.
    fn names(files: &[PathBuf], root: &Path) -> Vec<String> {
        files
            .iter()
            .map(|f| f.strip_prefix(root).expect("under root").to_string_lossy().into_owned())
            .collect()
    }

    // Collects with a fresh cache.
    fn collect_fresh(roots: &[PathBuf], opts: &WalkOpts) -> Result<Vec<PathBuf>, String> {
        collect(roots, opts, &WalkCache::default())
    }

    // Checks defaults: hidden files kept, ignored files and .git skipped.
    #[test]
    fn test_collect_defaults() {
        let t = tree();
        let files = collect_fresh(&[t.path().to_path_buf()], &WalkOpts::default()).expect("walk");
        assert_eq!(names(&files, t.path()), vec![".gitignore", ".hidden/c.txt", "a.rs", "sub/b.md"]);
    }

    // Checks that no_ignore also returns ignored files.
    #[test]
    fn test_collect_no_ignore() {
        let t = tree();
        let opts = WalkOpts { no_ignore: true, globs: vec![] };
        let files = collect_fresh(&[t.path().to_path_buf()], &opts).expect("walk");
        assert!(names(&files, t.path()).contains(&"skip.log".to_string()));
    }

    // Checks glob include, exclude and bad-glob handling.
    #[test]
    fn test_collect_globs() {
        let t = tree();
        let root = [t.path().to_path_buf()];
        let inc = WalkOpts { no_ignore: false, globs: vec!["*.md".into()] };
        assert_eq!(names(&collect_fresh(&root, &inc).expect("walk"), t.path()), vec!["sub/b.md"]);
        let exc = WalkOpts { no_ignore: false, globs: vec!["!*.md".into()] };
        assert!(
            !names(&collect_fresh(&root, &exc).expect("walk"), t.path()).contains(&"sub/b.md".to_string())
        );
        let bad = WalkOpts { no_ignore: false, globs: vec!["[".into()] };
        assert!(collect_fresh(&root, &bad).unwrap_err().starts_with("bad glob"));
    }

    // Checks that a file root is returned directly and a missing root errors.
    #[test]
    fn test_collect_file_and_missing_roots() {
        let t = tree();
        let f = t.path().join("a.rs");
        assert_eq!(collect_fresh(std::slice::from_ref(&f), &WalkOpts::default()).expect("walk"), vec![f]);
        let err = collect_fresh(&[t.path().join("nope")], &WalkOpts::default()).unwrap_err();
        assert!(err.ends_with("no such file or directory"));
    }

    // Checks a symlink to a file is returned as a file root and one to a directory is walked.
    #[test]
    fn test_collect_symlink_roots() {
        let t = tree();
        let link = t.path().join("link.rs");
        std::os::unix::fs::symlink(t.path().join("a.rs"), &link).expect("symlink");
        assert_eq!(
            collect_fresh(std::slice::from_ref(&link), &WalkOpts::default()).expect("walk"),
            vec![link]
        );
        let dlink = t.path().join("sublink");
        std::os::unix::fs::symlink(t.path().join("sub"), &dlink).expect("symlink");
        let files = collect_fresh(&[dlink], &WalkOpts::default()).expect("walk");
        assert!(files.iter().any(|f| f.ends_with("b.md")));
    }

    // Checks the error reason texts.
    #[test]
    fn test_reason_texts() {
        use std::io::{Error, ErrorKind};
        assert_eq!(reason(&Error::from(ErrorKind::NotFound)), "no such file or directory");
        assert_eq!(reason(&Error::from(ErrorKind::PermissionDenied)), "permission denied");
        assert_eq!(reason(&Error::other("boom")), "boom");
    }

    // Checks an unchanged tree is scanned once and then reused.
    #[test]
    fn test_listing_is_reused_while_unchanged() {
        let t = tree();
        let cache = WalkCache::default();
        let root = [t.path().to_path_buf()];
        let a = collect(&root, &WalkOpts::default(), &cache).expect("walk");
        let b = collect(&root, &WalkOpts::default(), &cache).expect("walk");
        assert_eq!(a, b);
        assert_eq!((cache.scans.load(Ordering::Relaxed), cache.reuses.load(Ordering::Relaxed)), (1, 1));
    }

    // Checks a new file, in the root or a subdirectory, forces a rescan that sees it.
    #[test]
    fn test_new_files_invalidate_listing() {
        let t = tree();
        let cache = WalkCache::default();
        let root = [t.path().to_path_buf()];
        collect(&root, &WalkOpts::default(), &cache).expect("walk");
        t.write("new.rs", b"n");
        let after = collect(&root, &WalkOpts::default(), &cache).expect("walk");
        assert!(names(&after, t.path()).contains(&"new.rs".to_string()));
        age_all(t.path());
        collect(&root, &WalkOpts::default(), &cache).expect("walk");
        t.write("sub/deeper.rs", b"n");
        let after = collect(&root, &WalkOpts::default(), &cache).expect("walk");
        assert!(names(&after, t.path()).contains(&"sub/deeper.rs".to_string()));
    }

    // Checks deleting a file or directory forces a rescan.
    #[test]
    fn test_deletions_invalidate_listing() {
        let t = tree();
        let cache = WalkCache::default();
        let root = [t.path().to_path_buf()];
        collect(&root, &WalkOpts::default(), &cache).expect("walk");
        std::fs::remove_file(t.path().join("sub/b.md")).expect("rm");
        let after = collect(&root, &WalkOpts::default(), &cache).expect("walk");
        assert!(!names(&after, t.path()).contains(&"sub/b.md".to_string()));
        age_all(t.path());
        collect(&root, &WalkOpts::default(), &cache).expect("walk");
        std::fs::remove_dir_all(t.path().join(".hidden")).expect("rm -r");
        let after = collect(&root, &WalkOpts::default(), &cache).expect("walk");
        assert!(!names(&after, t.path()).contains(&".hidden/c.txt".to_string()));
    }

    // Checks editing an ignore file, in the tree or in an ancestor, forces a rescan.
    #[test]
    fn test_ignore_file_changes_invalidate_listing() {
        let t = tree();
        let cache = WalkCache::default();
        let root = [t.path().join("sub")];
        t.write("sub/.gitignore", b"b.md\n");
        age_all(t.path());
        let first = collect(&root, &WalkOpts::default(), &cache).expect("walk");
        assert!(!names(&first, &t.path().join("sub")).contains(&"b.md".to_string()));
        std::fs::write(t.path().join("sub/.gitignore"), b"nothing\n").expect("edit");
        let second = collect(&root, &WalkOpts::default(), &cache).expect("walk");
        assert!(names(&second, &t.path().join("sub")).contains(&"b.md".to_string()));
        age_all(t.path());
        collect(&root, &WalkOpts::default(), &cache).expect("walk");
        std::fs::write(t.path().join(".gitignore"), b"*.md\n").expect("edit ancestor");
        assert_eq!(cache.scans.load(Ordering::Relaxed), 3);
    }

    // Checks freshly modified directories are never trusted.
    #[test]
    fn test_racy_listing_is_rescanned() {
        let t = TempDir::new();
        t.write("a.rs", b"x");
        let cache = WalkCache::default();
        let root = [t.path().to_path_buf()];
        collect(&root, &WalkOpts::default(), &cache).expect("walk");
        collect(&root, &WalkOpts::default(), &cache).expect("walk");
        assert_eq!(cache.scans.load(Ordering::Relaxed), 2);
    }

    // Checks the cache holds a bounded number of listings.
    #[test]
    fn test_cache_is_bounded() {
        let t = tree();
        let cache = WalkCache::default();
        for i in 0..(MAX_LISTINGS + 2) {
            let d = t.path().join(format!("d{i}"));
            std::fs::create_dir_all(&d).expect("mkdir");
            collect(&[d], &WalkOpts::default(), &cache).expect("walk");
        }
        assert!(cache.map.lock().expect("lock").len() <= MAX_LISTINGS);
    }

    // Checks glob filtering works on cached listings too.
    #[test]
    fn test_globs_apply_to_cached_listing() {
        let t = tree();
        let cache = WalkCache::default();
        let root = [t.path().to_path_buf()];
        collect(&root, &WalkOpts::default(), &cache).expect("walk");
        let opts = WalkOpts { no_ignore: false, globs: vec!["*.rs".into()] };
        assert_eq!(names(&collect(&root, &opts, &cache).expect("walk"), t.path()), vec!["a.rs"]);
        assert_eq!(cache.reuses.load(Ordering::Relaxed), 1);
    }

    // Checks several roots give one sorted list without duplicates.
    #[test]
    fn test_multiple_roots_are_sorted_and_deduped() {
        let t = tree();
        let roots = [t.path().join("sub"), t.path().to_path_buf(), t.path().join("a.rs")];
        let files = collect_fresh(&roots, &WalkOpts::default()).expect("walk");
        let mut sorted = files.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(files, sorted);
        assert_eq!(files.iter().filter(|f| f.ends_with("b.md")).count(), 1);
    }
}
