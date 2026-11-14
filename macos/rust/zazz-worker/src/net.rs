// Network for scripts: the policy check, then HTTPS through ureq with bundled roots and no redirects, or a replay file in tests.
use std::time::Duration;
use zazz::http::{self, NetPolicy, Request, Response};
use zazz::{Error, Json};

pub const MAX_BODY: u64 = 16 << 20;

// Something that answers HTTP requests for a guest.
pub trait Backend {
    // Sends one request.
    fn send(&self, req: &Request, timeout: Duration) -> Result<Response, Error>;
}

// Real HTTPS.
pub struct Https;

impl Backend for Https {
    // Sends a request with ureq, refusing redirects and capping the body.
    fn send(&self, req: &Request, timeout: Duration) -> Result<Response, Error> {
        let config = ureq::Agent::config_builder()
            .max_redirects(0)
            .max_redirects_will_error(false)
            .http_status_as_error(false)
            .timeout_global(Some(timeout))
            .build();
        let agent = ureq::Agent::new_with_config(config);
        let mut b = ureq::http::Request::builder().method(req.method.as_str()).uri(&req.url);
        for (k, v) in &req.headers {
            b = b.header(k, v);
        }
        let r = b.body(req.body.clone()).map_err(|e| Error::bad_input(&format!("bad request: {e}")))?;
        let mut resp = agent.run(r).map_err(|e| Error::failed(&format!("{}: {e}", req.url)))?;
        let headers = resp
            .headers()
            .iter()
            .map(|(k, v)| (k.as_str().to_string(), String::from_utf8_lossy(v.as_bytes()).into_owned()))
            .collect();
        let status = resp.status().as_u16();
        let body = resp.body_mut().with_config().limit(MAX_BODY).read_to_vec();
        let body = body.map_err(|e| Error::failed(&format!("{}: reading the body: {e}", req.url)))?;
        Ok(Response { status, headers, body })
    }
}

// Answers from a replay document (tests/http.json); unknown requests are refused.
#[derive(Debug)]
pub struct Replay(pub Vec<(String, String, Response)>);

impl Replay {
    // Reads [{"request": {"method", "url"}, "response": {"status", "headers", "json" | "text"}}].
    pub fn parse(doc: &Json) -> Result<Replay, String> {
        let items = doc.arr().ok_or("http replay: must be a list")?;
        let mut out = Vec::new();
        for it in items {
            let url = it["request"]["url"].str().ok_or("http replay: every request needs a url")?;
            let method = it["request"].str_or("method", "GET").to_ascii_uppercase();
            let r = &it["response"];
            let status = r["status"].i64().unwrap_or(200) as u16;
            let mut resp = match r.get("json") {
                Some(j) => Response::json(status, j),
                None => Response::text(status, r.str_or("text", "")),
            };
            for h in r["headers"].arr().unwrap_or(&[]) {
                resp.headers
                    .push((h[0].str().unwrap_or("").to_string(), h[1].str().unwrap_or("").to_string()));
            }
            out.push((method, url.to_string(), resp));
        }
        Ok(Replay(out))
    }
}

impl Backend for Replay {
    // Returns the recorded response for a method and URL.
    fn send(&self, req: &Request, _timeout: Duration) -> Result<Response, Error> {
        let m = req.method.to_ascii_uppercase();
        let hit = self.0.iter().find(|(rm, ru, _)| *rm == m && *ru == req.url);
        hit.map(|(_, _, r)| r.clone()).ok_or_else(|| {
            Error::network_denied(&format!("unexpected request {m} {} (not in the replay file)", req.url))
        })
    }
}

// Checks the policy, then sends through the backend.
pub fn send(
    policy: &NetPolicy,
    backend: &dyn Backend,
    req: &Request,
    timeout: Duration,
) -> Result<Response, Error> {
    http::check(policy, &req.url)?;
    backend.send(req, timeout)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use zazz::json;

    // Checks replay parsing and lookups.
    #[test]
    fn test_replay() {
        let doc = json!([
            {"request": {"url": "https://a.com/x"}, "response": {"json": {"v": 1}, "headers": [["etag", "1"]]}},
            {"request": {"method": "post", "url": "https://a.com/y"}, "response": {"status": 201, "text": "made"}}
        ]);
        let r = Replay::parse(&doc).expect("parse");
        let t = Duration::from_secs(1);
        let x = r.send(&Request::get("https://a.com/x"), t).expect("x");
        assert_eq!((x.status, x.header("etag")), (200, Some("1")));
        let mut post = Request::get("https://a.com/y");
        post.method = "POST".into();
        assert_eq!(r.send(&post, t).expect("y").body, b"made");
        assert_eq!(r.send(&Request::get("https://a.com/z"), t).unwrap_err().code, "network_denied");
        assert!(Replay::parse(&json!({})).is_err());
        assert!(Replay::parse(&json!([{"request": {}}])).is_err());
    }

    // Checks the policy is applied before any backend is used.
    #[test]
    fn test_send_applies_policy() {
        let r = Replay(vec![("GET".into(), "https://a.com/".into(), Response::text(200, "ok"))]);
        let t = Duration::from_secs(1);
        assert!(send(&NetPolicy::All, &r, &Request::get("https://a.com/"), t).is_ok());
        assert_eq!(
            send(&NetPolicy::None, &r, &Request::get("https://a.com/"), t).unwrap_err().code,
            "network_denied"
        );
    }

    // Checks the real client reports connection failures as errors (no network is needed: the host does not resolve).
    #[test]
    fn test_https_failure_is_an_error() {
        let e =
            Https.send(&Request::get("https://zazz-test.invalid/"), Duration::from_millis(500)).unwrap_err();
        assert_eq!(e.code, "failed");
        let mut bad = Request::get("https://a.com/");
        bad.method = "BAD METHOD".into();
        assert_eq!(Https.send(&bad, Duration::from_millis(500)).unwrap_err().code, "bad_input");
    }
}
