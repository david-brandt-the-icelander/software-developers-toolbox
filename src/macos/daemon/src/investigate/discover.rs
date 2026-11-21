// Seedless discovery: before any model turn, zazzle turns the question into search terms and ranks the real declarations inside the scope (by name, by the route literals in their attributes and their class's, by path) as candidate entry points, production code before tests, simulators and fakes, so a question with no starting symbol starts from code that exists instead of guessed names.
use super::seed;
use crate::outline::{self, Lang};
use std::path::{Path, PathBuf};

const MAX_FILES: usize = 4000;
// How many candidates a listing shows.
pub const SHOW: usize = 8;
// Words of a question that say what kind of thing it asks about rather than naming it.
const STOP: &[&str] = &[
    "all",
    "and",
    "any",
    "are",
    "but",
    "can",
    "for",
    "from",
    "has",
    "how",
    "into",
    "its",
    "not",
    "the",
    "then",
    "this",
    "that",
    "through",
    "trace",
    "with",
    "what",
    "when",
    "where",
    "which",
    "who",
    "why",
    "does",
    "each",
    "facing",
    "flow",
    "handoff",
    "request",
    "requests",
    "endpoint",
    "endpoints",
    "handler",
    "http",
    "https",
    "layer",
    "stage",
    "stages",
    "step",
    "steps",
    "path",
    "code",
    "repository",
    "logic",
    "explain",
    "show",
    "should",
    "would",
    "could",
    "about",
    "after",
    "before",
    "between",
    "their",
    "them",
    "there",
    "they",
];
// Words that make a question about a web entry point, so declarations with route attributes rank first.
const WEB: &[&str] =
    &["endpoint", "endpoints", "http", "https", "route", "routes", "controller", "api", "rest", "handler"];
// What marks a route on or above a declaration; the first ones name an HTTP method, so the declaration handles requests itself.
const VERBS: &[&str] = &[
    "HttpGet",
    "HttpPost",
    "HttpPut",
    "HttpDelete",
    "HttpPatch",
    "MapGet(",
    "MapPost(",
    "MapPut(",
    "MapDelete(",
    "MapPatch(",
    "@app.route",
    "@router.",
    "@GetMapping",
    "@PostMapping",
    "@PutMapping",
    "@DeleteMapping",
];
const PREFIXES: &[&str] = &["Route(", "@RequestMapping"];
// Path words of code that is not the production code a question is about.
const SIDE: &[&str] = &[
    "test",
    "tests",
    "spec",
    "specs",
    "simulator",
    "simulators",
    "sim",
    "fake",
    "fakes",
    "mock",
    "mocks",
    "stub",
    "stubs",
    "fixture",
    "fixtures",
    "sample",
    "samples",
    "example",
    "examples",
    "bench",
    "benchmarks",
];

// One candidate declaration: where it is, what it is, how well it matches and why.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub path: String,
    pub kind: String,
    pub name: String,
    pub start: usize,
    pub end: usize,
    pub score: usize,
    pub why: Vec<String>,
}

impl Candidate {
    // The candidate as one listing line under its handle.
    pub fn line(&self, handle: &str) -> String {
        format!("{handle} {}", self.brief())
    }

    // Whether some of the terms match the candidate's name or path.
    pub fn touches(&self, terms: &[String]) -> bool {
        let words: Vec<String> = seed::words(&self.name).into_iter().chain(seed::words(&self.path)).collect();
        !shared(terms, &words).is_empty()
    }

    // Where the candidate is, what it is and why it matched.
    pub fn brief(&self) -> String {
        format!(
            "{}:{}-{} {} {} — {}",
            self.path,
            self.start,
            self.end,
            self.kind,
            self.name,
            self.why.join("; ")
        )
    }
}

// The search terms of a question: its lowercase words of three letters or more, split at case changes and separators, without the words that only say what kind of thing it asks about.
pub fn terms(question: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for w in
        question.split(|c: char| c.is_whitespace() || ",;:!?()[]{}'\"`".contains(c)).flat_map(seed::words)
    {
        if w.len() >= 3 && !STOP.contains(&w.as_str()) && !out.contains(&w) {
            out.push(w);
        }
    }
    out
}

// Whether a question asks about a web entry point.
pub fn web(question: &str) -> bool {
    question.split(|c: char| !c.is_alphanumeric()).any(|w| WEB.contains(&w.to_lowercase().as_str()))
}

// The string literals of some lines.
fn literals(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find('"') {
        let after = &rest[i + 1..];
        let Some(j) = after.find('"') else { break };
        out.push(after[..j].to_string());
        rest = &after[j + 1..];
    }
    out
}

// The route marks and literals in the attribute lines above and on a declaration (1-based lines).
fn route(lines: &[&str], lead: usize, start: usize) -> Option<String> {
    let text: Vec<&str> =
        lines[lead.saturating_sub(1)..start.min(lines.len())].iter().map(|l| l.trim()).collect();
    let marked: Vec<&str> =
        text.iter().copied().filter(|l| VERBS.iter().chain(PREFIXES).any(|m| l.contains(m))).collect();
    (!marked.is_empty()).then(|| marked.join(" "))
}

// Whether a route names an HTTP method.
fn verb(route: &str) -> bool {
    VERBS.iter().any(|m| route.contains(m))
}

// A word without a plural ending, so `washes` matches `wash` and `subscriptions` matches `subscription`.
pub(crate) fn stem(w: &str) -> String {
    match w.len() > 4 {
        true if w.ends_with("ies") => format!("{}y", &w[..w.len() - 3]),
        true if w.ends_with("shes") || w.ends_with("ches") || w.ends_with("sses") || w.ends_with("xes") => {
            w[..w.len() - 2].to_string()
        }
        true if w.ends_with('s') && !w.ends_with("ss") => w[..w.len() - 1].to_string(),
        _ => w.to_string(),
    }
}

// The terms found among some words.
pub(crate) fn shared(terms: &[String], words: &[String]) -> Vec<String> {
    let stems: Vec<String> = words.iter().map(|w| stem(w)).collect();
    terms.iter().filter(|t| stems.contains(&stem(t))).cloned().collect()
}

// Whether terms ask about tests, so test code is not ranked below production code.
pub(crate) fn wants_tests(terms: &[String]) -> bool {
    terms.iter().any(|t| matches!(t.as_str(), "test" | "tests" | "tested" | "testing" | "spec" | "specs"))
}

// Ranks the declarations inside the scope against the terms (a web question favours those with routes; tests rank with production code when the terms ask for tests), best first, at most `limit`; scanned once per job for the same scope and terms whatever the limit (a scan the job's stop cut short is not kept).
pub fn discover(
    engine: &crate::engine::Engine,
    cwd: &Path,
    scope: &[PathBuf],
    terms: &[String],
    web: bool,
    limit: usize,
) -> Vec<Candidate> {
    if terms.is_empty() {
        return Vec::new();
    }
    let run = || {
        let found = scan(engine, cwd, scope, terms, web);
        (found, !crate::budget::spent())
    };
    let mut found = match crate::budget::shared() {
        None => run().0,
        Some(shared) => {
            let key = format!("{}\0{scope:?}\0{terms:?}\0{web}", cwd.display());
            shared.decls.get_or(&key, run).map(|(found, _)| found).unwrap_or_default()
        }
    };
    found.truncate(limit);
    found
}

// The scan behind `discover`: every code file in the scope outlined, its declarations scored, best first; files not yet read when the job is stopped are skipped.
fn scan(
    engine: &crate::engine::Engine,
    cwd: &Path,
    scope: &[PathBuf],
    terms: &[String],
    web: bool,
) -> Vec<Candidate> {
    let opts = crate::walk::WalkOpts { no_ignore: false, globs: Vec::new() };
    let Ok(files) = crate::walk::collect(scope, &opts, &engine.walks) else { return Vec::new() };
    let code: Vec<PathBuf> = files
        .into_iter()
        .filter(|f| {
            !matches!(outline::lang_of(f), Lang::Markdown | Lang::Json | Lang::Toml | Lang::Generic)
                && outline::worth_outlining(f)
        })
        .take(MAX_FILES)
        .collect();
    let rel = |p: &Path| p.strip_prefix(cwd).unwrap_or(p).display().to_string();
    let job = crate::budget::current();
    let tests = wants_tests(terms);
    let per_file = crate::pool::par_map(&code, crate::pool::cpus(), |_, f| {
        if job.spent() {
            return Vec::new();
        }
        let Ok(loaded) = engine.load(f) else { return Vec::new() };
        if loaded.data.len() > outline::MAX_PARSE_BYTES {
            return Vec::new();
        }
        let text = String::from_utf8_lossy(&loaded.data).to_string();
        let lines: Vec<&str> = text.lines().collect();
        let symbols = engine.outline(f, &loaded);
        let path = rel(f);
        let path_words = seed::words(&path);
        let side = path_words.iter().any(|w| SIDE.contains(&w.as_str())) && !tests;
        let in_path: Vec<String> =
            shared(terms, &path_words).into_iter().filter(|t| !SIDE.contains(&t.as_str())).collect();
        let lang = outline::lang_of(f);
        let mut out = Vec::new();
        for s in symbols.iter() {
            let lead = crate::symbols::lead_start(&lines, s.start, lang);
            let mut own = route(&lines, lead, s.start);
            let outer = symbols.iter().rfind(|o| o.depth < s.depth && o.start <= s.start && o.end >= s.end);
            if let (Some(o), Some(_)) = (outer, &own) {
                let olead = crate::symbols::lead_start(&lines, o.start, lang);
                if let Some(prefix) = route(&lines, olead, o.start) {
                    own = own.map(|r| format!("{prefix} {r}"));
                }
            }
            let named = shared(terms, &seed::words(&s.name));
            let lower = s.name.to_lowercase();
            let joined = terms
                .windows(2)
                .filter(|w| lower.contains(&format!("{}{}", stem(&w[0]), stem(&w[1]))))
                .count();
            let routed = own.as_ref().map_or(Vec::new(), |r| {
                let words: Vec<String> = literals(r).iter().flat_map(|l| seed::words(l)).collect();
                shared(terms, &words)
            });
            if named.is_empty() && routed.is_empty() {
                continue;
            }
            let mut score = 3 * named.len() + 4 * joined + 3 * routed.len() + in_path.len();
            let mut why = Vec::new();
            if !named.is_empty() {
                why.push(format!("name matches {}", named.join(", ")));
            }
            if let Some(r) = &own {
                score += match (web, verb(r)) {
                    (true, true) => 8,
                    (true, false) => 2,
                    (false, _) => 1,
                };
                why.push(format!("route {r}"));
            }
            if !in_path.is_empty() {
                why.push(format!("path matches {}", in_path.join(", ")));
            }
            if side {
                score /= 3;
                why.push("a test, simulator or fake path".into());
            }
            if score >= 3 {
                out.push(Candidate {
                    path: path.clone(),
                    kind: s.kind.to_string(),
                    name: s.name.clone(),
                    start: lead,
                    end: s.end,
                    score,
                    why,
                });
            }
        }
        out
    });
    let mut all: Vec<Candidate> = per_file.into_iter().flatten().collect();
    all.sort_by(|a, b| b.score.cmp(&a.score).then(a.path.cmp(&b.path)).then(a.start.cmp(&b.start)));
    all
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    // Checks a declaration found only by its route (in a class with no route of its own) is listed with why, and a file too big to parse is skipped.
    #[test]
    fn test_route_only_and_big_files() {
        let t = crate::testutil::TempDir::new();
        let root = t.path().canonicalize().expect("root");
        t.write(
            "Api/Probe.cs",
            b"public class Health\n{\n    [HttpGet(\"queue/status\")]\n    public void Ping() { }\n}\n",
        );
        t.write(
            "Api/Big.cs",
            format!("// queue status\n{}", "x".repeat(outline::MAX_PARSE_BYTES + 10)).as_bytes(),
        );
        let e = crate::engine::Engine::new();
        let found = discover(
            &e,
            &root,
            std::slice::from_ref(&root),
            &terms("what serves the queue status endpoint?"),
            true,
            SHOW,
        );
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!((found[0].name.as_str(), found[0].start), ("Ping", 3));
        assert_eq!(found[0].why, ["route [HttpGet(\"queue/status\")]"]);
    }

    // Checks the terms leave out the words that only say what kind of thing a question asks about, and a web question is told apart.
    #[test]
    fn test_terms_and_web() {
        assert_eq!(
            terms(
                "Trace a subscription wash request from its Django-facing HTTP endpoint through eligibility"
            ),
            ["subscription", "wash", "django", "eligibility"]
        );
        assert!(web("its HTTP endpoint") && !web("how is the cache keyed?"));
        assert_eq!(literals(r#"[HttpPost("{id}/wash")] [Route("api/subs")]"#), ["{id}/wash", "api/subs"]);
        assert_eq!(
            ["washes", "subscriptions", "policies", "status", "boxes", "wash"].map(stem),
            ["wash", "subscription", "policy", "statu", "box", "wash"]
        );
        assert!(verb("[HttpPost(\"x\")]") && !verb("[Route(\"api\")]"));
        assert_eq!(literals("[HttpPost(\"open"), Vec::<String>::new(), "an unclosed quote names nothing");
        assert_eq!(["class", "classes", "matches", "bus"].map(stem), ["class", "class", "match", "bus"]);
        let e = crate::engine::Engine::new();
        let here = std::path::Path::new(".");
        assert!(discover(&e, here, &[here.join("no-such-folder")], &["wash".into()], true, SHOW).is_empty());
        assert!(
            discover(&e, here, &[here.to_path_buf()], &[], true, SHOW).is_empty(),
            "no terms, no candidates"
        );
    }
}
