---
name: parallel-bug-fix
description: Fix several independent bugs at once, one sub-agent per bug in its own git worktree and branch. Use when given a list of bugs to fix.
---

# parallel-bug-fix

Input: a list of bugs. Spawn one sub-agent per bug in parallel with `isolation: "worktree"`; never combine fixes.

## Per-agent brief
1. Branch `fix/<kebab-bug-name>`.
2. Find the cause with Grep/Read; locate the related tests.
3. Make the minimal fix; no refactors, no comments on unchanged lines.
4. Write a test that fails without the fix, then run the affected tests and the repo linter (see AGENTS.md for commands); retry up to 3 times.
5. Commit with a message saying what broke and why.
6. Report: changes, files touched, test results.

## After all agents finish
- Present a table of branches with results for the user to review; push nothing without being asked.
