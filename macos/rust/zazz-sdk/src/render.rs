// Renders a script's returned output for a human: raw strings, tables for lists of flat objects, JSON otherwise.
use crate::json::Json;

// Formats a scalar for a table cell, or None for nested values.
fn cell(v: &Json) -> Option<String> {
    match v {
        Json::Null => Some(String::new()),
        Json::Str(s) => Some(s.replace(['\t', '\n'], " ")),
        Json::Arr(_) | Json::Obj(_) => None,
        other => Some(other.to_string()),
    }
}

// Builds header and rows when every item is an object with scalar fields.
fn table(items: &[Json]) -> Option<(Vec<String>, Vec<Vec<String>>)> {
    let mut cols: Vec<String> = Vec::new();
    for it in items {
        for (k, _) in it.obj()? {
            if !cols.contains(k) {
                cols.push(k.clone());
            }
        }
    }
    let rows = items.iter().map(|it| cols.iter().map(|c| cell(&it[c.as_str()])).collect::<Option<Vec<_>>>());
    Some((cols.clone(), rows.collect::<Option<Vec<_>>>()?))
}

// Lays out a table: aligned columns on a terminal, tab-separated otherwise.
fn layout(cols: &[String], rows: &[Vec<String>], tty: bool) -> String {
    if !tty {
        let lines: Vec<String> =
            std::iter::once(cols.join("\t")).chain(rows.iter().map(|r| r.join("\t"))).collect();
        return lines.join("\n") + "\n";
    }
    let header = cols.to_vec();
    let width =
        |i: usize| std::iter::once(&header).chain(rows).map(|r| r[i].chars().count()).max().unwrap_or(0);
    let widths: Vec<usize> = (0..cols.len()).map(width).collect();
    let fmt = |r: &Vec<String>| {
        let cells: Vec<String> = r.iter().zip(&widths).map(|(c, w)| format!("{c:w$}")).collect();
        cells.join("  ").trim_end().to_string()
    };
    let mut out: Vec<String> = vec![fmt(&header)];
    out.extend(rows.iter().map(fmt));
    out.join("\n") + "\n"
}

// Renders output for a human; empty for null.
pub fn render(output: &Json, tty: bool) -> String {
    match output {
        Json::Null => String::new(),
        Json::Str(s) if s.ends_with('\n') || s.is_empty() => s.clone(),
        Json::Str(s) => format!("{s}\n"),
        Json::Arr(items) if !items.is_empty() => match table(items) {
            Some((cols, rows)) => layout(&cols, &rows, tty),
            None => output.pretty() + "\n",
        },
        _ => output.pretty() + "\n",
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::json;

    // Checks strings, null and plain JSON rendering.
    #[test]
    fn test_render_scalars() {
        assert_eq!(render(&Json::Null, true), "");
        assert_eq!(render(&json!("hi"), true), "hi\n");
        assert_eq!(render(&json!("hi\n"), true), "hi\n");
        assert_eq!(render(&json!(""), true), "");
        assert_eq!(render(&json!({"a": 1}), true), "{\n  \"a\": 1\n}\n");
        assert_eq!(render(&json!([]), false), "[]\n");
    }

    // Checks lists of flat objects become tables: aligned on a TTY, TSV otherwise.
    #[test]
    fn test_render_tables() {
        let v = json!([{"name": "a", "n": 1}, {"name": "bbb", "ok": true, "x": null}]);
        assert_eq!(render(&v, false), "name\tn\tok\tx\na\t1\t\t\nbbb\t\ttrue\t\n");
        assert_eq!(render(&v, true), "name  n  ok    x\na     1\nbbb      true\n");
        let nested = json!([{"a": [1]}]);
        assert!(render(&nested, true).starts_with("[\n"));
        assert!(render(&json!([1, 2]), true).starts_with("[\n"));
        assert_eq!(render(&json!([{"s": "a\tb"}]), false), "s\na b\n");
    }
}
