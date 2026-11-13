// Installing scripts as commands: names in the registry and shims (symlinks to the zazz invoker) in ~/.zazzle/bin.
use super::add;
use super::Zazz;
use crate::cmd::Output;
use std::path::{Path, PathBuf};

const RESERVED: [&str; 18] = [
    "zazz",
    "zazzle",
    "zazzled",
    "zazz-worker",
    "pack",
    "test",
    "add",
    "call",
    "run",
    "exec-name",
    "install",
    "uninstall",
    "list",
    "ls",
    "describe",
    "rm",
    "help",
    "new",
];

// A plain-text reply for humans.
fn text(s: String, status: u8) -> Output {
    Output { text: s, status, idable: false, raw: false }
}

// Checks an install name: lowercase letters, digits and dashes, not one of zazz's own words.
pub fn check_name(name: &str) -> Result<(), String> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name.starts_with(|c: char| c.is_ascii_lowercase())
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if !ok {
        return Err(format!("'{name}' is not a valid command name (lowercase letters, digits and '-')"));
    }
    if RESERVED.contains(&name) {
        return Err(format!("'{name}' is reserved by zazz"));
    }
    Ok(())
}

// Parses `PATH|ID [--as NAME]`.
fn parse(args: &[String]) -> Result<(String, Option<String>), String> {
    match args {
        [what] => Ok((what.clone(), None)),
        [what, flag, name] if flag == "--as" => Ok((what.clone(), Some(name.clone()))),
        _ => Err("usage: zazz install FILE.zazz|ID [--as NAME]".into()),
    }
}

// Points a shim at the invoker, replacing only an existing symlink.
fn make_shim(bin: &Path, name: &str, invoker: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(bin).map_err(|e| format!("{}: {e}", bin.display()))?;
    let shim = bin.join(name);
    match std::fs::symlink_metadata(&shim) {
        Ok(m) if !m.file_type().is_symlink() => {
            return Err(format!("{} exists and is not a zazz shim", shim.display()))
        }
        Ok(_) => std::fs::remove_file(&shim).map_err(|e| format!("{}: {e}", shim.display()))?,
        Err(_) => {}
    }
    std::os::unix::fs::symlink(invoker, &shim).map_err(|e| format!("{}: {e}", shim.display()))?;
    Ok(shim)
}

// Runs `zazz install`: adds the file if needed, records the name and creates the shim.
pub fn install(z: &Zazz, cwd: &Path, args: &[String]) -> Output {
    let (what, as_name) = match parse(args) {
        Ok(x) => x,
        Err(e) => return text(e, 2),
    };
    let reg = match z.registry() {
        Ok(r) => r,
        Err(e) => return text(format!("zazz install: {e}"), 1),
    };
    let hex = if cwd.join(&what).is_file() {
        match add::run(z, cwd, std::slice::from_ref(&what)) {
            Ok(v) => v["hash"].as_str().unwrap_or("").trim_start_matches("b3:").to_string(),
            Err(e) => {
                return text(
                    format!(
                        "zazz install: {}: {}",
                        e["code"].as_str().unwrap_or(""),
                        e["message"].as_str().unwrap_or("")
                    ),
                    1,
                )
            }
        }
    } else {
        match reg.resolve(&what) {
            Ok(h) => h,
            Err(e) => return text(format!("zazz install: {e}"), 1),
        }
    };
    let meta = reg.read_meta(&hex).unwrap_or_default();
    let name = as_name.unwrap_or_else(|| meta["name"].as_str().unwrap_or("").to_string());
    if let Err(e) = check_name(&name) {
        return text(format!("zazz install: {e}"), 2);
    }
    let invoker = match z.invoker() {
        Some(i) => i,
        None => return text("zazz install: the zazz invoker was not found beside zazzled".into(), 1),
    };
    if let Err(e) = reg.write_atomic(&reg.root.join("names").join(&name), hex.as_bytes()) {
        return text(format!("zazz install: {e}"), 1);
    }
    let bin = z.bin_dir();
    let shim = match make_shim(&bin, &name, &invoker) {
        Ok(s) => s,
        Err(e) => return text(format!("zazz install: {e}"), 1),
    };
    let hint = path_hint(&bin, std::env::var_os("PATH"));
    text(format!("installed {name} (b3:{}) as {}{hint}", &hex[..12], shim.display()), 0)
}

// A reminder to put the shim directory on PATH, when it is not there yet.
fn path_hint(bin: &Path, path: Option<std::ffi::OsString>) -> String {
    if path.is_some_and(|p| std::env::split_paths(&p).any(|d| d == bin)) {
        return String::new();
    }
    format!("\nadd it to PATH: export PATH=\"{}:$PATH\"", bin.display())
}

// Runs `zazz uninstall NAME`: removes the name and its shim (the script stays registered).
pub fn uninstall(z: &Zazz, args: &[String]) -> Output {
    let [name] = args else { return text("usage: zazz uninstall NAME".into(), 2) };
    let reg = match z.registry() {
        Ok(r) => r,
        Err(e) => return text(format!("zazz uninstall: {e}"), 1),
    };
    if check_name(name).is_err() || std::fs::remove_file(reg.root.join("names").join(name)).is_err() {
        return text(format!("zazz uninstall: '{name}' is not installed"), 1);
    }
    remove_shim(&z.bin_dir(), name);
    text(format!("uninstalled {name}"), 0)
}

// Removes a shim if it is a symlink.
fn remove_shim(bin: &Path, name: &str) {
    let shim = bin.join(name);
    if std::fs::symlink_metadata(&shim).is_ok_and(|m| m.file_type().is_symlink()) {
        let _ = std::fs::remove_file(shim);
    }
}

// Removes every installed name (and shim) that points at a script.
pub fn forget(z: &Zazz, hex: &str) {
    let Ok(reg) = z.registry() else { return };
    let Ok(rd) = std::fs::read_dir(reg.root.join("names")) else { return };
    for e in rd.flatten() {
        if std::fs::read_to_string(e.path()).is_ok_and(|t| t.trim() == hex) {
            let _ = std::fs::remove_file(e.path());
            remove_shim(&z.bin_dir(), &e.file_name().to_string_lossy());
        }
    }
}

// Runs `zazz list`: installed commands with their script ids.
pub fn list(z: &Zazz) -> Output {
    let Ok(reg) = z.registry() else { return text("zazz list: no registry".into(), 1) };
    let mut rows: Vec<String> = std::fs::read_dir(reg.root.join("names"))
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| {
                    let hex = std::fs::read_to_string(e.path()).ok()?;
                    let meta = reg.read_meta(hex.trim());
                    let version = meta
                        .as_ref()
                        .and_then(|m| m["version"].as_str().map(str::to_string))
                        .unwrap_or_else(|| "?".into());
                    Some(format!(
                        "{}  b3:{}  {version}",
                        e.file_name().to_string_lossy(),
                        hex.trim().get(..12).unwrap_or("")
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    rows.sort();
    if rows.is_empty() {
        return text("no commands installed; use `zazz install FILE.zazz`".into(), 0);
    }
    text(rows.join("\n"), 0)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    // Checks names: valid ones pass, reserved and malformed ones are refused.
    #[test]
    fn test_check_name() {
        assert!(check_name("csv-sum").is_ok() && check_name("a1").is_ok());
        for bad in ["", "Csv", "1a", "a_b", "zazz", "run", &"a".repeat(65)] {
            assert!(check_name(bad).is_err(), "{bad}");
        }
    }

    // Checks argument parsing.
    #[test]
    fn test_parse() {
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(parse(&v(&["x"])).expect("one"), ("x".to_string(), None));
        assert_eq!(parse(&v(&["x", "--as", "n"])).expect("as"), ("x".to_string(), Some("n".to_string())));
        assert!(parse(&v(&["x", "--to", "n"])).is_err() && parse(&v(&[])).is_err());
    }

    // Checks the PATH hint and registry failures.
    #[test]
    fn test_hint_and_broken_registry() {
        assert_eq!(path_hint(Path::new("/b"), Some("/x:/b".into())), "");
        assert!(path_hint(Path::new("/b"), None).contains("export PATH=\"/b:$PATH\""));
        let t = TempDir::new();
        let z = Zazz::default();
        z.set_root(&t.write("not-a-dir", b""));
        assert_eq!(list(&z).status, 1);
        assert_eq!(uninstall(&z, &["x".into()]).status, 1);
        assert_eq!(install(&z, t.path(), &["x".into()]).status, 1);
        forget(&z, "ab");
        assert_eq!(uninstall(&Zazz::default(), &["run".into()]).status, 1);
        let fresh = Zazz::default();
        fresh.set_root(&t.path().join("fresh"));
        forget(&fresh, "ab");
        t.write("fresh/names/other", b"cd");
        forget(&fresh, "ab");
        assert!(t.path().join("fresh/names/other").exists());
    }

    // Checks shims: created, replaced when a symlink, refused over a real file, removed only when a symlink.
    #[test]
    fn test_shims() {
        let t = TempDir::new();
        let bin = t.path().join("bin");
        let shim = make_shim(&bin, "s", Path::new("/usr/bin/zazz")).expect("shim");
        assert_eq!(std::fs::read_link(&shim).expect("link"), PathBuf::from("/usr/bin/zazz"));
        make_shim(&bin, "s", Path::new("/opt/zazz")).expect("replace");
        t.write("bin/real", b"x");
        assert!(make_shim(&bin, "real", Path::new("/opt/zazz")).unwrap_err().contains("not a zazz shim"));
        remove_shim(&bin, "real");
        assert!(bin.join("real").exists());
        remove_shim(&bin, "s");
        assert!(!bin.join("s").exists());
        std::fs::write(t.path().join("file"), "").expect("file");
        assert!(make_shim(&t.path().join("file"), "x", Path::new("/z")).is_err());
    }
}
