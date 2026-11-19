# Coding agent

**Kind:** agent
**Evidence:** `agents/skills/zazzle/SKILL.md`, `agents/skills/shell-tools/SKILL.md`, `agents/skills/fix-network/SKILL.md`, `agents/skills/free-disk-space/SKILL.md` (written to an agent), `src/macos/daemon/src/mcp.rs`, `src/macos/daemon/src/mcp_structure.rs`, `src/macos/shell-tools/src/mcp.rs` (MCP servers over standard input and output), `src/macos/daemon/src/proxy/mod.rs` (`zazzle proxy` in front of another MCP server), `src/macos/zazzle/src/zazzle_core.h` (`ZAZZLE_SESSION`: the session a shell's commands share), `agents/skills/zazz/SKILL.md` and `src/macos/daemon/src/zazz/mcptools.rs` (zazz scripts, from a shell or as MCP tools), `src/macos/csharp-indexer/src/Cli/HookPolicy.cs` (the Claude Code hook that redirects its raw commands), `agents/skills/zazzle/SKILL.md` (`Git`: `zazzle git commit -m`, never the editor), `agents/skills/zazzle-agent/SKILL.md` (`Local models: llm and prompt`: `zazzle agent prompt`), `src/macos/daemon/src/setup/mod.rs` (`INSTRUCTIONS`: `llm` on reading tools and `zazzle_brief` before a task)

## What they want from the system

- To find and read the code it needs, and keep up with changes, without re-reading what it already has: [shell-tools journey](../agents/skills/shell-tools/.dev/user-journey.json), `explore-with-shell-tools`; [shell-tools use cases](../src/macos/shell-tools/src/.dev/use-case.json), `reread-changed-file`, `find-and-read-item`.
- To search, read, outline and edit a project's files through tools, with short answers it is not sent twice: [daemon use cases](../src/macos/daemon/src/.dev/use-case.json), `use-over-mcp`; from a shell, [zazzle use cases](../src/macos/zazzle/src/.dev/use-case.json), `run-a-command`.
- To change a file it has read without overwriting a newer version or writing outside the project: [daemon command use cases](../src/macos/daemon/src/cmd/.dev/use-case.json), `edit-a-file-safely`.
- To learn whether a build or test passed, and why not, from a short answer: [zazzle run use cases](../src/macos/daemon/src/wrap/.dev/use-case.json), `run-a-build-or-test`.
- To get back what a cleaned output cut, without rerunning the command or calling the other server again: [daemon command use cases](../src/macos/daemon/src/cmd/.dev/use-case.json), `get-a-cut-output-in-full`.
- To use another MCP server's tools while spending fewer tokens on their results: [zazzle proxy use cases](../src/macos/daemon/src/proxy/.dev/use-case.json), `clean-another-server`.
- To get compact, current answers about the code's structure in its work tree from zazzle, which asks csharp-indexer, and be pointed at zazzle when it reaches for grep, cat or a raw read of a C# file: [indexer use cases](../src/macos/csharp-indexer/src/.dev/use-case.json), `answer-code-question`; [indexer CLI use cases](../src/macos/csharp-indexer/src/Cli/.dev/use-case.json), `redirect-raw-command`.
- To run git and commit with a message in one non-interactive command: [zazzle git use cases](../src/macos/git-tools/src/.dev/use-case.json), `agent-commits-with-message`.
- To write a small program once, have it built and tested on this machine, and call it by id with JSON in and out, inside a sandbox: [zazz use cases](../src/macos/daemon/src/zazz/.dev/use-case.json), `add-and-call-a-script`.
- To get a second opinion, a review or a summary from an open model running on the developer's machine, without the text leaving it: [ai-agent use cases](../src/macos/ai-agent/src/.dev/use-case.json), `ask-a-local-model`.
- To spend fewer tokens by handing work to that local model: the rule sections and skills a task needs, and only the part of an output it asked for: [daemon use cases](../src/macos/daemon/src/.dev/use-case.json), `brief-before-a-task`, `post-process-output`; [ai-agent use cases](../src/macos/ai-agent/src/.dev/use-case.json), `answer-for-zazzle`.
- To repair the developer's Mac network, or free disk space for them, changing only what they authorized and reporting truthfully: [fix-network skill](../agents/skills/fix-network/.dev/use-case.json), `fix-the-developers-network`; [free-disk-space skill](../agents/skills/free-disk-space/.dev/use-case.json), `free-space-for-developer`.

## How they reach it

An MCP client, such as Claude Code, that starts `zazzle daemon mcp`, `zazzle proxy` or `zazzle shell-tools serve` (csharp-indexer offers it no tools of its own); or a shell in which it runs `zazzle` (including `zazzle git`), `shell-tools`, `fix-network` and `free-disk-space`. The skills in `agents/skills/` and the snippet `csharp-indexer init` prints tell it how.

## Not to be confused with

The [Developer](developer.md), the person whose session the agent works in, who authorizes what it changes and types any password. The tools do not tell them apart; the skills' rules do.
