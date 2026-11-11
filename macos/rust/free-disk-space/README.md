# free-disk-space

`free-disk-space` — single Rust binary for reclaiming space on macOS/APFS.

Build: `cargo build --release -p free-disk-space` in `macos/rust`, or download a release.

```bash
free-disk-space clean all                 # dry run: caches, downloads, trash, dev artifacts, docker
free-disk-space clean caches dev --apply  # really do it (also: brew, xcode targets)
free-disk-space new --root ~ --min-size 5M  # resumable compression scan (Ctrl-C safe)
free-disk-space resume
free-disk-space status
free-disk-space list --top 30
free-disk-space dedupe                    # dry run: identical same-named files to clone
free-disk-space dedupe --sample 5 --apply # verify on a few duplicates, then drop --sample
free-disk-space compress --sample 5       # verify on a few files first
free-disk-space compress --limit 20G --yes
```

- **Scan** records every processed directory in SQLite (`~/.local/state/free-disk-space/scan.sqlite3`, or `$FDS_STATE`), so `resume` continues where it stopped. Candidates are large, inactive, uncompressed, single-link files whose sampled compressibility is high; archives, media, databases, VM disks, credentials and app bundles are skipped.
- **Dedupe** uses the full path of every file the scan saw (recorded in the same state file). Files sharing a leaf name (with extension) and size, of which at least one is writable, are SHA-256 hashed; identical ones on the same volume are replaced by APFS copy-on-write clones (`clonefile`, like `cp -c`) of one kept copy, each keeping its own mode, owner, times, ACL and xattrs. A dry run unless `--apply`. System files (not owned by you, owned by root, or under `/Library`, `/Applications`, `/private` and the like) are only touched with `--include-system`. Hard-linked, open, compressed, immutable and iCloud-placeholder files are never replaced. Run it before `compress`, which makes files ineligible.
- **Compress** uses the built-in `ditto --hfsCompression` into a temp file, verifies flags, size, mode/owner, xattrs, SHA-256 and allocated-size gain, then atomically swaps it in. Open, recently changed, flagged or hard-linked files are skipped; it aborts after 3 consecutive failures. Reports verified savings separately from the free-space delta.
- **Clean** is a dry run unless `--apply`. Dev artifacts need a manifest/lockfile, git-ignored and untracked contents, no modification today (Atlantic/Reykjavik), and no running process using them; eligibility is rechecked before deletion. Docker is only touched if already running (never started); containers and volumes are kept.

Caveats: unit-tested in CI on macOS and Linux; smoke-tested on Linux only for the scan/dry-run paths; `compress`, `dedupe --apply`, Finder trash, and Docker steps are untested on a real Mac. APFS clones (shared blocks) can't be detected (so `dedupe` savings are an upper bound), so compare the free-space delta on a `--sample` run before going broad.
