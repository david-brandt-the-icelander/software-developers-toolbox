---
name: describe-use-case
description: Create or maintain .dev use-case.json files when documenting or changing what an actor does, step by step, with a system or folder.
---

# Use cases

A use case is an actor's goal and the interaction needed to reach it. Describe them in `<folder>/.dev/use-case.json`: one entity per goal an actor pursues through code in that folder, with the actor, the `goal`, the `preconditions` and observable `postconditions`, a `flow` of steps in order, and `alternatives`, each branching off a step (`atStep`) with the condition and the outcome. A step names who acts (`participantKind` is `actor` for a person or outside system, with its `actorDocument`, and `system` for the code), what they do, and the sources it was read from.

A use case needs an identifiable actor, a goal, a flow the code implements, preconditions and postconditions. One create, read, update or delete method is not a use case. Alternatives are the branches the code has: errors, rejections, early exits. Do not invent a step to complete an expected story, and do not invent personas, motives or screens. Steps run by code in another folder are linked through `relatedViews`, not written here. Link the operations the steps call, the capability and the features they serve, and the journeys they are part of.

These views overlap and are not a hierarchy. A capability organizes what a folder can do; an operation is one way in; a feature is behavior the product offers; a use case is one actor's goal, step by step; a user journey is a person's experience over time, across use cases. A folder may need several of them, or none.

Read the child folders' views first, then the folder's own source, its public interfaces and the code that wires it together, bottom up, leaving out `.dev`, build output and third-party code. Write the view in the folder that owns the behavior, not in the nearest ancestor. A grouping folder gets a view only when its own code establishes it. Keep the architecture diagrams; these views complement them. If nothing establishes this kind, write nothing: no placeholder files.

Write following [schema.json](schema.json); [example.json](example.json) is fictional and is not evidence about this system. A file may hold several entities, with IDs unique in the file. Every entity cites the files and exact symbols that establish it, and so does every step, stage and alternative that has them. `$schema` is the path from the file to the schema; every other path is relative to the repository root. A related view must exist, be of the kind and folder it claims, and be related by evidence; it need not be in a child folder. Never copy a child's internals into its parent: link to them.

Run `python3 Meta/Agents/Skills/validate_diagrams.py` from the repository root, with `jsonschema` installed. It checks the schema, unique IDs, that sources and related views exist and agree, and that every `actorDocument` lands on a file in `Actors/`. Then read the claims against the sources: a file that passes is well-formed, not true. No secrets, configuration values or method bodies. Do not run the system or its infrastructure to document it. Update the view in the same commit as the code, and delete it when it stops applying.

## Actors

Every entity names its primary actor in `actorDocument`, the path of its file `Actors/<name>.md`, written with the [describe-actor](../describe-actor/skill.md) skill. Other people and outside systems that take part are `supportingActors`, each with its file. Keep the actor's role as the code knows it; whether they are signed in, or allowed, is a precondition, not a different actor. The folder's own code, pages and services are the system, never actors. When a use case or journey is added or deleted, add or remove the link to it in the actor's file.
