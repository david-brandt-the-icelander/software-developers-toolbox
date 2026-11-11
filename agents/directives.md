# Directives for writing AGENTS.md files

Sources: agents.md, Claude Code memory and best-practices docs. Applies to AGENTS.md, CLAUDE.md and nested copies.

## Shape
- Keep under ~100 lines (hard cap 200); longer files lose adherence and cost tokens every session.
- Markdown headings + short imperative bullets. No prose paragraphs, no ALL CAPS emphasis.
- Every line must be checkable ("use 4-space indent", not "keep code tidy").
- No contradictions; if two lines could conflict, delete one.

## Include
- Exact build, test, lint, typecheck commands, including single-test command.
- Environment setup a fresh checkout needs (e.g. venv creation).
- Conventions that differ from tool defaults; pitfalls and legacy patterns not to copy.
- What to run before finishing or committing; commit/PR rules.
- Repo facts not discoverable from code; anything a reviewer corrected twice.

## Exclude
- Directory trees, architecture tours, dependency lists (the agent can read them).
- Personal preferences (use user-level files), secrets, one-off task notes.
- Multi-step procedures or single-area rules: move to a skill or nested AGENTS.md.
- Hard enforcement: use hooks/CI; the file is guidance only.

## Structure and pointers
- Root file is an index: short rules plus links to detail files (`agents/skills/<name>/SKILL.md`, `macos/rust/style.md`).
- Reference files by path in backticks; the agent loads them on demand.
- Closest AGENTS.md to the edited file wins; user chat overrides all files.
- Claude Code reads CLAUDE.md, not AGENTS.md: add a CLAUDE.md containing `@AGENTS.md`.
- Review the file when it grows; delete lines the agent already follows unprompted.

## Skills (agents/skills/<name>/SKILL.md)
- Frontmatter: `name` (matches directory), `description` (what it does + when to use, key use case first, concrete trigger words).
- Body is short imperative steps and exact commands; push details to sibling files.
- Keep under 500 lines; mark side-effecting skills `disable-model-invocation: true` when they must be user-triggered.
