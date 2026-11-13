// A thread pool started once per process, so commands can work in parallel without spawning threads per request.
use std::any::Any;
use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

// A queued unit of pool work.
type Job = Box<dyn FnOnce() + Send>;

// The job queue that the pool threads drain.
struct Pool {
    tx: Mutex<Sender<Job>>,
}

static POOL: OnceLock<Pool> = OnceLock::new();

// A pointer to the caller's work loop; it is only followed while the batch is open.
struct WorkPtr(*const (dyn Fn() + Sync));

// SAFETY: the pointee is Sync, and Batch::help follows the pointer only while the owning par_map call is still blocked in Batch::close.
unsafe impl Send for WorkPtr {}
// SAFETY: as for Send; sharing the pointer only shares access to a Sync closure.
unsafe impl Sync for WorkPtr {}

// Whether new helpers may start, how many are running, and the first panic one of them raised.
#[derive(Default)]
struct State {
    closed: bool,
    active: usize,
    panic: Option<Box<dyn Any + Send>>,
}

// One par_map call's shared state between the caller and its helper jobs.
struct Batch {
    state: Mutex<State>,
    idle: Condvar,
    work: WorkPtr,
}

impl Batch {
    // Runs the work loop as a helper, unless the caller already finished and closed the batch.
    fn help(&self) {
        {
            let mut s = self.state.lock().expect("batch lock");
            if s.closed {
                return;
            }
            s.active += 1;
        }
        // SAFETY: the batch was open when active was raised, so close() is still waiting and the closure is alive.
        let result = catch_unwind(AssertUnwindSafe(|| unsafe { (*self.work.0)() }));
        let mut s = self.state.lock().expect("batch lock");
        if let Err(p) = result {
            s.panic.get_or_insert(p);
        }
        s.active -= 1;
        if s.active == 0 {
            self.idle.notify_all();
        }
    }

    // Stops helpers from starting, waits for running ones, and returns any panic they raised.
    fn close(&self) -> Option<Box<dyn Any + Send>> {
        let mut s = self.state.lock().expect("batch lock");
        s.closed = true;
        while s.active > 0 {
            s = self.idle.wait(s).expect("batch lock");
        }
        s.panic.take()
    }
}

// Closes the batch when dropped, so a panicking caller still waits for helpers that use its closure.
struct CloseGuard<'a>(&'a Batch);

impl Drop for CloseGuard<'_> {
    // Waits for helpers before the caller's stack frame goes away.
    fn drop(&mut self) {
        self.0.close();
    }
}

// Returns the number of CPUs this process may use.
pub fn cpus() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get())
}

// Runs queued jobs until the queue closes.
fn worker(rx: &Mutex<Receiver<Job>>) {
    loop {
        let next = rx.lock().expect("pool queue").recv();
        let Ok(job) = next else { return };
        job();
    }
}

// Starts the pool threads; a helper that fails to start only reduces parallelism.
fn start(threads: usize) -> Pool {
    let (tx, rx) = channel::<Job>();
    let rx = Arc::new(Mutex::new(rx));
    for _ in 0..threads {
        let rx = Arc::clone(&rx);
        let _ = std::thread::Builder::new().name("zazzled-pool".into()).spawn(move || worker(&rx));
    }
    Pool { tx: Mutex::new(tx) }
}

// Queues a job on the pool, starting the pool on first use.
fn submit(job: Job) {
    let pool = POOL.get_or_init(|| start(cpus()));
    let _ = pool.tx.lock().expect("pool sender").send(job);
}

// Applies f to every item on the caller plus up to workers - 1 pool threads, and returns the results in item order.
pub fn par_map<T: Sync, R: Send>(items: &[T], workers: usize, f: impl Fn(usize, &T) -> R + Sync) -> Vec<R> {
    let helpers = workers.min(items.len()).saturating_sub(1);
    if helpers == 0 {
        return items.iter().enumerate().map(|(i, t)| f(i, t)).collect();
    }
    let next = AtomicUsize::new(0);
    let slots: Vec<Mutex<Option<R>>> = items.iter().map(|_| Mutex::new(None)).collect();
    let work = || loop {
        let i = next.fetch_add(1, Ordering::Relaxed);
        let Some(item) = items.get(i) else { return };
        let r = f(i, item);
        *slots[i].lock().expect("result slot") = Some(r);
    };
    let local: &(dyn Fn() + Sync) = &work;
    // SAFETY: only the lifetime is erased; CloseGuard keeps this frame alive until every helper using the pointer has returned.
    let ptr: *const (dyn Fn() + Sync + 'static) = unsafe { std::mem::transmute(local) };
    let batch =
        Arc::new(Batch { state: Mutex::new(State::default()), idle: Condvar::new(), work: WorkPtr(ptr) });
    for _ in 0..helpers {
        let b = Arc::clone(&batch);
        submit(Box::new(move || b.help()));
    }
    let guard = CloseGuard(&batch);
    work();
    std::mem::forget(guard);
    if let Some(p) = batch.close() {
        resume_unwind(p);
    }
    slots.into_iter().map(|s| s.into_inner().expect("result slot").expect("every item computed")).collect()
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    // Checks results keep item order on the pool and on the single-thread path.
    #[test]
    fn test_par_map_keeps_order() {
        let items: Vec<usize> = (0..1000).collect();
        let many = par_map(&items, 4, |i, x| (i, x * 2));
        assert!(many.iter().enumerate().all(|(i, &(j, v))| i == j && v == i * 2));
        assert_eq!(par_map(&items, 1, |_, x| x + 1)[999], 1000);
        assert!(par_map(&Vec::<u8>::new(), 4, |_, x| *x).is_empty());
    }

    // Checks par_map called from inside par_map work finishes instead of deadlocking on a busy pool.
    #[test]
    fn test_par_map_nested() {
        let outer: Vec<usize> = (0..16).collect();
        let sums = par_map(&outer, 8, |_, &n| {
            par_map(&(0..n).collect::<Vec<_>>(), 8, |_, x| *x).iter().sum::<usize>()
        });
        assert_eq!(sums[15], 105);
    }

    // Checks a panic in the work reaches the caller.
    #[test]
    fn test_par_map_propagates_panic() {
        let items: Vec<usize> = (0..64).collect();
        let r = catch_unwind(|| par_map(&items, 4, |_, &x| if x == 63 { panic!("boom") } else { x }));
        assert!(r.is_err());
    }

    // Checks a helper records its panic and that a closed batch runs nothing.
    #[test]
    fn test_batch_help_records_panic_and_respects_close() {
        let boom = || panic!("helper boom");
        let local: &(dyn Fn() + Sync) = &boom;
        // SAFETY: the batch and the closure live until the end of this test.
        let ptr: *const (dyn Fn() + Sync + 'static) = unsafe { std::mem::transmute(local) };
        let b = Batch { state: Mutex::new(State::default()), idle: Condvar::new(), work: WorkPtr(ptr) };
        b.help();
        assert!(b.close().is_some());
        b.help();
        assert_eq!(b.state.lock().expect("lock").active, 0);
    }
}
