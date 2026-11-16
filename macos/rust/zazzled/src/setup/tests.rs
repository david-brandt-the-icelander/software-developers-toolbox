// Tests for `zazzle setup`: planning, applying and undoing across Claude Code, the Claude desktop app and Codex in a temporary home folder.
use super::*;
use crate::testutil::TempDir;
use std::sync::Mutex;

static LAUNCHCTL: Mutex<Vec<String>> = Mutex::new(Vec::new());

// Records launchctl calls instead of running launchctl.
fn fake_launchctl(args: &[&str]) -> Result<(), String> {
    LAUNCHCTL.lock().expect("lock").push(args.join(" "));
    Ok(())
}

// Returns an environment rooted in a temporary home, with every client's folder present.
fn env(t: &TempDir, macos: bool) -> Env {
    let home = t.path().to_path_buf();
    let env = Env {
        home: home.clone(),
        zazzled: PathBuf::from("/opt/z/zazzled"),
        indexer: Some(PathBuf::from("/opt/z/zazzle-csharp-indexer")),
        codex_home: home.join(".codex"),
        macos,
        uid: 501,
        launchctl: fake_launchctl,
    };
    for c in Client::ALL {
        std::fs::create_dir_all(env.config_path(c).parent().expect("parent")).expect("mkdir");
    }
    std::fs::create_dir_all(home.join(".claude")).expect("mkdir");
    env
}

// Returns setup options from words.
fn opts(words: &[&str]) -> Opts {
    parse(&words.iter().map(|w| w.to_string()).collect::<Vec<_>>()).expect("parse")
}

// Writes the clients' files with a mix of servers.
fn seed(env: &Env) {
    let code = r#"{"numStartups": 1, "mcpServers": {"gh": {"type": "stdio", "command": "npx", "args": ["-y", "gh"]}, "jira": {"type": "http", "url": "https://jira/mcp"}, "api": {"type": "http", "url": "https://api/mcp", "headers": {"Authorization": "Bearer s3cret"}}}}"#;
    std::fs::write(env.config_path(Client::ClaudeCode), code).expect("write");
    std::fs::write(
        env.config_path(Client::ClaudeDesktop),
        r#"{"mcpServers": {"fs": {"command": "fs-mcp"}}}"#,
    )
    .expect("write");
    let codex =
        "model = \"o\"\n\n[mcp_servers.api]\nurl = \"https://api/mcp\"\nbearer_token_env_var = \"API\"\n";
    std::fs::write(env.config_path(Client::Codex), codex).expect("write");
    std::fs::write(env.home.join(".claude/CLAUDE.md"), "# Mine\n").expect("write");
}

// Checks option parsing and its errors.
#[test]
fn test_parse() {
    let o = opts(&["--apply", "--client", "codex", "--repo", "/r", "--hook", "--no-wrap"]);
    assert_eq!(
        o,
        Opts {
            action: Action::Apply,
            clients: vec![Client::Codex],
            repo: Some("/r".into()),
            hook: true,
            wrap: false
        }
    );
    let bad = |w: &[&str]| parse(&w.iter().map(|x| x.to_string()).collect::<Vec<_>>()).expect_err("bad");
    assert!(bad(&["--apply", "--undo"]).contains("give one of"));
    assert!(bad(&["--client", "vscode"]).contains("unknown client"));
    assert!(bad(&["--client"]).contains("needs a name"));
    assert!(bad(&["--repo"]).contains("needs a directory"));
    assert!(bad(&["--what"]).contains("unknown setup option"));
    assert_eq!(bad(&["--help"]), USAGE);
}

// Checks the plan changes nothing and says what it would do, including what it leaves alone and why.
#[test]
fn test_plan() {
    let t = TempDir::new();
    let env = env(&t, true);
    seed(&env);
    let before = std::fs::read_to_string(env.config_path(Client::ClaudeCode)).expect("read");
    let repo = t.path().join("repo");
    std::fs::create_dir_all(&repo).expect("mkdir");
    std::fs::write(repo.join(".mcp.json"), r#"{"mcpServers": {"team": {"command": "t"}}}"#).expect("write");
    let r = run(&opts(&["--repo", &repo.to_string_lossy(), "--hook"]), &env).expect("plan");
    assert!(
        r.starts_with("zazzle setup: the plan; nothing is changed yet.\nzazzled: /opt/z/zazzled ("),
        "{r}"
    );
    assert!(
        r.contains("\nclaude-code: ~/.claude.json\n  + add zazzle: /opt/z/zazzled mcp (user scope"),
        "{r}"
    );
    assert!(r.contains("  ~ wrap gh (stdio): /opt/z/zazzled proxy --log -- npx -y gh\n"), "{r}");
    assert!(
        r.contains("  = leave jira (http): no headers or token in the entry, so it signs in with OAuth"),
        "{r}"
    );
    assert!(r.contains("  ~ route api (http) through http://127.0.0.1:"), "{r}");
    assert!(r.contains("  + add zazzle's instructions to ~/.claude/CLAUDE.md\n  + add the hook"), "{r}");
    assert!(
        r.contains(&format!(
            "+ add zazzle: /bin/sh -c cd '{}' && exec '/opt/z/zazzled' mcp",
            repo.canonicalize().expect("canon").display()
        )),
        "{r}"
    );
    assert!(
        r.contains("not changed, since it may be shared with your team") && r.contains("team (stdio)"),
        "{r}"
    );
    assert!(r.contains("Next, for an agent: show this plan to the user and ask before applying."), "{r}");
    assert!(!r.contains("s3cret"), "never prints secrets");
    assert_eq!(std::fs::read_to_string(env.config_path(Client::ClaudeCode)).expect("read"), before);
    let codex_api = r.split("codex:").nth(1).expect("codex section");
    let code_port = r
        .split("route api (http) through http://127.0.0.1:")
        .nth(1)
        .expect("port")
        .get(..4)
        .expect("4 digits")
        .to_string();
    assert!(
        codex_api.contains(&format!("through http://127.0.0.1:{code_port}/mcp")),
        "one proxy per upstream: {r}"
    );
}

// Checks --apply writes every file with backups, starts the proxy agent, and a second run changes nothing; --undo restores the servers.
#[test]
fn test_apply_and_undo() {
    let t = TempDir::new();
    let env = env(&t, true);
    seed(&env);
    LAUNCHCTL.lock().expect("lock").clear();
    let r = run(&opts(&["--apply", "--hook"]), &env).expect("apply");
    assert!(
        r.contains("+ added zazzle")
            && r.contains("~ wrapped gh")
            && r.contains("backups:\n  ~/.claude.json.zazzle-backup-"),
        "{r}"
    );
    let code: Value =
        serde_json::from_str(&std::fs::read_to_string(env.config_path(Client::ClaudeCode)).expect("read"))
            .expect("json");
    assert_eq!(
        code["mcpServers"]["zazzle"],
        json!({"type": "stdio", "command": "/opt/z/zazzled", "args": ["mcp"]})
    );
    assert_eq!(code["mcpServers"]["gh"]["args"], json!(["proxy", "--log", "--", "npx", "-y", "gh"]));
    assert_eq!(code["mcpServers"]["jira"]["url"], json!("https://jira/mcp"));
    let port = code["mcpServers"]["api"]["url"].as_str().expect("url").to_string();
    assert!(port.starts_with("http://127.0.0.1:") && code["numStartups"] == json!(1));
    let codex = std::fs::read_to_string(env.config_path(Client::Codex)).expect("read");
    assert!(codex.starts_with("model = \"o\"\n") && codex.contains(&format!("url = \"{port}\"")), "{codex}");
    assert!(std::fs::read_to_string(env.home.join(".claude/CLAUDE.md"))
        .expect("md")
        .starts_with("# Mine\n\n<!-- zazzle setup: begin -->"));
    assert!(std::fs::read_to_string(env.codex_home.join("AGENTS.md"))
        .expect("md")
        .starts_with("<!-- zazzle setup: begin -->"));
    assert!(std::fs::read_to_string(env.home.join(".claude/settings.json"))
        .expect("settings")
        .contains("/opt/z/zazzle-csharp-indexer hook"));
    let routes = load_routes(&env);
    assert_eq!(routes.len(), 1);
    assert!(LAUNCHCTL.lock().expect("lock").iter().any(|c| c.starts_with("bootstrap gui/501 ")));
    let plist = env.home.join(format!("Library/LaunchAgents/com.zazzle.proxy.{}.plist", routes[0].0));
    assert!(std::fs::read_to_string(plist).expect("plist").contains("<string>https://api/mcp</string>"));

    let again = run(&opts(&["--apply", "--hook"]), &env).expect("again");
    assert!(
        again.contains("= leave gh (stdio): already behind the proxy") && again.contains("already routed"),
        "{again}"
    );
    assert!(!again.contains("+ added zazzle") && !again.contains("backups:"), "{again}");

    let undo = run(&opts(&["--undo"]), &env).expect("undo");
    assert!(
        undo.contains("- removed zazzle") && undo.contains("~ unwrapped gh (stdio): back to npx"),
        "{undo}"
    );
    assert!(
        undo.contains("~ unrouted api (http): back to https://api/mcp") && undo.contains("- remove the hook"),
        "{undo}"
    );
    let code: Value =
        serde_json::from_str(&std::fs::read_to_string(env.config_path(Client::ClaudeCode)).expect("read"))
            .expect("json");
    assert_eq!(code["mcpServers"]["gh"], json!({"type": "stdio", "command": "npx", "args": ["-y", "gh"]}));
    assert_eq!(code["mcpServers"]["api"]["url"], json!("https://api/mcp"));
    assert!(code["mcpServers"].get("zazzle").is_none());
    assert_eq!(std::fs::read_to_string(env.home.join(".claude/CLAUDE.md")).expect("md"), "# Mine\n\n");
    assert!(!std::fs::read_to_string(env.home.join(".claude/settings.json"))
        .expect("settings")
        .contains("hook\""));
    assert!(load_routes(&env).is_empty() && !env.state_path().exists());
    assert!(LAUNCHCTL
        .lock()
        .expect("lock")
        .iter()
        .any(|c| c.starts_with("bootout gui/501/com.zazzle.proxy.")));
}

// Checks what happens off macOS, without the indexer, with --no-wrap, with a client named but not installed, and without --repo.
#[test]
fn test_other_cases() {
    let t = TempDir::new();
    let mut env = env(&t, false);
    env.indexer = None;
    seed(&env);
    std::fs::remove_dir_all(env.codex_home.clone()).expect("rm codex");
    let r = run(&opts(&["--hook"]), &env).expect("plan");
    assert!(
        r.contains("C# indexer: not found")
            && r.contains("no --repo: the Claude desktop app starts servers in /"),
        "{r}"
    );
    assert!(!r.contains("route api") && r.contains("only macOS has"), "{r}");
    assert!(
        r.contains("\ncodex: not installed, skipped")
            && r.contains("= no hook: it comes with the C# indexer"),
        "{r}"
    );
    let r = run(&opts(&["--no-wrap", "--client", "codex"]), &env).expect("plan");
    assert!(r.contains("\ncodex: ~/.codex/config.toml\n  + add zazzle") && !r.contains("~ wrap"), "{r}");
    let r = run(&opts(&["--apply", "--no-wrap", "--client", "claude-desktop"]), &env).expect("apply");
    assert!(
        r.contains("= leave fs (stdio): --no-wrap") && r.contains("+ added zazzle: /opt/z/zazzled mcp"),
        "{r}"
    );
    assert!(run(&opts(&["--repo", "/no/such/repo"]), &env)
        .expect_err("bad repo")
        .contains("not a directory"));
    std::fs::write(env.config_path(Client::ClaudeDesktop), "{broken").expect("write");
    assert!(run(&opts(&["--client", "claude-desktop"]), &env).is_err());
}

// Checks the instructions block and hook edits on their own: idempotent, removable, and tolerant of odd files.
#[test]
fn test_block_and_hook() {
    let t = TempDir::new();
    let md = t.path().join("A.md");
    let mut backups = Vec::new();
    edit_block(&md, true, &mut backups).expect("remove from nothing");
    edit_block(&md, false, &mut backups).expect("add");
    edit_block(&md, false, &mut backups).expect("again");
    assert_eq!(std::fs::read_to_string(&md).expect("md").matches(BLOCK_BEGIN).count(), 1);
    std::fs::write(&md, format!("x{BLOCK_BEGIN}y{BLOCK_END}z")).expect("write");
    edit_block(&md, true, &mut backups).expect("remove");
    assert_eq!(std::fs::read_to_string(&md).expect("md"), "xz");
    std::fs::write(&md, "no newline").expect("write");
    edit_block(&md, false, &mut backups).expect("add");
    assert!(std::fs::read_to_string(&md).expect("md").starts_with("no newline\n\n<!--"));
    let s = t.path().join("settings.json");
    std::fs::write(&s, r#"{"hooks": {"PreToolUse": 3}, "other": 1}"#).expect("write");
    edit_hook(&s, Path::new("/opt/zazzle-csharp-indexer"), false, &mut backups).expect("add");
    edit_hook(&s, Path::new("/opt/zazzle-csharp-indexer"), false, &mut backups).expect("again");
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&s).expect("s")).expect("json");
    assert_eq!(v["hooks"]["PreToolUse"].as_array().expect("array").len(), 1);
    assert!(hook_present(&s));
    edit_hook(&s, Path::new(""), true, &mut backups).expect("remove");
    assert!(!hook_present(&s));
    std::fs::write(&s, "[1]").expect("write");
    edit_hook(&s, Path::new("/ix"), false, &mut backups).expect("replaces a non-object");
    std::fs::write(&s, "{bad").expect("write");
    assert!(edit_hook(&s, Path::new("/ix"), false, &mut backups).is_err());
    assert!(write_file(Path::new("/proc/zazzle-no/x"), "y").is_err());
}

// Checks the helpers that recognise zazzle and wrapped servers, quote for the shell, and describe servers.
#[test]
fn test_helpers() {
    let s = |command: &str, args: &[&str]| Server {
        table: vec!["mcpServers".into()],
        name: "x".into(),
        kind: Kind::Stdio { command: command.into(), args: args.iter().map(|a| a.to_string()).collect() },
        env: vec![],
        headers: vec![],
        bearer: None,
    };
    assert!(
        is_zazzle(&s("/a/zazzled", &["mcp"]))
            && is_zazzle(&s("/bin/sh", &["-c", "cd '/r' && exec '/a/zazzled' mcp"]))
    );
    assert!(!is_zazzle(&s("/a/zazzled", &["proxy"])) && !is_zazzle(&s("npx", &["mcp"])));
    assert_eq!(
        wrapped(&s("/a/zazzle", &["proxy", "--log", "--", "npx", "gh"])),
        Some(("npx".into(), vec!["gh".into()]))
    );
    assert_eq!(wrapped(&s("/a/zazzled", &["proxy", "--log"])), None);
    assert_eq!(wrapped(&s("/a/zazzled", &["proxy", "--"])), None);
    assert_eq!(wrapped(&s("npx", &["proxy"])), None);
    assert_eq!(sh_quote("it's"), r"'it'\''s'");
    let mut p = s("x", &[]);
    p.table = vec!["projects".into(), "/w".into(), "mcpServers".into()];
    assert_eq!(describe(&p), "x (stdio, project /w)");
    p.kind = Kind::Other("?".into());
    assert_eq!(describe(&p), "x (other, project /w)");
    assert!(wrap_args(&p).is_empty());
    let t = TempDir::new();
    let e = env(&t, true);
    let one: Routes = vec![(8809, "https://a".into())];
    save_routes(&e, &one).expect("save");
    assert_eq!(load_routes(&e), one);
    assert!(free_port(&one) > 8809);
    assert_eq!(e.short(Path::new("/elsewhere")), "/elsewhere");
    assert!(Env::detect().is_ok());
}
