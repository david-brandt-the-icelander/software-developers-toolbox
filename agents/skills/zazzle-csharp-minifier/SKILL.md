---
name: zazzle-csharp-minifier
description: Keep C# tests in one file per method under test in repositories using zazzle-csharp-minifier, and list or rename C# identifiers.
disable-model-invocation: true
---

# zazzle-csharp-minifier

## Keep the test layout
- Read the repository's marked `AGENTS.md` section for its root and switches.
- After C# edits, run `zazzle-csharp-minifier --split-tests` with the recorded `--root`, then review the diff.
- Each test moves to `<ClassUnderTest>/<MethodUnderTest>.cs` beside its original file, as a partial test class; shared fields and helpers stay in the original file.
- Give every test a `// When` section that calls exactly one member of non-test code; `// Act` through `// Assert` also works.
- Fix tests marked `// zazzle-csharp-minifier: unresolved method under test (<reason>)` by clarifying their section and rerunning.
- The pre-commit hook checks staged C# paths with `--check`; run the command it prints when it refuses a commit.
- A file reported as "left unchanged" needs the manual fix explained by its diagnostic.

## Read code
- Reuse available tools and discover only the tool names needed.
- Read complete relevant symbols or bounded ranges, with enough budget to include the required code.
- Resolve referenced constants, fields and attributes before describing their behavior or wire values.
- For zazzle searches, use `glob: ["*.cs"]` and `max: 20`.

## Rename
- Run `zazzle-csharp-minifier --list-identifiers > names.txt`, put `=> newName` after each name to change, then run `zazzle-csharp-minifier --rename names.txt`.
- Renames update references and move namespace folders, type files and test files that follow the names.
- The tool refuses renames that introduce compiler errors or collide with existing files.
