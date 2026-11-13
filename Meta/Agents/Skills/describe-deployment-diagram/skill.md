---
name: describe-deployment-diagram
description: Create or maintain .dev/deployment-diagram.json files when documenting or changing a folder's deployment architecture.
---

# Deployment diagrams

Create a diagram only where the folder contains a deployable executable, service, container or package with independent deployment. Verify the project entrypoint, project file and repository deployment instructions. Show artifacts, runtime hosts, clients and actual external infrastructure with conditional dependencies labeled explicitly. A `client` or `external-service` node that is an actor (a person in a role, or an external system with goals of its own toward this one) carries `actor`, the path of its file `Actors/<name>.md`, written with the `describe-actor` skill. Distinguish in-process libraries from separate processes. Use logical host names if physical topology is unspecified. Include protocols when supported by source, but do not invent servers, addresses, ports, replicas or deployment guarantees. Do not execute deployment commands or start infrastructure to document it.

Work bottom up through source folders. Read existing diagrams and relevant code before editing. Write `deployment-diagram.json` under the owning folder's `.dev` directory, following [schema.json](schema.json). Use [example.json](example.json) for a complete fictional example, not as evidence about this service. Set `$schema` to the relative path from the diagram file to this skill's schema. All other paths are relative to the repository root, and `folder` identifies the owning source folder. Keep stable IDs and concise descriptions. Record source paths and symbols sufficient to verify the model. Avoid secrets and incidental runtime values.

Validate against the schema with a JSON Schema Draft 2020-12 validator, then check source paths, unique IDs, relationship endpoints and any child-diagram references. Run `python3 Meta/Agents/Skills/validate_diagrams.py` from the repository root in an environment with `jsonschema` installed; it also checks IDs and file references. Schema validation alone does not establish architectural accuracy. Reconcile diagrams after relevant code changes and remove stale diagrams when their applicability changes. Do not run the application or infrastructure merely to document it.
