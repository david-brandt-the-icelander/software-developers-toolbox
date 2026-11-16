// `zazzle setup --check`: starts every configured MCP server the way its client would (same command, environment and folder) and does a real MCP handshake, so an agent can show the user what works and what to fix.
use super::clients::{Client, ConfigFile, Kind, Server};
use super::{is_zazzle, Env, Opts};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(20);
const STDERR_TAIL: usize = 300;

// One server's outcome: it works, it needs the user (such as an OAuth sign-in), or it failed with a reason.
#[derive(Debug, PartialEq)]
enum Outcome {
    Ok(String),
    Note(String),
    Fail(String),
}

// Checks each chosen client's servers and returns the report.
pub fn run(opts: &Opts, env: &Env) -> Result<String, String> {
    let chosen: Vec<Client> = if opts.clients.is_empty() {
        Client::ALL.into_iter().filter(|c| env.installed(*c)).collect()
    } else {
        opts.clients.clone()
    };
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    let mut out = String::from("zazzle setup --check: each server started the way its client starts it.\n");
    let (mut ok, mut failed) = (0, 0);
    let mut hints: Vec<&str> = Vec::new();
    for c in chosen {
        let file = ConfigFile::load(c, &env.config_path(c))?;
        out.push_str(&format!("\n{}: {}\n", c.name(), env.short(&file.path)));
        let servers = file.servers();
        if !servers.iter().any(is_zazzle) {
            failed += 1;
            out.push_str("  FAIL zazzle: not configured\n");
            hints.push("zazzle is missing from a client: run `zazzle setup`, then `zazzle setup --apply`.");
        }
        for s in &servers {
            let folder = folder(c, s, opts.repo.as_deref(), &cwd);
            let outcome = probe(s, &folder);
            let (word, text) = match &outcome {
                Outcome::Ok(t) => ("ok  ", t),
                Outcome::Note(t) => ("note", t),
                Outcome::Fail(t) => ("FAIL", t),
            };
            out.push_str(&format!("  {word} {}: {text}\n", s.name));
            match outcome {
                Outcome::Fail(t) => {
                    failed += 1;
                    if t.contains("cannot start") {
                        hints.push("A command did not start: apps started from the Dock do not see your shell's PATH, so give the command's absolute path.");
                    }
                }
                _ => ok += 1,
            }
        }
    }
    hints.dedup();
    out.push_str(&format!("\n{ok} working, {failed} failed.\n"));
    hints.iter().for_each(|h| out.push_str(&format!("{h}\n")));
    Ok(out)
}

// Returns the folder a client starts a stdio server in: `/` for the desktop app, the project for Claude Code's project servers, else --repo or the current folder.
fn folder(c: Client, s: &Server, repo: Option<&Path>, cwd: &Path) -> PathBuf {
    match (c, s.table.first().map(String::as_str)) {
        (Client::ClaudeDesktop, _) => PathBuf::from("/"),
        (Client::ClaudeCode, Some("projects")) => PathBuf::from(&s.table[1]),
        _ => repo.unwrap_or(cwd).to_path_buf(),
    }
}

// Checks one server.
fn probe(s: &Server, folder: &Path) -> Outcome {
    match &s.kind {
        Kind::Stdio { command, args } => probe_stdio(command, args, &s.env, folder, is_zazzle(s)),
        Kind::Http { url, .. } => {
            let mut headers = s.headers.clone();
            if let Some(var) = &s.bearer {
                match std::env::var(var) {
                    Ok(token) => headers.push(("Authorization".into(), format!("Bearer {token}"))),
                    Err(_) => {
                        return Outcome::Note(format!("its token variable {var} is not set in this shell"))
                    }
                }
            }
            probe_http(url, &headers)
        }
        Kind::Other(why) => Outcome::Note(format!("not checked: {why}")),
    }
}

// Waits for the JSON-RPC response with an id, skipping notifications and other lines.
fn response(lines: &mpsc::Receiver<String>, id: u64, until: Instant) -> Result<Value, String> {
    loop {
        let left = until.saturating_duration_since(Instant::now());
        let line = lines.recv_timeout(left).map_err(|_| "no answer in time".to_string())?;
        let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
        if v["id"] == json!(id) {
            return match v.get("error") {
                Some(e) => Err(format!("error {}", e["message"].as_str().unwrap_or("without a message"))),
                None => Ok(v["result"].clone()),
            };
        }
    }
}

// Starts a stdio server, initializes it, lists its tools and, for zazzle, asks `zazzle_overview`; stops it afterwards.
fn probe_stdio(
    command: &str,
    args: &[String],
    env: &[(String, String)],
    folder: &Path,
    zazzle: bool,
) -> Outcome {
    let child = Command::new(command)
        .args(args)
        .envs(env.iter().map(|(k, v)| (k, v)))
        .current_dir(folder)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => return Outcome::Fail(format!("cannot start `{command}` in {}: {e}", folder.display())),
    };
    let (tx, lines) = mpsc::channel();
    let stdout = child.stdout.take().expect("piped");
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let mut stdin = child.stdin.take().expect("piped");
    let until = Instant::now() + TIMEOUT;
    let mut talk = || -> Result<Outcome, String> {
        let mut send = |v: Value| writeln!(stdin, "{v}").map_err(|e| format!("cannot write to it: {e}"));
        send(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "zazzle-setup-check", "version": crate::cmd::VERSION}}}))?;
        response(&lines, 1, until)?;
        send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))?;
        send(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}))?;
        let tools = response(&lines, 2, until)?["tools"].as_array().map_or(0, Vec::len);
        if !zazzle {
            return Ok(Outcome::Ok(format!("{tools} tools")));
        }
        send(
            json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "zazzle_overview", "arguments": {}}}),
        )?;
        let r = response(&lines, 3, until)?;
        let text = r["content"][0]["text"].as_str().unwrap_or("").lines().next().unwrap_or("").to_string();
        match r["isError"].as_bool() == Some(true) {
            true => Ok(Outcome::Note(format!(
                "{tools} tools, but zazzle_overview failed: {text} (C# reads and structure need the indexer and a git folder)"
            ))),
            false => Ok(Outcome::Ok(format!("{tools} tools; zazzle_overview: {text}"))),
        }
    };
    let result = talk();
    let _ = child.kill();
    let mut err = String::new();
    if let Some(mut e) = child.stderr.take() {
        let _ = e.read_to_string(&mut err);
    }
    let _ = child.wait();
    match result {
        Ok(outcome) => outcome,
        Err(why) => {
            let tail: String =
                err.trim().chars().rev().take(STDERR_TAIL).collect::<Vec<_>>().into_iter().rev().collect();
            let tail = if tail.is_empty() {
                String::new()
            } else {
                format!("; its stderr ends: {}", tail.replace('\n', " | "))
            };
            Outcome::Fail(format!("{why}{tail}"))
        }
    }
}

// Sends an MCP initialize request to a Streamable HTTP server with the entry's headers.
fn probe_http(url: &str, headers: &[(String, String)]) -> Outcome {
    let config =
        ureq::Agent::config_builder().timeout_global(Some(TIMEOUT)).http_status_as_error(false).build();
    let agent = ureq::Agent::new_with_config(config);
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "zazzle-setup-check", "version": crate::cmd::VERSION}}});
    let mut req = agent
        .post(url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream");
    for (k, v) in headers {
        req = req.header(k.as_str(), v.as_str());
    }
    match req.send(body.to_string()) {
        Ok(r) => match r.status().as_u16() {
            200..=299 => Outcome::Ok(format!("answers at {url}")),
            401 | 403 => Outcome::Note(format!(
                "asks for sign-in at {url} (HTTP {}); the client signs in",
                r.status().as_u16()
            )),
            code => Outcome::Fail(format!("HTTP {code} from {url}")),
        },
        Err(e) => Outcome::Fail(format!("cannot reach {url}: {e}")),
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use std::net::TcpListener;

    // Returns a shell script that answers initialize, tools/list and tools/call with fixed lines.
    fn fake_server(tools: usize, overview_error: bool) -> (String, Vec<String>) {
        let list: Vec<String> = (0..tools).map(|n| format!("{{\"name\":\"t{n}\"}}")).collect();
        let script = format!(
            "read a; echo 'not json'; echo '{{\"jsonrpc\":\"2.0\",\"method\":\"notifications/x\"}}'; echo '{{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{{}}}}'; read b; read c; echo '{{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{{\"tools\":[{}]}}}}'; read d; echo '{{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{{\"isError\":{overview_error},\"content\":[{{\"type\":\"text\",\"text\":\"repo: 12 files\"}}]}}}}'; sleep 5",
            list.join(",")
        );
        ("/bin/sh".into(), vec!["-c".into(), script])
    }

    // Checks stdio probing: a working server, zazzle's overview and its failure, an error reply, a command that does not start, and one that exits early.
    #[test]
    fn test_probe_stdio() {
        let (cmd, args) = fake_server(3, false);
        assert_eq!(probe_stdio(&cmd, &args, &[], Path::new("/"), false), Outcome::Ok("3 tools".into()));
        assert_eq!(
            probe_stdio(&cmd, &args, &[("X".into(), "1".into())], Path::new("/"), true),
            Outcome::Ok("3 tools; zazzle_overview: repo: 12 files".into())
        );
        let (cmd, args) = fake_server(1, true);
        assert!(
            matches!(probe_stdio(&cmd, &args, &[], Path::new("/"), true), Outcome::Note(t) if t.contains("zazzle_overview failed"))
        );
        let err = "read a; echo '{\"jsonrpc\":\"2.0\",\"id\":1,\"error\":{\"message\":\"bad\"}}'; sleep 5";
        assert_eq!(
            probe_stdio("/bin/sh", &["-c".into(), err.into()], &[], Path::new("/"), false),
            Outcome::Fail("error bad".into())
        );
        assert!(
            matches!(probe_stdio("/no/such/zazzle", &[], &[], Path::new("/"), false), Outcome::Fail(t) if t.contains("cannot start"))
        );
        let dies = ["-c".to_string(), "echo oops >&2; exit 3".into()];
        assert!(
            matches!(probe_stdio("/bin/sh", &dies, &[], Path::new("/"), false), Outcome::Fail(t) if t.contains("stderr ends: oops"))
        );
        let quiet = ["-c".to_string(), "exit 0".into()];
        assert_eq!(
            probe_stdio("/bin/sh", &quiet, &[], Path::new("/"), false),
            Outcome::Fail("no answer in time".into())
        );
    }

    // Reads a whole HTTP request (headers and content-length body), so closing afterwards does not reset the connection.
    fn read_request(s: &mut std::net::TcpStream) {
        let mut got = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let text = String::from_utf8_lossy(&got).to_ascii_lowercase();
            if let Some(end) = text.find("\r\n\r\n") {
                let length = text[..end]
                    .lines()
                    .find_map(|l| {
                        l.strip_prefix("content-length:").and_then(|v| v.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                if got.len() >= end + 4 + length {
                    return;
                }
            }
            match s.read(&mut buf) {
                Ok(0) | Err(_) => return,
                Ok(n) => got.extend_from_slice(&buf[..n]),
            }
        }
    }

    // Serves one HTTP reply with a status.
    fn http_once(status: u16) -> String {
        let l = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = l.local_addr().expect("addr");
        std::thread::spawn(move || {
            if let Ok((mut s, _)) = l.accept() {
                read_request(&mut s);
                let _ =
                    write!(s, "HTTP/1.1 {status} X\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{{}}");
            }
        });
        format!("http://{addr}/mcp")
    }

    // Checks HTTP probing: answers, sign-in, errors, an unreachable server, and a token variable that is not set.
    #[test]
    fn test_probe_http() {
        assert!(matches!(probe_http(&http_once(200), &[("A".into(), "b".into())]), Outcome::Ok(_)));
        assert!(matches!(probe_http(&http_once(401), &[]), Outcome::Note(t) if t.contains("sign-in")));
        assert!(matches!(probe_http(&http_once(500), &[]), Outcome::Fail(t) if t.contains("HTTP 500")));
        assert!(
            matches!(probe_http("http://127.0.0.1:1/mcp", &[]), Outcome::Fail(t) if t.contains("cannot reach"))
        );
        let s = Server {
            table: vec!["mcp_servers".into()],
            name: "l".into(),
            kind: Kind::Http { url: http_once(200), auth: true },
            env: vec![],
            headers: vec![],
            bearer: Some("ZAZZLE_TEST_SURELY_UNSET".into()),
        };
        assert!(matches!(probe(&s, Path::new("/")), Outcome::Note(t) if t.contains("not set")));
        let other = Server { kind: Kind::Other("SSE".into()), bearer: None, ..s };
        assert!(matches!(probe(&other, Path::new("/")), Outcome::Note(_)));
    }

    // Checks a whole run over installed clients: a working zazzle, a command that does not start, a missing zazzle and an unset token.
    #[test]
    fn test_run() {
        let t = crate::testutil::TempDir::new();
        let home = t.path().to_path_buf();
        let env = Env {
            home: home.clone(),
            zazzled: PathBuf::from("/opt/z/zazzled"),
            indexer: None,
            codex_home: home.join(".codex"),
            macos: false,
            uid: 501,
            launchctl: |_| Ok(()),
        };
        let (cmd, mut args) = fake_server(3, false);
        args[1].push_str("; : zazzled mcp");
        let code = json!({"mcpServers": {
            "zazzle": {"command": cmd, "args": args},
            "gone": {"command": "/no/such/server"}
        }});
        std::fs::write(env.config_path(Client::ClaudeCode), code.to_string()).expect("write");
        std::fs::create_dir_all(home.join(".codex")).expect("mkdir");
        let codex = "[mcp_servers.api]\nurl = \"http://127.0.0.1:1/mcp\"\nbearer_token_env_var = \"ZAZZLE_TEST_UNSET_TOKEN\"\n";
        std::fs::write(env.config_path(Client::Codex), codex).expect("write");
        let opts = super::super::parse(&["--check".to_string()]).expect("parse");
        let r = run(&opts, &env).expect("run");
        assert!(r.contains("claude-code: ~/.claude.json"), "{r}");
        assert!(r.contains("ok   zazzle:"), "{r}");
        assert!(r.contains("FAIL gone:"), "{r}");
        assert!(r.contains("codex: ~/.codex/config.toml\n  FAIL zazzle: not configured"), "{r}");
        assert!(r.contains("note api: its token variable ZAZZLE_TEST_UNSET_TOKEN"), "{r}");
        assert!(!r.contains("claude-desktop"), "{r}");
        assert!(r.contains("2 working, 2 failed."), "{r}");
        assert!(r.contains("absolute path") && r.contains("zazzle setup --apply"), "{r}");
    }

    // Checks which folder each client starts servers in.
    #[test]
    fn test_folder() {
        let s = |table: &[&str]| Server {
            table: table.iter().map(|t| t.to_string()).collect(),
            name: "x".into(),
            kind: Kind::Other(String::new()),
            env: vec![],
            headers: vec![],
            bearer: None,
        };
        let cwd = Path::new("/cwd");
        assert_eq!(folder(Client::ClaudeDesktop, &s(&["mcpServers"]), None, cwd), PathBuf::from("/"));
        assert_eq!(
            folder(Client::ClaudeCode, &s(&["projects", "/w/app", "mcpServers"]), None, cwd),
            PathBuf::from("/w/app")
        );
        assert_eq!(
            folder(Client::Codex, &s(&["mcp_servers"]), Some(Path::new("/r")), cwd),
            PathBuf::from("/r")
        );
        assert_eq!(folder(Client::ClaudeCode, &s(&["mcpServers"]), None, cwd), PathBuf::from("/cwd"));
    }
}
