// qwen-chat: talk to Qwen3.5-9B in the terminal, run locally through llama.cpp built into the binary.

mod download;
mod engine;
mod prompt;

use clap::Parser;
use engine::{Engine, Stop};
use llama_cpp_2::llama_backend::LlamaBackend;
use prompt::{Segment, Splitter, Turn};
use std::io::{self, BufRead, IsTerminal, Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};

const DEFAULT_SYSTEM: &str = "You are a helpful assistant.";
// Most tokens kept free for the reply when deciding whether a message fits.
const REPLY_RESERVE: i32 = 1024;
const HELP: &str = "/reset new conversation · /think toggle reasoning · /exit quit · Ctrl-C stops a reply";
const DIM: &str = "\x1b[2m";
const RESET: &str = "\x1b[0m";

static GENERATING: AtomicBool = AtomicBool::new(false);
static CANCEL: AtomicBool = AtomicBool::new(false);

// Command-line options.
#[derive(Parser)]
#[command(version, about = "Chat with Qwen3.5-9B locally. Downloads the 5.7 GB weights on first run.")]
struct Args {
    /// Ask one question, print the answer and exit (stdin is used when it is not a terminal)
    #[arg(short, long)]
    prompt: Option<String>,
    /// Let the model reason before answering (shown dimmed)
    #[arg(long)]
    think: bool,
    /// System message
    #[arg(short, long, default_value = DEFAULT_SYSTEM)]
    system: String,
    /// GGUF weights to use instead of the downloaded Qwen3.5-9B Q4_K_M
    #[arg(short, long)]
    model: Option<PathBuf>,
    /// Context size in tokens
    #[arg(long, default_value_t = 8192)]
    ctx: u32,
    /// Most tokens in one reply
    #[arg(long, default_value_t = 4096)]
    max_tokens: usize,
    /// CPU threads (default: all cores)
    #[arg(long)]
    threads: Option<i32>,
    /// Layers to run on the GPU (Metal on macOS; 0 = CPU only)
    #[arg(long, default_value_t = 999)]
    gpu_layers: u32,
    /// Sampling seed
    #[arg(long, default_value_t = 0xC0FFEE)]
    seed: u32,
    /// Only download the weights, then exit
    #[arg(long)]
    download: bool,
    /// Show llama.cpp's log
    #[arg(short, long)]
    verbose: bool,
}

// A conversation: the engine, what was said, and the current mode.
struct Chat<'m> {
    engine: Engine<'m>,
    history: Vec<Turn>,
    system: String,
    think: bool,
    seed: u32,
    max_tokens: usize,
}

// Where reply text goes and whether it is styled.
struct Out {
    color: bool,
    reasoning_to_stderr: bool,
}

// Prints one reply segment, dimming reasoning on a terminal.
fn show(out: &Out, seg: &Segment) {
    match seg {
        Segment::Answer(t) => {
            print!("{t}");
            let _ = io::stdout().flush();
        }
        Segment::Reasoning(t) if out.reasoning_to_stderr => eprint!("{t}"),
        Segment::Reasoning(t) if out.color => {
            print!("{DIM}{t}{RESET}");
            let _ = io::stdout().flush();
        }
        Segment::Reasoning(t) => print!("{t}"),
    }
}

// Room kept for the reply: a quarter of the context, at most REPLY_RESERVE.
fn reserve(n_ctx: i32) -> i32 {
    (n_ctx / 4).min(REPLY_RESERVE)
}

// Message for a reply that did not end by itself.
fn stop_note(stop: &Stop) -> Option<&'static str> {
    match stop {
        Stop::Done => None,
        Stop::Cancelled => Some("[stopped]"),
        Stop::Limit => Some("[reply limit reached]"),
        Stop::ContextFull => Some("[context full: /reset or use --ctx]"),
    }
}

impl<'m> Chat<'m> {
    // Puts the new message into the context, rebuilding it from recent turns when it is full.
    fn load_message(&mut self, user: &str) -> Result<(), String> {
        if self.engine.used() > 0 {
            let tokens = self.engine.tokenize(&prompt::user_turn(user, true, self.think));
            if self.engine.used() + tokens.len() as i32 + reserve(self.engine.n_ctx()) <= self.engine.n_ctx()
            {
                return self.engine.feed(&tokens);
            }
            self.engine.clear();
        }
        let mut skip = 0;
        loop {
            let text = prompt::full(&self.system, &self.history[skip..], user, self.think);
            let tokens = self.engine.tokenize(&text);
            if tokens.len() as i32 + reserve(self.engine.n_ctx()) <= self.engine.n_ctx() {
                if skip > 0 {
                    eprintln!("[context full: dropped the {skip} oldest exchanges]");
                }
                self.history.drain(..skip);
                return self.engine.feed(&tokens);
            }
            if skip == self.history.len() {
                return Err("message too long for the context; raise --ctx".into());
            }
            skip += 1;
        }
    }

    // Sends a message and streams the reply, returning why it stopped.
    fn send(&mut self, user: &str, out: &Out) -> Result<Stop, String> {
        self.load_message(user)?;
        let mut split = Splitter::new(self.think);
        CANCEL.store(false, Ordering::SeqCst);
        GENERATING.store(true, Ordering::SeqCst);
        let stop = self.engine.generate(self.max_tokens, &CANCEL, |piece| {
            split.push(piece).iter().for_each(|s| show(out, s));
        });
        GENERATING.store(false, Ordering::SeqCst);
        let stop = stop?;
        self.history.push(Turn { user: user.to_string(), answer: split.answer() });
        Ok(stop)
    }

    // Starts over with an empty context.
    fn reset(&mut self) {
        self.engine.clear();
        self.history.clear();
    }

    // Turns reasoning on or off for the following replies.
    fn toggle_think(&mut self) {
        self.think = !self.think;
        self.engine.set_sampling(engine::sampling(self.think), self.seed);
    }
}

// Reads one line from the terminal; None at end of input.
fn read_line(stdin: &mut impl BufRead) -> Option<String> {
    print!("\n> ");
    let _ = io::stdout().flush();
    let mut line = String::new();
    match stdin.read_line(&mut line) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(line.trim().to_string()),
    }
}

// The interactive loop: read a message, stream the answer, repeat.
fn repl(chat: &mut Chat, out: &Out) -> Result<(), String> {
    eprintln!("qwen-chat · Qwen3.5-9B · thinking {} · {HELP}", if chat.think { "on" } else { "off" });
    let mut stdin = io::stdin().lock();
    while let Some(line) = read_line(&mut stdin) {
        match line.as_str() {
            "" => continue,
            "/exit" | "/quit" => break,
            "/help" => eprintln!("{HELP}"),
            "/reset" => {
                chat.reset();
                eprintln!("[new conversation]");
            }
            "/think" => {
                chat.toggle_think();
                eprintln!("[thinking {}]", if chat.think { "on" } else { "off" });
            }
            msg => {
                let stop = chat.send(msg, out)?;
                println!();
                if let Some(note) = stop_note(&stop) {
                    eprintln!("{note}");
                }
            }
        }
    }
    Ok(())
}

// Locates or downloads the weights.
fn model_path(args: &Args) -> Result<PathBuf, String> {
    if let Some(p) = &args.model {
        return Ok(p.clone());
    }
    let env = |k: &str| std::env::var(k).ok();
    let dir = download::cache_dir(env("QWEN_CHAT_HOME"), env("XDG_CACHE_HOME"), env("HOME"))
        .ok_or("set HOME or QWEN_CHAT_HOME")?;
    download::ensure(&dir)
}

// The one-shot question from --prompt or piped stdin, if any.
fn one_shot(args: &Args) -> Result<Option<String>, String> {
    if let Some(p) = &args.prompt {
        return Ok(Some(p.clone()));
    }
    if io::stdin().is_terminal() {
        return Ok(None);
    }
    let mut text = String::new();
    io::stdin().read_to_string(&mut text).map_err(|e| format!("stdin: {e}"))?;
    Ok(Some(text.trim().to_string()))
}

// Ctrl-C stops a reply in progress, and quits otherwise.
fn install_ctrlc() -> Result<(), String> {
    ctrlc::set_handler(|| {
        if GENERATING.load(Ordering::SeqCst) {
            CANCEL.store(true, Ordering::SeqCst);
            return;
        }
        eprintln!();
        std::process::exit(130);
    })
    .map_err(|e| format!("Ctrl-C handler: {e}"))
}

// Loads everything and runs a one-shot question or the chat loop.
fn run(args: Args) -> Result<(), String> {
    let path = model_path(&args)?;
    if args.download {
        println!("{}", path.display());
        return Ok(());
    }
    let question = one_shot(&args)?;
    let mut backend = LlamaBackend::init().map_err(|e| format!("llama.cpp: {e}"))?;
    if !args.verbose {
        backend.void_logs();
    }
    eprintln!("loading {}", path.display());
    let model = engine::load_model(&backend, &path, args.gpu_layers)?;
    let threads =
        args.threads.unwrap_or_else(|| std::thread::available_parallelism().map_or(4, |n| n.get() as i32));
    let engine = Engine::new(&backend, &model, args.ctx, threads, engine::sampling(args.think), args.seed)?;
    let mut chat = Chat {
        engine,
        history: Vec::new(),
        system: args.system,
        think: args.think,
        seed: args.seed,
        max_tokens: args.max_tokens,
    };
    install_ctrlc()?;
    let Some(q) = question else {
        let out = Out { color: io::stdout().is_terminal(), reasoning_to_stderr: false };
        return repl(&mut chat, &out);
    };
    let out = Out { color: false, reasoning_to_stderr: true };
    let stop = chat.send(&q, &out)?;
    println!();
    match stop_note(&stop) {
        Some(note) => Err(note.to_string()),
        None => Ok(()),
    }
}

// Entry point: prints any error and exits with status 1.
fn main() -> ExitCode {
    match run(Args::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("qwen-chat: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Small contexts keep a quarter free, large ones at most REPLY_RESERVE.
    #[test]
    fn test_reserve() {
        assert_eq!(reserve(1024), 256);
        assert_eq!(reserve(32768), REPLY_RESERVE);
    }

    // Only replies that were cut short get a note.
    #[test]
    fn test_stop_note() {
        assert_eq!(stop_note(&Stop::Done), None);
        assert_eq!(stop_note(&Stop::Cancelled), Some("[stopped]"));
        assert!(stop_note(&Stop::Limit).is_some());
        assert!(stop_note(&Stop::ContextFull).is_some());
    }

    // Lines are trimmed and end of input ends the loop.
    #[test]
    fn test_read_line() {
        let mut input = io::Cursor::new(b"  hello \n".to_vec());
        assert_eq!(read_line(&mut input), Some("hello".into()));
        assert_eq!(read_line(&mut input), None);
    }

    // The command line parses with defaults.
    #[test]
    fn test_args_defaults() {
        let a = Args::parse_from(["qwen-chat", "-p", "hi", "--think"]);
        assert_eq!(a.prompt.as_deref(), Some("hi"));
        assert!(a.think);
        assert_eq!(a.ctx, 8192);
        assert_eq!(a.system, DEFAULT_SYSTEM);
    }
}
