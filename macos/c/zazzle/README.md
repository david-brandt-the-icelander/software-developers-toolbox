# zazzle (invoker)

The client half of `zazzle`/`zazzled` (docs: `macos/rust/zazzled/README.md`). A freestanding C program: on Linux it uses no libc at all (raw syscalls for x86_64 and aarch64); on macOS arm64 it links libSystem, because the kernel kills a Mach-O without dyld, but it still uses raw BSD syscalls for all I/O.

## What it does
- Builds the socket path from `$ZAZZLE_SOCK` or `$HOME/.zazzle/sock` and connects; checks the daemon runs as the same user.
- If no daemon is listening it starts one (`$ZAZZLE_DAEMON`, else `zazzled` beside itself) with standard streams on `/dev/null`, detached, and retries with backoff for up to 3 s. On macOS with `zazzled install` launchd owns the socket and starts the daemon on demand.
- Sends one message: `cwd(empty) NUL session NUL args... NUL` with SCM_RIGHTS fds 0, 1, 2 and an `open(".")` directory fd. The daemon writes straight into the caller's stdout/stderr and answers with one status byte, which becomes the exit code. `ZAZZLE_SESSION` is the session key.
- `zazzle free-disk-space ARGS` and `zazzle fix-network ARGS` skip the daemon: the invoker `exec`s that binary from its own directory with `ARGS`, so the exit status and streams are the tool's own. Nothing stays resident; `free-disk-space daemon install` still registers its own launchd login item for scheduled cleaning (the plist stores the real `free-disk-space` path, not `zazzle`).
- Waits for that byte by polling without sleeping for up to 25 µs (timed with `CNTVCT_EL0` on arm64, `clock_gettime` on x86_64), then blocks. Fast replies (about 10–15 µs of daemon work for `read`) then skip the sleep/wake-up, which cost about 25 µs per call on a Linux VM; slow commands lose at most 25 µs of CPU.

## Build and test
- `make` builds `build/zazzle`, `build/exit0` (startup floor), `build/hello_libc`, `build/spawnbench`. Build `zazzled` first: `cd ../../rust && cargo build --release -p zazzled`.
- `make unit` runs the host unit tests of the request logic against a mock syscall layer (`test/sys_mock.h`); `make coverage` fails below 95% gcov branch coverage of `src/zazzle_core.h` (currently 100%).
- `sh test/smoke.sh` runs the invoker against the real daemon: output, exit status, errors, session dedupe, stdin, write confinement and autostart.
- `sh bench/run.sh [N]` prints spawn latency in microseconds against `grep`, `cat` and a libc hello.

## Layout
- `src/zazzle_core.h` the logic; `src/zazzle.c` the entry; `src/entry.h` raw `_start` (Linux) or `main` (macOS); `src/sys*.h` per-platform syscalls and struct layouts.

## Why not freestanding on macOS
A statically linked Mach-O with a valid ad-hoc signature and `LC_UNIXTHREAD` is killed (`Killed: 9`) on exec on macOS 26, and the linker refuses dynamic executables that skip libSystem. The macOS floor is therefore dyld plus libSystem (about 1.3 ms on a CI runner); the MCP front end (`zazzled mcp`) avoids the spawn entirely.
