---
name: verify-before-merge
description: Prove a change works beyond unit tests: test the merge result, run the production-style build, sweep routes, drive changed journeys, and report what was not verified. Use before opening or merging a PR.
---

# verify-before-merge

1. Scope: affected apps (shared packages affect every consumer), changed user journeys named as a user sees them, adjacent behavior that could regress.
2. Test the merge result against the current base SHA, not the branch tip.
3. Run the artifact the container actually runs (production server build); typecheck, lint, tests and dev server can all pass while deployed rendering fails.
4. On a branch deployment: sweep routes first, then run end-to-end journeys the diff touches. Specs relying on new test ids need the code committed and pushed.
5. Read red results honestly: a soft 404 is a 200 with a 404 body; a 403 is usually the WAF; filter known third-party noise explicitly.
6. Use test accounts and test personal ids only; flag specs that place real orders.
7. Report: each command, where it ran and against which SHA; journeys exercised and last screen reached; failures with logs; what was not exercised and why; manual checks still required.
