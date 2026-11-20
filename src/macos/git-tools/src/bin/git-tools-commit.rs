// The `git-tools-commit` program, the same as `zazzle git commit`.
fn main() {
    std::process::exit(git_tools::commit::main(&git_tools::args()));
}
