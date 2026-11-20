// shell-tools: the JSON file operations the daemon runs for `zazzle call` (read, search, list, stat, outline, diff_since), with per-session memory of what each session was sent.
#![cfg_attr(all(coverage_nightly, test), feature(coverage_attribute))]
pub mod diff;
pub mod engine;
pub mod fsio;
pub mod host;
pub mod list;
pub mod outline;
pub mod read;
pub mod req;
pub mod search;
pub mod state;
