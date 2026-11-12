// Reclaim macOS disk space via a resumable scan, compression, clone dedupe and safe cleanups.
mod clean;
mod compress;
mod dedupe;
mod scan;
#[cfg(test)]
mod testkit;
mod util;

use clap::{Args, Parser, Subcommand, ValueEnum};

// Return the default scan state path.
fn default_state() -> String {
    std::env::var("FDS_STATE")
        .unwrap_or_else(|_| format!("{}/.local/state/free-disk-space/scan.sqlite3", util::home()))
}

// Parse a size argument for clap.
fn size_arg(text: &str) -> Result<i64, String> {
    util::parse_size(text)
}

const EXAMPLES: &str = "examples:
  free-disk-space clean all                    # dry run of cache/downloads/trash/dev/docker cleanup
  free-disk-space clean caches dev --apply     # actually clean just those
  free-disk-space new --root ~ --min-size 5M   # start a compression scan (Ctrl-C safe)
  free-disk-space resume                       # continue it later
  free-disk-space status                       # progress, candidates, free space
  free-disk-space dedupe                       # dry run: same-named identical files to clone
  free-disk-space dedupe --sample 5 --apply    # replace 5 duplicates with APFS clones
  free-disk-space compress --sample 5          # verify on 5 files first
  free-disk-space compress --limit 20G --yes   # then go bigger

state file: $FDS_STATE or ~/.local/state/free-disk-space/scan.sqlite3";

// Command-line interface.
#[derive(Parser)]
#[command(
    version,
    about = "Reclaim disk space on macOS: resumable scan, APFS compression, clone dedupe, cleanups.",
    after_help = EXAMPLES
)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

// Shared --state option.
#[derive(Args)]
struct StateArg {
    /// scan state file
    #[arg(long, default_value_t = default_state())]
    state: String,
}

// Available subcommands.
#[derive(Subcommand)]
enum Cmd {
    /// start a new compression scan
    New(NewArgs),
    /// continue an interrupted scan
    Resume(StateArg),
    /// show scan progress, candidates and free space
    Status(StateArg),
    /// list best candidates
    List(ListArgs),
    /// compress candidates in place (verified)
    ///
    /// Each file is recompressed via `ditto --hfsCompression` into a temp file in the same directory,
    /// checked (flags, size, mode, owner, xattrs, SHA-256, allocated size), then atomically swapped
    /// in. Skips hard-linked, open, recently changed and flagged files.
    Compress(CompressArgs),
    /// replace identical same-named files with APFS clones (dry run unless --apply)
    ///
    /// Files recorded by the scan that share a leaf name and size are SHA-256 hashed when at least
    /// one is writable. Identical ones are replaced by copy-on-write clones of one kept copy (like
    /// `cp -c`), keeping each file's own metadata. Skips system files (not owned by you, or under
    /// system paths), hard-linked, open, compressed and flagged files.
    Dedupe(DedupeArgs),
    /// delete regenerable files (dry run unless --apply)
    ///
    /// Targets: caches, downloads, trash, dev, docker (all of these = `all`), plus opt-in brew, xcode.
    Clean(CleanArgs),
}

// Options for new.
#[derive(Args)]
struct NewArgs {
    #[command(flatten)]
    state: StateArg,
    /// where to scan (default: your home; /System/Volumes/Data for all)
    #[arg(long, default_value_t = util::home())]
    root: String,
    /// smallest file to consider
    #[arg(long, default_value = "1M")]
    min_size: String,
    /// only files untouched this long
    #[arg(long, default_value_t = 14)]
    min_age_days: i64,
    /// min sampled compressibility %
    #[arg(long, default_value_t = 25)]
    min_savings_pct: i64,
    /// also descend into other filesystems
    #[arg(long)]
    cross_devices: bool,
    /// skip this directory (repeatable)
    #[arg(long, value_name = "PATH")]
    exclude: Vec<String>,
    /// discard an unfinished scan
    #[arg(long)]
    force: bool,
}

// Candidate states for list.
#[derive(Clone, Copy, ValueEnum)]
enum Status {
    Pending,
    Done,
    Skipped,
    Failed,
}

// Options for list.
#[derive(Args)]
struct ListArgs {
    #[command(flatten)]
    state: StateArg,
    #[arg(long, default_value_t = 30)]
    top: i64,
    #[arg(long, value_enum, default_value = "pending")]
    status: Status,
}

// Options for compress.
#[derive(Args)]
struct CompressArgs {
    #[command(flatten)]
    state: StateArg,
    /// only process the N best files, then stop
    #[arg(long, value_name = "N")]
    sample: Option<usize>,
    #[arg(long)]
    max_files: Option<usize>,
    /// stop once this much estimated savings is planned (e.g. 20G)
    #[arg(long, value_name = "SIZE")]
    limit: Option<String>,
    /// re-check file is this old
    #[arg(long, default_value_t = 14)]
    min_age_days: i64,
    /// min verified gain per file
    #[arg(long, value_parser = size_arg, default_value = "64K")]
    min_gain: i64,
    /// extra free space to keep beyond the temp copy
    #[arg(long, value_parser = size_arg, default_value = "2G")]
    reserve: i64,
    #[arg(long)]
    dry_run: bool,
    /// don't ask for confirmation
    #[arg(short, long)]
    yes: bool,
}

// Options for dedupe.
#[derive(Args)]
struct DedupeArgs {
    #[command(flatten)]
    state: StateArg,
    /// actually replace duplicates (default is a dry run)
    #[arg(long)]
    apply: bool,
    /// also process files not owned by you or under system paths such as /Library
    #[arg(long)]
    include_system: bool,
    /// smallest file to consider
    #[arg(long, default_value = "1")]
    min_size: String,
    /// only process N duplicates, then stop
    #[arg(long, value_name = "N")]
    sample: Option<u64>,
}

// Cleanup targets.
#[derive(Clone, Copy, ValueEnum)]
enum Target {
    All,
    Caches,
    Downloads,
    Trash,
    Dev,
    Docker,
    Brew,
    Xcode,
}

// Options for clean.
#[derive(Args)]
struct CleanArgs {
    #[arg(required = true, value_enum)]
    targets: Vec<Target>,
    /// actually delete (default is a dry run)
    #[arg(long)]
    apply: bool,
    /// project tree for `dev`
    #[arg(long, default_value = "~/dev")]
    dev_root: String,
    /// graceful stop seconds
    #[arg(long, default_value_t = 30)]
    docker_stop_timeout: u64,
}

// Return the lowercase name of a clap value.
fn value_name<T: ValueEnum>(v: T) -> String {
    v.to_possible_value().map(|p| p.get_name().to_string()).unwrap_or_default()
}

// Parse arguments and dispatch to the chosen command.
fn main() {
    run(Cli::parse());
}

// Dispatch a parsed command line to the chosen command.
fn run(cli: Cli) {
    match cli.command {
        Cmd::New(a) => scan::cmd_new(&scan::NewOpts {
            state: a.state.state,
            root: a.root,
            min_size: a.min_size,
            min_age_days: a.min_age_days,
            min_savings_pct: a.min_savings_pct,
            cross_devices: a.cross_devices,
            exclude: a.exclude,
            force: a.force,
        }),
        Cmd::Resume(a) => scan::cmd_resume(&a.state),
        Cmd::Status(a) => scan::cmd_status(&a.state),
        Cmd::List(a) => scan::cmd_list(&a.state.state, &value_name(a.status), a.top),
        Cmd::Compress(a) => compress::cmd_compress(&compress::CompressOpts {
            state: a.state.state,
            sample: a.sample,
            max_files: a.max_files,
            limit: a.limit,
            min_age_days: a.min_age_days,
            min_gain: a.min_gain,
            reserve: a.reserve,
            dry_run: a.dry_run,
            yes: a.yes,
        }),
        Cmd::Dedupe(a) => dedupe::cmd_dedupe(&dedupe::DedupeOpts {
            state: a.state.state,
            apply: a.apply,
            include_system: a.include_system,
            min_size: a.min_size,
            sample: a.sample,
        }),
        Cmd::Clean(a) => {
            let targets: Vec<String> = a.targets.into_iter().map(value_name).collect();
            clean::cmd_clean(&targets, a.apply, &a.dev_root, a.docker_stop_timeout);
        }
    }
}

// Unit tests for argument parsing and dispatch.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::Sandbox;

    // Parse a command line and run it.
    fn go(args: &[&str]) {
        let mut full = vec!["free-disk-space"];
        full.extend_from_slice(args);
        run(Cli::try_parse_from(full).expect("valid arguments"));
    }

    // Check the default state path honours FDS_STATE and falls back to the home directory.
    #[test]
    fn test_default_state_paths() {
        let sb = Sandbox::new();
        assert_eq!(default_state(), sb.state());
        std::env::remove_var("FDS_STATE");
        assert!(default_state().ends_with("/.local/state/free-disk-space/scan.sqlite3"));
    }

    // Check size and value-name helpers.
    #[test]
    fn test_size_arg_and_value_name() {
        assert_eq!(size_arg("2K"), Ok(2048));
        assert!(size_arg("nope").is_err());
        assert_eq!(value_name(Target::Docker), "docker");
        assert_eq!(value_name(Status::Pending), "pending");
    }

    // Check invalid command lines are rejected by the parser.
    #[test]
    fn test_cli_rejects_bad_arguments() {
        let _sb = Sandbox::new();
        assert!(Cli::try_parse_from(["fds"]).is_err());
        assert!(Cli::try_parse_from(["fds", "clean"]).is_err());
        assert!(Cli::try_parse_from(["fds", "compress", "--min-gain", "x"]).is_err());
    }

    // Check every subcommand dispatches to its implementation.
    #[test]
    fn test_run_dispatches_every_command() {
        let sb = Sandbox::new();
        std::env::set_var("FDS_FORCE", "1");
        let root = sb.dir("root");
        sb.write("root/a/f", &vec![b'a'; 1 << 20]);
        let root = root.to_str().unwrap();
        go(&[
            "new",
            "--root",
            root,
            "--min-size",
            "1K",
            "--min-age-days",
            "0",
            "--exclude",
            "/nowhere",
            "--force",
        ]);
        go(&["resume"]);
        go(&["status"]);
        go(&["list", "--top", "3", "--status", "done"]);
        go(&["compress", "--dry-run", "--sample", "1", "--yes", "--min-gain", "1", "--reserve", "0"]);
        go(&["dedupe", "--min-size", "1", "--sample", "1", "--include-system"]);
        let home = sb.home().to_string_lossy().into_owned();
        go(&["clean", "caches", "trash", "--dev-root", &home]);
    }
}
