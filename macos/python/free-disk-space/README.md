# free-disk-space

`free_disk_space.py` — stock-Python-3 tool for reclaiming space on macOS/APFS.

```bash
./free_disk_space.py clean all                 # dry run: caches, downloads, trash, dev artifacts, docker
./free_disk_space.py clean caches dev --apply  # really do it (also: brew, xcode targets)
./free_disk_space.py new --root ~ --min-size 5M  # resumable compression scan (Ctrl-C safe)
./free_disk_space.py resume
./free_disk_space.py status
./free_disk_space.py list --top 30
./free_disk_space.py compress --sample 5       # verify on a few files first
./free_disk_space.py compress --limit 20G --yes
```

- **Scan** records every processed directory in SQLite (`~/.local/state/free-disk-space/scan.sqlite3`, or `$FDS_STATE`), so `resume` continues where it stopped. Candidates are large, inactive, uncompressed, single-link files whose sampled compressibility is high; archives, media, databases, VM disks, credentials and app bundles are skipped.
- **Compress** uses the built-in `ditto --hfsCompression` into a temp file, verifies flags, size, mode/owner, xattrs, SHA-256 and allocated-size gain, then atomically swaps it in. Open, recently changed, flagged or hard-linked files are skipped; it aborts after 3 consecutive failures. Reports verified savings separately from the free-space delta.
- **Clean** is a dry run unless `--apply`. Dev artifacts need a manifest/lockfile, git-ignored and untracked contents, no modification today (Atlantic/Reykjavik), and no running process using them; eligibility is rechecked before deletion. Docker is only touched if already running (never started); containers and volumes are kept.

Caveats: written and tested on Linux only for the scan/dry-run paths; `compress`, Finder trash, and Docker steps are untested on a real Mac. APFS clones (shared blocks) can't be detected, so compare the free-space delta on a `--sample` run before going broad.
