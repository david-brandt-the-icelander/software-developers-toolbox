// Terse view of Python, TypeScript and Dart files: hides imports, logging, region markers, constructors that only assign fields and catches that only log and rethrow, drops TypeScript access modifiers and shortens long local names (locals.rs), keeping source line numbers.
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
const CONSOLE_METHODS: [&str; 6] = ["log", "debug", "info", "warn", "error", "trace"];
const SIDE_EFFECTS: [&str; 7] =
    ["assignment", "await", "yield", "update_expression", "increment", "decrement", "named_expression"];

// The languages this view handles.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Lang {
    Python,
    TypeScript,
    Tsx,
    Dart,
}

// Returns the language of a path from its extension, if this view handles it.
pub fn lang_of(path: &Path) -> Option<Lang> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "py" | "pyi" => Some(Lang::Python),
        "ts" | "mts" | "cts" => Some(Lang::TypeScript),
        "tsx" => Some(Lang::Tsx),
        "dart" => Some(Lang::Dart),
        _ => None,
    }
}

// Returns the tree-sitter grammar for a language.
fn grammar(lang: Lang) -> Language {
    match lang {
        Lang::Python => tree_sitter_python::LANGUAGE.into(),
        Lang::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        Lang::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
        Lang::Dart => tree_sitter_dart::LANGUAGE.into(),
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
        let (a, b) = (n.start_byte(), n.end_byte());
        let before = &self.src[self.starts[self.line(a)]..a];
        let after =
            &self.text(self.line(b.saturating_sub(1)))[b - self.starts[self.line(b.saturating_sub(1))]..];
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
        self.alone(n) && self.hide(n.start_position().row, n.end_position().row, note, step)
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
        if is_logging(n, lang, src)
            && n.parent().is_some_and(|p| matches!(p.kind(), "block" | "statement_block"))
        {
            plan.hide_node(n, None, "log statements");
        } else if n.kind() == "comment" && is_region(&src[n.byte_range()]) {
            plan.hide_node(n, None, "regions");
        } else if n.kind() == "accessibility_modifier" && n.parent().is_some_and(|p| is_member(p.kind())) {
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

// True for a TypeScript class member that can carry an access modifier, as opposed to a constructor parameter.
fn is_member(kind: &str) -> bool {
    matches!(
        kind,
        "public_field_definition" | "method_definition" | "method_signature" | "abstract_method_signature"
    )
}

// True for a comment that only marks a region: `# region`, `//#region`, `// #endregion`.
fn is_region(comment: &str) -> bool {
    let c = comment.trim_start_matches(['#', '/']).trim_start().trim_start_matches('#').trim_start();
    let word = c.strip_prefix("end").unwrap_or(c);
    word.strip_prefix("region").is_some_and(|r| r.is_empty() || r.starts_with(char::is_whitespace))
}

// Collapses each run of top-level imports into one line.
fn imports(root: Node, lang: Lang, plan: &mut Plan) {
    let is_import = |k: &str| match lang {
        Lang::Python => matches!(k, "import_statement" | "import_from_statement" | "future_import_statement"),
        Lang::TypeScript | Lang::Tsx => k == "import_statement",
        Lang::Dart => matches!(k, "import_or_export" | "part_directive" | "part_of_directive"),
    };
    let kids: Vec<Node> = (0..root.child_count()).filter_map(|i| root.child(i)).collect();
    let mut i = 0;
    while i < kids.len() {
        let run = kids[i..].iter().take_while(|k| is_import(k.kind())).count();
        if run > 0 {
            let (first, last) = (kids[i], kids[i + run - 1]);
            let (a, b) = (first.start_position().row, last.end_position().row);
            if b > a && plan.alone(first) && plan.alone(last) {
                plan.hide(a, b, Some(format!("{run} import(s)")), "imports");
            }
        }
        i += run.max(1);
    }
}

// Returns a call's callee text, such as `self._log.debug` or `console.error`, for an expression statement that is only a call.
fn call_target<'t, 's>(stmt: Node<'t>, lang: Lang, src: &'s str) -> Option<(Node<'t>, &'s str)> {
    if stmt.kind() != "expression_statement" {
        return None;
    }
    let call = stmt.named_child(0)?;
    let callee = match (lang, call.kind()) {
        (Lang::Python, "call") | (Lang::TypeScript | Lang::Tsx, "call_expression") => {
            call.child_by_field_name("function")?
        }
        (Lang::Dart, "call_expression") => call.named_child(0)?,
        _ => return None,
    };
    Some((call, &src[callee.byte_range()]))
}

// True for a statement that only logs, with no side effects in its arguments.
fn is_logging(stmt: Node, lang: Lang, src: &str) -> bool {
    let Some((call, callee)) = call_target(stmt, lang, src) else { return false };
    if has_side_effects(call) {
        return false;
    }
    let Some((receiver, method)) = callee.rsplit_once('.') else {
        return lang == Lang::Dart && matches!(callee, "debugPrint" | "log");
    };
    let last = receiver.rsplit('.').next().unwrap_or(receiver).trim_end_matches(['?', '!']);
    if last == "console" {
        return lang != Lang::Python && lang != Lang::Dart && CONSOLE_METHODS.contains(&method);
    }
    let r = last.trim_start_matches('_').to_ascii_lowercase();
    let logger = matches!(r.as_str(), "log" | "logs" | "logging") || r.ends_with("logger");
    logger && (LOG_METHODS.contains(&method) || method.starts_with("log"))
}

// True when a node contains an assignment, await, yield, increment or decrement.
fn has_side_effects(n: Node) -> bool {
    let mut cursor = n.walk();
    let mut stack = vec![n];
    while let Some(x) = stack.pop() {
        if SIDE_EFFECTS.iter().any(|s| x.kind().contains(s)) {
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
        if n.end_position().row > n.start_position().row {
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
        (Lang::TypeScript | Lang::Tsx, "method_definition") => {
            let (name, params, body) = (field("name")?, field("parameters")?, field("body")?);
            let ok = text(name) == "constructor" && statements(body).iter().all(|s| ts_store(*s, src));
            ok.then(|| (text(name).to_string(), text(params)))
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

// Returns the named statements of a block, skipping comments.
fn statements(block: Node) -> Vec<Node> {
    kids(block).filter(|s| s.kind() != "comment").collect()
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
    )
}

// Collapses a catch clause whose body only logs and then rethrows what it caught.
fn catch(n: Node, lang: Lang, plan: &mut Plan) {
    let src = plan.src;
    let Some((start, block)) = rethrow_catch(n, lang, src) else { return };
    let header = src[start..block.start_byte()].trim().trim_end_matches(':').trim_end();
    let (first, last) = (plan.line(start), block.end_position().row);
    let open_line = plan.line(block.start_byte());
    let starts_line = src[plan.starts[first]..start].trim().is_empty();
    let block_ends_line = plan.text(last)[block.end_position().column..].trim().is_empty();
    if starts_line && block_ends_line {
        plan.hide(first, last, Some(format!("{header}: logs and rethrows")), "log-and-rethrow catches");
    } else if lang != Lang::Python
        && plan.text(open_line)[block.start_position().column + 1..].trim().is_empty()
        && plan.text(last).trim() == "}"
        && plan.hide(open_line + 1, last, None, "log-and-rethrow catches")
    {
        let end = plan.starts[open_line] + plan.text(open_line).len();
        plan.changes.push((block.start_byte(), end, "{ ⋯ logs and rethrows }".to_string()));
    }
}

// Returns where a catch clause starts and its block when the block only logs and then rethrows; None otherwise.
fn rethrow_catch<'t>(n: Node<'t>, lang: Lang, src: &str) -> Option<(usize, Node<'t>)> {
    let (start, block) = match (lang, n.kind()) {
        (Lang::Python, "except_clause") => (n.start_byte(), kids(n).find(|c| c.kind() == "block")?),
        (Lang::TypeScript | Lang::Tsx, "catch_clause") => (n.start_byte(), n.child_by_field_name("body")?),
        (Lang::Dart, "catch_clause") => {
            let on = n.prev_sibling().and_then(|t| t.prev_sibling()).filter(|t| t.kind() == "on");
            (on.unwrap_or(n).start_byte(), n.next_named_sibling().filter(|b| b.kind() == "block")?)
        }
        _ => return None,
    };
    let caught = n.child_by_field_name("parameter").map(|p| &src[p.byte_range()]);
    let last_ok = |l: &Node| match lang {
        Lang::Python => l.kind() == "raise_statement" && l.named_child_count() == 0,
        Lang::Dart => l.kind() == "rethrow_statement",
        _ => l.kind() == "throw_statement" && l.named_child(0).map(|e| &src[e.byte_range()]) == caught,
    };
    rethrows(block, &last_ok, lang, src).then_some((start, block))
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
        assert_eq!(lang_of(Path::new("b.js")), None);
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
}
