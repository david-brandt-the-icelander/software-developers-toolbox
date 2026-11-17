// Small helpers shared by commands: path display and resolution, hex, and line clipping.
use std::path::{Path, PathBuf};

pub const MAX_LINE: usize = 240;

// Resolves a path from an override variable's value, else from HOME joined with a relative path.
pub fn home_path(
    over: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
    rel: &str,
) -> Result<PathBuf, String> {
    match (over, home) {
        (Some(p), _) => Ok(PathBuf::from(p)),
        (None, Some(h)) => Ok(PathBuf::from(h).join(rel)),
        (None, None) => Err("HOME is not set".into()),
    }
}

// Shows a path relative to the working directory when it lies under it.
pub fn display(path: &Path, cwd: &Path) -> String {
    match path.strip_prefix(cwd) {
        Ok(p) if p.as_os_str().is_empty() => ".".into(),
        Ok(p) => p.to_string_lossy().into_owned(),
        Err(_) => path.to_string_lossy().into_owned(),
    }
}

// Resolves an argument against the working directory.
pub fn resolve(arg: &str, cwd: &Path) -> PathBuf {
    let p = Path::new(arg);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        cwd.join(p)
    }
}

// Resolves a path argument of a file command: like resolve, but a relative path that does not exist from the working directory is also looked for from the solution folders above it (holding a .sln or .slnx), its repository's root, or the one repository below it that has it; `REPO:path` names a repository as zazzle shows it.
pub fn locate(arg: &str, cwd: &Path) -> PathBuf {
    let local = resolve(arg, cwd);
    if local.exists() || Path::new(arg).is_absolute() {
        return local;
    }
    if let Some((name, rest)) = arg.split_once(':') {
        if let Some(r) = repositories(cwd).into_iter().find(|(n, _)| n == name).map(|(_, r)| r.join(rest)) {
            return r;
        }
    }
    let tree = crate::indexer::work_tree(cwd);
    let solutions = cwd
        .ancestors()
        .skip(1)
        .take_while(|a| tree.is_none_or(|t| a.starts_with(t)))
        .filter(|a| has_solution(a));
    let above = solutions.chain(tree).map(|d| d.join(arg)).find(|p| p.exists());
    if let Some(p) = above {
        return p;
    }
    let below: Vec<PathBuf> = match tree {
        Some(_) => Vec::new(),
        None => repositories(cwd).into_iter().map(|(_, r)| r.join(arg)).filter(|p| p.exists()).collect(),
    };
    match below.as_slice() {
        [one] => one.clone(),
        _ => local,
    }
}

// Names up to three files in the work tree (or the working directory) whose name matches a missing path's file name or contains its stem, for an error to offer.
pub fn similar(arg: &str, cwd: &Path, walks: &crate::walk::WalkCache) -> String {
    let want = Path::new(arg).file_name().and_then(|n| n.to_str()).unwrap_or(arg).to_lowercase();
    let stem = want.split('.').next().unwrap_or(&want).to_string();
    if stem.len() < 3 {
        return String::new();
    }
    let root = crate::indexer::work_tree(cwd).unwrap_or(cwd).to_path_buf();
    let Ok(files) = crate::walk::collect(&[root], &crate::walk::WalkOpts::default(), walks) else {
        return String::new();
    };
    let name = |f: &PathBuf| f.file_name().and_then(|n| n.to_str()).unwrap_or("").to_lowercase();
    let mut hits: Vec<&PathBuf> = files.iter().filter(|f| name(f) == want).collect();
    if hits.is_empty() {
        hits = files.iter().filter(|f| name(f).contains(&stem)).collect();
    }
    hits.sort_by_key(|f| rank(f));
    let shown: Vec<String> = hits.iter().take(3).map(|f| display(f, cwd)).collect();
    match (shown.len(), hits.len()) {
        (0, _) => String::new(),
        (n, all) if all > n => format!("; similar: {} (+{} more)", shown.join(", "), all - n),
        _ => format!("; similar: {}", shown.join(", ")),
    }
}

// True when a folder holds a Visual Studio solution file.
fn has_solution(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|mut es| {
        es.any(|e| e.is_ok_and(|e| e.path().extension().is_some_and(|x| x == "sln" || x == "slnx")))
    })
}

// Lists the repositories a working directory reaches, by the names zazzle shows: its own work tree (by folder name), else those below it (by path from it).
fn repositories(cwd: &Path) -> Vec<(String, PathBuf)> {
    match crate::indexer::work_tree(cwd) {
        Some(t) => vec![(crate::indexer::root_name(t), t.to_path_buf())],
        None => crate::cmd::structure::repos_below(cwd).into_iter().map(|r| (display(&r, cwd), r)).collect(),
    }
}

// Renders the first four bytes of a hash as eight hex digits.
pub fn hex8(hash: &[u8; 32]) -> String {
    hash[..4].iter().map(|b| format!("{b:02x}")).collect()
}

// True for a directory name that holds tests or documentation: `tests`, `__tests__`, .NET projects such as `UnitTests.Core` or `Core.Tests`, `docs`.
fn side_dir(dir: &str) -> bool {
    const SIDE_DIRS: [&str; 9] = ["test", "tests", "spec", "specs", "__tests__", "testdata", "doc", "docs", "documentation"];
    let lower = dir.to_ascii_lowercase();
    SIDE_DIRS.contains(&lower.as_str())
        || lower.ends_with("tests")
        || lower.contains(".tests.")
        || ["unittests.", "integrationtests.", "contracttests.", "tests."].iter().any(|p| lower.starts_with(p))
}

// Ranks a path for display: ordinary files before tests and documentation, then shallower before deeper.
pub fn rank(path: &Path) -> (bool, usize) {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let in_side_dir = path.parent().is_some_and(|p| p.components().any(|c| c.as_os_str().to_str().is_some_and(side_dir)));
    let test_name = name.starts_with("test_")
        || ["_test.", ".test.", ".spec."].iter().any(|m| name.contains(m))
        || ["Tests.cs", "Test.cs", "Tests.cshtml.cs"].iter().any(|s| name.ends_with(s) && name.len() > s.len());
    let docs_name = [".md", ".markdown", ".rst", ".txt"].iter().any(|e| name.to_ascii_lowercase().ends_with(e));
    (in_side_dir || test_name || docs_name, path.components().count())
}

// Moves an index down to the nearest UTF-8 character boundary.
fn floor_boundary(s: &str, mut i: usize) -> usize {
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

// Moves an index up to the nearest UTF-8 character boundary.
fn ceil_boundary(s: &str, mut i: usize) -> usize {
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

// Trims a long line to a window around the match, marking the cut ends with an ellipsis.
pub fn clip(line: &str, m_start: usize, max: usize) -> String {
    if line.len() <= max {
        return line.to_string();
    }
    let begin = floor_boundary(line, m_start.saturating_sub(max / 3).min(line.len() - max));
    let end = ceil_boundary(line, (begin + max).min(line.len()));
    let head = if begin > 0 { "…" } else { "" };
    let tail = if end < line.len() { "…" } else { "" };
    format!("{head}{}{tail}", &line[begin..end])
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    // Checks the override wins, then HOME, and a missing HOME is an error.
    #[test]
    fn test_home_path() {
        assert_eq!(home_path(Some("/o".into()), Some("/h".into()), "x").expect("over"), PathBuf::from("/o"));
        assert_eq!(home_path(None, Some("/h".into()), ".z/s").expect("home"), PathBuf::from("/h/.z/s"));
        assert_eq!(home_path(None, None, "x").unwrap_err(), "HOME is not set");
    }

    // Checks relative, root and outside paths.
    #[test]
    fn test_display_variants() {
        let cwd = Path::new("/w");
        assert_eq!(display(Path::new("/w/a/b.rs"), cwd), "a/b.rs");
        assert_eq!(display(Path::new("/w"), cwd), ".");
        assert_eq!(display(Path::new("/x/y"), cwd), "/x/y");
    }

    // Checks absolute and relative resolution.
    #[test]
    fn test_resolve_variants() {
        assert_eq!(resolve("/a/b", Path::new("/w")), PathBuf::from("/a/b"));
        assert_eq!(resolve("a/b", Path::new("/w")), PathBuf::from("/w/a/b"));
    }

    // Checks locate: the working directory first, then solution folders, the repository root, one repository below, and REPO:path.
    #[test]
    fn test_locate() {
        let t = crate::testutil::TempDir::new();
        let w = t.path().canonicalize().expect("canon");
        for d in ["api/.git", "api/src/Shop/Orders", "web/.git", "web/src"] {
            std::fs::create_dir_all(w.join(d)).expect("mkdir");
        }
        std::fs::write(w.join("api/src/Shop/Shop.sln"), "").expect("sln");
        std::fs::write(w.join("api/src/Shop/Orders/A.cs"), "").expect("cs");
        std::fs::write(w.join("api/README.md"), "").expect("md");
        std::fs::write(w.join("web/src/app.ts"), "").expect("ts");
        std::fs::write(w.join("web/README.md"), "").expect("md");
        let deep = w.join("api/src/Shop/Orders");
        assert_eq!(locate("A.cs", &deep), deep.join("A.cs"));
        assert_eq!(locate("Orders/A.cs", &deep), w.join("api/src/Shop/Orders/A.cs"));
        assert_eq!(locate("README.md", &deep), w.join("api/README.md"));
        assert_eq!(locate("api:src/Shop/Orders/A.cs", &deep), w.join("api/src/Shop/Orders/A.cs"));
        assert_eq!(locate("new.cs", &deep), deep.join("new.cs"));
        assert_eq!(locate("src/app.ts", &w), w.join("web/src/app.ts"));
        assert_eq!(locate("README.md", &w), w.join("README.md"));
        assert_eq!(locate("web:src/app.ts", &w), w.join("web/src/app.ts"));
        assert_eq!(locate("nope:x", &w), w.join("nope:x"));
        assert_eq!(locate("/abs/x", &w), PathBuf::from("/abs/x"));
    }

    // Checks the hex rendering.
    #[test]
    fn test_hex8_renders_four_bytes() {
        let mut h = [0u8; 32];
        h[0] = 0xab;
        h[3] = 0x01;
        assert_eq!(hex8(&h), "ab000001");
    }

    // Checks that short lines pass through unchanged.
    #[test]
    fn test_clip_short_line_unchanged() {
        assert_eq!(clip("hello", 1, 10), "hello");
    }

    // Checks clipping at the start, middle and end of a long line.
    #[test]
    fn test_clip_windows() {
        let line = "a".repeat(100);
        assert_eq!(clip(&line, 0, 30), format!("{}…", "a".repeat(30)));
        assert_eq!(clip(&line, 50, 30), format!("…{}…", "a".repeat(30)));
        assert_eq!(clip(&line, 99, 30), format!("…{}", "a".repeat(30)));
        assert_eq!(clip(&line, 80, 30), format!("…{}", "a".repeat(30)));
    }

    // Checks that clipping never splits a multi-byte character.
    #[test]
    fn test_clip_respects_char_boundaries() {
        let line = "é".repeat(60);
        let out = clip(&line, 51, 30);
        assert!(out.contains('é') && out.is_char_boundary(out.len()));
    }

    // Checks the window start and end both move to character boundaries.
    #[test]
    fn test_clip_moves_end_to_boundary() {
        let line = "é".repeat(60);
        assert_eq!(clip(&line, 0, 31), format!("{}…", "é".repeat(16)));
    }

    // Checks test files and directories rank after ordinary files, and deeper after shallower.
    #[test]
    fn test_rank() {
        assert_eq!(rank(Path::new("src/a.rs")), (false, 2));
        assert_eq!(rank(Path::new("src/deep/er/a.rs")), (false, 4));
        for t in [
            "tests/a.rs",
            "src/test/a.rs",
            "src/__tests__/a.js",
            "a_test.go",
            "a.test.ts",
            "b.spec.js",
            "test_a.py",
            "x/testdata/y.txt",
            "UnitTests.Core/Services/A.cs",
            "Core.Tests/A.cs",
            "src/FooTests.cs",
            "documentation/a.html",
            "README.md",
        ] {
            assert!(rank(Path::new(t)).0, "{t}");
        }
        assert!(!rank(Path::new("src/latest.rs")).0);
        assert!(!rank(Path::new("src/Latest.cs")).0 && !rank(Path::new("src/Tests.cs")).0);
        assert!(!rank(Path::new("N1.Core/Services/A.cs")).0);
        assert_eq!(rank(Path::new("/")), (false, 1));
    }
}
