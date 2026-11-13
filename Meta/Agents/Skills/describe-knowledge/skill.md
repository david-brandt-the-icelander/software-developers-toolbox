---
name: describe-knowledge
description: Create or maintain a folder's .dev knowledge files (vision, requirements, backlog, plan, decisions, blockers, contracts, verification, delivery, configuration, observability, runbooks and the rest) when recording what a solution knows about itself, intends, or is waiting on.
---

# Knowledge files

A knowledge file holds one kind of knowledge about the folder its `.dev/` directory belongs to: `<folder>/.dev/<kind>.json`. The catalogue in [`../../Knowledge/README.md`](../../Knowledge/README.md) lists the kinds, what each holds and where it usually lives. Read it, and the [schema](../../Knowledge/schemas/) and [fictional example](../../Knowledge/examples/) of each kind you write, before writing. The examples show the shape; they are not evidence about the system in front of you.

## Choosing the kind and the folder

- Write a kind only when there is something of that kind to say. No empty files, no placeholders. A missing file means nothing is recorded, not that there is nothing.
- Put each fact in the most specific folder it is about, once. Other records link to it with a `relationship`; a parent never copies its children. What is about the whole solution (vision, stakeholders, roadmap, plans across folders) goes in the repository root's `.dev/`.
- Wanted work is a `backlog` item in the folder that will own it, whether it is a feature request, a defect, debt or anything else. A defect needs the expected and the actual behavior; debt needs the compromise the code shows; a feature request needs an actual proposal. That something is missing is not, on its own, any of these.
- A decision waiting on someone is a `decisions` record with `status: open`, naming who it waits on (`waitingOn`) and what it blocks. Anything else that stops work is a `blockers` record. Never leave a question only in a chat or a commit message.
- What the code does is described by the views of the other skills (diagrams, features, capabilities, operations, use cases, journeys). Knowledge files hold what those do not: intent, constraints, promises, procedures, settings and signals. Link to the views; do not restate them.

## Writing a record

- Every record has an `id` unique in the file, a `title` and a `summary` a person reads first. Put the full explanation in `details`, as Markdown, but keep inventories, steps, criteria and signals in the kind's own typed fields rather than in prose.
- Fill in `origin` for intended work: who or what asked, when, and where (a sentence, or an object with `kind`, `by`, `date`, `reference`). Roles, never personal names or addresses.
- Set `evidenceStatus` for the whole record: `source-verified` only when you checked its substance against the code, and then cite the files and symbols in `sources`; `documented` for prose nobody checked; `proposed` for intent. A record that mixes them takes the weakest, or is split.
- Add any further field that helps a person or agent understand the record. Fields the schema does not name are allowed; keep the names plain and consistent within the repository.
- Use `links` for anything else useful (designs, tickets, conversations, documents), and `relationships` for links to other records: `{relation, path, entityId}`, with the path from the repository root. Read the relation literally: a verification record `verifies` its subject, a task is `part-of` its plan, a decision `blocks` what waits on it.
- Keep IDs when a record moves; change its `folder`, its paths and its `$schema`, and repair every link to it in the same commit.

## Particular kinds

- **configuration**: trace each setting from where it is read: its default, validation, scope, whether it is secret, and when a change takes effect (at startup, per use, on reload or at first use). Record it at the folder that owns its meaning, and link the folders that only pass it on. Never write a secret's value, not even as an example or default.
- **observability**: follow a signal from where it is emitted to where it is stored and who can read it. Record its real name, meaning, unit, dimensions, cadence, retention and what it deliberately leaves out. A logging call does not prove the signal is always emitted or kept; read the conditions. Readings themselves (numbers, dashboards, results) are not knowledge files.
- **contracts** and **versions**: a contract is what something outside the folder relies on; say what may change and what may not. Keep application releases, contract versions and stored-format versions as separate records.
- **verification**: the criteria and the checks (automated, with the test's path and symbol, or manual, with what a person does). Test results are readings and stay with the tools that produce them.
- **runbooks**: steps a person or agent can follow as written, and how to tell they worked. Use `purpose: setup` for getting a working copy running.
- **Work ends by being deleted.** When a backlog item is done and a view describes the result, delete the item; likewise plans, tasks and blockers once finished, and open decisions once taken (record the decision itself if it is still worth knowing). Git keeps the history.

## Checking

Run from the repository root, with `jsonschema` installed:

```bash
python3 Meta/Agents/Knowledge/validate_knowledge.py
```

It checks each file against its kind's schema, its name, `$schema` and `folder`, unique IDs, and that every path and `entityId` it mentions exists. A file that passes is well-formed, not true: read each `source-verified` record against the sources it cites. Update knowledge files in the same commit as the code or decision they describe.
