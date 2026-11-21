// Investigate jobs kept on disk by run id, so a caller whose wait was cut off (a client timeout, a disconnect, a daemon restart) gets the same result and usage back without any model being asked again: the request, each model call's usage appended as it ends, and the terminal result with its usage, written before the reply is sent. Private like the output store (a 0700 folder, 0600 files, since results quote source), and reaped after a day.
use crate::adaptive::calls::{self, Call};
use crate::cmd::Output;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const MAX_AGE: Duration = Duration::from_secs(24 * 3600);
// How long past its own deadline a repeated submission waits for a run still working in this process.
const GRACE: Duration = if cfg!(test) { Duration::from_millis(300) } else { Duration::from_secs(10) };
const REQUEST: &str = "request.json";
const USAGE: &str = "usage.jsonl";
const RESULT: &str = "result.json";
const EVIDENCE: &str = "evidence.jsonl";
// How many kept evidence records an interrupted run's report lists.
const SHOW_EVIDENCE: usize = 12;

// A run working in this process: whether it ended, the signal its waiters sleep on, when its job ends, and its cancel handle.
struct Live {
    done: Mutex<bool>,
    ended: Condvar,
    until: Instant,
    stop: crate::budget::Stop,
}

// The runs working in this process, by folder.
static RUNNING: LazyLock<Mutex<HashMap<PathBuf, Arc<Live>>>> = LazyLock::new(Mutex::default);

// Returns the run store: $ZAZZLE_RUNS_DIR, else ~/.zazzle/runs.
pub fn dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("ZAZZLE_RUNS_DIR").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(d));
    }
    Some(PathBuf::from(std::env::var_os("HOME")?).join(".zazzle/runs"))
}

// True for a run id safe as a folder name: 1 to 64 letters, digits, `-`, `_` or `.`, not starting with a dot.
pub fn is_id(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && !id.starts_with('.')
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}

// Makes a new run id.
pub fn new_id() -> String {
    format!("run-{}", crate::mcp::random_hex())
}

// Milliseconds since the epoch.
fn now_ms() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis())
}

// Appends one JSON line to a private file.
pub fn append(path: &Path, v: &Value) -> io::Result<()> {
    let mut f = fs::OpenOptions::new().append(true).create(true).mode(0o600).open(path)?;
    f.write_all(format!("{v}\n").as_bytes())
}

// Writes a private file atomically through a temporary name.
fn write_private(path: &Path, text: &str) -> io::Result<()> {
    let tmp = path.with_extension("tmp");
    fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?
        .write_all(text.as_bytes())?;
    fs::rename(&tmp, path)
}

// The usage records of a run so far.
fn recorded(folder: &Path) -> Vec<Call> {
    fs::read_to_string(folder.join(USAGE))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .map(|v| Call::from_json(&v))
        .collect()
}

// A new run's handle: its folder and its entry among the runs working here; dropped unfinished, it keeps an interrupted result.
pub struct Job {
    folder: PathBuf,
    live: Arc<Live>,
    started: Instant,
    finished: bool,
}

impl Job {
    // Where each model call's usage is appended as it ends.
    pub fn usage(&self) -> PathBuf {
        self.folder.join(USAGE)
    }

    // Where the job's evidence is appended as it is found: each lookup that found something and each finding verified.
    pub fn evidence(&self) -> PathBuf {
        self.folder.join(EVIDENCE)
    }

    // The run's cancel handle, for the job's threads to watch.
    pub fn stop(&self) -> crate::budget::Stop {
        self.live.stop.clone()
    }

    // Keeps the terminal result with its usage (state `done`, or `cancelled` when it was), then wakes anyone waiting for it.
    pub fn finish(mut self, res: &Result<Output, String>) -> io::Result<()> {
        self.finished = true;
        let kept = self.keep(if self.live.stop.cancelled() { "cancelled" } else { "done" }, res);
        self.end();
        kept
    }

    // Writes the result file: the state, the output (or the error), and the usage of every call recorded.
    fn keep(&self, state: &str, res: &Result<Output, String>) -> io::Result<()> {
        let calls = recorded(&self.folder);
        let secs = self.started.elapsed().as_secs_f64();
        let mut v =
            json!({"state": state, "finished_ms": now_ms() as u64, "usage": calls::usage(&calls, secs)});
        match res {
            Ok(o) => {
                v["text"] = json!(o.text);
                v["status"] = json!(o.status);
                v["notes"] = json!(o.notes);
            }
            Err(e) => v["error"] = json!(e),
        }
        write_private(&self.folder.join(RESULT), &serde_json::to_string_pretty(&v).unwrap_or_default())
    }

    // Takes the run off the list working here and wakes its waiters.
    fn end(&self) {
        RUNNING.lock().unwrap_or_else(|p| p.into_inner()).remove(&self.folder);
        *self.live.done.lock().unwrap_or_else(|p| p.into_inner()) = true;
        self.live.ended.notify_all();
    }
}

impl Drop for Job {
    // A run that ends without finish (a panic) keeps an interrupted result, so a waiter is not left without one.
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.keep("interrupted", &Ok(interrupted(&self.folder)));
            self.end();
        }
    }
}

// What starting a run found: a new run to work on, or the result of the run that id already names.
pub enum Begun {
    New(Job),
    Earlier(Result<Output, String>),
}

// Starts run `id` for `argv` in `cwd`, ending by `until`, or returns what that id already holds: its kept result, the result of the same run still working here once it ends (no polling), or, when it was working in a daemon that stopped, an interrupted result with the usage and evidence it recorded. An empty `argv` only looks the run up; under the same id, another command line, another checkout or another state of the checkout is refused.
pub fn begin(root: &Path, id: &str, argv: &[String], cwd: &Path, until: Instant) -> Result<Begun, String> {
    if !is_id(id) {
        return Err(format!("--run-id takes 1 to 64 letters, digits, '-', '_' or '.', got '{id}'"));
    }
    let folder = root.join(id);
    let snap = if argv.is_empty() { String::new() } else { snapshot(cwd) };
    let mut running = RUNNING.lock().unwrap_or_else(|p| p.into_inner());
    if folder.join(REQUEST).is_file() {
        let asked: Value =
            serde_json::from_str(&fs::read_to_string(folder.join(REQUEST)).unwrap_or_default())
                .unwrap_or_default();
        let same = asked["argv"].as_array().is_some_and(|a| {
            a.iter().map(|x| x.as_str().unwrap_or_default()).eq(argv.iter().map(String::as_str))
        });
        let other = match () {
            _ if argv.is_empty() => None,
            _ if !same => Some("for another command line".to_string()),
            _ if asked["cwd"].as_str().is_some_and(|c| !same_dir(Path::new(c), cwd)) => {
                Some(format!("in another checkout ({})", asked["cwd"].as_str().unwrap_or("unknown")))
            }
            // A run kept before checkout states were recorded has none to compare, so it is not refused for it.
            _ if asked["snapshot"].as_str().is_some_and(|s| s != snap) => Some(format!(
                "on another state of this checkout ({} then, {snap} now)",
                asked["snapshot"].as_str().unwrap_or("unknown")
            )),
            _ => None,
        };
        if let Some(how) = other {
            return Err(format!("run {id} was started {how}; look it up with `--run-id {id}` alone, or start a new run with another id"));
        }
        let live = running.get(&folder).cloned();
        drop(running);
        return Ok(Begun::Earlier(match live {
            Some(l) => wait(&folder, id, &l),
            None => kept(&folder).unwrap_or_else(|| Ok(interrupted(&folder))),
        }));
    }
    if argv.is_empty() {
        return Err(format!("no investigate run {id}"));
    }
    reap(root, &running);
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&folder)
        .map_err(|e| format!("{}: {e}", folder.display()))?;
    let request = json!({"id": id, "argv": argv, "cwd": cwd, "snapshot": snap, "started_ms": now_ms() as u64, "pid": std::process::id()});
    write_private(&folder.join(REQUEST), &request.to_string())
        .map_err(|e| format!("{}: {e}", folder.display()))?;
    let live =
        Arc::new(Live { done: Mutex::new(false), ended: Condvar::new(), until, stop: Default::default() });
    running.insert(folder.clone(), Arc::clone(&live));
    Ok(Begun::New(Job { folder, live, started: Instant::now(), finished: false }))
}

// Cancels run `id`: a run working here stops starting new work and its waits end now, and its result (what it had gathered, with the cancel as its stop reason) is returned once it is kept; a run that already ended returns its result unchanged.
pub fn cancel(root: &Path, id: &str) -> Result<Output, String> {
    if !is_id(id) {
        return Err(format!("--run-id takes 1 to 64 letters, digits, '-', '_' or '.', got '{id}'"));
    }
    let folder = root.join(id);
    let live = RUNNING.lock().unwrap_or_else(|p| p.into_inner()).get(&folder).cloned();
    match live {
        Some(l) => {
            l.stop.cancel();
            wait(&folder, id, &l)
        }
        None if folder.join(REQUEST).is_file() => {
            kept(&folder).unwrap_or_else(|| Ok(interrupted(&folder))).map(|mut o| {
                o.notes.push_str(&format!("\n-- run {id} had already ended; nothing was cancelled\n"));
                o
            })
        }
        None => Err(format!("no investigate run {id}")),
    }
}

// Waits, without polling, for a run working in this process to end (at most its deadline and a grace), then returns its kept result.
fn wait(folder: &Path, id: &str, live: &Live) -> Result<Output, String> {
    let mut done = live.done.lock().unwrap_or_else(|p| p.into_inner());
    while !*done {
        let left = (live.until + GRACE).saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(format!("run {id} is still working past its time limit"));
        }
        done = live.ended.wait_timeout(done, left).unwrap_or_else(|p| p.into_inner()).0;
    }
    drop(done);
    kept(folder).unwrap_or_else(|| Ok(interrupted(folder)))
}

// The kept result of a run, if it has one.
fn kept(folder: &Path) -> Option<Result<Output, String>> {
    let v: Value = serde_json::from_str(&fs::read_to_string(folder.join(RESULT)).ok()?).ok()?;
    Some(match v["error"].as_str() {
        Some(e) => Err(e.to_string()),
        None => Ok(Output {
            text: v["text"].as_str().unwrap_or_default().to_string(),
            status: v["status"].as_u64().unwrap_or(0) as u8,
            idable: false,
            raw: false,
            notes: v["notes"].as_str().unwrap_or_default().to_string(),
        }),
    })
}

// The report of a run that stopped without a result: no answer, the evidence it kept and the usage its model calls recorded before it stopped (unreported counters stay unknown).
fn interrupted(folder: &Path) -> Output {
    let calls = recorded(folder);
    let secs = calls.iter().map(|c| c.secs).sum();
    let id = folder.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    Output {
        text: format!(
            "investigate run {id} was interrupted before it finished (the daemon stopped, or the run failed); no answer was kept.\n{}usage recorded before it stopped: {}\n-- ask again with a new run id to start over.",
            gathered(folder),
            calls::usage_line(&calls, secs)
        ),
        status: 1,
        idable: false,
        raw: false,
        notes: String::new(),
    }
}

// The evidence a run kept before it stopped, one line each (a finding with its citation, else a lookup that found something), for its interrupted report; nothing when it kept none.
fn gathered(folder: &Path) -> String {
    let kept: Vec<Value> = fs::read_to_string(folder.join(EVIDENCE))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    if kept.is_empty() {
        return String::new();
    }
    let line = |v: &Value| match (v["finding"].as_str(), v["lookup"].as_str()) {
        (Some(f), _) => format!(
            "- verified by {}: {f} ({})\n",
            v["by"].as_str().unwrap_or("?"),
            v["citation"].as_str().unwrap_or("?")
        ),
        (None, Some(l)) => format!("- {l} found something\n"),
        _ => String::new(),
    };
    let mut out = format!(
        "evidence kept before it stopped ({} records, in {}):\n",
        kept.len(),
        folder.join(EVIDENCE).display()
    );
    out.extend(kept.iter().take(SHOW_EVIDENCE).map(line));
    out
}

// Whether two paths are the same folder, also when one is reached through a symlink or written differently.
fn same_dir(a: &Path, b: &Path) -> bool {
    let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    canon(a) == canon(b)
}

// The state of the checkout a run reads: the commit, with a hash of the uncommitted changes (tracked diffs and the list of changed and untracked files) when there are any, or `no git checkout` outside a git work tree.
pub fn snapshot(cwd: &Path) -> String {
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| o.stdout)
    };
    let Some(head) = git(&["rev-parse", "--verify", "-q", "HEAD"]) else { return "no git checkout".into() };
    let head = String::from_utf8_lossy(&head).trim().to_string();
    let status = git(&["status", "--porcelain=v1", "-z", "--untracked-files=all"]).unwrap_or_default();
    if status.is_empty() {
        return head;
    }
    let mut h = blake3::Hasher::new();
    h.update(&status);
    h.update(&git(&["diff", "HEAD", "--binary"]).unwrap_or_default());
    format!("{head} with changes {}", &h.finalize().to_hex()[..16])
}

// Removes runs older than MAX_AGE that are not working here.
fn reap(root: &Path, running: &HashMap<PathBuf, Arc<Live>>) {
    let Ok(read) = fs::read_dir(root) else { return };
    for e in read.flatten() {
        let old =
            e.metadata().and_then(|m| m.modified()).is_ok_and(|t| t.elapsed().is_ok_and(|a| a > MAX_AGE));
        if old && !running.contains_key(&e.path()) {
            let _ = fs::remove_dir_all(e.path());
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests;
