// The zazz.json manifest: strict parsing, defaults, and validation against the archive it came in.
use crate::zip::{check_name, Archive};
use serde_json::{Map, Value};

pub const MANIFEST: &str = "zazz.json";
pub const SIGNATURE: &str = "zazz.sig";
pub const MESSAGES: [&str; 3] = ["json-command", "openapi", "service-call"];
const PLACEHOLDERS: [&str; 3] = ["$CWD", "$TMP", "$HOME"];
const TOP_KEYS: [&str; 13] = [
    "zazz",
    "name",
    "version",
    "description",
    "kind",
    "entry",
    "edition",
    "native",
    "tests",
    "messages",
    "sandbox",
    "limits",
    "cli",
];
const MAX_CALL_MS: u64 = 3_600_000;
const MAX_MEMORY_MB: u64 = 4096;
const MAX_TEST_MS: u64 = 600_000;

// What the archive holds and how it is built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Rust,
    Wasm,
    NativeBin,
}

// The network a script asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Network {
    None,
    Hosts(Vec<String>),
    All,
}

// Filesystem, network and exec access the script asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sandbox {
    pub read: Vec<String>,
    pub write: Vec<String>,
    pub network: Network,
    pub exec: bool,
}

// Where the script's tests are and how long they may run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tests {
    pub paths: Vec<String>,
    pub module: Option<String>,
    pub timeout_ms: u64,
}

// A parsed, validated manifest with defaults filled in.
#[derive(Debug, Clone, PartialEq)]
pub struct Manifest {
    pub name: String,
    pub version: String,
    pub description: String,
    pub kind: Kind,
    pub entry: String,
    pub edition: String,
    pub native: bool,
    pub tests: Option<Tests>,
    pub messages: Vec<String>,
    pub sandbox: Sandbox,
    pub call_ms: u64,
    pub memory_mb: u64,
    pub cli: Option<Value>,
}

// Fails if an object has keys outside the allowed set.
fn only_keys(m: &Map<String, Value>, allowed: &[&str], at: &str) -> Result<(), String> {
    match m.keys().find(|k| !allowed.contains(&k.as_str())) {
        Some(k) => Err(format!("{at}: unknown key '{k}' (allowed: {})", allowed.join(", "))),
        None => Ok(()),
    }
}

// Returns an optional string field.
fn opt_str(m: &Map<String, Value>, key: &str) -> Result<Option<String>, String> {
    match m.get(key) {
        None => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(format!("{key}: must be a string")),
    }
}

// Returns a required, non-empty string field.
fn need_str(m: &Map<String, Value>, key: &str) -> Result<String, String> {
    match opt_str(m, key)? {
        Some(s) if !s.is_empty() => Ok(s),
        _ => Err(format!("{key}: required")),
    }
}

// Returns an optional boolean field, defaulting to false.
fn opt_bool(m: &Map<String, Value>, key: &str) -> Result<bool, String> {
    match m.get(key) {
        None => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(format!("{key}: must be true or false")),
    }
}

// Returns an optional whole number within a range, or the default.
fn opt_num(m: &Map<String, Value>, at: &str, key: &str, default: u64, max: u64) -> Result<u64, String> {
    match m.get(key) {
        None => Ok(default),
        Some(v) => v
            .as_u64()
            .filter(|n| (1..=max).contains(n))
            .ok_or_else(|| format!("{at}.{key}: must be 1..={max}")),
    }
}

// Returns an optional list of strings.
fn opt_list(m: &Map<String, Value>, key: &str) -> Result<Option<Vec<String>>, String> {
    let Some(v) = m.get(key) else { return Ok(None) };
    let items = v.as_array().ok_or_else(|| format!("{key}: must be a list of strings"))?;
    let strs: Option<Vec<String>> = items.iter().map(|i| i.as_str().map(str::to_string)).collect();
    strs.map(Some).ok_or_else(|| format!("{key}: must be a list of strings"))
}

// Returns an optional nested object.
fn opt_obj<'a>(m: &'a Map<String, Value>, key: &str) -> Result<Option<&'a Map<String, Value>>, String> {
    match m.get(key) {
        None => Ok(None),
        Some(Value::Object(o)) => Ok(Some(o)),
        Some(_) => Err(format!("{key}: must be an object")),
    }
}

// Checks a script name: lowercase letters, digits and dashes, starting with a letter.
fn check_script_name(name: &str) -> Result<(), String> {
    let ok = name.len() <= 64
        && name.starts_with(|c: char| c.is_ascii_lowercase())
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if ok {
        Ok(())
    } else {
        Err(format!("name: '{name}' must be lowercase letters, digits and '-', starting with a letter"))
    }
}

// Checks a sandbox path: a placeholder or an absolute path, with no '..' and no empty components.
pub fn check_sandbox_path(p: &str) -> Result<(), String> {
    let rest =
        PLACEHOLDERS.iter().find_map(|ph| p.strip_prefix(ph)).or_else(|| p.strip_prefix('/').map(|_| p));
    let bad = |why: &str| Err(format!("sandbox path '{p}': {why}"));
    let Some(rest) = rest else { return bad("must start with $CWD, $TMP, $HOME or '/'") };
    if p.contains('\0') || p.split('/').any(|c| c == ".." || c == ".") {
        return bad("must not contain '.' or '..' components");
    }
    if !rest.is_empty() && !rest.starts_with('/') {
        return bad("a placeholder must be followed by '/' or nothing");
    }
    Ok(())
}

// Checks a host pattern: lowercase DNS labels, optionally starting with a single "*." wildcard.
pub fn check_host(h: &str) -> Result<(), String> {
    let name = h.strip_prefix("*.").unwrap_or(h);
    let label_ok = |l: &str| {
        (1..=63).contains(&l.len())
            && !l.starts_with('-')
            && !l.ends_with('-')
            && l.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    };
    let numeric = name.split('.').all(|l| l.chars().all(|c| c.is_ascii_digit()));
    if name.len() > 253 || numeric || !name.split('.').all(label_ok) {
        return Err(format!(
            "host '{h}' must be a lowercase DNS name (optionally starting with '*.'), not an IP"
        ));
    }
    Ok(())
}

// Parses the network field: "none", "all" or {"hosts": [...]}.
fn parse_network(v: Option<&Value>) -> Result<Network, String> {
    match v {
        None => Ok(Network::None),
        Some(Value::String(s)) if s == "none" => Ok(Network::None),
        Some(Value::String(s)) if s == "all" => Ok(Network::All),
        Some(Value::Object(o)) => {
            only_keys(o, &["hosts"], "sandbox.network")?;
            let hosts = opt_list(o, "hosts")?.filter(|h| !h.is_empty());
            let hosts = hosts.ok_or("sandbox.network.hosts: must be a non-empty list")?;
            hosts.iter().try_for_each(|h| check_host(h))?;
            Ok(Network::Hosts(hosts))
        }
        Some(_) => Err("sandbox.network: must be \"none\", \"all\" or {\"hosts\": [...]}".into()),
    }
}

// Parses the sandbox object, defaulting to read-only access to the caller's directory.
fn parse_sandbox(m: Option<&Map<String, Value>>) -> Result<Sandbox, String> {
    let empty = Map::new();
    let m = m.unwrap_or(&empty);
    only_keys(m, &["read", "write", "network", "exec"], "sandbox")?;
    let read = opt_list(m, "read")?.unwrap_or_else(|| vec!["$CWD".into()]);
    let write = opt_list(m, "write")?.unwrap_or_default();
    read.iter().chain(&write).try_for_each(|p| check_sandbox_path(p))?;
    Ok(Sandbox { read, write, network: parse_network(m.get("network"))?, exec: opt_bool(m, "exec")? })
}

// Parses the tests object.
fn parse_tests(m: Option<&Map<String, Value>>, kind: Kind) -> Result<Option<Tests>, String> {
    let Some(m) = m else { return Ok(None) };
    only_keys(m, &["paths", "module", "timeout_ms"], "tests")?;
    let paths = opt_list(m, "paths")?.unwrap_or_default();
    let module = opt_str(m, "module")?;
    paths.iter().chain(module.iter()).try_for_each(|p| check_name(p).map_err(|e| format!("tests: {e}")))?;
    match (kind, paths.is_empty(), &module) {
        (Kind::Wasm, _, None) => return Err("tests.module: required for kind \"wasm\"".into()),
        (Kind::Rust | Kind::NativeBin, true, _) => {
            return Err("tests.paths: required for kinds \"rust\" and \"native-bin\"".into())
        }
        _ => {}
    }
    Ok(Some(Tests { paths, module, timeout_ms: opt_num(m, "tests", "timeout_ms", 120_000, MAX_TEST_MS)? }))
}

// Parses the message list, which must include json-command.
fn parse_messages(m: &Map<String, Value>) -> Result<Vec<String>, String> {
    let msgs = opt_list(m, "messages")?.unwrap_or_else(|| vec!["json-command".into()]);
    if let Some(bad) = msgs.iter().find(|s| !MESSAGES.contains(&s.as_str())) {
        return Err(format!("messages: unknown message '{bad}' (known: {})", MESSAGES.join(", ")));
    }
    if !msgs.iter().any(|s| s == "json-command") {
        return Err("messages: must include \"json-command\"".into());
    }
    if msgs.iter().any(|s| s == "service-call") && !msgs.iter().any(|s| s == "openapi") {
        return Err("messages: \"service-call\" needs \"openapi\"".into());
    }
    Ok(msgs)
}

// Parses the kind field.
fn parse_kind(m: &Map<String, Value>) -> Result<Kind, String> {
    match need_str(m, "kind")?.as_str() {
        "rust" => Ok(Kind::Rust),
        "wasm" => Ok(Kind::Wasm),
        "native-bin" => Ok(Kind::NativeBin),
        other => Err(format!("kind: '{other}' must be \"rust\", \"wasm\" or \"native-bin\"")),
    }
}

impl Manifest {
    // Parses manifest JSON text and fills in defaults.
    pub fn parse(text: &[u8]) -> Result<Manifest, String> {
        let wrap = |e: String| format!("{MANIFEST}: {e}");
        let v: Value = serde_json::from_slice(text).map_err(|e| wrap(format!("invalid JSON: {e}")))?;
        Manifest::from_value(&v).map_err(wrap)
    }

    // Builds a manifest from a parsed JSON value.
    fn from_value(v: &Value) -> Result<Manifest, String> {
        let m = v.as_object().ok_or("must be a JSON object")?;
        only_keys(m, &TOP_KEYS, "top level")?;
        if m.get("zazz").and_then(Value::as_u64) != Some(1) {
            return Err("zazz: must be 1".into());
        }
        let name = need_str(m, "name")?;
        check_script_name(&name)?;
        let kind = parse_kind(m)?;
        let entry = need_str(m, "entry")?;
        check_name(&entry).map_err(|e| format!("entry: {e}"))?;
        let edition = opt_str(m, "edition")?.unwrap_or_else(|| "2021".into());
        if !["2018", "2021", "2024"].contains(&edition.as_str()) {
            return Err(format!("edition: '{edition}' must be 2018, 2021 or 2024"));
        }
        let limits = opt_obj(m, "limits")?;
        let empty = Map::new();
        let lm = limits.unwrap_or(&empty);
        only_keys(lm, &["call_ms", "memory_mb"], "limits")?;
        let native = opt_bool(m, "native")?;
        if native && kind != Kind::Rust {
            return Err("native: only kind \"rust\" can ask for a native build".into());
        }
        let cli = m.get("cli").cloned();
        if cli.as_ref().is_some_and(|c| !c.is_object()) {
            return Err("cli: must be an object".into());
        }
        Ok(Manifest {
            name,
            version: opt_str(m, "version")?.unwrap_or_else(|| "0.0.0".into()),
            description: opt_str(m, "description")?.unwrap_or_default(),
            kind,
            entry,
            edition,
            native,
            tests: parse_tests(opt_obj(m, "tests")?, kind)?,
            messages: parse_messages(m)?,
            sandbox: parse_sandbox(opt_obj(m, "sandbox")?)?,
            call_ms: opt_num(lm, "limits", "call_ms", 30_000, MAX_CALL_MS)?,
            memory_mb: opt_num(lm, "limits", "memory_mb", 256, MAX_MEMORY_MB)?,
            cli,
        })
    }

    // Checks that the files the manifest names exist in the archive.
    pub fn check_against(&self, a: &Archive) -> Result<(), String> {
        let wrap = |e: String| Err(format!("{MANIFEST}: {e}"));
        if a.get(&self.entry).is_none() {
            return wrap(format!("entry: '{}' is not in the archive", self.entry));
        }
        let suffix = match self.kind {
            Kind::Rust => ".rs",
            Kind::Wasm => ".wasm",
            Kind::NativeBin => "",
        };
        if !self.entry.ends_with(suffix) {
            return wrap(format!("entry: '{}' must end in {suffix}", self.entry));
        }
        let Some(t) = &self.tests else { return Ok(()) };
        if let Some(m) = t.module.as_ref().filter(|m| a.get(m).is_none()) {
            return wrap(format!("tests.module: '{m}' is not in the archive"));
        }
        if let Some(p) = t.paths.iter().find(|p| a.get(p).is_none() && a.under(p).next().is_none()) {
            return wrap(format!("tests.paths: nothing at '{p}' in the archive"));
        }
        Ok(())
    }

    // Reads and validates the manifest of an archive.
    pub fn of(a: &Archive) -> Result<Manifest, String> {
        let text = a.get(MANIFEST).ok_or_else(|| format!("{MANIFEST} is missing from the archive"))?;
        let m = Manifest::parse(text)?;
        m.check_against(a)?;
        Ok(m)
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::zip::Entry;
    use serde_json::json;

    // Parses a JSON value as a manifest.
    fn parse(v: Value) -> Result<Manifest, String> {
        Manifest::parse(v.to_string().as_bytes())
    }

    // A minimal valid Rust manifest.
    fn base() -> Value {
        json!({"zazz": 1, "name": "csv-sum", "kind": "rust", "entry": "src/main.rs"})
    }

    // Returns the base manifest with one key replaced.
    fn with(key: &str, v: Value) -> Value {
        let mut b = base();
        b[key] = v;
        b
    }

    // Checks defaults are filled in for a minimal manifest.
    #[test]
    fn test_parse_minimal_defaults() {
        let m = parse(base()).expect("parse");
        assert_eq!((m.kind, m.edition.as_str(), m.version.as_str()), (Kind::Rust, "2021", "0.0.0"));
        assert_eq!(m.messages, vec!["json-command".to_string()]);
        assert_eq!(
            m.sandbox,
            Sandbox { read: vec!["$CWD".into()], write: vec![], network: Network::None, exec: false }
        );
        assert_eq!(
            (m.call_ms, m.memory_mb, m.native, m.tests.is_none(), m.cli.is_none()),
            (30_000, 256, false, true, true)
        );
    }

    // Checks a full manifest with every section parses.
    #[test]
    fn test_parse_full() {
        let v = json!({
            "zazz": 1, "name": "gh-sync", "version": "1.2.3", "description": "d", "kind": "rust",
            "entry": "src/main.rs", "edition": "2024", "native": true,
            "tests": {"paths": ["tests"], "timeout_ms": 5000},
            "messages": ["json-command", "openapi", "service-call"],
            "sandbox": {"read": ["$CWD", "/usr/share/dict"], "write": ["$TMP/out"], "network": {"hosts": ["api.github.com", "*.example.org"]}, "exec": false},
            "limits": {"call_ms": 1000, "memory_mb": 64},
            "cli": {"args": []}
        });
        let m = parse(v).expect("parse");
        assert_eq!(parse(with("name", json!("a1-b"))).expect("digits").name, "a1-b");
        let net = |n: &str| parse(with("sandbox", json!({"network": n}))).expect("net").sandbox.network;
        assert_eq!((net("none"), net("all")), (Network::None, Network::All));
        assert_eq!(m.sandbox.network, Network::Hosts(vec!["api.github.com".into(), "*.example.org".into()]));
        assert_eq!(m.tests.expect("tests").timeout_ms, 5000);
        assert_eq!((m.call_ms, m.memory_mb, m.native), (1000, 64, true));
    }

    // Checks each malformed field gets a message naming it.
    #[test]
    fn test_parse_errors_name_the_field() {
        let cases: Vec<(Value, &str)> = vec![
            (json!([]), "JSON object"),
            (with("zazz", json!(2)), "zazz: must be 1"),
            (with("nmae", json!("x")), "unknown key 'nmae'"),
            (with("name", json!("Bad Name")), "name: 'Bad Name'"),
            (with("name", json!(5)), "name: must be a string"),
            (with("kind", json!("python")), "kind: 'python'"),
            (with("entry", json!("../x.rs")), "entry:"),
            (with("edition", json!("2015")), "edition:"),
            (with("native", json!("yes")), "native: must be true or false"),
            (with("messages", json!(["openapi"])), "must include \"json-command\""),
            (with("messages", json!(["json-command", "x"])), "unknown message 'x'"),
            (with("messages", json!(["json-command", "service-call"])), "needs \"openapi\""),
            (with("messages", json!("json-command")), "list of strings"),
            (with("messages", json!([1])), "list of strings"),
            (with("sandbox", json!([])), "sandbox: must be an object"),
            (with("sandbox", json!({"net": 1})), "unknown key 'net'"),
            (with("sandbox", json!({"read": ["relative"]})), "must start with"),
            (with("sandbox", json!({"read": ["$CWD/../x"]})), "'..'"),
            (with("sandbox", json!({"write": ["$CWDx"]})), "followed by '/'"),
            (with("sandbox", json!({"network": "some"})), "sandbox.network: must be"),
            (with("sandbox", json!({"network": {"hosts": []}})), "non-empty list"),
            (with("sandbox", json!({"network": {"hosts": ["A.com"]}})), "lowercase DNS"),
            (with("sandbox", json!({"network": {"hosts": ["10.0.0.1"]}})), "not an IP"),
            (with("sandbox", json!({"network": {"ports": []}})), "unknown key 'ports'"),
            (with("limits", json!({"call_ms": 0})), "limits.call_ms: must be"),
            (with("tests", json!({"paths": ["t"], "timeout_ms": -1})), "tests.timeout_ms"),
            (with("limits", json!({"cpu": 1})), "unknown key 'cpu'"),
            (with("tests", json!({"paths": []})), "tests.paths: required"),
            (with("tests", json!({"paths": ["/abs"]})), "tests:"),
            (with("tests", json!({"x": 1})), "unknown key 'x'"),
            (with("cli", json!([])), "cli: must be an object"),
            (with("name", json!("")), "name: required"),
            (with("name", json!("a".repeat(65))), "name: 'aaaa"),
            (with("name", json!("a_b")), "name: 'a_b'"),
            (with("sandbox", json!({"network": {"hosts": ["a.".repeat(130)]}})), "lowercase DNS"),
        ];
        for (v, want) in cases {
            let err = parse(v.clone()).unwrap_err();
            assert!(err.contains(want), "{v} -> {err} (wanted {want})");
            assert!(err.starts_with("zazz.json: "), "{err}");
        }
        assert!(Manifest::parse(b"{").unwrap_err().contains("invalid JSON"));
    }

    // Checks kind-specific rules for wasm and native-bin manifests.
    #[test]
    fn test_parse_kind_rules() {
        let wasm =
            json!({"zazz": 1, "name": "w", "kind": "wasm", "entry": "w.wasm", "tests": {"paths": ["t"]}});
        assert!(parse(wasm).unwrap_err().contains("tests.module: required"));
        let wasm = json!({"zazz": 1, "name": "w", "kind": "wasm", "entry": "w.wasm", "native": true});
        assert!(parse(wasm).unwrap_err().contains("only kind \"rust\""));
        let bin = json!({"zazz": 1, "name": "b", "kind": "native-bin", "entry": "bin/b"});
        assert_eq!(parse(bin).expect("bin").kind, Kind::NativeBin);
    }

    // Checks the sandbox path and host validators directly.
    #[test]
    fn test_path_and_host_checks() {
        for ok in ["$CWD", "$TMP/x", "$HOME/data", "/usr/share"] {
            assert!(check_sandbox_path(ok).is_ok(), "{ok}");
        }
        for bad in ["x", "$CWD/./x", "/a\0b", "$PWD"] {
            assert!(check_sandbox_path(bad).is_err(), "{bad}");
        }
        for ok in ["localhost", "api.github.com", "*.example.org", "a-b.c1"] {
            assert!(check_host(ok).is_ok(), "{ok}");
        }
        for bad in ["", "-a.com", "a-.com", "a..b", "*.*.a", "a_b.com", &"a".repeat(64)] {
            assert!(check_host(bad).is_err(), "{bad}");
        }
    }

    // Builds an archive from names with fixed contents.
    fn archive(names: &[&str], manifest: &Value) -> Archive {
        let mut e: Vec<Entry> =
            names.iter().map(|n| Entry { name: n.to_string(), data: b"x".to_vec() }).collect();
        e.push(Entry { name: MANIFEST.into(), data: manifest.to_string().into_bytes() });
        Archive::new(e)
    }

    // Checks the manifest is checked against the files actually in the archive.
    #[test]
    fn test_of_checks_archive_contents() {
        let m = with("tests", json!({"paths": ["tests"]}));
        assert!(Manifest::of(&archive(&["src/main.rs", "tests/a.rs"], &m)).is_ok());
        let file = with("tests", json!({"paths": ["tests/a.rs"]}));
        assert!(Manifest::of(&archive(&["src/main.rs", "tests/a.rs"], &file)).is_ok());
        assert!(Manifest::of(&archive(&["src/main.rs"], &m)).unwrap_err().contains("nothing at 'tests'"));
        assert!(Manifest::of(&archive(&["src/lib.rs"], &base())).unwrap_err().contains("not in the archive"));
        let bad_ext = with("entry", json!("src/main.txt"));
        assert!(Manifest::of(&archive(&["src/main.txt"], &bad_ext)).unwrap_err().contains("must end in .rs"));
        assert!(Manifest::of(&Archive::new(vec![])).unwrap_err().contains("missing"));
        let w =
            json!({"zazz": 1, "name": "w", "kind": "wasm", "entry": "w.wasm", "tests": {"module": "t.wasm"}});
        assert!(Manifest::of(&archive(&["w.wasm"], &w)).unwrap_err().contains("tests.module"));
        assert!(Manifest::of(&archive(&["w.wasm", "t.wasm"], &w)).is_ok());
        let bin = json!({"zazz": 1, "name": "b", "kind": "native-bin", "entry": "bin/b"});
        assert!(Manifest::of(&archive(&["bin/b"], &bin)).is_ok());
    }
}
