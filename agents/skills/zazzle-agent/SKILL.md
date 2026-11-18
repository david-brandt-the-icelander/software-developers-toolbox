---
name: zazzle-agent
description: Find which rule files (AGENTS.md, CLAUDE.md, CLAUDE.local.md, .claude/rules) and skill files (SKILL.md) apply to a file or folder with `zazzle agent get-rules PATH`. Use before editing in a folder you have not worked in, or when starting work somewhere other than the repository root.
---

# zazzle agent

`zazzle agent get-rules ...` or `zazzle get-rules ...` (MCP `zazzle_get_rules`) is answered by zazzled, which remembers per session what it sent; the `zazzle-agent` binary runs the same lister without that memory. Docs: `macos/rust/zazzle-agent-rules/list/README.md`.

## get-rules
- `zazzle agent get-rules PATH... [--purpose NAME]... [--name-only] [--skill-bodies] [--again]` prints the rules and skills for the home folder and every folder from `/` down to PATH (a file or folder that exists; else exit 66). Several paths print their union, each file once.
- In a session, files and segments already sent (by get-rules, or after a read or edit) are left out and named in one `-- already delivered` line; a changed file, a new purpose's segments, or a reset after compaction are sent again. `--again` sends everything. Universal rules stay in force even when not repeated.
- Each rule is a `==> rule FILE <==` header and its text; each file a rule imports (`@path`) follows it as `==> import FILE <==`; each skill is a `==> skill FILE <==` header and its description only. `--skill-bodies` prints whole skills; `--name-only` prints just `rule FILE`, `import FILE` or `skill FILE` lines. `.claude/rules` files with `paths:` globs appear only for matching paths.
- Order is farthest first, closest last; the closest file wins, and the user's chat overrides all files. A `CLAUDE.md` that only imports `@AGENTS.md` is not repeated.
- Read a skill's file only when its description fits the task.
- `--purpose NAME` (repeatable) keeps only files and segments marked for NAME with a marker on its own line or after a comment opener (`<!-- #zazzle-purpose:NAME -->`, or a `-begin:NAME` / `-end:NAME` pair); unmarked files stay. A segment prints as `FILE:A-B` and only those lines.
