# afo (phase 0 spike)

Freestanding C invoker for the `afod` daemon (`macos/rust/afod/`). Phase 0 only measures round-trip cost; there are no real commands yet.

## What it does
- Connects to `$AFO_SOCK` or `$HOME/.agent-file-operations/sock`, checks the peer uid, and sends argv with a 4-byte length header.
- Default variant passes fds 0, 1, 2 and an `open(".")` fd with SCM_RIGHTS; the daemon writes straight to the caller's stdout and answers with one status byte.
- `afo-relay` sends no fds; the daemon streams the payload back and the invoker copies it to stdout (for comparison).
- The first argument is the payload size in bytes the daemon should emit (default 16).

## Build and test
- `make` builds `build/afo`, `build/afo-relay`, `build/exit0` (startup floor), `build/hello_libc` and `build/spawnbench`.
- Build the daemon first: `cd ../../rust && cargo build --release -p afod`.
- `sh test/smoke.sh` checks both variants and the no-daemon error.
- `sh bench/run.sh [N]` prints spawn latency in microseconds per variant.
- On macOS the binary links libSystem (the kernel kills Mach-O executables without dyld) but still uses raw syscalls for all I/O.

## Backends
- `src/sys_linux_x86_64.h`, `src/sys_linux_aarch64.h`, `src/sys_darwin_arm64.h`: raw syscalls and struct layouts. The macOS BSD syscall ABI is not supported by Apple, so it must stay a small, stable set.
- `src/entry.h`: raw `_start` (no libc) on Linux, `main` on macOS.

## Results
Linux x86_64 (Xeon 2.1 GHz VM, 3000 runs, microseconds, p50): `exit0` 129, `afo` 167-182, `afo-relay` 169-186, `hello_libc` 983. The daemon round trip adds about 40-55 µs over the startup floor.

macOS arm64 (GitHub Actions virtual M1, noisy, 3000 runs, p50): `exit0` 1335, `hello_libc` 1333, `afo` 1488 (16 B) and 1711 (4 KB), `afo-relay` 1651 and 1567. The round trip adds about 150-370 µs over a libSystem-linked floor.

Fd passing and relaying are indistinguishable at 16 B and 4 KB on both systems, so the daemon writes to the caller's stdout directly.

## macOS: no dyld-free binary
A statically linked Mach-O (`LC_UNIXTHREAD`, no dyld, valid ad-hoc signature, with and without a platform version) is killed (`Killed: 9`) on exec on the macOS 26 runner, and the linker refuses dynamic executables that skip libSystem. The macOS startup floor is therefore dyld plus libSystem; the invoker cannot get below a libSystem hello. Lowering per-call cost on macOS means avoiding the process spawn, for example with an MCP front end on the same daemon.
