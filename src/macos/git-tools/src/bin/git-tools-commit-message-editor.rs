// The `git-tools-commit-message-editor` program, the same as `zazzle git message`, for use as git's core.editor.
fn main() {
    std::process::exit(git_tools::message::main(&git_tools::args()));
}
