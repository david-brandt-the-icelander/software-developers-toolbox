---
name: zazzle-csharp-minifier
description: Keep C# in the layout `zazzle-csharp-minifier` enforces: comments on files, types and members moved to document/<path>.md, one test file per method under test in <ClassUnderTest>/<MethodUnderTest>.cs, and each .cs split into declarations plus src/<name>.cs implementation. Use when adding, moving or editing C# code or tests, writing a doc comment, or when the pre-commit check refuses a commit.
---

# zazzle-csharp-minifier

Docs: `zazzle-csharp-minifier --help`; source and README in the software-developers-toolbox repository, `macos/dotnet/zazzle-csharp-minifier/`.

## Use
- After editing C#, run the tool with the switches this repository's AGENTS.md names (from the root it names), then commit the result: `zazzle-csharp-minifier --strip-comments --split-tests --split-headers`.
- Before committing, `zazzle-csharp-minifier --check <same switches>` lists every file that breaks the layout and exits 1; the pre-commit hook runs exactly this on staged `.cs` files and refuses the commit until it passes.
- Pass files or folders to limit a run: `zazzle-csharp-minifier --split-headers src/Orders`.
- Never hand-edit the layout back: write code normally, then rerun the tool.

## What each switch does
- `--strip-comments`: comments on the file, namespaces, types, members, enum members and accessors move to `document/<path>.cs.md` (one `##` section per declaration, appended on later runs). Comments inside bodies, initializers and parameter lists stay. Read the document for intent; write new intent there or as a comment the next run moves.
- `--split-tests`: each test moves to `<ClassUnderTest>/<MethodUnderTest>.cs` beside its test file, as a `partial` test class. The method under test is what the test's `// When` section calls (`// When` … `// Then`, or `// Act` … `// Assert`): give every test a `// When` section that calls exactly one member of non-test code.
- A test the tool cannot place keeps `// zazzle-csharp-minifier: unresolved method under test (<reason>)` as its first line: `grep -rn "zazzle-csharp-minifier: unresolved"` finds them; fix the `// When` section and rerun.
- `--split-headers`: each `.cs` keeps the declarations (types, fields, `partial` member signatures) and `src/<same name>.cs` beside it holds the bodies. What can split follows the project's target framework: methods (C# 9+), properties and indexers (C# 13, .NET 9), constructors and events (C# 14, .NET 10). Add new members to the header file with their bodies and rerun; edit existing bodies in `src/`.

## Restructured projects (`--restructure`)
- Each production project becomes `<project>/incl/` (declarations: what to read), `src/` (bodies, top-level programs), `doc/` (moved comments, `doc/test/` for tests) and `test/` (its test project, tests in `<ClassUnderTest>/<MethodUnderTest>.cs`); the `.csproj` compiles only `incl/**` and `src/**`.
- Read `incl/` first and always. Open a file in `src/`, `doc/` or `test/` only by its exact path, when you need that file's bodies, comments or tests; never list or search those folders wholesale. Each project's `.ignore` keeps them out of broad ripgrep searches (Grep, Glob); an explicit path still works.
- Add new code as a plain `.cs` anywhere in the project (or in `incl/` with bodies) and rerun `--restructure`; it splits and places it.

## When the check fails
- Run the command it prints, review the diff, commit.
- A file reported as "left unchanged" needs a manual fix (the reason follows); the tool never changes code it cannot prove unchanged.

## Rename
- Never rename a C# namespace, type or member by hand: run `zazzle-csharp-minifier --list-identifiers > names.txt`, write `=> newName` after each name to change (`Store => Shop:`), delete the other lines if you like, then `zazzle-csharp-minifier --rename names.txt`.
- It renames every reference, moves the folders and files that carry the names (namespace folders, `Type.cs`, test folders and files) and fixes the comment documents; it refuses renames that add a compiler error.
