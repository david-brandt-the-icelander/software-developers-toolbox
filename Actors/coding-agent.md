# Coding agent

**Kind:** agent
**Evidence:** `agents/skills/zazzle/SKILL.md`, `agents/skills/agent-fs/SKILL.md`, `agents/skills/fix-network/SKILL.md`, `agents/skills/free-disk-space/SKILL.md` (written to an agent), `macos/rust/zazzled/src/mcp.rs`, `macos/rust/zazzled/src/mcp_structure.rs`, `macos/rust/agent-fs/src/mcp.rs` (MCP servers over standard input and output), `macos/rust/zazzled/src/proxy/mod.rs` (`zazzle proxy` in front of another MCP server), `macos/c/zazzle/src/zazzle_core.h` (`ZAZZLE_SESSION`: the session a shell's commands share), `agents/skills/zazz/SKILL.md` and `macos/rust/zazzled/src/zazz/mcptools.rs` (zazz scripts, from a shell or as MCP tools), `macos/dotnet/zazzle-csharp-indexer/src/Cli/HookPolicy.cs` (the Claude Code hook that redirects its raw commands)

## What they want from the system

- To find and read the code it needs, and keep up with changes, without re-reading what it already has: [agent-fs journey](../agents/skills/agent-fs/.dev/user-journey.json), `explore-with-afs`; [agent-fs use cases](../macos/rust/agent-fs/src/.dev/use-case.json), `reread-changed-file`, `find-and-read-item`.
- To search, read, outline and edit a project's files through tools, with short answers it is not sent twice: [zazzled use cases](../macos/rust/zazzled/src/.dev/use-case.json), `use-over-mcp`; from a shell, [zazzle use cases](../macos/c/zazzle/src/.dev/use-case.json), `run-a-command`.
- To change a file it has read without overwriting a newer version or writing outside the project: [zazzled command use cases](../macos/rust/zazzled/src/cmd/.dev/use-case.json), `edit-a-file-safely`.
- To learn whether a build or test passed, and why not, from a short answer: [zazzle run use cases](../macos/rust/zazzled/src/wrap/.dev/use-case.json), `run-a-build-or-test`.
- To get back what a cleaned output cut, without rerunning the command or calling the other server again: [zazzled command use cases](../macos/rust/zazzled/src/cmd/.dev/use-case.json), `get-a-cut-output-in-full`.
- To use another MCP server's tools while spending fewer tokens on their results: [zazzle proxy use cases](../macos/rust/zazzled/src/proxy/.dev/use-case.json), `clean-another-server`.
- To get compact, current answers about the code's structure in its work tree from zazzle, which asks zazzle-csharp-indexer, and be pointed at zazzle when it reaches for grep, cat or a raw read of a C# file: [indexer use cases](../macos/dotnet/zazzle-csharp-indexer/src/.dev/use-case.json), `answer-code-question`; [indexer CLI use cases](../macos/dotnet/zazzle-csharp-indexer/src/Cli/.dev/use-case.json), `redirect-raw-command`.
- To write a small program once, have it built and tested on this machine, and call it by id with JSON in and out, inside a sandbox: [zazz use cases](../macos/rust/zazzled/src/zazz/.dev/use-case.json), `add-and-call-a-script`.
- To repair the developer's Mac network, or free disk space for them, changing only what they authorized and reporting truthfully: [fix-network skill](../agents/skills/fix-network/.dev/use-case.json), `fix-the-developers-network`; [free-disk-space skill](../agents/skills/free-disk-space/.dev/use-case.json), `free-space-for-developer`.

## How they reach it

An MCP client, such as Claude Code, that starts `zazzled mcp`, `zazzle proxy` or `afs serve` (zazzle-csharp-indexer offers it no tools of its own); or a shell in which it runs `zazzle`, `afs`, `fix-network` and `free-disk-space`. The skills in `agents/skills/` and the snippet `zazzle-csharp-indexer init` prints tell it how.

## Not to be confused with

The [Developer](developer.md), the person whose session the agent works in, who authorizes what it changes and types any password. The tools do not tell them apart; the skills' rules do.
