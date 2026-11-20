# git-tools

`zazzle git` is a small git front end with an editor made only for commit messages, pull request bodies, changelog entries and rebase todo lists. The editor has no modes: you type, Ctrl+S saves, Esc cancels.

```bash
zazzle git commit                 # draft the message, choose the files, commit
zazzle git commit --no-add        # start with only what is already staged
zazzle git message FILE           # edit one file; exit 0 saved, 1 cancelled
zazzle git amend                  # edit the last commit's message, adding what is staged
zazzle git undo                   # take back the last commit, keeping its changes staged
zazzle git status -sb             # anything else goes to git unchanged
git config --global core.editor 'zazzle git message'
```

`zazzle git --help` (`-h`, `help`) and `--version` (`-v`, `version`) are answered here; `zazzle git help message`, `help commit`, `help amend` and `help undo` show this tool's help, other topics go to `git help`.

## Programs
One crate builds three programs, installed beside `zazzle`, which hands `zazzle git ARGS` to `git-tools`:
- `git-tools`: help, version, `message`, `commit`, `amend`, `undo`; everything else replaces itself with `git ARGS`.
- `git-tools-commit`: the same as `zazzle git commit`.
- `git-tools-commit-message-editor`: the same as `zazzle git message`; use it where a single program name is needed.

## zazzle git commit
- The draft message lives in `.git/ZAZZLE_COMMIT_MSG` and the file marks in `.git/ZAZZLE_COMMIT_FILES` until a commit succeeds, so a failing hook or a cancel never loses them; marks are forgotten when `HEAD` moves.
- Tab opens the Files view. Each changed file shows what will happen to it: `[ADD]`, `[IGNORE]` (added to `.gitignore`; a tracked file is also untracked), `[REMOVE]` (untracked, kept on disk), `[DELETE]` (deleted from disk, asked again before committing) or blank (left out of this commit).
- Changed tracked files start as `[ADD]` (like `git commit -a`); with `--no-add` only staged files do, and a file you staged keeps exactly what you staged.
- The first run asks once whether to make `zazzle git message` git's editor for every repository (`git config --global core.editor 'zazzle git message'`); either answer is remembered as `zazzle.editorOffered` in your global git config, and the default is no. Undo with `git config --global --unset core.editor`; remove `zazzle.editorOffered` to be asked again.
- Then it runs `git commit --cleanup=strip -F DRAFT` with your other options (`--amend`, `--signoff`, ...). With `-m`, `-F`, `-C` or `--no-edit` it runs `git commit [-a]` directly.

## zazzle git amend and undo
- `zazzle git amend` writes the last commit's message to `.git/ZAZZLE_AMEND_MSG` with comments listing the staged changes, opens it in the editor, and on Ctrl+S runs `git commit --amend --cleanup=strip -F FILE`, so staged changes are added like `git commit --amend`. Esc leaves the commit as it is (exit 1). If git commit fails (a hook), the file is kept and its path printed.
- `zazzle git amend --no-edit` runs `git commit --amend --no-edit`. Nothing else is passed through (exit 2); with no commit yet it says so and exits 1.
- `zazzle git undo` runs `git reset --soft HEAD~1` and prints `undid HASH SUBJECT; its changes are staged`. It refuses (exit 1) when there is no commit, when HEAD is the first commit (`git update-ref -d HEAD` keeps its changes staged with no commit), and when HEAD is already on the upstream branch, since that rewrites shared history; `--force` undoes a pushed commit anyway.

## Keys
| Key | Action |
|---|---|
| Ctrl+S | Commit / save (Start rebase for a todo list) |
| Esc | Cancel (asks first if anything changed) |
| F1, Ctrl+G | Help (F1 may not reach the editor: macOS brightness, GNOME Terminal) |
| Tab, Shift+Tab | Switch between editing and the Files or Rebase view |
| Arrows | Move; double tap: word left/right, page up/down; triple tap: start/end of line or text |
| Shift+arrows, Ctrl+A | Select; select all |
| Ctrl+C, Ctrl+X, Ctrl+V | Copy, cut, paste (pbcopy/pbpaste, wl-copy, xclip, and OSC 52 for the terminal) |
| Ctrl+Z, Ctrl+Y | Undo, redo |
| Ctrl+K | Delete to the end of the line |
| Alt+Up/Down | Move the line (reorder commits in the Rebase view) |
| Ctrl+F, Ctrl+R | Find (Enter: next); replace (Enter: one, Alt+A: all, Tab: switch field) |
| Files view | Space cycles blank → [ADD] → [IGNORE] → [REMOVE]; A toggles all to [ADD] or blank; Backspace marks [DELETE] |
| Rebase view | p r e s f d set pick, reword, edit, squash, fixup, drop |

Commit messages show `subject N/50`; characters past 50 are amber and past 72 red. Git's comment block at the end is shown dimmed and cannot be edited. Files over 256 KiB or not UTF-8 are refused.

Taps: presses of the same arrow within 250 ms count as a double or triple tap. Presses closer than 60 ms, or reported as repeats by terminals with the kitty keyboard protocol, are a held key and move one step each. On macOS, Ctrl+arrows switch desktops by default; the editor does not use them.

## Build and test
- `cargo build --release -p git-tools` (in `src/macos`).
- `cargo test -p git-tools` runs the unit tests and `tests/pty.rs`, which drives the real programs on a pseudo-terminal in scratch repositories (commit with marks, deletion prompt, failing hook, amend, interactive rebase, `zazzle git amend` and `undo`).
- Coverage: `cargo +nightly llvm-cov --branch -p git-tools --summary-only` (CI requires 95% of lines and branches).
