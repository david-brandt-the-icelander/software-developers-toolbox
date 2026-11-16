// The `zazzle-git` program behind `zazzle git`: help, version, message, commit, and everything else forwarded to git.
fn main() {
    std::process::exit(zazzle_git::router::main(&zazzle_git::args()));
}
