# AGENTS.md

Personal developer toolbox: small scripts, mostly stdlib Python.

## Rules
- Python: follow `macos/python/style.md`. Always use a venv for projects with dependencies.
- Every code file, class and function has exactly one plain-sentence `#` comment; no docstrings or metadata.
- Write code for low token use and easy agent comprehension.
- Never commit `__pycache__`, `.venv`, or state/log files.
- Editing AGENTS.md or skills: follow `agents/directives.md`.
- Decide reversible details and continue; ask first when ambiguity would waste hours or an action is destructive or shared.
- Fork from the default branch; commit, push and open a draft PR on your own branch; never merge, force-push or touch another session's branch.
- Keep tests with code; report what was actually verified. Prefer CI over heavy local runs (`agents/skills/machine-load/SKILL.md`).
- Never read untracked `.env` files, copy credentials into files, or start Docker without permission.
- Never commit build artifacts.

## Skills
- Free disk space on macOS (compression scan, cache/dev/docker cleanup): `agents/skills/free-disk-space/SKILL.md`
- Diagnose/repair macOS Wi-Fi, DNS, VPN: `agents/skills/fix-network/SKILL.md`
- Plain human-sounding prose: `agents/skills/natural-writing/SKILL.md`
- Long file writes without timeouts: `agents/skills/large-file-writes/SKILL.md`
- Fix several bugs in parallel worktrees: `agents/skills/parallel-bug-fix/SKILL.md`
- Review changes or a PR: `agents/skills/review-changes/SKILL.md`
- PR checks and CI diagnosis: `agents/skills/pr-ci-monitoring/SKILL.md`
- Broad test coverage via stubs: `agents/skills/test-stub-then-fill/SKILL.md`
- Keep a Mac responsive under load: `agents/skills/machine-load/SKILL.md`
- Delegating to other agents and reviewing them: `agents/skills/delegate-and-review/SKILL.md`

## Layout
- `macos/python/` macOS scripts and Python guidance; `agents/` agent docs, skills, `hooks/` (wire via `settings-snippet.json`) and `agents/agents/` subagents.
