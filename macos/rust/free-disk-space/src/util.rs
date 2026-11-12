// Shared output, process, filesystem and stop-signal helpers.
#[cfg(test)]
use std::cell::Cell;
use std::fs::Metadata;
#[cfg(not(test))]
use std::io::IsTerminal;
use std::io::{Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(not(test))]
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

// Seconds between heartbeat lines for long commands.
const HEARTBEAT: Duration = Duration::from_secs(30);

static STOP: AtomicBool = AtomicBool::new(false);

#[cfg(test)]
thread_local! {
    // Per-test terminal stand-in.
    static FAKE_TTY: Cell<bool> = const { Cell::new(false) };
}

#[cfg(all(test, not(target_os = "macos")))]
thread_local! {
    // Per-test BSD file flags stand-in: a repeating script of values and the next position.
    static FAKE_FLAGS: std::cell::RefCell<(Vec<u32>, usize)> = const { std::cell::RefCell::new((Vec::new(), 0)) };
}

// Tell whether stdout is a terminal, computed once.
#[cfg(not(test))]
pub fn is_tty() -> bool {
    static TTY: OnceLock<bool> = OnceLock::new();
    *TTY.get_or_init(|| std::io::stdout().is_terminal())
}

// Tell whether the current test pretends stdout is a terminal.
#[cfg(test)]
pub fn is_tty() -> bool {
    FAKE_TTY.with(Cell::get)
}

// Make the current test thread see stdout as a terminal or not.
#[cfg(test)]
pub fn set_tty(on: bool) {
    FAKE_TTY.with(|t| t.set(on));
}

// Make the current test thread see these BSD file flags on successive calls, repeating (Linux only).
#[cfg(all(test, not(target_os = "macos")))]
pub fn set_flags(script: &[u32]) {
    FAKE_FLAGS.with(|f| *f.borrow_mut() = (script.to_vec(), 0));
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
    for unit in ["B", "KiB", "MiB", "GiB"] {
        if n.abs() < 1024.0 {
            return if unit == "B" { format!("{} B", n as i64) } else { format!("{n:.1} {unit}") };
        }
        n /= 1024.0;
    }
    format!("{n:.1} TiB")
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
    #[cfg(test)]
    panic!("die: {msg}");
    #[cfg(not(test))]
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

// Pick the terminal width from an ioctl result, defaulting to 100 columns.
fn cols_from(ok: bool, ws_col: u16) -> usize {
    if ok && ws_col > 0 {
        ws_col as usize
    } else {
        100
    }
}

// Return the terminal width, defaulting to 100 columns.
fn term_cols() -> usize {
    // SAFETY: ioctl writes into a zeroed winsize struct we own.
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    let ok = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) } == 0;
    cols_from(ok, ws.ws_col)
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

// Clear or set the stop request in tests.
#[cfg(test)]
pub fn set_stop(on: bool) {
    STOP.store(on, Ordering::SeqCst);
}

// Request a graceful stop on the first Ctrl-C and abort on the second.
fn interrupt() {
    if STOP.swap(true, Ordering::SeqCst) {
        eprintln!("\naborted");
        std::process::exit(130);
    }
    println!("\n{}", c("33", "Stopping after the current item (Ctrl-C again to abort)..."));
}

// Install the Ctrl-C handler.
pub fn install_sigint() {
    let _ = ctrlc::set_handler(interrupt);
}

// Return the volume holding user data.
pub fn data_volume() -> &'static str {
    if Path::new("/System/Volumes/Data").is_dir() {
        "/System/Volumes/Data"
    } else {
        "/"
    }
}

// Return (total, free) bytes of the volume holding a path, or None if it cannot be read.
fn volume_at(volume: &str) -> Option<(i64, i64)> {
    let path = std::ffi::CString::new(volume).ok()?;
    // SAFETY: statvfs fills a zeroed struct we own from a valid C string.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(path.as_ptr(), &mut st) } != 0 {
        return None;
    }
    let unit = st.f_frsize as i64;
    Some((st.f_blocks as i64 * unit, st.f_bavail as i64 * unit))
}

// Return free bytes on the volume holding a path, or 0 if it cannot be read.
fn free_bytes_at(volume: &str) -> i64 {
    volume_at(volume).map_or(0, |(_, free)| free)
}

// Return (total, free) bytes of the data volume, or None if it cannot be read.
pub fn data_volume_space() -> Option<(i64, i64)> {
    volume_at(data_volume())
}

// Return seconds the machine has been awake; the clock stops while it sleeps.
pub fn awake_secs() -> f64 {
    #[cfg(target_os = "macos")]
    let clock = libc::CLOCK_UPTIME_RAW;
    #[cfg(not(target_os = "macos"))]
    let clock = libc::CLOCK_MONOTONIC;
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: clock_gettime writes into a timespec we own.
    unsafe { libc::clock_gettime(clock, &mut ts) };
    ts.tv_sec as f64 + ts.tv_nsec as f64 / 1e9
}

// Tell whether the main display is awake, which is false during background maintenance wakes.
#[cfg(target_os = "macos")]
pub fn display_awake() -> bool {
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGMainDisplayID() -> u32;
        fn CGDisplayIsAsleep(display: u32) -> u32;
    }
    // SAFETY: both CoreGraphics calls take and return plain integers.
    unsafe { CGDisplayIsAsleep(CGMainDisplayID()) == 0 }
}

// Tell whether the main display is awake; always true where there is no display check.
#[cfg(not(target_os = "macos"))]
pub fn display_awake() -> bool {
    true
}

// Return free bytes on the data volume.
pub fn free_bytes() -> i64 {
    free_bytes_at(data_volume())
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

// Resolve an absolute tool path into $FDS_TOOLS when that directory is set, so tests can fake macOS tools.
fn tool(program: &str) -> PathBuf {
    let name = Path::new(program).file_name();
    match (std::env::var_os("FDS_TOOLS"), name) {
        (Some(dir), Some(name)) if program.starts_with('/') => Path::new(&dir).join(name),
        _ => PathBuf::from(program),
    }
}

// Run a command with a time limit and return its exit code and output, or None on spawn failure.
pub fn run(cmd: &[&str], timeout: f64, env: &[(&str, &str)]) -> Option<Output> {
    let mut command = Command::new(tool(cmd[0]));
    command.args(&cmd[1..]).envs(env.iter().copied());
    spawn_wait(command, timeout, None, HEARTBEAT).ok()
}

// Spawn a command and wait for it, printing a heartbeat line every interval if a label is given.
fn spawn_wait(
    mut command: Command,
    timeout: f64,
    label: Option<&str>,
    every: Duration,
) -> Result<Output, String> {
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
            if beat.elapsed() >= every {
                println!("  ... {label} still running ({})", hms(elapsed));
                beat = Instant::now();
            }
        }
        thread::sleep(Duration::from_millis(50));
    }
}

// Run a command, optionally with a heartbeat label, and return its result.
fn run_heartbeat(cmd: &[&str], timeout: f64, label: Option<&str>) -> Result<Output, String> {
    let mut command = Command::new(tool(cmd[0]));
    command.args(&cmd[1..]);
    spawn_wait(command, timeout, label, HEARTBEAT)
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
#[cfg(all(not(target_os = "macos"), not(test)))]
pub fn flags(_md: &Metadata) -> u32 {
    0
}

// Return the test-controlled stand-in for BSD file flags.
#[cfg(all(not(target_os = "macos"), test))]
pub fn flags(_md: &Metadata) -> u32 {
    FAKE_FLAGS.with(|f| {
        let (script, pos) = &mut *f.borrow_mut();
        let value = script.get(*pos).copied().unwrap_or(0);
        *pos = if *pos + 1 < script.len() { *pos + 1 } else { 0 };
        value
    })
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

// Unit tests for the shared helpers.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{chmod, Sandbox};

    // Check size parsing with and without suffixes.
    #[test]
    fn test_parse_size_suffixes() {
        assert_eq!(parse_size("10M").unwrap(), 10 << 20);
        assert_eq!(parse_size("1.5k").unwrap(), 1536);
        assert_eq!(parse_size("2GB").unwrap(), 2 << 30);
        assert_eq!(parse_size(" 3t ").unwrap(), 3 << 40);
        assert_eq!(parse_size("7").unwrap(), 7);
        assert!(parse_size("x").is_err());
    }

    // Check human-readable sizes and durations.
    #[test]
    fn test_human_and_hms() {
        assert_eq!(human(512.0), "512 B");
        assert_eq!(human(1536.0), "1.5 KiB");
        assert_eq!(human(3.0 * 1024.0 * 1024.0), "3.0 MiB");
        assert_eq!(human(5.0 * 1024f64.powi(3)), "5.0 GiB");
        assert_eq!(human(2.0 * 1024f64.powi(4)), "2.0 TiB");
        assert_eq!(human(3000.0 * 1024f64.powi(4)), "3000.0 TiB");
        assert_eq!(human_i(-2048), "-2.0 KiB");
        assert_eq!(hms(3725.0), "1:02:05");
    }

    // Check that a SHA-256 digest matches a known value and a missing file errors.
    #[test]
    fn test_sha256_known() {
        let sb = Sandbox::new();
        let file = sb.write("abc", b"abc");
        assert!(sha256(&file).unwrap().starts_with("ba7816bf"));
        assert!(sha256(&sb.path("missing")).is_err());
    }

    // Check colors appear only on a terminal without NO_COLOR.
    #[test]
    fn test_color_tty_and_no_color() {
        let _sb = Sandbox::new();
        assert_eq!(c("1", "x"), "x");
        set_tty(true);
        assert_eq!(c("1", "x"), "\x1b[1mx\x1b[0m");
        std::env::set_var("NO_COLOR", "1");
        assert_eq!(c("1", "x"), "x");
    }

    // Check the terminal width falls back to 100 columns.
    #[test]
    fn test_cols_from_cases() {
        assert_eq!(cols_from(true, 80), 80);
        assert_eq!(cols_from(true, 0), 100);
        assert_eq!(cols_from(false, 80), 100);
        assert!(term_cols() > 0);
    }

    // Check the progress ticker in non-terminal mode rate-limits and prints.
    #[test]
    fn test_ticker_plain_mode() {
        let _sb = Sandbox::new();
        let mut tk = Ticker::new();
        tk.tick("first", false);
        let first = tk.last;
        assert!(first > 0.0);
        tk.tick("skipped", false);
        assert_eq!(tk.last, first);
        tk.tick("forced", true);
        tk.clear();
        assert!(!tk.shown);
    }

    // Check the progress ticker in terminal mode shows and clears an in-place line.
    #[test]
    fn test_ticker_tty_mode() {
        let _sb = Sandbox::new();
        set_tty(true);
        let mut tk = Ticker::new();
        assert!(tk.interval < 1.0);
        tk.tick("a long progress line", true);
        assert!(tk.shown);
        tk.clear();
        assert!(!tk.shown);
        tk.clear();
    }

    // Check the stop flag and the first interrupt request.
    #[test]
    fn test_stop_and_interrupt() {
        let _sb = Sandbox::new();
        assert!(!stopped());
        interrupt();
        assert!(stopped());
        set_stop(false);
        install_sigint();
        install_sigint();
    }

    // Check warn prints and die panics under test.
    #[test]
    #[should_panic(expected = "die: boom")]
    fn test_die_panics_in_tests() {
        let _sb = Sandbox::new();
        warn("careful");
        die("boom");
    }

    // Check home and tilde expansion.
    #[test]
    fn test_expand_and_realpath() {
        let sb = Sandbox::new();
        let home = sb.home().to_string_lossy().into_owned();
        assert_eq!(super::home(), home);
        assert_eq!(expand("~"), home);
        assert_eq!(expand("~/x"), format!("{home}/x"));
        assert_eq!(expand("~bob/x"), "~bob/x");
        assert_eq!(expand("/etc"), "/etc");
        assert_eq!(realpath("~"), std::fs::canonicalize(&home).unwrap().to_string_lossy());
        assert_eq!(realpath("/no/such/dir"), "/no/such/dir");
        std::env::remove_var("HOME");
        assert_eq!(super::home(), "/");
        assert!(now() > 1e9);
    }

    // Check free-space helpers.
    #[test]
    fn test_free_space_helpers() {
        let _sb = Sandbox::new();
        assert!(free_bytes() > 0);
        let (total, free) = data_volume_space().unwrap();
        assert!(total >= free && free > 0);
        assert!(volume_at("/no/such/volume").is_none());
        let a = awake_secs();
        assert!(a > 0.0 && awake_secs() >= a);
        assert!(display_awake() || cfg!(target_os = "macos"));
        assert_eq!(free_bytes_at("/no/such/volume"), 0);
        assert_eq!(free_bytes_at("bad\0path"), 0);
        assert!(report_free("Test") > 0);
        assert!(data_volume().starts_with('/'));
    }

    // Check command running returns output, exit codes, timeouts and spawn failures.
    #[test]
    fn test_run_outcomes() {
        let sb = Sandbox::new();
        sb.tool("fake", "echo out; echo err >&2; echo \"$FDS_X\"; exit 3");
        sb.tool("slow", "sleep 5");
        let ok = run(&["fake"], 10.0, &[("FDS_X", "val")]).unwrap();
        assert_eq!((ok.code, ok.stdout.as_str(), ok.stderr.as_str()), (3, "out\nval\n", "err\n"));
        let timed = run(&["slow"], 0.2, &[]).unwrap();
        assert_eq!(timed.code, -2);
        assert!(timed.stdout.ends_with("(timed out)"));
        assert!(run(&["no-such-tool-xyz"], 1.0, &[]).is_none());
        sb.tool("killed", "kill -9 $$");
        assert_eq!(run(&["killed"], 10.0, &[]).unwrap().code, -1);
    }

    // Check the heartbeat command wrapper combines output and reports failures.
    #[test]
    fn test_run_cmd_and_heartbeat() {
        let sb = Sandbox::new();
        sb.tool("both", "echo out; echo err >&2");
        sb.tool("nap", "sleep 0.3");
        assert_eq!(run_cmd(&["both"], "both", 10.0), (0, "out\nerr\n".to_string()));
        assert_eq!(run_cmd(&["no-such-tool-xyz"], "x", 1.0).0, -1);
        let mut command = Command::new("nap");
        command.arg("x");
        let out = spawn_wait(command, 10.0, Some("nap"), Duration::from_millis(10)).unwrap();
        assert_eq!(out.code, 0);
    }

    // Check absolute tool paths are redirected only when FDS_TOOLS is set.
    #[test]
    fn test_tool_redirect() {
        let sb = Sandbox::new();
        assert_eq!(tool("/usr/bin/ditto"), sb.bin.join("ditto"));
        assert_eq!(tool("git"), PathBuf::from("git"));
        assert_eq!(tool("/"), PathBuf::from("/"));
        std::env::remove_var("FDS_TOOLS");
        assert_eq!(tool("/usr/bin/ditto"), PathBuf::from("/usr/bin/ditto"));
    }

    // Check executables are found on PATH only when they are executable files.
    #[test]
    fn test_which_lookup() {
        let sb = Sandbox::new();
        sb.tool("present", "true");
        sb.write("bin/plain", b"x");
        sb.dir("bin/adir");
        assert_eq!(which("present"), Some(sb.bin.join("present")));
        assert_eq!(which("plain"), None);
        assert_eq!(which("adir"), None);
        std::env::remove_var("PATH");
        assert_eq!(which("present"), None);
    }

    // Check metadata helpers and timestamp setting.
    #[test]
    fn test_metadata_helpers() {
        let sb = Sandbox::new();
        let file = sb.write("f", &[1u8; 5000]);
        let md = std::fs::symlink_metadata(&file).unwrap();
        assert!(alloc(&md) >= 5000);
        assert_eq!(perm_bits(&md), md.mode() & 0o7777);
        assert_eq!(flags(&md), 0);
        set_times(&file, 1_000_000_000_500, 2_000_000_000_250).unwrap();
        let md = std::fs::symlink_metadata(&file).unwrap();
        assert_eq!(mtime_ns(&md), 2_000_000_000_250);
        assert!(set_times(&sb.path("missing"), 0, 0).is_err());
        assert!(cstring(Path::new("a\0b")).is_err());
        assert!(access(&file, libc::R_OK));
        assert!(!access(&sb.path("missing"), libc::F_OK));
        assert!(!access(Path::new("a\0b"), libc::F_OK));
    }

    // Check fake BSD flags are visible through flags.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn test_flags_stand_in() {
        let sb = Sandbox::new();
        let md = std::fs::metadata(sb.write("f", b"x")).unwrap();
        set_flags(&[UF_COMPRESSED, 0]);
        assert_eq!((flags(&md), flags(&md), flags(&md)), (UF_COMPRESSED, 0, UF_COMPRESSED));
    }

    // Check extended attribute listing handles output, failure and missing tool.
    #[test]
    fn test_xattr_names_outcomes() {
        let sb = Sandbox::new();
        let file = sb.write("f", b"x");
        sb.tool("xattr", "printf 'a\\n\\ncom.apple.decmpfs\\nb\\n'");
        let names = xattr_names(&file).unwrap();
        assert_eq!(names.into_iter().collect::<Vec<_>>(), ["a", "b"]);
        sb.tool("xattr", "echo 'denied' >&2; exit 1");
        assert!(xattr_names(&file).unwrap_err().to_string().contains("denied"));
        std::fs::remove_file(sb.bin.join("xattr")).unwrap();
        assert!(xattr_names(&file).unwrap_err().to_string().contains("failed to start"));
    }

    // Check temp names stay beside the file and are removed only when present.
    #[test]
    fn test_temp_beside_and_remove() {
        let sb = Sandbox::new();
        let long = "n".repeat(300);
        let tmp = temp_beside(&sb.path(&long));
        assert_eq!(tmp.parent(), Some(sb.root.as_path()));
        assert!(tmp.file_name().unwrap().len() < 120);
        assert!(temp_beside(Path::new("/")).to_string_lossy().ends_with(".tmp"));
        remove_temp(&tmp);
        std::fs::write(&tmp, b"x").unwrap();
        chmod(&tmp, 0o600);
        remove_temp(&tmp);
        assert!(!tmp.exists());
    }
}
