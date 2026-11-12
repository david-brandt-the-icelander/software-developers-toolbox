---
name: afo
description: Read, search, outline and edit files with the `afo` tool (or its MCP server `afod mcp`) instead of cat, sed, grep -r, find or sed -i. Use for any code search, reading part of a large file, finding where a symbol is defined and used, or making a safe edit; replies are short, capped and deduplicated per session.
---

# afo

Prefer `afo` over shell reads and edits. Docs: `macos/rust/afod/README.md`.

## Use
- MCP (best, no process spawn): add the server `afod mcp`; tools `afo_find`, `afo_read`, `afo_ls`, `afo_outline`, `afo_refs`, `afo_edit`.
- CLI: `afo find|read|ls|outline|refs|edit ...`; run `afo help` for flags. Set `AFO_SESSION=<any stable id>` in the shell so repeats are deduplicated.
- Locate first, read second: `afo outline FILE` (ranges per symbol), `afo find PATTERN`, `afo refs SYMBOL` (definitions first, each use shows its enclosing symbol), then `afo read FILE:A-B`.
- A reply starting `[r:ID]` was sent to you. A later `[r:ID] already sent: ...` means that content is still in your context; do not request it again. Add `--disable-session-deduping` only if you lost it.
- Output is capped (`find` 50 matches, `read` ~3000 tokens, others ~8000). The footer says how to narrow or continue; follow it instead of raising caps.
- Regex is Rust syntax (no look-around or backreferences); use `-F` for literal text.

## Edit
- Unique text: `afo edit FILE --old 'text' --new 'text'` (fails if the text is missing or not unique; `--all` for every match).
- Lines: `afo edit FILE --lines A-B --new TEXT --expect SHA` with the sha from `afo read`'s header, or `--at A#xyz-B#xyz` anchors from `afo read --anchors`. A stale sha or anchor is refused: read again.
- Also `--after N --new TEXT --expect SHA` and `--create --new TEXT`. `--new -` reads standard input.
- Edits are written atomically and only inside the current directory (symlinks are resolved first).

## Build
- `cd macos/rust && cargo build --release -p afod`, then `cd ../c/afo && make`; binaries `target/release/afod` and `macos/c/afo/build/afo`.
- macOS: `afod install` makes launchd start the daemon on demand. Elsewhere `afo` starts `afod` itself.
