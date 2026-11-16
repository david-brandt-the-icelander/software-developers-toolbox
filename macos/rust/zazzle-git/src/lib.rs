// zazzle git: a git front end with a small commit message editor (`zazzle git message`) and a commit flow (`zazzle git commit`).

pub mod buffer;
pub mod clipboard;
pub mod commit;
pub mod editor;
pub mod files;
pub mod find;
pub mod git;
pub mod keys;
pub mod kind;
pub mod message;
pub mod offer;
pub mod rebase;
pub mod render;
pub mod router;
pub mod tap;
pub mod term;

// Returns the program's arguments after its name.
pub fn args() -> Vec<String> {
    std::env::args().skip(1).collect()
}
