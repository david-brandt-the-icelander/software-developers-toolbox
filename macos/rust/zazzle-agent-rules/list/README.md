# zazzle agent get-rules

Prints the rule and skill files that apply to a path, so an agent working anywhere on the machine can ask what to follow.

```bash
zazzle agent get-rules PATH [--purpose NAME]... [--name-only] [--skill-bodies]
```

PATH may be a file or a folder; it must exist (exit 66 otherwise). Output is each file as a `==> rule FILE <==` header line, its text, then a blank line (the `head` multi-file format), farthest folder first and closest last (the closest wins). A skill prints only its front-matter `description:` line; `--skill-bodies` prints whole skill files, and `--name-only` prints just one `rule FILE` or `skill FILE` line per file.

Looked up in every folder from `/` down to PATH's folder, plus the home folder (first, unless PATH is under it):
- rules: `AGENTS.md`, `CLAUDE.md`, `.claude/CLAUDE.md`, `.codex/AGENTS.md`, `.claude/rules/*.md`
- skills: `agents/skills/*/SKILL.md`, `.agents/skills/*/SKILL.md`, `.claude/skills/*/SKILL.md`

A file reached twice (a symlink) is printed once, and a file whose only lines are `@file` imports of listed files (a `CLAUDE.md` holding `@AGENTS.md`) is left out.

The binary is `zazzle-agent`, run by `zazzle agent ...`. Test: `cargo test -p zazzle-agent-rules-list`.

## Purposes
`--purpose NAME` (or `--purpose=NAME`, repeatable; the name is letters, digits, `_`, `.` and `-`) keeps only what is marked for those purposes. A marker is on its own line or after a comment opener (`<!--`, `//`, `/*`, `--`, `#`, `;`, `%`), so text that merely quotes one is ignored:
- `#zazzle-purpose:NAME`: the whole file is for NAME.
- `#zazzle-purpose-begin:NAME` ... `#zazzle-purpose-end:NAME`: the lines between are for NAME (an unclosed begin runs to the end of the file).

With purposes given:
- a file with no markers is kept whole;
- a file with a file-level marker for a wanted purpose is kept whole, even if it also has segments for it;
- otherwise the wanted segments print as `rule FILE:A-B` (with `--name-only`) or as a `==> rule FILE:A-B <==` block holding only those lines, without marker lines (1-based line ranges, merged, readable with `zazzle read FILE:A-B`);
- a file whose markers are all for other purposes is dropped.
