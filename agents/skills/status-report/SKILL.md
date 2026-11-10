---
name: status-report
description: Research work across repositories over a period and draft audience-specific status reports. Use for standup notes, weekly summaries or project updates.
---

# status-report

1. Scope the period, repos and audiences.
2. Pre-filter with a cheap shell sweep (commits in the window per repo) before spending agents; fan out one agent per active repo, never serial over many repos.
3. Decide unpushed or landed by content, not ancestry (see `agents/skills/branch-landed-check/SKILL.md`).
4. One report per audience:
   - Developer standup: one-minute speaking notes, cross-repo themes first.
   - Business owner: customer and money terms, no branch, PR or repo names.
   - Personal record: full detail, commit volume, open PRs and branches.
5. Project-update fields: suggested status (on track, at risk, off track) with reason; two-three sentence summary; landed on mainline; complete but not deployed (kept separate); foundations; next period; needs attention (unverified claims).
6. Post to a tracker only when asked.
