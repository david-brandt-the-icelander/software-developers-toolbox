---
name: pr-ci-monitoring
description: Diagnose a PR's checks, missing CI, conflicts or stale runs before reporting green. Use when asked whether a PR passes or why CI did not run.
---

# pr-ci-monitoring

## Read the whole state first
```bash
gh pr view <n> --repo <owner>/<repo> --json number,isDraft,mergeable,mergeStateStatus,headRefOid,statusCheckRollup
```

## Symptoms
- No checks and `CONFLICTING`: GitHub skips `pull_request` runs it cannot merge; merge the base in.
- No checks and `MERGEABLE`: read the workflow `on:` filters for this base branch.
- `SKIPPED` job: a change filter, not a failure.
- `UNSTABLE`: read individual conclusions, not the rollup.
- Green on an older SHA: compare the run `headSha` with `headRefOid` before saying green.

## Failure cause
```bash
RUN=$(gh run list --repo <owner>/<repo> --branch <b> --limit 1 --json databaseId --jq '.[0].databaseId')
gh run view $RUN --repo <owner>/<repo> --log-failed | grep -iE "error|FAIL|Assert" | head -20
```
- Grep logs; never read them whole.
- Pinned-value gates print the correct value; copy only values a tool printed in your run.
- Regenerate conflicting generated files against the merged tree.
- If only CI caught it, add a fast local test that fails on the defect.

## Rules
- Re-run only after pushing a change.
- Do not poll; check all open PRs in one loop, once.
- Done means: run SHA equals branch head, conclusion success, no open review defects, PR marked ready.
