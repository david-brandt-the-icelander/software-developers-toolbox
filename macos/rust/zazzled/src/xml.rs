// Terse view of XML files (.xml, project files, XAML, plists, resources, schemas), line by line: collapses comments of 3+ lines and runs of 10+ sibling elements with the same tag to their first and last, and cuts long attribute values and blobs, keeping source line numbers.
use crate::data::{shorten_strings, shorten_words};
use crate::markup::Plan;
use std::path::Path;

const LONG_RUN: usize = 10;
const LONG_COMMENT: usize = 3;

// True for an XML path by its extension.
pub fn is_xml(path: &Path) -> bool {
    const EXTS: [&str; 17] = [
        "xml", "csproj", "fsproj", "vbproj", "vcxproj", "props", "targets", "xaml", "axaml", "plist", "resx",
        "xsd", "xsl", "xslt", "wsdl", "nuspec", "manifest",
    ];
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| EXTS.contains(&e.to_ascii_lowercase().as_str()))
}

// Returns the view of a file, or None when nothing would be shortened: a header, then `N`, `N~` and `A-B` lines.
pub fn view(shown: &str, text: &str) -> Option<String> {
    let src: Vec<&str> = text.lines().collect();
    let mut plan = Plan { lines: Vec::new(), steps: Vec::new() };
    let mut i = 0;
    while i < src.len() {
        if let Some(end) = comment_end(&src, i) {
            if end + 1 - i >= LONG_COMMENT {
                plan.hide(i, end, format!("comment, {} line(s)", end - i + 1), "comments");
            } else {
                (i..=end).for_each(|l| plan.keep(l, src[l], false));
            }
            i = end + 1;
            continue;
        }
        if let Some((blocks, tag)) = run(&src, i) {
            let (first, last) = (blocks[0], blocks[blocks.len() - 1]);
            let (second, before_last) = (blocks[1].0, blocks[blocks.len() - 2].1);
            (first.0..second).for_each(|l| keep(&mut plan, src[l], l));
            plan.hide(second, before_last, format!("{} <{tag}> element(s)", blocks.len() - 2), "elements");
            (before_last + 1..=last.1).for_each(|l| keep(&mut plan, src[l], l));
            i = last.1 + 1;
            continue;
        }
        keep(&mut plan, src[i], i);
        i += 1;
    }
    plan.steps.retain(|(_, n)| *n > 0);
    if plan.steps.is_empty() {
        return None;
    }
    let steps: Vec<String> = plan.steps.iter().map(|(s, n)| format!("{n} {s}")).collect();
    Some(format!("{shown} {} lines; terse: {}\n{}", src.len(), steps.join(", "), plan.lines.join("\n")))
}

// Keeps a line, cutting long attribute values and blobs.
fn keep(plan: &mut Plan, line: &str, i: usize) {
    let (s, a) = shorten_strings(line);
    let (s, b) = shorten_words(&s);
    plan.add("long values", a + b);
    plan.keep(i, &s, a + b > 0);
}

// Returns the last line of a comment that starts line i alone and ends a line.
fn comment_end(src: &[&str], i: usize) -> Option<usize> {
    if !src[i].trim_start().starts_with("<!--") {
        return None;
    }
    let end = (i..src.len()).find(|&j| src[j].contains("-->"))?;
    src[end].trim_end().ends_with("-->").then_some(end)
}

// Returns the tag name of an element that starts a trimmed line, `<Name ...`.
fn tag(t: &str) -> Option<&str> {
    let rest = t.strip_prefix('<')?;
    let end = rest.find(|c: char| c.is_whitespace() || c == '>' || c == '/').unwrap_or(rest.len());
    let name = &rest[..end];
    (!name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | ':')))
        .then_some(name)
}

// Returns the last line of the element that starts line i: the same line when it closes there, else the line with its closing tag at the same indentation.
fn element_end(src: &[&str], i: usize) -> Option<usize> {
    let t = src[i].trim();
    let name = tag(t)?;
    let close = format!("</{name}>");
    if t.ends_with("/>") || t.ends_with(&close) {
        return Some(i);
    }
    let k = src[i].len() - src[i].trim_start().len();
    src.iter()
        .enumerate()
        .skip(i + 1)
        .find(|(_, l)| l.len() - l.trim_start().len() == k && !l.trim().is_empty())
        .filter(|(_, l)| l.trim() == close)
        .map(|(j, _)| j)
}

// Returns the elements of a run of 10+ siblings with the same tag starting at line i, and the tag; blank lines may separate them.
fn run<'s>(src: &[&'s str], i: usize) -> Option<(Vec<(usize, usize)>, &'s str)> {
    let name = tag(src[i].trim())?;
    let k = src[i].len() - src[i].trim_start().len();
    let mut blocks = Vec::new();
    let mut start = i;
    while let Some(end) = element_end(src, start) {
        blocks.push((start, end));
        let next = (end + 1..src.len()).find(|&l| !src[l].trim().is_empty());
        match next {
            Some(n) if src[n].len() - src[n].trim_start().len() == k && tag(src[n].trim()) == Some(name) => {
                start = n
            }
            _ => break,
        }
    }
    (blocks.len() >= LONG_RUN).then_some((blocks, name))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    // Checks comments, runs of single-line and multi-line elements, long values, and what stays: short runs, mixed tags, short comments.
    #[test]
    fn test_view_xml() {
        let refs: String = (1..=12)
            .map(|n| format!("    <PackageReference Include=\"P{n}\" Version=\"1.0\" />\n"))
            .collect();
        let deps: String = (1..=10)
            .map(|n| format!("    <dependency>\n      <artifactId>a{n}</artifactId>\n    </dependency>\n\n"))
            .collect();
        let blob = "A".repeat(130);
        let src = format!("<!--\n  licence\n-->\n<Project>\n  <!-- one -->\n  <ItemGroup>\n{refs}  </ItemGroup>\n  <deps>\n{deps}  </deps>\n  <data name=\"x\"><value>{blob}</value></data>\n</Project>\n");
        let v = view("a.csproj", &src).expect("view");
        assert_eq!(v, "a.csproj 63 lines; terse: 1 comments, 1 long values, 2 elements\n1-3\t⋯ comment, 3 line(s)\n4\t<Project>\n5\t  <!-- one -->\n6\t  <ItemGroup>\n7\t    <PackageReference Include=\"P1\" Version=\"1.0\" />\n8-17\t⋯ 10 <PackageReference> element(s)\n18\t    <PackageReference Include=\"P12\" Version=\"1.0\" />\n19\t  </ItemGroup>\n20\t  <deps>\n21\t    <dependency>\n22\t      <artifactId>a1</artifactId>\n23\t    </dependency>\n24\t\n25-55\t⋯ 8 <dependency> element(s)\n56\t\n57\t    <dependency>\n58\t      <artifactId>a10</artifactId>\n59\t    </dependency>\n60\t\n61\t  </deps>\n62~\t  <data name=\"x\"><value>AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA…</value></data>\n63\t</Project>");
        assert!(view("a.xml", "<a>\n  <b/>\n  <c/>\n  <b/>\n  <!-- x\n  -->\n</a>\n").is_none());
        assert!(view("a.xml", "<a>\n  <b>\n    <c/>\n</a>\n<!-- open\n").is_none());
        assert!(
            is_xml(Path::new("A.CSPROJ")) && is_xml(Path::new("Info.plist")) && !is_xml(Path::new("a.html"))
        );
    }
}
