// The WASM backend: one call per instance through the zazz_host imports; stdio is the caller's, wired by the host.
use crate::call::{Call, Http};
use crate::error::{Error, Result};
use crate::handlers::{mode_of, Handlers};
use crate::http::{Request, Response};
use crate::json::Json;
use crate::proto;

#[link(wasm_import_module = "zazz_host")]
extern "C" {
    fn request_len() -> i64;
    fn take(ptr: *mut u8, cap: i32) -> i64;
    fn send(ptr: *const u8, len: i32);
    fn http(ptr: *const u8, len: i32) -> i64;
    fn cancelled() -> i32;
}

// Copies the host's inbox (the call message, or the last http reply) into memory.
fn take_inbox(len: i64) -> Vec<u8> {
    let mut buf = vec![0u8; len.max(0) as usize];
    // SAFETY: buf is a live buffer of the declared capacity.
    let n = unsafe { take(buf.as_mut_ptr(), buf.len() as i32) };
    buf.truncate(n.max(0) as usize);
    buf
}

// Sends one message to the host.
fn send_json(v: &Json) {
    let text = v.to_string();
    // SAFETY: text is live for the call and its length is passed with it.
    unsafe { send(text.as_ptr(), text.len() as i32) };
}

// Network access through the host, which enforces the allowlist.
struct HostHttp;

impl Http for HostHttp {
    // Sends the request through zazz_host.http.
    fn send(&self, req: &Request) -> Result<Response> {
        let wire = req.encode();
        // SAFETY: wire is live for the call and its length is passed with it.
        let n = unsafe { http(wire.as_ptr(), wire.len() as i32) };
        let reply = take_inbox(n.abs());
        if n < 0 {
            return Err(Error::from_json(&Json::parse_bytes(&reply)?));
        }
        Response::decode(&reply)
    }
}

// Serves the single call this instance was created for.
pub fn serve(h: Handlers) {
    // SAFETY: request_len has no preconditions.
    let msg = match Json::parse_bytes(&take_inbox(unsafe { request_len() })) {
        Ok(m) => m,
        Err(e) => return send_json(&proto::result_err(0, &e)),
    };
    let id = msg["id"].i64().unwrap_or(0);
    let stdio: (Box<dyn std::io::Read>, Box<dyn std::io::Write>, Box<dyn std::io::Write>) =
        (Box::new(std::io::stdin()), Box::new(std::io::stdout()), Box::new(std::io::stderr()));
    let mut call = Call::real(id, mode_of(&msg), ".".into(), stdio, Box::new(HostHttp));
    call.progress_sink = Box::new(move |m| send_json(&proto::progress(id, m)));
    // SAFETY: cancelled has no preconditions.
    call.cancel_probe = Box::new(|| unsafe { cancelled() } != 0);
    let result = match proto::kind(&msg) {
        "hello" => proto::ready(&h.messages()),
        _ => match h.dispatch(&mut call, &msg) {
            Ok(out) => proto::result_ok(id, out, call.exit),
            Err(e) => proto::result_err(id, &e),
        },
    };
    let _ = std::io::Write::flush(&mut call.stdout);
    send_json(&result);
}
