# AGENTS.md

Personal developer toolbox: small scripts, mostly stdlib Python.

## Rules
- Python: follow `macos/python/style.md`. Always use a venv for projects with dependencies.
- Every code file, class and function has exactly one plain-sentence `#` comment; no docstrings or metadata.
- Write code for low token use and easy agent comprehension.
- Never commit `__pycache__`, `.venv`, or state/log files.
- Editing AGENTS.md or skills: follow `agents/directives.md`.

## Skills
- Free disk space on macOS (compression scan, cache/dev/docker cleanup): `agents/skills/free-disk-space/SKILL.md`
- Diagnose/repair macOS Wi-Fi, DNS, VPN: `agents/skills/fix-network/SKILL.md`

## Layout
- `macos/python/` macOS scripts and Python guidance; `agents/` agent docs and skills.
