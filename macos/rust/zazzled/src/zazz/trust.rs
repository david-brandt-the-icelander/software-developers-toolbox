// Trust: which authorities vouch for a signed script, what they grant, and verdicts cached off the hot path.
use super::crypto;
use super::Zazz;
use serde_json::{json, Value};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use zazz_format::manifest::Network;

pub const SYSTEM_TRUST_DIR: &str = "/etc/zazzle/trust.d";
pub const SYSTEM_POLICY: &str = "/etc/zazzle/policy.json";
const DEFAULT_TTL: u64 = 600;

// Seconds since the Unix epoch.
pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

// True when a path is owned by root and not writable by group or others, so no agent running as the user changed it.
fn root_owned(p: &Path) -> bool {
    std::fs::metadata(p).is_ok_and(|m| m.uid() == 0 && m.mode() & 0o022 == 0)
}

// The global policy: the system file when it is root-owned, else built-in defaults.
pub fn policy(z: &Zazz) -> Value {
    let defaults = json!({"trust_ttl": DEFAULT_TTL, "network": true, "native": true, "require_tests": false});
    let (path, check) = z.policy_path();
    let read = (!check || root_owned(&path)).then(|| std::fs::read(&path).ok()).flatten();
    let Some(v) = read.and_then(|b| serde_json::from_slice::<Value>(&b).ok()) else { return defaults };
    let mut out = defaults;
    for k in ["trust_ttl", "network", "native", "require_tests"] {
        if let Some(x) = v.get(k) {
            out[k] = x.clone();
        }
    }
    out
}

// The trusted authority certificates (DER) from the trust directory, ignoring it unless it is root-owned.
pub fn anchors(z: &Zazz) -> Vec<Vec<u8>> {
    let (dir, check) = z.trust_dir();
    if check && !root_owned(&dir) {
        return Vec::new();
    }
    let Ok(rd) = std::fs::read_dir(&dir) else { return Vec::new() };
    let mut files: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "pem" || x == "crt"))
        .collect();
    files.sort();
    files
        .iter()
        .filter(|p| !check || root_owned(p))
        .filter_map(|p| std::fs::read(p).ok())
        .flat_map(|b| crypto::certs_from(&b).unwrap_or_default())
        .collect()
}

// Intersects two grant sets: native needs both; network and filesystem keep only what both allow.
pub fn intersect(a: &Value, b: &Value) -> Value {
    let native = a["native"] == true && b["native"] == true;
    let net = match (net_of(&a["network"]), net_of(&b["network"])) {
        (Network::None, _) | (_, Network::None) => json!("none"),
        (Network::All, other) | (other, Network::All) => net_json(&other),
        (Network::Hosts(x), Network::Hosts(y)) => {
            let mut both = hosts_within(&x, &y);
            for h in hosts_within(&y, &x) {
                if !both.contains(&h) {
                    both.push(h);
                }
            }
            net_json(&Network::Hosts(both))
        }
    };
    let mut out = json!({"native": native, "network": net});
    match (a.get("fs"), b.get("fs")) {
        (Some(f), None) | (None, Some(f)) => out["fs"] = f.clone(),
        (Some(f), Some(g)) => {
            out["fs"] = json!({"read": paths_within(&strs(&f["read"]), &strs(&g["read"])), "write": paths_within(&strs(&f["write"]), &strs(&g["write"]))})
        }
        (None, None) => {}
    }
    out
}

// Reads a list of strings.
fn strs(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

// Reads a network grant: "none" (or missing), "all", or {"hosts": [...]}.
pub fn net_of(v: &Value) -> Network {
    match v {
        Value::String(s) if s == "all" => Network::All,
        Value::Object(_) => Network::Hosts(strs(&v["hosts"])),
        _ => Network::None,
    }
}

// Writes a network grant as JSON.
pub fn net_json(n: &Network) -> Value {
    match n {
        Network::None => json!("none"),
        Network::All => json!("all"),
        Network::Hosts(h) if h.is_empty() => json!("none"),
        Network::Hosts(h) => json!({"hosts": h}),
    }
}

// The hosts of `want` that some pattern of `allow` covers (a wildcard is kept only under an equal or wider one).
pub fn hosts_within(want: &[String], allow: &[String]) -> Vec<String> {
    want.iter()
        .filter(|w| {
            allow.iter().any(|a| {
                if let Some(base) = w.strip_prefix("*.") {
                    a.strip_prefix("*.").is_some_and(|ab| base == ab || base.ends_with(&format!(".{ab}")))
                } else {
                    zazz::http::host_matches(a, w)
                }
            })
        })
        .cloned()
        .collect()
}

// The sandbox paths of `want` that lie under some path of `allow`.
pub fn paths_within(want: &[String], allow: &[String]) -> Vec<String> {
    let under = |p: &str, root: &str| {
        p == root
            || p.strip_prefix(root).is_some_and(|r| r.starts_with('/'))
            || (root == "/" && p.starts_with('/'))
    };
    want.iter().filter(|w| allow.iter().any(|a| under(w, a))).cloned().collect()
}

// Checks the Keychain trusts a chain (macOS); elsewhere nothing is trusted this way.
#[cfg(target_os = "macos")]
fn keychain_trusts(chain: &[Vec<u8>]) -> bool {
    use security_framework::certificate::SecCertificate;
    use security_framework::policy::SecPolicy;
    use security_framework::trust::SecTrust;
    let Ok(certs) = chain.iter().map(|d| SecCertificate::from_der(d)).collect::<Result<Vec<_>, _>>() else {
        return false;
    };
    SecTrust::create_with_certificates(&certs, &[SecPolicy::create_x509()])
        .is_ok_and(|t| t.evaluate_with_error().is_ok())
}

// Checks the Keychain trusts a chain (macOS); elsewhere nothing is trusted this way.
#[cfg(not(target_os = "macos"))]
fn keychain_trusts(_chain: &[Vec<u8>]) -> bool {
    false
}

// Evaluates a signer's chain now: trusted or not, by whom, with which grants, until when.
pub fn evaluate(z: &Zazz, chain: &[Vec<u8>]) -> Value {
    let ttl = policy(z)["trust_ttl"].as_u64().unwrap_or(DEFAULT_TTL);
    let (at, signer) = (now(), crypto::subject(&chain[0]));
    let leaf_grants = crypto::grants_of(&chain[0]).unwrap_or(json!({}));
    let anchors = anchors(z);
    let found = crypto::verify_chain(chain, &anchors, at);
    let (trusted, authority, grants, reason) = match found {
        Ok(i) => {
            let g = crypto::grants_of(&anchors[i]).unwrap_or(json!({}));
            let g = if anchors[i] == chain[chain.len() - 1] && chain.len() == 1 {
                g
            } else {
                intersect(&g, &leaf_grants)
            };
            (true, Some(crypto::subject(&anchors[i])), g, None)
        }
        Err(_)
            if keychain_trusts(chain)
                && crypto::verify_chain(chain, &chain[chain.len() - 1..], at).is_ok() =>
        {
            (true, Some("keychain".to_string()), leaf_grants, None)
        }
        Err(e) => (false, None, json!({}), Some(e)),
    };
    json!({"trusted": trusted, "signer": signer, "authority": authority, "grants": grants, "reason": reason, "checked_at": at, "valid_until": at + ttl})
}

// What a verdict allows, after the global policy: network as JSON, filesystem grants, and whether native code may run.
pub fn effective(z: &Zazz, verdict: &Value) -> (Value, Option<Value>, bool) {
    let p = policy(z);
    if verdict["trusted"] != true {
        return (json!("none"), None, false);
    }
    let g = &verdict["grants"];
    let net = if p["network"] == false { json!("none") } else { net_json(&net_of(&g["network"])) };
    (net, g.get("fs").cloned(), p["native"] != false && g["native"] == true)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    // A Zazz whose trust directory and policy are in a temporary directory, without the root-ownership check.
    fn zazz(t: &TempDir) -> Zazz {
        let z = Zazz::default();
        z.set_trust_paths(&t.path().join("trust"), &t.path().join("policy.json"));
        std::fs::create_dir_all(t.path().join("trust")).expect("trust dir");
        z
    }

    // Checks host and path intersections.
    #[test]
    fn test_within() {
        let s = |a: &[&str]| a.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(
            hosts_within(
                &s(&["a.x.com", "b.com", "*.x.com", "*.y.x.com", "*.z.com"]),
                &s(&["*.x.com", "b.com"])
            ),
            s(&["a.x.com", "b.com", "*.x.com", "*.y.x.com"])
        );
        assert_eq!(
            paths_within(&s(&["$CWD", "$CWD/out", "/etc", "$HOME"]), &s(&["$CWD/out", "/"])),
            s(&["$CWD/out", "/etc"])
        );
        assert_eq!(paths_within(&s(&["$CWD"]), &s(&["$CWD"])), s(&["$CWD"]));
        assert!(paths_within(&s(&["$CWDX"]), &s(&["$CWD"])).is_empty());
    }

    // Checks grant intersection for native, network and filesystem.
    #[test]
    fn test_intersect() {
        let a = json!({"native": true, "network": "all", "fs": {"read": ["$CWD"], "write": []}});
        let b = json!({"native": false, "network": {"hosts": ["api.x.com"]}});
        assert_eq!(
            intersect(&a, &b),
            json!({"native": false, "network": {"hosts": ["api.x.com"]}, "fs": {"read": ["$CWD"], "write": []}})
        );
        assert_eq!(intersect(&b, &json!({"network": "none"}))["network"], "none");
        let c = json!({"native": true, "network": {"hosts": ["*.x.com"]}, "fs": {"read": ["$CWD/a"], "write": ["$CWD/a"]}});
        assert_eq!(
            intersect(&a, &c),
            json!({"native": true, "network": {"hosts": ["*.x.com"]}, "fs": {"read": [], "write": []}})
        );
        assert_eq!(intersect(&c, &b)["network"], json!({"hosts": ["api.x.com"]}));
        assert_eq!(intersect(&json!({}), &json!({})), json!({"native": false, "network": "none"}));
        assert_eq!(net_json(&Network::Hosts(vec![])), json!("none"));
    }

    // Checks the policy defaults, overrides and that an unowned system file is ignored.
    #[test]
    fn test_policy() {
        let t = TempDir::new();
        let z = zazz(&t);
        assert_eq!(policy(&z)["trust_ttl"], DEFAULT_TTL);
        std::fs::write(t.path().join("policy.json"), r#"{"trust_ttl": 5, "network": false, "other": 1}"#)
            .expect("policy");
        let p = policy(&z);
        assert_eq!(
            (p["trust_ttl"].as_u64(), p["network"].as_bool(), p.get("other")),
            (Some(5), Some(false), None)
        );
        assert!(!root_owned(&t.path().join("policy.json")) || unsafe { libc::getuid() } == 0);
        assert!(!root_owned(&t.path().join("missing")));
    }

    // Checks evaluation: an authority in the trust directory vouches with its grants; others are untrusted.
    #[test]
    fn test_evaluate() {
        let t = TempDir::new();
        let z = zazz(&t);
        let k = crypto::generate_key();
        let cert =
            crypto::self_signed(&k, "ci", 30, &json!({"native": false, "network": {"hosts": ["api.x.com"]}}))
                .expect("cert");
        let v = evaluate(&z, std::slice::from_ref(&cert));
        assert_eq!(
            (v["trusted"].as_bool(), v["reason"].as_str().map(|r| r.contains("no trusted authority"))),
            (Some(false), Some(true))
        );
        assert_eq!(effective(&z, &v), (json!("none"), None, false));
        std::fs::write(t.path().join("trust/ci.pem"), crypto::cert_pem(&cert).expect("pem")).expect("write");
        std::fs::write(t.path().join("trust/notes.txt"), "ignored").expect("notes");
        let v = evaluate(&z, std::slice::from_ref(&cert));
        assert_eq!(v["trusted"], true, "{v}");
        assert!(v["authority"].as_str().expect("authority").contains("CN=ci"));
        assert_eq!(v["valid_until"].as_u64(), Some(v["checked_at"].as_u64().expect("at") + DEFAULT_TTL));
        assert_eq!(effective(&z, &v), (json!({"hosts": ["api.x.com"]}), None, false));
        std::fs::write(t.path().join("policy.json"), r#"{"network": false}"#).expect("policy");
        assert_eq!(effective(&z, &v).0, json!("none"));
        assert!(!keychain_trusts(std::slice::from_ref(&cert)));
        std::fs::write(t.path().join("policy.json"), r#"{"native": false}"#).expect("policy");
        let native =
            json!({"trusted": true, "grants": {"native": true, "network": "all", "fs": {"read": ["$CWD"]}}});
        assert_eq!(effective(&z, &native), (json!("all"), Some(json!({"read": ["$CWD"]})), false));
        std::fs::remove_file(t.path().join("policy.json")).expect("rm");
        assert!(effective(&z, &native).2);
    }

    // Checks a leaf issued by a trusted CA gets the CA's grants narrowed by its own.
    #[test]
    fn test_evaluate_issued_leaf() {
        let t = TempDir::new();
        let z = zazz(&t);
        let (ca_key, leaf_key) = (crypto::generate_key(), crypto::generate_key());
        let ca =
            crypto::self_signed(&ca_key, "org", 30, &json!({"native": true, "network": "all"})).expect("ca");
        let leaf = crypto::tests::leaf_from(
            &ca_key,
            &ca,
            &leaf_key,
            &json!({"native": false, "network": {"hosts": ["a.com"]}}),
        );
        std::fs::write(t.path().join("trust/org.crt"), crypto::cert_pem(&ca).expect("pem")).expect("write");
        let v = evaluate(&z, std::slice::from_ref(&leaf));
        assert_eq!(
            (v["trusted"].as_bool(), v["grants"].clone()),
            (Some(true), json!({"native": false, "network": {"hosts": ["a.com"]}})),
            "{v}"
        );
    }

    // Checks the system trust directory is only read when root-owned.
    #[test]
    fn test_anchor_ownership() {
        let t = TempDir::new();
        let z = Zazz::default();
        z.set_trust_paths_checked(&t.path().join("trust"), &t.path().join("policy.json"));
        std::fs::create_dir_all(t.path().join("trust")).expect("dir");
        let k = crypto::generate_key();
        let cert = crypto::self_signed(&k, "ci", 30, &json!({})).expect("cert");
        std::fs::write(t.path().join("trust/ci.pem"), crypto::cert_pem(&cert).expect("pem")).expect("write");
        let as_root = unsafe { libc::getuid() } == 0;
        assert_eq!(anchors(&z).len(), as_root as usize);
        std::fs::set_permissions(t.path().join("trust"), std::os::unix::fs::PermissionsExt::from_mode(0o777))
            .expect("chmod");
        assert!(anchors(&z).is_empty(), "a world-writable trust directory is ignored");
    }
}
