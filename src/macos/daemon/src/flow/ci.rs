// `zazzle flow ci`: waits inside zazzle for a pull request's checks on its current head and returns once, with every check's result and the failures' links; a head that moves while waiting makes the result stale instead of passing old checks off as current.
use super::deliver::run_gh;
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

// Waits until every check on the head has finished, the head moves or `limit` passes, and returns the one report.
pub fn wait(top: &Path, pr: &str, limit: Duration) -> Result<(String, bool), String> {
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
            return Ok((out, green));
        }
        std::thread::sleep(every().min(limit.saturating_sub(start.elapsed())).max(Duration::from_millis(10)));
        v = look(top, pr)?;
    }
}
