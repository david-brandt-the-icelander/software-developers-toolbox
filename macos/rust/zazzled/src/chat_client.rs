// `zazzle agent chat` and `zazzle agent prompt` on the command line: both talk to this terminal's chat in zazzled (one per terminal, kept between runs), so the model stays loaded in the daemon's worker; `/` commands switch effort and model, clear or start over.
use serde_json::Value;
use std::io::{BufRead, Write};

pub const PROMPT_USAGE: &str =
    "usage: zazzle agent prompt [--model M] [--effort low|medium|high] [--system TEXT] [--max-tokens N] [--] TEXT.. (`-` or no TEXT reads standard input; /help for commands)";
const HELP: &str = "/effort [low|medium|high]  show or set how hard the model thinks (low: no thinking)
/model [NAME]              show or switch the model (downloaded first when needed)
/clear                     forget the conversation, keep model and effort
/new                       release this terminal's chat and start a new one (also Ctrl+K)
/status                    model, effort and turns so far
/quit                      leave (also Ctrl+C); the chat is kept for this terminal";

// What the client needs from outside: the daemon's `llm-chat` command, and zazzle-llm's `fetch` (downloads a model, returns its full name).
pub struct Io<'a> {
    pub daemon: &'a dyn Fn(&[String]) -> Result<String, String>,
    pub fetch: &'a dyn Fn(Option<&str>) -> Result<String, String>,
}

// What a `/` command asks the chat loop to do after printing its text.
#[derive(Debug, PartialEq)]
pub enum Next {
    Stay(String),
    Quit,
}

// Calls `llm-chat OP KEY ARGS..` in the daemon.
fn call(io: &Io, op: &str, key: &str, args: &[&str]) -> Result<String, String> {
    let mut a = vec!["llm-chat".to_string(), op.into(), key.into()];
    a.extend(args.iter().map(|s| s.to_string()));
    (io.daemon)(&a)
}

// Opens this terminal's chat, choosing (and downloading) the default model the first time; returns whether it is resumed, its turns, model and effort.
fn open(io: &Io, key: &str) -> Result<(bool, u64, String, String), String> {
    let v: Value = serde_json::from_str(&call(io, "open", key, &[])?).map_err(|e| e.to_string())?;
    let model = (io.fetch)(v["model"].as_str())?;
    if v["model"].as_str() != Some(model.as_str()) {
        call(io, "set", key, &["--model", &model])?;
    }
    let effort = v["effort"].as_str().unwrap_or("low").to_string();
    Ok((v["resumed"] == true, v["turns"].as_u64().unwrap_or(0), model, effort))
}

// Runs a `/` command against this terminal's chat.
pub fn command(io: &Io, key: &str, line: &str) -> Result<Next, String> {
    let mut words = line.split_whitespace();
    let (cmd, arg) = (words.next().unwrap_or(""), words.next());
    let text = match (cmd, arg) {
        ("/help" | "/?", _) => HELP.to_string(),
        ("/quit" | "/exit", _) => return Ok(Next::Quit),
        ("/clear", _) => call(io, "clear", key, &[])?,
        ("/new", _) => release(io, key)?,
        ("/effort", Some(e)) if ["low", "medium", "high"].contains(&e) => {
            call(io, "set", key, &["--effort", e])?
        }
        ("/effort", Some(e)) => return Err(format!("effort is low, medium or high, not `{e}`")),
        ("/model", Some(m)) => {
            let full = (io.fetch)(Some(m))?;
            call(io, "set", key, &["--model", &full])?
        }
        ("/status" | "/effort" | "/model", None) => {
            let (_, turns, model, effort) = open(io, key)?;
            format!("model {model}, effort {effort}, {turns} turns in this conversation")
        }
        _ => return Err(format!("unknown command {cmd}; /help lists them")),
    };
    Ok(Next::Stay(text))
}

// Releases this terminal's chat and starts a new one with the same model and effort.
fn release(io: &Io, key: &str) -> Result<String, String> {
    let (_, _, model, effort) = open(io, key)?;
    call(io, "release", key, &[])?;
    call(io, "set", key, &["--model", &model, "--effort", &effort])?;
    Ok(format!("new conversation ({model}, effort {effort})"))
}

// Builds the prompt text from the words, putting standard input where a word is `-` or when there are none.
fn text(words: &[String], stdin: &mut dyn FnMut() -> String) -> String {
    if words.is_empty() {
        return stdin();
    }
    words.iter().map(|w| if w == "-" { stdin() } else { w.clone() }).collect::<Vec<_>>().join(" ")
}

// Runs `zazzle agent prompt`: like opening this terminal's chat, sending one message and leaving, without the banner; a `/` command runs as in the chat. Returns the text and the exit status (3 when cut short).
pub fn prompt(
    io: &Io,
    key: &str,
    argv: &[String],
    stdin: &mut dyn FnMut() -> String,
) -> Result<(String, u8), String> {
    let (mut flags, mut words, mut model, mut it) = (Vec::new(), Vec::new(), None, argv.iter());
    while let Some(a) = it.next() {
        let mut value = || it.next().cloned().ok_or_else(|| format!("{a} needs a value; {PROMPT_USAGE}"));
        match a.as_str() {
            "--model" => model = Some(value()?),
            f @ ("--effort" | "--system" | "--max-tokens") => flags.extend([f.to_string(), value()?]),
            "--" => words.extend(it.by_ref().cloned()),
            f if f.starts_with("--") => return Err(format!("unknown flag {f}; {PROMPT_USAGE}")),
            w => words.push(w.to_string()),
        }
    }
    let msg = text(&words, stdin);
    if msg.trim().is_empty() {
        return Err(PROMPT_USAGE.into());
    }
    if msg.starts_with('/') {
        return Ok(match command(io, key, msg.trim())? {
            Next::Stay(t) => (t, 0),
            Next::Quit => (String::new(), 0),
        });
    }
    let (_, _, chosen, _) = open(io, key)?;
    let full = match model {
        Some(m) => (io.fetch)(Some(&m))?,
        None => chosen,
    };
    let mut args: Vec<&str> = vec!["--model", &full];
    args.extend(flags.iter().map(String::as_str));
    args.extend(["--", &msg]);
    let answer = call(io, "send", key, &args)?;
    let code = if answer.ends_with("[the token limit cut this answer short]") { 3 } else { 0 };
    Ok((answer, code))
}

// What one key press means to the line editor.
#[derive(Debug, PartialEq)]
pub enum Key {
    None,
    Redraw,
    Submit(String),
    Interrupt,
    Release,
    Eof,
}

// A one-line editor for raw terminal input: typing, backspace, Ctrl+U to clear, Enter to send, Ctrl+C to leave, Ctrl+K for a new chat, Ctrl+D on an empty line to leave; escape sequences (arrows) are ignored.
#[derive(Default)]
pub struct Editor {
    pub line: String,
    bytes: Vec<u8>,
    escape: u8,
}

impl Editor {
    // Feeds one byte and says what it means.
    pub fn feed(&mut self, b: u8) -> Key {
        if self.escape == 1 {
            self.escape = if b == b'[' || b == b'O' { 2 } else { 0 };
            return Key::None;
        }
        if self.escape == 2 {
            if (0x40..=0x7e).contains(&b) {
                self.escape = 0;
            }
            return Key::None;
        }
        match b {
            0x1b => self.escape = 1,
            0x03 => return Key::Interrupt,
            0x0b => return Key::Release,
            0x04 if self.line.is_empty() => return Key::Eof,
            b'\r' | b'\n' => return Key::Submit(std::mem::take(&mut self.line)),
            0x7f | 0x08 if !self.bytes.is_empty() => self.bytes.clear(),
            0x7f | 0x08 => {
                self.line.pop();
                return Key::Redraw;
            }
            0x15 => {
                self.line.clear();
                return Key::Redraw;
            }
            b if b < 0x20 => {}
            b => {
                self.bytes.push(b);
                if let Ok(s) = std::str::from_utf8(&self.bytes) {
                    self.line.push_str(s);
                    self.bytes.clear();
                    return Key::Redraw;
                }
                if self.bytes.len() >= 4 {
                    self.bytes.clear();
                }
            }
        }
        Key::None
    }
}

// Where the chat reads lines: a raw terminal through the editor, or plain lines from a pipe.
pub trait Input {
    // Reads the next line or key that ends one.
    fn next(&mut self, out: &mut dyn Write) -> Key;
}

// Plain lines from a pipe or file, for scripts and tests.
pub struct Lines<R: BufRead>(pub R);

impl<R: BufRead> Input for Lines<R> {
    // Reads one line; the end of input leaves.
    fn next(&mut self, _out: &mut dyn Write) -> Key {
        let mut s = String::new();
        match self.0.read_line(&mut s) {
            Ok(0) | Err(_) => Key::Eof,
            Ok(_) => Key::Submit(s.trim_end_matches(['\n', '\r']).to_string()),
        }
    }
}

const PROMPT: &str = "› ";

// Runs the chat loop: the banner (saying whether this terminal's chat is resumed), then each message or `/` command until Ctrl+C, Ctrl+D or /quit.
pub fn chat(io: &Io, key: &str, input: &mut dyn Input, out: &mut dyn Write) -> Result<(), String> {
    let (resumed, turns, model, effort) = open(io, key)?;
    let start = match resumed {
        true => format!("resuming this terminal's conversation ({turns} turns)"),
        false => "new conversation".to_string(),
    };
    let _ = writeln!(out, "zazzle chat with {model} at effort {effort}: {start}\n/help for commands, Ctrl+K for a new conversation, Ctrl+C or /quit to leave");
    loop {
        let _ = write!(out, "{PROMPT}").and_then(|()| out.flush());
        let line = match input.next(out) {
            Key::Submit(l) => l,
            Key::Release => {
                let _ = writeln!(out, "\n{}", release(io, key)?);
                continue;
            }
            _ => {
                let _ = writeln!(out);
                return Ok(());
            }
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let reply = match line.starts_with('/') {
            true => command(io, key, line).map(|n| match n {
                Next::Stay(t) => Some(t),
                Next::Quit => None,
            }),
            false => {
                let _ = write!(out, "…").and_then(|()| out.flush());
                let r = call(io, "send", key, &["--", line]).map(Some);
                let _ = write!(out, "\r\x1b[2K");
                r
            }
        };
        match reply {
            Ok(Some(t)) => _ = writeln!(out, "{}\n", t.trim_end()),
            Ok(None) => return Ok(()),
            Err(e) => _ = writeln!(out, "error: {e}\n"),
        }
    }
}

// A raw-mode terminal on standard input, restored when dropped.
pub struct Raw {
    saved: libc::termios,
    editor: Editor,
}

impl Raw {
    // Switches standard input to raw mode (no echo, no line buffering, Ctrl+C as a byte); None when it is not a terminal.
    pub fn new() -> Option<Raw> {
        // SAFETY: termios is plain data; tcgetattr fills it for fd 0 and fails cleanly when it is not a terminal.
        let mut t: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: as above.
        if unsafe { libc::tcgetattr(0, &mut t) } != 0 {
            return None;
        }
        Some(Raw { saved: t, editor: Editor::default() })
    }

    // Sets raw mode on (reading a line) or back to the saved mode (while the model answers, so Ctrl+C still stops this program).
    fn set(&self, raw: bool) {
        let mut t = self.saved;
        if raw {
            t.c_lflag &= !(libc::ICANON | libc::ECHO | libc::ISIG | libc::IEXTEN);
            t.c_iflag &= !(libc::IXON | libc::ICRNL);
            t.c_cc[libc::VMIN] = 1;
            t.c_cc[libc::VTIME] = 0;
        }
        // SAFETY: t is a valid termios for fd 0, read from it above.
        unsafe { libc::tcsetattr(0, libc::TCSANOW, &t) };
    }
}

impl Input for Raw {
    // Reads bytes in raw mode, redrawing the line as it changes, until a key ends it.
    fn next(&mut self, out: &mut dyn Write) -> Key {
        self.set(true);
        let mut b = [0u8; 1];
        let key = loop {
            // SAFETY: b is a valid one-byte buffer.
            if unsafe { libc::read(0, b.as_mut_ptr().cast(), 1) } != 1 {
                break Key::Eof;
            }
            match self.editor.feed(b[0]) {
                Key::Redraw => {
                    _ = write!(out, "\r\x1b[2K{PROMPT}{}", self.editor.line).and_then(|()| out.flush())
                }
                Key::Submit(l) => {
                    let _ = writeln!(out, "\r");
                    break Key::Submit(l);
                }
                Key::None => {}
                k => break k,
            }
        };
        self.set(false);
        key
    }
}

impl Drop for Raw {
    // Restores the terminal.
    fn drop(&mut self) {
        self.set(false);
    }
}

// Names this terminal for its chat: ZAZZLE_CHAT_SESSION, else the terminal device and the shell's session id, so each terminal window or tab has its own chat.
pub fn terminal_key() -> String {
    if let Ok(k) = std::env::var("ZAZZLE_CHAT_SESSION") {
        return k;
    }
    // SAFETY: getsid(0) asks about this process and cannot fail for it.
    let sid = unsafe { libc::getsid(0) };
    for fd in 0..3 {
        // SAFETY: ttyname returns a static string or null for a non-terminal fd.
        let p = unsafe { libc::ttyname(fd) };
        if !p.is_null() {
            // SAFETY: p is a valid NUL-terminated string from ttyname.
            let name = unsafe { std::ffi::CStr::from_ptr(p) }.to_string_lossy().into_owned();
            return format!("{name}:{sid}");
        }
    }
    format!("sid:{sid}")
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests;
