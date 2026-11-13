// Terse view of HTML and Razor files: hides comments of three or more lines, inline styles, SVG drawings, head boilerplate and Razor directives, and shortens long class and style values, keeping source line numbers.
use std::path::Path;

const LONG_CLASS: usize = 4;
const LONG_STYLE: usize = 40;
const LONG_COMMENT: usize = 3;
const BOILERPLATE: [&str; 3] = ["<meta", "<link", "<base"];
const IMPORTS: [&str; 3] = ["@using ", "@addTagHelper ", "@removeTagHelper "];

// The markup languages the view handles.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    Html,
    Razor,
}

// Returns the markup kind of a path from its extension, if it has one.
pub fn kind_of(path: &Path) -> Option<Kind> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "html" | "htm" | "xhtml" => Some(Kind::Html),
        "cshtml" | "razor" => Some(Kind::Razor),
        _ => None,
    }
}

// The view being built: its lines and how many places each step changed; styles.rs builds its view with it too.
pub(crate) struct Plan<'a> {
    pub(crate) lines: Vec<String>,
    pub(crate) steps: Vec<(&'a str, usize)>,
}

impl<'a> Plan<'a> {
    // Counts one change for a step.
    pub(crate) fn count(&mut self, step: &'a str) {
        match self.steps.iter_mut().find(|(s, _)| *s == step) {
            Some((_, n)) => *n += 1,
            None => self.steps.push((step, 1)),
        }
    }

    // Adds a note standing for source lines a to b (0-based).
    pub(crate) fn hide(&mut self, a: usize, b: usize, note: String, step: &'a str) {
        self.lines.push(format!("{}-{}\t⋯ {note}", a + 1, b + 1));
        self.count(step);
    }

    // Adds a source line, unchanged or changed, dropping a second blank line in a row.
    pub(crate) fn keep(&mut self, i: usize, text: &str, changed: bool) {
        let text = text.trim_end();
        if text.is_empty() && self.lines.last().is_some_and(|l| l.ends_with('\t')) {
            return;
        }
        self.lines.push(if changed { format!("{}~\t{text}", i + 1) } else { format!("{}\t{text}", i + 1) });
    }
}

// Returns the view of a file, or None when nothing would be shortened: a header with the line count and what was shortened, then `N`, `N~` and `A-B` lines.
pub fn view(shown: &str, text: &str, kind: Kind) -> Option<String> {
    let src: Vec<&str> = text.lines().collect();
    let mut plan = Plan { lines: Vec::new(), steps: Vec::new() };
    let mut i = 0;
    while i < src.len() {
        if let Some(end) = verbatim(&src, i, kind) {
            (i..=end).for_each(|j| plan.keep(j, src[j], false));
            i = end + 1;
        } else if let Some(end) = closed_by(&src, i, "<!--", "-->")
            .or_else(|| razor_comment(&src, i, kind))
            .filter(|e| e + 1 - i >= LONG_COMMENT)
        {
            plan.hide(i, end, format!("comment, {} line(s)", end - i + 1), "comments");
            i = end + 1;
        } else if let Some((end, tag)) =
            ["style", "svg"].iter().find_map(|t| element(&src, i, t).map(|e| (e, *t)))
        {
            plan.hide(
                i,
                end,
                format!("<{tag}>, {} line(s)", end - i + 1),
                if tag == "svg" { "svgs" } else { "styles" },
            );
            i = end + 1;
        } else if let Some(end) =
            run(&src, i, |t| kind == Kind::Razor && IMPORTS.iter().any(|d| t.starts_with(d)))
        {
            plan.hide(i, end, format!("{} @using/tag helper line(s)", end - i + 1), "usings");
            i = end + 1;
        } else if let Some(end) =
            run(&src, i, |t| BOILERPLATE.iter().any(|b| t.starts_with(b)) && t.ends_with('>'))
        {
            plan.hide(i, end, format!("{} meta/link tag(s)", end - i + 1), "head tags");
            i = end + 1;
        } else {
            let (short, n) = shorten(src[i]);
            (0..n).for_each(|_| plan.count("long attributes"));
            plan.keep(i, &short, n > 0);
            i += 1;
        }
    }
    if plan.steps.is_empty() {
        return None;
    }
    let steps: Vec<String> = plan.steps.iter().map(|(s, n)| format!("{n} {s}")).collect();
    Some(format!("{shown} {} lines; terse: {}\n{}", src.len(), steps.join(", "), plan.lines.join("\n")))
}

// Returns the last line of a block shown as it is: <script>, <pre>, <textarea> and Razor code blocks.
fn verbatim(src: &[&str], i: usize, kind: Kind) -> Option<usize> {
    let t = src[i].trim_start();
    for tag in ["script", "pre", "textarea"] {
        if opens(t, tag) {
            let close = format!("</{tag}>");
            return Some((i..src.len()).find(|&j| src[j].contains(&close)).unwrap_or(src.len() - 1));
        }
    }
    let code = ["@code", "@functions", "@{"].iter().any(|k| t.starts_with(k));
    (kind == Kind::Razor && code).then(|| braces_end(src, i))
}

// Returns the line where the braces opened on line i close, or the last line when they never do.
fn braces_end(src: &[&str], i: usize) -> usize {
    let (mut depth, mut opened) = (0i64, false);
    for (j, line) in src.iter().enumerate().skip(i) {
        opened |= line.contains('{');
        depth += line.matches('{').count() as i64 - line.matches('}').count() as i64;
        if opened && depth <= 0 {
            return j;
        }
    }
    src.len() - 1
}

// True when text starts with the opening tag of an element.
fn opens(t: &str, tag: &str) -> bool {
    t.strip_prefix('<')
        .and_then(|r| r.strip_prefix(tag))
        .is_some_and(|r| r.starts_with([' ', '>', '\t']) || r.is_empty())
}

// Returns the last line of a block that starts a line with `open` and ends a line with `close`, alone on those lines.
fn closed_by(src: &[&str], i: usize, open: &str, close: &str) -> Option<usize> {
    if !src[i].trim_start().starts_with(open) {
        return None;
    }
    let end = (i..src.len()).find(|&j| src[j].contains(close))?;
    src[end].trim_end().ends_with(close).then_some(end)
}

// Returns the last line of a whole-line Razor comment `@* ... *@`.
fn razor_comment(src: &[&str], i: usize, kind: Kind) -> Option<usize> {
    (kind == Kind::Razor).then(|| closed_by(src, i, "@*", "*@")).flatten()
}

// Returns the last line of an element whose opening tag starts line i and whose closing tag ends a later or the same line.
fn element(src: &[&str], i: usize, tag: &str) -> Option<usize> {
    if !opens(src[i].trim_start(), tag) {
        return None;
    }
    let (open, close) = (format!("<{tag}"), format!("</{tag}>"));
    let mut depth = 0usize;
    for (j, line) in src.iter().enumerate().skip(i) {
        depth += line.matches(&open).count();
        depth = depth.saturating_sub(line.matches(&close).count());
        if depth == 0 {
            return line.trim_end().ends_with(&close).then_some(j);
        }
    }
    None
}

// Returns the last line of a run of at least two lines from i whose trimmed text passes the test.
fn run(src: &[&str], i: usize, test: impl Fn(&str) -> bool) -> Option<usize> {
    let n = src[i..].iter().take_while(|l| test(l.trim())).count();
    (n >= 2).then(|| i + n - 1)
}

// Shortens long class values to their first classes and long style values to `…`; returns the line and how many it changed.
fn shorten(line: &str) -> (String, usize) {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    let mut changed = 0;
    while let Some((before, attr, value, after)) = next_attr(rest) {
        out.push_str(before);
        let short = match attr {
            "class" if !dynamic(value) && value.split_whitespace().count() > LONG_CLASS => {
                let first: Vec<&str> = value.split_whitespace().take(LONG_CLASS - 1).collect();
                Some(format!("{} …", first.join(" ")))
            }
            "style" if !dynamic(value) && value.len() > LONG_STYLE => Some("…".to_string()),
            _ => None,
        };
        changed += usize::from(short.is_some());
        out.push_str(&format!("{attr}=\"{}\"", short.as_deref().unwrap_or(value)));
        rest = after;
    }
    out.push_str(rest);
    (out, changed)
}

// True for an attribute value computed by Razor or a template, which is left alone.
fn dynamic(value: &str) -> bool {
    value.contains('@') || value.contains("{{") || value.contains("${")
}

// Finds the next double-quoted class or style attribute: the text before it, its name, its value and the text after it.
fn next_attr(s: &str) -> Option<(&str, &str, &str, &str)> {
    let mut from = 0;
    loop {
        let at = from + s[from..].find("=\"")?;
        let name_start =
            s[..at].rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '-')).map_or(0, |p| p + 1);
        let name = &s[name_start..at];
        let close = at + 2 + s[at + 2..].find('"')?;
        let spaced = name_start > 0 && s[..name_start].ends_with(char::is_whitespace);
        if spaced && (name == "class" || name == "style") {
            return Some((&s[..name_start], name, &s[at + 2..close], &s[close + 1..]));
        }
        from = close + 1;
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    // Returns the view's lines after the header.
    fn body(text: &str, kind: Kind) -> Vec<String> {
        let v = view("f", text, kind).expect("view");
        v.lines().skip(1).map(str::to_string).collect()
    }

    // Checks file extensions map to their markup kind.
    #[test]
    fn test_kind_of() {
        assert_eq!(kind_of(Path::new("a/index.HTML")), Some(Kind::Html));
        assert_eq!(kind_of(Path::new("Index.cshtml")), Some(Kind::Razor));
        assert_eq!(kind_of(Path::new("App.razor")), Some(Kind::Razor));
        assert_eq!(kind_of(Path::new("a.cs")), None);
        assert_eq!(kind_of(Path::new("Makefile")), None);
    }

    // Checks comments, styles, SVGs and head tags collapse to notes that keep their line numbers.
    #[test]
    fn test_view_html() {
        let html = "<html>\n<head>\n<meta charset=\"utf-8\">\n<link rel=\"icon\" href=\"i.png\">\n<style>\nb { x: 1 }\n</style>\n</head>\n<!-- one -->\n<!--\ntwo\n-->\n<svg viewBox=\"0 0 1 1\">\n<svg><path/></svg>\n</svg>\n\n\n<p>hi</p>";
        let v = view("f.html", html, Kind::Html).expect("view");
        assert!(v.starts_with("f.html 18 lines; terse: 1 head tags, 1 styles, 1 comments, 1 svgs\n"), "{v}");
        assert_eq!(
            body(html, Kind::Html),
            [
                "1\t<html>",
                "2\t<head>",
                "3-4\t⋯ 2 meta/link tag(s)",
                "5-7\t⋯ <style>, 3 line(s)",
                "8\t</head>",
                "9\t<!-- one -->",
                "10-12\t⋯ comment, 3 line(s)",
                "13-15\t⋯ <svg>, 3 line(s)",
                "16\t",
                "18\t<p>hi</p>"
            ]
        );
    }

    // Checks long class and style values are shortened and dynamic or short ones are not.
    #[test]
    fn test_shorten() {
        assert_eq!(
            shorten(
                r#"<div class="a b c d e" id="x" style="color: red; margin: 0 auto; padding: 4px 8px 4px">"#
            ),
            (r#"<div class="a b c …" id="x" style="…">"#.to_string(), 2)
        );
        assert_eq!(shorten(r#"<p class="a b">"#), (r#"<p class="a b">"#.to_string(), 0));
        assert_eq!(shorten(r#"<p class="@x a b c d e">"#).1, 0);
        assert_eq!(shorten(r#"<p data-class="a b c d e f">"#).1, 0);
        assert_eq!(shorten(r#"<p class="a b c d e"#).1, 0);
        assert_eq!(body("<i class=\"a b c d e\"></i>", Kind::Html), ["1~\t<i class=\"a b c …\"></i>"]);
    }

    // Checks scripts, pre blocks and Razor code blocks are shown as they are.
    #[test]
    fn test_verbatim() {
        let html = "<script>\n// <!-- not a comment -->\nlet c = '<p class=\"a b c d e\">';\n</script>\n<pre>\n<!-- kept -->\n</pre>\n<!--\nx\n-->";
        assert_eq!(
            body(html, Kind::Html)[..7],
            [
                "1\t<script>",
                "2\t// <!-- not a comment -->",
                "3\tlet c = '<p class=\"a b c d e\">';",
                "4\t</script>",
                "5\t<pre>",
                "6\t<!-- kept -->",
                "7\t</pre>"
            ]
        );
        let razor = "@code\n{\n    int x = 1; // <!-- y -->\n}\n@{ var a = 1; }\n@* gone\nx\n*@";
        assert_eq!(
            body(razor, Kind::Razor),
            [
                "1\t@code",
                "2\t{",
                "3\t    int x = 1; // <!-- y -->",
                "4\t}",
                "5\t@{ var a = 1; }",
                "6-8\t⋯ comment, 3 line(s)"
            ]
        );
        assert_eq!(braces_end(&["@code {", "x"], 0), 1);
        assert_eq!(verbatim(&["<script src=a.js>"], 0, Kind::Html), Some(0));
        assert_eq!(verbatim(&["@code {"], 0, Kind::Html), None);
    }

    // Checks Razor usings, tag helpers and comments collapse, and plain HTML keeps @-lines.
    #[test]
    fn test_view_razor() {
        let razor = "@page \"/\"\n@using A\n@using B\n@addTagHelper *, X\n@addTagHelper *, Y\n@*\nnote\n*@\n<h1>@Title</h1>";
        assert_eq!(
            body(razor, Kind::Razor),
            [
                "1\t@page \"/\"",
                "2-5\t⋯ 4 @using/tag helper line(s)",
                "6-8\t⋯ comment, 3 line(s)",
                "9\t<h1>@Title</h1>"
            ]
        );
        assert_eq!(view("f", "@using A\n@using B\n@* x *@", Kind::Html), None);
    }

    // Checks blocks that do not close cleanly are left as source.
    #[test]
    fn test_unclosed() {
        assert_eq!(view("f", "<!-- open\n<p>", Kind::Html), None);
        assert_eq!(view("f", "<!-- a --> <p>", Kind::Html), None);
        assert_eq!(view("f", "<style>\nb{}", Kind::Html), None);
        assert_eq!(view("f", "<svg>\n</svg><p>", Kind::Html), None);
        assert_eq!(view("f", "<meta a>\n<p>", Kind::Html), None);
        assert!(opens("<style>", "style") && opens("<svg", "svg") && !opens("<svgx>", "svg"));
    }
}
