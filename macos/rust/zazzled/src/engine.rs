// The engine: shared state behind every command, including the metadata caches.
use crate::meta::{Info, Meta, MAX_META_SIZE};
use crate::outline::{self, Lang, Symbol};
use crate::store::{StatKey, Store};
use std::collections::HashMap;
use std::fs::Metadata;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const MAX_ENTRIES: usize = 4096;
const MAX_OUTLINES: usize = 2048;
const RACY_NS: i64 = 2_000_000_000;

// Reads the identifying fields from file metadata.
pub fn key_of(md: &Metadata) -> StatKey {
    StatKey {
        dev: md.dev(),
        ino: md.ino(),
        size: md.size(),
        mtime_ns: md.mtime() * 1_000_000_000 + md.mtime_nsec(),
    }
}

// Cached outlines keyed by content hash and language.
type OutlineMap = HashMap<([u8; 32], Lang), Arc<Vec<Symbol>>>;

// A hot cached record and whether it was computed too close to the file's mtime to trust.
struct Entry {
    key: StatKey,
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
    store: Option<Store>,
    pub walks: crate::walk::WalkCache,
    pub sessions: crate::session::Sessions,
    outlines: Mutex<OutlineMap>,
    edit_lock: Mutex<()>,
    pub hits: AtomicU64,
    pub misses: AtomicU64,
    pub outputs: Option<PathBuf>,
}

impl Default for Engine {
    // Creates an engine with no persistent store.
    fn default() -> Engine {
        Engine::new()
    }
}

// Returns the current time in nanoseconds since the epoch.
fn now_ns() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos() as i64)
}

impl Engine {
    // Creates an engine with only the in-memory cache.
    pub fn new() -> Engine {
        Engine {
            started: Instant::now(),
            cache: Mutex::new(HashMap::new()),
            store: None,
            walks: crate::walk::WalkCache::default(),
            sessions: crate::session::Sessions::default(),
            outlines: Mutex::new(HashMap::new()),
            edit_lock: Mutex::new(()),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            outputs: None,
        }
    }

    // Creates an engine backed by a persistent store.
    pub fn with_store(store: Store) -> Engine {
        Engine { store: Some(store), ..Engine::new() }
    }

    // Returns the engine with an output store directory for clean, run and proxy outputs.
    pub fn with_outputs(self, dir: Option<PathBuf>) -> Engine {
        Engine { outputs: dir, ..self }
    }

    // Returns the hot cached metadata when the stat still matches and the entry is trustworthy.
    fn cached(&self, path: &Path, key: StatKey) -> Option<Arc<Meta>> {
        let cache = self.cache.lock().expect("cache lock");
        let e = cache.get(path)?;
        (e.key == key && !e.racy).then(|| Arc::clone(&e.meta))
    }

    // Remembers metadata in the hot cache and the store, clearing the hot cache first if it is full.
    fn store_meta(&self, path: &Path, key: StatKey, meta: &Arc<Meta>) {
        let mut cache = self.cache.lock().expect("cache lock");
        if cache.len() >= MAX_ENTRIES {
            cache.clear();
        }
        let racy = now_ns() - key.mtime_ns < RACY_NS;
        cache.insert(path.to_path_buf(), Entry { key, meta: Arc::clone(meta), racy });
        drop(cache);
        if let Some(s) = &self.store {
            s.put(path, key, meta.info());
        }
    }

    // Returns the symbol outline of loaded content, cached by content hash and language.
    pub fn outline(&self, path: &Path, loaded: &Loaded) -> Arc<Vec<Symbol>> {
        let key = (loaded.meta.hash, outline::lang_of(path));
        if let Some(o) = self.outlines.lock().expect("outline lock").get(&key) {
            return Arc::clone(o);
        }
        let syms = Arc::new(outline::outline(path, &loaded.data, &loaded.meta.starts));
        let mut cache = self.outlines.lock().expect("outline lock");
        if cache.len() >= MAX_OUTLINES {
            cache.clear();
        }
        cache.insert(key, Arc::clone(&syms));
        syms
    }

    // Takes the lock that serialises edits to files.
    pub fn edit_guard(&self) -> std::sync::MutexGuard<'_, ()> {
        self.edit_lock.lock().expect("edit lock")
    }

    // Drops any cached record for a path, for example after an edit.
    pub fn forget(&self, path: &Path) {
        self.cache.lock().expect("cache lock").remove(path);
    }

    // Returns the number of hot cached records.
    pub fn cached_entries(&self) -> usize {
        self.cache.lock().expect("cache lock").len()
    }

    // Returns the number of persisted records.
    pub fn stored_records(&self) -> usize {
        self.store.as_ref().map_or(0, Store::len)
    }

    // Writes pending persistent records to disk.
    pub fn flush(&self) -> io::Result<bool> {
        self.store.as_ref().map_or(Ok(false), Store::flush)
    }

    // Stats a path and checks it is a readable regular file of supported size.
    fn stat_file(path: &Path) -> Result<StatKey, String> {
        let md = std::fs::metadata(path).map_err(|e| crate::walk::reason(&e))?;
        if !md.is_file() {
            return Err("not a regular file".into());
        }
        if md.len() > MAX_META_SIZE {
            return Err(format!("file too large ({} MB)", md.len() >> 20));
        }
        Ok(key_of(&md))
    }

    // Reads a file and its metadata, using the hot cache when the stat is unchanged.
    pub fn load(&self, path: &Path) -> Result<Loaded, String> {
        self.load_with(path, &Engine::stat_file)
    }

    // Loads a file using the given stat function, which tests replace to simulate concurrent changes.
    fn load_with(
        &self,
        path: &Path,
        stat: &dyn Fn(&Path) -> Result<StatKey, String>,
    ) -> Result<Loaded, String> {
        let key = stat(path)?;
        let cached = self.cached(path, key);
        let data = std::fs::read(path).map_err(|e| crate::walk::reason(&e))?;
        let key_after = stat(path)?;
        if let Some(meta) = cached.filter(|_| key_after == key) {
            self.hits.fetch_add(1, Ordering::Relaxed);
            return Ok(Loaded { data, meta });
        }
        self.misses.fetch_add(1, Ordering::Relaxed);
        let meta = Arc::new(Meta::compute(&data));
        if key_after == key {
            self.store_meta(path, key, &meta);
        }
        Ok(Loaded { data, meta })
    }

    // Returns a file's summary, reading the file only when neither cache has a valid record; it never hashes the file.
    pub fn info(&self, path: &Path) -> Result<Info, String> {
        let key = Engine::stat_file(path)?;
        if let Some(m) = self.cached(path, key) {
            self.hits.fetch_add(1, Ordering::Relaxed);
            return Ok(m.info());
        }
        if let Some(i) = self.store.as_ref().and_then(|s| s.get(path, &key)) {
            self.hits.fetch_add(1, Ordering::Relaxed);
            return Ok(i);
        }
        self.misses.fetch_add(1, Ordering::Relaxed);
        let data = std::fs::read(path).map_err(|e| crate::walk::reason(&e))?;
        let info = Info::compute(&data);
        if let (Some(s), Ok(after)) = (&self.store, Engine::stat_file(path)) {
            if after == key {
                s.put(path, key, info);
            }
        }
        Ok(info)
    }

    // Returns the persisted summary for a file whose metadata the caller already has, without reading it.
    pub fn stored_info(&self, path: &Path, md: &Metadata) -> Option<Info> {
        self.store.as_ref()?.get(path, &key_of(md))
    }

    // Persists the summary of content the caller already read.
    pub fn remember(&self, path: &Path, md: &Metadata, data: &[u8]) {
        if let Some(s) = &self.store {
            s.put(path, key_of(md), Info::compute(data));
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::testutil::{age, TempDir};
    use std::cell::Cell;

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
        let first = e.info(&p).expect("info");
        std::fs::write(&p, b"x\ny\n").expect("rewrite");
        age(&p);
        let second = e.info(&p).expect("info");
        assert_eq!((first.lines, second.lines), (1, 2));
    }

    // Checks info() answers from the hot cache that a load filled, without reading again.
    #[test]
    fn test_info_hot_cache_hit() {
        let t = TempDir::new();
        let p = t.write("a.txt", b"x\n");
        age(&p);
        let e = Engine::new();
        e.load(&p).expect("load");
        assert_eq!(e.info(&p).expect("info").lines, 1);
        assert_eq!(e.hits.load(Ordering::Relaxed), 1);
    }

    // Checks info() on its own computes the hash-free summary and leaves the hot cache of full metadata alone.
    #[test]
    fn test_info_alone_does_not_fill_hot_cache() {
        let t = TempDir::new();
        let p = t.write("a.txt", b"x\r\ny\r\n");
        age(&p);
        let e = Engine::new();
        let i = e.info(&p).expect("info");
        assert_eq!((i.lines, i.crlf, i.size), (2, true, 6));
        e.info(&p).expect("info");
        assert_eq!((e.hits.load(Ordering::Relaxed), e.misses.load(Ordering::Relaxed)), (0, 2));
        assert_eq!(e.cached_entries(), 0);
    }

    // Checks forget removes the record.
    #[test]
    fn test_forget_removes_entry() {
        let t = TempDir::new();
        let p = t.write("a.txt", b"x\n");
        age(&p);
        let e = Engine::new();
        e.load(&p).expect("load");
        assert_eq!(e.cached_entries(), 1);
        e.forget(&p);
        assert_eq!(e.cached_entries(), 0);
    }

    // Checks error cases: missing and directory.
    #[test]
    fn test_load_errors() {
        let t = TempDir::new();
        let e = Engine::new();
        assert_eq!(e.load(&t.path().join("none")).err().as_deref(), Some("no such file or directory"));
        assert_eq!(e.info(t.path()).err().as_deref(), Some("not a regular file"));
    }

    // Checks the hot cache clears when full.
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
                    Entry { key: StatKey { dev: 0, ino: 0, size: 0, mtime_ns: 0 }, meta: m, racy: false },
                );
            }
        }
        e.load(&p).expect("load");
        assert_eq!(e.cached_entries(), 1);
    }

    // Checks outlines are cached by content and cleared when the cache is full.
    #[test]
    fn test_outline_cache() {
        let t = TempDir::new();
        let p = t.write("a.rs", b"fn f() {}\n");
        let e = Engine::new();
        let loaded = e.load(&p).expect("load");
        let a = e.outline(&p, &loaded);
        let b = e.outline(&p, &loaded);
        assert!(Arc::ptr_eq(&a, &b));
        assert_eq!(a[0].name, "f");
        {
            let mut c = e.outlines.lock().expect("lock");
            for i in 0..MAX_OUTLINES {
                let mut key = [0u8; 32];
                key[..8].copy_from_slice(&(i as u64).to_le_bytes());
                c.insert((key, Lang::Generic), Arc::new(vec![]));
            }
        }
        let other = t.write("b.rs", b"fn g() {}\n");
        e.outline(&other, &e.load(&other).expect("load"));
        assert_eq!(e.outlines.lock().expect("lock").len(), 1);
    }

    // Checks the edit guard can be taken repeatedly.
    #[test]
    fn test_edit_guard() {
        let e = Engine::new();
        drop(e.edit_guard());
        drop(e.edit_guard());
    }

    // Checks the default constructor.
    #[test]
    fn test_default_is_empty() {
        let e = Engine::default();
        assert_eq!((e.cached_entries(), e.stored_records()), (0, 0));
        assert!(!e.flush().expect("flush without store"));
    }

    // Checks the file-modified-during-read path does not cache.
    #[test]
    fn test_stat_mismatch_after_read_is_not_cached() {
        let t = TempDir::new();
        let p = t.write("a.txt", b"x\n");
        age(&p);
        let e = Engine::new();
        let st = Engine::stat_file(&p).expect("stat");
        let fake = StatKey { size: st.size + 1, ..st };
        let meta = Arc::new(Meta::compute(b"x\n"));
        e.store_meta(&p, fake, &meta);
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

    // Builds an engine with a store in a temp directory.
    fn stored(dir: &TempDir) -> Engine {
        Engine::with_store(Store::open(&dir.path().join("cache")).expect("open store"))
    }

    // Checks info() answers from the persistent store after a restart without reading the file.
    #[test]
    fn test_info_served_from_store_after_restart() {
        let t = TempDir::new();
        let p = t.write("src/a.txt", b"one\ntwo\nthree\n");
        age(&p);
        let first = stored(&t);
        assert_eq!(first.info(&p).expect("info").lines, 3);
        assert!(first.flush().expect("flush"));
        drop(first);
        let second = stored(&t);
        assert_eq!(second.stored_records(), 1);
        let i = second.info(&p).expect("info");
        assert_eq!(i.lines, 3);
        assert_eq!((second.hits.load(Ordering::Relaxed), second.misses.load(Ordering::Relaxed)), (1, 0));
    }

    // Checks stored_info and remember cooperate and respect changes.
    #[test]
    fn test_remember_and_stored_info() {
        let t = TempDir::new();
        let p = t.write("bin.dat", b"a\0b");
        age(&p);
        let e = stored(&t);
        let md = std::fs::metadata(&p).expect("meta");
        assert!(e.stored_info(&p, &md).is_none());
        e.remember(&p, &md, b"a\0b");
        assert!(e.stored_info(&p, &md).expect("stored").binary);
        std::fs::write(&p, b"text now\n").expect("rewrite");
        age(&p);
        assert!(e.stored_info(&p, &std::fs::metadata(&p).expect("meta")).is_none());
        let plain = Engine::new();
        plain.remember(&p, &md, b"x");
        assert!(plain.stored_info(&p, &md).is_none());
    }
}
