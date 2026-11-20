// Where models and the chosen defaults live, and the download that resumes, hashes and installs a model file.
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const CHUNK: usize = 1 << 20;
const TICK: Duration = Duration::from_millis(500);

// The effort levels, least first.
pub const EFFORTS: [&str; 3] = ["low", "medium", "high"];

// The model and effort `llm` chose, used by `prompt` when it names neither.
#[derive(Debug, PartialEq)]
pub struct Defaults {
    pub model: String,
    pub effort: String,
}

// One file to download: where from, what it must hash to and how big it is.
pub struct Want<'a> {
    pub url: String,
    pub file: &'a str,
    pub sha256: &'a str,
    pub bytes: u64,
}

// Returns ai-agent's directory: $ZAZZLE_LLM_DIR, else ~/.zazzle/v1/llm.
pub fn root() -> Result<PathBuf, String> {
    if let Some(d) = std::env::var_os("ZAZZLE_LLM_DIR") {
        return Ok(PathBuf::from(d));
    }
    let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
    Ok(PathBuf::from(home).join(".zazzle/v1/llm"))
}

// Creates a directory and its parents, private to the user.
fn make_dir(dir: &Path) -> Result<(), String> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|e| format!("creating {}: {e}", dir.display()))
}

// Checks an effort name.
pub fn effort(name: &str) -> Result<String, String> {
    if EFFORTS.contains(&name) {
        return Ok(name.to_string());
    }
    Err(format!("no effort '{name}'; known: {}", EFFORTS.join(" ")))
}

// Reads the defaults file, falling back to the default model and medium effort.
pub fn defaults(root: &Path) -> Defaults {
    let text = std::fs::read_to_string(root.join("defaults")).unwrap_or_default();
    let get = |key: &str| text.lines().find_map(|l| l.strip_prefix(key)?.strip_prefix('=')).map(str::trim);
    Defaults {
        model: get("model").unwrap_or(crate::catalog::DEFAULT).to_string(),
        effort: get("effort").filter(|e| EFFORTS.contains(e)).unwrap_or("medium").to_string(),
    }
}

// Writes the defaults file.
pub fn save_defaults(root: &Path, d: &Defaults) -> Result<(), String> {
    make_dir(root)?;
    let path = root.join("defaults");
    std::fs::write(&path, format!("model={}\neffort={}\n", d.model, d.effort))
        .map_err(|e| format!("writing {}: {e}", path.display()))
}

// Returns where a model file is installed.
pub fn model_path(root: &Path, file: &str) -> PathBuf {
    root.join("models").join(file)
}

// Returns where a model file's partial download is kept.
pub fn part_path(root: &Path, file: &str) -> PathBuf {
    root.join("models").join(format!(".{file}.part"))
}

// Returns a model file's lock file.
pub fn lock_path(root: &Path, file: &str) -> PathBuf {
    root.join("models").join(format!(".{file}.lock"))
}

// True when a model file is installed; files are renamed into place only after their hash matched, so the size is enough.
pub fn installed(root: &Path, file: &str, bytes: u64) -> bool {
    std::fs::metadata(model_path(root, file)).is_ok_and(|m| m.len() == bytes)
}

// Removes an installed model file and any partial download; true when there was something to remove.
pub fn remove(root: &Path, file: &str) -> Result<bool, String> {
    let mut removed = false;
    for p in [model_path(root, file), part_path(root, file)] {
        match std::fs::remove_file(&p) {
            Ok(()) => removed = true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("removing {}: {e}", p.display())),
        }
    }
    Ok(removed)
}

// Holds a flock on a model's lock file until dropped: exclusive while it downloads or prune deletes it, shared while a prompt or worker runs it.
pub struct Lock(File);

impl Lock {
    // Opens (creating) a lock file and flocks it with `op`; None when LOCK_NB found another holder.
    fn open(path: &Path, op: libc::c_int) -> Result<Option<Lock>, String> {
        let f = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)
            .map_err(|e| format!("opening {}: {e}", path.display()))?;
        // SAFETY: flock takes an open descriptor owned by f, which outlives the lock.
        if unsafe { libc::flock(f.as_raw_fd(), op) } == 0 {
            return Ok(Some(Lock(f)));
        }
        let e = std::io::Error::last_os_error();
        if e.kind() == std::io::ErrorKind::WouldBlock {
            return Ok(None);
        }
        Err(format!("locking {}: {e}", path.display()))
    }
}

impl Drop for Lock {
    // Releases the lock.
    fn drop(&mut self) {
        // SAFETY: the descriptor is still open; unlocking cannot invalidate it.
        unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

// Takes a shared lock on an installed model while it runs, so prune leaves it alone; waits while prune holds it, then fails if prune removed the model.
pub fn hold(root: &Path, file: &str, bytes: u64) -> Result<Option<Lock>, String> {
    let lock = Lock::open(&lock_path(root, file), libc::LOCK_SH)?;
    if !installed(root, file, bytes) {
        return Err(format!("{file} was removed while starting; run again"));
    }
    Ok(lock)
}

// Takes a model's lock exclusively without waiting; None when a download or run holds it.
pub fn try_lock(root: &Path, file: &str) -> Result<Option<Lock>, String> {
    Lock::open(&lock_path(root, file), libc::LOCK_EX | libc::LOCK_NB)
}

// True when a process holds the lock file (a download, run or prune of that model); a missing file is not held.
pub fn busy(path: &Path) -> bool {
    let Ok(f) = File::open(path) else { return false };
    // SAFETY: flock takes an open descriptor owned by f; closing f at the end of this function releases the probe.
    unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) != 0 }
}

// Returns the lowercase hex of a digest.
fn hex(digest: &[u8]) -> String {
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

// Returns a file's SHA-256 as lowercase hex, reading it all.
pub fn sha256(path: &Path) -> Result<String, String> {
    let mut f = File::open(path).map_err(|e| format!("opening {}: {e}", path.display()))?;
    let (mut hasher, mut buf) = (Sha256::new(), vec![0u8; CHUNK]);
    loop {
        match f.read(&mut buf).map_err(|e| format!("reading {}: {e}", path.display()))? {
            0 => return Ok(hex(&hasher.finalize())),
            k => hasher.update(&buf[..k]),
        }
    }
}

// Hashes the bytes already in a partial download, so the download can go on from where it stopped.
fn hash_existing(path: &Path, hasher: &mut Sha256) -> u64 {
    let Ok(mut f) = File::open(path) else { return 0 };
    let (mut buf, mut n) = (vec![0u8; CHUNK], 0u64);
    loop {
        match f.read(&mut buf) {
            Ok(0) | Err(_) => return n,
            Ok(k) => {
                hasher.update(&buf[..k]);
                n += k as u64;
            }
        }
    }
}

// Returns the HTTP agent: connection and response timeouts but none on the body, which can take an hour.
fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(30)))
        .timeout_recv_response(Some(Duration::from_secs(60)))
        .tls_config(
            ureq::tls::TlsConfig::builder().root_certs(ureq::tls::RootCerts::PlatformVerifier).build(),
        )
        .build()
        .into()
}

// Downloads a file into the models directory unless it is installed, resuming a partial download, and installs it only when its SHA-256 matches; `progress` gets bytes done and total.
pub fn fetch(root: &Path, want: &Want, progress: &mut dyn FnMut(u64, u64)) -> Result<PathBuf, String> {
    let dir = root.join("models");
    make_dir(&dir)?;
    let _lock = Lock::open(&lock_path(root, want.file), libc::LOCK_EX)?;
    let path = dir.join(want.file);
    if installed(root, want.file, want.bytes) {
        return Ok(path);
    }
    let part = part_path(root, want.file);
    let mut hasher = Sha256::new();
    let mut done = hash_existing(&part, &mut hasher);
    if done > want.bytes {
        std::fs::remove_file(&part).map_err(|e| format!("removing {}: {e}", part.display()))?;
        (hasher, done) = (Sha256::new(), 0);
    }
    if done < want.bytes {
        done = download(want, &part, &mut hasher, done, progress)?;
    }
    let got = hex(&hasher.finalize());
    if done != want.bytes || got != want.sha256 {
        let _ = std::fs::remove_file(&part);
        return Err(format!(
            "{}: got {done} bytes with SHA-256 {got}, expected {} with {}",
            want.file, want.bytes, want.sha256
        ));
    }
    std::fs::rename(&part, &path).map_err(|e| format!("installing {}: {e}", path.display()))?;
    Ok(path)
}

// Requests the rest of a file from byte `from` and appends it to the partial file, hashing as it goes; returns the bytes now held.
fn download(
    want: &Want,
    part: &Path,
    hasher: &mut Sha256,
    from: u64,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<u64, String> {
    let mut req = agent().get(&want.url);
    if from > 0 {
        req = req.header("Range", format!("bytes={from}-"));
    }
    let res = req.call().map_err(|e| format!("downloading {}: {e}", want.url))?;
    let mut done = from;
    let mut out = OpenOptions::new().create(true).append(true).open(part);
    if from > 0 && res.status() != 206 {
        *hasher = Sha256::new();
        done = 0;
        out = File::create(part);
    }
    let mut out = out.map_err(|e| format!("writing {}: {e}", part.display()))?;
    let mut body = res.into_body();
    let mut reader = body.as_reader();
    let (mut buf, mut last) = (vec![0u8; CHUNK], Instant::now());
    progress(done, want.bytes);
    loop {
        let n = reader.read(&mut buf).map_err(|e| format!("downloading {}: {e}", want.url))?;
        if n == 0 || done + n as u64 > want.bytes {
            done += n as u64;
            break;
        }
        out.write_all(&buf[..n]).map_err(|e| format!("writing {}: {e}", part.display()))?;
        hasher.update(&buf[..n]);
        done += n as u64;
        if last.elapsed() >= TICK {
            progress(done, want.bytes);
            last = Instant::now();
        }
    }
    progress(done, want.bytes);
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    // A temporary directory removed when dropped.
    struct Tmp(PathBuf);

    impl Tmp {
        // Creates an empty directory under the system temporary directory.
        fn new(tag: &str) -> Tmp {
            let p = std::env::temp_dir().join(format!("ai-agent-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).expect("tmp");
            Tmp(p)
        }
    }

    impl Drop for Tmp {
        // Removes the directory.
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    // Serves a body on a local port, answering a Range request with 206 when `ranges` is set; returns the URL.
    fn serve(body: Vec<u8>, ranges: bool) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        std::thread::spawn(move || {
            for mut s in listener.incoming().flatten() {
                let mut buf = [0u8; 4096];
                let n = s.read(&mut buf).unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]).to_lowercase();
                let from = head
                    .lines()
                    .find_map(|l| l.strip_prefix("range: bytes=")?.trim_end_matches('-').trim().parse().ok())
                    .filter(|_| ranges)
                    .unwrap_or(0usize);
                let status = if from > 0 { "206 Partial Content" } else { "200 OK" };
                let rest = &body[from..];
                let h = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    rest.len()
                );
                let _ = s.write_all(h.as_bytes()).and_then(|()| s.write_all(rest));
            }
        });
        format!("http://{addr}/model.gguf")
    }

    // Returns the lowercase hex SHA-256 of some bytes.
    fn sha(b: &[u8]) -> String {
        Sha256::digest(b).iter().map(|x| format!("{x:02x}")).collect()
    }

    // Checks defaults fall back, round-trip through the file and reject unknown efforts.
    #[test]
    fn test_store_defaults() {
        let t = Tmp::new("defaults");
        assert_eq!(
            defaults(&t.0),
            Defaults { model: crate::catalog::DEFAULT.into(), effort: "medium".into() }
        );
        let d = Defaults { model: "gpt-oss-20b".into(), effort: "high".into() };
        save_defaults(&t.0.join("sub"), &d).expect("save");
        assert_eq!(defaults(&t.0.join("sub")), d);
        assert_eq!(effort("low"), Ok("low".into()));
        assert!(effort("max").expect_err("max").contains("low medium high"));
    }

    // Checks a free lock can be taken, a held one is busy and refused, a run's shared lock refuses prune, and hold fails for a missing model.
    #[test]
    fn test_store_locks() {
        let t = Tmp::new("locks");
        std::fs::create_dir_all(t.0.join("models")).expect("models");
        let lock = lock_path(&t.0, "m.gguf");
        assert!(!busy(&lock));
        let held = try_lock(&t.0, "m.gguf").expect("take").expect("free");
        assert!(busy(&lock) && try_lock(&t.0, "m.gguf").expect("try").is_none());
        drop(held);
        assert!(!busy(&lock));
        assert!(matches!(hold(&t.0, "m.gguf", 3), Err(e) if e.contains("removed while starting")));
        std::fs::write(model_path(&t.0, "m.gguf"), b"abc").expect("model");
        let run = hold(&t.0, "m.gguf", 3).expect("hold");
        assert!(run.is_some() && busy(&lock) && try_lock(&t.0, "m.gguf").expect("try").is_none());
        assert_eq!(sha256(&model_path(&t.0, "m.gguf")), Ok(sha(b"abc")));
        assert!(sha256(&t.0.join("gone")).is_err());
    }

    // Checks a download installs the file, a second fetch reuses it, a partial download resumes, and a wrong hash fails and removes it.
    #[test]
    fn test_store_fetch() {
        let t = Tmp::new("fetch");
        let body: Vec<u8> = (0..3_000_000u32).map(|i| (i % 251) as u8).collect();
        let want = Want {
            url: serve(body.clone(), true),
            file: "m.gguf",
            sha256: &sha(&body),
            bytes: body.len() as u64,
        };
        let mut seen = Vec::new();
        let p = fetch(&t.0, &want, &mut |d, total| seen.push((d, total))).expect("fetch");
        assert_eq!(std::fs::read(&p).expect("read"), body);
        assert_eq!(seen.last(), Some(&(body.len() as u64, body.len() as u64)));
        assert!(installed(&t.0, "m.gguf", body.len() as u64));
        fetch(&t.0, &Want { url: "http://127.0.0.1:9/x".into(), ..want }, &mut |_, _| {}).expect("cached");

        assert_eq!(remove(&t.0, "m.gguf"), Ok(true));
        assert_eq!(remove(&t.0, "m.gguf"), Ok(false));
        let part = t.0.join("models/.m.gguf.part");
        std::fs::write(&part, &body[..1_000_000]).expect("part");
        let mut first = None;
        let want = Want {
            url: serve(body.clone(), true),
            file: "m.gguf",
            sha256: &sha(&body),
            bytes: body.len() as u64,
        };
        fetch(&t.0, &want, &mut |d, _| _ = first.get_or_insert(d)).expect("resume");
        assert_eq!(first, Some(1_000_000));

        std::fs::write(&part, &body[..10]).expect("part");
        let plain = Want {
            url: serve(body.clone(), false),
            file: "n.gguf",
            sha256: &sha(&body),
            bytes: body.len() as u64,
        };
        std::fs::rename(&part, t.0.join("models/.n.gguf.part")).expect("mv");
        assert_eq!(std::fs::read(fetch(&t.0, &plain, &mut |_, _| {}).expect("restart")).expect("read"), body);

        let bad = Want {
            url: serve(body.clone(), true),
            file: "b.gguf",
            sha256: &sha(b"other"),
            bytes: body.len() as u64,
        };
        assert!(fetch(&t.0, &bad, &mut |_, _| {}).expect_err("hash").contains("expected"));
        assert!(!t.0.join("models/.b.gguf.part").exists());
        let gone = Want { url: "http://127.0.0.1:9/x".into(), file: "g.gguf", sha256: "00", bytes: 5 };
        assert!(fetch(&t.0, &gone, &mut |_, _| {}).expect_err("down").contains("downloading"));
    }
}
