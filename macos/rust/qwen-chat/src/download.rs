// Finds the Qwen3.5-9B weights in the cache, downloading and verifying them on first use.

use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const FILE: &str = "Qwen3.5-9B-Q4_K_M.gguf";
const REPO: &str = "unsloth/Qwen3.5-9B-GGUF";
// Pinned so the checksum below always matches what is served.
const REVISION: &str = "3885219b6810b007914f3a7950a8d1b469d598a5";
const SHA256: &str = "03b74727a860a56338e042c4420bb3f04b2fec5734175f4cb9fa853daf52b7e8";
pub const SIZE: u64 = 5_680_522_464;
const CHUNK: usize = 1 << 20;
const ATTEMPTS: u32 = 8;

// Why a download attempt failed: a dropped connection is retried, a local problem is not.
#[derive(Debug, PartialEq)]
enum Fail {
    Retry(String),
    Fatal(String),
}

// Cache directory: $QWEN_CHAT_HOME, else the platform cache folder.
pub fn cache_dir(
    home_override: Option<String>,
    xdg: Option<String>,
    home: Option<String>,
) -> Option<PathBuf> {
    if let Some(dir) = home_override.filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    if cfg!(target_os = "macos") {
        return home.map(|h| Path::new(&h).join("Library/Caches/qwen-chat"));
    }
    if let Some(x) = xdg.filter(|d| !d.is_empty()) {
        return Some(Path::new(&x).join("qwen-chat"));
    }
    home.map(|h| Path::new(&h).join(".cache/qwen-chat"))
}

// Download address of the pinned weights file.
fn url() -> String {
    format!("https://huggingface.co/{REPO}/resolve/{REVISION}/{FILE}")
}

// Lowercase hex of a digest.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// Hashes what an earlier, interrupted download already wrote.
fn hash_existing(path: &Path, hasher: &mut Sha256) -> io::Result<u64> {
    let Ok(mut f) = File::open(path) else {
        return Ok(0);
    };
    let mut buf = vec![0; CHUNK];
    let mut total = 0;
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            return Ok(total);
        }
        hasher.update(&buf[..n]);
        total += n as u64;
    }
}

// Progress line for a download.
fn progress(done: u64, total: u64) -> String {
    let gb = |b: u64| b as f64 / 1e9;
    format!("\rdownloading {FILE}: {:.2}/{:.2} GB ({}%)", gb(done), gb(total), done * 100 / total.max(1))
}

// Streams the file into part, resuming at its current length, and returns the checksum.
fn fetch(part: &Path) -> Result<String, Fail> {
    let mut hasher = Sha256::new();
    let local = |e: io::Error| Fail::Fatal(format!("{}: {e}", part.display()));
    let start = hash_existing(part, &mut hasher).map_err(local)?;
    let config = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(30)))
        .timeout_recv_response(Some(Duration::from_secs(60)))
        .build();
    let agent = ureq::Agent::new_with_config(config);
    let mut req = agent.get(url());
    if start > 0 {
        req = req.header("Range", format!("bytes={start}-"));
    }
    let resp = req.call().map_err(|e| Fail::Retry(e.to_string()))?;
    if start > 0 && resp.status() != 206 {
        return Err(Fail::Fatal(format!("server ignored the resume request; delete {}", part.display())));
    }
    let mut body = resp.into_body().into_reader();
    let mut out = OpenOptions::new().create(true).append(true).open(part).map_err(local)?;
    let mut buf = vec![0; CHUNK];
    let mut done = start;
    let mut shown = u64::MAX;
    loop {
        let n = body.read(&mut buf).map_err(|e| Fail::Retry(e.to_string()))?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n]).map_err(local)?;
        hasher.update(&buf[..n]);
        done += n as u64;
        if done * 100 / SIZE != shown {
            shown = done * 100 / SIZE;
            eprint!("{}", progress(done, SIZE));
        }
    }
    eprintln!();
    if done < SIZE {
        return Err(Fail::Retry(format!("connection closed at {done} bytes")));
    }
    Ok(hex(&hasher.finalize()))
}

// Runs fetch until it succeeds, waiting longer after each dropped connection.
fn fetch_with_retries(part: &Path) -> Result<String, String> {
    let mut attempt = 1;
    loop {
        match fetch(part) {
            Ok(sum) => return Ok(sum),
            Err(Fail::Fatal(e)) => return Err(e),
            Err(Fail::Retry(e)) if attempt >= ATTEMPTS => {
                return Err(format!("download failed: {e}; run again to resume"));
            }
            Err(Fail::Retry(e)) => {
                eprintln!("\n{e}; resuming (attempt {}/{ATTEMPTS})", attempt + 1);
                std::thread::sleep(Duration::from_secs(2 * u64::from(attempt)));
                attempt += 1;
            }
        }
    }
}

// Path of the weights, downloading them into dir if they are not there yet.
pub fn ensure(dir: &Path) -> Result<PathBuf, String> {
    let path = dir.join(FILE);
    if path.exists() {
        return Ok(path);
    }
    fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    eprintln!("first run: fetching {REPO} {FILE} ({:.1} GB) into {}", SIZE as f64 / 1e9, dir.display());
    let part = dir.join(format!("{FILE}.part"));
    let sum = fetch_with_retries(&part)?;
    if sum != SHA256 {
        let _ = fs::remove_file(&part);
        return Err(format!("checksum mismatch: got {sum}, want {SHA256}; the partial file was removed"));
    }
    fs::rename(&part, &path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    // The override wins, then the platform default.
    #[test]
    fn test_cache_dir_order() {
        let o = cache_dir(Some("/x".into()), Some("/xdg".into()), Some("/h".into()));
        assert_eq!(o, Some(PathBuf::from("/x")));
        let d = cache_dir(Some(String::new()), None, Some("/h".into())).expect("home set");
        let want =
            if cfg!(target_os = "macos") { "/h/Library/Caches/qwen-chat" } else { "/h/.cache/qwen-chat" };
        assert_eq!(d, PathBuf::from(want));
        assert_eq!(cache_dir(None, None, None), None);
    }

    // XDG_CACHE_HOME is used off macOS.
    #[test]
    fn test_cache_dir_xdg() {
        let d = cache_dir(None, Some("/xdg".into()), Some("/h".into())).expect("home set");
        if !cfg!(target_os = "macos") {
            assert_eq!(d, PathBuf::from("/xdg/qwen-chat"));
        }
    }

    // A file that is already present is returned without any network access.
    #[test]
    fn test_ensure_existing() {
        let dir = std::env::temp_dir().join(format!("qwen-chat-test-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("temp dir");
        fs::write(dir.join(FILE), b"x").expect("write");
        assert_eq!(ensure(&dir), Ok(dir.join(FILE)));
        fs::remove_dir_all(&dir).expect("cleanup");
    }

    // Hashing a partial file counts its bytes, and a missing one counts zero.
    #[test]
    fn test_hash_existing() {
        let p = std::env::temp_dir().join(format!("qwen-chat-hash-{}", std::process::id()));
        fs::write(&p, b"abc").expect("write");
        let mut h = Sha256::new();
        assert_eq!(hash_existing(&p, &mut h).expect("read"), 3);
        assert_eq!(hex(&h.finalize()), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        fs::remove_file(&p).expect("cleanup");
        assert_eq!(hash_existing(&p, &mut Sha256::new()).expect("missing"), 0);
    }

    // The progress line shows gigabytes and percent.
    #[test]
    fn test_progress() {
        assert_eq!(progress(500_000_000, 1_000_000_000), format!("\rdownloading {FILE}: 0.50/1.00 GB (50%)"));
    }
}
