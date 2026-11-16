// The `zazzle-git-commit` program, the same as `zazzle git commit`.
fn main() {
    std::process::exit(zazzle_git::commit::main(&zazzle_git::args()));
}
