# qwen-chat

`qwen-chat` lets you talk to the Qwen3.5-9B language model in the terminal. The model runs locally: llama.cpp is compiled into the binary (Metal on macOS, CPU elsewhere), and no server or Python is needed.

```bash
qwen-chat                                   # interactive chat
qwen-chat --think                           # let the model reason first (reasoning shown dimmed)
qwen-chat -p "Explain RAII in two lines"    # one question, answer on stdout
git diff | qwen-chat -s "Review this diff."  # piped stdin is one question
qwen-chat --download                        # only fetch the weights, print their path
```

## The model
The 5.7 GB weights (`Qwen3.5-9B-Q4_K_M.gguf` from `unsloth/Qwen3.5-9B-GGUF`, pinned revision and SHA-256, Apache-2.0) are too large to put in the binary itself. On first run they are downloaded once into the cache folder. An interrupted download resumes, and the file is checked against its checksum before it is used.

| Cache folder | |
|---|---|
| `$QWEN_CHAT_HOME` | if set |
| `~/Library/Caches/qwen-chat` | macOS |
| `$XDG_CACHE_HOME/qwen-chat`, else `~/.cache/qwen-chat` | Linux |

`--model PATH` uses any other GGUF file instead. Use a Qwen3.5 model, because the prompt format is Qwen's ChatML.

## Chat
Type a message and press Enter. Replies stream as they are generated. `/reset` starts a new conversation, `/think` toggles reasoning, and `/exit` (or Ctrl-D) quits. Ctrl-C stops a reply and keeps the chat open; at the prompt it quits.

Sampling follows Qwen's recommendations: temperature 0.7, top-p 0.8, top-k 20 and presence penalty 1.5, or temperature 1.0 and top-p 0.95 with `--think`. When the conversation outgrows `--ctx` (default 8192 tokens), the oldest exchanges are dropped.

| Flag | |
|---|---|
| `-p, --prompt TEXT` | one question, then exit |
| `-s, --system TEXT` | system message |
| `--think` | reasoning on |
| `-m, --model PATH` | other GGUF weights |
| `--ctx N` | context size in tokens (8192) |
| `--max-tokens N` | longest reply (4096) |
| `--threads N` | CPU threads (all cores) |
| `--gpu-layers N` | layers on the GPU (999 = all; 0 = CPU only) |
| `--seed N` | sampling seed |
| `-v, --verbose` | show llama.cpp's log |

Exit status: 0 ok, 1 error or a one-shot reply that was cut short, 130 Ctrl-C at the prompt.

## Build
```bash
cd macos/rust && cargo build --release -p qwen-chat
```
The first build compiles llama.cpp, so it needs CMake and a C++ compiler and takes a few minutes. qwen-chat is not yet part of the release tarball or the Homebrew formula.

Memory: about 6 GB for the weights plus the context. On a 4-core Linux CPU it answers at a few tokens per second; on Apple silicon with Metal it is much faster.
