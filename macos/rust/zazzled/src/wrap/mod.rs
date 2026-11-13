// zazzle run: runs a command in the caller's environment and prints its output cleaned for an agent, with its exit code.
use crate::cmd::clean;
use std::fs::File;
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::ExitStatusExt;
use std::process::{Command, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

pub mod profile;

pub const USAGE: &str =
    "usage: zazzle run [--lossless] [--budget T] [--timeout SECS] [--profile NAME|none] [--] CMD ARGS..";
const DEFAULT_BUDGET: usize = 8000;
const DEFAULT_TIMEOUT: u64 = 3600;
const MAX_OUTPUT: usize = 64 << 20;
const CHUNK: usize = 64 << 10;
// How long to wait for the output pipe to close after the command exits, in case a background child keeps it open.
const DRAIN_WAIT: Duration = Duration::from_secs(2);
const TIMED_OUT: i32 = 124;

// The parsed options and the command to run.
#[derive(Debug, PartialEq)]
pub struct Opts {
    pub lossy: bool,
    pub budget: usize,
    pub timeout: Duration,
    pub profile: Option<String>,
    pub argv: Vec<String>,
}

// What a finished command produced: its merged output, exit code, run time, and whether it timed out or was cut.
#[derive(Debug)]
pub struct Ran {
    pub output: Vec<u8>,
    pub code: i32,
    pub elapsed: Duration,
    pub timed_out: bool,
    pub cut: bool,
}

// Parses a positive number given after a flag.
fn number(v: Option<&String>, flag: &str) -> Result<u64, String> {
    match v.map(|s| s.parse::<u64>()) {
        Some(Ok(n)) if n > 0 => Ok(n),
        _ => Err(format!("{flag} needs a number of at least 1")),
    }
}

// Parses the options before the command; the command starts at `--` or the first argument that is not an option.
pub fn parse(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts {
        lossy: true,
        budget: DEFAULT_BUDGET,
        timeout: Duration::from_secs(DEFAULT_TIMEOUT),
        profile: None,
        argv: Vec::new(),
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--" => break,
            "--lossless" => o.lossy = false,
            "--budget" => o.budget = number(it.next(), a)? as usize,
            "--timeout" => o.timeout = Duration::from_secs(number(it.next(), a)?),
            "--profile" => {
                let name = it.next().ok_or("--profile needs a name")?;
                if name != "none" && profile::named(name).is_none() {
                    let names: Vec<&str> = profile::PROFILES.iter().map(|p| p.name).collect();
                    return Err(format!("unknown profile '{name}'; use none or one of {}", names.join(", ")));
                }
                o.profile = Some(name.clone());
            }
            flag if flag.starts_with("--") => return Err(format!("unknown run option '{flag}'; {USAGE}")),
            _ => {
                o.argv.push(a.clone());
                break;
            }
        }
    }
    o.argv.extend(it.cloned());
    if o.argv.is_empty() {
        return Err(USAGE.into());
    }
    Ok(o)
}

// Creates a pipe whose ends are not inherited by child processes, returning the read and write ends.
fn pipe() -> io::Result<(File, OwnedFd)> {
    let mut fds = [0; 2];
    // SAFETY: fds has room for the two descriptors pipe writes.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: pipe succeeded, so both descriptors are open and owned by nothing else.
    let (r, w) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    for fd in [&r, &w] {
        // SAFETY: fd is an open descriptor; setting close-on-exec has no other effect.
        unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) };
    }
    Ok((File::from(r), w))
}

// Reads the pipe into a shared buffer until it closes, keeping at most MAX_OUTPUT bytes and noting if more came.
fn drain(mut from: File, into: Arc<Mutex<(Vec<u8>, bool)>>, done: mpsc::Sender<()>) {
    let mut chunk = vec![0; CHUNK];
    while let Ok(n @ 1..) = from.read(&mut chunk) {
        let mut buf = into.lock().expect("output lock");
        let room = MAX_OUTPUT - buf.0.len();
        buf.1 |= n > room;
        buf.0.extend_from_slice(&chunk[..n.min(room)]);
    }
    let _ = done.send(());
}

// Runs the command with stdout and stderr merged into one pipe, stopping it after the timeout.
pub fn run_command(argv: &[String], timeout: Duration) -> Result<Ran, String> {
    let (prog, args) = argv.split_first().ok_or("no command given")?;
    let (reader, writer) = pipe().map_err(|e| format!("cannot create a pipe: {e}"))?;
    let err_writer = writer.try_clone().map_err(|e| format!("cannot create a pipe: {e}"))?;
    let start = Instant::now();
    let mut child = Command::new(prog)
        .args(args)
        .stdout(Stdio::from(writer))
        .stderr(Stdio::from(err_writer))
        .spawn()
        .map_err(|e| format!("cannot run {prog}: {e}"))?;
    let buf = Arc::new(Mutex::new((Vec::new(), false)));
    let (done_tx, done_rx) = mpsc::channel();
    let shared = Arc::clone(&buf);
    std::thread::spawn(move || drain(reader, shared, done_tx));
    let pid = child.id();
    let (status_tx, status_rx) = mpsc::channel();
    std::thread::spawn(move || status_tx.send(child.wait()));
    let (status, timed_out) = match status_rx.recv_timeout(timeout) {
        Ok(s) => (s, false),
        Err(_) => {
            // SAFETY: kill only sends a signal; pid is the child this process started and still waits for.
            unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
            (status_rx.recv().expect("waiter answers"), true)
        }
    };
    let status = status.map_err(|e| format!("waiting for {prog}: {e}"))?;
    let elapsed = start.elapsed();
    let _ = done_rx.recv_timeout(DRAIN_WAIT);
    let (output, cut) = std::mem::take(&mut *buf.lock().expect("output lock"));
    let code = if timed_out {
        TIMED_OUT
    } else {
        status.code().unwrap_or_else(|| 128 + status.signal().unwrap_or(0))
    };
    Ok(Ran { output, code, elapsed, timed_out, cut })
}

// Cleans a finished command's output with its profile and the clean steps, and adds a footer with the exit code.
pub fn render(o: &Opts, ran: &Ran) -> String {
    render_parts(o, ran).0
}

// Renders the output and says whether anything was dropped or cut that read-again --full can bring back.
fn render_parts(o: &Opts, ran: &Ran) -> (String, bool) {
    let text = String::from_utf8_lossy(&ran.output);
    let chosen = match o.profile.as_deref() {
        _ if !o.lossy => None,
        Some("none") => None,
        Some(name) => profile::named(name),
        None => profile::detect(&o.argv),
    };
    let tidy = clean::tidy_lines(&text);
    let (lines, dropped) = match chosen {
        Some(p) => p.apply(tidy),
        None => (tidy, profile::Dropped::default()),
    };
    let (body, steps) = clean::shrink_steps(&lines.join("\n"), o.lossy, o.budget);
    let cut = ran.cut || !dropped.0.is_empty() || clean::cuts(&steps);
    let mut footer = format!("[exit {} in {:.1}s", ran.code, ran.elapsed.as_secs_f64());
    if ran.timed_out {
        footer.push_str(&format!("; killed after the {}s timeout", o.timeout.as_secs()));
    }
    if ran.cut {
        footer.push_str(&format!("; output over {} MiB was not read", MAX_OUTPUT >> 20));
    }
    if let Some(p) = chosen {
        let what = dropped.describe();
        footer.push_str(&format!("; {} profile", p.name));
        if !what.is_empty() {
            footer.push_str(&format!(" dropped {what}"));
        }
    }
    footer.push(']');
    (if body.is_empty() { footer } else { format!("{body}\n{footer}") }, cut)
}

// Renders the output and, given a store, keeps it with the full output for read-again and tags it with the id.
pub fn deliver(o: &Opts, ran: &Ran, store: Option<&std::path::Path>) -> String {
    let (text, cut) = render_parts(o, ran);
    match store {
        Some(dir) => clean::store(dir, text, &String::from_utf8_lossy(&ran.output), cut),
        None => text,
    }
}

// Runs `zazzled wrap ARGS`: parses, runs the command, prints the cleaned output and returns its exit code.
pub fn main(args: &[String], out: &mut dyn io::Write) -> Result<i32, String> {
    let o = parse(args)?;
    let ran = run_command(&o.argv, o.timeout)?;
    writeln!(out, "{}", deliver(&o, &ran, crate::outputs::dir().as_deref())).map_err(|e| e.to_string())?;
    Ok(ran.code)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    // Converts string slices into owned arguments.
    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    // Builds options for a command with defaults.
    fn opts(argv: &[&str]) -> Opts {
        parse(&v(argv)).expect("opts")
    }

    // A finished run with the given output and exit code.
    fn ran(output: &str, code: i32) -> Ran {
        Ran {
            output: output.as_bytes().to_vec(),
            code,
            elapsed: Duration::from_millis(1500),
            timed_out: false,
            cut: false,
        }
    }

    // Checks options before the command, `--`, and that the command's own flags are left alone.
    #[test]
    fn test_parse() {
        let o = parse(&v(&[
            "--lossless",
            "--budget",
            "50",
            "--timeout",
            "9",
            "--profile",
            "go",
            "--",
            "go",
            "test",
        ]))
        .expect("opts");
        assert_eq!(
            o,
            Opts {
                lossy: false,
                budget: 50,
                timeout: Duration::from_secs(9),
                profile: Some("go".into()),
                argv: v(&["go", "test"])
            }
        );
        let o = opts(&["cargo", "test", "--", "--nocapture"]);
        assert_eq!(
            (o.lossy, o.budget, o.profile, o.argv),
            (true, DEFAULT_BUDGET, None, v(&["cargo", "test", "--", "--nocapture"]))
        );
        assert_eq!(opts(&["--profile", "none", "make"]).profile.as_deref(), Some("none"));
    }

    // Checks the one-line errors for bad options and a missing command.
    #[test]
    fn test_parse_errors() {
        let err = |a: &[&str]| parse(&v(a)).unwrap_err();
        assert_eq!(err(&[]), USAGE);
        assert_eq!(err(&["--"]), USAGE);
        assert_eq!(err(&["--budget", "0", "x"]), "--budget needs a number of at least 1");
        assert_eq!(err(&["--timeout"]), "--timeout needs a number of at least 1");
        assert_eq!(err(&["--profile"]), "--profile needs a name");
        assert!(err(&["--profile", "maven", "x"])
            .starts_with("unknown profile 'maven'; use none or one of cargo, pytest"));
        assert!(err(&["--what", "x"]).starts_with("unknown run option '--what'; usage:"));
    }

    // Checks a real command: merged output in order, its exit code, and an unknown program.
    #[test]
    fn test_run_command() {
        let r = run_command(
            &v(&["sh", "-c", "echo out; echo err >&2; echo out2; exit 3"]),
            Duration::from_secs(30),
        )
        .expect("run");
        assert_eq!(
            (String::from_utf8_lossy(&r.output).as_ref(), r.code, r.timed_out, r.cut),
            ("out\nerr\nout2\n", 3, false, false)
        );
        let killed = run_command(&v(&["sh", "-c", "kill -9 $$"]), Duration::from_secs(30)).expect("run");
        assert_eq!(killed.code, 128 + 9);
        assert!(run_command(&v(&["/nonexistent/zazzle-cmd"]), Duration::from_secs(1))
            .unwrap_err()
            .starts_with("cannot run /nonexistent/zazzle-cmd: "));
        assert_eq!(run_command(&[], Duration::from_secs(1)).unwrap_err(), "no command given");
    }

    // Checks the timeout kills the command and keeps what it printed.
    #[test]
    fn test_run_command_timeout() {
        let r = run_command(&v(&["sh", "-c", "echo started; exec sleep 30"]), Duration::from_secs(1))
            .expect("run");
        assert_eq!(
            (String::from_utf8_lossy(&r.output).as_ref(), r.code, r.timed_out),
            ("started\n", TIMED_OUT, true)
        );
        assert!(r.elapsed < Duration::from_secs(10));
    }

    // Checks output beyond the cap is counted as cut rather than kept.
    #[test]
    fn test_run_command_cap() {
        let script = format!("head -c {} /dev/zero", MAX_OUTPUT + CHUNK * 2);
        let r = run_command(&v(&["sh", "-c", &script]), Duration::from_secs(60)).expect("run");
        assert_eq!((r.output.len(), r.cut), (MAX_OUTPUT, true));
    }

    // Checks the detected profile drops noise, the clean steps apply and the footer reports both.
    #[test]
    fn test_render_profile() {
        let o = opts(&["cargo", "test"]);
        let text = render(
            &o,
            &ran("\x1b[32m   Compiling demo v0.1\x1b[0m\ntest a ... ok\ntest b ... FAILED\n", 101),
        );
        assert_eq!(
            text,
            "test b ... FAILED\n[exit 101 in 1.5s; cargo profile dropped 1 progress lines, 1 passing tests]"
        );
        let quiet = render(&o, &ran("test a ... ok\n", 0));
        assert_eq!(quiet, "[exit 0 in 1.5s; cargo profile dropped 1 passing tests]");
        let nothing = render(&o, &ran("hello\n", 0));
        assert_eq!(nothing, "hello\n[exit 0 in 1.5s; cargo profile]");
    }

    // Checks --profile none, an explicit profile, --lossless and the timeout and cut notes.
    #[test]
    fn test_render_options() {
        let out = "test a ... ok\n--- PASS: T (0s)\n";
        assert_eq!(
            render(&opts(&["--profile", "none", "cargo"]), &ran(out, 0)),
            "test a ... ok\n--- PASS: T (0s)\n[exit 0 in 1.5s]"
        );
        assert_eq!(
            render(&opts(&["--profile", "go", "x"]), &ran(out, 0)),
            "test a ... ok\n[exit 0 in 1.5s; go profile dropped 1 passing tests]"
        );
        assert_eq!(
            render(&opts(&["--lossless", "cargo", "test"]), &ran(out, 0)),
            "test a ... ok\n--- PASS: T (0s)\n[exit 0 in 1.5s]"
        );
        let mut r = ran("x\n", TIMED_OUT);
        r.timed_out = true;
        r.cut = true;
        assert_eq!(
            render(&opts(&["--timeout", "5", "make"]), &r),
            "x\n[exit 124 in 1.5s; killed after the 5s timeout; output over 64 MiB was not read]"
        );
    }

    // Checks a stored run output carries its id, a hint when the profile dropped lines, and comes back in full.
    #[test]
    fn test_deliver_stores_output() {
        let t = crate::testutil::TempDir::new();
        let dir = t.path().join("out");
        let shown =
            deliver(&opts(&["cargo", "test"]), &ran("test a ... ok\ntest b ... FAILED\n", 101), Some(&dir));
        assert!(shown.starts_with("[r:") && shown.contains("test b ... FAILED\n[exit 101"), "{shown}");
        let id = &shown[3..13];
        assert!(shown.ends_with(&format!("\n[full output: read-again r:{id} --full]")));
        let stored = crate::outputs::load(&dir, id).expect("stored");
        assert_eq!(format!("[r:{id}]\n{}", stored.shown), shown);
        assert_eq!(stored.full, "test a ... ok\ntest b ... FAILED\n");
        let plain = deliver(&opts(&["make"]), &ran("ok\n", 0), Some(&dir));
        assert!(plain.starts_with("[r:") && !plain.contains("--full"), "{plain}");
        assert_eq!(deliver(&opts(&["make"]), &ran("ok\n", 0), None), "ok\n[exit 0 in 1.5s]");
    }

    // Checks the entry point prints the rendered output and returns the command's exit code, or an option error.
    #[test]
    fn test_main() {
        let _g = crate::testutil::env_lock();
        let t = crate::testutil::TempDir::new();
        let saved = std::env::var_os("ZAZZLE_OUTPUTS_DIR");
        std::env::set_var("ZAZZLE_OUTPUTS_DIR", t.path());
        let mut out = Vec::new();
        assert_eq!(main(&v(&["sh", "-c", "echo hi; exit 2"]), &mut out), Ok(2));
        let text = String::from_utf8_lossy(&out).into_owned();
        assert!(text.starts_with("[r:") && text.contains("]\nhi\n[exit 2 in "), "{text}");
        assert!(crate::outputs::load(t.path(), &text[3..13]).is_ok());
        match saved {
            Some(v) => std::env::set_var("ZAZZLE_OUTPUTS_DIR", v),
            None => std::env::remove_var("ZAZZLE_OUTPUTS_DIR"),
        }
        assert_eq!(
            main(&v(&["--bogus"]), &mut out).unwrap_err().split(';').next(),
            Some("unknown run option '--bogus'")
        );
    }
}
