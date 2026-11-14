// Packs a script directory into deterministic .zazz bytes, checking the manifest and limits first.
use crate::manifest::{Manifest, SIGNATURE};
use crate::zip::{self, check_name, Archive, Entry, Limits};
use std::path::Path;

// Skips hidden entries and the top-level build directory.
fn skipped(rel: &str, name: &str) -> bool {
    name.starts_with('.') || rel == "target"
}

// Collects every regular file under a directory as entries, refusing symlinks and special files.
fn collect(
    root: &Path,
    dir: &Path,
    prefix: &str,
    limits: &Limits,
    out: &mut Vec<Entry>,
) -> Result<(), String> {
    let mut items: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .collect::<Result<_, _>>()
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    items.sort_by_key(|e| e.file_name());
    for item in items {
        let name = item.file_name().to_string_lossy().into_owned();
        let rel = format!("{prefix}{name}");
        if skipped(&rel, &name) {
            continue;
        }
        let kind = item.file_type().map_err(|e| format!("{rel}: {e}"))?;
        if kind.is_dir() {
            collect(root, &item.path(), &format!("{rel}/"), limits, out)?;
            continue;
        }
        if !kind.is_file() {
            return Err(format!("{rel}: symlinks and special files cannot be packed"));
        }
        check_name(&rel)?;
        if out.len() >= limits.entries {
            return Err(format!("more than {} files under {}", limits.entries, root.display()));
        }
        let data = std::fs::read(item.path()).map_err(|e| format!("{rel}: {e}"))?;
        if data.len() as u64 > limits.entry {
            return Err(format!(
                "{rel}: {} bytes is over the per-entry limit of {}",
                data.len(),
                limits.entry
            ));
        }
        out.push(Entry { name: rel, data });
    }
    Ok(())
}

// Packs a directory: validates its manifest against its files and returns the archive bytes and manifest.
pub fn pack_dir(dir: &Path, limits: &Limits) -> Result<(Vec<u8>, Manifest), String> {
    let mut entries = Vec::new();
    collect(dir, dir, "", limits, &mut entries)?;
    if entries.iter().any(|e| e.name == SIGNATURE) {
        return Err(format!("{SIGNATURE} is written by `zazz sign`; remove it from the directory"));
    }
    let total: u64 = entries.iter().map(|e| e.data.len() as u64).sum();
    if total > limits.uncompressed {
        return Err(format!("files add up to {total} bytes; the limit is {}", limits.uncompressed));
    }
    let archive = Archive::new(entries);
    let manifest = Manifest::of(&archive)?;
    let bytes = zip::write(&archive)?;
    if bytes.len() as u64 > limits.compressed {
        return Err(format!("packed size {} is over the limit of {}", bytes.len(), limits.compressed));
    }
    Ok((bytes, manifest))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // A temporary directory removed when dropped.
    struct Tmp(PathBuf);

    impl Drop for Tmp {
        // Removes the directory tree.
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    // Creates a script directory with the given files.
    fn tree(tag: &str, files: &[(&str, &str)]) -> Tmp {
        let d = std::env::temp_dir().join(format!("zazz-pack-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        for (n, c) in files {
            let p = d.join(n);
            std::fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
            std::fs::write(p, c).expect("write");
        }
        Tmp(d)
    }

    const MANIFEST: &str = r#"{"zazz":1,"name":"demo","kind":"rust","entry":"src/main.rs"}"#;

    // Checks a directory packs, round-trips, skips hidden files and target/, and is deterministic.
    #[test]
    fn test_pack_dir_roundtrip_and_skips() {
        let t = tree(
            "ok",
            &[
                ("zazz.json", MANIFEST),
                ("src/main.rs", "fn main() {}"),
                (".git/x", "x"),
                ("target/y", "y"),
                (".hidden", "z"),
            ],
        );
        let (bytes, m) = pack_dir(&t.0, &Limits::DEFAULT).expect("pack");
        assert_eq!(m.name, "demo");
        let a = zip::read(&bytes, &Limits::DEFAULT).expect("read");
        let names: Vec<&str> = a.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["src/main.rs", "zazz.json"]);
        assert_eq!(pack_dir(&t.0, &Limits::DEFAULT).expect("again").0, bytes);
    }

    // Checks packing refuses a bad manifest, a signature file, symlinks and a missing directory.
    #[test]
    fn test_pack_dir_refusals() {
        let t = tree("nomani", &[("src/main.rs", "fn main() {}")]);
        assert!(pack_dir(&t.0, &Limits::DEFAULT).unwrap_err().contains("missing"));
        let t = tree("sig", &[("zazz.json", MANIFEST), ("src/main.rs", ""), ("zazz.sig", "{}")]);
        assert!(pack_dir(&t.0, &Limits::DEFAULT).unwrap_err().contains("zazz sign"));
        let t = tree("link", &[("zazz.json", MANIFEST), ("src/main.rs", "")]);
        std::os::unix::fs::symlink("/etc/hosts", t.0.join("link")).expect("symlink");
        assert!(pack_dir(&t.0, &Limits::DEFAULT).unwrap_err().contains("symlinks"));
        assert!(pack_dir(Path::new("/nonexistent-zazz-dir"), &Limits::DEFAULT).is_err());
    }

    // Checks the entry count, per-entry, total and packed-size limits.
    #[test]
    fn test_pack_dir_limits() {
        let big = "q".repeat(5000);
        let t = tree("lim", &[("zazz.json", MANIFEST), ("src/main.rs", &big)]);
        let with = |f: fn(&mut Limits)| {
            let mut l = Limits::DEFAULT;
            f(&mut l);
            pack_dir(&t.0, &l).unwrap_err()
        };
        assert!(with(|l| l.entries = 1).contains("more than 1 files"));
        assert!(with(|l| l.entry = 100).contains("per-entry limit"));
        assert!(with(|l| l.uncompressed = 1000).contains("add up to"));
        assert!(with(|l| l.compressed = 10).contains("packed size"));
    }
}
