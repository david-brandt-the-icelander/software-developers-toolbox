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
// Room a kept context leaves past a prompt's own need, so the next, longer turn of the same conversation still fits and reuses it.
const ROOM: usize = 8192;

// One prompt: the model file, the messages (system, earlier turns, user), the effort, the most tokens to generate, an optional GBNF grammar the answer must follow and the sampling temperature.
pub struct Ask<'a> {
    pub path: &'a Path,
    pub system: Option<&'a str>,
    pub history: &'a [(String, String)],
    pub user: &'a str,
    pub effort: &'a str,
    pub max_tokens: usize,
    pub grammar: Option<&'a str>,
    pub temp: f32,
}

// What generation reports as it goes: the rendered prompt once, then each piece of text.
pub enum Event<'a> {
    Prompt(&'a str),
    Text(&'a str),
}

// How a generation ended: tokens written, whether it stopped at the token limit, the prompt's tokens and how many of them the kept context already held.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct Done {
    pub tokens: usize,
    pub truncated: bool,
    pub prompt: usize,
    pub reused: usize,
}

// How many leading prompt tokens a kept context can serve again: the prefix it shares with the new prompt, leaving at least the last token to decode for its logits.
pub fn reusable<T: PartialEq>(kept: &[T], prompt: &[T]) -> usize {
    kept.iter().zip(prompt).take_while(|(a, b)| a == b).count().min(prompt.len().saturating_sub(1))
}

// The context sizes to try for a prompt that needs `need` tokens, largest first: room for the conversation to grow, then exactly the need, so a machine short of memory still answers.
fn sizes(need: usize, limit: usize) -> Vec<usize> {
    let roomy = (need + ROOM).min(limit);
    if roomy > need {
        vec![roomy, need]
    } else {
        vec![need]
    }
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

// A model loaded once and asked many times, as the worker does for daemon's chats and agents' calls; the model is leaked (a process loads one for its life) so the context it keeps between prompts can borrow it.
pub struct Loaded {
    // Declared first so the kept context is freed before the backend.
    kept: std::sync::Mutex<Option<Kept>>,
    backend: LlamaBackend,
    model: &'static LlamaModel,
    path: std::path::PathBuf,
}

// The context kept from the last prompt and the tokens its cache holds, so a prompt sharing their prefix decodes only the rest.
struct Kept {
    ctx: llama_cpp_2::context::LlamaContext<'static>,
    tokens: Vec<llama_cpp_2::token::LlamaToken>,
}

// Starts llama.cpp and loads the model onto the GPU when there is one, with its routine startup logs held back.
pub fn load(path: &Path) -> Result<Loaded, String> {
    crate::quiet::run(|| {
        let mut backend = LlamaBackend::init().map_err(|e| format!("starting llama.cpp: {e}"))?;
        if std::env::var_os("ZAZZLE_LLM_LOG").is_none() {
            backend.void_logs();
        }
        let params = LlamaModelParams::default().with_n_gpu_layers(u32::MAX);
        let model = LlamaModel::load_from_file(&backend, path, &params)
            .map_err(|e| format!("loading {}: {e}", path.display()))?;
        let model: &'static LlamaModel = Box::leak(Box::new(model));
        Ok(Loaded { backend, model, path: path.to_path_buf(), kept: Default::default() })
    })
}

// Loads the model, then answers one prompt with it.
pub fn generate(ask: &Ask, out: &mut dyn FnMut(Event)) -> Result<Done, String> {
    load(ask.path)?.generate(ask, out)
}

impl Loaded {
    // Renders and decodes the prompt, and reports the prompt and each piece of generated text to `out` until an end token or the limit; `ask.path` is not read again. The context is kept for the next prompt, which decodes only what follows the prefix they share; a failure drops it.
    pub fn generate(&self, ask: &Ask, out: &mut dyn FnMut(Event)) -> Result<Done, String> {
        let mut kept = self.kept.lock().unwrap_or_else(|p| p.into_inner());
        let done = self.run(ask, &mut kept, out);
        if done.is_err() {
            *kept = None;
        }
        done
    }

    // One generation with the kept context: reuses or replaces it, decodes the new part of the prompt, then samples.
    fn run(&self, ask: &Ask, kept: &mut Option<Kept>, out: &mut dyn FnMut(Event)) -> Result<Done, String> {
        let (backend, model, path) = (&self.backend, self.model, &self.path);
        let tmpl =
            model.chat_template(None).map_err(|e| format!("{}: no chat template: {e}", path.display()))?;
        let vocab = model.vocab();
        let (bos, eos) = (token_text(model, vocab.bos()), token_text(model, vocab.eos()));
        let turn = Turn {
            system: ask.system,
            history: ask.history,
            user: ask.user,
            effort: ask.effort,
            bos: &bos,
            eos: &eos,
        };
        let prompt = template::render(tmpl.to_str().map_err(|e| e.to_string())?, &turn)?;
        let body = prompt.strip_prefix(bos.as_str()).filter(|_| !bos.is_empty() && vocab.should_add_bos());
        let tokens = vocab.tokenize(body.unwrap_or(&prompt).as_bytes(), true, true);
        let limit = model.n_ctx_train() as usize;
        if tokens.len() >= limit {
            return Err(format!("the prompt is {} tokens; this model reads at most {limit}", tokens.len()));
        }
        let need = (tokens.len() + ask.max_tokens).min(limit);
        let fits = kept.as_ref().is_some_and(|k| k.ctx.n_ctx() as usize >= need);
        if !fits {
            let threads = std::thread::available_parallelism().map_or(4, |n| n.get()) as i32;
            *kept = None;
            let mut failed = String::new();
            for n_ctx in sizes(need, limit) {
                let cparams = LlamaContextParams::default()
                    .with_n_ctx(NonZeroU32::new(n_ctx as u32))
                    .with_n_batch(BATCH as u32)
                    .with_n_threads(threads)
                    .with_n_threads_batch(threads);
                match crate::quiet::run(|| model.new_context(backend, cparams).map_err(|e| e.to_string())) {
                    Ok(ctx) => {
                        *kept = Some(Kept { ctx, tokens: Vec::new() });
                        break;
                    }
                    Err(e) => failed = format!("creating a context: {e}"),
                }
            }
            if kept.is_none() {
                return Err(failed);
            }
        }
        let k = kept.as_mut().ok_or("no context")?;
        let mut from = reusable(&k.tokens, &tokens);
        // A cache that cannot drop just its tail (a recurrent model's) is cleared and the prompt read whole.
        if !k.ctx.clear_kv_cache_seq(Some(0), Some(from as u32), None).unwrap_or(false) {
            k.ctx.clear_kv_cache();
            from = 0;
        }
        k.tokens.truncate(from);
        let ctx = &mut k.ctx;
        let mut batch = LlamaBatch::new(BATCH, 1);
        for (i, chunk) in tokens[from..].chunks(BATCH).enumerate() {
            batch.clear();
            for (j, t) in chunk.iter().enumerate() {
                let pos = from + i * BATCH + j;
                batch.add(*t, pos as i32, &[0], pos + 1 == tokens.len()).map_err(|e| e.to_string())?;
            }
            ctx.decode(&mut batch).map_err(|e| format!("reading the prompt: {e}"))?;
        }
        k.tokens.extend_from_slice(&tokens[from..]);
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        let mut chain = Vec::new();
        if let Some(g) = ask.grammar {
            chain.push(LlamaSampler::grammar(model, g, "root").map_err(|e| format!("--grammar: {e}"))?);
        }
        if ask.temp <= 0.0 {
            chain.push(LlamaSampler::greedy());
        } else {
            chain.extend([
                LlamaSampler::top_k(40),
                LlamaSampler::top_p(0.95, 1),
                LlamaSampler::min_p(0.05, 1),
                LlamaSampler::temp(ask.temp),
                LlamaSampler::dist(seed),
            ]);
        }
        let mut sampler = LlamaSampler::chain_simple(chain);
        let (mut pos, mut made, mut pending) = (tokens.len(), 0, Vec::new());
        out(Event::Prompt(&prompt));
        while pos < need && made < ask.max_tokens {
            let t = sampler.sample(ctx, batch.n_tokens() - 1);
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
            k.tokens.push(t);
            (pos, made) = (pos + 1, made + 1);
        }
        let truncated = made >= ask.max_tokens || pos >= need;
        Ok(Done { tokens: made, truncated, prompt: tokens.len(), reused: from })
    }
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

    // Checks a context is first tried with room to grow, then at exactly the prompt's need, and never past the model's limit.
    #[test]
    fn test_engine_sizes() {
        assert_eq!(sizes(1000, 32768), [1000 + ROOM, 1000]);
        assert_eq!(sizes(30000, 32768), [32768, 30000]);
        assert_eq!(sizes(4096, 4096), [4096], "no room left: one size");
    }

    // Checks the reusable prefix is the shared leading tokens, never the whole prompt.
    #[test]
    fn test_engine_reusable() {
        assert_eq!(reusable(&[1, 2, 3, 9], &[1, 2, 3, 4, 5]), 3);
        assert_eq!(reusable(&[1, 2, 3], &[1, 2, 3]), 2, "the last token is decoded again for its logits");
        assert_eq!(reusable::<u8>(&[], &[1]), 0);
        assert_eq!(reusable(&[7], &[1, 2]), 0);
    }
}
