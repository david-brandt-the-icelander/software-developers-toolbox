// What the current thread's job can be stopped by: its deadline and an explicit cancel, so blocking calls deep inside a command (the indexer's requests and retries, a model's answer, a hosted agent) end with the job instead of running on after its caller stopped waiting or cancelled it.
use std::cell::{Cell, RefCell};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

thread_local! {
    // The current thread's job deadline, if it works for one.
    static UNTIL: Cell<Option<Instant>> = const { Cell::new(None) };
    // The current thread's job's cancel handle, if it has one.
    static STOP: RefCell<Option<Stop>> = const { RefCell::new(None) };
    // What the current thread's job shares across its threads (its lookups and evidence), if it has any.
    static SHARED: RefCell<Option<Arc<crate::investigate::cache::Shared>>> = const { RefCell::new(None) };
}

// A job's cancel handle: a flag every thread of the job sees, and the connections to shut when it is set.
#[derive(Clone, Default)]
pub struct Stop {
    flag: Arc<AtomicBool>,
    open: Arc<Mutex<Vec<(u64, UnixStream)>>>,
}

// Numbers the watched connections so each guard forgets only its own.
static WATCHED: AtomicU64 = AtomicU64::new(0);

impl Stop {
    // Cancels the job: nothing new starts, and its open connections are shut so their waits end now.
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
        for (_, s) in self.open.lock().unwrap_or_else(|p| p.into_inner()).drain(..) {
            let _ = s.shutdown(std::net::Shutdown::Both);
        }
    }

    // Whether the job was cancelled.
    pub fn cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }
}

// What a thread carries for its job, to hand to the threads it starts.
#[derive(Clone, Default)]
pub struct Carried(Option<Instant>, Option<Stop>, Option<Arc<crate::investigate::cache::Shared>>);

impl Carried {
    // Whether the carried job may start no new work: it was cancelled or its deadline passed (for threads that do not carry it).
    pub fn spent(&self) -> bool {
        self.1.as_ref().is_some_and(Stop::cancelled) || self.0.is_some_and(|d| Instant::now() >= d)
    }
}

// The current thread's job, to carry into a thread it starts.
pub fn current() -> Carried {
    Carried(UNTIL.get(), STOP.with_borrow(Clone::clone), SHARED.with_borrow(Clone::clone))
}

// Runs `f` as part of the carried job.
pub fn carry<R>(c: Carried, f: impl FnOnce() -> R) -> R {
    let (prev_until, prev_stop, prev_shared) = (UNTIL.replace(c.0), STOP.replace(c.1), SHARED.replace(c.2));
    let r = f();
    UNTIL.set(prev_until);
    STOP.set(prev_stop);
    SHARED.set(prev_shared);
    r
}

// Runs `f` with `shared` as what the current job's threads share.
pub fn sharing<R>(shared: Arc<crate::investigate::cache::Shared>, f: impl FnOnce() -> R) -> R {
    let prev = SHARED.replace(Some(shared));
    let r = f();
    SHARED.set(prev);
    r
}

// What the current job's threads share, if it shares anything.
pub fn shared() -> Option<Arc<crate::investigate::cache::Shared>> {
    SHARED.with_borrow(Clone::clone)
}

// Whether the current job may start no new work: it was cancelled or its deadline passed.
pub fn spent() -> bool {
    current().spent()
}

// Runs `f` with `deadline` as this thread's job deadline (an earlier one already set stays), restoring the previous one after.
pub fn within<R>(deadline: Instant, f: impl FnOnce() -> R) -> R {
    let prev = UNTIL.get();
    UNTIL.set(Some(prev.map_or(deadline, |p| p.min(deadline))));
    let r = f();
    UNTIL.set(prev);
    r
}

// Runs `f` as the job `stop` cancels.
pub fn stopping<R>(stop: &Stop, f: impl FnOnce() -> R) -> R {
    let prev = STOP.replace(Some(stop.clone()));
    let r = f();
    STOP.set(prev);
    r
}

// Whether the current thread's job was cancelled.
pub fn cancelled() -> bool {
    STOP.with_borrow(|s| s.as_ref().is_some_and(Stop::cancelled))
}

// Whether no new work may start: the deadline has passed or the job was cancelled.
pub fn over(deadline: Instant) -> bool {
    Instant::now() >= deadline || cancelled()
}

// Why no new work may start, in the words a stop reason uses.
pub fn why_over() -> &'static str {
    if cancelled() {
        "the run was cancelled"
    } else {
        "the time limit ran out"
    }
}

// A wait cut to the time the job has left: the wait and whether the deadline shortened it; an error when no time is left or the job was cancelled.
pub fn cap(wait: Duration) -> Result<(Duration, bool), String> {
    if cancelled() {
        return Err("the run was cancelled before this step".into());
    }
    let Some(d) = UNTIL.get() else { return Ok((wait, false)) };
    let left = d.saturating_duration_since(Instant::now());
    match left.is_zero() {
        true => Err("the job's time limit ran out before this step".into()),
        false => Ok((left.min(wait), left < wait)),
    }
}

// Keeps a connection shut-able by the current job's cancel while the guard lives; one opened after the cancel is shut at once.
pub fn watch(stream: &UnixStream) -> Watch {
    let (stop, id) = (STOP.with_borrow(Clone::clone), WATCHED.fetch_add(1, Ordering::Relaxed));
    if let (Some(s), Ok(c)) = (&stop, stream.try_clone()) {
        s.open.lock().unwrap_or_else(|p| p.into_inner()).push((id, c));
        if s.cancelled() {
            s.cancel();
        }
    }
    Watch(stop, id)
}

// A watched connection: the job's cancel handle and the connection's number.
pub struct Watch(Option<Stop>, u64);

impl Drop for Watch {
    // Forgets this connection once the call that opened it ends.
    fn drop(&mut self) {
        if let Some(s) = &self.0 {
            s.open.lock().unwrap_or_else(|p| p.into_inner()).retain(|(id, _)| *id != self.1);
        }
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

    // Checks a cancel is seen by the job's threads (carried into them), stops new work, and shuts the job's watched connections, one watched after the cancel included.
    #[test]
    fn test_cancel() {
        let stop = Stop::default();
        let far = Instant::now() + Duration::from_secs(60);
        assert!(!over(far) && !cancelled() && why_over() == "the time limit ran out");
        let (a, mut b) = UnixStream::pair().expect("pair");
        stopping(&stop, || {
            let _w = watch(&a);
            let carried = current();
            let seen = std::thread::spawn(move || carry(carried, cancelled));
            assert!(!seen.join().expect("thread"));
            stop.cancel();
            let carried = current();
            assert!(std::thread::spawn(move || carry(carried, || over(far))).join().expect("thread"));
            assert_eq!(
                (why_over(), cap(Duration::from_secs(1)).unwrap_err().contains("cancelled")),
                ("the run was cancelled", true)
            );
            let (c, mut d) = UnixStream::pair().expect("pair");
            let _late = watch(&c);
            assert_eq!(
                std::io::Read::read(&mut d, &mut [0u8; 1]).expect("shut"),
                0,
                "watched after the cancel: shut at once"
            );
        });
        assert_eq!(
            std::io::Read::read(&mut b, &mut [0u8; 1]).expect("shut"),
            0,
            "the cancel shut the open connection"
        );
        assert!(!cancelled(), "outside the job");
    }
}
