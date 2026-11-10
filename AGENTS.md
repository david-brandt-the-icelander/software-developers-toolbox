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
- Triage prod errors/alarms read-only, fix in worktree: `agents/skills/error-triage/SKILL.md`
- Report what a branch promotion ships and its risks: `agents/skills/release-promotion-check/SKILL.md`
- Merge without semantic conflicts; pick merge method: `agents/skills/safe-merge/SKILL.md`
- Finish a change: CI, change note, commit, draft PR, report: `agents/skills/change-delivery/SKILL.md`
- Did a branch land (squash-merge aware): `agents/skills/branch-landed-check/SKILL.md`
- Audit and clean branches and worktrees: `agents/skills/branch-cleanup/SKILL.md`
- Where and how to write an agent rule: `agents/skills/agent-rule-authoring/SKILL.md`
- Per-audience status reports across repos: `agents/skills/status-report/SKILL.md`
- Find why code is as it is from the record: `agents/skills/why-is-it-like-this/SKILL.md`
- Verify beyond unit tests before merging: `agents/skills/verify-before-merge/SKILL.md`
- Recover a node that fails at startup: `agents/skills/recover-failed-node/SKILL.md`
- Ticket to verified PR with a human gate: `agents/skills/task-end-to-end/SKILL.md`
- Personal TODO.md and WORKLOG.md: `agents/skills/todo-and-worklog/SKILL.md`
- Database migration guardrails: `agents/skills/migration-safety/SKILL.md`
- Second agent shadows a small spec in a worktree: `agents/skills/shadow-implementation/SKILL.md`

## Layout
- `macos/python/` macOS scripts and Python guidance; `agents/` agent docs, skills, `hooks/` (wire via `settings-snippet.json`) and `agents/agents/` subagents.
