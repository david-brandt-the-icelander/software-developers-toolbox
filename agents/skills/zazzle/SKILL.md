---
name: zazzle
description: Read, search, outline and edit files with the `zazzle` tool (or its MCP server `zazzled mcp`) instead of cat, sed, grep -r, find or sed -i. Use for any code search, reading part of a large file, finding where a symbol is defined and used, or making a safe edit; replies are short, capped and deduplicated per session.
---

# zazzle

Prefer `zazzle` over shell reads and edits. Docs: `macos/rust/zazzled/README.md`.

## Use
- MCP (best, no process spawn): add the server `zazzled mcp`; tools `zazzle_find`, `zazzle_read`, `zazzle_ls`, `zazzle_outline`, `zazzle_refs`, `zazzle_edit`.
- CLI: `zazzle find|read|ls|outline|refs|edit ...`; run `zazzle help` for flags. Set `ZAZZLE_SESSION=<any stable id>` in the shell so repeats are deduplicated.
- Locate first, read second: `zazzle outline FILE` (ranges per symbol), `zazzle find PATTERN`, `zazzle refs SYMBOL` (definitions first, each use shows its enclosing symbol), then `zazzle read FILE:A-B`.
- A reply starting `[r:ID]` was sent to you. A later `[r:ID] already sent: ...` means that content is still in your context; do not request it again. Add `--disable-session-deduping` only if you lost it.
- Output is capped (`find` 50 matches, `read` ~3000 tokens, others ~8000). The footer says how to narrow or continue; follow it instead of raising caps.
- Regex is Rust syntax (no look-around or backreferences); use `-F` for literal text.

## Edit
- Unique text: `zazzle edit FILE --old 'text' --new 'text'` (fails if the text is missing or not unique; `--all` for every match).
- Lines: `zazzle edit FILE --lines A-B --new TEXT --expect SHA` with the sha from `zazzle read`'s header, or `--at A#xyz-B#xyz` anchors from `zazzle read --anchors`. A stale sha or anchor is refused: read again.
- Also `--after N --new TEXT --expect SHA` and `--create --new TEXT`. `--new -` reads standard input.
- Edits are written atomically and only inside the current directory (symlinks are resolved first).

## Build
- `cd macos/rust && cargo build --release -p zazzled`, then `cd ../c/zazzle && make`; binaries `target/release/zazzled` and `macos/c/zazzle/build/zazzle`.
- macOS: `zazzled install` makes launchd start the daemon on demand. Elsewhere `zazzle` starts `zazzled` itself.
