# ai-agent-rules

Prints the rule and skill files that apply to a path, so an agent working anywhere on the machine can ask what to follow; checks those files for broken references; and explains why each one applies or not.

```bash
zazzle agent get-rules PATH [--purpose NAME]... [--name-only] [--skill-bodies]
zazzle ai-agent-rules check [PATH] [--json]
zazzle ai-agent-rules explain PATH [--purpose NAME]... [--json]
```

PATH may be a file or a folder; it must exist (exit 66 otherwise). A symlinked file gets the rules of the folder it sits in, not of its target. `--help` (also after `get-rules`) prints the usage. Output is each file as a `==> rule FILE <==` header line, its text, then a blank line (the `head` multi-file format), farthest folder first and closest last (the closest wins). A skill prints only its front-matter `description:` (a folded or multi-line one joined onto one line); `--skill-bodies` prints whole skill files, and `--name-only` prints just one `rule FILE`, `import FILE` or `skill FILE` line per file.

Looked up in every folder from `/` down to PATH's folder, plus the home folder (first, unless PATH is under it):
- rules: `AGENTS.md`, `AGENTS.override.md`, `CLAUDE.md`, `CLAUDE.local.md`, `.claude/CLAUDE.md`, `.codex/AGENTS.md`, `.codex/AGENTS.override.md`, and every `.md` under `.claude/rules/` (subfolders too, up to 8 deep); a later file in the same folder wins over an earlier one
- skills: `agents/skills/*/SKILL.md`, `.agents/skills/*/SKILL.md`, `.claude/skills/*/SKILL.md`

A file reached twice (a symlink) is printed once, and a file whose only lines are `@file` imports of listed files (a `CLAUDE.md` holding `@AGENTS.md`) is left out.

A `.claude/rules` file whose front matter has `paths:` globs (`*`, `?`, `**`, `{a,b}`; inline, `[a, b]` or `- item` lines) applies only when PATH matches one, relative to the folder holding `.claude/` (from `/` when PATH is elsewhere); a folder matches when files under it could.

Files a rule imports with `@path` (relative to the rule, `~/` for home, or absolute; not inside code) print right after it as `==> import FILE <==`, each once, nested up to 5 deep, and narrowed by `--purpose` like the rest.

The binary is `ai-agent-rules`: `zazzle ai-agent-rules ARGS` runs it in the caller's folder; `zazzle agent ARGS` goes to the daemon, which links this crate (give `check` and `explain` an absolute PATH there). `--version` prints the version. Test: `cargo test -p ai-agent-rules`; coverage: `cargo +nightly llvm-cov --branch -p ai-agent-rules --summary-only` (CI requires 95% of lines and branches).

## check
`check [PATH]` (default `.`) reads the files that apply at PATH, and when PATH is a folder also those in every folder below it (not hidden folders, `node_modules` or symlinks), and prints one `FILE:LINE: message` line per problem:
- an `@path` import (outside code, containing `.` or `/`, so `@someone` is a mention) that points to no file;
- a `paths:` glob of a `.claude/rules` file that matches no file or folder under the folder holding `.claude/`; only rules inside the repository (the nearest folder above PATH holding `.git`, or PATH) are checked, so home rules are not;
- a skill whose front matter has no `name` or `description`, or whose `name` is not its folder's name.

Exit 1 with problems (printed to standard output), 0 when clean (`no problems in N files`), 66 when PATH does not exist. `--json` prints `[{"file","line","kind","message"}, ...]` with `kind` `import`, `paths` or `skill`.

## explain
`explain PATH [--purpose NAME]...` prints one line per file found for PATH, `applies KIND FILE: why` or `skipped KIND FILE: why`:
- applies: `in DIR (the home folder | the path itself | the path's folder | N folders up)`, plus `paths: `GLOB` matches` for a scoped rule; `imported by FILE` for an import; ` (lines A-B)` after FILE when only marked segments apply;
- skipped: a scoped rule whose globs all miss (`but no paths: glob matches (...)`), a file or import `marked only for other purposes`, `the same file as FILE` (a symlink) and a file that `only imports files that are listed`.

`--json` prints `[{"file","kind","applies","reason","folder","glob","importedBy","lines"}, ...]`, with null for fields that do not fit the reason and `lines` as `[[A,B], ...]`.

## Purposes
`--purpose NAME` (or `--purpose=NAME`, repeatable; the name is letters, digits, `_`, `.` and `-`, starting with a letter or digit and not ending in `-`, else exit 2) keeps only what is marked for those purposes. A marker is on its own line or after a comment opener (`<!--`, `//`, `/*`, `--`, `#`, `;`, `%`), so text that merely quotes one is ignored:
- `#zazzle-purpose:NAME`: the whole file is for NAME.
- `#zazzle-purpose-begin:NAME` ... `#zazzle-purpose-end:NAME`: the lines between are for NAME (an unclosed begin runs to the end of the file).

With purposes given:
- a file with no markers is kept whole;
- a file with a file-level marker for a wanted purpose is kept whole, even if it also has segments for it;
- otherwise the wanted segments print as `rule FILE:A-B` (with `--name-only`) or as a `==> rule FILE:A-B <==` block holding only those lines, without marker lines (1-based line ranges, merged, readable with `zazzle read FILE:A-B`);
- a file whose markers are all for other purposes is dropped.
