// Batched file loading on macOS: files are opened and stamped one by one, then read together with one lio_listio call.
use crate::fsio::{self, Ident, Loaded};
use std::fs::File;
use std::io;
use std::os::unix::io::AsRawFd;
use std::path::Path;

const BATCH: usize = 16;
const MAX_AIO_BYTES: u64 = 4 << 20;

// A file opened for the batch with its stamp, destination buffer and read request.
struct Pending<'a> {
    index: usize,
    path: &'a Path,
    file: File,
    ident: Ident,
    buf: Vec<u8>,
    cb: libc::aiocb,
}

// Open a file and prepare its read request, or return None when it should be loaded normally (empty or large).
fn prepare<'a>(index: usize, path: &'a Path) -> io::Result<Option<Pending<'a>>> {
    let file = File::open(path)?;
    let md = file.metadata()?;
    if !md.is_file() {
        return Err(io::Error::other("not a regular file"));
    }
    if md.len() == 0 || md.len() > MAX_AIO_BYTES {
        return Ok(None);
    }
    // SAFETY: the descriptor is valid for the lifetime of `file`; the call only enables read-ahead.
    unsafe { libc::fcntl(file.as_raw_fd(), libc::F_RDAHEAD, 1) };
    let mut buf = vec![0u8; md.len() as usize];
    // SAFETY: aiocb is plain C data for which all-zero bytes are a valid value.
    let mut cb: libc::aiocb = unsafe { std::mem::zeroed() };
    cb.aio_fildes = file.as_raw_fd();
    cb.aio_offset = 0;
    cb.aio_buf = buf.as_mut_ptr().cast();
    cb.aio_nbytes = buf.len();
    cb.aio_lio_opcode = libc::LIO_READ;
    Ok(Some(Pending { index, path, file, ident: Ident::of(&md), buf, cb }))
}

// Submit every read in one call and wait until none is still in progress.
fn submit(pending: &mut [Pending]) {
    let mut list: Vec<*mut libc::aiocb> = pending.iter_mut().map(|p| &mut p.cb as *mut libc::aiocb).collect();
    // SAFETY: every aiocb and buffer lives in `pending`, which is neither moved nor dropped until the wait
    // loop below has seen every request finish or never start.
    unsafe {
        libc::lio_listio(libc::LIO_WAIT, list.as_mut_ptr(), list.len() as libc::c_int, std::ptr::null_mut())
    };
    for p in pending.iter_mut() {
        // SAFETY: the aiocb is valid; aio_error and aio_suspend only inspect it.
        while unsafe { libc::aio_error(&p.cb) } == libc::EINPROGRESS {
            let one = [&p.cb as *const libc::aiocb];
            unsafe { libc::aio_suspend(one.as_ptr(), 1, std::ptr::null()) };
        }
    }
}

// Turn a completed request into the loaded value, falling back to a normal read if it did not fully succeed.
fn settle(mut p: Pending) -> Loaded {
    // SAFETY: the request is no longer in progress, so aio_error and aio_return only read its result.
    let error = unsafe { libc::aio_error(&p.cb) };
    let got = if error == libc::EINVAL { -1 } else { unsafe { libc::aio_return(&mut p.cb) } };
    drop(p.file);
    if error == 0 && got == p.buf.len() as isize {
        return Ok((p.ident, p.buf));
    }
    fsio::load(p.path)
}

// Load one batch of at most BATCH paths.
fn batch(chunk: &[&Path]) -> Vec<Loaded> {
    let mut out: Vec<Option<Loaded>> = (0..chunk.len()).map(|_| None).collect();
    let mut pending = vec![];
    for (i, path) in chunk.iter().enumerate() {
        match prepare(i, path) {
            Ok(Some(p)) => pending.push(p),
            Ok(None) => out[i] = Some(fsio::load(path)),
            Err(e) => out[i] = Some(Err(e)),
        }
    }
    submit(&mut pending);
    for p in pending {
        let index = p.index;
        out[index] = Some(settle(p));
    }
    out.into_iter().map(|r| r.expect("every path has a result")).collect()
}

// Load files in batches using POSIX AIO for the reads.
pub fn load_many(paths: &[&Path]) -> Vec<Loaded> {
    paths.chunks(BATCH).flat_map(batch).collect()
}
