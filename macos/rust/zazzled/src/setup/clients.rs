// The MCP clients `zazzle setup` configures (Claude Code, the Claude desktop app, Codex), where each keeps its servers, and reading and editing those files while keeping everything else they hold.
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use toml_edit::{Array, DocumentMut, Item, Table};

// An MCP client whose user-level configuration setup edits.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Client {
    ClaudeCode,
    ClaudeDesktop,
    Codex,
}

impl Client {
    pub const ALL: [Client; 3] = [Client::ClaudeCode, Client::ClaudeDesktop, Client::Codex];

    // Returns the name used on the command line and in reports.
    pub fn name(self) -> &'static str {
        match self {
            Client::ClaudeCode => "claude-code",
            Client::ClaudeDesktop => "claude-desktop",
            Client::Codex => "codex",
        }
    }

    // Returns the client a command-line name stands for.
    pub fn parse(s: &str) -> Option<Client> {
        Client::ALL.into_iter().find(|c| c.name() == s)
    }
}

// What a server entry runs: a command speaking stdio, a Streamable HTTP URL (with static auth when the entry carries headers or a bearer token variable), or a transport the proxy cannot carry.
#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    Stdio { command: String, args: Vec<String> },
    Http { url: String, auth: bool },
    Other(String),
}

// One configured MCP server: the table it sits in, its name, what it runs, and the environment, headers and bearer-token variable needed to start or call it.
#[derive(Debug, Clone, PartialEq)]
pub struct Server {
    pub table: Vec<String>,
    pub name: String,
    pub kind: Kind,
    pub env: Vec<(String, String)>,
    pub headers: Vec<(String, String)>,
    pub bearer: Option<String>,
}

// A configuration file's parsed contents: JSON for the Claude apps, TOML for Codex.
#[derive(Debug, Clone)]
pub enum Doc {
    Json(Value),
    Toml(DocumentMut),
}

// One client's configuration file, as read.
#[derive(Debug, Clone)]
pub struct ConfigFile {
    pub client: Client,
    pub path: PathBuf,
    pub exists: bool,
    pub doc: Doc,
}

// Returns a JSON object's string pairs, such as `env` or `headers`.
fn json_pairs(v: Option<&Value>) -> Vec<(String, String)> {
    let obj = v.and_then(Value::as_object);
    obj.map(|o| o.iter().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string()))).collect())
        .unwrap_or_default()
}

// Returns a TOML table's string pairs, inline or not.
fn toml_pairs(v: Option<&Item>) -> Vec<(String, String)> {
    let Some(t) = v.and_then(Item::as_table_like) else { return Vec::new() };
    t.iter().filter_map(|(k, v)| Some((k.to_string(), v.as_str()?.to_string()))).collect()
}

impl ConfigFile {
    // Reads a client's file, or an empty document when it does not exist yet.
    pub fn load(client: Client, path: &Path) -> Result<ConfigFile, String> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => Some(t),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        let bad = |e: String| {
            format!(
                "{}: not valid {}: {e}",
                path.display(),
                if client == Client::Codex { "TOML" } else { "JSON" }
            )
        };
        let doc = match (client, &text) {
            (Client::Codex, Some(t)) => Doc::Toml(t.parse::<DocumentMut>().map_err(|e| bad(e.to_string()))?),
            (Client::Codex, None) => Doc::Toml(DocumentMut::new()),
            (_, Some(t)) if t.trim().is_empty() => Doc::Json(json!({})),
            (_, Some(t)) => Doc::Json(serde_json::from_str(t).map_err(|e| bad(e.to_string()))?),
            (_, None) => Doc::Json(json!({})),
        };
        Ok(ConfigFile { client, path: path.to_path_buf(), exists: text.is_some(), doc })
    }

    // Returns the tables that hold servers: `mcpServers` (and Claude Code's per-project `projects.*.mcpServers`), or Codex's `mcp_servers`; the first is where new entries go.
    pub fn tables(&self) -> Vec<Vec<String>> {
        match &self.doc {
            Doc::Toml(_) => vec![vec!["mcp_servers".into()]],
            Doc::Json(v) => {
                let mut out = vec![vec!["mcpServers".to_string()]];
                if self.client == Client::ClaudeCode {
                    if let Some(projects) = v.get("projects").and_then(Value::as_object) {
                        for (p, cfg) in projects {
                            if cfg.get("mcpServers").and_then(Value::as_object).is_some_and(|m| !m.is_empty())
                            {
                                out.push(vec!["projects".into(), p.clone(), "mcpServers".into()]);
                            }
                        }
                    }
                }
                out
            }
        }
    }

    // Returns every configured server, in file order.
    pub fn servers(&self) -> Vec<Server> {
        let mut out = Vec::new();
        for table in self.tables() {
            match &self.doc {
                Doc::Json(v) => {
                    let Some(m) = json_at(v, &table).and_then(Value::as_object) else { continue };
                    for (name, e) in m {
                        out.push(Server {
                            table: table.clone(),
                            name: name.clone(),
                            kind: json_kind(e),
                            env: json_pairs(e.get("env")),
                            headers: json_pairs(e.get("headers")),
                            bearer: None,
                        });
                    }
                }
                Doc::Toml(d) => {
                    let Some(t) = d.get("mcp_servers").and_then(Item::as_table_like) else { continue };
                    for (name, e) in t.iter() {
                        let Some(e) = e.as_table_like() else { continue };
                        out.push(Server {
                            table: table.clone(),
                            name: name.to_string(),
                            kind: toml_kind(e),
                            env: toml_pairs(e.get("env")),
                            headers: toml_pairs(e.get("http_headers")),
                            bearer: e.get("bearer_token_env_var").and_then(Item::as_str).map(str::to_string),
                        });
                    }
                }
            }
        }
        out
    }

    // Adds or replaces a stdio server with exactly this command and these arguments.
    pub fn put_stdio(&mut self, table: &[String], name: &str, command: &str, args: &[String]) {
        let claude_code = self.client == Client::ClaudeCode;
        match &mut self.doc {
            Doc::Json(v) => {
                let mut entry = Map::new();
                if claude_code {
                    entry.insert("type".into(), json!("stdio"));
                }
                entry.insert("command".into(), json!(command));
                entry.insert("args".into(), json!(args));
                json_table(v, table).insert(name.to_string(), Value::Object(entry));
            }
            Doc::Toml(d) => {
                let mut t = Table::new();
                t["command"] = toml_edit::value(command);
                t["args"] = toml_edit::value(args.iter().collect::<Array>());
                toml_table(d).insert(name, Item::Table(t));
            }
        }
    }

    // Changes a stdio server's command and arguments (no `args` when there are none), keeping its other settings (environment, timeouts).
    pub fn set_command(&mut self, table: &[String], name: &str, command: &str, args: &[String]) {
        match &mut self.doc {
            Doc::Json(v) => {
                if let Some(e) = json_table(v, table).get_mut(name).and_then(Value::as_object_mut) {
                    e.insert("command".into(), json!(command));
                    match args.is_empty() {
                        true => e.remove("args"),
                        false => e.insert("args".into(), json!(args)),
                    };
                }
            }
            Doc::Toml(d) => {
                if let Some(e) = toml_table(d).get_mut(name).and_then(Item::as_table_like_mut) {
                    e.insert("command", toml_edit::value(command));
                    match args.is_empty() {
                        true => e.remove("args"),
                        false => e.insert("args", toml_edit::value(args.iter().collect::<Array>())),
                    };
                }
            }
        }
    }

    // Changes an HTTP server's URL, keeping its headers and other settings.
    pub fn set_url(&mut self, table: &[String], name: &str, url: &str) {
        match &mut self.doc {
            Doc::Json(v) => {
                if let Some(e) = json_table(v, table).get_mut(name).and_then(Value::as_object_mut) {
                    e.insert("url".into(), json!(url));
                }
            }
            Doc::Toml(d) => {
                if let Some(e) = toml_table(d).get_mut(name).and_then(Item::as_table_like_mut) {
                    e.insert("url", toml_edit::value(url));
                }
            }
        }
    }

    // Removes a server.
    pub fn remove(&mut self, table: &[String], name: &str) {
        match &mut self.doc {
            Doc::Json(v) => {
                json_table(v, table).remove(name);
            }
            Doc::Toml(d) => {
                toml_table(d).remove(name);
            }
        }
    }

    // Returns the file's new text: indented JSON with a final newline, or the TOML document as edited.
    pub fn text(&self) -> String {
        match &self.doc {
            Doc::Json(v) => format!("{}\n", serde_json::to_string_pretty(v).unwrap_or_default()),
            Doc::Toml(d) => d.to_string(),
        }
    }
}

// Returns the JSON value at a path of object keys.
fn json_at<'v>(v: &'v Value, path: &[String]) -> Option<&'v Value> {
    path.iter().try_fold(v, |v, k| v.get(k))
}

// Returns the JSON object at a path, creating objects (and replacing non-objects) on the way.
fn json_table<'v>(v: &'v mut Value, path: &[String]) -> &'v mut Map<String, Value> {
    let mut at = v;
    for k in path {
        if !at.is_object() {
            *at = json!({});
        }
        let obj = at.as_object_mut().expect("an object");
        at = obj.entry(k.clone()).or_insert_with(|| json!({}));
    }
    if !at.is_object() {
        *at = json!({});
    }
    at.as_object_mut().expect("an object")
}

// Returns Codex's `mcp_servers` table, creating it as a plain parent of `[mcp_servers.NAME]` tables.
fn toml_table(d: &mut DocumentMut) -> &mut dyn toml_edit::TableLike {
    if d.get("mcp_servers").and_then(Item::as_table_like).is_none() {
        let mut t = Table::new();
        t.set_implicit(true);
        d["mcp_servers"] = Item::Table(t);
    }
    d["mcp_servers"].as_table_like_mut().expect("a table")
}

// Classifies a JSON server entry.
fn json_kind(e: &Value) -> Kind {
    let s = |k: &str| e.get(k).and_then(Value::as_str).map(str::to_string);
    if let Some(command) = s("command") {
        let args = e.get("args").and_then(Value::as_array);
        let args = args
            .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        return Kind::Stdio { command, args };
    }
    match (s("url"), s("type").as_deref()) {
        (Some(_), Some("sse")) => Kind::Other("SSE transport, which the proxy does not carry".into()),
        (Some(url), _) => Kind::Http { url, auth: !json_pairs(e.get("headers")).is_empty() },
        _ => Kind::Other("no command or url".into()),
    }
}

// Classifies a Codex server table.
fn toml_kind(e: &dyn toml_edit::TableLike) -> Kind {
    let s = |k: &str| e.get(k).and_then(Item::as_str).map(str::to_string);
    if let Some(command) = s("command") {
        let args = e.get("args").and_then(Item::as_array);
        let args = args
            .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        return Kind::Stdio { command, args };
    }
    match s("url") {
        Some(url) => {
            let auth = s("bearer_token_env_var").is_some()
                || !toml_pairs(e.get("http_headers")).is_empty()
                || !toml_pairs(e.get("env_http_headers")).is_empty();
            Kind::Http { url, auth }
        }
        None => Kind::Other("no command or url".into()),
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    // Checks reading Claude Code's file: user and project tables, stdio, HTTP with and without headers, SSE and broken entries.
    #[test]
    fn test_claude_code_servers() {
        let t = TempDir::new();
        let p = t.path().join(".claude.json");
        std::fs::write(
            &p,
            r#"{"numStartups": 3, "mcpServers": {"gh": {"type": "stdio", "command": "npx", "args": ["-y", "gh"], "env": {"TOKEN": "x"}},
               "jira": {"type": "http", "url": "https://jira/mcp"}, "api": {"type": "http", "url": "https://api/mcp", "headers": {"Authorization": "Bearer y"}},
               "old": {"type": "sse", "url": "https://old/sse"}, "odd": {}},
               "projects": {"/w/app": {"mcpServers": {"db": {"command": "db-mcp"}}}, "/w/empty": {"mcpServers": {}}}}"#,
        )
        .expect("write");
        let f = ConfigFile::load(Client::ClaudeCode, &p).expect("load");
        let s = f.servers();
        let names: Vec<&str> = s.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["gh", "jira", "api", "old", "odd", "db"]);
        assert_eq!(s[0].kind, Kind::Stdio { command: "npx".into(), args: vec!["-y".into(), "gh".into()] });
        assert_eq!(s[0].env, [("TOKEN".to_string(), "x".to_string())]);
        assert_eq!(s[1].kind, Kind::Http { url: "https://jira/mcp".into(), auth: false });
        assert_eq!(s[2].kind, Kind::Http { url: "https://api/mcp".into(), auth: true });
        assert!(matches!(s[3].kind, Kind::Other(_)) && matches!(s[4].kind, Kind::Other(_)));
        assert_eq!(s[5].table, ["projects", "/w/app", "mcpServers"]);
        assert!(f.text().starts_with("{\n  \"numStartups\": 3,"), "keeps key order: {}", f.text());
    }

    // Checks editing JSON: adding, rewrapping a command while keeping env, changing a URL, removing, and a missing file.
    #[test]
    fn test_json_edits() {
        let t = TempDir::new();
        let p = t.path().join("claude_desktop_config.json");
        let mut f = ConfigFile::load(Client::ClaudeDesktop, &p).expect("missing is empty");
        assert!(!f.exists && f.servers().is_empty());
        let table = vec!["mcpServers".to_string()];
        f.put_stdio(&table, "zazzle", "/bin/zazzled", &["mcp".into()]);
        f.put_stdio(&table, "gh", "npx", &["gh".into()]);
        if let Doc::Json(v) = &mut f.doc {
            v["mcpServers"]["gh"]["env"] = json!({"T": "1"});
            v["mcpServers"]["web"] = json!({"url": "https://w/mcp"});
        }
        f.set_command(
            &table,
            "gh",
            "/bin/zazzled",
            &["proxy".into(), "--".into(), "npx".into(), "gh".into()],
        );
        f.set_url(&table, "web", "http://127.0.0.1:8809/mcp");
        f.set_command(&table, "absent", "x", &[]);
        f.put_stdio(&table, "bare", "b", &["x".into()]);
        f.set_command(&table, "bare", "b", &[]);
        assert!(!f.text().contains("\"x\""), "no empty args left: {}", f.text());
        f.remove(&table, "bare");
        f.set_url(&table, "absent", "x");
        let s = f.servers();
        assert_eq!(s[1].env.len(), 1);
        assert!(matches!(&s[1].kind, Kind::Stdio { args, .. } if args[0] == "proxy"));
        assert!(matches!(&s[2].kind, Kind::Http { url, .. } if url.contains("8809")));
        f.remove(&table, "web");
        assert_eq!(f.servers().len(), 2);
        assert!(!f.text().contains("\"type\""), "desktop entries carry no type");
        let mut v = json!({"mcpServers": 3});
        json_table(&mut v, &table).insert("a".into(), json!(1));
        let mut w = json!(5);
        json_table(&mut w, &table);
        assert_eq!(w, json!({"mcpServers": {}}));
    }

    // Checks Codex's TOML: reading stdio and HTTP servers with each auth form, editing them, and keeping comments.
    #[test]
    fn test_codex_toml() {
        let t = TempDir::new();
        let p = t.path().join("config.toml");
        std::fs::write(&p, "# my settings\nmodel = \"o\"\n\n[mcp_servers.gh]\ncommand = \"npx\"\nargs = [\"gh\"]\nenv = { T = \"1\" }\n\n[mcp_servers.linear]\nurl = \"https://l/mcp\"\nbearer_token_env_var = \"LINEAR\"\n\n[mcp_servers.open]\nurl = \"https://o/mcp\"\n\n[mcp_servers.hdr]\nurl = \"https://h/mcp\"\nhttp_headers = { X = \"y\" }\n\n[mcp_servers.bad]\nx = 1\n\n[mcp_servers]\nnum = 5\n").expect("write");
        let mut f = ConfigFile::load(Client::Codex, &p).expect("load");
        let s = f.servers();
        assert_eq!(s.len(), 5);
        assert_eq!(s[0].env, [("T".to_string(), "1".to_string())]);
        assert!(matches!(&s[1].kind, Kind::Http { auth: true, .. }));
        assert!(matches!(&s[2].kind, Kind::Http { auth: false, .. }));
        assert!(matches!(&s[3].kind, Kind::Http { auth: true, .. }) && s[3].headers.len() == 1);
        assert!(matches!(s[4].kind, Kind::Other(_)));
        let table = f.tables()[0].clone();
        f.set_command(&table, "missing", "x", &[]);
        f.set_url(&table, "missing", "u");
        assert!(!f.text().contains("missing"));
        f.put_stdio(&table, "zazzle", "/bin/zazzled", &["mcp".into()]);
        f.set_command(&table, "gh", "/bin/zazzled", &["proxy".into(), "--".into(), "npx".into()]);
        f.set_url(&table, "open", "http://127.0.0.1:8809/mcp");
        f.remove(&table, "bad");
        f.set_command(&table, "linear", "c", &[]);
        let text = f.text();
        assert!(text.starts_with("# my settings\nmodel = \"o\"\n"), "{text}");
        assert!(
            text.contains("[mcp_servers.zazzle]\ncommand = \"/bin/zazzled\"\nargs = [\"mcp\"]"),
            "{text}"
        );
        assert!(text.contains("env = { T = \"1\" }") && !text.contains("[mcp_servers.bad]"), "{text}");
        let mut empty = ConfigFile::load(Client::Codex, &t.path().join("none.toml")).expect("empty");
        empty.put_stdio(&table, "zazzle", "z", &["mcp".into()]);
        assert_eq!(empty.text(), "[mcp_servers.zazzle]\ncommand = \"z\"\nargs = [\"mcp\"]\n");
    }

    // Checks unreadable and invalid files, an empty JSON file, and client names.
    #[test]
    fn test_load_errors_and_names() {
        let t = TempDir::new();
        let p = t.path().join("bad.json");
        std::fs::write(&p, "{nope").expect("write");
        assert!(ConfigFile::load(Client::ClaudeDesktop, &p).expect_err("bad").contains("not valid JSON"));
        std::fs::write(&p, "= =").expect("write");
        assert!(ConfigFile::load(Client::Codex, &p).expect_err("bad").contains("not valid TOML"));
        std::fs::write(&p, "  \n").expect("write");
        assert!(ConfigFile::load(Client::ClaudeDesktop, &p).expect("empty").servers().is_empty());
        assert!(ConfigFile::load(Client::Codex, t.path()).is_err());
        assert!(Client::ALL.iter().all(|c| Client::parse(c.name()) == Some(*c)));
        assert_eq!(Client::parse("vscode"), None);
    }
}
