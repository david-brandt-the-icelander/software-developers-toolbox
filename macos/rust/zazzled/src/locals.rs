// Short names for long locals in the code terse view (Python, JavaScript, TypeScript, Dart, Rust, Go, Java, Kotlin, Swift, C, C++, Ruby, PHP, Scala): a local of 6+ characters is renamed at every use, only where the syntax tree shows all of them.
use crate::terse::Lang;
use std::collections::HashSet;
use tree_sitter::Node;

const MIN_LEN: usize = 6;
const KEYWORDS: &[&str] = &[
    "and", "as", "case", "def", "del", "do", "elif", "else", "enum", "for", "from", "get", "if", "in", "is",
    "late", "let", "new", "not", "null", "of", "on", "or", "pass", "set", "this", "true", "try", "type",
    "var", "void", "with", "None", "True", "abs", "all", "any", "bin", "chr", "dir", "hex", "id", "int",
    "iter", "len", "list", "map", "max", "min", "next", "num", "oct", "ord", "pow", "str", "sum", "box",
    "dyn", "fn", "impl", "loop", "mod", "move", "mut", "pub", "ref", "self", "use", "cap", "chan", "func",
    "go", "make", "nil", "byte", "char", "it", "fun", "val", "by", "out", "auto", "bool", "long", "goto",
    "std", "asm", "xor", "some", "Self", "end", "then", "when", "lazy", "match", "yield", "redo", "echo",
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
    let macro_kinds = ["preproc_def", "preproc_function_def"];
    let macros: HashSet<&str> = match lang {
        Lang::C | Lang::Cpp => preorder(root)
            .into_iter()
            .filter(|n| macro_kinds.contains(&n.kind()))
            .flat_map(|n| words(&src[n.byte_range()]))
            .collect(),
        _ => HashSet::new(),
    };
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
            let spans =
                if macros.contains(name.as_str()) { None } else { uses(scope, &nodes, &name, lang, src) };
            let (Some(want), Some(spans)) = (abbreviate(&name), spans) else {
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
        Lang::JavaScript | Lang::TypeScript | Lang::Tsx => matches!(
            kind,
            "function_declaration"
                | "function_expression"
                | "arrow_function"
                | "method_definition"
                | "generator_function_declaration"
                | "generator_function"
        ),
        Lang::Dart => matches!(kind, "function_body" | "function_expression_body"),
        Lang::Rust => matches!(kind, "function_item" | "closure_expression"),
        Lang::Go => matches!(kind, "function_declaration" | "method_declaration" | "func_literal"),
        Lang::Java => matches!(kind, "method_declaration" | "constructor_declaration" | "lambda_expression"),
        Lang::Kotlin => matches!(kind, "function_declaration" | "lambda_literal" | "anonymous_function"),
        Lang::Swift => matches!(kind, "function_declaration" | "init_declaration" | "lambda_literal"),
        Lang::C | Lang::Cpp => matches!(kind, "function_definition" | "lambda_expression"),
        Lang::Ruby => matches!(kind, "method" | "singleton_method"),
        Lang::Php => matches!(
            kind,
            "function_definition" | "method_declaration" | "anonymous_function" | "arrow_function"
        ),
        Lang::Scala => matches!(kind, "function_definition" | "lambda_expression"),
        Lang::Shell => false,
    }
}

// True for a node kind that is a parameter list.
fn is_params(kind: &str) -> bool {
    matches!(
        kind,
        "parameters"
            | "lambda_parameters"
            | "formal_parameters"
            | "formal_parameter_list"
            | "closure_parameters"
            | "parameter_list"
            | "inferred_parameters"
            | "function_value_parameters"
            | "lambda_function_type_parameters"
            | "lambda_parameter"
            | "parameter"
            | "method_parameters"
            | "block_parameters"
            | "bindings"
    )
}

// True when nothing in a function should be renamed: it reaches its locals by name at run time (eval, locals(), vars(), exec, with), its parse has errors, or a C function definition has no function declarator (a macro hid its shape).
fn poisoned(nodes: &[Node], lang: Lang, src: &str) -> bool {
    let dynamic: &[&str] = match lang {
        Lang::Python => &["locals", "vars", "eval", "exec"],
        Lang::JavaScript | Lang::TypeScript | Lang::Tsx => &["eval"],
        Lang::Ruby => &["eval", "binding", "local_variables", "local_variable_get", "local_variable_set"],
        Lang::Php => &["compact", "extract", "get_defined_vars", "eval"],
        _ => &[],
    };
    let not_a_function = matches!(lang, Lang::C | Lang::Cpp)
        && nodes.first().is_some_and(|f| {
            f.kind() == "function_definition"
                && f.child_by_field_name("declarator").is_none_or(|d| !has_kind(d, "function_declarator"))
        });
    not_a_function
        || nodes.iter().any(|n| {
            n.is_error()
                || n.is_missing()
                || n.kind() == "with_statement" && lang != Lang::Python
                || n.kind() == "dynamic_variable_name"
                || matches!(n.kind(), "identifier" | "name") && dynamic.contains(&&src[n.byte_range()])
        })
}

// True when a node or one of its descendants has the given kind.
fn has_kind(n: Node, kind: &str) -> bool {
    preorder(n).iter().any(|x| x.kind() == kind)
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
        (Lang::JavaScript | Lang::TypeScript | Lang::Tsx | Lang::Java, "variable_declarator") => {
            field("name")
        }
        (Lang::JavaScript | Lang::TypeScript | Lang::Tsx, "for_in_statement") if has_decl_keyword(n) => {
            field("left")
        }
        (Lang::Java, "enhanced_for_statement") => field("name"),
        (Lang::Rust, "let_declaration" | "for_expression") => field("pattern"),
        (Lang::Go, "short_var_declaration") => field("left"),
        (Lang::Go, "range_clause") if has_decl_keyword(n) => field("left"),
        (Lang::Go, "var_spec") => return kids(n).filter(|c| c.kind() == "identifier").collect(),
        (Lang::Kotlin, "variable_declaration")
            if n.parent().is_some_and(|p| {
                matches!(p.kind(), "property_declaration" | "for_statement" | "when_subject")
            }) =>
        {
            kids(n).find(|c| c.kind() == "identifier")
        }
        (Lang::Swift, "property_declaration") => field("name"),
        (Lang::Swift, "for_statement") => field("item"),
        (Lang::C | Lang::Cpp, "declaration") => {
            let mut cursor = n.walk();
            return n.children_by_field_name("declarator", &mut cursor).flat_map(targets).collect();
        }
        (Lang::Cpp, "for_range_loop") => field("declarator"),
        (Lang::Ruby, "assignment" | "operator_assignment") => field("left"),
        (Lang::Ruby, "for") => field("pattern"),
        (Lang::Php, "assignment_expression") => field("left"),
        (Lang::Php, "foreach_statement") => {
            let all: Vec<Node> = (0..n.child_count()).filter_map(|i| n.child(i)).collect();
            let after = all.iter().position(|c| c.kind() == "as").map_or(all.len(), |i| i + 1);
            return all[after..]
                .iter()
                .filter(|c| c.kind() != "compound_statement")
                .flat_map(|c| targets(*c))
                .collect();
        }
        (Lang::Scala, "val_definition" | "var_definition") => field("pattern"),
        (Lang::Scala, "enumerator") => n.named_child(0),
        (Lang::Dart, "initialized_variable_declaration" | "initialized_identifier" | "for_statement") => {
            field("name")
        }
        _ => None,
    };
    target.map(targets).unwrap_or_default()
}

// True when a for-in or for-of statement declares its variable with const, let or var, or a Go range clause with `:=`.
fn has_decl_keyword(n: Node) -> bool {
    (0..n.child_count())
        .filter_map(|i| n.child(i))
        .any(|c| matches!(c.kind(), "const" | "let" | "var" | ":="))
}

// Returns a node's named children.
fn kids<'t>(n: Node<'t>) -> impl Iterator<Item = Node<'t>> {
    (0..n.named_child_count()).filter_map(move |i| n.named_child(i))
}

// Returns the plain identifiers of an assignment target, looking inside tuple and list patterns.
fn targets(t: Node) -> Vec<Node> {
    match t.kind() {
        "identifier" | "simple_identifier" => vec![t],
        "variable_name" => kids(t).filter(|c| c.kind() == "name").collect(),
        "tuple_pattern"
        | "pattern_list"
        | "list_pattern"
        | "expression_list"
        | "pattern"
        | "left_assignment_list"
        | "pair" => kids(t).flat_map(targets).collect(),
        "init_declarator" | "pointer_declarator" | "reference_declarator" | "array_declarator" => {
            t.child_by_field_name("declarator").or_else(|| t.named_child(0)).map(targets).unwrap_or_default()
        }
        _ => Vec::new(),
    }
}

// Returns the spans to rename for a local, or None when a use could not be renamed safely.
fn uses(scope: Node, nodes: &[Node], name: &str, lang: Lang, src: &str) -> Option<Vec<(usize, usize)>> {
    let named = |n: &&Node| src[n.byte_range()] == *name;
    let in_format = |n: &Node| {
        let t = &src[n.byte_range()];
        n.kind() == "string_content"
            && (t.contains(&format!("{{{name}}}")) || t.contains(&format!("{{{name}:")))
    };
    let in_template = |n: &Node| {
        let t = &src[n.byte_range()];
        n.kind() == "string_literal"
            && t.match_indices(&format!("${name}"))
                .any(|(i, m)| !t[i + m.len()..].starts_with(|c: char| c.is_alphanumeric() || c == '_'))
    };
    if lang == Lang::Rust && nodes.iter().any(in_format)
        || lang == Lang::Kotlin && nodes.iter().any(in_template)
    {
        return None;
    }
    let mut spans = Vec::new();
    let mut decls = Vec::new();
    for n in nodes.iter().filter(named) {
        match n.kind() {
            "identifier" | "identifier_dollar_escaped" | "simple_identifier" => {}
            "name" if lang == Lang::Php && n.parent().is_some_and(|p| p.kind() == "variable_name") => {}
            "shorthand_property_identifier"
            | "shorthand_property_identifier_pattern"
            | "shorthand_field_identifier"
            | "type_identifier" => return None,
            _ => continue,
        }
        let parent = n.parent()?;
        if std::iter::successors(Some(parent), |x| x.parent())
            .take(4)
            .any(|a| declared_by(a, lang).iter().any(|d| d.id() == n.id()))
        {
            decls.push(*n);
        }
        match role(*n, parent, scope, lang, src) {
            Role::Use => spans.push((n.start_byte(), n.end_byte())),
            Role::Other => {}
            Role::Unsafe => return None,
        }
    }
    if lang == Lang::Ruby && decls.first().is_some_and(|d| spans.iter().any(|&(a, _)| a < d.start_byte())) {
        return None;
    }
    if !lang.function_scoped() {
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
        (Lang::JavaScript | Lang::TypeScript | Lang::Tsx, "arrow_function")
            if is_field(parent, "parameter") =>
        {
            return Role::Unsafe
        }
        (Lang::Java | Lang::Scala, "lambda_expression") if is_field(parent, "parameters") => {
            return Role::Unsafe
        }
        (Lang::Java, "field_access") if is_field(parent, "field") => return Role::Other,
        (Lang::Java, "method_invocation") if is_field(parent, "name") => return Role::Other,
        (Lang::Java | Lang::Rust, "scoped_identifier") if !in_use(parent) => return Role::Other,
        (Lang::Rust, "macro_invocation") if is_field(parent, "macro") => return Role::Unsafe,
        (Lang::Kotlin, "navigation_expression")
            if parent.named_child(0).is_some_and(|f| f.id() != n.id()) =>
        {
            return Role::Other
        }
        (Lang::Kotlin, "value_argument") if n.next_sibling().is_some_and(|s| s.kind() == "=") => {
            return Role::Other
        }
        (Lang::Kotlin, "user_type") => return Role::Other,
        (Lang::Swift, "navigation_suffix" | "value_argument_label") => return Role::Other,
        (Lang::C | Lang::Cpp, "qualified_identifier") => return Role::Other,
        (Lang::Ruby, "call") if is_field(parent, "method") => {
            return if parent.child_by_field_name("receiver").is_some() { Role::Other } else { Role::Unsafe }
        }
        (Lang::Php, "variable_name")
            if parent.parent().is_some_and(|g| {
                g.kind() == "scoped_property_access_expression"
                    && g.child_by_field_name("name").is_some_and(|f| f.id() == parent.id())
            }) =>
        {
            return Role::Other
        }
        (Lang::Scala, "field_expression") if is_field(parent, "field") => return Role::Other,
        (Lang::Scala, "stable_identifier") => return Role::Other,
        (Lang::Scala, "assignment_expression")
            if is_field(parent, "left") && parent.parent().is_some_and(|a| a.kind() == "arguments") =>
        {
            return Role::Other
        }
        (Lang::Cpp, "lambda_capture_initializer") if is_field(parent, "left") => return Role::Unsafe,
        (Lang::Rust, "token_tree") if n.prev_sibling().is_some_and(|p| matches!(p.kind(), "::" | ".")) => {
            return Role::Other
        }
        (Lang::Go, "literal_element")
            if parent.parent().is_some_and(|k| {
                k.kind() == "keyed_element"
                    && k.child_by_field_name("key").is_some_and(|c| c.id() == parent.id())
            }) =>
        {
            return Role::Unsafe
        }
        (_, "jsx_opening_element" | "jsx_closing_element" | "jsx_self_closing_element") => {
            return Role::Unsafe
        }
        (
            _,
            "function_declaration"
            | "function_definition"
            | "class_declaration"
            | "function_signature"
            | "function_item"
            | "shorthand_field_initializer"
            | "method_declaration"
            | "constructor_declaration"
            | "labeled_statement"
            | "break_statement"
            | "continue_statement"
            | "marker_annotation"
            | "annotation",
        ) => return Role::Unsafe,
        _ => {}
    }
    let mut p = Some(parent);
    while let Some(x) = p.filter(|x| x.id() != scope.id()) {
        let unsafe_kind = is_params(x.kind())
            || x.kind().starts_with("preproc")
            || matches!(
                x.kind(),
                "global_statement"
                    | "nonlocal_statement"
                    | "import_statement"
                    | "import_from_statement"
                    | "class_definition"
                    | "case_clause"
                    | "use_declaration"
                    | "class_body"
                    | "import_declaration"
                    | "global_declaration"
                    | "function_static_declaration"
            );
        let debug_format = x.kind() == "interpolation" && self_documenting(&src[x.byte_range()]);
        if unsafe_kind || debug_format {
            return Role::Unsafe;
        }
        p = x.parent();
    }
    Role::Use
}

// True when a node is part of a Rust `use` declaration.
fn in_use(n: Node) -> bool {
    std::iter::successors(Some(n), |x| x.parent()).any(|x| x.kind() == "use_declaration")
}

// True for a Python f-string field like `{name=}` that prints the expression's own text.
fn self_documenting(field: &str) -> bool {
    let inner = field.trim_start_matches('{').trim_end_matches('}');
    let expr = inner.split([':', '!']).next().unwrap_or(inner);
    expr.trim_end().ends_with('=') && !["==", "!=", "<=", ">="].iter().any(|op| expr.trim_end().ends_with(op))
}

// Returns the block a declaration outside Python is visible in: its enclosing block, loop, or Go if or switch.
fn block_of(decl: Node) -> Option<Node> {
    const BLOCKS: &[&str] = &[
        "statement_block",
        "block",
        "for_statement",
        "for_in_statement",
        "function_body",
        "function_expression_body",
        "for_expression",
        "enhanced_for_statement",
        "constructor_body",
        "switch_block",
        "compound_statement",
        "for_range_loop",
        "while_statement",
        "switch_statement",
        "when_expression",
        "statements",
        "if_statement",
        "expression_switch_statement",
        "type_switch_statement",
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
        let language = crate::terse::grammar(lang);
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
        assert_eq!(split_words("A_bc"), ["A", "bc"]);
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

    // Checks Rust locals, closures, paths and fields, and the places that keep a name: format strings, shorthand, shadowing, macro names, use.
    #[test]
    fn test_rust() {
        let src = "fn f(invoice_repo: &Repo) {\n    let invoice_rows = invoice_repo.rows();\n    let mut running_total = 0;\n    for invoice_row in &invoice_rows {\n        running_total += invoice_row.amount;\n    }\n    let total = |value_item: u8| value_item + 1;\n    std::mem::drop(invoice_rows);\n    s.running_total = running_total;\n}\n";
        assert_eq!(
            renamed(src, Lang::Rust),
            ["invoice_rows->ir@3", "running_total->rt@3", "invoice_row->ir2@2"]
        );
        let none = |src: &str| renamed(src, Lang::Rust).is_empty();
        assert!(none("fn f() {\n    let counter = 1;\n    println!(\"{counter}\");\n}\n"));
        assert!(none("fn f() {\n    let counter = 1;\n    format!(\"{counter:>3}\");\n}\n"));
        assert!(none("fn f() {\n    let counter = 1;\n    S { counter }\n}\n"));
        assert!(none("fn f() {\n    let counter = 1;\n    let S { counter, .. } = s;\n}\n"));
        assert!(none("fn f() {\n    let counter = 1;\n    let counter = counter + 1;\n}\n"));
        assert!(none("fn f() {\n    let counter = 1;\n    counter!(x);\n}\n"));
        assert!(none("fn f() {\n    let counter = 1;\n    use a::counter;\n}\n"));
        assert!(none("fn f() {\n    let counter = 1;\n    fn counter() {}\n}\n"));
        assert!(none("fn f() {\n    let counter = 1;\n    let g = |counter| counter;\n}\n"));
        assert_eq!(
            renamed("fn f() {\n    let counter = 1;\n    m!(counter, a::counter);\n}\n", Lang::Rust),
            ["counter->cou@2"]
        );
        assert_eq!(
            renamed("fn f() {\n    let counter = 1;\n    a::counter(counter);\n}\n", Lang::Rust),
            ["counter->cou@2"]
        );
    }

    // Checks Go locals from := and var and range, and the places that keep a name: struct keys, redeclared names, if-scoped names.
    #[test]
    fn test_go() {
        let src = "func f(repo Repo) {\n\tinvoiceRows, err := repo.Rows()\n\tvar runningTotal int\n\tfor _, invoiceRow := range invoiceRows {\n\t\trunningTotal += invoiceRow.Amount\n\t}\n\tg := func(valueItem int) int { return valueItem }\n\tuse(err, g, x.runningTotal)\n}\n";
        assert_eq!(renamed(src, Lang::Go), ["invoiceRows->ir@2", "runningTotal->rt@2", "invoiceRow->ir2@2"]);
        let none = |src: &str| renamed(src, Lang::Go).is_empty();
        assert!(none("func f() {\n\tcounter := 1\n\t_ = T{counter: counter}\n}\n"));
        assert!(none("func f() {\n\tcounter, a := 1, 2\n\tcounter, b := 3, 4\n}\n"));
        assert!(none("func f() {\n\tif counter := g(); counter > 0 {\n\t}\n\tuse(counter)\n}\n"));
        assert!(none("func f(counter int) {\n\tcounter := 1\n}\n"));
        assert!(none("func f() {\n\tfor counter = range xs {\n\t}\n}\n"));
        assert_eq!(
            renamed("func f() {\n\tif counter := g(); counter > 0 {\n\t}\n}\n", Lang::Go),
            ["counter->cou@2"]
        );
    }

    // Checks Java locals and enhanced-for, fields and method names, and the places that keep a name: lambdas, labels, local classes.
    #[test]
    fn test_java() {
        let src = "class A {\n  void f(Repo repo) {\n    List<Row> invoiceRows = repo.rows();\n    int runningTotal = 0;\n    for (Row invoiceRow : invoiceRows) { runningTotal += invoiceRow.amount; }\n    this.runningTotal = runningTotal;\n    invoiceRows.runningTotal();\n  }\n}\n";
        assert_eq!(
            renamed(src, Lang::Java),
            ["invoiceRows->ir@3", "runningTotal->rt@3", "invoiceRow->ir2@2"]
        );
        let none = |src: &str| renamed(src, Lang::Java).is_empty();
        assert!(none("class A {\n  void f() {\n    int counter = 1;\n    g(counter -> counter);\n  }\n}\n"));
        assert!(none(
            "class A {\n  void f() {\n    int counter = 1;\n    g((counter, b) -> counter);\n  }\n}\n"
        ));
        assert!(none("class A {\n  void f() {\n    int counter = 1;\n    counter: for (;;) { break counter; }\n  }\n}\n"));
        assert!(none(
            "class A {\n  void f() {\n    int counter = 1;\n    new Object() { int counter = 2; };\n  }\n}\n"
        ));
        assert!(none("class A {\n  void counter() {\n    int counter = 1;\n  }\n}\n"));
        assert!(none("class A {\n  void f(int counter) {\n    counter = 1;\n  }\n}\n"));
        assert_eq!(
            renamed(
                "class A {\n  A() {\n    int counter = 1;\n    a.b.counter(counter);\n  }\n}\n",
                Lang::Java
            ),
            ["counter->cou@2"]
        );
    }

    // Checks JavaScript locals follow the TypeScript rules.
    #[test]
    fn test_javascript() {
        let src = "function f() {\n  const invoiceRows = g();\n  return invoiceRows;\n}\n";
        assert_eq!(renamed(src, Lang::JavaScript), ["invoiceRows->ir@2"]);
        assert!(
            renamed("function f() {\n  const counter = 1;\n  eval('x');\n}\n", Lang::JavaScript).is_empty()
        );
        let loops = "function f(obj) {\n  for (const invoiceKey in obj) { use(invoiceKey); }\n  for (existing in obj) { use(existing); }\n}\n";
        assert_eq!(renamed(loops, Lang::JavaScript), ["invoiceKey->ik@2"]);
    }

    // Checks Kotlin locals and the places that keep a name: `$name` templates, named arguments, members, types, lambda parameters.
    #[test]
    fn test_kotlin() {
        let src = "fun f(repo: Repo) {\n    val invoiceRows = repo.rows(invoiceRows = 1)\n    var runningTotal = 0\n    for (invoiceRow in invoiceRows) { runningTotal += invoiceRow.amount }\n    x.runningTotal = runningTotal\n    println(\"${runningTotal}\")\n}\n";
        assert_eq!(
            renamed(src, Lang::Kotlin),
            ["invoiceRows->ir@2", "runningTotal->rt@4", "invoiceRow->ir2@2"]
        );
        let none = |src: &str| renamed(src, Lang::Kotlin).is_empty();
        assert!(none("fun f() {\n    val counter = 1\n    println(\"$counter!\")\n}\n"));
        assert!(none("fun f() {\n    val counter = 1\n    xs.map { counter -> counter }\n}\n"));
        assert!(none("fun f(counter: Int) {\n    val counter = 2\n}\n"));
        assert_eq!(renamed("fun f() {\n    val counter = 1\n    println(\"$counters\")\n    val y: counter = counter\n}\n", Lang::Kotlin), ["counter->cou@2"]);
        assert_eq!(
            renamed("fun f() {\n    when (val counter = g()) { else -> counter }\n}\n", Lang::Kotlin),
            ["counter->cou@2"]
        );
    }

    // Checks Swift locals, interpolation, labels and members, and parameters keep their names.
    #[test]
    fn test_swift() {
        let src = "func f(repo: Repo) {\n    let invoiceRows = repo.rows(invoiceRows: 1)\n    var runningTotal = 0\n    for invoiceRow in invoiceRows { runningTotal += invoiceRow.amount }\n    print(\"\\(runningTotal) \\(x.runningTotal)\")\n}\n";
        assert_eq!(
            renamed(src, Lang::Swift),
            ["invoiceRows->ir@2", "runningTotal->rt@3", "invoiceRow->ir2@2"]
        );
        let none = |src: &str| renamed(src, Lang::Swift).is_empty();
        assert!(none("func f(counter: Int) {\n    let counter = 2\n}\n"));
        assert!(none("func f() {\n    let counter = 1\n    xs.map { counter in counter }\n}\n"));
        assert!(none("func f() {\n    let counter = 1\n    let counter = 2\n}\n"));
    }

    // Checks C and C++ locals through pointers, references and arrays, and the places that keep a name: macros, init-captures, type names, misparsed macros.
    #[test]
    fn test_c_cpp() {
        let src = "int f(int n) {\n    int total_sum = 0, *item_ptr = 0, buffer_data[4];\n    for (int index_value = 0; index_value < n; index_value++) total_sum += buffer_data[index_value];\n    return total_sum + *item_ptr + s.total_sum;\n}\n";
        assert_eq!(
            renamed(src, Lang::C),
            ["total_sum->ts@3", "item_ptr->ip@2", "buffer_data->bd@2", "index_value->iv@4"]
        );
        let cpp = "int f() {\n    auto counter = 1;\n    for (const auto& line_item : xs) { counter += line_item; }\n    auto g = [&counter]() { return counter; };\n    return std::counter(counter);\n}\n";
        assert_eq!(renamed(cpp, Lang::Cpp), ["counter->cou@5", "line_item->li@2"]);
        let none_c = |src: &str| renamed(src, Lang::C).is_empty();
        let none_cpp = |src: &str| renamed(src, Lang::Cpp).is_empty();
        assert!(none_c(
            "#define USE() (counter + 1)\nint f() {\n    int counter = 1;\n    return USE();\n}\n"
        ));
        assert!(none_c("int f() {\n    int counter = 1;\n#ifdef counter\n    return 0;\n#endif\n    return counter;\n}\n"));
        assert!(none_cpp(
            "int f() {\n    int counter = 1;\n    auto g = [counter = 2]() { return counter; };\n}\n"
        ));
        assert!(none_cpp("void f() {\n    Fn handler_fn([](int) {});\n    Ref wr(nurse, handler_fn);\n}\n"));
        assert!(none_c("static WSD struct Stat {\n    int now_value[10];\n} stats = {0};\n"));
        assert!(none_c("int f() {\n    int counter = 1;\n    return counter +;\n}\n"));
        assert!(none_c("int f() {\n    int counter = 1;\n    @@ counter $$;\n    return counter;\n}\n"));
    }

    // Checks Ruby locals and the places that keep a name: block parameters, receiverless calls, uses before the first assignment, binding.
    #[test]
    fn test_ruby() {
        let src = "def f(repo)\n  invoice_rows = repo.rows(invoice_rows: 1)\n  running_total = 0\n  invoice_rows.each { |row| running_total += row.amount }\n  x.running_total = running_total\n  \"#{running_total}\"\nend\n";
        assert_eq!(renamed(src, Lang::Ruby), ["invoice_rows->ir@2", "running_total->rt@4"]);
        let none = |src: &str| renamed(src, Lang::Ruby).is_empty();
        assert!(none("def f\n  counter = 1\n  counter(2)\nend\n"));
        assert!(none("def f\n  puts counter\n  counter = 1\nend\n"));
        assert!(none("def f\n  counter = 1\n  binding.local_variable_get(:counter)\nend\n"));
        assert!(none("def f\n  counter = 1\n  xs.each { |counter| counter }\nend\n"));
        assert_eq!(
            renamed("def f\n  for counter in xs\n    p counter\n  end\nend\n", Lang::Ruby),
            ["counter->cou@2"]
        );
    }

    // Checks PHP locals, foreach, closures and interpolation, and the places that keep a name: properties, static properties, compact, globals.
    #[test]
    fn test_php() {
        let src = "<?php\nfunction f($repo) {\n    $invoiceRows = $repo->invoiceRows;\n    foreach ($invoiceRows as $key => $rowValue) { echo \"$rowValue {$key}\"; }\n    $g = function () use ($invoiceRows) { return $invoiceRows; };\n    return self::$invoiceRows;\n}\n";
        assert_eq!(renamed(src, Lang::Php), ["invoiceRows->ir@4", "rowValue->rv@2"]);
        let none = |src: &str| renamed(src, Lang::Php).is_empty();
        assert!(none("<?php\nfunction f() {\n    $counter = 1;\n    return compact('counter');\n}\n"));
        assert!(none("<?php\nfunction f() {\n    global $counter;\n    $counter = 1;\n}\n"));
        assert!(none(
            "<?php\nfunction f() {\n    $counter = 1;\n    $name = 'counter';\n    return $$name;\n}\n"
        ));
        assert!(none("<?php\nfunction f($counter) {\n    $counter = 2;\n}\n"));
    }

    // Checks Scala locals, for enumerators, named arguments and fields, and that case patterns keep their names.
    #[test]
    fn test_scala() {
        let src = "object A {\n  def f(repo: Repo) = {\n    val invoiceRows = repo.rows(invoiceRows = 1)\n    var runningTotal = 0\n    for (invoiceRow <- invoiceRows) runningTotal += invoiceRow.amount\n    println(s\"$runningTotal ${x.runningTotal}\")\n  }\n}\n";
        assert_eq!(
            renamed(src, Lang::Scala),
            ["invoiceRows->ir@2", "runningTotal->rt@3", "invoiceRow->ir2@2"]
        );
        let none = |src: &str| renamed(src, Lang::Scala).is_empty();
        assert!(none("object A {\n  def f() = {\n    val counter = 1\n    x match { case counter => counter }\n  }\n}\n"));
        assert!(none(
            "object A {\n  def f() = {\n    val counter = 1\n    xs.map(counter => counter)\n  }\n}\n"
        ));
    }

    // Checks a PHP function named like a variable and a Scala assignment outside an argument list.
    #[test]
    fn test_php_scala_edges() {
        let php = "<?php\nfunction f() {\n    $counter = 1;\n    counter($counter);\n}\n";
        assert_eq!(renamed(php, Lang::Php), ["counter->cou@2"]);
        let scala = "object A {\n  def f() = {\n    var counter = 1\n    counter = 2\n  }\n}\n";
        assert_eq!(renamed(scala, Lang::Scala), ["counter->cou@2"]);
    }
}
