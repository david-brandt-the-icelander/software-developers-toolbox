// Library root for the zazzle daemon: engine, commands, wire protocol and front ends.
#![cfg_attr(all(coverage_nightly, test), feature(coverage_attribute))]

pub mod args;
pub mod cli;
pub mod client;
pub mod cmd;
pub mod engine;
pub mod launchd;
pub mod mcp;
pub mod meta;
pub mod outline;
pub mod proto;
pub mod server;
pub mod session;
pub mod store;
pub mod tokens;
pub mod util;
pub mod walk;
pub mod zazz;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub mod testutil;
