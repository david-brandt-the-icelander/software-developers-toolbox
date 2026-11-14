// Terse view of Nim files, line by line (no tree-sitter grammar for Nim is good enough): hides runs of top-level imports, single-line logging statements and region comments, keeping source line numbers.
use std::path::Path;

const LEVELS: [&str; 7] = ["debug", "info", "notice", "warn", "error", "fatal", "trace"];

// True for a Nim source or script path.
pub fn is_nim(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "nim" | "nims"))
}

// Returns the view of a file, or None when nothing would be shortened: a header, then `N` and `A-B` lines.
pub fn view(shown: &str, text: &str) -> Option<String> {
    let src: Vec<&str> = text.lines().collect();
    let code = code_lines(&src);
    let mut out = Vec::new();
    let mut steps: Vec<(&str, usize)> = Vec::new();
    let mut count = |step: &'static str| match steps.iter_mut().find(|(s, _)| *s == step) {
        Some((_, n)) => *n += 1,
        None => steps.push((step, 1)),
    };
    let mut i = 0;
    while i < src.len() {
        let t = src[i].trim();
        if let Some((end, n)) = import_run(&src, &code, i) {
            out.push(format!("{}-{}\t⋯ {n} import(s)", i + 1, end + 1));
            count("imports");
            i = end + 1;
        } else if code[i] && (is_region(t) || is_logging(&src, i)) {
            count(if is_region(t) { "regions" } else { "log statements" });
            i += 1;
        } else {
            let blank_again = t.is_empty() && out.last().is_some_and(|l: &String| l.ends_with('\t'));
            if !blank_again {
                out.push(format!("{}\t{}", i + 1, src[i].trim_end()));
            }
            i += 1;
        }
    }
    if steps.is_empty() {
        return None;
    }
    let steps: Vec<String> = steps.iter().map(|(s, n)| format!("{n} {s}")).collect();
    Some(format!("{shown} {} lines; terse: {}\n{}", src.len(), steps.join(", "), out.join("\n")))
}

// Marks the lines that are code, not inside a triple-quoted string.
fn code_lines(src: &[&str]) -> Vec<bool> {
    let mut inside = false;
    src.iter()
        .map(|l| {
            let starts_inside = inside;
            if l.matches("\"\"\"").count() % 2 == 1 {
                inside = !inside;
            }
            !starts_inside && !inside
        })
        .collect()
}

// Returns the last line and statement count of a run of two or more lines of top-level imports starting at line i.
fn import_run(src: &[&str], code: &[bool], i: usize) -> Option<(usize, usize)> {
    let (mut end, mut n, mut j) = (i, 0, i);
    while j < src.len() && code[j] && is_import(src[j]) {
        let last = statement_end(src, j)?;
        n += 1;
        end = last;
        j = last + 1;
    }
    (end > i).then_some((end, n))
}

// True for a top-level `import`, `from .. import` or `include` line.
fn is_import(line: &str) -> bool {
    !line.starts_with(char::is_whitespace)
        && (line.starts_with("import ")
            || line.starts_with("include ")
            || line.starts_with("from ") && line.contains(" import "))
}

// Returns the last line of a statement starting at line i, following open brackets and trailing commas; None if it never closes.
fn statement_end(src: &[&str], i: usize) -> Option<usize> {
    let mut depth = 0i32;
    for (j, line) in src.iter().enumerate().skip(i) {
        let code = line.split('#').next().unwrap_or("");
        depth += code.matches(['(', '[', '{']).count() as i32 - code.matches([')', ']', '}']).count() as i32;
        if depth <= 0 && !code.trim_end().ends_with(',') {
            return Some(j);
        }
    }
    None
}

// True for a comment that only marks a region: `# region x`, `#endregion`.
fn is_region(t: &str) -> bool {
    let c = t.strip_prefix('#').map(str::trim_start);
    let word = c.map(|c| c.strip_prefix("end").unwrap_or(c));
    word.is_some_and(|w| {
        w.strip_prefix("region").is_some_and(|r| r.is_empty() || r.starts_with(char::is_whitespace))
    })
}

// True for an indented line that is one whole logging statement with no side effects: `debug "x"`, `info("x")`, `log.warn x`.
fn is_logging(src: &[&str], i: usize) -> bool {
    let line = src[i];
    let t = line.trim();
    let indent = line.len() - line.trim_start().len();
    let head = t.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '.')).next().unwrap_or("");
    let level =
        head.rsplit_once('.').map_or(head, |(r, m)| if matches!(r, "log" | "logger" | "L") { m } else { "" });
    let rest = &t[head.len()..];
    let called = rest.starts_with(' ') || rest.starts_with('(');
    let code: String = t.split('"').step_by(2).collect();
    let effects =
        ["=", "inc(", "dec(", "add(", "del(", "discard"].iter().any(|e| code.replace("==", "").contains(e));
    let next = src[i + 1..].iter().find(|l| !l.trim().is_empty());
    let continues = next.is_some_and(|l| l.len() - l.trim_start().len() > indent)
        || t.ends_with([',', '(', '[', '{', '&', '+', '='])
        || statement_end(src, i) != Some(i);
    indent > 0 && LEVELS.contains(&level) && called && !effects && !continues
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    const NIM: &str = r#"import std/strutils
import std/[tables,
  sequtils]
from os import getEnv

# region loading
proc load(api: Api, id: int): int =
  debug "loading ", id
  log.info("x")
  echo "hi"
  let responseBody = api.get(id)
  info "count ", responseBody.len
  error("failed")
  info("x " & $id,
    "y")
  warn "n=", inc(n)
  let doc = """
  debug "inside a string"
  """
  result = responseBody.len
# endregion
"#;

    // Checks import runs across a multi-line import, logging, regions, and what stays: echo, continued lines, side effects, strings.
    #[test]
    fn test_view_nim() {
        let v = view("a.nim", NIM).expect("view");
        assert!(v.starts_with("a.nim 21 lines; terse: 1 imports, 2 regions, 4 log statements\n"), "{v}");
        let lines: Vec<&str> = v.lines().skip(1).collect();
        for want in [
            "1-4\t⋯ 3 import(s)",
            "10\t  echo \"hi\"",
            "14\t  info(\"x \" & $id,",
            "16\t  warn \"n=\", inc(n)",
            "18\t  debug \"inside a string\"",
        ] {
            assert!(lines.contains(&want), "{want} in {v}");
        }
        assert!(!lines
            .iter()
            .any(|l| ["6\t", "8\t", "9\t", "12\t", "13\t", "21\t"].iter().any(|k| l.starts_with(k))));
        assert!(view("a.nim", "proc f() =\n  echo 1\n").is_none());
        assert!(view("a.nim", "proc f() =\n  debug \"x\",\n    y\n").is_none());
        assert!(view("a.nim", "info \"top\"\nproc f() =\n  debug \"x\",\n  y\n").is_none());
        assert!(view("a.nim", "let s = \"\"\"\nimport a\nimport b\n\"\"\"\n").is_none());
        let edges = "include a\nimport b,\n  c\n\n\nproc f() =\n  debug \"x\" &\n    \"y\"\n  info \"z\"\n";
        let v = view("a.nim", edges).expect("view");
        assert!(
            v.contains("1-3\t⋯ 2 import(s)\n4\t\n6\tproc") && v.contains("7\t  debug") && !v.contains("9\t"),
            "{v}"
        );
        assert!(view("a.nim", "import a\nimport (b\n").is_none());
        assert!(is_nim(Path::new("a/B.NIMS")) && !is_nim(Path::new("a.nimble")));
    }
}
