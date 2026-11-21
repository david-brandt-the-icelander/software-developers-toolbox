# zazzle agent llm / prompt / status / prune

Runs open-weight models on your own machine, so an agent (or you) can ask a local model from the shell: a second opinion, a summary of a long log, a classification, without an API key or sending anything off the machine.

```bash
zazzle agent llm                                        # the catalogue: size, downloaded or not, the default
zazzle agent llm --model qwen3.5-9b --effort high       # download it (once) and make it and the effort the default
zazzle agent chat                                       # talk in this terminal; /help, Ctrl+K new conversation, Ctrl+C leaves
zazzle agent prompt "Why is the sky blue?"             # one message in this terminal's conversation
git diff | zazzle agent prompt "Review this diff:" -    # `-` is standard input; no TEXT at all reads only standard input
zazzle agent prompt --model gpt-oss-20b --effort low --system "Answer in one line." "What is RAII?"
zazzle agent llm --remove qwen3.5-9b                    # delete a downloaded model
zazzle ai-agent status [--json] [--verify]              # the default, downloaded models and their checks, partial downloads, disk use
zazzle ai-agent prune [--apply]                         # list (or delete) every model but the default, stale partials, orphaned locks
```

## Two downloads on first use
- **The program.** `ai-agent` (this crate) is not in the release tarball or the Homebrew bottle: it carries llama.cpp and is only wanted by those who use it. The `zazzle` invoker hands `agent llm` and `agent prompt` to `zazzle daemon llm`, which finds `ai-agent` beside itself (a development build) or in `~/.zazzle/v1/tools`, downloading it first from `https://icelander.net/downloads/zazzle/vX.Y.Z/tools/PLATFORM/` (`src/tools.rs` in daemon). Like the language modules, it is accepted only when its SHA-256 matches the manifest compiled into that daemon (`ZAZZLE_TOOLS_MANIFEST`, written by `release/tools.sh`), then exec'd with the caller's terminal, environment and exit status. `ZAZZLE_TOOLS_DIR` and `ZAZZLE_TOOLS_URL` move the directory and the download URL.
- **The model.** `ai-agent` downloads a model the first time it is chosen or asked, from Hugging Face at the commit the catalogue pins (`src/catalog.rs`), into `~/.zazzle/v1/llm/models` (`ZAZZLE_LLM_DIR` moves `~/.zazzle/v1/llm`). Progress goes to standard error. An interrupted download resumes where it stopped; the file is installed only when its SHA-256 matches the catalogue's, so a changed or corrupted file is never loaded. Two prompts never download the same model at once (a lock file, `models/.FILE.lock`, held exclusively while downloading and shared while a prompt or worker runs the model).

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
`zazzle agent chat` and `zazzle agent prompt` start each terminal's conversation at `low` (`/effort` changes it). `ai-agent prompt --effort low|medium|high` (default `medium`, or what `llm --effort` saved) is passed to the model's own chat template as `enable_thinking` (off for `low`) and `reasoning_effort` (gpt-oss and Qwen 3.8 read it). It also sets the default token limit, thinking included: 4096, 8192 and 32768 (`--max-tokens` overrides). Models without a thinking mode (`qwen3-coder-next`) answer the same at every effort.

## Output
- `zazzle agent prompt` and `chat` print the whole answer once it is done; thinking is dropped. `ai-agent prompt` streams it, and prints thinking to standard error with `--show-thinking`.
- Exit 0 on a full answer; 3 when the token limit cut it short (a note on standard error says whether any answer came at all); 4 when `--no-download` finds the model missing; 2 for errors (unknown model, a failed download, a prompt longer than the model reads).
- For programs: `--no-download` never starts a model download, `--grammar GBNF` holds the answer to a grammar (root rule `root`; use `--effort low` so no thinking comes first), and `--temp 0` always takes the likeliest token, so the same question gets the same answer. The daemon uses all three when agents ask the model (`zazzle brief`, `--llm`): see `src/macos/daemon/README.md`.
- `ZAZZLE_LLM_LOG=1` shows llama.cpp's own log on standard error.

<<<<<<< HEAD
## The worker
`ai-agent worker [--model M] [--effort E] [--no-download]` loads a model once and answers JSON lines on standard input (`{"user", "history"?: [{"role","content"}..], "system"?, "effort"?, "max_tokens"?, "temp"?, "grammar"?}`), one JSON line each (`{"text", "tokens", "truncated", "prompt_tokens", "cached_tokens"}` or `{"error"}`: the tokens generated, whether the limit cut them short, the prompt's tokens and how many of them were already in the kept context), after a `{"ready": MODEL}` line. The worker keeps its context between prompts, so a prompt that starts like the previous one (the next turn of a chat or an investigation) decodes only what follows the shared prefix; a model whose cache cannot drop just its tail reads each prompt whole. daemon keeps one running for chats and agents' calls and stops it when idle (`src/macos/daemon/README.md`, "Chat and prompt"). `ai-agent fetch [--model M]` downloads a model unless it is here and prints its full name; the chat client uses it so a download shows its progress in the user's terminal.
=======
## Status and prune
`status` and `prune` are reached as `zazzle ai-agent status|prune` (the invoker routes only `agent llm` and `agent prompt` to ai-agent; `zazzle agent status` goes to the rule lister).

- `status` prints the default model and effort and whether it is downloaded; each file in `models/` with its size and check (`*` marks the default): `ok (size)`, `ok (sha256)` with `--verify`, `CORRUPT: ..`, or `not in the catalogue`, and `, in use` while a download or run holds its lock; partial downloads with their size, the expected size and `stale` or `downloading`; `store: PATH (N GB on disk)`; `problem: ..` lines; and `ai-agent VERSION`. Models are checked by size (a file is renamed into place only after its hash matched); `--verify` also hashes each one, reading every byte. Nothing is written.
- `status --json` prints one object: `tool` (`ai-agent`), `version`, `ok`, `store`, `disk_bytes`, `default` {`model`, `effort`, `downloaded`}, `models` [{`name`, `file`, `bytes`, `expected_bytes`, `check`: `size`|`sha256`|`corrupt`|`unknown`, `default`, `in_use`}], `partials` [{`name`, `file`, `bytes`, `expected_bytes`, `downloading`}] and `problems`. Exit 0 when ok; 1 when the default model is not downloaded (or not in the catalogue) or a model is corrupt; 2 on errors.
- `prune` lists what it would remove, with sizes and the total: every downloaded model except the default (files not in the catalogue included), partial downloads except the missing default's (it resumes), and lock files of models with nothing left. `--apply` deletes them, taking each model's lock first. A model whose lock is held (a download, prompt or worker) is `skipped`; the default model is never deleted. Exit 0, or 2 when a file could not be removed (the rest still are).
>>>>>>> bf41754 (Add tool commands: git amend/undo, rules check/explain, free-disk-space top/decompress, fix-network status, indexer status/stop/restart/uninit, ai-agent status/prune)

## How it runs
- `src/engine.rs`: llama.cpp through the `llama-cpp-2` crate. The model is memory-mapped with every layer on the GPU (Metal on Apple Silicon; the Linux and Intel macOS builds use the CPU). The context is the prompt plus the token limit, capped at what the model was trained on. Sampling: a grammar first when given, then top-k 40, top-p 0.95, min-p 0.05 and temperature 0.7 (`--temp`), or the likeliest token at `--temp 0`.
- `src/template.rs`: the prompt is the model's own Jinja chat template (from the GGUF file), rendered with minijinja the way Hugging Face transformers renders it (`trim_blocks`, `lstrip_blocks`, Python string methods, `raise_exception`, `strftime_now`), so each family gets its exact format, including Gemma 4's and gpt-oss's.
- `src/think.rs`: splits the streamed text into thinking and answer across the families' markers (`<think>`, Gemma 4's thought channel, gpt-oss's analysis channel), holding back text that may be the start of a marker.

## Build and test
- `cargo build --release -p ai-agent` in `src/macos` (add `--features metal` on Apple Silicon); put the binary beside `daemon` to use it without the download.
- `cargo test -p ai-agent`: the catalogue, downloads (resume, hash, lock) against a local server, locks, status and prune on a temporary store, templates, the thinking splitter and the command line. The engine itself needs a model: `ai-agent prompt --model qwen3.5-2b --effort low "What is the capital of Iceland?"`.
- The release builds it with `release/tools.sh` (see `release/README.md`).
