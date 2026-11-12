// Small helpers shared by commands: path display and resolution, hex, and line clipping.
use std::path::{Path, PathBuf};

pub const MAX_LINE: usize = 240;

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

// Renders the first four bytes of a hash as eight hex digits.
pub fn hex8(hash: &[u8; 32]) -> String {
    hash[..4].iter().map(|b| format!("{b:02x}")).collect()
}

// Ranks a path for display: ordinary files before tests, then shallower before deeper.
pub fn rank(path: &Path) -> (bool, usize) {
    const TEST_DIRS: [&str; 6] = ["test", "tests", "spec", "specs", "__tests__", "testdata"];
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let in_test_dir =
        path.components().any(|c| c.as_os_str().to_str().is_some_and(|s| TEST_DIRS.contains(&s)));
    let test_name =
        name.starts_with("test_") || ["_test.", ".test.", ".spec."].iter().any(|m| name.contains(m));
    (in_test_dir || test_name, path.components().count())
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
        ] {
            assert!(rank(Path::new(t)).0, "{t}");
        }
        assert!(!rank(Path::new("src/latest.rs")).0);
        assert_eq!(rank(Path::new("/")), (false, 1));
    }
}
