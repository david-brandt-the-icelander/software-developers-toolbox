// The `zazzle-git-commit-message-editor` program, the same as `zazzle git message`, for use as git's core.editor.
fn main() {
    std::process::exit(zazzle_git::message::main(&zazzle_git::args()));
}
