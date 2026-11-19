# zazzle (invoker)

The launcher: the only zazzle command on PATH, the client of the daemon (docs: `src/macos/daemon/README.md`), and the way to run every other tool (`zazzle TOOL ARGS`). A freestanding C program: on Linux it uses no libc at all (raw syscalls for x86_64 and aarch64); on macOS arm64 it links libSystem, because the kernel kills a Mach-O without dyld, but it still uses raw BSD syscalls for all I/O.

## What it does
- Builds the socket path from `$ZAZZLE_SOCK` or `$HOME/.zazzle/sock` and connects; checks the daemon runs as the same user.
- Finds the tools' folder from its own real path (symbolic links resolved, so Homebrew's `bin/zazzle` and `bin/z` lead to the keg): `../libexec/zazzle/` beside its `bin/` in an install, else its own folder (a build).
- If no daemon is listening it starts one (`$ZAZZLE_DAEMON`, else `daemon` in the tools' folder) with standard streams on `/dev/null`, detached, and retries with backoff for up to 3 s. On macOS with `zazzle daemon install` launchd owns the socket and starts the daemon on demand.
- Looks at its own name: as `zazzle` or its short name `z` it sends its arguments as they are; as `zazz` it prefixes them with `zazz`; under any other name (an installed script's shim) it sends `zazz exec-name NAME ARGS`. `free-disk-space`, `fix-network` and `git` are exec'd only as `zazzle` or `z`. So no zazz script can be named `z`.
- Sends one message: `cwd(empty) NUL session NUL args... NUL` with SCM_RIGHTS fds 0, 1, 2 and an `open(".")` directory fd. The daemon writes straight into the caller's stdout/stderr and answers with one status byte, which becomes the exit code. A daemon retiring because an upgrade replaced its binary answers instead with the byte 0xFE followed by `daemon-restart`; the invoker then reconnects (starting the new daemon) and sends the request once more. Anything short of the full tag, or a second restart reply, is returned as status 254. `ZAZZLE_SESSION` is the session key.
- `zazzle TOOL ARGS` skips the daemon for the tools: `daemon`, `fix-network`, `free-disk-space`, `git-tools` (also as `zazzle git`), `ai-agent-rules`, `csharp-indexer` and `cyberspace-generator`. The launcher `exec`s that program from the tools' folder with `ARGS`, so the exit status and streams are the tool's own; git-tools needs the caller's own terminal. Nothing stays resident; `zazzle free-disk-space daemon install` still registers its own launchd login item for scheduled cleaning (the plist names free-disk-space's upgrade-proof path, not `zazzle`).
- `zazzle agent llm|prompt|chat|status|prune ARGS` and `zazzle ai-agent ARGS` skip the daemon request too: they `exec` `daemon llm llm|prompt|chat|status|prune ARGS` (`daemon llm ARGS` for `ai-agent`), which needs the caller's terminal. `llm`, `status` and `prune` download `ai-agent` on first use and exec it (`src/macos/ai-agent`); `prompt` and `chat` are clients of this terminal's chat in the daemon, which keeps the model loaded. Other `zazzle agent` commands go to the daemon.
- `zazzle proxy ARGS`, `zazzle setup ARGS` and `zazzle run ARGS` also skip the daemon: they `exec` `daemon proxy ARGS`, `daemon setup ARGS` and `daemon wrap ARGS` (the daemon binary chosen as above), with the caller's environment, streams and exit status ([why](../daemon/src/.dev/decisions.json)).
- Waits for that byte by polling without sleeping for up to 25 µs (timed with `CNTVCT_EL0` on arm64, `clock_gettime` on x86_64), then blocks ([why](src/.dev/decisions.json)). Measured on a Linux VM: fast replies (about 10–15 µs of daemon work for `read`) skip a sleep/wake-up that costs about 25 µs per call; slow commands lose at most 25 µs of CPU.

## Build and test
- `make` builds `build/zazzle`, `build/exit0` (startup floor), `build/hello_libc`, `build/spawnbench`. Build the daemon first: `cd .. && cargo build --release -p daemon`; `test/smoke.sh` takes another daemon from `$DAEMON`.
- `make unit` runs the host unit tests of the request logic against a mock syscall layer (`test/sys_mock.h`); `make coverage` fails below 95% gcov branch coverage of `src/zazzle_core.h` (currently 100%).
- `sh test/smoke.sh` runs the invoker against the real daemon: output, exit status, errors, session dedupe, stdin, write confinement and autostart.
- `sh bench/run.sh [N]` prints spawn latency in microseconds against `grep`, `cat` and a libc hello.
- `sh bench/refs.sh [COLD WARM]` times `refs Error` and `refs Result` over the cargo registry's crate sources: cold (a fresh `zazzle daemon run` per run, so every file is parsed) and warm (through a running daemon).

## Layout
Each folder describes itself in its `.dev/folder.json`: [src](src/.dev/folder.json), [test](test/.dev/folder.json), [bench](bench/.dev/folder.json).

## Why not freestanding on macOS
The decision and its reasons: [.dev/decisions.json](.dev/decisions.json). The macOS floor is about 1.3 ms on a CI runner; the MCP front end (`zazzle daemon mcp`) avoids the spawn entirely.
