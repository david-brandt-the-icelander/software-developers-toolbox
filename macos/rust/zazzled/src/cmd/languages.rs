// The languages command: lists the language modules this zazzled trusts, turns downloading on first use (JIT) on or off, and downloads the modules not yet installed.
use super::{Ctx, Output};
use crate::modules::{self, Store};

const USAGE: &str = "usage: languages [jit [on|off] | fetch [NAME..]]";

// Runs `languages`, `languages jit [on|off]` or `languages fetch [NAME..]`.
pub fn run(_ctx: &Ctx, args: &[String]) -> Result<Output, String> {
    let store = Store::from_env().ok_or("no home directory for the language modules")?;
    run_with(&store, args).map(Output::plain)
}

// Runs the command against a module store.
pub fn run_with(store: &Store, args: &[String]) -> Result<String, String> {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args[..] {
        [] => Ok(list(store)),
        ["jit"] => Ok(format!("jit {}", if store.jit() { "on" } else { "off" })),
        ["jit", v @ ("on" | "off")] => {
            store.set_jit(v == "on")?;
            modules::forget_failures();
            Ok(format!("jit {v}: {}", jit_note(v == "on")))
        }
        ["fetch", ..] => fetch(store, &args[1..]),
        _ => Err(USAGE.into()),
    }
}

// Says what a JIT setting means.
fn jit_note(on: bool) -> &'static str {
    if on {
        "a missing module downloads the first time a file needs it"
    } else {
        "nothing downloads until `zazzle languages fetch`; files in missing languages read as source"
    }
}

// Lists the modules with their state, the JIT setting, the directory and the download source.
fn list(store: &Store) -> String {
    if store.entries.is_empty() {
        return "this zazzled carries no language module manifest (a development build): only the built-in grammars \
                (C, C#, JavaScript, Python, Rust, TypeScript) are available"
            .into();
    }
    let installed: Vec<&str> =
        store.entries.iter().filter(|e| store.installed(e)).map(|e| e.name.as_str()).collect();
    let missing: Vec<&str> =
        store.entries.iter().filter(|e| !store.installed(e)).map(|e| e.name.as_str()).collect();
    let join = |v: &[&str]| if v.is_empty() { "none".to_string() } else { v.join(" ") };
    format!(
        "jit {} ({})\ninstalled ({}): {}\nmissing ({}): {}\ndirectory {}\nsource {}",
        if store.jit() { "on" } else { "off" },
        jit_note(store.jit()),
        installed.len(),
        join(&installed),
        missing.len(),
        join(&missing),
        store.dir.display(),
        store.base
    )
}

// Downloads the named modules, or all, that are not installed; fails if any download fails.
fn fetch(store: &Store, names: &[&str]) -> Result<String, String> {
    if store.entries.is_empty() {
        return Err("this zazzled carries no language module manifest (a development build), so it has nothing to fetch".into());
    }
    let names: Vec<String> = names.iter().map(|n| n.to_string()).collect();
    let results = store.fetch_missing(&names);
    modules::forget_failures();
    let got: Vec<&str> = results.iter().filter(|r| matches!(r.1, Ok(true))).map(|r| r.0.as_str()).collect();
    let had = results.iter().filter(|r| matches!(r.1, Ok(false))).count();
    let failed: Vec<String> =
        results.iter().filter_map(|(n, r)| r.as_ref().err().map(|e| format!("{n}: {e}"))).collect();
    let mut out = format!(
        "downloaded {}: {}; already installed {had}",
        got.len(),
        if got.is_empty() { "none".into() } else { got.join(" ") }
    );
    if failed.is_empty() {
        Ok(out)
    } else {
        out.push_str(&format!("\nfailed {}:\n{}", failed.len(), failed.join("\n")));
        Err(out)
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::modules::Entry;
    use crate::testutil::TempDir;

    // Runs the command with words.
    fn go(store: &Store, args: &[&str]) -> Result<String, String> {
        run_with(store, &args.iter().map(|a| a.to_string()).collect::<Vec<_>>())
    }

    // Checks listing, the JIT setting, a fetch that fails, usage errors and a development build with no manifest.
    #[test]
    fn test_languages_command() {
        let t = TempDir::new();
        let entries = vec![Entry { name: "lua".into(), sha256: "00".into(), file: "lua.so".into() }];
        let store = Store { entries, dir: t.path().join("languages"), base: "http://127.0.0.1:9".into() };
        let list = go(&store, &[]).expect("list");
        assert!(
            list.starts_with("jit on (") && list.contains("\ninstalled (0): none\nmissing (1): lua\n"),
            "{list}"
        );
        assert_eq!(go(&store, &["jit"]).expect("jit"), "jit on");
        assert!(go(&store, &["jit", "off"]).expect("off").starts_with("jit off: nothing downloads"));
        assert!(go(&store, &[]).expect("list").starts_with("jit off ("));
        assert!(go(&store, &["jit", "on"]).expect("on").starts_with("jit on: a missing module"));
        let failed = go(&store, &["fetch", "lua", "cobol"]).expect_err("fetch");
        assert!(
            failed.starts_with("downloaded 0: none; already installed 0\nfailed 2:\nlua: downloading"),
            "{failed}"
        );
        assert_eq!(go(&store, &["jit", "maybe"]).expect_err("usage"), USAGE);
        let dev = Store { entries: Vec::new(), dir: t.path().join("dev"), base: String::new() };
        assert!(go(&dev, &[]).expect("dev").contains("development build"));
        assert!(go(&dev, &["fetch"]).expect_err("dev fetch").contains("nothing to fetch"));
    }

    // Checks a successful fetch reports what it downloaded.
    #[test]
    fn test_languages_fetch() {
        let bytes = std::fs::read(crate::testutil::lua_module()).expect("module");
        let t = TempDir::new();
        let sha: String = {
            use sha2::Digest;
            sha2::Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect()
        };
        let entries = vec![Entry { name: "lua".into(), sha256: sha, file: "lua.so".into() }];
        let store = Store { entries, dir: t.path().join("languages"), base: "unused".into() };
        store.store(&store.entries[0], &bytes).expect("store");
        assert_eq!(go(&store, &["fetch"]).expect("fetch"), "downloaded 0: none; already installed 1");
        assert!(go(&store, &[]).expect("list").contains("installed (1): lua\nmissing (0): none"));
    }
}
