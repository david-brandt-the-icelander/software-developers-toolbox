---
name: free-disk-space
description: Reclaim disk space on a Mac (APFS). Use when asked to free space, clean caches/Downloads/Trash/dev build artifacts/Docker, deduplicate identical files into APFS clones, or find files to transparently compress. Wraps the `free-disk-space` Rust binary in macos/rust/free-disk-space.
---

# free-disk-space

Binary: `free-disk-space`, or `zazzle free-disk-space ...` (same program, started from the `zazzle` binary's directory, including `daemon install` for the scheduled run); built at `macos/rust/target/release/free-disk-space` (build: `cd macos/rust && cargo build --release`; macOS only for compress/dedupe/Finder/Docker steps).

## Cleanup (dry run by default)
1. Preview: `macos/rust/target/release/free-disk-space clean all`
2. Apply only what the user authorized: `... clean caches dev --apply` (targets: caches downloads trash dev docker brew xcode all).
3. `downloads` empties ~/Downloads fully; apply only with explicit user authorization.
4. `dev` keeps files modified today (Atlantic/Reykjavik), non-git-ignored, tracked, or in-use dirs; `--dev-root` sets the tree.
5. Docker is never started; it is cleaned and shut down only if already running.

## Compression (resumable)
1. `... new --root ~ --min-size 5M` (Ctrl-C safe); continue with `resume`; inspect with `status`, `list --top 30`.
2. Verify first: `... compress --sample 5`, then compare "verified saving" with "free-space change".
3. Go wider: `... compress --limit 20G --yes`.
4. State: `~/.local/state/free-disk-space/scan.sqlite3` (`--state` or `$FDS_STATE`).

## Dedupe (after a scan, before compress)
1. Preview: `... dedupe` hashes same-named, same-sized files and lists identical ones; nothing changes.
2. Verify first: `... dedupe --sample 5 --apply`, check the files and the free-space change, then `... dedupe --apply`.
3. Add `--include-system` only with explicit user authorization; default touches only files the user owns outside system paths.
4. Scans made before version 1.1 hold no file names; rerun `new`.

## Background daemon (launchd login item)
1. Install only with the user's explicit authorization, since it deletes on its own: `... daemon install [--threshold 30G|10%] [--cadence 15m] [--targets caches dev]` (defaults: 30G, 15m, caches dev).
2. `... daemon status`; `... daemon uninstall`; foreground test: `... daemon run`. Log: `~/Library/Logs/free-disk-space.log`.
3. Cadence is awake time only and skipped while the display sleeps; it never wakes the Mac. Add `trash`, `downloads` or `docker` to `--targets` only with explicit authorization.

## Rules
- Report initial/final free space, what was deleted/compressed, protected entries, failures.
- Never claim summed file sizes equal reclaimed space.
- Leave uncertain items untouched and list them.
- Compress and dedupe are untested on real Macs beyond the sample step; stop on any failure.
