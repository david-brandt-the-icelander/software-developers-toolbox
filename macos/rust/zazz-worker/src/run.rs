// Runs one fresh instance of a module: WASI preview 1 plus the zazz_host imports, with memory and time limits.
use crate::engine::{self, HOST_MODULE};
use crate::net::{self, Backend};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use wasmtime::{Caller, Extern, Linker, Memory, Module, Store, StoreLimits, StoreLimitsBuilder, Trap};
use wasmtime_wasi::cli::{StdinStream, StdoutStream};
use wasmtime_wasi::p1::WasiP1Ctx;
use wasmtime_wasi::{FsPerms, I32Exit, WasiCtxBuilder};
use zazz::http::{NetPolicy, Request};
use zazz::{proto, Json};

// A host directory the guest sees under a guest path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preopen {
    pub host: PathBuf,
    pub guest: String,
    pub write: bool,
}

// What every instance of one worker gets: arguments, environment, directories, network and limits.
#[derive(Debug, Clone)]
pub struct Setup {
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub preopens: Vec<Preopen>,
    pub net: NetPolicy,
    pub memory_mb: u64,
    pub call_ms: u64,
}

// The guest's standard streams.
pub struct Io {
    pub stdin: Box<dyn StdinStream + Send + Sync>,
    pub stdout: Box<dyn StdoutStream + Send + Sync>,
    pub stderr: Box<dyn StdoutStream + Send + Sync>,
}

// Callbacks into the worker while an instance runs.
pub struct Hooks {
    pub cancelled: Box<dyn Fn() -> bool + Send>,
    pub progress: Box<dyn FnMut(&Json) + Send>,
    pub backend: Arc<dyn Backend + Send + Sync>,
}

// How an instance ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ending {
    Exit(i32),
    Timeout,
    OutOfMemory,
    Crash(String),
}

// Everything an instance sent and how it ended.
#[derive(Debug)]
pub struct Outcome {
    pub sent: Vec<Json>,
    pub ending: Ending,
}

// The data held by each store.
pub struct Host {
    wasi: WasiP1Ctx,
    limits: StoreLimits,
    inbox: Vec<u8>,
    sent: Vec<Json>,
    net: NetPolicy,
    deadline: Instant,
    hooks: Hooks,
}

// Returns the guest's exported memory.
fn memory(caller: &mut Caller<'_, Host>) -> wasmtime::Result<Memory> {
    caller
        .get_export("memory")
        .and_then(Extern::into_memory)
        .ok_or_else(|| wasmtime::format_err!("the module exports no memory"))
}

// Reads guest bytes at a pointer and length.
fn read_guest(caller: &mut Caller<'_, Host>, ptr: i32, len: i32) -> wasmtime::Result<Vec<u8>> {
    let mem = memory(caller)?;
    let mut buf = vec![0u8; usize::try_from(len).map_err(|_| wasmtime::format_err!("negative length"))?];
    mem.read(&caller, ptr as u32 as usize, &mut buf)?;
    Ok(buf)
}

// Copies the inbox into guest memory: the call message at first, later the last HTTP reply.
fn take(mut caller: Caller<'_, Host>, ptr: i32, cap: i32) -> wasmtime::Result<i64> {
    let mem = memory(&mut caller)?;
    let n = caller.data().inbox.len().min(cap.max(0) as usize);
    let bytes = caller.data().inbox[..n].to_vec();
    mem.write(&mut caller, ptr as u32 as usize, &bytes)?;
    Ok(n as i64)
}

// Receives a message from the guest: progress goes out at once, everything else is kept.
fn send(mut caller: Caller<'_, Host>, ptr: i32, len: i32) -> wasmtime::Result<()> {
    let bytes = read_guest(&mut caller, ptr, len)?;
    let msg = Json::parse_bytes(&bytes).map_err(|e| wasmtime::format_err!("send: {}", e.message))?;
    let host = caller.data_mut();
    if proto::kind(&msg) == "progress" {
        (host.hooks.progress)(&msg);
    } else {
        host.sent.push(msg);
    }
    Ok(())
}

// Performs an HTTP request for the guest; returns the reply length, or minus the error length.
fn http(mut caller: Caller<'_, Host>, ptr: i32, len: i32) -> wasmtime::Result<i64> {
    let bytes = read_guest(&mut caller, ptr, len)?;
    let host = caller.data_mut();
    let timeout = host.deadline.saturating_duration_since(Instant::now()).max(Duration::from_millis(1));
    let result =
        Request::decode(&bytes).and_then(|req| net::send(&host.net, &*host.hooks.backend, &req, timeout));
    let (inbox, sign) = match result {
        Ok(resp) => (resp.encode(), 1),
        Err(e) => (e.to_json().to_string().into_bytes(), -1),
    };
    host.inbox = inbox;
    Ok(sign * host.inbox.len() as i64)
}

// Builds the linker: WASI preview 1 plus the zazz_host functions.
pub fn linker() -> Linker<Host> {
    let mut l: Linker<Host> = Linker::new(engine::engine());
    wasmtime_wasi::p1::add_to_linker_sync(&mut l, |h| &mut h.wasi).expect("WASI links into a fresh linker");
    let ok = "host functions have unique names";
    l.func_wrap(HOST_MODULE, "request_len", |c: Caller<'_, Host>| c.data().inbox.len() as i64).expect(ok);
    l.func_wrap(HOST_MODULE, "take", take).expect(ok);
    l.func_wrap(HOST_MODULE, "send", send).expect(ok);
    l.func_wrap(HOST_MODULE, "http", http).expect(ok);
    l.func_wrap(HOST_MODULE, "cancelled", |c: Caller<'_, Host>| (c.data().hooks.cancelled)() as i32)
        .expect(ok);
    l
}

// Builds the WASI context for one instance.
fn wasi(setup: &Setup, io: Io) -> Result<WasiP1Ctx, String> {
    let mut b = WasiCtxBuilder::new();
    b.args(&setup.args);
    for (k, v) in &setup.env {
        b.env(k, v);
    }
    b.stdin(io.stdin).stdout(io.stdout).stderr(io.stderr);
    for p in &setup.preopens {
        let perms = if p.write { FsPerms::ReadWrite } else { FsPerms::ReadOnly };
        b.preopened_dir(&p.host, &p.guest, perms).map_err(|e| format!("{}: {e:#}", p.host.display()))?;
    }
    Ok(b.build_p1())
}

// Classifies how _start ended.
fn ending(r: wasmtime::Result<()>) -> Ending {
    let Err(e) = r else { return Ending::Exit(0) };
    if let Some(exit) = e.downcast_ref::<I32Exit>() {
        return Ending::Exit(exit.0);
    }
    match e.downcast_ref::<Trap>() {
        Some(Trap::Interrupt) => Ending::Timeout,
        _ if format!("{e:#}").contains("growing memory") => Ending::OutOfMemory,
        Some(t) => Ending::Crash(t.to_string()),
        None => Ending::Crash(e.root_cause().to_string()),
    }
}

// Instantiates the module with a message in the inbox and runs _start to the end.
pub fn run(
    linker: &Linker<Host>,
    module: &Module,
    setup: &Setup,
    message: &[u8],
    io: Io,
    hooks: Hooks,
) -> Result<Outcome, String> {
    let limits = StoreLimitsBuilder::new()
        .memory_size((setup.memory_mb as usize) << 20)
        .trap_on_grow_failure(true)
        .instances(1)
        .build();
    let host = Host {
        wasi: wasi(setup, io)?,
        limits,
        inbox: message.to_vec(),
        sent: Vec::new(),
        net: setup.net.clone(),
        deadline: Instant::now() + Duration::from_millis(setup.call_ms),
        hooks,
    };
    let mut store = Store::new(engine::engine(), host);
    store.limiter(|h| &mut h.limits);
    store.set_epoch_deadline(engine::ticks(setup.call_ms));
    engine::start_ticker();
    let instance = match linker.instantiate(&mut store, module) {
        Ok(i) => i,
        Err(e) => return Ok(Outcome { sent: Vec::new(), ending: ending(Err(e)) }),
    };
    let start =
        instance.get_typed_func::<(), ()>(&mut store, "_start").map_err(|e| format!("_start: {e:#}"))?;
    let end = ending(start.call(&mut store, ()));
    Ok(Outcome { sent: std::mem::take(&mut store.data_mut().sent), ending: end })
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::net::Replay;
    use std::sync::Mutex;
    use wasmtime_wasi::p2::pipe::{MemoryInputPipe, MemoryOutputPipe};
    use zazz::http::Response;

    // Compiles WAT into a loaded module.
    fn module(text: &str) -> Module {
        let wasm = wat::parse_str(text).expect("wat");
        engine::load(&engine::compile(&wasm).expect("compile")).expect("load")
    }

    // A setup with small limits and no directories.
    fn setup(call_ms: u64) -> Setup {
        Setup {
            args: vec!["t".into()],
            env: vec![],
            preopens: vec![],
            net: NetPolicy::All,
            memory_mb: 2,
            call_ms,
        }
    }

    // Memory-backed stdio, returning the stdout pipe for inspection.
    fn io() -> (Io, MemoryOutputPipe) {
        let out = MemoryOutputPipe::new(1 << 20);
        let io = Io {
            stdin: Box::new(MemoryInputPipe::new("")),
            stdout: Box::new(out.clone()),
            stderr: Box::new(MemoryOutputPipe::new(1 << 20)),
        };
        (io, out)
    }

    // Hooks that record progress and answer HTTP from a fixed list.
    fn hooks(progress: Arc<Mutex<Vec<Json>>>) -> Hooks {
        let replay = Replay(vec![("GET".into(), "https://a.com/".into(), Response::text(200, "hi"))]);
        Hooks {
            cancelled: Box::new(|| true),
            progress: Box::new(move |m| progress.lock().expect("lock").push(m.clone())),
            backend: Arc::new(replay),
        }
    }

    // A guest that echoes the inbox through send, reports progress, asks cancelled and makes an HTTP call.
    const ECHO: &str = r#"(module
      (import "zazz_host" "request_len" (func $len (result i64)))
      (import "zazz_host" "take" (func $take (param i32 i32) (result i64)))
      (import "zazz_host" "send" (func $send (param i32 i32)))
      (import "zazz_host" "http" (func $http (param i32 i32) (result i64)))
      (import "zazz_host" "cancelled" (func $cancelled (result i32)))
      (memory (export "memory") 1)
      (data (i32.const 2048) "{\"t\":\"progress\",\"id\":1,\"msg\":\"m\"}")
      (global $req_len (mut i32) (i32.const 0))
      (func (export "_start")
        (local $n i32)
        (local.set $n (i32.wrap_i64 (call $len)))
        (drop (call $take (i32.const 0) (local.get $n)))
        (call $send (i32.const 2048) (i32.const 33))
        (call $send (i32.const 0) (local.get $n))
        (if (i32.eqz (call $cancelled)) (then unreachable))
        (drop (call $http (i32.const 4096) (i32.const 3)))))"#;

    // Checks the host functions: inbox, send, progress, cancelled and a refused HTTP frame.
    #[test]
    fn test_run_echo() {
        let notes = Arc::new(Mutex::new(Vec::new()));
        let (io, _) = io();
        let msg = br#"{"t":"result","id":1,"ok":true,"output":5}"#;
        let o = run(&linker(), &module(ECHO), &setup(5000), msg, io, hooks(notes.clone())).expect("run");
        assert_eq!(o.ending, Ending::Exit(0));
        assert_eq!(o.sent, vec![Json::parse_bytes(msg).expect("json")]);
        assert_eq!(notes.lock().expect("lock").len(), 1);
    }

    // Checks traps, timeouts, explicit exits and memory limits are classified.
    #[test]
    fn test_run_endings() {
        let r = |text: &str, ms: u64| {
            let (io, _) = io();
            run(&linker(), &module(text), &setup(ms), b"", io, hooks(Arc::default())).expect("run").ending
        };
        let wasi_exit = r#"(module (import "wasi_snapshot_preview1" "proc_exit" (func $exit (param i32))) (memory (export "memory") 1) (func (export "_start") (call $exit (i32.const 7))))"#;
        assert_eq!(r(wasi_exit, 1000), Ending::Exit(7));
        let crash = r#"(module (memory (export "memory") 1) (func (export "_start") unreachable))"#;
        assert!(matches!(r(crash, 1000), Ending::Crash(_)));
        let spin = r#"(module (memory (export "memory") 1) (func (export "_start") (loop (br 0))))"#;
        assert_eq!(r(spin, 50), Ending::Timeout);
        let grow = r#"(module (memory (export "memory") 1) (func (export "_start") (drop (memory.grow (i32.const 100)))))"#;
        assert_eq!(r(grow, 1000), Ending::OutOfMemory);
        let too_big = r#"(module (memory (export "memory") 100) (func (export "_start")))"#;
        assert_eq!(r(too_big, 1000), Ending::OutOfMemory);
    }

    // Checks a guest without memory traps on host calls, and bad preopens are reported.
    #[test]
    fn test_run_errors() {
        let no_mem = r#"(module (import "zazz_host" "send" (func $send (param i32 i32))) (func (export "_start") (call $send (i32.const 0) (i32.const 1))))"#;
        let (io1, _) = io();
        assert!(matches!(
            run(&linker(), &module(no_mem), &setup(1000), b"", io1, hooks(Arc::default()))
                .expect("run")
                .ending,
            Ending::Crash(_)
        ));
        let mut s = setup(1000);
        s.preopens.push(Preopen { host: "/nonexistent-zazz".into(), guest: ".".into(), write: false });
        let (io2, _) = io();
        assert!(run(&linker(), &module(ECHO), &s, b"", io2, hooks(Arc::default())).is_err());
        let bad_send = r#"(module (import "zazz_host" "send" (func $send (param i32 i32))) (memory (export "memory") 1) (func (export "_start") (call $send (i32.const 0) (i32.const 3))))"#;
        let (io3, _) = io();
        assert!(matches!(
            run(&linker(), &module(bad_send), &setup(1000), b"", io3, hooks(Arc::default()))
                .expect("run")
                .ending,
            Ending::Crash(_)
        ));
        let neg = r#"(module (import "zazz_host" "send" (func $send (param i32 i32))) (memory (export "memory") 1) (func (export "_start") (call $send (i32.const 0) (i32.const -1))))"#;
        let (io4, _) = io();
        assert!(matches!(
            run(&linker(), &module(neg), &setup(1000), b"", io4, hooks(Arc::default())).expect("run").ending,
            Ending::Crash(_)
        ));
    }

    // Checks a guest can write to stdout and read a preopened directory through WASI.
    #[test]
    fn test_run_wasi_stdout() {
        let hello = r#"(module (import "wasi_snapshot_preview1" "fd_write" (func $w (param i32 i32 i32 i32) (result i32)))
          (memory (export "memory") 1) (data (i32.const 8) "\10\00\00\00\02\00\00\00") (data (i32.const 16) "hi")
          (func (export "_start") (drop (call $w (i32.const 1) (i32.const 8) (i32.const 1) (i32.const 32)))))"#;
        let (io, out) = io();
        let mut s = setup(1000);
        s.env.push(("A".into(), "B".into()));
        s.preopens.push(Preopen { host: std::env::temp_dir(), guest: ".".into(), write: true });
        assert_eq!(
            run(&linker(), &module(hello), &s, b"", io, hooks(Arc::default())).expect("run").ending,
            Ending::Exit(0)
        );
        assert_eq!(&out.contents()[..], b"hi");
    }
}
