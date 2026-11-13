// The native backend: serves protocol frames on fd 3, with the caller's stdio descriptors attached to each call.
use crate::call::{Call, NoHttp};
use crate::error::Error;
use crate::fdpass;
use crate::handlers::{mode_of, Handlers};
use crate::json::Json;
use crate::proto;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

pub const CONTROL_FD: i32 = 3;

// Reads one frame and the descriptors sent with its first bytes; None at end of stream.
fn recv_frame(sock: &mut UnixStream) -> io::Result<Option<(Json, Vec<OwnedFd>)>> {
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

// Turns the three passed descriptors into stdio streams, or empty streams when they are missing.
fn stdio(fds: Vec<OwnedFd>) -> (Box<dyn Read>, Box<dyn Write>, Box<dyn Write>) {
    let mut it = fds.into_iter().map(File::from);
    match (it.next(), it.next(), it.next()) {
        (Some(i), Some(o), Some(e)) => (Box::new(i), Box::new(o), Box::new(e)),
        _ => (Box::new(io::empty()), Box::new(io::sink()), Box::new(io::sink())),
    }
}

// Runs one call message and returns the result frame; panics become "failed" results.
fn run_call(h: &Handlers, sock: &UnixStream, msg: &Json, fds: Vec<OwnedFd>, root: &Path) -> (Json, bool) {
    let id = msg["id"].i64().unwrap_or(0);
    let mut call = Call::real(id, mode_of(msg), root.to_path_buf(), stdio(fds), Box::new(NoHttp));
    if let Ok(mut progress_sock) = sock.try_clone() {
        call.progress_sink =
            Box::new(move |m| drop(proto::write_frame(&mut progress_sock, &proto::progress(id, m))));
    }
    if let Ok(probe) = sock.try_clone() {
        call.cancel_probe = Box::new(move || fdpass::pending(&probe));
    }
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let r = h.dispatch(&mut call, msg);
        let _ = call.stdout.flush();
        let _ = call.stderr.flush();
        (r, call.exit)
    }));
    match outcome {
        Ok((Ok(out), exit)) => (proto::result_ok(id, out, exit), false),
        Ok((Err(e), _)) => (proto::result_err(id, &e), false),
        Err(p) => {
            let why = p
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| p.downcast_ref::<String>().cloned());
            (proto::result_err(id, &Error::failed(&format!("panic: {}", why.unwrap_or_default()))), true)
        }
    }
}

// Serves frames on a socket until it closes, a shutdown arrives or a handler panics.
pub fn serve_on(h: &Handlers, mut sock: UnixStream, root: PathBuf) -> io::Result<()> {
    while let Some((msg, fds)) = recv_frame(&mut sock)? {
        match proto::kind(&msg) {
            "hello" => proto::write_frame(&mut sock, &proto::ready(&h.messages()))?,
            "shutdown" => return Ok(()),
            "cancel" => {}
            _ => {
                let (result, poisoned) = run_call(h, &sock, &msg, fds, &root);
                proto::write_frame(&mut sock, &result)?;
                if poisoned {
                    return Ok(());
                }
            }
        }
    }
    Ok(())
}

// Serves on the control descriptor the launcher passes as fd 3, rooted at the current directory.
pub fn serve(h: Handlers) {
    // SAFETY: the launcher always passes the control socket as fd 3 and nothing else in the process owns it.
    let sock = unsafe { UnixStream::from_raw_fd(CONTROL_FD) };
    let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    if let Err(e) = serve_on(&h, sock, root) {
        eprintln!("zazz: control channel failed: {e}");
        std::process::exit(70);
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use std::os::fd::AsFd;

    // Sends a frame with descriptors attached.
    fn send(sock: &UnixStream, v: &Json, fds: &[i32]) {
        let body = v.to_string();
        let mut buf = (body.len() as u32).to_le_bytes().to_vec();
        buf.extend_from_slice(body.as_bytes());
        fdpass::send_with_fds(sock.as_raw_fd(), &buf, fds).expect("send");
    }

    // Starts a server thread over a socket pair and returns the launcher end.
    fn start(h: fn() -> Handlers) -> (UnixStream, std::thread::JoinHandle<io::Result<()>>) {
        let (launcher, script) = UnixStream::pair().expect("pair");
        let t = std::thread::spawn(move || serve_on(&h(), script, std::env::temp_dir()));
        (launcher, t)
    }

    // Handlers used by the tests: echo, stdin copy, progress, exit codes and a panic.
    fn handlers() -> Handlers {
        Handlers::new().json_command(|call, input| {
            match input.str_or("do", "") {
                "copy" => {
                    let data = call.read_input("-")?;
                    call.stdout().write_all(&data)?;
                }
                "progress" => call.progress("half"),
                "exit" => call.set_exit(4),
                "panic" => panic!("boom"),
                "panic-string" => std::panic::panic_any(String::from("owned")),
                "fail" => return Err(Error::bad_input("nope")),
                "cancel" => return Ok(Json::Bool(call.cancelled())),
                _ => {}
            }
            Ok(input)
        })
    }

    // Checks hello, a call with forwarded stdio, progress, errors and shutdown; like zazzled, it keeps its copies of the passed sockets open until the result, since macOS can discard unix sockets in flight that nothing else holds.
    #[test]
    fn test_serve_on_calls() {
        let (mut l, t) = start(handlers);
        send(&l, &proto::hello(), &[]);
        assert_eq!(
            proto::read_frame(&mut l).expect("r").expect("ready")["messages"][0].str(),
            Some("json-command")
        );
        let (mut in_w, in_r) = UnixStream::pair().expect("in");
        let (mut out_r, out_w) = UnixStream::pair().expect("out");
        in_w.write_all(b"piped").expect("stdin");
        drop(in_w);
        let err_w = out_w.try_clone().expect("err");
        let fds = [in_r.as_fd().as_raw_fd(), out_w.as_raw_fd(), err_w.as_raw_fd()];
        send(&l, &crate::json!({"t": "json-command", "id": 1, "input": {"do": "copy"}}), &fds);
        let r = proto::read_frame(&mut l).expect("r").expect("result");
        assert_eq!((r["id"].i64(), r["ok"].bool()), (Some(1), Some(true)));
        drop((in_r, out_w, err_w));
        let mut s = String::new();
        out_r.read_to_string(&mut s).expect("stdout");
        assert_eq!(s, "piped");
        send(&l, &crate::json!({"t": "json-command", "id": 2, "input": {"do": "progress"}}), &[]);
        assert_eq!(proto::kind(&proto::read_frame(&mut l).expect("r").expect("p")), "progress");
        assert_eq!(proto::read_frame(&mut l).expect("r").expect("result")["id"].i64(), Some(2));
        send(
            &l,
            &crate::json!({"t": "json-command", "id": 3, "mode": "human", "input": {"do": "exit"}}),
            &[],
        );
        assert_eq!(proto::read_frame(&mut l).expect("r").expect("result")["exit"].i64(), Some(4));
        send(&l, &crate::json!({"t": "json-command", "id": 4, "input": {"do": "fail"}}), &[]);
        assert_eq!(
            proto::read_frame(&mut l).expect("r").expect("result")["error"]["code"].str(),
            Some("bad_input")
        );
        send(&l, &crate::json!({"t": "cancel", "id": 9}), &[]);
        send(&l, &crate::json!({"t": "shutdown"}), &[]);
        t.join().expect("join").expect("served");
    }

    // Checks a panic is reported as failed and ends the worker.
    #[test]
    fn test_serve_on_panics() {
        for (what, text) in [("panic", "panic: boom"), ("panic-string", "panic: owned")] {
            let (mut l, t) = start(handlers);
            send(&l, &crate::json!({"t": "json-command", "id": 1, "input": {"do": what}}), &[]);
            let r = proto::read_frame(&mut l).expect("r").expect("result");
            assert_eq!(r["error"]["message"].str(), Some(text));
            t.join().expect("join").expect("ended");
        }
    }

    // Checks the cancel probe sees a frame waiting on the control socket.
    #[test]
    fn test_cancel_probe() {
        let (mut l, t) = start(handlers);
        let mut pending = Vec::new();
        proto::write_frame(
            &mut pending,
            &crate::json!({"t": "json-command", "id": 1, "input": {"do": "cancel"}}),
        )
        .expect("w");
        proto::write_frame(&mut pending, &crate::json!({"t": "cancel", "id": 1})).expect("w");
        l.write_all(&pending).expect("send both");
        assert_eq!(proto::read_frame(&mut l).expect("r").expect("result")["output"], Json::Bool(true));
        drop(l);
        t.join().expect("join").expect("eof ends");
    }

    // Checks a truncated frame is an error rather than a hang.
    #[test]
    fn test_serve_on_truncated() {
        let (mut l, t) = start(handlers);
        l.write_all(&[10, 0]).expect("partial");
        drop(l);
        assert!(t.join().expect("join").is_err());
    }
}
