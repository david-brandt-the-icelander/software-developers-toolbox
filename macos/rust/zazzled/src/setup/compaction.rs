// The compaction section of `zazzle setup --report`: the autoCompactWindow that the cost model makes cheapest for the context growth the usage log shows.
use serde_json::Value;
use std::path::Path;

// Tokens in context right after start or compaction, the summary a compaction writes, and a call's own overhead beyond zazzle's answer.
const BASE: f64 = 20_000.0;
const SUMMARY: f64 = 5_000.0;
const CALL_OVERHEAD: f64 = 500.0;
const DEFAULT_GROWTH: f64 = 2_000.0;
const MARGIN: f64 = 1.15;

// Returns the cost per call, in input-token units, of compacting at window T: cached reads of the average context plus each compaction's cost spread over the calls between compactions.
pub fn per_call(b: f64, t: f64, s: f64, g: f64) -> f64 {
    0.1 * (b + t) / 2.0 + (0.1 * t + 5.0 * s + 2.0 * b) / ((t - b) / g)
}

// Returns the window, in steps of 1,000 tokens up to a million, with the lowest cost per call.
pub fn best(b: f64, s: f64, g: f64) -> f64 {
    let mut t = (b + 2.0 * g).ceil();
    let mut best = (t, per_call(b, t, s, g));
    while t < 1_000_000.0 {
        t += 1_000.0;
        let c = per_call(b, t, s, g);
        if c < best.1 {
            best = (t, c);
        }
    }
    best.0
}

// Returns the mean tokens zazzle returned per successful call in the usage log since `since_ms`, and how many calls that is.
pub fn growth(usage: &Path, since_ms: u128) -> Option<(f64, usize)> {
    let text = std::fs::read_to_string(usage).ok()?;
    let sizes: Vec<f64> = text
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| v["ts_ms"].as_u64().is_some_and(|t| u128::from(t) >= since_ms) && v["status"] == 0)
        .filter_map(|v| v["delivered_est_tokens"].as_f64())
        .collect();
    (!sizes.is_empty()).then(|| (sizes.iter().sum::<f64>() / sizes.len() as f64, sizes.len()))
}

// Writes the report section: the growth used, the cheapest window, and the recommended setting with its margin.
pub fn section(usage: &Path, since_ms: u128, current: Option<&Value>) -> String {
    let (g, from) = match growth(usage, since_ms) {
        Some((tokens, n)) => (
            tokens + CALL_OVERHEAD,
            format!(
                "{n} calls in the usage log: {tokens:.0} tokens per answer + {CALL_OVERHEAD:.0} per call"
            ),
        ),
        None => (DEFAULT_GROWTH, "no usage log entries, so a default".to_string()),
    };
    let t = best(BASE, SUMMARY, g);
    let rec = ((t * MARGIN) / 10_000.0).ceil() * 10_000.0;
    let now = current.map_or("not set by the policy".to_string(), |v| v.to_string());
    format!(
        "\nCompaction (cost per call ≈ 0.1·(B+T)/2 + (0.1·T + 5·S + 2·B)/((T−B)/g), B={BASE:.0}, S={SUMMARY:.0}):\n  growth g = {g:.0} tokens per call ({from})\n  cheapest window T ≈ {t:.0}; recommended autoCompactWindow = {rec:.0} (T plus {:.0}%); now: {now}\n  to use it: zazzle setup --set autoCompactWindow={rec:.0} --apply\n",
        (MARGIN - 1.0) * 100.0
    )
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    // Checks the search finds the model's minimum and that faster growth moves it up.
    #[test]
    fn test_best() {
        let (b, s, g) = (20_000.0, 5_000.0, 2_000.0);
        let t = best(b, s, g);
        assert!(
            per_call(b, t, s, g) <= per_call(b, t - 1_000.0, s, g)
                && per_call(b, t, s, g) <= per_call(b, t + 1_000.0, s, g)
        );
        assert!(best(b, s, 8_000.0) > t);
        assert!(t > b && t < 1_000_000.0);
    }

    // Checks growth comes from recent successful calls and the section names the recommendation.
    #[test]
    fn test_section() {
        let t = TempDir::new();
        let log = t.write("usage.jsonl", b"{\"ts_ms\":5,\"status\":0,\"delivered_est_tokens\":1000}\n{\"ts_ms\":5,\"status\":1,\"delivered_est_tokens\":9}\n{\"ts_ms\":1,\"status\":0,\"delivered_est_tokens\":7}\nnot json\n");
        assert_eq!(growth(&log, 2), Some((1000.0, 1)));
        let s = section(&log, 2, Some(&Value::from(160000)));
        assert!(
            s.contains("growth g = 1500 tokens per call (1 calls in the usage log")
                && s.contains("now: 160000"),
            "{s}"
        );
        assert!(s.contains("to use it: zazzle setup --set autoCompactWindow="), "{s}");
        let none = section(&t.path().join("missing"), 0, None);
        assert!(none.contains("no usage log entries") && none.contains("not set by the policy"), "{none}");
    }
}
