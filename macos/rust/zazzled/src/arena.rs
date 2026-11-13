// Per-thread pooled arenas for tree-sitter: a parse's allocations bump through a reserved range with size-class free lists, and the range resets after the parse.
use std::cell::Cell;
use std::ffi::c_void;
use std::ptr::null_mut;
use std::sync::Once;

// Address space reserved per thread; only touched pages use memory, and 1 MiB sources need at most about 100 MB.
const RESERVE: usize = 256 << 20;
// Touched memory kept between parses; 99% of parses peak below 5 MB.
const KEEP: usize = 5 << 20;
const HEADER: usize = 16;
const CLASSES: usize = 7;

static INSTALL: Once = Once::new();

thread_local! {
    // This thread's arena; a const, drop-free cell, so the allocation hooks read it as a plain thread-local load.
    static ARENA: Cell<*mut Arena> = const { Cell::new(null_mut()) };
    // Frees this thread's arena when the thread ends; touched only when the arena is created, never on the hot path.
    static OWNER: Owner = const { Owner };
}

// One thread's arena: the reserved range, the bump offset, free lists per size class, the live block count and whether a parse is running.
struct Arena {
    base: *mut u8,
    cap: usize,
    top: usize,
    high: usize,
    free: [*mut u8; CLASSES],
    live: usize,
    active: bool,
}

// Releases the thread's arena at thread exit.
struct Owner;

impl Drop for Owner {
    // Unmaps the arena, unless it was abandoned because blocks outlived a parse.
    fn drop(&mut self) {
        let a = ARENA.replace(null_mut());
        if !a.is_null() {
            // SAFETY: a came from Box::into_raw in create and is no longer reachable through ARENA.
            drop(unsafe { Box::from_raw(a) });
        }
    }
}

// Returns the size class (payload 16 << class bytes) for a request, or None if it is too big for the arena.
fn class_of(n: usize) -> Option<usize> {
    let c = n.max(16).next_power_of_two().trailing_zeros() as usize - 4;
    (c < CLASSES).then_some(c)
}

// Maps fresh zeroed pages at addr (or anywhere when addr is null) and returns them, or null on failure.
fn map(addr: *mut u8, len: usize) -> *mut u8 {
    #[cfg(target_os = "linux")]
    let extra = libc::MAP_NORESERVE;
    #[cfg(not(target_os = "linux"))]
    let extra = 0;
    let fixed = if addr.is_null() { 0 } else { libc::MAP_FIXED };
    let flags = libc::MAP_PRIVATE | libc::MAP_ANON | extra | fixed;
    // SAFETY: anonymous mapping; with MAP_FIXED, addr lies inside this arena's own reservation, which nothing else uses.
    let p = unsafe { libc::mmap(addr.cast(), len, libc::PROT_READ | libc::PROT_WRITE, flags, -1, 0) };
    if p == libc::MAP_FAILED {
        null_mut()
    } else {
        p.cast()
    }
}

// Returns the system page size.
fn page_size() -> usize {
    // SAFETY: sysconf has no preconditions.
    let n = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    usize::try_from(n).unwrap_or(4096)
}

impl Arena {
    // Reserves cap bytes of address space, or returns None if the system refuses.
    fn new(cap: usize) -> Option<Arena> {
        let base = map(null_mut(), cap);
        (!base.is_null()).then_some(Arena {
            base,
            cap,
            top: 0,
            high: 0,
            free: [null_mut(); CLASSES],
            live: 0,
            active: false,
        })
    }

    // Returns true if p was handed out by this arena.
    fn contains(&self, p: *const u8) -> bool {
        (p as usize).wrapping_sub(self.base as usize) < self.cap
    }

    // Hands out a block of at least n bytes while a parse is running, or null to fall back to the system allocator.
    fn alloc(&mut self, n: usize) -> *mut u8 {
        let Some(c) = class_of(n).filter(|_| self.active) else { return null_mut() };
        let p = self.free[c];
        if !p.is_null() {
            // SAFETY: a freed payload holds the next free pointer in its first bytes, written by release.
            self.free[c] = unsafe { p.cast::<*mut u8>().read() };
            self.live += 1;
            return p;
        }
        let size = HEADER + (16 << c);
        if self.top + size > self.cap {
            return null_mut();
        }
        // SAFETY: top + size is within the reservation, so the header and payload lie inside mapped memory.
        let p = unsafe {
            let h = self.base.add(self.top);
            h.cast::<usize>().write(c);
            h.add(HEADER)
        };
        self.top += size;
        self.high = self.high.max(self.top);
        self.live += 1;
        p
    }

    // Returns the payload size of a block this arena handed out.
    fn payload(p: *const u8) -> usize {
        // SAFETY: p came from alloc, so the class written by alloc sits HEADER bytes before it.
        16 << unsafe { p.sub(HEADER).cast::<usize>().read() }
    }

    // Puts a block back on its size class's free list.
    fn release(&mut self, p: *mut u8) {
        let c = (Arena::payload(p) / 16).trailing_zeros() as usize;
        // SAFETY: payloads are at least 16 bytes, so the next pointer fits; the block is no longer in use.
        unsafe { p.cast::<*mut u8>().write(self.free[c]) };
        self.free[c] = p;
        self.live -= 1;
    }

    // Empties the arena for the next parse and returns touched pages beyond KEEP to the system.
    fn reset(&mut self) {
        if self.high > KEEP {
            let page = page_size();
            let end = (self.high.div_ceil(page) * page).min(self.cap);
            // SAFETY: KEEP..end lies inside the reservation; remapping replaces those pages with fresh untouched ones.
            map(unsafe { self.base.add(KEEP) }, end - KEEP);
        }
        self.top = 0;
        self.high = 0;
        self.free = [null_mut(); CLASSES];
        self.live = 0;
    }
}

impl Drop for Arena {
    // Releases the reservation.
    fn drop(&mut self) {
        // SAFETY: base and cap describe the mapping created in new, and no block is used after its parse ends.
        unsafe { libc::munmap(self.base.cast(), self.cap) };
    }
}

// Returns this thread's arena pointer, null if it has none or the thread is shutting down.
fn current() -> *mut Arena {
    ARENA.try_with(Cell::get).unwrap_or(null_mut())
}

// Tree-sitter's malloc: the arena during a parse, else the system allocator.
unsafe extern "C" fn ts_malloc(n: usize) -> *mut c_void {
    let a = current();
    // SAFETY: a is this thread's arena; the hooks never re-enter, so this is the only reference while it runs.
    let p = if a.is_null() { null_mut() } else { unsafe { (*a).alloc(n) } };
    if p.is_null() {
        // SAFETY: plain libc allocation.
        unsafe { libc::malloc(n) }
    } else {
        p.cast()
    }
}

// Tree-sitter's calloc: zeroed memory from the arena during a parse, else from the system allocator.
unsafe extern "C" fn ts_calloc(k: usize, size: usize) -> *mut c_void {
    let Some(n) = k.checked_mul(size) else { return null_mut() };
    let a = current();
    // SAFETY: as in ts_malloc.
    let p = if a.is_null() { null_mut() } else { unsafe { (*a).alloc(n) } };
    if p.is_null() {
        // SAFETY: plain libc allocation.
        return unsafe { libc::calloc(k, size) };
    }
    // SAFETY: the block holds at least n bytes and may be a reused free-list block, so it must be cleared.
    unsafe { p.write_bytes(0, n) };
    p.cast()
}

// Tree-sitter's free: arena blocks go back on their free list, others to the system allocator.
unsafe extern "C" fn ts_free(p: *mut c_void) {
    if p.is_null() {
        return;
    }
    let a = current();
    // SAFETY: as in ts_malloc; contains only compares addresses.
    if !a.is_null() && unsafe { (*a).contains(p.cast()) } {
        // SAFETY: p is a live block of this arena.
        unsafe { (*a).release(p.cast()) };
    } else {
        // SAFETY: p is not an arena block, so it came from libc.
        unsafe { libc::free(p) };
    }
}

// Tree-sitter's realloc: arena blocks grow by copying into a new block, others use the system allocator.
unsafe extern "C" fn ts_realloc(p: *mut c_void, n: usize) -> *mut c_void {
    let a = current();
    // SAFETY: as in ts_malloc; contains only compares addresses.
    if p.is_null() || a.is_null() || !unsafe { (*a).contains(p.cast()) } {
        // SAFETY: p is null or a libc block; realloc(NULL, n) allocates.
        return if p.is_null() { unsafe { ts_malloc(n) } } else { unsafe { libc::realloc(p, n) } };
    }
    let old = Arena::payload(p.cast());
    if n <= old {
        return p;
    }
    // SAFETY: new and p are distinct live blocks of at least n and old bytes, with old < n.
    unsafe {
        let new = ts_malloc(n);
        if !new.is_null() {
            std::ptr::copy_nonoverlapping(p.cast::<u8>(), new.cast::<u8>(), old);
            ts_free(p);
        }
        new
    }
}

// Points tree-sitter at the arena-aware allocator; its system path is plain libc, so objects allocated before stay valid.
fn install() {
    // SAFETY: the hooks are thread-safe and compatible with the default libc allocator.
    unsafe { tree_sitter::set_allocator(Some(ts_malloc), Some(ts_calloc), Some(ts_realloc), Some(ts_free)) };
}

// Returns this thread's arena, creating it and registering its cleanup on first use; null if the reservation fails.
fn create() -> *mut Arena {
    let a = current();
    if !a.is_null() {
        return a;
    }
    let Some(arena) = Arena::new(RESERVE) else { return null_mut() };
    OWNER.with(|_| ());
    let a = Box::into_raw(Box::new(arena));
    ARENA.set(a);
    a
}

// Ends a parse scope: deactivates the arena and resets it, or abandons it if a block is still live.
struct ScopeGuard(*mut Arena);

impl Drop for ScopeGuard {
    // Runs on normal exit and on unwind, so a panicking parse never leaves the arena active.
    fn drop(&mut self) {
        // SAFETY: self.0 is this thread's arena and tree-sitter is not running, so no hook holds a reference.
        let a = unsafe { &mut *self.0 };
        a.active = false;
        if a.live == 0 {
            a.reset();
            return;
        }
        eprintln!(
            "zazzled: {} tree-sitter blocks outlived their parse; abandoning this thread's arena",
            a.live
        );
        ARENA.set(null_mut());
    }
}

// Runs f with this thread's tree-sitter allocations in its arena; everything tree-sitter allocates in f must be freed in f.
pub fn scope<R>(f: impl FnOnce() -> R) -> R {
    INSTALL.call_once(install);
    let a = create();
    // SAFETY: a is null or this thread's arena, and tree-sitter is not running, so no hook holds a reference.
    let entered = !a.is_null() && unsafe { !std::mem::replace(&mut (*a).active, true) };
    // Built only when this call activated the arena; constructing it otherwise would reset an outer scope on drop.
    let _guard = if entered { Some(ScopeGuard(a)) } else { None };
    f()
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    // Installs a fresh active arena of the given size on this test thread, runs f, then removes it.
    fn with_test_arena<R>(cap: usize, f: impl FnOnce() -> R) -> R {
        let mut a = Arena::new(cap).expect("reserve");
        a.active = true;
        let p = Box::into_raw(Box::new(a));
        let old = ARENA.replace(p);
        let r = f();
        ARENA.set(old);
        // SAFETY: p came from Box::into_raw above and is no longer installed.
        drop(unsafe { Box::from_raw(p) });
        r
    }

    // Returns this thread's arena, which the test requires to exist.
    fn arena() -> &'static mut Arena {
        // SAFETY: tests read the arena only while no tree-sitter call is running on this thread.
        unsafe { &mut *current() }
    }

    // Returns whether p lies in this thread's arena.
    fn in_arena(p: *mut c_void) -> bool {
        !current().is_null() && arena().contains(p.cast())
    }

    // Checks size classes cover 16 to 1024 bytes and reject larger requests.
    #[test]
    fn test_class_of_bounds() {
        assert_eq!(
            (class_of(0), class_of(16), class_of(17), class_of(1024)),
            (Some(0), Some(0), Some(1), Some(6))
        );
        assert_eq!(class_of(1025), None);
    }

    // Checks blocks come from the arena, are reused after free, and calloc clears reused blocks.
    #[test]
    fn test_hooks_alloc_free_reuse_and_calloc() {
        with_test_arena(1 << 20, || unsafe {
            let a = ts_malloc(100);
            assert!(in_arena(a));
            a.cast::<u8>().write_bytes(0xab, 100);
            ts_free(a);
            let b = ts_calloc(10, 10);
            assert_eq!(a, b);
            assert!((0..100).all(|i| *b.cast::<u8>().add(i) == 0));
            ts_free(b);
            assert_eq!(arena().live, 0);
            assert!(ts_calloc(usize::MAX, 2).is_null());
        });
    }

    // Checks large requests, exhaustion and inactive arenas fall back to the system allocator.
    #[test]
    fn test_hooks_fall_back_to_system() {
        with_test_arena(4096, || unsafe {
            let big = ts_malloc(4096);
            assert!(!big.is_null() && !in_arena(big));
            ts_free(big);
            let blocks: Vec<*mut c_void> = (0..40).map(|_| ts_malloc(100)).collect();
            assert!(blocks.iter().any(|p| !in_arena(*p)));
            blocks.into_iter().for_each(|p| ts_free(p));
            arena().active = false;
            let off = ts_calloc(4, 4);
            assert!(!in_arena(off));
            ts_free(off);
            ts_free(null_mut());
        });
    }

    // Checks realloc keeps a block that still fits, copies when growing, and passes system blocks through.
    #[test]
    fn test_hooks_realloc() {
        with_test_arena(1 << 20, || unsafe {
            let p = ts_realloc(null_mut(), 20);
            assert!(in_arena(p));
            p.cast::<u8>().write_bytes(7, 20);
            assert_eq!(ts_realloc(p, 32), p);
            let q = ts_realloc(p, 200);
            assert!(q != p && in_arena(q) && *q.cast::<u8>().add(19) == 7);
            let r = ts_realloc(q, 5000);
            assert!(!in_arena(r) && *r.cast::<u8>().add(19) == 7);
            let s = ts_realloc(r, 10000);
            assert!(!s.is_null() && *s.cast::<u8>().add(19) == 7);
            ts_free(s);
            assert_eq!(arena().live, 0);
        });
    }

    // Checks the hooks use the system allocator on a thread that has no arena.
    #[test]
    fn test_hooks_without_arena() {
        std::thread::spawn(|| unsafe {
            assert!(current().is_null());
            let p = ts_realloc(ts_calloc(2, 8), 64);
            assert!(!p.is_null());
            ts_free(p);
        })
        .join()
        .expect("thread");
    }

    // Checks a scope resets the arena, gives pages beyond KEEP back, and that nested scopes leave the outer one active.
    #[test]
    fn test_scope_resets_and_nests() {
        scope(|| {
            assert!(scope(|| arena().active));
            let v: Vec<*mut c_void> =
                (0..((KEEP / 1040) + 100)).map(|_| unsafe { ts_malloc(1024) }).collect();
            assert!(v.iter().all(|p| in_arena(*p)));
            assert!(arena().high > KEEP);
            v.iter().for_each(|p| unsafe { ts_free(*p) });
        });
        let a = arena();
        assert_eq!((a.active, a.top, a.high, a.live), (false, 0, 0, 0));
    }

    // Checks a block that outlives its scope makes the thread abandon that arena and start a new one.
    #[test]
    fn test_scope_abandons_arena_with_live_blocks() {
        let leaked = scope(|| unsafe { ts_malloc(64) });
        assert!(!leaked.is_null() && current().is_null());
        scope(|| assert!(arena().active));
        assert!(!current().is_null());
    }

    // Checks a panic inside a scope still deactivates and resets the arena.
    #[test]
    fn test_scope_survives_panic() {
        let r = std::panic::catch_unwind(|| scope(|| panic!("parse panicked")));
        assert!(r.is_err());
        assert!(!arena().active);
    }
}
