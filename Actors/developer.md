# Developer

**Kind:** person in a role
**Evidence:** `README.md` (`Install`: `brew install zazzcat/z/zazzle`, the release tarball or `cargo build`), `src/macos/fix-network/src/main.rs`, `src/macos/free-disk-space/src/main.rs`, `src/macos/shell-tools/src/main.rs` (the command lines a person runs in a terminal), `src/macos/zazzle/src/zazzle_core.h` (`tool_binary`: `zazzle free-disk-space`, `zazzle fix-network` and `zazzle git` start those tools), `src/macos/git-tools/src/message.rs` (`HELP`: `zazzle git message` as git's core.editor), `src/macos/daemon/src/launchd.rs` (`zazzle daemon install` for the user's LaunchAgent), `src/macos/daemon/src/zazz/install.rs` (`zazzle zazz install` and the shims it makes), `src/macos/ai-agent/src/main.rs` (`zazzle agent llm` lists, downloads and chooses models), `src/macos/csharp-indexer/src/Cli/Init.cs` and `Report.cs` (`csharp-indexer init` and `report`, run in a project), `agents/skills/fix-network/SKILL.md` and `agents/skills/free-disk-space/SKILL.md` (`Rules`: privileged steps use the user's own terminal `sudo`; apply only what the user authorized)

## What they want from the system

- To learn which layer of their Mac's network is broken, and what fixes it, without changing anything: [fix-network use cases](../src/macos/fix-network/src/.dev/use-case.json), `diagnose-network`.
- To get the network working again with as little disruption as possible, and see that it does: [fix-network use cases](../src/macos/fix-network/src/.dev/use-case.json), `repair-network`.
- To get disk space back without losing anything they need, and not run out again: [free-disk-space journey](../src/macos/free-disk-space/.dev/user-journey.json), `reclaim-and-keep-space`; [use cases](../src/macos/free-disk-space/src/.dev/use-case.json), `clean-up`, `compress-files`, `dedupe-files`, `keep-space-free`.
- To decide what a coding agent may change on their Mac when it repairs the network or frees space for them: [fix-network skill](../agents/skills/fix-network/.dev/use-case.json), `fix-the-developers-network`; [free-disk-space skill](../agents/skills/free-disk-space/.dev/use-case.json), `free-space-for-developer`.
- To read, search, list or outline files as JSON from a shell or a script: [shell-tools use cases](../src/macos/shell-tools/src/.dev/use-case.json), `one-shot-request`.
- To have the zazzle daemon there whenever something connects, without starting it themselves: [daemon use cases](../src/macos/daemon/src/.dev/use-case.json), `install-launch-agent`.
- To have their coding agent use zazzle for files and for C# structure, which zazzle gets from csharp-indexer, in a project, and see from real use what to improve: [indexer CLI use cases](../src/macos/csharp-indexer/src/Cli/.dev/use-case.json), `wire-into-project`, `review-usage`.
- To write commit messages, choose the files of a commit and edit rebase todo lists in a small editor without modes, without losing a message when a commit fails: [zazzle git use cases](../src/macos/git-tools/src/.dev/use-case.json), `write-message-as-git-editor`, `commit-with-draft-and-files`, `edit-rebase-todo`.
- To choose which local open model, and how much thinking, answers prompts on their machine: [ai-agent use cases](../src/macos/ai-agent/src/.dev/use-case.json), `choose-a-model`.
- To install a zazz script as an ordinary command, with flags and plain output instead of JSON: [zazz use cases](../src/macos/daemon/src/zazz/.dev/use-case.json), `install-and-run-a-script`.

## How they reach it

A terminal on their own Mac, or a Linux machine for `zazzle`, `daemon`, `csharp-indexer` and `shell-tools`, with the binaries installed from the `zazzle` Homebrew formula, the release tarballs or a local build. `free-disk-space` and `fix-network` also run as `zazzle free-disk-space ...` and `zazzle fix-network ...`, and `git-tools` as `zazzle git ...`; git itself starts the zazzle git editor when it is set as core.editor. `free-disk-space daemon install` and `zazzle daemon install` leave a launchd agent running on their behalf.

## Not to be confused with

- The [Coding agent](coding-agent.md), which runs the same binaries in the developer's session and acts only on what the developer authorizes. The tools themselves do not tell them apart: the difference is in the skills' rules.
- The [Owner](owner.md), who releases the tools. The owner is a developer too.
