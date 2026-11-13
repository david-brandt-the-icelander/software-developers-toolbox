// The zazzled binary: a thin wrapper over the library entry point.
use std::process::ExitCode;

// Runs the daemon, MCP server or installer depending on the arguments.
fn main() -> ExitCode {
    zazzled::cli::main(std::env::args().skip(1).collect())
}
