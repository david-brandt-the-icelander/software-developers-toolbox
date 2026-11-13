---
name: describe-feature
description: Create or maintain .dev feature.json files when documenting or changing the behavior a folder offers, its outcome and its limits.
---

# Features

A feature is a distinct behavior or facility the product offers; it may use several operations and span capabilities. Describe them in `<folder>/.dev/feature.json`: one entity per feature, with the `behavior` in the present tense, the `outcome` for the person or system that uses it, the `constraints` that limit it (quotas, ordering, retries, who is excluded; an empty list when the code has none) and where it is `implementation`, as source and symbol: the orchestration, configuration and entrypoints that carry it out.

A command name, a persistence implementation, a transport adapter or a group of data types is not a feature on its own. State only what the code does today. A feature somebody wants and the code does not have is a `backlog` item (see `../../Knowledge/README.md`), never an entity here. Link the capability it belongs to, the operations it calls and the use cases that exercise it with `relatedViews`, rather than repeating their contents.

These views overlap and are not a hierarchy. A capability organizes what a folder can do; an operation is one way in; a feature is behavior the product offers; a use case is one actor's goal, step by step; a user journey is a person's experience over time, across use cases. A folder may need several of them, or none.

Read the child folders' views first, then the folder's own source, its public interfaces and the code that wires it together, bottom up, leaving out `.dev`, build output and third-party code. Write the view in the folder that owns the behavior, not in the nearest ancestor. A grouping folder gets a view only when its own code establishes it. Keep the architecture diagrams; these views complement them. If nothing establishes this kind, write nothing: no placeholder files.

Write following [schema.json](schema.json); [example.json](example.json) is fictional and is not evidence about this system. A file may hold several entities, with IDs unique in the file. Every entity cites the files and exact symbols that establish it, and so does every step, stage and alternative that has them. `$schema` is the path from the file to the schema; every other path is relative to the repository root. A related view must exist, be of the kind and folder it claims, and be related by evidence; it need not be in a child folder. Never copy a child's internals into its parent: link to them.

Run `python3 Meta/Agents/Skills/validate_diagrams.py` from the repository root, with `jsonschema` installed. It checks the schema, unique IDs, that sources and related views exist and agree, and that every `actorDocument` lands on a file in `Actors/`. Then read the claims against the sources: a file that passes is well-formed, not true. No secrets, configuration values or method bodies. Do not run the system or its infrastructure to document it. Update the view in the same commit as the code, and delete it when it stops applying.
