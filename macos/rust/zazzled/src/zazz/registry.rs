// The script registry under ~/.zazzle/v1/zazz: artifacts and metadata by id, locks, names and scratch space.
use serde_json::Value;
use std::fs::File;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime};

const DIRS: [&str; 7] = ["wasm", "bin", "meta", "names", "locks", "tmp", "sdk"];
const MIN_PREFIX: usize = 12;
const STALE_TMP: Duration = Duration::from_secs(3600);

static TMP_COUNTER: AtomicUsize = AtomicUsize::new(0);

// The registry rooted at one directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registry {
    pub root: PathBuf,
}

// An exclusive lock on one script id, released when dropped.
pub struct Lock(File);

impl Drop for Lock {
    // Releases the lock.
    fn drop(&mut self) {
        // SAFETY: the descriptor is owned by self.0 and still open.
        unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

// The default registry directory: $ZAZZ_STORE, else ~/.zazzle/v1/zazz.
pub fn default_root() -> Result<PathBuf, String> {
    if let Some(p) = std::env::var_os("ZAZZ_STORE") {
        return Ok(PathBuf::from(p));
    }
    let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
    Ok(PathBuf::from(home).join(".zazzle/v1/zazz"))
}

// Strips the "b3:" prefix and checks an id or prefix is hex.
pub fn hex_of(id: &str) -> Option<&str> {
    let h = id.strip_prefix("b3:").unwrap_or(id);
    (!h.is_empty() && h.len() <= 64 && h.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()))
        .then_some(h)
}

impl Registry {
    // Opens (creating if needed) a registry at a directory, private to the user.
    pub fn open(root: &Path) -> Result<Registry, String> {
        for d in DIRS {
            std::fs::create_dir_all(root.join(d)).map_err(|e| format!("{}: {e}", root.join(d).display()))?;
        }
        let private = std::os::unix::fs::PermissionsExt::from_mode(0o700);
        std::fs::set_permissions(root, private).map_err(|e| format!("{}: {e}", root.display()))?;
        Ok(Registry { root: root.to_path_buf() })
    }

    // Where a script's metadata lives.
    pub fn meta_path(&self, hex: &str) -> PathBuf {
        self.root.join("meta").join(format!("{hex}.json"))
    }

    // Where a script's module lives (kind is "wasm" or "cwasm").
    pub fn wasm_path(&self, hex: &str, ext: &str) -> PathBuf {
        self.root.join("wasm").join(format!("{hex}.{ext}"))
    }

    // Where a script's native binary lives.
    pub fn bin_path(&self, hex: &str) -> PathBuf {
        self.root.join("bin").join(hex)
    }

    // Reads a script's metadata.
    pub fn read_meta(&self, hex: &str) -> Option<Value> {
        let text = std::fs::read(self.meta_path(hex)).ok()?;
        serde_json::from_slice(&text).ok()
    }

    // Writes a file atomically inside the registry.
    pub fn write_atomic(&self, path: &Path, data: &[u8]) -> Result<(), String> {
        let n = TMP_COUNTER.fetch_add(1, Ordering::SeqCst);
        let tmp = self.root.join("tmp").join(format!("w-{}-{n}", std::process::id()));
        let r = std::fs::write(&tmp, data).and_then(|()| std::fs::rename(&tmp, path));
        r.inspect_err(|_| drop(std::fs::remove_file(&tmp))).map_err(|e| format!("{}: {e}", path.display()))
    }

    // Writes a script's metadata atomically.
    pub fn write_meta(&self, hex: &str, meta: &Value) -> Result<(), String> {
        self.write_atomic(&self.meta_path(hex), meta.to_string().as_bytes())
    }

    // Every registered script id (hex), sorted.
    pub fn ids(&self) -> Vec<String> {
        let Ok(rd) = std::fs::read_dir(self.root.join("meta")) else { return Vec::new() };
        let mut out: Vec<String> = rd
            .filter_map(|e| e.ok()?.file_name().to_str()?.strip_suffix(".json").map(str::to_string))
            .collect();
        out.sort();
        out
    }

    // Resolves an id, a unique prefix of at least 12 hex digits, or an installed name, to a full hex id.
    pub fn resolve(&self, what: &str) -> Result<String, String> {
        if let Ok(target) = std::fs::read_to_string(self.root.join("names").join(what)) {
            return Ok(target.trim().to_string());
        }
        let hex =
            hex_of(what).ok_or_else(|| format!("'{what}' is not a script id, prefix or installed name"))?;
        if hex.len() < MIN_PREFIX {
            return Err(format!("'{what}': give at least {MIN_PREFIX} hex digits of the id"));
        }
        let hits: Vec<String> = self.ids().into_iter().filter(|id| id.starts_with(hex)).collect();
        match hits.as_slice() {
            [one] => Ok(one.clone()),
            [] => Err(format!("no script with id {what}; add it with `zazz add FILE.zazz`")),
            _ => Err(format!("'{what}' matches {} scripts; give more digits", hits.len())),
        }
    }

    // Takes the exclusive lock for a script id, waiting for other builders.
    pub fn lock(&self, hex: &str) -> Result<Lock, String> {
        let p = self.root.join("locks").join(format!("{hex}.lock"));
        let f = File::create(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        // SAFETY: f is an open descriptor for the duration of the call.
        if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(format!("{}: {}", p.display(), std::io::Error::last_os_error()));
        }
        Ok(Lock(f))
    }

    // Creates a fresh private scratch directory.
    pub fn scratch(&self, tag: &str) -> Result<PathBuf, String> {
        let n = TMP_COUNTER.fetch_add(1, Ordering::SeqCst);
        let p = self.root.join("tmp").join(format!("{tag}.{}.{n}", std::process::id()));
        std::fs::create_dir(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        std::fs::set_permissions(&p, std::os::unix::fs::PermissionsExt::from_mode(0o700))
            .map_err(|e| e.to_string())?;
        Ok(p)
    }

    // Removes scratch entries older than an hour, left by crashed builds.
    pub fn purge_tmp(&self) -> usize {
        let Ok(rd) = std::fs::read_dir(self.root.join("tmp")) else { return 0 };
        let mut n = 0;
        for e in rd.flatten() {
            let old = e
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| SystemTime::now().duration_since(t).ok());
            if old.is_some_and(|age| age > STALE_TMP) {
                let p = e.path();
                let gone = if p.is_dir() { std::fs::remove_dir_all(&p) } else { std::fs::remove_file(&p) };
                n += gone.is_ok() as usize;
            }
        }
        n
    }

    // Removes a script's artifacts, metadata and lock file.
    pub fn remove(&self, hex: &str) -> bool {
        let existed = self.meta_path(hex).exists();
        for p in [
            self.meta_path(hex),
            self.wasm_path(hex, "wasm"),
            self.wasm_path(hex, "cwasm"),
            self.bin_path(hex),
        ] {
            let _ = std::fs::remove_file(p);
        }
        let _ = std::fs::remove_file(self.root.join("locks").join(format!("{hex}.lock")));
        existed
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::testutil::{env_lock, TempDir};

    const A: &str = "aaaaaaaaaaaa1111111111111111111111111111111111111111111111111111";
    const B: &str = "aaaaaaaaaaaa2222222222222222222222222222222222222222222222222222";

    // Checks open creates the layout privately and metadata round-trips.
    #[test]
    fn test_open_and_meta() {
        let t = TempDir::new();
        let r = Registry::open(&t.path().join("z")).expect("open");
        for d in DIRS {
            assert!(r.root.join(d).is_dir());
        }
        let mode = std::fs::metadata(&r.root).expect("md");
        assert_eq!(std::os::unix::fs::PermissionsExt::mode(&mode.permissions()) & 0o777, 0o700);
        r.write_meta(A, &serde_json::json!({"name": "x"})).expect("write");
        assert_eq!(r.read_meta(A).expect("read")["name"], "x");
        assert_eq!(r.read_meta(B), None);
        assert_eq!(r.ids(), vec![A.to_string()]);
        assert!(r.write_atomic(&r.root.join("nope/x"), b"").is_err());
        std::fs::write(t.path().join("file"), "").expect("file");
        assert!(Registry::open(&t.path().join("file")).is_err());
    }

    // Checks resolution by full id, prefix, name, and the errors.
    #[test]
    fn test_resolve() {
        let t = TempDir::new();
        let r = Registry::open(t.path()).expect("open");
        for id in [A, B] {
            r.write_meta(id, &serde_json::json!({})).expect("meta");
        }
        assert_eq!(r.resolve(&format!("b3:{A}")).expect("full"), A);
        assert_eq!(r.resolve("aaaaaaaaaaaa1").expect("prefix"), A);
        assert!(r.resolve("aaaaaaaaaaaa").unwrap_err().contains("matches 2"));
        assert!(r.resolve("aaaa").unwrap_err().contains("at least 12"));
        assert!(r.resolve("ffffffffffffff").unwrap_err().contains("no script"));
        assert!(r.resolve("Not-Hex").unwrap_err().contains("not a script id"));
        std::fs::write(r.root.join("names/sum"), format!("{B}\n")).expect("name");
        assert_eq!(r.resolve("sum").expect("name"), B);
        assert_eq!((hex_of("b3:"), hex_of("ABC"), hex_of("abc")), (None, None, Some("abc")));
    }

    // Checks locks, scratch dirs, purging and removal.
    #[test]
    fn test_lock_scratch_purge_remove() {
        let t = TempDir::new();
        let r = Registry::open(t.path()).expect("open");
        let l = r.lock(A).expect("lock");
        drop(l);
        let s = r.scratch("x").expect("scratch");
        assert!(s.is_dir());
        assert_eq!(r.purge_tmp(), 0);
        let old = SystemTime::now() - Duration::from_secs(7200);
        std::fs::File::open(&s).expect("open").set_modified(old).expect("age dir");
        let f = r.root.join("tmp/old-file");
        std::fs::write(&f, "").expect("file");
        std::fs::File::options().write(true).open(&f).expect("open").set_modified(old).expect("age file");
        assert_eq!(r.purge_tmp(), 2);
        r.write_meta(A, &serde_json::json!({})).expect("meta");
        std::fs::write(r.wasm_path(A, "wasm"), "").expect("wasm");
        assert!(r.remove(A));
        assert!(!r.remove(A));
        assert!(!r.wasm_path(A, "wasm").exists());
        let bad = Registry { root: t.path().join("missing") };
        assert!(bad.lock(A).is_err() && bad.scratch("x").is_err());
        assert_eq!((bad.ids().len(), bad.purge_tmp()), (0, 0));
    }

    // Checks the default root follows ZAZZ_STORE, then HOME.
    #[test]
    fn test_default_root() {
        let _g = env_lock();
        std::env::set_var("ZAZZ_STORE", "/s");
        assert_eq!(default_root().expect("env"), PathBuf::from("/s"));
        std::env::remove_var("ZAZZ_STORE");
        let home = std::env::var_os("HOME");
        std::env::set_var("HOME", "/h");
        assert_eq!(default_root().expect("home"), PathBuf::from("/h/.zazzle/v1/zazz"));
        std::env::remove_var("HOME");
        assert!(default_root().is_err());
        if let Some(h) = home {
            std::env::set_var("HOME", h);
        }
    }
}
