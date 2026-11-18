// Runs one prompt through a GGUF model with llama.cpp: renders the model's chat template, decodes the prompt and streams sampled text.
use crate::template::{self, Turn};
use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::LlamaModel;
use llama_cpp_2::sampling::LlamaSampler;
use std::num::NonZeroU32;
use std::path::Path;

const BATCH: usize = 512;

// One prompt: the model file, the messages, the effort and the most tokens to generate.
pub struct Ask<'a> {
    pub path: &'a Path,
    pub system: Option<&'a str>,
    pub user: &'a str,
    pub effort: &'a str,
    pub max_tokens: usize,
}

// What generation reports as it goes: the rendered prompt once, then each piece of text.
pub enum Event<'a> {
    Prompt(&'a str),
    Text(&'a str),
}

// How a generation ended: tokens written, and whether it stopped at the token limit.
pub struct Done {
    pub tokens: usize,
    pub truncated: bool,
}

// Returns the text of a special token such as BOS, or an empty string.
fn token_text(model: &LlamaModel, t: llama_cpp_2::token::LlamaToken) -> String {
    String::from_utf8_lossy(&model.vocab().token_to_piece(t, true, None)).into_owned()
}

// Moves the longest complete UTF-8 prefix out of the byte buffer, keeping a character split across tokens for later.
fn take_utf8(bytes: &mut Vec<u8>) -> String {
    let n = match std::str::from_utf8(bytes) {
        Ok(_) => bytes.len(),
        Err(e) if e.error_len().is_none() => e.valid_up_to(),
        Err(_) => bytes.len(),
    };
    let text = String::from_utf8_lossy(&bytes[..n]).into_owned();
    bytes.drain(..n);
    text
}

// Loads the model, renders and decodes the prompt, and reports the prompt and each piece of generated text to `out` until an end token or the limit.
pub fn generate(ask: &Ask, out: &mut dyn FnMut(Event)) -> Result<Done, String> {
    let mut backend = LlamaBackend::init().map_err(|e| format!("starting llama.cpp: {e}"))?;
    if std::env::var_os("ZAZZLE_LLM_LOG").is_none() {
        backend.void_logs();
    }
    let params = LlamaModelParams::default().with_n_gpu_layers(u32::MAX);
    let model = LlamaModel::load_from_file(&backend, ask.path, &params)
        .map_err(|e| format!("loading {}: {e}", ask.path.display()))?;
    let tmpl =
        model.chat_template(None).map_err(|e| format!("{}: no chat template: {e}", ask.path.display()))?;
    let vocab = model.vocab();
    let (bos, eos) = (token_text(&model, vocab.bos()), token_text(&model, vocab.eos()));
    let turn = Turn { system: ask.system, user: ask.user, effort: ask.effort, bos: &bos, eos: &eos };
    let prompt = template::render(tmpl.to_str().map_err(|e| e.to_string())?, &turn)?;
    let body = prompt.strip_prefix(bos.as_str()).filter(|_| !bos.is_empty() && vocab.should_add_bos());
    let tokens = vocab.tokenize(body.unwrap_or(&prompt).as_bytes(), true, true);
    let limit = model.n_ctx_train() as usize;
    if tokens.len() >= limit {
        return Err(format!("the prompt is {} tokens; this model reads at most {limit}", tokens.len()));
    }
    let n_ctx = (tokens.len() + ask.max_tokens).min(limit);
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()) as i32;
    let cparams = LlamaContextParams::default()
        .with_n_ctx(NonZeroU32::new(n_ctx as u32))
        .with_n_batch(BATCH as u32)
        .with_n_threads(threads)
        .with_n_threads_batch(threads);
    let mut ctx = model.new_context(&backend, cparams).map_err(|e| format!("creating a context: {e}"))?;
    let mut batch = LlamaBatch::new(BATCH, 1);
    for (i, chunk) in tokens.chunks(BATCH).enumerate() {
        batch.clear();
        for (j, t) in chunk.iter().enumerate() {
            let pos = i * BATCH + j;
            batch.add(*t, pos as i32, &[0], pos + 1 == tokens.len()).map_err(|e| e.to_string())?;
        }
        ctx.decode(&mut batch).map_err(|e| format!("reading the prompt: {e}"))?;
    }
    let seed =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
    let mut sampler = LlamaSampler::chain_simple([
        LlamaSampler::top_k(40),
        LlamaSampler::top_p(0.95, 1),
        LlamaSampler::min_p(0.05, 1),
        LlamaSampler::temp(0.7),
        LlamaSampler::dist(seed),
    ]);
    let (mut pos, mut made, mut pending) = (tokens.len(), 0, Vec::new());
    out(Event::Prompt(&prompt));
    while pos < n_ctx && made < ask.max_tokens {
        let t = sampler.sample(&ctx, batch.n_tokens() - 1);
        sampler.accept(t);
        if vocab.is_eog(t) {
            break;
        }
        pending.extend(vocab.token_to_piece(t, true, None));
        let text = take_utf8(&mut pending);
        if !text.is_empty() {
            out(Event::Text(&text));
        }
        batch.clear();
        batch.add(t, pos as i32, &[0], true).map_err(|e| e.to_string())?;
        ctx.decode(&mut batch).map_err(|e| format!("generating: {e}"))?;
        (pos, made) = (pos + 1, made + 1);
    }
    let truncated = made >= ask.max_tokens || pos >= n_ctx;
    Ok(Done { tokens: made, truncated })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Checks a character split across tokens waits for its last byte, and invalid bytes do not stall the stream.
    #[test]
    fn test_engine_take_utf8() {
        let mut b = "aé".as_bytes()[..2].to_vec();
        assert_eq!(take_utf8(&mut b), "a");
        b.push("é".as_bytes()[1]);
        assert_eq!(take_utf8(&mut b), "é");
        let mut bad = vec![b'x', 0xff, b'y'];
        assert_eq!(take_utf8(&mut bad), "x\u{fffd}y");
        assert!(bad.is_empty());
    }
}
