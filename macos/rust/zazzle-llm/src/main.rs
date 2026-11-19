// zazzle-llm: `zazzle agent llm` lists, fetches and chooses open models; `zazzle agent prompt` asks one, downloading it first when needed.
mod catalog;
mod engine;
mod store;
mod template;
mod think;

use clap::{Parser, Subcommand};
use std::io::{IsTerminal, Read, Write};
use std::path::Path;
use std::process::ExitCode;

// The command line, named as zazzle users type it.
#[derive(Parser)]
#[command(
    name = "zazzle-llm",
    bin_name = "zazzle agent",
    version,
    about = "Run open models locally: choose one with llm, ask it with prompt"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

// The two commands.
#[derive(Subcommand)]
enum Cmd {
    /// List the models, or download one and make it (and an effort) the default.
    Llm {
        /// Download this model (unless present) and make it the default.
        #[arg(long)]
        model: Option<String>,
        /// Make this the default effort: low (no thinking), medium or high.
        #[arg(long)]
        effort: Option<String>,
        /// Delete this model's downloaded file.
        #[arg(long, conflicts_with_all = ["model", "effort"])]
        remove: Option<String>,
    },
    /// Ask a model; TEXT words are joined, `-` (or no TEXT) reads standard input.
    Prompt {
        /// The model to ask instead of the default; downloaded first when missing.
        #[arg(long)]
        model: Option<String>,
        /// How hard to think: low (no thinking), medium or high, instead of the default.
        #[arg(long)]
        effort: Option<String>,
        /// A system message to put before the prompt.
        #[arg(long)]
        system: Option<String>,
        /// The most tokens to generate, thinking included (default: 4096 low, 8192 medium, 32768 high).
        #[arg(long)]
        max_tokens: Option<usize>,
        /// Also print the model's thinking, to standard error.
        #[arg(long)]
        show_thinking: bool,
        /// Fail (exit 4) instead of downloading a model that is not here; zazzle uses it for agents.
        #[arg(long)]
        no_download: bool,
        /// A GBNF grammar (root rule `root`) the answer must follow; use with --effort low.
        #[arg(long)]
        grammar: Option<String>,
        /// Sampling temperature; 0 always takes the likeliest token (repeatable answers).
        #[arg(long, default_value_t = 0.7)]
        temp: f32,
        /// The prompt.
        text: Vec<String>,
    },
}

// Returns the default token limit for an effort.
fn max_tokens(effort: &str) -> usize {
    match effort {
        "low" => 4096,
        "high" => 32768,
        _ => 8192,
    }
}

// Builds the prompt from the words, putting standard input where a word is `-` or when there are no words.
fn prompt_text(
    words: &[String],
    stdin: &mut dyn FnMut() -> Result<String, String>,
) -> Result<String, String> {
    if words.is_empty() {
        return stdin();
    }
    let mut parts = Vec::new();
    for w in words {
        parts.push(if w == "-" { stdin()? } else { w.clone() });
    }
    Ok(parts.join(" "))
}

// Reads all of standard input.
fn read_stdin() -> Result<String, String> {
    let mut s = String::new();
    std::io::stdin().read_to_string(&mut s).map_err(|e| format!("reading standard input: {e}"))?;
    Ok(s)
}

// Downloads a model unless installed, showing progress on standard error.
fn ensure(root: &Path, m: &catalog::Model) -> Result<std::path::PathBuf, String> {
    let path = store::model_path(root, m.file);
    if store::installed(root, m.file, m.bytes) {
        return Ok(path);
    }
    let tty = std::io::stderr().is_terminal();
    eprintln!("fetching {} ({}) from huggingface.co/{}", m.name, catalog::gb(m.bytes), m.repo);
    let want = store::Want { url: catalog::url(m), file: m.file, sha256: m.sha256, bytes: m.bytes };
    let mut show = |done: u64, total: u64| {
        if tty {
            eprint!("\r{} / {} ({}%)   ", catalog::gb(done), catalog::gb(total), done * 100 / total.max(1));
        }
    };
    let path = store::fetch(root, &want, &mut show);
    if tty {
        eprintln!();
    }
    path
}

// Prints the catalogue with what is installed and the defaults.
fn list(root: &Path) -> String {
    let d = store::defaults(root);
    let mut out = String::from("  model              size     here  about\n");
    for m in catalog::MODELS {
        let mark = if m.name == d.model { '*' } else { ' ' };
        let here = if store::installed(root, m.file, m.bytes) { "yes" } else { "-" };
        out.push_str(&format!("{mark} {:<18} {:>8} {here:<5} {}\n", m.name, catalog::gb(m.bytes), m.about));
    }
    out.push_str(&format!(
        "default: {} at effort {}; models in {}\nchoose: zazzle agent llm --model NAME [--effort low|medium|high]\nask:    zazzle agent prompt TEXT\n",
        d.model,
        d.effort,
        root.join("models").display()
    ));
    out
}

// Runs `llm`: lists, removes, or fetches and saves the defaults.
fn llm(
    root: &Path,
    model: Option<String>,
    effort: Option<String>,
    remove: Option<String>,
) -> Result<String, String> {
    if let Some(name) = remove {
        let m = catalog::find(&name)?;
        let gone = store::remove(root, m.file)?;
        return Ok(if gone {
            format!("removed {}", m.name)
        } else {
            format!("{} was not downloaded", m.name)
        });
    }
    if model.is_none() && effort.is_none() {
        return Ok(list(root));
    }
    let mut d = store::defaults(root);
    if let Some(e) = effort {
        d.effort = store::effort(&e)?;
    }
    if let Some(name) = model {
        let m = catalog::find(&name)?;
        ensure(root, m)?;
        d.model = m.name.to_string();
    }
    store::save_defaults(root, &d)?;
    Ok(format!("default: {} at effort {}; ask with: zazzle agent prompt TEXT", d.model, d.effort))
}

// The options of `prompt`.
struct PromptArgs {
    model: Option<String>,
    effort: Option<String>,
    system: Option<String>,
    max_tokens: Option<usize>,
    show_thinking: bool,
    no_download: bool,
    grammar: Option<String>,
    temp: f32,
    text: Vec<String>,
}

// Runs `prompt`: picks the model and effort, fetches the model when missing, and streams the answer to standard output; returns 3 when the token limit cut it short, 4 when --no-download finds the model missing.
fn prompt(root: &Path, a: PromptArgs) -> Result<u8, String> {
    let d = store::defaults(root);
    let m = catalog::find(a.model.as_deref().unwrap_or(&d.model))?;
    let effort = store::effort(a.effort.as_deref().unwrap_or(&d.effort))?;
    let user = prompt_text(&a.text, &mut read_stdin)?;
    if user.trim().is_empty() {
        return Err("nothing to ask: give TEXT, or pipe it in".into());
    }
    if a.no_download && !store::installed(root, m.file, m.bytes) {
        eprintln!(
            "error: {} is not downloaded ({}): run `zazzle agent llm --model {}` first",
            m.name,
            catalog::gb(m.bytes),
            m.name
        );
        return Ok(4);
    }
    let path = ensure(root, m)?;
    let ask = engine::Ask {
        path: &path,
        system: a.system.as_deref(),
        user: &user,
        effort: &effort,
        max_tokens: a.max_tokens.unwrap_or_else(|| max_tokens(&effort)),
        grammar: a.grammar.as_deref(),
        temp: a.temp,
    };
    let (mut stdout, mut stderr) = (std::io::stdout().lock(), std::io::stderr());
    let (mut filter, mut ended_nl, mut answered) = (think::Filter::new(""), true, false);
    let mut show = |pieces: Vec<think::Piece>| {
        for p in pieces {
            match p {
                think::Piece::Answer(s) => {
                    let _ = stdout.write_all(s.as_bytes()).and_then(|()| stdout.flush());
                    (ended_nl, answered) = (s.ends_with('\n'), true);
                }
                think::Piece::Thought(s) if a.show_thinking => _ = stderr.write_all(s.as_bytes()),
                think::Piece::Thought(_) => {}
            }
        }
    };
    let done = engine::generate(&ask, &mut |ev| match ev {
        engine::Event::Prompt(p) => filter = think::Filter::new(p),
        engine::Event::Text(t) => show(filter.push(t)),
    })?;
    show(filter.finish());
    if !ended_nl {
        println!();
    }
    if !done.truncated {
        return Ok(0);
    }
    let what = if answered { "the answer is cut short" } else { "still thinking, no answer yet" };
    eprintln!("[stopped after {} tokens, {what}: raise --max-tokens, or lower --effort]", done.tokens);
    Ok(3)
}

// Parses the command line and runs it; errors go to standard error with status 2, an answer cut short by the token limit exits 3, a missing model under --no-download exits 4.
fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = store::root().and_then(|root| match cli.cmd {
        Cmd::Llm { model, effort, remove } => llm(&root, model, effort, remove).map(|t| {
            println!("{}", t.trim_end());
            0
        }),
        Cmd::Prompt {
            model,
            effort,
            system,
            max_tokens,
            show_thinking,
            no_download,
            grammar,
            temp,
            text,
        } => prompt(
            &root,
            PromptArgs { model, effort, system, max_tokens, show_thinking, no_download, grammar, temp, text },
        ),
    });
    match result {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Checks words are joined, `-` takes standard input in place, and no words means standard input alone.
    #[test]
    fn test_main_prompt_text() {
        let mut stdin = || Ok("PIPED".to_string());
        assert_eq!(prompt_text(&["a".into(), "b".into()], &mut stdin), Ok("a b".into()));
        assert_eq!(
            prompt_text(&["Summarize:".into(), "-".into()], &mut stdin),
            Ok("Summarize: PIPED".into())
        );
        assert_eq!(prompt_text(&[], &mut stdin), Ok("PIPED".into()));
        assert_eq!((max_tokens("low"), max_tokens("medium"), max_tokens("high")), (4096, 8192, 32768));
    }

    // Checks `llm` lists the catalogue with the default marked, saves an effort, rejects unknown names, and removes nothing missing.
    #[test]
    fn test_main_llm() {
        let root = std::env::temp_dir().join(format!("zazzle-llm-main-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let text = llm(&root, None, None, None).expect("list");
        assert!(text.contains(&format!("* {:<18}", catalog::DEFAULT)) && text.contains("gpt-oss-120b"));
        assert!(llm(&root, None, Some("high".into()), None).expect("effort").contains("at effort high"));
        assert_eq!(store::defaults(&root).effort, "high");
        assert!(llm(&root, Some("nope".into()), None, None).is_err());
        assert!(llm(&root, None, Some("max".into()), None).is_err());
        assert_eq!(
            llm(&root, None, None, Some("qwen3.5-2b".into())),
            Ok("qwen3.5-2b was not downloaded".into())
        );
        let r = prompt(
            &root,
            PromptArgs {
                model: None,
                effort: None,
                system: None,
                max_tokens: None,
                show_thinking: false,
                no_download: false,
                grammar: None,
                temp: 0.7,
                text: vec![" ".into()],
            },
        );
        assert!(r.expect_err("empty").contains("nothing to ask"));
        let absent = PromptArgs {
            model: Some("qwen3.5-2b".into()),
            effort: None,
            system: None,
            max_tokens: None,
            show_thinking: false,
            no_download: true,
            grammar: None,
            temp: 0.7,
            text: vec!["hi".into()],
        };
        assert_eq!(prompt(&root, absent), Ok(4));
        let _ = std::fs::remove_dir_all(&root);
    }
}
