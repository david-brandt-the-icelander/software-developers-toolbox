---
name: describe-capability
description: Create or maintain .dev capability.json files when documenting or changing what a folder can do and who may ask for it.
---

# Capabilities

A capability is a coherent area of what the system can do, organized around a domain responsibility, a device or an outside service, with a set of related operations through which it is reached. Describe them in `<folder>/.dev/capability.json`: one entity per capability, named as a noun phrase (`name`), with its `responsibility` in a sentence, who may use it (`access`, one line per rule, as the code enforces it; if nothing checks, say so) and the `operations` through which it is reached, as source and symbol, at the boundary that owns them.

A capability needs both a cohesive responsibility and a callable interface. An adapter, a namespace, a collection of models, a scheduler wrapper or a folder that only wires things together is not one on its own. Do not make one per class, and do not call every method a capability. Link the features that make it up, the operations that expose it and the use cases that use it with `relatedViews`.

These views overlap and are not a hierarchy. A capability organizes what a folder can do; an operation is one way in; a feature is behavior the product offers; a use case is one actor's goal, step by step; a user journey is a person's experience over time, across use cases. A folder may need several of them, or none.

Read the child folders' views first, then the folder's own source, its public interfaces and the code that wires it together, bottom up, leaving out `.dev`, build output and third-party code. Write the view in the folder that owns the behavior, not in the nearest ancestor. A grouping folder gets a view only when its own code establishes it. Keep the architecture diagrams; these views complement them. If nothing establishes this kind, write nothing: no placeholder files.

Write following [schema.json](schema.json); [example.json](example.json) is fictional and is not evidence about this system. A file may hold several entities, with IDs unique in the file. Every entity cites the files and exact symbols that establish it, and so does every step, stage and alternative that has them. `$schema` is the path from the file to the schema; every other path is relative to the repository root. A related view must exist, be of the kind and folder it claims, and be related by evidence; it need not be in a child folder. Never copy a child's internals into its parent: link to them.

Run `python3 Meta/Agents/Skills/validate_diagrams.py` from the repository root, with `jsonschema` installed. It checks the schema, unique IDs, that sources and related views exist and agree, and that every `actorDocument` lands on a file in `Actors/`. Then read the claims against the sources: a file that passes is well-formed, not true. No secrets, configuration values or method bodies. Do not run the system or its infrastructure to document it. Update the view in the same commit as the code, and delete it when it stops applying.
