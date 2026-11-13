---
name: populate-dev
description: Populate, reconcile or assess a repository's .dev folders from source evidence, choosing for each folder which diagrams, behavior views and knowledge files it needs and routing each to its describe-* skill. Use it to document a repository, bring its .dev folders up to date after changes, or plan that work.
---

# Populating `.dev` folders

This skill decides what each folder needs; the `describe-*` skills say how to write each file. Read [`../README.md`](../README.md) for the views, and [`../../Knowledge/README.md`](../../Knowledge/README.md) for the knowledge files, before starting. Read the existing `.dev` files and `Actors/` before writing anything new.

## Choosing what each folder needs

Work bottom up: a folder's children are assessed before the folder itself, so a parent can link to what its children say instead of repeating it. Leave out `.git`, dependencies, build output and generated code. For each folder, read its source, its public entrypoints, the code that wires it together and its tests, and decide which of these the evidence establishes:

| Evidence in the folder | Write, with its skill |
| --- | --- |
| A maintained folder with a purpose of its own | `folder` knowledge file ([describe-knowledge](../describe-knowledge/skill.md)) |
| A cohesive capability, adapter, boundary or subsystem | component diagram |
| More than one class declared in the folder itself | class diagram |
| A state machine | one state diagram per machine |
| Something deployed on its own | deployment diagram, and `delivery` and `pipeline` knowledge files |
| Callable entrypoints | operations, and the capability they belong to |
| Behavior the product offers | features |
| An actor's goal, step by step | use cases, and the actor's file in `Actors/` |
| A person's experience over time, in documentation or the code | a user journey |
| Settings read, signals emitted, promises to outside code, procedures, tests, known limits | `configuration`, `observability`, `contracts`, `runbooks`, `verification`, `limitations` |
| Intent, wanted work, open questions | `vision`, `requirements`, `backlog`, `plan`, `decisions`, `blockers` and the rest, where the owner or the repository's documents state them |

A search finds candidates; one match does not justify a file. Most folders need few of these, and some need none beyond `folder`. These kinds overlap and are not a sequence: there is no need for every folder to have a capability, a feature and a use case.

## Writing

- Verify each substantive claim against the implementation, its configuration, its authorization and its tests. That a file exists verifies nothing. Cite exact symbols where the code supports the statement, and mark what you did not check `documented` or `proposed`.
- Keep existing IDs, history and origins. Moving a record changes its folder, its paths and its `$schema`; repair every link to it in the same commit.
- Consolidate duplicates at the one folder that owns the fact, and link to it from elsewhere.
- Do not invent intent. A vision, a requirement or a backlog item comes from the owner, the repository's documents or the code's own evidence (an unhandled case, a compromise it states), and its `origin` says which.
- Keep readings out: test results, measurements, what is deployed now.
- No secrets, personal details, configuration values or method bodies.

## Assessing and planning

When asked for an assessment or a plan rather than the files, inspect the tree and list: the existing files to update, the missing files to add, and the candidates that need a person's judgement, each with its evidence and how anyone can tell it is done. Keep confirmed gaps apart from optional improvements and from work already done. Do not carry out the plan because this skill is active. If the plan is to be kept, write it as a `plan` knowledge file in the root's `.dev/`, with its backlog items in the folders that will own them.

## Checking

Run both validators from the repository root, with `jsonschema` installed, and fix what they report:

```bash
python3 Meta/Agents/Skills/validate_diagrams.py
python3 Meta/Agents/Knowledge/validate_knowledge.py
```

Passing shows the files are well-formed, not that they are true, complete or that their relationships point the right way. Read the result against the sources before finishing. Do not run the system or its infrastructure to document it.
