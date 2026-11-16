// Where zazzle may write: the working directory, the git work tree holding it, the user's workspaces and the roots an MCP client names.
use crate::indexer::work_tree;
use std::path::{Path, PathBuf};

// Returns the file listing the user's workspaces, one folder per line: ~/.zazzle/workspaces.
pub fn file(home: &Path) -> PathBuf {
    home.join(".zazzle/workspaces")
}

// Expands a leading `~/` against the home folder.
fn expand(line: &str, home: &Path) -> PathBuf {
    match line.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None if line == "~" => home.to_path_buf(),
        None => PathBuf::from(line),
    }
}

// True when a folder is too broad to be a workspace: the filesystem root or the home folder itself.
pub fn too_broad(dir: &Path, home: &Path) -> bool {
    let real = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let d = real(dir);
    d.parent().is_none() || d == real(home)
}

// Reads the configured workspaces: the workspaces file and ZAZZLE_WORKSPACES (colon-separated), skipping blanks, comments and too-broad folders.
pub fn configured(home: &Path) -> Vec<PathBuf> {
    let text = std::fs::read_to_string(file(home)).unwrap_or_default();
    let env = std::env::var("ZAZZLE_WORKSPACES").unwrap_or_default();
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .chain(env.split(':').filter(|l| !l.is_empty()))
        .map(|l| expand(l, home))
        .filter(|p| p.is_absolute() && !too_broad(p, home))
        .collect()
}

// Returns the canonical folders writes may go into, for a command run in `cwd` for a client that named `client` roots; the home folder and `/` never count, so a chat started there edits only its workspaces.
pub fn roots(cwd: &Path, client: &[PathBuf]) -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"));
    let mut all = vec![cwd.to_path_buf()];
    all.extend(work_tree(cwd).map(Path::to_path_buf));
    all.extend(client.iter().cloned());
    all.retain(|p| p.is_absolute() && !too_broad(p, &home));
    all.extend(configured(&home));
    let mut out: Vec<PathBuf> = Vec::new();
    for p in all.iter().filter_map(|p| p.canonicalize().ok()) {
        if !out.contains(&p) {
            out.push(p);
        }
    }
    out
}

// Checks a path stays inside one of the roots after resolving symlinks, and returns its real location.
pub fn confine(roots: &[PathBuf], path: &Path) -> Result<PathBuf, String> {
    let mut base = path.to_path_buf();
    let mut tail = Vec::new();
    while !base.exists() {
        let Some(name) = base.file_name().map(|n| n.to_os_string()) else {
            return Err("path with .. through a missing directory".into());
        };
        tail.push(name);
        base.pop();
    }
    let mut real =
        base.canonicalize().map_err(|e| format!("{}: {}", base.display(), crate::walk::reason(&e)))?;
    real.extend(tail.iter().rev());
    if roots.iter().any(|r| real.starts_with(r)) {
        return Ok(real);
    }
    Err(format!(
        "refusing to write outside the working directory, its git work tree and your workspaces ({}); add a folder with `zazzle setup --workspace DIR --apply`",
        match roots.is_empty() {
            true => "none".to_string(),
            false => roots.iter().map(|r| r.display().to_string()).collect::<Vec<_>>().join(", "),
        }
    ))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::testutil::{env_lock, TempDir};

    // Checks the roots: the working directory, its work tree, configured and client workspaces, and too-broad folders left out.
    #[test]
    fn test_roots_and_confine() {
        let _g = env_lock();
        let saved = (std::env::var_os("HOME"), std::env::var_os("ZAZZLE_WORKSPACES"));
        let home = TempDir::new();
        let h = home.path().canonicalize().expect("home");
        std::fs::create_dir_all(h.join("dev/api/.git")).expect("mkdir");
        std::fs::create_dir_all(h.join("dev/api/src")).expect("mkdir");
        std::fs::create_dir_all(h.join("wt/feature")).expect("mkdir");
        std::fs::create_dir_all(h.join("other")).expect("mkdir");
        std::fs::create_dir_all(h.join(".zazzle")).expect("mkdir");
        std::fs::write(file(&h), "# mine\n~/wt\n\n/\n~\n").expect("write");
        std::env::set_var("HOME", &h);
        std::env::set_var("ZAZZLE_WORKSPACES", format!("{}:", h.join("other").display()));
        let r = roots(&h.join("dev/api/src"), &[h.join("client"), PathBuf::from("rel"), h.clone()]);
        assert_eq!(r, vec![h.join("dev/api/src"), h.join("dev/api"), h.join("wt"), h.join("other")]);
        std::fs::create_dir_all(h.join("client")).expect("mkdir");
        let r = roots(&h.join("dev/api/src"), &[h.join("client")]);
        assert_eq!(r[2], h.join("client"));
        assert_eq!(roots(&h, &[]), vec![h.join("wt"), h.join("other")]);
        assert_eq!(roots(Path::new("/"), &[]), vec![h.join("wt"), h.join("other")]);
        assert_eq!(confine(&r, &h.join("dev/api/new/x.txt")).expect("in tree"), h.join("dev/api/new/x.txt"));
        assert!(confine(&r, &h.join("wt/feature/y.rs")).is_ok());
        let err = confine(&r, &h.join("elsewhere/z")).unwrap_err();
        assert!(
            err.starts_with("refusing to write outside the working directory")
                && err.contains("--workspace DIR")
        );
        assert_eq!(
            confine(&r, Path::new("/nonexistent-zz/../..")).unwrap_err(),
            "path with .. through a missing directory"
        );
        assert!(too_broad(Path::new("/"), &h) && too_broad(&h, &h) && !too_broad(&h.join("dev"), &h));
        assert_eq!(expand("~", &h), h);
        std::env::remove_var("ZAZZLE_WORKSPACES");
        match saved {
            (Some(v), w) => {
                std::env::set_var("HOME", v);
                if let Some(w) = w {
                    std::env::set_var("ZAZZLE_WORKSPACES", w);
                }
            }
            (None, _) => std::env::remove_var("HOME"),
        }
    }
}
