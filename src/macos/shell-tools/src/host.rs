// What the daemon lends the JSON operations: its cached walk, its search scan and its outlines.
use crate::state::Res;
use regex::bytes::Regex;
use std::path::{Path, PathBuf};

// One file's matches from a scan: how many lines match and the first of them as (line, 1-based column).
pub struct Matches {
    pub path: PathBuf,
    pub count: usize,
    pub hits: Vec<(usize, usize)>,
}

// A symbol with its 1-based line range and nesting depth (0 at the top level).
pub struct Symbol {
    pub start: usize,
    pub end: usize,
    pub depth: usize,
    pub kind: String,
    pub name: String,
}

// The daemon services the operations run on; tests supply a plain implementation.
pub trait Host {
    // Returns the regular files under the roots, sorted, filtered by the globs and, unless no_ignore, by the ignore files.
    fn files(&self, roots: &[PathBuf], globs: &[String], no_ignore: bool) -> Res<Vec<PathBuf>>;

    // Returns the directories below a root, sorted, honouring the ignore files unless no_ignore.
    fn dirs(&self, root: &Path, no_ignore: bool) -> Res<Vec<PathBuf>>;

    // Scans the files and returns those that match, in the order given, with at most `max` hits each.
    fn search(&self, files: &[PathBuf], re: &Regex, max: usize) -> Vec<Matches>;

    // Returns each file's symbols, or None for a file that has no outline (unsupported, binary or unreadable).
    fn outline(&self, files: &[PathBuf]) -> Vec<Option<Vec<Symbol>>>;
}

// Resolves a request path against the caller's working directory, dropping `.` components so equal paths compare equal.
pub fn resolve(cwd: &Path, path: &str) -> PathBuf {
    cwd.join(path).components().collect()
}

// Returns a path as the caller sees it: relative to the working directory when under it, "." for the directory itself.
pub fn display(path: &Path, cwd: &Path) -> String {
    match path.strip_prefix(cwd) {
        Ok(p) if p.as_os_str().is_empty() => ".".into(),
        Ok(p) => p.to_string_lossy().into_owned(),
        Err(_) => path.to_string_lossy().into_owned(),
    }
}

// True when a path below one of the roots has a component starting with a dot.
pub fn hidden(path: &Path, roots: &[PathBuf]) -> bool {
    let rel = roots.iter().find_map(|r| path.strip_prefix(r).ok()).unwrap_or(path);
    rel.components().any(|c| c.as_os_str().to_string_lossy().starts_with('.'))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub mod testing {
    use super::*;

    // A host over the real file system with no ignore files, globs as suffixes and a one-symbol-per-`fn` outline.
    pub struct Plain;

    // Lists every file and directory below a path, depth first.
    fn walk(dir: &Path, files: &mut Vec<PathBuf>, dirs: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                dirs.push(p.clone());
                walk(&p, files, dirs);
            } else {
                files.push(p);
            }
        }
    }

    impl Host for Plain {
        // Lists files below each root (or the root itself when it is a file), keeping those ending in a glob.
        fn files(&self, roots: &[PathBuf], globs: &[String], _no_ignore: bool) -> Res<Vec<PathBuf>> {
            let mut out = vec![];
            for r in roots {
                if !r.exists() {
                    return Err(format!("{}: no such file or directory", r.display()));
                }
                if r.is_file() {
                    out.push(r.clone());
                    continue;
                }
                walk(r, &mut out, &mut vec![]);
            }
            out.retain(|f| globs.iter().all(|g| f.to_string_lossy().ends_with(g.trim_start_matches('*'))));
            out.sort();
            Ok(out)
        }

        // Lists directories below the root.
        fn dirs(&self, root: &Path, _no_ignore: bool) -> Res<Vec<PathBuf>> {
            let mut dirs = vec![];
            walk(root, &mut vec![], &mut dirs);
            dirs.sort();
            Ok(dirs)
        }

        // Counts matching lines of readable text files and keeps the first hits.
        fn search(&self, files: &[PathBuf], re: &Regex, max: usize) -> Vec<Matches> {
            let mut out = vec![];
            for f in files {
                let Ok(bytes) = std::fs::read(f) else { continue };
                if bytes.contains(&0) {
                    continue;
                }
                let lines: Vec<(usize, usize)> = bytes
                    .split(|b| *b == b'\n')
                    .enumerate()
                    .filter_map(|(i, l)| re.find(l).map(|m| (i + 1, m.start() + 1)))
                    .collect();
                if !lines.is_empty() {
                    let hits = lines.iter().take(max).copied().collect();
                    out.push(Matches { path: f.clone(), count: lines.len(), hits });
                }
            }
            out
        }

        // Outlines .rs files: `impl X` lines open a container, `fn x` lines are symbols, other files have none.
        fn outline(&self, files: &[PathBuf]) -> Vec<Option<Vec<Symbol>>> {
            files
                .iter()
                .map(|f| {
                    if f.extension().is_none_or(|e| e != "rs") {
                        return None;
                    }
                    let text = std::fs::read_to_string(f).ok()?;
                    let mut out = vec![];
                    for (i, line) in text.lines().enumerate() {
                        let depth = line.len() - line.trim_start().len();
                        let mut words = line.split_whitespace();
                        if let (Some(kind @ ("fn" | "impl")), Some(name)) = (words.next(), words.next()) {
                            let name = name.split('(').next().unwrap_or(name).to_string();
                            out.push(Symbol {
                                start: i + 1,
                                end: i + 1,
                                depth: depth / 4,
                                kind: kind.into(),
                                name,
                            });
                        }
                    }
                    Some(out)
                })
                .collect()
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    // Paths under the working directory are shown relative to it, others whole.
    #[test]
    fn test_display_relative() {
        let cwd = Path::new("/w");
        assert_eq!(display(Path::new("/w/a/b"), cwd), "a/b");
        assert_eq!(display(Path::new("/w"), cwd), ".");
        assert_eq!(display(Path::new("/x/y"), cwd), "/x/y");
        assert_eq!(resolve(cwd, "a"), PathBuf::from("/w/a"));
        assert_eq!(resolve(cwd, "/abs"), PathBuf::from("/abs"));
        assert_eq!(resolve(cwd, "."), PathBuf::from("/w"));
        assert_eq!(resolve(cwd, "./a/./b"), PathBuf::from("/w/a/b"));
    }

    // Only components below the root count as hidden.
    #[test]
    fn test_hidden_below_root() {
        let roots = [PathBuf::from("/w/.github")];
        assert!(!hidden(Path::new("/w/.github/ci.yml"), &roots));
        assert!(hidden(Path::new("/w/.github/.x/ci.yml"), &roots));
        assert!(hidden(Path::new("/other/.a"), &roots));
    }
}
