# zazzle agent list-rules

Prints the rule and skill files that apply to a path, so an agent working anywhere on the machine can ask what to follow.

```bash
zazzle agent list-rules PATH
```

Output is one `rule FILE` or `skill FILE` per line, farthest folder first and closest last (the closest wins). PATH may be a file or a folder; it must exist (exit 66 otherwise).

Looked up in the home folder, then in every folder from `/` down to PATH's folder:
- rules: `AGENTS.md`, `CLAUDE.md`, `.claude/CLAUDE.md`, `.codex/AGENTS.md`, `.claude/rules/*.md`
- skills: `agents/skills/*/SKILL.md`, `.agents/skills/*/SKILL.md`, `.claude/skills/*/SKILL.md`

The binary is `zazzle-agent`, run by `zazzle agent ...`. Test: `cargo test -p zazzle-agent-rules-list`.
