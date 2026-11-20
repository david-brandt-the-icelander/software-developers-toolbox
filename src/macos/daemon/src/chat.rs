// Per-terminal chats with the local model, kept by daemon: each terminal's model, effort and conversation live in a file under ~/.zazzle/v1/chats, so a chat resumes after the worker or the daemon stopped; `zazzle agent chat` and `zazzle agent prompt` drive them through the internal `llm-chat` command.
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;

// How much conversation a chat keeps, in characters; the oldest turns go first.
const MAX_HISTORY: usize = 48_000;
// A chat untouched this long starts afresh.
const STALE: Duration = Duration::from_secs(7 * 24 * 3600);

pub const USAGE: &str = "usage: llm-chat open|clear|release KEY | llm-chat set KEY [--model M] [--effort E] | llm-chat send KEY [--effort E] [--system S] [--max-tokens N] -- TEXT";

// One terminal's chat: the model (None until chosen), the effort and the turns so far as (role, text).
#[derive(Debug, Default, PartialEq)]
pub struct Chat {
    pub model: Option<String>,
    pub effort: String,
    pub history: Vec<(String, String)>,
}

// Returns the folder chats are kept in: ZAZZLE_CHAT_DIR, else ~/.zazzle/v1/chats.
fn dir() -> Result<PathBuf, String> {
    match std::env::var_os("ZAZZLE_CHAT_DIR") {
        Some(d) => Ok(PathBuf::from(d)),
        None => crate::store::default_dir().map(|d| d.join("chats")).map_err(|e| e.to_string()),
    }
}

// Returns the file of a terminal's chat, named by a hash of its key.
fn file(dir: &Path, key: &str) -> PathBuf {
    dir.join(format!("{}.json", &blake3::hash(key.as_bytes()).to_hex()[..20]))
}

// Loads a terminal's chat; None when there is none or it is older than a week.
pub fn load(dir: &Path, key: &str) -> Option<Chat> {
    let f = file(dir, key);
    let age = f.metadata().and_then(|m| m.modified()).ok()?.elapsed().unwrap_or_default();
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&f).ok()?).ok()?;
    let history = v["history"].as_array().map(Vec::as_slice).unwrap_or_default();
    (age < STALE).then(|| Chat {
        model: v["model"].as_str().map(str::to_string),
        effort: v["effort"].as_str().unwrap_or("low").to_string(),
        history: history
            .iter()
            .filter_map(|t| Some((t[0].as_str()?.to_string(), t[1].as_str()?.to_string())))
            .collect(),
    })
}

// Saves a terminal's chat, dropping its oldest turns past the history limit.
pub fn save(dir: &Path, key: &str, chat: &mut Chat) -> Result<(), String> {
    while chat.history.iter().map(|(_, t)| t.len()).sum::<usize>() > MAX_HISTORY && chat.history.len() > 2 {
        chat.history.drain(..2);
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let v = json!({"model": chat.model, "effort": chat.effort, "history": chat.history});
    let f = file(dir, key);
    std::fs::write(&f, v.to_string()).map_err(|e| format!("{}: {e}", f.display()))
}

// Something that answers a chat turn: the model and the request.
pub type Ask<'a> = &'a dyn Fn(Option<&str>, &Value) -> Result<crate::llm::Reply, String>;

// Runs one `llm-chat` operation in a chats folder and returns its text: open (JSON with whether a chat is resumed), set, clear, release, or send (the answer).
pub fn op(dir: &Path, argv: &[String], ask: Ask) -> Result<String, String> {
    let (Some(op), Some(key)) = (argv.first(), argv.get(1)) else { return Err(USAGE.into()) };
    let found = load(dir, key);
    let resumed = found.as_ref().is_some_and(|c| !c.history.is_empty());
    let mut chat = found.unwrap_or_else(|| Chat { effort: "low".into(), ..Chat::default() });
    let (mut words, mut req, mut model, mut it) = (Vec::new(), json!({}), None, argv[2..].iter());
    while let Some(a) = it.next() {
        let mut value = || it.next().cloned().ok_or_else(|| format!("{a} needs a value; {USAGE}"));
        match a.as_str() {
            "--model" => model = Some(value()?),
            "--effort" if op == "set" => chat.effort = value()?,
            "--effort" | "--system" => req[&a[2..]] = value()?.into(),
            "--max-tokens" => {
                req["max_tokens"] = value()?.parse::<u64>().map_err(|_| "--max-tokens needs a number")?.into()
            }
            "--" => words.extend(it.by_ref().cloned()),
            f if f.starts_with("--") => return Err(format!("unknown flag {f}; {USAGE}")),
            w => words.push(w.to_string()),
        }
    }
    match op.as_str() {
        "open" => {
            let turns = chat.history.len() / 2;
            Ok(json!({"resumed": resumed, "turns": turns, "model": chat.model, "effort": chat.effort})
                .to_string())
        }
        "set" => {
            chat.model = model.or(chat.model);
            save(dir, key, &mut chat).map(|()| format!("model {}, effort {}", shown(&chat), chat.effort))
        }
        "clear" => {
            chat.history.clear();
            save(dir, key, &mut chat).map(|()| "cleared: the next message starts a new conversation".into())
        }
        "release" => {
            let _ = std::fs::remove_file(file(dir, key));
            Ok("released".into())
        }
        "send" => {
            let text = words.join(" ");
            if text.trim().is_empty() {
                return Err(USAGE.into());
            }
            req["user"] = text.clone().into();
            req["history"] = chat.history.iter().map(|(r, c)| json!({"role": r, "content": c})).collect();
            if req["effort"].is_null() {
                req["effort"] = chat.effort.clone().into();
            }
            let reply = ask(model.as_deref().or(chat.model.as_deref()), &req)?;
            chat.history.extend([("user".into(), text), ("assistant".into(), reply.text.clone())]);
            save(dir, key, &mut chat)?;
            let cut = if reply.truncated { "\n[the token limit cut this answer short]" } else { "" };
            Ok(format!("{}{cut}", reply.text))
        }
        _ => Err(USAGE.into()),
    }
}

// Names the chat's model, or the default.
fn shown(chat: &Chat) -> &str {
    chat.model.as_deref().unwrap_or("the default model")
}

// Runs `llm-chat` in the daemon with the shared worker.
pub fn run(argv: &[String]) -> Result<String, String> {
    op(&dir()?, argv, &crate::llm::ask)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::llm::Reply;
    use crate::testutil::TempDir;

    // Answers with the model, effort and the number of earlier turns, and fails on `boom`.
    fn echo(model: Option<&str>, req: &Value) -> Result<Reply, String> {
        if req["user"] == "boom" {
            return Err("local model: down".into());
        }
        let n = req["history"].as_array().map_or(0, Vec::len);
        let text = format!(
            "{}|{}|{n}|{}",
            model.unwrap_or("-"),
            req["effort"].as_str().unwrap_or("-"),
            req["user"].as_str().unwrap_or("")
        );
        Ok(Reply { text, truncated: req["user"] == "long", tokens: 0 })
    }

    // Runs an operation with the echo model.
    fn go(dir: &Path, argv: &[&str]) -> Result<String, String> {
        op(dir, &argv.iter().map(|s| s.to_string()).collect::<Vec<_>>(), &echo)
    }

    // Checks a chat starts at low effort, keeps its turns across calls, takes a model and effort, clears, and is released.
    #[test]
    fn test_chat_turns_persist() {
        let t = TempDir::new();
        let d = t.path().join("chats");
        let fresh = serde_json::json!({"effort": "low", "model": null, "resumed": false, "turns": 0});
        let open =
            |k: &str| serde_json::from_str::<Value>(&go(&d, &["open", k]).expect("open")).expect("json");
        assert_eq!(open("tty1"), fresh);
        assert_eq!(go(&d, &["send", "tty1", "--", "hi", "there"]).expect("send"), "-|low|0|hi there");
        assert_eq!(
            go(&d, &["send", "tty1", "--effort", "high", "--", "again"]).expect("again"),
            "-|high|2|again"
        );
        assert_eq!((open("tty1")["resumed"].clone(), open("tty1")["turns"].clone()), (json!(true), json!(2)));
        assert_eq!(open("tty2"), fresh);
        assert_eq!(
            go(&d, &["set", "tty1", "--model", "qwen3.5-9b", "--effort", "medium"]).expect("set"),
            "model qwen3.5-9b, effort medium"
        );
        assert_eq!(go(&d, &["send", "tty1", "x"]).expect("model"), "qwen3.5-9b|medium|4|x");
        assert_eq!(
            go(&d, &["send", "tty1", "--model", "gpt-oss-20b", "y"]).expect("once"),
            "gpt-oss-20b|medium|6|y"
        );
        assert_eq!(open("tty1")["model"], "qwen3.5-9b", "--model on send is for that message");
        assert_eq!(
            go(&d, &["send", "tty1", "long"]).expect("long"),
            "qwen3.5-9b|medium|8|long\n[the token limit cut this answer short]"
        );
        assert!(go(&d, &["clear", "tty1"]).expect("clear").starts_with("cleared"));
        assert_eq!(go(&d, &["send", "tty1", "y"]).expect("cleared"), "qwen3.5-9b|medium|0|y");
        assert_eq!(go(&d, &["release", "tty1"]).expect("release"), "released");
        assert_eq!(open("tty1")["resumed"], false);
        assert_eq!(go(&d, &["set", "tty3"]).expect("bare"), "model the default model, effort low");
    }

    // Checks a failed turn is not recorded, the history is trimmed oldest first, and bad command lines are refused.
    #[test]
    fn test_chat_errors_and_trim() {
        let t = TempDir::new();
        let d = t.path().to_path_buf();
        assert_eq!(go(&d, &["send", "k", "boom"]), Err("local model: down".into()));
        assert!(go(&d, &["open", "k"]).expect("open").contains(r#""turns":0"#));
        // Each echoed turn is ~40,000 characters (the answer repeats the message), so only the last one fits.
        let big = "w".repeat(20_000);
        for _ in 0..3 {
            go(&d, &["send", "k", &big]).expect("big");
        }
        let c = load(&d, "k").expect("chat");
        assert!(c.history.len() == 2 && c.history[0].0 == "user", "{}", c.history.len());
        let huge = "h".repeat(60_000);
        go(&d, &["send", "solo", &huge]).expect("one huge turn");
        assert_eq!(load(&d, "solo").expect("solo").history.len(), 2, "the newest turn is always kept");
        for bad in [
            &["send"][..],
            &["send", "k"],
            &["nope", "k"],
            &["send", "k", "--model"],
            &["send", "k", "--max-tokens", "x", "a"],
            &["send", "k", "--bogus", "a"],
        ] {
            assert!(go(&d, bad).is_err(), "{bad:?}");
        }
        std::fs::write(file(&d, "junk"), "not json").expect("junk");
        assert_eq!(load(&d, "junk"), None);
        assert_eq!(
            go(&d, &["send", "k", "--max-tokens", "9", "--system", "s", "--", "ok"]).expect("flags"),
            "-|low|2|ok"
        );
    }
}
