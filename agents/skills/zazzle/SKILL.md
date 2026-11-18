---
name: zazzle
description: Read, search, outline and edit files with the `zazzle` tool (or its MCP server `zazzled mcp`) instead of cat, sed, grep -r, find or sed -i, shrink noisy command output with `zazzle clean`, and run git or commit with `zazzle git commit -m`. Use for any code search, reading part of a large file, finding where a symbol is defined and used, or making a safe edit; replies are short, capped and deduplicated per session.
---

# zazzle

Prefer `zazzle` over shell reads and edits. Docs: `macos/rust/zazzled/README.md`.

## Use
- Configure clients with `zazzle setup` (the plan; follow it, ask the user before `--apply`, then `--check` after they restart the apps): it adds zazzle to Claude Code, the Claude desktop app and Codex and puts their other MCP servers behind the zazzle proxy; `--workspace DIR` lets edits reach every repository below DIR, and the hook it installs (leave it out with `--no-hook`) makes Claude Code and Codex refuse raw searches, reads, in-place writes and builds with the exact zazzle command to use (Codex asks the user to trust it once); reads and edits through zazzle bring a folder's nested AGENTS.md, CLAUDE.md and scoped rules; `--undo` reverses it, `--off`/`--on` take zazzle out and put the same setup back (for a with/without benchmark), and `--uninstall` removes it. User-specific choices (extra instruction text, Claude Code session keys such as autoCompactWindow) live in `~/.zazzle/policy.toml`; `zazzle setup --explain` describes it and `zazzle setup --set KEY=VALUE --apply` changes it, so nobody edits `~/.claude` or `~/.codex` by hand. `zazzle setup --verify` exercises zazzle from inside a repository and from elsewhere, and `--report` shows what went through zazzle and what bypassed it. If zazzle's tools are missing or failing, run `zazzle setup --check` (local; `--probe` also starts the other servers) and show the user its report. If the C# indexer is unavailable, carry on with find, read, ls, outline and refs.
- When the hook refuses a command, use the zazzle tool it names; run builds and tests as `zazzle run -- CMD` (the same command, same exit code). End a command with `# allow-raw` only when zazzle cannot do it (the bypass is logged).
- MCP (best, no process spawn): add the server `zazzled mcp`; tools `zazzle_find`, `zazzle_read`, `zazzle_ls`, `zazzle_outline`, `zazzle_refs`, `zazzle_edit`, `zazzle_read_again`, and from the C# indexer `zazzle_find_symbol`.
- CLI: `zazzle find|read|ls|outline|refs|edit ...` (`z` is the same command where Homebrew installed it); run `zazzle help` for the agent commands and `zazzle help COMMAND` (or `zazzle COMMAND --help`) for a command's flags. Set `ZAZZLE_SESSION=<any stable id>` in the shell so repeats are deduplicated.
- Locate first, read second: `zazzle outline FILE` (ranges per symbol), `zazzle find PATTERN`, `zazzle refs SYMBOL [--also SYMBOL]` (definitions first, each use shows its enclosing symbol; for usage questions), then `zazzle read FILE:A-B`, or read one symbol with its doc comments directly: `zazzle read FILE --symbol Type.method` (overloads by parameter types, `Type.method(int, string)`, which stay right when code moves, or `#N`; a miss lists the candidates). Several paths in one read share one budget; a missing path lists similar files to try.
- A reply starting `[r:ID]` was sent to you. A later `[r:ID] already sent: ...` means that content is still in your context; do not request it again. If you no longer have it (dropped or compacted), `zazzle read-again r:ID` (MCP `zazzle_read_again`) returns it exactly, or the current version if it changed; keep ids in your notes instead of content. `clean`, `run` and proxied outputs carry `[r:ID]` too and are kept for an hour; when a reply says something was cut, `zazzle read-again r:ID --full --lines A-B` gives the uncut lines.
- Output is capped (`find` 50 matches, `read` 6000 tokens, others ~8000). The footer says how to narrow or continue.
- Run builds, tests and installs with `zazzle run -- CMD ARGS` (`zazzle run -- cargo test`, `zazzle run -- dotnet test`, `zazzle run -- npx vitest run`, `zazzle run -- npx tsc --noEmit`): passing tests, progress and framework stack frames are dropped and counted, failures and warnings stay, and the footer gives the exit code, which is also the command's own.
- Pipe noisy command output through `zazzle clean` (`cargo test 2>&1 | zazzle clean`, `grep -rn x . | zazzle clean`): it strips colours and redraws, groups grep output by file, minifies JSON, collapses repeats, progress bars and blobs, and keeps head and tail within `--budget`. Every cut is marked; add `--lossless` to keep progress lines, blobs and long lines. `set -o pipefail` keeps the command's exit status.
- Regex is Rust syntax (no look-around or backreferences); use `-F` for literal text.

- `zazzle read` returns the literal source of a range (`FILE:A-B`, `REV:FILE`) or one symbol (`--symbol NAME`, with its comments, attributes and documentation), with line numbers and the sha edits need. A selection that fits the budget (default 6000 tokens, at most 16000) comes back whole; past it, the header says INCOMPLETE and the last line names the omitted range and the read that continues it.
- `--terse` (MCP `terse: true`) is an opt-in lossy summary for skimming large files: it folds imports, comments, logging, long lists, strings and repeated markup, shortens long C# locals and has no sha. Never use it to answer questions about exact behaviour, SQL, attributes or names.
- Grammars are downloaded modules: a file in a language whose module is missing reads as plain source while JIT is off or offline. `zazzle languages` shows them, `zazzle languages fetch` downloads the missing ones, `zazzle languages jit on|off` sets downloading on first use.
- Structure from `zazzle-csharp-indexer`'s index of the git work tree: `zazzle find-symbol NAME...` for where something is declared (literal names, several at once; `--path DIR|GLOB` limits it to a part of the tree); in a folder above several repositories it searches them all, and `--repo DIR` picks one.
- To see what the MCP proxy does to each request, `zazzle trace on DIR` (and `zazzle trace off`): every proxied request gets a folder DIR/MMDD_HHmmss with the request, the raw and returned responses and results.json (tokens, timings).
- To pin an answer to a commit, add `--commit SHA` (MCP `commit`; `HEAD` just reports it): repositories at another commit are refused or left out, and the answer ends with `[commit SHA REPO]`.
- Read a C# symbol without knowing its file: `zazzle read --symbol Acme.Shop.Orders.OrderService.Submit` (MCP: `zazzle_read` with `symbol` and no `paths`); `find-symbol` takes qualified names too. Paths may be relative to the repository or solution root, or `REPO:path`.

## Edit
- Every edit except `--create` needs `--expect SHA` from `zazzle read FILE` (or `--at` anchors from `--anchors`); a refusal means read the file again.
- Unique text: `zazzle edit FILE --old 'text' --new 'text' --expect SHA` (fails if the text is missing or not unique; `--all` for every match).
- Lines: `zazzle edit FILE --lines A-B --new TEXT --expect SHA` with the sha from `zazzle read`'s header, or `--at A#xyz-B#xyz` anchors from `zazzle read --anchors`. A stale sha or anchor is refused: read again.
- Also `--after N --new TEXT --expect SHA` and `--create --new TEXT`. `--new -` reads standard input.
- Edits are written atomically and only inside the current directory (symlinks are resolved first).

## Git
- `zazzle git ARGS` runs git (`zazzle git status -sb`); `zazzle git commit -m MSG [--no-add]` commits all tracked changes (or only the staged ones), like `git commit -a -m`. Agents always pass `-m`: without it the command opens an interactive terminal editor (`zazzle git message`) meant for people.

## Build
- `cd macos/rust && cargo build --release -p zazzled`, then `cd ../c/zazzle && make`; binaries `target/release/zazzled` and `macos/c/zazzle/build/zazzle`.
- macOS: `zazzled install` makes launchd start the daemon on demand. Elsewhere `zazzle` starts `zazzled` itself.
