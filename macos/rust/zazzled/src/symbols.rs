// Symbol lookup for read --symbol: dotted names, #N for duplicates, and the doc comments and attributes above.
use crate::outline::{Lang, Symbol};
use crate::util::clip;

const MAX_CANDIDATES: usize = 10;
const SIGNATURE_SCAN: usize = 8;
const SIGNATURE_CLIP: usize = 120;

// Why a name did not resolve to one symbol, with what to show the caller.
#[derive(Debug, PartialEq)]
pub enum Miss {
    None(Vec<String>),
    Many(usize, Vec<String>),
    Index(usize),
}

// Splits a symbol name into path segments: `Trait for Type` is `Type`, `a::b` and `a.b` split, generics drop.
fn segments(name: &str) -> Vec<String> {
    let name = name.rsplit_once(" for ").map_or(name, |(_, ty)| ty);
    name.split("::")
        .flat_map(|p| p.split('.'))
        .map(|p| p.split('<').next().unwrap_or_default().trim().to_string())
        .filter(|p| !p.is_empty())
        .collect()
}

// Returns each symbol's qualified path: its containers' segments followed by its own.
fn paths(syms: &[Symbol]) -> Vec<Vec<String>> {
    let mut stack: Vec<(usize, Vec<String>)> = Vec::new();
    let mut out = Vec::with_capacity(syms.len());
    for s in syms {
        while stack.last().is_some_and(|(d, _)| *d >= s.depth) {
            stack.pop();
        }
        let mut path = stack.last().map(|(_, p)| p.clone()).unwrap_or_default();
        path.extend(segments(&s.name));
        stack.push((s.depth, path.clone()));
        out.push(path);
    }
    out
}

// Splits a query into its segments and an optional 1-based pick from `#N`.
fn parse_query(query: &str) -> (Vec<String>, Option<usize>) {
    let (name, pick) = match query.rsplit_once('#') {
        Some((n, k)) if k.parse::<usize>().is_ok() => (n, k.parse().ok()),
        _ => (query, None),
    };
    (segments(name), pick)
}

// True when the query segments are the last segments of the path, compared with the given equality.
fn ends_with(path: &[String], want: &[String], eq: impl Fn(&str, &str) -> bool) -> bool {
    path.len() >= want.len() && path[path.len() - want.len()..].iter().zip(want).all(|(a, b)| eq(a, b))
}

// True when a line declares the name: it holds the name as a whole word and is not an attribute or comment.
fn declares(l: &str, name: &str) -> bool {
    let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
    let t = l.trim_start();
    !t.starts_with(['[', '#', '@', '/', '*'])
        && l.match_indices(name)
            .any(|(i, _)| !word(l[..i].chars().next_back()) && !word(l[i + name.len()..].chars().next()))
}

// Returns the declaration line of a symbol: the first of its lines declaring it, trimmed and clipped.
fn signature(s: &Symbol, line: &dyn Fn(usize) -> String) -> String {
    let last = s.end.min(s.start + SIGNATURE_SCAN);
    let found = (s.start..=last).map(line).find(|l| declares(l, &s.name)).unwrap_or_else(|| line(s.start));
    clip(found.trim(), 0, SIGNATURE_CLIP)
}

// Describes a symbol for candidate lists: its lines, kind, qualified name and declaration line.
fn describe(s: &Symbol, path: &[String], line: &dyn Fn(usize) -> String) -> String {
    format!("{}-{} {} {}: {}", s.start, s.end, s.kind, path.join("."), signature(s, line))
}

// Finds the index of the one symbol a query names, exact case first, then any case; `line` gives a file line by number.
pub fn find(syms: &[Symbol], query: &str, line: &dyn Fn(usize) -> String) -> Result<usize, Miss> {
    let (want, pick) = parse_query(query);
    let all = paths(syms);
    let exact = |a: &str, b: &str| a == b;
    let loose = |a: &str, b: &str| a.eq_ignore_ascii_case(b);
    let mut hits: Vec<usize> = (0..syms.len()).filter(|&i| ends_with(&all[i], &want, exact)).collect();
    if hits.is_empty() {
        hits = (0..syms.len()).filter(|&i| ends_with(&all[i], &want, loose)).collect();
    }
    let list =
        |ids: &[usize]| ids.iter().take(MAX_CANDIDATES).map(|&i| describe(&syms[i], &all[i], line)).collect();
    match (hits.len(), pick) {
        (0, _) => {
            let last = want.last().map(|w| w.to_ascii_lowercase()).unwrap_or_default();
            let near: Vec<usize> =
                (0..syms.len()).filter(|&i| syms[i].name.to_ascii_lowercase().contains(&last)).collect();
            Err(Miss::None(list(&near)))
        }
        (1, None | Some(1)) => Ok(hits[0]),
        (n, Some(k)) if (1..=n).contains(&k) => Ok(hits[k - 1]),
        (n, Some(_)) => Err(Miss::Index(n)),
        (_, None) => {
            let numbered = hits
                .iter()
                .enumerate()
                .map(|(k, &i)| format!("#{} {}", k + 1, describe(&syms[i], &all[i], line)));
            Err(Miss::Many(hits.len(), numbered.take(MAX_CANDIDATES).collect()))
        }
    }
}

// Line prefixes that belong to the declaration below them: doc comments, comments, attributes, decorators.
fn lead_prefixes(lang: Lang) -> &'static [&'static str] {
    match lang {
        Lang::Rust => &["//", "/*", "*", "#["],
        Lang::Python => &["#", "@"],
        Lang::C => &["//", "/*", "*"],
        Lang::CSharp => &["//", "/*", "*", "["],
        Lang::JavaScript | Lang::TypeScript | Lang::Tsx => &["//", "/*", "*", "@"],
        _ => &["//", "/*", "*", "#[", "@"],
    }
}

// Returns the first line of the comments and attributes directly above a declaration (1-based), or its own start.
pub fn lead_start(lines: &[&str], start: usize, lang: Lang) -> usize {
    let prefixes = lead_prefixes(lang);
    let mut first = start;
    while first > 1 {
        let t = lines[first - 2].trim_start();
        if t.is_empty() || !prefixes.iter().any(|p| t.starts_with(p)) {
            break;
        }
        first -= 1;
    }
    first
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    // Builds a symbol.
    fn sym(kind: &'static str, name: &str, start: usize, end: usize, depth: usize) -> Symbol {
        Symbol { kind, name: name.to_string(), start, end, depth }
    }

    // A small outline: a struct, two impls with methods of the same name, a module and a nested function.
    fn outline() -> Vec<Symbol> {
        vec![
            sym("struct", "Invoice", 1, 3, 0),
            sym("impl", "Invoice", 5, 12, 0),
            sym("fn", "new", 6, 8, 1),
            sym("fn", "total", 9, 11, 1),
            sym("impl", "Display for Invoice<T>", 14, 18, 0),
            sym("fn", "fmt", 15, 17, 1),
            sym("mod", "billing::tax", 20, 30, 0),
            sym("fn", "total", 21, 29, 1),
        ]
    }

    // Stands in for file lines: line N reads `decl N`, so signatures fall back to the start line.
    fn line(n: usize) -> String {
        format!("  decl {n}  ")
    }

    // Finds with the stand-in lines.
    fn find(syms: &[Symbol], query: &str) -> Result<usize, Miss> {
        super::find(syms, query, &line)
    }

    // Converts string slices into owned segments.
    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    // Checks impl, path and generic names split into the segments a query uses.
    #[test]
    fn test_segments() {
        assert_eq!(segments("Display for Invoice<T>"), v(&["Invoice"]));
        assert_eq!(segments("billing::tax"), v(&["billing", "tax"]));
        assert_eq!(segments("Acme.Billing"), v(&["Acme", "Billing"]));
        assert_eq!(segments("Vec<u8>"), v(&["Vec"]));
        assert_eq!(segments("<>"), Vec::<String>::new());
    }

    // Checks qualified paths follow nesting and reset at shallower symbols.
    #[test]
    fn test_paths() {
        let p = paths(&outline());
        assert_eq!(p[3], v(&["Invoice", "total"]));
        assert_eq!(p[5], v(&["Invoice", "fmt"]));
        assert_eq!(p[7], v(&["billing", "tax", "total"]));
    }

    // Checks queries with dots, double colons and #N picks.
    #[test]
    fn test_parse_query() {
        assert_eq!(parse_query("Invoice.total"), (v(&["Invoice", "total"]), None));
        assert_eq!(parse_query("tax::total#2"), (v(&["tax", "total"]), Some(2)));
        assert_eq!(parse_query("a#b"), (v(&["a#b"]), None));
    }

    // Checks unique names, qualified names, any-case fallback and #N picks resolve.
    #[test]
    fn test_find_resolves() {
        let o = outline();
        assert_eq!(find(&o, "new"), Ok(2));
        assert_eq!(find(&o, "Invoice.total"), Ok(3));
        assert_eq!(find(&o, "tax::total"), Ok(7));
        assert_eq!(find(&o, "invoice.fmt"), Ok(5));
        assert_eq!(find(&o, "total#2"), Ok(7));
        assert_eq!(find(&o, "new#1"), Ok(2));
    }

    // Checks ambiguous names list numbered candidates, and misses list near names or say how many there are.
    #[test]
    fn test_find_misses() {
        let o = outline();
        assert_eq!(
            find(&o, "total"),
            Err(Miss::Many(
                2,
                v(&["#1 9-11 fn Invoice.total: decl 9", "#2 21-29 fn billing.tax.total: decl 21"])
            ))
        );
        assert_eq!(find(&o, "total#3"), Err(Miss::Index(2)));
        assert_eq!(find(&o, "new#2"), Err(Miss::Index(1)));
        assert_eq!(
            find(&o, "Tota"),
            Err(Miss::None(v(&["9-11 fn Invoice.total: decl 9", "21-29 fn billing.tax.total: decl 21"])))
        );
        assert_eq!(find(&o, "nothing"), Err(Miss::None(vec![])));
        let many: Vec<Symbol> = (0..20).map(|i| sym("fn", "f", i + 1, i + 1, 0)).collect();
        let Err(Miss::Many(total, list)) = find(&many, "f") else { panic!("expected many") };
        assert_eq!((total, list.len()), (20, MAX_CANDIDATES));
    }

    // Checks whole-word matching and that attribute and comment lines never count as declarations.
    #[test]
    fn test_declares() {
        assert!(declares("fn total() {", "total") && declares("total", "total"));
        assert!(!declares("fn totals() {", "total") && !declares("fn subtotal()", "total"));
        assert!(!declares("x Total2 Total_x", "Total"));
        assert!(!declares("#[doc = \"total\"]", "total") && !declares("@total", "total"));
    }

    // Checks the signature is the first line naming the symbol, clipped, and falls back to the start line.
    #[test]
    fn test_signature() {
        let s = sym("method", "Total", 3, 6, 1);
        let src = |n: usize| {
            ["", "", "[Obsolete(\"Total\")]", "  // Totals2 Total", "  public decimal Total(decimal r)"]
                [n - 1]
                .to_string()
        };
        assert_eq!(signature(&s, &src), "public decimal Total(decimal r)");
        let long = |_: usize| format!("fn Total({})", "x".repeat(300));
        assert_eq!(signature(&s, &long).len(), SIGNATURE_CLIP + "…".len());
        assert_eq!(signature(&s, &line), "decl 3");
    }

    // Checks doc comments and attributes above a declaration are included, stopping at blank lines and code.
    #[test]
    fn test_lead_start() {
        let cs = [
            "}",
            "",
            "/// <summary>Total.</summary>",
            "/// <param name=\"r\">Rate.</param>",
            "[Obsolete]",
            "public decimal Total(decimal r)",
        ];
        assert_eq!(lead_start(&cs, 6, Lang::CSharp), 3);
        let rs = ["use x;", "/// Docs.", "#[inline]", "fn f() {}"];
        assert_eq!(lead_start(&rs, 4, Lang::Rust), 2);
        let py = ["# about f", "@cache", "def f():"];
        assert_eq!(lead_start(&py, 3, Lang::Python), 1);
        assert_eq!(lead_start(&["#include <a.h>", "int f() {}"], 2, Lang::C), 2);
        assert_eq!(lead_start(&["/** doc */", "function f() {}"], 2, Lang::JavaScript), 1);
        assert_eq!(lead_start(&["@x", "func f()"], 2, Lang::Generic), 1);
        assert_eq!(lead_start(&["fn f() {}"], 1, Lang::Rust), 1);
    }
}
