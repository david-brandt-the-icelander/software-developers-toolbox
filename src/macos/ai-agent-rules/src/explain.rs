// `explain PATH`: lists each rule, import and skill file that applies to a path and why, and each file found on the way that does not apply and why.
use super::{json_str, trace, Target, Traced, Why};
use std::fs;
use std::path::Path;

// Says where a folder is relative to the path: the home folder, the path itself, its folder, or how many folders up.
fn level(dir: &Path, t: &Target, home: Option<&Path>) -> String {
    if home == Some(dir) && !t.dir.starts_with(dir) {
        return "the home folder".to_string();
    }
    match (t.dir.components().count().saturating_sub(dir.components().count()), t.is_dir) {
        (0, true) => "the path itself".to_string(),
        (0, false) => "the path's folder".to_string(),
        (1, _) => "1 folder up".to_string(),
        (n, _) => format!("{n} folders up"),
    }
}

// Says in words why a file applies or not.
fn reason(why: &Why, t: &Target, home: Option<&Path>) -> String {
    let at = |d: &Path| format!("in {} ({})", d.display(), level(d, t, home));
    match why {
        Why::Folder(d) => at(d),
        Why::Glob(d, g) => format!("{}, paths: `{g}` matches", at(d)),
        Why::Import(p) => format!("imported by {}", p.display()),
        Why::NoGlob(d, globs) => format!("{}, but no paths: glob matches (`{}`)", at(d), globs.join("`, `")),
        Why::Purpose => "marked only for other purposes".to_string(),
        Why::Repeat(p) => format!("the same file as {}", p.display()),
        Why::OnlyImports => "only imports files that are listed".to_string(),
    }
}

// Formats a line ranges suffix such as ` (lines 3-4, 9-9)`, or nothing for a whole file.
fn ranges(lines: &[(usize, usize)]) -> String {
    if lines.is_empty() {
        return String::new();
    }
    let parts: Vec<String> = lines.iter().map(|(a, b)| format!("{a}-{b}")).collect();
    format!(" (lines {})", parts.join(", "))
}

// Formats one file as a line: applies or skipped, its kind, its path and the reason.
fn line(x: &Traced, t: &Target, home: Option<&Path>) -> String {
    let verdict = if x.why.applies() { "applies" } else { "skipped" };
    let f = &x.found;
    let why = reason(&x.why, t, home);
    format!("{verdict} {} {}{}: {why}\n", f.kind.name(), f.path.display(), ranges(&f.lines))
}

// Quotes an optional path or text as JSON, or null.
fn opt(v: Option<String>) -> String {
    v.map_or("null".to_string(), |s| json_str(&s))
}

// Formats one file as a JSON object: file, kind, applies, reason, folder, glob, importedBy and lines.
fn object(x: &Traced, t: &Target, home: Option<&Path>) -> String {
    let show = |p: &Path| Some(p.to_string_lossy().into_owned());
    let (folder, glob, by) = match &x.why {
        Why::Folder(d) | Why::NoGlob(d, _) => (show(d), None, None),
        Why::Glob(d, g) => (show(d), Some(g.clone()), None),
        Why::Import(p) => (None, None, show(p)),
        _ => (None, None, None),
    };
    let lines: Vec<String> = x.found.lines.iter().map(|(a, b)| format!("[{a},{b}]")).collect();
    format!(
        "{{\"file\":{},\"kind\":\"{}\",\"applies\":{},\"reason\":{},\"folder\":{},\"glob\":{},\"importedBy\":{},\"lines\":[{}]}}",
        json_str(&x.found.path.to_string_lossy()),
        x.found.kind.name(),
        x.why.applies(),
        json_str(&reason(&x.why, t, home)),
        opt(folder),
        opt(glob),
        opt(by),
        lines.join(",")
    )
}

// Runs explain and returns one line per file found (or a JSON array) and exit 0, or 66 when PATH is missing.
pub fn run(path: &Path, home: Option<&Path>, wanted: &[String], top: &Path, as_json: bool) -> (String, i32) {
    let (t, all) = match trace(path, home, wanted, top) {
        Ok(r) => r,
        Err(e) => return (format!("{e}\n"), 66),
    };
    let home = home.and_then(|h| fs::canonicalize(h).ok());
    let home = home.as_deref();
    if as_json {
        let items: Vec<String> = all.iter().map(|x| object(x, &t, home)).collect();
        return (format!("[{}]\n", items.join(",\n")), 0);
    }
    if all.is_empty() {
        return (format!("no rule or skill files found for {}\n", t.path.display()), 0);
    }
    (all.iter().map(|x| line(x, &t, home)).collect(), 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{put, run_in, strs, temp, write};

    // Explains a path with a home and purposes, searching no higher than root, as text lines relative to root.
    fn lines(root: &Path, path: &str, home: &Path, wanted: &[&str]) -> Vec<String> {
        let (text, code) = run(&root.join(path), Some(home), &strs(wanted), root, false);
        assert_eq!(code, 0);
        let prefix = format!("{}/", root.display());
        text.lines().map(|l| l.replace(&prefix, "")).collect()
    }

    // Every reason is given: folder levels, a matching and a failing glob, imports, purposes, repeats and files that only import.
    #[test]
    fn test_explain_reasons() {
        let root = temp("exp");
        let home = root.join("home");
        put(&home.join("agents/skills/h/SKILL.md"));
        write(&root.join("AGENTS.md"), "top\n");
        write(&root.join("CLAUDE.md"), "@AGENTS.md\n");
        write(&root.join(".claude/rules/ts.md"), "---\npaths: src/**/*.ts\n---\n");
        write(&root.join(".claude/rules/docs.md"), "---\npaths: docs/**, *.md\n---\n");
        write(&root.join(".claude/rules/p.md"), "#zazzle-purpose:other\n");
        write(&root.join(".claude/rules/i.md"), "@../../other.md\n");
        write(
            &root.join("src/AGENTS.md"),
            "@../notes.md\n#zazzle-purpose-begin:a\nmine\n#zazzle-purpose-end:a\n",
        );
        put(&root.join("notes.md"));
        write(&root.join("other.md"), "#zazzle-purpose:other\n");
        put(&root.join("src/x/a.ts"));
        std::os::unix::fs::symlink(root.join("AGENTS.md"), root.join("src/x/CLAUDE.md")).expect("symlink");
        let r = root.display();
        assert_eq!(
            lines(&root, "src/x/a.ts", &home, &["a"]),
            vec![
                format!("applies skill home/agents/skills/h/SKILL.md: in home (the home folder)"),
                format!("applies rule AGENTS.md: in {r} (2 folders up)"),
                "skipped rule CLAUDE.md: only imports files that are listed".to_string(),
                format!("skipped rule .claude/rules/docs.md: in {r} (2 folders up), but no paths: glob matches (`docs/**`, `*.md`)"),
                format!("applies rule .claude/rules/i.md: in {r} (2 folders up)"),
                "skipped import other.md: marked only for other purposes".to_string(),
                "skipped rule .claude/rules/p.md: marked only for other purposes".to_string(),
                format!("applies rule .claude/rules/ts.md: in {r} (2 folders up), paths: `src/**/*.ts` matches"),
                format!("applies rule src/AGENTS.md (lines 3-3): in src (1 folder up)"),
                "skipped rule src/x/CLAUDE.md: the same file as AGENTS.md".to_string(),
            ]
        );
        let all = lines(&root, "src", &home, &[]);
        assert!(all.contains(&"applies rule src/AGENTS.md: in src (the path itself)".to_string()));
        assert!(all.contains(&"applies import notes.md: imported by src/AGENTS.md".to_string()));
        let file = lines(&root, "src/AGENTS.md", &home, &[]);
        assert!(file.contains(&"applies rule src/AGENTS.md: in src (the path's folder)".to_string()));
    }

    // A home folder above the path keeps its place as a parent folder, and a path with nothing found says so.
    #[test]
    fn test_explain_home_above_and_nothing_found() {
        let root = temp("exp-home");
        let home = root.join("h");
        put(&home.join("AGENTS.md"));
        fs::create_dir_all(home.join("p/q")).expect("folders");
        assert_eq!(
            lines(&root, "h/p/q", &home, &[]),
            vec![format!("applies rule h/AGENTS.md: in h (2 folders up)")]
        );
        let empty = temp("exp-empty");
        let (text, _) = run(&empty, None, &[], &empty, false);
        assert_eq!(text, format!("no rule or skill files found for {}\n", empty.display()));
    }

    // JSON gives each file's fields, with null for those that do not apply to its reason.
    #[test]
    fn test_explain_json() {
        let root = temp("exp-json");
        write(&root.join("AGENTS.md"), "@n.md\n#zazzle-purpose-begin:a\nx\n#zazzle-purpose-end:a\n");
        put(&root.join("n.md"));
        write(&root.join(".claude/rules/g.md"), "---\npaths: \"**\"\n---\n");
        write(&root.join(".claude/rules/o.md"), "#zazzle-purpose:other\n");
        let dir = root.to_str().expect("utf8");
        let (json, code) = run_in(&root, &["explain", dir, "--json", "--purpose", "a"]);
        assert_eq!(code, 0);
        let items: Vec<&str> =
            json.trim_end().trim_start_matches('[').trim_end_matches(']').split(",\n").collect();
        let file = |n: &str| json_str(&root.join(n).to_string_lossy());
        assert_eq!(
            items[0],
            format!("{{\"file\":{},\"kind\":\"rule\",\"applies\":true,\"reason\":{},\"folder\":{},\"glob\":null,\"importedBy\":null,\"lines\":[[3,3]]}}", file("AGENTS.md"), json_str(&format!("in {dir} (the path itself)")), json_str(dir))
        );
        assert!(items[1].contains("\"glob\":\"**\""));
        assert!(items[2].contains("\"applies\":false") && items[2].contains("\"folder\":null"));
        assert_eq!(items.len(), 3);
        let (all, _) = run_in(&root, &["explain", dir, "--json"]);
        assert!(all.contains(&format!("\"importedBy\":{}", file("AGENTS.md"))));
        assert_eq!(run_in(&root, &["explain", root.join("nope").to_str().expect("utf8")]).1, 66);
    }
}
