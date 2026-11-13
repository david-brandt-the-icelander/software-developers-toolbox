// MCP proxy over Streamable HTTP: a local listener that forwards to the real endpoint and cleans its replies.
use super::{Filter, Pending};
use std::io::{self, BufRead, BufReader, BufWriter, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::time::Duration;
use ureq::http::{self, HeaderMap, StatusCode};
use ureq::tls::{RootCerts, TlsConfig};

const MAX_BODY: usize = 64 << 20;
const MAX_HEAD: usize = 64 << 10;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
// How long a kept-alive connection may wait for its next request before the proxy closes it.
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
const HOP_BY_HOP: &[&str] = &[
    "accept-encoding",
    "connection",
    "content-length",
    "host",
    "keep-alive",
    "proxy-authorization",
    "proxy-connection",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

// One client request: method, headers in arrival order and body.
#[derive(Debug, PartialEq)]
struct Req {
    method: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    keep: bool,
}

// Writes a body as HTTP/1.1 chunks, so a connection can stay open after a reply of unknown length.
struct Chunked<'a, W: Write>(&'a mut W);

impl<W: Write> Write for Chunked<'_, W> {
    // Writes one chunk; an empty write would end the body, so it is skipped.
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if !buf.is_empty() {
            write!(self.0, "{:x}\r\n", buf.len())?;
            self.0.write_all(buf)?;
            self.0.write_all(b"\r\n")?;
        }
        Ok(buf.len())
    }

    // Flushes the connection.
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

// A failure answered to the client with an HTTP status and a one-line message.
#[derive(Debug, PartialEq)]
struct Fail(u16, String);

// True for headers that describe one connection and must not be forwarded.
fn hop(name: &str) -> bool {
    HOP_BY_HOP.iter().any(|h| h.eq_ignore_ascii_case(name))
}

// Returns a request header's value by case-insensitive name.
fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
}

// Reads one CRLF-terminated head line within the remaining head budget.
fn head_line(r: &mut impl BufRead, used: &mut usize) -> Result<String, Fail> {
    let mut line = String::new();
    let n = r
        .by_ref()
        .take((MAX_HEAD - *used) as u64)
        .read_line(&mut line)
        .map_err(|_| Fail(400, "request head is not valid UTF-8".into()))?;
    *used += n;
    if !line.ends_with('\n') {
        return Err(if *used >= MAX_HEAD {
            Fail(431, "request head too large".into())
        } else {
            Fail(400, "request ended early".into())
        });
    }
    Ok(line.trim_end_matches(['\r', '\n']).to_string())
}

// Reads one request head and its body, which must be sized by Content-Length.
fn read_request(r: &mut impl BufRead) -> Result<Req, Fail> {
    let mut used = 0;
    let first = head_line(r, &mut used)?;
    let method = first.split(' ').next().unwrap_or_default().to_string();
    let http10 = first.ends_with("HTTP/1.0");
    let mut headers = Vec::new();
    loop {
        let line = head_line(r, &mut used)?;
        if line.is_empty() {
            break;
        }
        let (k, v) = line.split_once(':').ok_or_else(|| Fail(400, format!("bad header line '{line}'")))?;
        headers.push((k.trim().to_string(), v.trim().to_string()));
    }
    if header(&headers, "transfer-encoding").is_some() {
        return Err(Fail(411, "send the body with Content-Length".into()));
    }
    let len = match header(&headers, "content-length") {
        None => 0,
        Some(v) => v.parse::<usize>().map_err(|_| Fail(400, format!("bad Content-Length '{v}'")))?,
    };
    if len > MAX_BODY {
        return Err(Fail(413, format!("body over {} MiB", MAX_BODY >> 20)));
    }
    let mut body = vec![0; len];
    r.read_exact(&mut body).map_err(|_| Fail(400, "request body ended early".into()))?;
    let connection = header(&headers, "connection").unwrap_or("").to_ascii_lowercase();
    let keep = if http10 { connection.contains("keep-alive") } else { !connection.contains("close") };
    Ok(Req { method, headers, body, keep })
}

// Returns the host of an Origin value such as `http://localhost:3000` or `http://[::1]`.
fn origin_host(origin: &str) -> &str {
    let rest = origin.split_once("://").map_or(origin, |(_, r)| r);
    let rest = rest.split('/').next().unwrap_or_default();
    match rest.find(']') {
        Some(i) if rest.starts_with('[') => &rest[..=i],
        _ => rest.split(':').next().unwrap_or_default(),
    }
}

// True unless a web page on another host sent the request (the DNS-rebinding check the MCP spec asks for).
fn origin_ok(headers: &[(String, String)]) -> bool {
    header(headers, "origin").is_none_or(|o| matches!(origin_host(o), "localhost" | "127.0.0.1" | "[::1]"))
}

// Builds the upstream client: statuses and redirects pass through, bodies come uncompressed, connects time out.
fn agent(env_proxy: bool) -> ureq::Agent {
    let mut cfg = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .max_redirects(0)
        .max_redirects_will_error(false)
        .accept_encoding("identity")
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .tls_config(TlsConfig::builder().root_certs(RootCerts::PlatformVerifier).build());
    if !env_proxy {
        cfg = cfg.proxy(None);
    }
    cfg.build().new_agent()
}

// Sends the request to the upstream endpoint with the client's end-to-end headers.
fn forward(agent: &ureq::Agent, url: &str, req: Req) -> Result<http::Response<ureq::Body>, Fail> {
    let mut b = http::Request::builder().method(req.method.as_str()).uri(url);
    for (k, v) in req.headers.iter().filter(|(k, _)| !hop(k)) {
        b = b.header(k, v);
    }
    let bad = |e: http::Error| Fail(400, format!("cannot forward request: {e}"));
    // GET and HEAD carry no body; every other method sends one, even empty, so it goes out with Content-Length: 0.
    let sent = if matches!(req.method.as_str(), "GET" | "HEAD") {
        agent.run(b.body(()).map_err(bad)?)
    } else {
        agent.run(b.body(req.body).map_err(bad)?)
    };
    sent.map_err(|e| Fail(502, format!("upstream {url}: {e}")))
}

// Writes a status line, the upstream's end-to-end headers and any extra header lines.
fn write_head(
    out: &mut impl Write,
    status: StatusCode,
    headers: &HeaderMap,
    extra: &str,
    keep: bool,
) -> io::Result<()> {
    write!(out, "HTTP/1.1 {} {}\r\n", status.as_u16(), status.canonical_reason().unwrap_or("Unknown"))?;
    for (k, v) in headers.iter().filter(|(k, _)| !hop(k.as_str())) {
        out.write_all(k.as_str().as_bytes())?;
        out.write_all(b": ")?;
        out.write_all(v.as_bytes())?;
        out.write_all(b"\r\n")?;
    }
    write!(out, "{extra}connection: {}\r\n\r\n", if keep { "keep-alive" } else { "close" })
}

// Answers a failure with a plain-text body.
fn write_fail(out: &mut impl Write, Fail(code, msg): Fail, keep: bool) -> io::Result<()> {
    let status = StatusCode::from_u16(code).expect("valid status code");
    let extra = format!("content-type: text/plain\r\ncontent-length: {}\r\n", msg.len() + 1);
    write_head(out, status, &HeaderMap::new(), &extra, keep)?;
    writeln!(out, "{msg}")
}

// Writes one server-sent event, replacing its data with the cleaned message when it answers a recorded request.
fn write_event(
    out: &mut impl Write,
    lines: &[String],
    filter: &Filter,
    pending: &mut Pending,
) -> io::Result<()> {
    let data: Vec<&str> = lines
        .iter()
        .filter_map(|l| l.strip_prefix("data:"))
        .map(|d| d.strip_prefix(' ').unwrap_or(d))
        .collect();
    let cleaned = if data.is_empty() { None } else { filter.rewrite_text(pending, &data.join("\n")) };
    let mut text = String::new();
    for l in lines.iter().filter(|l| cleaned.is_none() || !l.starts_with("data:")) {
        text.push_str(l);
        text.push('\n');
    }
    if let Some(c) = cleaned {
        text.push_str("data: ");
        text.push_str(&c);
        text.push('\n');
    }
    text.push('\n');
    out.write_all(text.as_bytes())?;
    out.flush()
}

// Relays an event stream one event at a time, flushing each so streaming is not delayed.
fn relay_events(
    from: impl BufRead,
    out: &mut impl Write,
    filter: &Filter,
    pending: &mut Pending,
) -> io::Result<()> {
    let mut event = Vec::new();
    for line in from.lines() {
        let line = line?;
        if line.is_empty() {
            write_event(out, &event, filter, pending)?;
            event.clear();
        } else {
            event.push(line);
        }
    }
    if !event.is_empty() {
        write_event(out, &event, filter, pending)?;
    }
    Ok(())
}

// Relays an upstream response, cleaning JSON bodies whole and event streams per event; anything else is copied.
fn relay(
    out: &mut impl Write,
    res: http::Response<ureq::Body>,
    filter: &Filter,
    mut pending: Pending,
    keep: bool,
) -> io::Result<()> {
    let (parts, body) = res.into_parts();
    let mut body = body.into_reader();
    let ctype =
        parts.headers.get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_ascii_lowercase();
    let plain = !parts.headers.contains_key("content-encoding");
    if plain && ctype.starts_with("application/json") {
        let mut buf = Vec::new();
        body.by_ref().take(MAX_BODY as u64 + 1).read_to_end(&mut buf)?;
        if buf.len() <= MAX_BODY {
            let cleaned = std::str::from_utf8(&buf).ok().and_then(|t| filter.rewrite_text(&mut pending, t));
            let buf = cleaned.map_or(buf, String::into_bytes);
            write_head(
                out,
                parts.status,
                &parts.headers,
                &format!("content-length: {}\r\n", buf.len()),
                keep,
            )?;
            return out.write_all(&buf);
        }
        return stream(out, &parts, Some(&buf), &mut body, keep, |b, o| io::copy(b, o).map(|_| ()));
    }
    if plain && ctype.starts_with("text/event-stream") {
        return stream(out, &parts, None, &mut body, keep, |b, mut o| {
            relay_events(BufReader::new(b), &mut o, filter, &mut pending)
        });
    }
    stream(out, &parts, None, &mut body, keep, |b, o| io::copy(b, o).map(|_| ()))
}

// Sends a reply whose length is not known up front: chunked on a kept connection, else until close; replies that cannot have a body get none.
fn stream(
    out: &mut impl Write,
    parts: &http::response::Parts,
    start: Option<&[u8]>,
    body: &mut ureq::BodyReader<'static>,
    keep: bool,
    copy: impl FnOnce(&mut ureq::BodyReader<'static>, &mut dyn Write) -> io::Result<()>,
) -> io::Result<()> {
    let status = parts.status;
    if status.is_informational() || status == StatusCode::NO_CONTENT || status == StatusCode::NOT_MODIFIED {
        return write_head(out, status, &parts.headers, "", keep);
    }
    if !keep {
        write_head(out, status, &parts.headers, "", false)?;
        out.write_all(start.unwrap_or_default())?;
        return copy(body, out);
    }
    write_head(out, status, &parts.headers, "transfer-encoding: chunked\r\n", true)?;
    let mut chunked = Chunked(out);
    chunked.write_all(start.unwrap_or_default())?;
    copy(body, &mut chunked)?;
    chunked.0.write_all(b"0\r\n\r\n")
}

// Handles one request on a connection: checks it, forwards it and relays the answer, or answers the failure; returns whether the connection stays open.
fn exchange(
    r: &mut impl BufRead,
    out: &mut impl Write,
    agent: &ureq::Agent,
    url: &str,
    filter: &Filter,
) -> io::Result<bool> {
    if r.fill_buf()?.is_empty() {
        return Ok(false);
    }
    let req = match read_request(r) {
        Ok(req) => req,
        Err(fail) => {
            write_fail(out, fail, false)?;
            out.flush()?;
            return Ok(false);
        }
    };
    let keep = req.keep;
    if !origin_ok(&req.headers) {
        write_fail(out, Fail(403, "requests from other web origins are refused".into()), keep)?;
    } else {
        let mut pending = Pending::default();
        if let Ok(text) = std::str::from_utf8(&req.body) {
            pending.note_text(text);
        }
        match forward(agent, url, req) {
            Ok(res) => relay(out, res, filter, pending, keep)?,
            Err(fail) => write_fail(out, fail, keep)?,
        }
    }
    out.flush()?;
    Ok(keep)
}

// Serves one connection: requests one after another while the client keeps it open, until it closes, asks to, sends something unreadable or stays idle past the timeout.
fn handle(stream: TcpStream, agent: &ureq::Agent, url: &str, filter: &Filter) -> io::Result<()> {
    stream.set_read_timeout(Some(IDLE_TIMEOUT))?;
    let mut r = BufReader::new(stream.try_clone()?);
    let mut w = BufWriter::new(stream);
    while exchange(&mut r, &mut w, agent, url, filter)? {}
    Ok(())
}

// Accepts connections until the listener fails, serving each on its own thread.
fn serve(listener: TcpListener, agent: ureq::Agent, url: String, filter: Filter) {
    let shared = Arc::new((agent, url, filter));
    for stream in listener.incoming().flatten() {
        let s = Arc::clone(&shared);
        std::thread::spawn(move || handle(stream, &s.0, &s.1, &s.2));
    }
}

// Listens on the given address and forwards every request to the upstream MCP endpoint.
pub fn run(url: &str, listen: &str, filter: &Filter) -> Result<(), String> {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(format!("--upstream must be an http:// or https:// URL, not '{url}'"));
    }
    let listener = TcpListener::bind(listen).map_err(|e| format!("cannot listen on {listen}: {e}"))?;
    let addr = listener.local_addr().map_err(|e| e.to_string())?;
    eprintln!("zazzled proxy: http://{addr} → {url}");
    serve(listener, agent(true), url.to_string(), filter.clone());
    Ok(())
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::net::SocketAddr;

    // A filter with the default settings.
    fn filter() -> Filter {
        Filter {
            lossy: true,
            budget: 8000,
            skip: Vec::new(),
            log: false,
            store: None,
            config: crate::proxy::config::Config::default(),
        }
    }

    // Parses a request from text.
    fn req(text: &str) -> Result<Req, Fail> {
        read_request(&mut Cursor::new(text.as_bytes().to_vec()))
    }

    // A tool result whose text is pretty JSON, answering id 1.
    fn tool_result() -> String {
        r#"{"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"{\n  \"a\": [\n    1,\n    2\n  ]\n}"}]}}"#
            .to_string()
    }

    // A tools/call request with id 1.
    const CALL: &str = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"t"}}"#;

    // Checks a POST with a body and a GET without one parse, with headers kept in order.
    #[test]
    fn test_read_request_ok() {
        let r = req("POST /mcp HTTP/1.1\r\nHost: x\r\nContent-Length: 2\r\nMcp-Session-Id: s\r\n\r\n{}")
            .expect("req");
        assert_eq!(r.method, "POST");
        assert_eq!(r.body, b"{}");
        assert_eq!(header(&r.headers, "mcp-session-id"), Some("s"));
        let r = req("GET /mcp HTTP/1.1\nAccept: text/event-stream\n\n").expect("req");
        assert_eq!((r.method.as_str(), r.body.len(), r.headers.len()), ("GET", 0, 1));
    }

    // Checks each malformed request gets its status.
    #[test]
    fn test_read_request_errors() {
        let code = |t: &str| req(t).unwrap_err().0;
        assert_eq!(code(""), 400);
        assert_eq!(code("POST / HTTP/1.1\r\nno colon\r\n\r\n"), 400);
        assert_eq!(code("POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n"), 411);
        assert_eq!(code("POST / HTTP/1.1\r\nContent-Length: x\r\n\r\n"), 400);
        assert_eq!(code(&format!("POST / HTTP/1.1\r\nContent-Length: {}\r\n\r\n", MAX_BODY + 1)), 413);
        assert_eq!(code("POST / HTTP/1.1\r\nContent-Length: 5\r\n\r\nab"), 400);
        assert_eq!(code(&format!("GET / HTTP/1.1\r\nX: {}\r\n\r\n", "a".repeat(MAX_HEAD))), 431);
        let mut bad = Cursor::new(b"GET / HTTP/1.1\r\nX: \xff\r\n\r\n".to_vec());
        assert_eq!(read_request(&mut bad).unwrap_err().0, 400);
    }

    // Checks local origins pass, absent origins pass and other origins are refused.
    #[test]
    fn test_origin() {
        let h = |o: &str| vec![("Origin".to_string(), o.to_string())];
        assert!(origin_ok(&[]));
        assert!(origin_ok(&h("http://localhost:3000")) && origin_ok(&h("http://127.0.0.1")));
        assert!(origin_ok(&h("http://[::1]:8080/x")));
        assert!(!origin_ok(&h("https://evil.example")) && !origin_ok(&h("null")));
        assert_eq!(origin_host("localhost"), "localhost");
    }

    // Checks events are cleaned when they answer a recorded call and copied otherwise, including a final unterminated one.
    #[test]
    fn test_relay_events() {
        let mut pending = Pending::default();
        pending.note_text(CALL);
        let stream = format!(
            ": ping\n\nevent: message\nid: 4\ndata: {}\n\ndata: {{\"method\":\"x\"}}\nretry: 5",
            tool_result()
        );
        let mut out = Vec::new();
        relay_events(Cursor::new(stream), &mut out, &filter(), &mut pending).expect("relay");
        let text = String::from_utf8(out).expect("utf8");
        assert!(text.starts_with(": ping\n\nevent: message\nid: 4\ndata: {"), "{text}");
        assert!(text.contains(r#""text":"{\"a\":[1,2]}""#));
        assert!(text.ends_with("\n\ndata: {\"method\":\"x\"}\nretry: 5\n\n"));
    }

    // Starts a fake upstream that answers according to the X-Mode request header; returns its URL.
    fn upstream() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut r = BufReader::new(stream.try_clone().expect("clone"));
                let Ok(rq) = read_request(&mut r) else { continue };
                let mut w = stream;
                let mode = header(&rq.headers, "x-mode").unwrap_or("").to_string();
                let (ctype, body) = match mode.as_str() {
                    "sse" => ("text/event-stream", format!("event: message\ndata: {}\n\n", tool_result())),
                    "gzip" => ("application/json\r\ncontent-encoding: gzip", tool_result()),
                    "missing" => ("text/plain", "not here".to_string()),
                    "empty" => ("text/plain", String::new()),
                    "big" => ("application/json", " ".repeat(MAX_BODY + 1)),
                    _ => ("application/json", tool_result()),
                };
                let status = match mode.as_str() {
                    "missing" => "404 Not Found",
                    "empty" => "204 No Content",
                    _ => "200 OK",
                };
                let _ = write!(
                    w,
                    "HTTP/1.1 {status}\r\ncontent-type: {ctype}\r\nmcp-session-id: s1\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        format!("http://{addr}/mcp")
    }

    // Starts the proxy in front of a URL; returns its address.
    fn proxy(url: String) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        std::thread::spawn(move || serve(listener, agent(false), url, filter()));
        addr
    }

    // Sends a raw request to the proxy and returns the whole raw response.
    fn call(addr: SocketAddr, extra: &str, body: &str) -> String {
        let mut s = TcpStream::connect(addr).expect("connect");
        write!(
            s,
            "POST /mcp HTTP/1.1\r\nHost: x\r\nConnection: close\r\n{extra}Content-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .expect("send");
        let mut out = String::new();
        s.read_to_string(&mut out).expect("read");
        out
    }

    // Checks JSON and event-stream answers are cleaned with headers kept and the length recomputed.
    #[test]
    fn test_proxy_cleans_json_and_sse() {
        let addr = proxy(upstream());
        let json = call(addr, "", CALL);
        assert!(json.starts_with("HTTP/1.1 200 OK\r\n") && json.contains("mcp-session-id: s1\r\n"), "{json}");
        let (head, body) = json.split_once("\r\n\r\n").expect("split");
        assert!(head.contains(&format!("content-length: {}\r\n", body.len())));
        assert!(body.contains(r#""text":"{\"a\":[1,2]}""#), "{body}");
        let sse = call(addr, "X-Mode: sse\r\n", CALL);
        assert!(
            sse.contains("event: message\ndata: {") && sse.contains(r#""text":"{\"a\":[1,2]}""#),
            "{sse}"
        );
    }

    // Reads one response from a kept-open connection: its head, then a body sized by Content-Length or sent in chunks.
    fn response(r: &mut BufReader<TcpStream>) -> (String, String) {
        let mut head = String::new();
        loop {
            let mut line = String::new();
            r.read_line(&mut line).expect("head line");
            head.push_str(&line);
            if line == "\r\n" {
                break;
            }
        }
        let lower = head.to_ascii_lowercase();
        let mut body = Vec::new();
        if let Some(n) =
            lower.split("content-length: ").nth(1).and_then(|v| v.split("\r\n").next()?.parse::<usize>().ok())
        {
            body.resize(n, 0);
            r.read_exact(&mut body).expect("body");
        } else if lower.contains("transfer-encoding: chunked") {
            loop {
                let mut size = String::new();
                r.read_line(&mut size).expect("chunk size");
                let n = usize::from_str_radix(size.trim(), 16).expect("hex size");
                let mut chunk = vec![0; n + 2];
                r.read_exact(&mut chunk).expect("chunk");
                if n == 0 {
                    break;
                }
                body.extend_from_slice(&chunk[..n]);
            }
        }
        (head, String::from_utf8(body).expect("utf8"))
    }

    // Checks one connection carries several requests: a sized JSON answer, a chunked event stream and an error, then closes on request.
    #[test]
    fn test_proxy_keeps_connection_open() {
        let addr = proxy(upstream());
        let s = TcpStream::connect(addr).expect("connect");
        let mut w = s.try_clone().expect("clone");
        let mut r = BufReader::new(s);
        let send = |w: &mut TcpStream, extra: &str, body: &str| {
            write!(w, "POST /mcp HTTP/1.1\r\nHost: x\r\n{extra}Content-Length: {}\r\n\r\n{body}", body.len())
                .expect("send")
        };
        send(&mut w, "", CALL);
        let (head, body) = response(&mut r);
        assert!(
            head.contains("connection: keep-alive") && body.contains(r#""text":"{\"a\":[1,2]}""#),
            "{head}{body}"
        );
        send(&mut w, "X-Mode: sse\r\n", CALL);
        let (head, body) = response(&mut r);
        assert!(
            head.contains("transfer-encoding: chunked") && body.contains(r#""text":"{\"a\":[1,2]}""#),
            "{head}{body}"
        );
        send(&mut w, "X-Mode: empty\r\n", "");
        let (head, body) = response(&mut r);
        assert!(
            head.starts_with("HTTP/1.1 204 No Content\r\n") && !head.contains("chunked") && body.is_empty(),
            "{head}"
        );
        send(&mut w, "Origin: https://evil.example\r\n", CALL);
        let (head, _) = response(&mut r);
        assert!(head.starts_with("HTTP/1.1 403") && head.contains("connection: keep-alive"));
        send(&mut w, "Connection: close\r\n", CALL);
        let (head, _) = response(&mut r);
        assert!(head.contains("connection: close"));
        let mut rest = String::new();
        r.read_to_string(&mut rest).expect("closed");
        assert!(rest.is_empty());
    }

    // Checks HTTP/1.0 closes unless it asks to keep the connection, and a client that just hangs up ends the connection quietly.
    #[test]
    fn test_proxy_http10_and_hangup() {
        let addr = proxy(upstream());
        let mut s = TcpStream::connect(addr).expect("connect");
        write!(s, "POST /mcp HTTP/1.0\r\nContent-Length: {}\r\n\r\n{CALL}", CALL.len()).expect("send");
        let mut out = String::new();
        s.read_to_string(&mut out).expect("read");
        assert!(out.contains("connection: close"));
        let s = TcpStream::connect(addr).expect("connect");
        let mut w = s.try_clone().expect("clone");
        let mut r = BufReader::new(s);
        write!(
            w,
            "POST /mcp HTTP/1.0\r\nConnection: keep-alive\r\nContent-Length: {}\r\n\r\n{CALL}",
            CALL.len()
        )
        .expect("send");
        assert!(response(&mut r).0.contains("connection: keep-alive"));
        w.shutdown(std::net::Shutdown::Write).expect("hang up");
        let mut rest = String::new();
        r.read_to_string(&mut rest).expect("closed");
        assert!(rest.is_empty());
    }

    // Checks encoded, error, oversized and unrequested answers pass through untouched.
    #[test]
    fn test_proxy_passes_through() {
        let addr = proxy(upstream());
        let gz = call(addr, "X-Mode: gzip\r\n", CALL);
        assert!(gz.contains("content-encoding: gzip") && gz.ends_with(&tool_result()), "{gz}");
        let missing = call(addr, "X-Mode: missing\r\n", "");
        assert!(
            missing.starts_with("HTTP/1.1 404 Not Found\r\n") && missing.ends_with("not here"),
            "{missing}"
        );
        let big = call(addr, "X-Mode: big\r\n", CALL);
        assert!(big.starts_with("HTTP/1.1 200 OK\r\n") && !big.contains("content-length"));
        assert_eq!(big.split_once("\r\n\r\n").expect("split").1.len(), MAX_BODY + 1);
        let other = call(addr, "", r#"{"id":1,"method":"tools/list"}"#);
        assert!(other.ends_with(&tool_result()));
    }

    // Checks refusals: a foreign origin, a bad header name, an unreachable upstream and a malformed request.
    #[test]
    fn test_proxy_failures() {
        let addr = proxy(upstream());
        let evil = call(addr, "Origin: https://evil.example\r\n", CALL);
        assert!(
            evil.starts_with("HTTP/1.1 403 Forbidden\r\n")
                && evil.ends_with("other web origins are refused\n")
        );
        let bad = call(addr, "Bad Name: x\r\n", CALL);
        assert!(
            bad.starts_with("HTTP/1.1 400 Bad Request\r\n") && bad.contains("cannot forward request"),
            "{bad}"
        );
        let dead = TcpListener::bind("127.0.0.1:0").expect("bind").local_addr().expect("addr");
        let down = call(proxy(format!("http://{dead}/mcp")), "", CALL);
        assert!(
            down.starts_with("HTTP/1.1 502 Bad Gateway\r\n") && down.contains("upstream http://"),
            "{down}"
        );
        let mut s = TcpStream::connect(addr).expect("connect");
        s.write_all(b"POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n").expect("send");
        let mut out = String::new();
        s.read_to_string(&mut out).expect("read");
        assert!(out.starts_with("HTTP/1.1 411 Length Required\r\n"));
    }

    // Checks the run entry point's argument errors.
    #[test]
    fn test_run_errors() {
        assert_eq!(
            run("ftp://x", "127.0.0.1:0", &filter()).unwrap_err(),
            "--upstream must be an http:// or https:// URL, not 'ftp://x'"
        );
        assert!(run("http://x", "not an address", &filter())
            .unwrap_err()
            .starts_with("cannot listen on not an address: "));
    }
}
