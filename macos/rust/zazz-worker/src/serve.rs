// Serve mode: reads the worker's configuration, then runs one fresh instance per call frame on the control socket.
use crate::net::{Backend, Https};
use crate::run::{self, Ending, Hooks, Io, Outcome, Preopen, Setup};
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use wasmtime::{Linker, Module};
use wasmtime_wasi::cli::{InputFile, OutputFile};
use wasmtime_wasi::p2::pipe::{ClosedInputStream, SinkOutputStream};
use zazz::http::NetPolicy;
use zazz::{fdpass, proto, Error, Json};

// Reads one frame and the descriptors sent with it; None at end of stream.
fn recv(sock: &mut UnixStream) -> io::Result<Option<(Json, Vec<OwnedFd>)>> {
    use std::io::Read;
    let mut header = [0u8; 4];
    let (n, fds) = fdpass::recv_with_fds(sock.as_raw_fd(), &mut header)?;
    if n == 0 {
        return Ok(None);
    }
    sock.read_exact(&mut header[n..])?;
    let mut body = vec![0u8; proto::frame_len(header)?];
    sock.read_exact(&mut body)?;
    Ok(Some((proto::parse_body(&body)?, fds)))
}

// Reads the network policy from a hello frame: "none", "all" or {"hosts": [...]}.
pub fn net_policy(v: &Json) -> NetPolicy {
    match v {
        Json::Str(s) if s == "all" => NetPolicy::All,
        Json::Obj(_) => NetPolicy::Hosts(
            v["hosts"].arr().unwrap_or(&[]).iter().filter_map(|h| h.str().map(str::to_string)).collect(),
        ),
        _ => NetPolicy::None,
    }
}

// Builds the per-worker setup from the hello frame zazzled sends first.
pub fn setup_from(hello: &Json) -> Setup {
    let preopens = hello["preopens"]
        .arr()
        .unwrap_or(&[])
        .iter()
        .filter_map(|p| {
            Some(Preopen {
                host: p["host"].str()?.into(),
                guest: p["guest"].str()?.into(),
                write: p["write"].bool().unwrap_or(false),
            })
        })
        .collect();
    let env = hello["env"]
        .obj()
        .unwrap_or(&[])
        .iter()
        .filter_map(|(k, v)| Some((k.clone(), v.str()?.to_string())))
        .collect();
    let args = hello.strs("args").unwrap_or_else(|_| vec!["script".into()]);
    Setup {
        args,
        env,
        preopens,
        net: net_policy(&hello["network"]),
        memory_mb: hello["memory_mb"].i64().unwrap_or(256).max(1) as u64,
        call_ms: hello["call_ms"].i64().unwrap_or(30_000).max(1) as u64,
    }
}

// Wraps the caller's three descriptors as the guest's stdio, or closed streams when they are missing.
fn stdio(fds: Vec<OwnedFd>) -> Io {
    let mut it = fds.into_iter().map(File::from);
    match (it.next(), it.next(), it.next()) {
        (Some(i), Some(o), Some(e)) => Io {
            stdin: Box::new(InputFile::new(i)),
            stdout: Box::new(OutputFile::new(o)),
            stderr: Box::new(OutputFile::new(e)),
        },
        _ => Io {
            stdin: Box::new(ClosedInputStream),
            stdout: Box::new(SinkOutputStream),
            stderr: Box::new(SinkOutputStream),
        },
    }
}

// Turns an instance's outcome into the result frame for the launcher.
pub fn result_of(id: i64, o: Outcome) -> Json {
    if let Some(r) = o.sent.into_iter().rev().find(|m| matches!(proto::kind(m), "result" | "ready")) {
        return r;
    }
    let err = match o.ending {
        Ending::Exit(code) => {
            Error::new("failed", &format!("the script exited with status {code} without a result"))
        }
        Ending::Timeout => Error::new("timeout", "the call ran past its time limit"),
        Ending::OutOfMemory => Error::new("out_of_memory", "the call ran past its memory limit"),
        Ending::Crash(why) => Error::new("script_crashed", &why),
    };
    proto::result_err(id, &err)
}

// Runs one message in a fresh instance and returns the frame to send back.
fn call(
    linker: &Linker<run::Host>,
    module: &Module,
    setup: &Setup,
    sock: &UnixStream,
    msg: &Json,
    fds: Vec<OwnedFd>,
    backend: &Arc<dyn Backend + Send + Sync>,
) -> Json {
    let id = msg["id"].i64().unwrap_or(0);
    let (probe, progress) = (sock.try_clone().ok(), sock.try_clone().ok());
    let hooks = Hooks {
        cancelled: Box::new(move || probe.as_ref().is_some_and(fdpass::pending)),
        progress: Box::new(move |m| {
            if let Some(mut s) = progress.as_ref() {
                let _ = proto::write_frame(&mut s, m);
            }
        }),
        backend: backend.clone(),
    };
    match run::run(linker, module, setup, msg.to_string().as_bytes(), stdio(fds), hooks) {
        Ok(o) => result_of(id, o),
        Err(e) => proto::result_err(id, &Error::failed(&e)),
    }
}

// Serves a control socket: a hello frame with the configuration, then calls until shutdown or end of stream.
pub fn serve_on(
    module: &Module,
    mut sock: UnixStream,
    backend: Arc<dyn Backend + Send + Sync>,
) -> io::Result<()> {
    let linker = run::linker();
    let mut setup: Option<Setup> = None;
    while let Some((msg, fds)) = recv(&mut sock)? {
        let reply = match (proto::kind(&msg), &setup) {
            ("shutdown", _) => return Ok(()),
            ("cancel", _) => continue,
            ("hello", _) => {
                let s = setup_from(&msg);
                let r = call(
                    &linker,
                    module,
                    &s,
                    &sock,
                    &zazz::json!({"t": "hello", "id": 0}),
                    Vec::new(),
                    &backend,
                );
                setup = Some(s);
                r
            }
            (_, Some(s)) => call(&linker, module, s, &sock, &msg, fds, &backend),
            (_, None) => {
                proto::result_err(msg["id"].i64().unwrap_or(0), &Error::new("protocol", "send hello first"))
            }
        };
        proto::write_frame(&mut sock, &reply)?;
    }
    Ok(())
}

// Serves the control socket on fd 3 with the precompiled module on fd 4.
pub fn main() -> Result<(), String> {
    use std::io::Read;
    use std::os::fd::FromRawFd;
    // SAFETY: zazzled passes the precompiled module as fd 4 and the control socket as fd 3; nothing else owns them.
    let (mut module_file, sock) = unsafe { (File::from_raw_fd(4), UnixStream::from_raw_fd(3)) };
    let mut bytes = Vec::new();
    module_file.read_to_end(&mut bytes).map_err(|e| format!("reading the module from fd 4: {e}"))?;
    drop(module_file);
    let module = crate::engine::load(&bytes)?;
    serve_on(&module, sock, Arc::new(Https)).map_err(|e| format!("control channel: {e}"))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::net::Replay;
    use std::io::{Read, Write};
    use zazz::json;

    // A guest that answers with the inbox message as its result, after sending progress.
    const ANSWER: &str = r#"(module
      (import "zazz_host" "request_len" (func $len (result i64)))
      (import "zazz_host" "take" (func $take (param i32 i32) (result i64)))
      (import "zazz_host" "send" (func $send (param i32 i32)))
      (import "wasi_snapshot_preview1" "fd_write" (func $w (param i32 i32 i32 i32) (result i32)))
      (memory (export "memory") 1)
      (data (i32.const 60000) "{\"t\":\"progress\",\"id\":1,\"msg\":\"m\"}")
      (data (i32.const 61000) "\10\ef\00\00\03\00\00\00") (data (i32.const 61200) "out")
      (func (export "_start")
        (local $n i32)
        (local.set $n (i32.wrap_i64 (call $len)))
        (drop (call $take (i32.const 0) (local.get $n)))
        (drop (call $w (i32.const 1) (i32.const 61000) (i32.const 1) (i32.const 61100)))
        (call $send (i32.const 60000) (i32.const 33))
        (call $send (i32.const 0) (local.get $n))))"#;

    // Loads a WAT guest.
    fn module(text: &str) -> Module {
        crate::engine::load(&crate::engine::compile(&wat::parse_str(text).expect("wat")).expect("compile"))
            .expect("load")
    }

    // Sends a frame with descriptors attached.
    fn send(sock: &UnixStream, v: &Json, fds: &[i32]) {
        let body = v.to_string();
        let mut buf = (body.len() as u32).to_le_bytes().to_vec();
        buf.extend_from_slice(body.as_bytes());
        fdpass::send_with_fds(sock.as_raw_fd(), &buf, fds).expect("send");
    }

    // Starts a worker thread on a socket pair.
    fn start(text: &'static str) -> (UnixStream, std::thread::JoinHandle<io::Result<()>>) {
        let (l, w) = UnixStream::pair().expect("pair");
        let t = std::thread::spawn(move || serve_on(&module(text), w, Arc::new(Replay(vec![]))));
        (l, t)
    }

    // Checks hello, a call with forwarded stdout, progress, the protocol error and shutdown.
    #[test]
    fn test_serve_on() {
        let (mut l, t) = start(ANSWER);
        send(&l, &json!({"t": "json-command", "id": 9}), &[]);
        assert_eq!(
            proto::read_frame(&mut l).expect("r").expect("f")["error"]["code"].str(),
            Some("protocol")
        );
        let hello = json!({"t": "hello", "proto": 1, "call_ms": 2000, "memory_mb": 16, "network": "none", "args": ["s"], "env": {"K": "V"}, "preopens": []});
        send(&l, &hello, &[]);
        assert_eq!(proto::kind(&proto::read_frame(&mut l).expect("r").expect("progress")), "progress");
        assert_eq!(
            proto::read_frame(&mut l).expect("r").expect("not ready")["error"]["code"].str(),
            Some("failed")
        );
        let (mut out_r, out_w) = UnixStream::pair().expect("out");
        let (in_r, in_w) = UnixStream::pair().expect("in");
        drop(in_w);
        let err_w = out_w.try_clone().expect("err");
        let call = json!({"t": "result", "id": 1, "ok": true, "output": 3});
        send(&l, &call, &[in_r.as_raw_fd(), out_w.as_raw_fd(), err_w.as_raw_fd()]);
        drop((in_r, out_w, err_w));
        assert_eq!(proto::kind(&proto::read_frame(&mut l).expect("r").expect("progress")), "progress");
        assert_eq!(proto::read_frame(&mut l).expect("r").expect("result")["output"], Json::Int(3));
        let mut s = String::new();
        out_r.read_to_string(&mut s).expect("stdout");
        assert_eq!(s, "out");
        send(&l, &json!({"t": "cancel"}), &[]);
        send(&l, &json!({"t": "shutdown"}), &[]);
        t.join().expect("join").expect("served");
    }

    // Checks a guest that sends nothing gets a synthesised error, and end of stream ends the loop.
    #[test]
    fn test_serve_on_without_result() {
        let (mut l, t) =
            start(r#"(module (memory (export "memory") 1) (func (export "_start") unreachable))"#);
        send(&l, &json!({"t": "hello"}), &[]);
        assert_eq!(
            proto::read_frame(&mut l).expect("r").expect("f")["error"]["code"].str(),
            Some("script_crashed")
        );
        l.write_all(b"").expect("noop");
        drop(l);
        t.join().expect("join").expect("eof");
    }

    // Checks every ending maps to an error code, and kept results win.
    #[test]
    fn test_result_of() {
        let code = |e: Ending| {
            result_of(1, Outcome { sent: vec![], ending: e })["error"]["code"].str().map(str::to_string)
        };
        assert_eq!(code(Ending::Exit(2)).as_deref(), Some("failed"));
        assert_eq!(code(Ending::Timeout).as_deref(), Some("timeout"));
        assert_eq!(code(Ending::OutOfMemory).as_deref(), Some("out_of_memory"));
        assert_eq!(code(Ending::Crash("x".into())).as_deref(), Some("script_crashed"));
        let kept =
            result_of(1, Outcome { sent: vec![json!({"t": "result", "id": 1})], ending: Ending::Timeout });
        assert_eq!(kept["t"].str(), Some("result"));
    }

    // Checks the setup and network policy are read from hello.
    #[test]
    fn test_setup_from() {
        let s = setup_from(
            &json!({"network": {"hosts": ["a.com"]}, "preopens": [{"host": "/tmp", "guest": ".", "write": true}, {"bad": 1}]}),
        );
        assert_eq!(
            (s.net, s.preopens.len(), s.args, s.memory_mb, s.call_ms),
            (NetPolicy::Hosts(vec!["a.com".into()]), 1, vec!["script".to_string()], 256, 30_000)
        );
        assert_eq!(net_policy(&json!("all")), NetPolicy::All);
        assert_eq!(net_policy(&json!(null)), NetPolicy::None);
    }
}
