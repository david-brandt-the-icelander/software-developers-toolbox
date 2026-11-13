# Agent skills

Skills that tell an agent how to do one particular job in a repository. Each skill is a folder with a `skill.md`: a short Markdown file whose front matter gives the skill's `name` and a one-line `description` an agent can match a task against, followed by a page of instructions. Where a skill produces a JSON file, the folder also holds the schema that file must follow and a complete, fictional example of a correctly filled-out one; where it produces a Markdown file, a template and a fictional example. Both Claude Code and Codex read this format.

## The skills

| Skill | Use it when | Produces |
| --- | --- | --- |
| [describe-component-diagram](describe-component-diagram/skill.md) | A folder is a cohesive capability, adapter, boundary or subsystem that belongs in a system component diagram | `<folder>/.dev/component-diagram.json` |
| [describe-class-diagram](describe-class-diagram/skill.md) | A folder declares more than one class (records, nested and partial classes counted; descendant folders not) | `<folder>/.dev/class-diagram.json` |
| [describe-state-diagram](describe-state-diagram/skill.md) | A folder implements a state machine; one file per machine, named after its class | `<folder>/.dev/<Identifier>.state-diagram.json` |
| [describe-deployment-diagram](describe-deployment-diagram/skill.md) | A folder contains an independently deployable executable, service, container or package | `<folder>/.dev/deployment-diagram.json` |
| [describe-use-case](describe-use-case/skill.md) | An actor reaches a goal through a folder's code: the steps in order, and the branches the code has | `<folder>/.dev/use-case.json` |
| [describe-user-journey](describe-user-journey/skill.md) | Documentation or the code shows a person's experience over time, across use cases and touchpoints | `<folder>/.dev/user-journey.json` |
| [describe-capability](describe-capability/skill.md) | A folder can do something for someone, and the code says who may ask for it | `<folder>/.dev/capability.json` |
| [describe-feature](describe-feature/skill.md) | A folder offers behavior with an outcome and limits on it | `<folder>/.dev/feature.json` |
| [describe-operation](describe-operation/skill.md) | A folder exposes callable entrypoints: routes, commands, handlers, messages, scheduled jobs | `<folder>/.dev/operation.json` |
| [describe-actor](describe-actor/skill.md) | A use case, journey or diagram names an actor: a person in a role, an external system with goals of its own, or an agent that uses the system | `Actors/<name>.md` |
| [describe-knowledge](describe-knowledge/skill.md) | There is intent, wanted work, an open decision, a blocker, a contract, a check, a setting, a signal, a procedure or a limit to record for a folder | `<folder>/.dev/<kind>.json`, following [the knowledge catalogue](../Knowledge/README.md) |
| [populate-dev](populate-dev/skill.md) | A repository's `.dev` folders are to be written, brought up to date, or assessed and planned: it decides what each folder needs and routes to the skills above | the files the other skills produce |

`validate_diagrams.py` checks the nine schemas (four diagrams, five behavior views) and their examples, then every `.dev/*.json` view in the repository (skipping `.git`, `node_modules` and build output): schema validity, unique IDs, relationship endpoints, the folder each diagram claims, source paths that exist (in a behavior view, every source it cites, down to its steps, stages and alternatives), and child-diagram references that point at a diagram of the right kind for the right folder, use-case alternatives that branch from a step that exists, and `relatedViews` that point at a view of the kind and folder they claim. If the repository has an `Actors/` folder it checks the actors too: every `actor` field in a diagram, every `actorDocument` in a behavior view and every `Actors/<name>.md` link in a Markdown file lands on a file, and every file in `Actors/` is linked from at least one other file. It needs the `jsonschema` package. Knowledge files are left to [`../Knowledge/validate_knowledge.py`](../Knowledge/validate_knowledge.py). Schema validation shows a diagram is well-formed, not that it is true; truth is checked by an agent reading the sources the diagram cites.

## The `.dev/` convention, and `Actors/`

Architecture is documented next to the code it describes. Every source folder with something architectural to say gets a `.dev/` directory holding JSON views that follow the schemas here: diagrams (component, class, state, deployment) and behavior views (use case, user journey, capability, feature, operation), which are written rather than drawn and which link to each other through `relatedViews`. The same `.dev/` directories hold the folder's [knowledge files](../Knowledge/README.md). The [Cyberspace explorer](https://github.com/david-brandt-the-icelander/cyberspace-explorer) shows both. Diagrams are drawn bottom up: a folder's component diagram lists its direct children and references their diagrams by repository-relative path, so the folder tree is the component tree. Each diagram records the source paths and symbols it was derived from, so a reviewer can verify it. Diagrams change in the same commit as the code and are deleted, not kept, when they stop applying. The rules in the skills are mostly about what not to draw: no invented transitions, servers or ports, no component per namespace, no method bodies, credentials or configuration values, and no running the application to document it.

Actors are the one thing documented at the repository root rather than next to the code, because an actor is not next to any code: the same resident appears in several folders' diagrams and several journeys. `Actors/` holds one Markdown file per actor named anywhere in the repository, and whatever names an actor links to its file: a use case or user journey through `actorDocument` and `supportingActors`, a `client` or `external-service` node in a deployment diagram and an external component in a component diagram, through their `actor` field. An actor file says who they are, what they want and how they reach the system, and links to the views that serve them rather than restating them. An actor nothing references is deleted, not kept.

## Installing in a repository

1. Copy the skill folders and `validate_diagrams.py` to `Meta/Agents/Skills/` in the target repository, and [`../Knowledge/`](../Knowledge/README.md) to `Meta/Agents/Knowledge/`. The skills refer to those paths; each validator finds its schemas next to itself and scans the whole repository for `.dev/*.json`.
2. Add to the top-level `AGENTS.md` a concise statement of when to use the skills, for example:

   ```markdown
   ## Architecture diagrams

   Architecture is documented per folder in `.dev/` directories as JSON diagrams, using the skills
   under `Meta/Agents/Skills/`. Use `describe-component-diagram` when a folder is a cohesive
   capability, adapter, boundary or subsystem; `describe-class-diagram` when a folder declares more
   than one class; `describe-state-diagram` when a folder implements a state machine, one file per
   machine named after its class; and `describe-deployment-diagram` when a folder contains an
   independently deployable executable, service, container or package. Use `describe-use-case`,
   `describe-user-journey`, `describe-capability`, `describe-feature` and `describe-operation` for
   what the folder does, offers and exposes, linking them to each other with `relatedViews`. Every
   actor a use case, journey or diagram names has a file in `Actors/`, written with
   `describe-actor` and linked from whatever names it. Record intent, wanted work, open decisions,
   blockers, contracts, checks, settings, signals, procedures and limits as knowledge files with
   `describe-knowledge`; use `populate-dev` to decide what a folder needs. Read the relevant
   `.dev` files and actors before changing a folder, update them in the same commit as the code,
   and run `python3 Meta/Agents/Skills/validate_diagrams.py` and
   `python3 Meta/Agents/Knowledge/validate_knowledge.py` before finishing.
   ```
3. Make `CLAUDE.md` a pointer to `AGENTS.md`, so Claude and Codex read one set of instructions:

   ```markdown
   Read `AGENTS.md`. It is the single set of instructions for every agent working in this repository.
   ```

4. Run the validators from the repository root:

   ```bash
   python3 Meta/Agents/Skills/validate_diagrams.py
   python3 Meta/Agents/Knowledge/validate_knowledge.py
   ```

## Writing a new skill

One folder, one `skill.md`, one job. Say when to use it, what goes in, what stays out, and how to check the result. Where it produces a JSON file, add `schema.json` and an `example.json` that says in its own description that it is fictional; where it produces a Markdown file, a `template.md` and an `example.md` that says the same, so an agent does not take it as evidence about the system in front of it. Keep it to a page: skills are read on every task they apply to.
