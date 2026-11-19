// Tests for the editor's key handling, kept in their own file so editor.rs stays short.

use super::*;
use crate::clipboard::SystemClipboard;

// Returns a key event without modifiers.
fn k(key: Key) -> KeyEvent {
    KeyEvent { key, shift: false, alt: false, repeat: false }
}

// Returns a key event with Shift held.
fn shift(key: Key) -> KeyEvent {
    KeyEvent { key, shift: true, alt: false, repeat: false }
}

// Returns a key event with Alt held.
fn alt(key: Key) -> KeyEvent {
    KeyEvent { key, shift: false, alt: true, repeat: false }
}

// Builds an editor for lines of a kind, with optional files and an in-memory clipboard.
fn editor(lines: &[&str], kind: Kind, files: Vec<Entry>) -> Editor {
    let buf = Buffer::new(lines.iter().map(|s| s.to_string()).collect(), vec!["# help".into()]);
    Editor::new(buf, kind, '#', files, Box::new(SystemClipboard::with_commands(None, None)))
}

// Sends keys at times far enough apart that they are not taps.
fn send(ed: &mut Editor, keys: &[KeyEvent]) -> Option<Outcome> {
    let mut out = None;
    for (i, key) in keys.iter().enumerate() {
        out = ed.handle(key.clone(), 10_000 + i as u64 * 1000);
    }
    out
}

// Types text one character at a time.
fn typed(s: &str) -> Vec<KeyEvent> {
    s.chars().map(|c| k(Key::Char(c))).collect()
}

// Builds a file entry.
fn entry(path: &str, x: char, y: char, mark: Mark) -> Entry {
    Entry { path: path.into(), orig: None, x, y, mark }
}

// Checks typing, saving, the empty-message check, and cancel with and without changes.
#[test]
fn test_editor_save_and_cancel() {
    let mut ed = editor(&[""], Kind::Commit, Vec::new());
    assert_eq!(send(&mut ed, &[k(Key::Ctrl('s'))]), None);
    assert!(ed.message.contains("empty"));
    assert_eq!(send(&mut ed, &[k(Key::Esc)]), Some(Outcome::Cancel));
    send(&mut ed, &typed("Fix"));
    assert_eq!(ed.buf.text(), "Fix\n# help\n");
    assert_eq!(send(&mut ed, &[k(Key::Esc)]), None);
    assert_eq!(ed.prompt, Prompt::Discard);
    assert_eq!(send(&mut ed, &[k(Key::Char('n'))]), None);
    assert_eq!(ed.prompt, Prompt::None);
    assert_eq!(send(&mut ed, &[k(Key::Esc), k(Key::Char('y'))]), Some(Outcome::Cancel));
    assert_eq!(send(&mut ed, &[k(Key::Ctrl('s'))]), Some(Outcome::Save));
    let mut text = editor(&[""], Kind::Text, Vec::new());
    assert_eq!(send(&mut text, &[k(Key::Ctrl('s'))]), Some(Outcome::Save));
}

// Checks help, the rebase warning that blocks saving, and the delete confirmation.
#[test]
fn test_editor_prompts() {
    let mut ed = editor(&["x"], Kind::Text, Vec::new());
    send(&mut ed, &[k(Key::F1)]);
    assert_eq!(ed.prompt, Prompt::Help);
    send(&mut ed, &[k(Key::Char('z'))]);
    assert_eq!((ed.prompt.clone(), ed.buf.text()), (Prompt::None, "x\n# help\n".to_string()));
    send(&mut ed, &[k(Key::Ctrl('g'))]);
    assert_eq!(ed.prompt, Prompt::Help);
    let mut rb = editor(&["squash a x"], Kind::Rebase, Vec::new());
    assert_eq!(send(&mut rb, &[k(Key::Ctrl('s'))]), None);
    assert!(rb.message.contains("squash"));
    let files = vec![entry("junk", '?', '?', Mark::Delete)];
    let mut fd = editor(&["msg"], Kind::Commit, files);
    assert_eq!(send(&mut fd, &[k(Key::Ctrl('s'))]), None);
    assert_eq!(fd.prompt, Prompt::Delete(vec!["junk".into()]));
    assert_eq!(send(&mut fd, &[k(Key::Char('n'))]), None);
    assert_eq!(send(&mut fd, &[k(Key::Ctrl('s')), k(Key::Char('y'))]), Some(Outcome::Save));
}

// Checks Tab and Shift+Tab cycling, and the starting view per kind.
#[test]
fn test_editor_views() {
    let mut ed = editor(&["m"], Kind::Commit, vec![entry("a", ' ', 'M', Mark::Add)]);
    assert_eq!(ed.views, vec![View::Edit, View::Files]);
    send(&mut ed, &[k(Key::Tab)]);
    assert_eq!(ed.view, View::Files);
    send(&mut ed, &[k(Key::BackTab)]);
    assert_eq!(ed.view, View::Edit);
    let rb = editor(&["pick a x"], Kind::Rebase, Vec::new());
    assert_eq!((rb.view, rb.views.len()), (View::Rebase, 2));
    let mut single = editor(&["m"], Kind::Text, Vec::new());
    send(&mut single, &[k(Key::Tab)]);
    assert_eq!(single.view, View::Edit);
}

// Checks editing keys: Enter, paste, Backspace, Delete, Ctrl+K, undo and redo.
#[test]
fn test_editor_editing_keys() {
    let mut ed = editor(&["ab"], Kind::Text, Vec::new());
    send(&mut ed, &[k(Key::Right), k(Key::Enter), k(Key::Paste("x\ny".into()))]);
    assert_eq!(ed.buf.editable_text(), "a\nx\nyb");
    send(&mut ed, &[k(Key::Backspace), k(Key::Delete)]);
    assert_eq!(ed.buf.editable_text(), "a\nx\n");
    send(&mut ed, &[k(Key::Up), k(Key::Ctrl('k'))]);
    assert_eq!(ed.buf.editable_text(), "a\n\n");
    send(&mut ed, &[k(Key::Ctrl('z'))]);
    assert_eq!(ed.buf.editable_text(), "a\nx\n");
    send(&mut ed, &[k(Key::Ctrl('y'))]);
    assert_eq!(ed.buf.editable_text(), "a\n\n");
    send(&mut ed, &[k(Key::Ctrl('y'))]);
    assert_eq!(ed.message, "Nothing to redo");
    let mut fresh = editor(&[""], Kind::Text, Vec::new());
    send(&mut fresh, &[k(Key::Ctrl('z'))]);
    assert_eq!(fresh.message, "Nothing to undo");
    send(&mut fresh, &[k(Key::Ctrl('q')), k(Key::Ignored)]);
    assert_eq!(fresh.buf.editable_text(), "");
}

// Checks select all, copy, cut and paste, and the messages when nothing is selected or copied.
#[test]
fn test_editor_clipboard() {
    let mut ed = editor(&["hello"], Kind::Text, Vec::new());
    send(&mut ed, &[k(Key::Ctrl('v'))]);
    assert_eq!(ed.message, "The clipboard is empty");
    send(&mut ed, &[k(Key::Ctrl('c'))]);
    assert_eq!(ed.message, "Nothing selected");
    send(&mut ed, &[k(Key::Ctrl('a')), k(Key::Ctrl('c'))]);
    assert_eq!(ed.osc52.take().as_deref(), Some("hello"));
    send(&mut ed, &[k(Key::Ctrl('x'))]);
    assert_eq!(ed.buf.editable_text(), "");
    send(&mut ed, &[k(Key::Ctrl('v')), k(Key::Ctrl('v'))]);
    assert_eq!(ed.buf.editable_text(), "hellohello");
}

// Checks single, double and triple taps of the arrows, holding a key, Shift selection and Alt line moves.
#[test]
fn test_editor_arrows_and_taps() {
    let mut ed = editor(&["one two three", "second", "third"], Kind::Text, Vec::new());
    ed.page = 1;
    ed.handle(k(Key::Right), 0);
    assert_eq!(ed.buf.cursor, Pos { row: 0, col: 1 });
    ed.handle(k(Key::Right), 150);
    assert_eq!(ed.buf.cursor, Pos { row: 0, col: 4 });
    ed.handle(k(Key::Right), 300);
    assert_eq!(ed.buf.cursor, Pos { row: 0, col: 13 });
    ed.handle(k(Key::Left), 1000);
    ed.handle(k(Key::Left), 1100);
    assert_eq!(ed.buf.cursor, Pos { row: 0, col: 8 });
    ed.handle(k(Key::Left), 1200);
    assert_eq!(ed.buf.cursor, Pos { row: 0, col: 0 });
    ed.handle(k(Key::Down), 2000);
    assert_eq!(ed.buf.cursor.row, 1);
    ed.handle(k(Key::Down), 2100);
    assert_eq!(ed.buf.cursor.row, 1);
    ed.handle(k(Key::Down), 2200);
    assert_eq!(ed.buf.cursor, Pos { row: 2, col: 5 });
    ed.handle(k(Key::Up), 3000);
    ed.handle(k(Key::Up), 3100);
    ed.handle(k(Key::Up), 3200);
    assert_eq!(ed.buf.cursor, Pos { row: 0, col: 0 });
    ed.handle(KeyEvent { key: Key::Right, shift: true, alt: false, repeat: true }, 4000);
    ed.handle(shift(Key::Right), 4010);
    assert_eq!(ed.buf.selected_text().as_deref(), Some("on"));
    ed.handle(alt(Key::Down), 5000);
    assert_eq!(ed.buf.line(1), "one two three");
    ed.handle(k(Key::PageDown), 6000);
    assert_eq!(ed.buf.cursor.row, 2);
    ed.handle(shift(Key::PageUp), 7000);
    assert_eq!(ed.buf.cursor.row, 1);
}

// Checks the Files view: moving, cycling, toggling all, delete marks, undo and paging.
#[test]
fn test_editor_files_view() {
    let files = vec![
        entry("a", ' ', 'M', Mark::Add),
        entry("b", '?', '?', Mark::Skip),
        entry("c", ' ', 'M', Mark::Add),
    ];
    let mut ed = editor(&["m"], Kind::Commit, files);
    ed.page = 2;
    send(&mut ed, &[k(Key::Tab), k(Key::Down), k(Key::Char(' '))]);
    assert_eq!(ed.files[1].mark, Mark::Add);
    assert!(ed.changed());
    send(&mut ed, &[k(Key::Char('A'))]);
    assert!(ed.files.iter().all(|e| e.mark == Mark::Skip));
    send(&mut ed, &[k(Key::Char('a'))]);
    assert!(ed.files.iter().all(|e| e.mark == Mark::Add));
    send(&mut ed, &[k(Key::Backspace)]);
    assert_eq!(ed.files[1].mark, Mark::Delete);
    send(&mut ed, &[k(Key::Ctrl('z'))]);
    assert_eq!(ed.files[1].mark, Mark::Add);
    send(&mut ed, &[k(Key::Ctrl('z')), k(Key::Ctrl('z')), k(Key::Ctrl('z'))]);
    assert_eq!(ed.files[1].mark, Mark::Skip);
    send(&mut ed, &[k(Key::Ctrl('z'))]);
    assert_eq!(ed.message, "Nothing to undo");
    send(&mut ed, &[k(Key::PageDown)]);
    assert_eq!(ed.list_row, 2);
    send(&mut ed, &[k(Key::PageUp), k(Key::Left), k(Key::Char('x'))]);
    assert_eq!(ed.list_row, 0);
    ed.handle(k(Key::Down), 100);
    ed.handle(k(Key::Down), 200);
    assert_eq!(ed.list_row, 2);
    ed.handle(k(Key::Up), 1000);
    ed.handle(k(Key::Up), 1100);
    ed.handle(k(Key::Up), 1200);
    assert_eq!(ed.list_row, 0);
}

// Checks the Rebase view: setting commands, reordering, undo, paging and keys that do nothing.
#[test]
fn test_editor_rebase_view() {
    let mut ed = editor(&["pick a one", "pick b two", "exec make"], Kind::Rebase, Vec::new());
    ed.page = 1;
    send(&mut ed, &[k(Key::Down), k(Key::Char('s'))]);
    assert_eq!(ed.buf.line(1), "squash b two");
    send(&mut ed, &[alt(Key::Up)]);
    assert_eq!(ed.buf.line(0), "squash b two");
    send(&mut ed, &[k(Key::Ctrl('z'))]);
    assert_eq!(ed.buf.line(0), "pick a one");
    send(&mut ed, &[k(Key::Ctrl('y'))]);
    assert_eq!(ed.buf.line(0), "squash b two");
    send(&mut ed, &[k(Key::PageDown), k(Key::PageDown), k(Key::Char('p'))]);
    assert!(ed.message.contains("p r e s f d"));
    send(&mut ed, &[k(Key::PageUp), k(Key::Right)]);
    assert_eq!(ed.buf.cursor.row, 1);
}

// Checks the find bar while editing: typing searches, Enter finds the next match, Backspace, paste and Esc.
#[test]
fn test_editor_find() {
    let mut ed = editor(&["foo bar", "bar foo"], Kind::Text, Vec::new());
    send(&mut ed, &[k(Key::Ctrl('f'))]);
    send(&mut ed, &typed("bar"));
    assert_eq!(ed.buf.selection(), Some((Pos { row: 0, col: 4 }, Pos { row: 0, col: 7 })));
    send(&mut ed, &[k(Key::Enter)]);
    assert_eq!(ed.buf.cursor, Pos { row: 1, col: 3 });
    send(&mut ed, &[k(Key::Backspace), k(Key::Paste("rz".into()))]);
    assert_eq!(ed.message, "Not found");
    send(&mut ed, &[k(Key::Tab), k(Key::Ctrl('r')), k(Key::Ctrl('f')), k(Key::Esc)]);
    assert!(ed.find.is_none());
    ed.buf.anchor = Some(Pos { row: 0, col: 0 });
    ed.buf.cursor = Pos { row: 0, col: 3 };
    send(&mut ed, &[k(Key::Ctrl('f'))]);
    assert_eq!(ed.find.as_ref().map(|f| f.query.as_str()), Some("foo"));
}

// Checks find and replace: replacing one match, switching fields, replacing all and undoing it.
#[test]
fn test_editor_replace() {
    let mut ed = editor(&["a b a", "a"], Kind::Text, Vec::new());
    send(&mut ed, &[k(Key::Ctrl('r'))]);
    send(&mut ed, &typed("a"));
    send(&mut ed, &[k(Key::Tab)]);
    send(&mut ed, &typed("x"));
    assert_eq!(ed.find.as_ref().map(|f| f.with.as_str()), Some("x"));
    send(&mut ed, &[k(Key::Enter)]);
    assert_eq!(ed.buf.editable_text(), "x b a\na");
    send(&mut ed, &[k(Key::BackTab), k(Key::Enter)]);
    send(&mut ed, &[alt(Key::Alt('a'))]);
    assert_eq!(ed.buf.editable_text(), "x b x\nx");
    assert_eq!(ed.message, "Replaced 1");
    send(&mut ed, &[k(Key::Esc), k(Key::Ctrl('z'))]);
    assert_eq!(ed.buf.editable_text(), "x b x\na");
    ed.buf.anchor = None;
    ed.buf.cursor = Pos::default();
    send(&mut ed, &[k(Key::Ctrl('r')), k(Key::Enter)]);
    assert_eq!((ed.buf.selection(), ed.message.as_str()), (None, "Not found"));
    send(&mut ed, &[k(Key::Esc)]);
    ed.buf.anchor = None;
    send(&mut ed, &[k(Key::Ctrl('r'))]);
    send(&mut ed, &typed("zz"));
    send(&mut ed, &[k(Key::Enter)]);
    assert_eq!(ed.message, "Not found");
}

// Checks finding rows in the list views, and that replace is refused there.
#[test]
fn test_editor_find_in_lists() {
    let files = vec![
        entry("src/a.rs", ' ', 'M', Mark::Add),
        entry("README", ' ', 'M', Mark::Add),
        entry("src/b.rs", ' ', 'M', Mark::Add),
    ];
    let mut ed = editor(&["m"], Kind::Commit, files);
    send(&mut ed, &[k(Key::Tab), k(Key::Ctrl('r'))]);
    assert!(ed.message.contains("Replace works while editing"));
    send(&mut ed, &[k(Key::Ctrl('f'))]);
    send(&mut ed, &typed("src"));
    assert_eq!(ed.list_row, 0);
    send(&mut ed, &[k(Key::Enter)]);
    assert_eq!(ed.list_row, 2);
    send(&mut ed, &typed("zz"));
    assert_eq!(ed.message, "Not found");
    send(&mut ed, &[k(Key::Ctrl('r')), alt(Key::Alt('a'))]);
    assert!(ed.find.as_ref().is_some_and(|f| !f.replace));
    let mut rb = editor(&["pick a one", "pick b two"], Kind::Rebase, Vec::new());
    send(&mut rb, &[k(Key::Ctrl('f'))]);
    send(&mut rb, &typed("two"));
    assert_eq!(rb.buf.cursor.row, 1);
}
