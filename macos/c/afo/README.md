# afo (invoker)

The client half of `afo`/`afod` (docs: `macos/rust/afod/README.md`). A freestanding C program: on Linux it uses no libc at all (raw syscalls for x86_64 and aarch64); on macOS arm64 it links libSystem, because the kernel kills a Mach-O without dyld, but it still uses raw BSD syscalls for all I/O.

## What it does
- Builds the socket path from `$AFO_SOCK` or `$HOME/.agent-file-operations/sock` and connects; checks the daemon runs as the same user.
- If no daemon is listening it starts one (`$AFO_DAEMON`, else `afod` beside itself) with standard streams on `/dev/null`, detached, and retries with backoff for up to 3 s. On macOS with `afod install` launchd owns the socket and starts the daemon on demand.
- Sends one message: `cwd(empty) NUL session NUL args... NUL` with SCM_RIGHTS fds 0, 1, 2 and an `open(".")` directory fd. The daemon writes straight into the caller's stdout/stderr and answers with one status byte, which becomes the exit code. `AFO_SESSION` is the session key.
- Waits for that byte by polling without sleeping for up to 25 µs (timed with `CNTVCT_EL0` on arm64, `clock_gettime` on x86_64), then blocks. Fast replies (about 10–15 µs of daemon work for `read`) then skip the sleep/wake-up, which cost about 25 µs per call on a Linux VM; slow commands lose at most 25 µs of CPU.

## Build and test
- `make` builds `build/afo`, `build/exit0` (startup floor), `build/hello_libc`, `build/spawnbench`. Build `afod` first: `cd ../../rust && cargo build --release -p afod`.
- `make unit` runs the host unit tests of the request logic against a mock syscall layer (`test/sys_mock.h`); `make coverage` fails below 95% gcov branch coverage of `src/afo_core.h` (currently 100%).
- `sh test/smoke.sh` runs the invoker against the real daemon: output, exit status, errors, session dedupe, stdin, write confinement and autostart.
- `sh bench/run.sh [N]` prints spawn latency in microseconds against `grep`, `cat` and a libc hello.

## Layout
- `src/afo_core.h` the logic; `src/afo.c` the entry; `src/entry.h` raw `_start` (Linux) or `main` (macOS); `src/sys*.h` per-platform syscalls and struct layouts.

## Why not freestanding on macOS
A statically linked Mach-O with a valid ad-hoc signature and `LC_UNIXTHREAD` is killed (`Killed: 9`) on exec on macOS 26, and the linker refuses dynamic executables that skip libSystem. The macOS floor is therefore dyld plus libSystem (about 1.3 ms on a CI runner); the MCP front end (`afod mcp`) avoids the spawn entirely.
