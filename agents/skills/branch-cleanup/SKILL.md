---
name: branch-cleanup
description: Periodic audit of local branches, worktrees and checkouts, classifying each as delivered, orphaned or retained and removing only what is safe. Use for "clean up my branches/worktrees".
---

# branch-cleanup

1. Protect live work first: list running agent sessions and processes whose cwd is inside a worktree; skip those.
2. Map topology: `git worktree list`, `git branch -vv`, per repo.
3. Classify every branch by content, not ancestry (see `agents/skills/branch-landed-check/SKILL.md`): delivered (content on default), orphaned (no PR, unique commits, stale), retained (open PR or unique wanted work).
4. Keep one long-lived base branch per initiative per repo; flag a base behind default or a second integration branch hiding under it.
5. Folder verdicts: keep, remove (clean and pushed), or ask.
6. Strip build artifacts only from stale checkouts (no commit in 7 days, clean tree).
7. Remove with `git worktree remove` and `git branch -d`; never `--force` or `-D` without asking.
8. Output a table: item, verdict, evidence, action taken.
