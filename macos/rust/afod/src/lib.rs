// Library root for the afo daemon: engine, commands, wire protocol and front ends.
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

pub mod args;
pub mod cli;
pub mod client;
pub mod cmd;
pub mod engine;
pub mod launchd;
pub mod mcp;
pub mod meta;
pub mod proto;
pub mod server;
pub mod tokens;
pub mod util;
pub mod walk;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub mod testutil;
