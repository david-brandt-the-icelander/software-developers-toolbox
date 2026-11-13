---
name: describe-user-journey
description: Create or maintain .dev user-journey.json files when documenting or changing a person's experience over time with the system, across use cases and touchpoints.
---

# User journeys

A user journey is a person's broader experience over time while pursuing a goal, possibly across several use cases, capabilities and touchpoints. Describe them in `<folder>/.dev/user-journey.json`: one entity per journey, with the `persona` (the actor's role), the `goal`, and `stages` in order, each with its `context`, the `touchpoint` (the page, message, device or person the actor meets), the `action` and the `outcome`, and the sources that establish it.

A journey needs evidence of a human experience over time: product requirements, acceptance scenarios, documentation written for the people who use the system, or an implemented experience such as a sequence of pages the code leads a person through. A request pipeline, a polling loop, a machine workflow or a guessed sequence of admin screens is not a journey. Record only the context and outcomes the evidence shows; leave out feelings, durations and research nobody did. It is fine for no folder to have one. Write it in the folder that owns the experience, usually the one that serves the touchpoints, and link the use cases behind its stages with `relatedViews`.

These views overlap and are not a hierarchy. A capability organizes what a folder can do; an operation is one way in; a feature is behavior the product offers; a use case is one actor's goal, step by step; a user journey is a person's experience over time, across use cases. A folder may need several of them, or none.

Read the child folders' views first, then the folder's own source, its public interfaces and the code that wires it together, bottom up, leaving out `.dev`, build output and third-party code. Write the view in the folder that owns the behavior, not in the nearest ancestor. A grouping folder gets a view only when its own code establishes it. Keep the architecture diagrams; these views complement them. If nothing establishes this kind, write nothing: no placeholder files.

Write following [schema.json](schema.json); [example.json](example.json) is fictional and is not evidence about this system. A file may hold several entities, with IDs unique in the file. Every entity cites the files and exact symbols that establish it, and so does every step, stage and alternative that has them. `$schema` is the path from the file to the schema; every other path is relative to the repository root. A related view must exist, be of the kind and folder it claims, and be related by evidence; it need not be in a child folder. Never copy a child's internals into its parent: link to them.

Run `python3 Meta/Agents/Skills/validate_diagrams.py` from the repository root, with `jsonschema` installed. It checks the schema, unique IDs, that sources and related views exist and agree, and that every `actorDocument` lands on a file in `Actors/`. Then read the claims against the sources: a file that passes is well-formed, not true. No secrets, configuration values or method bodies. Do not run the system or its infrastructure to document it. Update the view in the same commit as the code, and delete it when it stops applying.

## Actors

Every entity names its primary actor in `actorDocument`, the path of its file `Actors/<name>.md`, written with the [describe-actor](../describe-actor/skill.md) skill. Other people and outside systems that take part are `supportingActors`, each with its file. Keep the actor's role as the code knows it; whether they are signed in, or allowed, is a precondition, not a different actor. The folder's own code, pages and services are the system, never actors. When a use case or journey is added or deleted, add or remove the link to it in the actor's file.
