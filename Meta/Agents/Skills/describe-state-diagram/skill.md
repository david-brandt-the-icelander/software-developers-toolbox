---
name: describe-state-diagram
description: Create or maintain named .dev state-diagram.json files when documenting or changing implemented state machines.
---

# State diagrams

Create one file per implemented state machine, preferably named after its owning class. Identify persistent or derived state, initial state, events, guards and actions from executable branches and writes. Model orthogonal lifecycles separately or use descriptive suffixes for multiple machines in a class. Include rejection, retry and self transitions when relevant. An enum, a class named State, CRUD operations, a polling loop or a workflow alone is not proof of a state machine. Do not invent a transition to complete an expected lifecycle. Explain abstractions and the scope of machine state, including externally owned transitions. Every transition endpoint and the initial state must name a declared state.

Work bottom up through source folders. Read existing diagrams and relevant code before editing. Write `<identifier>.state-diagram.json` under the owning folder's `.dev` directory, following [schema.json](schema.json). Use [example.json](example.json) for a complete fictional example, not as evidence about this service. Set `$schema` to the relative path from the diagram file to this skill's schema. All other paths are relative to the repository root, and `folder` identifies the owning source folder. Keep stable IDs and concise descriptions. Record source paths and symbols sufficient to verify the model. Avoid secrets and incidental runtime values.

Validate against the schema with a JSON Schema Draft 2020-12 validator, then check source paths, unique IDs, relationship endpoints and any child-diagram references. Run `python3 Meta/Agents/Skills/validate_diagrams.py` from the repository root in an environment with `jsonschema` installed; it also checks IDs and file references. Schema validation alone does not establish architectural accuracy. Reconcile diagrams after relevant code changes and remove stale diagrams when their applicability changes. Do not run the application or infrastructure merely to document it.
