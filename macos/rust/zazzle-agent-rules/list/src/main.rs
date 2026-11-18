// The `zazzle-agent` program behind `zazzle agent`: runs list-rules and exits with its status.
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let (text, code) = zazzle_agent_rules_list::run(&args, home.as_deref());
    if code == 0 {
        print!("{text}");
    } else {
        eprint!("{text}");
    }
    std::process::exit(code);
}
