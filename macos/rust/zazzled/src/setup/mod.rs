// `zazzle setup`: plans, applies, undoes and checks the user-level configuration that makes Claude Code, the Claude desktop app and Codex use zazzle, and sends their other MCP servers through the zazzle proxy.
pub mod check;
pub mod clients;
pub mod verify;

use clients::{Client, ConfigFile, Kind, Server};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub const USAGE: &str = "usage: zazzle setup [--apply | --undo | --check | --verify | --report] [--client claude-code|claude-desktop|codex].. [--repo DIR] [--workspace DIR].. [--hook] [--hook-edits] [--no-wrap]";
const FIRST_PORT: u16 = 8809;
const BLOCK_BEGIN: &str = "<!-- zazzle setup: begin -->";
const BLOCK_END: &str = "<!-- zazzle setup: end -->";
// The tools each client's hook sees: shell commands and search, read and edit tools (edits are logged, or refused with --hook-edits).
const CLAUDE_MATCHER: &str = "Bash|Grep|Glob|Read|Edit|Write|MultiEdit|NotebookEdit";
const CODEX_MATCHER: &str = "Bash|apply_patch";

// What agents are told: in the MCP handshake, and in the instruction files setup writes.
pub const INSTRUCTIONS: &str = "\
Use zazzle (MCP server `zazzle`) to read, search, list and edit files instead of shell grep, rg, find, cat, sed or the built-in Read and Grep tools; its answers are shorter, capped and deduplicated per session.
- `zazzle_read` gives code as a terse view for many languages (C# from zazzle-csharp-indexer) with source line numbers and no sha; read with `source: true` for the exact code and the sha that `zazzle_edit` needs.
- `zazzle_find`, `zazzle_refs`, `zazzle_outline` and `zazzle_ls` locate code; read a range or one symbol rather than whole files.
- For C# structure: `zazzle_overview` (start here on an unfamiliar repository), `zazzle_find_symbol`, `zazzle_changed_since`, `zazzle_git_log`, `zazzle_git_blame`. In a folder above several repositories they cover all of them; pass `repo` to pick one.
- A reply tagged `[r:ID]` comes back exactly with `zazzle_read_again`; keep ids in notes instead of content.
- Run builds and tests as `zazzle run -- CMD` (for example `zazzle run -- cargo test`): the same command in the same shell with the same exit code, its output cleaned. When repository instructions name a command such as `cargo test`, run exactly that command behind `zazzle run --`.
- Edits may go anywhere in the git work tree you started in, in folders the client names and in the user's workspaces (`~/.zazzle/workspaces`).
- If a zazzle tool is missing or fails, say so and use the native tool; where the hook refuses a raw command, `# allow-raw` at its end lets it through (logged).
- Say what you needed and could not get with `zazzle_report_gap`. If zazzle seems misconfigured, run `zazzle setup --check` and show the user its report.";

// What setup was asked to do.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Action {
    Plan,
    Apply,
    Undo,
    Check,
    Verify,
    Report,
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
}

// Parses the arguments after `zazzled setup`.
pub fn parse(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts {
        action: Action::Plan,
        clients: Vec::new(),
        repo: None,
        workspaces: Vec::new(),
        hook: false,
        hook_edits: false,
        wrap: true,
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut action = |x: Action| {
            if o.action != Action::Plan {
                return Err(format!("give one of --apply, --undo, --check, --verify and --report\n{USAGE}"));
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
            "--hook" => o.hook = true,
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

// Where setup reads and writes: the home directory, this zazzled, the C# indexer, Codex's home, and whether launchd can keep HTTP proxies running.
#[derive(Debug, Clone)]
pub struct Env {
    pub home: PathBuf,
    pub zazzled: PathBuf,
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
        let zazzled = std::env::current_exe().map_err(|e| format!("cannot find zazzled: {e}"))?;
        let zazzled = stable_exe(&zazzled.canonicalize().unwrap_or(zazzled));
        let indexer = crate::indexer::Indexer::locate().ok().map(|ix| ix.exe);
        let codex_home =
            std::env::var_os("CODEX_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".codex"));
        // SAFETY: getuid has no preconditions.
        let uid = unsafe { libc::getuid() };
        Ok(Env {
            home,
            zazzled,
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
fn stable_exe(p: &Path) -> PathBuf {
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
    let z = env.zazzled.to_string_lossy();
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

// Records the routes and added workspaces, or removes the record when there are none.
fn save_state(env: &Env, routes: &Routes, workspaces: &[String]) -> Result<(), String> {
    let p = env.state_path();
    if routes.is_empty() && workspaces.is_empty() {
        let _ = std::fs::remove_file(&p);
        return Ok(());
    }
    let map: serde_json::Map<String, Value> = routes.iter().map(|(p, u)| (p.to_string(), json!(u))).collect();
    let v = json!({"routes": map, "workspaces": workspaces});
    write_file(&p, &format!("{}\n", serde_json::to_string_pretty(&v).unwrap_or_default()))
}

// Returns the local URL an HTTP proxy on a port serves.
fn local_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}/mcp")
}

// Returns a string quoted for /bin/sh.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

// Returns the file name of a command, such as `zazzled` for `/opt/homebrew/bin/zazzled`.
fn base(command: &str) -> &str {
    command.rsplit('/').next().unwrap_or(command)
}

// True for a server entry that is zazzle itself: `zazzled mcp`, or a shell that changes folder and runs it.
fn is_zazzle(s: &Server) -> bool {
    match &s.kind {
        Kind::Stdio { command, args } => {
            base(command) == "zazzled" && args.first().is_some_and(|a| a == "mcp")
                || base(command) == "sh"
                    && args.iter().any(|a| a.contains("zazzled") && a.trim_end().ends_with("mcp"))
        }
        _ => false,
    }
}

// Returns the original command of a stdio server setup put behind the proxy (`zazzled proxy .. -- CMD ARGS`).
fn wrapped(s: &Server) -> Option<(String, Vec<String>)> {
    let Kind::Stdio { command, args } = &s.kind else { return None };
    if !matches!(base(command), "zazzled" | "zazzle") || args.first().map(String::as_str) != Some("proxy") {
        return None;
    }
    let at = args.iter().position(|a| a == "--")?;
    let (cmd, rest) = args[at + 1..].split_first()?;
    Some((cmd.clone(), rest.to_vec()))
}

// Returns the zazzle entry's command for a client: the desktop app starts servers in `/`, so with a repository its entry changes folder first.
fn zazzle_command(env: &Env, c: Client, repo: Option<&Path>) -> (String, Vec<String>) {
    let z = env.zazzled.to_string_lossy().to_string();
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
    } else if !has_zazzle {
        let (command, args) = zazzle_command(env, c, opts.repo.as_deref());
        ops.push(Op::AddZazzle { table: first.clone(), command, args });
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
                env.zazzled.display(),
                wrap_args(server).join(" ")
            ),
            Op::Route { server, port } => format!(
                "~ {} {} through {} (a LaunchAgent runs `zazzled proxy --upstream` there; headers pass through)",
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
    let configured = crate::workspace::configured(&env.home);
    if configured.is_empty() && new == text {
        out.push_str("  none: edits stay inside the folder a chat starts in and its git work tree (add one with --workspace DIR, such as ~/dev)\n");
    }
    configured.iter().for_each(|w| out.push_str(&format!("  = workspace {}\n", env.short(w))));
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
    match opts.action {
        Action::Check => return check::run(opts, env),
        Action::Verify => return verify::run(opts, env),
        Action::Report => return check::report(env),
        _ => {}
    }
    if let Some(r) = &opts.repo {
        if !r.is_dir() {
            return Err(format!("--repo {}: not a directory", r.display()));
        }
    }
    let repo = opts.repo.as_ref().map(|r| r.canonicalize().unwrap_or_else(|_| r.clone()));
    let opts = Opts { repo, clients: opts.clients.clone(), workspaces: opts.workspaces.clone(), ..*opts };
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
    out.push_str(&format!("zazzled: {} ({})\n", env.zazzled.display(), crate::cmd::VERSION));
    match &env.indexer {
        Some(ix) => out.push_str(&format!("C# indexer: {}\n", ix.display())),
        None => out.push_str("C# indexer: not found beside zazzled or on PATH, so C# reads come back as source and the structure tools fail\n"),
    }
    match &opts.repo {
        Some(r) => out.push_str(&format!("repository for the Claude desktop app: {}\n", r.display())),
        None => out.push_str("no --repo: the Claude desktop app starts servers in /, so pass --repo DIR to give its zazzle a folder\n"),
    }
    let mut backups = Vec::new();
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
            let has = std::fs::read_to_string(p).unwrap_or_default().contains(BLOCK_BEGIN);
            match (opts.action, has) {
                (Action::Undo, true) => {
                    out.push_str(&format!("  - remove zazzle's instructions from {}\n", env.short(p)))
                }
                (Action::Plan | Action::Apply, false) => out.push_str(&format!(
                    "  + {} zazzle's instructions to {}\n",
                    if apply { "added" } else { "add" },
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
            None => {
                out.push_str("  = no hook (add --hook to send raw searches, reads and builds to zazzle)\n")
            }
        }
        if apply {
            apply_plan(env, &mut plan, &mut backups)?;
            if let Some(p) = instr {
                edit_block(p.as_path(), opts.action == Action::Undo, &mut backups)?;
            }
            if let Some(h) = &hook {
                edit_hook(&h.path, (opts.action != Action::Undo).then_some(h), &mut backups)?;
            }
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
    let z = env.zazzled.to_string_lossy().to_string();
    let mut changed = false;
    for op in &plan.ops {
        changed |= !matches!(op, Op::Leave { .. });
        match op {
            Op::AddZazzle { table, command, args } => plan.file.put_stdio(table, "zazzle", command, args),
            Op::RemoveZazzle { table } => plan.file.remove(table, "zazzle"),
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

// Adds zazzle's instructions to a markdown file between markers, or removes them.
fn edit_block(path: &Path, remove: bool, backups: &mut Vec<PathBuf>) -> Result<(), String> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let found = text.find(BLOCK_BEGIN).zip(text.find(BLOCK_END)).filter(|(a, b)| a < b);
    let new = match (found, remove) {
        (Some((a, b)), true) => {
            let end = b + BLOCK_END.len();
            let end = if text[end..].starts_with('\n') { end + 1 } else { end };
            format!("{}{}", &text[..a], &text[end..])
        }
        (None, false) => {
            let sep = if text.is_empty() || text.ends_with("\n\n") {
                ""
            } else if text.ends_with('\n') {
                "\n"
            } else {
                "\n\n"
            };
            format!("{text}{sep}{BLOCK_BEGIN}\n## zazzle\n{INSTRUCTIONS}\n{BLOCK_END}\n")
        }
        _ => return Ok(()),
    };
    backup(path, backups)?;
    write_or_remove(path, &new)
}

// True for a PreToolUse entry setup wrote: `zazzled hook`, or the C# indexer's hook from earlier versions.
fn our_hook(e: &Value) -> bool {
    e["hooks"].as_array().is_some_and(|hs| {
        hs.iter().any(|h| {
            h["command"].as_str().is_some_and(|c| {
                c.contains("zazzled") && c.contains(" hook --client ")
                    || c.contains("zazzle-csharp-indexer") && c.ends_with(" hook")
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
        if v["hooks"].as_object().is_some_and(|h| h.is_empty()) {
            v.as_object_mut().expect("an object").remove("hooks");
        }
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
    let plist = crate::launchd::agent_plist(&label, &env.zazzled, &args, &log);
    let run = |a: &[&str]| (env.launchctl)(a);
    match start {
        true => crate::launchd::install_agent(&label, &plist, &env.home, env.uid, &run).map(|_| ()),
        false => crate::launchd::uninstall_agent(&label, &env.home, env.uid, &run).map(|_| ()),
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests;
