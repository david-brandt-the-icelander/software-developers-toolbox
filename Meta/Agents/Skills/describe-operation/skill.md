---
name: describe-operation
description: Create or maintain .dev operation.json files when documenting or changing a folder's callable entrypoints: routes, commands, handlers, messages and scheduled jobs.
---

# Operations

An operation is one callable action: a command that changes something, or a query that reads something, within a capability. Describe them in `<folder>/.dev/operation.json`: one entity per action, with its real `entrypoint` as source and symbol, whether it is a `command` or a `query` (`action`), how a caller reaches it (`transport`, such as `POST /api/scores`, an exported function, a message or a scheduled job), the `authorization` the code checks, the `result`, and the `failures` the code has, each a condition and what the caller sees.

Judge command or query by the effect, not by the method name or the HTTP verb: a POST that only reads is a query. Read authorization and failures from the code, not from convention; if nothing checks, say so. `failures` lists the cases found, not every exception that could be thrown, and an empty list means none were catalogued. A folder that hosts several actions has one file with several entities; the folder itself is not an action. Prefer the product's and subsystems' interfaces and scheduled entrypoints; leave out accessors, setup and plumbing. Record the names the code actually uses. Link the capability that offers each operation and the use cases that call it with `relatedViews`.

These views overlap and are not a hierarchy. A capability organizes what a folder can do; an operation is one way in; a feature is behavior the product offers; a use case is one actor's goal, step by step; a user journey is a person's experience over time, across use cases. A folder may need several of them, or none.

Read the child folders' views first, then the folder's own source, its public interfaces and the code that wires it together, bottom up, leaving out `.dev`, build output and third-party code. Write the view in the folder that owns the behavior, not in the nearest ancestor. A grouping folder gets a view only when its own code establishes it. Keep the architecture diagrams; these views complement them. If nothing establishes this kind, write nothing: no placeholder files.

Write following [schema.json](schema.json); [example.json](example.json) is fictional and is not evidence about this system. A file may hold several entities, with IDs unique in the file. Every entity cites the files and exact symbols that establish it, and so does every step, stage and alternative that has them. `$schema` is the path from the file to the schema; every other path is relative to the repository root. A related view must exist, be of the kind and folder it claims, and be related by evidence; it need not be in a child folder. Never copy a child's internals into its parent: link to them.

Run `python3 Meta/Agents/Skills/validate_diagrams.py` from the repository root, with `jsonschema` installed. It checks the schema, unique IDs, that sources and related views exist and agree, and that every `actorDocument` lands on a file in `Actors/`. Then read the claims against the sources: a file that passes is well-formed, not true. No secrets, configuration values or method bodies. Do not run the system or its infrastructure to document it. Update the view in the same commit as the code, and delete it when it stops applying.
