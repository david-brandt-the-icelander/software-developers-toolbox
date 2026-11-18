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
            workspaces: vec![],
            hook: true,
            hook_edits: false,
            wrap: false,
            probe: false
        }
    );
    let o = opts(&["--hook-edits", "--workspace", "/a", "--workspace", "/b", "--verify"]);
    assert!(o.hook && o.hook_edits && o.workspaces == [PathBuf::from("/a"), PathBuf::from("/b")]);
    assert_eq!((o.action, opts(&["--report"]).action), (Action::Verify, Action::Report));
    let bad = |w: &[&str]| parse(&w.iter().map(|x| x.to_string()).collect::<Vec<_>>()).expect_err("bad");
    assert!(bad(&["--apply", "--undo"]).contains("give one of"));
    assert!(bad(&["--client", "vscode"]).contains("unknown client"));
    assert!(bad(&["--client"]).contains("needs a name"));
    assert!(bad(&["--repo"]).contains("needs a directory"));
    assert!(bad(&["--workspace"]).contains("needs a directory"));
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
    assert!(r.contains("  + add zazzle's instructions to ~/.claude/CLAUDE.md\n  + add the hook in ~/.claude/settings.json (Bash|Grep|Glob|Read|Edit|Write|MultiEdit|NotebookEdit): /opt/z/zazzled hook --client claude-code\n"), "{r}");
    assert!(r.contains("+ add the hook in ~/.codex/hooks.json (Bash|apply_patch): /opt/z/zazzled hook --client codex\n    Codex runs a new or changed hook only once you trust it"), "{r}");
    assert!(r.contains("= no hook: the desktop app's chat has none"), "{r}");
    assert!(r.contains("  + add workspace ~/repo: zazzle may edit files below it"), "{r}");
    assert!(
        r.contains(&format!(
            "+ add zazzle: /bin/sh -c cd '{}' && exec '/opt/z/zazzled' mcp",
            repo.canonicalize().expect("canon").display()
        )),
        "{r}"
    );
    assert!(
        r.contains("not changed (they stay direct, outside zazzle's proxy):\n  = team (stdio) (~/repo/.mcp.json): shared with the repository's other users"),
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
    let dev = t.path().join("dev");
    std::fs::create_dir_all(&dev).expect("mkdir");
    std::fs::create_dir_all(env.home.join(".zazzle")).expect("mkdir");
    std::fs::write(crate::workspace::file(&env.home), "/mine").expect("write");
    let old = r#"{"hooks": {"PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "/x/zazzle-csharp-indexer hook"}]}]}}"#;
    std::fs::write(env.home.join(".claude/settings.json"), old).expect("write");
    let ws = dev.to_string_lossy().to_string();
    let r = run(&opts(&["--apply", "--hook", "--workspace", &ws]), &env).expect("apply");
    assert!(
        r.contains("~ updated the hook in ~/.claude/settings.json")
            && r.contains("+ added the hook in ~/.codex/hooks.json"),
        "{r}"
    );
    assert!(r.contains("+ added workspace ~/dev") && r.contains("= workspace /mine"), "{r}");
    assert!(
        r.contains("+ added zazzle")
            && r.contains("~ wrapped gh")
            && r.contains("backups:\n")
            && r.contains("\n  ~/.claude.json.zazzle-backup-"),
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
    let settings = std::fs::read_to_string(env.home.join(".claude/settings.json")).expect("settings");
    assert!(
        settings.contains("/opt/z/zazzled hook --client claude-code") && !settings.contains("indexer"),
        "{settings}"
    );
    assert!(std::fs::read_to_string(env.codex_home.join("hooks.json"))
        .expect("hooks")
        .contains("hook --client codex"));
    let canon = dev.canonicalize().expect("canon").display().to_string();
    assert_eq!(
        std::fs::read_to_string(crate::workspace::file(&env.home)).expect("ws"),
        format!("/mine\n{canon}\n")
    );
    let routes = load_routes(&env);
    assert_eq!(routes.len(), 1);
    assert!(LAUNCHCTL.lock().expect("lock").iter().any(|c| c.starts_with("bootstrap gui/501 ")));
    let plist = env.home.join(format!("Library/LaunchAgents/com.zazzle.proxy.{}.plist", routes[0].0));
    assert!(std::fs::read_to_string(plist).expect("plist").contains("<string>https://api/mcp</string>"));

    let again = run(&opts(&["--apply", "--hook", "--workspace", &ws]), &env).expect("again");
    assert!(
        again.contains("= hook in ~/.claude/settings.json")
            && again.contains("= hook in ~/.codex/hooks.json"),
        "{again}"
    );
    assert!(
        again.contains("= leave gh (stdio): already behind the proxy") && again.contains("already routed"),
        "{again}"
    );
    assert!(!again.contains("+ added zazzle") && !again.contains("backups:"), "{again}");
    let md = env.home.join(".claude/CLAUDE.md");
    let mine = std::fs::read_to_string(&md)
        .expect("md")
        .replace("<!-- zazzle setup: begin -->", "my own rule\n\n<!-- zazzle setup: begin -->");
    std::fs::write(&md, format!("{mine}\nmore of mine\n")).expect("write");
    std::fs::write(policy::path(&env.home), "[instructions]\nextra = \"Answer briefly.\"\n").expect("policy");
    let refreshed = run(&opts(&["--apply", "--workspace", &ws]), &env).expect("refresh");
    assert!(refreshed.contains("~ updated zazzle's instructions in ~/.claude/CLAUDE.md"), "{refreshed}");
    let text = std::fs::read_to_string(&md).expect("md");
    assert!(
        text.contains("my own rule\n\n<!-- zazzle setup: begin -->")
            && text.ends_with("<!-- zazzle setup: end -->\n\nmore of mine\n"),
        "{text}"
    );
    assert!(text.contains("Answer briefly.\n<!-- zazzle setup: end -->"), "{text}");
    let same = run(&opts(&["--apply", "--workspace", &ws]), &env).expect("same");
    assert!(!same.contains("~ updated zazzle's instructions") && !same.contains("backups:"), "{same}");

    let undo = run(&opts(&["--undo"]), &env).expect("undo");
    assert!(
        undo.contains("- removed zazzle") && undo.contains("~ unwrapped gh (stdio): back to npx"),
        "{undo}"
    );
    assert!(
        undo.contains("~ unrouted api (http): back to https://api/mcp")
            && undo.contains("- remove zazzle's hook from ~/.claude/settings.json")
            && undo.contains("- remove workspace"),
        "{undo}"
    );
    let code: Value =
        serde_json::from_str(&std::fs::read_to_string(env.config_path(Client::ClaudeCode)).expect("read"))
            .expect("json");
    assert_eq!(code["mcpServers"]["gh"], json!({"type": "stdio", "command": "npx", "args": ["-y", "gh"]}));
    assert_eq!(code["mcpServers"]["api"]["url"], json!("https://api/mcp"));
    assert!(code["mcpServers"].get("zazzle").is_none());
    assert_eq!(
        std::fs::read_to_string(env.home.join(".claude/CLAUDE.md")).expect("md"),
        "# Mine\n\nmy own rule\n\n\nmore of mine\n",
        "undo keeps what is outside the markers"
    );
    assert!(!env.home.join(".claude/settings.json").exists(), "only zazzle's hooks were in it");
    assert!(!env.codex_home.join("hooks.json").exists() && !env.codex_home.join("AGENTS.md").exists());
    assert_eq!(std::fs::read_to_string(crate::workspace::file(&env.home)).expect("ws"), "/mine\n");
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
    let r = run(&opts(&[]), &env).expect("plan");
    assert!(
        r.contains("C# indexer: not found")
            && r.contains("no --repo: the Claude desktop app starts servers in /"),
        "{r}"
    );
    assert!(!r.contains("route api") && r.contains("only macOS has"), "{r}");
    assert!(
        r.contains("\ncodex: not installed, skipped")
            && r.contains("+ add the hook in ~/.claude/settings.json"),
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
    let home = env.home.to_string_lossy().to_string();
    assert!(run(&opts(&["--workspace", &home]), &env).expect_err("broad").contains("too broad"));
    assert!(run(&opts(&["--workspace", "/no/such/dir"]), &env)
        .expect_err("missing")
        .contains("not a directory"));
    let file = t.write("plain.txt", b"x");
    assert!(run(&opts(&["--workspace", &file.to_string_lossy()]), &env)
        .expect_err("file")
        .contains("not a directory"));
    let r = run(&opts(&["--client", "claude-code", "--no-hook"]), &env).expect("plan");
    assert!(
        r.contains("none: edits stay inside the folder a chat starts in")
            && r.contains("= no hook (--no-hook"),
        "{r}"
    );
    std::fs::write(env.config_path(Client::ClaudeDesktop), "{broken").expect("write");
    assert!(run(&opts(&["--client", "claude-desktop"]), &env).is_err());
}

// Checks the instructions block and hook edits on their own: idempotent, removable, and tolerant of odd files.
#[test]
fn test_block_and_hook() {
    let t = TempDir::new();
    let md = t.path().join("A.md");
    let mut backups = Vec::new();
    let b = block_text("");
    let b = Some(b.as_str());
    edit_block(&md, None, &mut backups).expect("remove from nothing");
    edit_block(&md, b, &mut backups).expect("add");
    edit_block(&md, b, &mut backups).expect("again");
    assert_eq!(std::fs::read_to_string(&md).expect("md").matches(BLOCK_BEGIN).count(), 1);
    std::fs::write(&md, format!("x{BLOCK_BEGIN}y{BLOCK_END}z")).expect("write");
    edit_block(&md, None, &mut backups).expect("remove");
    assert_eq!(std::fs::read_to_string(&md).expect("md"), "xz");
    std::fs::write(&md, "no newline").expect("write");
    edit_block(&md, b, &mut backups).expect("add");
    assert!(std::fs::read_to_string(&md).expect("md").starts_with("no newline\n\n<!--"));
    let s = t.path().join("settings.json");
    std::fs::write(&s, r#"{"hooks": {"PreToolUse": 3}, "other": 1}"#).expect("write");
    let h = HookSpec { path: s.clone(), matcher: "Bash", command: "/z/zazzled hook --client codex".into() };
    edit_hook(&s, Some(&h), &mut backups).expect("add");
    edit_hook(&s, Some(&h), &mut backups).expect("again");
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&s).expect("s")).expect("json");
    assert_eq!(v["hooks"]["PreToolUse"].as_array().expect("array").len(), 1);
    assert_eq!(hook_command(&s).as_deref(), Some("/z/zazzled hook --client codex"));
    edit_hook(&s, None, &mut backups).expect("remove");
    edit_hook(&s, None, &mut backups).expect("remove again");
    let keep = t.path().join("keep.json");
    std::fs::write(&keep, r#"{"hooks": {"Stop": [1]}}"#).expect("write");
    edit_hook(&keep, Some(&h), &mut backups).expect("add");
    edit_hook(&keep, None, &mut backups).expect("remove");
    assert_eq!(
        std::fs::read_to_string(&keep).expect("kept"),
        "{\n  \"hooks\": {\n    \"Stop\": [\n      1\n    ]\n  }\n}\n"
    );
    assert!(!our_hook(&json!({"hooks": [{"command": "/z/zazzled mcp"}]})));
    assert_eq!(std::fs::read_to_string(&s).expect("kept"), "{\n  \"other\": 1\n}\n");
    let only = t.path().join("only.json");
    edit_hook(&only, Some(&h), &mut backups).expect("add");
    edit_hook(&only, None, &mut backups).expect("remove");
    assert!(!only.exists());
    assert!(write_or_remove(t.path(), "").is_err());
    assert_eq!(hook_command(&s), None);
    std::fs::write(&s, "[1]").expect("write");
    edit_hook(&s, Some(&h), &mut backups).expect("replaces a non-object");
    std::fs::write(&s, "{bad").expect("write");
    assert!(edit_hook(&s, Some(&h), &mut backups).is_err());
    assert_eq!(hook_command(&s), None);
    let mut spaced = env(&t, false);
    spaced.zazzled = PathBuf::from("/Apps/My Tools/zazzled");
    assert_eq!(
        hook_spec(&spaced, Client::Codex, true).expect("spec").command,
        "'/Apps/My Tools/zazzled' hook --client codex --edits"
    );
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
        cwd: None,
        enabled: true,
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
    std::fs::create_dir_all(e.home.join(".zazzle")).expect("mkdir");
    std::fs::write(e.state_path(), "[1]").expect("not an object");
    save_state(&e, &Vec::new(), &["/w".into()]).expect("save");
    assert_eq!(load_state(&e), (vec![], vec!["/w".to_string()]));
    save_state(&e, &one, &["/w".into()]).expect("save");
    assert_eq!(load_state(&e), (one.clone(), vec!["/w".to_string()]));
    assert!(free_port(&one) > 8809);
    assert_eq!(e.short(Path::new("/elsewhere")), "/elsewhere");
    assert!(Env::detect().is_ok());
    let cellar = t.path().join("Cellar/zazzle/1.2.11/bin/zazzled");
    assert_eq!(stable_exe(&cellar), cellar);
    std::fs::create_dir_all(t.path().join("opt/zazzle/bin")).expect("mkdir");
    std::fs::write(t.path().join("opt/zazzle/bin/zazzled"), b"x").expect("write");
    assert_eq!(stable_exe(&cellar), t.path().join("opt/zazzle/bin/zazzled"));
    assert_eq!(stable_exe(&t.path().join("Cellar/zazzle")), t.path().join("Cellar/zazzle"));
    assert_eq!(stable_exe(Path::new("/usr/bin/zazzled")), PathBuf::from("/usr/bin/zazzled"));
}

// Checks undo with nothing to undo, odd entries, a repository with and without a shared .mcp.json, and other files' content kept.
#[test]
fn test_edges() {
    let t = TempDir::new();
    let env = env(&t, true);
    let code = r#"{"mcpServers": {"x": 5, "tool": {"command": "npx"}}}"#;
    std::fs::write(env.config_path(Client::ClaudeCode), code).expect("write");
    let r = run(&opts(&["--undo", "--client", "claude-code"]), &env).expect("undo");
    assert!(r.contains("no servers"), "{r}");
    let repo = t.path().join("repo");
    std::fs::create_dir_all(&repo).expect("mkdir");
    let r = run(&opts(&["--client", "claude-code", "--repo", &repo.to_string_lossy()]), &env).expect("plan");
    assert!(!r.contains("shared with the repository"), "{r}");
    let shared = r#"{"mcpServers": {"zazzle": {"command": "zazzled", "args": ["mcp"]}}}"#;
    std::fs::write(repo.join(".mcp.json"), shared).expect("write");
    let r = run(&opts(&["--client", "claude-code", "--repo", &repo.to_string_lossy()]), &env).expect("plan");
    assert!(!r.contains("shared with the repository"), "{r}");
    std::fs::create_dir_all(repo.join("svc/.git")).expect("mkdir");
    std::fs::create_dir_all(repo.join("svc/.codex")).expect("mkdir");
    std::fs::write(repo.join("svc/.codex/config.toml"), "[mcp_servers.db]\ncommand = \"db\"\n")
        .expect("write");
    let plugin = env.home.join(".claude/plugins/cache/p/1.0");
    std::fs::create_dir_all(&plugin).expect("mkdir");
    std::fs::write(plugin.join(".mcp.json"), r#"{"mcpServers": {"pl": {"command": "x"}, "bad": 1}}"#)
        .expect("write");
    std::fs::write(env.config_path(Client::Codex), "[plugins.\"s@m\".mcp_servers.ps]\nenabled = true\n")
        .expect("write");
    let r = run(&opts(&["--client", "claude-code", "--repo", &repo.to_string_lossy()]), &env).expect("plan");
    assert!(
        r.contains("= db (stdio) (~/repo/svc/.codex/config.toml): shared with the repository's other users"),
        "{r}"
    );
    assert!(
        r.contains("= pl (Claude Code plugin, ~/.claude/plugins/cache/p/1.0/.mcp.json)")
            && !r.contains("= bad"),
        "{r}"
    );
    assert!(r.contains("= ps (Codex plugin s@m)"), "{r}");
    std::fs::write(repo.join("svc/.mcp.json"), "{broken").expect("write");
    std::fs::write(env.config_path(Client::Codex), "[[[").expect("write");
    std::fs::create_dir_all(env.home.join(".claude/plugins/node_modules/q")).expect("mkdir");
    std::fs::write(env.home.join(".claude/plugins/node_modules/q/.mcp.json"), r#"{"nm": {"command": "x"}}"#)
        .expect("write");
    std::os::unix::fs::symlink("/nonexistent-zz", env.home.join(".claude/plugins/dangling"))
        .expect("symlink");
    let deep = env.home.join(".claude/plugins/a/b/c/d/e/f");
    std::fs::create_dir_all(&deep).expect("mkdir");
    std::fs::write(deep.join(".mcp.json"), r#"{"deep": {"command": "x"}}"#).expect("write");
    let ws = repo.to_string_lossy().to_string();
    let r = run(&opts(&["--client", "claude-code", "--repo", &ws, "--workspace", &ws]), &env).expect("plan");
    assert!(
        !r.contains("= nm ") && !r.contains("= deep ") && r.contains("= db (stdio)") && r.contains("= pl ("),
        "{r}"
    );
    assert_eq!(r.matches("+ add workspace").count(), 1, "{r}");
    let mut backups = Vec::new();
    let md = t.path().join("B.md");
    std::fs::write(&md, "a\n\n").expect("write");
    edit_block(&md, Some(&block_text("")), &mut backups).expect("add");
    assert!(std::fs::read_to_string(&md).expect("md").starts_with("a\n\n<!--"));
    let s = t.path().join("s.json");
    let other = r#"{"hooks": {"PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "mine"}]}]}}"#;
    std::fs::write(&s, other).expect("write");
    assert_eq!(hook_command(&s), None);
    let h = HookSpec {
        path: s.clone(),
        matcher: CLAUDE_MATCHER,
        command: "/z/zazzled hook --client claude-code".into(),
    };
    edit_hook(&s, Some(&h), &mut backups).expect("add");
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&s).expect("s")).expect("json");
    assert_eq!(v["hooks"]["PreToolUse"].as_array().expect("array").len(), 2);
}

// Checks --off undoes an --apply but remembers it, --on redoes it, and --uninstall undoes it, forgets it and stops zazzled's agent.
#[test]
fn test_off_on_uninstall() {
    let t = TempDir::new();
    let env = env(&t, true);
    seed(&env);
    let dev = t.path().join("dev");
    std::fs::create_dir_all(&dev).expect("mkdir");
    let ws = dev.to_string_lossy().to_string();
    assert!(run(&opts(&["--off"]), &env)
        .expect_err("nothing applied")
        .contains("run `zazzle setup --apply`"));
    assert!(run(&opts(&["--on"]), &env).is_err());
    let before = std::fs::read_to_string(env.config_path(Client::Codex)).expect("codex");
    run(&opts(&["--apply", "--hook", "--workspace", &ws, "--client", "codex"]), &env).expect("apply");
    let applied = std::fs::read_to_string(env.config_path(Client::Codex)).expect("codex");
    let off = run(&opts(&["--off"]), &env).expect("off");
    assert!(
        off.starts_with("zazzle setup --off: zazzle is out of the way") && off.contains("- removed zazzle"),
        "{off}"
    );
    assert_eq!(std::fs::read_to_string(env.config_path(Client::Codex)).expect("codex"), before);
    assert!(!env.codex_home.join("hooks.json").exists() && !crate::workspace::file(&env.home).exists());
    assert!(
        !off.contains("claude-code: ~/.claude.json") && !off.contains("= workspace"),
        "only what was applied: {off}"
    );
    let on = run(&opts(&["--on"]), &env).expect("on");
    assert!(
        on.starts_with("zazzle setup --on: the setup from before --off is back.")
            && on.contains("+ added the hook"),
        "{on}"
    );
    assert_eq!(std::fs::read_to_string(env.config_path(Client::Codex)).expect("codex"), applied);
    assert!(std::fs::read_to_string(crate::workspace::file(&env.home)).expect("ws").contains("dev"));
    LAUNCHCTL.lock().expect("lock").clear();
    let gone = run(&opts(&["--uninstall"]), &env).expect("uninstall");
    assert!(
        gone.contains("- removed zazzle")
            && gone.contains("zazzled's LaunchAgent: not installed")
            && gone.contains("brew uninstall zazzle"),
        "{gone}"
    );
    assert!(LAUNCHCTL.lock().expect("lock").iter().any(|c| c.starts_with("bootout gui/501/")));
    assert_eq!(std::fs::read_to_string(env.config_path(Client::Codex)).expect("codex"), before);
    assert!(run(&opts(&["--on"]), &env).is_err(), "uninstall forgets the setup");
    run(&opts(&["--apply", "--client", "codex"]), &env).expect("apply");
    run(&opts(&["--undo", "--client", "codex"]), &env).expect("undo");
    assert!(run(&opts(&["--off"]), &env).is_err(), "undo forgets it too");
    let linux = Env { macos: false, ..env.clone() };
    assert!(!run(&opts(&["--uninstall", "--client", "codex"]), &linux)
        .expect("uninstall")
        .contains("LaunchAgent"));
}
