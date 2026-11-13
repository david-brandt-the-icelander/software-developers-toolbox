// Short names for long locals in the Python, TypeScript and Dart terse view: a local of 6+ characters is renamed at every use, only where the syntax tree shows all of them.
use crate::terse::Lang;
use std::collections::HashSet;
use tree_sitter::Node;

const MIN_LEN: usize = 6;
const KEYWORDS: [&str; 56] = [
    "and", "as", "case", "def", "del", "do", "elif", "else", "enum", "for", "from", "get", "if", "in", "is",
    "late", "let", "new", "not", "null", "of", "on", "or", "pass", "set", "this", "true", "try", "type",
    "var", "void", "with", "None", "True", "abs", "all", "any", "bin", "chr", "dir", "hex", "id", "int",
    "iter", "len", "list", "map", "max", "min", "next", "num", "oct", "ord", "pow", "str", "sum",
];

// One local to rename: the byte spans of its declaration and uses, and its new name.
pub struct Rename {
    pub spans: Vec<(usize, usize)>,
    pub name: String,
}

// Returns the renames for every function in a file, outer functions first.
pub fn renames(root: Node, lang: Lang, src: &str) -> Vec<Rename> {
    let mut out = Vec::new();
    let mut done: Vec<(Node, String, String)> = Vec::new();
    for scope in preorder(root).into_iter().filter(|n| is_scope(n.kind(), lang)) {
        let nodes = preorder(scope);
        if poisoned(&nodes, lang, src) {
            continue;
        }
        let text = &src[scope.byte_range()];
        let mut taken: HashSet<String> = words(text).map(str::to_string).collect();
        for (outer, old, new) in &done {
            if contains(*outer, scope) && taken.contains(old) {
                taken.insert(new.clone());
            }
        }
        for name in declared(scope, &nodes, lang, src) {
            let (Some(want), Some(spans)) = (abbreviate(&name), uses(scope, &nodes, &name, lang, src)) else {
                continue;
            };
            let mut short = want.clone();
            for n in 2.. {
                if !taken.contains(&short) && !KEYWORDS.contains(&short.as_str()) {
                    break;
                }
                short = format!("{want}{n}");
            }
            taken.insert(short.clone());
            done.push((scope, name, short.clone()));
            out.push(Rename { spans, name: short });
        }
    }
    out
}

// Returns a node and all its descendants, parents before children.
fn preorder(root: Node) -> Vec<Node> {
    let mut out = Vec::new();
    let mut cursor = root.walk();
    'walk: loop {
        out.push(cursor.node());
        if cursor.goto_first_child() {
            continue;
        }
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                break 'walk;
            }
        }
    }
    out
}

// True when a later function lies inside an earlier one; functions are visited in source order, so ending first is enough.
fn contains(earlier: Node, later: Node) -> bool {
    later.end_byte() <= earlier.end_byte()
}

// Returns the identifier-like words of a text.
fn words(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_')).filter(|w| !w.is_empty())
}

// True for a node kind that opens a function scope whose locals are renamed together.
fn is_scope(kind: &str, lang: Lang) -> bool {
    match lang {
        Lang::Python => kind == "function_definition",
        Lang::TypeScript | Lang::Tsx => matches!(
            kind,
            "function_declaration"
                | "function_expression"
                | "arrow_function"
                | "method_definition"
                | "generator_function_declaration"
                | "generator_function"
        ),
        Lang::Dart => matches!(kind, "function_body" | "function_expression_body"),
    }
}

// True for a node kind that is a parameter list.
fn is_params(kind: &str) -> bool {
    matches!(kind, "parameters" | "lambda_parameters" | "formal_parameters" | "formal_parameter_list")
}

// True when a function reaches its locals by name at run time (eval, locals(), vars(), exec, with), so nothing in it is renamed.
fn poisoned(nodes: &[Node], lang: Lang, src: &str) -> bool {
    let dynamic: &[&str] = match lang {
        Lang::Python => &["locals", "vars", "eval", "exec"],
        Lang::TypeScript | Lang::Tsx => &["eval"],
        Lang::Dart => &[],
    };
    nodes.iter().any(|n| {
        n.kind() == "with_statement" && lang != Lang::Python
            || n.kind() == "identifier" && dynamic.contains(&&src[n.byte_range()])
    })
}

// Returns the nearest function scope around a node, not counting the node itself.
fn owner<'t>(n: Node<'t>, lang: Lang) -> Option<Node<'t>> {
    std::iter::successors(n.parent(), |x| x.parent()).find(|x| is_scope(x.kind(), lang))
}

// Returns the names a function declares as its own locals, in source order without repeats.
fn declared(scope: Node, nodes: &[Node], lang: Lang, src: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for n in nodes.iter().filter(|n| owner(**n, lang).is_some_and(|o| o.id() == scope.id())) {
        for t in declared_by(*n, lang) {
            let name = src[t.byte_range()].to_string();
            if !out.contains(&name) {
                out.push(name);
            }
        }
    }
    out
}

// Returns the identifiers a node declares as locals: assignment and loop targets, with/except aliases and variable declarators.
fn declared_by(n: Node, lang: Lang) -> Vec<Node> {
    let field = |f: &str| n.child_by_field_name(f);
    let target = match (lang, n.kind()) {
        (Lang::Python, "assignment" | "for_statement" | "for_in_clause") => field("left"),
        (Lang::Python, "as_pattern_target") => n.named_child(0),
        (Lang::Python, "named_expression") => field("name"),
        (Lang::TypeScript | Lang::Tsx, "variable_declarator") => field("name"),
        (Lang::TypeScript | Lang::Tsx, "for_in_statement") if has_decl_keyword(n) => field("left"),
        (Lang::Dart, "initialized_variable_declaration" | "initialized_identifier" | "for_statement") => {
            field("name")
        }
        _ => None,
    };
    target.map(targets).unwrap_or_default()
}

// True when a for-in or for-of statement declares its variable with const, let or var.
fn has_decl_keyword(n: Node) -> bool {
    (0..n.child_count()).filter_map(|i| n.child(i)).any(|c| matches!(c.kind(), "const" | "let" | "var"))
}

// Returns the plain identifiers of an assignment target, looking inside tuple and list patterns.
fn targets(t: Node) -> Vec<Node> {
    match t.kind() {
        "identifier" => vec![t],
        "tuple_pattern" | "pattern_list" | "list_pattern" => {
            (0..t.named_child_count()).filter_map(|i| t.named_child(i)).flat_map(targets).collect()
        }
        _ => Vec::new(),
    }
}

// Returns the spans to rename for a local, or None when a use could not be renamed safely.
fn uses(scope: Node, nodes: &[Node], name: &str, lang: Lang, src: &str) -> Option<Vec<(usize, usize)>> {
    let named = |n: &&Node| src[n.byte_range()] == *name;
    let mut spans = Vec::new();
    let mut decls = Vec::new();
    for n in nodes.iter().filter(named) {
        match n.kind() {
            "identifier" | "identifier_dollar_escaped" => {}
            "shorthand_property_identifier" | "shorthand_property_identifier_pattern" => return None,
            _ => continue,
        }
        let parent = n.parent()?;
        if declared_by(parent, lang).iter().any(|d| d.id() == n.id())
            || parent.parent().is_some_and(|g| declared_by(g, lang).iter().any(|d| d.id() == n.id()))
        {
            decls.push(*n);
        }
        match role(*n, parent, scope, lang, src) {
            Role::Use => spans.push((n.start_byte(), n.end_byte())),
            Role::Other => {}
            Role::Unsafe => return None,
        }
    }
    if lang != Lang::Python {
        let [decl] = decls[..] else { return None };
        let block = block_of(decl)?;
        if spans.iter().any(|&(a, _)| a < block.start_byte() || a >= block.end_byte()) {
            return None;
        }
    } else if decls.iter().any(|d| owner(*d, lang).is_some_and(|o| o.id() != scope.id())) {
        return None;
    }
    Some(spans)
}

// What an identifier with a local's name is at one place: a use of the local, another thing with that name, or a place that cannot be renamed.
enum Role {
    Use,
    Other,
    Unsafe,
}

// Classifies one identifier node inside a function.
fn role(n: Node, parent: Node, scope: Node, lang: Lang, src: &str) -> Role {
    let is_field = |p: Node, f: &str| p.child_by_field_name(f).is_some_and(|c| c.id() == n.id());
    match (lang, parent.kind()) {
        (Lang::Python, "attribute") if is_field(parent, "attribute") => return Role::Other,
        (Lang::Python, "keyword_argument") if is_field(parent, "name") => return Role::Other,
        (Lang::Dart, "member_expression") if is_field(parent, "property") => return Role::Other,
        (Lang::Dart, "label" | "cascade_selector") => return Role::Other,
        (Lang::TypeScript | Lang::Tsx, "arrow_function") if is_field(parent, "parameter") => {
            return Role::Unsafe
        }
        (_, "jsx_opening_element" | "jsx_closing_element" | "jsx_self_closing_element") => {
            return Role::Unsafe
        }
        (_, "function_declaration" | "function_definition" | "class_declaration" | "function_signature") => {
            return Role::Unsafe
        }
        _ => {}
    }
    let mut p = Some(parent);
    while let Some(x) = p.filter(|x| x.id() != scope.id()) {
        let unsafe_kind = is_params(x.kind())
            || matches!(
                x.kind(),
                "global_statement"
                    | "nonlocal_statement"
                    | "import_statement"
                    | "import_from_statement"
                    | "class_definition"
                    | "case_clause"
            );
        let debug_format = x.kind() == "interpolation" && self_documenting(&src[x.byte_range()]);
        if unsafe_kind || debug_format {
            return Role::Unsafe;
        }
        p = x.parent();
    }
    Role::Use
}

// True for a Python f-string field like `{name=}` that prints the expression's own text.
fn self_documenting(field: &str) -> bool {
    let inner = field.trim_start_matches('{').trim_end_matches('}');
    let expr = inner.split([':', '!']).next().unwrap_or(inner);
    expr.trim_end().ends_with('=') && !["==", "!=", "<=", ">="].iter().any(|op| expr.trim_end().ends_with(op))
}

// Returns the block a TypeScript or Dart declaration is visible in: its enclosing block or loop.
fn block_of(decl: Node) -> Option<Node> {
    const BLOCKS: [&str; 6] = [
        "statement_block",
        "block",
        "for_statement",
        "for_in_statement",
        "function_body",
        "function_expression_body",
    ];
    std::iter::successors(decl.parent(), |x| x.parent()).find(|x| BLOCKS.contains(&x.kind()))
}

// Returns the short form of a local's name of 6+ characters: its words' initials, or the first three letters of a one-word name; None unless it starts with a letter.
pub fn abbreviate(name: &str) -> Option<String> {
    if name.chars().count() < MIN_LEN {
        return None;
    }
    let parts = split_words(name);
    let short: String = if parts.len() > 1 {
        parts.iter().filter_map(|w| w.chars().next()).flat_map(char::to_lowercase).collect()
    } else {
        name.chars().take(3).flat_map(char::to_lowercase).collect()
    };
    short.starts_with(char::is_alphabetic).then_some(short)
}

// Splits a name into words like the regex `[A-Z]?[a-z0-9]+|[A-Z]+(?![a-z])`: camelCase, snake_case and acronyms.
fn split_words(name: &str) -> Vec<String> {
    let cs: Vec<char> = name.chars().collect();
    let lower = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit();
    let (mut out, mut i) = (Vec::new(), 0);
    while i < cs.len() {
        let start = i;
        if cs[i].is_ascii_uppercase() {
            while i < cs.len() && cs[i].is_ascii_uppercase() {
                i += 1;
            }
            let run = i - start;
            if run == 1 && i < cs.len() && lower(cs[i])
                || run > 1 && i < cs.len() && cs[i].is_ascii_lowercase()
            {
                if run > 1 {
                    out.push(cs[start..i - 1].iter().collect());
                }
                let word_start = i - 1;
                while i < cs.len() && lower(cs[i]) {
                    i += 1;
                }
                out.push(cs[word_start..i].iter().collect());
            } else {
                out.push(cs[start..i].iter().collect());
            }
        } else if lower(cs[i]) {
            while i < cs.len() && lower(cs[i]) {
                i += 1;
            }
            out.push(cs[start..i].iter().collect());
        } else {
            i += 1;
        }
    }
    out
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use tree_sitter::Parser;

    // Returns each rename as `old->new@count` for a snippet, in order.
    fn renamed(src: &str, lang: Lang) -> Vec<String> {
        let language: tree_sitter::Language = match lang {
            Lang::Python => tree_sitter_python::LANGUAGE.into(),
            Lang::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Lang::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Lang::Dart => tree_sitter_dart::LANGUAGE.into(),
        };
        crate::arena::scope(|| {
            let mut parser = Parser::new();
            parser.set_language(&language).expect("grammar");
            let tree = parser.parse(src, None).expect("parse");
            renames(tree.root_node(), lang, src)
                .iter()
                .map(|r| format!("{}->{}@{}", &src[r.spans[0].0..r.spans[0].1], r.name, r.spans.len()))
                .collect()
        })
    }

    // Checks names split into words and shorten like the C# view's rule.
    #[test]
    fn test_abbreviate() {
        assert_eq!(split_words("invoiceRepository"), ["invoice", "Repository"]);
        assert_eq!(split_words("HTTPClient_v2"), ["HTTP", "Client", "v2"]);
        assert_eq!(split_words("ABC1x"), ["ABC", "1x"]);
        assert_eq!(split_words("A1b"), ["A1b"]);
        assert_eq!(split_words("_my__name"), ["my", "name"]);
        assert_eq!(split_words("valueX"), ["value", "X"]);
        assert_eq!(split_words("valueXY"), ["value", "XY"]);
        assert_eq!(abbreviate("invoiceRepository").as_deref(), Some("ir"));
        assert_eq!(abbreviate("invoice_id").as_deref(), Some("ii"));
        assert_eq!(abbreviate("response").as_deref(), Some("res"));
        assert_eq!(abbreviate("MAX_ROWS").as_deref(), Some("mr"));
        assert_eq!(abbreviate("short"), None);
        assert_eq!(abbreviate("_private"), None);
        assert_eq!(abbreviate("1abcdef"), None);
    }

    // Checks Python locals are renamed at every use, members and keyword names keep theirs, and collisions get numbers.
    #[test]
    fn test_python() {
        let src = "def f(invoice_repository):\n    invoice_rows = invoice_repository.rows(invoice_rows=1)\n    for invoice_row in invoice_rows:\n        x.invoice_rows = invoice_row\n    (left_side, right_side) = 1, 2\n    with open(p) as file_handle: pass\n    if (match_count := 3): pass\n    print(f'{left_side} {right_side=}')\n    return [line_item for line_item in invoice_rows], file_handle, match_count, f'{left_side:>3}'\n";
        assert_eq!(
            renamed(src, Lang::Python),
            [
                "invoice_rows->ir@3",
                "invoice_row->ir2@2",
                "left_side->ls@3",
                "file_handle->fh@2",
                "match_count->mc@2",
                "line_item->li@2"
            ]
        );
    }

    // Checks Python names that cannot be renamed safely keep theirs.
    #[test]
    fn test_python_unsafe() {
        let none = |src: &str| renamed(src, Lang::Python).is_empty();
        assert!(none("def f(counter):\n    counter = counter + 1\n"));
        assert!(none("def f():\n    counter = 1\n    def g(counter): pass\n"));
        assert!(none("def f():\n    counter = 1\n    def g():\n        nonlocal counter\n"));
        assert!(none("def f():\n    global counter\n    counter = 1\n"));
        assert!(none("def f():\n    import counter\n    counter = 1\n"));
        assert!(none("def f():\n    counter = 1\n    class K:\n        counter = 2\n"));
        assert!(none("def f():\n    counter = 1\n    def counter(): pass\n"));
        assert!(none("def f():\n    counter = 1\n    match y:\n        case {'k': counter}: pass\n"));
        assert!(none("def f():\n    counter = 1\n    return locals()\n"));
        assert_eq!(renamed("def f():\n    inner_name = 1\n", Lang::Python), ["inner_name->in2@1"]);
        let kept = "def f():\n    counter = 1\n    g(counter=counter, x=counter.real)\n    y.counter = 2\n";
        assert_eq!(renamed(kept, Lang::Python), ["counter->cou@3"]);
        let inner = "def f():\n    counter = 1\n    def g():\n        counter = 2\n";
        assert_eq!(renamed(inner, Lang::Python), ["counter->cou@1"]);
        assert!(none("def f():\n    counter = 1\n    print(f'{counter = }')\n"));
        assert!(none("counter = 1\n"));
        assert_eq!(
            renamed("def f():\n    counter = 1\n    print(f'{counter == 1}')\n", Lang::Python),
            ["counter->cou@2"]
        );
    }

    // Checks TypeScript locals, block scoping and the places that keep a name.
    #[test]
    fn test_typescript() {
        let src = "function f(invoiceRepository: R) {\n  const invoiceRows = invoiceRepository.rows();\n  let runningTotal = 0;\n  for (const invoiceRow of invoiceRows) { runningTotal += invoiceRow.amount; }\n  const g = (counter) => counter;\n  return { k: runningTotal, invoiceRows: invoiceRows };\n}\n";
        assert_eq!(
            renamed(src, Lang::TypeScript),
            ["invoiceRows->ir@3", "runningTotal->rt@3", "invoiceRow->ir2@2"]
        );
        let none = |src: &str| renamed(src, Lang::Tsx).is_empty();
        assert!(none("function f() {\n  const counter = 1;\n  return { counter };\n}\n"));
        assert!(none("function f() {\n  const { counter } = o;\n}\n"));
        assert!(none("function f() {\n  { const counter = 1; }\n  return counter;\n}\n"));
        let inner = "function f() {\n  const counter = 1;\n  const g = () => { const counter = 2; };\n}\n";
        assert_eq!(renamed(inner, Lang::Tsx), ["counter->cou@1"]);
        assert!(none("function f() {\n  const counter = 1;\n  const g = counter => 1;\n}\n"));
        assert!(none("function f() {\n  const button = 1;\n  return <button>{button}</button>;\n}\n"));
        assert!(none("function f() {\n  const counter = 1;\n  function counter() {}\n}\n"));
        assert!(none("function f() {\n  const counter = 1;\n  eval('counter');\n}\n"));
        assert!(none("function f() {\n  for (counter of xs) {}\n}\n"));
        assert!(none("function f() {\n  g(counter);\n  { const counter = 1; }\n}\n"));
        assert!(none("function f() {\n  const counter = 1;\n  with (o) { counter; }\n}\n"));
        let arrow = "function f() {\n  const counter = 1;\n  const g = () => counter;\n}\n";
        assert_eq!(renamed(arrow, Lang::TypeScript), ["counter->cou@2"]);
        assert_eq!(
            renamed(
                "function f() {\n  for (let counter = 0; counter < 3; counter++) {}\n}\n",
                Lang::TypeScript
            ),
            ["counter->cou@3"]
        );
    }

    // Checks an inner function does not reuse a short name the outer function gave a name the inner one uses.
    #[test]
    fn test_nested_scopes() {
        let src = "function f() {\n  const invoiceRows = 1;\n  const g = () => {\n    const itemRecords = invoiceRows;\n    return itemRecords;\n  };\n}\n";
        assert_eq!(renamed(src, Lang::TypeScript), ["invoiceRows->ir@2", "itemRecords->ir2@2"]);
        let apart =
            "function f() {\n  const invoiceRows = 1;\n}\nfunction g() {\n  const itemRecords = 2;\n}\n";
        assert_eq!(renamed(apart, Lang::TypeScript), ["invoiceRows->ir@1", "itemRecords->ir@1"]);
    }

    // Checks Dart locals, interpolation, member and label names, and local functions.
    #[test]
    fn test_dart() {
        let src = "void f(Repo invoiceRepository) {\n  final invoiceRows = invoiceRepository.rows(invoiceRows: 1);\n  var a = 1, runningTotal = 2;\n  for (final invoiceRow in invoiceRows) { runningTotal += invoiceRow.amount; }\n  x..invoiceRows = runningTotal;\n  print('$runningTotal ${invoiceRows.length}');\n}\n";
        assert_eq!(
            renamed(src, Lang::Dart),
            ["invoiceRows->ir@3", "runningTotal->rt@4", "invoiceRow->ir2@2"]
        );
        let none = |src: &str| renamed(src, Lang::Dart).is_empty();
        assert!(none("void f() {\n  final counter = 1;\n  int counter2(int counter) => counter;\n}\n"));
        assert!(none("void f() {\n  final counter = 1;\n  void counter() {}\n}\n"));
        assert!(none("void f() {\n  if (a) { final counter = 1; }\n  print(counter);\n}\n"));
        let member = "void f() {\n  final counter = 1;\n  print(x.counter + counter.abs());\n}\n";
        assert_eq!(renamed(member, Lang::Dart), ["counter->cou@2"]);
    }
}
