// `zazzle flow verify`: runs the repository's own checks that apply to what changed, reads their results (tests passed, failed and skipped, coverage, the first failures) and keeps a receipt pinned to the exact tree they ran on, so a changed tree never inherits an old result; logs stay in the git directory, cited by path.
use super::policy::{Check, Policy};
use super::repo;
use serde_json::{json, Value};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

// How long one check may run unless --timeout says otherwise.
pub const CHECK_SECS: u64 = 1800;

// What one check did: passed, failed, missing (its program is not installed) or not applicable; its counts, coverage, the first failures, seconds, exit code and log.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Outcome {
    pub name: String,
    pub kind: String,
    pub run: String,
    pub status: String,
    pub passed: usize,
    pub failed: usize,
    pub skipped: usize,
    pub coverage: Option<f64>,
    pub failures: Vec<String>,
    pub secs: f64,
    pub code: Option<i32>,
    pub log: String,
}

// A verification receipt: the tree and head it ran on, the branch, when, and each check's outcome.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Receipt {
    pub tree: String,
    pub head: String,
    pub branch: String,
    pub at: u64,
    pub checks: Vec<Outcome>,
}

impl Receipt {
    // True when every check that ran passed and none was missing.
    pub fn green(&self) -> bool {
        !self.checks.is_empty()
            && self.checks.iter().all(|c| c.status == "passed" || c.status == "not applicable")
    }

    // The receipt as JSON.
    pub fn json(&self) -> Value {
        json!({
            "tree": self.tree, "head": self.head, "branch": self.branch, "at": self.at, "green": self.green(),
            "checks": self.checks.iter().map(|c| json!({
                "name": c.name, "kind": c.kind, "run": c.run, "status": c.status, "passed": c.passed, "failed": c.failed,
                "skipped": c.skipped, "coverage": c.coverage, "failures": c.failures, "seconds": c.secs, "exit": c.code, "log": c.log,
            })).collect::<Vec<_>>(),
        })
    }

    // Reads a receipt back.
    pub fn from_json(v: &Value) -> Receipt {
        let s = |x: &Value| x.as_str().unwrap_or_default().to_string();
        let n = |x: &Value| x.as_u64().unwrap_or_default() as usize;
        Receipt {
            tree: s(&v["tree"]),
            head: s(&v["head"]),
            branch: s(&v["branch"]),
            at: v["at"].as_u64().unwrap_or_default(),
            checks: v["checks"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|c| Outcome {
                    name: s(&c["name"]),
                    kind: s(&c["kind"]),
                    run: s(&c["run"]),
                    status: s(&c["status"]),
                    passed: n(&c["passed"]),
                    failed: n(&c["failed"]),
                    skipped: n(&c["skipped"]),
                    coverage: c["coverage"].as_f64(),
                    failures: c["failures"].as_array().into_iter().flatten().map(s).collect(),
                    secs: c["seconds"].as_f64().unwrap_or_default(),
                    code: c["exit"].as_i64().map(|x| x as i32),
                    log: s(&c["log"]),
                })
                .collect(),
        }
    }

    // The receipt as report lines: one per check, failures and logs under failed ones.
    pub fn text(&self) -> String {
        let mut out = format!(
            "verified tree {} (HEAD {}{}):\n",
            short(&self.tree),
            short(&self.head),
            if self.branch.is_empty() { String::new() } else { format!(" on {}", self.branch) }
        );
        if self.checks.is_empty() {
            out.push_str("  no checks found: add them to .zazzle/flow.json\n");
        }
        for c in &self.checks {
            out.push_str(&format!("  {} {} ({}): {}\n", mark(&c.status), c.name, c.kind, c.summary()));
            for f in c.failures.iter().take(5) {
                out.push_str(&format!("      {f}\n"));
            }
            if c.status == "failed" {
                out.push_str(&format!("      log: {}\n", c.log));
            }
        }
        out
    }

    // One line for a delivery receipt.
    pub fn line(&self) -> String {
        let bad: Vec<String> = self
            .checks
            .iter()
            .filter(|c| c.status != "passed" && c.status != "not applicable")
            .map(|c| format!("{} {}", c.name, c.status))
            .collect();
        match bad.is_empty() {
            true => format!("{} checks passed on tree {}", self.checks.len(), short(&self.tree)),
            false => format!("not green on tree {}: {}", short(&self.tree), bad.join(", ")),
        }
    }
}

impl Outcome {
    // The counts, coverage and time in a few words.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        match self.status.as_str() {
            "missing" => return format!("not run: {}", self.failures.first().cloned().unwrap_or_default()),
            "not applicable" => return "not applicable to the changed paths".into(),
            _ => {}
        }
        if self.passed + self.failed + self.skipped > 0 {
            parts.push(format!("{} passed, {} failed, {} skipped", self.passed, self.failed, self.skipped));
        }
        if let Some(c) = self.coverage {
            parts.push(format!("coverage {c:.2}%"));
        }
        if self.status == "failed" && parts.is_empty() {
            parts.push(format!("exit {}", self.code.map_or("killed".into(), |c| c.to_string())));
        }
        parts.push(format!("{:.1} s", self.secs));
        format!("{} {}", self.status, parts.join(", "))
    }
}

// The mark for a status.
fn mark(status: &str) -> &'static str {
    match status {
        "passed" => "✓",
        "failed" => "✗",
        "not applicable" => "-",
        _ => "?",
    }
}

// A name made safe for a file name: every character that is not a letter or digit becomes `-`.
pub fn safe(name: &str) -> String {
    name.chars().map(|ch| if ch.is_alphanumeric() { ch } else { '-' }).collect()
}

// The first eight characters of an id.
pub fn short(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

// Reads the counts, coverage and first failures from a check's output: cargo, dotnet, pytest, go, jest and vitest summaries, and coverage lines.
pub fn parse(out: &str) -> (usize, usize, usize, Option<f64>, Vec<String>) {
    let (mut passed, mut failed, mut skipped, mut cov, mut fails) = (0, 0, 0, None, Vec::new());
    let num = |l: &str, word: &str| -> Option<usize> {
        let i = l.find(word)?;
        l[..i].trim_end().trim_end_matches(':').rsplit(|c: char| !c.is_ascii_digit()).next()?.parse().ok()
    };
    let after = |l: &str, word: &str| -> Option<usize> {
        let i = l.find(word)? + word.len();
        l[i..].trim_start().split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()
    };
    for l in out.lines() {
        let t = l.trim();
        if t.starts_with("test result:") {
            passed += num(t, " passed").unwrap_or(0);
            failed += num(t, " failed").unwrap_or(0);
            skipped += num(t, " ignored").unwrap_or(0);
        } else if (t.starts_with("Passed!") || t.starts_with("Failed!")) && t.contains("Failed:") {
            failed += after(t, "Failed:").unwrap_or(0);
            passed += after(t, "Passed:").unwrap_or(0);
            skipped += after(t, "Skipped:").unwrap_or(0);
        } else if t.starts_with("Tests:")
            || (t.starts_with('=') && (t.contains(" passed") || t.contains(" failed")) && t.contains(" in "))
        {
            passed += num(t, " passed").unwrap_or(0);
            failed += num(t, " failed").unwrap_or(0);
            skipped += num(t, " skipped").unwrap_or(0);
        } else if t.starts_with("--- FAIL:")
            || (t.starts_with("test ") && t.ends_with("... FAILED"))
            || t.starts_with("FAILED ")
            || t.starts_with("failed ") && t.contains("::")
        {
            fails.push(t.to_string());
        } else if t.starts_with("ok  \t") || t.starts_with("ok \t") {
            passed += 1;
        } else if t.starts_with("FAIL\t") {
            failed += 1;
        }
        let low = t.to_lowercase();
        if low.contains("coverage") || t.starts_with("TOTAL") {
            if let Some(p) = t
                .split_whitespace()
                .filter_map(|w| {
                    w.trim_end_matches(['%', ',', ';']).parse::<f64>().ok().filter(|_| w.contains('%'))
                })
                .next_back()
            {
                cov = Some(p);
            }
        }
    }
    fails.dedup();
    (passed, failed, skipped, cov, fails)
}

// Tells whether the program a command line starts with can be found: a path, a shell keyword, builtin or group, or a name on PATH.
fn installed(run: &str) -> bool {
    const SHELL: &[&str] = &[
        "sh", "bash", "cd", "env", "make", "true", "false", "test", "[", "[[", "!", "(", "{", "if", "for",
        "while", "exec", "echo", "printf", "set", "export", "exit", "return", ":", "cp", "cat",
    ];
    let first = run.split_whitespace().find(|w| !w.contains('=')).unwrap_or_default();
    if first.contains('/') || first.starts_with(['(', '{', '$']) || SHELL.contains(&first) {
        return true;
    }
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(first).is_file()))
}

// Runs one check in the top folder's `dir` with `sh -c`, the output going to `log`, stopped after `limit`.
pub fn run_check(top: &Path, c: &Check, log: &Path, limit: Duration) -> Outcome {
    let mut o = Outcome {
        name: c.name.clone(),
        kind: c.kind.clone(),
        run: c.run.clone(),
        log: log.display().to_string(),
        ..Outcome::default()
    };
    if !installed(&c.run) {
        o.status = "missing".into();
        o.failures
            .push(format!("`{}` is not installed", c.run.split_whitespace().next().unwrap_or_default()));
        return o;
    }
    let t = Instant::now();
    let spawned = Command::new("sh")
        .arg("-c")
        .arg(format!("{} 2>&1", c.run))
        .current_dir(top.join(&c.dir))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn();
    let mut child = match spawned {
        Ok(ch) => ch,
        Err(e) => {
            o.status = "failed".into();
            o.failures.push(format!("could not start: {e}"));
            return o;
        }
    };
    let mut so = child.stdout.take().expect("piped");
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = so.read_to_string(&mut s);
        s
    });
    let code = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st.code(),
            Ok(None) if t.elapsed() >= limit => {
                let _ = child.kill();
                let _ = child.wait();
                o.failures.push(format!("stopped after {} s", limit.as_secs()));
                break None;
            }
            _ => std::thread::sleep(Duration::from_millis(20)),
        }
    };
    let out = reader.join().unwrap_or_default();
    let _ = std::fs::write(log, &out);
    let (p, f, s, cov, fails) = parse(&out);
    (o.passed, o.failed, o.skipped, o.coverage, o.secs, o.code) =
        (p, f, s, cov, t.elapsed().as_secs_f64(), code);
    o.failures.extend(fails);
    o.status = if code == Some(0) && f == 0 { "passed".into() } else { "failed".into() };
    if o.status == "failed" && o.failures.is_empty() {
        o.failures.extend(
            out.lines()
                .rev()
                .filter(|l| !l.trim().is_empty())
                .take(3)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .map(|l| l.trim().to_string()),
        );
    }
    o
}

// The receipts folder.
fn receipts(top: &Path) -> Result<PathBuf, String> {
    repo::state(top, "receipts")
}

// The receipt for a tree, if one was kept.
pub fn receipt_for(top: &Path, tree: &str) -> Option<Receipt> {
    let text = std::fs::read_to_string(receipts(top).ok()?.join(format!("{tree}.json"))).ok()?;
    serde_json::from_str::<Value>(&text).ok().map(|v| Receipt::from_json(&v))
}

// Runs the checks that apply to what changed against `base` (all of them with no base, or those named in `only`) on the working tree as it stands, and keeps the receipt; generators are not checks here, since they rewrite files (`flow generate` runs them).
pub fn verify(
    top: &Path,
    p: &Policy,
    base: Option<&str>,
    only: &[String],
    limit: Duration,
) -> Result<Receipt, String> {
    let tree = repo::worktree_tree(top)?;
    let changed = match base {
        Some(b) => repo::changed(top, b, &tree)?,
        None => Vec::new(),
    };
    let logs = repo::state(top, "logs")?;
    let mut r = Receipt {
        tree: tree.clone(),
        head: repo::head(top).unwrap_or_default(),
        branch: repo::branch(top).unwrap_or_default(),
        at: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()),
        checks: Vec::new(),
    };
    let applicable = match base {
        Some(_) => p.applicable(&changed),
        None => p.checks.iter().collect(),
    };
    for c in p.checks.iter().filter(|c| c.kind != "generate") {
        if !only.is_empty() && !only.contains(&c.name) {
            continue;
        }
        if !applicable.iter().any(|a| a.name == c.name) {
            r.checks.push(Outcome {
                name: c.name.clone(),
                kind: c.kind.clone(),
                run: c.run.clone(),
                status: "not applicable".into(),
                ..Outcome::default()
            });
            continue;
        }
        let safe = safe(&c.name);
        r.checks.push(run_check(top, c, &logs.join(format!("{}-{safe}.log", short(&tree))), limit));
    }
    let path = receipts(top)?.join(format!("{tree}.json"));
    std::fs::write(&path, serde_json::to_string_pretty(&r.json()).unwrap_or_default())
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(r)
}
