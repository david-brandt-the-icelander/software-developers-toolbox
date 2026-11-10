#!/usr/bin/env python3
# Reclaim macOS disk space via a resumable scan, compression, clone dedupe and safe cleanups, on stock Python 3.
from __future__ import annotations

import argparse
import bisect
import ctypes
import datetime
import hashlib
import json
import os
import shutil
import signal
import sqlite3
import stat
import subprocess
import sys
import time
import zlib

VERSION = "1.1"
IS_TTY = sys.stdout.isatty()
IS_MAC = sys.platform == "darwin"
HOME = os.path.expanduser("~")

UF_COMPRESSED = 0x20
UF_IMMUTABLE = 0x2
UF_APPEND = 0x4
SF_IMMUTABLE = 0x20000
SF_DATALESS = 0x40000000  # iCloud / File Provider placeholder

DEFAULT_STATE = os.environ.get(
    "FDS_STATE", os.path.join(HOME, ".local", "state", "free-disk-space", "scan.sqlite3")
)

# --------------------------------------------------------------------------- output


# Wrap text in an ANSI color code when writing to a terminal.
def c(code: str, text: str) -> str:
    if IS_TTY and not os.environ.get("NO_COLOR"):
        return "\033[%sm%s\033[0m" % (code, text)
    return text


# Format a byte count as a short human-readable size.
def human(n: float) -> str:
    n = float(n)
    for unit in ("B", "KiB", "MiB", "GiB", "TiB"):
        if abs(n) < 1024 or unit == "TiB":
            return ("%d %s" % (n, unit)) if unit == "B" else ("%.1f %s" % (n, unit))
        n /= 1024
    return str(n)


# Format seconds as H:MM:SS.
def hms(sec: float) -> str:
    sec = int(sec)
    return "%d:%02d:%02d" % (sec // 3600, sec % 3600 // 60, sec % 60)


# Parse sizes like 10M or 2G into bytes.
def parse_size(text: str) -> int:
    t = text.strip().upper().rstrip("B")
    mult = {"K": 1 << 10, "M": 1 << 20, "G": 1 << 30, "T": 1 << 40}
    if t and t[-1] in mult:
        return int(float(t[:-1]) * mult[t[-1]])
    return int(float(t))


# Print a line immediately.
def log(msg: str = "") -> None:
    print(msg, flush=True)


# Print a warning to stderr.
def warn(msg: str) -> None:
    print(c("33", "warning: ") + msg, file=sys.stderr, flush=True)


# Print an error to stderr and exit.
def die(msg: str, code: int = 1) -> "None":
    print(c("31", "error: ") + msg, file=sys.stderr, flush=True)
    sys.exit(code)


# Rate-limited progress line, in place on a terminal and periodic lines otherwise.
class Ticker:

    # Set the minimum interval between progress updates.
    def __init__(self, interval: float = 0.0):
        self.interval = interval or (0.4 if IS_TTY else 15.0)
        self.last = 0.0
        self.shown = False

    # Show progress text if the interval has elapsed or force is set.
    def tick(self, text: str, force: bool = False) -> None:
        now = time.time()
        if force or now - self.last >= self.interval:
            self.last = now
            if IS_TTY:
                cols = shutil.get_terminal_size((100, 20)).columns
                sys.stdout.write("\r\033[K" + text[: cols - 1])
                sys.stdout.flush()
                self.shown = True
            else:
                print(text, flush=True)

    # Erase the in-place progress line.
    def clear(self) -> None:
        if IS_TTY and self.shown:
            sys.stdout.write("\r\033[K")
            sys.stdout.flush()
            self.shown = False


STOP = False


# Request a graceful stop on the first Ctrl-C and abort on the second.
def _on_sigint(signum, frame):  # noqa: ANN001
    global STOP
    if STOP:
        raise KeyboardInterrupt
    STOP = True
    print("\n" + c("33", "Stopping after the current item (Ctrl-C again to abort)..."), flush=True)


# Install the Ctrl-C handler.
def install_sigint() -> None:
    signal.signal(signal.SIGINT, _on_sigint)


# Return the volume holding user data.
def data_volume() -> str:
    return "/System/Volumes/Data" if os.path.isdir("/System/Volumes/Data") else "/"


# Return free bytes on the data volume.
def free_bytes() -> int:
    return shutil.disk_usage(data_volume()).free


# Print and return free space with a label.
def report_free(label: str) -> int:
    f = free_bytes()
    log("%s free space on %s: %s" % (label, data_volume(), c("1", human(f))))
    return f


# Run a subprocess, printing a heartbeat every 30 seconds so long jobs never go silent.
def run_cmd(cmd, label: str, timeout: float = 3600, heartbeat: float = 30.0):
    t0 = time.time()
    try:
        p = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    except OSError as e:
        return -1, str(e)
    out = ""
    while True:
        try:
            out, _ = p.communicate(timeout=heartbeat)
            break
        except subprocess.TimeoutExpired:
            el = time.time() - t0
            if el > timeout:
                p.kill()
                out, _ = p.communicate()
                return -2, out + "\n(timed out)"
            log("  ... %s still running (%s)" % (label, hms(el)))
    return p.returncode, out or ""


# --------------------------------------------------------------------------- scan state

SCHEMA = """
CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT);
CREATE TABLE IF NOT EXISTS dirs(id INTEGER PRIMARY KEY, path TEXT UNIQUE, done INTEGER DEFAULT 0);
CREATE INDEX IF NOT EXISTS dirs_done ON dirs(done);
CREATE TABLE IF NOT EXISTS cands(
  path TEXT PRIMARY KEY, size INTEGER, alloc INTEGER, est_saved INTEGER,
  mtime_ns INTEGER, ino INTEGER, status TEXT DEFAULT 'pending', note TEXT, saved INTEGER DEFAULT 0);
CREATE INDEX IF NOT EXISTS cands_status ON cands(status, est_saved);
CREATE TABLE IF NOT EXISTS files(
  path TEXT PRIMARY KEY, name TEXT, size INTEGER, cloned INTEGER DEFAULT 0) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS files_name ON files(name, size);
"""

SKIP_EXT = {
    # already compressed archives / media
    "zip", "gz", "tgz", "bz2", "xz", "zst", "7z", "rar", "lz4", "lzma", "br", "jar", "war", "apk", "ipa",
    "dmg", "iso", "jpg", "jpeg", "png", "gif", "heic", "heif", "webp", "avif", "mp3", "m4a", "aac", "flac",
    "ogg", "opus", "mp4", "m4v", "mov", "mkv", "avi", "webm", "wmv", "pack", "rpm", "deb", "whl", "woff2",
    # live databases / VM disks / app state
    "sqlite", "sqlite3", "db", "db3", "sqlite-wal", "sqlite-shm", "db-wal", "db-shm", "ldb", "realm", "mdb",
    "ibd", "raw", "img", "vmdk", "vdi", "vhd", "vhdx", "qcow2", "sparseimage", "sparsebundle", "lock",
    # credentials
    "pem", "key", "p12", "pfx", "keychain", "keychain-db", "gpg", "kdbx",
}
SKIP_NAMES = {".DS_Store", "Docker.raw", "id_rsa", "id_ed25519", "id_ecdsa", ".netrc", ".npmrc", ".pypirc"}
SKIP_DIR_SUFFIX = (
    ".app", ".photoslibrary", ".vmwarevm", ".pvm", ".utm", ".sparsebundle", ".tvlibrary", ".musiclibrary",
    ".imovielibrary", ".fcpbundle", ".xcarchive", ".lproj_never",
)
SKIP_DIR_NAMES = {
    ".Trash", ".Spotlight-V100", ".fseventsd", ".DocumentRevisions-V100", ".TemporaryItems", ".ssh", ".gnupg",
    ".aws", ".gcloud", "Keychains", "Mobile Documents", "CloudStorage", "com.docker.docker", ".git_never",
}
SKIP_PREFIXES = (
    "/System", "/usr", "/bin", "/sbin", "/dev", "/Volumes", "/cores", "/.vol", "/private/var/vm",
    "/private/var/db", "/System/Volumes/VM", "/System/Volumes/Preboot", "/System/Volumes/Update",
    "/System/Volumes/Hardware", "/System/Volumes/iSCPreboot", "/Library/Apple",
)


# Open (or create) the scan state database.
def open_db(path: str, create: bool) -> sqlite3.Connection:
    if not create and not os.path.exists(path):
        die("no scan state at %s - start one with: %s new" % (path, os.path.basename(sys.argv[0])))
    os.makedirs(os.path.dirname(path), exist_ok=True)
    db = sqlite3.connect(path)
    db.execute("PRAGMA journal_mode=WAL")
    db.execute("PRAGMA synchronous=NORMAL")
    db.executescript(SCHEMA)
    return db


# Read a JSON value from the meta table.
def meta_get(db, key, default=None):
    r = db.execute("SELECT value FROM meta WHERE key=?", (key,)).fetchone()
    return json.loads(r[0]) if r else default


# Write a JSON value to the meta table.
def meta_set(db, key, value) -> None:
    db.execute("INSERT OR REPLACE INTO meta(key,value) VALUES(?,?)", (key, json.dumps(value)))


# Estimate compressed/raw size from head, middle and tail samples of a file.
def sample_ratio(path: str, size: int):
    chunk = 64 * 1024
    try:
        with open(path, "rb") as f:
            if size <= 4 * chunk:
                data = f.read()
            else:
                parts = []
                for off in (0, size // 2 - chunk // 2, size - chunk):
                    f.seek(off)
                    parts.append(f.read(chunk))
                data = b"".join(parts)
    except OSError:
        return None
    if not data:
        return None
    return len(zlib.compress(data, 3)) / len(data)


# Return estimated savings if a file is a compression candidate, else None.
def consider(path: str, st: os.stat_result, cfg: dict, cutoff: float):
    if not stat.S_ISREG(st.st_mode) or st.st_size < cfg["min_size"] or st.st_nlink != 1:
        return None
    flags = getattr(st, "st_flags", 0)
    if flags & (UF_COMPRESSED | UF_IMMUTABLE | UF_APPEND | SF_IMMUTABLE | SF_DATALESS):
        return None
    if st.st_mtime > cutoff:
        return None
    alloc = getattr(st, "st_blocks", 0) * 512
    if alloc < st.st_size * 0.8:  # sparse / already smaller on disk
        return None
    name = os.path.basename(path)
    if name in SKIP_NAMES or name.startswith("id_"):
        return None
    ext = name.rsplit(".", 1)[-1].lower() if "." in name else ""
    if ext in SKIP_EXT:
        return None
    ratio = sample_ratio(path, st.st_size)
    if ratio is None:
        return None
    pct = (1 - ratio) * 100
    est = int(st.st_size * (1 - ratio) * 0.85)
    if pct < cfg["min_pct"] or est < 128 * 1024:
        return None
    return est


# Tell whether a directory must not be scanned.
def dir_excluded(path: str, name: str, cfg: dict) -> bool:
    if name in SKIP_DIR_NAMES or name.endswith(SKIP_DIR_SUFFIX):
        return True
    for p in SKIP_PREFIXES:
        if path == p or path.startswith(p + "/"):
            return True
    for p in cfg.get("exclude", []):
        if path == p or path.startswith(p.rstrip("/") + "/"):
            return True
    return False


# Process pending directories until done or interrupted and return True if finished.
def run_scan(db: sqlite3.Connection, cfg: dict) -> bool:
    stats = meta_get(db, "stats", {"dirs": 0, "files": 0, "bytes": 0, "errors": 0, "cands": 0, "est": 0})
    prior_elapsed = meta_get(db, "elapsed", 0.0)
    cutoff = time.time() - cfg["min_age_days"] * 86400
    t0 = time.time()
    tk = Ticker()
    last_commit = time.time()
    session_files = 0

    # Persist scan statistics and commit the transaction.
    def commit():
        meta_set(db, "stats", stats)
        meta_set(db, "elapsed", prior_elapsed + time.time() - t0)
        db.commit()

    while not STOP:
        row = db.execute("SELECT id, path FROM dirs WHERE done=0 ORDER BY id DESC LIMIT 1").fetchone()
        if not row:
            break
        did, d = row
        try:
            with os.scandir(d) as it:
                for e in it:
                    try:
                        if e.is_symlink():
                            continue
                        if e.is_dir(follow_symlinks=False):
                            if dir_excluded(e.path, e.name, cfg):
                                continue
                            if not cfg["cross"] and e.stat(follow_symlinks=False).st_dev != cfg["dev"]:
                                continue
                            db.execute("INSERT OR IGNORE INTO dirs(path) VALUES(?)", (e.path,))
                        elif e.is_file(follow_symlinks=False):
                            st = e.stat(follow_symlinks=False)
                            stats["files"] += 1
                            stats["bytes"] += st.st_size
                            session_files += 1
                            db.execute("INSERT OR IGNORE INTO files(path,name,size) VALUES(?,?,?)",
                                       (e.path, e.name, st.st_size))
                            est = consider(e.path, st, cfg, cutoff)
                            if est:
                                cur = db.execute(
                                    "INSERT OR IGNORE INTO cands(path,size,alloc,est_saved,mtime_ns,ino) "
                                    "VALUES(?,?,?,?,?,?)",
                                    (e.path, st.st_size, st.st_blocks * 512, est, st.st_mtime_ns, st.st_ino),
                                )
                                if cur.rowcount:
                                    stats["cands"] += 1
                                    stats["est"] += est
                    except (OSError, UnicodeError):
                        stats["errors"] += 1
        except OSError:
            stats["errors"] += 1
        db.execute("UPDATE dirs SET done=1 WHERE id=?", (did,))
        stats["dirs"] += 1
        if time.time() - last_commit > 2:
            commit()
            last_commit = time.time()
        pending = db.execute("SELECT COUNT(*) FROM dirs WHERE done=0").fetchone()[0]
        el = time.time() - t0
        tk.tick(
            "scanned %d dirs (%d queued) | %d files, %s | %d candidates, ~%s saveable | %s | %s"
            % (stats["dirs"], pending, stats["files"], human(stats["bytes"]), stats["cands"],
               human(stats["est"]), hms(prior_elapsed + el), d[-40:] if not IS_TTY else d)
        )
    commit()
    tk.clear()
    pending = db.execute("SELECT COUNT(*) FROM dirs WHERE done=0").fetchone()[0]
    if pending == 0:
        meta_set(db, "finished", time.time())
        db.commit()
        return True
    return False


# Start a fresh scan and run it.
def cmd_new(args) -> None:
    root = os.path.realpath(os.path.expanduser(args.root))
    if not os.path.isdir(root):
        die("root %s is not a directory" % root)
    if os.path.exists(args.state):
        db0 = open_db(args.state, False)
        unfinished = db0.execute("SELECT COUNT(*) FROM dirs WHERE done=0").fetchone()[0]
        db0.close()
        if unfinished and not args.force:
            die("an unfinished scan exists (%d dirs pending). Use `resume`, or `new --force` to discard it."
                % unfinished)
        for suffix in ("", "-wal", "-shm"):
            if os.path.exists(args.state + suffix):
                os.remove(args.state + suffix)
    db = open_db(args.state, True)
    cfg = {
        "root": root, "dev": os.stat(root).st_dev, "min_size": parse_size(args.min_size),
        "min_age_days": args.min_age_days, "min_pct": args.min_savings_pct, "cross": args.cross_devices,
        "exclude": [os.path.realpath(os.path.expanduser(x)) for x in args.exclude],
    }
    meta_set(db, "cfg", cfg)
    meta_set(db, "started", time.time())
    db.execute("INSERT OR IGNORE INTO dirs(path) VALUES(?)", (root,))
    db.commit()
    log("Scanning %s  (min size %s, untouched >= %d days, >= %d%% estimated savings)"
        % (root, human(cfg["min_size"]), cfg["min_age_days"], cfg["min_pct"]))
    log("State: %s   (Ctrl-C is safe; continue with `resume`)" % args.state)
    finish_scan(db, cfg)


# Run the scan loop and report whether it completed or paused.
def finish_scan(db, cfg) -> None:
    install_sigint()
    done = run_scan(db, cfg)
    s = meta_get(db, "stats")
    if done:
        log(c("32", "Scan complete: ") + "%d candidates, ~%s estimated saveable." % (s["cands"], human(s["est"])))
        log("Next: `dedupe` to preview duplicate cloning, `list` then `compress --sample 5` to compress.")
    else:
        log(c("33", "Scan paused. ") + "Run `resume` to continue.")


# Continue an interrupted scan.
def cmd_resume(args) -> None:
    db = open_db(args.state, False)
    cfg = meta_get(db, "cfg")
    if not cfg:
        die("state file has no scan configuration; start with `new`")
    if meta_get(db, "finished"):
        log("Scan already complete. Use `new` to rescan, or `status`/`compress`.")
        return
    pending = db.execute("SELECT COUNT(*) FROM dirs WHERE done=0").fetchone()[0]
    log("Resuming scan of %s (%d directories pending)" % (cfg["root"], pending))
    finish_scan(db, cfg)


# Show scan progress, candidate totals and free space.
def cmd_status(args) -> None:
    db = open_db(args.state, False)
    cfg = meta_get(db, "cfg", {})
    s = meta_get(db, "stats", {})
    pending = db.execute("SELECT COUNT(*) FROM dirs WHERE done=0").fetchone()[0]
    log(c("1", "Scan") + ": root %s" % cfg.get("root"))
    log("  state     : %s" % ("complete" if meta_get(db, "finished") else "in progress (%d dirs queued)" % pending))
    log("  processed : %d dirs, %d files, %s, %d errors, %s elapsed"
        % (s.get("dirs", 0), s.get("files", 0), human(s.get("bytes", 0)), s.get("errors", 0),
           hms(meta_get(db, "elapsed", 0))))
    log(c("1", "Candidates"))
    for status, n, est, saved in db.execute(
        "SELECT status, COUNT(*), SUM(est_saved), SUM(saved) FROM cands GROUP BY status ORDER BY status"
    ):
        log("  %-8s %7d files   estimated %-10s verified saved %s" % (status, n, human(est or 0), human(saved or 0)))
    names, cloned = db.execute("SELECT COUNT(*), SUM(cloned) FROM files").fetchone()
    log(c("1", "File names") + ": %d recorded, %d replaced by clones" % (names, cloned or 0))
    report_free("Current")


# List the best candidates by estimated savings.
def cmd_list(args) -> None:
    db = open_db(args.state, False)
    rows = db.execute(
        "SELECT path,size,est_saved,status,note FROM cands WHERE status=? ORDER BY est_saved DESC LIMIT ?",
        (args.status, args.top),
    ).fetchall()
    for p, size, est, status, note in rows:
        log("%10s  ~%10s  %s%s" % (human(size), human(est), p, ("  [%s]" % note) if note else ""))
    if not rows:
        log("(none with status %s)" % args.status)


# --------------------------------------------------------------------------- compress


# Return the SHA-256 hex digest of a file.
def sha256(path: str) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for b in iter(lambda: f.read(1 << 20), b""):
            h.update(b)
    return h.hexdigest()


# Return a file's extended attribute names, excluding compression internals.
def xattr_names(path: str) -> set:
    r = subprocess.run(["/usr/bin/xattr", path], capture_output=True, text=True)
    if r.returncode != 0:
        raise OSError("xattr failed: " + r.stderr.strip())
    ignore = {"com.apple.decmpfs", "com.apple.ResourceFork"}
    return {x.strip() for x in r.stdout.splitlines() if x.strip()} - ignore


# Tell whether any process has the file open, treating failures as in use.
def in_use(path: str) -> bool:
    try:
        r = subprocess.run(["/usr/sbin/lsof", "-t", "--", path], capture_output=True, text=True, timeout=60)
    except (OSError, subprocess.SubprocessError):
        return True  # cannot tell -> treat as in use
    return bool(r.stdout.strip())


# Compress one file in place via a verified temp copy and return (status, note, saved bytes).
def compress_one(path: str, row, args):
    size, mtime_ns, ino = row
    try:
        st = os.lstat(path)
    except OSError as e:
        return "skipped", "gone: %s" % e, 0
    if not stat.S_ISREG(st.st_mode) or st.st_nlink != 1:
        return "skipped", "not a plain single-link file", 0
    if (st.st_size, st.st_mtime_ns, st.st_ino) != (size, mtime_ns, ino):
        return "skipped", "changed since scan", 0
    flags = getattr(st, "st_flags", 0)
    if flags & (UF_COMPRESSED | UF_IMMUTABLE | UF_APPEND | SF_IMMUTABLE | SF_DATALESS):
        return "skipped", "already compressed / flagged", 0
    if time.time() - st.st_mtime < args.min_age_days * 86400:
        return "skipped", "modified recently", 0
    d = os.path.dirname(path)
    if not os.access(d, os.W_OK):
        return "skipped", "directory not writable", 0
    need = st.st_size + args.reserve
    if free_bytes() < need:
        return "skipped", "not enough free space for temp copy (need %s)" % human(need), 0
    if in_use(path):
        return "skipped", "file is open by a process", 0

    tmp = os.path.join(d, ".fds-%d-%s.tmp" % (os.getpid(), os.path.basename(path)[:80]))
    try:
        r = subprocess.run(["/usr/bin/ditto", "--hfsCompression", path, tmp], capture_output=True, text=True)
        if r.returncode != 0:
            return "failed", "ditto: " + r.stderr.strip()[:200], 0
        t = os.lstat(tmp)
        if not getattr(t, "st_flags", 0) & UF_COMPRESSED:
            return "skipped", "APFS declined to compress (no gain)", 0
        saved = st.st_blocks * 512 - t.st_blocks * 512
        if saved < args.min_gain:
            return "skipped", "gain too small (%s)" % human(max(saved, 0)), 0
        if t.st_size != st.st_size:
            return "failed", "size mismatch", 0
        if (stat.S_IMODE(t.st_mode), t.st_uid, t.st_gid) != (stat.S_IMODE(st.st_mode), st.st_uid, st.st_gid):
            return "failed", "permissions/ownership differ", 0
        if not xattr_names(path) <= xattr_names(tmp):
            return "failed", "extended attributes lost", 0
        if sha256(path) != sha256(tmp):
            return "failed", "content hash mismatch", 0
        os.utime(tmp, ns=(st.st_atime_ns, st.st_mtime_ns))
        st2 = os.lstat(path)
        if (st2.st_size, st2.st_mtime_ns, st2.st_ino, st2.st_nlink) != (st.st_size, st.st_mtime_ns, st.st_ino, 1):
            return "skipped", "changed during compression", 0
        os.replace(tmp, path)
        return "done", "", saved
    except OSError as e:
        return "failed", str(e), 0
    finally:
        if os.path.lexists(tmp):
            try:
                os.remove(tmp)
            except OSError:
                pass


# Compress planned candidates in place and report verified savings.
def cmd_compress(args) -> None:
    if not IS_MAC and not os.environ.get("FDS_FORCE"):
        die("compress requires macOS (uses /usr/bin/ditto and APFS compression)")
    if IS_MAC and not os.path.exists("/usr/bin/ditto"):
        die("/usr/bin/ditto not found - cannot compress; nothing was changed")
    db = open_db(args.state, False)
    if not meta_get(db, "finished"):
        warn("scan is not complete; compressing from the candidates found so far")
    limit_files = args.sample or args.max_files or 10 ** 9
    limit_bytes = parse_size(args.limit) if args.limit else None
    rows = db.execute(
        "SELECT path,size,mtime_ns,ino,est_saved FROM cands WHERE status='pending' ORDER BY est_saved DESC"
    ).fetchall()
    plan, tot = [], 0
    for r in rows:
        if len(plan) >= limit_files or (limit_bytes is not None and tot >= limit_bytes):
            break
        plan.append(r)
        tot += r[4]
    if not plan:
        log("No pending candidates.")
        return
    log("Plan: %d files, ~%s estimated savings%s" % (len(plan), human(tot), "  (SAMPLE run)" if args.sample else ""))
    if args.dry_run:
        for p, size, _m, _i, est in plan[:25]:
            log("  %10s ~%10s %s" % (human(size), human(est), p))
        log("Dry run - nothing changed.")
        return
    if IS_TTY and not args.yes:
        if input("Compress these files in place? [y/N] ").strip().lower() != "y":
            log("Aborted.")
            return
    install_sigint()
    free0 = report_free("Initial")
    tk = Ticker()
    saved_total = n_done = n_skip = n_fail = consecutive_fail = 0
    t0 = time.time()
    for i, (path, size, mtime_ns, ino, est) in enumerate(plan, 1):
        if STOP:
            break
        tk.tick("[%d/%d] %s | saved %s | %s" % (i, len(plan), path[-60:], human(saved_total), hms(time.time() - t0)),
                force=True)
        status, note, saved = compress_one(path, (size, mtime_ns, ino), args)
        tk.clear()
        db.execute("UPDATE cands SET status=?, note=?, saved=? WHERE path=?", (status, note, saved, path))
        db.commit()
        if status == "done":
            n_done += 1
            saved_total += saved
            consecutive_fail = 0
            log("%s %-10s %s" % (c("32", "done  "), "-" + human(saved), path))
        elif status == "skipped":
            n_skip += 1
            log("%s %s: %s" % (c("2", "skip  "), path, note))
        else:
            n_fail += 1
            consecutive_fail += 1
            log("%s %s: %s" % (c("31", "FAILED"), path, note))
            if consecutive_fail >= 3:
                log(c("31", "3 consecutive failures - aborting for safety."))
                break
        if not IS_TTY and time.time() - tk.last >= 15:
            tk.tick("progress: %d/%d files, verified saved %s" % (i, len(plan), human(saved_total)), force=True)
    free1 = report_free("Final")
    log("Compressed %d, skipped %d, failed %d in %s" % (n_done, n_skip, n_fail, hms(time.time() - t0)))
    log("Verified allocated-space saving (sum over files): %s" % c("1", human(saved_total)))
    log("Overall free-space change on the volume:         %s" % c("1", human(free1 - free0)))
    log("(These can differ: snapshots, shared blocks/clones and other writers affect free space.)")
    if args.sample:
        log("Sample finished. If it looks right, run `compress` without --sample.")


# --------------------------------------------------------------------------- cleanup helpers


# Return allocated bytes, newest mtime and item count of a tree without following symlinks.
def tree_size(path: str):
    total, newest, n = 0, 0.0, 0
    stack = [path]
    while stack:
        p = stack.pop()
        try:
            st = os.lstat(p)
        except OSError:
            continue
        total += getattr(st, "st_blocks", st.st_size // 512) * 512
        newest = max(newest, st.st_mtime)
        n += 1
        if stat.S_ISDIR(st.st_mode):
            try:
                with os.scandir(p) as it:
                    stack.extend(e.path for e in it)
            except OSError:
                pass
    return total, newest, n


# Delete a path recursively without following symlinks or changing permissions.
def rm_tree(path: str, res: dict, tk: Ticker, apply: bool) -> None:
    try:
        st = os.lstat(path)
    except OSError:
        return
    res["scanned"] += 1
    if stat.S_ISDIR(st.st_mode):
        try:
            with os.scandir(path) as it:
                kids = [e.path for e in it]
        except OSError as e:
            res["protected"].append("%s (%s)" % (path, e.strerror))
            return
        for k in kids:
            rm_tree(k, res, tk, apply)
            if STOP:
                return
        if apply:
            try:
                os.rmdir(path)
            except OSError:
                pass  # non-empty because a child was protected
    else:
        size = getattr(st, "st_blocks", 0) * 512
        if apply:
            try:
                os.unlink(path)
            except OSError as e:
                res["protected"].append("%s (%s)" % (path, e.strerror))
                return
        res["bytes"] += size
        res["files"] += 1
    tk.tick("  %s %d items, %s ..." % ("removed" if apply else "found", res["scanned"], human(res["bytes"])))


# Remove a directory's contents but keep the directory itself.
def empty_dir(path: str, apply: bool, label: str) -> dict:
    res = {"bytes": 0, "files": 0, "scanned": 0, "protected": []}
    path = os.path.expanduser(path)
    if not os.path.isdir(path) or os.path.islink(path):
        log("  - %s: not present" % label)
        return res
    tk = Ticker()
    try:
        kids = [e.path for e in os.scandir(path)]
    except OSError as e:
        log("  - %s: cannot read (%s)" % (label, e.strerror))
        return res
    for k in kids:
        rm_tree(k, res, tk, apply)
        if STOP:
            break
    tk.clear()
    log("  %s %-45s %10s%s" % ("-" if apply else "~", label, human(res["bytes"]),
                               ("   (%d protected, skipped)" % len(res["protected"])) if res["protected"] else ""))
    return res


CACHE_DIRS = [
    "~/.cache", "~/.nuget/packages", "~/Library/Caches",
    "~/Library/Application Support/Code/CachedExtensionVSIXs",
    "~/Library/Application Support/Code/CachedData",
    "~/Library/Application Support/Code/logs",
    "~/Library/Application Support/Claude/Cache",
    "~/Library/Application Support/Claude/Code Cache",
]
XCODE_DIRS = ["~/Library/Developer/Xcode/DerivedData"]


# Empty each listed directory and accumulate totals.
def clean_dirs(title: str, dirs, apply: bool, ctx: dict) -> None:
    log(c("1", title))
    for d in dirs:
        r = empty_dir(d, apply, d)
        ctx["bytes"] += r["bytes"]
        ctx["protected"].extend(r["protected"])


# Empty the Trash through Finder.
def clean_trash(apply: bool, ctx: dict) -> None:
    log(c("1", "Trash"))
    if not apply:
        r = empty_dir("~/.Trash", False, "~/.Trash (via Finder when applied)")
        ctx["bytes"] += r["bytes"]
        return
    if not IS_MAC:
        log("  - skipped: Finder is macOS only")
        return
    code, out = run_cmd(["/usr/bin/osascript", "-e", 'tell application "Finder" to empty trash'], "empty trash")
    log("  - Finder empty trash: %s" % ("ok" if code == 0 else "failed: " + out.strip()[:200]))
    if code != 0:
        ctx["failures"].append("empty trash: " + out.strip()[:200])


# Run Homebrew cleanup if installed.
def clean_brew(apply: bool, ctx: dict) -> None:
    log(c("1", "Homebrew"))
    brew = shutil.which("brew")
    if not brew:
        log("  - brew not installed")
        return
    if not apply:
        log("  ~ would run: brew cleanup --prune=all -s")
        return
    code, out = run_cmd([brew, "cleanup", "--prune=all", "-s"], "brew cleanup")
    log("  - brew cleanup: %s" % ("ok" if code == 0 else "failed: " + out.strip()[-200:]))


# ---- dev artifacts

# name -> (required sibling manifests (any), required lockfiles (any, or None), required child (any) or None)
ARTIFACTS = {
    "node_modules": (["package.json"], ["package-lock.json", "yarn.lock", "pnpm-lock.yaml", "bun.lockb", "bun.lock"], None),
    ".next": (["package.json"], None, None),
    ".nuxt": (["package.json"], None, None),
    "dist": (["package.json"], ["package-lock.json", "yarn.lock", "pnpm-lock.yaml", "bun.lockb", "bun.lock"], None),
    "build": (["package.json", "build.gradle", "build.gradle.kts", "CMakeLists.txt", "setup.py"], None, None),
    "target": (["Cargo.toml", "pom.xml"], None, ["CACHEDIR.TAG", ".rustc_info.json", "classes", "maven-status"]),
    ".gradle": (["build.gradle", "build.gradle.kts", "settings.gradle", "settings.gradle.kts"], None, None),
    "__pycache__": ([], None, None),
    ".pytest_cache": ([], None, ["CACHEDIR.TAG", "v"]),
    ".mypy_cache": ([], None, ["CACHEDIR.TAG"]),
    ".ruff_cache": ([], None, ["CACHEDIR.TAG"]),
    "obj": (["*.csproj", "*.fsproj", "*.vbproj"], None, None),
    "bin": (["*.csproj", "*.fsproj", "*.vbproj"], None, None),
    "DerivedData": (["*.xcodeproj", "*.xcworkspace", "Package.swift"], None, None),
}


# Tell whether the directory contains any entry matching one of the glob patterns.
def _has_any(parent: str, names) -> bool:
    import fnmatch
    try:
        listing = os.listdir(parent)
    except OSError:
        return False
    return any(fnmatch.fnmatch(x, pat) for pat in names for x in listing)


# Return the timestamp of local midnight in Atlantic/Reykjavik.
def today_start_ts() -> float:
    try:
        from zoneinfo import ZoneInfo
        tz = ZoneInfo("Atlantic/Reykjavik")
    except Exception:  # Iceland is UTC+0 year-round
        tz = datetime.timezone.utc
    now = datetime.datetime.now(tz)
    return now.replace(hour=0, minute=0, second=0, microsecond=0).timestamp()


# Run a git command in a directory with optional locks disabled.
def git(parent: str, *a):
    env = dict(os.environ, GIT_OPTIONAL_LOCKS="0")
    return subprocess.run(["git", "-C", parent] + list(a), capture_output=True, env=env, timeout=60)


# Open files, working directories and command lines of running processes.
class Snapshot:

    # Capture the process snapshot via lsof and ps.
    def __init__(self):
        self.ok = False
        self.paths, self.cwds, self.cmds = [], [], []
        try:
            out = subprocess.run(["lsof", "-nP", "-F", "fn"], capture_output=True, text=True, timeout=180).stdout
            ps = subprocess.run(["ps", "-axo", "command="], capture_output=True, text=True, timeout=30).stdout
        except (OSError, subprocess.SubprocessError):
            return
        cur, paths, cwds = None, set(), set()
        for line in out.splitlines():
            if line.startswith("f"):
                cur = line[1:]
            elif line.startswith("n/"):
                (cwds if cur == "cwd" else paths).add(line[1:])
        self.paths, self.cwds = sorted(paths), sorted(cwds)
        self.cmds = ps.splitlines()
        self.ok = bool(paths or cwds)

    @staticmethod
    # Tell whether a sorted path list contains the root or anything beneath it.
    def _under(sorted_list, root: str) -> bool:
        i = bisect.bisect_left(sorted_list, root)
        if i < len(sorted_list) and sorted_list[i] == root:
            return True
        j = bisect.bisect_left(sorted_list, root + "/")
        return j < len(sorted_list) and sorted_list[j].startswith(root + "/")

    # Tell whether a running process uses the candidate or its project directory.
    def uses(self, cand: str) -> bool:
        parent = os.path.dirname(cand)
        return (self._under(self.paths, cand) or self._under(self.cwds, parent)
                or any(parent + "/" in cmd or cmd.endswith(parent) for cmd in self.cmds))


# Return (ok, reason) for whether a build artifact directory is safe to delete.
def verify_artifact(cand: str, cutoff: float, snap: Snapshot):
    name, parent = os.path.basename(cand), os.path.dirname(cand)
    if os.path.islink(cand) or not os.path.isdir(cand):
        return False, "not a real directory"
    need = ARTIFACTS[name]
    if need[0] and not _has_any(parent, need[0]):
        return False, "no %s next to it" % "/".join(need[0])
    if need[1] and not _has_any(parent, need[1]):
        return False, "no lockfile - not provably regenerable"
    if need[2] and not any(os.path.exists(os.path.join(cand, x)) for x in need[2]):
        return False, "contents not recognised as generated"
    if name == "__pycache__":
        for f in os.listdir(cand):
            if not f.endswith((".pyc", ".pyo")):
                return False, "contains non-.pyc file"
    r = git(parent, "rev-parse", "--is-inside-work-tree")
    if r.returncode != 0:
        return False, "not inside a git work tree"
    if git(parent, "check-ignore", "-q", "--", name).returncode != 0:
        return False, "not ignored by git"
    r = git(parent, "ls-files", "-z", "--", name)
    if r.returncode != 0 or r.stdout:
        return False, "git tracks files inside"
    _size, newest, _n = tree_size(cand)
    if newest >= cutoff:
        return False, "modified today (Atlantic/Reykjavik)"
    if not snap.ok:
        return False, "could not snapshot running processes"
    if snap.uses(cand):
        return False, "in use by a running process/session"
    return True, ""


# Yield known build-artifact directories under a root without descending into them.
def find_artifacts(root: str):
    for dp, dns, _fns in os.walk(root, followlinks=False):
        keep = []
        for dn in dns:
            full = os.path.join(dp, dn)
            if dn in ARTIFACTS:
                yield full  # do not descend
            elif dn != ".git":
                keep.append(dn)
        dns[:] = keep


# Delete verified, old, unused build artifacts under the dev root.
def clean_dev(apply: bool, ctx: dict, dev_root: str) -> None:
    log(c("1", "Dev build artifacts under %s" % dev_root))
    root = os.path.realpath(os.path.expanduser(dev_root))
    if not os.path.isdir(root):
        log("  - not present")
        return
    if not shutil.which("git"):
        log("  - git not available; skipping")
        return
    cutoff = today_start_ts()
    log("  keeping anything modified since %s (Atlantic/Reykjavik)"
        % datetime.datetime.fromtimestamp(cutoff, datetime.timezone.utc).strftime("%Y-%m-%d 00:00"))
    log("  snapshotting running processes ...")
    snap = Snapshot()
    tk = Ticker()
    cands = []
    for cand in find_artifacts(root):
        cands.append(cand)
        tk.tick("  found %d candidate directories, at %s" % (len(cands), cand))
    tk.clear()
    kept = 0
    for cand in cands:
        if STOP:
            break
        ok, why = verify_artifact(cand, cutoff, snap)
        size = tree_size(cand)[0] if ok or apply is False else 0
        if not ok:
            kept += 1
            if why in ("modified today (Atlantic/Reykjavik)", "in use by a running process/session"):
                log("  keep   %s  [%s]" % (cand, why))
            else:
                ctx["uncertain"].append("%s [%s]" % (cand, why))
            continue
        if not apply:
            log("  ~ would delete %10s  %s" % (human(size), cand))
            ctx["bytes"] += size
            continue
        ok2, why2 = verify_artifact(cand, cutoff, snap)  # recheck right before deleting
        if not ok2:
            ctx["uncertain"].append("%s [changed before delete: %s]" % (cand, why2))
            continue
        res = {"bytes": 0, "files": 0, "scanned": 0, "protected": []}
        rm_tree(cand, res, tk, True)
        tk.clear()
        ctx["bytes"] += res["bytes"]
        ctx["protected"].extend(res["protected"])
        log("  - deleted %10s  %s%s" % (human(res["bytes"]), cand,
                                         "  (%d protected)" % len(res["protected"]) if res["protected"] else ""))
    log("  %d candidates examined, %d kept/uncertain" % (len(cands), kept))


# ---- docker


# Tell whether Docker Desktop is already running and responding.
def docker_running() -> bool:
    if not shutil.which("docker"):
        return False
    if subprocess.run(["pgrep", "-fl", "Docker Desktop|com.docker.backend"], capture_output=True).returncode != 0:
        return False
    try:
        return subprocess.run(["docker", "info", "--format", "{{.ServerVersion}}"], capture_output=True,
                              timeout=20).returncode == 0
    except subprocess.SubprocessError:
        return False


# Prune images and build cache, stop containers and quit Docker only if it is already running.
def clean_docker(apply: bool, ctx: dict, stop_timeout: int) -> dict:
    status = {"state": "not running (left alone)"}
    log(c("1", "Docker"))
    if not docker_running():
        log("  - Docker is not running; not starting it. Nothing done.")
        return status
    if not apply:
        log("  ~ would: docker image prune -a -f; docker builder prune -a -f; stop all containers "
            "(%ds grace); quit Docker Desktop. Containers and volumes are never deleted." % stop_timeout)
        status["state"] = "running (dry run)"
        return status
    for label, cmd in (("image prune", ["docker", "image", "prune", "-a", "-f"]),
                       ("builder prune", ["docker", "builder", "prune", "-a", "-f"])):
        code, out = run_cmd(cmd, label)
        tail = out.strip().splitlines()[-1] if out.strip() else ""
        log("  - %s: %s" % (label, tail if code == 0 else "failed: " + out.strip()[-200:]))
        if code != 0:
            ctx["failures"].append("docker " + label)
    ids = subprocess.run(["docker", "ps", "-q"], capture_output=True, text=True).stdout.split()
    if ids:
        code, out = run_cmd(["docker", "stop", "-t", str(stop_timeout)] + ids, "stopping %d containers" % len(ids),
                            timeout=stop_timeout * len(ids) + 120)
        log("  - stopped %d containers gracefully: %s" % (len(ids), "ok" if code == 0 else "FAILED " + out[-200:]))
        if code != 0:
            ctx["failures"].append("docker stop")
            status["state"] = "still running (container stop failed; Docker left running)"
            return status
    run_cmd(["/usr/bin/osascript", "-e", 'tell application "Docker" to quit'], "quit Docker Desktop", timeout=120)
    deadline = time.time() + 180
    while time.time() < deadline and docker_running():
        time.sleep(3)
    if docker_running():
        status["state"] = "STILL RUNNING after quit request"
        ctx["failures"].append("docker did not shut down")
    else:
        status["state"] = "shut down"
    log("  - Docker Desktop: %s" % status["state"])
    return status


# Run the selected cleanup targets and print a summary.
def cmd_clean(args) -> None:
    targets = list(args.targets)
    if "all" in targets:
        targets = ["caches", "downloads", "trash", "dev", "docker"] + [t for t in targets if t in ("brew", "xcode")]
    order = ["caches", "xcode", "downloads", "trash", "dev", "brew", "docker"]
    targets = [t for t in order if t in targets]
    apply = args.apply
    log(c("1", "free_disk_space clean") + (" - APPLYING" if apply else " - DRY RUN (add --apply to delete)"))
    if apply and "downloads" in targets:
        log(c("33", "Note: ~/Downloads will be emptied completely, including hidden files."))
    install_sigint()
    free0 = report_free("Initial")
    ctx = {"bytes": 0, "protected": [], "failures": [], "uncertain": []}
    docker_status = None
    t0 = time.time()
    for t in targets:
        if STOP:
            break
        if t == "caches":
            clean_dirs("Caches", CACHE_DIRS, apply, ctx)
        elif t == "xcode":
            clean_dirs("Xcode derived data", XCODE_DIRS, apply, ctx)
        elif t == "downloads":
            clean_dirs("Downloads", ["~/Downloads"], apply, ctx)
        elif t == "trash":
            clean_trash(apply, ctx)
        elif t == "dev":
            clean_dev(apply, ctx, args.dev_root)
        elif t == "brew":
            clean_brew(apply, ctx)
        elif t == "docker":
            docker_status = clean_docker(apply, ctx, args.docker_stop_timeout)
        log("  [elapsed %s | free now %s]" % (hms(time.time() - t0), human(free_bytes())))
    free1 = report_free("Final  ")
    log(c("1", "Summary"))
    log("  %s (sum of file sizes seen): %s" % ("deleted" if apply else "would delete", human(ctx["bytes"])))
    if apply:
        log("  actual free-space change on volume: %s  (differs from the sum: snapshots, shared blocks, "
            "concurrent writers)" % human(free1 - free0))
    if docker_status:
        log("  docker: %s" % docker_status["state"])
    for title, key in (("protected / skipped entries", "protected"), ("failures", "failures"),
                       ("uncertain, left untouched", "uncertain")):
        items = ctx[key]
        if items:
            log("  %s (%d):" % (title, len(items)))
            for x in items[:30]:
                log("    - " + x)
            if len(items) > 30:
                log("    ... and %d more" % (len(items) - 30))
    if not apply:
        log("Nothing was changed. Re-run with --apply.")
    log("Next: consider `new` + `compress` for transparent compression of inactive files.")


# --------------------------------------------------------------------------- dedupe

CLONE_NOFOLLOW = 0x1
COPYFILE_METADATA = 0x7  # ACL | stat | xattrs
COPYFILE_NOFOLLOW = 0xC0000
SYSTEM_PREFIXES = SKIP_PREFIXES + ("/Library", "/Applications", "/private", "/etc", "/var", "/tmp")
NO_CLONE_FLAGS = UF_COMPRESSED | UF_IMMUTABLE | UF_APPEND | SF_IMMUTABLE | SF_DATALESS


# Tell whether a file is not one of the invoking user's own files.
def is_system_file(path: str, st: os.stat_result) -> bool:
    if st.st_uid == 0 or st.st_uid != os.getuid():
        return True
    return any(path == p or path.startswith(p + "/") for p in SYSTEM_PREFIXES)


# Call a libc function that returns 0 on success, raising OSError otherwise.
def libc_call(name: str, path: str, *call_args) -> None:
    libc = ctypes.CDLL(None, use_errno=True)
    if getattr(libc, name)(*call_args) != 0:
        err = ctypes.get_errno()
        raise OSError(err, "%s: %s" % (name, os.strerror(err)), path)


# Clone a file copy-on-write on APFS, failing instead of falling back to a full copy.
def clone_file(src: str, dst: str) -> None:
    libc_call("clonefile", dst, os.fsencode(src), os.fsencode(dst), CLONE_NOFOLLOW)


# Make a file's ACL, mode, owner, flags, times and xattrs match another file's.
def copy_metadata(src: str, dst: str) -> None:
    libc_call("copyfile", dst, os.fsencode(src), os.fsencode(dst), None, COPYFILE_METADATA | COPYFILE_NOFOLLOW)
    for name in xattr_names(dst) - xattr_names(src):
        subprocess.run(["/usr/bin/xattr", "-d", name, dst], check=True, capture_output=True, timeout=60)


# Return (path, stat, replaceable) for the files of one name group that still match the scan.
def dedupe_members(rows, size: int, args, snap: Snapshot) -> list:
    members = []
    for path, cloned in rows:
        try:
            st = os.lstat(path)
        except OSError:
            continue
        if not stat.S_ISREG(st.st_mode) or st.st_size != size or getattr(st, "st_flags", 0) & NO_CLONE_FLAGS:
            continue
        if not args.include_system and is_system_file(path, st):
            continue
        replaceable = (
            not cloned and st.st_nlink == 1 and os.access(path, os.W_OK)
            and os.access(os.path.dirname(path), os.W_OK | os.X_OK)
            and not snap._under(snap.paths, path)
        )
        members.append((path, st, replaceable))
    return members


# Hash a name group and return (digest, keeper path, targets) per set of identical files on one volume.
def dedupe_plan(members: list) -> list:
    if len(members) < 2 or not any(m[2] for m in members):
        return []
    by_content = {}
    for member in members:
        try:
            digest = sha256(member[0])
        except OSError:
            continue
        by_content.setdefault((member[1].st_dev, digest), []).append(member)
    plans = []
    for (_dev, digest), same in by_content.items():
        # Keep a file that cannot be replaced if there is one, else the oldest.
        same.sort(key=lambda m: (m[2], m[1].st_mtime_ns, m[0]))
        targets = [m for m in same[1:] if m[2]]
        if targets:
            plans.append((digest, same[0][0], targets))
    return plans


# Swap one duplicate for a verified clone of the keeper and return (ok, note).
def clone_over(keeper: str, target: str, st: os.stat_result, digest: str):
    tmp = os.path.join(os.path.dirname(target), ".fds-%d-%s.tmp" % (os.getpid(), os.path.basename(target)[:80]))
    try:
        clone_file(keeper, tmp)
        copy_metadata(target, tmp)
        os.utime(tmp, ns=(st.st_atime_ns, st.st_mtime_ns))
        t = os.lstat(tmp)
        if (stat.S_IMODE(t.st_mode), t.st_uid, t.st_gid) != (stat.S_IMODE(st.st_mode), st.st_uid, st.st_gid):
            return False, "permissions/ownership differ"
        if xattr_names(target) != xattr_names(tmp):
            return False, "extended attributes differ"
        if sha256(tmp) != digest:
            return False, "content hash mismatch"
        st2 = os.lstat(target)
        if (st2.st_size, st2.st_mtime_ns, st2.st_ino, st2.st_nlink) != (st.st_size, st.st_mtime_ns, st.st_ino, 1):
            return False, "changed during dedupe"
        os.replace(tmp, target)
        return True, ""
    except (OSError, subprocess.SubprocessError) as e:
        return False, str(e)
    finally:
        if os.path.lexists(tmp):
            try:
                os.remove(tmp)
            except OSError:
                pass


# Clone or preview the duplicates of one name group, updating the tally, and return False to stop.
def dedupe_group(db: sqlite3.Connection, members: list, args, tally: dict) -> bool:
    for digest, keeper, targets in dedupe_plan(members):
        for target, st, _replaceable in targets:
            if STOP or (args.sample and tally["files"] >= args.sample):
                return False
            if not args.apply:
                ok, note = True, ""
            else:
                ok, note = clone_over(keeper, target, st, digest)
            if not ok:
                tally["failed"] += 1
                tally["streak"] += 1
                log("%s %s: %s" % (c("31", "FAILED"), target, note))
                if tally["streak"] >= 3:
                    log(c("31", "3 consecutive failures - aborting for safety."))
                    return False
                continue
            if args.apply:
                db.execute("UPDATE files SET cloned=1 WHERE path=?", (target,))
                db.commit()
            tally["streak"] = 0
            tally["files"] += 1
            tally["bytes"] += st.st_blocks * 512
            if args.apply or tally["files"] <= 50:
                log("%s %10s  %s <- %s" % ("cloned" if args.apply else "~ would clone",
                                           human(st.st_blocks * 512), target, keeper))
    return True


# Replace byte-identical same-named files with copy-on-write clones of one kept copy.
def cmd_dedupe(args) -> None:
    if args.apply and not IS_MAC and not os.environ.get("FDS_FORCE"):
        die("dedupe --apply requires macOS (APFS clonefile)")
    db = open_db(args.state, False)
    if not db.execute("SELECT 1 FROM files LIMIT 1").fetchone():
        die("no file names recorded in this scan state; run `new` to rescan")
    if not meta_get(db, "finished"):
        warn("scan is not complete; deduplicating from the file names found so far")
    groups = db.execute(
        "SELECT name, size FROM files WHERE size >= ? GROUP BY name, size HAVING COUNT(*) > 1 ORDER BY size DESC",
        (max(parse_size(args.min_size), 1),),
    ).fetchall()
    log(c("1", "free_disk_space dedupe") + (" - APPLYING" if args.apply else " - DRY RUN (add --apply to clone)"))
    log("%d groups of same-named, same-sized files to hash%s"
        % (len(groups), "" if args.include_system else " (system files excluded)"))
    install_sigint()
    snap = Snapshot()
    if args.apply and not snap.ok:
        die("could not snapshot open files with lsof; nothing was changed")
    free0 = report_free("Initial")
    tk = Ticker()
    tally = {"files": 0, "bytes": 0, "failed": 0, "streak": 0}
    t0 = time.time()
    for i, (name, size) in enumerate(groups, 1):
        tk.tick("[%d/%d] %s %s | %d duplicates, %s | %s"
                % (i, len(groups), human(size), name[-40:], tally["files"], human(tally["bytes"]),
                   hms(time.time() - t0)))
        rows = db.execute("SELECT path, cloned FROM files WHERE name=? AND size=?", (name, size)).fetchall()
        tk.clear()
        if not dedupe_group(db, dedupe_members(rows, size, args, snap), args, tally):
            break
    tk.clear()
    free1 = report_free("Final  ")
    log("%s %d duplicate files, failed %d in %s" % ("Cloned" if args.apply else "Would clone", tally["files"],
                                                   tally["failed"], hms(time.time() - t0)))
    log("Allocated size of the replaced duplicates (upper bound on savings): %s" % c("1", human(tally["bytes"])))
    if not args.apply:
        log("Nothing was changed. Re-run with --apply (try --sample 5 first).")
        return
    log("Overall free-space change on the volume: %s" % c("1", human(free1 - free0)))
    log("(These can differ: files that already shared blocks, snapshots and other writers affect free space.)")


# --------------------------------------------------------------------------- CLI


# Build the command-line parser.
def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        prog="free_disk_space.py",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        description="Reclaim disk space on macOS: resumable scan, APFS compression, clone dedupe, cleanups.",
        epilog="""examples:
  free_disk_space.py clean all                    # dry run of cache/downloads/trash/dev/docker cleanup
  free_disk_space.py clean caches dev --apply     # actually clean just those
  free_disk_space.py new --root ~ --min-size 5M   # start a compression scan (Ctrl-C safe)
  free_disk_space.py resume                       # continue it later
  free_disk_space.py status                       # progress, candidates, free space
  free_disk_space.py dedupe                       # dry run: same-named identical files to clone
  free_disk_space.py dedupe --sample 5 --apply    # replace 5 duplicates with APFS clones
  free_disk_space.py compress --sample 5          # verify on 5 files first
  free_disk_space.py compress --limit 20G --yes   # then go bigger

state file: $FDS_STATE or ~/.local/state/free-disk-space/scan.sqlite3""",
    )
    p.add_argument("--version", action="version", version=VERSION)
    sub = p.add_subparsers(dest="cmd", metavar="command")
    sub.required = True

    # Add the shared --state option to a subcommand.
    def common(sp):
        sp.add_argument("--state", default=DEFAULT_STATE, help="scan state file (default: %(default)s)")

    n = sub.add_parser("new", help="start a new compression scan", description="Start a fresh resumable scan.")
    common(n)
    n.add_argument("--root", default=HOME, help="where to scan (default: your home; /System/Volumes/Data for all)")
    n.add_argument("--min-size", default="1M", help="smallest file to consider (default 1M)")
    n.add_argument("--min-age-days", type=int, default=14, help="only files untouched this long (default 14)")
    n.add_argument("--min-savings-pct", type=int, default=25, help="min sampled compressibility %% (default 25)")
    n.add_argument("--cross-devices", action="store_true", help="also descend into other filesystems")
    n.add_argument("--exclude", action="append", default=[], metavar="PATH", help="skip this directory (repeatable)")
    n.add_argument("--force", action="store_true", help="discard an unfinished scan")
    n.set_defaults(fn=cmd_new)

    r = sub.add_parser("resume", help="continue an interrupted scan")
    common(r)
    r.set_defaults(fn=cmd_resume)

    s = sub.add_parser("status", help="show scan progress, candidates and free space")
    common(s)
    s.set_defaults(fn=cmd_status)

    l = sub.add_parser("list", help="list best candidates")
    common(l)
    l.add_argument("--top", type=int, default=30)
    l.add_argument("--status", default="pending", choices=["pending", "done", "skipped", "failed"])
    l.set_defaults(fn=cmd_list)

    k = sub.add_parser(
        "compress", help="compress candidates in place (verified)",
        description="Each file is recompressed via `ditto --hfsCompression` into a temp file in the same "
                    "directory, checked (flags, size, mode, owner, xattrs, SHA-256, allocated size), then "
                    "atomically swapped in. Skips hard-linked, open, recently changed and flagged files.")
    common(k)
    k.add_argument("--sample", type=int, metavar="N", help="only process the N best files, then stop")
    k.add_argument("--max-files", type=int)
    k.add_argument("--limit", metavar="SIZE", help="stop once this much estimated savings is planned (e.g. 20G)")
    k.add_argument("--min-age-days", type=int, default=14, help="re-check file is this old (default 14)")
    k.add_argument("--min-gain", type=parse_size, default=64 * 1024, help="min verified gain per file (default 64K)")
    k.add_argument("--reserve", type=parse_size, default=2 << 30,
                   help="extra free space to keep beyond the temp copy (default 2G)")
    k.add_argument("--dry-run", action="store_true")
    k.add_argument("-y", "--yes", action="store_true", help="don't ask for confirmation")
    k.set_defaults(fn=cmd_compress)

    d = sub.add_parser(
        "dedupe", help="replace identical same-named files with APFS clones (dry run unless --apply)",
        description="Files recorded by the scan that share a leaf name and size are SHA-256 hashed when at "
                    "least one is writable. Identical ones are replaced by copy-on-write clones of one kept "
                    "copy (like `cp -c`), keeping each file's own metadata. Skips system files (not owned by "
                    "you, or under system paths), hard-linked, open, compressed and flagged files.")
    common(d)
    d.add_argument("--apply", action="store_true", help="actually replace duplicates (default is a dry run)")
    d.add_argument("--include-system", action="store_true",
                   help="also process files not owned by you or under system paths such as /Library")
    d.add_argument("--min-size", default="1", help="smallest file to consider (default 1 byte)")
    d.add_argument("--sample", type=int, metavar="N", help="only process N duplicates, then stop")
    d.set_defaults(fn=cmd_dedupe)

    cl = sub.add_parser(
        "clean", help="delete regenerable files (dry run unless --apply)",
        description="Targets: caches, downloads, trash, dev, docker (all of these = `all`), plus opt-in brew, xcode.")
    cl.add_argument("targets", nargs="+",
                    choices=["all", "caches", "downloads", "trash", "dev", "docker", "brew", "xcode"])
    cl.add_argument("--apply", action="store_true", help="actually delete (default is a dry run)")
    cl.add_argument("--dev-root", default="~/dev", help="project tree for `dev` (default ~/dev)")
    cl.add_argument("--docker-stop-timeout", type=int, default=30, help="graceful stop seconds (default 30)")
    cl.set_defaults(fn=cmd_clean)
    return p


# Parse arguments and dispatch to the chosen command.
def main() -> None:
    args = build_parser().parse_args()
    try:
        args.fn(args)
    except KeyboardInterrupt:
        print("\naborted", file=sys.stderr)
        sys.exit(130)
    except BrokenPipeError:
        pass


if __name__ == "__main__":
    main()
