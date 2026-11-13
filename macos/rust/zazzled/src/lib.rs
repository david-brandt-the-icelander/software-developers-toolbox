// Library root for the zazzle daemon: engine, commands, wire protocol and front ends.
#![cfg_attr(all(coverage_nightly, test), feature(coverage_attribute))]

pub mod arena;
pub mod args;
pub mod cli;
pub mod client;
pub mod cmd;
pub mod engine;
pub mod indexer;
pub mod launchd;
pub mod markup;
pub mod mcp;
pub mod meta;
pub mod outline;
pub mod outputs;
pub mod params;
pub mod pool;
pub mod proto;
pub mod proxy;
pub mod server;
pub mod session;
pub mod store;
pub mod symbols;
pub mod tokens;
pub mod util;
pub mod walk;
pub mod wrap;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub mod testutil;
