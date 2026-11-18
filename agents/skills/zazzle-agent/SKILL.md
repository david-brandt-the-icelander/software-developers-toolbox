---
name: zazzle-agent
description: Find which rule files (AGENTS.md, CLAUDE.md, .claude/rules) and skill files (SKILL.md) apply to a file or folder with `zazzle agent get-rules PATH`. Use before editing in a folder you have not worked in, or when starting work somewhere other than the repository root.
---

# zazzle agent

Binary: `zazzle-agent`, or `zazzle agent ...` (same program, started from the `zazzle` binary's directory). Docs: `macos/rust/zazzle-agent-rules/list/README.md`.

## get-rules
- `zazzle agent get-rules PATH [--purpose NAME]...` prints the text of every rule and skill file for the home folder and every folder from `/` down to PATH (a file or folder that exists; else exit 66).
- Each file is a `==> rule FILE <==` (or `skill`) header and its text; `--name-only` prints just `rule FILE` or `skill FILE` lines, for choosing which skills to read.
- Order is farthest first, closest last; the closest file wins, and the user's chat overrides all files.
- Skill files are long: prefer `--name-only` first and read a skill only when its name fits the task.
- `--purpose NAME` (repeatable) keeps only files and segments marked `#zazzle-purpose:NAME` or between `#zazzle-purpose-begin:NAME` and `#zazzle-purpose-end:NAME`; unmarked files stay. A segment prints as `FILE:A-B`: read just those lines with `zazzle read FILE:A-B`.
