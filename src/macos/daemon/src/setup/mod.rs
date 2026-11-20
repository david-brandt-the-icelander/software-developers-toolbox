// `zazzle setup`: plans, applies, undoes and checks the user-level configuration that makes Claude Code, the Claude desktop app and Codex use zazzle, and sends their other MCP servers through the zazzle proxy.
pub mod check;
pub mod clients;
pub mod compaction;
pub mod policy;
pub mod verify;

use clients::{Client, ConfigFile, Kind, Server};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub const USAGE: &str = "usage: zazzle setup [--apply | --undo | --off | --on | --uninstall | --check | --verify | --report] [--client claude-code|claude-desktop|codex].. [--repo DIR] [--workspace DIR].. [--no-hook] [--hook-edits] [--explain] [--set KEY=VALUE].. [--brief] [--no-wrap] [--probe]";
const FIRST_PORT: u16 = 8809;
const BLOCK_BEGIN: &str = "<!-- zazzle setup: begin -->";
const BLOCK_END: &str = "<!-- zazzle setup: end -->";
// The tools each client's hook sees: shell commands and search, read and edit tools (edits are logged, or refused with --hook-edits).
const CLAUDE_MATCHER: &str = "Bash|Grep|Glob|Read|Edit|Write|MultiEdit|NotebookEdit|mcp__zazzle__.*";
const CODEX_MATCHER: &str = "Bash|apply_patch";

// What agents are told in the MCP handshake: short, since some clients repeat it with every tool they list.
pub const MCP_INSTRUCTIONS: &str = "\
Find code precisely, then read it literally: zazzle_read with symbol when you know the name, zazzle_find_symbol (several names, paths to scope) or zazzle_refs for declarations and uses, zazzle_find for text.
- zazzle_read returns the literal source of a range (PATH:A-B) or one symbol with its comments and attributes, with line numbers and the sha zazzle_edit needs. If it says INCOMPLETE, read the range it names.
- Put independent lookups in one call (several paths, or parallel calls); finish reading a command's output before starting another.
- For a task's rules, zazzle_get_rules with the paths (and a purpose) gives the instructions that apply, once per session; rules already sent stay in force. Check a plan against them, e.g. frozen published contracts.
- Before a task, zazzle_brief with your intent, paths and purpose is your rule read: every rule section that applies plus the skills it needs. Do not read AGENTS.md or CLAUDE.md yourself first.
- Local model: read short index results (find_symbol, refs, outline) yourself. Use zazzle_investigate for a question that needs several files: seed it with the entry point's symbol (the endpoint handler) and scope paths to the layers the call crosses and their tests; it returns checked path:line citations, candidates for a seed it cannot find, and PARTIAL with ranked excerpts of the evidence when no finding is verified. It returns one result within about 90 s: call it as a tool and wait, never from a script that yields, sleeps or polls. Use `llm` only on large outputs. Never call it just to have used it.
- Spend your effort on the change: formatting, linting, test and coverage runs, dependency audits, keeping the branch current, documentation, changelogs, commit messages, pull request text and CI waits are zazzle flow's (verify, tidy, coverage, audit, update, write, deliver, ci) and its local agent's, not yours.
- If the C# indexer fails, use zazzle_find, zazzle_read and zazzle_refs; zazzle_find_symbol needs the indexer.";

// What agents are told in the instruction files setup writes (CLAUDE.md, AGENTS.md), read once per session.
pub const INSTRUCTIONS: &str = "\
zazzle (MCP server `zazzle`, or the `zazzle` command) reads, searches and edits files with line numbers, exact references and safe edits.
- Locate precisely: when you know the symbol, `zazzle_read` with `symbol` returns its source in one step; otherwise `zazzle_find_symbol` (literal names, several at once, `paths` to scope) and `zazzle_refs` (definitions and uses, exact for C#, `also` for more names) before broad text search with `zazzle_find`.
- Read literally: `zazzle_read` returns the source of a range (`PATH:A-B`, `REV:PATH`) or one `symbol` with its comments, attributes and documentation, with the sha `zazzle_edit` needs. A complete method or range comes back whole when it fits the budget; otherwise the reply says INCOMPLETE and names the exact range to read next. `terse: true` is an opt-in lossy summary, never needed for correctness.
- Rules for the work at hand: `zazzle_get_rules` (or `zazzle get-rules PATH... --purpose NAME`) returns the instructions and constraints that apply to those paths, including repository-wide ones, and leaves out what this session already received; those rules still apply. Check a plan against them before acting, especially constraints such as frozen published contracts.
- Batch independent lookups: several paths in one `zazzle_read`, several edits in one `zazzle_edit` (`edits`), and independent tool calls together.
- Builds and tests: `zazzle run -- CMD` runs the command in the same shell with the same exit code and cleaned output. Collect a command's output before issuing the next command.
- The user's local model (needs `zazzle agent llm`) does work that would otherwise fill your context; use it where that saves something, not early or by quota.
  - Short index results (find_symbol, refs, outline, a range) you read yourself: they are already small. `llm` passes output under ~400 tokens through unchanged.
  - A question that needs evidence from several files: `zazzle_investigate` (`zazzle investigate QUESTION --path P --symbol S`). Seed it from the actual entry point (the endpoint's handler symbol, not a name you guess) and give one `--path` per layer the call crosses (the facade, the service, the handoff to the next system) and their tests. It returns findings whose `path:line` citations zazzle checked and quoted, plus what it could not establish. A seed not in the paths comes back with real candidates, or as no evidence without asking the model; PARTIAL means no finding could be verified, and ranked verbatim excerpts of the evidence follow with the lookups that found nothing, so you need not repeat them; INCOMPLETE means it stopped early. It returns one result within about 90 s, waiting for the local model included (`seconds` changes that): call it as a tool and wait for it, never from a script or loop that yields, sleeps or polls. Verify anything you act on.
  - Large output you need only part of (API responses, logs, long test runs): give the tool `llm` (or `--llm 'INSTRUCTION'`, also `zazzle run --llm '..' -- CMD`); the answer is marked as post-processed, so drop `llm` when exact text matters.
  - Before a task, `zazzle_brief` (`zazzle brief INTENT --path P --purpose NAME`) is your rule read: it returns every rule section that applies to those paths and purposes, then the skills the local model ranks as needed, cut only between sections; what this session already has is named, not repeated. Do not read AGENTS.md, CLAUDE.md or skill files yourself before it: it would only send them again.
  - Rules reach you apart from the source: on the command line on standard error, so `zazzle read X | zazzle agent prompt` sends the model only the source and you still get the rules you must follow.
- Spend your effort on the change itself. Formatting and linting (`zazzle flow tidy`), test and coverage runs (`zazzle flow verify`, `zazzle flow coverage`), dependency audits (`zazzle flow audit`), merging the target branch in (`zazzle flow update`), documentation, changelogs, commit messages and pull request text (`zazzle flow write docs|changelog|commit|pr`), delivery (`zazzle flow deliver`) and CI waits (`zazzle flow ci`) are done by zazzle's tools and its local agent; do not do them by hand or check their output beyond the receipt.
- `zazzle_diff` shows what changed with stats first; `zazzle_replace` changes a pattern across files (`dry_run` first).
- When an answer must come from a particular commit, pass `commit` (a sha, or `HEAD`); the answer ends with `[commit SHA REPO]`.
- Edits may go anywhere in the git work tree you started in, in folders the client names and in the user's workspaces (`~/.zazzle/workspaces`).
- If a zazzle tool is missing or fails, say so and use the native tool; where the hook refuses a raw command, `# allow-raw` at its end lets it through (logged).
- If zazzle seems misconfigured, tell the user.";

// What setup was asked to do.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Action {
    Plan,
    Apply,
    Undo,
    Check,
    Verify,
    Report,
    Off,
    On,
    Uninstall,
    Explain,
}

// The parsed `zazzle setup` command line.
#[derive(Debug, PartialEq)]
pub struct Opts {
    pub action: Action,
    pub clients: Vec<Client>,
    pub repo: Option<PathBuf>,
    pub workspaces: Vec<PathBuf>,
    pub hook: bool,
    pub hook_edits: bool,
    pub wrap: bool,
    pub probe: bool,
    pub sets: Vec<String>,
    pub brief: bool,
}

// Parses the arguments after `zazzle daemon setup`.
pub fn parse(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts {
        action: Action::Plan,
        clients: Vec::new(),
        repo: None,
        workspaces: Vec::new(),
        hook: true,
        hook_edits: false,
        wrap: true,
        probe: false,
        sets: Vec::new(),
        brief: false,
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut action = |x: Action| {
            if o.action != Action::Plan {
                return Err(format!("give one of --apply, --undo, --off, --on, --uninstall, --check, --verify and --report\n{USAGE}"));
            }
            o.action = x;
            Ok(())
        };
        match a.as_str() {
            "--apply" => action(Action::Apply)?,
            "--undo" => action(Action::Undo)?,
            "--check" => action(Action::Check)?,
            "--verify" => action(Action::Verify)?,
            "--report" => action(Action::Report)?,
            "--off" => action(Action::Off)?,
            "--on" => action(Action::On)?,
            "--uninstall" => action(Action::Uninstall)?,
            "--explain" => action(Action::Explain)?,
            "--set" => o.sets.push(it.next().ok_or("--set needs KEY=VALUE")?.clone()),
            "--hook" => o.hook = true,
            "--no-hook" => o.hook = false,
            "--probe" => o.probe = true,
            "--brief" => o.brief = true,
            "--hook-edits" => (o.hook, o.hook_edits) = (true, true),
            "--workspace" => {
                o.workspaces.push(PathBuf::from(it.next().ok_or("--workspace needs a directory")?))
            }
            "--no-wrap" => o.wrap = false,
            "--repo" => o.repo = Some(PathBuf::from(it.next().ok_or("--repo needs a directory")?)),
            "--client" => {
                let name = it.next().ok_or("--client needs a name")?;
                let c = Client::parse(name).ok_or_else(|| format!("unknown client '{name}'\n{USAGE}"))?;
                o.clients.push(c);
            }
            "-h" | "--help" => return Err(USAGE.into()),
            other => return Err(format!("unknown setup option '{other}'\n{USAGE}")),
        }
    }
    Ok(o)
}

// Where setup reads and writes: the home directory, this daemon, the C# indexer, Codex's home, and whether launchd can keep HTTP proxies running.
#[derive(Debug, Clone)]
pub struct Env {
    pub home: PathBuf,
    pub daemon: PathBuf,
    pub indexer: Option<PathBuf>,
    pub codex_home: PathBuf,
    pub macos: bool,
    pub uid: u32,
    pub launchctl: fn(&[&str]) -> Result<(), String>,
}

impl Env {
    // Finds the paths for the current user and this binary.
    pub fn detect() -> Result<Env, String> {
        let home = PathBuf::from(std::env::var_os("HOME").ok_or("HOME is not set")?);
        let daemon = std::env::current_exe().map_err(|e| format!("cannot find daemon: {e}"))?;
        let daemon = stable_exe(&daemon.canonicalize().unwrap_or(daemon));
        let indexer = crate::indexer::Indexer::locate().ok().map(|ix| ix.exe);
        let codex_home =
            std::env::var_os("CODEX_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".codex"));
        // SAFETY: getuid has no preconditions.
        let uid = unsafe { libc::getuid() };
        Ok(Env {
            home,
            daemon,
            indexer,
            codex_home,
            macos: cfg!(target_os = "macos"),
            uid,
            launchctl: crate::launchd::launchctl,
        })
    }

    // Returns a client's user-level configuration file.
    pub fn config_path(&self, c: Client) -> PathBuf {
        match c {
            Client::ClaudeCode => self.home.join(".claude.json"),
            Client::ClaudeDesktop if self.macos => {
                self.home.join("Library/Application Support/Claude/claude_desktop_config.json")
            }
            Client::ClaudeDesktop => self.home.join(".config/Claude/claude_desktop_config.json"),
            Client::Codex => self.codex_home.join("config.toml"),
        }
    }

    // Returns the file a client reads standing instructions from, if it has one.
    fn instructions_path(&self, c: Client) -> Option<PathBuf> {
        match c {
            Client::ClaudeCode => Some(self.home.join(".claude/CLAUDE.md")),
            Client::ClaudeDesktop => None,
            Client::Codex => Some(self.codex_home.join("AGENTS.md")),
        }
    }

    // True when the client looks installed: its configuration file or folder exists.
    fn installed(&self, c: Client) -> bool {
        let p = self.config_path(c);
        p.exists() || p.parent().is_some_and(Path::exists)
    }

    // Returns the record of HTTP servers setup routed through local proxies.
    fn state_path(&self) -> PathBuf {
        self.home.join(".zazzle/setup.json")
    }

    // Writes a path with the home directory as `~`.
    fn short(&self, p: &Path) -> String {
        p.strip_prefix(&self.home).map_or_else(|_| p.display().to_string(), |r| format!("~/{}", r.display()))
    }
}

// Returns a path that survives upgrades: Homebrew's versioned `Cellar/NAME/VERSION/...` becomes its `opt/NAME/...` link when that exists.
pub fn stable_exe(p: &Path) -> PathBuf {
    let parts: Vec<_> = p.components().collect();
    let Some(at) = parts.iter().position(|c| c.as_os_str() == "Cellar") else { return p.to_path_buf() };
    let (Some(name), true) = (parts.get(at + 1), parts.len() > at + 3) else { return p.to_path_buf() };
    let mut opt: PathBuf = parts[..at].iter().collect();
    opt.push("opt");
    opt.push(name);
    opt.extend(&parts[at + 3..]);
    if opt.exists() {
        opt
    } else {
        p.to_path_buf()
    }
}

// One change to make (or made) to a configuration file, in the order planned.
#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    AddZazzle { table: Vec<String>, command: String, args: Vec<String> },
    DirectCalls,
    UndoDirectCalls,
    Wrap { server: Server },
    Route { server: Server, port: u16 },
    Leave { server: Server, why: String },
    Unwrap { server: Server, command: String, args: Vec<String> },
    Unroute { server: Server, url: String },
    RemoveZazzle { table: Vec<String> },
}

// The plan for one client: its file, what to do to it, and the instruction and hook files to touch.
#[derive(Debug)]
pub struct ClientPlan {
    pub client: Client,
    pub file: ConfigFile,
    pub ops: Vec<Op>,
    pub instructions: Option<PathBuf>,
    pub hook: Option<HookSpec>,
}

// A client's PreToolUse hook: the file it lives in, the tools it sees and the command it runs.
#[derive(Debug, Clone, PartialEq)]
pub struct HookSpec {
    pub path: PathBuf,
    pub matcher: &'static str,
    pub command: String,
}

// Returns the hook a client can have: Claude Code's in ~/.claude/settings.json, Codex's in hooks.json; the desktop app's chat has none.
fn hook_spec(env: &Env, c: Client, edits: bool) -> Option<HookSpec> {
    let z = env.daemon.to_string_lossy();
    let exe = if z.contains(char::is_whitespace) { sh_quote(&z) } else { z.to_string() };
    let command = format!("{exe} hook --client {}{}", c.name(), if edits { " --edits" } else { "" });
    match c {
        Client::ClaudeCode => {
            Some(HookSpec { path: env.home.join(".claude/settings.json"), matcher: CLAUDE_MATCHER, command })
        }
        Client::Codex => {
            Some(HookSpec { path: env.codex_home.join("hooks.json"), matcher: CODEX_MATCHER, command })
        }
        Client::ClaudeDesktop => None,
    }
}

// HTTP servers setup routed: local port to upstream URL.
type Routes = Vec<(u16, String)>;

// Reads setup's record: the routes, and the workspace lines setup added.
fn load_state(env: &Env) -> (Routes, Vec<String>) {
    let text = std::fs::read_to_string(env.state_path()).unwrap_or_default();
    let v: Value = serde_json::from_str(&text).unwrap_or(json!({}));
    let routes = v.get("routes").and_then(Value::as_object);
    let routes = routes
        .map(|m| m.iter().filter_map(|(p, u)| Some((p.parse().ok()?, u.as_str()?.to_string()))).collect())
        .unwrap_or_default();
    let added = v["workspaces"].as_array().into_iter().flatten().filter_map(Value::as_str).map(String::from);
    (routes, added.collect())
}

// Reads the routes setup recorded.
fn load_routes(env: &Env) -> Routes {
    load_state(env).0
}

// Records the routes and added workspaces, keeping the rest of the record.
fn save_state(env: &Env, routes: &Routes, workspaces: &[String]) -> Result<(), String> {
    let map: serde_json::Map<String, Value> = routes.iter().map(|(p, u)| (p.to_string(), json!(u))).collect();
    let routes = (!map.is_empty()).then(|| json!(map));
    let workspaces = (!workspaces.is_empty()).then(|| json!(workspaces));
    update_state(env, &[("routes", routes), ("workspaces", workspaces)])
}

// Sets or removes keys of setup's record, removing the file when nothing is left.
fn update_state(env: &Env, keys: &[(&str, Option<Value>)]) -> Result<(), String> {
    let p = env.state_path();
    let mut v: Value =
        serde_json::from_str(&std::fs::read_to_string(&p).unwrap_or_default()).unwrap_or(json!({}));
    if !v.is_object() {
        v = json!({});
    }
    let m = v.as_object_mut().expect("an object");
    for (k, val) in keys {
        match val {
            Some(val) => m.insert(k.to_string(), val.clone()),
            None => m.remove(*k),
        };
    }
    write_or_remove(&p, &format!("{}\n", serde_json::to_string_pretty(&v).unwrap_or_default()))
}

// Records the options of the last --apply, so --off and --on can undo and redo exactly that; None forgets them.
fn save_applied(env: &Env, opts: Option<&Opts>) -> Result<(), String> {
    let v = opts.map(|o| {
        json!({
            "clients": o.clients.iter().map(|c| c.name()).collect::<Vec<_>>(),
            "repo": o.repo,
            "workspaces": o.workspaces,
            "hook": o.hook,
            "hook_edits": o.hook_edits,
            "wrap": o.wrap,
        })
    });
    update_state(env, &[("applied", v)])
}

// Reads the options of the last --apply, if setup recorded them.
fn load_applied(env: &Env, action: Action) -> Option<Opts> {
    let text = std::fs::read_to_string(env.state_path()).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    let a = v.get("applied")?;
    let paths = |k: &str| {
        a[k].as_array().into_iter().flatten().filter_map(Value::as_str).map(PathBuf::from).collect::<Vec<_>>()
    };
    Some(Opts {
        action,
        clients: a["clients"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|c| Client::parse(c.as_str()?))
            .collect(),
        repo: a["repo"].as_str().map(PathBuf::from),
        workspaces: paths("workspaces"),
        hook: a["hook"].as_bool().unwrap_or(false),
        hook_edits: a["hook_edits"].as_bool().unwrap_or(false),
        wrap: a["wrap"].as_bool().unwrap_or(true),
        probe: false,
        sets: Vec::new(),
        brief: false,
    })
}

// Returns the local URL an HTTP proxy on a port serves.
fn local_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}/mcp")
}

// Returns a string quoted for /bin/sh.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

// Returns the file name of a command, such as `daemon` for `/opt/homebrew/libexec/zazzle/daemon`.
fn base(command: &str) -> &str {
    command.rsplit('/').next().unwrap_or(command)
}

// True for a server entry that is zazzle itself: `zazzle daemon mcp`, or a shell that changes folder and runs it.
fn is_zazzle(s: &Server) -> bool {
    match &s.kind {
        Kind::Stdio { command, args } => {
            base(command) == "daemon" && args.first().is_some_and(|a| a == "mcp")
                || base(command) == "sh"
                    && args.iter().any(|a| a.contains("daemon") && a.trim_end().ends_with("mcp"))
        }
        _ => false,
    }
}

// Returns the original command of a stdio server setup put behind the proxy (`zazzle daemon proxy .. -- CMD ARGS`).
fn wrapped(s: &Server) -> Option<(String, Vec<String>)> {
    let Kind::Stdio { command, args } = &s.kind else { return None };
    if !matches!(base(command), "daemon" | "zazzle") || args.first().map(String::as_str) != Some("proxy") {
        return None;
    }
    let at = args.iter().position(|a| a == "--")?;
    let (cmd, rest) = args[at + 1..].split_first()?;
    Some((cmd.clone(), rest.to_vec()))
}

// Returns the zazzle entry's command for a client: the desktop app starts servers in `/`, so with a repository its entry changes folder first.
fn zazzle_command(env: &Env, c: Client, repo: Option<&Path>) -> (String, Vec<String>) {
    let z = env.daemon.to_string_lossy().to_string();
    match (c, repo) {
        (Client::ClaudeDesktop, Some(r)) => (
            "/bin/sh".into(),
            vec!["-c".into(), format!("cd {} && exec {} mcp", sh_quote(&r.to_string_lossy()), sh_quote(&z))],
        ),
        _ => (z, vec!["mcp".into()]),
    }
}

// Plans one client's changes for --apply (or for --undo).
fn plan_client(env: &Env, opts: &Opts, c: Client, routes: &mut Routes) -> Result<ClientPlan, String> {
    let file = ConfigFile::load(c, &env.config_path(c))?;
    let servers = file.servers();
    let mut ops = Vec::new();
    let undo = opts.action == Action::Undo;
    let first = file.tables()[0].clone();
    let has_zazzle = servers.iter().any(|s| s.table == first && is_zazzle(s));
    if undo {
        if has_zazzle {
            ops.push(Op::RemoveZazzle { table: first.clone() });
        }
        if c == Client::Codex && has_zazzle {
            ops.push(Op::UndoDirectCalls);
        }
    } else {
        if !has_zazzle {
            let (command, args) = zazzle_command(env, c, opts.repo.as_deref());
            ops.push(Op::AddZazzle { table: first.clone(), command, args });
        }
        if c == Client::Codex && (!has_zazzle || file.codex_polls()) {
            ops.push(Op::DirectCalls);
        }
    }
    for s in servers.into_iter().filter(|s| !is_zazzle(s)) {
        if let Some((command, args)) = wrapped(&s) {
            if undo {
                ops.push(Op::Unwrap { server: s, command, args });
            } else {
                ops.push(Op::Leave { server: s, why: "already behind the proxy".into() });
            }
            continue;
        }
        let routed = match &s.kind {
            Kind::Http { url, .. } => routes.iter().find(|(p, _)| local_url(*p) == *url).cloned(),
            _ => None,
        };
        if let Some((_, upstream)) = routed {
            match undo {
                true => ops.push(Op::Unroute { server: s, url: upstream }),
                false => {
                    ops.push(Op::Leave { server: s, why: "already routed through a local proxy".into() })
                }
            }
            continue;
        }
        if undo {
            continue;
        }
        let why = match &s.kind {
            _ if !opts.wrap => Some("--no-wrap".to_string()),
            Kind::Stdio { .. } => None,
            Kind::Http { auth: false, .. } => Some(
                "no headers or token in the entry, so it signs in with OAuth, which the proxy cannot carry"
                    .into(),
            ),
            Kind::Http { .. } if !env.macos => {
                Some("HTTP servers are routed through LaunchAgents, which only macOS has".into())
            }
            Kind::Http { .. } => None,
            Kind::Other(why) => Some(why.clone()),
        };
        match (why, &s.kind) {
            (Some(why), _) => ops.push(Op::Leave { server: s, why }),
            (None, Kind::Http { url, .. }) => {
                let port = match routes.iter().find(|(_, u)| u == url) {
                    Some((p, _)) => *p,
                    None => {
                        let p = free_port(routes);
                        routes.push((p, url.clone()));
                        p
                    }
                };
                ops.push(Op::Route { server: s, port });
            }
            (None, _) => ops.push(Op::Wrap { server: s }),
        }
    }
    let instructions = env.instructions_path(c);
    let hook = hook_spec(env, c, opts.hook_edits).filter(|_| opts.hook || undo);
    Ok(ClientPlan { client: c, file, ops, instructions, hook })
}

// Returns the first port from 8809 that no route uses and nothing listens on.
fn free_port(routes: &Routes) -> u16 {
    (FIRST_PORT..u16::MAX)
        .find(|p| {
            !routes.iter().any(|(q, _)| q == p) && std::net::TcpListener::bind(("127.0.0.1", *p)).is_ok()
        })
        .unwrap_or(FIRST_PORT)
}

// Describes a server for the report: its name, transport and where it sits.
fn describe(s: &Server) -> String {
    let what = match &s.kind {
        Kind::Stdio { .. } => "stdio",
        Kind::Http { .. } => "http",
        Kind::Other(_) => "other",
    };
    match s.table.first().map(String::as_str) {
        Some("projects") => format!("{} ({what}, project {})", s.name, s.table[1]),
        _ => format!("{} ({what})", s.name),
    }
}

// Returns the command line a wrapped stdio server runs.
fn wrap_args(s: &Server) -> Vec<String> {
    let Kind::Stdio { command, args } = &s.kind else { return Vec::new() };
    let mut out = vec!["proxy".to_string(), "--log".into(), "--".into(), command.clone()];
    out.extend(args.iter().cloned());
    out
}

// Writes one plan or result line per change.
fn report_ops(env: &Env, plan: &ClientPlan, done: bool, out: &mut String) {
    let verb = |planned: &'static str, made: &'static str| if done { made } else { planned };
    for op in &plan.ops {
        let line = match op {
            Op::AddZazzle { command, args, .. } => {
                let scope = match plan.client {
                    Client::ClaudeCode => "user scope: every project, started in the project's folder",
                    Client::ClaudeDesktop => "started in the folder given with --repo",
                    Client::Codex => "started in Codex's working folder",
                };
                format!("+ {} zazzle: {} {} ({scope})", verb("add", "added"), command, args.join(" "))
            }
            Op::Wrap { server } => format!(
                "~ {} {}: {} {}",
                verb("wrap", "wrapped"),
                describe(server),
                env.daemon.display(),
                wrap_args(server).join(" ")
            ),
            Op::Route { server, port } => format!(
                "~ {} {} through {} (a LaunchAgent runs `zazzle daemon proxy --upstream` there; headers pass through)",
                verb("route", "routed"),
                describe(server),
                local_url(*port)
            ),
            Op::Leave { server, why } => format!("= leave {}: {why}", describe(server)),
            Op::Unwrap { server, command, .. } => {
                format!("~ {} {}: back to {command}", verb("unwrap", "unwrapped"), describe(server))
            }
            Op::Unroute { server, url } => format!("~ {} {}: back to {url}", verb("unroute", "unrouted"), describe(server)),
            Op::RemoveZazzle { .. } => format!("- {} zazzle", verb("remove", "removed")),
            Op::DirectCalls => format!(
                "+ {} Codex call zazzle's tools directly with a 120 s tool timeout (features.code_mode.direct_only_tool_namespaces, mcp_servers.zazzle.tool_timeout_sec), so a long call returns one result instead of being polled",
                verb("let", "let")
            ),
            Op::UndoDirectCalls => {
                format!("- {} mcp__zazzle from features.code_mode.direct_only_tool_namespaces", verb("remove", "removed"))
            }
        };
        out.push_str(&format!("  {line}\n"));
    }
    if plan.ops.is_empty() {
        out.push_str("  no servers\n");
    }
}

// Checks the folders given with --workspace and --repo and returns them canonical, or says which one cannot be a workspace.
fn wanted_workspaces(env: &Env, opts: &Opts) -> Result<Vec<PathBuf>, String> {
    let mut out: Vec<PathBuf> = Vec::new();
    for w in opts.workspaces.iter().chain(opts.repo.iter()) {
        let real = w.canonicalize().map_err(|_| format!("--workspace {}: not a directory", w.display()))?;
        if !real.is_dir() {
            return Err(format!("--workspace {}: not a directory", w.display()));
        }
        if crate::workspace::too_broad(&real, &env.home) {
            return Err(format!(
                "--workspace {}: too broad; name the folder that holds your repositories, such as ~/dev",
                w.display()
            ));
        }
        if !out.contains(&real) {
            out.push(real);
        }
    }
    Ok(out)
}

// Plans, and with --apply or --undo makes, the workspace lines: adds the wanted folders, or removes the lines setup added.
fn workspaces(
    env: &Env,
    opts: &Opts,
    added: &mut Vec<String>,
    backups: &mut Vec<PathBuf>,
    out: &mut String,
) -> Result<(), String> {
    let file = crate::workspace::file(&env.home);
    let text = std::fs::read_to_string(&file).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    let new = match opts.action {
        Action::Undo => {
            let gone: Vec<&str> =
                lines.iter().copied().filter(|l| added.iter().any(|a| a == l.trim())).collect();
            gone.iter().for_each(|l| out.push_str(&format!("  - remove workspace {l}\n")));
            added.clear();
            lines.iter().copied().filter(|l| !gone.contains(l)).map(|l| format!("{l}\n")).collect::<String>()
        }
        _ => {
            let mut t = text.clone();
            for w in wanted_workspaces(env, opts)? {
                let line = w.display().to_string();
                if lines.iter().any(|l| l.trim() == line) {
                    continue;
                }
                let verb = if opts.action == Action::Apply { "added" } else { "add" };
                out.push_str(&format!(
                    "  + {verb} workspace {}: zazzle may edit files below it\n",
                    env.short(&w)
                ));
                if !t.is_empty() && !t.ends_with('\n') {
                    t.push('\n');
                }
                t.push_str(&format!("{line}\n"));
                added.push(line);
            }
            t
        }
    };
    let entries = |t: &str| {
        t.lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .collect::<Vec<_>>()
    };
    let (after, before) = (entries(&new), entries(&text));
    if after.is_empty() {
        out.push_str("  none: edits stay inside the folder a chat starts in and its git work tree (add one with --workspace DIR, such as ~/dev)\n");
    }
    for w in after.iter().filter(|w| before.contains(w)) {
        out.push_str(&format!("  = workspace {}\n", env.short(Path::new(w))));
    }
    if new != text && opts.action != Action::Plan {
        backup(&file, backups)?;
        write_or_remove(&file, &new)?;
    }
    Ok(())
}

// Finds MCP servers setup does not change: those plugins provide, and those in repositories' shared project files.
pub fn unmanaged(env: &Env, opts: &Opts) -> Vec<String> {
    let mut out = Vec::new();
    let plugins = env.home.join(".claude/plugins");
    for f in files_named(&plugins, &[".mcp.json", "plugin.json"], 6) {
        let v: Value =
            serde_json::from_str(&std::fs::read_to_string(&f).unwrap_or_default()).unwrap_or_default();
        let servers = v.get("mcpServers").unwrap_or(&v);
        for name in servers.as_object().into_iter().flatten().filter(|(_, s)| s.is_object()).map(|(n, _)| n) {
            out.push(format!("{name} (Claude Code plugin, {}): a plugin update would overwrite a change, so it stays direct", env.short(&f)));
        }
    }
    if let Ok(t) = std::fs::read_to_string(env.config_path(Client::Codex)) {
        if let Ok(doc) = t.parse::<toml_edit::DocumentMut>() {
            for (plugin, p) in
                doc.get("plugins").and_then(|p| p.as_table_like()).into_iter().flat_map(|t| t.iter())
            {
                let servers =
                    p.as_table_like().and_then(|t| t.get("mcp_servers")).and_then(|m| m.as_table_like());
                for (name, _) in servers.into_iter().flat_map(|m| m.iter()) {
                    out.push(format!(
                        "{name} (Codex plugin {plugin}): the plugin starts it, so it stays direct"
                    ));
                }
            }
        }
    }
    let roots: Vec<PathBuf> =
        opts.repo.iter().cloned().chain(crate::workspace::configured(&env.home)).collect();
    for root in roots {
        for dir in std::iter::once(root.clone()).chain(crate::cmd::structure::repos_below(&root)) {
            for (file, client) in [(".mcp.json", Client::ClaudeCode), (".codex/config.toml", Client::Codex)] {
                let Ok(f) = ConfigFile::load(client, &dir.join(file)) else { continue };
                for s in f.servers().iter().filter(|s| !is_zazzle(s)) {
                    out.push(format!(
                        "{} ({}): shared with the repository's other users, so it stays direct",
                        describe(s),
                        env.short(&dir.join(file))
                    ));
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

// Lists files with one of the names below a folder, up to a depth, without following links.
fn files_named(dir: &Path, names: &[&str], depth: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return out };
    for e in entries.flatten() {
        let Ok(t) = e.file_type() else { continue };
        let name = e.file_name().to_string_lossy().into_owned();
        if t.is_file() && names.contains(&name.as_str()) {
            out.push(e.path());
        } else if t.is_dir() && depth > 1 && name != "node_modules" {
            out.extend(files_named(&e.path(), names, depth - 1));
        }
    }
    out.sort();
    out
}

// Writes the hook line for one client's plan.
fn report_hook(env: &Env, h: &HookSpec, c: Client, action: Action, out: &mut String) {
    let present = hook_command(&h.path);
    let line = match (action, present.as_deref()) {
        (Action::Undo, Some(_)) => format!("- remove zazzle's hook from {}", env.short(&h.path)),
        (Action::Undo, None) => return,
        (_, Some(p)) if p == h.command => format!("= hook in {}: {}", env.short(&h.path), h.command),
        (a, p) => format!(
            "{} the hook in {} ({}): {}",
            match (a, p) {
                (Action::Apply, None) => "+ added",
                (Action::Apply, Some(_)) => "~ updated",
                (_, None) => "+ add",
                _ => "~ update",
            },
            env.short(&h.path),
            h.matcher,
            h.command
        ),
    };
    out.push_str(&format!("  {line}\n"));
    if c == Client::Codex && action != Action::Undo {
        out.push_str("    Codex runs a new or changed hook only once you trust it: `/hooks` in the CLI, Settings > Hooks in the app\n");
    }
}

// Runs setup and returns its report.
pub fn run(opts: &Opts, env: &Env) -> Result<String, String> {
    let applied = |action| {
        load_applied(env, action).ok_or("setup has no record of an --apply to turn off or on: run `zazzle setup --apply` with your options first")
    };
    let mut set_note = String::new();
    if !opts.sets.is_empty() {
        let p = policy::path(&env.home);
        let text = std::fs::read_to_string(&p).unwrap_or_else(|_| policy::TEMPLATE.to_string());
        let new = policy::set(&text, &opts.sets)?;
        match opts.action {
            Action::Apply => {
                write_file(&p, &new)?;
                set_note = format!("policy: set {} in {}\n", opts.sets.join(", "), env.short(&p));
            }
            Action::Plan => {
                return Ok(format!(
                "zazzle setup --set: {} would become:\n{new}\nAdd --apply to write it and apply the setup.",
                env.short(&p)
            ))
            }
            _ => return Err("--set goes with --apply (or alone, to preview the policy)".into()),
        }
    }
    match opts.action {
        Action::Explain => {
            Ok(policy::explain(&policy::load(&env.home)?, &env.short(&policy::path(&env.home))))
        }
        Action::Check if opts.brief => Ok(drift(env)),
        Action::Check => check::run(opts, env),
        Action::Verify => verify::run(opts, env),
        Action::Report => check::report(env),
        Action::Off => {
            let a = applied(Action::Undo)?;
            let out = change(&a, env)?;
            save_applied(env, Some(&a))?;
            Ok(format!("zazzle setup --off: zazzle is out of the way, as if it was never set up; `zazzle setup --on` puts back the same setup.\n{out}"))
        }
        Action::On => {
            let a = applied(Action::Apply)?;
            Ok(format!("zazzle setup --on: the setup from before --off is back.\n{}", change(&a, env)?))
        }
        Action::Uninstall => {
            let undo = Opts {
                action: Action::Undo,
                ..load_applied(env, Action::Undo).unwrap_or_else(|| Opts {
                    action: Action::Undo,
                    clients: opts.clients.clone(),
                    repo: None,
                    workspaces: vec![],
                    hook: false,
                    hook_edits: false,
                    wrap: true,
                    probe: false,
                    sets: Vec::new(),
                    brief: false,
                })
            };
            let mut out = change(&undo, env)?;
            save_applied(env, None)?;
            if env.macos {
                let msg = crate::launchd::uninstall(&env.home, env.uid, &|a| (env.launchctl)(a))?;
                out.push_str(&format!("\ndaemon's LaunchAgent: {msg}\n"));
            }
            out.push_str(&format!(
                "\nzazzle is no longer configured anywhere. To remove the programs too:\n  brew uninstall zazzle    (or delete {}, the folder holding it and the zazzle and z launchers)\n  rm -rf ~/.zazzle          (the activity log, workspaces and caches; your backups stay beside each file)\n",
                env.daemon.display()
            ));
            Ok(out)
        }
        _ => Ok(format!("{set_note}{}", change(opts, env)?)),
    }
}

// Plans, applies or undoes the setup and returns its report; --apply records its options for --off and --on, --undo forgets them.
fn change(opts: &Opts, env: &Env) -> Result<String, String> {
    if let Some(r) = &opts.repo {
        if !r.is_dir() {
            return Err(format!("--repo {}: not a directory", r.display()));
        }
    }
    let repo = opts.repo.as_ref().map(|r| r.canonicalize().unwrap_or_else(|_| r.clone()));
    let opts = Opts {
        repo,
        clients: opts.clients.clone(),
        workspaces: opts.workspaces.clone(),
        sets: Vec::new(),
        brief: false,
        ..*opts
    };
    let (mut routes, mut added) = load_state(env);
    let chosen: Vec<Client> = if opts.clients.is_empty() {
        Client::ALL.into_iter().filter(|c| env.installed(*c)).collect()
    } else {
        opts.clients.clone()
    };
    let apply = opts.action != Action::Plan;
    let mut out = String::new();
    out.push_str(match opts.action {
        Action::Plan => "zazzle setup: the plan; nothing is changed yet.\n",
        Action::Apply => "zazzle setup --apply: changes made.\n",
        _ => "zazzle setup --undo: changes undone.\n",
    });
    out.push_str(&format!("daemon: {} ({})\n", env.daemon.display(), crate::cmd::VERSION));
    match &env.indexer {
        Some(ix) => out.push_str(&format!("C# indexer: {}\n", ix.display())),
        None => out.push_str("C# indexer: not found beside the daemon or on PATH, so C# reads come back as source and the structure tools fail\n"),
    }
    match &opts.repo {
        Some(r) => out.push_str(&format!("repository for the Claude desktop app: {}\n", r.display())),
        None => out.push_str("no --repo: the Claude desktop app starts servers in /, so pass --repo DIR to give its zazzle a folder\n"),
    }
    let mut backups = Vec::new();
    let policy = policy::load(&env.home)?;
    let block = block_text(&policy.extra);
    if apply && opts.action == Action::Apply && policy::ensure(&env.home)? {
        out.push_str(&format!(
            "policy: wrote {} (nothing in it is owned until you name a key)\n",
            env.short(&policy::path(&env.home))
        ));
    }
    out.push_str(&format!("\nworkspaces ({}):\n", env.short(&crate::workspace::file(&env.home))));
    workspaces(env, &opts, &mut added, &mut backups, &mut out)?;
    for c in Client::ALL.into_iter().filter(|c| !chosen.contains(c)) {
        out.push_str(&format!(
            "\n{}: not installed, skipped (name it with --client to configure it anyway)\n",
            c.name()
        ));
    }
    for c in chosen {
        let mut plan = plan_client(env, &opts, c, &mut routes)?;
        out.push_str(&format!("\n{}: {}\n", c.name(), env.short(&plan.file.path)));
        report_ops(env, &plan, apply, &mut out);
        let (instr, hook) = (plan.instructions.clone(), plan.hook.clone());
        if let Some(p) = &instr {
            let text = std::fs::read_to_string(p).unwrap_or_default();
            let now = current_block(&text).map(|(a, b)| &text[a..b]);
            match (opts.action, now) {
                (Action::Undo, Some(_)) => {
                    out.push_str(&format!("  - remove zazzle's instructions from {}\n", env.short(p)))
                }
                (Action::Plan | Action::Apply, None) => out.push_str(&format!(
                    "  + {} zazzle's instructions to {}\n",
                    if apply { "added" } else { "add" },
                    env.short(p)
                )),
                (Action::Plan | Action::Apply, Some(b)) if b != block => out.push_str(&format!(
                    "  ~ {} zazzle's instructions in {}\n",
                    if apply { "updated" } else { "update" },
                    env.short(p)
                )),
                _ => {}
            }
        }
        match &hook {
            Some(h) => report_hook(env, h, c, opts.action, &mut out),
            None if c == Client::ClaudeDesktop => out.push_str(
                "  = no hook: the desktop app's chat has none, so it relies on zazzle's instructions\n",
            ),
            None => out.push_str(
                "  = no hook (--no-hook was given; without it raw searches, reads and builds go to zazzle)\n",
            ),
        }
        if apply {
            apply_plan(env, &mut plan, &mut backups)?;
            if let Some(p) = instr {
                edit_block(
                    p.as_path(),
                    (opts.action != Action::Undo).then_some(block.as_str()),
                    &mut backups,
                )?;
            }
            if let Some(h) = &hook {
                edit_hook(&h.path, (opts.action != Action::Undo).then_some(h), &mut backups)?;
                if c == Client::ClaudeCode {
                    let cmd = format!("{} setup --check --brief", env.daemon.display());
                    edit_session_start(
                        &h.path,
                        (opts.action != Action::Undo).then_some(cmd.as_str()),
                        &mut backups,
                    )?;
                }
            }
        }
        if c == Client::ClaudeCode {
            session_keys(env, &policy, opts.action, apply, &mut backups, &mut out)?;
        }
    }
    let direct = unmanaged(env, &opts);
    if !direct.is_empty() {
        out.push_str("\nnot changed (they stay direct, outside zazzle's proxy):\n");
        direct.iter().for_each(|d| out.push_str(&format!("  = {d}\n")));
    }
    if apply {
        let used: Routes = if opts.action == Action::Undo { Vec::new() } else { routes.clone() };
        let stale: Routes = load_routes(env).into_iter().filter(|r| !used.contains(r)).collect();
        for (port, url) in routes.iter().filter(|_| opts.action == Action::Apply && env.macos) {
            agent(env, *port, url, true)?;
        }
        for (port, url) in stale.iter().filter(|_| env.macos) {
            agent(env, *port, url, false)?;
        }
        save_state(env, &used, &added)?;
        save_applied(env, (opts.action == Action::Apply).then_some(&opts))?;
        update_state(
            env,
            &[("applied_version", (opts.action == Action::Apply).then(|| json!(crate::cmd::VERSION)))],
        )?;
        if !backups.is_empty() {
            out.push_str("\nbackups:\n");
            backups.iter().for_each(|b: &PathBuf| out.push_str(&format!("  {}\n", env.short(b))));
        }
        out.push_str("\nNext: restart Claude Code, the Claude desktop app and Codex (they read their settings at start), trust the hook in Codex if asked, then run `zazzle setup --check` and `zazzle setup --verify`.\n");
    } else {
        out.push_str("\nNext, for an agent: show this plan to the user and ask before applying. Quit Claude Code (terminal, VS Code and desktop), the Claude desktop app and Codex first, since they rewrite their settings files, then run `zazzle setup --apply` with the same options. `zazzle setup --undo` reverses it.\n");
    }
    Ok(out)
}

// Re-applies the last --apply when this daemon is a different release from the one that applied it, so an upgrade keeps the managed setup current; development builds never do.
pub fn reapply_after_upgrade(env: &Env, version: &str) -> Result<Option<String>, String> {
    let state: Value = serde_json::from_str(&std::fs::read_to_string(env.state_path()).unwrap_or_default())
        .unwrap_or(json!({}));
    let Some(applied) = load_applied(env, Action::Apply) else { return Ok(None) };
    if version.contains("-dev") || state["applied_version"].as_str() == Some(version) {
        return Ok(None);
    }
    let mut out = change(&applied, env)?;
    let block = block_text(&policy::load(&env.home)?.extra);
    for p in stale_blocks(env, &block) {
        edit_block(&p, Some(&block), &mut Vec::new())?;
        out.push_str(&format!("~ updated zazzle's instructions in {}\n", env.short(&p)));
    }
    update_state(env, &[("applied_version", Some(json!(version)))])?;
    Ok(Some(out))
}

// Returns the instruction files of every client, applied or not, that hold a zazzle block other than this one, so one version's guidance is never left beside another's.
fn stale_blocks(env: &Env, block: &str) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for p in Client::ALL.into_iter().filter_map(|c| env.instructions_path(c)) {
        let text = std::fs::read_to_string(&p).unwrap_or_default();
        if current_block(&text).is_some_and(|(a, b)| &text[a..b] != block) && !out.contains(&p) {
            out.push(p);
        }
    }
    out
}

// Says in one line how the clients differ from the last --apply (instructions block, hooks, session keys), or nothing when they match; the SessionStart hook prints it into the agent's context.
fn drift(env: &Env) -> String {
    let Some(applied) = load_applied(env, Action::Apply) else { return String::new() };
    let Ok(policy) = policy::load(&env.home) else {
        return "zazzle setup drift: ~/.zazzle/policy.toml does not parse. Tell the user.".into();
    };
    let block = block_text(&policy.extra);
    let mut found = Vec::new();
    let clients: Vec<Client> = match applied.clients.is_empty() {
        true => Client::ALL.into_iter().filter(|c| env.installed(*c)).collect(),
        false => applied.clients.clone(),
    };
    for c in clients {
        if let Some(p) = env.instructions_path(c) {
            let text = std::fs::read_to_string(&p).unwrap_or_default();
            match current_block(&text).map(|(a, b)| &text[a..b]) {
                None => found.push(format!("no zazzle instructions in {}", env.short(&p))),
                Some(b) if b != block => {
                    found.push(format!("zazzle's instructions in {} are out of date", env.short(&p)))
                }
                _ => {}
            }
        }
        if let Some(h) = hook_spec(env, c, applied.hook_edits).filter(|_| applied.hook) {
            if hook_command(&h.path).as_deref() != Some(h.command.as_str()) {
                found.push(format!("the {} hook in {} is missing or changed", c.name(), env.short(&h.path)));
            }
        }
        if c == Client::ClaudeCode {
            let settings: Value = serde_json::from_str(
                &std::fs::read_to_string(env.home.join(".claude/settings.json")).unwrap_or_default(),
            )
            .unwrap_or(json!({}));
            for (k, v) in &policy.claude {
                if policy::get(&settings, k).as_ref() != Some(v) {
                    found.push(format!("{k} is not {v}"));
                }
            }
        }
    }
    for p in stale_blocks(env, &block) {
        let note = format!("zazzle's instructions in {} are out of date", env.short(&p));
        if !found.contains(&note) {
            found.push(note);
        }
    }
    match found.is_empty() {
        true => String::new(),
        false => format!(
            "zazzle setup drift: {}. Tell the user; their zazzle setup no longer matches what they applied.",
            found.join("; ")
        ),
    }
}

// Sets the Claude Code session keys the policy names (or, on undo, restores them) and reports each change.
fn session_keys(
    env: &Env,
    policy: &policy::Policy,
    action: Action,
    apply: bool,
    backups: &mut Vec<PathBuf>,
    out: &mut String,
) -> Result<(), String> {
    let path = env.home.join(".claude/settings.json");
    let state: Value = serde_json::from_str(&std::fs::read_to_string(env.state_path()).unwrap_or_default())
        .unwrap_or(json!({}));
    let owned = state["session_keys"].as_object().cloned().unwrap_or_default();
    let want = if action == Action::Undo { serde_json::Map::new() } else { policy.claude.clone() };
    if want.is_empty() && owned.is_empty() {
        return Ok(());
    }
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let mut v: Value = if text.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(&text).map_err(|e| format!("{}: not valid JSON: {e}", path.display()))?
    };
    let before = v.clone();
    let (lines, record) = policy::apply_keys(&mut v, &want, &owned);
    out.push_str(&format!(
        "  session keys in {} (from {}):\n",
        env.short(&path),
        env.short(&policy::path(&env.home))
    ));
    lines.iter().for_each(|l| out.push_str(&format!("    {l}\n")));
    if apply {
        if v != before {
            backup(&path, backups)?;
            write_or_remove(&path, &format!("{}\n", serde_json::to_string_pretty(&v).unwrap_or_default()))?;
        }
        update_state(env, &[("session_keys", (!record.is_empty()).then(|| Value::Object(record)))])?;
    }
    Ok(())
}

// Returns the managed block's text: zazzle's instructions and the policy's extra text, between the markers.
fn block_text(extra: &str) -> String {
    let extra = if extra.trim().is_empty() { String::new() } else { format!("\n{}", extra.trim()) };
    format!("{BLOCK_BEGIN}\n## zazzle\n{INSTRUCTIONS}{extra}\n{BLOCK_END}")
}

// Returns the managed block a file holds now, if any.
fn current_block(text: &str) -> Option<(usize, usize)> {
    text.find(BLOCK_BEGIN)
        .zip(text.find(BLOCK_END))
        .filter(|(a, b)| a < b)
        .map(|(a, b)| (a, b + BLOCK_END.len()))
}

// Writes a file, creating its folder.
fn write_file(path: &Path, text: &str) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

// Copies a file to `PATH.zazzle-backup-SECONDS` before setup changes it, once per run.
fn backup(path: &Path, backups: &mut Vec<PathBuf>) -> Result<(), String> {
    if !path.exists() || backups.iter().any(|b| b.to_string_lossy().starts_with(&*path.to_string_lossy())) {
        return Ok(());
    }
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let to = PathBuf::from(format!("{}.zazzle-backup-{secs}", path.display()));
    std::fs::copy(path, &to).map_err(|e| format!("{}: {e}", to.display()))?;
    backups.push(to);
    Ok(())
}

// Makes a client's planned changes to its configuration file.
fn apply_plan(env: &Env, plan: &mut ClientPlan, backups: &mut Vec<PathBuf>) -> Result<(), String> {
    let z = env.daemon.to_string_lossy().to_string();
    let mut changed = false;
    for op in &plan.ops {
        changed |= !matches!(op, Op::Leave { .. });
        match op {
            Op::AddZazzle { table, command, args } => plan.file.put_stdio(table, "zazzle", command, args),
            Op::RemoveZazzle { table } => plan.file.remove(table, "zazzle"),
            Op::DirectCalls => plan.file.set_codex_direct(),
            Op::UndoDirectCalls => plan.file.unset_codex_direct(),
            Op::Wrap { server } => plan.file.set_command(&server.table, &server.name, &z, &wrap_args(server)),
            Op::Unwrap { server, command, args } => {
                plan.file.set_command(&server.table, &server.name, command, args)
            }
            Op::Route { server, port } => plan.file.set_url(&server.table, &server.name, &local_url(*port)),
            Op::Unroute { server, url } => plan.file.set_url(&server.table, &server.name, url),
            Op::Leave { .. } => {}
        }
    }
    if changed {
        backup(&plan.file.path, backups)?;
        write_file(&plan.file.path, &plan.file.text())?;
    }
    Ok(())
}

// Puts zazzle's managed block (`want`) into a markdown file between markers, replacing an older one, or removes it when `want` is None; text outside the markers is kept as it is.
fn edit_block(path: &Path, want: Option<&str>, backups: &mut Vec<PathBuf>) -> Result<(), String> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let new = match (current_block(&text), want) {
        (Some((a, end)), None) => {
            let end = if text[end..].starts_with('\n') { end + 1 } else { end };
            format!("{}{}", &text[..a], &text[end..])
        }
        (Some((a, end)), Some(block)) if text[a..end] != *block => {
            format!("{}{block}{}", &text[..a], &text[end..])
        }
        (None, Some(block)) => {
            let sep = if text.is_empty() || text.ends_with("\n\n") {
                ""
            } else if text.ends_with('\n') {
                "\n"
            } else {
                "\n\n"
            };
            format!("{text}{sep}{block}\n")
        }
        _ => return Ok(()),
    };
    backup(path, backups)?;
    write_or_remove(path, &new)
}

// True for a PreToolUse entry setup wrote: `zazzle daemon hook`, or the C# indexer's hook from earlier versions.
fn our_hook(e: &Value) -> bool {
    e["hooks"].as_array().is_some_and(|hs| {
        hs.iter().any(|h| {
            h["command"].as_str().is_some_and(|c| {
                c.contains("daemon") && c.contains(" hook --client ")
                    || c.contains("csharp-indexer") && c.ends_with(" hook")
            })
        })
    })
}

// Puts zazzle's PreToolUse hook into a hooks file (Claude Code settings or Codex hooks.json), replacing an older one, or removes it when `want` is None.
fn edit_hook(path: &Path, want: Option<&HookSpec>, backups: &mut Vec<PathBuf>) -> Result<(), String> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let mut v: Value = if text.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(&text).map_err(|e| format!("{}: not valid JSON: {e}", path.display()))?
    };
    if !v.is_object() {
        v = json!({});
    }
    let pre = &mut v["hooks"]["PreToolUse"];
    if !pre.is_array() {
        *pre = json!([]);
    }
    let list = pre.as_array_mut().expect("an array");
    let entry = want.map(|h| json!({"matcher": h.matcher, "hooks": [{"type": "command", "command": h.command, "timeout": 30}]}));
    let ours: Vec<Value> = list.iter().filter(|e| our_hook(e)).cloned().collect();
    if ours == entry.iter().cloned().collect::<Vec<_>>() {
        return Ok(());
    }
    list.retain(|e| !our_hook(e));
    list.extend(entry);
    if list.is_empty() {
        v["hooks"].as_object_mut().expect("an object").remove("PreToolUse");
    }
    if want.is_none_or(|h| h.matcher == CLAUDE_MATCHER) {
        let compact = &mut v["hooks"]["PostCompact"];
        let mut kept: Vec<Value> =
            compact.as_array().cloned().unwrap_or_default().into_iter().filter(|e| !our_hook(e)).collect();
        kept.extend(
            want.map(|h| json!({"hooks": [{"type": "command", "command": h.command, "timeout": 10}]})),
        );
        match kept.is_empty() {
            true => {
                v["hooks"].as_object_mut().expect("an object").remove("PostCompact");
            }
            false => *compact = Value::Array(kept),
        }
    }
    if v["hooks"].as_object().is_some_and(|h| h.is_empty()) {
        v.as_object_mut().expect("an object").remove("hooks");
    }
    backup(path, backups)?;
    write_or_remove(path, &format!("{}\n", serde_json::to_string_pretty(&v).unwrap_or_default()))
}

// True for the SessionStart entry setup wrote: `zazzle daemon setup --check --brief`.
fn our_session_start(e: &Value) -> bool {
    e["hooks"].as_array().is_some_and(|hs| {
        hs.iter().any(|h| {
            h["command"]
                .as_str()
                .is_some_and(|c| c.contains("daemon") && c.ends_with(" setup --check --brief"))
        })
    })
}

// Puts the SessionStart hook that reports setup drift into Claude Code's settings, or removes it when `want` is None.
fn edit_session_start(path: &Path, want: Option<&str>, backups: &mut Vec<PathBuf>) -> Result<(), String> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let mut v: Value = if text.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(&text).map_err(|e| format!("{}: not valid JSON: {e}", path.display()))?
    };
    if !v.is_object() {
        v = json!({});
    }
    let entry = want.map(|c| json!({"hooks": [{"type": "command", "command": c, "timeout": 10}]}));
    let list: Vec<Value> = v["hooks"]["SessionStart"].as_array().cloned().unwrap_or_default();
    let ours: Vec<Value> = list.iter().filter(|e| our_session_start(e)).cloned().collect();
    if ours == entry.iter().cloned().collect::<Vec<_>>() {
        return Ok(());
    }
    let mut list: Vec<Value> = list.into_iter().filter(|e| !our_session_start(e)).collect();
    list.extend(entry);
    if !v["hooks"].is_object() {
        v["hooks"] = json!({});
    }
    let hooks = v["hooks"].as_object_mut().expect("an object");
    match list.is_empty() {
        true => hooks.remove("SessionStart"),
        false => hooks.insert("SessionStart".into(), Value::Array(list)),
    };
    if hooks.is_empty() {
        v.as_object_mut().expect("an object").remove("hooks");
    }
    backup(path, backups)?;
    write_or_remove(path, &format!("{}\n", serde_json::to_string_pretty(&v).unwrap_or_default()))
}

// Writes a file, or removes it when nothing of substance is left (`{}` or blank), as when undo empties a file setup created.
fn write_or_remove(path: &Path, text: &str) -> Result<(), String> {
    match matches!(text.trim(), "" | "{}") {
        true => std::fs::remove_file(path).or_else(|e| match e.kind() {
            std::io::ErrorKind::NotFound => Ok(()),
            _ => Err(format!("{}: {e}", path.display())),
        }),
        false => write_file(path, text),
    }
}

// Returns the command of zazzle's hook in a hooks file, if it has one.
fn hook_command(path: &Path) -> Option<String> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    let entry = v["hooks"]["PreToolUse"].as_array()?.iter().find(|e| our_hook(e))?.clone();
    entry["hooks"].as_array()?.iter().find_map(|h| h["command"].as_str().map(String::from))
}

// Starts (or stops) the LaunchAgent that keeps an HTTP proxy for one upstream running on a local port.
fn agent(env: &Env, port: u16, url: &str, start: bool) -> Result<(), String> {
    let label = format!("com.zazzle.proxy.{port}");
    let args = ["proxy", "--log", "--upstream", url, "--listen", &format!("127.0.0.1:{port}")];
    let log = env.home.join(format!(".zazzle/proxy-{port}.log"));
    let plist = crate::launchd::agent_plist(&label, &env.daemon, &args, &log);
    let run = |a: &[&str]| (env.launchctl)(a);
    match start {
        true => crate::launchd::install_agent(&label, &plist, &env.home, env.uid, &run).map(|_| ()),
        false => crate::launchd::uninstall_agent(&label, &env.home, env.uid, &run).map(|_| ()),
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests;
