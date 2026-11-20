// What an investigation's answer claims, checked against the files: each `FINDING path:A-B claim` line is verified to lie inside the scope and the file, and comes back with the cited lines read literally.
use std::path::{Path, PathBuf};

// Cited lines shown at most per finding; the rest are counted.
const EXCERPT: usize = 12;

// The model's final answer: its findings, what it could not establish, and its report on itself: its status, how well it understood the task and how well its results are supported (high, medium or low), the open question a stronger model should take over with the context it would need, and what only the user can give.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Answer {
    pub findings: Vec<Claim>,
    pub unknowns: Vec<String>,
    pub status: String,
    pub understanding: String,
    pub confidence: String,
    pub escalate: Option<String>,
    pub context: Vec<String>,
    pub blocked: Option<String>,
}

// One cited claim as the model wrote it.
#[derive(Debug, Clone, PartialEq)]
pub struct Claim {
    pub path: String,
    pub start: usize,
    pub end: usize,
    pub text: String,
}

// Reads the answer: the `FINDING path:A[-B] claim`, `UNKNOWN text` (or `GAP text`) and self-report lines after an `ANSWER` line, or anywhere when the model left the ANSWER line out; None when there are none of them.
pub fn parse_answer(reply: &str) -> Option<Answer> {
    let is = |l: &str, w: &str| strip_word(l.trim(), w).is_some();
    let headed = reply.lines().any(|l| is(l, "ANSWER"));
    let keyed = |l: &str| {
        ["FINDING", "UNKNOWN", "GAP", "STATUS", "CONFIDENCE", "ESCALATE", "BLOCKED"].iter().any(|w| is(l, w))
    };
    if !headed && !reply.lines().any(keyed) {
        return None;
    }
    let mut lines = reply.lines().map(str::trim).skip_while(|l| headed && !is(l, "ANSWER")).peekable();
    if headed {
        lines.next();
    }
    let mut a = Answer::default();
    for l in lines {
        if let Some(rest) = strip_word(l, "FINDING") {
            match claim(rest) {
                Some(c) => a.findings.push(c),
                None => a.unknowns.push(format!("unreadable finding: {rest}")),
            }
        } else if let Some(rest) = strip_word(l, "UNKNOWN").or_else(|| strip_word(l, "GAP")) {
            a.unknowns.push(rest.to_string());
        } else if let Some(rest) = strip_word(l, "STATUS") {
            a.status = word_of(rest, &["complete", "partial", "failed"]);
        } else if let Some(rest) = strip_word(l, "CONFIDENCE") {
            (a.understanding, a.confidence) = levels(rest);
        } else if let Some(rest) = strip_word(l, "ESCALATE").filter(|r| !r.is_empty() && !is(r, "no")) {
            a.escalate = Some(rest.to_string());
        } else if let Some(rest) = strip_word(l, "CONTEXT").filter(|r| !r.is_empty()) {
            a.context.push(rest.to_string());
        } else if let Some(rest) = strip_word(l, "BLOCKED").filter(|r| !r.is_empty() && !is(r, "no")) {
            a.blocked = Some(rest.to_string());
        }
    }
    Some(a)
}

// The first word of `s` when it is one of `allowed` (any case), lowercased; else empty.
fn word_of(s: &str, allowed: &[&str]) -> String {
    let w = s.split_whitespace().next().unwrap_or_default().to_lowercase();
    if allowed.contains(&w.as_str()) {
        w
    } else {
        String::new()
    }
}

// Reads `understanding=high result=medium` (or two bare levels in that order) into the two levels; one not given is empty.
fn levels(s: &str) -> (String, String) {
    const L: &[&str] = &["high", "medium", "low"];
    let (mut u, mut r, mut bare) = (String::new(), String::new(), Vec::new());
    for part in s.split([' ', ',', ';']).filter(|p| !p.is_empty()) {
        match part.split_once(['=', ':']) {
            Some((k, v)) if k.to_lowercase().starts_with("under") => u = word_of(v, L),
            Some((_, v)) => r = word_of(v, L),
            None => bare.push(word_of(part, L)),
        }
    }
    let mut bare = bare.into_iter().filter(|w| !w.is_empty());
    if u.is_empty() {
        u = bare.next().unwrap_or_default();
    }
    if r.is_empty() {
        r = bare.next().unwrap_or_default();
    }
    (u, r)
}

// Returns what follows a leading keyword (any case), or None.
pub fn strip_word<'a>(line: &'a str, word: &str) -> Option<&'a str> {
    let head = line.get(..word.len())?;
    let rest = &line[word.len()..];
    let starts = rest.is_empty() || rest.starts_with(char::is_whitespace) || rest.starts_with(':');
    (head.eq_ignore_ascii_case(word) && starts).then(|| rest.trim_start_matches(':').trim())
}

// Parses `path:A[-B] claim`.
fn claim(s: &str) -> Option<Claim> {
    let (cite, text) = s.split_once(char::is_whitespace).unwrap_or((s, ""));
    let (path, range) = cite.rsplit_once(':')?;
    let (a, b) = range.split_once('-').unwrap_or((range, range));
    let (start, end) = (a.parse().ok()?, b.parse().ok()?);
    let text = text.trim().trim_start_matches(['|', '-', '—', ':']).trim().to_string();
    Some(Claim { path: path.to_string(), start, end, text })
}

// A claim checked against its file: the literal lines, or why it was rejected.
pub enum Checked {
    Verified { excerpt: String },
    Rejected(String),
}

// Checks one claim: inside a scope entry, an existing file, a valid line range; returns the cited lines with their numbers.
pub fn check(cwd: &Path, scope: &[PathBuf], c: &Claim) -> Checked {
    let full = match std::fs::canonicalize(cwd.join(&c.path)) {
        Ok(p) => p,
        Err(_) => return Checked::Rejected("no such file".into()),
    };
    if !in_scope(&full, scope) {
        return Checked::Rejected("outside the investigated scope".into());
    }
    let Ok(text) = std::fs::read_to_string(&full) else {
        return Checked::Rejected("not a readable text file".into());
    };
    let lines: Vec<&str> = text.lines().collect();
    if c.start == 0 || c.end < c.start {
        return Checked::Rejected(format!("{}-{} is not a line range", c.start, c.end));
    }
    if c.end > lines.len() {
        return Checked::Rejected(format!("line {} is past the end ({} lines)", c.end, lines.len()));
    }
    let shown = c.end.min(c.start + EXCERPT - 1);
    let mut excerpt: String = (c.start..=shown).map(|n| format!("   {n}\t{}\n", lines[n - 1])).collect();
    if shown < c.end {
        excerpt.push_str(&format!(
            "   … {} more cited lines: zazzle read {}:{}-{}\n",
            c.end - shown,
            c.path,
            shown + 1,
            c.end
        ));
    }
    Checked::Verified { excerpt }
}

// True when a canonical path is a scope entry or lies under one.
pub fn in_scope(full: &Path, scope: &[PathBuf]) -> bool {
    scope.iter().any(|s| full.starts_with(s))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    // Checks the ANSWER block is found, findings and unknowns are read, and bad citations become unknowns.
    #[test]
    fn test_parse_answer() {
        assert_eq!(parse_answer("READ a.rs"), None);
        let bare = parse_answer("FINDING a.rs:2 x\nUNKNOWN y").expect("no ANSWER line");
        assert_eq!((bare.findings.len(), bare.unknowns.len()), (1, 1));
        assert!(parse_answer("ANSWER: done").is_some());
        let a = parse_answer("thinking aloud\nanswer\nFINDING src/a.rs:3-5 — the key is checked\nfinding b.rs:7 one line\nUNKNOWN whether retries happen\nFINDING nowhere\nnoise").expect("answer");
        assert_eq!(
            a.findings[0],
            Claim { path: "src/a.rs".into(), start: 3, end: 5, text: "the key is checked".into() }
        );
        assert_eq!((a.findings[1].start, a.findings[1].end), (7, 7));
        assert_eq!(
            a.unknowns,
            vec!["whether retries happen".to_string(), "unreadable finding: nowhere".to_string()]
        );
        assert_eq!(strip_word("FINDINGS x", "FINDING"), None);
        assert_eq!(strip_word("UNKNOWN", "UNKNOWN"), Some(""));
    }

    // Checks verified excerpts are literal and capped, and each kind of bad citation is rejected with its reason.
    #[test]
    fn test_check() {
        let t = TempDir::new();
        let body: String = (1..=30).map(|i| format!("line {i}\n")).collect();
        t.write("src/a.rs", body.as_bytes());
        t.write("other/b.rs", b"x\n");
        t.write("src/bin.dat", &[0xff, 0xfe, 0x00]);
        let cwd = t.path().canonicalize().expect("cwd");
        let scope = vec![cwd.join("src")];
        let c = |p: &str, a, b| Claim { path: p.into(), start: a, end: b, text: String::new() };
        let Checked::Verified { excerpt } = check(&cwd, &scope, &c("src/a.rs", 2, 3)) else {
            panic!("verified")
        };
        assert_eq!(excerpt, "   2\tline 2\n   3\tline 3\n");
        let Checked::Verified { excerpt } = check(&cwd, &scope, &c("src/a.rs", 1, 20)) else {
            panic!("long")
        };
        assert!(excerpt.ends_with("   … 8 more cited lines: zazzle read src/a.rs:13-20\n"), "{excerpt}");
        for (claim, why) in [
            (c("src/none.rs", 1, 1), "no such file"),
            (c("other/b.rs", 1, 1), "outside the investigated scope"),
            (c("src/a.rs", 31, 31), "line 31 is past the end (30 lines)"),
            (c("src/a.rs", 0, 1), "0-1 is not a line range"),
            (c("src/a.rs", 5, 4), "5-4 is not a line range"),
            (c("src/bin.dat", 1, 1), "not a readable text file"),
        ] {
            match check(&cwd, &scope, &claim) {
                Checked::Rejected(r) => assert_eq!(r, why),
                Checked::Verified { .. } => panic!("{why}"),
            }
        }
    }

    // Checks the self-report lines: status, both confidence spellings, ESCALATE with its context, BLOCKED, GAP, and that `no` and unknown words are dropped.
    #[test]
    fn test_parse_self_report() {
        let a = parse_answer("STATUS partial now\nCONFIDENCE understanding=high result=low\nGAP the retry path\nESCALATE Should enforcement need a new version?\nCONTEXT the frozen contract rule\nCONTEXT \nBLOCKED decision which version to ship").expect("report");
        assert_eq!(
            (a.status.as_str(), a.understanding.as_str(), a.confidence.as_str()),
            ("partial", "high", "low")
        );
        assert_eq!(a.unknowns, ["the retry path"]);
        assert_eq!(a.escalate.as_deref(), Some("Should enforcement need a new version?"));
        assert_eq!(a.context, ["the frozen contract rule"]);
        assert_eq!(a.blocked.as_deref(), Some("decision which version to ship"));
        let b = parse_answer("ANSWER\nSTATUS done\nCONFIDENCE medium, high\nESCALATE no\nBLOCKED no")
            .expect("bare");
        assert_eq!(
            (b.status.as_str(), b.understanding.as_str(), b.confidence.as_str()),
            ("", "medium", "high")
        );
        assert_eq!((b.escalate, b.blocked), (None, None));
        assert_eq!(levels("result:high"), (String::new(), "high".into()));
        assert_eq!(levels("sure"), (String::new(), String::new()));
    }
}
