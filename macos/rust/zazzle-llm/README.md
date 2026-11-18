# zazzle agent llm / prompt

Runs open-weight models on your own machine, so an agent (or you) can ask a local model from the shell: a second opinion, a summary of a long log, a classification, without an API key or sending anything off the machine.

```bash
zazzle agent llm                                        # the catalogue: size, downloaded or not, the default
zazzle agent llm --model qwen3.5-9b --effort high       # download it (once) and make it and the effort the default
zazzle agent prompt "Why is the sky blue?"             # ask the default model
git diff | zazzle agent prompt "Review this diff:" -    # `-` is standard input; no TEXT at all reads only standard input
zazzle agent prompt --model gpt-oss-20b --effort low --system "Answer in one line." "What is RAII?"
zazzle agent llm --remove qwen3.5-9b                    # delete a downloaded model
```

## Two downloads on first use
- **The program.** `zazzle-llm` (this crate) is not in the release tarball or the Homebrew bottle: it carries llama.cpp and is only wanted by those who use it. The `zazzle` invoker hands `agent llm` and `agent prompt` to `zazzled llm`, which finds `zazzle-llm` beside itself (a development build) or in `~/.zazzle/v1/tools`, downloading it first from `https://icelander.net/downloads/zazzle/vX.Y.Z/tools/PLATFORM/` (`src/tools.rs` in zazzled). Like the language modules, it is accepted only when its SHA-256 matches the manifest compiled into that zazzled (`ZAZZLED_TOOLS_MANIFEST`, written by `macos/release/tools.sh`), then exec'd with the caller's terminal, environment and exit status. `ZAZZLE_TOOLS_DIR` and `ZAZZLE_TOOLS_URL` move the directory and the download URL.
- **The model.** `zazzle-llm` downloads a model the first time it is chosen or asked, from Hugging Face at the commit the catalogue pins (`src/catalog.rs`), into `~/.zazzle/v1/llm/models` (`ZAZZLE_LLM_DIR` moves `~/.zazzle/v1/llm`). Progress goes to standard error. An interrupted download resumes where it stopped; the file is installed only when its SHA-256 matches the catalogue's, so a changed or corrupted file is never loaded. Two prompts never download the same model at once (a lock file).

## Models
4-bit GGUF files (MXFP4 for gpt-oss), smallest first; the memory figure is the Mac each one fits comfortably.

| model | size | for |
|---|---|---|
| `qwen3.5-2b` | 1.3 GB | quick summaries and classification, any Mac |
| `qwen3.5-4b` | 2.7 GB | the default: good reasoning for its size, 8 GB |
| `qwen3.5-9b` | 5.7 GB | general use and code, 16 GB |
| `gemma-4-12b` | 7.1 GB | writing and many languages, 16 GB |
| `gpt-oss-20b` | 12.1 GB | reasoning with native low/medium/high effort, 24 GB |
| `qwen3.8-27b` | 16.6 GB | the strongest dense model here, code and agents, 32 GB |
| `gemma-4-26b-a4b` | 16.9 GB | mixture of experts (4B active): fast and broad, 32 GB |
| `qwen3.6-35b-a3b` | 22.1 GB | mixture of experts (3B active): fast with large-model knowledge, 32 GB |
| `qwen3-coder-next` | 49.3 GB | code, no thinking, 64 GB |
| `gpt-oss-120b` | 63.4 GB | the strongest reasoning here, 96 GB |

A name may be shortened to a unique prefix (`--model gemma-4-1`). To add a model: its architecture must be one the bundled llama.cpp knows (`llama-cpp-2`), then add an entry with the repository, the commit (`https://huggingface.co/api/models/REPO`, `sha`), the file and its LFS SHA-256 and size (`/api/models/REPO/tree/COMMIT`).

## Effort
`--effort low|medium|high` (default `medium`, or what `llm --effort` saved) is passed to the model's own chat template as `enable_thinking` (off for `low`) and `reasoning_effort` (gpt-oss and Qwen 3.8 read it). It also sets the default token limit, thinking included: 4096, 8192 and 32768 (`--max-tokens` overrides). Models without a thinking mode (`qwen3-coder-next`) answer the same at every effort.

## Output
- The answer streams to standard output. Thinking is dropped, or printed to standard error with `--show-thinking`.
- Exit 0 on a full answer; 3 when the token limit cut it short (a note on standard error says whether any answer came at all); 2 for errors (unknown model, a failed download, a prompt longer than the model reads).
- `ZAZZLE_LLM_LOG=1` shows llama.cpp's own log on standard error.

## How it runs
- `src/engine.rs`: llama.cpp through the `llama-cpp-2` crate. The model is memory-mapped with every layer on the GPU (Metal on Apple Silicon; the Linux and Intel macOS builds use the CPU). The context is the prompt plus the token limit, capped at what the model was trained on. Sampling: top-k 40, top-p 0.95, min-p 0.05, temperature 0.7.
- `src/template.rs`: the prompt is the model's own Jinja chat template (from the GGUF file), rendered with minijinja the way Hugging Face transformers renders it (`trim_blocks`, `lstrip_blocks`, Python string methods, `raise_exception`, `strftime_now`), so each family gets its exact format, including Gemma 4's and gpt-oss's.
- `src/think.rs`: splits the streamed text into thinking and answer across the families' markers (`<think>`, Gemma 4's thought channel, gpt-oss's analysis channel), holding back text that may be the start of a marker.

## Build and test
- `cargo build --release -p zazzle-llm` in `macos/rust` (add `--features metal` on Apple Silicon); put the binary beside `zazzled` to use it without the download.
- `cargo test -p zazzle-llm`: the catalogue, downloads (resume, hash, lock) against a local server, templates, the thinking splitter and the command line. The engine itself needs a model: `zazzle-llm prompt --model qwen3.5-2b --effort low "What is the capital of Iceland?"`.
- The release builds it with `macos/release/tools.sh` (see `macos/release/README.md`).
