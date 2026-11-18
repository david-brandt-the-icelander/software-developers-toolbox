// `zazzle setup --verify`: uses zazzle the way an agent would, from inside a repository and from elsewhere, then shows the activity entries it left as evidence.
use super::check::{hook_selftest, Session};
use super::clients::{Client, ConfigFile, Kind};
use super::{is_zazzle, wrapped, Env, Opts};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;

// The session verify's own zazzle calls carry, so reports of agent activity leave them out.
const VERIFY_SESSION: &str = "zazzle-setup-verify";

// The outcome of one step: what was done, and whether it worked.
struct Step {
    ok: bool,
    what: String,
}

// Picks the repository to verify in: --repo, else the git work tree of the current folder.
fn repository(opts: &Opts) -> Result<PathBuf, String> {
    let start = match &opts.repo {
        Some(r) => r.clone(),
        None => std::env::current_dir().map_err(|e| e.to_string())?,
    };
    let start = start.canonicalize().map_err(|_| format!("{}: not a directory", start.display()))?;
    crate::indexer::work_tree(&start)
        .map(Path::to_path_buf)
        .ok_or_else(|| format!("{} is not in a git repository: give --repo DIR", start.display()))
}

// Picks a small text file of the repository to search and read, and a word in it.
fn sample(repo: &Path) -> Option<(PathBuf, String)> {
    let listed = Command::new("git").args(["-C", &repo.to_string_lossy(), "ls-files"]).output().ok()?;
    let files = String::from_utf8_lossy(&listed.stdout).into_owned();
    let preferred = ["README.md", "readme.md", "README", "Cargo.toml", "package.json"];
    let mut names: Vec<&str> = files.lines().collect();
    names.sort_by_key(|n| (!preferred.contains(n), n.len()));
    names.iter().map(|n| repo.join(n)).find_map(|p| {
        let text = std::fs::read_to_string(&p).ok().filter(|t| t.len() < 200_000)?;
        let word = text
            .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .find(|w| w.len() >= 5 && w.chars().all(|c| c.is_ascii_alphabetic()))?
            .to_string();
        Some((p, word))
    })
}

// Runs search, exact read, a reversible edit and the structure overview through a zazzle MCP server started in `start`, naming files as `name` gives them.
fn exercise(env: &Env, start: &Path, repo: &Path, name: &dyn Fn(&Path) -> String) -> Vec<Step> {
    let mut steps = Vec::new();
    let tag = [("ZAZZLE_SESSION".to_string(), VERIFY_SESSION.to_string())];
    let mut s = match Session::start(&env.zazzled.to_string_lossy(), &["mcp".into()], &tag, start) {
        Ok(s) => s,
        Err(e) => return vec![Step { ok: false, what: format!("start zazzled mcp: {e}") }],
    };
    let mut step = |ok: bool, what: String| steps.push(Step { ok, what });
    let Some((file, word)) = sample(repo) else {
        step(false, "no readable tracked text file to search".into());
        s.stop();
        return steps;
    };
    let shown = name(&file);
    let found = s.call("zazzle_find", json!({"pattern": word, "paths": [name(repo)]}));
    step(
        matches!(&found, Ok((t, false)) if t.contains(&word) || t.contains("match")),
        format!("search: zazzle_find {word:?} in {}: {}", name(repo), first(&found)),
    );
    let read = s.call("zazzle_read", json!({"paths": [shown], "source": true, "head": 5}));
    let sha = read.as_ref().ok().and_then(|(t, _)| sha_of(t));
    step(sha.is_some(), format!("exact read: zazzle_read {shown} source=true: {}", first(&read)));
    let scratch = repo.join(format!(".zazzle-verify-{}.txt", std::process::id()));
    let sname = name(&scratch);
    let made = s.call("zazzle_edit", json!({"path": sname, "create": true, "new": "zazzle verify\n"}));
    let created = matches!(&made, Ok((_, false))) && scratch.exists();
    let changed = created
        && s.call("zazzle_read", json!({"paths": [sname], "source": true, "disable_session_deduping": true}))
            .ok()
            .and_then(|(t, _)| sha_of(&t))
            .and_then(|sha| {
                s.call(
                    "zazzle_edit",
                    json!({"path": sname, "old": "verify", "new": "verified", "expect": sha}),
                )
                .ok()
            })
            .is_some_and(|(_, err)| !err)
        && std::fs::read_to_string(&scratch).is_ok_and(|t| t == "zazzle verified\n");
    let _ = std::fs::remove_file(&scratch);
    let what = match (created, changed) {
        (true, true) => format!("reversible edit: created, edited (with the read's sha) and removed {sname}"),
        _ => format!("reversible edit of {sname}: {}", first(&made)),
    };
    step(changed && !scratch.exists(), what);
    let overview = s.call("zazzle_overview", json!({"repo": name(repo)}));
    let no_indexer = first(&overview).contains("zazzle-csharp-indexer is not installed");
    let what = match no_indexer {
        true => "structure: zazzle_overview skipped, since zazzle-csharp-indexer is not installed (C# terse views and structure tools need it)".to_string(),
        false => format!("structure: zazzle_overview: {}", first(&overview)),
    };
    step(no_indexer || matches!(&overview, Ok((_, false))), what);
    s.stop();
    steps
}

// Returns the first line of a tool answer or the error.
fn first(r: &Result<(String, bool), String>) -> String {
    let t = match r {
        Ok((t, _)) => t.as_str(),
        Err(e) => e.as_str(),
    };
    t.lines().find(|l| !l.starts_with("[r:")).unwrap_or("").chars().take(100).collect()
}

// Finds the sha a source read reports in its header (`PATH N lines, sha XXXXXXXX`).
fn sha_of(text: &str) -> Option<String> {
    let at = text.find(", sha ")? + 6;
    let sha: String = text[at..].chars().take_while(char::is_ascii_hexdigit).collect();
    (!sha.is_empty()).then_some(sha)
}

// Runs a command through `zazzled wrap` and checks the exit code and the cleaned output come back.
fn run_step(env: &Env, repo: &Path) -> Step {
    let out = Command::new(&env.zazzled)
        .args(["wrap", "sh", "-c", "echo zazzle verify; exit 3"])
        .env("ZAZZLE_SESSION", VERIFY_SESSION)
        .current_dir(repo)
        .output();
    let ok = out
        .as_ref()
        .is_ok_and(|o| o.status.code() == Some(3) && String::from_utf8_lossy(&o.stdout).contains("[exit 3"));
    Step {
        ok,
        what: "command: `zazzle run -- sh -c 'echo ..; exit 3'` kept exit code 3 and cleaned its output"
            .into(),
    }
}

// Calls each server setup put behind the proxy through its configured command, which the proxy logs.
fn proxy_steps(env: &Env, clients: &[Client]) -> Vec<Step> {
    let mut steps = Vec::new();
    for c in clients {
        let Ok(file) = ConfigFile::load(*c, &env.config_path(*c)) else { continue };
        for s in file.servers().iter().filter(|s| s.enabled && !is_zazzle(s) && wrapped(s).is_some()) {
            let Kind::Stdio { command, args } = &s.kind else { continue };
            let folder = s.cwd.as_ref().map(PathBuf::from).unwrap_or_else(|| env.home.clone());
            let env: Vec<(String, String)> =
                s.env.iter().cloned().chain([("ZAZZLE_SESSION".into(), VERIFY_SESSION.into())]).collect();
            let what = match Session::start(command, args, &env, &folder) {
                Ok(m) => {
                    let tools = m.tools;
                    m.stop();
                    Ok(format!(
                        "MCP call: {} ({}) answered tools/list through zazzle's proxy: {tools} tools",
                        s.name,
                        c.name()
                    ))
                }
                Err(e) => Err(format!("MCP call: {} ({}) through zazzle's proxy: {e}", s.name, c.name())),
            };
            steps.push(match what {
                Ok(w) => Step { ok: true, what: w },
                Err(w) => Step { ok: false, what: w },
            });
        }
    }
    if steps.is_empty() {
        steps.push(Step {
            ok: true,
            what: "MCP call through the proxy: no stdio server is wrapped yet, so nothing to call".into(),
        });
    }
    steps
}

// Runs `zazzle setup --verify` and returns its report.
pub fn run(opts: &Opts, env: &Env) -> Result<String, String> {
    let repo = repository(opts)?;
    let since = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let clients: Vec<Client> = Client::ALL.into_iter().filter(|c| env.installed(*c)).collect();
    let mut out = format!("zazzle setup --verify: using zazzle as an agent would, on {}.\n", repo.display());
    let mut failed = 0;
    let mut section = |title: String, steps: Vec<Step>, out: &mut String| {
        out.push_str(&format!("\n{title}\n"));
        for s in steps {
            failed += usize::from(!s.ok);
            out.push_str(&format!("  {} {}\n", if s.ok { "ok  " } else { "FAIL" }, s.what));
        }
    };
    let inside = exercise(env, &repo, &repo, &|p| crate::util::display(p, &repo));
    section("chat started inside the repository (relative paths):".into(), inside, &mut out);
    let elsewhere = env.home.canonicalize().unwrap_or_else(|_| env.home.clone());
    let outside = exercise(env, &elsewhere, &repo, &|p| p.display().to_string());
    section(format!("chat started elsewhere ({}, absolute paths):", elsewhere.display()), outside, &mut out);
    section(
        "commands and MCP servers:".into(),
        std::iter::once(run_step(env, &repo))
            .chain(match opts.probe {
                true => proxy_steps(env, &clients),
                false => vec![Step { ok: true, what: "MCP calls through the proxy: not made, since they start other servers (--probe makes them)".into() }],
            })
            .collect(),
        &mut out,
    );
    let hooks: Vec<Step> = clients
        .iter()
        .filter_map(|c| {
            super::hook_spec(env, *c, false).and_then(|h| super::hook_command(&h.path)).map(|cmd| (c, cmd))
        })
        .map(|(c, cmd)| match hook_selftest(&cmd) {
            Ok(()) => Step {
                ok: true,
                what: format!(
                    "{}'s hook redirected `rg` and `cargo test` and let `git status` through",
                    c.name()
                ),
            },
            Err(e) => Step { ok: false, what: format!("{}'s hook: {e}", c.name()) },
        })
        .collect();
    let hooks = match hooks.is_empty() {
        true => vec![Step {
            ok: true,
            what: "no hook installed: routing is by instructions only (apply without --no-hook)".into(),
        }],
        false => hooks,
    };
    section("enforcement:".into(), hooks, &mut out);
    out.push_str("\nevidence, from the activity log:\n");
    match crate::activity::path() {
        Some(log) => {
            let entries: Vec<Value> = crate::activity::read(&log, since);
            super::check::summary(&entries).1.iter().for_each(|l| out.push_str(&format!("  {l}\n")));
            if !entries.iter().any(|e| e["src"] == "zazzle") {
                failed += 1;
                out.push_str("  FAIL no zazzle command was logged: is another zazzled (an older version) serving the socket? `zazzled uninstall` then retry\n");
            }
        }
        None => out.push_str("  no activity log (HOME is not set)\n"),
    }
    out.push_str(&format!("\n{}\n", match failed {
        0 => "All steps passed. This shows zazzle works; `zazzle setup --report` shows what agents actually route through it.".to_string(),
        n => format!("{n} steps failed."),
    }));
    Ok(out)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::testutil::{env_lock, TempDir};
    use std::os::unix::fs::PermissionsExt;

    // A stand-in zazzled: `mcp` answers the tools verify calls (really creating and editing files) and logs them; `wrap` exits 3.
    const FAKE: &str = r#"#!/usr/bin/env python3
import json, os, sys
log = os.environ.get("ZAZZLE_ACTIVITY_LOG")
def note(cmd):
    if log:
        open(log, "a").write(json.dumps({"src": "zazzle", "cmd": cmd, "session": os.environ.get("ZAZZLE_SESSION", ""), "ts": 9999999999}) + "\n")
fail = os.environ.get("FAKE_FAIL")
if sys.argv[1] == "wrap":
    print("zazzle verify\n[exit 3 in 0.0s]")
    sys.exit(3)
for line in sys.stdin:
    m = json.loads(line)
    if "id" not in m:
        continue
    r = {}
    if m["method"] == "tools/list":
        r = {"tools": [{"name": "zazzle_find"}]}
    if m["method"] == "tools/call":
        name, a = m["params"]["name"], m["params"]["arguments"]
        note(name)
        text, err = "", False
        if fail:
            text, err = "boom", True
        elif name == "zazzle_find":
            text = "README.md\n1: " + a["pattern"]
        elif name == "zazzle_read":
            text = "[r:abcde]\n" + a["paths"][0] + " 1 lines, sha 0123abcd\n1\tx"
        elif name == "zazzle_edit" and a.get("create"):
            open(a["path"], "w").write(a["new"])
            text = "ok created"
        elif name == "zazzle_edit":
            t = open(a["path"]).read()
            open(a["path"], "w").write(t.replace(a["old"], a["new"]))
            text = "ok"
        else:
            text, err = "error: zazzle-csharp-indexer is not installed (beside zazzled or on PATH)", True
        r = {"content": [{"type": "text", "text": text}], "isError": err}
    print(json.dumps({"jsonrpc": "2.0", "id": m["id"], "result": r}), flush=True)
"#;

    // Builds an environment whose zazzled is the stand-in, with a git repository holding a README.
    fn setup(t: &TempDir) -> (Env, PathBuf) {
        let home = t.path().canonicalize().expect("home");
        let exe = t.write("bin/zazzled", FAKE.as_bytes());
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let repo = home.join("dev/api");
        std::fs::create_dir_all(&repo).expect("mkdir");
        std::fs::write(repo.join("README.md"), "Orders service\n").expect("write");
        let git = |args: &[&str]| {
            Command::new("git").args(["-C", &repo.to_string_lossy()]).args(args).output().expect("git")
        };
        git(&["init", "-q"]);
        git(&["add", "."]);
        let env = Env {
            home: home.clone(),
            zazzled: exe,
            indexer: None,
            codex_home: home.join(".codex"),
            macos: false,
            uid: 501,
            launchctl: |_| Ok(()),
        };
        (env, repo)
    }

    // Checks a whole verify run against the stand-in: every step passes, the scratch file is gone, and the log is the evidence.
    #[test]
    fn test_verify_run() {
        let _g = env_lock();
        let t = TempDir::new();
        let (env, repo) = setup(&t);
        let log = env.home.join("activity.log");
        let saved = std::env::var_os("ZAZZLE_ACTIVITY_LOG");
        std::env::set_var("ZAZZLE_ACTIVITY_LOG", &log);
        let wrapped = json!({"mcpServers": {"tools": {"command": env.zazzled, "args": ["proxy", "--log", "--", "x"]}, "zazzle": {"command": env.zazzled, "args": ["mcp"]}}});
        std::fs::write(env.config_path(Client::ClaudeCode), wrapped.to_string()).expect("write");
        let local = Opts { repo: Some(repo.join(".")), ..super::super::parse(&[]).expect("opts") };
        let wrapped =
            json!({"mcpServers": {"tools": {"command": env.zazzled, "args": ["proxy", "--log", "--", "x"]}}});
        std::fs::write(env.config_path(Client::ClaudeCode), wrapped.to_string()).expect("write");
        assert!(run(&local, &env).expect("verify").contains(
            "MCP calls through the proxy: not made, since they start other servers (--probe makes them)"
        ));
        let opts = Opts { probe: true, ..local };
        let r = run(&opts, &env).expect("verify");
        assert!(r.contains("ok   search: zazzle_find \"Orders\" in .: README.md"), "{r}");
        assert!(
            r.contains("ok   exact read: zazzle_read README.md source=true: README.md 1 lines, sha 0123abcd"),
            "{r}"
        );
        assert!(
            r.contains(
                "ok   reversible edit: created, edited (with the read's sha) and removed .zazzle-verify-"
            ),
            "{r}"
        );
        assert!(r.contains("structure: zazzle_overview skipped"), "{r}");
        assert!(
            r.contains("chat started elsewhere") && r.contains(&format!("in {}:", repo.display())),
            "{r}"
        );
        assert!(
            r.contains("ok   command: `zazzle run -- sh -c")
                && r.contains("MCP call: tools (claude-code) answered tools/list"),
            "{r}"
        );
        assert!(r.contains("no hook installed: routing is by instructions only"), "{r}");
        assert!(r.contains("through zazzle: 24 commands") && r.ends_with("All steps passed. This shows zazzle works; `zazzle setup --report` shows what agents actually route through it.\n"), "{r}");
        assert!(std::fs::read_dir(&repo).expect("dir").all(|e| !e
            .expect("entry")
            .file_name()
            .to_string_lossy()
            .starts_with(".zazzle-verify")));
        std::fs::remove_file(&log).expect("rm");
        std::fs::write(&env.zazzled, "#!/bin/sh\nexit 1\n").expect("broken");
        let r = run(&opts, &env).expect("verify");
        assert!(
            r.contains("FAIL start zazzled mcp:") && r.contains("FAIL no zazzle command was logged"),
            "{r}"
        );
        assert!(r.contains("FAIL MCP call: tools (claude-code) through zazzle's proxy"), "{r}");
        std::env::set_var("ZAZZLE_ACTIVITY_LOG", "");
        assert!(run(&opts, &env).expect("verify").contains("no activity log"));
        std::fs::write(&env.zazzled, FAKE).expect("restore");
        std::env::set_var("FAKE_FAIL", "1");
        std::fs::write(env.config_path(Client::ClaudeCode), "{}").expect("no servers");
        let r = run(&opts, &env).expect("verify");
        std::env::remove_var("FAKE_FAIL");
        assert!(
            r.contains("FAIL search:")
                && r.contains("FAIL exact read:")
                && r.contains("FAIL reversible edit of"),
            "{r}"
        );
        assert!(
            r.contains("FAIL structure: zazzle_overview: boom")
                && r.contains("no stdio server is wrapped yet"),
            "{r}"
        );
        std::fs::create_dir_all(env.codex_home.clone()).expect("mkdir");
        std::fs::write(env.config_path(Client::Codex), "[[[").expect("broken toml");
        assert!(run(&opts, &env).expect("verify").contains("no stdio server is wrapped yet"));
        let bare = env.home.join("bare");
        std::fs::create_dir_all(&bare).expect("mkdir");
        Command::new("git").args(["init", "-q"]).current_dir(&bare).output().expect("git");
        let bare_opts = Opts { repo: Some(bare.clone()), ..super::super::parse(&[]).expect("opts") };
        assert!(run(&bare_opts, &env).expect("verify").contains("no readable tracked text file to search"));
        match saved {
            Some(v) => std::env::set_var("ZAZZLE_ACTIVITY_LOG", v),
            None => std::env::remove_var("ZAZZLE_ACTIVITY_LOG"),
        }
    }

    // Checks the helpers: picking the repository, a sample word, the sha and the first line of an answer.
    #[test]
    fn test_helpers() {
        let t = TempDir::new();
        let (_env, repo) = setup(&t);
        let opts = |r: &Path| Opts { repo: Some(r.to_path_buf()), ..super::super::parse(&[]).expect("opts") };
        assert_eq!(repository(&opts(&repo.join("."))).expect("repo"), repo);
        assert!(repository(&opts(Path::new("/nonexistent-zz"))).unwrap_err().contains("not a directory"));
        assert!(repository(&opts(Path::new("/"))).unwrap_err().contains("give --repo DIR"));
        assert_eq!(sample(&repo), Some((repo.join("README.md"), "Orders".to_string())));
        assert_eq!(sample(t.path()), None);
        assert_eq!(sha_of("[r:x]\na.rs 3 lines, sha 00ff\n1\tx"), Some("00ff".into()));
        assert_eq!(sha_of("no sha"), None);
        assert_eq!(first(&Err("boom\nmore".into())), "boom");
        assert_eq!(first(&Ok(("[r:x]\nline".into(), false))), "line");
    }
}
