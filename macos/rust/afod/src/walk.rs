// Directory walking that honours ignore files, skips .git and applies glob filters.
use ignore::overrides::OverrideBuilder;
use ignore::WalkBuilder;
use std::path::{Path, PathBuf};

// Options controlling which files are returned.
#[derive(Default)]
pub struct WalkOpts {
    pub no_ignore: bool,
    pub globs: Vec<String>,
}

// Collects regular files under the roots, sorted, with ignore rules and globs applied.
pub fn collect(roots: &[PathBuf], opts: &WalkOpts) -> Result<Vec<PathBuf>, String> {
    let mut out = Vec::new();
    for root in roots {
        let meta =
            std::fs::symlink_metadata(root).map_err(|e| format!("{}: {}", root.display(), reason(&e)))?;
        if meta.is_file() || (meta.file_type().is_symlink() && root.is_file()) {
            out.push(root.clone());
            continue;
        }
        walk_dir(root, opts, &mut out)?;
    }
    out.sort();
    out.dedup();
    Ok(out)
}

// Gives a short human reason for an I/O error.
pub fn reason(e: &std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::NotFound => "no such file or directory".into(),
        std::io::ErrorKind::PermissionDenied => "permission denied".into(),
        _ => e.to_string(),
    }
}

// Walks one directory root and appends its files.
fn walk_dir(root: &Path, opts: &WalkOpts, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let mut b = WalkBuilder::new(root);
    b.hidden(false).require_git(false).filter_entry(|e| e.file_name() != ".git");
    if opts.no_ignore {
        b.ignore(false).git_ignore(false).git_global(false).git_exclude(false).parents(false);
    }
    if !opts.globs.is_empty() {
        let mut ob = OverrideBuilder::new(root);
        for g in &opts.globs {
            ob.add(g).map_err(|e| format!("bad glob '{g}': {e}"))?;
        }
        b.overrides(ob.build().map_err(|e| format!("bad glob: {e}"))?);
    }
    for entry in b.build().flatten() {
        if entry.file_type().is_some_and(|t| t.is_file()) {
            out.push(entry.into_path());
        }
    }
    Ok(())
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    // Builds a small tree with an ignored file and a .git directory.
    fn tree() -> TempDir {
        let t = TempDir::new();
        t.write("a.rs", b"x");
        t.write("sub/b.md", b"y");
        t.write(".hidden/c.txt", b"z");
        t.write("skip.log", b"l");
        t.write(".gitignore", b"*.log\n");
        t.write(".git/config", b"g");
        t
    }

    // Lists file names relative to a root for assertions.
    fn names(files: &[PathBuf], root: &Path) -> Vec<String> {
        files
            .iter()
            .map(|f| f.strip_prefix(root).expect("under root").to_string_lossy().into_owned())
            .collect()
    }

    // Checks defaults: hidden files kept, ignored files and .git skipped.
    #[test]
    fn test_collect_defaults() {
        let t = tree();
        let files = collect(&[t.path().to_path_buf()], &WalkOpts::default()).expect("walk");
        assert_eq!(names(&files, t.path()), vec![".gitignore", ".hidden/c.txt", "a.rs", "sub/b.md"]);
    }

    // Checks that no_ignore also returns ignored files.
    #[test]
    fn test_collect_no_ignore() {
        let t = tree();
        let opts = WalkOpts { no_ignore: true, globs: vec![] };
        let files = collect(&[t.path().to_path_buf()], &opts).expect("walk");
        assert!(names(&files, t.path()).contains(&"skip.log".to_string()));
    }

    // Checks glob filtering and a bad glob error.
    #[test]
    fn test_collect_globs() {
        let t = tree();
        let opts = WalkOpts { no_ignore: false, globs: vec!["*.md".into()] };
        let files = collect(&[t.path().to_path_buf()], &opts).expect("walk");
        assert_eq!(names(&files, t.path()), vec!["sub/b.md"]);
        let bad = WalkOpts { no_ignore: false, globs: vec!["[".into()] };
        assert!(collect(&[t.path().to_path_buf()], &bad).unwrap_err().starts_with("bad glob"));
    }

    // Checks that a file root is returned directly and a missing root errors.
    #[test]
    fn test_collect_file_and_missing_roots() {
        let t = tree();
        let f = t.path().join("a.rs");
        assert_eq!(collect(std::slice::from_ref(&f), &WalkOpts::default()).expect("walk"), vec![f]);
        let err = collect(&[t.path().join("nope")], &WalkOpts::default()).unwrap_err();
        assert!(err.ends_with("no such file or directory"));
    }

    // Checks a symlink to a file is returned as a file root.
    #[test]
    fn test_collect_symlink_root() {
        let t = tree();
        let link = t.path().join("link.rs");
        std::os::unix::fs::symlink(t.path().join("a.rs"), &link).expect("symlink");
        assert_eq!(collect(std::slice::from_ref(&link), &WalkOpts::default()).expect("walk"), vec![link]);
    }

    // Checks the error reason texts.
    #[test]
    fn test_reason_texts() {
        use std::io::{Error, ErrorKind};
        assert_eq!(reason(&Error::from(ErrorKind::NotFound)), "no such file or directory");
        assert_eq!(reason(&Error::from(ErrorKind::PermissionDenied)), "permission denied");
        assert_eq!(reason(&Error::other("boom")), "boom");
    }

    // Checks a symlink to a directory is walked as a directory root.
    #[test]
    fn test_collect_symlink_to_directory() {
        let t = tree();
        let link = t.path().join("sublink");
        std::os::unix::fs::symlink(t.path().join("sub"), &link).expect("symlink");
        let files = collect(&[link], &WalkOpts::default()).expect("walk");
        assert!(files.iter().any(|f| f.ends_with("b.md")));
    }
}
