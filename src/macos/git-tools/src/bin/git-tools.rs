// The `git-tools` program behind `zazzle git`: help, version, message, commit, and everything else forwarded to git.
fn main() {
    std::process::exit(git_tools::router::main(&git_tools::args()));
}
