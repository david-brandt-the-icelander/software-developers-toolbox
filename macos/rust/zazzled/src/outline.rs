// Symbol outlines: tree-sitter for common languages and line-based outliners for the rest.
use crate::meta::line_bytes;
use regex::Regex;
use std::path::Path;
use std::sync::OnceLock;
use tree_sitter::{Language, Node, Parser};

const MAX_LEVEL: usize = 200;
// Tree-sitter parses at roughly 5 MB/s, so larger files (usually generated) get the line-based outline instead.
pub(crate) const MAX_PARSE_BYTES: usize = 1 << 20;

// One symbol in a file: what it is, its name, its line range and how deeply it is nested.
#[derive(Debug, Clone, PartialEq)]
pub struct Symbol {
    pub kind: &'static str,
    pub name: String,
    pub start: usize,
    pub end: usize,
    pub depth: usize,
}

// The languages an outline can be built for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Lang {
    Rust,
    Python,
    C,
    CSharp,
    JavaScript,
    TypeScript,
    Tsx,
    Markdown,
    Toml,
    Json,
    Generic,
}

// A node kind that produces a symbol: its tree-sitter kind, its label and whether it contains further symbols.
type KindRow = (&'static str, &'static str, bool);

const RUST: &[KindRow] = &[
    ("function_item", "fn", false),
    ("struct_item", "struct", false),
    ("enum_item", "enum", false),
    ("union_item", "union", false),
    ("trait_item", "trait", true),
    ("impl_item", "impl", true),
    ("mod_item", "mod", true),
    ("const_item", "const", false),
    ("static_item", "static", false),
    ("type_item", "type", false),
    ("macro_definition", "macro", false),
];
const PYTHON: &[KindRow] = &[("function_definition", "def", false), ("class_definition", "class", true)];
const C: &[KindRow] = &[
    ("function_definition", "fn", false),
    ("struct_specifier", "struct", false),
    ("enum_specifier", "enum", false),
    ("union_specifier", "union", false),
    ("type_definition", "typedef", false),
    ("preproc_def", "#define", false),
    ("preproc_function_def", "#define", false),
];
const CSHARP: &[KindRow] = &[
    ("namespace_declaration", "namespace", true),
    ("file_scoped_namespace_declaration", "namespace", false),
    ("class_declaration", "class", true),
    ("struct_declaration", "struct", true),
    ("record_declaration", "record", true),
    ("interface_declaration", "interface", true),
    ("enum_declaration", "enum", false),
    ("delegate_declaration", "delegate", false),
    ("method_declaration", "method", false),
    ("constructor_declaration", "ctor", false),
    ("property_declaration", "property", false),
    ("event_declaration", "event", false),
];
const JS: &[KindRow] = &[
    ("function_declaration", "function", false),
    ("generator_function_declaration", "function", false),
    ("class_declaration", "class", true),
    ("method_definition", "method", false),
];
const TS: &[KindRow] = &[
    ("function_declaration", "function", false),
    ("generator_function_declaration", "function", false),
    ("class_declaration", "class", true),
    ("abstract_class_declaration", "class", true),
    ("method_definition", "method", false),
    ("interface_declaration", "interface", false),
    ("type_alias_declaration", "type", false),
    ("enum_declaration", "enum", false),
];
const FUNCTION_VALUES: [&str; 3] = ["arrow_function", "function_expression", "function"];
const DECLARATOR_WRAPPERS: [&str; 4] =
    ["pointer_declarator", "function_declarator", "parenthesized_declarator", "array_declarator"];

// Maps a file path to its outline language by extension.
pub fn lang_of(path: &Path) -> Lang {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "rs" => Lang::Rust,
        "py" | "pyi" => Lang::Python,
        "c" | "h" => Lang::C,
        "cs" => Lang::CSharp,
        "js" | "jsx" | "mjs" | "cjs" => Lang::JavaScript,
        "ts" | "mts" | "cts" => Lang::TypeScript,
        "tsx" => Lang::Tsx,
        "md" | "markdown" => Lang::Markdown,
        "toml" => Lang::Toml,
        "json" => Lang::Json,
        _ => Lang::Generic,
    }
}

// Returns true for extensions worth outlining when a whole directory is asked for.
pub fn worth_outlining(path: &Path) -> bool {
    const CODE: [&str; 14] =
        ["go", "swift", "java", "kt", "cpp", "cc", "hpp", "rb", "php", "sh", "lua", "cs", "scala", "gd"];
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    lang_of(path) != Lang::Generic || CODE.contains(&ext.as_str())
}

// Returns the tree-sitter grammar for a language and its kind-id table, which is built once per process; None when the language has no grammar or its module is unavailable.
fn grammar(lang: Lang) -> Option<(Language, &'static [Class])> {
    static TABLES: [OnceLock<Vec<Class>>; 7] = [const { OnceLock::new() }; 7];
    let (slot, name, kinds): (usize, &str, &[KindRow]) = match lang {
        Lang::Rust => (0, "rust", RUST),
        Lang::Python => (1, "python", PYTHON),
        Lang::C => (2, "c", C),
        Lang::JavaScript => (3, "javascript", JS),
        Lang::TypeScript => (4, "typescript", TS),
        Lang::Tsx => (5, "tsx", TS),
        Lang::CSharp => (6, "csharp", CSHARP),
        _ => return None,
    };
    let language = load_grammar(name)?;
    let table = TABLES[slot].get_or_init(|| class_table(&language, kinds));
    Some((language, table))
}

// Returns a grammar from its language module.
#[cfg(not(test))]
fn load_grammar(name: &str) -> Option<Language> {
    crate::modules::language(name)
}

// Returns a grammar from its crate, linked into tests only.
#[cfg(test)]
fn load_grammar(name: &str) -> Option<Language> {
    Some(match name {
        "rust" => tree_sitter_rust::LANGUAGE.into(),
        "python" => tree_sitter_python::LANGUAGE.into(),
        "c" => tree_sitter_c::LANGUAGE.into(),
        "javascript" => tree_sitter_javascript::LANGUAGE.into(),
        "typescript" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "tsx" => tree_sitter_typescript::LANGUAGE_TSX.into(),
        _ => tree_sitter_c_sharp::LANGUAGE.into(),
    })
}

// Returns a node's source text.
fn text(node: Node, data: &[u8]) -> String {
    node.utf8_text(data).unwrap_or("").split_whitespace().collect::<Vec<_>>().join(" ")
}

// Follows a C declarator chain down to the declared name.
fn declared_name(mut d: Node, data: &[u8]) -> Option<String> {
    while DECLARATOR_WRAPPERS.contains(&d.kind()) {
        d = d.child_by_field_name("declarator")?;
    }
    Some(text(d, data))
}

// Finds a symbol's name from its node.
fn name_of(node: Node, data: &[u8]) -> Option<String> {
    let field = |f: &str| node.child_by_field_name(f);
    match node.kind() {
        "impl_item" => {
            let ty = text(field("type")?, data);
            Some(field("trait").map_or(ty.clone(), |t| format!("{} for {ty}", text(t, data))))
        }
        "function_definition" if field("declarator").is_some() => declared_name(field("declarator")?, data),
        "type_definition" => declared_name(field("declarator")?, data),
        "struct_specifier" | "enum_specifier" | "union_specifier" => {
            field("body")?;
            Some(text(field("name")?, data))
        }
        _ => Some(text(field("name")?, data)),
    }
}

// What a node kind means for the outline, looked up by kind id instead of comparing kind names per node.
#[derive(Clone, Copy)]
enum Class {
    Plain,
    VarDecl,
    Symbol(&'static str, bool),
}

// Builds the kind-id table for a grammar, so classify does one array lookup per node.
fn class_table(language: &Language, kinds: &[KindRow]) -> Vec<Class> {
    (0..language.node_kind_count() as u16)
        .map(|id| match language.node_kind_for_id(id) {
            Some("variable_declarator") => Class::VarDecl,
            Some(k) => {
                kinds.iter().find(|(n, _, _)| *n == k).map_or(Class::Plain, |(_, l, c)| Class::Symbol(l, *c))
            }
            None => Class::Plain,
        })
        .collect()
}

// Decides whether a node is a symbol, returning its label, name and whether it contains symbols.
fn classify(node: Node, table: &[Class], data: &[u8]) -> Option<(&'static str, String, bool)> {
    match table.get(usize::from(node.kind_id())).copied().unwrap_or(Class::Plain) {
        Class::Plain => None,
        Class::VarDecl => {
            let is_fn =
                node.child_by_field_name("value").is_some_and(|v| FUNCTION_VALUES.contains(&v.kind()));
            is_fn.then(|| ("fn", text(node.child_by_field_name("name").unwrap_or(node), data), false))
        }
        Class::Symbol(label, container) => {
            let name = name_of(node, data).filter(|n| !n.is_empty())?;
            Some((label, name, container))
        }
    }
}

// Returns the last line a node covers, ignoring a trailing newline.
fn last_row(node: Node) -> usize {
    let end = node.end_position();
    (end.row + usize::from(end.column != 0)).max(node.start_position().row + 1)
}

// Walks the tree collecting symbols, descending into containers and plain nodes but not into leaf symbols.
fn collect(node: Node, kinds: &[Class], data: &[u8], depth: usize, level: usize, out: &mut Vec<Symbol>) {
    if level > MAX_LEVEL {
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match classify(child, kinds, data) {
            Some((kind, name, container)) => {
                out.push(Symbol {
                    kind,
                    name,
                    start: child.start_position().row + 1,
                    end: last_row(child),
                    depth,
                });
                if container {
                    collect(child, kinds, data, depth + 1, level + 1, out);
                }
            }
            None => collect(child, kinds, data, depth, level + 1, out),
        }
    }
}

// Builds an outline with tree-sitter; None if the language has no grammar or parsing fails.
fn parse_outline(lang: Lang, data: &[u8]) -> Option<Vec<Symbol>> {
    if data.len() > MAX_PARSE_BYTES {
        return None;
    }
    let (language, classes) = grammar(lang)?;
    crate::arena::scope(|| {
        let mut parser = Parser::new();
        parser.set_language(&language).ok()?;
        let tree = parser.parse(data, None)?;
        let mut out = Vec::new();
        collect(tree.root_node(), classes, data, 0, 0, &mut out);
        Some(out)
    })
}

// Returns the lines of a file as text, with undecodable lines blanked.
fn text_lines<'a>(data: &'a [u8], starts: &[u32]) -> Vec<&'a str> {
    (1..=starts.len()).map(|n| std::str::from_utf8(line_bytes(data, starts, n)).unwrap_or("")).collect()
}

// Sets each symbol's end from the next boundary and returns them; `ends[i]` is the last line of symbol i.
fn close_ranges(
    heads: Vec<(&'static str, String, usize, usize)>,
    total: usize,
    end_of: impl Fn(usize, &[(&'static str, String, usize, usize)]) -> usize,
) -> Vec<Symbol> {
    let ends: Vec<usize> = (0..heads.len()).map(|i| end_of(i, &heads).min(total)).collect();
    heads
        .into_iter()
        .zip(ends)
        .map(|((kind, name, start, depth), end)| Symbol { kind, name, start, end, depth })
        .collect()
}

// Outlines Markdown by its ATX headings, ignoring fenced code.
fn markdown_outline(lines: &[&str]) -> Vec<Symbol> {
    let (mut heads, mut fenced) = (Vec::new(), false);
    for (i, line) in lines.iter().enumerate() {
        if line.starts_with("```") || line.starts_with("~~~") {
            fenced = !fenced;
        }
        let level = line.bytes().take_while(|b| *b == b'#').count();
        if fenced || !(1..=6).contains(&level) || !line[level..].starts_with(' ') {
            continue;
        }
        let name = line[level..].trim().trim_end_matches('#').trim().to_string();
        const KINDS: [&str; 6] = ["h1", "h2", "h3", "h4", "h5", "h6"];
        heads.push((KINDS[level - 1], name, i + 1, level - 1));
    }
    close_ranges(heads, lines.len(), |i, h| {
        h[i + 1..].iter().find(|n| n.3 <= h[i].3).map_or(lines.len(), |n| n.2 - 1)
    })
}

// Outlines TOML by its table headers.
fn toml_outline(lines: &[&str]) -> Vec<Symbol> {
    let mut heads = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim();
        if t.starts_with('[') && t.contains(']') {
            let name = t.trim_start_matches('[').split(']').next().unwrap_or("").trim().to_string();
            heads.push(("table", name, i + 1, 0));
        }
    }
    close_ranges(heads, lines.len(), |i, h| h.get(i + 1).map_or(lines.len(), |n| n.2 - 1))
}

// Finds the byte offset and name of each top-level key in a JSON object.
fn json_keys(data: &[u8]) -> Vec<(usize, String)> {
    let (mut depth, mut i, mut keys) = (0usize, 0usize, Vec::new());
    while i < data.len() {
        match data[i] {
            b'{' | b'[' => depth += 1,
            b'}' | b']' => depth = depth.saturating_sub(1),
            b'"' => {
                let start = i + 1;
                i = start;
                while i < data.len() && data[i] != b'"' {
                    i += if data[i] == b'\\' { 2 } else { 1 };
                }
                let after = data[(i + 1).min(data.len())..].iter().find(|b| !b.is_ascii_whitespace());
                if depth == 1 && after == Some(&b':') {
                    keys.push((start, String::from_utf8_lossy(&data[start..i.min(data.len())]).into_owned()));
                }
            }
            _ => {}
        }
        i += 1;
    }
    keys
}

// Outlines JSON by its top-level keys.
fn json_outline(data: &[u8], starts: &[u32], total: usize) -> Vec<Symbol> {
    let line_of = |off: usize| starts.partition_point(|s| (*s as usize) <= off);
    let heads: Vec<_> =
        json_keys(data).into_iter().map(|(off, name)| ("key", name, line_of(off), 0)).collect();
    close_ranges(heads, total, |i, h| h.get(i + 1).map_or(total, |n| n.2 - 1))
}

// Returns the regex that recognises declarations in languages without a grammar.
fn declaration_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^(\s*)(?:(?:pub(?:\([^)]*\))?|export|async|static|public|private|protected|final|abstract|default|open|internal|override)\s+)*(fn|func|function|def|class|struct|enum|interface|trait|impl|type|module|mod|object|protocol|extension|namespace|record|union)\s+([A-Za-z_][\w:.<>]*)")
            .expect("declaration regex is valid")
    })
}

// Counts leading whitespace columns.
fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

// Finds where a declaration at the given line ends by indentation, taking a lone closing bracket as part of it.
fn indent_end(lines: &[&str], at: usize) -> usize {
    let base = indent_of(lines[at]);
    let mut end = at;
    for (j, line) in lines.iter().enumerate().skip(at + 1) {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        if indent_of(line) > base {
            end = j;
            continue;
        }
        if t.starts_with(['}', ')', ']']) || t == "end" {
            end = j;
        }
        break;
    }
    end + 1
}

// Outlines other languages with declaration keywords and indentation.
fn generic_outline(lines: &[&str]) -> Vec<Symbol> {
    let re = declaration_regex();
    let mut out: Vec<Symbol> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let Some(c) = re.captures(line) else { continue };
        let kind = match &c[2] {
            "fn" | "func" | "function" | "def" => "fn",
            "mod" | "module" => "mod",
            other => KIND_NAMES.iter().find(|k| **k == other).copied().unwrap_or("decl"),
        };
        let (start, end) = (i + 1, indent_end(lines, i));
        let depth = out.iter().filter(|s| s.end >= end).count();
        out.push(Symbol { kind, name: c[3].to_string(), start, end, depth });
    }
    out
}

const KIND_NAMES: [&str; 11] = [
    "class",
    "struct",
    "enum",
    "interface",
    "trait",
    "impl",
    "type",
    "object",
    "protocol",
    "extension",
    "namespace",
];

// Builds the outline of a file's content.
pub fn outline(path: &Path, data: &[u8], starts: &[u32]) -> Vec<Symbol> {
    let lang = lang_of(path);
    if let Some(syms) = parse_outline(lang, data) {
        return syms;
    }
    let lines = text_lines(data, starts);
    match lang {
        Lang::Markdown => markdown_outline(&lines),
        Lang::Toml => toml_outline(&lines),
        Lang::Json => json_outline(data, starts, lines.len()),
        _ => generic_outline(&lines),
    }
}

// Returns the lines where a C# file uses a name as an identifier, so not in comments or strings; None if it cannot be parsed.
pub fn csharp_identifier_lines(data: &[u8], name: &str) -> Option<Vec<usize>> {
    if data.len() > MAX_PARSE_BYTES {
        return None;
    }
    let (language, _) = grammar(Lang::CSharp)?;
    crate::arena::scope(|| {
        let mut parser = Parser::new();
        parser.set_language(&language).ok()?;
        let tree = parser.parse(data, None)?;
        let mut cursor = tree.walk();
        let mut lines = Vec::new();
        loop {
            let node = cursor.node();
            if node.kind() == "identifier" && data.get(node.byte_range()) == Some(name.as_bytes()) {
                lines.push(node.start_position().row + 1);
            }
            if cursor.goto_first_child() || cursor.goto_next_sibling() {
                continue;
            }
            while !cursor.goto_next_sibling() {
                if !cursor.goto_parent() {
                    lines.dedup();
                    return Some(lines);
                }
            }
        }
    })
}

// Returns the innermost symbol that contains a line.
pub fn enclosing(symbols: &[Symbol], line: usize) -> Option<&Symbol> {
    symbols.iter().filter(|s| s.start <= line && line <= s.end).max_by_key(|s| s.depth)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::meta::line_starts;

    // Builds an outline from source text for a file name.
    fn out(name: &str, src: &str) -> Vec<Symbol> {
        outline(Path::new(name), src.as_bytes(), &line_starts(src.as_bytes()))
    }

    // Checks C# identifier lines skip comments, strings and other names but keep interpolations and declarations.
    #[test]
    fn test_csharp_identifier_lines() {
        let src = "// Widget here\nclass Widget {\n  string s = \"Widget\";\n  void M(Widget w) { var t = $\"{Widget.X}\"; }\n  /* Widget */ int Widgets;\n}\n";
        assert_eq!(csharp_identifier_lines(src.as_bytes(), "Widget"), Some(vec![2, 4]));
        assert_eq!(csharp_identifier_lines(b"", "Widget"), Some(vec![]));
        assert_eq!(csharp_identifier_lines(&vec![b' '; MAX_PARSE_BYTES + 1], "Widget"), None);
    }

    // Summarises symbols as "kind name start-end/depth" strings.
    fn brief(syms: &[Symbol]) -> Vec<String> {
        syms.iter().map(|s| format!("{} {} {}-{}/{}", s.kind, s.name, s.start, s.end, s.depth)).collect()
    }

    // Checks C# namespaces, types and members, with attributes inside a method's range.
    #[test]
    fn test_csharp_outline() {
        let src = "namespace Acme.Billing;\n\npublic sealed class Invoice\n{\n    public Invoice(int id) { }\n    public int Id { get; }\n    [Obsolete]\n    public decimal Total(decimal r)\n    {\n        return r;\n    }\n    public delegate void D();\n    public event D Changed { add { } remove { } }\n}\nnamespace Old { public interface I { } public enum E { A } public record R(int X); public struct S { } }\n";
        assert_eq!(
            brief(&out("a.cs", src)),
            [
                "namespace Acme.Billing 1-1/0",
                "class Invoice 3-14/0",
                "ctor Invoice 5-5/1",
                "property Id 6-6/1",
                "method Total 7-11/1",
                "delegate D 12-12/1",
                "event Changed 13-13/1",
                "namespace Old 15-15/0",
                "interface I 15-15/1",
                "enum E 15-15/1",
                "record R 15-15/1",
                "struct S 15-15/1"
            ]
        );
    }

    // Checks Rust items, impl naming and nesting.
    #[test]
    fn test_rust_outline() {
        let src = "struct A {\n    x: u32,\n}\n\nimpl A {\n    fn new() -> A {\n        A { x: 1 }\n    }\n}\n\nimpl Display for A {\n    fn fmt(&self) {}\n}\n\nmod m {\n    pub fn f() {}\n}\n\nconst K: u32 = 1;\nstatic S: u32 = 2;\ntype T = u32;\nenum E { A }\nunion U { a: u32 }\ntrait Tr { fn g(&self); }\nmacro_rules! mm { () => {} }\n";
        assert_eq!(
            brief(&out("a.rs", src)),
            [
                "struct A 1-3/0",
                "impl A 5-9/0",
                "fn new 6-8/1",
                "impl Display for A 11-13/0",
                "fn fmt 12-12/1",
                "mod m 15-17/0",
                "fn f 16-16/1",
                "const K 19-19/0",
                "static S 20-20/0",
                "type T 21-21/0",
                "enum E 22-22/0",
                "union U 23-23/0",
                "trait Tr 24-24/0",
                "macro mm 25-25/0"
            ]
        );
    }

    // Checks Python functions, classes and decorated definitions.
    #[test]
    fn test_python_outline() {
        let src = "def top():\n    pass\n\n@deco\nclass K:\n    def m(self):\n        pass\n";
        assert_eq!(brief(&out("a.py", src)), ["def top 1-2/0", "class K 5-7/0", "def m 6-7/1"]);
    }

    // Checks C functions, structs, typedefs and macros.
    #[test]
    fn test_c_outline() {
        let src = "#define MAX 3\n#define SQ(x) ((x)*(x))\nstruct S {\n    int a;\n};\ntypedef int myint;\nenum Color { RED };\nstatic int *get(void) {\n    return 0;\n}\nint main(int argc, char **argv) {\n    return 0;\n}\nstruct S *fwd(struct S *s);\n";
        assert_eq!(
            brief(&out("a.c", src)),
            [
                "#define MAX 1-1/0",
                "#define SQ 2-2/0",
                "struct S 3-5/0",
                "typedef myint 6-6/0",
                "enum Color 7-7/0",
                "fn get 8-10/0",
                "fn main 11-13/0"
            ]
        );
    }

    // Checks JavaScript functions, classes, methods and arrow-function constants.
    #[test]
    fn test_javascript_outline() {
        let src = "function f() {}\nexport class C {\n  m() {}\n}\nconst g = () => 1;\nconst n = 5;\nlet h = function () {};\n";
        assert_eq!(
            brief(&out("a.js", src)),
            ["function f 1-1/0", "class C 2-4/0", "method m 3-3/1", "fn g 5-5/0", "fn h 7-7/0"]
        );
    }

    // Checks TypeScript interfaces, types, enums, abstract classes and TSX.
    #[test]
    fn test_typescript_outline() {
        let src = "interface I { a: number }\ntype T = string;\nenum E { A }\nabstract class B {}\nfunction* gen() {}\n";
        assert_eq!(
            brief(&out("a.ts", src)),
            ["interface I 1-1/0", "type T 2-2/0", "enum E 3-3/0", "class B 4-4/0", "function gen 5-5/0"]
        );
        assert_eq!(brief(&out("a.tsx", "function App() { return <div/>; }\n")), ["function App 1-1/0"]);
    }

    // Checks Markdown headings, nesting, fences and trailing hashes.
    #[test]
    fn test_markdown_outline() {
        let src = "# Title\n\ntext\n\n## Sub ##\n\n```\n# not a heading\n```\n\n## Next\n\n#hashtag\n";
        assert_eq!(brief(&out("a.md", src)), ["h1 Title 1-13/0", "h2 Sub 5-10/1", "h2 Next 11-13/1"]);
    }

    // Checks TOML tables and arrays of tables.
    #[test]
    fn test_toml_outline() {
        let src = "name = 1\n[package]\nx = 1\n\n[[bin]]\nname = \"a\"\n[bad\n";
        assert_eq!(brief(&out("Cargo.toml", src)), ["table package 2-4/0", "table bin 5-7/0"]);
    }

    // Checks JSON top-level keys, escapes and nested keys being ignored.
    #[test]
    fn test_json_outline() {
        let src = "{\n  \"a\": 1,\n  \"b\\\"q\": {\n    \"inner\": 2\n  },\n  \"c\": [1, 2]\n}\n";
        assert_eq!(brief(&out("a.json", src)), ["key a 2-2/0", "key b\\\"q 3-5/0", "key c 6-7/0"]);
        assert!(out("a.json", "[1, 2]").is_empty());
        assert!(out("a.json", "{\"unterminated").is_empty());
    }

    // Checks the generic outliner on a Go-like source.
    #[test]
    fn test_generic_outline() {
        let src = "package main\n\ntype Server struct {\n}\n\nfunc (s *Server) Run() {\n}\n\nfunc main() {\n\tx := 1\n}\n\nclass Foo\n  def bar\n    1\n  end\nend\n\nexport async function go() {\n  x()\n}\n\nobject Bar {\n}\n";
        let got = brief(&out("a.go", src));
        assert!(got.contains(&"fn main 9-11/0".to_string()), "{got:?}");
        assert!(got.contains(&"type Server 3-4/0".to_string()), "{got:?}");
        assert!(got.contains(&"class Foo 13-17/0".to_string()), "{got:?}");
        assert!(got.contains(&"fn bar 14-16/1".to_string()), "{got:?}");
        assert!(got.contains(&"fn go 19-21/0".to_string()), "{got:?}");
        assert!(got.contains(&"object Bar 23-24/0".to_string()), "{got:?}");
        assert_eq!(out("a.txt", "just prose\n").len(), 0);
        let odd = out("a.txt", "mod thing\nunion U\n");
        assert_eq!(odd[0].kind, "mod");
    }

    // Checks the generic outliner labels unknown keywords as declarations.
    #[test]
    fn test_generic_unknown_keyword_label() {
        let got = out("a.txt", "record Point\nmodule M\n");
        assert_eq!(got[0].kind, "decl");
        assert_eq!(got[1].kind, "mod");
    }

    // Checks language selection and the directory filter.
    #[test]
    fn test_lang_of_and_filter() {
        for (n, l) in [
            ("a.rs", Lang::Rust),
            ("a.PY", Lang::Python),
            ("a.h", Lang::C),
            ("a.mjs", Lang::JavaScript),
            ("a.mts", Lang::TypeScript),
            ("a.tsx", Lang::Tsx),
            ("a.markdown", Lang::Markdown),
            ("a.toml", Lang::Toml),
            ("a.json", Lang::Json),
            ("a.xyz", Lang::Generic),
            ("noext", Lang::Generic),
        ] {
            assert_eq!(lang_of(Path::new(n)), l, "{n}");
        }
        assert!(worth_outlining(Path::new("a.go")) && worth_outlining(Path::new("a.rs")));
        assert!(!worth_outlining(Path::new("a.txt")) && !worth_outlining(Path::new("LICENSE")));
    }

    // Checks the innermost enclosing symbol is chosen.
    #[test]
    fn test_enclosing() {
        let syms = out("a.rs", "impl A {\n    fn f() {\n        1;\n    }\n}\n");
        assert_eq!(enclosing(&syms, 3).map(|s| s.name.as_str()), Some("f"));
        assert_eq!(enclosing(&syms, 5).map(|s| s.name.as_str()), Some("A"));
        assert!(enclosing(&syms, 99).is_none());
    }

    // Checks syntax errors still yield the symbols that parsed and deep nesting is bounded.
    #[test]
    fn test_broken_and_deep_sources() {
        assert_eq!(
            brief(&out("a.rs", "fn ok() {}\nfn broken( {\n")).first().map(String::as_str),
            Some("fn ok 1-1/0")
        );
        let deep = format!("fn f() {{ {} }}\nfn after() {{}}\n", "(".repeat(100) + &")".repeat(100));
        assert_eq!(out("a.rs", &deep).len(), 2);
        let nested = format!("{}{}", "mod m { ".repeat(250), "}".repeat(250));
        assert!(out("a.rs", &nested).len() <= MAX_LEVEL + 1);
    }

    // Checks anonymous or unnamed constructs produce no symbols.
    #[test]
    fn test_unnamed_constructs_are_skipped() {
        assert!(out("a.c", "struct { int a; } anon;\nstruct S;\n").is_empty());
        assert!(out("a.js", "const x = { a: 1 };\n").is_empty());
    }

    // Checks tilde fences hide headings and blank lines stay inside a declaration.
    #[test]
    fn test_markdown_tilde_fence_and_blank_lines_in_blocks() {
        assert_eq!(brief(&out("a.md", "# A\n~~~\n# no\n~~~\n")), ["h1 A 1-4/0"]);
        let got = brief(&out("a.txt", "func a() {\n  x\n\n  y\n}\nfunc b() {\n}\n"));
        assert_eq!(got, ["fn a 1-5/0", "fn b 6-7/0"]);
    }

    // Checks lines before any symbol have no enclosing symbol.
    #[test]
    fn test_enclosing_before_first_symbol() {
        let syms = out("a.rs", "\n\nfn f() {}\n");
        assert!(enclosing(&syms, 1).is_none());
    }

    // Checks files over the parse limit get the line-based outline instead of tree-sitter.
    #[test]
    fn test_outline_skips_tree_sitter_for_huge_files() {
        let small = b"int f(void) {\n  return 0;\n}\n";
        let p = Path::new("gen.c");
        assert_eq!(outline(p, small, &line_starts(small))[0].kind, "fn");
        let big = small.repeat(MAX_PARSE_BYTES / small.len() + 1);
        let starts = line_starts(&big);
        assert_eq!(outline(p, &big, &starts), generic_outline(&text_lines(&big, &starts)));
    }

    // Checks the kind-id table marks symbol kinds and variable declarators, and leaves other kinds plain.
    #[test]
    fn test_class_table_by_kind_id() {
        let (language, table) = grammar(Lang::Rust).expect("rust grammar");
        let at = |kind: &str| table[usize::from(language.id_for_node_kind(kind, true))];
        assert!(matches!(at("function_item"), Class::Symbol("fn", false)));
        assert!(matches!(at("impl_item"), Class::Symbol("impl", true)));
        assert!(matches!(at("block"), Class::Plain));
        let (js, js_table) = grammar(Lang::JavaScript).expect("js grammar");
        assert!(matches!(
            js_table[usize::from(js.id_for_node_kind("variable_declarator", true))],
            Class::VarDecl
        ));
        assert!(grammar(Lang::Markdown).is_none());
    }
}
