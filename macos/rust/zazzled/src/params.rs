// Parameter lists for read --symbol: a query's `Name(T1, T2)` and a declaration's parameters, compared by type.

const MODIFIERS: [&str; 10] =
    ["ref", "out", "in", "params", "this", "scoped", "readonly", "final", "mut", "const"];
const RECEIVERS: [&str; 6] = ["self", "&self", "&mutself", "mutself", "cls", "this"];

// Splits text at top-level commas, ignoring commas inside (), <>, [] and {}, and a trailing comma.
pub fn split(s: &str) -> Vec<String> {
    let (mut parts, mut depth, mut cur) = (Vec::new(), 0i32, String::new());
    for c in s.chars() {
        match c {
            '(' | '<' | '[' | '{' => depth += 1,
            '>' if cur.ends_with(['=', '-']) => {}
            ')' | '>' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(std::mem::take(&mut cur).trim().to_string());
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    let last = cur.trim().to_string();
    if !last.is_empty() {
        parts.push(last);
    }
    parts
}

// Removes whitespace so `Dictionary<string, int>` and `Dictionary<string,int>` compare equal.
fn norm(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

// Splits a query like `Invoice.Total(decimal, int)` into the name and its parameter types, if any are given.
pub fn split_query(query: &str) -> (&str, Option<Vec<String>>) {
    match query.split_once('(') {
        Some((name, rest)) if rest.ends_with(')') => (name, Some(split(&rest[..rest.len() - 1]))),
        _ => (query, None),
    }
}

// Returns the parameter list that follows a name in declaration text: from the `(` after the name (generics allowed between) to its match.
pub fn declared(text: &str, name: &str) -> Option<Vec<String>> {
    let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
    for (i, _) in text.match_indices(name) {
        if word(text[..i].chars().next_back()) || word(text[i + name.len()..].chars().next()) {
            continue;
        }
        let mut rest = text[i + name.len()..].trim_start();
        if rest.starts_with('<') {
            let close = matching(rest, '<', '>')?;
            rest = rest[close + 1..].trim_start();
        }
        if rest.starts_with('(') {
            let close = matching(rest, '(', ')')?;
            return Some(split(&rest[1..close]));
        }
    }
    None
}

// Returns the index of the bracket that closes the one opening the text.
fn matching(text: &str, open: char, close: char) -> Option<usize> {
    let mut depth = 0;
    for (i, c) in text.char_indices() {
        if c == open {
            depth += 1;
        } else if c == close && !arrow(text, i) {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
    }
    None
}

// Returns a declared parameter's type and name, from `Type name` or `name: Type`, without default, attributes or modifiers.
pub fn parts(param: &str) -> (String, String) {
    let mut p = param.trim();
    while let Some(rest) = p.strip_prefix('[').or_else(|| p.strip_prefix("#[")) {
        p = rest.split_once(']').map_or("", |(_, r)| r).trim_start();
    }
    while p.starts_with('@') {
        p = p.split_once(char::is_whitespace).map_or("", |(_, r)| r).trim_start();
    }
    let p = split_top(p, '=').0;
    if let Some((name, ty)) = split_top_colon(p) {
        let name = name.trim().trim_start_matches("mut ").trim_end_matches('?');
        return (norm(ty), name.trim().to_string());
    }
    let words: Vec<&str> = p.split_whitespace().filter(|w| !MODIFIERS.contains(w)).collect();
    match words.as_slice() {
        [] => (String::new(), String::new()),
        [only] => (String::new(), only.to_string()),
        [ty @ .., name] => {
            let stars: String = name.chars().take_while(|c| matches!(c, '*' | '&')).collect();
            (norm(&format!("{}{stars}", ty.join(" "))), name.trim_start_matches(['*', '&']).to_string())
        }
    }
}

// True when the character at a byte offset is the `>` of an arrow (`=>` or `->`), not a closing bracket.
fn arrow(s: &str, i: usize) -> bool {
    s.as_bytes()[i] == b'>' && i > 0 && matches!(s.as_bytes()[i - 1], b'=' | b'-')
}

// Splits text at the first top-level occurrence of a character.
fn split_top(s: &str, at: char) -> (&str, &str) {
    let mut depth = 0i32;
    for (i, c) in s.char_indices() {
        match c {
            '(' | '<' | '[' | '{' => depth += 1,
            '>' if arrow(s, i) => {}
            ')' | '>' | ']' | '}' => depth -= 1,
            _ if c == at && depth == 0 && !s[i + 1..].starts_with('>') => return (&s[..i], &s[i + 1..]),
            _ => {}
        }
    }
    (s, "")
}

// Splits `name: Type` at a top-level single colon, leaving `a::b` paths alone.
fn split_top_colon(s: &str) -> Option<(&str, &str)> {
    let mut depth = 0i32;
    let bytes = s.as_bytes();
    for (i, c) in s.char_indices() {
        match c {
            '(' | '<' | '[' | '{' => depth += 1,
            '>' if arrow(s, i) => {}
            ')' | '>' | ']' | '}' => depth -= 1,
            ':' if depth == 0 && bytes.get(i + 1) != Some(&b':') && (i == 0 || bytes[i - 1] != b':') => {
                return Some((&s[..i], &s[i + 1..]));
            }
            _ => {}
        }
    }
    None
}

// True when the wanted parameter types match the declared parameters in order: by type, or by name for untyped ones; receivers are not counted.
pub fn matches(want: &[String], declared: &[String]) -> bool {
    let params: Vec<(String, String)> =
        declared.iter().filter(|p| !RECEIVERS.contains(&norm(p).as_str())).map(|p| parts(p)).collect();
    params.len() == want.len()
        && params.iter().zip(want).all(|((ty, name), w)| {
            let w = norm(w);
            if ty.is_empty() {
                *name == w
            } else {
                *ty == w
            }
        })
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    // Converts string slices into owned strings.
    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    // Checks top-level commas split and nested ones do not.
    #[test]
    fn test_split() {
        assert_eq!(
            split("int a, Dictionary<string, int> b, Func<(int, int)> f"),
            v(&["int a", "Dictionary<string, int> b", "Func<(int, int)> f"])
        );
        assert_eq!(split(""), Vec::<String>::new());
        assert_eq!(split(" "), Vec::<String>::new());
        assert_eq!(split("a,\n"), v(&["a"]));
        assert_eq!(split("Func<int> f = x => x, int y"), v(&["Func<int> f = x => x", "int y"]));
        assert_eq!(split("f: impl Fn(u8) -> u8, x: i32"), v(&["f: impl Fn(u8) -> u8", "x: i32"]));
    }

    // Checks queries with and without parameter lists.
    #[test]
    fn test_split_query() {
        assert_eq!(
            split_query("Invoice.Total(decimal, int)"),
            ("Invoice.Total", Some(v(&["decimal", "int"])))
        );
        assert_eq!(split_query("Total()"), ("Total", Some(vec![])));
        assert_eq!(split_query("Total"), ("Total", None));
        assert_eq!(split_query("Total(int"), ("Total(int", None));
    }

    // Checks the parameter list is found after the name, across lines and generics, and not after a longer name.
    #[test]
    fn test_declared() {
        assert_eq!(
            declared("public decimal Total(decimal rate, int digits) => 1;", "Total"),
            Some(v(&["decimal rate", "int digits"]))
        );
        assert_eq!(
            declared("fn total<T: Into<f64>>(\n    &self,\n    x: T,\n) -> f64 {", "total"),
            Some(v(&["&self", "x: T"]))
        );
        assert_eq!(declared("void Totals(int a)\nvoid Total()", "Total"), Some(vec![]));
        assert_eq!(declared("class Total {", "Total"), None);
        assert_eq!(declared("void Total(int a", "Total"), None);
        assert_eq!(declared("Total<int", "Total"), None);
        assert_eq!(declared("fn run<F: Fn() -> u8>(f: F, n: u8)", "run"), Some(v(&["f: F", "n: u8"])));
        assert_eq!(declared("x.subTotal(int a); Total(int b)", "Total"), Some(v(&["int b"])));
    }

    // Checks types and names come out of every style of declaration.
    #[test]
    fn test_parts() {
        let p = |s: &str| parts(s);
        assert_eq!(p("decimal rate = 1.0m"), ("decimal".into(), "rate".into()));
        assert_eq!(
            p("[NotNull] ref Dictionary<string, int> map"),
            ("Dictionary<string,int>".into(), "map".into())
        );
        assert_eq!(p("params object[] args"), ("object[]".into(), "args".into()));
        assert_eq!(p("const char *name"), ("char*".into(), "name".into()));
        assert_eq!(p("mut buf: &mut Vec<u8>"), ("&mutVec<u8>".into(), "buf".into()));
        assert_eq!(p("x?: number = 3"), ("number".into(), "x".into()));
        assert_eq!(p("path: std::path::Path"), ("std::path::Path".into(), "path".into()));
        assert_eq!(p("#[allow(unused)] n: usize"), ("usize".into(), "n".into()));
        assert_eq!(p("@NonNull final String s"), ("String".into(), "s".into()));
        assert_eq!(p("Func<int, bool> f = x => true"), ("Func<int,bool>".into(), "f".into()));
        assert_eq!(p("f: Box<dyn Fn(u8) -> u8>"), ("Box<dynFn(u8)->u8>".into(), "f".into()));
        assert_eq!(p("std::string s"), ("std::string".into(), "s".into()));
        assert_eq!(p("Func<a -> b> f"), ("Func<a->b>".into(), "f".into()));
        assert_eq!(p(":T"), ("T".into(), "".into()));
        assert_eq!(p("value"), ("".into(), "value".into()));
        assert_eq!(p(""), ("".into(), "".into()));
        assert_eq!(p("[unclosed"), ("".into(), "".into()));
        assert_eq!(p("@Only"), ("".into(), "".into()));
    }

    // Checks matching by type, by name for untyped parameters, and that receivers and counts are respected.
    #[test]
    fn test_matches() {
        let d = |s: &[&str]| v(s);
        assert!(matches(&v(&["decimal", "int"]), &d(&["decimal rate", "int digits"])));
        assert!(!matches(&v(&["decimal"]), &d(&["decimal rate", "int digits"])));
        assert!(!matches(&v(&["int", "decimal"]), &d(&["decimal rate", "int digits"])));
        assert!(matches(&v(&["T"]), &d(&["&self", "x: T"])));
        assert!(matches(&v(&["Dictionary<string,int>"]), &d(&["Dictionary<string, int> m"])));
        assert!(matches(&v(&["x"]), &d(&["self", "x"])));
        assert!(matches(&[], &d(&[])));
        assert!(!matches(&v(&["System.Decimal"]), &d(&["decimal rate"])));
    }
}
