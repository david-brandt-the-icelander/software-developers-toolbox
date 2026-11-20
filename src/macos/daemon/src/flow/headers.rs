// `zazzle flow headers`: license headers are a tool's job: every source file the policy's `license` covers (changed ones, or all tracked files) must start with the header in the file's own comment style, after a shebang; `--fix` adds it where it is missing. Generated files are left alone.
use super::policy::{License, Policy};
use super::repo;
use std::path::Path;

// The line comment a file's extension uses, or None for files that get no header.
pub fn style(path: &str) -> Option<&'static str> {
    let ext = path.rsplit_once('.').map(|(_, e)| e.to_lowercase())?;
    match ext.as_str() {
        "rs" | "c" | "h" | "cc" | "cpp" | "hpp" | "cs" | "java" | "js" | "jsx" | "ts" | "tsx" | "go"
        | "swift" | "kt" | "scala" | "dart" | "m" => Some("//"),
        "py" | "sh" | "bash" | "rb" | "pl" | "toml" | "yaml" | "yml" | "r" | "ex" | "exs" => Some("#"),
        "sql" | "lua" | "hs" => Some("--"),
        _ => None,
    }
}

// The header as comment lines in a style.
pub fn header(lic: &License, style: &str) -> String {
    lic.lines
        .iter()
        .map(|l| if l.is_empty() { format!("{style}\n") } else { format!("{style} {l}\n") })
        .collect()
}

// Tells whether a file's text starts with the header (after a shebang line).
pub fn has_header(text: &str, lic: &License, style: &str) -> bool {
    let body = if text.starts_with("#!") { text.split_once('\n').map_or("", |(_, r)| r) } else { text };
    body.starts_with(&header(lic, style))
}

// Adds the header at the top of a file's text, after a shebang line, with a blank line after it.
pub fn add_header(text: &str, lic: &License, style: &str) -> String {
    let h = header(lic, style);
    match text.strip_prefix("#!").and_then(|_| text.split_once('\n')) {
        Some((shebang, rest)) => format!("{shebang}\n{h}\n{rest}"),
        None => format!("{h}\n{text}"),
    }
}

// The files the header covers among `files`: inside its paths, with a comment style, not a skipped extension, not generated, and present.
fn covered(top: &Path, p: &Policy, lic: &License, files: &[String]) -> Vec<(String, &'static str)> {
    files
        .iter()
        .filter(|f| lic.paths.is_empty() || repo::under(f, &lic.paths))
        .filter(|f| !lic.skip.iter().any(|s| f.ends_with(&format!(".{}", s.trim_start_matches('.')))))
        .filter(|f| !p.is_generated(f) && top.join(f).is_file())
        .filter_map(|f| style(f).map(|s| (f.clone(), s)))
        .collect()
}

// Checks (and with `fix`, adds) the headers of the changed files against `base`, or of every tracked file; returns the report and whether every covered file has it.
pub fn headers(top: &Path, p: &Policy, base: Option<&str>, fix: bool) -> Result<(String, bool), String> {
    let Some(lic) = &p.license else {
        return Ok((
            "headers: no license header is configured (add \"license\" to .zazzle/flow.json)\n".into(),
            true,
        ));
    };
    let files: Vec<String> = match base {
        Some(b) => repo::changed(top, b, &repo::worktree_tree(top)?)?,
        None => repo::git(top, &["ls-files"])?.lines().map(str::to_string).collect(),
    };
    let files = covered(top, p, lic, &files);
    let mut missing = Vec::new();
    for (f, s) in &files {
        let text = std::fs::read_to_string(top.join(f)).unwrap_or_default();
        if has_header(&text, lic, s) {
            continue;
        }
        if fix {
            std::fs::write(top.join(f), add_header(&text, lic, s)).map_err(|e| format!("{f}: {e}"))?;
        }
        missing.push(f.clone());
    }
    let out = match (missing.is_empty(), fix) {
        (true, _) => format!("headers: all {} covered file(s) have the license header\n", files.len()),
        (false, true) => format!("headers: added the license header to {}\n", missing.join(", ")),
        (false, false) => {
            format!("headers: missing in {} (`zazzle flow headers --fix` adds it)\n", missing.join(", "))
        }
    };
    Ok((out, missing.is_empty() || fix))
}
