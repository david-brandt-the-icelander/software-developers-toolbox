// Reads fields of the YAML front matter at the top of a markdown file: enough for skill descriptions and rule `paths`.

// Returns the lines between the opening and closing `---` of the front matter, or nothing if the file has none.
fn lines(text: &str) -> Option<Vec<&str>> {
    let mut it = text.trim_start_matches('\u{feff}').lines();
    if it.next()?.trim_end() != "---" {
        return None;
    }
    Some(it.take_while(|l| l.trim_end() != "---").collect())
}

// Returns a top-level key's inline value and the non-empty indented lines that continue it.
fn entry<'a>(lines: &[&'a str], key: &str) -> Option<(&'a str, Vec<&'a str>)> {
    let i = lines.iter().position(|l| l.strip_prefix(key).is_some_and(|r| r.starts_with(':')))?;
    let value = lines[i][key.len() + 1..].trim();
    let more = lines[i + 1..]
        .iter()
        .take_while(|l| l.trim().is_empty() || l.starts_with([' ', '\t']))
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect();
    Some((value, more))
}

// Strips the YAML quotes around a value.
fn unquote(v: &str) -> String {
    let v = v.trim();
    let quoted = |q: char| v.len() >= 2 && v.starts_with(q) && v.ends_with(q);
    if quoted('"') {
        return v[1..v.len() - 1].replace("\\\"", "\"");
    }
    if quoted('\'') {
        return v[1..v.len() - 1].replace("''", "'");
    }
    v.to_string()
}

// Splits an inline list at commas that are not inside `{...}` glob alternatives.
fn split(s: &str) -> Vec<&str> {
    let (mut out, mut depth, mut start) = (Vec::new(), 0i32, 0);
    for (i, c) in s.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => depth -= 1,
            ',' if depth <= 0 => {
                out.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&s[start..]);
    out
}

// Returns a text field on one line: plain, quoted and folded (`>`) values joined with spaces, literal (`|`) ones with newlines.
pub fn field(text: &str, key: &str) -> Option<String> {
    let lines = lines(text)?;
    let (value, more) = entry(&lines, key)?;
    let (sep, first) = match value.chars().next() {
        Some('|') => ("\n", ""),
        Some('>') => (" ", ""),
        _ => (" ", value),
    };
    let parts: Vec<&str> = std::iter::once(first).chain(more).filter(|p| !p.is_empty()).collect();
    Some(unquote(&parts.join(sep)))
}

// Returns a list field written inline (`key: a, b` or `key: [a, b]`) or as `- item` lines, or nothing if it is absent or empty.
pub fn list(text: &str, key: &str) -> Option<Vec<String>> {
    let lines = lines(text)?;
    let (value, more) = entry(&lines, key)?;
    let inline = value.trim_start_matches('[').trim_end_matches(']');
    let items: Vec<String> = split(inline)
        .into_iter()
        .chain(more.into_iter().filter_map(|l| l.strip_prefix('-')))
        .map(unquote)
        .filter(|s| !s.is_empty())
        .collect();
    (!items.is_empty()).then_some(items)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Plain, quoted, folded and literal descriptions come back on one line or with their newlines.
    #[test]
    fn test_field_forms() {
        assert_eq!(field("---\ndescription: Does s.\n---\n", "description").as_deref(), Some("Does s."));
        assert_eq!(
            field("---\ndescription: \"Say \\\"hi\\\"\"\n---\n", "description").as_deref(),
            Some("Say \"hi\"")
        );
        assert_eq!(field("---\ndescription: 'it''s'\n---\n", "description").as_deref(), Some("it's"));
        let folded = "---\nname: m\ndescription: >-\n  Multi line\n\n  description.\nother: x\n---\n";
        assert_eq!(field(folded, "description").as_deref(), Some("Multi line description."));
        assert_eq!(field("---\ndescription: |\n  a\n  b\n---\n", "description").as_deref(), Some("a\nb"));
        assert_eq!(
            field("---\ndescription: starts\n  and goes on\n", "description").as_deref(),
            Some("starts and goes on")
        );
        assert_eq!(field("\u{feff}--- \ndescription: x\n--- \n", "description").as_deref(), Some("x"));
    }

    // A missing front matter, key or file gives nothing, and a key is matched whole.
    #[test]
    fn test_field_absent() {
        assert_eq!(field("no front matter", "description"), None);
        assert_eq!(field("", "description"), None);
        assert_eq!(field("---\nname: x\n---\ndescription: body\n", "description"), None);
        assert_eq!(field("---\ndescriptions: x\n---\n", "description"), None);
        assert_eq!(field("---\ndescription: \"\n---\n", "description").as_deref(), Some("\""));
    }

    // Lists are read inline, bracketed or as items, keeping commas inside braces.
    #[test]
    fn test_list_forms() {
        let want = |v: &[&str]| Some(v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(list("---\npaths: src/**, \"lib/*.rs\"\n---\n", "paths"), want(&["src/**", "lib/*.rs"]));
        assert_eq!(list("---\npaths: [src/{a,b}/**, x]\n---\n", "paths"), want(&["src/{a,b}/**", "x"]));
        assert_eq!(
            list("---\npaths:\n  - \"src/**\"\n  - 'docs/*.md'\n---\n", "paths"),
            want(&["src/**", "docs/*.md"])
        );
        assert_eq!(list("---\npaths:\n---\n", "paths"), None);
        assert_eq!(list("---\nname: x\n---\n", "paths"), None);
        assert_eq!(list("plain", "paths"), None);
        assert_eq!(list("---\npaths: a}, b\n---\n", "paths"), want(&["a}", "b"]));
    }
}
