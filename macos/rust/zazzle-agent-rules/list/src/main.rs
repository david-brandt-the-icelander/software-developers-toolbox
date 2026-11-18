// The `zazzle-agent` program behind `zazzle agent`: runs get-rules and exits with its status, without panicking on a closed pipe or a non-UTF-8 argument.
use std::io::Write;

// Reads the arguments and HOME, runs the command from `/` and prints its output.
fn main() {
    let mut args = Vec::new();
    for a in std::env::args_os().skip(1) {
        let Ok(a) = a.into_string() else {
            eprintln!("arguments must be valid UTF-8");
            std::process::exit(2);
        };
        args.push(a);
    }
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let (text, code) = zazzle_agent_rules_list::run(&args, home.as_deref(), std::path::Path::new("/"));
    // Output errors (a closed pipe) are ignored: the reader has gone away.
    let _ = if code == 0 {
        std::io::stdout().lock().write_all(text.as_bytes())
    } else {
        std::io::stderr().lock().write_all(text.as_bytes())
    };
    std::process::exit(code);
}
