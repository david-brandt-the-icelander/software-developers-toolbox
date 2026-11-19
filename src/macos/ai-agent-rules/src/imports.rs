// Finds the `@file` imports in a rule file, the way Claude Code reads them.
use std::fs;
use std::path::{Path, PathBuf};

// Resolves an import to an existing file: relative to the importing file's folder, `~/` under home, or absolute.
pub fn resolve(token: &str, dir: &Path, home: Option<&Path>) -> Option<PathBuf> {
    let path = match token.strip_prefix("~/") {
        Some(rest) => home?.join(rest),
        None => dir.join(token),
    };
    fs::canonicalize(path).ok().filter(|p| p.is_file())
}

// Returns the files a text imports, in order and once each: `@path` words outside fenced code blocks and code spans.
pub fn imports(text: &str, dir: &Path, home: Option<&Path>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut fenced = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        let plain: Vec<&str> = line.split('`').step_by(2).collect();
        for word in plain.join(" ").split_whitespace() {
            let Some(token) = word.strip_prefix('@') else { continue };
            let token = token.trim_end_matches(|c| ".,;:!?)".contains(c));
            if let Some(path) = resolve(token, dir, home).filter(|p| !out.contains(p)) {
                out.push(path);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // Makes a folder with a few files to import.
    fn tree(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("zalr-imp-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("docs")).expect("folder is creatable");
        fs::write(dir.join("docs/style.md"), "s").expect("writable");
        fs::write(dir.join("notes.md"), "n").expect("writable");
        fs::canonicalize(&dir).expect("folder exists")
    }

    // Imports are found in prose, once each, with trailing punctuation dropped, and home-relative ones need a home.
    #[test]
    fn test_imports_found() {
        let dir = tree("found");
        let text =
            "See @docs/style.md, and @notes.md.\nAgain @notes.md\nmail me@x.com @missing.md @~/notes.md\n";
        assert_eq!(imports(text, &dir, None), vec![dir.join("docs/style.md"), dir.join("notes.md")]);
        assert_eq!(imports("@~/notes.md", Path::new("/"), Some(&dir)), vec![dir.join("notes.md")]);
        assert_eq!(imports("@docs", &dir, None), Vec::<PathBuf>::new());
    }

    // Imports inside code blocks and code spans are ignored.
    #[test]
    fn test_imports_skip_code() {
        let dir = tree("code");
        let text = "```\n@notes.md\n```\nuse `@notes.md` here\n";
        assert!(imports(text, &dir, None).is_empty());
    }
}
