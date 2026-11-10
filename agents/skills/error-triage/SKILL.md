---
name: error-triage
description: Read-only triage of production errors (Sentry issues, CloudWatch or similar alarms), sorting each into auto-fix, ask or leave, then fixing in a detached worktree. Use when asked to check prod alarms, triage Sentry, or explain why an alarm fired.
disable-model-invocation: true
---

# error-triage

## Hard rules
- Monitoring and cloud tools are read only: no resolve, assign, comment, ignore, alert edits or paid AI analysis runs; never consume queue messages. Report alert-silencing advice instead.
- Any infrastructure change (thresholds, WAF, scaling, redrives, templates) is proposed with the exact diff or command and waits for a yes; deploys go through the pipeline.
- No branches, commits or pushes; fixes end as an uncommitted diff the user reviews.
- Event and log data stays in the session: output carries only ids, culprits, counts, dates and your own explanation, never payloads, messages, emails or personal ids. Tests use made-up data.
- If the monitoring tool is not connected, say how to connect it; do not scrape its web UI.

## Steps
1. Read `notes.md` beside the project's triage skill (known-benign items collapse to one line).
2. Pull unresolved issues or alarm history: last 7-30 days, production only, top 25 by volume; state what was capped.
3. Group issues sharing a root cause. Trace each stack frame to code in this repo, with `path:line`; if it ends in a dependency or service, say which. `git show <release>` finds the shipping change.
4. For alarms, read the metric filter before trusting its name, then metrics ±30 min and logs around the fire time (UTC).
5. Classify, with leave over ask over auto:
   - Leave: code that creates, charges, refunds or finalizes money state; migrations; API response shape; retry or timeout semantics; vendored code; not-our-code causes; correct alerts.
   - Ask: read-only access to those areas, or anything a customer or other system would notice.
   - Auto: clear cause, local fix, safe during rolling deploy, reproducible by a test.
6. Report time window, environment and "N of M shown", then three tables (auto, ask, leave) with issue, events, last seen, cause, fix or reason. Suggest downgrading a never-actionable alert rather than dropping it. Stop and wait on ask rows.
7. Implement in a detached worktree from the default branch (`git worktree add --detach`), copy local env files without reading them, write a failing test first, fix, then run lint and tests. If a fix fails the check, revert it and its test and move it to leave. Never `--force`.
8. Hand over: one line per fix (path:line, test), check exit codes, suggested branch and commit message (without issue ids). Remove the worktree only when clean and pushed.
