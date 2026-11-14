// Terse view of Python, JavaScript, TypeScript, Dart, Rust, Go, Java, Kotlin, Swift, C, C++, Ruby, PHP, Scala, Perl, Lua, Elixir and shell files: hides imports, logging, region markers, constructors that only assign fields and catches that only log and rethrow (and Go's `if err != nil` returns), drops TypeScript, Java, Kotlin, Swift, Rust, PHP and Scala access modifiers and shortens long local names (locals.rs), keeping source line numbers.
use std::path::Path;
use tree_sitter::{Language, Node, Parser};

const LOG_METHODS: [&str; 19] = [
    "debug",
    "info",
    "information",
    "warn",
    "warning",
    "error",
    "exception",
    "critical",
    "fatal",
    "trace",
    "verbose",
    "log",
    "silly",
    "finest",
    "finer",
    "fine",
    "config",
    "severe",
    "shout",
];
const LONG_HEREDOC: usize = 6;
const CONSOLE_METHODS: [&str; 6] = ["log", "debug", "info", "warn", "error", "trace"];
const SIDE_EFFECTS: [&str; 7] =
    ["assignment", "await", "yield", "update_expression", "increment", "decrement", "named_expression"];

// The languages this view handles.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Lang {
    Python,
    JavaScript,
    TypeScript,
    Tsx,
    Dart,
    Rust,
    Go,
    Java,
    Kotlin,
    Swift,
    C,
    Cpp,
    Ruby,
    Php,
    Scala,
    Shell,
    Perl,
    Lua,
    Elixir,
}

impl Lang {
    // True for JavaScript and TypeScript, which share their grammar's node kinds.
    pub fn is_script(self) -> bool {
        matches!(self, Lang::JavaScript | Lang::TypeScript | Lang::Tsx)
    }

    // True for languages whose locals belong to the whole function rather than a block: Python, Ruby, PHP and Elixir.
    pub fn function_scoped(self) -> bool {
        matches!(self, Lang::Python | Lang::Ruby | Lang::Php | Lang::Elixir)
    }
}

// Returns the language of a path from its extension, if this view handles it.
pub fn lang_of(path: &Path) -> Option<Lang> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "py" | "pyi" => Some(Lang::Python),
        "js" | "jsx" | "mjs" | "cjs" => Some(Lang::JavaScript),
        "ts" | "mts" | "cts" => Some(Lang::TypeScript),
        "tsx" => Some(Lang::Tsx),
        "dart" => Some(Lang::Dart),
        "rs" => Some(Lang::Rust),
        "go" => Some(Lang::Go),
        "java" => Some(Lang::Java),
        "kt" | "kts" => Some(Lang::Kotlin),
        "rb" | "rake" => Some(Lang::Ruby),
        "php" => Some(Lang::Php),
        "scala" | "sc" => Some(Lang::Scala),
        "sh" | "bash" | "zsh" | "ksh" => Some(Lang::Shell),
        "pl" | "pm" => Some(Lang::Perl),
        "lua" => Some(Lang::Lua),
        "ex" | "exs" => Some(Lang::Elixir),
        "swift" => Some(Lang::Swift),
        "c" => Some(Lang::C),
        "cpp" | "cc" | "cxx" | "c++" | "h" | "hpp" | "hh" | "hxx" | "h++" | "ipp" => Some(Lang::Cpp),
        _ => None,
    }
}

// Returns the tree-sitter grammar for a language.
pub(crate) fn grammar(lang: Lang) -> Language {
    match lang {
        Lang::Python => tree_sitter_python::LANGUAGE.into(),
        Lang::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
        Lang::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        Lang::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
        Lang::Dart => tree_sitter_dart::LANGUAGE.into(),
        Lang::Rust => tree_sitter_rust::LANGUAGE.into(),
        Lang::Go => tree_sitter_go::LANGUAGE.into(),
        Lang::Java => tree_sitter_java::LANGUAGE.into(),
        Lang::Kotlin => tree_sitter_kotlin_ng::LANGUAGE.into(),
        Lang::Swift => tree_sitter_swift::LANGUAGE.into(),
        Lang::C => tree_sitter_c::LANGUAGE.into(),
        Lang::Cpp => tree_sitter_cpp::LANGUAGE.into(),
        Lang::Ruby => tree_sitter_ruby::LANGUAGE.into(),
        Lang::Php => tree_sitter_php::LANGUAGE_PHP.into(),
        Lang::Scala => tree_sitter_scala::LANGUAGE.into(),
        Lang::Shell => tree_sitter_bash::LANGUAGE.into(),
        Lang::Perl => ts_parser_perl::LANGUAGE.into(),
        Lang::Lua => tree_sitter_lua::LANGUAGE.into(),
        Lang::Elixir => tree_sitter_elixir::LANGUAGE.into(),
    }
}

// The view being built: hidden line ranges with optional notes, text changes, and how many places each step changed.
struct Plan<'a> {
    src: &'a str,
    starts: Vec<usize>,
    hidden: Vec<(usize, usize, Option<String>)>,
    changes: Vec<(usize, usize, String)>,
    steps: Vec<(&'static str, usize)>,
}

impl<'a> Plan<'a> {
    // Starts a plan for a source text.
    fn new(src: &'a str) -> Self {
        let starts = std::iter::once(0).chain(src.match_indices('\n').map(|(i, _)| i + 1)).collect();
        Plan { src, starts, hidden: Vec::new(), changes: Vec::new(), steps: Vec::new() }
    }

    // Returns the 0-based line holding a byte offset.
    fn line(&self, byte: usize) -> usize {
        self.starts.partition_point(|&s| s <= byte) - 1
    }

    // Returns the text of a 0-based line without its newline.
    fn text(&self, line: usize) -> &'a str {
        let end = self.starts.get(line + 1).map_or(self.src.len(), |&e| e - 1);
        self.src[self.starts[line]..end].trim_end_matches('\r')
    }

    // True when a node has only whitespace before it on its first line and nothing but `;` after it on its last.
    fn alone(&self, n: Node) -> bool {
        let (a, b) = (n.start_byte(), end_byte(n, self.src));
        let before = &self.src[self.starts[self.line(a)]..a];
        let last = self.line(b.saturating_sub(1));
        let after = self.text(last).get(b - self.starts[last]..).unwrap_or("");
        before.trim().is_empty() && after.trim().trim_start_matches(';').trim().is_empty()
    }

    // True when a line is already inside a hidden range.
    fn is_hidden(&self, line: usize) -> bool {
        self.hidden.iter().any(|&(a, b, _)| a <= line && line <= b)
    }

    // Hides lines a to b (0-based) with an optional note, unless they overlap a hidden range.
    fn hide(&mut self, a: usize, b: usize, note: Option<String>, step: &'static str) -> bool {
        if (a..=b).any(|l| self.is_hidden(l)) {
            return false;
        }
        self.hidden.push((a, b, note));
        self.count(step);
        true
    }

    // Hides the lines of a node that stands alone on them.
    fn hide_node(&mut self, n: Node, note: Option<String>, step: &'static str) -> bool {
        self.alone(n) && self.hide(n.start_position().row, end_row(n, self.src), note, step)
    }

    // Replaces a byte range with new text, unless its line is hidden.
    fn change(&mut self, a: usize, b: usize, new: String, step: &'static str) {
        if !self.is_hidden(self.line(a)) {
            self.changes.push((a, b, new));
            self.count(step);
        }
    }

    // Counts one change for a step.
    fn count(&mut self, step: &'static str) {
        match self.steps.iter_mut().find(|(s, _)| *s == step) {
            Some((_, n)) => *n += 1,
            None => self.steps.push((step, 1)),
        }
    }

    // Renders the view lines: notes for hidden ranges, changed lines as `N~`, and no second blank line in a row.
    fn render(mut self) -> Vec<String> {
        self.hidden.sort_by_key(|h| h.0);
        let mut out: Vec<String> = Vec::new();
        let mut line = 0;
        while line < self.starts.len() {
            if let Some((a, b, note)) = self.hidden.iter().find(|h| h.0 == line) {
                if let Some(note) = note {
                    out.push(format!("{}-{}\t⋯ {note}", a + 1, b + 1));
                }
                line = b + 1;
                continue;
            }
            let (start, text) = (self.starts[line], self.text(line));
            let mut mine: Vec<&(usize, usize, String)> =
                self.changes.iter().filter(|c| c.0 >= start && c.0 < start + text.len().max(1)).collect();
            mine.sort_by_key(|c| std::cmp::Reverse(c.0));
            let mut s = text.to_string();
            for (a, b, new) in &mine {
                s.replace_range(a - start..(b - start).min(s.len()), new);
            }
            let s = s.trim_end();
            let blank_again = s.is_empty() && out.last().is_some_and(|l| l.ends_with('\t'));
            if !blank_again && !(line + 1 == self.starts.len() && text.is_empty()) {
                let key = if mine.is_empty() { format!("{}", line + 1) } else { format!("{}~", line + 1) };
                out.push(format!("{key}\t{s}"));
            }
            line += 1;
        }
        out
    }
}

// Returns where a node's text ends, not counting the newline that C preprocessor lines include.
fn end_byte(n: Node, src: &str) -> usize {
    let b = n.end_byte();
    if b > n.start_byte() && src.as_bytes()[b - 1] == b'\n' {
        b - 1
    } else {
        b
    }
}

// Returns the last line a node's text is on, not counting the newline that C preprocessor lines include.
fn end_row(n: Node, src: &str) -> usize {
    let row = n.end_position().row;
    if end_byte(n, src) < n.end_byte() && n.end_position().column == 0 {
        row - 1
    } else {
        row
    }
}

// Returns the view of a file, or None when nothing would be shortened or it cannot be parsed: a header, then `N`, `N~` and `A-B` lines.
pub fn view(shown: &str, src: &str, lang: Lang) -> Option<String> {
    if src.len() > crate::outline::MAX_PARSE_BYTES {
        return None;
    }
    crate::arena::scope(|| {
        let mut parser = Parser::new();
        parser.set_language(&grammar(lang)).ok()?;
        let tree = parser.parse(src, None)?;
        plan_view(shown, src, lang, tree.root_node())
    })
}

// Applies every rule to a parsed file and renders the view, or None when nothing changed.
fn plan_view(shown: &str, src: &str, lang: Lang, root: Node) -> Option<String> {
    let mut plan = Plan::new(src);
    imports(root, lang, &mut plan);
    let mut nodes = Vec::new();
    let mut cursor = root.walk();
    'walk: loop {
        nodes.push(cursor.node());
        if cursor.goto_first_child() {
            continue;
        }
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                break 'walk;
            }
        }
    }
    for &n in &nodes {
        constructor(n, lang, &mut plan);
        catch(n, lang, &mut plan);
    }
    for &n in &nodes {
        let shell_block = |p: &str| {
            lang == Lang::Lua && p == "chunk"
                || lang == Lang::Elixir && matches!(p, "do_block" | "body")
                || lang == Lang::Shell
                    && matches!(
                        p,
                        "program" | "do_group" | "if_statement" | "elif_clause" | "else_clause" | "case_item"
                    )
        };
        let in_block = n.parent().is_some_and(|p| shell_block(p.kind()))
            || n.parent().is_some_and(|p| {
                matches!(
                    p.kind(),
                    "block"
                        | "statement_block"
                        | "constructor_body"
                        | "statements"
                        | "compound_statement"
                        | "body_statement"
                        | "begin"
                        | "then"
                        | "else"
                        | "block_body"
                        | "do"
                        | "ensure"
                )
            });
        if (is_logging(n, lang, src) || is_log_guard(n, lang, src)) && in_block {
            plan.hide_node(n, None, "log statements");
        } else if matches!(n.kind(), "comment" | "line_comment") && is_region(&src[n.byte_range()])
            || n.kind() == "preproc_call" && is_pragma_region(&src[n.byte_range()])
        {
            plan.hide_node(n, None, "regions");
        } else if n.kind() == "heredoc_body" && end_row(n, src) + 1 - n.start_position().row >= LONG_HEREDOC {
            let (a, b) = (n.start_position().row, end_row(n, src));
            plan.hide(a, b, Some(format!("heredoc, {} line(s)", b - a + 1)), "heredoc bodies");
        } else if is_access_modifier(n, lang) {
            let end = src[n.end_byte()..]
                .find(|c: char| !c.is_whitespace())
                .map_or(src.len(), |i| n.end_byte() + i);
            plan.change(n.start_byte(), end, String::new(), "access modifiers");
        }
    }
    for r in crate::locals::renames(root, lang, src) {
        let shown: Vec<(usize, usize)> =
            r.spans.into_iter().filter(|&(a, _)| !plan.is_hidden(plan.line(a))).collect();
        if !shown.is_empty() {
            plan.changes.extend(shown.into_iter().map(|(a, b)| (a, b, r.name.clone())));
            plan.count("locals shortened");
        }
    }
    if plan.steps.is_empty() {
        return None;
    }
    let total = plan.starts.len() - usize::from(src.ends_with('\n'));
    let steps: Vec<String> = plan.steps.iter().map(|(s, n)| format!("{n} {s}")).collect();
    Some(format!("{shown} {total} lines; terse: {}\n{}", steps.join(", "), plan.render().join("\n")))
}

// True for an access modifier on a declaration: TypeScript class members (not constructor parameters), Java's public, private and protected, and Rust's pub.
fn is_access_modifier(n: Node, lang: Lang) -> bool {
    let parent = n.parent().map(|p| p.kind()).unwrap_or_default();
    match (lang, n.kind()) {
        (Lang::TypeScript | Lang::Tsx, "accessibility_modifier") => matches!(
            parent,
            "public_field_definition"
                | "method_definition"
                | "method_signature"
                | "abstract_method_signature"
        ),
        (Lang::Java, "public" | "private" | "protected") => parent == "modifiers",
        (Lang::Rust, "visibility_modifier") => parent != "use_declaration",
        (Lang::Kotlin | Lang::Swift, "visibility_modifier") => parent == "modifiers",
        (Lang::Php, "visibility_modifier") => {
            matches!(parent, "method_declaration" | "property_declaration" | "const_declaration")
        }
        (Lang::Scala, "access_modifier") => parent == "modifiers",
        _ => false,
    }
}

// True for a comment that only marks a region: `# region`, `//#region`, `// #endregion`, `--#region`.
fn is_region(comment: &str) -> bool {
    let c = comment.trim_start_matches(['#', '/', '-']).trim_start().trim_start_matches('#').trim_start();
    let word = c.strip_prefix("end").unwrap_or(c);
    word.strip_prefix("region").is_some_and(|r| r.is_empty() || r.starts_with(char::is_whitespace))
}

// True for a C or C++ `#pragma region` or `#pragma endregion` line.
fn is_pragma_region(line: &str) -> bool {
    let rest = line.trim_start_matches('#').trim_start().strip_prefix("pragma").unwrap_or("").trim_start();
    is_region(&format!("//{rest}"))
}

// Collapses each run of top-level imports into one line.
fn imports(root: Node, lang: Lang, plan: &mut Plan) {
    let src = plan.src;
    let is_import = |n: &Node| match lang {
        Lang::Python => {
            matches!(n.kind(), "import_statement" | "import_from_statement" | "future_import_statement")
        }
        Lang::JavaScript | Lang::TypeScript | Lang::Tsx => n.kind() == "import_statement",
        Lang::Dart => matches!(n.kind(), "import_or_export" | "part_directive" | "part_of_directive"),
        Lang::Rust => matches!(n.kind(), "use_declaration" | "extern_crate_declaration"),
        Lang::Go | Lang::Java | Lang::Swift | Lang::Scala => n.kind() == "import_declaration",
        Lang::Kotlin => n.kind() == "import",
        Lang::C | Lang::Cpp => n.kind() == "preproc_include",
        Lang::Php => n.kind() == "namespace_use_declaration",
        Lang::Shell => {
            n.kind() == "command"
                && n.child_by_field_name("name")
                    .is_some_and(|c| matches!(&src[c.byte_range()], "source" | "."))
        }
        Lang::Perl => n.kind() == "use_statement",
        Lang::Lua => {
            let t = &src[n.byte_range()];
            n.kind() == "variable_declaration"
                && t.split_once('=').is_some_and(|(_, v)| v.trim_start().starts_with("require"))
        }
        Lang::Elixir => {
            n.kind() == "call"
                && kids(*n).all(|c| c.kind() != "do_block")
                && n.child_by_field_name("target")
                    .is_some_and(|t| matches!(&src[t.byte_range()], "alias" | "import" | "require" | "use"))
        }
        Lang::Ruby => {
            n.kind() == "call"
                && n.child_by_field_name("receiver").is_none()
                && n.child_by_field_name("method")
                    .is_some_and(|m| matches!(&src[m.byte_range()], "require" | "require_relative"))
        }
    };
    let mut containers = vec![root];
    if lang == Lang::Elixir {
        containers.extend(elixir_modules(root, src));
    }
    for container in containers {
        import_runs(container, &is_import, plan);
    }
}

// Returns the do-blocks of Elixir `defmodule` calls, where a module's alias, import, require and use lines sit.
fn elixir_modules<'t>(root: Node<'t>, src: &str) -> Vec<Node<'t>> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        let module = n.kind() == "call"
            && n.child_by_field_name("target").is_some_and(|t| &src[t.byte_range()] == "defmodule");
        if module {
            out.extend(kids(n).filter(|c| c.kind() == "do_block"));
        }
        stack.extend(kids(n));
    }
    out
}

// Collapses each run of import lines among a container's children into one line.
fn import_runs(container: Node, is_import: &dyn Fn(&Node) -> bool, plan: &mut Plan) {
    let kids: Vec<Node> = (0..container.child_count()).filter_map(|i| container.child(i)).collect();
    let mut i = 0;
    while i < kids.len() {
        let run = kids[i..].iter().take_while(|k| is_import(k)).count();
        if run > 0 {
            let (first, last) = (kids[i], kids[i + run - 1]);
            let (a, b) = (first.start_position().row, end_row(last, plan.src));
            if b > a && plan.alone(first) && plan.alone(last) {
                let count: usize = kids[i..i + run].iter().map(|k| import_count(*k)).sum();
                plan.hide(a, b, Some(format!("{count} import(s)")), "imports");
            }
        }
        i += run.max(1);
    }
}

// Returns how many imports a declaration makes: one, or each spec of a Go `import (..)` block.
fn import_count(n: Node) -> usize {
    let specs = kids(n).find(|c| c.kind() == "import_spec_list").map_or(0, |l| kids(l).count());
    specs.max(1)
}

// Returns a call's callee text, such as `self._log.debug` or `console.error`, for an expression statement that is only a call.
fn call_target<'t, 's>(stmt: Node<'t>, lang: Lang, src: &'s str) -> Option<(Node<'t>, &'s str)> {
    let call = match (lang, stmt.kind()) {
        (Lang::Kotlin | Lang::Swift | Lang::Scala, "call_expression")
        | (Lang::Ruby | Lang::Elixir, "call")
        | (Lang::Shell, "command")
        | (Lang::Lua, "function_call") => stmt,
        (_, "expression_statement") => stmt.named_child(0)?,
        _ => return None,
    };
    let callee = match (lang, call.kind()) {
        (Lang::Python, "call")
        | (
            Lang::JavaScript | Lang::TypeScript | Lang::Tsx | Lang::Go | Lang::C | Lang::Cpp,
            "call_expression",
        ) => call.child_by_field_name("function")?.byte_range(),
        (Lang::Dart | Lang::Kotlin | Lang::Swift, "call_expression") => call.named_child(0)?.byte_range(),
        (Lang::Scala, "call_expression") | (Lang::Php, "function_call_expression") => {
            call.child_by_field_name("function")?.byte_range()
        }
        (Lang::Ruby, "call") => {
            let method = call.child_by_field_name("method")?;
            call.child_by_field_name("receiver").map_or(method.start_byte(), |r| r.start_byte())
                ..method.end_byte()
        }
        (Lang::Php, "member_call_expression" | "scoped_call_expression") => {
            let name = call.child_by_field_name("name")?;
            let owner = call.child_by_field_name("object").or_else(|| call.child_by_field_name("scope"))?;
            owner.start_byte()..name.end_byte()
        }
        (Lang::C | Lang::Cpp, "binary_expression") => stream_log(call, src)?.byte_range(),
        (Lang::Rust, "macro_invocation") => call.child_by_field_name("macro")?.byte_range(),
        (Lang::Shell | Lang::Lua, "command" | "function_call") => {
            call.child_by_field_name("name")?.byte_range()
        }
        (Lang::Elixir, "call") => call.child_by_field_name("target")?.byte_range(),
        (Lang::Perl, "method_call_expression") => {
            let method = call.child_by_field_name("method")?;
            call.child_by_field_name("invocant")?.start_byte()..method.end_byte()
        }
        (Lang::Perl, "ambiguous_function_call_expression" | "function_call_expression") => {
            call.child_by_field_name("function")?.byte_range()
        }
        (Lang::Java, "method_invocation") => {
            let name = call.child_by_field_name("name")?;
            call.child_by_field_name("object").map_or(name.start_byte(), |o| o.start_byte())..name.end_byte()
        }
        _ => return None,
    };
    Some((call, &src[callee]))
}

// True for a statement that only logs, with no side effects in its arguments.
fn is_logging(stmt: Node, lang: Lang, src: &str) -> bool {
    let Some((call, callee)) = call_target(stmt, lang, src) else { return false };
    if lang == Lang::Rust {
        return is_log_macro(callee) && !macro_side_effects(call, src);
    }
    if lang == Lang::Shell {
        let runs = ["command_substitution", "process_substitution", "arithmetic_expansion"];
        let shell_log = matches!(callee, "log" | "logger" | "debug")
            || callee.starts_with("log_")
            || callee.starts_with("debug_");
        return shell_log && !preorder_kinds(call).iter().any(|k| runs.contains(k));
    }
    if has_side_effects(call) || lang == Lang::Go && src[call.byte_range()].contains("<-") {
        return false;
    }
    if call.kind() == "binary_expression" {
        return true;
    }
    let colon = if lang == Lang::Lua { "." } else { ":" };
    let normalized: String =
        callee.replace("->", ".").replace("::", ".").replace(':', colon).split_whitespace().collect();
    let Some((receiver, method)) = normalized.rsplit_once('.') else {
        return match lang {
            Lang::Dart => matches!(callee, "debugPrint" | "log"),
            Lang::Swift => matches!(callee, "NSLog" | "os_log" | "debugPrint"),
            Lang::C | Lang::Cpp => is_c_log_macro(callee),
            Lang::Php => callee == "error_log",
            Lang::Perl => matches!(callee, "DEBUG" | "INFO" | "WARN" | "ERROR" | "TRACE"),
            _ => false,
        };
    };
    let last = receiver
        .rsplit('.')
        .next()
        .unwrap_or(receiver)
        .trim_end_matches(['?', '!'])
        .trim_matches(['{', '}', '$']);
    if last == "console" {
        return lang.is_script() && CONSOLE_METHODS.contains(&method);
    }
    if matches!(last, "Log" | "Timber") && matches!(method, "d" | "i" | "w" | "e" | "v" | "wtf") {
        return true;
    }
    let r = last.trim_matches('_').to_ascii_lowercase();
    let logger =
        matches!(r.as_str(), "log" | "logs" | "logging" | "slog" | "spdlog") || r.ends_with("logger");
    if lang == Lang::Go {
        let m = method.to_ascii_lowercase();
        let base = m.trim_end_matches("context").trim_end_matches("ln").trim_end_matches(['f', 'w']);
        let exits = base.starts_with("fatal") || base.starts_with("panic");
        return logger && !exits && (LOG_METHODS.contains(&base) || base == "print" || base == "logattrs");
    }
    logger && (LOG_METHODS.contains(&method) || method.starts_with("log"))
}

// Returns the kinds of a node and all its descendants.
fn preorder_kinds(n: Node) -> Vec<&'static str> {
    let mut out = Vec::new();
    let mut stack = vec![n];
    while let Some(x) = stack.pop() {
        out.push(x.kind());
        stack.extend(kids(x));
    }
    out
}

// True for a C or C++ logging macro by its conventional name: `LOG_INFO`, `DLOG_WARN`, `ALOGD`, `LOGE`, ...
fn is_c_log_macro(name: &str) -> bool {
    let upper = name.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
    upper && ["LOG_", "DLOG", "ALOG", "VLOG"].iter().any(|p| name.starts_with(p))
        || matches!(name, "LOGD" | "LOGI" | "LOGW" | "LOGE" | "LOGV")
}

// Returns the call that starts a C++ stream log statement, `LOG(INFO) << x` or `qDebug() << x`, if the statement is one.
fn stream_log<'t>(expr: Node<'t>, src: &str) -> Option<Node<'t>> {
    let mut left = expr;
    while left.kind() == "binary_expression" {
        let op = left.child_by_field_name("operator")?;
        if &src[op.byte_range()] != "<<" {
            return None;
        }
        left = left.child_by_field_name("left")?;
    }
    let name = left.child_by_field_name("function").filter(|_| left.kind() == "call_expression")?;
    let streams = ["LOG", "DLOG", "VLOG", "LOG_IF", "qDebug", "qInfo", "qWarning", "qCritical"];
    streams.contains(&&src[name.byte_range()]).then_some(name)
}

// True for a Rust logging macro: `debug!`, `info!`, `warn!`, `error!`, `trace!` or `event!`, bare or under `log::` or `tracing::`.
fn is_log_macro(callee: &str) -> bool {
    let (path, name) = callee.rsplit_once("::").unwrap_or(("", callee));
    matches!(path, "" | "log" | "tracing")
        && matches!(name, "debug" | "info" | "warn" | "error" | "trace" | "event")
}

// True when a Rust macro's arguments, outside string literals, contain an assignment, `?` or `.await`.
fn macro_side_effects(call: Node, src: &str) -> bool {
    let Some(args) = kids(call).find(|c| c.kind() == "token_tree") else { return false };
    let mut code = src[args.byte_range()].to_string();
    let mut stack = vec![args];
    while let Some(n) = stack.pop() {
        if n.kind().contains("string_literal") || n.kind() == "char_literal" {
            let (a, b) = (n.start_byte() - args.start_byte(), n.end_byte() - args.start_byte());
            code.replace_range(a..b, &" ".repeat(b - a));
        } else {
            stack.extend(kids(n));
        }
    }
    let assigns =
        code.replace("==", "").replace("!=", "").replace("<=", "").replace(">=", "").replace("=>", "");
    assigns.contains('=') || code.contains('?') || code.contains("await")
}

// True for an `if` with no else whose condition asks whether a log level is on and whose body only logs.
fn is_log_guard(n: Node, lang: Lang, src: &str) -> bool {
    const GUARDS: [&str; 6] =
        ["isDebugEnabled", "isTraceEnabled", "isInfoEnabled", "IsEnabled", "isEnabledFor", "isLoggable"];
    if n.kind() != "if_statement" || n.child_by_field_name("alternative").is_some() {
        return false;
    }
    let (Some(cond), Some(body)) = (n.child_by_field_name("condition"), n.child_by_field_name("consequence"))
    else {
        return false;
    };
    let body = statements(body);
    GUARDS.iter().any(|g| src[cond.byte_range()].contains(g))
        && !body.is_empty()
        && body.iter().all(|s| is_logging(*s, lang, src))
}

// True when a node contains an assignment, await, yield, increment or decrement.
fn has_side_effects(n: Node) -> bool {
    let mut cursor = n.walk();
    let mut stack = vec![n];
    while let Some(x) = stack.pop() {
        if SIDE_EFFECTS.iter().any(|s| x.kind().contains(s)) || matches!(x.kind(), "++" | "--") {
            return true;
        }
        stack.extend(x.children(&mut cursor));
    }
    false
}

// Returns a parameter list's text on one line, without a trailing comma.
fn one_line(text: &str) -> String {
    let joined = text.split_whitespace().collect::<Vec<_>>().join(" ");
    joined
        .replace(", )", ")")
        .replace(", }", "}")
        .replace(",)", ")")
        .replace(",}", "}")
        .replace("( ", "(")
        .replace("{ ", "{")
}

// Collapses a multi-line constructor that only stores its parameters into one note.
fn constructor(n: Node, lang: Lang, plan: &mut Plan) {
    if let Some((name, params)) = stores_only(n, lang, plan.src) {
        if end_row(n, plan.src) > n.start_position().row {
            let note = format!("{name}{}: only assigns fields", one_line(params));
            plan.hide_node(n, Some(note), "constructors");
        }
    }
}

// Returns a constructor's name and parameter text when its body only stores parameters; None for anything else.
fn stores_only<'s>(n: Node, lang: Lang, src: &'s str) -> Option<(String, &'s str)> {
    let text = |x: Node| &src[x.byte_range()];
    let field = |f: &str| n.child_by_field_name(f);
    match (lang, n.kind()) {
        (Lang::Python, "function_definition") => {
            let (name, params, body) = (field("name")?, field("parameters")?, field("body")?);
            let ok = text(name) == "__init__" && statements(body).iter().all(|s| py_store(*s, src));
            ok.then(|| (text(name).to_string(), text(params)))
        }
        (Lang::JavaScript | Lang::TypeScript | Lang::Tsx, "method_definition") => {
            let (name, params, body) = (field("name")?, field("parameters")?, field("body")?);
            let ok = text(name) == "constructor" && statements(body).iter().all(|s| ts_store(*s, src));
            ok.then(|| (text(name).to_string(), text(params)))
        }
        (Lang::Java, "constructor_declaration") => {
            let (name, params, body) = (field("name")?, field("parameters")?, field("body")?);
            let ok = statements(body).iter().all(|s| java_store(*s, src));
            ok.then(|| (text(name).to_string(), text(params)))
        }
        (Lang::Rust, "function_item") => {
            let (name, params, body) = (field("name")?, field("parameters")?, field("body")?);
            let [only] = statements(body)[..] else { return None };
            let inits = only.child_by_field_name("body").filter(|_| only.kind() == "struct_expression")?;
            let plain = |f: Node| {
                f.kind() == "shorthand_field_initializer"
                    || f.kind() == "field_initializer"
                        && f.child_by_field_name("value").is_some_and(|v| is_value(v.kind()))
            };
            kids(inits).all(plain).then(|| (text(name).to_string(), text(params)))
        }
        (Lang::Ruby, "method") => {
            let (name, params, body) = (field("name")?, field("parameters")?, field("body")?);
            let plain = |s: &Node| {
                s.kind() == "assignment"
                    && s.child_by_field_name("left").is_some_and(|l| l.kind() == "instance_variable")
                    && s.child_by_field_name("right").is_some_and(|r| is_value(r.kind()))
            };
            let ok = text(name) == "initialize" && statements(body).iter().all(plain);
            ok.then(|| (text(name).to_string(), text(params)))
        }
        (Lang::Php, "method_declaration") => {
            let (name, params, body) = (field("name")?, field("parameters")?, field("body")?);
            let ok = text(name) == "__construct" && statements(body).iter().all(|s| php_store(*s, src));
            ok.then(|| (text(name).to_string(), text(params)))
        }
        (Lang::Swift, "init_declaration") => {
            let body = field("body")?;
            let all: Vec<Node> = (0..n.child_count()).filter_map(|i| n.child(i)).collect();
            let open = all.iter().find(|c| c.kind() == "(")?.start_byte();
            let close = all.iter().rev().find(|c| c.kind() == ")")?.end_byte();
            let ok = statements(body).iter().all(|s| swift_store(*s, src));
            ok.then(|| ("init".to_string(), &src[open..close]))
        }
        (Lang::Cpp, "function_definition") => {
            let inits = kids(n).find(|c| c.kind() == "field_initializer_list")?;
            let declarator = field("declarator")?;
            let (name, params) = (
                declarator.child_by_field_name("declarator")?,
                declarator.child_by_field_name("parameters")?,
            );
            let plain = |i: Node| {
                let args: Vec<Node> = i.named_child(1).map(|a| kids(a).collect()).unwrap_or_default();
                matches!(&args[..], [v] if is_value(v.kind()))
            };
            let ok = statements(field("body")?).is_empty() && kids(inits).all(plain);
            ok.then(|| (text(name).to_string(), text(params)))
        }
        (Lang::Go, "function_declaration") => {
            let (name, params, body) = (field("name")?, field("parameters")?, field("body")?);
            let [ret] = statements(body)[..] else { return None };
            let [value] = kids(ret.named_child(0)?).collect::<Vec<_>>()[..] else { return None };
            let lit = if value.kind() == "unary_expression" {
                value.child_by_field_name("operand")?
            } else {
                value
            };
            let elements = lit.child_by_field_name("body").filter(|_| lit.kind() == "composite_literal")?;
            let plain = |e: Node| {
                e.kind() == "keyed_element"
                    && e.child_by_field_name("value")
                        .and_then(|v| v.named_child(0))
                        .is_some_and(|v| is_value(v.kind()))
            };
            (ret.kind() == "return_statement" && kids(elements).all(plain))
                .then(|| (text(name).to_string(), text(params)))
        }
        (Lang::Dart, "member_declaration") => {
            let decl = n.named_child(0).filter(|d| d.kind() == "declaration")?;
            let ctor = ["constructor_signature", "constant_constructor_signature"];
            let sig = kids(decl).find(|c| ctor.contains(&c.kind()))?;
            let params = kids(sig).find(|c| c.kind() == "formal_parameter_list")?;
            let inits = kids(decl).find(|c| c.kind() == "initializers");
            let plain = |i: Node| text(i).split(',').all(|e| is_plain_init(e.trim_start_matches(':').trim()));
            inits
                .is_none_or(plain)
                .then(|| (src[sig.start_byte()..params.start_byte()].trim().to_string(), text(params)))
        }
        _ => None,
    }
}

// True for a Dart initializer that stores a name or calls super: `_log = log`, `super(key)`.
fn is_plain_init(e: &str) -> bool {
    let simple = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '.');
    e.starts_with("super") && !e.contains('=')
        || e.split_once('=').is_some_and(|(l, r)| simple(l.trim()) && simple(r.trim()))
}

// Returns the named statements of a block, skipping comments and looking inside a Swift `statements` child.
fn statements(block: Node) -> Vec<Node> {
    let inner = kids(block).find(|c| c.kind() == "statements").unwrap_or(block);
    kids(inner).filter(|s| !s.kind().contains("comment")).collect()
}

// True for a Python constructor statement that is a docstring, `pass`, a super().__init__ call or `self.x = name`.
fn py_store(s: Node, src: &str) -> bool {
    match (s.kind(), s.named_child(0).map(|e| (e, e.kind()))) {
        ("pass_statement", _) | ("expression_statement", Some((_, "string"))) => true,
        ("expression_statement", Some((e, "call"))) => {
            src[e.byte_range()].starts_with("super(") && !has_side_effects(e)
        }
        ("expression_statement", Some((e, "assignment"))) => stores_name(e, "self.", src).unwrap_or(false),
        _ => false,
    }
}

// True for a TypeScript constructor statement that is a super(..) call or `this.x = name`.
fn ts_store(s: Node, src: &str) -> bool {
    match (s.kind(), s.named_child(0).map(|e| (e, e.kind()))) {
        ("expression_statement", Some((e, "call_expression"))) => {
            src[e.byte_range()].starts_with("super(") && !has_side_effects(e)
        }
        ("expression_statement", Some((e, "assignment_expression"))) => {
            stores_name(e, "this.", src).unwrap_or(false)
        }
        _ => false,
    }
}

// True for a Java constructor statement that is a super(..) or this(..) call or `this.x = name`.
fn java_store(s: Node, src: &str) -> bool {
    match (s.kind(), s.named_child(0).map(|e| (e, e.kind()))) {
        ("explicit_constructor_invocation", _) => !has_side_effects(s),
        ("expression_statement", Some((e, "assignment_expression"))) => {
            stores_name(e, "this.", src).unwrap_or(false)
        }
        _ => false,
    }
}

// True for a PHP constructor statement that is `$this->x = $name;`.
fn php_store(s: Node, src: &str) -> bool {
    let Some(e) = s
        .named_child(0)
        .filter(|e| s.kind() == "expression_statement" && e.kind() == "assignment_expression")
    else {
        return false;
    };
    stores_name(e, "$this->", src).unwrap_or(false)
}

// True for a Swift initializer statement that is `self.x = name`.
fn swift_store(s: Node, src: &str) -> bool {
    let (Some(target), Some(value)) = (s.child_by_field_name("target"), s.child_by_field_name("result"))
    else {
        return false;
    };
    s.kind() == "assignment" && src[target.byte_range()].starts_with("self.") && is_value(value.kind())
}

// True for an assignment of a plain name or literal to a member of the instance, `self.x = x` or `this.x = x`.
fn stores_name(e: Node, owner: &str, src: &str) -> Option<bool> {
    let (l, r) = (e.child_by_field_name("left")?, e.child_by_field_name("right")?);
    Some(src[l.byte_range()].starts_with(owner) && is_value(r.kind()))
}

// True for an expression kind that is a plain name or literal.
fn is_value(kind: &str) -> bool {
    matches!(
        kind,
        "identifier"
            | "none"
            | "null"
            | "undefined"
            | "true"
            | "false"
            | "integer"
            | "float"
            | "number"
            | "string"
            | "integer_literal"
            | "float_literal"
            | "string_literal"
            | "boolean_literal"
            | "int_literal"
            | "interpreted_string_literal"
            | "nil"
            | "null_literal"
            | "decimal_integer_literal"
            | "simple_identifier"
            | "number_literal"
            | "nullptr"
            | "variable_name"
    )
}

// Collapses a catch clause whose body only logs and then rethrows what it caught.
fn catch(n: Node, lang: Lang, plan: &mut Plan) {
    let src = plan.src;
    let Some((start, block)) = rethrow_catch(n, lang, src) else { return };
    let (inline, step) = if lang == Lang::Go {
        let body = statements(block);
        let logs = if body.len() > 1 { "⋯ logs; " } else { "" };
        let ret = body.last().map_or(String::new(), |l| one_line(&src[l.byte_range()]));
        (format!("{{ {logs}{ret} }}"), "error returns")
    } else {
        ("{ ⋯ logs and rethrows }".to_string(), "log-and-rethrow catches")
    };
    let open = (0..block.child_count())
        .filter_map(|i| block.child(i))
        .find(|c| c.kind() == "{")
        .map_or(block.start_byte(), |b| b.start_byte());
    let header = src[start..open].trim().trim_end_matches(':').trim_end();
    let (first, last) = (plan.line(start), block.end_position().row);
    let open_line = plan.line(open);
    let starts_line = src[plan.starts[first]..start].trim().is_empty();
    let block_ends_line = plan.text(last)[block.end_position().column..].trim().is_empty();
    if starts_line && block_ends_line && lang != Lang::Go {
        plan.hide(first, last, Some(format!("{header}: logs and rethrows")), step);
    } else if !matches!(lang, Lang::Python | Lang::Ruby)
        && plan.text(open_line)[open - plan.starts[open_line] + 1..].trim().is_empty()
        && plan.text(last).trim() == "}"
        && plan.hide(open_line + 1, last, None, step)
    {
        let end = plan.starts[open_line] + plan.text(open_line).len();
        plan.changes.push((open, end, inline));
    }
}

// Returns where a catch clause starts and its block when the block only logs and then rethrows; None otherwise.
fn rethrow_catch<'t>(n: Node<'t>, lang: Lang, src: &str) -> Option<(usize, Node<'t>)> {
    let (start, block) = match (lang, n.kind()) {
        (Lang::Python, "except_clause") => (n.start_byte(), kids(n).find(|c| c.kind() == "block")?),
        (Lang::Scala, "catch_clause") => return scala_rethrow(n, src),
        (Lang::Ruby, "rescue") => (n.start_byte(), n.child_by_field_name("body")?),
        (
            Lang::JavaScript | Lang::TypeScript | Lang::Tsx | Lang::Java | Lang::Cpp | Lang::Php,
            "catch_clause",
        ) => (n.start_byte(), n.child_by_field_name("body")?),
        (Lang::Kotlin, "catch_block") => (n.start_byte(), kids(n).find(|c| c.kind() == "block")?),
        (Lang::Swift, "catch_block") => (n.start_byte(), n),
        (Lang::Go, "if_statement") if n.child_by_field_name("alternative").is_none() => {
            (n.start_byte(), n.child_by_field_name("consequence")?)
        }
        (Lang::Dart, "catch_clause") => {
            let on = n.prev_sibling().and_then(|t| t.prev_sibling()).filter(|t| t.kind() == "on");
            (on.unwrap_or(n).start_byte(), n.next_named_sibling().filter(|b| b.kind() == "block")?)
        }
        _ => return None,
    };
    let text = |x: Node| &src[x.byte_range()];
    let caught: Option<&str> = match lang {
        Lang::Java => kids(n)
            .find(|c| c.kind() == "catch_formal_parameter")
            .and_then(|p| p.child_by_field_name("name"))
            .map(text),
        Lang::Kotlin => kids(n).find(|c| c.kind() == "identifier").map(text),
        Lang::Php => n.child_by_field_name("name").map(text),
        Lang::Swift => Some(n.child_by_field_name("error").and_then(bound_name).map_or("error", text)),
        Lang::Go => nil_check(n).map(text),
        _ => n.child_by_field_name("parameter").map(text),
    };
    let last_ok = |l: &Node| match lang {
        Lang::Python => l.kind() == "raise_statement" && l.named_child_count() == 0,
        Lang::Ruby => l.kind() == "identifier" && text(*l) == "raise",
        Lang::Php => {
            let thrown = if l.kind() == "expression_statement" { l.named_child(0) } else { Some(*l) };
            thrown.is_some_and(|t| t.kind() == "throw_expression" && t.named_child(0).map(text) == caught)
        }
        Lang::Cpp => l.kind() == "throw_statement" && l.named_child_count() == 0,
        Lang::Dart => l.kind() == "rethrow_statement",
        Lang::Go => l.kind() == "return_statement" && returns_error(*l, caught, src),
        Lang::Swift => {
            let throws = kids(*l).any(|c| c.kind() == "throw_keyword");
            l.kind() == "control_transfer_statement" && throws && kids(*l).nth(1).map(text) == caught
        }
        _ => {
            matches!(l.kind(), "throw_statement" | "throw_expression") && l.named_child(0).map(text) == caught
        }
    };
    if lang == Lang::Go && caught.is_none() {
        return None;
    }
    rethrows(block, &last_ok, lang, src).then_some((start, block))
}

// Returns where a Scala catch starts and its case block when it has one case, `case e: T =>`, that only logs and rethrows `e`.
fn scala_rethrow<'t>(n: Node<'t>, src: &str) -> Option<(usize, Node<'t>)> {
    let cases = kids(n).find(|c| c.kind() == "case_block")?;
    let [clause] = kids(cases).collect::<Vec<_>>()[..] else { return None };
    let pattern = clause.child_by_field_name("pattern")?;
    let bound =
        if pattern.kind() == "typed_pattern" { pattern.child_by_field_name("pattern")? } else { pattern };
    let caught = (bound.kind() == "identifier").then(|| &src[bound.byte_range()]);
    let mut cursor = clause.walk();
    let body: Vec<Node> = clause.children_by_field_name("body", &mut cursor).collect();
    let thrown =
        |l: &Node| l.kind() == "throw_expression" && l.named_child(0).map(|e| &src[e.byte_range()]) == caught;
    let ok = body.last().is_some_and(thrown)
        && body[..body.len() - 1].iter().all(|s| is_logging(*s, Lang::Scala, src));
    ok.then_some((n.start_byte(), cases))
}

// Returns the name a Swift catch pattern binds, as in `catch let failure as MyError`.
fn bound_name(pattern: Node) -> Option<Node> {
    let mut stack = vec![pattern];
    while let Some(p) = stack.pop() {
        if let Some(name) = p.child_by_field_name("bound_identifier") {
            return Some(name);
        }
        stack.extend(kids(p));
    }
    None
}

// Returns the name a Go `if` checks with `name != nil`, if that is its whole condition.
fn nil_check(n: Node) -> Option<Node> {
    let cond = n.child_by_field_name("condition").filter(|c| c.kind() == "binary_expression")?;
    let (left, right) = (cond.child_by_field_name("left")?, cond.child_by_field_name("right")?);
    let op = cond.child_by_field_name("operator")?;
    (left.kind() == "identifier" && right.kind() == "nil" && op.kind() == "!=").then_some(left)
}

// True for a Go return whose last value is the checked error and whose other values are plain names or literals.
fn returns_error(ret: Node, err: Option<&str>, src: &str) -> bool {
    let values: Vec<Node> = ret.named_child(0).map(|l| kids(l).collect()).unwrap_or_default();
    values.split_last().is_some_and(|(last, rest)| {
        Some(&src[last.byte_range()]) == err && rest.iter().all(|v| is_value(v.kind()))
    })
}

// Returns a node's named children.
fn kids<'t>(n: Node<'t>) -> impl Iterator<Item = Node<'t>> {
    (0..n.named_child_count()).filter_map(move |i| n.named_child(i))
}

// True when a block's statements are logging followed by a last statement that passes the test.
fn rethrows(block: Node, last_ok: &dyn Fn(&Node) -> bool, lang: Lang, src: &str) -> bool {
    let body = statements(block);
    body.last().is_some_and(last_ok) && body[..body.len() - 1].iter().all(|s| is_logging(*s, lang, src))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    const PY: &str = r#"import logging
from dataclasses import dataclass
from typing import (
    Optional,
    List,
)

logger = logging.getLogger(__name__)


class InvoiceRepository:
    def __init__(self, api, cache, retries=3):
        """Store collaborators."""
        super().__init__()
        self.api = api
        self.cache = cache
        self.retries = retries

    # region loading
    def load(self, invoice_id: str) -> Optional[dict]:
        logger.debug("loading %s", invoice_id)
        try:
            return self.api.get(invoice_id)
        except IOError as e:
            logger.error("failed: %s", e)
            raise
        except KeyError:
            logger.warning("missing")
            return None
    # endregion

    def count(self):
        self.calls += 1
        logger.info("count %d", self.calls)
        return len(self.cache)
"#;
    const TS: &str = r#"import { Injectable } from '@angular/core';
import { HttpClient } from '@angular/common/http';
import type { Invoice } from './invoice';

@Injectable()
export class InvoiceRepository {
  private readonly cache = new Map<string, Invoice>();
  public retries: number;

  public constructor(
    private readonly http: HttpClient,
    retries: number,
  ) {
    this.retries = retries;
  }

  // #region loading
  public async load(id: string): Promise<Invoice> {
    console.debug(`loading ${id}`);
    this.logger?.info('load', { id });
    try {
      return await this.http.get<Invoice>(`/invoices/${id}`).toPromise();
    } catch (e) {
      console.error('failed', e);
      throw e;
    }
  }
  // #endregion

  protected count(): number {
    console.log(this.calls++);
    return this.cache.size;
  }
}
"#;
    const RUST: &str = r#"use std::io;
use std::fmt::{self, Write};

pub struct Repo { api: Api, retries: u32 }

impl Repo {
    pub fn new(api: Api, retries: u32) -> Self {
        Self { api, retries: retries }
    }

    // region loading
    pub(crate) fn load(&self, id: &str) -> Result<u32, Error> {
        log::debug!("loading {}", id);
        info!(target: "x", "y {id:?}");
        let response_body = self.api.get(id)?;
        let total_sum = response_body.len() + self.retries as u32;
        println!("{total_sum}");
        Ok(total_sum)
    }
}
"#;
    const GO: &str = r#"package repo

import (
	"fmt"
	"log"
)

type Repo struct {
	api     Api
	retries int
}

func NewRepo(api Api, retries int) *Repo {
	return &Repo{
		api:     api,
		retries: retries,
	}
}

func (r *Repo) Load(id string) (int, error) {
	log.Printf("loading %s", id)
	responseBody, err := r.api.Get(id)
	if err != nil {
		r.logger.Error("failed", "err", err)
		return 0, err
	}
	totalSum := len(responseBody) + r.retries
	fmt.Println(totalSum)
	return totalSum, nil
}
"#;
    const JAVA: &str = r#"package repo;

import java.util.List;
import java.util.Map;

public class Repo {
    private final Api api;
    private static final Logger LOG = LoggerFactory.getLogger(Repo.class);

    public Repo(Api api, int retries) {
        super();
        this.api = api;
    }

    //region loading
    public int load(String id) throws IOException {
        LOG.debug("loading {}", id);
        try {
            String responseBody = api.get(id);
            for (String lineItem : responseBody.lines()) { System.out.println(lineItem); }
            return responseBody.length();
        } catch (IOException e) {
            log.error("failed", e);
            throw e;
        }
    }
}
"#;
    const KOTLIN: &str = r#"package repo

import java.io.IOException
import kotlinx.coroutines.flow.Flow

class Repo(private val api: Api) {
    private val log = LoggerFactory.getLogger(Repo::class.java)

    // region loading
    fun load(id: String): Int {
        log.debug("loading {}", id)
        Log.d(TAG, "load $id")
        try {
            val responseBody = api.get(id = id)
            for (lineItem in responseBody.lines()) { println("$lineItem ${lineItem.length}") }
            return responseBody.length
        } catch (e: IOException) {
            log.error("failed", e)
            throw e
        }
    }
}
"#;
    const SWIFT: &str = r#"import Foundation
import os

public final class Repo {
    private let api: Api
    private let retries: Int

    public init(api: Api, retries: Int) {
        self.api = api
        self.retries = retries
    }

    // MARK: - loading
    func load(id: String) throws -> Int {
        logger.debug("loading \(id)")
        NSLog("load %@", id)
        do {
            let responseBody = try api.get(id: id)
            for lineItem in responseBody.lines { print("\(lineItem)") }
            return responseBody.count
        } catch {
            logger.error("failed \(error)")
            throw error
        }
    }
}
"#;
    const CPP: &str = r#"#include <string>
#include <vector>
#include "repo.h"

namespace repo {

Repo::Repo(Api api, int retries)
    : api_(api),
      retries_(retries) {}

#pragma region loading
int Repo::load(const std::string& id) {
    spdlog::debug("loading {}", id);
    LOG_INFO("load %s", id.c_str());
    try {
        auto responseBody = api_.get(id);
        for (const auto& lineItem : responseBody.lines()) { std::cout << lineItem; }
        auto counter = [&responseBody](int extra) { return responseBody.size() + extra; };
        return counter(0);
    } catch (const std::exception& e) {
        logger->error("failed {}", e.what());
        throw;
    }
}
#pragma endregion

}  // namespace repo
"#;
    const C: &str = r#"#include <stdio.h>
#include <stdlib.h>
#define CHECK(x) if (!(x)) return status

int parse(const char *input, int length) {
    int status = 0;
    int running_total = 0;
    LOG_DEBUG("parsing %d", length);
    for (int index_value = 0; index_value < length; index_value++) {
        running_total += input[index_value];
    }
    CHECK(running_total > 0);
    printf("%d\n", running_total);
    return running_total;
}
"#;
    const RUBY: &str = r##"require 'json'
require_relative 'api'

class Repo
  attr_reader :api

  def initialize(api, retries = 3)
    @api = api
    @retries = retries
  end

  # region loading
  def load(id, verbose: false)
    logger.debug("loading #{id}")
    Rails.logger.info "load"
    begin
      response_body = @api.get(id, verbose: verbose)
      response_body.lines.each { |line_item| puts "#{line_item} #{line_item.size}" }
      total_sum = response_body.size
      total_sum
    rescue IOError => e
      logger.error("failed #{e}")
      raise
    end
  end
end
"##;
    const PHP: &str = r#"<?php

namespace App\Repo;

use Psr\Log\LoggerInterface;
use App\Api;

class Repo
{
    public function __construct(
        private Api $api,
        private LoggerInterface $logger,
    ) {
    }

    // region loading
    public function load(string $id): int
    {
        $this->logger->debug("loading {$id}");
        error_log("load");
        try {
            $responseBody = $this->api->get($id);
            foreach ($responseBody as $lineItem) { echo "$lineItem {$lineItem}"; }
            $totalSum = count($responseBody);
            return $totalSum;
        } catch (\RuntimeException $e) {
            $this->logger->error('failed', ['e' => $e]);
            throw $e;
        }
    }
}
"#;
    const SCALA: &str = r#"package repo

import scala.util.Try
import org.slf4j.LoggerFactory

class Repo(private val api: Api) {
  private val log = LoggerFactory.getLogger(getClass)

  // region loading
  def load(id: String): Int = {
    log.debug(s"loading $id")
    try {
      val responseBody = api.get(id = id)
      for (lineItem <- responseBody.lines) println(s"$lineItem ${lineItem.length}")
      val totalSum = responseBody.length
      totalSum
    } catch {
      case e: java.io.IOException =>
        log.error("failed", e)
        throw e
    }
  }
}
"#;
    const SHELL: &str = r##"#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "$0")/lib.sh"
. ./env.sh

# region helpers
log() { echo "[$(date +%T)] $*" >&2; }
# endregion

main() {
  local target="${1:-all}"
  log "building $target"
  log_info "x" | tee out
  debug "$(git rev-parse HEAD)"
  cat <<EOF
usage: build [target]
  all      build everything
  docs     build the docs
  test     run the tests
  clean    remove build output
  help     show this text
EOF
  echo "done"
}
main "$@"
"##;
    const PERL: &str = r##"use strict;
use warnings;
use List::Util qw(sum);

# region loading
sub load {
    my ($self, $id) = @_;
    $self->{log}->debug("loading $id");
    $log->info("x");
    my $response_body = $self->{api}->get($id);
    my @line_items = split /\n/, $response_body;
    for my $line_item (@line_items) { print "$line_item\n"; }
    my %by_name = map { $_ => 1 } @line_items;
    my $total_sum = scalar(@line_items) + $by_name{x} + $line_items[0];
    return $total_sum;
}
"##;
    const LUA: &str = r##"local json = require("json")
local util = require "util"

--#region loading
local function load(api, id)
  log.debug("loading " .. id)
  logger:info("x")
  local response_body = api:get(id)
  local total_sum = 0
  for index_value, line_item in ipairs(response_body) do
    total_sum = total_sum + #line_item + index_value
  end
  local t = { total_sum = total_sum, n = response_body.n }
  return t
end
--#endregion
"##;
    const ELIXIR: &str = r##"defmodule Repo do
  require Logger
  alias App.Api
  import Enum, only: [map: 2]

  def load(api, id) do
    Logger.debug("loading #{id}")
    response_body = Api.get(api, id)
    line_items = String.split(response_body, "\n")
    total_sum = Enum.count(line_items) + response_body.size
    case total_sum do
      0 -> {:error, :empty}
      count_value -> {:ok, count_value}
    end
  end
end
"##;
    const DART: &str = r#"import 'dart:async';
import 'package:http/http.dart' as http;
import 'package:logging/logging.dart';

part 'repo.g.dart';

class InvoiceRepository {
  final http.Client client;
  final Logger _log;
  final int retries;

  InvoiceRepository({
    required this.client,
    required Logger log,
    this.retries = 3,
  }) : _log = log;

  // #region loading
  Future<String> load(String id) async {
    _log.fine('loading $id');
    debugPrint('load $id');
    try {
      final response = await client.get(Uri.parse('/invoices/$id'));
      return response.body;
    } on http.ClientException catch (e) {
      _log.severe('failed', e);
      rethrow;
    }
  }
  // #endregion

  int count() {
    print('count');
    return retries;
  }
}
"#;

    // Returns the view's lines after the header.
    fn body(src: &str, lang: Lang) -> Vec<String> {
        view("f", src, lang).expect("view").lines().skip(1).map(str::to_string).collect()
    }

    // True when the view of a snippet hides nothing and changes nothing.
    fn untouched(src: &str, lang: Lang) -> bool {
        view("f", src, lang).is_none()
    }

    // Checks file extensions map to their language.
    #[test]
    fn test_lang_of() {
        assert_eq!(lang_of(Path::new("a/b.PY")), Some(Lang::Python));
        assert_eq!(lang_of(Path::new("b.pyi")), Some(Lang::Python));
        assert_eq!(lang_of(Path::new("b.mts")), Some(Lang::TypeScript));
        assert_eq!(lang_of(Path::new("b.tsx")), Some(Lang::Tsx));
        assert_eq!(lang_of(Path::new("b.dart")), Some(Lang::Dart));
        assert_eq!(lang_of(Path::new("b.jsx")), Some(Lang::JavaScript));
        assert_eq!(lang_of(Path::new("b.rs")), Some(Lang::Rust));
        assert_eq!(lang_of(Path::new("b.go")), Some(Lang::Go));
        assert_eq!(lang_of(Path::new("B.java")), Some(Lang::Java));
        assert_eq!(lang_of(Path::new("b.kt")), Some(Lang::Kotlin));
        assert_eq!(lang_of(Path::new("b.swift")), Some(Lang::Swift));
        assert_eq!(lang_of(Path::new("b.c")), Some(Lang::C));
        assert_eq!(lang_of(Path::new("b.hpp")), Some(Lang::Cpp));
        assert_eq!(lang_of(Path::new("b.h")), Some(Lang::Cpp));
        assert_eq!(lang_of(Path::new("b.rb")), Some(Lang::Ruby));
        assert_eq!(lang_of(Path::new("b.php")), Some(Lang::Php));
        assert_eq!(lang_of(Path::new("b.scala")), Some(Lang::Scala));
        assert_eq!(lang_of(Path::new("b.sh")), Some(Lang::Shell));
        assert_eq!(lang_of(Path::new("b.zsh")), Some(Lang::Shell));
        assert_eq!(lang_of(Path::new("b.pl")), Some(Lang::Perl));
        assert_eq!(lang_of(Path::new("b.pm")), Some(Lang::Perl));
        assert_eq!(lang_of(Path::new("b.lua")), Some(Lang::Lua));
        assert_eq!(lang_of(Path::new("b.ex")), Some(Lang::Elixir));
        assert_eq!(lang_of(Path::new("b.exs")), Some(Lang::Elixir));
        assert_eq!(lang_of(Path::new("b.erl")), None);
        assert_eq!(lang_of(Path::new("Makefile")), None);
    }

    // Checks the Python rules on a whole class.
    #[test]
    fn test_view_python() {
        let v = view("repo.py", PY, Lang::Python).expect("view");
        assert!(v.starts_with("repo.py 35 lines; terse: 1 imports, 1 constructors, 1 log-and-rethrow catches, 2 regions, 3 log statements\n"), "{v}");
        assert_eq!(
            body(PY, Lang::Python),
            [
                "1-6\t⋯ 3 import(s)",
                "7\t",
                "8\tlogger = logging.getLogger(__name__)",
                "9\t",
                "11\tclass InvoiceRepository:",
                "12-17\t⋯ __init__(self, api, cache, retries=3): only assigns fields",
                "18\t",
                "20\t    def load(self, invoice_id: str) -> Optional[dict]:",
                "22\t        try:",
                "23\t            return self.api.get(invoice_id)",
                "24-26\t⋯ except IOError as e: logs and rethrows",
                "27\t        except KeyError:",
                "29\t            return None",
                "31\t",
                "32\t    def count(self):",
                "33\t        self.calls += 1",
                "35\t        return len(self.cache)",
            ]
        );
    }

    // Checks the TypeScript rules on a whole class, including the access modifiers and a `} catch {` on one line.
    #[test]
    fn test_view_typescript() {
        let lines = body(TS, Lang::TypeScript);
        assert_eq!(
            lines[..6],
            [
                "1-3\t⋯ 3 import(s)",
                "4\t",
                "5\t@Injectable()",
                "6\texport class InvoiceRepository {",
                "7~\t  readonly cache = new Map<string, Invoice>();",
                "8~\t  retries: number;"
            ]
        );
        assert!(lines.contains(
            &"10-15\t⋯ constructor(private readonly http: HttpClient, retries: number): only assigns fields"
                .to_string()
        ));
        assert!(lines.contains(&"23~\t    } catch (e) { ⋯ logs and rethrows }".to_string()));
        assert!(
            lines.contains(&"27\t  }".to_string())
                && !lines.iter().any(|l| l.starts_with("24") || l.starts_with("19"))
        );
        assert!(lines.contains(&"31\t    console.log(this.calls++);".to_string()));
        assert!(lines.contains(&"30~\t  count(): number {".to_string()));
        assert_eq!(
            view("f", TS, Lang::Tsx).map(|v| v.lines().count()),
            view("f", TS, Lang::TypeScript).map(|v| v.lines().count())
        );
    }

    // Checks the Dart rules on a whole class, including an `on T catch` and a constructor with an initializer.
    #[test]
    fn test_view_dart() {
        let lines = body(DART, Lang::Dart);
        assert_eq!(lines[0], "1-5\t⋯ 4 import(s)");
        assert!(lines.contains(&"12-16\t⋯ InvoiceRepository({required this.client, required Logger log, this.retries = 3}): only assigns fields".to_string()));
        assert!(lines
            .contains(&"25~\t    } on http.ClientException catch (e) { ⋯ logs and rethrows }".to_string()));
        assert!(!lines
            .iter()
            .any(|l| l.starts_with("20\t") || l.starts_with("21\t") || l.starts_with("18\t")));
        assert!(lines.contains(&"33\t    print('count');".to_string()));
        assert!(
            lines.contains(
                &"23~\t      final res = await client.get(Uri.parse('/invoices/$id'));".to_string()
            ),
            "{lines:?}"
        );
        assert!(lines.contains(&"24~\t      return res.body;".to_string()));
        assert!(view("f", DART, Lang::Dart).expect("view").contains(", 1 locals shortened"));
    }

    // Checks which statements count as logging.
    #[test]
    fn test_logging() {
        let py = |s: &str| untouched(&format!("def f():\n    x = 1\n    {s}\n"), Lang::Python);
        assert!(!py("logging.info('x')") && !py("self._logger.warning('x', x)") && !py("log.exception('x')"));
        assert!(
            py("logger.info(y := 1)")
                && py("self.api.debug('x')")
                && py("print('x')")
                && py("console.log('x')")
        );
        assert!(py("logger.info") && py("x"));
        let ts = |s: &str| untouched(&format!("function f() {{\n  {s}\n}}\n"), Lang::TypeScript);
        assert!(
            !ts("console.warn('x');") && !ts("this.logger.logInformation('x');") && !ts("logger?.error(e);")
        );
        assert!(
            ts("logger.info(await f());")
                && ts("logger.info(i++);")
                && ts("logger.info(x = 1);")
                && ts("console.table(x);")
        );
        assert!(ts("api.info('x');") && ts("console.log('a'), f();"));
        let dart = |s: &str| untouched(&format!("void f() {{\n  {s}\n}}\n"), Lang::Dart);
        assert!(
            !dart("log('x');")
                && !dart("debugPrint('x');")
                && !dart("_logger.d('x');".replace(".d(", ".debug(").as_str())
        );
        assert!(dart("print('x');") && dart("console.log('x');") && dart("_log.info(i++);"));
        assert!(untouched("x = 1\nlogger.info('x'); y = 2\n", Lang::Python));
    }

    // Checks constructors that do more than store their parameters are kept.
    #[test]
    fn test_constructor_kept() {
        assert!(untouched(
            "class A:\n    def __init__(self, a):\n        self.a = a\n        self.load()\n",
            Lang::Python
        ));
        assert!(untouched("class A:\n    def __init__(self, a):\n        self.a = a + 1\n", Lang::Python));
        assert!(untouched("class A:\n    def __init__(self, a):\n        a.b = a\n", Lang::Python));
        assert!(untouched("class A:\n    def setup(self, a):\n        self.a = a\n", Lang::Python));
        assert!(untouched("class A:\n    def __init__(self, a): self.a = a\n", Lang::Python));
        assert_eq!(
            body("class A:\n    def __init__(self):\n        pass\n", Lang::Python),
            ["1\tclass A:", "2-3\t⋯ __init__(self): only assigns fields"]
        );
        assert!(untouched(
            "class A {\n  constructor(a) {\n    this.a = a;\n    this.load();\n  }\n}\n",
            Lang::TypeScript
        ));
        assert!(untouched("class A {\n  constructor(a) {\n    this.a = f(a);\n  }\n}\n", Lang::TypeScript));
        assert!(untouched("class A {\n  constructor(a) {\n    super(a++);\n  }\n}\n", Lang::TypeScript));
        assert!(untouched("class A {\n  constructor(a) {\n    x.a = a;\n  }\n}\n", Lang::TypeScript));
        assert!(untouched("class A {\n  start(a) {\n    this.a = a;\n  }\n}\n", Lang::TypeScript));
        assert_eq!(
            body("class A {\n  constructor(a) {\n    super(a);\n    this.a = a;\n  }\n}\n", Lang::TypeScript)
                [1],
            "2-5\t⋯ constructor(a): only assigns fields"
        );
        assert!(untouched("class A {\n  A(\n    this.a,\n  ) : b = f(a);\n}\n", Lang::Dart));
        assert_eq!(
            body("class A {\n  const A(\n    this.a,\n  ) : super(a);\n}\n", Lang::Dart)[1],
            "2-4\t⋯ const A(this.a): only assigns fields"
        );
        assert!(untouched("class A {\n  factory A(\n    int a,\n  ) => A._(a);\n}\n", Lang::Dart));
        assert!(untouched("class A {\n  int x = 1;\n}\n", Lang::Dart));
        assert!(is_plain_init("super(key)") && is_plain_init("_a = a") && is_plain_init("this.a = b.c"));
        assert!(!is_plain_init("a = f(b)") && !is_plain_init("= a"));
    }

    // Checks catches that do more than log and rethrow are kept, and those on their own lines are hidden whole.
    #[test]
    fn test_catch() {
        assert!(!view("f", "try:\n    f()\nexcept E as e:\n    log.error(e)\n    raise e\n", Lang::Python)
            .expect("view")
            .contains("rethrows"));
        assert!(untouched("try:\n    f()\nexcept E:\n    g()\n    raise\n", Lang::Python));
        assert!(
            untouched("try:\n    f()\nexcept E: raise\n", Lang::Python)
                == untouched("try:\n    f()\nexcept E: raise\n", Lang::Python)
        );
        let ts = |s: &str| {
            view("f", &format!("try {{\n  f();\n}} {s}"), Lang::TypeScript)
                .is_none_or(|v| !v.contains("rethrows"))
        };
        assert!(
            ts("catch (e) {\n  logger.error(e);\n  throw new Error('x');\n}\n")
                && ts("catch (e) {\n  g();\n  throw e;\n}\n")
        );
        assert!(ts("catch {\n  throw e;\n}\n") && ts("catch (e) {\n  throw e;\n} finally {\n  g();\n}\n"));
        assert!(ts("catch (e) { throw e; }\n"));
        assert_eq!(
            body("try {\n  f();\n}\ncatch (e)\n{\n  console.error(e);\n  throw e;\n}\n", Lang::TypeScript)[3],
            "4-8\t⋯ catch (e): logs and rethrows"
        );
        assert_eq!(
            body("void g() {\n  try {\n    f();\n  } catch (e) {\n    rethrow;\n  }\n}\n", Lang::Dart)[3],
            "4~\t  } catch (e) { ⋯ logs and rethrows }"
        );
        assert!(untouched(
            "void g() {\n  try {\n    f();\n  } catch (e) {\n    g();\n    rethrow;\n  }\n}\n",
            Lang::Dart
        ));
        assert!(untouched(
            "void g() {\n  try {\n    f();\n  } on E catch (e) {\n    throw e;\n  }\n}\n",
            Lang::Dart
        ));
    }

    // Checks statements, imports and catches that share a line with other code are kept, and the last line is kept without a newline.
    #[test]
    fn test_shared_lines() {
        let v = body("import a\nimport b  # x\ndef f():\n    # note\n    logger.info('x')  # why\n    logger.info('y')\n    return 1", Lang::Python);
        assert_eq!(
            v,
            [
                "1\timport a",
                "2\timport b  # x",
                "3\tdef f():",
                "4\t    # note",
                "5\t    logger.info('x')  # why",
                "7\t    return 1"
            ]
        );
        let ts = |s: &str| {
            view("f", &format!("function f() {{\n  try {{\n    g();\n  }} {s}\n}}\n"), Lang::TypeScript)
        };
        assert!(ts("catch (e)\n  {\n    throw e;\n  } finally {\n    h();\n  }").is_none());
        assert!(
            ts("catch (e) {\n    throw err;\n  }").is_none() && ts("catch (e) {\n    return;\n  }").is_none()
        );
        assert!(is_plain_init("superb = c"));
    }

    // Checks region markers are recognised and other comments are not.
    #[test]
    fn test_is_region() {
        for c in ["# region x", "#region", "# endregion", "// #region a", "//#endregion", "// region"] {
            assert!(is_region(c), "{c}");
        }
        for c in ["# regional", "// a region", "# end", "/* region */x"] {
            assert!(!is_region(c), "{c}");
        }
    }

    // Checks the rendering details: CRLF lines, imports that share a line, one import, and files too big to parse.
    #[test]
    fn test_render_edges() {
        assert_eq!(
            body("import a\r\nimport b\r\nx = 1\r\n", Lang::Python),
            ["1-2\t⋯ 2 import(s)", "3\tx = 1"]
        );
        assert!(untouched("import a\nx = 1\n", Lang::Python));
        assert!(untouched("import a; import b\n", Lang::Python));
        assert!(untouched("x = 1\n", Lang::Dart));
        let big = "x = 1\n".repeat(crate::outline::MAX_PARSE_BYTES / 6 + 1);
        assert!(untouched(&big, Lang::Python));
        assert_eq!(one_line("(\n  a,\n  b,\n)"), "(a, b)");
        assert_eq!(one_line("({\n  required this.a,\n})"), "({required this.a})");
    }

    // Checks the Rust rules on a whole impl: use runs, the field-only `new`, pub, log macros and short locals.
    #[test]
    fn test_view_rust() {
        let v = view("a.rs", RUST, Lang::Rust).expect("view");
        assert!(v.starts_with("a.rs 20 lines; terse: 1 imports, 1 constructors, 2 access modifiers, 1 regions, 2 log statements, 1 locals shortened\n"), "{v}");
        let lines = body(RUST, Lang::Rust);
        for want in [
            "1-2\t⋯ 2 import(s)",
            "4~\tstruct Repo { api: Api, retries: u32 }",
            "7-9\t⋯ new(api: Api, retries: u32): only assigns fields",
            "12~\t    fn load(&self, id: &str) -> Result<u32, Error> {",
            "15~\t        let rb = self.api.get(id)?;",
            "17\t        println!(\"{total_sum}\");",
        ] {
            assert!(lines.contains(&want.to_string()), "{want}: {lines:?}");
        }
    }

    // Checks the Go rules: an import block, a field-only constructor, logging, an `if err != nil` return and short locals.
    #[test]
    fn test_view_go() {
        let lines = body(GO, Lang::Go);
        for want in [
            "3-6\t⋯ 2 import(s)",
            "13-18\t⋯ NewRepo(api Api, retries int): only assigns fields",
            "23~\t\tif err != nil { ⋯ logs; return 0, err }",
            "27~\t\tts := len(rb) + r.retries",
            "28~\t\tfmt.Println(ts)",
        ] {
            assert!(lines.contains(&want.to_string()), "{want}: {lines:?}");
        }
        assert!(!lines.iter().any(|l| l.starts_with("21\t") || l.starts_with("24") || l.starts_with("25")));
        let quiet =
            "func f() error {\n\tx, err := g()\n\tif err != nil {\n\t\treturn err\n\t}\n\treturn h(x)\n}\n";
        assert_eq!(body(quiet, Lang::Go)[2], "3~\t\tif err != nil { return err }");
    }

    // Checks the Java rules: imports, the field-only constructor, modifiers, a region, logging, a rethrowing catch and short locals.
    #[test]
    fn test_view_java() {
        let lines = body(JAVA, Lang::Java);
        for want in [
            "3-4\t⋯ 2 import(s)",
            "6~\tclass Repo {",
            "8~\t    static final Logger LOG = LoggerFactory.getLogger(Repo.class);",
            "10-13\t⋯ Repo(Api api, int retries): only assigns fields",
            "19~\t            String rb = api.get(id);",
            "22~\t        } catch (IOException e) { ⋯ logs and rethrows }",
        ] {
            assert!(lines.contains(&want.to_string()), "{want}: {lines:?}");
        }
        assert!(!lines.iter().any(|l| l.starts_with("15\t") || l.starts_with("17\t")));
    }

    // Checks JavaScript uses the TypeScript rules.
    #[test]
    fn test_view_javascript() {
        let src = "import a from 'a';\nimport b from 'b';\nexport class A {\n  constructor(api) {\n    this.api = api;\n  }\n  load(id) {\n    console.debug(id);\n    const responseBody = this.api.get(id);\n    return responseBody;\n  }\n}\n";
        assert_eq!(
            body(src, Lang::JavaScript),
            [
                "1-2\t⋯ 2 import(s)",
                "3\texport class A {",
                "4-6\t⋯ constructor(api): only assigns fields",
                "7\t  load(id) {",
                "9~\t    const rb = this.api.get(id);",
                "10~\t    return rb;",
                "11\t  }",
                "12\t}"
            ]
        );
    }

    // Checks logging that must stay: side effects, program exits, receives, guards with else, and plain prints.
    #[test]
    fn test_logging_kept() {
        let rs = |s: &str| {
            view("f", &format!("fn f() -> R {{\n    let x = 1;\n    {s}\n}}\n"), Lang::Rust).is_none()
        };
        assert!(
            !rs("debug!(\"{:?} {}\", x, a == b);")
                && !rs("tracing::info!(\"x\");")
                && !rs("log::warn!(\"x {x:?}\");")
        );
        assert!(
            rs("info!(\"{}\", g()?);") && rs("info!(\"{}\", g().await);") && rs("info!(user = x, \"m\");")
        );
        assert!(rs("println!(\"x\");") && rs("other::info!(\"x\");") && rs("eprintln!(\"x\");"));
        let go = |s: &str| view("f", &format!("func f() {{\n\t{s}\n}}\n"), Lang::Go).is_none();
        assert!(
            !go("log.Printf(\"%d\", x)")
                && !go("slog.InfoContext(ctx, \"m\")")
                && !go("r.logger.Debugw(\"m\")")
        );
        assert!(
            go("log.Fatalf(\"x\")") && go("log.Panic(x)") && go("log.Print(<-ch)") && go("fmt.Println(x)")
        );
        let java = |s: &str| {
            view("f", &format!("class A {{\n  void f() {{\n    {s}\n  }}\n}}\n"), Lang::Java).is_none()
        };
        assert!(
            !java("LOGGER.info(\"x\");")
                && !java("if (log.isDebugEnabled()) {\n      log.debug(\"x\");\n    }")
        );
        assert!(java("log.info(i++);") && java("System.out.println(x);"));
        assert!(java("if (log.isDebugEnabled()) {\n      g();\n    }"));
        let shown = |s: &str| {
            view("f", &format!("class A {{\n  void f() {{\n    {s}\n  }}\n}}\n"), Lang::Java).expect("view")
        };
        let with_else =
            "if (log.isDebugEnabled()) {\n      log.debug(\"x\");\n    } else {\n      g();\n    }";
        assert!(shown(with_else).contains("    if (log.isDebugEnabled())"));
        assert!(shown("if (ready()) {\n      log.debug(\"x\");\n    }").contains("    if (ready())"));
        assert!(java("if (log.isDebugEnabled()) {\n    }"));
    }

    // Checks constructors, catches and error returns that do more are kept.
    #[test]
    fn test_kept_new_languages() {
        let none = |s: &str, l: Lang| view("f", s, l).is_none();
        assert!(none(
            "impl A {\n    fn new(a: u8) -> Self {\n        Self { a, ..Default::default() }\n    }\n}\n",
            Lang::Rust
        ));
        assert!(none(
            "impl A {\n    fn new(a: u8) -> Self {\n        Self { a: a + 1 }\n    }\n}\n",
            Lang::Rust
        ));
        assert!(none(
            "impl A {\n    fn new(a: u8) -> Self {\n        g();\n        Self { a }\n    }\n}\n",
            Lang::Rust
        ));
        assert!(none("impl A {\n    fn new(a: u8) -> Self {\n        Other::new(a)\n    }\n}\n", Lang::Rust));
        assert!(none("func NewA(a int) *A {\n\treturn &A{\n\t\ta: f(a),\n\t}\n}\n", Lang::Go));
        assert!(none("func NewA(a int) A {\n\treturn A{\n\t\ta,\n\t}\n}\n", Lang::Go));
        assert!(none("func NewA(a int) (*A, error) {\n\treturn &A{\n\t\ta: a,\n\t}, nil\n}\n", Lang::Go));
        assert!(none("func NewA(a int) *A {\n\tg()\n\treturn nil\n}\n", Lang::Go));
        assert!(none("func NewA(a int) *A {\n\treturn g(\n\t\ta)\n}\n", Lang::Go));
        let go_err = |s: &str| {
            view("f", &format!("func f() error {{\n\terr := g()\n\t{s}\n\treturn nil\n}}\n"), Lang::Go)
                .is_none_or(|v| !v.contains("error returns"))
        };
        assert!(
            go_err("if err != nil {\n\t\treturn fmt.Errorf(\"x: %w\", err)\n\t}")
                && go_err("if err != nil {\n\t\treturn err\n\t} else {\n\t\tg()\n\t}")
        );
        assert!(
            go_err("if err == nil {\n\t\treturn err\n\t}")
                && go_err("if ok {\n\t\treturn err\n\t}")
                && go_err("if err != nil {\n\t\treturn other\n\t}")
        );
        assert!(
            go_err("if err != nil {\n\t\treturn f(), err\n\t}")
                && go_err("if err != nil {\n\t\tg()\n\t\treturn err\n\t}")
                && go_err("if x.y != nil {\n\t\treturn err\n\t}")
        );
        assert!(none("class A {\n  A(int a) {\n    this.a = a;\n    init();\n  }\n}\n", Lang::Java));
        assert!(none("class A {\n  A(int a) {\n    this.a = a * 2;\n  }\n}\n", Lang::Java));
        let java = |s: &str| {
            view(
                "f",
                &format!("class A {{\n  void f() {{\n    try {{\n      g();\n    }} {s}\n  }}\n}}\n"),
                Lang::Java,
            )
            .is_none()
        };
        assert!(
            java("catch (Exception e) {\n      throw new RuntimeException(e);\n    }")
                && java("catch (Exception e) {\n      g();\n      throw e;\n    }")
        );
        assert!(none("pub use a::b;\n", Lang::Rust));
    }

    // Checks the remaining edges: shared lines, imports after code, more Go log calls, char literals and other nil checks.
    #[test]
    fn test_more_edges() {
        assert!(view("f", "function f() {\n  g(); console.log(1);\n}\n", Lang::JavaScript).is_none());
        assert!(view("f", "x = 1; import a\nimport b\n", Lang::Python).is_none());
        let go = |s: &str| view("f", &format!("func f() {{\n\t{s}\n}}\n"), Lang::Go).is_none();
        assert!(!go("log.Println(x)") && !go("slog.LogAttrs(ctx, slog.LevelInfo, \"m\")"));
        assert!(view("f", "fn f() {\n    debug!(\"{}\", 'c');\n}\n", Lang::Rust).is_some());
        let go_err = |s: &str| {
            let src = format!("func f() error {{\n\terr := g()\n\t{s}\n\treturn nil\n}}\n");
            view("f", &src, Lang::Go).is_none_or(|v| !v.contains("error returns"))
        };
        assert!(
            go_err("if err != nil {\n\t\tpanic(err)\n\t}")
                && go_err("if err != other {\n\t\treturn err\n\t}")
        );
    }

    // Asserts each wanted line is in a view.
    fn has_all(lines: &[String], want: &[&str]) {
        for w in want {
            assert!(lines.contains(&w.to_string()), "{w}: {lines:?}");
        }
    }

    // Checks the Kotlin rules: imports, Android and SLF4J logging, a region, modifiers, a rethrowing catch, and locals except `$name` templates.
    #[test]
    fn test_view_kotlin() {
        let lines = body(KOTLIN, Lang::Kotlin);
        has_all(
            &lines,
            &[
                "3-4\t⋯ 2 import(s)",
                "6~\tclass Repo(val api: Api) {",
                "14~\t            val rb = api.get(id = id)",
                "15~\t            for (lineItem in rb.lines()) { println(\"$lineItem ${lineItem.length}\") }",
                "17~\t        } catch (e: IOException) { ⋯ logs and rethrows }",
            ],
        );
        assert!(!lines
            .iter()
            .any(|l| l.starts_with("9\t") || l.starts_with("11\t") || l.starts_with("12\t")));
    }

    // Checks the Swift rules: imports, the field-only init, modifiers, NSLog and logger calls, a bare catch that rethrows, and locals; MARK comments stay.
    #[test]
    fn test_view_swift() {
        let lines = body(SWIFT, Lang::Swift);
        has_all(
            &lines,
            &[
                "1-2\t⋯ 2 import(s)",
                "4~\tfinal class Repo {",
                "8-11\t⋯ init(api: Api, retries: Int): only assigns fields",
                "13\t    // MARK: - loading",
                "18~\t            let rb = try api.get(id: id)",
                "19~\t            for li in rb.lines { print(\"\\(li)\") }",
                "21~\t        } catch { ⋯ logs and rethrows }",
            ],
        );
        assert!(!lines.iter().any(|l| l.starts_with("15\t") || l.starts_with("16\t")));
    }

    // Checks the C++ rules: includes, the initializer-list constructor, pragma regions, spdlog and macro logging, a bare rethrow, and locals with a lambda capture.
    #[test]
    fn test_view_cpp() {
        let lines = body(CPP, Lang::Cpp);
        has_all(
            &lines,
            &[
                "1-3\t⋯ 3 import(s)",
                "7-9\t⋯ Repo::Repo(Api api, int retries): only assigns fields",
                "18~\t        auto cou = [&rb](int extra) { return rb.size() + extra; };",
                "20~\t    } catch (const std::exception& e) { ⋯ logs and rethrows }",
            ],
        );
        assert!(!lines.iter().any(|l| ["11\t", "13\t", "14\t", "25\t"].iter().any(|k| l.starts_with(k))));
    }

    // Checks C: includes, a LOG_ macro, and locals except one a #define names.
    #[test]
    fn test_view_c() {
        let lines = body(C, Lang::C);
        has_all(
            &lines,
            &[
                "1-2\t⋯ 2 import(s)",
                "6\t    int status = 0;",
                "7~\t    int rt = 0;",
                "9~\t    for (int iv = 0; iv < length; iv++) {",
            ],
        );
        assert!(!lines.iter().any(|l| l.starts_with("8\t")));
    }

    // Checks what the new languages keep: prints, side effects, rethrows of something else, constructors with logic, and stream logs.
    #[test]
    fn test_kept_kotlin_swift_c() {
        let kt = |s: &str| view("f", &format!("fun f() {{\n    {s}\n}}\n"), Lang::Kotlin).is_none();
        assert!(
            !kt("Log.d(TAG, \"x\")")
                && !kt("Timber.w(\"x\")")
                && kt("println(x)")
                && kt("log.info(i++)")
                && kt("Log.x(y)")
        );
        let sw = |s: &str| view("f", &format!("func f() {{\n    {s}\n}}\n"), Lang::Swift).is_none();
        assert!(
            !sw("os_log(\"x\")") && !sw("logger.info(\"x\")") && sw("print(x)") && sw("logger.info(x = 1)")
        );
        let cpp = |s: &str| view("f", &format!("void f() {{\n    {s}\n}}\n"), Lang::Cpp).is_none();
        assert!(
            !cpp("LOG(INFO) << \"x\" << y;")
                && !cpp("qDebug() << x;")
                && !cpp("LOGE(\"x\");")
                && !cpp("log_->debug(\"x\");")
        );
        assert!(
            cpp("std::cout << x;")
                && cpp("printf(\"x\");")
                && cpp("LOG(INFO) << i++;")
                && cpp("x << LOG(INFO);")
                && cpp("Logger_x(1);")
        );
        assert!(cpp(
            "try {\n        g();\n    } catch (...) {\n        throw std::runtime_error(\"x\");\n    }"
        ));
        let ctor = |s: &str| view("f", s, Lang::Cpp).is_none();
        assert!(
            ctor("A::A(int a)\n    : a_(a) {\n    init();\n}\n")
                && ctor("A::A(int a)\n    : a_(a + 1) {}\n")
                && ctor("int f(int a)\n{\n    return a;\n}\n")
        );
        assert!(view("f", "struct A {\n  init(a: Int) {\n    self.a = a\n    load()\n  }\n}\n", Lang::Swift)
            .is_none());
        let kt_catch = "fun f() {\n    try {\n        g()\n    } catch (e: Exception) {\n        throw other\n    }\n}\n";
        assert!(view("f", kt_catch, Lang::Kotlin).is_none());
        let sw_catch = "func f() throws {\n    do {\n        try g()\n    } catch let failure as MyError {\n        throw failure\n    }\n}\n";
        assert!(view("f", sw_catch, Lang::Swift).is_some_and(|v| v.contains("{ ⋯ logs and rethrows }")));
        assert!(view("f", "#pragma once\n#pragma region a\n#pragma endregion\nint x;\n", Lang::Cpp)
            .is_some_and(|v| v.contains("2 regions")));
    }

    // Checks the last edges of the C++ and Swift rules.
    #[test]
    fn test_cpp_swift_edges() {
        let cpp = |s: &str| view("f", &format!("void f() {{\n    {s}\n}}\n"), Lang::Cpp).is_none();
        assert!(cpp("LOG(INFO) + x;") && cpp("log_info(x);"));
        assert!(!cpp("try {\n        g();\n    } catch (const E& e) {\n        log_->error(\"x\");\n        throw e;\n    }"));
        let rethrown =
            "void f() {\n    try {\n        g();\n    } catch (const E& e) {\n        throw e;\n    }\n}\n";
        assert!(view("f", rethrown, Lang::Cpp).is_none());
        let swift_return = "func f() {\n    do {\n        try g()\n    } catch {\n        return\n    }\n}\n";
        assert!(view("f", swift_return, Lang::Swift).is_none());
        assert!(view("f", "struct A {\n  init(a: Int) {\n    x = a\n  }\n}\n", Lang::Swift).is_none());
    }

    // Checks the Ruby rules: requires, the field-only initialize, logging, a region, a re-raising rescue and locals.
    #[test]
    fn test_view_ruby() {
        let lines = body(RUBY, Lang::Ruby);
        has_all(
            &lines,
            &[
                "1-2\t⋯ 2 import(s)",
                "7-10\t⋯ initialize(api, retries = 3): only assigns fields",
                "17~\t      rb = @api.get(id, verbose: verbose)",
                "18~\t      rb.lines.each { |line_item| puts \"#{line_item} #{line_item.size}\" }",
                "21-23\t⋯ rescue IOError => e: logs and rethrows",
            ],
        );
        assert!(!lines.iter().any(|l| ["12\t", "14\t", "15\t"].iter().any(|k| l.starts_with(k))));
    }

    // Checks the PHP rules: use runs, the promoted constructor, method visibility, logging, a rethrowing catch and locals in strings.
    #[test]
    fn test_view_php() {
        let lines = body(PHP, Lang::Php);
        has_all(&lines, &["5-6\t⋯ 2 import(s)", "10-14\t⋯ __construct(private Api $api, private LoggerInterface $logger): only assigns fields", "17~\t    function load(string $id): int", "23~\t            foreach ($rb as $li) { echo \"$li {$li}\"; }", "26~\t        } catch (\\RuntimeException $e) { ⋯ logs and rethrows }"]);
        assert!(!lines.iter().any(|l| l.starts_with("19\t") || l.starts_with("20\t")));
    }

    // Checks the Scala rules: imports, access modifiers, logging, a single-case rethrowing catch and locals with a named argument.
    #[test]
    fn test_view_scala() {
        let lines = body(SCALA, Lang::Scala);
        has_all(
            &lines,
            &[
                "3-4\t⋯ 2 import(s)",
                "6~\tclass Repo(val api: Api) {",
                "13~\t      val rb = api.get(id = id)",
                "14~\t      for (li <- rb.lines) println(s\"$li ${li.length}\")",
                "17~\t    } catch { ⋯ logs and rethrows }",
            ],
        );
        assert!(!lines.iter().any(|l| l.starts_with("11\t") || l.starts_with("9\t")));
    }

    // Checks what Ruby, PHP and Scala keep: prints, side effects, other raises, constructors with logic and multi-case catches.
    #[test]
    fn test_kept_ruby_php_scala() {
        let rb = |s: &str| view("f", &format!("def f\n  x = 1\n  {s}\nend\n"), Lang::Ruby).is_none();
        assert!(!rb("Rails.logger.info \"x\"") && rb("puts x") && rb("logger.info(x += 1)") && rb("info(x)"));
        assert!(view("f", "class A\n  def initialize(a)\n    @a = a\n    load\n  end\nend\n", Lang::Ruby)
            .is_none());
        let rescue = "def f\n  g\nrescue E => e\n  raise Other\nend\n";
        assert!(view("f", rescue, Lang::Ruby).is_none());
        let php = |s: &str| view("f", &format!("<?php\nfunction f() {{\n    {s}\n}}\n"), Lang::Php).is_none();
        assert!(
            !php("error_log('x');")
                && !php("Log::info('x');")
                && php("echo 'x';")
                && php("$log->info($i++);")
        );
        let ctor = "<?php\nclass A {\n    function __construct($a) {\n        $this->a = $a;\n        $this->init();\n    }\n}\n";
        assert!(view("f", ctor, Lang::Php).is_none_or(|v| !v.contains("only assigns")));
        let php_catch = "<?php\nfunction f() {\n    try {\n        g();\n    } catch (E $e) {\n        throw new X($e);\n    }\n}\n";
        assert!(view("f", php_catch, Lang::Php).is_none());
        let scala_two = "object A {\n  def f() = {\n    try {\n      g()\n    } catch {\n      case e: E => throw e\n      case x: X => throw x\n    }\n  }\n}\n";
        assert!(view("f", scala_two, Lang::Scala).is_none());
        let scala_other = "object A {\n  def f() = {\n    try {\n      g()\n    } catch {\n      case e: E => h()\n    }\n  }\n}\n";
        assert!(view("f", scala_other, Lang::Scala).is_none());
        assert!(view("f", "object A {\n  def f() = {\n    log.info(s\"x\")\n  }\n}\n", Lang::Scala).is_some());
    }

    // Checks the remaining Ruby, PHP and Scala edges: requires with a receiver, initialize with a plain local, untyped catch patterns.
    #[test]
    fn test_ruby_scala_edges() {
        assert!(view("f", "Kernel.require 'a'\nKernel.require 'b'\n", Lang::Ruby).is_none());
        assert!(view("f", "class A\n  def initialize(a)\n    x = a\n  end\nend\n", Lang::Ruby).is_none());
        let untyped = "object A {\n  def f() = {\n    try {\n      g()\n    } catch {\n      case e => throw e\n    }\n  }\n}\n";
        assert!(view("f", untyped, Lang::Scala).is_some_and(|v| v.contains("logs and rethrows")));
    }

    // Checks the shell rules: source runs, logging helpers without subcommands, regions and long heredocs; output, pipelines and subcommands stay.
    #[test]
    fn test_view_shell() {
        let v = view("a.sh", SHELL, Lang::Shell).expect("view");
        assert!(
            v.starts_with("a.sh 25 lines; terse: 1 imports, 2 regions, 1 log statements, 1 heredoc bodies\n"),
            "{v}"
        );
        let lines = body(SHELL, Lang::Shell);
        has_all(
            &lines,
            &[
                "3-4\t⋯ 2 import(s)",
                "13\t  log_info \"x\" | tee out",
                "14\t  debug \"$(git rev-parse HEAD)\"",
                "15\t  cat <<EOF",
                "16-21\t⋯ heredoc, 6 line(s)",
                "22\tEOF",
                "23\t  echo \"done\"",
            ],
        );
        assert!(!lines.iter().any(|l| l.starts_with("12\t") || l.starts_with("6\t")));
        let sh = |s: &str| view("f", &format!("f() {{\n  {s}\n}}\n"), Lang::Shell).is_none();
        assert!(
            !sh("logger -t app \"x\"")
                && !sh("debug_trace x")
                && sh("echo x")
                && sh("log $((i++))")
                && sh("info x")
        );
        assert!(view("f", "if true; then\n  log \"x\"\nfi\n", Lang::Shell).is_some());
        assert!(view("f", "cat <<EOF\na\nb\nEOF\n", Lang::Shell).is_none());
        assert!(view("f", "source a.sh\necho x\n", Lang::Shell).is_none());
    }

    // Checks the Perl, Lua and Elixir views: imports, regions, logging and locals, with parameters kept and interpolated uses renamed.
    #[test]
    fn test_view_perl_lua_elixir() {
        let perl = view("f", PERL, Lang::Perl).expect("view");
        assert!(
            perl.starts_with(
                "f 16 lines; terse: 1 imports, 1 regions, 2 log statements, 5 locals shortened\n"
            ),
            "{perl}"
        );
        let lines = body(PERL, Lang::Perl);
        has_all(
            &lines,
            &[
                "1-3\t⋯ 3 import(s)",
                "7\t    my ($self, $id) = @_;",
                "12~\t    for my $li2 (@li) { print \"$li2\\n\"; }",
                "14~\t    my $ts = scalar(@li) + $bn{x} + $li[0];",
            ],
        );
        assert!(!lines.iter().any(|l| l.starts_with("8\t") || l.starts_with("9\t")));
        let lines = body(LUA, Lang::Lua);
        has_all(
            &lines,
            &[
                "1-2\t⋯ 2 import(s)",
                "5\tlocal function load(api, id)",
                "10~\t  for iv, li in ipairs(rb) do",
                "13~\t  local t = { total_sum = ts, n = rb.n }",
            ],
        );
        assert!(!lines.iter().any(|l| ["4\t", "6\t", "7\t", "16\t"].iter().any(|k| l.starts_with(k))));
        let lines = body(ELIXIR, Lang::Elixir);
        has_all(
            &lines,
            &[
                "1\tdefmodule Repo do",
                "2-4\t⋯ 3 import(s)",
                "6\t  def load(api, id) do",
                "10~\t    ts = Enum.count(li) + rb.size",
                "13\t      count_value -> {:ok, count_value}",
            ],
        );
        assert!(!lines.iter().any(|l| l.starts_with("7\t")));
    }

    // Checks what Perl, Lua and Elixir keep: prints, logging with side effects, receivers that are not loggers and aliases inside a function.
    #[test]
    fn test_kept_perl_lua_elixir() {
        let pl = |s: &str| view("f", &format!("sub f {{\n    {s}\n}}\n"), Lang::Perl).is_none();
        assert!(
            !pl("$log->info(\"x\");")
                && !pl("DEBUG \"x\";")
                && pl("print \"x\";")
                && pl("$log->info($i++);")
                && pl("$cart->info(\"x\");")
        );
        let lua = |s: &str| view("f", &format!("function f()\n  {s}\nend\n"), Lang::Lua).is_none();
        assert!(
            !lua("log.info(\"x\")")
                && !lua("logger:debug(\"x\")")
                && lua("print(\"x\")")
                && lua("cart.info(\"x\")")
        );
        let ex = |s: &str| {
            view("f", &format!("defmodule A do\n  def f do\n    {s}\n  end\nend\n"), Lang::Elixir).is_none()
        };
        assert!(!ex("Logger.info(\"x\")") && ex("IO.puts(\"x\")") && ex("Cart.info(\"x\")"));
        assert!(ex("alias A.B\n    alias A.C"));
        assert!(view("f", "alias A.B\nalias A.C\n", Lang::Elixir).is_some());
        assert!(view("f", "local x = 1\nlocal y = 2\n", Lang::Lua).is_none());
    }
}
