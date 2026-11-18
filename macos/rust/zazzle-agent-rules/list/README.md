# zazzle agent get-rules

Prints the rule and skill files (names and text) that apply to a path, so an agent working anywhere on the machine can ask what to follow.

```bash
zazzle agent get-rules PATH [--purpose NAME]... [--name-only]
```

Output is each file as a `==> rule FILE <==` (or `skill`) header line, its text, then a blank line (the `head` multi-file format),  farthest folder first and closest last (the closest wins). PATH may be a file or a folder; it must exist (exit 66 otherwise).

Looked up in the home folder, then in every folder from `/` down to PATH's folder:
- rules: `AGENTS.md`, `CLAUDE.md`, `.claude/CLAUDE.md`, `.codex/AGENTS.md`, `.claude/rules/*.md`
- skills: `agents/skills/*/SKILL.md`, `.agents/skills/*/SKILL.md`, `.claude/skills/*/SKILL.md`

The binary is `zazzle-agent`, run by `zazzle agent ...`. Test: `cargo test -p zazzle-agent-rules-list`.

## Purposes
`--name-only` prints just one `rule FILE` or `skill FILE` line per file instead.

`--purpose NAME` (or `--purpose=NAME`, repeatable) keeps only what is marked for those purposes. Markers go on any line of a rule or skill file, for example in a Markdown comment:
- `#zazzle-purpose:NAME`: the whole file is for NAME.
- `#zazzle-purpose-begin:NAME` ... `#zazzle-purpose-end:NAME`: the lines between are for NAME (an unclosed begin runs to the end of the file).

With purposes given:
- a file with no markers is kept whole;
- a file with a file-level marker for a wanted purpose is kept whole, even if it also has segments for it;
- otherwise the wanted segments print as `rule FILE:A-B` (with `--name-only`) or as a `==> rule FILE:A-B <==` block holding only those lines (1-based line ranges, merged, readable with `zazzle read FILE:A-B`);
- a file whose markers are all for other purposes is dropped.
