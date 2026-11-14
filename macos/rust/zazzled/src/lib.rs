// Library root for the zazzle daemon: engine, commands, wire protocol and front ends.
#![cfg_attr(all(coverage_nightly, test), feature(coverage_attribute))]

pub mod arena;
pub mod args;
pub mod buildfile;
pub mod cli;
pub mod client;
pub mod cmd;
pub mod cobol;
pub mod config;
pub mod data;
pub mod engine;
pub mod indexer;
pub mod launchd;
pub mod locals;
pub mod markup;
pub mod mcp;
pub mod mcp_structure;
pub mod md;
pub mod meta;
pub mod minified;
pub mod modules;
pub mod nim;
pub mod outline;
pub mod outputs;
pub mod params;
pub mod pool;
pub mod proto;
pub mod proxy;
pub mod schema;
pub mod server;
pub mod session;
pub mod sql;
pub mod store;
pub mod styles;
pub mod symbols;
pub mod terse;
pub mod tokens;
pub mod util;
pub mod walk;
pub mod wrap;
pub mod xml;
pub mod zazz;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub mod testutil;
