// The outline operation: regex-based symbol lists per source file, so agents skip reading whole files.
use crate::fsio::{self, Ident};
use crate::req::OutlineArgs;
use crate::state::{Dep, Res, State};
use crate::walk;
use regex::Regex;
use serde_json::{json, Value};

const MAX_SYMBOLS: usize = 300;

// Return the line patterns for a file extension; each has `kind` and `name` groups.
fn patterns(ext: &str) -> &'static [&'static str] {
    match ext {
        "rs" => &[
            r#"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:(?:async|const|unsafe|default)\s+)*(?:extern\s+"[^"]*"\s+)?(?P<kind>fn|struct|enum|trait|mod|type|static|const|union)\s+(?P<name>[A-Za-z_]\w*)"#,
            r"^\s*(?:unsafe\s+)?(?P<kind>impl)(?:<[^>]*>)?\s+(?P<name>[^{]+?)\s*(?:where\b.*|\{.*)?$",
            r"^\s*(?P<kind>macro_rules)!\s+(?P<name>\w+)",
        ],
        "py" => &[r"^\s*(?:async\s+)?(?P<kind>def|class)\s+(?P<name>\w+)"],
        "js" | "jsx" | "mjs" | "ts" | "tsx" => &[
            r"^\s*(?:export\s+)?(?:default\s+)?(?:declare\s+)?(?:abstract\s+)?(?:async\s+)?(?P<kind>function|class|interface|type|enum)\*?\s+(?P<name>\w+)",
            r"^\s*(?:export\s+)?(?P<kind>const|let)\s+(?P<name>\w+)\s*(?::[^=]+)?=\s*(?:async\s*)?(?:\([^)]*\)|\w+)\s*(?::[^=]+)?=>",
        ],
        "go" => &[r"^(?P<kind>func|type|var|const)\s+(?:\([^)]*\)\s*)?(?P<name>\w+)"],
        "java" | "kt" | "cs" | "swift" | "scala" => &[
            r"^\s*(?:(?:public|private|protected|internal|static|final|abstract|sealed|open|data|partial)\s+)*(?P<kind>class|interface|enum|record|object|struct|trait|fun|func)\s+(?P<name>\w+)",
        ],
        "c" | "h" | "cc" | "cpp" | "hpp" => &[
            r"^\s*(?P<kind>struct|class|enum|union|namespace)\s+(?P<name>\w+)",
            r"^(?:[A-Za-z_][\w:<>*&]*\s+)+[*&]*(?P<name>[A-Za-z_]\w*)\s*\([^;)]*\)?\s*\{?\s*$",
        ],
        "sh" | "bash" => &[r"^(?:function\s+)?(?P<name>\w+)\s*\(\)\s*\{"],
        "md" => &[r"^(?P<kind>#{1,6})\s+(?P<name>.+?)\s*$"],
        _ => &[],
    }
}

// Return the kind label for a match, defaulting to "fn" and naming markdown headings by level.
fn kind_of(ext: &str, caps: &regex::Captures) -> String {
    match caps.name("kind").map(|m| m.as_str()) {
        Some(k) if ext == "md" => format!("h{}", k.len()),
        Some(k) => k.to_string(),
        None => "fn".to_string(),
    }
}

// Extract [line, kind, name] triples from text using compiled patterns.
fn symbols(text: &str, ext: &str, res: &[Regex]) -> Vec<Value> {
    let mut out = vec![];
    for (i, line) in text.lines().enumerate() {
        let Some(caps) = res.iter().find_map(|re| re.captures(line)) else { continue };
        out.push(json!([i + 1, kind_of(ext, &caps), caps["name"].trim()]));
    }
    out
}

// Walk the roots and outline every supported file.
fn compute(a: &OutlineArgs) -> Res<(Value, Vec<Dep>)> {
    let walker = walk::builder(&a.paths, &[], false, false, None)?;
    let cap = a.max_symbols.unwrap_or(MAX_SYMBOLS);
    let (mut deps, mut files, mut total, mut left) = (vec![], vec![], 0usize, cap);
    for entry in walker.build().flatten() {
        let Ok(md) = entry.metadata() else { continue };
        deps.push((entry.path().to_path_buf(), Some(Ident::of(&md))));
        if md.is_dir() {
            deps.extend(walk::ignore_file_deps(entry.path()));
        }
        let ext = entry.path().extension().and_then(|e| e.to_str()).unwrap_or("");
        if !md.is_file() || patterns(ext).is_empty() {
            continue;
        }
        let res: Vec<Regex> = patterns(ext).iter().map(|p| Regex::new(p).expect("static pattern")).collect();
        let Ok((_, bytes)) = fsio::load(entry.path()) else { continue };
        if fsio::is_binary(&bytes) {
            continue;
        }
        let mut syms = symbols(&String::from_utf8_lossy(&bytes), ext, &res);
        total += syms.len();
        syms.truncate(left);
        left -= syms.len();
        if !syms.is_empty() {
            files.push(json!({"path": walk::display(entry.path()), "symbols": syms}));
        }
    }
    files.sort_by(|x, y| x["path"].as_str().cmp(&y["path"].as_str()));
    let mut value = json!({"files": files, "total_symbols": total});
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

    // Compile the patterns for an extension.
    fn compiled(ext: &str) -> Vec<Regex> {
        patterns(ext).iter().map(|p| Regex::new(p).unwrap()).collect()
    }

    // Rust items, impls and macros are found with kinds and names.
    #[test]
    fn test_symbols_rust() {
        let src = "pub struct A;\nimpl<T> Foo for A<T> {\n    pub async fn go() {}\n}\nconst X: u8 = 1;\nmacro_rules! m { () => {} }\n";
        let got: Vec<String> = symbols(src, "rs", &compiled("rs")).iter().map(|v| v.to_string()).collect();
        assert_eq!(got[0], r#"[1,"struct","A"]"#);
        assert_eq!(got[1], r#"[2,"impl","Foo for A<T>"]"#);
        assert_eq!(got[2], r#"[3,"fn","go"]"#);
        assert_eq!(got[3], r#"[5,"const","X"]"#);
        assert_eq!(got[4], r#"[6,"macro_rules","m"]"#);
    }

    // Python defs, JS arrow functions and markdown headings are recognised.
    #[test]
    fn test_symbols_other_languages() {
        assert_eq!(symbols("class A:\n  def f(self):\n", "py", &compiled("py")).len(), 2);
        let js = symbols("export const f = async (x) => 1;\n", "js", &compiled("js"));
        assert_eq!(js[0].to_string(), r#"[1,"const","f"]"#);
        let md = symbols("# Title\ntext\n## Sub\n", "md", &compiled("md"));
        assert_eq!(md[1].to_string(), r#"[3,"h2","Sub"]"#);
    }

    // Unknown extensions have no patterns.
    #[test]
    fn test_patterns_unknown_extension() {
        assert!(patterns("xyz").is_empty());
        assert!(!patterns("rs").is_empty());
    }
}
