// Terse view of COBOL files, line by line (no maintained tree-sitter grammar): drops fixed-format sequence numbers (columns 1-6) and identification areas (73-80), collapses comment blocks of 3+ lines and runs of COPY statements, and hides debugging lines, keeping source line numbers.
use crate::markup::Plan;
use std::path::Path;

const LONG_COMMENT: usize = 3;

// True for a COBOL source or copybook path.
pub fn is_cobol(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "cbl" | "cob" | "cpy" | "cobol"))
}

// Returns the view of a file, or None when nothing would be shortened: a header, then `N`, `N~` and `A-B` lines.
pub fn view(shown: &str, text: &str) -> Option<String> {
    let src: Vec<&str> = text.lines().collect();
    let fixed = is_fixed(&src);
    let code: Vec<String> = src.iter().map(|l| if fixed { area(l) } else { l.to_string() }).collect();
    let mut plan = Plan { lines: Vec::new(), steps: Vec::new() };
    let mut i = 0;
    while i < src.len() {
        let comments = (i..src.len()).take_while(|&j| is_comment(src[j], fixed)).count();
        let copies = (i..src.len()).take_while(|&j| is_copy(&code[j])).count();
        if comments >= LONG_COMMENT {
            plan.hide(i, i + comments - 1, format!("comment, {comments} line(s)"), "comments");
            i += comments;
        } else if copies >= 2 {
            plan.hide(i, i + copies - 1, format!("{copies} copybook(s)"), "copybooks");
            i += copies;
        } else if fixed && matches!(src[i].chars().nth(6), Some('D' | 'd')) {
            plan.count("debugging lines");
            i += 1;
        } else {
            let changed = fixed && code[i].trim_end() != src[i].trim_end();
            if changed {
                plan.count("sequence areas");
            }
            plan.keep(i, &code[i], changed);
            i += 1;
        }
    }
    if plan.steps.is_empty() {
        return None;
    }
    let steps: Vec<String> = plan.steps.iter().map(|(s, n)| format!("{n} {s}")).collect();
    Some(format!("{shown} {} lines; terse: {}\n{}", src.len(), steps.join(", "), plan.lines.join("\n")))
}

// True when most non-blank lines look fixed-format: six sequence characters (digits or blanks) and an indicator in column 7.
fn is_fixed(src: &[&str]) -> bool {
    let lines: Vec<&&str> = src.iter().filter(|l| !l.trim().is_empty()).collect();
    let fixed = lines
        .iter()
        .filter(|l| {
            let c: Vec<char> = l.chars().take(7).collect();
            c.len() == 7
                && c[..6].iter().all(|c| c.is_ascii_digit() || *c == ' ')
                && matches!(c[6], ' ' | '*' | '/' | '-' | 'D' | 'd')
        })
        .count();
    !lines.is_empty() && fixed * 10 >= lines.len() * 9
}

// Returns a fixed-format line without its sequence area (columns 1-6) and identification area (columns 73-80), keeping columns 7-72 as they are.
fn area(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let end = chars.len().min(72);
    let body: String = chars.get(6..end).map(|c| c.iter().collect()).unwrap_or_default();
    format!("      {}", body.trim_end())
}

// True for a comment line: `*` or `/` in column 7 (fixed format) or a line starting with `*>`.
fn is_comment(line: &str, fixed: bool) -> bool {
    fixed && matches!(line.chars().nth(6), Some('*' | '/')) || line.trim_start().starts_with("*>")
}

// True for a COPY statement on one line: `COPY name.` or `COPY name REPLACING ...`.
fn is_copy(line: &str) -> bool {
    let t = line.trim().to_ascii_uppercase();
    t.starts_with("COPY ") && t.ends_with('.')
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    // Checks a fixed-format program: comment banners, copybooks, debugging lines and sequence areas, and that short comments stay.
    #[test]
    fn test_view_fixed() {
        let src = "000100* PROGRAM HEADER\n000200* WRITTEN 1987\n000300*\n000400 IDENTIFICATION DIVISION.                                         PAYROLL1\n000500 PROGRAM-ID. PAY.\n000600 DATA DIVISION.\n000700     COPY CUSTREC.\n000800     COPY INVREC.\n000900* ONE NOTE\n001000D    DISPLAY 'DEBUG'.\n001100     STOP RUN.\n";
        let v = view("pay.cbl", src).expect("view");
        assert_eq!(
            v,
            "pay.cbl 11 lines; terse: 1 comments, 5 sequence areas, 1 copybooks, 1 debugging lines\n1-3\t⋯ comment, 3 line(s)\n4~\t       IDENTIFICATION DIVISION.\n5~\t       PROGRAM-ID. PAY.\n6~\t       DATA DIVISION.\n7-8\t⋯ 2 copybook(s)\n9~\t      * ONE NOTE\n11~\t           STOP RUN."
        );
    }

    // Checks free-format comments collapse and a file with nothing to shorten comes back as None.
    #[test]
    fn test_view_free() {
        let src = "*> a\n*> b\n*> c\nIDENTIFICATION DIVISION.\nPROGRAM-ID. X.\n";
        assert!(view("x.cob", src)
            .is_some_and(|v| v.contains("1-3\t⋯ comment, 3 line(s)\n4\tIDENTIFICATION DIVISION.")));
        assert!(view("x.cob", "IDENTIFICATION DIVISION.\n*> one\n").is_none());
        assert!(view("x.cob", "").is_none());
        assert!(
            is_cobol(Path::new("A.CBL")) && is_cobol(Path::new("rec.cpy")) && !is_cobol(Path::new("a.c"))
        );
    }
}
