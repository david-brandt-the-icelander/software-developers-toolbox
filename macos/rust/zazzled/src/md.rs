// Terse view of Markdown files, line by line: hides HTML comments, badge lines, link reference definitions and tables of contents, leaving fenced code and front matter alone, keeping source line numbers.
use crate::markup::Plan;
use std::path::Path;

const MIN_TOC: usize = 3;

// True for a Markdown path.
pub fn is_markdown(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "md" | "markdown" | "mdx"))
}

// Returns the view of a file, or None when nothing would be shortened: a header, then `N` and `A-B` lines.
pub fn view(shown: &str, text: &str) -> Option<String> {
    let src: Vec<&str> = text.lines().collect();
    let prose = prose_lines(&src);
    let mut plan = Plan { lines: Vec::new(), steps: Vec::new() };
    let mut i = 0;
    while i < src.len() {
        if !prose[i] {
            plan.keep(i, src[i], false);
            i += 1;
        } else if let Some(end) = comment_end(&src, &prose, i) {
            if end > i {
                plan.hide(i, end, format!("comment, {} line(s)", end - i + 1), "comments");
            } else {
                plan.count("comments");
            }
            i = end + 1;
        } else if let Some(end) = run(&src, &prose, i, is_badges) {
            plan.hide(i, end, format!("{} badge line(s)", end - i + 1), "badges");
            i = end + 1;
        } else if let Some(end) = run(&src, &prose, i, is_link_definition).filter(|&e| e > i) {
            plan.hide(i, end, format!("{} link definition(s)", end - i + 1), "link definitions");
            i = end + 1;
        } else if let Some(end) = run(&src, &prose, i, is_toc_entry).filter(|&e| e + 1 - i >= MIN_TOC) {
            plan.hide(i, end, format!("table of contents, {} entries", end - i + 1), "tables of contents");
            i = end + 1;
        } else {
            plan.keep(i, src[i], false);
            i += 1;
        }
    }
    if plan.steps.is_empty() {
        return None;
    }
    let steps: Vec<String> = plan.steps.iter().map(|(s, n)| format!("{n} {s}")).collect();
    Some(format!("{shown} {} lines; terse: {}\n{}", src.len(), steps.join(", "), plan.lines.join("\n")))
}

// Marks the lines the rules may touch: not inside fenced code (``` or ~~~) or `---` front matter at the top.
fn prose_lines(src: &[&str]) -> Vec<bool> {
    let mut out = vec![true; src.len()];
    let mut fence: Option<(char, usize)> = None;
    let front = src.first().is_some_and(|l| l.trim_end() == "---");
    let front_end =
        if front { src.iter().skip(1).position(|l| l.trim_end() == "---").map(|p| p + 1) } else { None };
    for (i, line) in src.iter().enumerate() {
        if front_end.is_some_and(|e| i <= e) {
            out[i] = false;
            continue;
        }
        let t = line.trim_start();
        let marker = t.chars().next().filter(|c| matches!(c, '`' | '~'));
        let len = marker.map_or(0, |m| t.chars().take_while(|&c| c == m).count());
        match (fence, marker) {
            (None, Some(m)) if len >= 3 => {
                fence = Some((m, len));
                out[i] = false;
            }
            (Some((m, n)), Some(c)) if c == m && len >= n && t[len..].trim().is_empty() => {
                fence = None;
                out[i] = false;
            }
            (Some(_), _) => out[i] = false,
            _ => {}
        }
    }
    out
}

// Returns the last line of an HTML comment that starts line i and ends a line, alone on its lines.
fn comment_end(src: &[&str], prose: &[bool], i: usize) -> Option<usize> {
    if !src[i].trim_start().starts_with("<!--") {
        return None;
    }
    let end = (i..src.len()).take_while(|&j| prose[j]).find(|&j| src[j].contains("-->"))?;
    src[end].trim_end().ends_with("-->").then_some(end)
}

// Returns the last line of a run of lines from i that pass the test, or None if line i does not.
fn run(src: &[&str], prose: &[bool], i: usize, test: fn(&str) -> bool) -> Option<usize> {
    let n = (i..src.len()).take_while(|&j| prose[j] && test(src[j].trim())).count();
    (n > 0).then(|| i + n - 1)
}

// True for a line made only of badge images, `![a](b)` or `[![a](b)](c)`, separated by spaces.
fn is_badges(t: &str) -> bool {
    let mut rest = t;
    while !rest.is_empty() {
        match badge_end(rest) {
            Some(after) => rest = after.trim_start(),
            None => return false,
        }
    }
    !t.is_empty()
}

// Returns the text after a badge, `![a](b)` or `[![a](b)](c)`, that starts the text.
fn badge_end(t: &str) -> Option<&str> {
    match t.strip_prefix('[') {
        Some(inner) => link_target_end(image_end(inner)?.strip_prefix(']')?),
        None => image_end(t),
    }
}

// Returns the text after a badge image `![alt](src)` that starts the text; the source must look like a badge (shields.io, badgen, codecov, coveralls, `badge` or `.svg`).
fn image_end(t: &str) -> Option<&str> {
    let alt = t.strip_prefix("![")?;
    let close = alt.find("](")?;
    let src = &alt[close + 2..];
    let src = &src[..src.find(')').unwrap_or(src.len())];
    let badge =
        ["shields.io", "badgen", "codecov", "coveralls", "badge", ".svg"].iter().any(|m| src.contains(m));
    badge.then(|| link_target_end(&alt[close + 1..])).flatten()
}

// Returns the text after a link target `(url)` that starts the text.
fn link_target_end(t: &str) -> Option<&str> {
    let inner = t.strip_prefix('(')?;
    let close = inner.find(')')?;
    (!inner[..close].contains(char::is_whitespace) || inner[..close].contains('"'))
        .then(|| &inner[close + 1..])
}

// True for a link reference definition, `[label]: url`.
fn is_link_definition(t: &str) -> bool {
    t.starts_with('[')
        && t.find("]:").is_some_and(|i| i > 1 && !t[i + 2..].trim().is_empty() && !t[..i].contains('\n'))
}

// True for a table of contents entry: a list item that is only a link to an anchor, `- [Intro](#intro)`.
fn is_toc_entry(t: &str) -> bool {
    let item = t
        .strip_prefix(['-', '*', '+'])
        .or_else(|| t.split_once(". ").filter(|(n, _)| n.chars().all(|c| c.is_ascii_digit())).map(|(_, r)| r))
        .map(str::trim_start);
    item.and_then(|r| r.strip_prefix('['))
        .and_then(|r| r.split_once("](#"))
        .is_some_and(|(_, rest)| rest.find(')').is_some_and(|c| rest[c + 1..].trim().is_empty()))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    const README: &str = r#"---
title: x
---
<!-- markdownlint-disable -->
# Tool

[![CI](https://x/ci.svg)](https://x/ci) [![Crates](https://x/c.svg)](https://x/c)
![License](https://x/l.svg)

## Contents

- [Install](#install)
  - [From source](#from-source)
- [Usage](#usage)

<!--
long note
-->

## Install

```md
<!-- kept in code -->
- [a](#a)
- [b](#b)
- [c](#c)
```

See [the docs][docs] and [notes](#usage).

[docs]: https://x/docs
[api]: https://x/api
"#;

    // Checks comments, badges, the table of contents and link definitions are hidden, and front matter, code and prose stay.
    #[test]
    fn test_view_markdown() {
        let v = view("README.md", README).expect("view");
        let lines: Vec<&str> = v.lines().collect();
        assert_eq!(
            lines[0],
            "README.md 32 lines; terse: 2 comments, 1 badges, 1 tables of contents, 1 link definitions"
        );
        for want in [
            "1\t---",
            "2\ttitle: x",
            "5\t# Tool",
            "7-8\t⋯ 2 badge line(s)",
            "12-14\t⋯ table of contents, 3 entries",
            "16-18\t⋯ comment, 3 line(s)",
            "23\t<!-- kept in code -->",
            "24\t- [a](#a)",
            "29\tSee [the docs][docs] and [notes](#usage).",
            "31-32\t⋯ 2 link definition(s)",
        ] {
            assert!(lines.contains(&want), "{want}: {v}");
        }
        assert!(!lines.iter().any(|l| l.starts_with("4\t")));
        let fences = "````md\n```\n<!-- in -->\n````\n~~\n<!-- out -->\n~~~\n```\n<!-- in -->\n~~~~\n";
        assert!(view("a.md", fences).is_some_and(|v| v.contains("3\t<!-- in -->")
            && !v.contains("6\t")
            && v.contains("9\t<!-- in -->")));
        assert!(view("a.md", "# A\n\n- [a](#a)\n- [b](#b)\n\n[x]: y\ntext <!-- c -->\n<!-- open\n").is_none());
        assert!(
            is_markdown(Path::new("a/B.MD"))
                && is_markdown(Path::new("x.mdx"))
                && !is_markdown(Path::new("x.txt"))
        );
    }

    // Checks badge, link definition and table of contents line tests on edge cases.
    #[test]
    fn test_line_kinds() {
        assert!(is_badges("![a](b.svg) [![c](https://img.shields.io/x)](e)"));
        assert!(!is_badges("![a](b.svg) text") && !is_badges("[a](b)") && !is_badges("![a](b.svg c)"));
        assert!(!is_badges("") && !is_badges("![chart](https://x/bench.png)"));
        assert!(
            is_link_definition("[a]: http://x")
                && !is_link_definition("[]: x")
                && !is_link_definition("[a]:")
        );
        assert!(is_toc_entry("1. [A](#a)") && is_toc_entry("* [A](#a)"));
        assert!(
            !is_toc_entry("- [A](#a) and more")
                && !is_toc_entry("- [A](http://a)")
                && !is_toc_entry("x. [A](#a)")
        );
    }
}
