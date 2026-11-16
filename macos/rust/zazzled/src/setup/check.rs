// `zazzle setup --check` and `--report`: starts every configured MCP server the way its client would and does a real handshake, says which servers go through zazzle and which stay direct, whether routing is enforced by a hook or only asked for, whether repositories can be read and edited, and what the activity log shows.
use super::clients::{Client, ConfigFile, Kind, Server};
use super::{hook_command, is_zazzle, local_url, wrapped, Env, Opts};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(20);
const STDERR_TAIL: usize = 300;
// How far back --check's activity summary and --report look.
const DAY_SECS: u64 = 24 * 3600;
// The session id of hook self-tests, which summaries of agent activity leave out.
const SELFTEST_SESSION: &str = "zazzle-setup-selftest";

// One server's outcome: it works, it needs the user (such as an OAuth sign-in), or it failed with a reason.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    Ok(String),
    Note(String),
    Fail(String),
}

// How a configured server is reached: zazzle itself, through zazzle's proxy, or directly with the reason.
pub fn routing(s: &Server, routes: &[(u16, String)]) -> String {
    if is_zazzle(s) {
        return "zazzle".into();
    }
    if wrapped(s).is_some() {
        return "proxied: stdio".into();
    }
    match &s.kind {
        Kind::Http { url, .. } if routes.iter().any(|(p, _)| local_url(*p) == *url) => "proxied: http".into(),
        Kind::Http { auth: false, .. } => "direct: OAuth sign-in, which the proxy cannot carry".into(),
        Kind::Http { .. } => "direct: not routed yet, `zazzle setup --apply` routes it (macOS)".into(),
        Kind::Stdio { .. } => {
            "direct: not wrapped yet (added after setup?), `zazzle setup --apply` wraps it".into()
        }
        Kind::Other(why) => format!("direct: {why}"),
    }
}

// Says whether a client's routing is enforced by zazzle's hook or only asked for by instructions.
fn enforcement(env: &Env, c: Client) -> String {
    let instr = env
        .instructions_path(c)
        .map(|p| (std::fs::read_to_string(&p).unwrap_or_default().contains(super::BLOCK_BEGIN), p));
    let instr = match instr {
        Some((true, p)) => format!("instructions in {}", env.short(&p)),
        Some((false, p)) => format!("no instructions in {} (run setup --apply)", env.short(&p)),
        None => "MCP instructions only".into(),
    };
    match super::hook_spec(env, c, false).and_then(|h| hook_command(&h.path).map(|cmd| (h, cmd))) {
        Some((h, cmd)) => {
            let edits = if cmd.ends_with(" --edits") {
                "native edits refused"
            } else {
                "native edits allowed and logged"
            };
            let trust = if c == Client::Codex { "; Codex runs it once trusted (/hooks)" } else { "" };
            let test = match hook_selftest(&cmd) {
                Ok(()) => "self-test passed".to_string(),
                Err(e) => format!("SELF-TEST FAILED: {e}"),
            };
            format!("enforced by the hook in {} ({test}; {edits}{trust}); {instr}", env.short(&h.path))
        }
        None if c == Client::ClaudeDesktop => {
            format!("instructions only (the desktop chat has no hooks); {instr}")
        }
        None => format!("instructions only, nothing enforced (add --hook); {instr}"),
    }
}

// Runs a hook command on sample calls: raw search and build redirected, a plain command allowed.
pub fn hook_selftest(command: &str) -> Result<(), String> {
    let ask = |cmd: &str| -> Result<String, String> {
        let payload = json!({"session_id": SELFTEST_SESSION, "cwd": "/", "hook_event_name": "PreToolUse", "tool_name": "Bash", "tool_input": {"command": cmd}});
        let mut child = Command::new("/bin/sh")
            .args(["-c", command])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("cannot run it: {e}"))?;
        let _ = child.stdin.take().expect("piped").write_all(payload.to_string().as_bytes());
        let out = child.wait_with_output().map_err(|e| e.to_string())?;
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    };
    let denied = |o: &str| o.contains("\"permissionDecision\":\"deny\"");
    for (cmd, want) in [("rg zazzle_selftest", true), ("cargo test", true), ("git status", false)] {
        let got = ask(cmd)?;
        if denied(&got) != want {
            return Err(format!("`{cmd}` was {}", if want { "let through" } else { "refused" }));
        }
    }
    Ok(())
}

// Describes where a folder's repositories are and whether zazzle may edit there.
fn access(folder: &Path) -> String {
    let git = crate::indexer::work_tree(folder);
    let repos = match git {
        Some(t) => format!("in the git work tree {}", t.display()),
        None => match crate::cmd::structure::repos_below(folder).len() {
            0 => "no git repository here or below, so the structure tools fail".into(),
            n => format!("{n} git repositories below"),
        },
    };
    let roots = crate::workspace::roots(folder, &[]);
    format!(
        "{repos}; edits allowed below {}",
        roots.iter().map(|r| r.display().to_string()).collect::<Vec<_>>().join(", ")
    )
}

// Checks each chosen client's servers, routing and repository access, and returns the report.
pub fn run(opts: &Opts, env: &Env) -> Result<String, String> {
    let chosen: Vec<Client> = if opts.clients.is_empty() {
        Client::ALL.into_iter().filter(|c| env.installed(*c)).collect()
    } else {
        opts.clients.clone()
    };
    let routes = super::load_routes(env);
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    let mut out = String::from("zazzle setup --check: each server started the way its client starts it.\n");
    let (mut ok, mut failed) = (0, 0);
    let mut hints: Vec<&str> = Vec::new();
    for c in chosen {
        let file = ConfigFile::load(c, &env.config_path(c))?;
        out.push_str(&format!("\n{}: {}\n", c.name(), env.short(&file.path)));
        out.push_str(&format!("  routing: {}\n", enforcement(env, c)));
        if out.contains("SELF-TEST FAILED") {
            hints.push("A hook did not answer as expected: run `zazzle setup --hook --apply` again, then restart the client.");
        }
        let servers = file.servers();
        if !servers.iter().any(is_zazzle) {
            failed += 1;
            out.push_str("  FAIL zazzle: not configured\n");
            hints.push("zazzle is missing from a client: run `zazzle setup`, then `zazzle setup --apply`.");
        }
        for s in &servers {
            let route = routing(s, &routes);
            if !s.enabled {
                out.push_str(&format!("  off  {} [{route}]: disabled in the client\n", s.name));
                continue;
            }
            let folder = folder(c, s, opts.repo.as_deref(), &cwd);
            let outcome = probe(s, &folder);
            let (word, text) = match &outcome {
                Outcome::Ok(t) => ("ok  ", t),
                Outcome::Note(t) => ("note", t),
                Outcome::Fail(t) => ("FAIL", t),
            };
            out.push_str(&format!("  {word} {} [{route}]: {text}\n", s.name));
            if is_zazzle(s) {
                out.push_str(&format!(
                    "       repository access from {}: {}\n",
                    folder.display(),
                    access(&folder)
                ));
            }
            if route.starts_with("direct: not") {
                hints.push(
                    "Some servers were added after setup: `zazzle setup --apply` puts them behind the proxy.",
                );
            }
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
    let direct = super::unmanaged(env, opts);
    if !direct.is_empty() {
        out.push_str("\nother servers, not configured by setup (direct):\n");
        direct.iter().for_each(|d| out.push_str(&format!("  {d}\n")));
    }
    out.push_str(&format!("\nworkspaces: {}\n", access(&cwd)));
    if let Some(log) = crate::activity::path() {
        out.push_str(&format!(
            "\nlast 24 hours: {}\n",
            summary(&agents_only(crate::activity::read(&log, now().saturating_sub(DAY_SECS)))).0
        ));
    }
    hints.sort();
    hints.dedup();
    out.push_str(&format!("\n{ok} working, {failed} failed.\n"));
    hints.iter().for_each(|h| out.push_str(&format!("{h}\n")));
    out.push_str("Configured is not the same as used: `zazzle setup --verify` exercises zazzle, and `zazzle setup --report` shows what agents actually sent through it.\n");
    Ok(out)
}

// Returns the seconds since the epoch.
fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

// Drops the entries setup's own self-tests and verify runs wrote (sessions `zazzle-setup-*`), so summaries count only what agents did.
fn agents_only(entries: Vec<Value>) -> Vec<Value> {
    entries
        .into_iter()
        .filter(|e| !e["session"].as_str().is_some_and(|s| s.starts_with("zazzle-setup-")))
        .collect()
}

// Counts values by name, most frequent first, as `name N, name N`.
fn tally<'a>(names: impl Iterator<Item = &'a str>) -> String {
    let mut counts: Vec<(&str, usize)> = Vec::new();
    for n in names {
        match counts.iter_mut().find(|(k, _)| *k == n) {
            Some((_, c)) => *c += 1,
            None => counts.push((n, 1)),
        }
    }
    counts.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    counts.iter().map(|(k, c)| format!("{k} {c}")).collect::<Vec<_>>().join(", ")
}

// Summarizes activity entries: a one-line overview and the detailed lines.
pub fn summary(entries: &[Value]) -> (String, Vec<String>) {
    let of = |src: &'static str| entries.iter().filter(move |e| e["src"] == src);
    let str_of = |e: &'static str| move |v: &Value| v[e].as_str().unwrap_or("").to_string();
    let zazzle: Vec<String> = of("zazzle").map(str_of("cmd")).collect();
    let runs: Vec<&Value> = of("run").collect();
    let proxy: Vec<String> = of("proxy")
        .map(|e| format!("{} {}", e["server"].as_str().unwrap_or("?"), e["tool"].as_str().unwrap_or("")))
        .collect();
    let hook =
        |o: &str| of("hook").filter(|e| e["outcome"] == o).map(str_of("program")).collect::<Vec<String>>();
    let (redirected, native, raw, down) =
        (hook("redirected"), hook("native-edit"), hook("allow-raw"), hook("zazzle-unavailable"));
    let sessions = entries
        .iter()
        .filter_map(|e| e["session"].as_str())
        .filter(|s| !s.is_empty())
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    let line = format!(
        "{} zazzle commands, {} runs, {} proxied MCP calls; the hook redirected {} raw commands; {} bypasses ({} native edits, {} allow-raw, {} while zazzle was unavailable)",
        zazzle.len(), runs.len(), proxy.len(), redirected.len(), native.len() + raw.len() + down.len(), native.len(), raw.len(), down.len()
    );
    let failed_runs = runs.iter().filter(|r| r["status"].as_i64().unwrap_or(0) != 0).count();
    let mut lines = vec![
        format!("through zazzle: {} commands in {sessions} sessions ({})", zazzle.len(), tally(zazzle.iter().map(String::as_str))),
        format!("command runs through `zazzle run`: {} ({failed_runs} exited non-zero)", runs.len()),
        format!("MCP calls through zazzle's proxy: {} ({})", proxy.len(), tally(proxy.iter().map(String::as_str))),
        format!("raw commands the hook redirected to zazzle: {} ({})", redirected.len(), tally(redirected.iter().map(String::as_str))),
        format!("bypasses, which went around zazzle: native edits {} ({}), `# allow-raw` {} ({}), zazzle unavailable {} ({})",
            native.len(), tally(native.iter().map(String::as_str)), raw.len(), tally(raw.iter().map(String::as_str)), down.len(), tally(down.iter().map(String::as_str))),
    ];
    lines.retain(|l| !l.contains(": 0 (") || l.starts_with("bypasses"));
    (line, lines)
}

// Runs `zazzle setup --report`: what the activity log shows for the last day, and what it cannot show.
pub fn report(env: &Env) -> Result<String, String> {
    let log = crate::activity::path().unwrap_or_else(|| env.home.join(".zazzle/activity.log"));
    let entries = agents_only(crate::activity::read(&log, now().saturating_sub(DAY_SECS)));
    let mut out = format!("zazzle activity in the last 24 hours ({}):\n", env.short(&log));
    if entries.is_empty() {
        out.push_str("  nothing recorded: no agent has used zazzle, its hook or its proxy since then (or they run another zazzled)\n");
    }
    summary(&entries).1.iter().for_each(|l| out.push_str(&format!("  {l}\n")));
    out.push_str("\nNot visible in this log, so not counted as coverage:\n  - native tool calls the hook lets through (Claude Code's Read of non-C# files, Codex's own tools other than shell and apply_patch, web search)\n  - MCP servers that stay direct (OAuth, SSE, plugins, project files): their calls never reach zazzle\n  - the Claude desktop app's chat, which has no hooks\n  - clients whose hook is not installed or not yet trusted (Codex)\n");
    Ok(out)
}

// Returns the folder a client starts a stdio server in: the entry's own cwd, `/` for the desktop app, the project for Claude Code's project servers, else --repo or the current folder.
fn folder(c: Client, s: &Server, repo: Option<&Path>, cwd: &Path) -> PathBuf {
    if let Some(d) = &s.cwd {
        return PathBuf::from(d);
    }
    match (c, s.table.first().map(String::as_str)) {
        (Client::ClaudeDesktop, _) => PathBuf::from("/"),
        (Client::ClaudeCode, Some("projects")) => PathBuf::from(&s.table[1]),
        _ => repo.unwrap_or(cwd).to_path_buf(),
    }
}

// Checks one server.
fn probe(s: &Server, folder: &Path) -> Outcome {
    match &s.kind {
        Kind::Stdio { command, args } => {
            let env: Vec<(String, String)> = s
                .env
                .iter()
                .cloned()
                .chain([("ZAZZLE_SESSION".into(), "zazzle-setup-check".into())])
                .collect();
            probe_stdio(command, args, &env, folder, is_zazzle(s))
        }
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

// A running stdio MCP server, initialized, that tools can be called on.
pub struct Session {
    child: Child,
    stdin: ChildStdin,
    lines: mpsc::Receiver<String>,
    next: u64,
    until: Instant,
    pub tools: usize,
}

impl Session {
    // Starts a server in a folder with extra environment, initializes it and lists its tools.
    pub fn start(
        command: &str,
        args: &[String],
        env: &[(String, String)],
        folder: &Path,
    ) -> Result<Session, String> {
        let mut child = Command::new(command)
            .args(args)
            .envs(env.iter().map(|(k, v)| (k, v)))
            .current_dir(folder)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("cannot start `{command}` in {}: {e}", folder.display()))?;
        let (tx, lines) = mpsc::channel();
        let stdout = child.stdout.take().expect("piped");
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let stdin = child.stdin.take().expect("piped");
        let mut s = Session { child, stdin, lines, next: 1, until: Instant::now() + TIMEOUT, tools: 0 };
        let r = s.handshake();
        match r {
            Ok(()) => Ok(s),
            Err(e) => Err(s.fail(e)),
        }
    }

    // Sends initialize, the initialized notice and tools/list.
    fn handshake(&mut self) -> Result<(), String> {
        self.request("initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "zazzle-setup-check", "version": crate::cmd::VERSION}}))?;
        self.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))?;
        self.tools = self.request("tools/list", json!({}))?["tools"].as_array().map_or(0, Vec::len);
        Ok(())
    }

    // Writes one message.
    fn send(&mut self, v: Value) -> Result<(), String> {
        writeln!(self.stdin, "{v}").map_err(|e| format!("cannot write to it: {e}"))
    }

    // Sends a request and waits for its result.
    fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next;
        self.next += 1;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        response(&self.lines, id, self.until)
    }

    // Calls a tool and returns its text and whether it reported an error.
    pub fn call(&mut self, tool: &str, args: Value) -> Result<(String, bool), String> {
        self.until = Instant::now() + TIMEOUT;
        let r = self.request("tools/call", json!({"name": tool, "arguments": args}))?;
        let text = r["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|c| c["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        Ok((text, r["isError"].as_bool() == Some(true)))
    }

    // Stops the server and returns its standard error.
    pub fn stop(mut self) -> String {
        let _ = self.child.kill();
        let mut err = String::new();
        if let Some(mut e) = self.child.stderr.take() {
            let _ = e.read_to_string(&mut err);
        }
        let _ = self.child.wait();
        err
    }

    // Stops the server and adds the end of its standard error to a failure.
    pub fn fail(self, why: String) -> String {
        let err = self.stop();
        let tail: String =
            err.trim().chars().rev().take(STDERR_TAIL).collect::<Vec<_>>().into_iter().rev().collect();
        match tail.is_empty() {
            true => why,
            false => format!("{why}; its stderr ends: {}", tail.replace('\n', " | ")),
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
    let mut s = match Session::start(command, args, env, folder) {
        Ok(s) => s,
        Err(e) => return Outcome::Fail(e),
    };
    let tools = s.tools;
    if !zazzle {
        s.stop();
        return Outcome::Ok(format!("{tools} tools"));
    }
    match s.call("zazzle_overview", json!({})) {
        Err(e) => Outcome::Fail(s.fail(e)),
        Ok((text, err)) => {
            s.stop();
            let first = text.lines().next().unwrap_or("").to_string();
            match err {
                true => Outcome::Note(format!("{tools} tools, but zazzle_overview failed: {first} (C# reads and structure need the indexer and a git folder)")),
                false => Outcome::Ok(format!("{tools} tools; zazzle_overview: {first}")),
            }
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

    // Serves one HTTP reply with a status.
    fn http_once(status: u16) -> String {
        let l = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = l.local_addr().expect("addr");
        std::thread::spawn(move || {
            if let Ok((mut s, _)) = l.accept() {
                let mut buf = [0u8; 4096];
                let _ = s.read(&mut buf);
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
            cwd: None,
            enabled: true,
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
            "gone": {"command": "/no/such/server"},
            "off": {"command": "x", "disabled": true},
            "wrapped": {"command": "/opt/z/zazzled", "args": ["proxy", "--log", "--", "/no/such/server"]}
        }});
        let hook = "read l; case \"$l\" in *git*) ;; *) echo '{\"permissionDecision\":\"deny\"}';; esac";
        let settings = json!({"hooks": {"PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": format!("{hook} # zazzled hook --client claude-code")}]}]}});
        std::fs::create_dir_all(home.join(".claude")).expect("mkdir");
        std::fs::write(home.join(".claude/settings.json"), settings.to_string()).expect("write");
        let log = home.join("activity.log");
        let _g = crate::testutil::env_lock();
        let saved = std::env::var_os("ZAZZLE_ACTIVITY_LOG");
        std::env::set_var("ZAZZLE_ACTIVITY_LOG", &log);
        crate::activity::record(json!({"src": "zazzle", "cmd": "read", "session": "s"}));
        crate::activity::record(json!({"src": "hook", "outcome": "native-edit", "program": "apply_patch"}));
        std::fs::write(env.config_path(Client::ClaudeCode), code.to_string()).expect("write");
        std::fs::create_dir_all(home.join(".codex")).expect("mkdir");
        let codex = "[mcp_servers.api]\nurl = \"http://127.0.0.1:1/mcp\"\nbearer_token_env_var = \"ZAZZLE_TEST_UNSET_TOKEN\"\n";
        std::fs::write(env.config_path(Client::Codex), codex).expect("write");
        let opts = super::super::parse(&["--check".to_string()]).expect("parse");
        let r = run(&opts, &env).expect("run");
        assert!(r.contains("claude-code: ~/.claude.json\n  routing: enforced by the hook in ~/.claude/settings.json (self-test passed; native edits allowed and logged); no instructions in ~/.claude/CLAUDE.md"), "{r}");
        assert!(r.contains("ok   zazzle [zazzle]:") && r.contains("repository access from"), "{r}");
        assert!(r.contains("FAIL gone [direct: not wrapped yet"), "{r}");
        assert!(
            r.contains("off  off [direct: not wrapped yet") && r.contains("FAIL wrapped [proxied: stdio]"),
            "{r}"
        );
        assert!(
            r.contains(
                "codex: ~/.codex/config.toml\n  routing: instructions only, nothing enforced (add --hook)"
            ),
            "{r}"
        );
        assert!(r.contains("FAIL zazzle: not configured"), "{r}");
        assert!(r.contains("note api [direct: not routed yet"), "{r}");
        assert!(!r.contains("claude-desktop"), "{r}");
        assert!(r.contains("2 working, 3 failed."), "{r}");
        assert!(r.contains("absolute path") && r.contains("added after setup"), "{r}");
        assert!(r.contains("last 24 hours: 1 zazzle commands, 0 runs, 0 proxied MCP calls; the hook redirected 0 raw commands; 1 bypasses (1 native edits"), "{r}");
        let rep = report(&env).expect("report");
        assert!(
            rep.contains("through zazzle: 1 commands in 1 sessions (read 1)")
                && rep.contains("bypasses, which went around zazzle: native edits 1 (apply_patch 1)"),
            "{rep}"
        );
        assert!(rep.contains("Not visible in this log"), "{rep}");
        std::fs::remove_file(&log).expect("rm");
        assert!(report(&env).expect("report").contains("nothing recorded"));
        std::fs::write(home.join(".claude/settings.json"), settings.to_string().replace("*git*", "*zzz*"))
            .expect("write");
        let r = run(&opts, &env).expect("run");
        assert!(
            r.contains("SELF-TEST FAILED: `git status` was refused")
                && r.contains("A hook did not answer as expected"),
            "{r}"
        );
        match saved {
            Some(v) => std::env::set_var("ZAZZLE_ACTIVITY_LOG", v),
            None => std::env::remove_var("ZAZZLE_ACTIVITY_LOG"),
        }
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
            cwd: None,
            enabled: true,
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
        let own = Server { cwd: Some("/own".into()), ..s(&["mcp_servers"]) };
        assert_eq!(folder(Client::Codex, &own, Some(Path::new("/r")), cwd), PathBuf::from("/own"));
        let routes = vec![(8809, "https://u".to_string())];
        let http = |url: &str, auth: bool| Server { kind: Kind::Http { url: url.into(), auth }, ..s(&["m"]) };
        assert_eq!(routing(&http("http://127.0.0.1:8809/mcp", true), &routes), "proxied: http");
        assert!(routing(&http("https://o", false), &routes).starts_with("direct: OAuth"));
        assert!(routing(&s(&["m"]), &routes).starts_with("direct: "));
        assert!(hook_selftest("exit 0").unwrap_err().contains("`rg zazzle_selftest` was let through"));
        assert_eq!(tally(["a", "b", "a"].into_iter()), "a 2, b 1");
        assert!(access(Path::new("/nonexistent-zz")).starts_with("no git repository"));
    }
}
