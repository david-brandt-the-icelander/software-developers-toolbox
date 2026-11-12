---
name: agent-fs
description: Read, search, list and outline files with the `afs` tool instead of cat, sed -n, grep -r, git grep or find. Use for any repeated file reading or code search to cut context tokens; it returns unchanged markers and diffs for content already seen.
---

# agent-fs

Prefer `afs` over shell reads. Docs: `macos/rust/agent-fs/README.md`.

## Use
- Tools (MCP `afs serve`): `afs_read`, `afs_search`, `afs_list`, `afs_stat`, `afs_outline`, `afs_diff_since`.
- Without MCP, run `afs jsonl` once and pipe one JSON request per line; `afs call '<json>'` has no session memory.
- Start with `afs_outline` (`[start,end,kind,name]` per symbol) or `afs_search` (`files_only`, `fields:["line"]`), then `afs_read` only the `ranges` you need, e.g. `[[start,end]]` from an outline row.
- `{"unchanged":true,"ref":"rN"}` means the content is in your earlier result `rN`; do not re-request it. Use `fresh:true` only if you lost it.
- A `hunks` reply replaces re-reading: apply from the last `at` to the first (`at` = old line, `del` = lines removed, `ins` = new text).
- On `truncated`, continue with `next_ranges` / `next_cursor` or narrow the query; do not raise caps blindly.

## Build
- `cd macos/rust && cargo build --release -p agent-fs`; binary `target/release/afs`.
