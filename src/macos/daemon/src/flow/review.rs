// `zazzle review`: a multi-viewpoint review pinned to the exact tree. zazzle picks the viewpoints the change and its branch level call for (a consequential boundary adds its viewpoint even on a feature branch), a local planner writes each a focused brief, independent reviewers report concrete findings with evidence, impact and a fix, and a final reviewer deduplicates, challenges and reconciles them. Unless read-only, confirmed findings are then fixed, verified and re-reviewed on the new tree, round by round, so only decisions, missing access or blocked verification go back to the user; a ledger in the git directory keeps every finding across rounds and runs.
use super::edit::{self, FILES};
use super::policy::{Policy, LEVELS};
use super::repo;
use super::verify::{self, Receipt};
use crate::adaptive::agent::Rung;
use crate::adaptive::calls::{self, Ask, Backend, Log};
use serde_json::{json, Value};
use std::path::Path;
use std::time::{Duration, Instant};

// The viewpoints and what each examines.
pub const VIEWPOINTS: &[(&str, &str)] = &[
    ("correctness", "logic, edge cases, invariants and failure paths"),
    ("security", "authorization, injection, secrets and data exposure"),
    ("reliability", "concurrency, retries, timeouts and partial failures"),
    ("compatibility", "API contracts, migrations, client versions and deployment sequencing"),
    ("performance", "database queries, CPU, memory and network costs"),
    ("maintainability", "clarity, coupling and ease of changing the code safely"),
    ("testing", "missing scenarios, weak assertions and regression coverage"),
    ("operations", "logging, metrics, diagnosis and rollback"),
    ("overengineering", "unneeded abstractions, flexibility or infrastructure, and complex approaches where a simple one would do"),
    ("cognitive-load", "documentation and comments that can be shortened while keeping decisions, constraints and useful rationale"),
];
// The most tokens of diff a reviewer is sent.
const DIFF_TOKENS: usize = 12_000;

// What a review was asked: the base, the branch the change goes into (None: the review is of the current branch), the level (None: that branch's), the viewpoints (empty: zazzle picks), the intent, the reviewers, the final reviewer, the fixer, the writer for documentation findings, whether it may edit, its remediation rounds, each call's time limit, and whether to report the benchmark questions.
pub struct Opts {
    pub base: Option<String>,
    pub into: Option<String>,
    pub level: Option<String>,
    pub viewpoints: Vec<String>,
    pub intent: String,
    pub reviewers: Vec<Rung>,
    pub planner: Rung,
    pub last: Rung,
    pub fixer: Rung,
    pub writer: Rung,
    pub read_only: bool,
    pub rounds: usize,
    pub limit: Duration,
    pub bench: bool,
}

// One finding through its life: id, viewpoint, severity, `path:A-B`, the issue, impact and fix, whether its evidence checked out, its status (open, fixed, dropped, decision, dissent), the tree it was found on and fixed on, and a note (why dropped, the question, the dissent, how it was fixed).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Finding {
    pub id: String,
    pub viewpoint: String,
    pub severity: String,
    pub cite: String,
    pub issue: String,
    pub impact: String,
    pub fix: String,
    pub verified: bool,
    pub status: String,
    pub found_on: String,
    pub fixed_on: String,
    pub note: String,
}

// A reviewer's RESOLVED answers: the finding's id, whether it is resolved, and the evidence.
type Resolved = Vec<(String, bool, String)>;

// The scores the change gives each viewpoint (0 to 3), and the viewpoints whose boundary it touches.
pub fn signals(diff: &str, paths: &[String]) -> (Vec<(String, usize)>, Vec<String>) {
    let added: String = diff
        .lines()
        .filter(|l| l.starts_with('+') && !l.starts_with("+++"))
        .map(|l| l.to_lowercase() + "\n")
        .collect();
    let ps = paths.join("\n").to_lowercase();
    let hits = |words: &[&str]| words.iter().filter(|w| added.contains(*w) || ps.contains(*w)).count();
    let code = paths.iter().any(|p| !p.ends_with(".md") && !p.ends_with(".txt"));
    let tests = paths.iter().any(|p| p.to_lowercase().contains("test"));
    let comments = added
        .lines()
        .filter(|l| {
            let t = l.trim_start_matches('+').trim_start();
            t.starts_with("//") || t.starts_with('#') || t.starts_with("///") || t.starts_with('*')
        })
        .count();
    let lines = added.lines().count().max(1);
    let new_files = diff.matches("new file mode").count();
    let sec = hits(&[
        "auth",
        "token",
        "password",
        "secret",
        "permission",
        "credential",
        "exec(",
        "eval(",
        "sql",
        "cookie",
        "jwt",
        "sanitiz",
        "crypto",
    ]);
    let comp = hits(&[
        "api/",
        ".proto",
        "migration",
        "schema",
        "openapi",
        "contract",
        "public ",
        "pub fn",
        "serializ",
        "version",
    ]);
    let scores = vec![
        ("correctness".to_string(), if code { 3 } else { 1 }),
        ("security".into(), sec.min(3)),
        (
            "reliability".into(),
            hits(&[
                "async",
                "await",
                "thread",
                "lock",
                "mutex",
                "retry",
                "timeout",
                "spawn",
                "channel",
                "transaction",
                "concurren",
            ])
            .min(3),
        ),
        ("compatibility".into(), comp.min(3)),
        (
            "performance".into(),
            hits(&["query", "select ", "cache", "alloc", "clone()", "index", "batch", "n+1"]).min(3),
        ),
        ("maintainability".into(), if lines > 300 { 2 } else { 1 }),
        (
            "testing".into(),
            if code {
                if tests {
                    2
                } else {
                    3
                }
            } else {
                0
            },
        ),
        (
            "operations".into(),
            hits(&["log", "metric", "trace", "alert", "deploy", "rollback", ".yml", "env::var", "getenv"])
                .min(3),
        ),
        (
            "overengineering".into(),
            if new_files > 3 || hits(&["trait ", "interface ", "factory", "generic", "<t>", "abstract "]) > 1
            {
                2
            } else {
                1
            },
        ),
        (
            "cognitive-load".into(),
            if comments * 3 > lines || paths.iter().any(|p| p.ends_with(".md")) { 2 } else { 0 },
        ),
    ];
    let mut boundary = Vec::new();
    if sec >= 2 {
        boundary.push("security".to_string());
    }
    if comp >= 2 {
        boundary.push("compatibility".to_string());
    }
    (scores, boundary)
}

// The viewpoints a level always gets, and how many it gets in all.
fn budget(level: &str) -> (&'static [&'static str], usize) {
    match level {
        "production" => (&["correctness", "compatibility", "reliability", "operations", "security"], 5),
        "staging" => (&["correctness", "compatibility", "reliability", "operations"], 6),
        "development" => (&["correctness", "testing", "maintainability"], 4),
        _ => (&["correctness", "testing"], 2),
    }
}

// Picks the viewpoints: the level's required ones, then the highest scores up to its budget, plus any boundary the change touches; a viewpoint scoring 0 is left out unless required.
pub fn pick(level: &str, scores: &[(String, usize)], boundary: &[String]) -> Vec<String> {
    let (required, n) = budget(level);
    let mut out: Vec<String> = required.iter().map(|s| s.to_string()).collect();
    let mut rest: Vec<&(String, usize)> = scores.iter().filter(|(v, s)| *s > 0 && !out.contains(v)).collect();
    rest.sort_by_key(|x| std::cmp::Reverse(x.1));
    for (v, _) in rest {
        if out.len() >= n {
            break;
        }
        out.push(v.clone());
    }
    for b in boundary {
        if !out.contains(b) {
            out.push(b.clone());
        }
    }
    out
}

// Groups viewpoints into reviewer calls: one call at the feature level, pairs at development, one each above.
pub fn groups(level: &str, views: &[String]) -> Vec<Vec<String>> {
    let size = match level {
        "feature" => views.len().max(1),
        "development" => 2,
        _ => 1,
    };
    views.chunks(size).map(|c| c.to_vec()).collect()
}

// What a viewpoint examines.
fn about(v: &str) -> &'static str {
    VIEWPOINTS.iter().find(|(k, _)| *k == v).map_or("the change as a whole", |(_, d)| d)
}

// What a level's pass is for.
fn level_text(level: &str) -> &'static str {
    match level {
        "production" => "the exact release candidate against the deployed baseline: release blockers, compatibility, rollout and rollback, and whether required evidence is fresh",
        "staging" => "the integrated candidate across features: compatibility, migrations, deployment order, reliability and operations",
        "development" => "the complete change and its interaction with neighboring code: regressions and shared contracts",
        _ => "fast feedback on the changed behavior, obvious risks and focused regression coverage; the merged change gets a stricter pass at each later level, so stay brief",
    }
}

// Reads one `FINDING severity | path:A-B | issue | impact | fix` line.
pub fn parse_finding(rest: &str, viewpoint: &str) -> Option<Finding> {
    let f: Vec<&str> = rest.split('|').map(str::trim).collect();
    if f.len() < 3 {
        return None;
    }
    let severity = f[0].to_lowercase();
    let severity =
        if ["blocker", "major", "minor"].contains(&severity.as_str()) { severity } else { "minor".into() };
    Some(Finding {
        viewpoint: viewpoint.into(),
        severity,
        cite: f[1].trim_matches('`').into(),
        issue: f[2].into(),
        impact: f.get(3).copied().unwrap_or_default().into(),
        fix: f.get(4).copied().unwrap_or_default().into(),
        status: "open".into(),
        ..Finding::default()
    })
}

// Checks a finding's citation against the files, returning the cited lines when they exist.
fn evidence(top: &Path, cite: &str) -> Option<String> {
    let c =
        crate::investigate::cite::parse_answer(&format!("FINDING {cite} x"))?.findings.into_iter().next()?;
    match crate::investigate::cite::check(top, &[top.to_path_buf()], &c) {
        crate::investigate::cite::Checked::Verified { excerpt } => Some(excerpt),
        crate::investigate::cite::Checked::Rejected(_) => None,
    }
}

// The ledger file for a branch.
fn ledger_path(top: &Path, branch: &str) -> Result<std::path::PathBuf, String> {
    let safe: String =
        branch.chars().map(|c| if c.is_alphanumeric() || c == '-' { c } else { '_' }).collect();
    Ok(repo::state(top, "reviews")?.join(format!("{safe}.json")))
}

// A finding as JSON.
fn fjson(f: &Finding) -> Value {
    json!({"id": f.id, "viewpoint": f.viewpoint, "severity": f.severity, "cite": f.cite, "issue": f.issue, "impact": f.impact, "fix": f.fix, "verified": f.verified, "status": f.status, "found_on": f.found_on, "fixed_on": f.fixed_on, "note": f.note})
}

// A finding from JSON.
fn from_fjson(v: &Value) -> Finding {
    let s = |k: &str| v[k].as_str().unwrap_or_default().to_string();
    Finding {
        id: s("id"),
        viewpoint: s("viewpoint"),
        severity: s("severity"),
        cite: s("cite"),
        issue: s("issue"),
        impact: s("impact"),
        fix: s("fix"),
        verified: v["verified"] == true,
        status: s("status"),
        found_on: s("found_on"),
        fixed_on: s("fixed_on"),
        note: s("note"),
    }
}

// The open findings in a branch's review ledger, as `Fn [viewpoint] cite: issue`.
pub fn open_findings(top: &Path, branch: &str) -> Vec<String> {
    load_ledger(top, branch)
        .0
        .iter()
        .filter(|f| f.status == "open" || f.status == "fixed-unverified")
        .map(|f| format!("{} [{}] {}: {}", f.id, f.viewpoint, f.cite, f.issue))
        .collect()
}

// The ledger: findings and the passes made (tree, level, viewpoints), as stored.
fn load_ledger(top: &Path, branch: &str) -> (Vec<Finding>, Vec<Value>) {
    let v: Value = ledger_path(top, branch)
        .ok()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    (
        v["findings"].as_array().into_iter().flatten().map(from_fjson).collect(),
        v["passes"].as_array().cloned().unwrap_or_default(),
    )
}

// Saves the ledger.
fn save_ledger(top: &Path, branch: &str, findings: &[Finding], passes: &[Value]) -> Result<(), String> {
    let p = ledger_path(top, branch)?;
    let v = json!({"findings": findings.iter().map(fjson).collect::<Vec<_>>(), "passes": passes});
    std::fs::write(&p, serde_json::to_string_pretty(&v).unwrap_or_default())
        .map_err(|e| format!("{}: {e}", p.display()))
}

// The next free finding id.
fn next_id(all: &[Finding]) -> String {
    let n = all.iter().filter_map(|f| f.id.strip_prefix('F')?.parse::<usize>().ok()).max().unwrap_or(0);
    format!("F{}", n + 1)
}

// Cuts text to about `tokens` tokens at a line boundary.
fn cap(text: &str, tokens: usize) -> String {
    let mut out = String::new();
    for l in text.lines() {
        if crate::tokens::estimate(&out) > tokens {
            out.push_str("[cut here; read the files for the rest]\n");
            break;
        }
        out.push_str(l);
        out.push('\n');
    }
    out
}

// The state of one review run: the pinned revision, the diff and its paths, the verification, the ledger and the report so far.
struct Run<'a> {
    top: &'a Path,
    p: &'a Policy,
    o: &'a Opts,
    b: &'a dyn Backend,
    log: &'a Log,
    base: String,
    branch: String,
    level: String,
    tree: String,
    diff: String,
    paths: Vec<String>,
    receipt: Option<Receipt>,
    findings: Vec<Finding>,
    passes: Vec<Value>,
    gaps: Vec<String>,
}

impl Run<'_> {
    // Re-pins the run to the working tree as it stands: tree, diff, paths and its verification (a fresh receipt for this tree, else a new run of the applicable checks).
    fn pin(&mut self) -> Result<(), String> {
        self.tree = repo::worktree_tree(self.top)?;
        let p = self.p;
        self.diff = super::testfirst::keep_patch(&repo::diff(self.top, &self.base, &self.tree)?, &|f| {
            !p.is_generated(f)
        });
        let (paths, generated): (Vec<String>, Vec<String>) =
            repo::changed(self.top, &self.base, &self.tree)?.into_iter().partition(|f| !p.is_generated(f));
        self.paths = paths;
        if !generated.is_empty() && !self.gaps.iter().any(|g| g.starts_with("generated files")) {
            self.gaps.push(format!(
                "generated files left out of the review (their generator is reviewed instead): {}",
                generated.join(", ")
            ));
        }
        self.receipt = match verify::receipt_for(self.top, &self.tree) {
            Some(r) => Some(r),
            None if self.p.checks.is_empty() => None,
            None => Some(verify::verify(self.top, self.p, Some(&self.base), &[], self.o.limit)?),
        };
        Ok(())
    }

    // The verification in a few lines for a prompt.
    fn receipt_text(&self) -> String {
        self.receipt.as_ref().map_or_else(|| "no checks configured or found".into(), |r| r.text())
    }

    // Has the local planner write a focused brief per viewpoint; returns the briefs (a viewpoint without one gets its description).
    fn briefs(&self, views: &[String]) -> Vec<(String, String)> {
        let menu: String = views.iter().map(|v| format!("- {v}: {}\n", about(v))).collect();
        let stat = repo::git(self.top, &["diff", "--stat", &self.base, &self.tree]).unwrap_or_default();
        let system = "You brief code reviewers. For each viewpoint listed, write one or two sentences saying what in this change that reviewer should examine first and why, naming files. Be concise; reply only with lines `VIEW name | brief`.";
        let prompt = format!("Level: {} ({})\nIntent: {}\nViewpoints:\n{menu}Verification:\n{}\nChanged files:\n{stat}\nDiff (start):\n{}", self.level, level_text(&self.level), self.o.intent, self.receipt_text(), cap(&self.diff, 3000));
        let sections = [("brief", system), ("task", self.o.intent.as_str()), ("diff", self.diff.as_str())];
        let reply = self
            .log
            .text(
                self.b,
                &self.o.planner,
                &Ask::new("briefing", system, &prompt, &sections),
                self.top,
                Instant::now() + self.o.limit,
            )
            .unwrap_or_default();
        views
            .iter()
            .map(|v| {
                let line = reply
                    .lines()
                    .filter_map(|l| crate::investigate::cite::strip_word(l.trim(), "VIEW"))
                    .find(|l| l.split('|').next().is_some_and(|n| n.trim().eq_ignore_ascii_case(v)));
                let brief = line
                    .and_then(|l| l.split_once('|'))
                    .map(|(_, b)| b.trim().to_string())
                    .filter(|b| !b.is_empty())
                    .unwrap_or_else(|| format!("Examine {}.", about(v)));
                (v.clone(), brief)
            })
            .collect()
    }

    // One reviewer call on a group of viewpoints: its findings (evidence checked) and its RESOLVED answers for the open findings it was shown.
    fn review(
        &self,
        group: &[(String, String)],
        r: &Rung,
        role: &str,
        open: &[Finding],
    ) -> (Vec<Finding>, Resolved, Option<String>) {
        let names: Vec<&str> = group.iter().map(|(v, _)| v.as_str()).collect();
        let views: String = group.iter().map(|(v, b)| format!("- {v} ({}): {b}\n", about(v))).collect();
        let earlier: String = open
            .iter()
            .map(|f| format!("{} [{}] {} {}: {}\n", f.id, f.viewpoint, f.severity, f.cite, f.issue))
            .collect();
        let reads = if r.hosted() { " You may read any file in the repository; change nothing." } else { "" };
        let system = format!("You review one change at the {} level: {}.{reads} Report only concrete issues that need judgment, not formatting, lint or style a formatter or linter catches, and not coverage numbers: zazzle runs those tools. Do not run builds or tests. Show each issue with evidence (path:A-B in the current files), impact and a suggested fix; a clean change gets no FINDING lines. Be concise; reply only with lines:\nFINDING blocker|major|minor | path:A-B | the issue | its impact | the suggested fix\nRESOLVED Fn yes|no | evidence (one per earlier finding shown, if any)\nSCOPE what you checked\nCONFIDENCE understanding=high|medium|low result=high|medium|low", self.level, level_text(&self.level));
        let mut prompt = format!(
            "Viewpoints:\n{views}Revision: base {} → tree {} (branch {})\nIntent: {}\nVerification:\n{}\n",
            verify::short(&self.base),
            verify::short(&self.tree),
            self.branch,
            self.o.intent,
            self.receipt_text()
        );
        if !earlier.is_empty() {
            prompt.push_str(&format!("Earlier findings still open (check each on this revision; do not report them again):\n{earlier}"));
        }
        let diff = cap(&self.diff, DIFF_TOKENS);
        prompt.push_str(&format!("Diff:\n{diff}"));
        let receipt = self.receipt_text();
        let sections = [
            ("brief", system.as_str()),
            ("task", views.as_str()),
            ("evidence", receipt.as_str()),
            ("findings", earlier.as_str()),
            ("diff", diff.as_str()),
        ];
        let reply = match self.log.text(
            self.b,
            r,
            &Ask::new(role, &system, &prompt, &sections),
            self.top,
            Instant::now() + self.o.limit,
        ) {
            Ok(t) => t,
            Err(e) => {
                return (
                    Vec::new(),
                    Vec::new(),
                    Some(format!("{} ({}) failed: {e}", names.join("+"), r.label())),
                )
            }
        };
        let mut found = Vec::new();
        let mut resolved = Vec::new();
        for l in reply.lines().map(str::trim) {
            if let Some(rest) = crate::investigate::cite::strip_word(l, "FINDING") {
                if let Some(mut f) = parse_finding(rest, names.first().copied().unwrap_or_default()) {
                    if names.len() > 1 {
                        f.viewpoint = names.join("+");
                    }
                    f.verified = evidence(self.top, &f.cite).is_some();
                    f.found_on = self.tree.clone();
                    found.push(f);
                }
            } else if let Some(rest) = crate::investigate::cite::strip_word(l, "RESOLVED") {
                let (head, ev) = rest.split_once('|').unwrap_or((rest, ""));
                let mut w = head.split_whitespace();
                if let (Some(id), Some(ans)) = (w.next(), w.next()) {
                    resolved.push((id.to_string(), ans.eq_ignore_ascii_case("yes"), ev.trim().to_string()));
                }
            }
        }
        (found, resolved, None)
    }

    // Has the final reviewer deduplicate, challenge and reconcile new findings; returns them with ids and statuses (open, dropped, decision, dissent).
    fn adjudicate(&mut self, raw: Vec<Finding>) -> Vec<Finding> {
        if raw.is_empty() {
            return raw;
        }
        let listing: String = raw
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let ev = evidence(self.top, &f.cite).map_or_else(
                    || "evidence: the cited lines do not exist".to_string(),
                    |e| format!("cited lines:\n{e}"),
                );
                format!(
                    "R{} [{}] {} {} — {}; impact: {}; fix: {}\n{ev}\n",
                    i + 1,
                    f.viewpoint,
                    f.severity,
                    f.cite,
                    f.issue,
                    f.impact,
                    f.fix
                )
            })
            .collect();
        let system = "You are the final reviewer. Deduplicate the findings by underlying defect, challenge unsupported claims against the cited lines, reconcile conflicting fixes, and keep substantive dissent. Be concise; reply only with lines:\nKEEP Rn [Rm ...] | blocker|major|minor | one-line reason (Rm are duplicates merged into Rn)\nDROP Rn | why\nDISSENT Rn | the disagreement worth keeping\nDECIDE Rn | the question only the user can answer";
        let sections = [("brief", system), ("findings", listing.as_str())];
        let reply = self.log.text(
            self.b,
            &self.o.last,
            &Ask::new("adjudicate", system, &listing, &sections),
            self.top,
            Instant::now() + self.o.limit,
        );
        let reply = match reply {
            Ok(r) => r,
            Err(e) => {
                self.gaps.push(format!("the final reviewer ({}) failed: {e}; findings with checked evidence are kept as they are", self.o.last.label()));
                String::new()
            }
        };
        let idx = |s: &str| {
            s.trim()
                .trim_start_matches('R')
                .parse::<usize>()
                .ok()
                .filter(|n| *n >= 1 && *n <= raw.len())
                .map(|n| n - 1)
        };
        let mut out = raw.clone();
        let mut decided = vec![false; raw.len()];
        for l in reply.lines().map(str::trim) {
            let (word, rest) = l.split_once(char::is_whitespace).unwrap_or((l, ""));
            let (ids, note) = rest.split_once('|').unwrap_or((rest, ""));
            let mut ids = ids.split_whitespace().filter_map(|x| idx(x.trim_matches(['[', ']', ','])));
            let Some(first) = ids.next() else { continue };
            let note = note.trim().to_string();
            decided[first] = true;
            match word.to_uppercase().as_str() {
                "KEEP" => {
                    let (sev, why) = note.split_once('|').unwrap_or((&note, ""));
                    let sev = sev.trim().to_lowercase();
                    if ["blocker", "major", "minor"].contains(&sev.as_str()) {
                        out[first].severity = sev;
                    }
                    out[first].note = why.trim().to_string();
                    for d in ids {
                        decided[d] = true;
                        out[d].status = "dropped".into();
                        out[d].note = format!("duplicate of R{}", first + 1);
                    }
                }
                "DROP" => (out[first].status, out[first].note) = ("dropped".into(), note),
                "DISSENT" => (out[first].status, out[first].note) = ("dissent".into(), note),
                "DECIDE" => (out[first].status, out[first].note) = ("decision".into(), note),
                _ => decided[first] = false,
            }
        }
        for (i, f) in out.iter_mut().enumerate() {
            if !decided[i] && !f.verified {
                (f.status, f.note) =
                    ("dropped".into(), "not adjudicated, and its cited lines do not exist".into());
            }
        }
        for f in out.iter_mut() {
            f.id = next_id(&self.findings);
            self.findings.push(f.clone());
        }
        out
    }

    // One review pass over the given briefs: every group reviewed (stronger and different reviewers at higher levels), new findings adjudicated, RESOLVED answers applied to earlier open findings; returns how many new findings are open.
    fn pass(&mut self, briefs: &[(String, String)], round: usize) -> usize {
        let views: Vec<String> = briefs.iter().map(|(v, _)| v.clone()).collect();
        let gs = groups(&self.level, &views);
        let mut raw = Vec::new();
        let mut agents = Vec::new();
        for (i, g) in gs.iter().enumerate() {
            let r = self.reviewer(i, round);
            let group: Vec<(String, String)> =
                briefs.iter().filter(|(v, _)| g.contains(v)).cloned().collect();
            let open: Vec<Finding> = self
                .findings
                .iter()
                .filter(|f| f.status == "open" && g.iter().any(|v| f.viewpoint.split('+').any(|x| x == v)))
                .cloned()
                .collect();
            let role = if round == 0 {
                format!("review {}", g.join("+"))
            } else {
                format!("re-review {}", g.join("+"))
            };
            let (found, resolved, err) = self.review(&group, &r, &role, &open);
            agents.push(json!({"viewpoints": g, "agent": r.label()}));
            if let Some(e) = err {
                self.gaps.push(e);
            }
            for (id, yes, ev) in resolved {
                if let Some(f) = self.findings.iter_mut().find(|f| f.id == id && f.status == "open") {
                    if yes {
                        f.status = "resolved".into();
                        f.fixed_on = self.tree.clone();
                        f.note = ev;
                    }
                }
            }
            raw.extend(found);
        }
        self.passes.push(json!({"tree": self.tree, "level": self.level, "round": round, "viewpoints": views, "agents": agents}));
        self.adjudicate(raw).iter().filter(|f| f.status == "open").count()
    }

    // The reviewer for a group: the cheapest at the feature level, else rotating through the reviewers (strongest first above development), never the fixer when another is available after a fix.
    fn reviewer(&self, i: usize, round: usize) -> Rung {
        let rs = &self.o.reviewers;
        let order: Vec<&Rung> = match self.level.as_str() {
            "feature" => rs.iter().collect(),
            "development" => rs.iter().cycle().skip(i).take(rs.len()).collect(),
            _ => rs.iter().rev().cycle().skip(i).take(rs.len()).collect(),
        };
        let pick = |r: &&Rung| round == 0 || **r != self.o.fixer;
        order
            .iter()
            .find(|r| pick(r))
            .or(order.first())
            .map(|r| (*r).clone())
            .unwrap_or_else(|| self.o.planner.clone())
    }

    // Fixes the open findings: documentation findings (cognitive load, or a cited documentation file) go to the cheap writer, limited to documentation paths; the rest go to the fixer, limited to the change's other files and test files, adding regression tests where they apply; returns whether anything changed.
    fn remediate(&mut self) -> Result<bool, String> {
        let open: Vec<Finding> = self.findings.iter().filter(|f| f.status == "open").cloned().collect();
        let is_doc = |f: &Finding| {
            f.viewpoint.split('+').all(|v| v == "cognitive-load")
                || self.p.is_doc(f.cite.rsplit_once(':').map_or(f.cite.as_str(), |(p, _)| p))
        };
        let (docs, code): (Vec<Finding>, Vec<Finding>) = open.into_iter().partition(is_doc);
        let mut changed = false;
        if !code.is_empty() {
            let paths: Vec<String> = self.paths.iter().filter(|p| !self.p.is_doc(p)).cloned().collect();
            let p = self.p;
            let allowed = |x: &str| {
                !p.is_doc(x)
                    && !p.is_generated(x)
                    && (paths.iter().any(|y| y == x) || x.to_lowercase().contains("test"))
            };
            let how = if self.o.fixer.hosted() {
                "Reply with a line `FIXED Fn how` per finding, then STATUS and CONFIDENCE."
            } else {
                FILES
            };
            let intro = format!("Fix these confirmed review findings in the repository. For each, make the smallest change that removes the defect and, where it applies, add a meaningful regression test. Change only these files of the change ({}) or test files; leave documentation alone, the writer updates it. {} Keep the intent: {}", paths.join(", "), super::TOOLS_NOTE, self.o.intent);
            changed |=
                self.fix_with(&code, &self.o.fixer.clone(), "remediate", &intro, how, &allowed, "the fixer")?;
        }
        if !docs.is_empty() {
            let p = self.p;
            let allowed = |x: &str| p.is_doc(x) && !p.is_generated(x);
            let how = if self.o.writer.hosted() {
                "Edit the files directly, then reply with STATUS."
            } else {
                FILES
            };
            let intro = format!("Fix these confirmed documentation findings. Shorten or correct the text while keeping decisions, constraints and useful rationale; change only documentation files. Keep the intent: {}", self.o.intent);
            changed |= self.fix_with(
                &docs,
                &self.o.writer.clone(),
                "remediate docs",
                &intro,
                how,
                &allowed,
                "the writer",
            )?;
        }
        Ok(changed)
    }

    // Has one agent fix a set of findings within `allowed`, noting what it changed outside it; returns whether it changed anything.
    #[allow(clippy::too_many_arguments)]
    fn fix_with(
        &mut self,
        findings: &[Finding],
        r: &Rung,
        role: &str,
        intro: &str,
        how: &str,
        allowed: &dyn Fn(&str) -> bool,
        who: &str,
    ) -> Result<bool, String> {
        let list: String = findings
            .iter()
            .map(|f| {
                format!(
                    "{} [{}] {} at {}: {}\n  impact: {}\n  fix: {}\n{}\n",
                    f.id,
                    f.viewpoint,
                    f.severity,
                    f.cite,
                    f.issue,
                    f.impact,
                    f.fix,
                    evidence(self.top, &f.cite).unwrap_or_default()
                )
            })
            .collect();
        let prompt = format!("{intro}\n{list}{how}");
        let sections = [("brief", intro), ("findings", list.as_str())];
        let e = edit::edit(
            self.top,
            self.b,
            self.log,
            r,
            role,
            &prompt,
            &sections,
            allowed,
            Instant::now() + self.o.limit,
        )?;
        if !e.reverted.is_empty() {
            self.gaps
                .push(format!("{who} changed paths outside its scope, put back: {}", e.reverted.join(", ")));
        }
        Ok(!e.changed.is_empty())
    }
}

// Runs the review and returns its receipt (text, or JSON).
pub fn run(
    top: &Path,
    p: &Policy,
    o: &Opts,
    b: &dyn Backend,
    log: &Log,
    json_out: bool,
) -> Result<String, String> {
    let started = Instant::now();
    let branch = repo::branch(top).unwrap_or_else(|_| "HEAD".into());
    let level = match &o.level {
        Some(l) if LEVELS.contains(&l.as_str()) => l.clone(),
        Some(l) => return Err(format!("--level is one of {}, not `{l}`", LEVELS.join(", "))),
        None => p.level(o.into.as_deref().unwrap_or(&branch)),
    };
    let base = match &o.base {
        Some(b) => repo::git(top, &["rev-parse", "--verify", b])
            .map_err(|_| format!("--base {b}: no such revision"))?,
        None => repo::base(top, o.into.as_deref().unwrap_or(&p.target))?,
    };
    let (findings, passes) = load_ledger(top, &branch);
    let known = findings.len();
    let carried: Vec<String> = findings.iter().filter(|f| f.status == "open").map(|f| f.id.clone()).collect();
    let mut run = Run {
        top,
        p,
        o,
        b,
        log,
        base,
        branch: branch.clone(),
        level: level.clone(),
        tree: String::new(),
        diff: String::new(),
        paths: Vec::new(),
        receipt: None,
        findings,
        passes,
        gaps: Vec::new(),
    };
    run.pin()?;
    if run.diff.trim().is_empty() {
        return Ok(format!(
            "review: nothing to review: tree {} is the base {}\n",
            verify::short(&run.tree),
            verify::short(&run.base)
        ));
    }
    let start_tree = run.tree.clone();
    let earlier: Vec<Value> =
        run.passes.iter().filter(|x| x["tree"] == start_tree.as_str()).cloned().collect();
    let views = match o.viewpoints.is_empty() {
        true => {
            let (scores, boundary) = signals(&run.diff, &run.paths);
            pick(&level, &scores, &boundary)
        }
        false => o.viewpoints.clone(),
    };
    if let Some(bad) = views.iter().find(|v| !VIEWPOINTS.iter().any(|(k, _)| k == *v)) {
        return Err(format!(
            "unknown viewpoint `{bad}` (one of {})",
            VIEWPOINTS.iter().map(|(k, _)| *k).collect::<Vec<_>>().join(", ")
        ));
    }
    let briefs = run.briefs(&views);
    let mut new_open = run.pass(&briefs, 0);
    let mut rounds = 0;
    while !o.read_only && rounds < o.rounds && run.findings.iter().any(|f| f.status == "open") {
        rounds += 1;
        match run.remediate() {
            Ok(true) => {}
            Ok(false) => {
                run.gaps.push("the fixer changed nothing for the open findings".into());
                break;
            }
            Err(e) => {
                run.gaps.push(format!("remediation failed: {e}"));
                break;
            }
        }
        if let Err(e) = super::tidy::format(top, p, Some(&run.base), o.limit) {
            run.gaps.push(format!("formatting the fixes failed: {e}"));
        }
        run.pin()?;
        let touched: Vec<String> = run
            .findings
            .iter()
            .filter(|f| f.status == "open")
            .flat_map(|f| f.viewpoint.split('+').map(str::to_string).collect::<Vec<_>>())
            .collect();
        let again: Vec<(String, String)> =
            briefs.iter().filter(|(v, _)| touched.contains(v)).cloned().collect();
        new_open = run.pass(&again, rounds);
    }
    let green = run.receipt.as_ref().is_none_or(|r| r.green());
    for f in run.findings.iter_mut().filter(|f| f.status == "resolved") {
        f.status = if green { "fixed".into() } else { "fixed-unverified".into() };
    }
    save_ledger(top, &branch, &run.findings, &run.passes)?;
    let calls = log.calls();
    let secs = started.elapsed().as_secs_f64();
    let this_run: Vec<&Finding> = run
        .findings
        .iter()
        .enumerate()
        .filter(|(i, f)| *i >= known || carried.contains(&f.id))
        .map(|(_, f)| f)
        .collect();
    let by = |s: &str| this_run.iter().filter(|f| f.status == s).copied().collect::<Vec<_>>();
    let (fixed, unverified, open, decide, dissent, dropped) =
        (by("fixed"), by("fixed-unverified"), by("open"), by("decision"), by("dissent"), by("dropped"));
    if json_out {
        let v = json!({
            "branch": branch, "level": level, "base": run.base, "tree": run.tree, "reviewed_from": start_tree,
            "viewpoints": views, "verification": run.receipt.as_ref().map(Receipt::json),
            "findings": this_run.iter().map(|f| fjson(f)).collect::<Vec<_>>(), "gaps": run.gaps,
            "usage": calls::usage(&calls, secs), "remediation_rounds": rounds, "new_open_last_round": new_open,
        });
        return Ok(serde_json::to_string_pretty(&v).unwrap_or_default());
    }
    let mut out = format!(
        "review: {branch} at the {level} level, base {} → tree {}{}\n",
        verify::short(&run.base),
        verify::short(&run.tree),
        if run.tree != start_tree {
            format!(" (reviewed from {}, fixed in {rounds} round(s))", verify::short(&start_tree))
        } else {
            String::new()
        }
    );
    let firsts: Vec<String> = run
        .passes
        .iter()
        .rfind(|x| x["tree"] == start_tree.as_str() && x["round"] == 0)
        .into_iter()
        .flat_map(|x| x["agents"].as_array().cloned().unwrap_or_default())
        .map(|a| {
            format!(
                "{} ({})",
                a["viewpoints"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("+"),
                a["agent"].as_str().unwrap_or_default()
            )
        })
        .collect();
    out.push_str(&format!("viewpoints: {}\n", firsts.join(", ")));
    out.push_str(&format!(
        "verification: {}\n",
        run.receipt.as_ref().map_or("no checks configured or found".to_string(), Receipt::line)
    ));
    let line = |f: &Finding| {
        format!(
            "    {} [{}] {} {}: {} — impact: {}; fix: {}{}\n",
            f.id,
            f.viewpoint,
            f.severity,
            f.cite,
            f.issue,
            f.impact,
            f.fix,
            if f.note.is_empty() { String::new() } else { format!(" ({})", f.note) }
        )
    };
    for (title, list) in [
        (format!("fixed and verified on tree {}", verify::short(&run.tree)), &fixed),
        ("fixed, but the checks are not green".to_string(), &unverified),
        ("open".to_string(), &open),
        ("needs your decision".to_string(), &decide),
        ("dissent kept for you".to_string(), &dissent),
    ] {
        if !list.is_empty() {
            out.push_str(&format!("  {title}:\n"));
            for f in list {
                out.push_str(&line(f));
            }
        }
    }
    if !dropped.is_empty() {
        out.push_str(&format!(
            "  dropped: {} ({})\n",
            dropped.len(),
            dropped.iter().map(|f| format!("{}: {}", f.id, f.note)).collect::<Vec<_>>().join("; ")
        ));
    }
    if this_run.iter().all(|f| f.status == "dropped") {
        out.push_str("  no confirmed findings\n");
    }
    for g in &run.gaps {
        out.push_str(&format!("gap: {g}\n"));
    }
    out.push_str(&calls::phases(&calls, secs));
    if o.bench {
        out.push_str(&bench(&calls, &this_run, &earlier, &level, &views));
    }
    let state = match (open.is_empty() && unverified.is_empty(), decide.is_empty()) {
        (true, true) => "nothing left for you".to_string(),
        (true, false) => format!("{} decision(s) for you", decide.len()),
        (false, _) => format!(
            "{} finding(s) still open{}",
            open.len() + unverified.len(),
            if o.read_only { " (read-only review)" } else { "" }
        ),
    };
    out.push_str(&format!("-- review {}: {state}\n", if green { "done" } else { "done, checks not green" }));
    Ok(out)
}

// The benchmark's three answers: whether local work replaced hosted work, whether review and remediation closed confirmed issues before handoff, and what this level added over earlier passes on the same tree.
fn bench(
    calls: &[calls::Call],
    findings: &[&Finding],
    earlier: &[Value],
    level: &str,
    views: &[String],
) -> String {
    let hosted: Vec<&calls::Call> = calls.iter().filter(|c| c.hosted).collect();
    let local: Vec<&calls::Call> = calls.iter().filter(|c| !c.hosted).collect();
    let local_phases: Vec<&str> = local.iter().map(|c| c.phase()).fold(Vec::new(), |mut v, p| {
        if !v.contains(&p) {
            v.push(p)
        }
        v
    });
    let fetched: usize = hosted.iter().map(|c| c.rounds.saturating_sub(1)).sum();
    let mut out = String::from("benchmark:\n");
    out.push_str(&format!(
        "  1. local work: {} local call(s) ({}) cost no hosted tokens; {} hosted call(s) took {} input tokens ({} cached) and {} output; {} extra hosted round(s) were the executors fetching or rereading on their own\n",
        local.len(), if local_phases.is_empty() { "none".to_string() } else { local_phases.join(", ") }, hosted.len(),
        hosted.iter().map(|c| c.input).sum::<usize>(), hosted.iter().map(|c| c.cached).sum::<usize>(), hosted.iter().map(|c| c.tokens).sum::<usize>(), fetched
    ));
    let confirmed = findings.iter().filter(|f| !["dropped"].contains(&f.status.as_str())).count();
    let closed = findings.iter().filter(|f| f.status == "fixed").count();
    out.push_str(&format!("  2. handoff: {confirmed} confirmed finding(s), {closed} fixed and verified before handoff, {} left open, {} for the user to decide\n", findings.iter().filter(|f| f.status == "open" || f.status == "fixed-unverified").count(), findings.iter().filter(|f| f.status == "decision").count()));
    let lower: Vec<&Value> = earlier
        .iter()
        .filter(|x| LEVELS.iter().position(|l| x["level"] == *l) < LEVELS.iter().position(|l| *l == level))
        .collect();
    match lower.is_empty() {
        true => out.push_str("  3. levels: no lower level reviewed this tree, so nothing was reused\n"),
        false => {
            let before: Vec<String> = lower
                .iter()
                .flat_map(|x| x["viewpoints"].as_array().cloned().unwrap_or_default())
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect();
            let (again, new): (Vec<&String>, Vec<&String>) = views.iter().partition(|v| before.contains(v));
            out.push_str(&format!("  3. levels: {} new viewpoint(s) at {level} ({}), {} repeated from lower levels on this tree ({}) with their findings shown so they are not reported again\n", new.len(), new.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", "), again.len(), again.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")));
        }
    }
    out
}
