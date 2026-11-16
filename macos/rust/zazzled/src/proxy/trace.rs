// Proxy tracing: while switched on (`zazzle trace on DIR`), each request the agent sends through a proxy gets a folder MMDD_HHmmss in DIR holding the request, the server's response before and after filtering, and a results file with token counts and timings.
use crate::tokens;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

// At most this many requests wait for their responses at once; the oldest are dropped first.
const MAX_OPEN: usize = 1000;

// A traced request waiting for its response: the message, when it was sent, and where its folder goes.
#[derive(Debug, Clone)]
pub struct Open {
    pub request: Value,
    pub sent: Instant,
    pub at: SystemTime,
    pub dir: PathBuf,
}

// Returns the control file that switches tracing: $ZAZZLE_TRACE_CONTROL, else ~/.zazzle/trace.json; tests use none unless they set the variable.
pub fn control() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("ZAZZLE_TRACE_CONTROL") {
        return (!p.is_empty()).then(|| PathBuf::from(p));
    }
    if cfg!(test) {
        return None;
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".zazzle/trace.json"))
}

// Reads the control file: whether tracing is on and its folder.
fn state() -> (bool, Option<PathBuf>) {
    let v: Value = control()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(json!({}));
    (v["enabled"] == json!(true), v["dir"].as_str().map(PathBuf::from))
}

// Returns the folder to trace into when tracing is on; read on every request, so switching takes effect at once in running proxies.
pub fn dir() -> Option<PathBuf> {
    match state() {
        (true, Some(d)) => Some(d),
        _ => None,
    }
}

// Runs `zazzle trace [on [DIR] | off | status]`: switches tracing for every proxy of this user, or says how it stands.
pub fn command(args: &[String], cwd: &Path) -> Result<String, String> {
    let ctl = control().ok_or("HOME is not set")?;
    let (on, dir) = state();
    let write = |enabled: bool, d: &Path| -> Result<(), String> {
        if let Some(parent) = ctl.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        let v = json!({"enabled": enabled, "dir": d});
        std::fs::write(&ctl, format!("{v}\n")).map_err(|e| format!("{}: {e}", ctl.display()))
    };
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        [] | ["status"] => Ok(match (on, dir) {
            (true, Some(d)) => format!(
                "trace: on, each proxied request goes to a folder in {} ({} so far)",
                d.display(),
                count(&d)
            ),
            (_, Some(d)) => format!("trace: off (it last wrote to {}, {} requests)", d.display(), count(&d)),
            _ => "trace: off".into(),
        }),
        ["on", rest @ ..] if rest.len() <= 1 => {
            let d = match rest.first() {
                Some(a) => crate::util::resolve(a, cwd),
                None => dir.unwrap_or_else(|| ctl.with_file_name("traces")),
            };
            std::fs::create_dir_all(&d).map_err(|e| format!("{}: {e}", d.display()))?;
            write(true, &d)?;
            Ok(format!("trace: on; every proxy writes each request it serves to a folder in {} from its next request on", d.display()))
        }
        ["off"] => {
            match dir {
                Some(d) => write(false, &d)?,
                None => return Ok("trace: off".into()),
            }
            Ok("trace: off; proxies stop writing at their next request".into())
        }
        _ => Err("usage: trace [on [DIR] | off | status]".into()),
    }
}

// Counts the request folders in a trace folder.
fn count(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .map_or(0, |es| es.flatten().filter(|e| e.path().join("4-results.json").exists()).count())
}

// Starts tracing the requests in a client message: each with an id and a method is kept until its response comes.
pub fn note(open: &mut Vec<(String, Open)>, msg: &Value) {
    let Some(dir) = dir() else { return };
    let messages: Vec<&Value> = match msg {
        Value::Array(items) => items.iter().collect(),
        one => vec![one],
    };
    for m in messages.into_iter().filter(|m| m.get("method").is_some()) {
        let Some(id) = m.get("id") else { continue };
        if open.len() >= MAX_OPEN {
            open.remove(0);
        }
        open.push((
            id.to_string(),
            Open { request: m.clone(), sent: Instant::now(), at: SystemTime::now(), dir: dir.clone() },
        ));
    }
}

// Takes the traced request a server message answers.
pub fn take(open: &mut Vec<(String, Open)>, m: &Value) -> Option<Open> {
    if m.get("method").is_some() {
        return None;
    }
    let id = m.get("id")?.to_string();
    let at = open.iter().position(|(k, _)| *k == id)?;
    Some(open.remove(at).1)
}

// Renders a time as local MMDD_HHmmss for folder names and as ISO 8601 with milliseconds and offset.
fn stamps(t: SystemTime) -> (String, String) {
    let d = t.duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default();
    let secs = d.as_secs() as libc::time_t;
    // SAFETY: localtime_r only writes the tm it is given; a zeroed tm is a valid starting value.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: both pointers are valid for the call.
    unsafe { libc::localtime_r(&secs, &mut tm) };
    let off = tm.tm_gmtoff / 60;
    let folder =
        format!("{:02}{:02}_{:02}{:02}{:02}", tm.tm_mon + 1, tm.tm_mday, tm.tm_hour, tm.tm_min, tm.tm_sec);
    let iso = format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}{}{:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec,
        d.subsec_millis(),
        if off < 0 { '-' } else { '+' },
        off.abs() / 60,
        off.abs() % 60
    );
    (folder, iso)
}

// Creates the request's folder, MMDD_HHmmss, with _2, _3.. when that second already has one.
fn folder(dir: &Path, at: SystemTime) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let base = stamps(at).0;
    for n in 1.. {
        let name = if n == 1 { base.clone() } else { format!("{base}_{n}") };
        match std::fs::create_dir(dir.join(&name)) {
            Ok(()) => return Ok(dir.join(name)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    unreachable!("some folder name is free")
}

// The text items of a response's result, joined: what the filter works on.
fn text_of(m: &Value) -> String {
    let r = &m["result"];
    let items = r["content"].as_array().or(r["contents"].as_array());
    items
        .into_iter()
        .flatten()
        .filter_map(|i| i["text"].as_str().or(i.pointer("/resource/text").and_then(Value::as_str)))
        .collect::<Vec<_>>()
        .join("\n")
}

// What the filter did to one response, for the results file.
pub struct Filtering {
    pub skipped: bool,
    pub lossy: bool,
    pub budget: usize,
    pub ms: f64,
}

// Writes a request's four files: the request, the response as the server sent it and as the proxy returned it, and the results; failures are ignored, since tracing must never break the proxy.
pub fn write(open: Open, raw: &Value, returned: &Value, filtering: &Filtering) {
    let Ok(dir) = folder(&open.dir, open.at) else { return };
    let pretty = |v: &Value| format!("{}\n", serde_json::to_string_pretty(v).unwrap_or_default());
    let (raw_text, returned_text) = (raw.to_string(), returned.to_string());
    let (t_raw, t_ret) = (tokens::estimate(&raw_text), tokens::estimate(&returned_text));
    let (x_raw, x_ret) = (text_of(raw), text_of(returned));
    let items = |m: &Value| {
        m["result"]["content"].as_array().or(m["result"]["contents"].as_array()).map_or(0, Vec::len)
    };
    let kept = x_ret
        .find("[r:")
        .map(|i| x_ret[i + 3..].chars().take_while(|c| c.is_ascii_alphanumeric()).collect::<String>());
    let upstream_ms = open.sent.elapsed().as_secs_f64() * 1000.0 - filtering.ms;
    let results = json!({
        "method": open.request["method"],
        "tool": open.request.pointer("/params/name"),
        "id": open.request["id"],
        "server": crate::activity::label(),
        "requested_at": stamps(open.at).1,
        "responded_at": stamps(SystemTime::now()).1,
        "tokens": {
            "raw": t_raw,
            "returned": t_ret,
            "saved": t_raw.saturating_sub(t_ret),
            "saved_percent": (t_raw.saturating_sub(t_ret) as f64 * 1000.0 / t_raw.max(1) as f64).round() / 10.0,
            "request": tokens::estimate(&open.request.to_string()),
        },
        "bytes": {"raw": raw_text.len(), "returned": returned_text.len()},
        "text": {
            "raw_tokens": tokens::estimate(&x_raw),
            "returned_tokens": tokens::estimate(&x_ret),
            "raw_lines": x_raw.lines().count(),
            "returned_lines": x_ret.lines().count(),
            "items_raw": items(raw),
            "items_returned": items(returned),
        },
        "timing_ms": {
            "upstream": (upstream_ms.max(0.0) * 10.0).round() / 10.0,
            "filter": (filtering.ms * 1000.0).round() / 1000.0,
        },
        "filter": {
            "changed": raw != returned,
            "skipped": filtering.skipped,
            "lossy": filtering.lossy,
            "budget": filtering.budget,
            "read_again_id": kept,
        },
        "error": returned.get("error").is_some() || returned.pointer("/result/isError") == Some(&json!(true)),
        "proxy": {"pid": std::process::id(), "zazzled": crate::cmd::VERSION},
    });
    let _ = std::fs::write(dir.join("1-request.json"), pretty(&open.request));
    let _ = std::fs::write(dir.join("2-response-raw.json"), pretty(raw));
    let _ = std::fs::write(dir.join("3-response.json"), pretty(returned));
    let _ = std::fs::write(dir.join("4-results.json"), pretty(&results));
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::testutil::{env_lock, TempDir};

    // Checks switching tracing on and off, the status lines, and the default folder.
    #[test]
    fn test_command() {
        let _g = env_lock();
        let t = TempDir::new();
        let saved = std::env::var_os("ZAZZLE_TRACE_CONTROL");
        std::env::remove_var("ZAZZLE_TRACE_CONTROL");
        assert_eq!(control(), None);
        assert_eq!(command(&[], t.path()).unwrap_err(), "HOME is not set");
        std::env::set_var("ZAZZLE_TRACE_CONTROL", t.path().join("z/trace.json"));
        let a = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(command(&a(&[]), t.path()).expect("status"), "trace: off");
        assert_eq!(command(&a(&["off"]), t.path()).expect("off"), "trace: off");
        assert!(command(&a(&["on", "tr"]), t.path())
            .expect("on")
            .contains(&t.path().join("tr").display().to_string()));
        assert_eq!(dir(), Some(t.path().join("tr")));
        std::fs::create_dir_all(t.path().join("tr/0101_000000")).expect("mkdir");
        std::fs::write(t.path().join("tr/0101_000000/4-results.json"), "{}").expect("write");
        assert!(command(&a(&["status"]), t.path()).expect("status").ends_with("(1 so far)"));
        assert!(command(&a(&["off"]), t.path()).expect("off").starts_with("trace: off;"));
        assert_eq!(dir(), None);
        assert!(command(&a(&[]), t.path()).expect("status").contains("it last wrote to"));
        assert!(command(&a(&["on"]), t.path()).expect("on").contains("/tr"));
        std::fs::write(t.path().join("file"), "x").expect("write");
        assert!(command(&a(&["on", "file/sub"]), t.path()).is_err());
        assert!(command(&a(&["sideways"]), t.path()).unwrap_err().starts_with("usage: trace"));
        assert!(command(&a(&["on", "a", "b"]), t.path()).unwrap_err().starts_with("usage: trace"));
        std::env::set_var("ZAZZLE_TRACE_CONTROL", t.path().join("file/trace.json"));
        assert!(command(&a(&["on", "tr"]), t.path()).is_err());
        std::env::set_var("ZAZZLE_TRACE_CONTROL", "");
        assert_eq!(control(), None);
        match saved {
            Some(v) => std::env::set_var("ZAZZLE_TRACE_CONTROL", v),
            None => std::env::remove_var("ZAZZLE_TRACE_CONTROL"),
        }
    }

    // Checks folder names: MMDD_HHmmss, then _2 for a second request in the same second, and the ISO stamp.
    #[test]
    fn test_folders_and_stamps() {
        let t = TempDir::new();
        let at = SystemTime::now();
        let a = folder(&t.path().join("new"), at).expect("first");
        let b = folder(&t.path().join("new"), at).expect("second");
        let name = a.file_name().expect("name").to_string_lossy().into_owned();
        assert!(name.len() == 11 && name.as_bytes()[4] == b'_', "{name}");
        assert_eq!(b.file_name().expect("name").to_string_lossy(), format!("{name}_2"));
        let iso = stamps(at).1;
        assert!(iso.len() == 29 && iso.as_bytes()[10] == b'T', "{iso}");
        std::fs::write(t.path().join("f"), "x").expect("write");
        assert!(folder(&t.path().join("f"), at).is_err());
        let open = Open {
            request: json!({"id": 1, "method": "ping"}),
            sent: Instant::now(),
            at,
            dir: t.path().join("f"),
        };
        let f = Filtering { skipped: true, lossy: true, budget: 1, ms: 0.0 };
        write(open, &json!({}), &json!({}), &f);
        let open = Open {
            request: json!({"id": 2, "method": "tools/call"}),
            sent: Instant::now(),
            at,
            dir: t.path().join("e"),
        };
        write(open, &json!({"id": 2, "error": {"code": 1}}), &json!({"id": 2, "error": {"code": 1}}), &f);
        let d = std::fs::read_dir(t.path().join("e")).expect("dir").flatten().next().expect("one").path();
        let r: Value =
            serde_json::from_str(&std::fs::read_to_string(d.join("4-results.json")).expect("read"))
                .expect("json");
        assert_eq!((r["error"].as_bool(), r["tool"].is_null()), (Some(true), true));
    }
}
