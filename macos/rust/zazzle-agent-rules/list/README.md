# zazzle agent get-rules

Prints the rule and skill files that apply to a path, so an agent working anywhere on the machine can ask what to follow.

```bash
zazzle agent get-rules PATH [--purpose NAME]... [--name-only] [--skill-bodies]
```

PATH may be a file or a folder; it must exist (exit 66 otherwise). A symlinked file gets the rules of the folder it sits in, not of its target. `--help` (also after `get-rules`) prints the usage. Output is each file as a `==> rule FILE <==` header line, its text, then a blank line (the `head` multi-file format), farthest folder first and closest last (the closest wins). A skill prints only its front-matter `description:` (a folded or multi-line one joined onto one line); `--skill-bodies` prints whole skill files, and `--name-only` prints just one `rule FILE`, `import FILE` or `skill FILE` line per file.

Looked up in every folder from `/` down to PATH's folder, plus the home folder (first, unless PATH is under it):
- rules: `AGENTS.md`, `AGENTS.override.md`, `CLAUDE.md`, `CLAUDE.local.md`, `.claude/CLAUDE.md`, `.codex/AGENTS.md`, `.codex/AGENTS.override.md`, and every `.md` under `.claude/rules/` (subfolders too, up to 8 deep); a later file in the same folder wins over an earlier one
- skills: `agents/skills/*/SKILL.md`, `.agents/skills/*/SKILL.md`, `.claude/skills/*/SKILL.md`

A file reached twice (a symlink) is printed once, and a file whose only lines are `@file` imports of listed files (a `CLAUDE.md` holding `@AGENTS.md`) is left out.

A `.claude/rules` file whose front matter has `paths:` globs (`*`, `?`, `**`, `{a,b}`; inline, `[a, b]` or `- item` lines) applies only when PATH matches one, relative to the folder holding `.claude/` (from `/` when PATH is elsewhere); a folder matches when files under it could.

Files a rule imports with `@path` (relative to the rule, `~/` for home, or absolute; not inside code) print right after it as `==> import FILE <==`, each once, nested up to 5 deep, and narrowed by `--purpose` like the rest.

The binary is `zazzle-agent`, run by `zazzle agent ...`. Test: `cargo test -p zazzle-agent-rules-list`.

## Purposes
`--purpose NAME` (or `--purpose=NAME`, repeatable; the name is letters, digits, `_`, `.` and `-`, starting with a letter or digit and not ending in `-`, else exit 2) keeps only what is marked for those purposes. A marker is on its own line or after a comment opener (`<!--`, `//`, `/*`, `--`, `#`, `;`, `%`), so text that merely quotes one is ignored:
- `#zazzle-purpose:NAME`: the whole file is for NAME.
- `#zazzle-purpose-begin:NAME` ... `#zazzle-purpose-end:NAME`: the lines between are for NAME (an unclosed begin runs to the end of the file).

With purposes given:
- a file with no markers is kept whole;
- a file with a file-level marker for a wanted purpose is kept whole, even if it also has segments for it;
- otherwise the wanted segments print as `rule FILE:A-B` (with `--name-only`) or as a `==> rule FILE:A-B <==` block holding only those lines, without marker lines (1-based line ranges, merged, readable with `zazzle read FILE:A-B`);
- a file whose markers are all for other purposes is dropped.
