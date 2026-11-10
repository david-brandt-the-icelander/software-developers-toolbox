---
name: migration-safety
description: Guardrails for authoring database migrations. Use when adding or changing schema migrations.
---

# migration-safety

- Never delete, rename or move applied migrations; the ledger and registry must match on every database.
- Pair each schema change with its migration; do not hand-edit generated SQL.
- Keep migrations compatible with the old code during rolling deploy: drop or rename in a later release than the code change.
- Split an existence guard from the statement it protects; one statement binds the table before the guard runs and fails when it is absent.
- Validate version state with the generated verification SQL before shipping, and mention backups for destructive changes.
- Parallel branches must keep each other's migration names and predecessor order; record them in the merge note (`agents/skills/safe-merge/SKILL.md`).
