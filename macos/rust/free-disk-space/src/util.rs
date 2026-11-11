// Shared output, process, filesystem and stop-signal helpers.
use std::fs::Metadata;
use std::io::{IsTerminal, Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

pub const UF_COMPRESSED: u32 = 0x20;
pub const UF_IMMUTABLE: u32 = 0x2;
pub const UF_APPEND: u32 = 0x4;
pub const SF_IMMUTABLE: u32 = 0x20000;
// Marks an iCloud / File Provider placeholder.
pub const SF_DATALESS: u32 = 0x4000_0000;
pub const NO_TOUCH_FLAGS: u32 = UF_COMPRESSED | UF_IMMUTABLE | UF_APPEND | SF_IMMUTABLE | SF_DATALESS;

static STOP: AtomicBool = AtomicBool::new(false);

// Tell whether stdout is a terminal, computed once.
pub fn is_tty() -> bool {
    static TTY: OnceLock<bool> = OnceLock::new();
    *TTY.get_or_init(|| std::io::stdout().is_terminal())
}

// Wrap text in an ANSI color code when writing to a terminal.
pub fn c(code: &str, text: &str) -> String {
    if is_tty() && std::env::var_os("NO_COLOR").is_none() {
        return format!("\x1b[{code}m{text}\x1b[0m");
    }
    text.to_string()
}

// Format a byte count as a short human-readable size.
pub fn human(n: f64) -> String {
    let mut n = n;
    for unit in ["B", "KiB", "MiB", "GiB", "TiB"] {
        if n.abs() < 1024.0 || unit == "TiB" {
            if unit == "B" {
                return format!("{} {unit}", n as i64);
            }
            return format!("{n:.1} {unit}");
        }
        n /= 1024.0;
    }
    n.to_string()
}

// Format a signed byte count as a short human-readable size.
pub fn human_i(n: i64) -> String {
    human(n as f64)
}

// Format seconds as H:MM:SS.
pub fn hms(sec: f64) -> String {
    let s = sec as u64;
    format!("{}:{:02}:{:02}", s / 3600, s % 3600 / 60, s % 60)
}

// Parse sizes like 10M or 2G into bytes.
pub fn parse_size(text: &str) -> Result<i64, String> {
    let t = text.trim().to_uppercase();
    let t = t.trim_end_matches('B');
    let (num, mult) = match t.chars().last() {
        Some('K') => (&t[..t.len() - 1], 1i64 << 10),
        Some('M') => (&t[..t.len() - 1], 1 << 20),
        Some('G') => (&t[..t.len() - 1], 1 << 30),
        Some('T') => (&t[..t.len() - 1], 1 << 40),
        _ => (t, 1),
    };
    let value: f64 = num.parse().map_err(|_| format!("invalid size: {text}"))?;
    Ok((value * mult as f64) as i64)
}

// Print a warning to stderr.
pub fn warn(msg: &str) {
    eprintln!("{}{msg}", c("33", "warning: "));
}

// Print an error to stderr and exit.
pub fn die(msg: &str) -> ! {
    eprintln!("{}{msg}", c("31", "error: "));
    std::process::exit(1);
}

// Return the current Unix time in seconds.
pub fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

// Return the user's home directory.
pub fn home() -> String {
    std::env::var("HOME").unwrap_or_else(|_| "/".to_string())
}

// Expand a leading ~ to the home directory.
pub fn expand(path: &str) -> String {
    match path.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => format!("{}{rest}", home()),
        _ => path.to_string(),
    }
}

// Expand ~ and resolve symlinks, keeping the path when it does not exist.
pub fn realpath(path: &str) -> String {
    let expanded = expand(path);
    std::fs::canonicalize(&expanded).map(|p| p.to_string_lossy().into_owned()).unwrap_or(expanded)
}

// Return the terminal width, defaulting to 100 columns.
fn term_cols() -> usize {
    // SAFETY: ioctl writes into a zeroed winsize struct we own.
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    let ok = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) } == 0;
    if ok && ws.ws_col > 0 {
        ws.ws_col as usize
    } else {
        100
    }
}

// Rate-limited progress line, in place on a terminal and periodic lines otherwise.
pub struct Ticker {
    interval: f64,
    pub last: f64,
    shown: bool,
}

impl Ticker {
    // Set the default interval between progress updates.
    pub fn new() -> Self {
        Self { interval: if is_tty() { 0.4 } else { 15.0 }, last: 0.0, shown: false }
    }

    // Show progress text if the interval has elapsed or force is set.
    pub fn tick(&mut self, text: &str, force: bool) {
        let t = now();
        if !force && t - self.last < self.interval {
            return;
        }
        self.last = t;
        if !is_tty() {
            println!("{text}");
            return;
        }
        let shown: String = text.chars().take(term_cols().saturating_sub(1)).collect();
        print!("\r\x1b[K{shown}");
        let _ = std::io::stdout().flush();
        self.shown = true;
    }

    // Erase the in-place progress line.
    pub fn clear(&mut self) {
        if is_tty() && self.shown {
            print!("\r\x1b[K");
            let _ = std::io::stdout().flush();
            self.shown = false;
        }
    }
}

// Tell whether a graceful stop was requested.
pub fn stopped() -> bool {
    STOP.load(Ordering::SeqCst)
}

// Request a graceful stop on the first Ctrl-C and abort on the second.
pub fn install_sigint() {
    let _ = ctrlc::set_handler(|| {
        if STOP.swap(true, Ordering::SeqCst) {
            eprintln!("\naborted");
            std::process::exit(130);
        }
        println!("\n{}", c("33", "Stopping after the current item (Ctrl-C again to abort)..."));
    });
}

// Return the volume holding user data.
pub fn data_volume() -> &'static str {
    if Path::new("/System/Volumes/Data").is_dir() {
        "/System/Volumes/Data"
    } else {
        "/"
    }
}

// Return free bytes on the data volume.
pub fn free_bytes() -> i64 {
    let path = std::ffi::CString::new(data_volume()).expect("no NUL in path");
    // SAFETY: statvfs fills a zeroed struct we own from a valid C string.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(path.as_ptr(), &mut st) } != 0 {
        return 0;
    }
    st.f_bavail as i64 * st.f_frsize as i64
}

// Print and return free space with a label.
pub fn report_free(label: &str) -> i64 {
    let f = free_bytes();
    println!("{label} free space on {}: {}", data_volume(), c("1", &human_i(f)));
    f
}

// Read a child pipe to a string in a background thread.
fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut p) = pipe {
            let _ = p.read_to_end(&mut buf);
        }
        String::from_utf8_lossy(&buf).into_owned()
    })
}

// Result of a finished subprocess.
pub struct Output {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

// Run a command with a time limit and return its exit code and output, or None on spawn failure.
pub fn run(cmd: &[&str], timeout: f64, env: &[(&str, &str)]) -> Option<Output> {
    let mut command = Command::new(cmd[0]);
    command.args(&cmd[1..]).envs(env.iter().copied());
    spawn_wait(command, timeout, None).ok()
}

// Spawn a command and wait for it, printing a heartbeat line every interval if a label is given.
fn spawn_wait(mut command: Command, timeout: f64, label: Option<&str>) -> Result<Output, String> {
    command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|e| e.to_string())?;
    let out = drain(child.stdout.take());
    let err = drain(child.stderr.take());
    let start = Instant::now();
    let mut beat = Instant::now();
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            let stdout = out.join().unwrap_or_default();
            let stderr = err.join().unwrap_or_default();
            return Ok(Output { code: status.code().unwrap_or(-1), stdout, stderr });
        }
        let elapsed = start.elapsed().as_secs_f64();
        if elapsed > timeout {
            let _ = child.kill();
            let _ = child.wait();
            let stdout = out.join().unwrap_or_default() + "\n(timed out)";
            return Ok(Output { code: -2, stdout, stderr: err.join().unwrap_or_default() });
        }
        if let Some(label) = label {
            if beat.elapsed() >= Duration::from_secs(30) {
                println!("  ... {label} still running ({})", hms(elapsed));
                beat = Instant::now();
            }
        }
        thread::sleep(Duration::from_millis(50));
    }
}

// Run a command, optionally with a heartbeat label, and return its result.
fn run_heartbeat(cmd: &[&str], timeout: f64, label: Option<&str>) -> Result<Output, String> {
    let mut command = Command::new(cmd[0]);
    command.args(&cmd[1..]);
    spawn_wait(command, timeout, label)
}

// Run a long command with a heartbeat and return (exit code, combined output).
pub fn run_cmd(cmd: &[&str], label: &str, timeout: f64) -> (i32, String) {
    match run_heartbeat(cmd, timeout, Some(label)) {
        Ok(o) => (o.code, o.stdout + &o.stderr),
        Err(e) => (-1, e),
    }
}

// Return the full path of an executable on PATH.
pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(name))
        .find(|p| p.metadata().map(|m| m.is_file() && m.mode() & 0o111 != 0).unwrap_or(false))
}

// Return BSD file flags, which only macOS exposes.
#[cfg(target_os = "macos")]
pub fn flags(md: &Metadata) -> u32 {
    std::os::macos::fs::MetadataExt::st_flags(md)
}

// Return BSD file flags, which only macOS exposes.
#[cfg(not(target_os = "macos"))]
pub fn flags(_md: &Metadata) -> u32 {
    0
}

// Return a file's modification time in nanoseconds.
pub fn mtime_ns(md: &Metadata) -> i64 {
    md.mtime() * 1_000_000_000 + md.mtime_nsec()
}

// Return a file's allocated bytes.
pub fn alloc(md: &Metadata) -> i64 {
    md.blocks() as i64 * 512
}

// Return the permission bits of a file mode.
pub fn perm_bits(md: &Metadata) -> u32 {
    md.mode() & 0o7777
}

// Set a file's access and modification times in nanoseconds without following symlinks.
pub fn set_times(path: &Path, atime_ns: i64, mtime_ns: i64) -> std::io::Result<()> {
    let cpath = cstring(path)?;
    let ts = |ns: i64| libc::timespec {
        tv_sec: ns.div_euclid(1_000_000_000) as libc::time_t,
        tv_nsec: ns.rem_euclid(1_000_000_000) as _,
    };
    let times = [ts(atime_ns), ts(mtime_ns)];
    // SAFETY: valid C path and a two-element timespec array.
    let rc =
        unsafe { libc::utimensat(libc::AT_FDCWD, cpath.as_ptr(), times.as_ptr(), libc::AT_SYMLINK_NOFOLLOW) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

// Convert a path to a C string.
pub fn cstring(path: &Path) -> std::io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))
}

// Tell whether the current user may access a path with the given libc mode bits.
pub fn access(path: &Path, mode: libc::c_int) -> bool {
    // SAFETY: access only reads the C string.
    cstring(path).map(|p| unsafe { libc::access(p.as_ptr(), mode) } == 0).unwrap_or(false)
}

// Return the SHA-256 hex digest of a file.
pub fn sha256(path: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

// Return a file's extended attribute names, excluding compression internals.
pub fn xattr_names(path: &Path) -> std::io::Result<std::collections::BTreeSet<String>> {
    let p = path.to_string_lossy();
    let out = run(&["/usr/bin/xattr", &p], 120.0, &[])
        .ok_or_else(|| std::io::Error::other("xattr failed to start"))?;
    if out.code != 0 {
        return Err(std::io::Error::other(format!("xattr failed: {}", out.stderr.trim())));
    }
    let ignore = ["com.apple.decmpfs", "com.apple.ResourceFork"];
    Ok(out
        .stdout
        .lines()
        .map(str::trim)
        .filter(|x| !x.is_empty() && !ignore.contains(x))
        .map(String::from)
        .collect())
}

// Build a same-directory temp path for an in-place file swap.
pub fn temp_beside(path: &Path) -> PathBuf {
    let name: String =
        path.file_name().map(|n| n.to_string_lossy().chars().take(80).collect()).unwrap_or_default();
    path.with_file_name(format!(".fds-{}-{name}.tmp", std::process::id()))
}

// Remove a temp file if it still exists, ignoring errors.
pub fn remove_temp(path: &Path) {
    if path.symlink_metadata().is_ok() {
        let _ = std::fs::remove_file(path);
    }
}

// Unit tests for the formatting helpers.
#[cfg(test)]
mod tests {
    use super::*;

    // Check size parsing with and without suffixes.
    #[test]
    fn test_parse_size_suffixes() {
        assert_eq!(parse_size("10M").unwrap(), 10 << 20);
        assert_eq!(parse_size("1.5k").unwrap(), 1536);
        assert_eq!(parse_size("2GB").unwrap(), 2 << 30);
        assert_eq!(parse_size("7").unwrap(), 7);
        assert!(parse_size("x").is_err());
    }

    // Check human-readable sizes and durations.
    #[test]
    fn test_human_and_hms() {
        assert_eq!(human(512.0), "512 B");
        assert_eq!(human(1536.0), "1.5 KiB");
        assert_eq!(hms(3725.0), "1:02:05");
    }

    // Check that a SHA-256 digest matches a known value.
    #[test]
    fn test_sha256_known() {
        let dir = std::env::temp_dir().join(format!("fds-test-{}", std::process::id()));
        std::fs::write(&dir, b"abc").unwrap();
        let digest = sha256(&dir).unwrap();
        std::fs::remove_file(&dir).unwrap();
        assert!(digest.starts_with("ba7816bf"));
    }
}
