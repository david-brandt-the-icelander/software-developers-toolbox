---
name: zazzle-flow
description: Verify, write up, deliver and review a change with zazzle instead of doing each step by hand - `zazzle flow verify` (tree-pinned test receipts), `zazzle flow write commit|changelog|pr` (a cheap writer drafts the prose), `zazzle flow deliver` (stage, commit, push, pull request, safe to retry), `zazzle flow test-first`, `zazzle flow ci` and `zazzle review` (multi-viewpoint review that fixes what it finds). Use when a change is ready to test, describe, commit, push, open as a pull request, or review.
---

# zazzle flow and zazzle review

## Before
- `zazzle flow preview` shows what applies: checks, the branch pull requests target and its review level, the effects delivery may have, and conflicts in the repository's guidance. Settle a conflict with the user before delivering.
- Delivery needs the repository to opt in with `.zazzle/flow.json` (`zazzle flow init` prints a starter; `--write` saves it). Verify and review work without it.

## Verify
- `zazzle flow verify` runs the repository's own checks that apply to the change. The receipt names the tree it ran on; a changed tree has no receipt until you verify again.
- A missing program, a skip or a check that does not apply is listed, never counted as a pass. Logs are cited by path; read them only for a failure.

## Write
- You decide the intent and the compatibility decision; the writer only phrases them: `zazzle flow write pr --intent 'TEXT' --compat 'none: no client sees a change'`.
- `changelog --apply` adds the entry under Unreleased; commit it with the change it describes. `commit` and `pr` with `--apply` save the draft for `deliver`.
- A draft that says `MISSING:` or fails validation comes back with exit 1: supply the fact and run it again.

## Deliver
- `zazzle flow deliver --paths P.. -m 'MESSAGE' --pr --draft [--body-file F] [--require-verified]`: only the named paths are committed. Running it again after a partial failure reuses the commit, the push and the open pull request.
- A remote with commits the branch lacks is reported, never forced: fetch and merge, then deliver again.
- `zazzle flow ci` waits for the pull request's checks on its current head and reports once. Do not poll yourself.

## Test first
- Settle the behavior, the test paths and a nearby reference test first: `zazzle flow test-first --behavior 'TEXT' --tests PATH --reference FILE --run 'CMD' --expect 'TEXT'`.
- A test is accepted only when it fails for the stated reason. The fixer changes production code only; if it says the assertion is wrong, that decision is yours.

## Review
- `zazzle review --into BRANCH --intent 'TEXT'` reviews at that branch's level (feature, development, staging, production; default: this branch's). Earlier levels are brief because each later merge gets a stricter pass.
- It fixes confirmed findings, re-verifies and re-reviews before reporting. What is left is a decision for the user, missing access, or blocked verification. Use `--read-only` for findings without edits.
- `--bench` reports each phase with what went to hosted agents (charged) and what ran locally.
