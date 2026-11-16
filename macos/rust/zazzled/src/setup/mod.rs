// `zazzle setup`: plans, applies, undoes and checks the user-level configuration that makes Claude Code, the Claude desktop app and Codex use zazzle, and sends their other MCP servers through the zazzle proxy.
pub mod check;
pub mod clients;

use clients::{Client, ConfigFile, Kind, Server};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub const USAGE: &str = "usage: zazzle setup [--apply | --undo | --check] [--client claude-code|claude-desktop|codex].. [--repo DIR] [--hook] [--no-wrap]";
const FIRST_PORT: u16 = 8809;
const BLOCK_BEGIN: &str = "<!-- zazzle setup: begin -->";
const BLOCK_END: &str = "<!-- zazzle setup: end -->";
const HOOK_MATCHER: &str = "Bash|Grep|Glob|Read";

// What agents are told: in the MCP handshake, and in the instruction files setup writes.
pub const INSTRUCTIONS: &str = "\
Use zazzle (MCP server `zazzle`) to read, search, list and edit files instead of shell grep, rg, find, cat, sed or the built-in Read and Grep tools; its answers are shorter, capped and deduplicated per session.
- `zazzle_read` gives code as a terse view for many languages (C# from zazzle-csharp-indexer) with source line numbers and no sha; read with `source: true` for the exact code and the sha that `zazzle_edit` needs.
- `zazzle_find`, `zazzle_refs`, `zazzle_outline` and `zazzle_ls` locate code; read a range or one symbol rather than whole files.
- For C# structure: `zazzle_overview` (start here on an unfamiliar repository), `zazzle_find_symbol`, `zazzle_changed_since`, `zazzle_git_log`, `zazzle_git_blame`. In a folder above several repositories they cover all of them; pass `repo` to pick one.
- A reply tagged `[r:ID]` comes back exactly with `zazzle_read_again`; keep ids in notes instead of content.
- Say what you needed and could not get with `zazzle_report_gap`. If zazzle seems misconfigured, run `zazzle setup --check` and show the user its report.";

// What setup was asked to do.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Action {
    Plan,
    Apply,
    Undo,
    Check,
}

// The parsed `zazzle setup` command line.
#[derive(Debug, PartialEq)]
pub struct Opts {
    pub action: Action,
    pub clients: Vec<Client>,
    pub repo: Option<PathBuf>,
    pub hook: bool,
    pub wrap: bool,
}

// Parses the arguments after `zazzled setup`.
pub fn parse(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts { action: Action::Plan, clients: Vec::new(), repo: None, hook: false, wrap: true };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut action = |x: Action| {
            if o.action != Action::Plan {
                return Err(format!("give one of --apply, --undo and --check\n{USAGE}"));
            }
            o.action = x;
            Ok(())
        };
        match a.as_str() {
            "--apply" => action(Action::Apply)?,
            "--undo" => action(Action::Undo)?,
            "--check" => action(Action::Check)?,
            "--hook" => o.hook = true,
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
        let zazzled = zazzled.canonicalize().unwrap_or(zazzled);
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
        p.exists()
            || p.parent().is_some_and(Path::exists)
            || c == Client::ClaudeCode && self.home.join(".claude").exists()
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
    pub hook: Option<PathBuf>,
}

// HTTP servers setup routed: local port to upstream URL.
type Routes = Vec<(u16, String)>;

// Reads the routes setup recorded.
fn load_routes(env: &Env) -> Routes {
    let text = std::fs::read_to_string(env.state_path()).unwrap_or_default();
    let v: Value = serde_json::from_str(&text).unwrap_or(json!({}));
    let routes = v.get("routes").and_then(Value::as_object);
    routes
        .map(|m| m.iter().filter_map(|(p, u)| Some((p.parse().ok()?, u.as_str()?.to_string()))).collect())
        .unwrap_or_default()
}

// Records the routes, or removes the record when there are none.
fn save_routes(env: &Env, routes: &Routes) -> Result<(), String> {
    let p = env.state_path();
    if routes.is_empty() {
        let _ = std::fs::remove_file(&p);
        return Ok(());
    }
    let map: serde_json::Map<String, Value> = routes.iter().map(|(p, u)| (p.to_string(), json!(u))).collect();
    write_file(
        &p,
        &format!("{}\n", serde_json::to_string_pretty(&json!({"routes": map})).unwrap_or_default()),
    )
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
    let hook =
        (c == Client::ClaudeCode && (opts.hook || undo)).then(|| env.home.join(".claude/settings.json"));
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

// Runs setup and returns its report.
pub fn run(opts: &Opts, env: &Env) -> Result<String, String> {
    if opts.action == Action::Check {
        return check::run(opts, env);
    }
    if let Some(r) = &opts.repo {
        if !r.is_dir() {
            return Err(format!("--repo {}: not a directory", r.display()));
        }
    }
    let repo = opts.repo.as_ref().map(|r| r.canonicalize().unwrap_or_else(|_| r.clone()));
    let opts =
        Opts { action: opts.action, clients: opts.clients.clone(), repo, hook: opts.hook, wrap: opts.wrap };
    let mut routes = load_routes(env);
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
        if let Some(p) = &hook {
            let verb = match (opts.action, hook_present(p), &env.indexer) {
                (Action::Undo, true, _) => Some("- remove"),
                (Action::Apply, false, Some(_)) => Some("+ added"),
                (Action::Plan, false, Some(_)) => Some("+ add"),
                (Action::Plan | Action::Apply, false, None) => {
                    out.push_str("  = no hook: it comes with the C# indexer, which is not installed\n");
                    None
                }
                _ => None,
            };
            if let Some(verb) = verb {
                out.push_str(&format!(
                    "  {verb} the hook that sends grep, cat, Glob and Read to zazzle, in {}\n",
                    env.short(p)
                ));
            }
        }
        if apply {
            apply_plan(env, &mut plan, &mut backups)?;
            if let Some(p) = instr {
                edit_block(p.as_path(), opts.action == Action::Undo, &mut backups)?;
            }
            match (hook, opts.action, &env.indexer) {
                (Some(p), Action::Undo, _) => edit_hook(&p, Path::new(""), true, &mut backups)?,
                (Some(p), _, Some(ix)) => edit_hook(&p, ix, false, &mut backups)?,
                _ => {}
            }
        }
    }
    if let Some(r) = &opts.repo {
        let shared = r.join(".mcp.json");
        if let Ok(f) = ConfigFile::load(Client::ClaudeCode, &shared) {
            let names: Vec<String> = f.servers().iter().filter(|s| !is_zazzle(s)).map(describe).collect();
            if !names.is_empty() {
                out.push_str(&format!(
                    "\nnot changed, since it may be shared with your team: {}: {}\n",
                    shared.display(),
                    names.join(", ")
                ));
            }
        }
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
        save_routes(env, &used)?;
        if !backups.is_empty() {
            out.push_str("\nbackups:\n");
            backups.iter().for_each(|b: &PathBuf| out.push_str(&format!("  {}\n", env.short(b))));
        }
        out.push_str("\nNext: restart Claude Code, the Claude desktop app and Codex (they read their settings at start), then run `zazzle setup --check`.\n");
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
    write_file(path, &new)
}

// Adds the indexer's PreToolUse hook to Claude Code's user settings, or removes it.
fn edit_hook(path: &Path, indexer: &Path, remove: bool, backups: &mut Vec<PathBuf>) -> Result<(), String> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let mut v: Value = if text.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(&text).map_err(|e| format!("{}: not valid JSON: {e}", path.display()))?
    };
    let ours = |e: &Value| {
        e["hooks"].as_array().is_some_and(|hs| {
            hs.iter().any(|h| {
                h["command"]
                    .as_str()
                    .is_some_and(|c| c.contains("zazzle-csharp-indexer") && c.ends_with(" hook"))
            })
        })
    };
    if !v.is_object() {
        v = json!({});
    }
    let pre = &mut v["hooks"]["PreToolUse"];
    if !pre.is_array() {
        *pre = json!([]);
    }
    let list = pre.as_array_mut().expect("an array");
    let had = list.iter().any(ours);
    list.retain(|e| !ours(e));
    if had != remove {
        return Ok(());
    }
    if !remove {
        let command = format!("{} hook", indexer.display());
        list.push(json!({"matcher": HOOK_MATCHER, "hooks": [{"type": "command", "command": command}]}));
    }
    backup(path, backups)?;
    write_file(path, &format!("{}\n", serde_json::to_string_pretty(&v).unwrap_or_default()))
}

// True when Claude Code's settings file has the indexer's hook.
fn hook_present(path: &Path) -> bool {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    text.contains("zazzle-csharp-indexer") && text.contains(" hook\"")
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
