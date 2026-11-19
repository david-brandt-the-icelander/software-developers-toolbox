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

// Return true when the kernel supports every opcode this module uses.
fn ring_supported(ring: &IoUring) -> bool {
    let mut probe = Probe::new();
    let codes = [opcode::OpenAt::CODE, opcode::Statx::CODE, opcode::Read::CODE, opcode::Close::CODE];
    ring.submitter()
        .register_probe(&mut probe)
        .ok()
        .is_some_and(|()| codes.iter().all(|&c| probe.is_supported(c)))
}

// Take a ring from the process pool or create one, or None when io_uring is unusable here.
fn take_ring() -> Option<IoUring> {
    take_from(&UNAVAILABLE, &POOL, ring_supported)
}

// Take a ring from a pool or create one that passes the check, remembering a failure in the flag.
fn take_from(
    unavailable: &AtomicBool,
    pool: &Mutex<Vec<IoUring>>,
    supported: impl Fn(&IoUring) -> bool,
) -> Option<IoUring> {
    if unavailable.load(Ordering::Relaxed) {
        return None;
    }
    if let Some(ring) = pool.lock().expect("ring pool").pop() {
        return Some(ring);
    }
    let ring = IoUring::new(RING_ENTRIES).ok().filter(|r| supported(r));
    if ring.is_none() {
        unavailable.store(true, Ordering::Relaxed);
    }
    ring
}

// Treat an interrupted wait as success so the caller waits again, and pass other errors on.
fn tolerate_interrupt(result: io::Result<usize>) -> io::Result<()> {
    match result {
        Err(e) if e.raw_os_error() != Some(libc::EINTR) => Err(e),
        _ => Ok(()),
    }
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
        tolerate_interrupt(ring.submit_and_wait(entries.len() - done.len()))?;
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
    let again = open_round(ring, paths, &denied, 0)?;
    opened.retain(|o| o.1 != -libc::EPERM);
    opened.extend(again);
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
    let outcome =
        open_all(ring, &paths, &mut fds, &mut results).and_then(|()| read_all(ring, &fds, &mut results));
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
        // The closure runs once when io_uring works here and not at all when the kernel lacks it.
        load_many(&refs).into_iter().for_each(|got| {
            for (p, g) in refs.iter().zip(got) {
                match (crate::fsio::load(p), g) {
                    (Ok(a), Ok(b)) => assert_eq!(a, b, "{p:?}"),
                    (Err(a), Err(b)) => assert_eq!(a.kind(), b.kind()),
                    (a, b) => panic!("{p:?}: {a:?} vs {b:?}"),
                }
            }
        });
    }

    // Write a file of the given content and open it for reading.
    fn open_file(content: &[u8]) -> (tempfile::TempDir, std::fs::File) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f");
        std::fs::write(&p, content).unwrap();
        (dir, std::fs::File::open(&p).unwrap())
    }

    // Build a statx value describing a regular file or directory of a size.
    fn statx_of(mode: u32, size: u64) -> libc::statx {
        // SAFETY: statx is plain C data for which all-zero bytes are a valid value.
        let mut s: libc::statx = unsafe { std::mem::zeroed() };
        s.stx_mode = mode as u16;
        s.stx_size = size;
        s
    }

    // Finishing a load handles each failed step and each read-size case.
    #[test]
    fn test_finish_cases() {
        use std::os::unix::io::AsRawFd;
        let content: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
        let (_d, f) = open_file(&content);
        let fd = f.as_raw_fd();
        let reg = |size: u64| statx_of(libc::S_IFREG, size);
        let prefix = |n: usize| content[..n].to_vec();
        let kind = |r: Loaded| r.unwrap_err().kind();
        assert_eq!(kind(finish(fd, &reg(1), -libc::ENOENT, 0, vec![])), io::ErrorKind::NotFound);
        assert_eq!(kind(finish(fd, &statx_of(libc::S_IFDIR, 1), 0, 0, vec![])), io::ErrorKind::Other);
        assert_eq!(finish(fd, &reg(1), 0, -libc::EIO, vec![]).unwrap_err().raw_os_error(), Some(libc::EIO));
        let (_d2, small) = open_file(&prefix(10));
        let small_fd = small.as_raw_fd();
        let whole = finish(small_fd, &reg(10), 0, 10, prefix(10)).unwrap();
        assert_eq!(whole.1, prefix(10));
        let short = finish(small_fd, &reg(10), 0, 4, prefix(4)).unwrap();
        assert_eq!(short.1, prefix(10));
        let big = finish(fd, &reg(content.len() as u64), 0, FIRST_READ as i32, prefix(FIRST_READ)).unwrap();
        assert_eq!(big.1, content);
    }

    // Reading the rest of a file works with exact, too-small and absent size hints, and fails on a bad descriptor.
    #[test]
    fn test_read_rest_cases() {
        use std::os::unix::io::AsRawFd;
        let content = vec![7u8; 5000];
        let (_d, f) = open_file(&content);
        assert_eq!(read_rest(f.as_raw_fd(), vec![], 5000).unwrap(), content);
        assert_eq!(read_rest(f.as_raw_fd(), vec![], 10).unwrap(), content);
        assert_eq!(read_rest(f.as_raw_fd(), vec![7u8; 100], 5000).unwrap(), content);
        assert!(read_rest(-1, vec![], 10).is_err());
    }

    // Interrupted waits are retried while other errors are reported.
    #[test]
    fn test_tolerate_interrupt() {
        assert!(tolerate_interrupt(Ok(3)).is_ok());
        assert!(tolerate_interrupt(Err(io::Error::from_raw_os_error(libc::EINTR))).is_ok());
        assert!(tolerate_interrupt(Err(io::Error::from_raw_os_error(libc::EIO))).is_err());
    }

    // Ring selection honours the unavailable flag, the pool and a failed capability check.
    #[test]
    fn test_take_from_rules() {
        let pool = Mutex::new(Vec::new());
        let down = AtomicBool::new(true);
        assert!(take_from(&down, &pool, |_| true).is_none());
        let up = AtomicBool::new(false);
        assert!(take_from(&up, &pool, |_| false).is_none());
        assert!(up.load(Ordering::Relaxed));
        let up = AtomicBool::new(false);
        take_from(&up, &pool, |_| true).into_iter().for_each(|ring| {
            assert!(ring_supported(&ring));
            pool.lock().unwrap().push(ring);
            assert!(take_from(&up, &pool, |_| false).is_some());
        });
    }

    // Bad and missing paths produce per-path errors without disturbing the rest of the batch.
    #[test]
    fn test_load_many_error_paths() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("good");
        std::fs::write(&good, "ok").unwrap();
        let nul = Path::new(std::ffi::OsStr::from_bytes(b"a\0b"));
        let missing = dir.path().join("missing");
        load_many(&[nul, missing.as_path(), good.as_path(), dir.path()]).into_iter().for_each(|done| {
            assert_eq!(done[0].as_ref().unwrap_err().kind(), io::ErrorKind::InvalidInput);
            assert_eq!(done[1].as_ref().unwrap_err().kind(), io::ErrorKind::NotFound);
            assert_eq!(done[2].as_ref().unwrap().1, b"ok");
            assert!(done[3].is_err());
        });
        load_many(&[missing.as_path()]).into_iter().for_each(|only_bad| assert!(only_bad[0].is_err()));
    }
}
