---
name: test-stub-then-fill
description: Plan broad unit-test coverage as compiling scenario stubs, then hand them to a cheaper model to fill in. Use for "tests for every method in X" or near-100% coverage; not for one-off tests.
---

# test-stub-then-fill

## Pass 1: planner (strong model)
1. Read the repo's test style guide (`*style*.md`, AGENTS.md); stubs must follow its naming, ordering and mocking rules.
2. Inventory public methods with real logic, listing branches and dependencies; use an Explore agent past ~10 files.
3. Skip records, DTOs, auto-properties and generated files; say so in the summary.
4. One test file per method, mirroring the class path.
5. Each stub body only raises not-implemented; its doc comment states the scenario, arrange values, mocks and expected result.
6. Compile the stubs (build only; never run the suite, stubs fail by design).
7. Write a self-contained fill-in prompt: repo path, style guide path, section ordering, command to find stubs, "do not edit passing tests", one reference test per category, final build/test commands, target state (no stubs left, all green).

## Pass 2: executor (cheap model, separate session)
- Per stub: read the production method, the stub doc comment and the reference test; write the test; run just that test; ask when a stub is ambiguous.

## Report
- Stub count, files skipped and why, and a note that tests fail until pass 2 finishes.
