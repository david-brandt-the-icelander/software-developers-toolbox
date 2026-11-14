// Terse view of JSON, YAML and TOML files, line by line: collapses long runs of list items and TOML `[[tables]]` to their first and last, comment blocks and long YAML block scalars or TOML multi-line strings, and shortens very long quoted strings, keeping source line numbers.
use crate::markup::Plan;
use std::path::Path;

const LONG_RUN: usize = 10;
const LONG_TABLES: usize = 6;
const LONG_TEXT: usize = 6;
const LONG_COMMENT: usize = 4;
const LONG_STRING: usize = 120;
const KEEP_STRING: usize = 40;

// A run of sibling blocks, as (first, last) lines, and the note and step when it collapses.
type Run = (Vec<(usize, usize)>, Option<(String, &'static str)>);

// The data formats the view handles.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    Json,
    Yaml,
    Toml,
}

// Returns the format of a path from its extension or a known lock file name, and whether it is a lock file.
pub fn kind_of(path: &Path) -> Option<(Kind, bool)> {
    let name = path.file_name()?.to_str()?.to_ascii_lowercase();
    let kind = match name.rsplit_once('.')?.1 {
        "json" | "jsonc" | "json5" => Kind::Json,
        "yaml" | "yml" => Kind::Yaml,
        "toml" => Kind::Toml,
        "lock" if matches!(name.as_str(), "cargo.lock" | "poetry.lock" | "uv.lock" | "pdm.lock") => {
            Kind::Toml
        }
        "lock" if matches!(name.as_str(), "composer.lock" | "pipfile.lock") => Kind::Json,
        _ => return None,
    };
    let lock = name.ends_with(".lock") || name.contains("-lock.") || name.contains(".lock.");
    Some((kind, lock))
}

// Returns the view of a file, or None when nothing would be shortened: a header, then `N`, `N~` and `A-B` lines.
pub fn view(shown: &str, text: &str, kind: Kind, lock: bool) -> Option<String> {
    let src: Vec<&str> = text.lines().collect();
    let mut ranges: Vec<(usize, usize, String, &str)> = Vec::new();
    let mut taken = vec![false; src.len()];
    let mut i = 0;
    while i < src.len() {
        if let Some(end) = comment_end(&src, i, kind) {
            ranges.push((i, end, format!("comment, {} line(s)", end - i + 1), "comments"));
            i = end + 1;
            continue;
        }
        if let Some((a, b, end)) = text_body(&src, i, kind) {
            if b + 1 - a >= LONG_TEXT {
                ranges.push((a, b, format!("text, {} line(s)", b - a + 1), "long texts"));
            }
            i = end + 1;
            continue;
        }
        if !taken[i] {
            let found = if kind == Kind::Toml && is_table_header(src[i]) {
                tables(&src, i)
            } else {
                items(&src, i, kind, lock)
            };
            if let Some((blocks, collapse)) = found {
                blocks.iter().for_each(|&(a, _)| taken[a] = true);
                if let Some((note, step)) = collapse {
                    ranges.push((blocks[1].0, blocks[blocks.len() - 2].1, note, step));
                }
            }
        }
        i += 1;
    }
    ranges.sort_by_key(|r| r.0);
    let mut kept: Vec<(usize, usize, String, &str)> = Vec::new();
    for r in ranges {
        if kept.last().is_none_or(|k| r.0 > k.1) {
            kept.push(r);
        }
    }
    let mut plan = Plan { lines: Vec::new(), steps: Vec::new() };
    let mut line = 0;
    let mut next = kept.into_iter().peekable();
    while line < src.len() {
        if let Some((a, b, note, step)) = next.next_if(|r| r.0 == line) {
            plan.hide(a, b, note, step);
            line = b + 1;
            continue;
        }
        let (short, n) = shorten_strings(src[line]);
        plan.add("long strings", n);
        plan.keep(line, &short, n > 0);
        line += 1;
    }
    plan.steps.retain(|(_, n)| *n > 0);
    if plan.steps.is_empty() {
        return None;
    }
    let steps: Vec<String> = plan.steps.iter().map(|(s, n)| format!("{n} {s}")).collect();
    Some(format!("{shown} {} lines; terse: {}\n{}", src.len(), steps.join(", "), plan.lines.join("\n")))
}

// Returns a line's indentation width.
fn indent(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

// Returns the last line of a comment block of 4+ lines starting at line i: `#` lines in YAML and TOML, `//` lines in JSON with comments.
fn comment_end(src: &[&str], i: usize, kind: Kind) -> Option<usize> {
    let mark = if kind == Kind::Json { "//" } else { "#" };
    let n = src[i..].iter().take_while(|l| l.trim_start().starts_with(mark)).count();
    (n >= LONG_COMMENT && (i == 0 || !src[i - 1].trim_start().starts_with(mark))).then(|| i + n - 1)
}

// Returns the first and last body lines of a YAML block scalar (`key: |`) or TOML multi-line string that opens on line i, and the last line it covers.
fn text_body(src: &[&str], i: usize, kind: Kind) -> Option<(usize, usize, usize)> {
    let t = src[i].trim_end();
    match kind {
        Kind::Yaml => {
            let head = t.trim_end_matches(['-', '+', '0', '1', '2', '3', '4', '5', '6', '7', '8', '9']);
            if !(head.ends_with(": |")
                || head.ends_with(": >")
                || head.ends_with("- |")
                || head.ends_with("- >"))
            {
                return None;
            }
            let k = indent(src[i]);
            let n = src[i + 1..].iter().take_while(|l| l.trim().is_empty() || indent(l) > k).count();
            let n = n - src[i + 1..i + 1 + n].iter().rev().take_while(|l| l.trim().is_empty()).count();
            (n > 0).then_some((i + 1, i + n, i + n))
        }
        Kind::Toml => {
            let quote = ["\"\"\"", "'''"].into_iter().find(|q| t.matches(q).count() == 1)?;
            let close = src[i + 1..].iter().position(|l| l.contains(quote))? + i + 1;
            Some((i + 1, close - 1, close))
        }
        Kind::Json => None,
    }
}

// True for a TOML `[[name]]` array-of-tables header.
fn is_table_header(line: &str) -> bool {
    line.starts_with("[[") && line.trim_end().ends_with("]]")
}

// Returns the blocks of a run of TOML `[[name]]` tables with the same name starting at line i, and the note and step when 6+ of them collapse.
fn tables(src: &[&str], i: usize) -> Option<Run> {
    let name = src[i].trim_end();
    let mut blocks = Vec::new();
    let mut start = i;
    while start < src.len() && src[start].trim_end() == name {
        let next =
            src[start + 1..].iter().position(|l| l.starts_with('[')).map_or(src.len(), |p| start + 1 + p);
        let end = (start..next).rev().find(|&l| !src[l].trim().is_empty()).unwrap_or(start);
        blocks.push((start, end));
        start = next;
    }
    let n = blocks.len();
    let collapse = (n >= LONG_TABLES).then(|| (format!("{} {name} table(s)", n - 2), "tables"));
    Some((blocks, collapse))
}

// Returns the blocks of a run of sibling list items starting at line i (any sibling entries in a lock file), and the note and step when 10+ of them collapse; multi-line items must name the same keys.
fn items(src: &[&str], i: usize, kind: Kind, lock: bool) -> Option<Run> {
    let k = indent(src[i]);
    let child = |l: &str| indent(l) == k && is_item(l.trim(), kind, lock);
    if !child(src[i]) {
        return None;
    }
    let mut blocks = Vec::new();
    let mut start = i;
    loop {
        let end = block_end(src, start, kind)?;
        blocks.push((start, end));
        let after = (end + 1..src.len()).find(|&l| !src[l].trim().is_empty() || kind == Kind::Json);
        match after {
            Some(next) if child(src[next]) => start = next,
            _ => break,
        }
    }
    let n = blocks.len();
    let shape = |&(a, b): &(usize, usize)| keys(src, a, b, kind);
    let multi: Vec<Vec<String>> = blocks.iter().filter(|b| b.1 > b.0).map(shape).collect();
    let uniform = lock || multi.iter().all(|m| *m == multi[0]);
    let word = if lock { "entries" } else { "item(s)" };
    let step = if lock { "lock entries" } else { "list items" };
    let collapse = (n >= LONG_RUN && uniform).then(|| (format!("{} {word}", n - 2), step));
    Some((blocks, collapse))
}

// Returns the keys an item's lines name directly under it, in order: `"key":` in JSON, `key:` in YAML (the first one after `- ` too).
fn keys(src: &[&str], a: usize, b: usize, kind: Kind) -> Vec<String> {
    let k = indent(src[a]);
    let inner = src[a + 1..=b].iter().map(|l| indent(l)).filter(|&w| w > k).min();
    let mut out = Vec::new();
    for (j, line) in src.iter().enumerate().take(b + 1).skip(a) {
        let t = if j == a { line.trim().trim_start_matches(['-', '{']).trim_start() } else { line.trim() };
        let direct = j == a || Some(indent(line)) == inner;
        let key = match kind {
            Kind::Json => t
                .starts_with('"')
                .then(|| string_end(t))
                .flatten()
                .filter(|&e| t[e..].trim_start().starts_with(':'))
                .map(|e| &t[..e]),
            _ => t
                .split_once(':')
                .map(|(key, _)| key)
                .filter(|key| !key.is_empty() && !key.contains([' ', '"', '\''])),
        };
        if let (true, Some(key)) = (direct, key) {
            out.push(key.to_string());
        }
    }
    out
}

// True for a line that starts a list item: a JSON array value, a YAML `- ` item, a TOML array value; in a lock file also an object member or mapping key.
fn is_item(t: &str, kind: Kind, lock: bool) -> bool {
    match kind {
        Kind::Json => {
            let member =
                t.starts_with('"') && string_end(t).is_some_and(|e| t[e..].trim_start().starts_with(':'));
            (lock || !member)
                && t.starts_with([
                    '{', '[', '"', '-', '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 't', 'f', 'n',
                ])
        }
        Kind::Yaml => t == "-" || t.starts_with("- ") || lock && t.ends_with(':') && !t.starts_with('#'),
        Kind::Toml => {
            !t.contains(" = ")
                && t.starts_with([
                    '{', '[', '"', '\'', '-', '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 't', 'f',
                ])
        }
    }
}

// Returns the byte just after the closing quote of a string that starts a text.
fn string_end(t: &str) -> Option<usize> {
    let mut escaped = false;
    for (i, c) in t.char_indices().skip(1) {
        match c {
            '\\' if !escaped => escaped = true,
            '"' if !escaped => return Some(i + 1),
            _ => escaped = false,
        }
    }
    None
}

// Returns the last line of the item that starts at line i: its closing bracket line in JSON and TOML, its deeper-indented lines in YAML; None when brackets never close.
fn block_end(src: &[&str], i: usize, kind: Kind) -> Option<usize> {
    let k = indent(src[i]);
    if kind == Kind::Yaml {
        let n = src[i + 1..].iter().take_while(|l| l.trim().is_empty() || indent(l) > k).count();
        return Some(i + n - src[i + 1..i + 1 + n].iter().rev().take_while(|l| l.trim().is_empty()).count());
    }
    let mut depth = 0i32;
    for (j, line) in src.iter().enumerate().skip(i) {
        depth += brackets(line);
        if depth <= 0 {
            return Some(j);
        }
    }
    None
}

// Returns how many brackets a line opens minus how many it closes, outside double-quoted strings.
fn brackets(line: &str) -> i32 {
    let (mut depth, mut quoted, mut escaped) = (0, false, false);
    for c in line.chars() {
        match c {
            '\\' if quoted && !escaped => {
                escaped = true;
                continue;
            }
            '"' if !escaped => quoted = !quoted,
            '{' | '[' if !quoted => depth += 1,
            '}' | ']' if !quoted => depth -= 1,
            _ => {}
        }
        escaped = false;
    }
    depth
}

// Shortens double-quoted strings longer than 120 characters to their first 40 and `…`; returns the line and how many it changed.
fn shorten_strings(line: &str) -> (String, usize) {
    let (mut out, mut n, mut rest) = (String::new(), 0, line);
    while let Some(q) = rest.find('"') {
        let Some(len) = string_end(&rest[q..]) else { break };
        let s = &rest[q..q + len];
        out.push_str(&rest[..q]);
        if s.chars().count() > LONG_STRING {
            let keep: String = s.chars().take(KEEP_STRING + 1).collect();
            out.push_str(&format!("{keep}…\""));
            n += 1;
        } else {
            out.push_str(s);
        }
        rest = &rest[q + len..];
    }
    out.push_str(rest);
    (out, n)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    // Returns a view's lines after the header.
    fn body(src: &str, kind: Kind, lock: bool) -> Vec<String> {
        view("f", src, kind, lock).expect("view").lines().skip(1).map(str::to_string).collect()
    }

    // Checks file names map to formats and lock files.
    #[test]
    fn test_kind_of() {
        assert_eq!(kind_of(Path::new("a/B.JSON")), Some((Kind::Json, false)));
        assert_eq!(kind_of(Path::new("package-lock.json")), Some((Kind::Json, true)));
        assert_eq!(kind_of(Path::new("x.yml")), Some((Kind::Yaml, false)));
        assert_eq!(kind_of(Path::new("pnpm-lock.yaml")), Some((Kind::Yaml, true)));
        assert_eq!(kind_of(Path::new("Cargo.toml")), Some((Kind::Toml, false)));
        assert_eq!(kind_of(Path::new("Cargo.lock")), Some((Kind::Toml, true)));
        assert_eq!(kind_of(Path::new("composer.lock")), Some((Kind::Json, true)));
        assert_eq!(kind_of(Path::new("yarn.lock")), None);
        assert_eq!(kind_of(Path::new("README")), None);
    }

    // Checks a JSON array of scalars and one of objects collapse to their first and last items, and a short array and object members stay.
    #[test]
    fn test_json_lists() {
        let nums: Vec<String> = (1..=12).map(|n| format!("    {n},")).collect();
        let objs: Vec<String> = (1..=10)
            .map(|n| format!("    {{\n      \"id\": {n},\n      \"tags\": [\"a\", \"b\"]\n    }},"))
            .collect();
        let src = format!(
            "{{\n  \"name\": \"x\",\n  \"nums\": [\n{}\n    13\n  ],\n  \"objs\": [\n{}\n    {{\"id\": 11}}\n  ],\n  \"short\": [\n    1,\n    2\n  ]\n}}\n",
            nums.join("\n"),
            objs.join("\n")
        );
        let lines = body(&src, Kind::Json, false);
        let v = lines.join("\n");
        assert!(v.contains("4\t    1,\n5-15\t⋯ 11 item(s)\n16\t    13\n"), "{v}");
        assert!(v.contains("19\t    {\n20\t      \"id\": 1,"), "{v}");
        assert!(v.contains("23-58\t⋯ 9 item(s)\n59\t    {\"id\": 11}"), "{v}");
        assert!(v.contains("\t    2\n"), "{v}");
        assert!(view("f", "{\n  \"a\": 1,\n  \"b\": [1, 2]\n}\n", Kind::Json, false).is_none());
    }

    // Checks lock files collapse object members too, YAML items and block scalars, TOML tables, arrays and multi-line strings, comments and long strings.
    #[test]
    fn test_lock_yaml_toml() {
        let members: Vec<String> = (1..=10)
            .map(|n| format!("    \"node_modules/p{n}\": {{\n      \"version\": \"1.0.{n}\"\n    }},"))
            .collect();
        let lock = format!("{{\n  \"packages\": {{\n{}\n    \"z\": {{}}\n  }}\n}}\n", members.join("\n"));
        assert!(body(&lock, Kind::Json, true).contains(&"6-32\t⋯ 9 entries".to_string()));
        let yaml_items: Vec<String> = (1..=11).map(|n| format!("  - name: s{n}\n    port: {n}")).collect();
        let yaml = format!(
            "# license\n# a\n# b\n# c\nservices:\n{}\nscript: |\n  a\n  b\n  c\n  d\n\n  e\n  f\nend: 1\n",
            yaml_items.join("\n")
        );
        let lines = body(&yaml, Kind::Yaml, false);
        for want in [
            "1-4\t⋯ comment, 4 line(s)",
            "6\t  - name: s1",
            "8-25\t⋯ 9 item(s)",
            "26\t  - name: s11",
            "28\tscript: |",
            "29-35\t⋯ text, 7 line(s)",
            "36\tend: 1",
        ] {
            assert!(lines.contains(&want.to_string()), "{want}: {lines:?}");
        }
        let pkgs: Vec<String> =
            (1..=7).map(|n| format!("[[package]]\nname = \"p{n}\"\nversion = \"1\"\n")).collect();
        let long = "x".repeat(130);
        let toml = format!(
            "{}[metadata]\nlong = \"{long}\"\nnote = \"\"\"\na\nb\nc\nd\ne\nf\n\"\"\"\n",
            pkgs.join("\n")
        );
        let lines = body(&toml, Kind::Toml, true);
        let short = format!("29~\tlong = \"{}…\"", "x".repeat(40));
        for want in [
            "1\t[[package]]",
            "5-23\t⋯ 5 [[package]] table(s)",
            "25\t[[package]]",
            &short,
            "30\tnote = \"\"\"",
            "31-36\t⋯ text, 6 line(s)",
            "37\t\"\"\"",
        ] {
            assert!(lines.contains(&want.to_string()), "{want}: {lines:?}");
        }
        let arr: Vec<String> = (1..=10).map(|n| format!("  \"m{n}\",")).collect();
        let toml_arr = format!("members = [\n{}\n]\n", arr.join("\n"));
        assert!(body(&toml_arr, Kind::Toml, false).contains(&"3-10\t⋯ 8 item(s)".to_string()));
        assert!(view("f", "a = 1\n[[x]]\nb = 2\n", Kind::Toml, false).is_none());
        let steps: Vec<String> = (1..=10)
            .map(|n| {
                if n % 2 == 0 {
                    format!("  - run: s{n}\n    shell: bash")
                } else {
                    format!("  - name: s{n}\n    uses: x")
                }
            })
            .collect();
        assert!(view("f", &format!("steps:\n{}\n", steps.join("\n")), Kind::Yaml, false).is_none());
        let short_string =
            "a = \"\"\"\nx\n\"\"\"\nb = 1\nc = 2\nd = 3\ne = 4\nf = 5\ng = 6\nh = \"\"\"\ny\n\"\"\"\n";
        assert!(view("f", short_string, Kind::Toml, false).is_none());
        let short_script = "run: |\n  - a\n  - b\n";
        assert!(view("f", short_script, Kind::Yaml, false).is_none());
        assert!(view("f", "[\n  1,\n  [2,\n", Kind::Json, false).is_none());
        assert!(view("f", "// a\n// b\n// c\n// d\n{}\n", Kind::Json, false).is_some());
    }

    // Checks string shortening keeps escapes, short strings and unclosed quotes.
    #[test]
    fn test_shorten_strings() {
        let long = format!("\"{}\\\"{}\"", "a".repeat(100), "b".repeat(40));
        let (s, n) = shorten_strings(&format!("  \"k\": {long}, \"ok\": \"y\", \"open"));
        assert_eq!(n, 1);
        assert_eq!(s, format!("  \"k\": \"{}…\", \"ok\": \"y\", \"open", "a".repeat(40)));
    }

    // Checks the rest of the edges: folded and item block scalars, bare `-` items, YAML lock entries, blank lines between items, quoted keys, brackets inside strings, runs nested in hidden items.
    #[test]
    fn test_edges() {
        let text: String = (0..6).map(|n| format!("    line {n}\n")).collect();
        for head in ["  key: >", "  - |", "  - >-"] {
            let src = format!("a:\n{head}\n{text}");
            assert!(
                view("f", &src, Kind::Yaml, false).is_some_and(|v| v.contains("⋯ text, 6 line(s)")),
                "{head}"
            );
        }
        let bare: String = (0..10).map(|n| format!("  -\n    \"k {n}\": 1\n\n")).collect();
        assert!(
            view("f", &format!("a:\n{bare}"), Kind::Yaml, false).is_some_and(|v| v.contains("⋯ 8 item(s)"))
        );
        let entries: String = (0..10).map(|n| format!("  /p{n}@1.0.0:\n    resolution: x\n")).collect();
        assert!(view("f", &format!("packages:\n{entries}"), Kind::Yaml, true)
            .is_some_and(|v| v.contains("⋯ 8 entries")));
        let tricky: Vec<String> = (0..10)
            .map(|n| {
                format!(
                    "  {{\n    \"s\": \"a\\\"}}[{n}\",\n    \"inner\": [\n{}\n    ]\n  }},",
                    (0..10).map(|m| format!("      {m},")).collect::<Vec<_>>().join("\n")
                )
            })
            .collect();
        let src = format!("[\n{}\n  {{}}\n]\n", tricky.join("\n"));
        let v = view("f", &src, Kind::Json, false).expect("view");
        assert!(v.contains("⋯ 9 item(s)") && v.matches("⋯ 8 item(s)").count() == 1, "{v}");
    }
}
