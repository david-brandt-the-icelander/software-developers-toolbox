// `zazzle flow ci`: waits inside zazzle for a pull request's checks on its current head and returns once, with every check's result and the failures' links; a head that moves while waiting makes the result stale instead of passing old checks off as current. A failed job's log is fetched and triaged by zazzle and the local model, so the executor reads the failing test, its place and its likely cause, not the log.
use super::deliver::run_gh;
use crate::adaptive::agent::Rung;
use crate::adaptive::calls::{Ask, Backend, Log};
use serde_json::Value;
use std::path::Path;
use std::time::{Duration, Instant};

// Seconds between looks at the checks: $ZAZZLE_CI_POLL_SECS, default 15.
fn every() -> Duration {
    Duration::from_millis(
        std::env::var("ZAZZLE_CI_POLL_SECS")
            .ok()
            .and_then(|s| s.parse::<f64>().ok())
            .map_or(15_000, |s| (s * 1000.0) as u64),
    )
}

// The pull request's head and checks, as gh reports them.
fn look(top: &Path, pr: &str) -> Result<Value, String> {
    let mut args = vec!["pr", "view"];
    if !pr.is_empty() {
        args.push(pr);
    }
    args.extend(["--json", "number,url,headRefOid,isDraft,statusCheckRollup"]);
    serde_json::from_str(&run_gh(top, &args)?).map_err(|e| format!("gh pr view: {e}"))
}

// One check's name, whether it finished, and its conclusion.
fn check(c: &Value) -> (String, bool, String) {
    let name = c["name"].as_str().or(c["context"].as_str()).unwrap_or("check").to_string();
    let state = c["state"].as_str().unwrap_or_default();
    let done =
        c["status"].as_str().map_or(!matches!(state, "PENDING" | "EXPECTED" | ""), |s| s == "COMPLETED");
    let concl = c["conclusion"].as_str().filter(|s| !s.is_empty()).unwrap_or(state).to_lowercase();
    (name, done, concl)
}

// Lines of a failed log worth reading, and the most kept.
const SIGNS: &[&str] = &[
    "error[",
    "error:",
    "Error:",
    "panicked at",
    "FAILED",
    "--- FAIL",
    "##[error]",
    "assertion",
    "Traceback",
    "Exception",
    " failed",
];
const EXCERPT_LINES: usize = 80;

// What a wait found: the report, whether every check passed, and the failed checks as (name, link).
pub type Waited = (String, bool, Vec<(String, String)>);

// Waits until every check on the head has finished, the head moves or `limit` passes, and returns the one report.
pub fn wait(top: &Path, pr: &str, limit: Duration) -> Result<Waited, String> {
    let start = Instant::now();
    let first = look(top, pr)?;
    let head = first["headRefOid"].as_str().unwrap_or_default().to_string();
    let mut v = first;
    loop {
        let now = v["headRefOid"].as_str().unwrap_or_default();
        if now != head {
            return Ok((
                format!(
                    "stale: the head moved from {} to {} while waiting; wait again for the new head\n",
                    super::verify::short(&head),
                    super::verify::short(now)
                ),
                false,
                Vec::new(),
            ));
        }
        let checks: Vec<(String, bool, String)> =
            v["statusCheckRollup"].as_array().into_iter().flatten().map(check).collect();
        let pending = checks.iter().filter(|c| !c.1).count();
        if pending == 0 || start.elapsed() >= limit {
            let failed: Vec<&Value> = v["statusCheckRollup"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|c| {
                    let (_, d, k) = check(c);
                    d && !["success", "skipped", "neutral"].contains(&k.as_str())
                })
                .collect();
            let mut out = format!(
                "pr: {} #{} head {}{}\n",
                v["url"].as_str().unwrap_or_default(),
                v["number"],
                super::verify::short(&head),
                if v["isDraft"] == true { " (draft)" } else { "" }
            );
            if checks.is_empty() {
                out.push_str("checks: none reported for this head\n");
            }
            for (name, done, concl) in &checks {
                out.push_str(&format!(
                    "  {} {name}: {}\n",
                    if !done {
                        "…"
                    } else if concl == "success" {
                        "✓"
                    } else if ["skipped", "neutral"].contains(&concl.as_str()) {
                        "-"
                    } else {
                        "✗"
                    },
                    if *done { concl.as_str() } else { "still running" }
                ));
            }
            for f in &failed {
                out.push_str(&format!(
                    "  failed: {} {}\n",
                    check(f).0,
                    f["detailsUrl"].as_str().or(f["targetUrl"].as_str()).unwrap_or_default()
                ));
            }
            let green = pending == 0 && failed.is_empty();
            out.push_str(&match (pending, green) {
                (0, true) => "-- all checks passed on this head\n".to_string(),
                (0, false) => format!("-- {} check(s) failed on this head\n", failed.len()),
                (n, _) => format!("-- {n} check(s) still running after {} s; wait again\n", limit.as_secs()),
            });
            let links = failed
                .iter()
                .map(|f| {
                    (
                        check(f).0,
                        f["detailsUrl"].as_str().or(f["targetUrl"].as_str()).unwrap_or_default().to_string(),
                    )
                })
                .collect();
            return Ok((out, green, links));
        }
        std::thread::sleep(every().min(limit.saturating_sub(start.elapsed())).max(Duration::from_millis(10)));
        v = look(top, pr)?;
    }
}

// The run and job ids in an Actions job link (`.../actions/runs/RUN/job/JOB`).
pub fn job_of(url: &str) -> Option<(String, String)> {
    let rest = url.split("/actions/runs/").nth(1)?;
    let (run, job) = rest.split_once("/job/")?;
    let job: String = job.chars().take_while(char::is_ascii_digit).collect();
    (!job.is_empty()).then(|| (run.to_string(), job))
}

// A log line without gh's `job<TAB>step<TAB>` prefix and its timestamp.
fn bare(line: &str) -> &str {
    let msg = line.rsplit('\t').next().unwrap_or(line);
    match msg.split_once(' ') {
        Some((ts, rest))
            if ts.len() >= 20 && ts.ends_with('Z') && ts.starts_with(|c: char| c.is_ascii_digit()) =>
        {
            rest
        }
        _ => msg,
    }
}

// The lines of a failed log worth reading, verbatim: each error, failing test or panic with a line before and two after, at most EXCERPT_LINES.
pub fn excerpt(log: &str) -> String {
    let lines: Vec<&str> = log.lines().map(bare).collect();
    let mut keep = vec![false; lines.len()];
    for (i, l) in lines.iter().enumerate() {
        if SIGNS.iter().any(|s| l.contains(s)) {
            for k in keep.iter_mut().take((i + 3).min(lines.len())).skip(i.saturating_sub(1)) {
                *k = true;
            }
        }
    }
    let mut out = String::new();
    let mut last = None;
    for (i, l) in lines.iter().enumerate().filter(|(i, _)| keep[*i]) {
        if out.lines().count() >= EXCERPT_LINES {
            out.push_str("[more cut]\n");
            break;
        }
        if last.is_some_and(|p| p + 1 != i) {
            out.push_str("…\n");
        }
        out.push_str(l);
        out.push('\n');
        last = Some(i);
    }
    if out.is_empty() {
        out = lines.iter().rev().take(15).rev().map(|l| format!("{l}\n")).collect();
    }
    out
}

// Fetches each failed job's log through gh, keeps the lines worth reading, and has the local model say per failure the test, the place, the cause, whose it is (this change, the base, infrastructure or a flake) and the next step; places that are not in this repository are marked.
pub fn triage(
    top: &Path,
    failed: &[(String, String)],
    changed: &[String],
    b: &dyn Backend,
    log: &Log,
    r: &Rung,
    limit: Duration,
) -> String {
    let mut logs = String::new();
    for (name, url) in failed {
        let text = match job_of(url) {
            Some((run, job)) => run_gh(top, &["run", "view", &run, "--job", &job, "--log-failed"])
                .map(|l| excerpt(&l))
                .unwrap_or_else(|e| format!("(log not available: {e})\n")),
            None => "(no Actions job link; open the check's page)\n".to_string(),
        };
        logs.push_str(&format!("### {name}\n{text}"));
    }
    let system = "You triage failed CI jobs from log excerpts. Be concise; reply only with lines:\nFAILURE job | test or step | path:line or - | the cause in one sentence\nOWNER this-change|base|infrastructure|flaky | why\nNEXT the smallest next step";
    let prompt = format!(
        "Files this change touches: {}\n{logs}",
        if changed.is_empty() { "unknown".to_string() } else { changed.join(", ") }
    );
    let sections = [("brief", system), ("evidence", logs.as_str())];
    let mut out = format!("log excerpts, cut by zazzle:\n{logs}");
    match log.text(b, r, &Ask::new("ci triage", system, &prompt, &sections), top, Instant::now() + limit) {
        Ok(reply) => {
            out.push_str(&format!("triage by {}:\n", r.label()));
            for l in reply.lines().map(str::trim).filter(|l| {
                ["FAILURE", "OWNER", "NEXT"]
                    .iter()
                    .any(|w| crate::investigate::cite::strip_word(l, w).is_some())
            }) {
                let place = l.split('|').nth(2).map(str::trim).unwrap_or("-");
                let missing = place != "-" && place.split(':').next().is_some_and(|f| !top.join(f).is_file());
                out.push_str(&format!(
                    "  {l}{}\n",
                    if missing { " (not a file in this repository)" } else { "" }
                ));
            }
        }
        Err(e) => out.push_str(&format!("triage by {} failed: {e}; read the excerpts above\n", r.label())),
    }
    out
}
