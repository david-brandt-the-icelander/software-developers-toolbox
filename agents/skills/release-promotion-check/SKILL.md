---
name: release-promotion-check
description: Read-only report of what ships when one branch is promoted to another (for example staging to production): PR table, migration and deploy risks, and what is still behind. Use when asked what a release contains or whether staging is ready.
---

# release-promotion-check

Fetch, compare, report; opening the PR, merging and pushing belong to the user. Use `origin/*` refs only.

1. `git fetch origin -q`.
2. List PRs shipping: `git log --merges --format='%h %s' origin/<prod>..origin/<staging>`; drop carrier merges (branch-to-branch syncs).
3. Describe each PR from its diff (`git diff --stat $m^1 $m`), never from its branch name; name the consumer that sees the change.
4. Safety checks:
   - `git merge-tree --write-tree origin/<prod> origin/<staging>` exits 0; diff the result against staging to find prod-only hotfixes.
   - Read every new migration; for drops, renames or raw SQL, grep prod code still using the objects (old tasks run against the migrated database during rollout); mention backups.
   - New required settings must exist in the prod environment before deploy.
5. List PRs still only on the integration branch (`origin/<staging>..origin/<dev>`) with one-line descriptions and what to check before including them.

## Report
- Counts, clean-merge status and `--shortstat`.
- PR table; group related PRs; mark CI-only changes.
- Risks found, each with concrete failure and remedy.
- Still on dev.
- Optional chat-ready bullet list for the team: one bullet per row, customer-facing first, CI and tooling last, no PR numbers.
