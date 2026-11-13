// Command-line entry for the zazzled binary: serve, mcp, install, uninstall and in-process run.
use crate::client;
use crate::cmd::{self, Ctx};
use crate::engine::Engine;
use crate::launchd;
use crate::mcp::{self, DaemonBackend};
use crate::proxy::{self, Upstream};
use crate::server::{self, Config};
use crate::store::{self, Store};
use std::cell::RefCell;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

const USAGE: &str = "usage: zazzled serve [--socket PATH] [--idle SECS] | mcp | proxy .. | install | uninstall | run CMD.. | version";
const DEFAULT_IDLE_SECS: u64 = 1800;

// What the binary was asked to do.
#[derive(Debug, PartialEq)]
pub enum Cli {
    Serve { socket: Option<PathBuf>, idle: Duration },
    Mcp,
    Install,
    Uninstall,
    Run(Vec<String>),
    Proxy(proxy::Opts),
    Version,
}

// Parses the serve options.
fn parse_serve(rest: &[String]) -> Result<Cli, String> {
    let env_idle = std::env::var("ZAZZLE_IDLE_SECS").ok().and_then(|v| v.parse().ok());
    let (mut socket, mut idle) = (None, env_idle.unwrap_or(DEFAULT_IDLE_SECS));
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        let mut value = |name: &str| it.next().cloned().ok_or_else(|| format!("{name} needs a value"));
        match a.as_str() {
            "--socket" => socket = Some(PathBuf::from(value("--socket")?)),
            "--idle" => {
                idle = value("--idle")?.parse().map_err(|_| "--idle needs a number of seconds".to_string())?
            }
            other => return Err(format!("unknown serve option '{other}'")),
        }
    }
    Ok(Cli::Serve { socket, idle: Duration::from_secs(idle) })
}

// Parses the command line.
pub fn parse(args: &[String]) -> Result<Cli, String> {
    let Some((cmd, rest)) = args.split_first() else { return Err(USAGE.into()) };
    match cmd.as_str() {
        "serve" => parse_serve(rest),
        "mcp" => Ok(Cli::Mcp),
        "install" => Ok(Cli::Install),
        "uninstall" => Ok(Cli::Uninstall),
        "run" => Ok(Cli::Run(rest.to_vec())),
        "proxy" => proxy::parse(rest).map(Cli::Proxy),
        "version" | "--version" => Ok(Cli::Version),
        _ => Err(USAGE.into()),
    }
}

// Runs one command in this process and writes its output; returns the exit status.
pub fn run_in_process(
    engine: &Engine,
    cwd: &Path,
    session: &str,
    args: &[String],
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> u8 {
    let ctx = Ctx {
        engine,
        cwd: cwd.to_path_buf(),
        session: session.to_string(),
        stdin: RefCell::new(Some(Box::new(std::io::stdin()))),
    };
    let o = cmd::run(&ctx, args);
    if o.status == 0 {
        let _ = writeln!(out, "{}", o.text);
    } else {
        let _ = writeln!(err, "{}", o.text);
    }
    o.status
}

// Builds the engine with the persistent cache, or without it if the cache cannot be opened.
fn open_engine() -> Engine {
    match store::default_dir().and_then(|d| Store::open(&d)) {
        Ok(s) => Engine::with_store(s),
        Err(e) => {
            eprintln!("zazzled: cache disabled: {e}");
            Engine::new()
        }
    }
}

// Binds the socket, serves until idle and removes the socket afterwards.
pub fn serve_main(socket: Option<PathBuf>, idle: Duration) -> Result<(), String> {
    let io = |e: std::io::Error| e.to_string();
    let (listener, bound) = match socket {
        Some(p) => (server::bind(&p).map_err(io)?, Some(p)),
        None => match launchd::activated_listener().map_err(io)? {
            Some(l) => (l, None),
            None => {
                let p = client::socket_path().map_err(io)?;
                (server::bind(&p).map_err(io)?, Some(p))
            }
        },
    };
    let result = server::serve(listener, Arc::new(open_engine()), &Config { idle }, &AtomicBool::new(false));
    bound.iter().for_each(|p| {
        let _ = std::fs::remove_file(p);
    });
    result.map_err(io)
}

// Serves MCP on standard input and output.
fn mcp_main() -> Result<(), String> {
    let sock = client::socket_path().map_err(|e| e.to_string())?;
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?.display().to_string();
    let backend = DaemonBackend { sock, cwd, session: mcp::new_session_id() };
    mcp::serve(&mut std::io::stdin().lock(), &mut std::io::stdout().lock(), &backend)
        .map_err(|e| e.to_string())
}

// Installs or removes the LaunchAgent; only meaningful on macOS.
pub fn agent_cmd(
    install: bool,
    is_macos: bool,
    exe: &Path,
    sock: &Path,
    home: &Path,
    uid: u32,
    run: &dyn Fn(&[&str]) -> Result<(), String>,
) -> Result<String, String> {
    if !is_macos {
        return Err("install is for macOS launchd; elsewhere the invoker starts zazzled on demand".into());
    }
    if install {
        launchd::install(exe, sock, home, uid, run)
    } else {
        launchd::uninstall(home, uid, run)
    }
}

// Installs or removes the LaunchAgent using the real environment.
fn agent_main(install: bool) -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let sock = client::socket_path().map_err(|e| e.to_string())?;
    let home = PathBuf::from(std::env::var_os("HOME").ok_or("HOME is not set")?);
    // SAFETY: getuid has no preconditions.
    let uid = unsafe { libc::getuid() };
    agent_cmd(install, cfg!(target_os = "macos"), &exe, &sock, &home, uid, &launchd::launchctl)
}

// Reports a result on the right stream and picks the exit code.
fn finish(r: Result<String, String>) -> ExitCode {
    match r {
        Ok(msg) => {
            if !msg.is_empty() {
                println!("{msg}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("zazzled: {e}");
            ExitCode::from(2)
        }
    }
}

// Runs the binary.
pub fn main(args: Vec<String>) -> ExitCode {
    let cli = match parse(&args) {
        Ok(c) => c,
        Err(e) => return finish(Err(e)),
    };
    match cli {
        Cli::Serve { socket, idle } => finish(serve_main(socket, idle).map(|()| String::new())),
        Cli::Mcp => finish(mcp_main().map(|()| String::new())),
        Cli::Install => finish(agent_main(true)),
        Cli::Uninstall => finish(agent_main(false)),
        Cli::Version => finish(Ok(format!("zazzled {}", cmd::VERSION))),
        Cli::Proxy(o) => match o.upstream {
            Upstream::Command(c) => match proxy::stdio::run(&c, &o.filter) {
                Ok(code) => ExitCode::from(code.clamp(0, 255) as u8),
                Err(e) => finish(Err(e)),
            },
            Upstream::Http { url, listen } => {
                finish(proxy::http::run(&url, &listen, &o.filter).map(|()| String::new()))
            }
        },
        Cli::Run(rest) => {
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
            let session = std::env::var("ZAZZLE_SESSION").unwrap_or_default();
            let status = run_in_process(
                &Engine::new(),
                &cwd,
                &session,
                &rest,
                &mut std::io::stdout(),
                &mut std::io::stderr(),
            );
            ExitCode::from(status)
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    // Converts string slices into owned arguments.
    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    // Checks every subcommand parses.
    #[test]
    fn test_parse_subcommands() {
        let _g = crate::testutil::env_lock();
        assert_eq!(parse(&v(&["mcp"])), Ok(Cli::Mcp));
        assert_eq!(parse(&v(&["install"])), Ok(Cli::Install));
        assert_eq!(parse(&v(&["uninstall"])), Ok(Cli::Uninstall));
        assert_eq!(parse(&v(&["version"])), Ok(Cli::Version));
        assert_eq!(parse(&v(&["--version"])), Ok(Cli::Version));
        assert_eq!(parse(&v(&["run", "find", "x"])), Ok(Cli::Run(v(&["find", "x"]))));
        assert!(matches!(parse(&v(&["proxy", "--", "srv"])), Ok(Cli::Proxy(_))));
        assert!(parse(&v(&["proxy"])).unwrap_err().starts_with("give either"));
        assert_eq!(parse(&v(&["serve"])), Ok(Cli::Serve { socket: None, idle: Duration::from_secs(1800) }));
    }

    // Checks the serve options and their errors.
    #[test]
    fn test_parse_serve_options() {
        let _g = crate::testutil::env_lock();
        let c = parse(&v(&["serve", "--socket", "/s", "--idle", "5"])).expect("parse");
        assert_eq!(c, Cli::Serve { socket: Some(PathBuf::from("/s")), idle: Duration::from_secs(5) });
        assert_eq!(parse(&v(&["serve", "--socket"])).unwrap_err(), "--socket needs a value");
        assert_eq!(parse(&v(&["serve", "--idle", "x"])).unwrap_err(), "--idle needs a number of seconds");
        assert_eq!(parse(&v(&["serve", "--bad"])).unwrap_err(), "unknown serve option '--bad'");
    }

    // Checks ZAZZLE_IDLE_SECS sets the default idle time and bad values are ignored.
    #[test]
    fn test_idle_default_from_environment() {
        let _g = crate::testutil::env_lock();
        std::env::set_var("ZAZZLE_IDLE_SECS", "7");
        assert_eq!(parse(&v(&["serve"])), Ok(Cli::Serve { socket: None, idle: Duration::from_secs(7) }));
        assert_eq!(
            parse(&v(&["serve", "--idle", "9"])),
            Ok(Cli::Serve { socket: None, idle: Duration::from_secs(9) })
        );
        std::env::set_var("ZAZZLE_IDLE_SECS", "soon");
        assert_eq!(parse(&v(&["serve"])), Ok(Cli::Serve { socket: None, idle: Duration::from_secs(1800) }));
        std::env::remove_var("ZAZZLE_IDLE_SECS");
    }

    // Checks usage errors for no command and unknown commands.
    #[test]
    fn test_parse_usage_errors() {
        assert_eq!(parse(&[]).unwrap_err(), USAGE);
        assert_eq!(parse(&v(&["bogus"])).unwrap_err(), USAGE);
    }

    // Checks in-process runs write to the right stream and return the status.
    #[test]
    fn test_run_in_process_streams() {
        let t = TempDir::new();
        t.write("a.txt", b"hi\n");
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let e = Engine::new();
        assert_eq!(run_in_process(&e, t.path(), "", &v(&["read", "a.txt"]), &mut out, &mut err), 0);
        assert!(String::from_utf8_lossy(&out).contains("1\thi") && err.is_empty());
        let (mut out, mut err) = (Vec::new(), Vec::new());
        assert_eq!(run_in_process(&e, t.path(), "", &v(&["nope"]), &mut out, &mut err), 2);
        assert!(out.is_empty() && String::from_utf8_lossy(&err).starts_with("error: unknown command"));
    }

    // Checks serve_main binds, serves, idles out and removes its socket.
    #[test]
    fn test_serve_main_lifecycle() {
        let _g = crate::testutil::env_lock();
        let dir = TempDir::new();
        std::env::set_var("ZAZZLE_CACHE_DIR", dir.path().join("cache"));
        let sock = dir.path().join("s");
        serve_main(Some(sock.clone()), Duration::from_millis(200)).expect("serve");
        std::env::remove_var("ZAZZLE_CACHE_DIR");
        assert!(!sock.exists());
        assert!(dir.path().join("cache/paths.log").exists());
    }

    // Checks serve_main falls back to the default socket path when none is given.
    #[test]
    fn test_serve_main_default_socket() {
        let _g = crate::testutil::env_lock();
        let dir = TempDir::new();
        let sock = dir.path().join("d/sock");
        std::env::set_var("ZAZZLE_SOCK", &sock);
        std::env::set_var("ZAZZLE_CACHE_DIR", dir.path().join("cache"));
        let r = serve_main(None, Duration::from_millis(200));
        std::env::remove_var("ZAZZLE_SOCK");
        std::env::remove_var("ZAZZLE_CACHE_DIR");
        r.expect("serve");
        assert!(!sock.exists());
    }

    // Checks serve_main reports a bind failure.
    #[test]
    fn test_serve_main_bind_error() {
        let _g = crate::testutil::env_lock();
        let dir = TempDir::new();
        std::env::set_var("ZAZZLE_CACHE_DIR", dir.path().join("cache"));
        let file = dir.write("f", b"x");
        let r = serve_main(Some(file.join("sub/s")), Duration::from_millis(100));
        std::env::remove_var("ZAZZLE_CACHE_DIR");
        assert!(r.is_err());
    }

    // Checks the daemon runs without a cache when the cache directory cannot be created.
    #[test]
    fn test_open_engine_without_cache() {
        let _g = crate::testutil::env_lock();
        let dir = TempDir::new();
        let file = dir.write("f", b"x");
        std::env::set_var("ZAZZLE_CACHE_DIR", file.join("sub"));
        let e = open_engine();
        std::env::remove_var("ZAZZLE_CACHE_DIR");
        assert_eq!(e.stored_records(), 0);
    }

    // Checks the agent command refuses off macOS and delegates on macOS.
    #[test]
    fn test_agent_cmd() {
        let home = TempDir::new();
        let ok = |_: &[&str]| Ok(());
        let (exe, sock) = (Path::new("/b/zazzled"), Path::new("/s"));
        assert!(agent_cmd(true, false, exe, sock, home.path(), 1, &ok)
            .unwrap_err()
            .starts_with("install is for macOS"));
        assert!(agent_cmd(true, true, exe, sock, home.path(), 1, &ok)
            .expect("install")
            .starts_with("installed "));
        assert!(agent_cmd(false, true, exe, sock, home.path(), 1, &ok)
            .expect("uninstall")
            .starts_with("removed "));
    }

    // Checks finish maps results to exit codes.
    #[test]
    fn test_finish_codes() {
        assert_eq!(format!("{:?}", finish(Ok(String::new()))), format!("{:?}", ExitCode::SUCCESS));
        assert_eq!(format!("{:?}", finish(Ok("m".into()))), format!("{:?}", ExitCode::SUCCESS));
        assert_eq!(format!("{:?}", finish(Err("e".into()))), format!("{:?}", ExitCode::from(2)));
    }

    // Checks main handles version, usage errors and in-process runs.
    #[test]
    fn test_main_paths() {
        let ok = format!("{:?}", ExitCode::SUCCESS);
        assert_eq!(format!("{:?}", main(v(&["version"]))), ok);
        assert_eq!(format!("{:?}", main(v(&["bogus"]))), format!("{:?}", ExitCode::from(2)));
        assert_eq!(format!("{:?}", main(v(&["run", "version"]))), ok);
    }
}
