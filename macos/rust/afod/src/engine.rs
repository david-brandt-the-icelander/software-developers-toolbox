// The engine: shared state behind every command, including the stat-validated metadata cache.
use crate::meta::{Meta, MAX_META_SIZE};
use std::collections::HashMap;
use std::fs::Metadata;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const MAX_ENTRIES: usize = 4096;
const RACY_NS: i128 = 2_000_000_000;

// The identity of a file's content as far as stat can tell.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Stat {
    dev: u64,
    ino: u64,
    size: u64,
    mtime_ns: i128,
}

impl Stat {
    // Reads the identifying fields from file metadata.
    fn of(md: &Metadata) -> Stat {
        Stat {
            dev: md.dev(),
            ino: md.ino(),
            size: md.size(),
            mtime_ns: i128::from(md.mtime()) * 1_000_000_000 + i128::from(md.mtime_nsec()),
        }
    }
}

// A cached metadata record and whether it was computed too close to the file's mtime to trust.
struct Entry {
    stat: Stat,
    meta: Arc<Meta>,
    racy: bool,
}

// File content together with its metadata.
pub struct Loaded {
    pub data: Vec<u8>,
    pub meta: Arc<Meta>,
}

// Shared daemon state.
pub struct Engine {
    pub started: Instant,
    cache: Mutex<HashMap<PathBuf, Entry>>,
    pub hits: AtomicU64,
    pub misses: AtomicU64,
}

impl Default for Engine {
    // Creates an engine with an empty cache.
    fn default() -> Engine {
        Engine::new()
    }
}

// Returns the current time in nanoseconds since the epoch.
fn now_ns() -> i128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos() as i128)
}

impl Engine {
    // Creates an engine with an empty cache.
    pub fn new() -> Engine {
        Engine {
            started: Instant::now(),
            cache: Mutex::new(HashMap::new()),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        }
    }

    // Returns the cached metadata when the stat still matches and the entry is trustworthy.
    fn cached(&self, path: &Path, stat: Stat) -> Option<Arc<Meta>> {
        let cache = self.cache.lock().expect("cache lock");
        let e = cache.get(path)?;
        (e.stat == stat && !e.racy).then(|| Arc::clone(&e.meta))
    }

    // Stores metadata for a path, clearing the cache first if it is full.
    fn store(&self, path: &Path, stat: Stat, meta: &Arc<Meta>) {
        let mut cache = self.cache.lock().expect("cache lock");
        if cache.len() >= MAX_ENTRIES {
            cache.clear();
        }
        let racy = now_ns() - stat.mtime_ns < RACY_NS;
        cache.insert(path.to_path_buf(), Entry { stat, meta: Arc::clone(meta), racy });
    }

    // Drops any cached record for a path, for example after an edit.
    pub fn forget(&self, path: &Path) {
        self.cache.lock().expect("cache lock").remove(path);
    }

    // Returns the number of cached records.
    pub fn cached_entries(&self) -> usize {
        self.cache.lock().expect("cache lock").len()
    }

    // Stats a path and checks it is a readable regular file of supported size.
    fn stat_file(path: &Path) -> Result<Stat, String> {
        let md = std::fs::metadata(path).map_err(|e| crate::walk::reason(&e))?;
        if !md.is_file() {
            return Err("not a regular file".into());
        }
        if md.len() > MAX_META_SIZE {
            return Err(format!("file too large ({} MB)", md.len() >> 20));
        }
        Ok(Stat::of(&md))
    }

    // Reads a file and its metadata, using the cache when the stat is unchanged.
    pub fn load(&self, path: &Path) -> Result<Loaded, String> {
        self.load_with(path, &Engine::stat_file)
    }

    // Loads a file using the given stat function, which tests replace to simulate concurrent changes.
    fn load_with(&self, path: &Path, stat: &dyn Fn(&Path) -> Result<Stat, String>) -> Result<Loaded, String> {
        let st = stat(path)?;
        let cached = self.cached(path, st);
        let data = std::fs::read(path).map_err(|e| crate::walk::reason(&e))?;
        let st_after = stat(path)?;
        if let Some(meta) = cached.filter(|_| st_after == st) {
            self.hits.fetch_add(1, Ordering::Relaxed);
            return Ok(Loaded { data, meta });
        }
        self.misses.fetch_add(1, Ordering::Relaxed);
        let meta = Arc::new(Meta::compute(&data));
        if st_after == st {
            self.store(path, st, &meta);
        }
        Ok(Loaded { data, meta })
    }

    // Returns a file's metadata, reading the file only on a cache miss.
    pub fn meta(&self, path: &Path) -> Result<Arc<Meta>, String> {
        let st = Engine::stat_file(path)?;
        if let Some(m) = self.cached(path, st) {
            self.hits.fetch_add(1, Ordering::Relaxed);
            return Ok(m);
        }
        Ok(self.load(path)?.meta)
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::testutil::TempDir;
    use std::cell::Cell;
    use std::time::Duration;

    // Backdates a file's mtime so the cache will trust it.
    fn age(path: &Path) {
        let f = std::fs::OpenOptions::new().write(true).open(path).expect("open");
        f.set_modified(SystemTime::now() - Duration::from_secs(3600)).expect("set mtime");
    }

    // Checks a miss then a hit for an old file, and that the data matches.
    #[test]
    fn test_load_caches_old_files() {
        let t = TempDir::new();
        let p = t.write("a.txt", b"one\ntwo\n");
        age(&p);
        let e = Engine::new();
        let a = e.load(&p).expect("load");
        let b = e.load(&p).expect("load");
        assert_eq!(a.data, b"one\ntwo\n");
        assert!(Arc::ptr_eq(&b.meta, &e.cached(&p, Engine::stat_file(&p).expect("stat")).expect("cached")));
        assert_eq!((e.hits.load(Ordering::Relaxed), e.misses.load(Ordering::Relaxed)), (1, 1));
        assert_eq!(e.cached_entries(), 1);
    }

    // Checks that a freshly written file is treated as racy and recomputed.
    #[test]
    fn test_racy_entries_are_not_trusted() {
        let t = TempDir::new();
        let p = t.write("a.txt", b"x");
        let e = Engine::new();
        e.load(&p).expect("load");
        e.load(&p).expect("load");
        assert_eq!((e.hits.load(Ordering::Relaxed), e.misses.load(Ordering::Relaxed)), (0, 2));
    }

    // Checks that a changed file is recomputed.
    #[test]
    fn test_changed_file_misses() {
        let t = TempDir::new();
        let p = t.write("a.txt", b"x\n");
        age(&p);
        let e = Engine::new();
        let first = e.meta(&p).expect("meta");
        std::fs::write(&p, b"x\ny\n").expect("rewrite");
        age(&p);
        let second = e.meta(&p).expect("meta");
        assert_eq!((first.lines(), second.lines()), (1, 2));
    }

    // Checks meta() hits without reading when the entry is trusted.
    #[test]
    fn test_meta_hit_path() {
        let t = TempDir::new();
        let p = t.write("a.txt", b"x\n");
        age(&p);
        let e = Engine::new();
        e.meta(&p).expect("meta");
        e.meta(&p).expect("meta");
        assert_eq!(e.hits.load(Ordering::Relaxed), 1);
    }

    // Checks forget removes the record.
    #[test]
    fn test_forget_removes_entry() {
        let t = TempDir::new();
        let p = t.write("a.txt", b"x\n");
        age(&p);
        let e = Engine::new();
        e.meta(&p).expect("meta");
        e.forget(&p);
        assert_eq!(e.cached_entries(), 0);
    }

    // Checks error cases: missing, directory and oversize.
    #[test]
    fn test_load_errors() {
        let t = TempDir::new();
        let e = Engine::new();
        assert_eq!(e.load(&t.path().join("none")).err().as_deref(), Some("no such file or directory"));
        assert_eq!(e.meta(t.path()).err().as_deref(), Some("not a regular file"));
    }

    // Checks the cache clears when full.
    #[test]
    fn test_cache_clears_when_full() {
        let t = TempDir::new();
        let p = t.write("a.txt", b"x\n");
        age(&p);
        let e = Engine::new();
        {
            let mut c = e.cache.lock().expect("lock");
            for i in 0..MAX_ENTRIES {
                let m = Arc::new(Meta::compute(b"q"));
                c.insert(
                    PathBuf::from(format!("/fake/{i}")),
                    Entry { stat: Stat { dev: 0, ino: 0, size: 0, mtime_ns: 0 }, meta: m, racy: false },
                );
            }
        }
        e.meta(&p).expect("meta");
        assert_eq!(e.cached_entries(), 1);
    }

    // Checks the default constructor.
    #[test]
    fn test_default_is_empty() {
        assert_eq!(Engine::default().cached_entries(), 0);
    }

    // Checks the file-modified-during-read path does not cache.
    #[test]
    fn test_stat_mismatch_after_read_is_not_cached() {
        let t = TempDir::new();
        let p = t.write("a.txt", b"x\n");
        age(&p);
        let e = Engine::new();
        let st = Engine::stat_file(&p).expect("stat");
        let fake = Stat { size: st.size + 1, ..st };
        let meta = Arc::new(Meta::compute(b"x\n"));
        e.store(&p, fake, &meta);
        assert!(e.cached(&p, st).is_none());
    }

    // Checks oversize files are refused without reading them.
    #[test]
    fn test_load_rejects_oversize_file() {
        let t = TempDir::new();
        let p = t.path().join("huge");
        std::fs::File::create(&p).expect("create").set_len(MAX_META_SIZE + (44 << 20)).expect("sparse");
        let e = Engine::new();
        assert_eq!(e.load(&p).err().as_deref(), Some("file too large (300 MB)"));
    }

    // Checks a file that changes between the two stat calls is returned but not cached.
    #[test]
    fn test_load_does_not_cache_when_stat_changes() {
        let t = TempDir::new();
        let p = t.write("a.txt", b"x\n");
        age(&p);
        let e = Engine::new();
        let calls = Cell::new(0);
        let stat = |path: &Path| {
            let mut st = Engine::stat_file(path)?;
            calls.set(calls.get() + 1);
            st.size += calls.get() as u64;
            Ok(st)
        };
        let loaded = e.load_with(&p, &stat).expect("load");
        assert_eq!(loaded.data, b"x\n");
        assert_eq!(e.cached_entries(), 0);
    }
}
