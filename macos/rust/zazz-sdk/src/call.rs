// The per-call context: mode, stdio and the edge services (filesystem, network, clock, env) that tests can fake.
use crate::error::{Error, Result};
use crate::http::{Request, Response};
use crate::json::Json;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

// Who is calling: an agent wanting JSON, or a human at a shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Agent,
    Human,
}

// One directory entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
}

// Filesystem access; paths are relative to the caller's directory unless absolute.
pub trait Fs {
    // Reads a whole file.
    fn read(&self, path: &str) -> Result<Vec<u8>>;
    // Creates or replaces a file.
    fn write(&self, path: &str, data: &[u8]) -> Result<()>;
    // Lists a directory, sorted by name.
    fn list(&self, path: &str) -> Result<Vec<DirEntry>>;
    // True if the path exists.
    fn exists(&self, path: &str) -> bool;
}

// Outgoing HTTPS.
pub trait Http {
    // Sends a request and waits for the response.
    fn send(&self, req: &Request) -> Result<Response>;
}

// Wall-clock time.
pub trait Clock {
    // Milliseconds since the Unix epoch.
    fn now_ms(&self) -> u64;
}

// Environment variables.
pub trait Env {
    // Returns a variable.
    fn var(&self, key: &str) -> Option<String>;
}

// The real filesystem, rooted at a directory.
pub struct StdFs {
    pub root: PathBuf,
}

impl StdFs {
    // Resolves a path against the root.
    fn at(&self, path: &str) -> PathBuf {
        let p = Path::new(path);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.root.join(p)
        }
    }
}

impl Fs for StdFs {
    // Reads a whole file.
    fn read(&self, path: &str) -> Result<Vec<u8>> {
        std::fs::read(self.at(path)).map_err(|e| with_path(path, e))
    }

    // Creates or replaces a file.
    fn write(&self, path: &str, data: &[u8]) -> Result<()> {
        std::fs::write(self.at(path), data).map_err(|e| with_path(path, e))
    }

    // Lists a directory, sorted by name.
    fn list(&self, path: &str) -> Result<Vec<DirEntry>> {
        let rd = std::fs::read_dir(self.at(path)).map_err(|e| with_path(path, e))?;
        let mut out = Vec::new();
        for item in rd {
            let item = item.map_err(|e| with_path(path, e))?;
            let is_dir = item.file_type().is_ok_and(|t| t.is_dir());
            out.push(DirEntry { name: item.file_name().to_string_lossy().into_owned(), is_dir });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    // True if the path exists.
    fn exists(&self, path: &str) -> bool {
        self.at(path).exists()
    }
}

// Prefixes an I/O error with the path it concerns.
fn with_path(path: &str, e: std::io::Error) -> Error {
    let mut err = Error::from(e);
    err.message = format!("{path}: {}", err.message);
    err
}

// Network access that always refuses, for hosts without a client.
pub struct NoHttp;

impl Http for NoHttp {
    // Refuses every request.
    fn send(&self, req: &Request) -> Result<Response> {
        Err(Error::network_denied(&format!("{}: no network client is available here", req.url)))
    }
}

// The system clock.
pub struct SystemClock;

impl Clock for SystemClock {
    // Milliseconds since the Unix epoch.
    fn now_ms(&self) -> u64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
    }
}

// The process environment.
pub struct StdEnv;

impl Env for StdEnv {
    // Returns a variable.
    fn var(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
}

// Everything a handler can touch during one call.
pub struct Call {
    pub id: i64,
    pub mode: Mode,
    pub exit: i64,
    pub stdin: Box<dyn Read>,
    pub stdout: Box<dyn Write>,
    pub stderr: Box<dyn Write>,
    pub fs: Box<dyn Fs>,
    pub http: Box<dyn Http>,
    pub clock: Box<dyn Clock>,
    pub env: Box<dyn Env>,
    pub progress_sink: Box<dyn FnMut(&str)>,
    pub cancel_probe: Box<dyn Fn() -> bool>,
}

impl Call {
    // True when a human is calling and wants plain output.
    pub fn is_human(&self) -> bool {
        self.mode == Mode::Human
    }

    // Standard input.
    pub fn stdin(&mut self) -> &mut dyn Read {
        &mut *self.stdin
    }

    // Standard output.
    pub fn stdout(&mut self) -> &mut dyn Write {
        &mut *self.stdout
    }

    // Standard error.
    pub fn stderr(&mut self) -> &mut dyn Write {
        &mut *self.stderr
    }

    // Sets the exit status a human caller sees (0 by default).
    pub fn set_exit(&mut self, code: i64) {
        self.exit = code;
    }

    // Writes a line to standard output.
    pub fn println(&mut self, line: &str) -> Result<()> {
        writeln!(self.stdout, "{line}").map_err(Error::from)
    }

    // Reads a whole input: standard input for "-", otherwise a file.
    pub fn read_input(&mut self, path: &str) -> Result<Vec<u8>> {
        if path != "-" {
            return self.fs.read(path);
        }
        let mut buf = Vec::new();
        self.stdin.read_to_end(&mut buf)?;
        Ok(buf)
    }

    // The filesystem.
    pub fn fs(&self) -> &dyn Fs {
        &*self.fs
    }

    // The network.
    pub fn http(&self) -> &dyn Http {
        &*self.http
    }

    // The clock.
    pub fn clock(&self) -> &dyn Clock {
        &*self.clock
    }

    // The environment.
    pub fn env(&self) -> &dyn Env {
        &*self.env
    }

    // Reports progress to the caller (shown on stderr for humans).
    pub fn progress(&mut self, msg: &str) {
        (self.progress_sink)(msg);
    }

    // True once the caller has cancelled; long loops should check it and return Error::cancelled().
    pub fn cancelled(&self) -> bool {
        (self.cancel_probe)()
    }

    // Returns Err(cancelled) when the caller has cancelled.
    pub fn check_cancelled(&self) -> Result<()> {
        if self.cancelled() {
            Err(Error::cancelled())
        } else {
            Ok(())
        }
    }

    // A call with real services rooted at a directory and the given streams.
    pub fn real(
        id: i64,
        mode: Mode,
        root: PathBuf,
        stdio: (Box<dyn Read>, Box<dyn Write>, Box<dyn Write>),
        http: Box<dyn Http>,
    ) -> Call {
        Call {
            id,
            mode,
            exit: 0,
            stdin: stdio.0,
            stdout: stdio.1,
            stderr: stdio.2,
            fs: Box::new(StdFs { root }),
            http,
            clock: Box::new(SystemClock),
            env: Box::new(StdEnv),
            progress_sink: Box::new(|_| {}),
            cancel_probe: Box::new(|| false),
        }
    }
}

// Reads a JSON input value for the call from text.
pub fn input_json(text: &str) -> Result<Json> {
    Json::parse(text)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static N: AtomicUsize = AtomicUsize::new(0);

    // Makes a fresh temporary directory.
    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "zazz-sdk-call-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&d).expect("mkdir");
        d
    }

    // Builds a real call over in-memory streams.
    fn call(root: PathBuf, stdin: &'static [u8]) -> Call {
        Call::real(
            1,
            Mode::Agent,
            root,
            (Box::new(stdin), Box::new(Vec::new()), Box::new(Vec::new())),
            Box::new(NoHttp),
        )
    }

    // Checks the real filesystem: write, read, list, exists, absolute paths and error mapping.
    #[test]
    fn test_std_fs() {
        let d = tmp();
        let c = call(d.clone(), b"");
        c.fs().write("a.txt", b"hi").expect("write");
        std::fs::create_dir(d.join("sub")).expect("sub");
        assert_eq!(c.fs().read("a.txt").expect("read"), b"hi");
        assert_eq!(c.fs().read(d.join("a.txt").to_str().expect("utf8")).expect("abs"), b"hi");
        let names: Vec<(String, bool)> =
            c.fs().list(".").expect("list").into_iter().map(|e| (e.name, e.is_dir)).collect();
        assert_eq!(names, vec![("a.txt".into(), false), ("sub".into(), true)]);
        assert!(c.fs().exists("a.txt") && !c.fs().exists("b"));
        let e = c.fs().read("missing").unwrap_err();
        assert_eq!((e.code.as_str(), e.message.starts_with("missing: ")), ("not_found", true));
        assert!(c.fs().list("missing").is_err());
        assert!(c.fs().write("no/dir/x", b"").is_err());
        let _ = std::fs::remove_dir_all(d);
    }

    // Checks stdio helpers, read_input, progress, cancellation and the default services.
    #[test]
    fn test_call_helpers() {
        let mut c = call(tmp(), b"from stdin");
        assert!(!c.is_human());
        c.set_exit(3);
        assert_eq!(c.exit, 3);
        assert_eq!(c.read_input("-").expect("stdin"), b"from stdin");
        c.println("out").expect("println");
        c.stdout().write_all(b"x").expect("stdout");
        c.stderr().write_all(b"e").expect("stderr");
        let mut s = String::new();
        c.stdin().read_to_string(&mut s).expect("stdin drained");
        c.progress("half");
        assert!(!c.cancelled());
        assert!(c.check_cancelled().is_ok());
        c.cancel_probe = Box::new(|| true);
        assert_eq!(c.check_cancelled().unwrap_err().code, "cancelled");
        assert!(c.read_input("nope").is_err());
        assert_eq!(c.http().send(&Request::get("https://a.com/")).unwrap_err().code, "network_denied");
        assert!(c.clock().now_ms() > 1_600_000_000_000);
        assert!(c.env().var("PATH").is_some() || c.env().var("PATH").is_none());
        assert_eq!(input_json("[1]").expect("json"), crate::json!([1]));
    }
}
