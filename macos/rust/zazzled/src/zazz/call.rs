// zazz call: runs a registered script in a warm worker, handing it the caller's streams (or pipes when there are none).
use super::pipeline::err;
use super::pool::{self, Spec};
use super::registry::Registry;
use super::Zazz;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, Instant};
use zazz::{fdpass, proto};
use zazz_format::manifest::Manifest;

const GRACE: Duration = Duration::from_millis(200);
const WATCHDOG_EXTRA: Duration = Duration::from_secs(5);
const CAPTURE_LIMIT: usize = 1 << 20;

static NEXT_ID: AtomicI64 = AtomicI64::new(1);

// The streams a call runs with: the caller's own, or pipes whose output is captured.
pub enum Streams {
    Caller { stdio: [OwnedFd; 3], conn: Option<RawFd> },
    Capture { stdin: Vec<u8> },
}

// What a finished call produced.
#[derive(Debug)]
pub struct Called {
    pub result: Value,
    pub stdout: String,
    pub stderr: String,
    pub progress: Vec<String>,
}

// Maps a sandbox path to a host path and the path the guest sees.
fn mount(entry: &str, cwd: &Path, tmp: &Path) -> Option<(PathBuf, String)> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let (base, guest, rest) = if let Some(r) = entry.strip_prefix("$CWD") {
        (cwd.to_path_buf(), ".".to_string(), r)
    } else if let Some(r) = entry.strip_prefix("$TMP") {
        (tmp.to_path_buf(), "/tmp".to_string(), r)
    } else if let Some(r) = entry.strip_prefix("$HOME") {
        let h = home?;
        let g = h.to_string_lossy().into_owned();
        (h, g, r)
    } else {
        (PathBuf::from(entry), entry.to_string(), "")
    };
    let rest = rest.trim_start_matches('/');
    let host = if rest.is_empty() { base } else { base.join(rest) };
    let guest = match (guest.as_str(), rest.is_empty()) {
        (_, true) => guest,
        (".", false) => rest.to_string(),
        (g, false) => format!("{}/{rest}", g.trim_end_matches('/')),
    };
    host.exists().then_some((host, guest))
}

// Builds the worker's preopened directories from the manifest's sandbox for this caller.
pub fn preopens(m: &Manifest, cwd: &Path, tmp: &Path) -> Vec<Value> {
    let mut out: Vec<(PathBuf, String, bool)> = Vec::new();
    let writes: Vec<String> = m.sandbox.write.clone();
    for entry in m.sandbox.read.iter().chain(writes.iter()) {
        let Some((host, guest)) = mount(entry, cwd, tmp) else { continue };
        let write = writes.contains(entry);
        match out.iter_mut().find(|(_, g, _)| *g == guest) {
            Some(slot) => slot.2 |= write,
            None => out.push((host, guest, write)),
        }
    }
    out.into_iter().map(|(h, g, w)| json!({"host": h.to_string_lossy(), "guest": g, "write": w})).collect()
}

// The worker spec for a script, caller directory and policy.
fn spec(z: &Zazz, reg: &Registry, hex: &str, m: &Manifest, cwd: &Path) -> Result<Spec, Value> {
    let tmp = reg.root.join("tmp").join(format!("run-{}", &hex[..16]));
    std::fs::create_dir_all(&tmp).map_err(|e| err("store", &e.to_string()))?;
    let hello = json!({
        "t": "hello", "proto": 1, "call_ms": m.call_ms, "memory_mb": m.memory_mb, "network": "none",
        "args": [m.name], "env": {}, "preopens": preopens(m, cwd, &tmp),
    });
    let worker = z.worker()?.to_string_lossy().into_owned();
    let key = format!("{hex}\n{}\n{hello}", cwd.display());
    Ok(Spec {
        key,
        program: vec![worker, "serve".into()],
        module: Some(reg.wasm_path(hex, "cwasm")),
        cwd: cwd.to_path_buf(),
        hello,
    })
}

// Pipes for a captured call: the child ends to pass, and threads collecting stdout and stderr.
struct Pipes {
    child: [OwnedFd; 3],
    out: std::thread::JoinHandle<Vec<u8>>,
    err: std::thread::JoinHandle<Vec<u8>>,
}

// Creates a pipe and returns (read end, write end).
fn pipe() -> Result<(OwnedFd, OwnedFd), Value> {
    let mut fds = [0; 2];
    // SAFETY: fds is a two-element array for pipe to fill.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(err("io", &std::io::Error::last_os_error().to_string()));
    }
    // SAFETY: pipe returned two fresh descriptors that nothing else owns.
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

// Reads a pipe to the end, keeping at most the capture limit.
fn collect(fd: OwnedFd) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = std::fs::File::from(fd).take(CAPTURE_LIMIT as u64).read_to_end(&mut buf);
        buf
    })
}

// Sets up pipes: stdin is fed from memory, stdout and stderr are collected.
fn capture(stdin: Vec<u8>) -> Result<Pipes, Value> {
    let (in_r, in_w) = pipe()?;
    let (out_r, out_w) = pipe()?;
    let (err_r, err_w) = pipe()?;
    std::thread::spawn(move || drop(std::fs::File::from(in_w).write_all(&stdin)));
    Ok(Pipes { child: [in_r, out_w, err_w], out: collect(out_r), err: collect(err_r) })
}

// Waits for readable input on the worker socket or a hang-up from the caller; returns which.
fn wait(sock: RawFd, conn: Option<RawFd>, timeout: Duration) -> (bool, bool) {
    let mut p = [
        libc::pollfd { fd: sock, events: libc::POLLIN, revents: 0 },
        libc::pollfd { fd: conn.unwrap_or(-1), events: libc::POLLIN, revents: 0 },
    ];
    let ms = timeout.as_millis().min(i32::MAX as u128) as i32;
    // SAFETY: p is a live array of two pollfds; a negative fd is ignored by poll.
    unsafe { libc::poll(p.as_mut_ptr(), 2, ms) };
    (p[0].revents != 0, p[1].revents != 0)
}

// Sends the call, then relays progress until the result arrives, the caller hangs up or the watchdog fires.
fn exchange(
    sock: &mut UnixStream,
    msg: &Value,
    fds: &[RawFd],
    conn: Option<RawFd>,
    limit: Duration,
    progress: &mut dyn FnMut(&str),
) -> Result<Value, (Value, bool)> {
    let body = msg.to_string();
    let mut frame = (body.len() as u32).to_le_bytes().to_vec();
    frame.extend_from_slice(body.as_bytes());
    fdpass::send_with_fds(sock.as_raw_fd(), &frame, fds)
        .map_err(|e| (err("protocol", &e.to_string()), false))?;
    let deadline = Instant::now() + limit;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err((err("timeout", "the call ran past its time limit"), false));
        }
        let (ready, hup) = wait(sock.as_raw_fd(), conn, left);
        if hup && !ready {
            let _ = proto::write_frame(sock, &zazz::json!({"t": "cancel"}));
            let _ = wait(sock.as_raw_fd(), None, GRACE);
            return Err((err("cancelled", "the caller went away"), false));
        }
        if !ready {
            continue;
        }
        let frame = match proto::read_frame(sock) {
            Ok(Some(f)) => pool::from_sdk(&f),
            _ => return Err((err("script_crashed", "the script worker stopped"), false)),
        };
        match frame["t"].as_str() {
            Some("progress") => progress(frame["msg"].as_str().unwrap_or("")),
            Some("result") => return Ok(frame),
            _ => return Err((err("protocol", "unexpected message from the script"), false)),
        }
    }
}

// Runs one message against a registered script and returns its result frame and captured output.
pub fn execute(z: &Zazz, hex: &str, cwd: &Path, mut msg: Value, streams: Streams) -> Result<Called, Value> {
    let reg = z.registry().map_err(|e| err("store", &e))?;
    let meta = reg.read_meta(hex).ok_or_else(|| err("not_found", &format!("no script b3:{hex}")))?;
    let m =
        Manifest::parse(meta["manifest"].to_string().as_bytes()).map_err(|e| err("invalid_manifest", &e))?;
    let spec = spec(z, &reg, hex, &m, cwd)?;
    msg["id"] = json!(NEXT_ID.fetch_add(1, Ordering::SeqCst));
    let mut w = z.pool.acquire(&spec).map_err(|e| err("script_crashed", &e))?;
    let (stdio, conn, pipes) = match streams {
        Streams::Caller { stdio, conn } => (stdio, conn, None),
        Streams::Capture { stdin } => {
            let p = capture(stdin)?;
            let fds = [p.child[0].try_clone(), p.child[1].try_clone(), p.child[2].try_clone()];
            let fds = fds.map(|f| f.expect("dup a pipe end"));
            (fds, None, Some(p))
        }
    };
    let raw = [stdio[0].as_raw_fd(), stdio[1].as_raw_fd(), stdio[2].as_raw_fd()];
    let mut notes = Vec::new();
    let show = (msg["mode"] == "human" && pipes.is_none()).then(|| stdio[2].try_clone().ok()).flatten();
    let limit = Duration::from_millis(m.call_ms) + WATCHDOG_EXTRA;
    let r = {
        let mut on_progress = |p: &str| {
            notes.push(p.to_string());
            if let Some(e) = show.as_ref().and_then(|e| e.try_clone().ok()) {
                let _ = writeln!(std::fs::File::from(e), "{}: {p}", m.name);
            }
        };
        exchange(&mut w.sock, &msg, &raw, conn, limit, &mut on_progress)
    };
    drop((stdio, show));
    let (stdout, stderr) = match pipes {
        Some(p) => {
            drop(p.child);
            let text = |h: std::thread::JoinHandle<Vec<u8>>| {
                String::from_utf8_lossy(&h.join().unwrap_or_default()).into_owned()
            };
            (text(p.out), text(p.err))
        }
        None => (String::new(), String::new()),
    };
    match r {
        Ok(result) => {
            z.pool.release(&spec.key, w, true);
            Ok(Called { result, stdout, stderr, progress: notes })
        }
        Err((e, _)) => {
            w.kill();
            Err(e)
        }
    }
}

// Parses `ID [JSON|-]` or `ID OPERATION [--params JSON] [--body JSON|-]` into the script id and message.
fn parse(
    args: &[String],
    read_stdin: &mut dyn FnMut() -> Result<String, String>,
) -> Result<(String, Value), Value> {
    let usage = || {
        err(
            "usage",
            "usage: zazz call ID [JSON|-]  |  zazz call ID OPERATION [--params JSON] [--body JSON|-]",
        )
    };
    let json_arg = |s: &str, f: &mut dyn FnMut() -> Result<String, String>| -> Result<Value, Value> {
        let text = if s == "-" { f().map_err(|e| err("bad_input", &e))? } else { s.to_string() };
        serde_json::from_str(&text).map_err(|e| err("bad_input", &format!("input is not JSON: {e}")))
    };
    let (id, rest) = args.split_first().ok_or_else(usage)?;
    let Some((second, more)) = rest.split_first() else {
        return Ok((id.clone(), json!({"t": "json-command", "mode": "agent", "input": null})));
    };
    if second == "-" || serde_json::from_str::<Value>(second).is_ok() {
        if !more.is_empty() {
            return Err(usage());
        }
        return Ok((
            id.clone(),
            json!({"t": "json-command", "mode": "agent", "input": json_arg(second, read_stdin)?}),
        ));
    }
    let (mut params, mut body) = (json!({}), Value::Null);
    let mut it = more.iter();
    while let Some(flag) = it.next() {
        let v = it.next().ok_or_else(usage)?;
        match flag.as_str() {
            "--params" => params = json_arg(v, read_stdin)?,
            "--body" => body = json_arg(v, read_stdin)?,
            _ => return Err(usage()),
        }
    }
    Ok((
        id.clone(),
        json!({"t": "service-call", "mode": "agent", "operation": second, "params": params, "body": body}),
    ))
}

// Turns a result frame into the agent's reply: the output, or {"error": ...}.
pub fn agent_reply(c: Called) -> Result<Value, Value> {
    let mut out = if c.result["ok"] == true {
        c.result["output"].clone()
    } else {
        return Err(c.result["error"].clone());
    };
    if !c.stdout.is_empty() || !c.stderr.is_empty() {
        out = json!({"output": out, "stdout": c.stdout, "stderr": c.stderr});
    }
    Ok(out)
}

// Runs `zazz call`.
pub fn run(ctx: &crate::cmd::Ctx, args: &[String]) -> Result<Value, Value> {
    let z = &ctx.engine.zazz;
    let mut stdin_used = false;
    let mut read = || {
        stdin_used = true;
        ctx.read_stdin().map(|b| String::from_utf8_lossy(&b).into_owned())
    };
    let (id, msg) = parse(args, &mut read)?;
    let reg = z.registry().map_err(|e| err("store", &e))?;
    let hex = reg.resolve(&id).map_err(|e| err("not_found", &e))?;
    let streams = match ctx.caller.borrow_mut().take() {
        Some(c) if !stdin_used => Streams::Caller { stdio: c.stdio, conn: c.conn },
        Some(c) => {
            let null = std::fs::File::open("/dev/null").map_err(|e| err("io", &e.to_string()))?;
            let [_, o, e] = c.stdio;
            Streams::Caller { stdio: [null.into(), o, e], conn: c.conn }
        }
        None => Streams::Capture {
            stdin: if stdin_used { Vec::new() } else { ctx.read_stdin().unwrap_or_default() },
        },
    };
    agent_reply(execute(z, &hex, &ctx.cwd, msg, streams)?)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    // Turns string slices into owned arguments.
    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    // Checks the two call forms, stdin input and usage errors.
    #[test]
    fn test_parse() {
        let mut no = || Err::<String, String>("no stdin".into());
        assert_eq!(parse(&v(&["id"]), &mut no).expect("bare").1["input"], Value::Null);
        assert_eq!(parse(&v(&["id", r#"{"a":1}"#]), &mut no).expect("json").1["input"]["a"], 1);
        let mut yes = || Ok::<String, String>("[2]".into());
        assert_eq!(parse(&v(&["id", "-"]), &mut yes).expect("stdin").1["input"], json!([2]));
        let (_, svc) =
            parse(&v(&["id", "sum", "--params", r#"{"n":1}"#, "--body", "-"]), &mut yes).expect("svc");
        assert_eq!(
            (svc["operation"].as_str(), svc["params"]["n"].as_i64(), svc["body"].clone()),
            (Some("sum"), Some(1), json!([2]))
        );
        assert!(parse(&v(&[]), &mut no).is_err());
        assert!(parse(&v(&["id", "{}", "x"]), &mut no).is_err());
        assert!(parse(&v(&["id", "op", "--params"]), &mut no).is_err());
        assert!(parse(&v(&["id", "op", "--x", "1"]), &mut no).is_err());
        assert!(parse(&v(&["id", "op", "--params", "{"]), &mut no).is_err());
        assert_eq!(parse(&v(&["id", "-"]), &mut no).unwrap_err()["code"], "bad_input");
    }

    // Checks sandbox paths map to host and guest paths, skipping missing ones and merging write access.
    #[test]
    fn test_preopens() {
        let t = TempDir::new();
        std::fs::create_dir(t.path().join("out")).expect("out");
        let m = Manifest::parse(br#"{"zazz":1,"name":"s","kind":"wasm","entry":"w.wasm","sandbox":{"read":["$CWD","/nonexistent-zazz","$TMP","$HOME"],"write":["$CWD/out","$CWD"]}}"#).expect("m");
        let p = preopens(&m, t.path(), Path::new("/tmp"));
        let guests: Vec<(&str, bool)> =
            p.iter().map(|x| (x["guest"].as_str().expect("g"), x["write"].as_bool().expect("w"))).collect();
        assert_eq!(&guests[..2], &[(".", true), ("/tmp", false)]);
        assert!(guests.contains(&("out", true)));
        assert_eq!(mount("/etc", t.path(), t.path()), Some((PathBuf::from("/etc"), "/etc".into())));
        assert_eq!(mount("$TMP/x", t.path(), Path::new("/")), None);
        std::fs::create_dir(t.path().join("x")).expect("x");
        assert_eq!(mount("$TMP/x", t.path(), t.path()), Some((t.path().join("x"), "/tmp/x".into())));
    }

    // Registers a hand-written WASM script directly in the engine's registry and returns its id.
    fn register(e: &crate::engine::Engine, wat_text: &str, call_ms: u64) -> String {
        let reg = e.zazz.registry().expect("reg");
        let wasm = wat::parse_str(wat_text).expect("wat");
        let hex = blake3::hash(&wasm).to_hex().to_string();
        let cwasm = super::super::compile::aot(&crate::testutil::worker_bin(), &wasm).expect("aot");
        std::fs::write(reg.wasm_path(&hex, "cwasm"), cwasm).expect("cwasm");
        let manifest = json!({"zazz": 1, "name": "w", "kind": "wasm", "entry": "w.wasm", "limits": {"call_ms": call_ms}});
        reg.write_meta(&hex, &json!({"id": format!("b3:{hex}"), "manifest": manifest})).expect("meta");
        hex
    }

    // A WASI command that answers hello with ready, then runs its body for calls.
    fn script(body: &str) -> String {
        format!(
            r#"(module
          (import "zazz_host" "request_len" (func $len (result i64)))
          (import "zazz_host" "take" (func $take (param i32 i32) (result i64)))
          (import "zazz_host" "send" (func $send (param i32 i32)))
          (import "wasi_snapshot_preview1" "fd_read" (func $read (param i32 i32 i32 i32) (result i32)))
          (memory (export "memory") 1)
          (data (i32.const 1000) "{{\"t\":\"ready\",\"messages\":[\"json-command\"]}}")
          (data (i32.const 2000) "\00\0c\00\00\10\00\00\00")
          (func (export "_start")
            (drop (call $take (i32.const 0) (i32.wrap_i64 (call $len))))
            (if (i32.eq (i32.load8_u (i32.const 6)) (i32.const 104)) (then (call $send (i32.const 1000) (i32.const 41)) (return)))
            {body}))"#
        )
    }

    // Checks worker-side endings: a timeout and a crash come back as error results and the worker survives.
    #[test]
    fn test_execute_timeout_and_crash() {
        let t = TempDir::new();
        let e = crate::testutil::zazz_engine(&t);
        let msg = || json!({"t": "json-command", "mode": "agent", "input": null});
        let spin = register(&e, &script("(loop (br 0))"), 100);
        let r = execute(&e.zazz, &spin, t.path(), msg(), Streams::Capture { stdin: vec![] }).expect("call");
        assert_eq!(r.result["error"]["code"], "timeout");
        let crash = register(&e, &script("unreachable"), 1000);
        let r = execute(&e.zazz, &crash, t.path(), msg(), Streams::Capture { stdin: vec![] }).expect("call");
        assert_eq!(r.result["error"]["code"], "script_crashed");
        assert_eq!(e.zazz.pool.idle_count(), 2);
        assert_eq!(e.zazz.pool.reap(Duration::from_secs(3600)), 0);
        assert_eq!(e.zazz.pool.reap(Duration::ZERO), 2);
        assert_eq!(
            execute(&e.zazz, "ffff", t.path(), msg(), Streams::Capture { stdin: vec![] }).unwrap_err()
                ["code"],
            "not_found"
        );
    }

    // Checks a call blocked in the host is cancelled when the caller hangs up, and killed by the watchdog otherwise.
    #[test]
    fn test_execute_cancel_and_watchdog() {
        let t = TempDir::new();
        let e = crate::testutil::zazz_engine(&t);
        let block = register(
            &e,
            &script("(drop (call $read (i32.const 0) (i32.const 2000) (i32.const 1) (i32.const 2100)))"),
            100,
        );
        let msg = || json!({"t": "json-command", "mode": "human", "input": null});
        let (keep_in, stdin) = std::os::unix::net::UnixStream::pair().expect("stdin");
        let streams = || {
            let out = std::fs::File::create(t.path().join("out")).expect("out");
            let (conn, hangup) = std::os::unix::net::UnixStream::pair().expect("conn");
            (stdin.try_clone().expect("dup").into(), out, conn, hangup)
        };
        let (sin, out, conn, hangup) = streams();
        drop(hangup);
        let s = Streams::Caller {
            stdio: [sin, out.try_clone().expect("o").into(), out.into()],
            conn: Some(conn.as_raw_fd()),
        };
        assert_eq!(execute(&e.zazz, &block, t.path(), msg(), s).unwrap_err()["code"], "cancelled");
        let (sin, out, conn, _hangup) = streams();
        let s = Streams::Caller {
            stdio: [sin, out.try_clone().expect("o").into(), out.into()],
            conn: Some(conn.as_raw_fd()),
        };
        let started = Instant::now();
        assert_eq!(execute(&e.zazz, &block, t.path(), msg(), s).unwrap_err()["code"], "timeout");
        assert!(started.elapsed() >= WATCHDOG_EXTRA);
        drop(keep_in);
        assert_eq!(e.zazz.pool.idle_count(), 0);
    }

    // Checks agent replies: plain output, output with captured streams, and errors.
    #[test]
    fn test_agent_reply() {
        let ok = |out: &str| Called {
            result: json!({"ok": true, "output": 5}),
            stdout: out.into(),
            stderr: String::new(),
            progress: vec![],
        };
        assert_eq!(agent_reply(ok("")).expect("plain"), json!(5));
        assert_eq!(agent_reply(ok("x")).expect("captured")["stdout"], "x");
        let bad = Called {
            result: json!({"ok": false, "error": {"code": "failed"}}),
            stdout: String::new(),
            stderr: String::new(),
            progress: vec![],
        };
        assert_eq!(agent_reply(bad).unwrap_err()["code"], "failed");
    }
}
