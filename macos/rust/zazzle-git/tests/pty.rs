// End-to-end tests: runs the real programs on a pseudo-terminal in scratch git repositories and types keys into them.

use std::fs;
use std::io::{Read, Write};
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const EDITOR: &str = env!("CARGO_BIN_EXE_zazzle-git-commit-message-editor");
const ZGIT: &str = env!("CARGO_BIN_EXE_zazzle-git");
const ZCOMMIT: &str = env!("CARGO_BIN_EXE_zazzle-git-commit");

// A program running on a pseudo-terminal, with everything it printed so far.
struct Pty {
    master: fs::File,
    child: Child,
    screen: Arc<Mutex<Vec<u8>>>,
}

impl Pty {
    // Starts the program on a new pseudo-terminal as its controlling terminal.
    fn spawn(prog: &str, args: &[&str], dir: &Path, env: &[(&str, &str)]) -> Pty {
        let (mut m, mut s) = (0, 0);
        let mut ws = libc::winsize { ws_row: 24, ws_col: 100, ws_xpixel: 0, ws_ypixel: 0 };
        // SAFETY: openpty writes two descriptors into the given ints.
        let rc = unsafe {
            libc::openpty(
                &mut m,
                &mut s,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::addr_of_mut!(ws) as _,
            )
        };
        assert_eq!(rc, 0);
        // SAFETY: openpty returned these descriptors and nothing else owns them.
        let (master, slave) = unsafe { (fs::File::from_raw_fd(m), OwnedFd::from_raw_fd(s)) };
        let mut cmd = Command::new(prog);
        cmd.args(args)
            .current_dir(dir)
            .env("HOME", offered_home(dir))
            .env(
                "PATH",
                format!("{}:{}", agents_bin(dir, 0).display(), std::env::var("PATH").unwrap_or_default()),
            )
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .envs(env.iter().copied());
        cmd.stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave));
        // SAFETY: setsid and ioctl are async-signal-safe and only make the pty the child's controlling terminal.
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                libc::ioctl(0, libc::TIOCSCTTY as _, 0);
                Ok(())
            });
        }
        let child = cmd.spawn().unwrap();
        let screen = Arc::new(Mutex::new(Vec::new()));
        let (sink, mut reader) = (screen.clone(), master.try_clone().unwrap());
        thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                sink.lock().unwrap().extend_from_slice(&buf[..n]);
            }
        });
        Pty { master, child, screen }
    }

    // Waits until the output contains the text, failing after a few seconds.
    fn expect(&self, text: &str) {
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(10) {
            if String::from_utf8_lossy(&self.screen.lock().unwrap()).contains(text) {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        let all = String::from_utf8_lossy(&self.screen.lock().unwrap()).into_owned();
        let tail: String = all.chars().rev().take(3000).collect::<Vec<_>>().into_iter().rev().collect();
        panic!("never saw {text:?} in ...{tail:?}");
    }

    // Types keys, pausing after them so separate arrow presses are not taps.
    fn keys(&mut self, bytes: &[u8]) {
        self.master.write_all(bytes).unwrap();
        thread::sleep(Duration::from_millis(300));
    }

    // Changes the terminal size, which sends the program SIGWINCH.
    fn resize(&mut self, rows: u16, cols: u16) {
        let ws = libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 };
        // SAFETY: TIOCSWINSZ reads the winsize struct for the pty master descriptor.
        unsafe { libc::ioctl(std::os::fd::AsRawFd::as_raw_fd(&self.master), libc::TIOCSWINSZ as _, &ws) };
        thread::sleep(Duration::from_millis(300));
    }

    // Sends the program a signal.
    fn signal(&self, sig: i32) {
        // SAFETY: kill sends a signal to the child's process id.
        unsafe { libc::kill(self.child.id() as i32, sig) };
    }

    // Waits for the program to exit and returns its exit code.
    fn exit(mut self) -> i32 {
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(20) {
            if let Some(s) = self.child.try_wait().unwrap() {
                return s.code().unwrap_or(-1);
            }
            thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        panic!("did not exit: {:?}", String::from_utf8_lossy(&self.screen.lock().unwrap()));
    }
}

// Returns a home directory beside the given one whose git config says the editor offer was already made.
fn offered_home(dir: &Path) -> PathBuf {
    let home = PathBuf::from(format!("{}-home", dir.display()));
    fs::create_dir_all(&home).unwrap();
    fs::write(home.join(".gitconfig"), "[zazzle]\n\teditorOffered = true\n").unwrap();
    home
}

// Writes stand-in claude, codex and gemini scripts beside the given folder: each saves its prompt and arguments in $HOME, waits, and answers.
fn agents_bin(dir: &Path, delay: u32) -> PathBuf {
    let bin = PathBuf::from(format!("{}-bin{delay}", dir.display()));
    fs::create_dir_all(&bin).unwrap();
    for name in ["claude", "codex", "gemini"] {
        let script = format!(
            "#!/bin/sh\ncat > \"$HOME/{name}-prompt.txt\"\necho \"$@\" > \"$HOME/{name}-args.txt\"\nsleep {delay}\necho \"Fake subject from {name}\"\n"
        );
        fs::write(bin.join(name), script).unwrap();
        Command::new("chmod").arg("+x").arg(bin.join(name)).status().unwrap();
    }
    bin
}

// Returns a PATH holding the stand-in agents that wait the given seconds, ahead of the real PATH.
fn path_with_agents(dir: &Path, delay: u32) -> String {
    format!("{}:{}", agents_bin(dir, delay).display(), std::env::var("PATH").unwrap_or_default())
}

// Makes an empty scratch directory.
fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("zg-e2e-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

// Runs git in a directory and returns its output, failing the test if git fails.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

// Creates a repository with one committed file.
fn repo(name: &str) -> PathBuf {
    let d = scratch(name);
    git(&d, &["init", "-q", "-b", "main"]);
    git(&d, &["config", "user.name", "Test"]);
    git(&d, &["config", "user.email", "test@example.com"]);
    fs::write(d.join("a.txt"), "one\n").unwrap();
    git(&d, &["add", "a.txt"]);
    git(&d, &["commit", "-q", "-m", "first"]);
    d
}

// Checks editing a commit message: typing, saving, and that git's comment block is kept.
#[test]
fn test_editor_saves_message() {
    let d = scratch("save");
    let f = d.join("COMMIT_EDITMSG");
    fs::write(&f, "\n# Please enter the commit message\n").unwrap();
    let mut p = Pty::spawn(EDITOR, &[f.to_str().unwrap()], &d, &[]);
    p.expect("^S Commit");
    p.keys(b"Hello");
    p.expect("subject 5/50");
    p.keys(b"\x13");
    assert_eq!(p.exit(), 0);
    assert_eq!(fs::read_to_string(&f).unwrap(), "Hello\n# Please enter the commit message\n");
}

// Checks that Esc on an unchanged file cancels with exit 1, and a changed file asks first.
#[test]
fn test_editor_cancels() {
    let d = scratch("cancel");
    let f = d.join("notes.md");
    fs::write(&f, "keep\n").unwrap();
    let mut p = Pty::spawn(EDITOR, &[f.to_str().unwrap()], &d, &[]);
    p.expect("^S Save");
    p.keys(b"x\x1b");
    p.expect("Discard your changes?");
    p.keys(b"y");
    assert_eq!(p.exit(), 1);
    assert_eq!(fs::read_to_string(&f).unwrap(), "keep\n");
    let mut q = Pty::spawn(EDITOR, &[f.to_str().unwrap()], &d, &[]);
    q.expect("^S Save");
    q.keys(b"\x1b");
    assert_eq!(q.exit(), 1);
}

// Checks a full `zazzle git commit`: message, Files view marks ([ADD], [IGNORE], left out) and the result in git.
#[test]
fn test_commit_with_file_marks() {
    let d = repo("marks");
    fs::write(d.join("a.txt"), "two\n").unwrap();
    fs::write(d.join("junk.log"), "x\n").unwrap();
    fs::write(d.join("new.txt"), "n\n").unwrap();
    fs::write(d.join("skip.txt"), "s\n").unwrap();
    let mut p = Pty::spawn(ZCOMMIT, &[], &d, &[]);
    p.expect("Tab Files");
    p.keys(b"Change a");
    p.keys(b"\t");
    p.expect("[ADD]");
    p.keys(b"\x1b[B");
    p.keys(b"  ");
    p.keys(b"\x1b[B");
    p.keys(b" ");
    p.expect("[IGNORE]");
    p.keys(b"\x13");
    assert_eq!(p.exit(), 0);
    assert_eq!(git(&d, &["log", "-1", "--format=%B"]).trim(), "Change a");
    let files = git(&d, &["show", "--name-only", "--format=", "HEAD"]);
    assert_eq!(files.lines().collect::<Vec<_>>(), vec![".gitignore", "a.txt", "new.txt"]);
    assert_eq!(fs::read_to_string(d.join(".gitignore")).unwrap(), "/junk.log\n");
    assert!(git(&d, &["status", "--porcelain"]).contains("?? skip.txt"));
    assert!(!d.join(".git/ZAZZLE_COMMIT_MSG").exists());
}

// Checks that [DELETE] asks before committing and deletes the file, and that Toggle-all and Remove work.
#[test]
fn test_commit_deletes_after_confirming() {
    let d = repo("delete");
    fs::write(d.join("b.txt"), "b\n").unwrap();
    git(&d, &["add", "b.txt"]);
    git(&d, &["commit", "-q", "-m", "add b"]);
    fs::write(d.join("a.txt"), "changed\n").unwrap();
    fs::write(d.join("b.txt"), "changed\n").unwrap();
    fs::write(d.join("tmp.bin"), "t\n").unwrap();
    let mut p = Pty::spawn(ZGIT, &["commit"], &d, &[]);
    p.expect("Tab Files");
    p.keys(b"Tidy\t");
    p.keys(b"a");
    p.keys(b"\x1b[B");
    p.keys(b"  ");
    p.expect("[REMOVE]");
    p.keys(b"\x1b[B");
    p.keys(b"\x7f");
    p.keys(b"\x13");
    p.expect("Delete 1 file from disk?");
    p.keys(b"n");
    p.keys(b"\x13");
    p.keys(b"y");
    assert_eq!(p.exit(), 0);
    assert!(!d.join("tmp.bin").exists());
    assert!(d.join("b.txt").exists());
    assert_eq!(git(&d, &["ls-files"]).trim(), "a.txt");
}

// Checks that a failing hook keeps the draft and marks, the next run reopens them, and a new commit forgets them.
#[test]
fn test_commit_keeps_draft_when_git_fails() {
    let d = repo("draft");
    fs::write(d.join("a.txt"), "two\n").unwrap();
    let hook = d.join(".git/hooks/pre-commit");
    fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
    Command::new("chmod").arg("+x").arg(&hook).status().unwrap();
    let mut p = Pty::spawn(ZCOMMIT, &["--no-add"], &d, &[]);
    p.expect("Tab Files");
    p.keys(b"Keep me\t");
    p.keys(b" ");
    p.keys(b"\x13");
    assert_eq!(p.exit(), 1);
    assert!(fs::read_to_string(d.join(".git/ZAZZLE_COMMIT_MSG")).unwrap().starts_with("Keep me\n"));
    assert!(fs::read_to_string(d.join(".git/ZAZZLE_COMMIT_FILES")).unwrap().contains("\"a.txt\":\"add\""));
    fs::remove_file(&hook).unwrap();
    let mut q = Pty::spawn(ZCOMMIT, &[], &d, &[]);
    q.expect("Keep me");
    q.keys(b"\x13");
    assert_eq!(q.exit(), 0);
    assert_eq!(git(&d, &["log", "-1", "--format=%s"]).trim(), "Keep me");
    assert!(!d.join(".git/ZAZZLE_COMMIT_FILES").exists());
}

// Checks an interactive rebase with the editor as both sequence editor and message editor.
#[test]
fn test_rebase_squash() {
    let d = repo("rebase");
    for n in ["b", "c"] {
        fs::write(d.join(format!("{n}.txt")), n).unwrap();
        git(&d, &["add", "."]);
        git(&d, &["commit", "-q", "-m", &format!("add {n}")]);
    }
    let env = [("GIT_SEQUENCE_EDITOR", EDITOR), ("GIT_EDITOR", EDITOR)];
    let mut p = Pty::spawn("git", &["rebase", "-i", "HEAD~2"], &d, &env);
    p.expect("^S Start rebase");
    p.keys(b"\x1b[B");
    p.keys(b"s");
    p.expect("squash");
    p.keys(b"\x13");
    p.expect("^S Commit");
    p.keys(b"\x13");
    assert_eq!(p.exit(), 0);
    assert_eq!(git(&d, &["rev-list", "--count", "HEAD"]).trim(), "2");
}

// Checks forwarding to git, version, `commit -m` and the nothing-to-commit error.
#[test]
fn test_zazzle_git_commands() {
    let d = repo("forward");
    let run = |args: &[&str]| Command::new(ZGIT).args(args).current_dir(&d).env("HOME", &d).output().unwrap();
    let out = run(&["rev-parse", "--abbrev-ref", "HEAD"]);
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "main");
    let out = run(&["status", "--no-such-flag"]);
    assert_ne!(out.status.code(), Some(0));
    let out = run(&["--version"]);
    assert!(String::from_utf8_lossy(&out.stdout).contains("git version"));
    let out = run(&["commit"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("nothing to commit"));
    fs::write(d.join("a.txt"), "three\n").unwrap();
    let out = run(&["commit", "-q", "-m", "direct"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(git(&d, &["log", "-1", "--format=%s"]).trim(), "direct");
    let out = Command::new(ZCOMMIT)
        .current_dir(std::env::temp_dir())
        .env("GIT_CEILING_DIRECTORIES", std::env::temp_dir())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
}

// Checks resizing (including a zero size), copying to the terminal clipboard, and that SIGTERM cancels and restores the terminal.
#[test]
fn test_editor_resize_copy_and_signal() {
    let d = scratch("signal");
    let f = d.join("PR.md");
    fs::write(&f, "body text\n").unwrap();
    let mut p = Pty::spawn(EDITOR, &[f.to_str().unwrap()], &d, &[]);
    p.expect("^S Save");
    p.resize(10, 120);
    p.resize(0, 0);
    p.keys(b"\x01\x03");
    p.expect("\x1b]52;c;Ym9keSB0ZXh0\x07");
    p.keys(b"more");
    p.signal(libc::SIGTERM);
    p.expect("\x1b[?1049l");
    assert_eq!(p.exit(), 1);
    assert_eq!(fs::read_to_string(&f).unwrap(), "body text\n");
}

// Checks `zazzle git commit --amend` with no changes: the draft starts from the last message.
#[test]
fn test_commit_amend() {
    let d = repo("amend");
    let mut p = Pty::spawn(ZCOMMIT, &["--amend"], &d, &[]);
    p.expect("first");
    p.keys(b"Re \x13");
    assert_eq!(p.exit(), 0);
    assert_eq!(git(&d, &["log", "--format=%s"]).trim(), "Re first");
}

// Checks a commit in a repository with no commits yet, where leaving a staged file out unstages it with git rm --cached.
#[test]
fn test_commit_first_commit_unstages() {
    let d = scratch("unborn");
    git(&d, &["init", "-q", "-b", "main"]);
    git(&d, &["config", "user.name", "Test"]);
    git(&d, &["config", "user.email", "test@example.com"]);
    fs::write(d.join("staged.txt"), "s\n").unwrap();
    fs::write(d.join("wanted.txt"), "w\n").unwrap();
    git(&d, &["add", "staged.txt"]);
    let mut p = Pty::spawn(ZCOMMIT, &[], &d, &[]);
    p.expect("Tab Files");
    p.keys(b"Start\t");
    p.keys(b"   ");
    p.keys(b"\x1b[B");
    p.keys(b" ");
    p.keys(b"\x13");
    assert_eq!(p.exit(), 0);
    assert_eq!(git(&d, &["ls-files"]).trim(), "wanted.txt");
}

// Checks that Esc in `zazzle git commit` cancels with exit 1 and makes no commit, and that -m with --no-add skips -a.
#[test]
fn test_commit_cancel_and_no_add_message() {
    let d = repo("cancel-commit");
    fs::write(d.join("a.txt"), "two\n").unwrap();
    let mut p = Pty::spawn(ZCOMMIT, &[], &d, &[]);
    p.expect("Tab Files");
    p.keys(b"\x1b");
    assert_eq!(p.exit(), 1);
    assert_eq!(git(&d, &["rev-list", "--count", "HEAD"]).trim(), "1");
    let out =
        Command::new(ZCOMMIT).args(["--no-add", "-m", "x"]).current_dir(&d).env("HOME", &d).output().unwrap();
    assert_ne!(out.status.code(), Some(0));
    assert_eq!(git(&d, &["rev-list", "--count", "HEAD"]).trim(), "1");
}

// Checks the one-time offer to make zazzle git's editor: yes sets core.editor, no leaves it, and neither asks again.
#[test]
fn test_commit_offers_editor_once() {
    for (answer, set) in [("y", true), ("n", false)] {
        let d = repo(&format!("offer-{answer}"));
        let home = scratch(&format!("offer-home-{answer}"));
        fs::write(d.join("a.txt"), "two\n").unwrap();
        let env = [("HOME", home.to_str().unwrap())];
        let mut p = Pty::spawn(ZCOMMIT, &[], &d, &env);
        p.expect("[y/N]");
        p.keys(format!("{answer}\r").as_bytes());
        p.expect("^S Commit");
        p.keys(b"\x1b");
        assert_eq!(p.exit(), 1);
        let config = fs::read_to_string(home.join(".gitconfig")).unwrap();
        assert_eq!(config.contains("editor = zazzle git message"), set, "{config}");
        assert!(config.contains("editorOffered = true"));
        let mut q = Pty::spawn(ZCOMMIT, &[], &d, &env);
        q.expect("^S Commit");
        q.keys(b"\x1b");
        let screen = String::from_utf8_lossy(&q.screen.lock().unwrap()).into_owned();
        assert_eq!(q.exit(), 1);
        assert!(!screen.contains("[y/N]"));
    }
}

// Checks the default message: the agent composes while the dots grow, and Ctrl+S on the empty message commits its suggestion.
#[test]
fn test_commit_uses_agent_suggestion() {
    let d = repo("agent");
    fs::write(d.join("a.txt"), "changed line\n").unwrap();
    let path = path_with_agents(&d, 1);
    let mut p = Pty::spawn(ZCOMMIT, &[], &d, &[("PATH", &path)]);
    p.expect("claude · effort low · claude is composing default commit message.");
    p.expect("default commit message..");
    p.expect("suggestion ready");
    p.expect("Fake subject from claude");
    p.keys(b"\x13");
    assert_eq!(p.exit(), 0);
    assert_eq!(git(&d, &["log", "-1", "--format=%s"]).trim(), "Fake subject from claude");
    let home = offered_home(&d);
    let prompt = fs::read_to_string(home.join("claude-prompt.txt")).unwrap();
    assert!(
        prompt.starts_with("Write a git commit message")
            && prompt.contains("+changed line")
            && prompt.contains("first")
    );
    let args = fs::read_to_string(home.join("claude-args.txt")).unwrap();
    assert!(args.contains("-p --tools  --no-session-persistence --effort low"), "{args}");
}

// Checks the prompt file from the home folder, cycling agent and effort (remembered for the next run), and a save that waits for the answer.
#[test]
fn test_commit_agent_prompt_file_and_cycling() {
    let d = repo("cycle");
    let home = scratch("cycle-home");
    fs::create_dir_all(home.join(".zazzle")).unwrap();
    fs::write(home.join(".zazzle/commit-message-prompt.md"), "Custom instructions XYZ").unwrap();
    fs::write(home.join(".gitconfig"), "[zazzle]\n\teditorOffered = true\n").unwrap();
    fs::write(d.join("a.txt"), "two\n").unwrap();
    let path = path_with_agents(&d, 1);
    let env = [("HOME", home.to_str().unwrap()), ("PATH", path.as_str())];
    let mut p = Pty::spawn(ZCOMMIT, &[], &d, &env);
    p.expect("claude is composing");
    p.keys(b"\x0e");
    p.expect("Agent: codex");
    p.keys(b"\x05");
    p.expect("Effort: medium");
    p.keys(b"\x13");
    p.expect("Saving when codex is done");
    assert_eq!(p.exit(), 0);
    assert_eq!(git(&d, &["log", "-1", "--format=%s"]).trim(), "Fake subject from codex");
    assert!(fs::read_to_string(home.join("codex-prompt.txt"))
        .unwrap()
        .starts_with("Custom instructions XYZ"));
    assert!(fs::read_to_string(home.join("codex-args.txt"))
        .unwrap()
        .contains("model_reasoning_effort=medium"));
    let config = fs::read_to_string(home.join(".gitconfig")).unwrap();
    assert!(config.contains("agent = codex") && config.contains("effort = medium"), "{config}");
    fs::write(d.join("a.txt"), "three\n").unwrap();
    let mut q = Pty::spawn(ZCOMMIT, &[], &d, &env);
    q.expect("codex · effort medium · codex is composing");
    q.keys(b"\x1b");
    assert_eq!(q.exit(), 1);
}

// Checks that without any agent CLI the editor says so and an empty message still cannot be saved.
#[test]
fn test_commit_without_agents() {
    let d = repo("noagent");
    fs::write(d.join("a.txt"), "two\n").unwrap();
    let out = Command::new("sh").args(["-c", "command -v git"]).output().unwrap();
    let real_git = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let bin = PathBuf::from(format!("{}-gitonly", d.display()));
    fs::create_dir_all(&bin).unwrap();
    std::os::unix::fs::symlink(&real_git, bin.join("git")).unwrap();
    let path = format!("{}:/usr/bin:/bin", bin.display());
    let mut p = Pty::spawn(ZCOMMIT, &[], &d, &[("PATH", &path)]);
    p.expect("No agent CLI found (claude, codex or gemini): write the message yourself");
    p.keys(b"\x13");
    p.expect("The message is empty");
    p.keys(b"\x1b");
    assert_eq!(p.exit(), 1);
}

// Checks the agent when git itself opens the editor: the prompt holds the staged diff, and an empty message commits the suggestion.
#[test]
fn test_git_commit_editor_uses_staged_diff() {
    let d = repo("staged");
    fs::write(d.join("a.txt"), "staged change\n").unwrap();
    git(&d, &["add", "a.txt"]);
    let path = path_with_agents(&d, 0);
    let mut p = Pty::spawn("git", &["commit"], &d, &[("GIT_EDITOR", EDITOR), ("PATH", &path)]);
    p.expect("suggestion ready");
    p.keys(b"\x13");
    assert_eq!(p.exit(), 0);
    assert_eq!(git(&d, &["log", "-1", "--format=%s"]).trim(), "Fake subject from claude");
    let prompt = fs::read_to_string(offered_home(&d).join("claude-prompt.txt")).unwrap();
    assert!(prompt.contains("+staged change"));
}
