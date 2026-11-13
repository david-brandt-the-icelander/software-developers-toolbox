# Developer

**Kind:** person in a role
**Evidence:** `README.md` (`Tools (macOS, Rust)`: install with `brew install zazzcat/z/zazzle`, the release tarball or `cargo build`), `macos/rust/fix-network/src/main.rs`, `macos/rust/free-disk-space/src/main.rs`, `macos/rust/agent-fs/src/main.rs` (the command lines a person runs in a terminal), `macos/c/zazzle/src/zazzle_core.h` (`is_tool`: `zazzle free-disk-space` and `zazzle fix-network` start those tools), `macos/rust/zazzled/src/launchd.rs` (`zazzled install` for the user's LaunchAgent), `macos/dotnet/zazzle-csharp-indexer/src/Cli/Init.cs` and `Report.cs` (`zazzle-csharp-indexer init` and `report`, run in a project), `agents/skills/fix-network/SKILL.md` and `agents/skills/free-disk-space/SKILL.md` (`Rules`: privileged steps use the user's own terminal `sudo`; apply only what the user authorized)

## What they want from the system

- To learn which layer of their Mac's network is broken, and what fixes it, without changing anything: [fix-network use cases](../macos/rust/fix-network/src/.dev/use-case.json), `diagnose-network`.
- To get the network working again with as little disruption as possible, and see that it does: [fix-network use cases](../macos/rust/fix-network/src/.dev/use-case.json), `repair-network`.
- To get disk space back without losing anything they need, and not run out again: [free-disk-space journey](../macos/rust/free-disk-space/.dev/user-journey.json), `reclaim-and-keep-space`; [use cases](../macos/rust/free-disk-space/src/.dev/use-case.json), `clean-up`, `compress-files`, `dedupe-files`, `keep-space-free`.
- To decide what a coding agent may change on their Mac when it repairs the network or frees space for them: [fix-network skill](../agents/skills/fix-network/.dev/use-case.json), `fix-the-developers-network`; [free-disk-space skill](../agents/skills/free-disk-space/.dev/use-case.json), `free-space-for-developer`.
- To read, search, list or outline files as JSON from a shell or a script: [agent-fs use cases](../macos/rust/agent-fs/src/.dev/use-case.json), `one-shot-request`.
- To have the zazzled daemon there whenever something connects, without starting it themselves: [zazzled use cases](../macos/rust/zazzled/src/.dev/use-case.json), `install-launch-agent`.
- To have their coding agent use zazzle for files and zazzle-csharp-indexer for C# structure in a project, and see from real use what to improve: [indexer CLI use cases](../macos/dotnet/zazzle-csharp-indexer/src/Cli/.dev/use-case.json), `wire-into-project`, `review-usage`.

## How they reach it

A terminal on their own Mac, or a Linux machine for `zazzle`, `zazzled`, `zazzle-csharp-indexer` and `agent-fs`, with the binaries installed from the `zazzle` Homebrew formula, the release tarballs or a local build. `free-disk-space` and `fix-network` also run as `zazzle free-disk-space ...` and `zazzle fix-network ...`. `free-disk-space daemon install` and `zazzled install` leave a launchd agent running on their behalf.

## Not to be confused with

- The [Coding agent](coding-agent.md), which runs the same binaries in the developer's session and acts only on what the developer authorizes. The tools themselves do not tell them apart: the difference is in the skills' rules.
- The [Owner](owner.md), who releases the tools. The owner is a developer too.
