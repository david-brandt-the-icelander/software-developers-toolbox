// HTTP requests and responses, their wire layout for the zazz_host ABI, and the host allowlist shared by host and fakes.
use crate::error::{Error, Result};
use crate::json::Json;

// Which hosts a script may reach.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetPolicy {
    None,
    Hosts(Vec<String>),
    All,
}

// An outgoing request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

// A response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

// The parts of an https URL the policy needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Url {
    pub host: String,
    pub port: u16,
    pub path: String,
}

impl Request {
    // A GET request.
    pub fn get(url: &str) -> Request {
        Request { method: "GET".into(), url: url.into(), headers: Vec::new(), body: Vec::new() }
    }

    // A POST request with a JSON body.
    pub fn post_json(url: &str, body: &Json) -> Request {
        let headers = vec![("content-type".into(), "application/json".into())];
        Request { method: "POST".into(), url: url.into(), headers, body: body.to_string().into_bytes() }
    }

    // Adds a header.
    pub fn header(mut self, k: &str, v: &str) -> Request {
        self.headers.push((k.to_ascii_lowercase(), v.to_string()));
        self
    }

    // Encodes the request for the host: u32 LE head length, JSON head, then the body.
    pub fn encode(&self) -> Vec<u8> {
        let head = crate::json!({"method": (&self.method), "url": (&self.url), "headers": (pairs_json(&self.headers))});
        frame(&head, &self.body)
    }

    // Decodes a request encoded by `encode`.
    pub fn decode(buf: &[u8]) -> Result<Request> {
        let (head, body) = unframe(buf)?;
        Ok(Request {
            method: head.need_str("method")?.to_string(),
            url: head.need_str("url")?.to_string(),
            headers: pairs_from(&head["headers"])?,
            body,
        })
    }
}

impl Response {
    // A response with a JSON body.
    pub fn json(status: u16, body: &Json) -> Response {
        let headers = vec![("content-type".into(), "application/json".into())];
        Response { status, headers, body: body.to_string().into_bytes() }
    }

    // A response with a text body.
    pub fn text(status: u16, body: &str) -> Response {
        Response { status, headers: Vec::new(), body: body.as_bytes().to_vec() }
    }

    // Parses the body as JSON.
    pub fn body_json(&self) -> Result<Json> {
        Json::parse_bytes(&self.body)
    }

    // Returns the first header with a name, ignoring case.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    // Encodes the response for the guest.
    pub fn encode(&self) -> Vec<u8> {
        let head = crate::json!({"status": (self.status as i64), "headers": (pairs_json(&self.headers))});
        frame(&head, &self.body)
    }

    // Decodes a response encoded by `encode`.
    pub fn decode(buf: &[u8]) -> Result<Response> {
        let (head, body) = unframe(buf)?;
        let status = head.need_i64("status")?;
        let status = u16::try_from(status).map_err(|_| Error::bad_input("status out of range"))?;
        Ok(Response { status, headers: pairs_from(&head["headers"])?, body })
    }
}

// Renders header pairs as [[k, v], ...].
fn pairs_json(pairs: &[(String, String)]) -> Json {
    Json::Arr(pairs.iter().map(|(k, v)| crate::json!([(k), (v)])).collect())
}

// Reads header pairs from [[k, v], ...].
fn pairs_from(v: &Json) -> Result<Vec<(String, String)>> {
    let bad = || Error::bad_input("headers must be [[name, value], ...]");
    let Some(items) = v.arr() else { return if v.is_null() { Ok(Vec::new()) } else { Err(bad()) } };
    let pair = |p: &Json| Some((p[0].str()?.to_string(), p[1].str()?.to_string()));
    items.iter().map(|p| pair(p).ok_or_else(bad)).collect()
}

// Joins a JSON head and a body with a length prefix.
fn frame(head: &Json, body: &[u8]) -> Vec<u8> {
    let h = head.to_string();
    let mut out = (h.len() as u32).to_le_bytes().to_vec();
    out.extend_from_slice(h.as_bytes());
    out.extend_from_slice(body);
    out
}

// Splits a framed buffer into its JSON head and body.
fn unframe(buf: &[u8]) -> Result<(Json, Vec<u8>)> {
    let short = || Error::bad_input("truncated http frame");
    let len = u32::from_le_bytes(buf.get(..4).ok_or_else(short)?.try_into().map_err(|_| short())?) as usize;
    let head = buf.get(4..4 + len).ok_or_else(short)?;
    Ok((Json::parse_bytes(head)?, buf[4 + len..].to_vec()))
}

// Parses an https URL, refusing other schemes, credentials and IP literals.
pub fn parse_url(url: &str) -> Result<Url> {
    let bad = |why: &str| Error::network_denied(&format!("{url}: {why}"));
    let rest = url.strip_prefix("https://").ok_or_else(|| bad("only https:// URLs are allowed"))?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, path) = rest.split_at(end);
    if authority.contains('@') {
        return Err(bad("credentials in URLs are not allowed"));
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().map_err(|_| bad("bad port"))?),
        None => (authority, 443),
    };
    let host = host.to_ascii_lowercase();
    let ip = host.starts_with('[')
        || host.split('.').all(|l| !l.is_empty() && l.bytes().all(|b| b.is_ascii_digit()));
    if host.is_empty() || ip {
        return Err(bad("a host name is required (IP addresses are not allowed)"));
    }
    let path = if path.is_empty() { "/".to_string() } else { path.to_string() };
    Ok(Url { host, port, path })
}

// True when a host matches a pattern: exact, or "*.example.com" for any subdomain (not the apex).
pub fn host_matches(pattern: &str, host: &str) -> bool {
    match pattern.strip_prefix("*.") {
        Some(base) => host.strip_suffix(base).is_some_and(|sub| sub.len() > 1 && sub.ends_with('.')),
        None => pattern == host,
    }
}

// Checks a request URL against a network policy, returning the parsed URL when allowed.
pub fn check(policy: &NetPolicy, url: &str) -> Result<Url> {
    let u = parse_url(url)?;
    match policy {
        NetPolicy::All => Ok(u),
        NetPolicy::Hosts(list) if list.iter().any(|p| host_matches(p, &u.host)) => Ok(u),
        NetPolicy::Hosts(_) => {
            Err(Error::network_denied(&format!("host {} is not in the allowed hosts", u.host)))
        }
        NetPolicy::None => Err(Error::network_denied(&format!("{url}: this script has no network access"))),
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    // Checks requests and responses round-trip through the wire layout.
    #[test]
    fn test_wire_roundtrip() {
        let r = Request::post_json("https://a.com/x", &crate::json!({"k": 1})).header("X-Key", "v");
        assert_eq!(Request::decode(&r.encode()).expect("req"), r);
        let g = Request::get("https://a.com/");
        assert_eq!(Request::decode(&g.encode()).expect("get"), g);
        let s = Response::json(201, &crate::json!([1]));
        let back = Response::decode(&s.encode()).expect("resp");
        assert_eq!(back, s);
        assert_eq!(back.body_json().expect("json"), crate::json!([1]));
        assert_eq!(back.header("Content-Type"), Some("application/json"));
        assert_eq!(back.header("x"), None);
        assert_eq!(Response::text(404, "no").body, b"no");
    }

    // Checks malformed frames are refused.
    #[test]
    fn test_wire_errors() {
        assert!(Request::decode(b"").is_err());
        assert!(Request::decode(&[9, 0, 0, 0, b'{']).is_err());
        assert!(Request::decode(&frame(&crate::json!({"method": 1}), b"")).is_err());
        assert!(
            Request::decode(&frame(&crate::json!({"method": "GET", "url": "u", "headers": 5}), b"")).is_err()
        );
        assert!(Request::decode(&frame(
            &crate::json!({"method": "GET", "url": "u", "headers": [[1, 2]]}),
            b""
        ))
        .is_err());
        assert!(Response::decode(&frame(&crate::json!({"status": 70000}), b"")).is_err());
        assert_eq!(
            Response::decode(&frame(&crate::json!({"status": 200}), b"b")).expect("no headers").body,
            b"b"
        );
    }

    // Checks URL parsing rules.
    #[test]
    fn test_parse_url() {
        assert_eq!(
            parse_url("https://API.GitHub.com:8443/repos?q=1").expect("url"),
            Url { host: "api.github.com".into(), port: 8443, path: "/repos?q=1".into() }
        );
        assert_eq!(parse_url("https://a.com").expect("bare").path, "/");
        for bad in [
            "http://a.com",
            "https://u:p@a.com/",
            "https://1.2.3.4/",
            "https://[::1]/",
            "https://a.com:x/",
            "https:///",
        ] {
            assert_eq!(parse_url(bad).unwrap_err().code, "network_denied", "{bad}");
        }
    }

    // Checks host patterns and the policy check.
    #[test]
    fn test_host_policy() {
        assert!(host_matches("a.com", "a.com"));
        assert!(!host_matches("a.com", "b.a.com"));
        assert!(host_matches("*.a.com", "b.a.com"));
        assert!(!host_matches("*.a.com", "a.com"));
        assert!(!host_matches("*.a.com", "xa.com"));
        let hosts = NetPolicy::Hosts(vec!["api.github.com".into()]);
        assert!(check(&hosts, "https://api.github.com/x").is_ok());
        assert!(check(&hosts, "https://evil.com/").unwrap_err().message.contains("not in the allowed hosts"));
        assert!(check(&NetPolicy::None, "https://a.com/").unwrap_err().message.contains("no network access"));
        assert!(check(&NetPolicy::All, "https://a.com/").is_ok());
        assert!(check(&NetPolicy::All, "http://a.com/").is_err());
    }
}
