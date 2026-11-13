---
name: describe-class-diagram
description: Create or maintain .dev/class-diagram.json files when documenting or changing a folder's class architecture.
---

# Class diagrams

Create a diagram when the immediate folder declares more than one distinct class, including record classes, nested classes and partial classes counted once. Include local interfaces, structs, enums and type aliases (kind `type`, for languages such as TypeScript) as context when classes use them. Descendant folders have their own diagrams; do not count their classes toward the parent threshold. Merge partial declarations and distinguish nested and generic types and repeated simple names by qualified IDs. Show meaningful declared member signatures, inheritance, interface implementations and source-evidenced dependencies. External referenced types may be lightweight nodes. Exclude compiler-generated build output; include checked-in migrations. Abstract external framework types may appear as named relation targets, but must have a node. Do not copy method bodies, attributes, credentials or configuration values.

Work bottom up through source folders. Read existing diagrams and relevant code before editing. Write `class-diagram.json` under the owning folder's `.dev` directory, following [schema.json](schema.json). Use [example.json](example.json) for a complete fictional example, not as evidence about this service. Set `$schema` to the relative path from the diagram file to this skill's schema. All other paths are relative to the repository root, and `folder` identifies the owning source folder. Keep stable IDs and concise descriptions. Record source paths and symbols sufficient to verify the model. Avoid secrets and incidental runtime values.

Validate against the schema with a JSON Schema Draft 2020-12 validator, then check source paths, unique IDs, relationship endpoints and any child-diagram references. Run `python3 Meta/Agents/Skills/validate_diagrams.py` from the repository root in an environment with `jsonschema` installed; it also checks IDs and file references. Schema validation alone does not establish architectural accuracy. Reconcile diagrams after relevant code changes and remove stale diagrams when their applicability changes. Do not run the application or infrastructure merely to document it.
