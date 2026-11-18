// Runs the model through llama.cpp: feeds prompt text and samples a streamed reply.

use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::context::LlamaContext;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::LlamaModel;
use llama_cpp_2::sampling::LlamaSampler;
use llama_cpp_2::token::LlamaToken;
use std::num::NonZeroU32;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

// Sampling settings Qwen recommends for each mode.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sampling {
    pub temp: f32,
    pub top_p: f32,
    pub top_k: i32,
    pub presence: f32,
}

// Why a reply ended.
#[derive(Debug, PartialEq)]
pub enum Stop {
    Done,
    Cancelled,
    Limit,
    ContextFull,
}

// A loaded model and its single conversation context.
pub struct Engine<'m> {
    model: &'m LlamaModel,
    ctx: LlamaContext<'m>,
    sampler: LlamaSampler,
    n_past: i32,
    batch_size: usize,
}

// Qwen's recommended settings for thinking or instruct mode.
pub fn sampling(think: bool) -> Sampling {
    if think {
        Sampling { temp: 1.0, top_p: 0.95, top_k: 20, presence: 1.5 }
    } else {
        Sampling { temp: 0.7, top_p: 0.8, top_k: 20, presence: 1.5 }
    }
}

// Loads the GGUF file, offloading gpu_layers layers to the GPU where there is one.
pub fn load_model(backend: &LlamaBackend, path: &Path, gpu_layers: u32) -> Result<LlamaModel, String> {
    let params = LlamaModelParams::default().with_n_gpu_layers(gpu_layers);
    LlamaModel::load_from_file(backend, path, &params).map_err(|e| format!("{}: {e}", path.display()))
}

// Builds the sampler chain: presence penalty, top-k, top-p, temperature, then a seeded draw.
fn make_sampler(model: &LlamaModel, s: Sampling, seed: u32) -> LlamaSampler {
    LlamaSampler::chain_simple([
        LlamaSampler::penalties(model.n_vocab(), 64, 1.0, 0.0, s.presence),
        LlamaSampler::top_k(s.top_k),
        LlamaSampler::top_p(s.top_p, 1),
        LlamaSampler::temp(s.temp),
        LlamaSampler::dist(seed),
    ])
}

// Returns the valid UTF-8 start of bytes and keeps an incomplete character for the next piece.
pub fn take_utf8(bytes: &mut Vec<u8>) -> String {
    let valid = match std::str::from_utf8(bytes) {
        Ok(_) => bytes.len(),
        Err(e) if e.error_len().is_none() => e.valid_up_to(),
        Err(_) => return String::from_utf8_lossy(&std::mem::take(bytes)).into_owned(),
    };
    let rest = bytes.split_off(valid);
    String::from_utf8(std::mem::replace(bytes, rest)).expect("checked valid above")
}

impl<'m> Engine<'m> {
    // Creates a context of ctx tokens running on threads CPU threads.
    pub fn new(
        backend: &LlamaBackend,
        model: &'m LlamaModel,
        ctx: u32,
        threads: i32,
        s: Sampling,
        seed: u32,
    ) -> Result<Self, String> {
        let batch = 512;
        let params = LlamaContextParams::default()
            .with_n_ctx(NonZeroU32::new(ctx))
            .with_n_batch(batch)
            .with_n_threads(threads)
            .with_n_threads_batch(threads);
        let ctx = model.new_context(backend, params).map_err(|e| format!("context: {e}"))?;
        let sampler = make_sampler(model, s, seed);
        Ok(Engine { model, ctx, sampler, n_past: 0, batch_size: batch as usize })
    }

    // Switches to new sampling settings for the next reply.
    pub fn set_sampling(&mut self, s: Sampling, seed: u32) {
        self.sampler = make_sampler(self.model, s, seed);
    }

    // Context size in tokens.
    pub fn n_ctx(&self) -> i32 {
        self.ctx.n_ctx() as i32
    }

    // Tokens already in the context.
    pub fn used(&self) -> i32 {
        self.n_past
    }

    // Turns prompt text, special tags included, into tokens.
    pub fn tokenize(&self, text: &str) -> Vec<LlamaToken> {
        self.model.vocab().tokenize(text.as_bytes(), false, true)
    }

    // Empties the context for a new conversation.
    pub fn clear(&mut self) {
        self.ctx.clear_kv_cache();
        self.sampler.reset();
        self.n_past = 0;
    }

    // Decodes tokens after what is already in the context.
    pub fn feed(&mut self, tokens: &[LlamaToken]) -> Result<(), String> {
        let mut batch = LlamaBatch::new(self.batch_size, 1);
        for (ci, chunk) in tokens.chunks(self.batch_size).enumerate() {
            batch.clear();
            let last_chunk = (ci + 1) * self.batch_size >= tokens.len();
            for (i, t) in chunk.iter().enumerate() {
                let logits = last_chunk && i + 1 == chunk.len();
                batch.add(*t, self.n_past + i as i32, &[0], logits).map_err(|e| format!("batch: {e}"))?;
            }
            self.ctx.decode(&mut batch).map_err(|e| format!("decode: {e}"))?;
            self.n_past += chunk.len() as i32;
        }
        Ok(())
    }

    // Samples up to max tokens, calling on_text with each piece, until the model ends its turn.
    pub fn generate(
        &mut self,
        max: usize,
        cancel: &AtomicBool,
        mut on_text: impl FnMut(&str),
    ) -> Result<Stop, String> {
        let vocab = self.model.vocab();
        let mut pending = Vec::new();
        let mut batch = LlamaBatch::new(1, 1);
        for _ in 0..max {
            if cancel.swap(false, Ordering::SeqCst) {
                return Ok(Stop::Cancelled);
            }
            if self.n_past >= self.n_ctx() {
                return Ok(Stop::ContextFull);
            }
            let token = self.sampler.sample(&self.ctx, -1);
            if vocab.is_eog(token) {
                return Ok(Stop::Done);
            }
            pending.extend(vocab.token_to_piece(token, true, None));
            on_text(&take_utf8(&mut pending));
            batch.clear();
            batch.add(token, self.n_past, &[0], true).map_err(|e| format!("batch: {e}"))?;
            self.ctx.decode(&mut batch).map_err(|e| format!("decode: {e}"))?;
            self.n_past += 1;
        }
        Ok(Stop::Limit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Complete text passes through and a split character waits for its other bytes.
    #[test]
    fn test_take_utf8_split_char() {
        let mut b = "hé".as_bytes().to_vec();
        b.pop();
        assert_eq!(take_utf8(&mut b), "h");
        assert_eq!(b, vec![0xc3]);
        b.push(0xa9);
        assert_eq!(take_utf8(&mut b), "é");
        assert!(b.is_empty());
    }

    // Bytes that can never be UTF-8 are replaced instead of held forever.
    #[test]
    fn test_take_utf8_invalid() {
        let mut b = vec![b'a', 0xff, b'b'];
        assert_eq!(take_utf8(&mut b), "a\u{fffd}b");
        assert!(b.is_empty());
    }

    // Thinking mode samples hotter and wider than instruct mode.
    #[test]
    fn test_sampling_modes() {
        assert_eq!(sampling(true).temp, 1.0);
        assert_eq!(sampling(false).top_p, 0.8);
    }
}
