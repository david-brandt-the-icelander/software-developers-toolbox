// Terse view of SQL files, line by line (a SQL grammar would add 13 MB to the binary for two rules): hides region comments and collapses the middle rows of long `INSERT .. VALUES` lists, keeping source line numbers.
use std::path::Path;

const LONG_ROWS: usize = 6;

// True for a SQL path.
pub fn is_sql(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("sql"))
}

// Returns the view of a file, or None when nothing would be shortened: a header, then `N` and `A-B` lines.
pub fn view(shown: &str, text: &str) -> Option<String> {
    let src: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    let (mut regions, mut inserts) = (0, 0);
    let mut i = 0;
    while i < src.len() {
        let t = src[i].trim();
        if is_region(t) {
            regions += 1;
            i += 1;
        } else if let Some((first, last)) = rows(&src, i) {
            for (j, line) in src.iter().enumerate().take(first + 1).skip(i) {
                out.push(format!("{}\t{}", j + 1, line.trim_end()));
            }
            out.push(format!("{}-{}\t⋯ {} row(s)", first + 2, last, last - first - 1));
            out.push(format!("{}\t{}", last + 1, src[last].trim_end()));
            inserts += 1;
            i = last + 1;
        } else {
            let blank_again = t.is_empty() && out.last().is_some_and(|l: &String| l.ends_with('\t'));
            if !blank_again {
                out.push(format!("{}\t{}", i + 1, src[i].trim_end()));
            }
            i += 1;
        }
    }
    let steps: Vec<String> = [(regions, "regions"), (inserts, "insert rows")]
        .iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, s)| format!("{n} {s}"))
        .collect();
    if steps.is_empty() {
        return None;
    }
    Some(format!("{shown} {} lines; terse: {}\n{}", src.len(), steps.join(", "), out.join("\n")))
}

// True for a comment that only marks a region: `-- region x`, `--#endregion`, `/* region */`.
fn is_region(t: &str) -> bool {
    let c = t.strip_prefix("--").or_else(|| t.strip_prefix("/*").and_then(|c| c.strip_suffix("*/")));
    let word = c.map(|c| c.trim().trim_start_matches('#').trim_start());
    let word = word.map(|w| w.strip_prefix("end").unwrap_or(w));
    word.is_some_and(|w| {
        w.strip_prefix("region").is_some_and(|r| r.is_empty() || r.starts_with(char::is_whitespace))
    })
}

// Returns the lines of the first and last row of a long `VALUES` list that starts on the line after line i, when every row is one `(..),` line.
fn rows(src: &[&str], i: usize) -> Option<(usize, usize)> {
    let t = src[i].trim().to_ascii_uppercase();
    if !(t.starts_with("INSERT") && t.ends_with("VALUES")) {
        return None;
    }
    let mut last = i;
    for (j, line) in src.iter().enumerate().skip(i + 1) {
        let r = line.trim();
        if !one_group(r) || r.contains("--") || r.contains("/*") {
            return None;
        }
        last = j;
        if r.ends_with(';') || !r.ends_with(',') {
            break;
        }
    }
    (last - i >= LONG_ROWS && src[last].trim().ends_with([';', ')'])).then_some((i + 1, last))
}

// True for a line that is one parenthesized group, ignoring parentheses inside quotes, and an optional `,` or `;` after it.
fn one_group(r: &str) -> bool {
    let (mut depth, mut groups, mut quoted, mut after) = (0i32, 0, false, String::new());
    for c in r.chars() {
        match c {
            '\'' => quoted = !quoted,
            '(' if !quoted => {
                groups += usize::from(depth == 0);
                depth += 1;
            }
            ')' if !quoted => depth -= 1,
            _ if depth == 0 && !quoted => after.push(c),
            _ => {}
        }
    }
    r.starts_with('(') && groups == 1 && depth == 0 && !quoted && matches!(after.trim(), "" | "," | ";")
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    // Checks regions and a long row list collapse to its first and last rows, and that short, shared-line, multi-line or commented rows stay.
    #[test]
    fn test_view_sql() {
        let src = "-- region seed\nCREATE TABLE t (id INT);\n\n\nINSERT INTO t (id, name) VALUES\n  (1, 'a'),\n  (2, 'b'),\n  (3, 'c'),\n  (4, 'd'),\n  (5, 'e'),\n  (6, 'f;g');\n/* endregion */\nSELECT 1;\n";
        let v = view("a.sql", src).expect("view");
        assert_eq!(
            v,
            "a.sql 13 lines; terse: 2 regions, 1 insert rows\n2\tCREATE TABLE t (id INT);\n3\t\n5\tINSERT INTO t (id, name) VALUES\n6\t  (1, 'a'),\n7-10\t⋯ 4 row(s)\n11\t  (6, 'f;g');\n13\tSELECT 1;"
        );
        let kept = |body: &str| view("a.sql", &format!("insert into t values\n{body}\n")).is_none();
        assert!(kept("  (1),\n  (2),\n  (3);"));
        assert!(kept("  (1), (2),\n  (3),\n  (4),\n  (5),\n  (6),\n  (7);"));
        assert!(kept("  (1, 'a\n b'),\n  (2),\n  (3),\n  (4),\n  (5),\n  (6);"));
        assert!(kept("  (1), -- x\n  (2),\n  (3),\n  (4),\n  (5),\n  (6);"));
        assert!(kept("  (1),\n  (2),\n  (3),\n  SELECT 1;"));
        assert!(kept("  (1),\n  (2),\n  (3),\n  (4),\n  (5),\n  (6),"));
        let upsert =
            "insert into t values\n  (1),\n  (2),\n  (3),\n  (4),\n  (5),\n  (6)\non conflict do nothing;\n";
        assert!(
            view("a.sql", upsert).is_some_and(|v| v.contains("3-6\t⋯ 4 row(s)\n7\t  (6)\n8\ton conflict"))
        );
        assert!(kept("  (1), /* x */\n  (2),\n  (3),\n  (4),\n  (5),\n  (6);"));
        assert!(kept("  (1) x,\n  (2),\n  (3),\n  (4),\n  (5),\n  (6);"));
        assert!(kept("  ('a--b'),\n  (2),\n  (3),\n  (4),\n  (5),\n  (6);"));
        assert!(kept("  ('a/*b'),\n  (2),\n  (3),\n  (4),\n  (5),\n  (6);"));
        assert!(view("a.sql", "INSERT INTO t SELECT * FROM u;\n").is_none());
        let quoted =
            "insert into t values\n  ('a(', 1),\n  ('b)', 2),\n  (3, 3),\n  (4, 4),\n  (5, 5),\n  (6, 6);\n";
        assert!(view("a.sql", quoted).is_some_and(|v| v.contains("⋯ 4 row(s)")));
        assert!(is_sql(Path::new("a/B.SQL")) && !is_sql(Path::new("a.sqlite")));
    }
}
