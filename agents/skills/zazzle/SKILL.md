---
name: zazzle
description: Read, search, outline and edit files with the `zazzle` tool (or its MCP server `zazzled mcp`) instead of cat, sed, grep -r, find or sed -i, and shrink noisy command output with `zazzle clean`. Use for any code search, reading part of a large file, finding where a symbol is defined and used, or making a safe edit; replies are short, capped and deduplicated per session.
---

# zazzle

Prefer `zazzle` over shell reads and edits. Docs: `macos/rust/zazzled/README.md`.

## Use
- MCP (best, no process spawn): add the server `zazzled mcp`; tools `zazzle_find`, `zazzle_read`, `zazzle_ls`, `zazzle_outline`, `zazzle_refs`, `zazzle_edit`, and from the C# indexer `zazzle_overview`, `zazzle_find_symbol`, `zazzle_changed_since`, `zazzle_git_log`, `zazzle_git_blame`, `zazzle_report_gap`.
- CLI: `zazzle find|read|ls|outline|refs|edit ...`; run `zazzle help` for flags. Set `ZAZZLE_SESSION=<any stable id>` in the shell so repeats are deduplicated.
- Locate first, read second: `zazzle outline FILE` (ranges per symbol), `zazzle find PATTERN`, `zazzle refs SYMBOL` (definitions first, each use shows its enclosing symbol), then `zazzle read FILE:A-B`, or read one symbol with its doc comments directly: `zazzle read FILE --symbol Type.method` (overloads by parameter types, `Type.method(int, string)`, which stay right when code moves, or `#N`; a miss lists the candidates).
- A reply starting `[r:ID]` was sent to you. A later `[r:ID] already sent: ...` means that content is still in your context; do not request it again. If you no longer have it (dropped or compacted), `zazzle read-again r:ID` (MCP `zazzle_read_again`) returns it exactly, or the current version if it changed; keep ids in your notes instead of content. `clean`, `run` and proxied outputs carry `[r:ID]` too and are kept for an hour; when a reply says something was cut, `zazzle read-again r:ID --full --lines A-B` gives the uncut lines.
- Output is capped (`find` 50 matches, `read` ~3000 tokens, others ~8000). The footer says how to narrow or continue; follow it instead of raising caps.
- Run builds, tests and installs with `zazzle run -- CMD ARGS` (`zazzle run -- cargo test`, `zazzle run -- dotnet test`, `zazzle run -- npx vitest run`, `zazzle run -- npx tsc --noEmit`): passing tests, progress and framework stack frames are dropped and counted, failures and warnings stay, and the footer gives the exit code, which is also the command's own.
- Pipe noisy command output through `zazzle clean` (`cargo test 2>&1 | zazzle clean`, `grep -rn x . | zazzle clean`): it strips colours and redraws, groups grep output by file, minifies JSON, collapses repeats, progress bars and blobs, and keeps head and tail within `--budget`. Every cut is marked; add `--lossless` to keep progress lines, blobs and long lines. `set -o pipefail` keeps the command's exit status.
- Regex is Rust syntax (no look-around or backreferences); use `-F` for literal text.

- Python, JavaScript, TypeScript, Dart, Rust, Go, Java, Kotlin, Swift, C, C++, Ruby, PHP, Scala, Perl, Lua, Elixir, Haskell, OCaml, F#, Clojure, Erlang, R, Julia, Zig, Objective-C, Groovy, PowerShell, SQL, Visual Basic, Nim and shell files read as a terse view too, without a sha: imports, logging, region markers, field-only constructors, log-and-rethrow catches, long shell heredocs and long SQL `INSERT` row lists are hidden and long locals (in most of these languages) have short names; read `--source` before editing.
- CSS, SCSS, Sass and Less files read as a terse view too, without a sha: long comments, imports, @font-face and @keyframes blocks and duplicate vendor prefixes are hidden and data URIs cut; read `--source` before editing.
- JSON, YAML, TOML and Markdown files read as a terse view too, without a sha: long lists keep their first and last items, lock files their first and last entries, and long comments, texts, strings, Markdown badges, comments, link definitions and tables of contents are hidden; read `--source` before editing.
- XML, INI files and Dockerfiles read as a terse view too: long comments, runs of 10+ same-tag XML elements (first and last kept), Dockerfile package lists and LABEL runs are hidden and long values cut; read `--source` before editing.
- Grammars for most languages are downloaded modules: a file in a language whose module is missing reads as plain source while JIT is off or offline. `zazzle languages` shows them, `zazzle languages fetch` downloads the missing ones, `zazzle languages jit on|off` sets downloading on first use.
- HTML and Razor files (`.html`, `.cshtml`, `.razor`) read as a terse view too, without a sha: long comments, `<style>`, `<svg>`, head tags and `@using` runs are hidden and long `class`/`style` values cut; read `--source` before editing.
- Structure from `zazzle-csharp-indexer`'s index of the git work tree: `zazzle overview` on an unfamiliar repository, `zazzle find-symbol NAME` for where something is declared, `zazzle changed-since [CURSOR]` to refresh after edits, `zazzle git-log [PATH]`, `zazzle git-blame FILE --start A --end B`. Say what you needed and could not get with `zazzle report-gap TEXT`.
- C# files read as a terse behavioural view with source line numbers and no sha (`N~` lines are changed, `A-B ⋯` lines are hidden, long locals have short names); use it to understand code, and `zazzle read FILE --source` before editing.

## Edit
- Every edit except `--create` needs `--expect SHA` from `zazzle read FILE --source` (or `--at` anchors from `--source --anchors`); a refusal means read the source again.
- Unique text: `zazzle edit FILE --old 'text' --new 'text' --expect SHA` (fails if the text is missing or not unique; `--all` for every match).
- Lines: `zazzle edit FILE --lines A-B --new TEXT --expect SHA` with the sha from `zazzle read`'s header, or `--at A#xyz-B#xyz` anchors from `zazzle read --anchors`. A stale sha or anchor is refused: read again.
- Also `--after N --new TEXT --expect SHA` and `--create --new TEXT`. `--new -` reads standard input.
- Edits are written atomically and only inside the current directory (symlinks are resolved first).

## Build
- `cd macos/rust && cargo build --release -p zazzled`, then `cd ../c/zazzle && make`; binaries `target/release/zazzled` and `macos/c/zazzle/build/zazzle`.
- macOS: `zazzled install` makes launchd start the daemon on demand. Elsewhere `zazzle` starts `zazzled` itself.
