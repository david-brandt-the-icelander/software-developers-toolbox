// Seeds for an investigation that do not resolve: real files and symbols inside the scope whose names resemble the missing name, so the model (or the caller) starts from something that exists.
use std::path::{Path, PathBuf};

const MAX_FILES: usize = 4000;
const MAX_BYTES: u64 = 512 * 1024;
const SHOW: usize = 8;

// Splits a name into lowercase words at case changes, digits and separators: `AuthorizeWashBy_sub` → authorize, wash, by, sub.
pub fn words(name: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut prev_lower = false;
    for c in name.chars() {
        if !c.is_alphanumeric() {
            out.extend((!cur.is_empty()).then(|| std::mem::take(&mut cur)));
            prev_lower = false;
            continue;
        }
        if c.is_uppercase() && prev_lower {
            out.push(std::mem::take(&mut cur));
        }
        prev_lower = c.is_lowercase() || c.is_ascii_digit();
        cur.extend(c.to_lowercase());
    }
    out.extend((!cur.is_empty()).then_some(cur));
    out
}

// Scores how much a name resembles the seed: containment either way scores highest, else the words they share (two at least); 0 for no resemblance or the seed itself.
pub fn score(seed: &str, name: &str) -> usize {
    let (s, n) = (seed.to_lowercase(), name.to_lowercase());
    if s == n || n.len() < 4 {
        return 0;
    }
    if n.contains(&s) || (s.contains(&n) && n.len() * 2 >= s.len()) {
        return 100;
    }
    let sw: Vec<String> = words(seed).into_iter().filter(|w| w.len() >= 3).collect();
    let nw = words(name);
    let shared = sw.iter().filter(|w| nw.contains(w)).count();
    if shared >= 2 {
        shared * 10
    } else {
        0
    }
}

// Tells whether a file is documentation or data rather than code, so a symbol's place in code is shown before a mention in docs.
fn doc(f: &Path) -> bool {
    let ext = f.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    matches!(ext.as_str(), "md" | "json" | "txt" | "yml" | "yaml" | "lock" | "csv" | "html")
}

// Lists up to eight files and eight symbols inside the scope that resemble a missing name, best first, as `file PATH` and `symbol NAME (PATH:LINE)` lines.
pub fn candidates(engine: &crate::engine::Engine, cwd: &Path, scope: &[PathBuf], name: &str) -> Vec<String> {
    let opts = crate::walk::WalkOpts { no_ignore: false, globs: Vec::new() };
    let Ok(mut files) = crate::walk::collect(scope, &opts, &engine.walks) else { return Vec::new() };
    files.sort_by_key(|f| doc(f));
    let rel = |p: &Path| p.strip_prefix(cwd).unwrap_or(p).display().to_string();
    let mut by_file: Vec<(usize, String)> = files
        .iter()
        .filter_map(|f| {
            let s = score(name, f.file_stem().and_then(|x| x.to_str()).unwrap_or(""));
            (s > 0).then(|| (s, format!("file {}", rel(f))))
        })
        .collect();
    by_file.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let ident = regex::Regex::new(r"[A-Za-z_][A-Za-z0-9_]{3,}").expect("valid regex");
    let mut syms: Vec<(usize, String, String)> = Vec::new();
    for f in files.iter().take(MAX_FILES).take_while(|_| !crate::budget::spent()) {
        if f.metadata().map_or(true, |m| m.len() > MAX_BYTES) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(f) else { continue };
        for (i, line) in text.lines().enumerate() {
            for m in ident.find_iter(line) {
                let s = score(name, m.as_str());
                if s > 0 && !syms.iter().any(|(_, n, _)| n == m.as_str()) {
                    syms.push((s, m.as_str().to_string(), format!("{}:{}", rel(f), i + 1)));
                }
            }
        }
    }
    syms.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let mut out: Vec<String> = by_file.into_iter().take(SHOW).map(|(_, l)| l).collect();
    out.extend(syms.into_iter().take(SHOW).map(|(_, n, at)| format!("symbol {n} ({at})")));
    out
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    // Checks names split into words and resemblance is scored by containment and shared words.
    #[test]
    fn test_seed_words_and_score() {
        assert_eq!(words("AuthorizeWashBy_subscription2"), ["authorize", "wash", "by", "subscription2"]);
        assert_eq!(words("HTTPServer"), ["httpserver"]);
        assert_eq!(score("AuthorizeWash", "AuthorizeWashBySubscription"), 100);
        assert_eq!(score("AuthorizeWashBySubscription", "authorize_wash"), 20);
        assert_eq!(score("AuthorizeWashBySubscription", "WashSubscriptionService"), 20);
        assert_eq!(score("AuthorizeWashBySubscription", "SubscriptionPlan"), 0);
        assert_eq!(score("Handle", "Handle"), 0);
        assert_eq!(score("Store", "store"), 0);
        assert_eq!(score("Store", "StoreRequest"), 100);
        assert_eq!(score("AuthorizeWashBySubscription", "Wash"), 0);
    }

    // Checks the candidates are real files and symbols inside the scope, files first, and nothing outside it.
    #[test]
    fn test_seed_candidates() {
        let t = TempDir::new();
        let root = t.path().canonicalize().expect("root");
        t.write(
            "api/WashSubscriptionService.cs",
            b"class WashSubscriptionService {\n  bool AuthorizeWash(int id) { return true; }\n}\n",
        );
        t.write("other/AuthorizeWashLegacy.cs", b"class AuthorizeWashLegacy {}\n");
        t.write("api/Big.cs", &vec![b'x'; 600 * 1024]);
        t.write("api/Bin.cs", &[0xff, 0xfe, b'A', b'u', b't', b'h']);
        t.write("api/Aaa.md", b"Docs mention AuthorizeWash first.\n");
        let e = crate::engine::Engine::new();
        let c = candidates(&e, &root, &[root.join("api")], "AuthorizeWashBySubscription");
        assert_eq!(
            c,
            [
                "file api/WashSubscriptionService.cs",
                "symbol AuthorizeWash (api/WashSubscriptionService.cs:2)",
                "symbol WashSubscriptionService (api/WashSubscriptionService.cs:1)"
            ]
        );
        assert!(candidates(&e, &root, &[root.join("api")], "Zebra").is_empty());
        assert!(candidates(&e, &root, &[root.join("none")], "x").is_empty());
    }
}
