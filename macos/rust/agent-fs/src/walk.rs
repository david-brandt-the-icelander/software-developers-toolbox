// Shared directory walking: ignore-aware builders, dependency stamps and path display.
use crate::fsio;
use crate::state::{Dep, Res};
use ignore::overrides::OverrideBuilder;
use ignore::WalkBuilder;
use std::path::Path;

// Build a walker over the roots honouring .gitignore, globs and depth.
pub fn builder(
    roots: &[String],
    globs: &[String],
    hidden: bool,
    no_ignore: bool,
    depth: Option<usize>,
) -> Res<WalkBuilder> {
    let roots: Vec<&str> =
        if roots.is_empty() { vec!["."] } else { roots.iter().map(String::as_str).collect() };
    for root in &roots {
        if !Path::new(root).exists() {
            return Err(format!("{root}: no such file or directory"));
        }
    }
    let mut wb = WalkBuilder::new(roots[0]);
    for root in &roots[1..] {
        wb.add(root);
    }
    if !globs.is_empty() {
        let mut ob = OverrideBuilder::new(".");
        for glob in globs {
            ob.add(glob).map_err(|e| format!("glob {glob}: {e}"))?;
        }
        wb.overrides(ob.build().map_err(|e| e.to_string())?);
    }
    wb.hidden(!hidden).ignore(!no_ignore).git_ignore(!no_ignore).git_exclude(!no_ignore);
    wb.git_global(!no_ignore).parents(!no_ignore).require_git(false).max_depth(depth);
    wb.filter_entry(|e| e.file_name() != ".git");
    Ok(wb)
}

// Return the stamps of a directory's ignore files, which change what a walk yields.
pub fn ignore_file_deps(dir: &Path) -> Vec<Dep> {
    [".gitignore", ".ignore"]
        .iter()
        .map(|name| dir.join(name))
        .map(|p| {
            let id = fsio::ident_of(&p);
            (p, id)
        })
        .collect()
}

// Return a path as text without a leading "./".
pub fn display(path: &Path) -> String {
    let text = path.to_string_lossy();
    text.strip_prefix("./").unwrap_or(&text).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Display drops only a leading "./".
    #[test]
    fn test_display_strips_dot_slash() {
        assert_eq!(display(Path::new("./a/b")), "a/b");
        assert_eq!(display(Path::new("a/./b")), "a/./b");
    }

    // A missing root is reported before walking.
    #[test]
    fn test_builder_missing_root() {
        assert!(builder(&["/definitely/not/here".to_string()], &[], false, false, None).is_err());
    }
}
