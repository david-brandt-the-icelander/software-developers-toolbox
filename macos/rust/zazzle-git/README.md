# zazzle-git

`zazzle git` is a small git front end with an editor made only for commit messages, pull request bodies, changelog entries and rebase todo lists. The editor has no modes: you type, Ctrl+S saves, Esc cancels.

```bash
zazzle git commit                 # draft the message, choose the files, commit
zazzle git commit --no-add        # start with only what is already staged
zazzle git message FILE           # edit one file; exit 0 saved, 1 cancelled
zazzle git status -sb             # anything else goes to git unchanged
git config --global core.editor 'zazzle git message'
```

`zazzle git --help` (`-h`, `help`) and `--version` (`-v`, `version`) are answered here; `zazzle git help message` and `help commit` show this tool's help, other topics go to `git help`.

## Programs
One crate builds three programs, installed beside `zazzle`, which hands `zazzle git ARGS` to `zazzle-git`:
- `zazzle-git`: help, version, `message`, `commit`; everything else replaces itself with `git ARGS`.
- `zazzle-git-commit`: the same as `zazzle git commit`.
- `zazzle-git-commit-message-editor`: the same as `zazzle git message`; use it where a single program name is needed.

## zazzle git commit
- The draft message lives in `.git/ZAZZLE_COMMIT_MSG` and the file marks in `.git/ZAZZLE_COMMIT_FILES` until a commit succeeds, so a failing hook or a cancel never loses them; marks are forgotten when `HEAD` moves.
- Tab opens the Files view. Each changed file shows what will happen to it: `[ADD]`, `[IGNORE]` (added to `.gitignore`; a tracked file is also untracked), `[REMOVE]` (untracked, kept on disk), `[DELETE]` (deleted from disk, asked again before committing) or blank (left out of this commit).
- Changed tracked files start as `[ADD]` (like `git commit -a`); with `--no-add` only staged files do, and a file you staged keeps exactly what you staged.
- The first run asks once whether to make `zazzle git message` git's editor for every repository (`git config --global core.editor 'zazzle git message'`); either answer is remembered as `zazzle.editorOffered` in your global git config, and the default is no. Undo with `git config --global --unset core.editor`; remove `zazzle.editorOffered` to be asked again.
- Then it runs `git commit --cleanup=strip -F DRAFT` with your other options (`--amend`, `--signoff`, ...). With `-m`, `-F`, `-C` or `--no-edit` it runs `git commit [-a]` directly.

## Layout and the agent's suggestion
- `zazzle git commit` shows two columns: the message on the left and the changed files on the right. Tab and Shift+Tab move the focus between them. Below 60 columns only the focused one is shown.
- When the editor opens with an empty commit message (in `zazzle git commit`, and when git itself opens it), an agent CLI starts composing a suggestion in the background. The line above the status bar shows `claude · effort low · claude is composing default commit message.` and adds a dot every 500 ms until the answer arrives. The suggestion is then shown dimmed in the empty message.
- Ctrl+S with an empty message commits the suggestion. If the agent is still working, the editor says it will save when the agent is done and saves then; Esc stops waiting.
- Agents: `claude`, `codex` and `gemini`, whichever are on `PATH`. If none is found, the line says so and you write the message yourself. Ctrl+N switches to the next agent found, Ctrl+E to the agent's next effort level (claude: low, medium, high, xhigh, max; codex: low, medium, high; gemini has none). Each switch starts a new suggestion, and the choice is remembered as `zazzle.agent` and `zazzle.effort` in your global git config.
- Agents run without tools and without saving a session: `claude -p --tools "" --no-session-persistence --effort E`, `codex exec --sandbox read-only --skip-git-repo-check --ephemeral -c model_reasoning_effort=E -`, `gemini --approval-mode plan -p ...`. The prompt goes in on stdin, and a run is stopped after 5 minutes.
- The prompt is the instructions plus the branch, the last 10 commit subjects and the changes. In `zazzle git commit` the changes are what the file marks would commit; when git opens the editor, they are the staged diff. Changing marks starts a new suggestion after a 1.5 s pause. Changes are cut at 60,000 characters and new files at 200 lines.
- Instructions: the nearest `.zazzle/commit-message-prompt.md`, looking from the current folder up to your home folder (and your home folder's own when you are outside it). Without one, a built-in prompt asks for a subject of at most 50 characters and an optional body wrapped at 72.

## Keys
| Key | Action |
|---|---|
| Ctrl+S | Commit / save (Start rebase for a todo list) |
| Esc | Cancel (asks first if anything changed) |
| F1, Ctrl+G | Help (F1 may not reach the editor: macOS brightness, GNOME Terminal) |
| Tab, Shift+Tab | Move between the message and the files column (or the Rebase view) |
| Arrows | Move; double tap: word left/right, page up/down; triple tap: start/end of line or text |
| Shift+arrows, Ctrl+A | Select; select all |
| Ctrl+C, Ctrl+X, Ctrl+V | Copy, cut, paste (pbcopy/pbpaste, wl-copy, xclip, and OSC 52 for the terminal) |
| Ctrl+Z, Ctrl+Y | Undo, redo |
| Ctrl+K | Delete to the end of the line |
| Alt+Up/Down | Move the line (reorder commits in the Rebase view) |
| Ctrl+N, Ctrl+E | Next agent CLI; next effort level (remembered) |
| Ctrl+F, Ctrl+R | Find (Enter: next); replace (Enter: one, Alt+A: all, Tab: switch field) |
| Files view | Space cycles blank → [ADD] → [IGNORE] → [REMOVE]; A toggles all to [ADD] or blank; Backspace marks [DELETE] |
| Rebase view | p r e s f d set pick, reword, edit, squash, fixup, drop |

Commit messages show `subject N/50`; characters past 50 are amber and past 72 red. Git's comment block at the end is shown dimmed and cannot be edited. Files over 256 KiB or not UTF-8 are refused.

Taps: presses of the same arrow within 250 ms count as a double or triple tap. Presses closer than 60 ms, or reported as repeats by terminals with the kitty keyboard protocol, are a held key and move one step each. On macOS, Ctrl+arrows switch desktops by default; the editor does not use them.

## Build and test
- `cargo build --release -p zazzle-git` (in `macos/rust`).
- `cargo test -p zazzle-git` runs the unit tests and `tests/pty.rs`, which drives the real programs on a pseudo-terminal in scratch repositories (commit with marks, deletion prompt, failing hook, amend, interactive rebase).
- Coverage: `cargo +nightly llvm-cov --branch -p zazzle-git --summary-only` (CI requires 95% of lines and branches).
