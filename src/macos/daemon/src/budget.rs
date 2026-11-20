// The time limit of the job the current thread works for, so blocking calls deep inside a command (the indexer's requests and retries) end with the job instead of running on after its caller stopped waiting.
use std::cell::Cell;
use std::time::{Duration, Instant};

thread_local! {
    // The current thread's job deadline, if it works for one.
    static UNTIL: Cell<Option<Instant>> = const { Cell::new(None) };
}

// Runs `f` with `deadline` as this thread's job deadline (an earlier one already set stays), restoring the previous one after.
pub fn within<R>(deadline: Instant, f: impl FnOnce() -> R) -> R {
    let prev = UNTIL.get();
    UNTIL.set(Some(prev.map_or(deadline, |p| p.min(deadline))));
    let r = f();
    UNTIL.set(prev);
    r
}

// A wait cut to the time the job has left: the wait and whether the deadline shortened it; an error when no time is left.
pub fn cap(wait: Duration) -> Result<(Duration, bool), String> {
    let Some(d) = UNTIL.get() else { return Ok((wait, false)) };
    let left = d.saturating_duration_since(Instant::now());
    match left.is_zero() {
        true => Err("the job's time limit ran out before this step".into()),
        false => Ok((left.min(wait), left < wait)),
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    // Checks a wait is cut to the deadline only inside `within`, the earlier of nested deadlines wins, and none left is an error.
    #[test]
    fn test_cap() {
        let long = Duration::from_secs(60);
        assert_eq!(cap(long), Ok((long, false)));
        within(Instant::now() + Duration::from_secs(5), || {
            let (w, cut) = cap(long).expect("left");
            assert!(cut && w <= Duration::from_secs(5));
            assert_eq!(cap(Duration::from_millis(10)), Ok((Duration::from_millis(10), false)));
            within(Instant::now() + Duration::from_secs(50), || {
                assert!(cap(long).expect("inner").0 <= Duration::from_secs(5))
            });
        });
        within(Instant::now(), || assert!(cap(long).unwrap_err().contains("time limit")));
        assert_eq!(cap(long), Ok((long, false)));
    }
}
