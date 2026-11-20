// Tests for the chat client against the real `llm-chat` operations in a temporary folder, with a model that echoes and a fetch that resolves names.
use super::*;
use crate::llm::Reply;
use crate::testutil::TempDir;
use std::cell::RefCell;

// Answers with the model, effort, earlier turns and the message.
fn echo(model: Option<&str>, req: &Value) -> Result<Reply, String> {
    let n = req["history"].as_array().map_or(0, Vec::len);
    let text = format!(
        "{}|{}|{n}|{}",
        model.unwrap_or("-"),
        req["effort"].as_str().unwrap_or("-"),
        req["user"].as_str().unwrap_or("")
    );
    Ok(Reply { text, truncated: req["user"] == "long", tokens: 0, queued_ms: 0 })
}

// Resolves a name prefix to a full model name, `qwen3.5-4b` by default, and fails on `nope`.
fn fetch(m: Option<&str>) -> Result<String, String> {
    match m {
        None => Ok("qwen3.5-4b".into()),
        Some("nope") => Err("unknown model nope".into()),
        Some(m) if m.starts_with("gemma") => Ok("gemma-4-12b".into()),
        Some(m) => Ok(m.to_string()),
    }
}

// Runs `f` with an Io whose daemon answers `llm-chat` from a chats folder in `t`.
fn with<T>(t: &TempDir, f: impl FnOnce(&Io) -> T) -> T {
    let dir = t.path().join("chats");
    let daemon = move |a: &[String]| {
        assert_eq!(a[0], "llm-chat");
        crate::chat::op(&dir, &a[1..], &echo)
    };
    f(&Io { daemon: &daemon, fetch: &fetch })
}

// Scripted keys for the chat loop.
struct Keys(RefCell<Vec<Key>>);

impl Input for Keys {
    // Gives the next scripted key, then the end.
    fn next(&mut self, _out: &mut dyn Write) -> Key {
        let mut k = self.0.borrow_mut();
        if k.is_empty() {
            Key::Eof
        } else {
            k.remove(0)
        }
    }
}

// Runs the chat loop with scripted keys and returns what it printed.
fn session(io: &Io, key: &str, keys: Vec<Key>) -> String {
    let mut out = Vec::new();
    chat(io, key, &mut Keys(RefCell::new(keys)), &mut out).expect("chat");
    String::from_utf8(out).expect("utf8")
}

// Checks a new chat at low effort with the default model, a resumed one with its turn count, `/` commands, Ctrl+K and leaving.
#[test]
fn test_chat_loop() {
    let t = TempDir::new();
    with(&t, |io| {
        let s = |l: &str| Key::Submit(l.into());
        let first =
            session(io, "tty1", vec![s("hi"), s(""), s("/effort high"), s("again"), s("/quit"), s("never")]);
        assert!(
            first.starts_with("zazzle chat with qwen3.5-4b at effort low: new conversation\n"),
            "{first}"
        );
        assert!(
            first.contains("qwen3.5-4b|low|0|hi\n") && first.contains("qwen3.5-4b|high|2|again\n"),
            "{first}"
        );
        assert!(!first.contains("never"));
        let second = session(
            io,
            "tty1",
            vec![
                s("/status"),
                Key::Release,
                s("fresh"),
                s("/model gemma"),
                s("x"),
                s("/model nope"),
                s("/bogus"),
                Key::Interrupt,
            ],
        );
        assert!(
            second.contains("at effort high: resuming this terminal's conversation (2 turns)"),
            "{second}"
        );
        assert!(second.contains("model qwen3.5-4b, effort high, 2 turns in this conversation"), "{second}");
        assert!(
            second.contains("new conversation (qwen3.5-4b, effort high)")
                && second.contains("qwen3.5-4b|high|0|fresh"),
            "{second}"
        );
        assert!(
            second.contains("model gemma-4-12b, effort high") && second.contains("gemma-4-12b|high|2|x"),
            "{second}"
        );
        assert!(
            second.contains("error: unknown model nope") && second.contains("error: unknown command /bogus"),
            "{second}"
        );
        let other = session(io, "tty2", vec![s("/help"), s("/clear"), s("/effort max"), s("/effort")]);
        assert!(
            other.contains("new conversation")
                && other.contains("/model [NAME]")
                && other.contains("cleared"),
            "{other}"
        );
        assert!(
            other.contains("error: effort is low, medium or high, not `max`")
                && other.contains("effort low, 0 turns"),
            "{other}"
        );
        let piped = session(io, "tty3", vec![]);
        let mut out = Vec::new();
        chat(io, "tty3", &mut Lines("one\n/quit\n".as_bytes()), &mut out).expect("lines");
        assert!(
            piped.ends_with("› \n") && String::from_utf8(out).expect("u").contains("qwen3.5-4b|low|0|one")
        );
    });
}

// Checks `agent prompt` joins the terminal's chat without a banner, reads standard input for `-`, runs `/` commands, takes per-message flags, exits 3 when cut short, and refuses bad command lines.
#[test]
fn test_prompt() {
    let t = TempDir::new();
    with(&t, |io| {
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let mut stdin = || "PIPED".to_string();
        assert_eq!(
            prompt(io, "k", &v(&["hello", "there"]), &mut stdin),
            Ok(("qwen3.5-4b|low|0|hello there".into(), 0))
        );
        assert_eq!(
            prompt(io, "k", &v(&["Sum:", "-"]), &mut stdin),
            Ok(("qwen3.5-4b|low|2|Sum: PIPED".into(), 0))
        );
        assert_eq!(prompt(io, "k", &v(&[]), &mut stdin).expect("stdin").0, "qwen3.5-4b|low|4|PIPED");
        let once = prompt(
            io,
            "k",
            &v(&["--model", "gemma", "--effort", "high", "--max-tokens", "9", "--system", "s", "--", "--x"]),
            &mut stdin,
        );
        assert_eq!(once, Ok(("gemma-4-12b|high|6|--x".into(), 0)));
        assert_eq!(
            prompt(io, "k", &v(&["/clear"]), &mut stdin).expect("clear").0,
            "cleared: the next message starts a new conversation"
        );
        assert_eq!(prompt(io, "k", &v(&["/quit"]), &mut stdin), Ok((String::new(), 0)));
        let (cut, code) = prompt(io, "k", &v(&["long"]), &mut stdin).expect("long");
        assert_eq!((cut.starts_with("qwen3.5-4b|low|0|long"), code), (true, 3));
        let mut empty = String::new;
        assert_eq!(prompt(io, "k", &v(&[]), &mut empty), Err(PROMPT_USAGE.into()));
        assert!(prompt(io, "k", &v(&["--temp", "0", "x"]), &mut stdin)
            .expect_err("flag")
            .contains("unknown flag --temp"));
        assert!(prompt(io, "k", &v(&["--effort"]), &mut stdin)
            .expect_err("value")
            .contains("--effort needs a value"));
        assert!(prompt(io, "k", &v(&["/nope"]), &mut stdin).is_err());
    });
}

// Checks the line editor: typing, UTF-8 split across bytes, backspace, Ctrl+U, Enter, arrows ignored, Ctrl+C, Ctrl+K and Ctrl+D only on an empty line.
#[test]
fn test_editor() {
    let mut e = Editor::default();
    let feed = |e: &mut Editor, bytes: &[u8]| bytes.iter().map(|&b| e.feed(b)).last().expect("key");
    assert_eq!(feed(&mut e, b"ab"), Key::Redraw);
    assert_eq!(feed(&mut e, "é".as_bytes()), Key::Redraw);
    assert_eq!(e.feed(0xc3), Key::None);
    assert_eq!(feed(&mut e, b"\x1b[D\x1bOA"), Key::None);
    assert_eq!(e.feed(0x7f), Key::None);
    assert_eq!(e.feed(0x04), Key::None);
    assert_eq!(e.feed(0x01), Key::None);
    assert_eq!(e.feed(b'\r'), Key::Submit("abé".into()));
    assert_eq!(feed(&mut e, b"zz\x15"), Key::Redraw);
    assert_eq!(e.line, "");
    assert_eq!((e.feed(0x03), e.feed(0x0b), e.feed(0x04)), (Key::Interrupt, Key::Release, Key::Eof));
    assert_eq!(feed(&mut e, &[0xff, 0xff, 0xff, 0xff, b'x']), Key::Redraw);
    assert_eq!(e.line, "x");
    assert_eq!(feed(&mut e, b"\x1bq\x1b[1;5D"), Key::None);
    assert_eq!((e.feed(0x04), e.line.as_str()), (Key::None, "x"), "Ctrl+D leaves only on an empty line");
    assert_eq!(
        (e.feed(0xe2), e.feed(0x08), e.feed(b'y'), e.line.as_str()),
        (Key::None, Key::None, Key::Redraw, "xy"),
        "a backspace drops a partial character, not the line"
    );
    let _g = crate::testutil::env_lock();
    std::env::set_var("ZAZZLE_CHAT_SESSION", "mine");
    assert_eq!(terminal_key(), "mine");
    std::env::remove_var("ZAZZLE_CHAT_SESSION");
    assert!(terminal_key().contains(':'));
    assert_eq!(text(&["a".into(), "-".into()], &mut || "b".into()), "a b");
}
