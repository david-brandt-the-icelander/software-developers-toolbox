---
name: change-delivery
description: Finishing procedure for a change: CI-first verification, per-file coverage, change-note entry, commit, push, draft PR and completion report. Use after making any change.
---

# change-delivery

## Verify
- Prefer dispatchable CI over local builds; batch fixes per round trip. Run locally only when CI cannot (DB-backed or generator tests, refused dispatch, user asks).
- Check job names, commit SHA and base branch; an empty or green list proves nothing.
- Failures that look unrelated usually exist on the base branch; dispatch it to confirm.
- Coverage bar (if the repo sets one) is per touched file, not aggregate; count only executable lines.

## Change note (if the repo requires entries)
- Add a new file per change (editing an old one does not count), path by timestamp.
- Prose only: title, what changed and why, resolution. No metadata, file lists, requester or testing-impact headings; git supplies those.
- Classify compatibility. For client-facing API changes give what changed, before/after examples, required client action and migration path.
- Keep the plan in the commit message.

## Ship
1. Update `.gitignore` for new artifacts or secrets.
2. Stage explicit paths, not `git add -A`.
3. Conventional Commits message.
4. Verify `git remote -v`, push the feature branch, open or update a draft PR on the right base; keep its description current.
5. Mark ready only when CI on the head SHA is green and gates are met.

## Report
- What changed, what was verified and where, what is still in flight, and follow-up work.
