// afs: a JSON-in, JSON-out file read and search tool for agents, with session caching and an MCP server.
#[cfg(target_os = "macos")]
mod aio;
mod diff;
mod engine;
mod fsio;
mod index;
mod list;
mod mcp;
mod outline;
mod par;
mod read;
mod req;
mod search;
mod state;
#[cfg(target_os = "linux")]
mod uring;
mod walk;

use clap::{Parser, Subcommand};
use engine::{all_ok, Engine};
use serde_json::{json, Value};
use std::io::{BufRead, Read};
use std::process::ExitCode;

const EXAMPLES: &str = "examples:
  afs call '{\"op\":\"read\",\"path\":\"src/main.rs\",\"ranges\":[[1,40]]}'
  afs call '{\"op\":\"search\",\"pattern\":\"TODO\",\"glob\":[\"*.rs\"],\"context\":1}'
  afs call '[{\"op\":\"list\",\"path\":\"src\"},{\"op\":\"outline\",\"paths\":[\"src\"]}]'
  echo '{\"op\":\"stat\",\"path\":\"Cargo.toml\",\"hash\":true}' | afs call
  afs serve     # MCP server over stdio: one session, unchanged/diff replies, result cache
  afs jsonl     # one JSON request per stdin line, one reply per stdout line, one session

ops: read search list stat outline diff_since (see the README for every field)";

// Command-line interface.
#[derive(Parser)]
#[command(version, about = "JSON file read/search tool for agents with session caching.", after_help = EXAMPLES)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

// The ways to run afs.
#[derive(Subcommand)]
enum Command {
    /// Run one JSON request, or an array of them, from the argument or stdin and print the reply.
    Call {
        /// The request JSON; read from stdin when omitted or "-".
        json: Option<String>,
    },
    /// Serve one JSON request per stdin line, keeping one session so repeats return unchanged.
    Jsonl,
    /// Serve the Model Context Protocol over stdio.
    Serve,
}

// Read the request text from the argument or stdin.
fn request_text(arg: Option<String>) -> std::io::Result<String> {
    match arg.as_deref() {
        Some(text) if text != "-" => Ok(text.to_string()),
        _ => {
            let mut text = String::new();
            std::io::stdin().read_to_string(&mut text)?;
            Ok(text)
        }
    }
}

// Parse request text and run it in a fresh session, returning the reply.
fn call_once(text: &str) -> Value {
    match serde_json::from_str::<Value>(text) {
        Ok(input) => Engine::new().call(input),
        Err(e) => json!({"ok": false, "error": format!("invalid JSON: {e}")}),
    }
}

// Run the line-oriented session loop.
fn jsonl() -> std::io::Result<()> {
    let mut engine = Engine::new();
    for line in std::io::stdin().lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(input) => engine.call(input),
            Err(e) => json!({"ok": false, "error": format!("invalid JSON: {e}")}),
        };
        println!("{reply}");
    }
    Ok(())
}

// Run the chosen command and return its exit code.
fn main() -> ExitCode {
    let result = match Cli::parse().command {
        Command::Call { json } => request_text(json).map(|text| {
            let reply = call_once(&text);
            println!("{reply}");
            all_ok(&reply)
        }),
        Command::Jsonl => jsonl().map(|_| true),
        Command::Serve => mcp::serve().map(|_| true),
    };
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("afs: {e}");
            ExitCode::FAILURE
        }
    }
}
