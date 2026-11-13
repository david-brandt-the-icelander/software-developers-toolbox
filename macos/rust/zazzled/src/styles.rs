// Terse view of CSS, SCSS, Sass and Less files: hides comments of 4+ lines, source map comments, import runs, @font-face and @keyframes blocks, vendor-prefixed duplicates and SCSS @debug lines, and shortens inline data URIs, keeping source line numbers.
use crate::markup::Plan;
use std::collections::HashSet;
use std::path::Path;

const LONG_COMMENT: usize = 4;
const LONG_DATA_URI: usize = 60;
const PREFIXES: [&str; 4] = ["-webkit-", "-moz-", "-ms-", "-o-"];
const IMPORTS: [&str; 3] = ["@import ", "@use ", "@forward "];

// The stylesheet languages the view handles.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    Css,
    Scss,
    Sass,
    Less,
}

// Returns the stylesheet kind of a path from its extension, if it has one.
pub fn kind_of(path: &Path) -> Option<Kind> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "css" => Some(Kind::Css),
        "scss" => Some(Kind::Scss),
        "sass" => Some(Kind::Sass),
        "less" => Some(Kind::Less),
        _ => None,
    }
}

// Returns the view of a file, or None when nothing would be shortened: a header with the line count and what was shortened, then `N`, `N~` and `A-B` lines.
pub fn view(shown: &str, text: &str, kind: Kind) -> Option<String> {
    let src: Vec<&str> = text.lines().collect();
    let blocks = if kind == Kind::Sass { indent_block_ids(&src) } else { block_ids(&src) };
    let plain = plain_properties(&src, &blocks);
    let mut plan = Plan { lines: Vec::new(), steps: Vec::new() };
    let mut i = 0;
    while i < src.len() {
        let t = src[i].trim();
        if let Some(end) = comment_end(&src, i, kind) {
            let n = end - i + 1;
            if t.starts_with("/*# sourceMappingURL") {
                plan.hide(i, end, "source map".into(), "comments");
            } else {
                plan.hide(i, end, format!("comment, {n} line(s)"), "comments");
            }
            i = end + 1;
        } else if let Some(end) = run(&src, i, |t| {
            IMPORTS.iter().any(|p| t.starts_with(p)) && (t.ends_with(';') || kind == Kind::Sass)
        }) {
            plan.hide(i, end, format!("{} import(s)", end - i + 1), "imports");
            i = end + 1;
        } else if let Some((end, name)) =
            if kind == Kind::Sass { indented_at_block(&src, i) } else { at_block(&src, i) }
        {
            plan.hide(i, end, format!("{name}, {} line(s)", end - i + 1), "font-face and keyframes blocks");
            i = end + 1;
        } else if t.starts_with("@debug ") && (kind == Kind::Scss && t.ends_with(';') || kind == Kind::Sass) {
            plan.count("log statements");
            i += 1;
        } else if prefixed(t, kind).is_some_and(|p| plain.contains(&(blocks[i], p.to_string()))) {
            plan.count("vendor prefixes");
            i += 1;
        } else {
            let (short, n) = shorten_data_uris(src[i]);
            (0..n).for_each(|_| plan.count("data URIs"));
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

// Returns, for each line, the id of the innermost block open at its start; braces in comments and strings are counted too, which only makes the view keep more.
fn block_ids(src: &[&str]) -> Vec<usize> {
    let (mut stack, mut next, mut out) = (vec![0usize], 1usize, Vec::with_capacity(src.len()));
    for line in src {
        out.push(*stack.last().unwrap_or(&0));
        for c in line.chars() {
            match c {
                '{' => {
                    stack.push(next);
                    next += 1;
                }
                '}' if stack.len() > 1 => {
                    stack.pop();
                }
                _ => {}
            }
        }
    }
    out
}

// Returns the unprefixed properties each block declares, as (block id, property) pairs.
fn plain_properties(src: &[&str], blocks: &[usize]) -> HashSet<(usize, String)> {
    let mut out = HashSet::new();
    for (i, line) in src.iter().enumerate() {
        if let Some((prop, _)) = line.trim().split_once(':') {
            let prop = prop.trim();
            if !prop.starts_with('-')
                && prop.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
                && !prop.is_empty()
            {
                out.insert((blocks[i], prop.to_ascii_lowercase()));
            }
        }
    }
    out
}

// Returns the property a vendor-prefixed declaration line sets without its prefix, like `transition` for `-webkit-transition: x;` (Sass needs no `;`).
fn prefixed(t: &str, kind: Kind) -> Option<&str> {
    let (prop, rest) = t.split_once(':')?;
    let bare = PREFIXES.iter().find_map(|p| prop.trim().strip_prefix(p))?;
    let ended = if kind == Kind::Sass { !rest.trim().is_empty() } else { rest.trim_end().ends_with(';') };
    (ended && bare.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')).then_some(bare)
}

// Returns the last line of a comment starting line i that is long enough to hide: `/* */` of 4+ lines or any source map comment, 4+ SCSS or Less `//` lines, or a Sass comment of 4+ lines including the deeper-indented lines it covers.
fn comment_end(src: &[&str], i: usize, kind: Kind) -> Option<usize> {
    let t = src[i].trim_start();
    if kind == Kind::Sass && (t.starts_with("//") || t.starts_with("/*") && !t.contains("*/")) {
        let end = indented_end(src, i, |l| l.trim_start().starts_with("//") && indent(l) == indent(src[i]));
        return (end + 1 - i >= LONG_COMMENT).then_some(end);
    }
    if t.starts_with("/*") {
        let end = (i..src.len()).find(|&j| src[j].contains("*/"))?;
        let ends_line = src[end].trim_end().ends_with("*/");
        let map = t.starts_with("/*# sourceMappingURL");
        return (ends_line && (map || end + 1 - i >= LONG_COMMENT)).then_some(end);
    }
    if kind != Kind::Css {
        let n = src[i..].iter().take_while(|l| l.trim_start().starts_with("//")).count();
        return (n >= LONG_COMMENT).then(|| i + n - 1);
    }
    None
}

// Returns the last line of a run of at least two lines from i whose trimmed text passes the test.
fn run(src: &[&str], i: usize, test: impl Fn(&str) -> bool) -> Option<usize> {
    let n = src[i..].iter().take_while(|l| test(l.trim())).count();
    (n >= 2).then(|| i + n - 1)
}

// Returns a line's indentation width.
fn indent(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

// Returns the last line of an indented block that opens line i: the following lines indented deeper, or passing the test, ignoring trailing blank lines.
fn indented_end(src: &[&str], i: usize, also: impl Fn(&str) -> bool) -> usize {
    let base = indent(src[i]);
    let mut end = i;
    for (j, line) in src.iter().enumerate().skip(i + 1) {
        if line.trim().is_empty() {
            continue;
        }
        if indent(line) > base || also(line) {
            end = j;
        } else {
            break;
        }
    }
    end
}

// Returns, for each Sass line, the id of the line it is nested under by indentation (0 at the top).
fn indent_block_ids(src: &[&str]) -> Vec<usize> {
    let mut stack: Vec<(usize, usize)> = Vec::new();
    let mut out = Vec::with_capacity(src.len());
    for (i, line) in src.iter().enumerate() {
        if line.trim().is_empty() {
            out.push(stack.last().map_or(0, |s| s.1));
            continue;
        }
        while stack.last().is_some_and(|s| s.0 >= indent(line)) {
            stack.pop();
        }
        out.push(stack.last().map_or(0, |s| s.1));
        stack.push((indent(line), i + 1));
    }
    out
}

// Returns the last line and name of a Sass `@font-face` or `@keyframes` block that opens line i and has deeper-indented lines.
fn indented_at_block(src: &[&str], i: usize) -> Option<(usize, String)> {
    let t = src[i].trim();
    let at = ["@font-face", "@keyframes", "@-webkit-keyframes", "@-moz-keyframes"]
        .iter()
        .find(|a| t.starts_with(**a))?;
    let end = indented_end(src, i, |_| false);
    let family = (*at == "@font-face").then(|| font_family(&src[i..=end])).flatten();
    let name = family.map_or(t.to_string(), |f| format!("@font-face {f}"));
    (end > i).then_some((end, name))
}

// Returns the closing line and name of an `@font-face` or `@keyframes` block that opens line i and closes alone on a later line.
fn at_block(src: &[&str], i: usize) -> Option<(usize, String)> {
    let t = src[i].trim();
    let at = ["@font-face", "@keyframes", "@-webkit-keyframes", "@-moz-keyframes"]
        .iter()
        .find(|a| t.starts_with(**a))?;
    let head = t.strip_suffix('{')?.trim();
    let mut depth = 0i64;
    for (j, line) in src.iter().enumerate().skip(i) {
        depth += line.matches('{').count() as i64 - line.matches('}').count() as i64;
        if depth <= 0 {
            let family = (*at == "@font-face").then(|| font_family(&src[i..=j])).flatten();
            let name = family.map_or(head.to_string(), |f| format!("@font-face {f}"));
            return (line.trim() == "}" && j > i).then_some((j, name));
        }
    }
    None
}

// Returns the font-family a @font-face block declares, if any.
fn font_family(block: &[&str]) -> Option<String> {
    let line = block.iter().find(|l| l.trim_start().starts_with("font-family"))?;
    Some(line.split_once(':')?.1.trim().trim_end_matches(';').trim().to_string())
}

// Shortens `url(data:...)` values longer than 60 characters to `url(data:…)`; returns the line and how many it changed.
fn shorten_data_uris(line: &str) -> (String, usize) {
    let (mut out, mut rest, mut n) = (String::new(), line, 0);
    while let Some(at) = rest.find("url(") {
        let inner = &rest[at + 4..];
        let Some(close) = inner.find(')') else { break };
        let value = inner[..close].trim().trim_matches(['"', '\'']);
        out.push_str(&rest[..at + 4]);
        if value.starts_with("data:") && value.len() > LONG_DATA_URI {
            out.push_str("data:…");
            n += 1;
        } else {
            out.push_str(&inner[..close]);
        }
        out.push(')');
        rest = &inner[close + 1..];
    }
    out.push_str(rest);
    (out, n)
}
#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    // Returns the view's lines after the header.
    fn body(text: &str, kind: Kind) -> Vec<String> {
        view("f", text, kind).expect("view").lines().skip(1).map(str::to_string).collect()
    }

    // Checks file extensions map to their stylesheet kind.
    #[test]
    fn test_kind_of() {
        assert_eq!(kind_of(Path::new("a/site.CSS")), Some(Kind::Css));
        assert_eq!(kind_of(Path::new("_buttons.scss")), Some(Kind::Scss));
        assert_eq!(kind_of(Path::new("a.sass")), Some(Kind::Sass));
        assert_eq!(kind_of(Path::new("a.less")), Some(Kind::Less));
        assert_eq!(kind_of(Path::new("a.styl")), None);
        assert_eq!(kind_of(Path::new("README")), None);
    }

    // Checks every rule on one stylesheet, with source line numbers.
    #[test]
    fn test_view_css() {
        let css = "/*!\n * Kit v1\n * MIT\n */\n@import \"a.css\";\n@import \"b.css\";\n@font-face {\n  font-family: \"Inter\";\n  src: url(inter.woff2);\n}\n@keyframes fade {\n  from { opacity: 0; }\n  to { opacity: 1; }\n}\n.btn {\n  -webkit-transition: color .2s;\n  transition: color .2s;\n  -webkit-tap-highlight-color: red;\n  background: url(\"data:image/svg+xml,%3csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 16 16'%3e%3c/svg%3e\") no-repeat;\n}\n/*# sourceMappingURL=kit.css.map */";
        let v = view("kit.css", css, Kind::Css).expect("view");
        assert!(v.starts_with("kit.css 21 lines; terse: 2 comments, 1 imports, 2 font-face and keyframes blocks, 1 vendor prefixes, 1 data URIs\n"), "{v}");
        assert_eq!(
            body(css, Kind::Css),
            [
                "1-4\t⋯ comment, 4 line(s)",
                "5-6\t⋯ 2 import(s)",
                "7-10\t⋯ @font-face \"Inter\", 4 line(s)",
                "11-14\t⋯ @keyframes fade, 4 line(s)",
                "15\t.btn {",
                "17\t  transition: color .2s;",
                "18\t  -webkit-tap-highlight-color: red;",
                "19~\t  background: url(data:…) no-repeat;",
                "20\t}",
                "21-21\t⋯ source map",
            ]
        );
    }

    // Checks SCSS comment runs and @debug lines, and that CSS keeps them.
    #[test]
    fn test_view_scss() {
        let scss = "@use \"sass:math\";\n@forward \"vars\";\n// one\n// two\n// three\n// four\n@mixin m($a) {\n  @debug \"a is #{$a}\";\n  width: $a;\n}\n";
        assert_eq!(
            body(scss, Kind::Scss),
            [
                "1-2\t⋯ 2 import(s)",
                "3-6\t⋯ comment, 4 line(s)",
                "7\t@mixin m($a) {",
                "9\t  width: $a;",
                "10\t}"
            ]
        );
        assert!(view("f", "// a\n// b\n// c\n// d\n@debug x;\n", Kind::Css).is_none());
    }

    // Checks what is kept: short comments, one import, unclosed or shared-line blocks, prefixes from other blocks, short data URIs.
    #[test]
    fn test_kept() {
        let none = |s: &str| view("f", s, Kind::Css).is_none();
        assert!(none("/*\n * Section\n */\na { color: red; }\n"));
        assert!(none("/* a\nb\nc\nd */ a { }\n"));
        assert!(none("/* open\n\n\n\n"));
        assert!(none("@import \"a.css\";\na { }\n"));
        assert!(none("@font-face {\n  font-family: x;\n} a { }\n"));
        assert!(none("@font-face { font-family: x; }\n"));
        assert!(none("@keyframes k {\n  from { }\n"));
        assert!(none("@font-face\n{\n}\n"));
        assert!(none("a {\n  -webkit-transition: x;\n}\nb {\n  transition: x;\n}\n"));
        assert!(none("a {\n  -webkit-transition: x\n  transition: x;\n}\n"));
        assert!(none("a {\n  background: url(data:image/png;base64,AAAA);\n  b: url(x.png;\n}\n"));
        assert!(none("}\n}\na { }\n"));
        assert_eq!(body("@font-face {\n  src: url(a);\n}\n", Kind::Css), ["1-3\t⋯ @font-face, 3 line(s)"]);
    }

    // Checks the indented Sass rules: imports without semicolons, comment blocks by indentation, @font-face blocks, @debug and prefixes by block.
    #[test]
    fn test_view_sass() {
        let sass = "@use \"a\"\n@import \"b\"\n/* Header\n   line two\n   line three\n   line four\n@font-face\n  font-family: \"Inter\"\n  src: url(a.woff2)\n=reset\n  -moz-appearance: none\n  appearance: none\n  -webkit-tap-highlight-color: red\n  @debug \"x\"\n.a\n  -webkit-transition: x\n.b\n  transition: x\n";
        assert_eq!(
            body(sass, Kind::Sass),
            [
                "1-2\t⋯ 2 import(s)",
                "3-6\t⋯ comment, 4 line(s)",
                "7-9\t⋯ @font-face \"Inter\", 3 line(s)",
                "10\t=reset",
                "12\t  appearance: none",
                "13\t  -webkit-tap-highlight-color: red",
                "15\t.a",
                "16\t  -webkit-transition: x",
                "17\t.b",
                "18\t  transition: x",
            ]
        );
        assert!(view("f", "// a\n// b\n.x\n  color: red\n", Kind::Sass).is_none());
        assert!(view("f", "@keyframes k\n.x\n  color: red\n", Kind::Sass).is_none());
        let deep = "// one\n   two\n   three\n// four\n.x\n  color: red\n";
        assert_eq!(body(deep, Kind::Sass)[0], "1-4\t⋯ comment, 4 line(s)");
    }

    // Checks Less uses the brace rules with `//` comments and no @debug.
    #[test]
    fn test_view_less() {
        let less = "// a\n// b\n// c\n// d\n@import (reference) \"x.less\";\n@import \"y.less\";\n.m() {\n  -webkit-box-shadow: @s;\n  box-shadow: @s;\n}\n@debug x;\n";
        assert_eq!(
            body(less, Kind::Less),
            [
                "1-4\t⋯ comment, 4 line(s)",
                "5-6\t⋯ 2 import(s)",
                "7\t.m() {",
                "9\t  box-shadow: @s;",
                "10\t}",
                "11\t@debug x;"
            ]
        );
    }

    // Checks blank lines inside indented Sass blocks keep the block together.
    #[test]
    fn test_sass_blank_lines() {
        let sass = "// one\n\n   two\n   three\n.x\n\n  -moz-appearance: none\n\n  appearance: none\n";
        assert_eq!(
            body(sass, Kind::Sass),
            ["1-4\t⋯ comment, 4 line(s)", "5\t.x", "6\t", "9\t  appearance: none"]
        );
    }
}
