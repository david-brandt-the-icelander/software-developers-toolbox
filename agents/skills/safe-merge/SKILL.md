---
name: safe-merge
description: Merge branches without semantic conflicts: order-dependence tests, re-applying sweeps, syncing the target first, choosing merge method and describing the merge. Use before merging or resolving conflicts on long-lived or wide-impact branches.
---

# safe-merge

## Semantic merges
- A merge is unsafe if merging A into B then applying B's remaining work differs from the reverse.
- Treat each side as a rule (rename, wrapper, signature change), not a diff; re-run each rule over what the other side added.
- State the population and count in the merge commit ("re-applied the rename to the 4 new call sites").
- Regenerate script-produced sweeps and generated files instead of merging them.

## Before merging
1. Merge the latest target branch into the feature branch first and resolve there.
2. Find divergence with `git merge-base`, list change notes added on each side since, and read any marked as needing merge coordination before resolving conflicts.
3. For wide-impact changes, leave merge instructions: scope (file count), predicted conflicts, concrete resolution steps, migration names and ordering that parallel branches must keep.

## Method
- Squash: one logical change with noisy commits.
- Merge commit or fast-forward: several distinct work items, so history stays reviewable.
- Rebase: linear history wanted and nobody consumed the branch.
- Every merge carries a real description of what is merged, not the default message.
- This is how, not whether: merging to deployed branches stays the user's call.
