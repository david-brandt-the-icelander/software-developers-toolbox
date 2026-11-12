// The outline operation: symbols with line ranges per source file, from tree-sitter grammars (line regexes for the rest).
use crate::fsio::{self, Ident};
use crate::par;
use crate::req::OutlineArgs;
use crate::state::{Dep, Res, State};
use crate::walk;
use regex::Regex;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use tree_sitter::{Language, Node, Parser};

const MAX_SYMBOLS: usize = 300;
const MAX_PARSE_BYTES: usize = 2 << 20;
const MAX_NAME: usize = 120;
const CHUNK: usize = 16;

// How to read symbols from one tree-sitter grammar: which node kinds are symbols, containers and pass-throughs.
struct Grammar {
    language: Language,
    items: &'static [(&'static str, &'static str)],
    containers: &'static [&'static str],
    passthrough: &'static [&'static str],
}

// One symbol with its 1-based line range.
struct Symbol {
    start: usize,
    end: usize,
    kind: String,
    name: String,
}

// Return the tree-sitter grammar for a file extension, if there is one.
fn grammar(ext: &str) -> Option<Grammar> {
    let js_items: &[(&str, &str)] = &[
        ("function_declaration", "fn"),
        ("generator_function_declaration", "fn"),
        ("class_declaration", "class"),
        ("method_definition", "method"),
        ("variable_declarator", "fn"),
    ];
    let js_pass: &[&str] =
        &["program", "export_statement", "lexical_declaration", "variable_declaration", "class_body"];
    let ts_items: &[(&str, &str)] = &[
        ("function_declaration", "fn"),
        ("function_signature", "fn"),
        ("generator_function_declaration", "fn"),
        ("class_declaration", "class"),
        ("abstract_class_declaration", "class"),
        ("method_definition", "method"),
        ("variable_declarator", "fn"),
        ("interface_declaration", "interface"),
        ("type_alias_declaration", "type"),
        ("enum_declaration", "enum"),
        ("internal_module", "namespace"),
        ("module", "namespace"),
    ];
    let ts_pass: &[&str] = &[
        "program",
        "export_statement",
        "lexical_declaration",
        "variable_declaration",
        "class_body",
        "statement_block",
        "ambient_declaration",
        "expression_statement",
    ];
    let c_items: &[(&str, &str)] = &[
        ("function_definition", "fn"),
        ("struct_specifier", "struct"),
        ("enum_specifier", "enum"),
        ("union_specifier", "union"),
        ("type_definition", "typedef"),
    ];
    let c_pass: &[&str] = &[
        "translation_unit",
        "declaration",
        "linkage_specification",
        "declaration_list",
        "preproc_ifdef",
        "preproc_if",
        "preproc_else",
        "preproc_elif",
    ];
    let cpp_items: &[(&str, &str)] = &[
        ("function_definition", "fn"),
        ("class_specifier", "class"),
        ("struct_specifier", "struct"),
        ("enum_specifier", "enum"),
        ("union_specifier", "union"),
        ("type_definition", "typedef"),
        ("namespace_definition", "namespace"),
    ];
    let cpp_pass: &[&str] = &[
        "translation_unit",
        "declaration",
        "linkage_specification",
        "declaration_list",
        "preproc_ifdef",
        "preproc_if",
        "preproc_else",
        "preproc_elif",
        "template_declaration",
        "field_declaration_list",
    ];
    let (language, items, containers, passthrough): (Language, &[_], &[_], &[_]) = match ext {
        "rs" => (
            tree_sitter_rust::LANGUAGE.into(),
            &[
                ("function_item", "fn"),
                ("function_signature_item", "fn"),
                ("struct_item", "struct"),
                ("enum_item", "enum"),
                ("trait_item", "trait"),
                ("mod_item", "mod"),
                ("type_item", "type"),
                ("const_item", "const"),
                ("static_item", "static"),
                ("union_item", "union"),
                ("impl_item", "impl"),
                ("macro_definition", "macro"),
            ],
            &["trait_item", "mod_item", "impl_item"],
            &["source_file", "declaration_list"],
        ),
        "py" => (
            tree_sitter_python::LANGUAGE.into(),
            &[("function_definition", "fn"), ("class_definition", "class")],
            &["class_definition"],
            &["module", "block", "decorated_definition"],
        ),
        "js" | "jsx" | "mjs" | "cjs" => {
            (tree_sitter_javascript::LANGUAGE.into(), js_items, &["class_declaration"], js_pass)
        }
        "ts" => (
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            ts_items,
            &["class_declaration", "abstract_class_declaration", "internal_module", "module"],
            ts_pass,
        ),
        "tsx" => (
            tree_sitter_typescript::LANGUAGE_TSX.into(),
            ts_items,
            &["class_declaration", "abstract_class_declaration", "internal_module", "module"],
            ts_pass,
        ),
        "go" => (
            tree_sitter_go::LANGUAGE.into(),
            &[
                ("function_declaration", "fn"),
                ("method_declaration", "method"),
                ("type_spec", "type"),
                ("const_spec", "const"),
                ("var_spec", "var"),
            ],
            &[],
            &["source_file", "type_declaration", "const_declaration", "var_declaration", "var_spec_list"],
        ),
        "java" => (
            tree_sitter_java::LANGUAGE.into(),
            &[
                ("class_declaration", "class"),
                ("interface_declaration", "interface"),
                ("enum_declaration", "enum"),
                ("record_declaration", "record"),
                ("method_declaration", "method"),
                ("constructor_declaration", "ctor"),
            ],
            &["class_declaration", "interface_declaration", "enum_declaration", "record_declaration"],
            &["program", "class_body", "interface_body", "enum_body", "enum_body_declarations"],
        ),
        "c" => (tree_sitter_c::LANGUAGE.into(), c_items, &[], c_pass),
        "h" | "cc" | "cpp" | "cxx" | "hh" | "hpp" | "hxx" => (
            tree_sitter_cpp::LANGUAGE.into(),
            cpp_items,
            &["class_specifier", "struct_specifier", "namespace_definition"],
            cpp_pass,
        ),
        _ => return None,
    };
    Some(Grammar { language, items, containers, passthrough })
}

// Return the line patterns for languages without a grammar; each has `kind` and `name` groups.
fn patterns(ext: &str) -> &'static [&'static str] {
    match ext {
        "kt" | "cs" | "swift" | "scala" => &[
            r"^\s*(?:(?:public|private|protected|internal|static|final|abstract|sealed|open|data|partial)\s+)*(?P<kind>class|interface|enum|record|object|struct|trait|fun|func)\s+(?P<name>\w+)",
        ],
        "sh" | "bash" => &[r"^(?:function\s+)?(?P<name>\w+)\s*\(\)\s*\{"],
        "md" => &[r"^(?P<kind>#{1,6})\s+(?P<name>.+?)\s*$"],
        _ => &[],
    }
}

// Return a node's source text collapsed to one short line.
fn text(node: Node, src: &[u8]) -> Option<String> {
    let raw = node.utf8_text(src).ok()?;
    let line: String = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    Some(line.chars().take(MAX_NAME).collect())
}

// Follow a C or C++ declarator chain down to the declared identifier.
fn declarator_name(mut node: Node, src: &[u8]) -> Option<String> {
    loop {
        if matches!(
            node.kind(),
            "identifier"
                | "field_identifier"
                | "type_identifier"
                | "qualified_identifier"
                | "destructor_name"
                | "operator_name"
        ) {
            return text(node, src);
        }
        node = node.child_by_field_name("declarator").or_else(|| node.named_child(0))?;
    }
}

// Return a symbol's display name and the name used to qualify what it contains.
fn name_of(node: Node, src: &[u8]) -> Option<(String, String)> {
    let kind = node.kind();
    if matches!(kind, "function_definition" | "type_definition") {
        if let Some(d) = node.child_by_field_name("declarator") {
            let name = declarator_name(d, src)?;
            return Some((name.clone(), name));
        }
    }
    let field = |f: &str| node.child_by_field_name(f).and_then(|n| text(n, src));
    let (display, scope) = match kind {
        "impl_item" => {
            let ty = field("type")?;
            (field("trait").map_or_else(|| ty.clone(), |t| format!("{t} for {ty}")), ty)
        }
        "variable_declarator" => {
            let value = node.child_by_field_name("value")?;
            if !matches!(value.kind(), "arrow_function" | "function_expression" | "function") {
                return None;
            }
            let name = field("name")?;
            (name.clone(), name)
        }
        "struct_specifier" | "enum_specifier" | "union_specifier" | "class_specifier" => {
            node.child_by_field_name("body")?;
            let name = field("name")?;
            (name.clone(), name)
        }
        _ => {
            let name = field("name")?;
            (name.clone(), name)
        }
    };
    Some((display, scope))
}

// Collect the symbols below a node, qualifying names inside containers with a dotted prefix.
fn walk_tree(node: Node, src: &[u8], g: &Grammar, prefix: &str, out: &mut Vec<Symbol>) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        let kind = child.kind();
        if let Some(&(_, label)) = g.items.iter().find(|(k, _)| *k == kind) {
            let Some((name, scope)) = name_of(child, src) else { continue };
            out.push(Symbol {
                start: child.start_position().row + 1,
                end: child.end_position().row + 1,
                kind: label.to_string(),
                name: format!("{prefix}{name}"),
            });
            if g.containers.contains(&kind) {
                walk_tree(child, src, g, &format!("{prefix}{scope}."), out);
            }
        } else if g.passthrough.contains(&kind) {
            walk_tree(child, src, g, prefix, out);
        }
    }
}

// Parse source with a grammar and return its symbols, or None when it cannot be parsed.
fn tree_symbols(source: &[u8], g: &Grammar) -> Option<Vec<Symbol>> {
    let mut parser = Parser::new();
    parser.set_language(&g.language).ok()?;
    let tree = parser.parse(source, None)?;
    let mut out = vec![];
    walk_tree(tree.root_node(), source, g, "", &mut out);
    Some(out)
}

// Extract symbols from text with line patterns; the end line is the start line.
fn regex_symbols(text: &str, ext: &str, res: &[Regex]) -> Vec<Symbol> {
    let mut out = vec![];
    for (i, line) in text.lines().enumerate() {
        let Some(caps) = res.iter().find_map(|re| re.captures(line)) else { continue };
        let kind = match caps.name("kind").map(|m| m.as_str()) {
            Some(k) if ext == "md" => format!("h{}", k.len()),
            Some(k) => k.to_string(),
            None => "fn".to_string(),
        };
        out.push(Symbol { start: i + 1, end: i + 1, kind, name: caps["name"].trim().to_string() });
    }
    out
}

// Return a file's symbols using its grammar, else line patterns, else None when unsupported.
fn file_symbols(path: &Path, bytes: &[u8]) -> Option<Vec<Symbol>> {
    let ext = path.extension().and_then(|e| e.to_str())?;
    if let Some(g) = grammar(ext) {
        return if bytes.len() > MAX_PARSE_BYTES { None } else { tree_symbols(bytes, &g) };
    }
    let res: Vec<Regex> = patterns(ext).iter().map(|p| Regex::new(p).expect("static pattern")).collect();
    (!res.is_empty()).then(|| regex_symbols(&String::from_utf8_lossy(bytes), ext, &res))
}

// Return true when outlines exist for this extension.
fn supported(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| grammar(e).is_some() || !patterns(e).is_empty())
}

// A path with its rendered symbols.
type Outlined = (String, Vec<Value>);

// Walk the roots and outline every supported file, parsing in parallel.
fn compute(a: &OutlineArgs) -> Res<(Value, Vec<Dep>)> {
    let walker = walk::builder(&a.paths, &[], false, false, None)?;
    let cap = a.max_symbols.unwrap_or(MAX_SYMBOLS);
    let (mut deps, mut files): (Vec<Dep>, Vec<PathBuf>) = (vec![], vec![]);
    for entry in walker.build().flatten() {
        let Ok(md) = entry.metadata() else { continue };
        deps.push((entry.path().to_path_buf(), Some(Ident::of(&md))));
        if md.is_dir() {
            deps.extend(walk::ignore_file_deps(entry.path()));
        }
        if md.is_file() && supported(entry.path()) {
            files.push(entry.path().to_path_buf());
        }
    }
    let parsed: Vec<Option<Outlined>> = par::map_chunks(&files, CHUNK, |chunk| {
        let refs: Vec<&Path> = chunk.iter().map(|p| p.as_path()).collect();
        chunk
            .iter()
            .zip(fsio::load_many(&refs))
            .map(|(path, loaded)| {
                let (_, bytes) = loaded.ok()?;
                if fsio::is_binary(&bytes) {
                    return None;
                }
                let symbols = file_symbols(path, &bytes)?;
                let rows = symbols.iter().map(|s| json!([s.start, s.end, s.kind, s.name])).collect();
                Some((walk::display(path), rows))
            })
            .collect()
    });
    let mut outlined: Vec<Outlined> = parsed.into_iter().flatten().filter(|o| !o.1.is_empty()).collect();
    outlined.sort_by(|x, y| x.0.cmp(&y.0));
    let total: usize = outlined.iter().map(|o| o.1.len()).sum();
    let mut left = cap;
    let mut out_files = vec![];
    for (path, mut rows) in outlined {
        rows.truncate(left);
        left -= rows.len();
        if !rows.is_empty() {
            out_files.push(json!({"path": path, "symbols": rows}));
        }
    }
    let mut value = json!({"files": out_files, "total_symbols": total});
    if total > cap {
        value["truncated"] = json!(true);
    }
    Ok((value, deps))
}

// Run an outline request through the session cache.
pub fn run(st: &mut State, a: OutlineArgs) -> Res<Value> {
    let mut keyed = a.clone();
    keyed.fresh = false;
    st.serve_cached(&format!("outline|{keyed:?}"), a.fresh, || compute(&a))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Outline source of the given extension and render each symbol as "start-end kind name".
    fn outline(ext: &str, src: &str) -> Vec<String> {
        let path = PathBuf::from(format!("x.{ext}"));
        let symbols = file_symbols(&path, src.as_bytes()).expect("supported");
        symbols.iter().map(|s| format!("{}-{} {} {}", s.start, s.end, s.kind, s.name)).collect()
    }

    // Rust items carry exact line ranges, impl names and dotted method names.
    #[test]
    fn test_outline_rust() {
        let src = "pub struct A;\nimpl<T> Foo for A<T> {\n    pub async fn go() {\n        let x = 1;\n    }\n}\nconst X: u8 = 1;\nmod m {\n    fn inner() {}\n}\nmacro_rules! mac { () => {} }\ntrait Tr { fn sig(&self); }\n";
        assert_eq!(
            outline("rs", src),
            [
                "1-1 struct A",
                "2-6 impl Foo for A<T>",
                "3-5 fn A<T>.go",
                "7-7 const X",
                "8-10 mod m",
                "9-9 fn m.inner",
                "11-11 macro mac",
                "12-12 trait Tr",
                "12-12 fn Tr.sig"
            ]
        );
    }

    // Python classes and methods (including decorated and async ones) are found with ranges.
    #[test]
    fn test_outline_python() {
        let src = "class A:\n    def f(self):\n        pass\n\n    @staticmethod\n    async def g():\n        pass\n\ndef top():\n    def nested():\n        pass\n";
        assert_eq!(outline("py", src), ["1-7 class A", "2-3 fn A.f", "6-7 fn A.g", "9-11 fn top"]);
    }

    // JavaScript and TypeScript functions, classes, arrow constants, interfaces and namespaces.
    #[test]
    fn test_outline_js_ts() {
        let js = "export function a() {}\nclass B {\n  m() {}\n}\nexport const c = async (x) => 1;\nconst plain = 5;\n";
        assert_eq!(outline("js", js), ["1-1 fn a", "2-4 class B", "3-3 method B.m", "5-5 fn c"]);
        let ts = "interface I { x: number }\ntype T = string;\nenum E { A }\nnamespace N {\n  export function f() {}\n}\nabstract class C { abstract g(): void }\n";
        assert_eq!(
            outline("ts", ts),
            ["1-1 interface I", "2-2 type T", "3-3 enum E", "4-6 namespace N", "5-5 fn N.f", "7-7 class C"]
        );
    }

    // Go functions, methods and type, const and var specs.
    #[test]
    fn test_outline_go() {
        let src = "package p\n\ntype S struct{}\n\nfunc (s S) M() {}\n\nfunc F() {}\n\nconst K = 1\n\nvar (\n\tV = 2\n)\n";
        assert_eq!(
            outline("go", src),
            ["3-3 type S", "5-5 method M", "7-7 fn F", "9-9 const K", "12-12 var V"]
        );
    }

    // Java types and members, C functions and typedefs, C++ classes and namespaces.
    #[test]
    fn test_outline_java_c_cpp() {
        let java = "class A {\n  A() {}\n  void m() {}\n  interface I {}\n}\n";
        assert_eq!(
            outline("java", java),
            ["1-5 class A", "2-2 ctor A.A", "3-3 method A.m", "4-4 interface A.I"]
        );
        let c = "struct P { int x; };\ntypedef unsigned long ul;\nstatic int *f(int a) {\n  return 0;\n}\nstruct P *use;\n";
        assert_eq!(outline("c", c), ["1-1 struct P", "2-2 typedef ul", "3-5 fn f"]);
        let cpp = "namespace n {\nclass K {\n public:\n  void m() {}\n};\n}\nvoid K::out() {}\ntemplate <class T> T id(T t) { return t; }\n";
        assert_eq!(
            outline("cpp", cpp),
            ["1-6 namespace n", "2-5 class n.K", "4-4 fn n.K.m", "7-7 fn K::out", "8-8 fn id"]
        );
    }

    // Languages without a grammar still get line-pattern outlines, and unknown ones are unsupported.
    #[test]
    fn test_outline_regex_fallback_and_unsupported() {
        assert_eq!(outline("md", "# Title\ntext\n## Sub\n"), ["1-1 h1 Title", "3-3 h2 Sub"]);
        assert_eq!(
            outline("kt", "data class P(val x: Int)\nfun main() {}\n"),
            ["1-1 class P", "2-2 fun main"]
        );
        assert!(file_symbols(Path::new("x.xyz"), b"abc").is_none());
        assert!(!supported(Path::new("a.xyz")) && supported(Path::new("a.rs")));
    }

    // Broken syntax still yields the parts that parse instead of failing.
    #[test]
    fn test_outline_tolerates_syntax_errors() {
        let got = outline("rs", "fn ok() {}\nfn broken( {\nstruct After;\n");
        assert!(got.iter().any(|s| s.ends_with("fn ok")));
    }
}
