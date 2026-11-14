// zazz keygen, zazz sign and zazz trust: make an authority, sign scripts with it, and trust it on this machine.
use super::pipeline::err;
use super::{crypto, trust, Zazz};
use serde_json::{json, Value};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use zazz_format::sig::{self, Envelope};
use zazz_format::zip::{self, Limits};

const DEFAULT_DAYS: u64 = 365;

// Splits a comma-separated list.
fn list(s: &str) -> Vec<String> {
    s.split(',').map(str::trim).filter(|x| !x.is_empty()).map(str::to_string).collect()
}

// Parses keygen options into a name, the grants and the validity in days.
fn keygen_args(args: &[String]) -> Result<(String, Value, u64), Value> {
    let usage = || {
        err("usage", "usage: zazz keygen NAME [--native] [--network none|all|HOST,...] [--read P,...] [--write P,...] [--days N]")
    };
    let (mut name, mut days) = (None, DEFAULT_DAYS);
    let mut grants = json!({"native": false, "network": "none"});
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = || it.next().cloned().ok_or_else(usage);
        match a.as_str() {
            "--native" => grants["native"] = json!(true),
            "--network" => {
                let v = value()?;
                grants["network"] = match v.as_str() {
                    "none" | "all" => json!(v),
                    _ => json!({"hosts": list(&v)}),
                };
            }
            "--read" | "--write" => {
                let key = &a[2..];
                grants["fs"] = grants.get("fs").cloned().unwrap_or(json!({"read": [], "write": []}));
                grants["fs"][key] = json!(list(&value()?));
            }
            "--days" => days = value()?.parse().map_err(|_| usage())?,
            s if s.starts_with('-') || name.is_some() => return Err(usage()),
            s => name = Some(s.to_string()),
        }
    }
    let name = name.ok_or_else(usage)?;
    super::install::check_name(&name).map_err(|e| err("bad_input", &e))?;
    if let Some(hosts) = grants["network"]["hosts"].as_array() {
        for h in hosts.iter().filter_map(Value::as_str) {
            zazz_format::manifest::check_host(h).map_err(|e| err("bad_input", &e))?;
        }
    }
    Ok((name, grants, days))
}

// Writes a private file (0600) that must not exist yet.
fn write_private(p: &Path, data: &[u8]) -> Result<(), Value> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(p)
        .map_err(|e| err("io", &format!("{}: {e}", p.display())))?;
    f.write_all(data).map_err(|e| err("io", &format!("{}: {e}", p.display())))
}

// Runs `zazz keygen`: a new P-256 key and a self-signed authority certificate carrying the grants.
pub fn keygen(z: &Zazz, args: &[String]) -> Result<Value, Value> {
    let (name, grants, days) = keygen_args(args)?;
    let dir = z.keys_dir();
    std::fs::create_dir_all(&dir).map_err(|e| err("io", &format!("{}: {e}", dir.display())))?;
    let _ = std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700));
    let (key_path, cert_path) = (dir.join(format!("{name}.key")), dir.join(format!("{name}.crt")));
    if key_path.exists() {
        return Err(err("exists", &format!("{} already exists", key_path.display())));
    }
    let k = crypto::generate_key();
    let cert = crypto::self_signed(&k, &name, days, &grants).map_err(|e| err("io", &e))?;
    write_private(&key_path, crypto::key_pem(&k).map_err(|e| err("io", &e))?.as_bytes())?;
    std::fs::write(&cert_path, crypto::cert_pem(&cert).map_err(|e| err("io", &e))?)
        .map_err(|e| err("io", &e.to_string()))?;
    Ok(json!({
        "key": key_path.to_string_lossy(), "cert": cert_path.to_string_lossy(), "grants": grants,
        "next": format!("trust it on a machine with: sudo zazz trust add {}", cert_path.display()),
    }))
}

// Finds the one key in the keys directory when none is named.
fn pick_key(dir: &Path, name: Option<String>) -> Result<String, Value> {
    if let Some(n) = name {
        return Ok(n);
    }
    let keys: Vec<String> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| e.file_name().to_str()?.strip_suffix(".key").map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    match keys.as_slice() {
        [one] => Ok(one.clone()),
        [] => Err(err("no_key", "no signing key; make one with `zazz keygen NAME`")),
        _ => Err(err("usage", "several keys exist; choose one with --key NAME")),
    }
}

// Runs `zazz sign FILE [--key NAME] [-o OUT]`: writes a copy of the archive with zazz.sig.
pub fn sign(z: &Zazz, cwd: &Path, args: &[String]) -> Result<Value, Value> {
    let usage = || err("usage", "usage: zazz sign FILE.zazz [--key NAME] [-o OUT]");
    let (mut file, mut key, mut out) = (None, None, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--key" => key = Some(it.next().cloned().ok_or_else(usage)?),
            "-o" => out = Some(it.next().cloned().ok_or_else(usage)?),
            s if s.starts_with('-') || file.is_some() => return Err(usage()),
            s => file = Some(s.to_string()),
        }
    }
    let file = cwd.join(file.ok_or_else(usage)?);
    let bytes = super::add::read_once(&file)?;
    let a = zip::read(&bytes, &Limits::DEFAULT).map_err(|e| err("invalid_archive", &e))?;
    zazz_format::manifest::Manifest::of(&a).map_err(|e| err("invalid_manifest", &e))?;
    let dir = z.keys_dir();
    let name = pick_key(&dir, key)?;
    let pem = std::fs::read_to_string(dir.join(format!("{name}.key")))
        .map_err(|e| err("no_key", &format!("{name}.key: {e}")))?;
    let k = crypto::key_from_pem(&pem).map_err(|e| err("no_key", &e))?;
    let chain = crypto::certs_from(
        &std::fs::read(dir.join(format!("{name}.crt")))
            .map_err(|e| err("no_key", &format!("{name}.crt: {e}")))?,
    )
    .map_err(|e| err("no_key", &e))?;
    let digest = sig::digest(&a);
    let env = Envelope { sig: crypto::sign(&k, &sig::message(&digest)), digest, chain };
    let signed = zip::write(&sig::with_envelope(&a, &env)).map_err(|e| err("invalid_archive", &e))?;
    let target =
        super::output_path(cwd, &out.map(PathBuf::from).unwrap_or(file)).map_err(|e| err("io", &e))?;
    super::write_new(&target, &signed).map_err(|e| err("io", &e))?;
    Ok(
        json!({"path": target.to_string_lossy(), "hash": zazz_format::script_id(&signed), "signer": crypto::subject(&env.chain[0])}),
    )
}

// Checks a signed archive: the envelope matches, the signature verifies, and returns the envelope (None when unsigned).
pub fn check_signature(a: &zip::Archive) -> Result<Option<Envelope>, Value> {
    let Some(env) = Envelope::of(a).map_err(|e| err("invalid_signature", &e))? else { return Ok(None) };
    crypto::verify(&env.chain[0], &sig::message(&env.digest), &env.sig)
        .map_err(|e| err("invalid_signature", &e))?;
    Ok(Some(env))
}

// Runs `zazz trust add CERT`, `zazz trust ls` and `zazz trust rm NAME`.
pub fn trust_cmd(z: &Zazz, cwd: &Path, args: &[String]) -> Result<Value, Value> {
    let (dir, _) = z.trust_dir();
    match args {
        [cmd] if cmd == "ls" => Ok(Value::Array(
            trust::anchors(z)
                .iter()
                .map(|c| json!({"authority": crypto::subject(c), "grants": crypto::grants_of(c)}))
                .collect(),
        )),
        [cmd, cert] if cmd == "add" => {
            let data = std::fs::read(cwd.join(cert)).map_err(|e| err("io", &format!("{cert}: {e}")))?;
            let certs = crypto::certs_from(&data).map_err(|e| err("bad_input", &e))?;
            add_to_os(z, &certs[0])?;
            let name = Path::new(cert)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "authority".into());
            let target = dir.join(format!("{name}.pem"));
            std::fs::create_dir_all(&dir)
                .and_then(|()| std::fs::write(&target, crypto::cert_pem(&certs[0]).unwrap_or_default()))
                .map_err(|e| {
                    err(
                        "permission",
                        &format!(
                            "{}: {e} (trusting an authority needs root: sudo zazz trust add {cert})",
                            target.display()
                        ),
                    )
                })?;
            Ok(json!({"trusted": crypto::subject(&certs[0]), "path": target.to_string_lossy()}))
        }
        [cmd, name] if cmd == "rm" => {
            let target = dir.join(format!("{name}.pem"));
            std::fs::remove_file(&target)
                .map_err(|e| err("permission", &format!("{}: {e}", target.display())))?;
            Ok(json!({"removed": target.to_string_lossy()}))
        }
        _ => Err(err("usage", "usage: zazz trust ls | add CERT | rm NAME")),
    }
}

// Also trusts the authority in the Keychain on macOS (macOS asks the human to authenticate).
#[cfg(target_os = "macos")]
fn add_to_os(z: &Zazz, der: &[u8]) -> Result<(), Value> {
    use security_framework::certificate::SecCertificate;
    use security_framework::trust_settings::{Domain, TrustSettings};
    if !z.uses_os_store() {
        return Ok(());
    }
    let c = SecCertificate::from_der(der).map_err(|e| err("bad_input", &e.to_string()))?;
    TrustSettings::new(Domain::User)
        .set_trust_settings_always(&c)
        .map_err(|e| err("permission", &format!("Keychain: {e}")))
}

// Nothing to do outside macOS: the trust directory is the store.
#[cfg(not(target_os = "macos"))]
fn add_to_os(_z: &Zazz, _der: &[u8]) -> Result<(), Value> {
    Ok(())
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    // Turns string slices into owned arguments.
    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    // Checks keygen option parsing into grants.
    #[test]
    fn test_keygen_args() {
        let (n, g, d) = keygen_args(&v(&[
            "ci",
            "--native",
            "--network",
            "api.x.com,*.y.com",
            "--read",
            "$CWD",
            "--write",
            "$CWD/out",
            "--days",
            "7",
        ]))
        .expect("ok");
        assert_eq!((n.as_str(), d), ("ci", 7));
        assert_eq!(
            g,
            json!({"native": true, "network": {"hosts": ["api.x.com", "*.y.com"]}, "fs": {"read": ["$CWD"], "write": ["$CWD/out"]}})
        );
        assert_eq!(keygen_args(&v(&["ci", "--network", "all"])).expect("all").1["network"], "all");
        for bad in [
            &["--native"][..],
            &["ci", "--days", "x"],
            &["ci", "x"],
            &["ci", "--bogus"],
            &["Bad"],
            &["ci", "--network", "1.2.3.4"],
            &["ci", "--days"],
        ] {
            assert!(keygen_args(&v(bad)).is_err(), "{bad:?}");
        }
        assert_eq!(list(" a, ,b "), vec!["a", "b"]);
    }

    // Checks the whole flow: keygen, sign, add untrusted, trust add, grants narrowing the network, and tampering refused.
    #[test]
    fn test_sign_and_trust_flow() {
        use crate::testutil::{run_cmd_with, zazz_engine, TempDir};
        let t = TempDir::new();
        let e = zazz_engine(&t);
        e.zazz.set_keys_dir(&t.path().join("keys"));
        e.zazz.set_trust_paths(&t.path().join("trust"), &t.path().join("policy.json"));
        let run = |args: &[&str]| {
            serde_json::from_str::<Value>(&run_cmd_with(&e, t.path(), "", args).text).expect("json")
        };
        assert_eq!(run(&["zazz", "sign", "w.zazz"])["error"]["code"], "unreadable");
        let made = run(&["zazz", "keygen", "ci", "--network", "api.x.com"]);
        assert!(made["cert"].as_str().expect("cert").ends_with("ci.crt"), "{made}");
        assert_eq!(run(&["zazz", "keygen", "ci"])["error"]["code"], "exists");
        let wasm =
            wat::parse_str(r#"(module (memory (export "memory") 1) (func (export "_start")))"#).expect("wat");
        t.write("w/zazz.json", br#"{"zazz":1,"name":"w","kind":"wasm","entry":"w.wasm","sandbox":{"network":{"hosts":["api.x.com","evil.com"]}}}"#);
        t.write("w/w.wasm", &wasm);
        run(&["zazz", "pack", "w"]);
        assert_eq!(run(&["zazz", "sign", "w.zazz", "--key", "nope"])["error"]["code"], "no_key");
        let signed = run(&["zazz", "sign", "w.zazz", "-o", "w.signed.zazz"]);
        assert!(signed["signer"].as_str().expect("signer").contains("CN=ci"), "{signed}");
        let added = run(&["zazz", "add", "w.signed.zazz"]);
        assert_eq!(
            (added["trusted"].as_bool(), added["warnings"].as_array().map(Vec::len)),
            (Some(false), Some(2)),
            "{added}"
        );
        let cert = made["cert"].as_str().expect("cert").to_string();
        assert!(run(&["zazz", "trust", "add", &cert])["trusted"]
            .as_str()
            .expect("trusted")
            .contains("CN=ci"));
        assert_eq!(run(&["zazz", "trust", "ls"])[0]["grants"]["network"], json!({"hosts": ["api.x.com"]}));
        let chain = crypto::certs_from(&std::fs::read(&cert).expect("cert")).expect("chain");
        let verdict = trust::evaluate(&e.zazz, &chain);
        let m = zazz_format::manifest::Manifest::parse(br#"{"zazz":1,"name":"w","kind":"wasm","entry":"w.wasm","sandbox":{"network":{"hosts":["api.x.com","evil.com"]}}}"#).expect("m");
        assert_eq!(super::super::call::granted(&e.zazz, &m, &verdict).0, json!({"hosts": ["api.x.com"]}));
        assert_eq!(super::super::call::granted(&e.zazz, &m, &json!({})).0, json!("none"));
        let a = zip::read(&std::fs::read(t.path().join("w.signed.zazz")).expect("read"), &Limits::DEFAULT)
            .expect("zip");
        let mut entries = a.entries.clone();
        entries.iter_mut().find(|x| x.name == "w.wasm").expect("wasm").data.push(0);
        t.write("tampered.zazz", &zip::write(&zip::Archive::new(entries)).expect("write"));
        assert_eq!(run(&["zazz", "add", "tampered.zazz"])["error"]["code"], "invalid_signature");
        assert!(run(&["zazz", "trust", "rm", "ci"])["removed"].is_string());
        assert_eq!(run(&["zazz", "trust", "rm", "ci"])["error"]["code"], "permission");
        assert_eq!(run(&["zazz", "trust"])["error"]["code"], "usage");
        assert_eq!(run(&["zazz", "trust", "add", "missing.crt"])["error"]["code"], "io");
        assert_eq!(run(&["zazz", "sign"])["error"]["code"], "usage");
        assert_eq!(run(&["zazz", "sign", "w.zazz", "--key"])["error"]["code"], "usage");
        assert_eq!(run(&["zazz", "sign", "w.zazz", "x.zazz"])["error"]["code"], "usage");
        assert_eq!(run(&["zazz", "sign", "--bogus"])["error"]["code"], "usage");
        assert_eq!(run(&["zazz", "trust", "bogus"])["error"]["code"], "usage");
        assert_eq!(run(&["zazz", "trust", "add", "a", "b"])["error"]["code"], "usage");
        assert_eq!(run(&["zazz", "trust", "x", "y"])["error"]["code"], "usage");
    }

    // Checks the key choice when none is named.
    #[test]
    fn test_pick_key() {
        let t = crate::testutil::TempDir::new();
        assert_eq!(pick_key(t.path(), Some("x".into())).expect("named"), "x");
        assert_eq!(pick_key(t.path(), None).unwrap_err()["code"], "no_key");
        t.write("a.key", b"");
        assert_eq!(pick_key(t.path(), None).expect("one"), "a");
        t.write("b.key", b"");
        assert_eq!(pick_key(t.path(), None).unwrap_err()["code"], "usage");
        assert!(write_private(&t.path().join("a.key"), b"").is_err());
    }
}
