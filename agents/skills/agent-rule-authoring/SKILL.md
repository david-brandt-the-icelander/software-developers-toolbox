---
name: agent-rule-authoring
description: How to add or change an agent rule: whether it is a rule, where it goes, how to word it and what to delete to pay for it. Use when editing AGENTS.md, nested instructions or skills; complements agents/directives.md.
---

# agent-rule-authoring

## Is it a rule?
- Not a rule: an incident narrative (the lesson is the rule), something code or a test already enforces, or a description of how the system works.

## Where
- Applies everywhere: root AGENTS.md. One area: nested AGENTS.md or scoped instructions. A multi-step procedure: a skill. Route every new file from the root index or it will not load.

## Wording
- One imperative present-tense sentence; add reasoning only if needed to apply it.
- Name the enforcement ("Enforced by <test>") and prefer a gate (test, hook, CI) to a paragraph.
- No war stories, ALL CAPS or decorative emphasis; never state a rule in two places, link instead.

## Finish
- Pay for it: delete or merge a line of equal size.
- Re-read as an agent with no context; check counter-copies in user-level files; measure `wc -c AGENTS.md` and report growth.
