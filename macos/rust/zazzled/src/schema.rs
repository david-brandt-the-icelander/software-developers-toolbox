// Terse view of HCL/Terraform, Protobuf and GraphQL files, line by line: collapses comments and GraphQL descriptions of 3+ lines, HCL heredoc bodies, runs of 3+ Protobuf imports and file options, and runs of 10+ enum values or HCL list and map items to their first and last, and cuts long strings, keeping source line numbers.
use crate::data::{shorten_strings, shorten_words};
use crate::markup::Plan;
use std::path::Path;

const LONG_COMMENT: usize = 3;
const LONG_HEREDOC: usize = 3;
const LONG_STATEMENTS: usize = 3;
const LONG_RUN: usize = 10;

// The file kinds the view handles.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    Hcl,
    Proto,
    GraphQl,
}

// Returns the kind of a path by its extension: .tf, .tfvars, .hcl; .proto; .graphql, .graphqls, .gql.
pub fn kind_of(path: &Path) -> Option<Kind> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "tf" | "tfvars" | "hcl" => Some(Kind::Hcl),
        "proto" => Some(Kind::Proto),
        "graphql" | "graphqls" | "gql" => Some(Kind::GraphQl),
        _ => None,
    }
}

// Returns the view of a file, or None when nothing would be shortened: a header, then `N`, `N~` and `A-B` lines.
pub fn view(shown: &str, text: &str, kind: Kind) -> Option<String> {
    let src: Vec<&str> = text.lines().collect();
    let mut plan = Plan { lines: Vec::new(), steps: Vec::new() };
    let mut i = 0;
    while i < src.len() {
        let block = comment_end(&src, i, kind)
            .map(|e| (e, "comment", "comments"))
            .or_else(|| description_end(&src, i, kind).map(|e| (e, "description", "descriptions")));
        if let Some((end, what, step)) = block {
            if end + 1 - i >= LONG_COMMENT {
                plan.hide(i, end, format!("{what}, {} line(s)", end - i + 1), step);
            } else {
                (i..=end).for_each(|l| keep(&mut plan, src[l], l));
            }
            i = end + 1;
            continue;
        }
        if let Some((end, what, step)) = statements(&src, i, kind) {
            plan.hide(i, end, format!("{} {what}", end - i + 1), step);
            i = end + 1;
            continue;
        }
        keep(&mut plan, src[i], i);
        if let Some(end) = heredoc_end(&src, i).filter(|_| kind == Kind::Hcl) {
            if end - i > LONG_HEREDOC {
                plan.hide(i + 1, end, format!("heredoc, {} line(s)", end - i), "heredocs");
            } else {
                (i + 1..=end).for_each(|l| plan.keep(l, src[l], false));
            }
            i = end + 1;
            continue;
        }
        if let Some((items, what, step)) = run(&src, i, kind) {
            let (first, last) = (items[0], items[items.len() - 1]);
            (i + 1..=first).for_each(|l| keep(&mut plan, src[l], l));
            plan.hide(first + 1, last - 1, format!("{} {what}", items.len() - 2), step);
            keep(&mut plan, src[last], last);
            i = last + 1;
            continue;
        }
        i += 1;
    }
    plan.steps.retain(|(_, n)| *n > 0);
    if plan.steps.is_empty() {
        return None;
    }
    let steps: Vec<String> = plan.steps.iter().map(|(s, n)| format!("{n} {s}")).collect();
    Some(format!("{shown} {} lines; terse: {}\n{}", src.len(), steps.join(", "), plan.lines.join("\n")))
}

// Keeps a line, cutting long strings and blobs.
fn keep(plan: &mut Plan, line: &str, i: usize) {
    let (s, a) = shorten_strings(line);
    let (s, b) = shorten_words(&s);
    plan.add("long values", a + b);
    plan.keep(i, &s, a + b > 0);
}

// True for a line comment: `#` or `//` in HCL, `//` in Protobuf, `#` in GraphQL.
fn is_line_comment(line: &str, kind: Kind) -> bool {
    let t = line.trim_start();
    t.starts_with("//") && kind != Kind::GraphQl || t.starts_with('#') && kind != Kind::Proto
}

// Returns the last line of the comment that starts line i: a run of line comments, or a `/* */` block that ends a line (not in GraphQL).
fn comment_end(src: &[&str], i: usize, kind: Kind) -> Option<usize> {
    if is_line_comment(src[i], kind) {
        return Some(i + src[i..].iter().take_while(|l| is_line_comment(l, kind)).count() - 1);
    }
    if kind == Kind::GraphQl || !src[i].trim_start().starts_with("/*") {
        return None;
    }
    let end = (i..src.len()).find(|&j| src[j].contains("*/"))?;
    src[end].trim_end().ends_with("*/").then_some(end)
}

// Returns the last line of a GraphQL `"""` description that starts line i and closes on a later line.
fn description_end(src: &[&str], i: usize, kind: Kind) -> Option<usize> {
    let rest = src[i].trim_start().strip_prefix("\"\"\"").filter(|_| kind == Kind::GraphQl)?;
    if rest.contains("\"\"\"") {
        return None;
    }
    (i + 1..src.len()).find(|&j| src[j].contains("\"\"\""))
}

// Returns the last line of a run of 3+ Protobuf `import` lines or top-level `option` lines starting at line i, with the note's noun and the step.
fn statements(src: &[&str], i: usize, kind: Kind) -> Option<(usize, &'static str, &'static str)> {
    if kind != Kind::Proto {
        return None;
    }
    let is = |l: &str, word: &str| l.starts_with(word) && l.trim_end().ends_with(';');
    let (word, what, step) = if is(src[i], "import ") {
        ("import ", "import(s)", "imports")
    } else if is(src[i], "option ") {
        ("option ", "option(s)", "options")
    } else {
        return None;
    };
    let n = src[i..].iter().take_while(|l| is(l, word)).count();
    (n >= LONG_STATEMENTS).then(|| (i + n - 1, what, step))
}

// Returns the terminator line of an HCL heredoc opened at the end of line i (`<<EOT` or `<<-EOT`).
fn heredoc_end(src: &[&str], i: usize) -> Option<usize> {
    let tag = heredoc_tag(src[i])?;
    (i + 1..src.len()).find(|&j| src[j].trim() == tag)
}

// Returns the identifier of a heredoc that a line opens at its end.
fn heredoc_tag(line: &str) -> Option<&str> {
    let t = line.trim_end();
    let at = t.rfind("<<")?;
    let tag = t[at + 2..].trim_start_matches('-');
    let ident = tag.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && tag.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    ident.then_some(tag)
}

// Returns the item lines of a run of 10+ one-line items opened by line i, with the note's noun and the step: values of a Protobuf or GraphQL enum, or items of an HCL list or map.
fn run(src: &[&str], i: usize, kind: Kind) -> Option<(Vec<usize>, &'static str, &'static str)> {
    let t = src[i].trim();
    let (what, step) = match kind {
        Kind::Proto | Kind::GraphQl if t.starts_with("enum ") && t.ends_with('{') => {
            ("value(s)", "enum values")
        }
        Kind::Hcl if t.ends_with('[') || t.ends_with('{') && t.contains('=') => ("item(s)", "list items"),
        _ => return None,
    };
    let k = indent(src[i]);
    let child = src[i + 1..].iter().find(|l| !l.trim().is_empty()).map(|l| indent(l)).filter(|&c| c > k)?;
    let mut items = Vec::new();
    for (j, line) in src.iter().enumerate().skip(i + 1) {
        if line.trim().is_empty() {
            continue;
        }
        if indent(line) != child || !is_item(line) {
            break;
        }
        if !is_line_comment(line, kind) {
            items.push(j);
        }
    }
    (items.len() >= LONG_RUN).then_some((items, what, step))
}

// True for a line that is one whole item: its brackets balance outside strings, and it opens no heredoc or GraphQL description.
fn is_item(line: &str) -> bool {
    let t = line.trim();
    if t.starts_with("\"\"\"") || t.starts_with("/*") || heredoc_tag(t).is_some() {
        return false;
    }
    let (mut depth, mut quoted, mut escaped) = (0i32, false, false);
    for c in t.chars() {
        match c {
            _ if escaped => escaped = false,
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            '{' | '[' | '(' if !quoted => depth += 1,
            '}' | ']' | ')' if !quoted => depth -= 1,
            _ => {}
        }
        if depth < 0 {
            return false;
        }
    }
    depth == 0
}

// Returns a line's leading whitespace width.
fn indent(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    // Checks a Terraform file: comments, a heredoc, a long tag map and CIDR list, a short heredoc, a block that stays, and a long string.
    #[test]
    fn test_view_hcl() {
        let tags: String = (1..=12).map(|n| format!("    Key{n} = \"v{n}\"\n")).collect();
        let cidrs: String = (1..=11).map(|n| format!("    \"10.0.{n}.0/24\",\n")).collect();
        let long = "x".repeat(130);
        let src = format!("# Network\n# for prod\n# owner: ops\nresource \"aws_instance\" \"web\" {{\n  ami = \"ami-1\"\n  user_data = <<-EOT\n    #!/bin/bash\n    # comment in script\n    apt-get update\n    echo hi\n  EOT\n  tags = {{\n{tags}  }}\n  cidrs = [\n{cidrs}  ]\n  note = <<EOT\none\nEOT\n  description = \"{long}\"\n}}\n// one\n");
        let v = view("main.tf", &src, Kind::Hcl).expect("view");
        assert_eq!(
            v,
            format!("main.tf 44 lines; terse: 1 comments, 1 long values, 1 heredocs, 2 list items\n1-3\t⋯ comment, 3 line(s)\n4\tresource \"aws_instance\" \"web\" {{\n5\t  ami = \"ami-1\"\n6\t  user_data = <<-EOT\n7-11\t⋯ heredoc, 5 line(s)\n12\t  tags = {{\n13\t    Key1 = \"v1\"\n14-23\t⋯ 10 item(s)\n24\t    Key12 = \"v12\"\n25\t  }}\n26\t  cidrs = [\n27\t    \"10.0.1.0/24\",\n28-36\t⋯ 9 item(s)\n37\t    \"10.0.11.0/24\",\n38\t  ]\n39\t  note = <<EOT\n40\tone\n41\tEOT\n42~\t  description = \"{}…\"\n43\t}}\n44\t// one", "x".repeat(40))
        );
        assert!(view("a.tf", "resource \"a\" \"b\" {\n  x = 1\n}\n", Kind::Hcl).is_none());
        assert!(view("a.tf", "x = <<EOT\nnever closed\n", Kind::Hcl).is_none());
        assert_eq!(kind_of(Path::new("prod.TFVARS")), Some(Kind::Hcl));
        assert_eq!(kind_of(Path::new(".terraform.lock.hcl")), Some(Kind::Hcl));
        assert_eq!(kind_of(Path::new("a.txt")), None);
    }

    // Checks a Protobuf file: a block comment, imports, options, a long enum and a message whose fields stay.
    #[test]
    fn test_view_proto() {
        let values: String = (0..12).map(|n| format!("  COLOR_{n} = {n}; // c{n}\n")).collect();
        let src = format!("/*\n * Licence\n */\nsyntax = \"proto3\";\nimport \"a.proto\";\nimport \"b.proto\";\nimport \"c.proto\";\noption go_package = \"x\";\noption java_package = \"y\";\noption csharp_namespace = \"Z\";\nenum Color {{\n{values}}}\nmessage M {{\n  // Name.\n  string name = 1 [(validate.rules).string = {{min_len: 1}}];\n}}\n");
        let v = view("a.proto", &src, Kind::Proto).expect("view");
        assert_eq!(v, "a.proto 28 lines; terse: 1 comments, 1 imports, 1 options, 1 enum values\n1-3\t⋯ comment, 3 line(s)\n4\tsyntax = \"proto3\";\n5-7\t⋯ 3 import(s)\n8-10\t⋯ 3 option(s)\n11\tenum Color {\n12\t  COLOR_0 = 0; // c0\n13-22\t⋯ 10 value(s)\n23\t  COLOR_11 = 11; // c11\n24\t}\n25\tmessage M {\n26\t  // Name.\n27\t  string name = 1 [(validate.rules).string = {min_len: 1}];\n28\t}");
        assert!(view("a.proto", "import \"a.proto\";\nimport \"b.proto\";\n/* open\n", Kind::Proto).is_none());
    }

    // Checks a GraphQL schema: descriptions, comments, a long enum with comments inside, and what stays: one-line descriptions and short enums.
    #[test]
    fn test_view_graphql() {
        let values: String =
            (0..10).map(|n| format!("  V{n} @deprecated(reason: \"no (really)\")\n  # note\n")).collect();
        let src = format!("\"\"\"\nA user.\nWith details.\n\"\"\"\ntype User {{\n  \"\"\"Id.\"\"\"\n  id: ID!\n}}\n# a\n# b\n# c\nenum Big {{\n{values}}}\nenum Small {{\n  A\n  B\n}}\n\"\"\"\nShort.\n\"\"\"\n");
        let v = view("s.graphql", &src, Kind::GraphQl).expect("view");
        assert_eq!(v, "s.graphql 40 lines; terse: 2 descriptions, 1 comments, 1 enum values\n1-4\t⋯ description, 4 line(s)\n5\ttype User {\n6\t  \"\"\"Id.\"\"\"\n7\t  id: ID!\n8\t}\n9-11\t⋯ comment, 3 line(s)\n12\tenum Big {\n13\t  V0 @deprecated(reason: \"no (really)\")\n14-30\t⋯ 8 value(s)\n31\t  V9 @deprecated(reason: \"no (really)\")\n32\t  # note\n33\t}\n34\tenum Small {\n35\t  A\n36\t  B\n37\t}\n38-40\t⋯ description, 3 line(s)");
        assert!(view("s.gql", "\"\"\"\nopen\n", Kind::GraphQl).is_none());
        assert!(!is_item("  a)") && is_item("  \"a\\\"(\""));
    }

    // Checks the edges: a GraphQL `//` line is not a comment, heredoc tags, a blank line inside a run, a multi-line item ending a run, and what is not one item.
    #[test]
    fn test_schema_edges() {
        assert!(!is_line_comment("// x", Kind::GraphQl) && is_line_comment("# x", Kind::Hcl));
        assert_eq!(heredoc_tag("x = <<_END_1"), Some("_END_1"));
        assert_eq!(heredoc_tag("a << 1"), None);
        assert_eq!(heredoc_tag("x = <<EOT-x"), None);
        let items: String = (1..=10).map(|n| format!("  \"a{n}\",\n")).collect();
        let src = format!("x = [\n{items}\n  \"b\",\n]\n");
        assert!(view("a.tf", &src, Kind::Hcl).is_some_and(|v| v.contains("3-12\t⋯ 9 item(s)\n13\t  \"b\",")));
        let src = format!("x = [\n  {{\n{items}]\n");
        assert!(view("a.tf", &src, Kind::Hcl).is_none());
        let src = format!("y = {{\n{items}  z = {{\n  }}\n}}\n");
        assert!(view("a.tf", &src, Kind::Hcl).is_some_and(|v| v.contains("⋯ 8 item(s)")));
        assert!(!is_item("\"\"\"") && !is_item("/* a") && !is_item("x = <<EOT") && !is_item("a {"));
        assert!(is_item("a\\b") && is_item("\"[{(\"") && is_item("f(a[1], {b})"));
    }
}
