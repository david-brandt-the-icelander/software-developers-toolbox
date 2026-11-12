// Batched file loading through io_uring: a few submission rounds open, stat, read and close many files at once.
use crate::fsio::{Ident, Loaded};
use io_uring::{opcode, squeue, types, IoUring, Probe};
use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

const RING_ENTRIES: u32 = 256;
const BATCH: usize = 64;
const FIRST_READ: usize = 64 * 1024;
const STAT: u64 = 1;
const READ: u64 = 2;

static UNAVAILABLE: AtomicBool = AtomicBool::new(false);
static POOL: Mutex<Vec<IoUring>> = Mutex::new(Vec::new());

// Take a ring from the pool or create one, or None when io_uring is unusable here.
fn take_ring() -> Option<IoUring> {
    if UNAVAILABLE.load(Ordering::Relaxed) {
        return None;
    }
    if let Some(ring) = POOL.lock().expect("ring pool").pop() {
        return Some(ring);
    }
    let ring = IoUring::new(RING_ENTRIES).ok();
    let supported = ring.as_ref().is_some_and(|r| {
        let mut probe = Probe::new();
        r.submitter().register_probe(&mut probe).is_ok()
            && [opcode::OpenAt::CODE, opcode::Statx::CODE, opcode::Read::CODE, opcode::Close::CODE]
                .iter()
                .all(|&code| probe.is_supported(code))
    });
    if !supported {
        UNAVAILABLE.store(true, Ordering::Relaxed);
        return None;
    }
    ring
}

// Submit entries, wait for all of them and return (user_data, result) pairs.
fn run(ring: &mut IoUring, entries: &[squeue::Entry]) -> io::Result<Vec<(u64, i32)>> {
    {
        let mut queue = ring.submission();
        for entry in entries {
            // SAFETY: the caller keeps every buffer and path the entry points to alive until this function
            // returns, and this function does not return before every completion has been received.
            unsafe { queue.push(entry) }.expect("queue is sized for a full batch");
        }
    }
    let mut done = Vec::with_capacity(entries.len());
    while done.len() < entries.len() {
        match ring.submit_and_wait(entries.len() - done.len()) {
            Err(e) if e.raw_os_error() == Some(libc::EINTR) => {}
            Err(e) => return Err(e),
            Ok(_) => {}
        }
        done.extend(ring.completion().map(|c| (c.user_data(), c.result())));
    }
    Ok(done)
}

// Open the selected paths and return (index, fd or negative errno) for each.
fn open_round(
    ring: &mut IoUring,
    paths: &[Option<CString>],
    which: &[usize],
    extra: i32,
) -> io::Result<Vec<(usize, i32)>> {
    let flags = libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NONBLOCK | extra;
    let entries: Vec<squeue::Entry> = which
        .iter()
        .map(|&i| {
            let path = paths[i].as_ref().expect("only valid paths are selected");
            opcode::OpenAt::new(types::Fd(libc::AT_FDCWD), path.as_ptr())
                .flags(flags)
                .build()
                .user_data(i as u64)
        })
        .collect();
    Ok(run(ring, &entries)?.into_iter().map(|(u, r)| (u as usize, r)).collect())
}

// Open every valid path, retrying without O_NOATIME where the kernel refuses it.
fn open_all(
    ring: &mut IoUring,
    paths: &[Option<CString>],
    fds: &mut [Option<i32>],
    results: &mut [Option<Loaded>],
) -> io::Result<()> {
    let valid: Vec<usize> = (0..paths.len()).filter(|&i| paths[i].is_some()).collect();
    let mut opened = open_round(ring, paths, &valid, libc::O_NOATIME)?;
    let denied: Vec<usize> = opened.iter().filter(|o| o.1 == -libc::EPERM).map(|o| o.0).collect();
    if !denied.is_empty() {
        let again = open_round(ring, paths, &denied, 0)?;
        opened.retain(|o| o.1 != -libc::EPERM);
        opened.extend(again);
    }
    for (i, res) in opened {
        match res {
            fd if fd >= 0 => fds[i] = Some(fd),
            errno => results[i] = Some(Err(io::Error::from_raw_os_error(-errno))),
        }
    }
    Ok(())
}

// Read the rest of a file from `buf.len()` to its end with pread.
fn read_rest(fd: i32, mut buf: Vec<u8>, size: usize) -> io::Result<Vec<u8>> {
    buf.reserve(size.saturating_sub(buf.len()) + 1);
    loop {
        let have = buf.len();
        let room = buf.capacity() - have;
        // SAFETY: the pointer and length describe the spare capacity of `buf`, which pread fills.
        let n = unsafe { libc::pread(fd, buf.as_mut_ptr().add(have).cast(), room, have as libc::off_t) };
        match n {
            0 => return Ok(buf),
            n if n > 0 => {
                // SAFETY: pread initialised `n` bytes of the spare capacity.
                unsafe { buf.set_len(have + n as usize) };
                if buf.capacity() == buf.len() {
                    buf.reserve(buf.len().max(4096));
                }
            }
            _ => return Err(io::Error::last_os_error()),
        }
    }
}

// Turn a statx result, a first read and the descriptor into the final loaded value.
fn finish(fd: i32, stat: &libc::statx, stat_res: i32, read_res: i32, mut buf: Vec<u8>) -> Loaded {
    if stat_res < 0 {
        return Err(io::Error::from_raw_os_error(-stat_res));
    }
    if u32::from(stat.stx_mode) & libc::S_IFMT != libc::S_IFREG {
        return Err(io::Error::other("not a regular file"));
    }
    if read_res < 0 {
        return Err(io::Error::from_raw_os_error(-read_res));
    }
    let size = stat.stx_size as usize;
    // SAFETY: the kernel initialised `read_res` bytes of the buffer's capacity, which is at least FIRST_READ.
    unsafe { buf.set_len((read_res as usize).min(buf.capacity())) };
    if buf.len() != size.min(FIRST_READ) || size > FIRST_READ {
        buf = read_rest(fd, buf, size)?;
    }
    let ns = |t: &libc::statx_timestamp| i128::from(t.tv_sec) * 1_000_000_000 + i128::from(t.tv_nsec);
    let dev = libc::makedev(stat.stx_dev_major, stat.stx_dev_minor);
    Ok((Ident::from_parts(dev, stat.stx_ino, stat.stx_size, ns(&stat.stx_mtime), ns(&stat.stx_ctime)), buf))
}

// Stat and read every opened file (stat first, linked), then close all descriptors.
fn read_all(ring: &mut IoUring, fds: &[Option<i32>], results: &mut [Option<Loaded>]) -> io::Result<()> {
    let n = fds.len();
    let mut bufs: Vec<Vec<u8>> =
        (0..n).map(|i| if fds[i].is_some() { vec![0u8; FIRST_READ] } else { vec![] }).collect();
    // SAFETY: statx is plain C data for which all-zero bytes are a valid value.
    let mut stats: Vec<libc::statx> = (0..n).map(|_| unsafe { std::mem::zeroed() }).collect();
    let mut entries = vec![];
    for (i, fd) in fds.iter().enumerate() {
        let Some(fd) = *fd else { continue };
        let stat_ptr = (&mut stats[i] as *mut libc::statx).cast::<types::statx>();
        entries.push(
            opcode::Statx::new(types::Fd(fd), c"".as_ptr(), stat_ptr)
                .flags(libc::AT_EMPTY_PATH)
                .mask(libc::STATX_BASIC_STATS)
                .build()
                .flags(squeue::Flags::IO_LINK)
                .user_data((i as u64) << 2 | STAT),
        );
        entries.push(
            opcode::Read::new(types::Fd(fd), bufs[i].as_mut_ptr(), FIRST_READ as u32)
                .offset(0)
                .build()
                .user_data((i as u64) << 2 | READ),
        );
    }
    let done = run(ring, &entries)?;
    let (mut stat_res, mut read_res) = (vec![0i32; n], vec![0i32; n]);
    for (user, res) in done {
        let slot = if user & 3 == STAT { &mut stat_res } else { &mut read_res };
        slot[(user >> 2) as usize] = res;
    }
    for (i, fd) in fds.iter().enumerate() {
        if let Some(fd) = *fd {
            results[i] = Some(finish(fd, &stats[i], stat_res[i], read_res[i], std::mem::take(&mut bufs[i])));
        }
    }
    Ok(())
}

// Close descriptors with the ring, falling back to close(2) if the ring fails.
fn close_all(ring: &mut IoUring, fds: &[Option<i32>]) {
    let entries: Vec<squeue::Entry> =
        fds.iter().flatten().map(|&fd| opcode::Close::new(types::Fd(fd)).build().user_data(0)).collect();
    if !entries.is_empty() && run(ring, &entries).is_err() {
        for &fd in fds.iter().flatten() {
            // SAFETY: each descriptor was opened by this module and is closed exactly once here.
            unsafe { libc::close(fd) };
        }
    }
}

// Load one batch of at most BATCH paths with three submission rounds.
fn batch(ring: &mut IoUring, chunk: &[&Path]) -> io::Result<Vec<Loaded>> {
    let paths: Vec<Option<CString>> =
        chunk.iter().map(|p| CString::new(p.as_os_str().as_bytes()).ok()).collect();
    let mut results: Vec<Option<Loaded>> = paths
        .iter()
        .map(|p| p.is_none().then(|| Err(io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))))
        .collect();
    let mut fds = vec![None; chunk.len()];
    let mut outcome = open_all(ring, &paths, &mut fds, &mut results);
    if outcome.is_ok() {
        outcome = read_all(ring, &fds, &mut results);
    }
    close_all(ring, &fds);
    outcome?;
    Ok(results.into_iter().map(|r| r.expect("every path has a result")).collect())
}

// Load files with io_uring, or return None when it is unavailable so the caller can read normally.
pub fn load_many(paths: &[&Path]) -> Option<Vec<Loaded>> {
    let mut ring = take_ring()?;
    let mut out = Vec::with_capacity(paths.len());
    for chunk in paths.chunks(BATCH) {
        match batch(&mut ring, chunk) {
            Ok(done) => out.extend(done),
            Err(_) => {
                UNAVAILABLE.store(true, Ordering::Relaxed);
                return None;
            }
        }
    }
    POOL.lock().expect("ring pool").push(ring);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // When the kernel allows io_uring, a batch matches ordinary loading; otherwise loading falls back.
    #[test]
    fn test_uring_matches_plain_load() {
        let dir = tempfile::tempdir().unwrap();
        let mut paths = vec![];
        for (i, len) in [0usize, 5, 65536, 65537, 400_000].into_iter().enumerate() {
            let p = dir.path().join(format!("f{i}"));
            std::fs::write(&p, vec![b'x'; len]).unwrap();
            paths.push(p);
        }
        paths.push(dir.path().join("missing"));
        let refs: Vec<&Path> = paths.iter().map(|p| p.as_path()).collect();
        let Some(got) = load_many(&refs) else {
            eprintln!("io_uring unavailable here; fallback path is covered by fsio tests");
            return;
        };
        for (p, g) in refs.iter().zip(got) {
            match (crate::fsio::load(p), g) {
                (Ok(a), Ok(b)) => assert_eq!(a, b, "{p:?}"),
                (Err(a), Err(b)) => assert_eq!(a.kind(), b.kind()),
                (a, b) => panic!("{p:?}: {a:?} vs {b:?}"),
            }
        }
    }
}
