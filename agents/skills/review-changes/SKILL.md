---
name: review-changes
description: Review uncommitted changes or a PR number, fix the issues found, and rate the result. Use for "review my changes" or "review PR 123".
---

# review-changes

1. Get the diff: `gh pr diff <n>` for a PR, else `git diff HEAD`.
2. Read every changed file in full, not just the hunks; read the PR description when intent is unclear.
3. List issues as critical, major or minor.
4. Fix them with minimal scoped edits; do not refactor adjacent code.
5. Run the repo's lint and the tests for the touched files; iterate until green.
6. Summarize with a 1-10 rating for correctness, style, test coverage and overall.

## Rules
- A bug is wrong behavior or lost information a reader cannot recover; a nitpick is preference. Treat doubtful findings as bugs.
- After fixing a bug, re-review (ideally with a different model); stop when a pass finds zero bugs. Track counts per round.
- Review commit by commit with `git log --reverse -p <base>..<head>`, then the branch as a whole.
- Give reviewers the spec, not only the diff.
- Reviewer sub-agents are read-only: see `agents/agents/readonly-reviewer.md`.
