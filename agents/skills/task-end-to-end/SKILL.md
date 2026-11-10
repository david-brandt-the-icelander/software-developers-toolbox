---
name: task-end-to-end
description: Take a ticket from link to verified PR in phases with a human gate before pushing. Use when asked to pick up and complete a ticket.
---

# task-end-to-end

1. Understand: read the ticket, comments and linked context; confirm it is the right ticket.
2. Shape: propose the approach and tests; agree it with the user for non-trivial work.
3. Implement test-first; use the repo's verification skills for risky areas (server build, monitoring-driven fixes).
4. Self-review the full diff.
5. Gate: lint, typecheck and tests pass; show results; stop for a human before pushing.
6. Push and open a draft PR linked to the ticket.
7. Verify on the branch deployment (`agents/skills/verify-before-merge/SKILL.md`).
8. Report on the ticket: what changed, how verified, what remains.

## When it goes sideways
- Blocked externally: say so and propose the blocked status.
- Plan wrong mid-implementation: revise and tell the user.
- Wrong ticket: stop and ask.
