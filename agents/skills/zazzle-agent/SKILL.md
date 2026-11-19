---
name: ai-agent-rules
description: Find which rule files (AGENTS.md, CLAUDE.md, CLAUDE.local.md, .claude/rules) and skill files (SKILL.md) apply to a file or folder with `zazzle agent get-rules PATH`, ask a local open model (Qwen, Gemma, gpt-oss) with `zazzle agent prompt`, shrink a command's output through it with `--llm`, get only the rules and skills a task needs with `zazzle brief INTENT`, and have it investigate a multi-file question with cited evidence (`zazzle investigate`). Use before editing in a folder you have not worked in, when starting work somewhere other than the repository root, or when the user asks for a local model's answer, a second opinion or a summary that must not leave the machine.
---

# zazzle agent

`zazzle agent get-rules ...` or `zazzle get-rules ...` (MCP `zazzle_get_rules`) is answered by daemon, which remembers per session what it sent; the `ai-agent-rules` binary runs the same lister without that memory. Docs: `src/macos/ai-agent-rules/README.md`.

## get-rules
- `zazzle agent get-rules PATH... [--purpose NAME]... [--name-only] [--skill-bodies] [--again]` prints the rules and skills for the home folder and every folder from `/` down to PATH (a file or folder that exists; else exit 66). Several paths print their union, each file once.
- In a session, files and segments already sent (by get-rules, or after a read or edit) are left out and named in one `-- already delivered` line; a changed file, a new purpose's segments, or a reset after compaction are sent again. `--again` sends everything. Universal rules stay in force even when not repeated.
- Each rule is a `==> rule FILE <==` header and its text; each file a rule imports (`@path`) follows it as `==> import FILE <==`; each skill is a `==> skill FILE <==` header and its description only. `--skill-bodies` prints whole skills; `--name-only` prints just `rule FILE`, `import FILE` or `skill FILE` lines. `.claude/rules` files with `paths:` globs appear only for matching paths.
- Order is farthest first, closest last; the closest file wins, and the user's chat overrides all files. A `CLAUDE.md` that only imports `@AGENTS.md` is not repeated.
- Read a skill's file only when its description fits the task.
- `--purpose NAME` (repeatable) keeps only files and segments marked for NAME with a marker on its own line or after a comment opener (`<!-- #zazzle-purpose:NAME -->`, or a `-begin:NAME` / `-end:NAME` pair); unmarked files stay. A segment prints as `FILE:A-B` and only those lines.

## Local models: llm and prompt
- `zazzle agent prompt [--model NAME] [--effort low|medium|high] [--system TEXT] [--max-tokens N] TEXT..` prints a local model's answer; `-` in TEXT (or no TEXT) reads standard input: `git diff | zazzle agent prompt "Review this diff:" -`.
- `zazzle agent llm` lists the models (size, downloaded or not, default); `--model NAME [--effort E]` downloads one and makes it the default; `--remove NAME` deletes it. Docs: `src/macos/ai-agent/README.md`.
- The first use downloads `ai-agent` and the model (gigabytes): tell the user before choosing a model that is not downloaded yet.
- Use `--effort low` for quick factual or formatting tasks; thinking is hidden unless `--show-thinking`.
- Exit 3 means the token limit cut the answer short: raise `--max-tokens` or lower `--effort`.
- To spend fewer tokens on output you need only part of (an API response, a log, a test run), add `--llm 'INSTRUCTION'` to the zazzle command (`zazzle run --llm 'Only the failing tests' -- cargo test`; MCP: `llm` on reading tools). The answer is the local model's and may drop detail: rerun without `--llm` when exact text matters.
- Before a task in an unfamiliar repository, `zazzle brief 'WHAT YOU WILL DO' --path PATH --purpose NAME` (MCP `zazzle_brief`). The paths and purposes decide what applies before the model is asked: every rule section that applies is printed verbatim, and the local model only ranks the skills. A budget (`--budget`, default 8000 tokens) drops whole skills, never part of a section or a rule. Sections this session already received (from `get-rules`, a read's notes or an earlier brief) are named, not repeated; `zazzle get-rules PATH --again` resends everything.
- A question that needs evidence from several files: `zazzle investigate 'QUESTION' --path SCOPE --symbol NAME` (MCP `zazzle_investigate`). Seed it with the real entry point (the endpoint's handler, found with `zazzle find-symbol` or `zazzle refs` first) and give one `--path` per layer the call crosses (facade, service, handoff to the next system) plus their tests. The local model follows read-only lookups inside the scope, starting from the symbols, and answers. Each `path:line` it cites comes back checked against the file and quoted; bad citations and unknowns are listed.
  - A symbol not found in the scope comes back with real candidate files and symbols; if no symbol exists and nothing resembles one, the result is "no evidence" and the model is not asked.
  - A lookup the model already made is not run again.
  - PARTIAL: the model answered with nothing after finding evidence, also after one repair turn; the evidence follows verbatim. INCOMPLETE: the run stopped early. The claims are the model's, so verify what you act on.
- Use the local model where it saves something. Read short results (find-symbol, refs, outline, a range) yourself; `--llm` passes output under ~400 tokens through unchanged. Don't spend a local call early, or just because one is expected.
- Rules reach you apart from the source. A read's rules go to standard error on the command line, so `zazzle read FILE | zazzle agent prompt 'Summarize:' -` sends the model only the source, while you still get the rules you must follow.
- `zazzle prompt TEXT` (MCP `zazzle_prompt`) asks the local model directly. These never download a model: if one is missing, ask the user to run `zazzle agent llm --model NAME`.
