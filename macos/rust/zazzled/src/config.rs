// Terse view of INI-style files (.ini, .cfg, .properties, .editorconfig) and Dockerfiles, line by line: collapses comment blocks of 4+ lines, Dockerfile runs of 6+ package names in a continued RUN and runs of 3+ LABEL lines, and cuts long values, keeping source line numbers.
use crate::data::shorten_words;
use crate::markup::Plan;
use std::path::Path;

const LONG_COMMENT: usize = 4;
const LONG_PACKAGES: usize = 6;
const LONG_LABELS: usize = 3;

// The file kinds the view handles.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    Ini,
    Dockerfile,
}

// Returns the kind of a path: INI by extension or name, Dockerfile by name (`Dockerfile`, `Containerfile`, `Dockerfile.dev`, `app.dockerfile`).
pub fn kind_of(path: &Path) -> Option<Kind> {
    let name = path.file_name()?.to_str()?.to_ascii_lowercase();
    let ext = name.rsplit_once('.').map(|(_, e)| e);
    if name == "dockerfile"
        || name == "containerfile"
        || name.starts_with("dockerfile.")
        || ext == Some("dockerfile")
    {
        return Some(Kind::Dockerfile);
    }
    matches!(ext, Some("ini" | "cfg" | "properties" | "editorconfig")).then_some(Kind::Ini)
}

// Returns the view of a file, or None when nothing would be shortened: a header, then `N`, `N~` and `A-B` lines.
pub fn view(shown: &str, text: &str, kind: Kind) -> Option<String> {
    let src: Vec<&str> = text.lines().collect();
    let mut plan = Plan { lines: Vec::new(), steps: Vec::new() };
    let mut i = 0;
    while i < src.len() {
        if let Some(end) = comment_end(&src, i, kind) {
            plan.hide(i, end, format!("comment, {} line(s)", end - i + 1), "comments");
            i = end + 1;
        } else if let Some(end) = labels(&src, i).filter(|_| kind == Kind::Dockerfile) {
            plan.hide(i, end, format!("{} label line(s)", end - i + 1), "labels");
            i = end + 1;
        } else if let Some((a, b)) = packages(&src, i).filter(|_| kind == Kind::Dockerfile) {
            plan.keep(i, src[i], false);
            plan.hide(a, b, format!("{} package(s)", b - a + 1), "package lists");
            i = b + 1;
        } else {
            let (s, n) = shorten_words(src[i]);
            plan.add("long values", n);
            plan.keep(i, &s, n > 0);
            i += 1;
        }
    }
    plan.steps.retain(|(_, n)| *n > 0);
    if plan.steps.is_empty() {
        return None;
    }
    let steps: Vec<String> = plan.steps.iter().map(|(s, n)| format!("{n} {s}")).collect();
    Some(format!("{shown} {} lines; terse: {}\n{}", src.len(), steps.join(", "), plan.lines.join("\n")))
}

// True for a comment line: `#` or `;` in INI files, `#` in Dockerfiles (not a `# syntax=` or `# escape=` directive).
fn is_comment(line: &str, kind: Kind) -> bool {
    let t = line.trim_start();
    let directive = t.starts_with("# syntax=") || t.starts_with("# escape=");
    t.starts_with('#') && !directive || kind == Kind::Ini && t.starts_with(';')
}

// Returns the last line of a comment block of 4+ lines that starts at line i.
fn comment_end(src: &[&str], i: usize, kind: Kind) -> Option<usize> {
    let n = src[i..].iter().take_while(|l| is_comment(l, kind)).count();
    (n >= LONG_COMMENT && (i == 0 || !is_comment(src[i - 1], kind))).then(|| i + n - 1)
}

// Returns the last line of a run of 3+ LABEL instructions, or of one LABEL continued over 3+ lines, starting at line i.
fn labels(src: &[&str], i: usize) -> Option<usize> {
    let mut end = i;
    let mut j = i;
    while j < src.len() && src[j].trim_start().to_ascii_uppercase().starts_with("LABEL ") {
        end = j;
        while src[end].trim_end().ends_with('\\') && end + 1 < src.len() {
            end += 1;
        }
        j = end + 1;
    }
    (j > i && end + 1 - i >= LONG_LABELS).then_some(end)
}

// Returns the lines of 6+ package names, one per continued line, after the line i that opens them (`apt-get install -y \`).
fn packages(src: &[&str], i: usize) -> Option<(usize, usize)> {
    if !src[i].trim_end().ends_with('\\') {
        return None;
    }
    let is_package = |l: &str| {
        let t = l.trim();
        let name = t.trim_end_matches('\\').trim();
        t.ends_with('\\')
            && !name.is_empty()
            && !name.contains(char::is_whitespace)
            && !name.starts_with(['-', '&', '|', ';'])
    };
    let n = src[i + 1..].iter().take_while(|l| is_package(l)).count();
    (n >= LONG_PACKAGES).then(|| (i + 1, i + n))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    // Checks file names map to kinds.
    #[test]
    fn test_kind_of() {
        assert_eq!(kind_of(Path::new("a/Dockerfile")), Some(Kind::Dockerfile));
        assert_eq!(kind_of(Path::new("Dockerfile.dev")), Some(Kind::Dockerfile));
        assert_eq!(kind_of(Path::new("app.dockerfile")), Some(Kind::Dockerfile));
        assert_eq!(kind_of(Path::new("Containerfile")), Some(Kind::Dockerfile));
        assert_eq!(kind_of(Path::new("php.INI")), Some(Kind::Ini));
        assert_eq!(kind_of(Path::new("setup.cfg")), Some(Kind::Ini));
        assert_eq!(kind_of(Path::new(".editorconfig")), Some(Kind::Ini));
        assert_eq!(kind_of(Path::new("a.conf")), None);
    }

    // Checks INI comment blocks and long values collapse, and short comments and values stay.
    #[test]
    fn test_view_ini() {
        let long = "x".repeat(130);
        let src = format!("; licence\n; a\n; b\n# c\n[core]\n; one\nkey = value\nblob = {long}\n");
        let v = view("a.ini", &src, Kind::Ini).expect("view");
        assert_eq!(
            v,
            format!("a.ini 8 lines; terse: 1 comments, 1 long values\n1-4\t⋯ comment, 4 line(s)\n5\t[core]\n6\t; one\n7\tkey = value\n8~\tblob = {}…", "x".repeat(40))
        );
        assert!(view("a.ini", "# a\n# b\n[x]\ny = 1\n", Kind::Ini).is_none());
    }

    // Checks Dockerfile comment blocks, label runs and package lists collapse, and directives, short lists and commands stay.
    #[test]
    fn test_view_dockerfile() {
        let src = "# syntax=docker/dockerfile:1\n# a\n# b\n# c\n# d\nFROM debian\nLABEL a=1\nLABEL b=2 \\\n      c=3\nRUN apt-get update && apt-get install -y \\\n    curl \\\n    git \\\n    make \\\n    gcc \\\n    g++ \\\n    jq \\\n    && rm -rf /var/lib/apt/lists/*\nRUN pip install \\\n    a \\\n    b\nCMD [\"x\"]\n";
        let v = view("Dockerfile", src, Kind::Dockerfile).expect("view");
        assert_eq!(
            v,
            "Dockerfile 21 lines; terse: 1 comments, 1 labels, 1 package lists\n1\t# syntax=docker/dockerfile:1\n2-5\t⋯ comment, 4 line(s)\n6\tFROM debian\n7-9\t⋯ 3 label line(s)\n10\tRUN apt-get update && apt-get install -y \\\n11-16\t⋯ 6 package(s)\n17\t    && rm -rf /var/lib/apt/lists/*\n18\tRUN pip install \\\n19\t    a \\\n20\t    b\n21\tCMD [\"x\"]"
        );
        assert!(view("Dockerfile", "FROM a\nLABEL a=1\nRUN x\n", Kind::Dockerfile).is_none());
    }
}
