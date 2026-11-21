// What one investigate job's threads share: the lookups and declaration scans already run, so parallel parts and later turns never run one twice (a caller asking for one still running waits for it), and where the job's evidence is kept in the run store as it is found.
use super::discover::Candidate;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Condvar, Mutex};
use std::time::Duration;

// How long a wait for another thread's lookup sleeps before it checks whether the job was stopped.
const WAIT_STEP: Duration = Duration::from_millis(50);

// One job's shared state: its lookups' outputs (text, status and the rules notes), its declaration scans, and its evidence file.
#[derive(Default)]
pub struct Shared {
    pub lookups: Cache<(String, u8, String)>,
    pub decls: Cache<Vec<Candidate>>,
    evidence: Option<PathBuf>,
}

impl Shared {
    // A job's shared state, keeping evidence in `evidence` when the job has a run.
    pub fn new(evidence: Option<PathBuf>) -> Shared {
        Shared { evidence, ..Shared::default() }
    }

    // Appends one evidence record to the run store, when the job has a run.
    pub fn checkpoint(&self, v: &Value) {
        if let Some(p) = &self.evidence {
            let _ = crate::runs::append(p, v);
        }
    }
}

// Appends one evidence record for the current job, when it keeps any.
pub fn checkpoint(v: Value) {
    if let Some(s) = crate::budget::shared() {
        s.checkpoint(&v);
    }
}

// Values by key, each computed once per job: a definitive result is kept, a transient one is dropped so the next caller tries again, and a caller asking for one another thread is computing waits for it.
pub struct Cache<V> {
    slots: Mutex<HashMap<String, Slot<V>>>,
    ready: Condvar,
}

impl<V> Default for Cache<V> {
    // An empty cache.
    fn default() -> Self {
        Cache { slots: Mutex::default(), ready: Condvar::new() }
    }
}

// A key's state: being computed, or its kept value.
enum Slot<V> {
    Running,
    Done(V),
}

// A key being computed by this thread: on drop (also on a panic) it keeps the value if one was set, else frees the key, and wakes the waiters.
struct Pending<'a, V> {
    cache: &'a Cache<V>,
    key: &'a str,
    kept: Option<V>,
}

impl<V> Drop for Pending<'_, V> {
    // Ends the computation: keeps or frees the key and wakes everyone waiting on it.
    fn drop(&mut self) {
        let mut slots = self.cache.slots.lock().unwrap_or_else(|p| p.into_inner());
        match self.kept.take() {
            Some(v) => slots.insert(self.key.to_string(), Slot::Done(v)),
            None => slots.remove(self.key),
        };
        self.cache.ready.notify_all();
    }
}

impl<V: Clone> Cache<V> {
    // The value for `key` and whether another lookup had already computed it: a kept one, the one another thread is computing (waited for), or `run`'s, kept when `run` says it is definitive; an error when the job was stopped during the wait.
    pub fn get_or(&self, key: &str, run: impl FnOnce() -> (V, bool)) -> Result<(V, bool), String> {
        let mut slots = self.slots.lock().unwrap_or_else(|p| p.into_inner());
        loop {
            match slots.get(key) {
                Some(Slot::Done(v)) => return Ok((v.clone(), true)),
                Some(Slot::Running) => {
                    let (wait, _) = crate::budget::cap(WAIT_STEP)?;
                    slots = self.ready.wait_timeout(slots, wait).unwrap_or_else(|p| p.into_inner()).0;
                }
                None => break,
            }
        }
        slots.insert(key.to_string(), Slot::Running);
        drop(slots);
        let mut pending = Pending { cache: self, key, kept: None };
        let (v, keep) = run();
        if keep {
            pending.kept = Some(v.clone());
        }
        drop(pending);
        Ok((v, false))
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Instant;

    // Checks a definitive value is computed once and shared, a transient one is tried again, threads asking for one in flight wait for it, a panic frees its key, and a wait ends when the job is cancelled.
    #[test]
    fn test_cache() {
        let c: Cache<String> = Cache::default();
        let runs = AtomicUsize::new(0);
        let count = |keep: bool| {
            runs.fetch_add(1, Ordering::SeqCst);
            ("v".to_string(), keep)
        };
        assert_eq!(c.get_or("a", || count(true)), Ok(("v".into(), false)));
        assert_eq!(c.get_or("a", || count(true)), Ok(("v".into(), true)));
        assert_eq!(runs.load(Ordering::SeqCst), 1, "kept: run once");
        assert_eq!(c.get_or("b", || count(false)), Ok(("v".into(), false)));
        assert_eq!(c.get_or("b", || count(false)), Ok(("v".into(), false)), "transient: tried again");
        assert_eq!(runs.load(Ordering::SeqCst), 3);

        let c = Arc::new(Cache::<usize>::default());
        let slow = AtomicUsize::new(0);
        std::thread::scope(|s| {
            let hs: Vec<_> = (0..4)
                .map(|_| {
                    s.spawn(|| {
                        c.get_or("k", || {
                            slow.fetch_add(1, Ordering::SeqCst);
                            std::thread::sleep(Duration::from_millis(120));
                            (7, true)
                        })
                    })
                })
                .collect();
            let got: Vec<_> = hs.into_iter().map(|h| h.join().expect("thread").expect("value")).collect();
            assert!(got.iter().all(|(v, _)| *v == 7));
            assert_eq!(
                got.iter().filter(|(_, shared)| *shared).count(),
                3,
                "three waited for the one in flight"
            );
        });
        assert_eq!(slow.load(Ordering::SeqCst), 1);

        let p = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = c.get_or("boom", || panic!("lookup failed"));
        }));
        assert!(p.is_err());
        assert_eq!(c.get_or("boom", || (1, true)), Ok((1, false)), "a panic frees the key");

        let stop = crate::budget::Stop::default();
        let c = Cache::<usize>::default();
        std::thread::scope(|s| {
            s.spawn(|| {
                c.get_or("held", || {
                    std::thread::sleep(Duration::from_millis(400));
                    (1, true)
                })
            });
            std::thread::sleep(Duration::from_millis(50));
            let started = Instant::now();
            let waiter = s.spawn(|| crate::budget::stopping(&stop, || c.get_or("held", || (2, true))));
            std::thread::sleep(Duration::from_millis(60));
            stop.cancel();
            assert!(waiter.join().expect("waiter").unwrap_err().contains("cancelled"));
            assert!(started.elapsed() < Duration::from_millis(350), "the wait ended with the cancel");
        });
    }

    // Checks evidence is appended only for a job with a run, and only inside that job.
    #[test]
    fn test_checkpoint() {
        let t = crate::testutil::TempDir::new();
        let path = t.path().join("evidence.jsonl");
        checkpoint(serde_json::json!({"outside": true}));
        crate::budget::sharing(Arc::new(Shared::new(Some(path.clone()))), || {
            checkpoint(serde_json::json!({"lookup": "READ a.cs"}))
        });
        crate::budget::sharing(Arc::new(Shared::new(None)), || checkpoint(serde_json::json!({"x": 1})));
        assert_eq!(std::fs::read_to_string(&path).expect("evidence"), "{\"lookup\":\"READ a.cs\"}\n");
    }
}
