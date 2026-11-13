---
name: cyberspace-generator
description: Validate and maintain a repository's .dev files (diagrams, behavior views, knowledge files, Actors/) with the `cyberspace-generator` tool instead of the Python validators. Use before committing any change to code or .dev files in a repository that has .dev folders and Meta/Agents.
---

# cyberspace-generator

Installed with zazzle. Docs: https://github.com/david-brandt-the-icelander/cyberspace-explorer#cyberspace-generator

## Use (from the repository root)
- `cyberspace-generator stale origin/main`: lists `.dev` files that cite files you changed; read and update each one in the same commit as the code.
- `cyberspace-generator validate`: replaces `validate_diagrams.py` and `validate_knowledge.py`; exit 0 means no problems. Add `--json` for a machine-readable report, `--meta DIR` if the skills and catalogue are not in `Meta/Agents`.
- `cyberspace-generator scan`: lists each folder's `.dev` kinds and `.dev` folders without `folder.json`.

## Rules
- Fix every problem `validate` prints before finishing; each line is `file: problem`.
- Passing means well-formed, not true: check claims against the sources.
