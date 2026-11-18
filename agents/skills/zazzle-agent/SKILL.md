---
name: zazzle-agent
description: Find which rule files (AGENTS.md, CLAUDE.md, .claude/rules) and skill files (SKILL.md) apply to a file or folder with `zazzle agent list-rules PATH`. Use before editing in a folder you have not worked in, or when starting work somewhere other than the repository root.
---

# zazzle agent

Binary: `zazzle-agent`, or `zazzle agent ...` (same program, started from the `zazzle` binary's directory). Docs: `macos/rust/zazzle-agent-rules/list/README.md`.

## list-rules
- `zazzle agent list-rules PATH` prints one `rule FILE` or `skill FILE` per line for the home folder and every folder from `/` down to PATH (a file or folder that exists; else exit 66).
- Order is farthest first, closest last; the closest file wins, and the user's chat overrides all files.
- It lists paths only: read the rule files, and read a skill file only when its name fits the task.
