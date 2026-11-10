---
name: branch-landed-check
description: Decide whether a branch's work has reached a target branch when PRs are squash-merged, so ancestry checks lie. Use when asked if a branch is merged, landed or unpushed.
---

# branch-landed-check

Squash merges rewrite commits: `git merge-base --is-ancestor` and `git branch --merged` report merged work as unlanded. Decide by content.

1. Prefer the repo's own tool or change-note comparison (entries the branch added versus those on the target).
2. Look for a merged PR on that exact head: `gh pr list --head <branch> --state merged`.
3. Compare trees: `git diff --stat origin/<target> origin/<branch>` with no output means identical content.
4. `git rev-list --count origin/<target>..origin/<branch>` of 0 means fully contained.
5. Partially landed (some entries present) is a finding; list which are missing.
6. Never push to a branch whose PR merged: branch from the target, cherry-pick, open a new PR.
