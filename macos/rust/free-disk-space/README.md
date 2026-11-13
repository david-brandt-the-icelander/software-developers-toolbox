# free-disk-space

`free-disk-space` — single Rust binary for reclaiming space on macOS/APFS.

Build: `cargo build --release -p free-disk-space` in `macos/rust`, or download a release: `https://icelander.net/downloads/zazzle/latest/zazzle-macos-universal.tar.gz` (with a `.sha256` next to it; also on the GitHub releases). Or with Homebrew: `brew install zazzcat/z/zazzle` (installs every tool here). A browser download of an unsigned build makes macOS say it "could not verify" the binary; run `xattr -d com.apple.quarantine afs fix-network free-disk-space` after unpacking.

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
free-disk-space daemon install            # background cleaner: login item, 30G threshold, every 15m
```

- **Scan** records every processed directory in SQLite (`~/.local/state/free-disk-space/scan.sqlite3`, or `$FDS_STATE`), so `resume` continues where it stopped. Candidates are large, inactive, uncompressed, single-link files whose sampled compressibility is high; archives, media, databases, VM disks, credentials and app bundles are skipped.
- **Dedupe** uses the full path of every file the scan saw (recorded in the same state file). Files sharing a leaf name (with extension) and size, of which at least one is writable, are SHA-256 hashed; identical ones on the same volume are replaced by APFS copy-on-write clones (`clonefile`, like `cp -c`) of one kept copy, each keeping its own mode, owner, times, ACL and xattrs. A dry run unless `--apply`. System files (not owned by you, owned by root, or under `/Library`, `/Applications`, `/private` and the like) are only touched with `--include-system`. Hard-linked, open, compressed, immutable and iCloud-placeholder files are never replaced. Run it before `compress`, which makes files ineligible.
- **Compress** uses the built-in `ditto --hfsCompression` into a temp file, verifies flags, size, mode/owner, xattrs, SHA-256 and allocated-size gain, then atomically swaps it in. Open, recently changed, flagged or hard-linked files are skipped; it aborts after 3 consecutive failures. Reports verified savings separately from the free-space delta. It first checks that `ditto --hfsCompression` really compresses a test file and stops if not: on macOS 26/27 it reports success but writes uncompressed copies.
- **Daemon** (`daemon install`) registers a launchd login item (`~/Library/LaunchAgents/local.free-disk-space.plist`, log in `~/Library/Logs/free-disk-space.log`) that runs `daemon run` now and at every login. Every `--cadence` (default `15m`; `90s`, `2h` also work) it compares free space on the data volume with `--threshold` (default `30G`; a percentage such as `10%` also works) and, when below, runs `clean --apply` on `--targets` (default `caches dev`; add `trash`, `docker`, ... explicitly). The cadence counts awake time only: the clock stops while the Mac sleeps, the process never schedules a wake, and checks are skipped while the display is asleep (maintenance wakes). `daemon status` and `daemon uninstall` manage it; `daemon run` runs in the foreground. Reinstall to change settings; the plist stores the path of the binary you installed from, so reinstall after moving or upgrading it.
- **Clean** is a dry run unless `--apply`. Dev artifacts need a manifest/lockfile, git-ignored and untracked contents, no modification today (Atlantic/Reykjavik), and no running process using them; eligibility is rechecked before deletion. Docker is only touched if already running (never started); containers and volumes are kept.

Caveats: unit-tested in CI on macOS and Linux; smoke-tested on Linux only for the scan/dry-run paths; `compress`, `dedupe --apply`, Finder trash, and Docker steps are untested on a real Mac. APFS clones (shared blocks) can't be detected (so `dedupe` savings are an upper bound), so compare the free-space delta on a `--sample` run before going broad.
