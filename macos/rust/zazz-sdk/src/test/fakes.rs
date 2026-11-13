// Fake edges for tests: an in-memory filesystem and a scripted network, both applying the manifest's sandbox policy.
use crate::call::{DirEntry, Fs, Http};
use crate::error::{Error, Result};
use crate::http::{self, NetPolicy, Request, Response};
use crate::json::Json;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::Path;
use std::rc::Rc;

// Filesystem rules from the manifest: placeholder-relative or absolute prefixes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsPolicy {
    pub read: Vec<String>,
    pub write: Vec<String>,
}

impl FsPolicy {
    // Everything allowed, the harness default.
    pub fn open() -> FsPolicy {
        FsPolicy {
            read: vec!["/".into(), "$CWD".into(), "$TMP".into(), "$HOME".into()],
            write: vec!["/".into(), "$CWD".into(), "$TMP".into(), "$HOME".into()],
        }
    }
}

const ROOTS: [&str; 3] = ["$CWD", "$TMP", "$HOME"];

// Normalises a script path to "$CWD/...", "$TMP/...", "$HOME/..." or an absolute path, resolving "." and "..".
pub fn normalize(path: &str) -> Result<String> {
    let placeholder =
        ROOTS.iter().find(|r| path == **r || path.strip_prefix(**r).is_some_and(|x| x.starts_with('/')));
    let (root, rest) = match (placeholder, path.strip_prefix('/')) {
        (Some(r), _) => (*r, &path[r.len()..]),
        (None, Some(r)) => ("", r),
        (None, None) => ("$CWD", path),
    };
    let mut parts: Vec<&str> = Vec::new();
    for c in rest.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                parts.pop().ok_or_else(|| Error::sandbox_denied(&format!("{path}: escapes its root")))?;
            }
            c => parts.push(c),
        }
    }
    let joined = parts.join("/");
    Ok(match (root, joined.is_empty()) {
        ("", true) => "/".into(),
        ("", false) => format!("/{joined}"),
        (r, true) => r.into(),
        (r, false) => format!("{r}/{joined}"),
    })
}

// True when a normalised path lies under a rule (same path or below it).
fn under(path: &str, rule: &str) -> bool {
    rule == "/" && path.starts_with('/')
        || path == rule
        || path.strip_prefix(rule).is_some_and(|r| r.starts_with('/'))
}

// The shared state behind a FakeFs.
#[derive(Default)]
struct FsState {
    cwd: String,
    files: BTreeMap<String, Vec<u8>>,
    written: BTreeMap<String, Vec<u8>>,
    denied: Vec<String>,
    write_failures: BTreeMap<String, Error>,
    policy: Option<FsPolicy>,
}

// An in-memory filesystem; clones share state, so the harness can inspect what a call wrote.
#[derive(Clone, Default)]
pub struct FakeFs(Rc<RefCell<FsState>>);

impl FakeFs {
    // Sets the absolute directory that stands for $CWD (the harness uses /work).
    pub fn set_cwd(&self, dir: &str) {
        self.0.borrow_mut().cwd = dir.trim_end_matches('/').to_string();
    }

    // Normalises a path and maps absolute paths inside the working directory to $CWD.
    fn canon(&self, path: &str) -> Result<String> {
        let p = normalize(path)?;
        let cwd = self.0.borrow().cwd.clone();
        match p.strip_prefix(&cwd) {
            Some(rest) if !cwd.is_empty() && (rest.is_empty() || rest.starts_with('/')) => {
                Ok(format!("$CWD{rest}"))
            }
            _ => Ok(p),
        }
    }

    // Sets the sandbox rules applied to every access.
    pub fn set_policy(&self, p: FsPolicy) {
        self.0.borrow_mut().policy = Some(p);
    }

    // Adds a file.
    pub fn add(&self, path: &str, data: impl AsRef<[u8]>) -> &FakeFs {
        let p = self.canon(path).expect("fixture paths stay inside their root");
        self.0.borrow_mut().files.insert(p, data.as_ref().to_vec());
        self
    }

    // Adds every file under a real directory, placed under `at` (for example "." for the caller's directory).
    pub fn load_dir(&self, real: &Path, at: &str) -> std::io::Result<&FakeFs> {
        for item in std::fs::read_dir(real)? {
            let item = item?;
            let name = format!("{}/{}", at.trim_end_matches('/'), item.file_name().to_string_lossy());
            if item.file_type()?.is_dir() {
                self.load_dir(&item.path(), &name)?;
            } else {
                self.add(&name, std::fs::read(item.path())?);
            }
        }
        Ok(self)
    }

    // Makes every access under a path fail with sandbox_denied.
    pub fn deny(&self, path: &str) -> &FakeFs {
        let p = self.canon(path).expect("deny paths stay inside their root");
        self.0.borrow_mut().denied.push(p);
        self
    }

    // Makes writes to one path fail with the given error.
    pub fn fail_write(&self, path: &str, err: Error) -> &FakeFs {
        let p = self.canon(path).expect("paths stay inside their root");
        self.0.borrow_mut().write_failures.insert(p, err);
        self
    }

    // What the script wrote to a path during the test, if anything.
    pub fn written(&self, path: &str) -> Option<Vec<u8>> {
        self.0.borrow().written.get(&self.canon(path).ok()?).cloned()
    }

    // What the script wrote to a path, as text.
    pub fn written_text(&self, path: &str) -> Option<String> {
        self.written(path).map(|b| String::from_utf8_lossy(&b).into_owned())
    }

    // Every path written, in order of name.
    pub fn written_paths(&self) -> Vec<String> {
        self.0.borrow().written.keys().cloned().collect()
    }

    // Normalises a path and applies the deny list and the policy for reading or writing.
    fn check(&self, path: &str, write: bool) -> Result<String> {
        let p = self.canon(path)?;
        let st = self.0.borrow();
        let verb = if write { "write" } else { "read" };
        if st.denied.iter().any(|d| under(&p, d)) {
            return Err(Error::sandbox_denied(&format!("{path}: access denied")));
        }
        if let Some(pol) = &st.policy {
            let rules = if write { &pol.write } else { &pol.read };
            let rules_ok =
                rules.iter().any(|r| under(&p, r)) || (!write && pol.write.iter().any(|r| under(&p, r)));
            if !rules_ok {
                return Err(Error::sandbox_denied(&format!(
                    "{path}: the sandbox does not allow {verb} here"
                )));
            }
        }
        Ok(p)
    }
}

impl Fs for FakeFs {
    // Reads a file from memory.
    fn read(&self, path: &str) -> Result<Vec<u8>> {
        let p = self.check(path, false)?;
        self.0
            .borrow()
            .files
            .get(&p)
            .cloned()
            .ok_or_else(|| Error::not_found(&format!("{path}: no such file")))
    }

    // Writes a file to memory and records it.
    fn write(&self, path: &str, data: &[u8]) -> Result<()> {
        let p = self.check(path, true)?;
        let mut st = self.0.borrow_mut();
        if let Some(e) = st.write_failures.get(&p) {
            return Err(e.clone());
        }
        st.files.insert(p.clone(), data.to_vec());
        st.written.insert(p, data.to_vec());
        Ok(())
    }

    // Lists the direct children of a directory.
    fn list(&self, path: &str) -> Result<Vec<DirEntry>> {
        let p = self.check(path, false)?;
        let prefix = if p == "/" { "/".to_string() } else { format!("{p}/") };
        let mut out: BTreeMap<String, bool> = BTreeMap::new();
        for k in self.0.borrow().files.keys() {
            let Some(rest) = k.strip_prefix(&prefix) else { continue };
            let (name, deeper) = rest.split_once('/').map_or((rest, false), |(n, _)| (n, true));
            *out.entry(name.to_string()).or_default() |= deeper;
        }
        if out.is_empty() {
            return Err(Error::not_found(&format!("{path}: no such directory")));
        }
        Ok(out.into_iter().map(|(name, is_dir)| DirEntry { name, is_dir }).collect())
    }

    // True if a file or directory exists (and the policy allows looking).
    fn exists(&self, path: &str) -> bool {
        self.read(path).is_ok() || self.list(path).is_ok()
    }
}

// One expected request and what to answer.
struct Expectation {
    method: String,
    url: String,
    reply: Result<Response>,
    used: usize,
}

// The shared state behind a FakeHttp.
#[derive(Default)]
struct HttpState {
    expected: Vec<Expectation>,
    requests: Vec<Request>,
    policy: Option<NetPolicy>,
}

// A scripted network: requests must match an expectation; clones share state.
#[derive(Clone, Default)]
pub struct FakeHttp(Rc<RefCell<HttpState>>);

impl FakeHttp {
    // Sets the manifest's network policy, checked before expectations.
    pub fn set_policy(&self, p: NetPolicy) {
        self.0.borrow_mut().policy = Some(p);
    }

    // Expects a request and answers it with a response.
    pub fn expect(&self, method: &str, url: &str, reply: Response) -> &FakeHttp {
        self.push(method, url, Ok(reply))
    }

    // Expects a request and answers it with JSON.
    pub fn expect_json(&self, method: &str, url: &str, status: u16, body: Json) -> &FakeHttp {
        self.push(method, url, Ok(Response::json(status, &body)))
    }

    // Expects a request and fails it (for example a timeout).
    pub fn expect_error(&self, method: &str, url: &str, err: Error) -> &FakeHttp {
        self.push(method, url, Err(err))
    }

    // Records one expectation.
    fn push(&self, method: &str, url: &str, reply: Result<Response>) -> &FakeHttp {
        let e = Expectation { method: method.to_ascii_uppercase(), url: url.to_string(), reply, used: 0 };
        self.0.borrow_mut().expected.push(e);
        self
    }

    // Loads expectations from tests/http.json: [{"request": {"method", "url"}, "response": {"status", "headers", "json" | "text"}}].
    pub fn replay(&self, doc: &Json) -> Result<&FakeHttp> {
        for item in doc.arr().ok_or_else(|| Error::bad_input("http.json must be a list"))? {
            let req = &item["request"];
            let resp = &item["response"];
            let status = resp["status"].i64().unwrap_or(200) as u16;
            let mut r = match resp.get("json") {
                Some(j) => Response::json(status, j),
                None => Response::text(status, resp.str_or("text", "")),
            };
            for h in resp["headers"].arr().unwrap_or(&[]) {
                r.headers.push((h[0].str().unwrap_or("").to_string(), h[1].str().unwrap_or("").to_string()));
            }
            self.expect(req.str_or("method", "GET"), req.need_str("url")?, r);
        }
        Ok(self)
    }

    // Every request the script sent, in order.
    pub fn requests(&self) -> Vec<Request> {
        self.0.borrow().requests.clone()
    }

    // Panics if an expectation was never used.
    pub fn assert_all_used(&self) {
        let st = self.0.borrow();
        let unused: Vec<String> =
            st.expected.iter().filter(|e| e.used == 0).map(|e| format!("{} {}", e.method, e.url)).collect();
        assert!(unused.is_empty(), "zazz::test: expected requests never sent: {}", unused.join(", "));
    }
}

impl Http for FakeHttp {
    // Applies the policy, records the request and answers from the matching expectation.
    fn send(&self, req: &Request) -> Result<Response> {
        let policy = self.0.borrow().policy.clone();
        if let Some(p) = &policy {
            http::check(p, &req.url)?;
        }
        let mut st = self.0.borrow_mut();
        st.requests.push(req.clone());
        let method = req.method.to_ascii_uppercase();
        let hit =
            st.expected.iter_mut().filter(|e| e.method == method && e.url == req.url).min_by_key(|e| e.used);
        let Some(e) = hit else {
            let known: Vec<String> = st.expected.iter().map(|e| format!("{} {}", e.method, e.url)).collect();
            panic!(
                "zazz::test: unexpected request {method} {} (expected one of: {})",
                req.url,
                known.join(", ")
            );
        };
        e.used += 1;
        e.reply.clone()
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    // Checks path normalisation.
    #[test]
    fn test_normalize() {
        assert_eq!(normalize("a/./b/../c").expect("rel"), "$CWD/a/c");
        assert_eq!(normalize(".").expect("dot"), "$CWD");
        assert_eq!(normalize("/x/../").expect("root"), "/");
        assert_eq!(normalize("/usr//share").expect("abs"), "/usr/share");
        assert_eq!(normalize("../x").unwrap_err().code, "sandbox_denied");
        assert_eq!(normalize("$TMP/a/../b").expect("tmp"), "$TMP/b");
        assert_eq!(normalize("$HOME").expect("home"), "$HOME");
        assert_eq!(normalize("$TMPX").expect("not a root"), "$CWD/$TMPX");
    }

    // Checks reads, writes, listing, deny rules, write failures and recorded writes.
    #[test]
    fn test_fake_fs_basics() {
        let fs = FakeFs::default();
        fs.add("in/a.csv", "1,2").add("in/sub/b.csv", "3").add("/etc/hosts", "h");
        assert_eq!(fs.read("in/a.csv").expect("read"), b"1,2");
        assert_eq!(fs.read("./in/../in/a.csv").expect("dots"), b"1,2");
        assert_eq!(fs.read("missing").unwrap_err().code, "not_found");
        let names: Vec<(String, bool)> =
            fs.list("in").expect("list").into_iter().map(|e| (e.name, e.is_dir)).collect();
        assert_eq!(names, vec![("a.csv".into(), false), ("sub".into(), true)]);
        assert_eq!(fs.list("/").expect("root")[0].name, "etc");
        assert!(fs.list("nope").is_err());
        assert!(fs.exists("in") && fs.exists("in/a.csv") && !fs.exists("zz"));
        fs.write("out.txt", b"done").expect("write");
        assert_eq!(fs.written_text("out.txt").as_deref(), Some("done"));
        assert_eq!(fs.written("never"), None);
        assert_eq!(fs.written_paths(), vec!["$CWD/out.txt".to_string()]);
        fs.deny("in/sub");
        assert_eq!(fs.read("in/sub/b.csv").unwrap_err().code, "sandbox_denied");
        fs.fail_write("full.txt", Error::failed("no space"));
        assert_eq!(fs.write("full.txt", b"x").unwrap_err().message, "no space");
    }

    // Checks the sandbox policy: reads need a read or write rule, writes need a write rule.
    #[test]
    fn test_fake_fs_policy() {
        let fs = FakeFs::default();
        fs.add("a", "x").add("/secret", "s").add("$TMP/t", "t");
        fs.set_policy(FsPolicy { read: vec!["$CWD".into()], write: vec!["$TMP".into()] });
        assert!(fs.read("a").is_ok());
        assert!(fs.read("$TMP/t").is_ok());
        assert!(fs.read("/secret").unwrap_err().message.contains("does not allow read"));
        assert!(fs.write("a", b"y").unwrap_err().message.contains("does not allow write"));
        assert!(fs.write("$TMP/u", b"y").is_ok());
        fs.set_policy(FsPolicy::open());
        assert!(fs.read("/secret").is_ok());
        fs.set_cwd("/work/");
        fs.add("/work/in.txt", "w");
        assert_eq!(fs.read("in.txt").expect("mapped"), b"w");
        assert_eq!(fs.read("/work").unwrap_err().code, "not_found");
        assert!(fs.read("/workx").is_err());
    }

    // Checks loading a real directory into the fake.
    #[test]
    fn test_fake_fs_load_dir() {
        let d = std::env::temp_dir().join(format!("zazz-fakefs-{}", std::process::id()));
        std::fs::create_dir_all(d.join("sub")).expect("mkdir");
        std::fs::write(d.join("sub/f.txt"), "hi").expect("write");
        let fs = FakeFs::default();
        fs.load_dir(&d, ".").expect("load");
        assert_eq!(fs.read("sub/f.txt").expect("read"), b"hi");
        assert!(fs.load_dir(&d.join("missing"), ".").is_err());
        let _ = std::fs::remove_dir_all(d);
    }

    // Checks expectations, recording, policy and replay files.
    #[test]
    fn test_fake_http() {
        let h = FakeHttp::default();
        h.expect_json("get", "https://api.x.com/a", 200, crate::json!({"ok": true})).expect_error(
            "POST",
            "https://api.x.com/b",
            Error::failed("timeout"),
        );
        let r = h.send(&Request::get("https://api.x.com/a")).expect("a");
        assert_eq!(r.body_json().expect("json")["ok"], Json::Bool(true));
        assert_eq!(
            h.send(&Request::post_json("https://api.x.com/b", &Json::Null)).unwrap_err().message,
            "timeout"
        );
        assert_eq!(h.requests().len(), 2);
        h.assert_all_used();
        h.set_policy(NetPolicy::Hosts(vec!["api.x.com".into()]));
        assert_eq!(h.send(&Request::get("https://evil.com/")).unwrap_err().code, "network_denied");
        let doc = crate::json!([
            {"request": {"url": "https://api.x.com/r"}, "response": {"json": [1], "headers": [["etag", "1"]]}},
            {"request": {"method": "DELETE", "url": "https://api.x.com/t"}, "response": {"status": 204, "text": "gone"}}
        ]);
        h.replay(&doc).expect("replay");
        assert_eq!(h.send(&Request::get("https://api.x.com/r")).expect("r").header("etag"), Some("1"));
        let mut del = Request::get("https://api.x.com/t");
        del.method = "DELETE".into();
        assert_eq!(h.send(&del).expect("t").status, 204);
        assert!(h.replay(&crate::json!({})).is_err());
        assert!(h.replay(&crate::json!([{"request": {}}])).is_err());
    }

    // Checks an unexpected request panics with the request shown.
    #[test]
    #[should_panic(expected = "unexpected request GET https://api.x.com/zzz")]
    fn test_fake_http_unexpected() {
        let h = FakeHttp::default();
        let _ = h.send(&Request::get("https://api.x.com/zzz"));
    }

    // Checks unused expectations are reported.
    #[test]
    #[should_panic(expected = "never sent: GET https://a.com/")]
    fn test_fake_http_unused() {
        let h = FakeHttp::default();
        h.expect("GET", "https://a.com/", Response::text(200, ""));
        h.assert_all_used();
    }
}
