// Disk store for outputs that cannot be produced again (clean, run, proxy): kept under an id for read-again, reaped after an hour or when the store passes its size limit.
use std::fs;
use std::io;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

pub const MAX_AGE: Duration = Duration::from_secs(3600);
const DEFAULT_MAX_GB: f64 = 2.0;
const ID_LEN: usize = 10;
const ALPHABET: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";
const SHOWN: &str = "shown";
const FULL: &str = "full";

// A stored output: the text that was shown and the full text it came from.
#[derive(Debug, PartialEq)]
pub struct Stored {
    pub shown: String,
    pub full: String,
}

// Returns the store directory: $ZAZZLE_OUTPUTS_DIR, else ~/.zazzle/outputs.
pub fn dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("ZAZZLE_OUTPUTS_DIR").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(d));
    }
    Some(PathBuf::from(std::env::var_os("HOME")?).join(".zazzle/outputs"))
}

// Returns the size limit in bytes: $ZAZZLE_OUTPUTS_MAX_GB gigabytes, default 2.
pub fn limit() -> u64 {
    let gb =
        std::env::var("ZAZZLE_OUTPUTS_MAX_GB").ok().and_then(|v| v.parse::<f64>().ok()).filter(|g| *g > 0.0);
    (gb.unwrap_or(DEFAULT_MAX_GB) * 1e9) as u64
}

// Derives an output id from the texts to store.
pub fn id_of(shown: &str, full: &str) -> String {
    let mut h = blake3::Hasher::new();
    h.update(shown.as_bytes());
    h.update(&[0]);
    h.update(full.as_bytes());
    let hash = h.finalize();
    let mut bits = hash.as_bytes().iter().take(8).fold(0u64, |acc, b| (acc << 8) | u64::from(*b));
    (0..ID_LEN)
        .map(|_| {
            let c = ALPHABET[(bits & 31) as usize] as char;
            bits >>= 5;
            c
        })
        .collect()
}

// True for a string shaped like an output id, so it is safe to use in a file name.
pub fn is_id(id: &str) -> bool {
    id.len() == ID_LEN && id.bytes().all(|b| ALPHABET.contains(&b))
}

// Writes a file readable only by its owner, atomically through a temporary name.
fn write_private(path: &Path, text: &str) -> io::Result<()> {
    let tmp = path.with_extension("tmp");
    {
        use std::io::Write;
        let mut f = fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
        f.write_all(text.as_bytes())?;
    }
    fs::rename(&tmp, path)
}

// Stores an output under its id, then reaps old entries and trims the store when it is too big.
pub fn save(dir: &Path, id: &str, shown: &str, full: &str, limit: u64) -> io::Result<()> {
    if !is_id(id) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "not an output id"));
    }
    fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    write_private(&dir.join(format!("{id}.{FULL}")), full)?;
    write_private(&dir.join(format!("{id}.{SHOWN}")), shown)?;
    reap(dir, SystemTime::now(), MAX_AGE, limit);
    Ok(())
}

// Loads a stored output by id.
pub fn load(dir: &Path, id: &str) -> io::Result<Stored> {
    if !is_id(id) {
        return Err(io::Error::new(io::ErrorKind::NotFound, "not an output id"));
    }
    Ok(Stored {
        shown: fs::read_to_string(dir.join(format!("{id}.{SHOWN}")))?,
        full: fs::read_to_string(dir.join(format!("{id}.{FULL}")))?,
    })
}

// Removes entries older than max_age, then, if the rest pass the limit, the oldest until half the bytes remain; returns how many entries went.
pub fn reap(dir: &Path, now: SystemTime, max_age: Duration, limit: u64) -> usize {
    let Ok(read) = fs::read_dir(dir) else { return 0 };
    let mut entries: Vec<(SystemTime, u64, PathBuf)> = read
        .flatten()
        .filter_map(|e| {
            let md = e.metadata().ok()?;
            Some((md.modified().ok()?, md.len(), e.path()))
        })
        .collect();
    entries.sort();
    let old = |t: &SystemTime| now.duration_since(*t).unwrap_or_default() > max_age;
    let mut removed = entries.iter().filter(|(t, _, p)| old(t) && fs::remove_file(p).is_ok()).count();
    entries.retain(|(t, _, _)| !old(t));
    let mut total: u64 = entries.iter().map(|(_, n, _)| n).sum();
    if total > limit {
        let target = total / 2;
        for (_, n, p) in &entries {
            if total <= target {
                break;
            }
            if fs::remove_file(p).is_ok() {
                total -= n;
                removed += 1;
            }
        }
    }
    removed.div_ceil(2)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    // Sets a file's modification time to some seconds before now.
    fn backdate(path: &Path, secs: u64) {
        let f = fs::File::options().write(true).open(path).expect("open");
        f.set_modified(SystemTime::now() - Duration::from_secs(secs)).expect("set mtime");
    }

    // Checks an output round-trips under a stable id, privately, and bad ids are refused.
    #[test]
    fn test_save_and_load() {
        let t = TempDir::new();
        let dir = t.path().join("out");
        let id = id_of("shown", "the full text");
        save(&dir, &id, "shown", "the full text", u64::MAX).expect("save");
        assert!(is_id(&id));
        assert!(save(&dir, "../x", "a", "b", u64::MAX).is_err());
        assert_eq!(
            load(&dir, &id).expect("load"),
            Stored { shown: "shown".into(), full: "the full text".into() }
        );
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs::metadata(&dir).expect("dir").permissions().mode() & 0o777, 0o700);
        assert_eq!(
            fs::metadata(dir.join(format!("{id}.full"))).expect("file").permissions().mode() & 0o777,
            0o600
        );
        assert!(load(&dir, "../../etc/x").is_err());
        assert!(load(&dir, "zzzzzzzzzz").is_err());
        assert!(!is_id("ABCDEFGHIJ") && !is_id("abc"));
    }

    // Checks entries older than an hour go and newer ones stay.
    #[test]
    fn test_reap_by_age() {
        let t = TempDir::new();
        let put = |shown: &str, full: &str| {
            let id = id_of(shown, full);
            save(t.path(), &id, shown, full, u64::MAX).expect("save");
            id
        };
        let (old, new) = (put("a", "old"), put("b", "new"));
        for ext in ["full", "shown"] {
            backdate(&t.path().join(format!("{old}.{ext}")), 3700);
        }
        assert_eq!(reap(t.path(), SystemTime::now(), MAX_AGE, u64::MAX), 1);
        assert!(load(t.path(), &old).is_err());
        assert!(load(t.path(), &new).is_ok());
        assert_eq!(reap(&t.path().join("missing"), SystemTime::now(), MAX_AGE, 0), 0);
    }

    // Checks a store over its limit loses its oldest entries until half its bytes remain.
    #[test]
    fn test_reap_by_size() {
        let t = TempDir::new();
        let ids: Vec<String> = (0..4)
            .map(|i| {
                let (shown, full) = (format!("s{i}"), "x".repeat(1000));
                let id = id_of(&shown, &full);
                save(t.path(), &id, &shown, &full, u64::MAX).expect("save");
                id
            })
            .collect();
        for (i, id) in ids.iter().enumerate() {
            for ext in ["full", "shown"] {
                backdate(&t.path().join(format!("{id}.{ext}")), 100 - i as u64 * 10);
            }
        }
        assert_eq!(reap(t.path(), SystemTime::now(), MAX_AGE, 3000), 2);
        assert!(load(t.path(), &ids[0]).is_err() && load(t.path(), &ids[1]).is_err());
        assert!(load(t.path(), &ids[2]).is_ok() && load(t.path(), &ids[3]).is_ok());
    }

    // Checks the store directory and limit come from the environment, with their defaults.
    #[test]
    fn test_dir_and_limit() {
        let _g = crate::testutil::env_lock();
        let saved = (std::env::var_os("ZAZZLE_OUTPUTS_DIR"), std::env::var_os("ZAZZLE_OUTPUTS_MAX_GB"));
        std::env::set_var("ZAZZLE_OUTPUTS_DIR", "/tmp/zout");
        std::env::set_var("ZAZZLE_OUTPUTS_MAX_GB", "0.5");
        assert_eq!((dir(), limit()), (Some(PathBuf::from("/tmp/zout")), 500_000_000));
        std::env::remove_var("ZAZZLE_OUTPUTS_DIR");
        std::env::set_var("ZAZZLE_OUTPUTS_MAX_GB", "nonsense");
        assert!(dir().is_some_and(|d| d.ends_with(".zazzle/outputs")));
        assert_eq!(limit(), 2_000_000_000);
        for (k, v) in [("ZAZZLE_OUTPUTS_DIR", saved.0), ("ZAZZLE_OUTPUTS_MAX_GB", saved.1)] {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
    }
}
