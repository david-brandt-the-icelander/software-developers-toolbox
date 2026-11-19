// Tests for drawing frames, kept in their own file so render.rs stays short.

use super::*;
use crate::buffer::Buffer;
use crate::clipboard::SystemClipboard;
use crate::editor::Editor;
use crate::editor::FindBar;
use crate::files::{Entry, Mark};
use crate::keys::{Key, KeyEvent};
use crate::kind::Kind;

// Builds an editor for lines of a kind with optional files.
fn editor(lines: &[&str], kind: Kind, files: Vec<Entry>) -> Editor {
    let buf = Buffer::new(lines.iter().map(|s| s.to_string()).collect(), vec!["# tail".into()]);
    Editor::new(buf, kind, '#', files, Box::new(SystemClipboard::with_commands(None, None)), None)
}

// Removes escape sequences so the visible text can be checked.
fn plain(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        if chars.peek() == Some(&'[') {
            chars.next();
            for n in chars.by_ref() {
                if n.is_ascii_alphabetic() || n == '~' {
                    break;
                }
            }
        }
    }
    out
}

// Sends one key.
fn press(ed: &mut Editor, key: Key) {
    ed.handle(KeyEvent { key, shift: false, alt: false, repeat: false }, 0);
}

// Checks the editing view: text, dimmed tail, length colours, the counter, the key bar and the cursor position.
#[test]
fn test_frame_edit_view() {
    let long = "x".repeat(75);
    let mut ed = editor(
        &[&long, "", "# note"],
        Kind::Commit,
        vec![Entry { path: "a".into(), orig: None, x: ' ', y: 'M', mark: Mark::Add }],
    );
    let mut s = Scroll::default();
    let f = frame(&mut ed, &mut s, 100, 8);
    assert!(f.contains(AMBER) && f.contains(RED) && f.contains(DIM));
    let p = plain(&f);
    assert!(p.contains("# tail"));
    assert!(p.contains("subject 75/50"));
    assert!(p.contains("^S Commit  Esc Cancel  Tab Files  ^F Find  ^R Replace  F1/^G Help"));
    assert!(f.ends_with("\x1b[1;1H\x1b[?25h"));
    assert_eq!(ed.page, 5);
    ed.buf.cursor = crate::buffer::Pos { row: 1, col: 0 };
    assert!(plain(&frame(&mut ed, &mut s, 100, 8)).contains("line 2 0/72"));
    let mut t = editor(&["héllo 世界"], Kind::Text, Vec::new());
    t.buf.cursor.col = 8;
    let f = frame(&mut t, &mut s, 100, 8);
    assert!(plain(&f).contains("Ln 1, Col 9"));
    assert!(f.ends_with("\x1b[1;11H\x1b[?25h"));
}

// Checks scrolling down and right to keep the cursor visible, and back.
#[test]
fn test_frame_scrolls_to_cursor() {
    let lines: Vec<String> = (0..30).map(|i| format!("line {i} {}", "y".repeat(40))).collect();
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    let mut ed = editor(&refs, Kind::Text, Vec::new());
    let mut s = Scroll::default();
    ed.buf.cursor = crate::buffer::Pos { row: 20, col: 45 };
    let f = frame(&mut ed, &mut s, 20, 10);
    assert_eq!(s.top, 13);
    assert!(s.left > 0);
    assert!(f.ends_with(&format!("\x1b[8;{}H\x1b[?25h", 45 - s.left + 1)));
    ed.buf.cursor = crate::buffer::Pos { row: 0, col: 0 };
    frame(&mut ed, &mut s, 20, 10);
    assert_eq!((s.top, s.left), (0, 0));
}

// Checks the two columns: message left, files right with a divider, focus moved by Tab, and one column on a narrow terminal.
#[test]
fn test_frame_two_columns() {
    let files = vec![
        Entry { path: "a.rs".into(), orig: None, x: ' ', y: 'M', mark: Mark::Add },
        Entry { path: "b.log".into(), orig: None, x: '?', y: '?', mark: Mark::Ignore },
    ];
    let mut ed = editor(&["msg"], Kind::Commit, files);
    let mut s = Scroll::default();
    let f = frame(&mut ed, &mut s, 100, 8);
    let p = plain(&f);
    assert!(p.contains("msg") && p.contains("│› [ADD]     M  a.rs") && p.contains("[IGNORE] ??  b.log"));
    assert!(!f.contains(&format!("{REVERSE}› [ADD]")));
    assert!(f.ends_with("\x1b[1;1H\x1b[?25h"));
    press(&mut ed, Key::Tab);
    press(&mut ed, Key::Down);
    let f = frame(&mut ed, &mut s, 100, 8);
    assert!(f.contains(&format!("{REVERSE}› [IGNORE]")));
    assert!(!f.ends_with("\x1b[?25h"));
    let p = plain(&f);
    assert!(p.contains("msg") && p.contains("2 files"));
    assert!(p.contains("Space Cycle  A All  ⌫ Delete  Tab Message  ^S Commit"));
    let narrow = plain(&frame(&mut ed, &mut s, 50, 8));
    assert!(!narrow.contains("│") && !narrow.contains("msg") && narrow.contains("› [IGNORE]"));
    press(&mut ed, Key::Tab);
    let narrow = plain(&frame(&mut ed, &mut s, 50, 8));
    assert!(narrow.contains("msg") && !narrow.contains("[IGNORE]"));
}

// Checks the agent line, the dots, the ghost suggestion in an empty message, and the agent keys in the bars and help.
#[test]
fn test_frame_agent_line_and_suggestion() {
    let (assist, state) = crate::assist::tests::fake(&["claude"], ("", ""));
    let buf = Buffer::new(vec!["".into()], vec!["# tail".into()]);
    let files = vec![Entry { path: "a.rs".into(), orig: None, x: ' ', y: 'M', mark: Mark::Add }];
    let clip = Box::new(SystemClipboard::with_commands(None, None));
    let mut ed = Editor::new(buf, Kind::Commit, '#', files, clip, Some(assist));
    let mut s = Scroll::default();
    ed.tick(1200);
    let p = plain(&frame(&mut ed, &mut s, 140, 8));
    assert!(p.contains("claude · effort low · claude is composing default commit message..."));
    assert!(p.contains("Tab Files  ^N Agent  ^E Effort  ^F Find"));
    state.borrow_mut().answer = Some(Ok("Suggested subject\n\nWhy".into()));
    ed.tick(1300);
    let f = frame(&mut ed, &mut s, 140, 8);
    assert!(f.contains(&format!("{DIM}Suggested subject{RESET}")));
    assert!(plain(&f).contains("suggestion ready") && plain(&f).contains("# tail"));
    press(&mut ed, Key::Tab);
    assert!(plain(&frame(&mut ed, &mut s, 140, 8))
        .contains("Tab Message  ^S Commit  Esc Cancel  ^N Agent  ^E Effort"));
    press(&mut ed, Key::BackTab);
    press(&mut ed, Key::Char('x'));
    assert!(!plain(&frame(&mut ed, &mut s, 140, 8)).contains("Suggested subject"));
    ed.prompt = Prompt::Help;
    assert!(plain(&frame(&mut ed, &mut s, 200, 40)).contains("Ctrl+N        next agent CLI"));
}

// Checks the Rebase view rows and key bar.
#[test]
fn test_frame_rebase_view() {
    let mut ed = editor(&["p 1a2b Add parser", "s 3c4d Fix"], Kind::Rebase, Vec::new());
    let p = plain(&frame(&mut ed, &mut Scroll::default(), 120, 6));
    assert!(p.contains("› pick   1a2b Add parser"));
    assert!(p.contains("  squash 3c4d Fix"));
    assert!(p.contains("2 lines"));
    assert!(p.contains("^S Start rebase  Esc Abort rebase"));
}

// Checks the find and replace bars and where the cursor goes.
#[test]
fn test_frame_find_bars() {
    let mut ed = editor(&["abc"], Kind::Text, Vec::new());
    ed.find = Some(FindBar { query: "ab".into(), ..FindBar::default() });
    let f = frame(&mut ed, &mut Scroll::default(), 80, 6);
    assert!(plain(&f).contains("Find: ab   Enter Next"));
    assert!(f.ends_with("\x1b[4;9H\x1b[?25h"));
    ed.find = Some(FindBar {
        query: "ab".into(),
        with: "x".into(),
        replace: true,
        on_with: true,
        ..FindBar::default()
    });
    let f = frame(&mut ed, &mut Scroll::default(), 80, 6);
    assert!(plain(&f).contains("Replace: x   Enter Replace"));
    assert!(f.ends_with("\x1b[4;22H\x1b[?25h"));
    ed.find.as_mut().unwrap().on_with = false;
    assert!(frame(&mut ed, &mut Scroll::default(), 80, 6).ends_with("\x1b[4;9H\x1b[?25h"));
}

// Checks the prompts, messages and help screen in the status line.
#[test]
fn test_frame_prompts_and_help() {
    let files = vec![Entry { path: "a".into(), orig: None, x: '?', y: '?', mark: Mark::Delete }];
    let mut ed = editor(&["m"], Kind::Commit, files);
    let mut s = Scroll::default();
    ed.prompt = Prompt::Delete(vec!["a".into()]);
    assert!(plain(&frame(&mut ed, &mut s, 80, 6)).contains("Delete 1 file from disk?  a   y/n"));
    ed.prompt = Prompt::Delete(vec!["a".into(), "b".into()]);
    assert!(plain(&frame(&mut ed, &mut s, 80, 6)).contains("Delete 2 files"));
    ed.prompt = Prompt::Discard;
    assert!(plain(&frame(&mut ed, &mut s, 80, 6)).contains("Discard your changes?"));
    ed.prompt = Prompt::None;
    ed.message = "hello".into();
    assert!(plain(&frame(&mut ed, &mut s, 80, 6)).contains("hello"));
    ed.prompt = Prompt::Help;
    let f = frame(&mut ed, &mut s, 200, 40);
    assert!(plain(&f).contains("Files (right column, Tab): Space cycles"));
    assert!(!f.ends_with("\x1b[?25h"));
    let mut rb = editor(&["pick a"], Kind::Rebase, Vec::new());
    rb.prompt = Prompt::Help;
    assert!(plain(&frame(&mut rb, &mut s, 200, 40)).contains("Rebase view: p pick"));
    let mut tiny = editor(&["\u{7}x"], Kind::Text, Vec::new());
    let p = plain(&frame(&mut tiny, &mut Scroll::default(), 1, 1));
    assert!(p.contains("?x"));
}
