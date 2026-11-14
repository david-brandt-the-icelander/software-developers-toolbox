// Language modules: tree-sitter grammars shipped as shared libraries beside zazzled's release (icelander.net/downloads/zazzle/vX.Y.Z/languages/PLATFORM/), accepted only when their SHA-256 matches the manifest compiled into zazzled, downloaded on first use when JIT is on, and loaded with dlopen.
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use tree_sitter::Language;

const EMBEDDED: &str = include_str!(concat!(env!("OUT_DIR"), "/languages.manifest"));
const BASE_URL: &str = "https://icelander.net/downloads/zazzle";
const ABI: u32 = 1;
const MAX_MODULE: u64 = 64 << 20;
const RETRY: Duration = Duration::from_secs(60);
const TIMEOUT: Duration = Duration::from_secs(60);

// One module the manifest names: its language, the SHA-256 its file must have, and its file name.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub name: String,
    pub sha256: String,
    pub file: String,
}

// Where modules come from and go: the trusted manifest, the local directory and the download base URL.
pub struct Store {
    pub entries: Vec<Entry>,
    pub dir: PathBuf,
    pub base: String,
}

// Parses manifest lines `NAME SHA256 FILE`, skipping blank lines and `#` comments.
pub fn parse(text: &str) -> Vec<Entry> {
    text.lines()
        .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
        .filter_map(|l| {
            let mut w = l.split_whitespace();
            let (name, sha256, file) = (w.next()?, w.next()?, w.next()?);
            Some(Entry { name: name.into(), sha256: sha256.to_ascii_lowercase(), file: file.into() })
        })
        .collect()
}

// Returns the download directory name for this build's platform.
pub fn platform() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos-universal"
    } else if cfg!(target_arch = "aarch64") {
        "linux-aarch64"
    } else {
        "linux-x86_64"
    }
}

// Returns the lowercase hex SHA-256 of some bytes.
fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

impl Store {
    // The store this zazzled uses: the embedded manifest, ZAZZLE_LANGUAGES_DIR or ~/.zazzle/v1/languages, and ZAZZLE_LANGUAGES_URL or this release's languages directory.
    pub fn from_env() -> Option<Store> {
        let dir = match std::env::var_os("ZAZZLE_LANGUAGES_DIR") {
            Some(d) => PathBuf::from(d),
            None => crate::store::default_dir().ok()?.join("languages"),
        };
        let base = std::env::var("ZAZZLE_LANGUAGES_URL")
            .unwrap_or_else(|_| format!("{BASE_URL}/v{}/languages/{}", crate::cmd::VERSION, platform()));
        Some(Store { entries: parse(EMBEDDED), dir, base })
    }

    // True when modules download on first use: unless `jit` in the directory says `off`.
    pub fn jit(&self) -> bool {
        std::fs::read_to_string(self.dir.join("jit")).map_or(true, |s| s.trim() != "off")
    }

    // Turns downloading on first use on or off.
    pub fn set_jit(&self, on: bool) -> Result<(), String> {
        self.make_dir()?;
        std::fs::write(self.dir.join("jit"), if on { "on\n" } else { "off\n" })
            .map_err(|e| format!("writing {}: {e}", self.dir.join("jit").display()))
    }

    // Creates the module directory, private to the user.
    fn make_dir(&self) -> Result<(), String> {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.dir)
            .map_err(|e| format!("creating {}: {e}", self.dir.display()))
    }

    // Returns the manifest entry for a language.
    pub fn entry(&self, name: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.name == name)
    }

    // True when a module's file is present and has the manifest's SHA-256.
    pub fn installed(&self, e: &Entry) -> bool {
        std::fs::read(self.dir.join(&e.file)).is_ok_and(|b| sha256_hex(&b) == e.sha256)
    }

    // Downloads a module, checks its SHA-256 against the manifest and stores it atomically.
    pub fn fetch(&self, e: &Entry) -> Result<(), String> {
        let url = format!("{}/{}", self.base.trim_end_matches('/'), e.file);
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(TIMEOUT))
            .tls_config(
                ureq::tls::TlsConfig::builder().root_certs(ureq::tls::RootCerts::PlatformVerifier).build(),
            )
            .build()
            .into();
        let res = agent.get(&url).call().map_err(|err| format!("downloading {url}: {err}"))?;
        let mut bytes = Vec::new();
        res.into_body()
            .into_reader()
            .take(MAX_MODULE + 1)
            .read_to_end(&mut bytes)
            .map_err(|err| format!("downloading {url}: {err}"))?;
        if bytes.len() as u64 > MAX_MODULE {
            return Err(format!("{url} is larger than {MAX_MODULE} bytes"));
        }
        self.store(e, &bytes)
    }

    // Checks a module's bytes against the manifest and writes them into the directory through a temporary file.
    pub fn store(&self, e: &Entry, bytes: &[u8]) -> Result<(), String> {
        let got = sha256_hex(bytes);
        if got != e.sha256 {
            return Err(format!("{}: SHA-256 {got} does not match the manifest's {}", e.file, e.sha256));
        }
        self.make_dir()?;
        let (tmp, path) = (self.dir.join(format!(".{}.part", e.file)), self.dir.join(&e.file));
        std::fs::write(&tmp, bytes).map_err(|err| format!("writing {}: {err}", tmp.display()))?;
        std::fs::rename(&tmp, &path).map_err(|err| format!("installing {}: {err}", path.display()))
    }

    // Downloads every named module (all when none are named) that is not installed with the right SHA-256; returns each one's result.
    pub fn fetch_missing(&self, names: &[String]) -> Vec<(String, Result<bool, String>)> {
        let wanted = |e: &&Entry| names.is_empty() || names.contains(&e.name);
        let mut out: Vec<(String, Result<bool, String>)> = self
            .entries
            .iter()
            .filter(wanted)
            .map(|e| {
                (e.name.clone(), if self.installed(e) { Ok(false) } else { self.fetch(e).map(|()| true) })
            })
            .collect();
        for n in names.iter().filter(|n| self.entry(n).is_none()) {
            out.push((n.clone(), Err("no such language module in this build's manifest".into())));
        }
        out
    }

    // Loads a language's module, downloading it first when it is missing and JIT is on.
    pub fn load(&self, name: &str) -> Result<Language, String> {
        let e = self.entry(name).ok_or_else(|| format!("no {name} module in this build's manifest"))?;
        if !self.installed(e) {
            if !self.jit() {
                return Err(format!("the {name} module is not installed; run `zazzle languages fetch`"));
            }
            self.fetch(e)?;
        }
        let path = self.dir.join(&e.file);
        let bytes = std::fs::read(&path).map_err(|err| format!("reading {}: {err}", path.display()))?;
        if sha256_hex(&bytes) != e.sha256 {
            return Err(format!("{} changed after it was checked", path.display()));
        }
        open(&path)
    }
}

// Returns dlerror's message.
fn dl_error() -> String {
    // SAFETY: dlerror returns null or a NUL-terminated message owned by libc.
    let msg = unsafe { libc::dlerror() };
    if msg.is_null() {
        "unknown dlopen error".into()
    } else {
        // SAFETY: checked non-null above.
        unsafe { CStr::from_ptr(msg) }.to_string_lossy().into_owned()
    }
}

// Opens a module and returns its grammar after checking the module format and tree-sitter ABI versions; the library stays loaded.
pub fn open(path: &Path) -> Result<Language, String> {
    let c =
        CString::new(path.as_os_str().as_bytes()).map_err(|_| "module path holds a NUL byte".to_string())?;
    // SAFETY: dlopen takes a NUL-terminated path; the handle is kept for the life of the process.
    let handle = unsafe { libc::dlopen(c.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
    if handle.is_null() {
        return Err(format!("loading {}: {}", path.display(), dl_error()));
    }
    // SAFETY: dlsym takes the open handle and a NUL-terminated name.
    let abi = unsafe { libc::dlsym(handle, c"ZAZZLE_LANGUAGE_ABI".as_ptr()) } as *const u32;
    // SAFETY: the symbol, when present, is the module's static u32.
    if abi.is_null() || unsafe { *abi } != ABI {
        return Err(format!("{} is not a zazzle language module of format {ABI}", path.display()));
    }
    // SAFETY: as above.
    let sym = unsafe { libc::dlsym(handle, c"zazzle_language".as_ptr()) };
    if sym.is_null() {
        return Err(format!("{} has no zazzle_language function", path.display()));
    }
    // SAFETY: the module exports zazzle_language as `extern "C" fn() -> *const ()`.
    let f: unsafe extern "C" fn() -> *const () = unsafe { std::mem::transmute(sym) };
    // SAFETY: the function takes no arguments and returns a static pointer.
    if unsafe { f() }.is_null() {
        return Err(format!("{} carries no grammar", path.display()));
    }
    // SAFETY: f returns the module's static TSLanguage, which stays loaded.
    let lang = Language::new(unsafe { tree_sitter_language::LanguageFn::from_raw(f) });
    let v = lang.abi_version();
    if !(tree_sitter::MIN_COMPATIBLE_LANGUAGE_VERSION..=tree_sitter::LANGUAGE_VERSION).contains(&v) {
        return Err(format!("{} uses tree-sitter ABI {v}, which this zazzled cannot read", path.display()));
    }
    Ok(lang)
}

// Loaded grammars by language, and when a failed load may be tried again.
type Cache = HashMap<String, Result<Language, Instant>>;

// Returns the process-wide cache of loaded modules.
fn cache() -> &'static Mutex<Cache> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

// Returns a language's grammar from its module, loading it once per process; a failed load is retried after a minute.
pub fn language(name: &str) -> Option<Language> {
    let mut cache = cache().lock().unwrap_or_else(|p| p.into_inner());
    match cache.get(name) {
        Some(Ok(lang)) => return Some(lang.clone()),
        Some(Err(at)) if at.elapsed() < RETRY => return None,
        _ => {}
    }
    let loaded = Store::from_env().ok_or_else(String::new).and_then(|s| s.load(name));
    cache.insert(name.to_string(), loaded.clone().map_err(|_| Instant::now()));
    loaded.ok()
}

// Forgets failed loads, so the next read tries again (after `languages fetch` or turning JIT on).
pub fn forget_failures() {
    cache().lock().unwrap_or_else(|p| p.into_inner()).retain(|_, v| v.is_ok());
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::testutil::TempDir;
    use std::io::Write;
    use std::net::TcpListener;

    // Serves the same body to every request on a local port and returns the base URL; the thread ends with the test process.
    fn serve(body: Vec<u8>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut s = stream;
                let mut buf = [0u8; 4096];
                let _ = s.read(&mut buf);
                let head =
                    format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                let _ = s.write_all(head.as_bytes()).and_then(|()| s.write_all(&body));
            }
        });
        format!("http://{addr}/languages/test")
    }

    // A store in a temporary directory with one Lua entry of the given SHA-256.
    fn store(t: &TempDir, sha256: &str, base: &str) -> Store {
        let entries = vec![Entry { name: "lua".into(), sha256: sha256.into(), file: "lua.so".into() }];
        Store { entries, dir: t.path().join("languages"), base: base.into() }
    }

    // Checks manifest lines parse, comments and short lines are skipped, and the platform names a download directory.
    #[test]
    fn test_parse_and_platform() {
        let e = parse("# zazzle 1.0\n\nlua ABC lua.dylib\nbroken line\n");
        assert_eq!(e, vec![Entry { name: "lua".into(), sha256: "abc".into(), file: "lua.dylib".into() }]);
        assert!(["macos-universal", "linux-x86_64", "linux-aarch64"].contains(&platform()));
        assert!(EMBEDDED.is_empty() || !parse(EMBEDDED).is_empty());
    }

    // Checks JIT is on by default and the setting persists.
    #[test]
    fn test_jit_setting() {
        let t = TempDir::new();
        let s = store(&t, "x", "http://127.0.0.1:9");
        assert!(s.jit());
        s.set_jit(false).expect("off");
        assert!(!s.jit());
        s.set_jit(true).expect("on");
        assert!(s.jit());
        let blocked =
            Store { entries: Vec::new(), dir: t.path().join("languages/jit/x"), base: String::new() };
        assert!(blocked.set_jit(true).is_err());
    }

    // Checks a module downloads, its hash is checked, it loads and parses, and what fails: a wrong hash, JIT off, a non-module, a missing entry.
    #[test]
    fn test_fetch_and_load() {
        let bytes = std::fs::read(crate::testutil::lua_module()).expect("module");
        let sha = sha256_hex(&bytes);
        let t = TempDir::new();
        let s = store(&t, &sha, &serve(bytes.clone()));
        assert_eq!(s.fetch_missing(&[]), vec![("lua".to_string(), Ok(true))]);
        assert_eq!(s.fetch_missing(&["lua".into(), "cobol".into()]).len(), 2);
        let lang = s.load("lua").expect("load");
        let mut p = tree_sitter::Parser::new();
        p.set_language(&lang).expect("set");
        assert_eq!(p.parse("local x = 1", None).expect("parse").root_node().kind(), "chunk");
        assert!(s.load("cobol").is_err());

        let t2 = TempDir::new();
        let wrong = store(&t2, "00", &serve(bytes.clone()));
        assert!(wrong.load("lua").expect_err("hash").contains("does not match"));
        wrong.set_jit(false).expect("off");
        assert!(wrong.load("lua").expect_err("jit off").contains("not installed"));

        let t3 = TempDir::new();
        let junk = b"not a library".to_vec();
        let fake = store(&t3, &sha256_hex(&junk), &serve(junk));
        assert!(fake.load("lua").expect_err("not a library").contains("loading"));
        let unreachable = store(&TempDir::new(), &sha, "http://127.0.0.1:9");
        assert!(unreachable.fetch(&unreachable.entries[0]).is_err());
        assert!(open(Path::new("/nonexistent/x.so")).is_err());
        let libc = if cfg!(target_os = "macos") { "/usr/lib/libSystem.B.dylib" } else { "libc.so.6" };
        assert!(open(Path::new(libc)).expect_err("not a module").contains("is not a zazzle language module"));
    }

    // Checks the process-wide loader returns nothing for a language this build has no manifest entry for, and caches the failure.
    #[test]
    fn test_language_without_manifest() {
        let _g = crate::testutil::env_lock();
        let t = TempDir::new();
        std::env::set_var("ZAZZLE_LANGUAGES_DIR", t.path());
        std::env::set_var("ZAZZLE_LANGUAGES_URL", "http://127.0.0.1:9");
        assert!(language("no-such-module").is_none());
        assert!(language("no-such-module").is_none());
        forget_failures();
        let s = Store::from_env().expect("store");
        assert_eq!((s.dir.as_path(), s.base.as_str()), (t.path(), "http://127.0.0.1:9"));
        std::env::remove_var("ZAZZLE_LANGUAGES_DIR");
        std::env::remove_var("ZAZZLE_LANGUAGES_URL");
        assert!(Store::from_env().expect("default").base.ends_with(&format!("/languages/{}", platform())));
    }
}
