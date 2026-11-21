// Whether a result covers what its items ask for, checked by zazzle from the files: each item needs a verified finding in the code that matches it best (never a document, never an unrelated file), and the evidence its kind of question needs: tests from test code, callers from other production files that use the type, a trace followed through each call on a field to its implementation, a route with its controller's prefix; what is missing becomes an unresolved item with the lookups that would settle it.
use super::items::Task;
use crate::cmd::Ctx;
use crate::investigate::discover::{self, Candidate};
use crate::investigate::Found;
use regex::Regex;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

// The files discovery ranks best for an item (each scoring at least half the best) that a finding may sit in to be about it.
const HOMES: usize = 3;
// The most lookups a check hands the next attempt, and how many calls deep it follows a trace before that attempt.
const FOLLOW: usize = 6;
const DEPTH: usize = 2;
// How many candidates a lookup of one method name considers.
const NAMED: usize = 64;

// A call on a field (`_service.Start(` or `this.store.Save(`): the receiver and the method.
static FIELD_CALL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(_[A-Za-z]\w*|this\.[A-Za-z]\w*)\s*\.\s*([A-Z]\w*)\s*(?:<[^>()]*>)?\(")
        .expect("valid regex")
});
// A type declaration with its base list: `class Name<T> : IBase, Other`.
static TYPE_DECL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(class|interface|record|struct)\s+([A-Z]\w*)(?:\s*<[^>{]*>)?(?:\s*\([^){]*\))?(?:\s*:\s*([^{\n]+))?")
        .expect("valid regex")
});
// A class-level route attribute.
static ROUTE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"\[Route\("[^"]*"\)\]"#).expect("valid regex"));
// A class declaration.
static CLASS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\bclass\s+[A-Z]").expect("valid regex"));
// A route or HTTP method attribute on a line.
static ROUTED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[(?:Route|Http[A-Z][a-z]+)\b").expect("valid regex"));

// Receivers whose calls are plumbing (logging, metrics, mapping, time, options), not a step a trace must follow.
const PLUMBING: &[&str] =
    &["log", "metric", "telemetry", "mapper", "clock", "time", "option", "config", "tracer"];
// Words before a name that make `name(` a call rather than a declaration.
const CALLING: &[&str] = &["await", "return", "new", "throw", "if", "while", "nameof", "using", "yield"];

// One item's coverage: its id and words, the citations of the findings about it, and what it still lacks.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Cover {
    pub id: String,
    pub text: String,
    pub findings: Vec<String>,
    pub missing: Vec<String>,
}

// A result's coverage: each item's, the citations about none of them, and the lookups that would settle what is missing.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Coverage {
    pub items: Vec<Cover>,
    pub unrelated: Vec<String>,
    pub follow: Vec<String>,
}

// What kinds of evidence an item's words ask for.
#[derive(Debug, Default, PartialEq)]
struct Needs {
    tests: bool,
    callers: bool,
    trace: bool,
    route: bool,
}

// Reads what an item asks for from its words.
fn needs(text: &str, terms: &[String]) -> Needs {
    let l = format!(" {} ", text.to_lowercase());
    let any = |ws: &[&str]| ws.iter().any(|w| l.contains(w));
    Needs {
        tests: discover::wants_tests(terms),
        callers: any(&[
            "caller",
            "called from",
            "called by",
            "call site",
            "used by",
            "usages",
            "who calls",
            "code calls",
            "what calls",
            "which calls",
        ]),
        trace: any(&[
            "trace",
            "call chain",
            " flow",
            "end to end",
            "end-to-end",
            "downstream",
            " through ",
            "follow",
        ]),
        route: any(&["route", "endpoint", " url", " uri"]),
    }
}

// How many of the terms some words name: the same stem, or one word starting the other when the shorter has four letters or more (`store` and `stored`, `cache` and `cached`).
fn named_terms(terms: &[String], words: &[String]) -> usize {
    let stems: Vec<String> = words.iter().map(|w| discover::stem(w)).collect();
    terms
        .iter()
        .map(|t| discover::stem(t))
        .filter(|t| {
            stems.iter().any(|w| {
                w == t
                    || (w.len().min(t.len()) >= 4 && (w.starts_with(t.as_str()) || t.starts_with(w.as_str())))
            })
        })
        .count()
}

// Whether some words name one of the terms.
fn shares(terms: &[String], words: &[String]) -> bool {
    named_terms(terms, words) > 0
}

// Whether a path is a document or data rather than code, which cannot establish what code does.
fn doc(path: &str) -> bool {
    let ext = Path::new(path).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    matches!(ext.as_str(), "md" | "txt" | "rst" | "json" | "yml" | "yaml" | "toml" | "csv" | "html" | "xml")
}

// Whether a path is test code.
fn test_path(path: &str) -> bool {
    crate::investigate::seed::words(path)
        .iter()
        .any(|w| matches!(w.as_str(), "test" | "tests" | "spec" | "specs"))
}

// The code lines of an excerpt with their numbers (`  12\tcode`).
fn lines(excerpt: &str) -> Vec<(usize, &str)> {
    excerpt
        .lines()
        .filter_map(|l| l.split_once('\t'))
        .filter_map(|(n, code)| n.trim().parse().ok().map(|n| (n, code)))
        .collect()
}

// A file's text, empty when it cannot be read.
fn text(cwd: &Path, path: &str) -> String {
    std::fs::read_to_string(cwd.join(path)).unwrap_or_default()
}

// Whether a line declares the method `m` (not calls it): Some(true) for a body, Some(false) for an interface or abstract member that ends with `;`.
fn declares(line: &str, m: &str) -> Option<bool> {
    let found = line.match_indices(m).any(|(i, _)| {
        let after = line[i + m.len()..].trim_start();
        let before = line[..i].trim_end();
        let word_start = !line[..i].ends_with(|c: char| c.is_alphanumeric() || c == '_' || c == '.');
        let last = before.rsplit(|c: char| c.is_whitespace()).next().unwrap_or("");
        word_start
            && (after.starts_with('(') || after.starts_with('<'))
            && before.ends_with(|c: char| c.is_alphanumeric() || matches!(c, '>' | ']' | '?'))
            && !CALLING.contains(&last)
    });
    found.then(|| !line.trim_end().ends_with(';'))
}

// The calls on fields in some lines, without plumbing, as (line, receiver, method).
fn field_calls(ls: &[(usize, &str)]) -> Vec<(usize, String, String)> {
    let mut out: Vec<(usize, String, String)> = Vec::new();
    for (n, l) in ls {
        for c in FIELD_CALL.captures_iter(l) {
            let (recv, m) = (c[1].to_string(), c[2].to_string());
            let lower = recv.to_lowercase();
            if !PLUMBING.iter().any(|p| lower.contains(p)) && !out.iter().any(|(_, _, x)| *x == m) {
                out.push((*n, recv, m));
            }
        }
    }
    out
}

// The types a file declares, each with whether it is an interface and the interfaces it lists as bases.
fn types(text: &str) -> Vec<(String, bool, Vec<String>)> {
    TYPE_DECL
        .captures_iter(text)
        .map(|c| {
            let bases = c.get(3).map_or(Vec::new(), |b| {
                b.as_str()
                    .split(',')
                    .map(|x| x.trim().split(['<', ' ']).next().unwrap_or("").to_string())
                    .filter(|x| x.len() > 1 && x.starts_with('I') && x[1..].starts_with(char::is_uppercase))
                    .collect()
            });
            (c[2].to_string(), &c[1] == "interface", bases)
        })
        .collect()
}

// The class-level `[Route("..")]` attribute of a file, with its line: the one in the attribute lines just above a class declaration.
fn class_route(text: &str) -> Option<(usize, String)> {
    let ls: Vec<&str> = text.lines().collect();
    for (i, l) in ls.iter().enumerate() {
        if !CLASS.is_match(l) {
            continue;
        }
        let mut j = i;
        while j > 0 && ls[j - 1].trim_start().starts_with('[') {
            j -= 1;
            if let Some(m) = ROUTE.find(ls[j]) {
                return Some((j + 1, m.as_str().to_string()));
            }
        }
    }
    None
}

// The declarations named exactly `m` in production code inside the scope outside the file `from` that calls it: those with a body, else the bodiless ones (an interface's).
fn named(ctx: &Ctx, scope: &[PathBuf], m: &str, from: &str) -> Vec<(Candidate, bool)> {
    let terms = discover::terms(m);
    let mut out: Vec<(Candidate, bool)> =
        discover::discover(ctx.engine, &ctx.cwd, scope, &terms, false, NAMED)
            .into_iter()
            .filter(|c| c.name == m && !test_path(&c.path) && c.path != from)
            .map(|c| {
                let t = text(&ctx.cwd, &c.path);
                let body = t
                    .lines()
                    .skip(c.start.saturating_sub(1))
                    .take(c.end + 1 - c.start)
                    .find_map(|l| declares(l, m));
                (c, body.unwrap_or(false))
            })
            .collect();
    if out.iter().any(|(_, body)| *body) {
        out.retain(|(_, body)| *body);
    }
    out
}

// The lookups that follow a call to `m` from the file `from` to its implementation and, up to `depth` calls deep, the calls that implementation makes on its fields.
fn follow_call(ctx: &Ctx, scope: &[PathBuf], m: &str, from: &str, depth: usize, out: &mut Vec<String>) {
    let found = named(ctx, scope, m, from);
    for (c, body) in found.iter().take(2) {
        let read = format!("READ {}:{}-{}", c.path, c.start, c.end);
        if out.contains(&read) {
            continue;
        }
        out.push(read);
        if *body && depth > 1 {
            let t = text(&ctx.cwd, &c.path);
            let ls: Vec<(usize, &str)> = t
                .lines()
                .enumerate()
                .skip(c.start.saturating_sub(1))
                .take(c.end + 1 - c.start)
                .map(|(i, l)| (i + 1, l))
                .collect();
            for (_, _, next) in field_calls(&ls) {
                follow_call(ctx, scope, &next, &c.path, depth - 1, out);
            }
        }
    }
}

// Checks each of the items `ids` of the task against the findings (a finding outside the item's best files is about it only when it names two of its terms when it has four or more, so one generic word such as `read` is not enough), with the files and declarations inside the scope.
pub fn check(ctx: &Ctx, scope: &[PathBuf], task: &Task, ids: &[String], findings: &[Found]) -> Coverage {
    let context = discover::terms(&task.context.join(" "));
    let code: Vec<&Found> = findings.iter().filter(|f| !doc(&f.path)).collect();
    let words = |f: &Found| -> Vec<String> {
        crate::investigate::seed::words(&f.path)
            .into_iter()
            .chain(crate::investigate::seed::words(&f.excerpt))
            .collect()
    };
    let mut cov = Coverage::default();
    let mut used: Vec<String> = Vec::new();
    for item in task.items.iter().filter(|i| ids.contains(&i.id)) {
        let mut terms = discover::terms(&item.text);
        terms.extend(context.iter().filter(|t| !terms.contains(t)).cloned().collect::<Vec<_>>());
        let need = needs(&item.text, &terms);
        let ranked = discover::discover(
            ctx.engine,
            &ctx.cwd,
            scope,
            &terms,
            discover::web(&item.text),
            discover::SHOW,
        );
        let mut homes: Vec<&Candidate> = Vec::new();
        let best = ranked.first().map_or(0, |c| c.score);
        for c in ranked.iter().filter(|c| c.score * 2 >= best) {
            if homes.len() < HOMES && !homes.iter().any(|h| h.path == c.path) {
                homes.push(c);
            }
        }
        let about: Vec<&Found> =
            code.iter().copied().filter(|f| homes.iter().any(|h| h.path == f.path)).collect();
        let relevant: Vec<&Found> = code
            .iter()
            .copied()
            .filter(|f| {
                about.contains(f) || named_terms(&terms, &words(f)) >= if terms.len() >= 4 { 2 } else { 1 }
            })
            .collect();
        let mut c = Cover {
            id: item.id.clone(),
            text: item.text.clone(),
            findings: relevant.iter().map(|f| f.cite.clone()).collect(),
            missing: Vec::new(),
        };
        let mut follow: Vec<String> = Vec::new();
        match (homes.first(), about.is_empty(), relevant.is_empty()) {
            (Some(h), true, _) => {
                c.missing.push(format!(
                    "no verified finding in the code that matches it best ({})",
                    homes.iter().map(|h| h.path.as_str()).collect::<Vec<_>>().join(", ")
                ));
                follow.push(format!("READ {}:{}-{}", h.path, h.start, h.end));
            }
            (None, _, true) => c.missing.push("no verified finding in code about it".into()),
            _ => {}
        }
        let subjects: Vec<(String, bool, Vec<String>)> = homes
            .iter()
            .filter(|h| !test_path(&h.path))
            .flat_map(|h| types(&text(&ctx.cwd, &h.path)))
            .filter(|(name, ..)| shares(&terms, &crate::investigate::seed::words(name)))
            .collect();
        let names: Vec<String> = subjects
            .iter()
            .flat_map(|(n, _, bases)| std::iter::once(n.clone()).chain(bases.iter().cloned()))
            .collect();
        let declared_in = |path: &str| types(&text(&ctx.cwd, path)).iter().any(|(n, ..)| names.contains(n));
        let uses = |path: &str| {
            let t = text(&ctx.cwd, path);
            names.iter().any(|n| t.contains(n.as_str()))
        };
        if need.tests
            && !relevant.iter().any(|f| test_path(&f.path))
            && !code.iter().any(|f| test_path(&f.path) && uses(&f.path))
        {
            c.missing.push(match names.first() {
                Some(n) => format!("no test evidence: no finding in test code that exercises {n}"),
                None => "no test evidence: no finding in test code".into(),
            });
            let mut tt = terms.clone();
            tt.push("tests".into());
            let tests = discover::discover(ctx.engine, &ctx.cwd, scope, &tt, false, discover::SHOW);
            follow.extend(
                tests
                    .iter()
                    .filter(|t| test_path(&t.path))
                    .take(2)
                    .map(|t| format!("READ {}:{}-{}", t.path, t.start, t.end)),
            );
        }
        if need.callers && !code.iter().any(|f| !test_path(&f.path) && !declared_in(&f.path) && uses(&f.path))
        {
            let interfaces: Vec<&String> =
                subjects.iter().filter(|(_, i, _)| !i).flat_map(|(_, _, b)| b).collect();
            c.missing.push(match (subjects.iter().find(|(_, i, _)| !i), interfaces.first()) {
                (Some((n, ..)), Some(i)) => {
                    format!("no caller: no finding in production code that uses {n} or its interface {i}")
                }
                (Some((n, ..)), None) => format!("no caller: no finding in production code that uses {n}"),
                (None, _) => "no caller: no finding in production code that uses it".into(),
            });
            follow.extend(interfaces.iter().map(|i| format!("REFS {i}")));
            follow.extend(subjects.iter().filter(|(_, i, _)| !i).take(1).map(|(n, ..)| format!("REFS {n}")));
        }
        if need.trace {
            for f in &relevant {
                for (n, recv, m) in field_calls(&lines(&f.excerpt)) {
                    let decls: Vec<Option<bool>> = code
                        .iter()
                        .filter(|g| g.path != f.path)
                        .flat_map(|g| lines(&g.excerpt).into_iter().map(|(_, l)| declares(l, &m)))
                        .collect();
                    if decls.contains(&Some(true)) {
                        continue;
                    }
                    let impls = named(ctx, scope, &m, &f.path);
                    if impls.is_empty() {
                        continue;
                    }
                    let only = if decls.contains(&Some(false)) {
                        "; only an interface declaration is cited"
                    } else {
                        ""
                    };
                    let gap = format!(
                        "the call `{recv}.{m}(` at {}:{n} is not followed to its implementation{only}",
                        f.path
                    );
                    if !c.missing.contains(&gap) {
                        c.missing.push(gap);
                        follow_call(ctx, scope, &m, &f.path, DEPTH, &mut follow);
                    }
                }
            }
            let files: std::collections::BTreeSet<&str> = relevant.iter().map(|f| f.path.as_str()).collect();
            if files.len() == 1 && c.missing.is_empty() {
                c.missing.push(format!(
                    "the trace stays in {}; the calls it makes are not followed",
                    files.iter().next().unwrap_or(&"")
                ));
                if let Some(h) = homes.first() {
                    let t = text(&ctx.cwd, &h.path);
                    let ls: Vec<(usize, &str)> = t
                        .lines()
                        .enumerate()
                        .skip(h.start.saturating_sub(1))
                        .take(h.end + 1 - h.start)
                        .map(|(i, l)| (i + 1, l))
                        .collect();
                    for (_, _, m) in field_calls(&ls) {
                        follow_call(ctx, scope, &m, &h.path, DEPTH, &mut follow);
                    }
                }
            }
        }
        if need.route {
            let routed: Vec<&&Found> = relevant
                .iter()
                .filter(|f| lines(&f.excerpt).iter().any(|(_, l)| ROUTED.is_match(l)))
                .collect();
            if routed.is_empty() {
                c.missing.push("no route attribute is cited".into());
                follow.extend(homes.first().map(|h| format!("READ {}:{}-{}", h.path, h.start, h.end)));
            }
            for f in routed {
                let Some((at, attr)) = class_route(&text(&ctx.cwd, &f.path)) else { continue };
                let cited = findings
                    .iter()
                    .any(|g| g.path == f.path && lines(&g.excerpt).iter().any(|(n, _)| *n == at));
                let gap = format!("the route prefix `{attr}` at {}:{at} is not cited", f.path);
                if !cited && !c.missing.contains(&gap) {
                    c.missing.push(gap);
                    follow.push(format!("READ {}:{at}-{at}", f.path));
                }
            }
        }
        used.extend(c.findings.iter().cloned());
        for l in follow {
            if !cov.follow.contains(&l) {
                cov.follow.push(l);
            }
        }
        cov.items.push(c);
    }
    cov.follow.truncate(FOLLOW);
    cov.unrelated = findings.iter().filter(|f| !used.contains(&f.cite)).map(|f| f.cite.clone()).collect();
    cov
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    // Checks what an item asks for is read from its words, declarations are told from calls, interface members from bodies, plumbing calls are left out, and a class's route prefix and base interfaces are found.
    #[test]
    fn test_parts_of_the_check() {
        let n = |t: &str| needs(t, &discover::terms(t));
        assert_eq!(
            n("Which production code calls it, and which tests cover it?"),
            Needs { tests: true, callers: true, ..Needs::default() }
        );
        assert_eq!(
            n("Trace the wash from its endpoint through the service"),
            Needs { trace: true, route: true, ..Needs::default() }
        );
        assert_eq!(
            declares("    public async Task<Wash> StartSubscriptionWash(Command c)", "StartSubscriptionWash"),
            Some(true)
        );
        assert_eq!(
            declares("    Task<Wash> StartSubscriptionWash(Command c);", "StartSubscriptionWash"),
            Some(false)
        );
        assert_eq!(
            declares("        return await _svc.StartSubscriptionWash(c);", "StartSubscriptionWash"),
            None
        );
        assert_eq!(declares("        var w = StartSubscriptionWash(c);", "StartSubscriptionWash"), None);
        assert_eq!(declares("        await StartSubscriptionWash(c);", "StartSubscriptionWash"), None);
        let calls = field_calls(&[(
            4,
            "await _store.Save(x); _logger.LogInformation(\"x\"); this.queue.Enqueue<T>(y);",
        )]);
        assert_eq!(
            calls,
            [
                (4, "_store".to_string(), "Save".to_string()),
                (4, "this.queue".to_string(), "Enqueue".to_string())
            ]
        );
        let src = "using X;\n[ApiController]\n[Route(\"api/v1/Things\")]\npublic class ThingsController : ControllerBase, IThings\n{\n}\npublic interface IThings { }\n";
        assert_eq!(class_route(src), Some((3, "[Route(\"api/v1/Things\")]".to_string())));
        assert_eq!(class_route("public class A { }\n"), None);
        assert_eq!(
            types(src),
            [
                ("ThingsController".to_string(), false, vec!["IThings".to_string()]),
                ("IThings".to_string(), true, vec![])
            ]
        );
        assert!(doc("Mcp/AGENTS.md") && !doc("Api/X.cs"));
        assert!(test_path("Tests/Core.Tests/CacheTests.cs") && !test_path("Core/Caching/Cache.cs"));
        assert_eq!(lines("  12\tfoo\n  13\tbar\nnoise\n"), [(12, "foo"), (13, "bar")]);
        let w = |x: &[&str]| x.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(shares(&w(&["stored"]), &w(&["store"])) && shares(&w(&["caches"]), &w(&["cache"])));
        assert!(!shares(&w(&["key"]), &w(&["keyboard"])), "a short term must match a word whole");
    }
}
