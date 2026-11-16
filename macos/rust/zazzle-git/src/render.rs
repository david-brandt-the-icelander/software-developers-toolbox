// Draws the editor state as one terminal frame: the text or list, the find bar, the status line and the bottom key bar.

use crate::buffer::Pos;
use crate::editor::{Editor, Prompt, View};
use crate::rebase;
use unicode_width::UnicodeWidthChar;

const RESET: &str = "\x1b[0m";
const DIM: &str = "\x1b[2m";
const REVERSE: &str = "\x1b[7m";
const BOLD: &str = "\x1b[1m";
const AMBER: &str = "\x1b[33m";
const RED: &str = "\x1b[31m";

// The scroll position kept between frames.
#[derive(Debug, Default)]
pub struct Scroll {
    pub top: usize,
    pub left: usize,
}

// How one character is drawn.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Style {
    Normal,
    Dim,
    Soft,
    Hard,
    Selected,
}

// Returns the escape sequence that starts a style.
fn sgr(s: Style) -> &'static str {
    match s {
        Style::Normal => RESET,
        Style::Dim => DIM,
        Style::Soft => AMBER,
        Style::Hard => RED,
        Style::Selected => REVERSE,
    }
}

// Returns the display width of a character (wide characters take two columns, control characters one).
fn width(c: char) -> usize {
    c.width().unwrap_or(1).max(1)
}

// Cuts text to a display width.
fn fit(text: &str, w: usize) -> String {
    let mut used = 0;
    text.chars()
        .take_while(|c| {
            used += width(*c);
            used <= w
        })
        .collect()
}

// Returns the bottom key bar for the current view.
fn key_bar(ed: &Editor) -> String {
    let save = ed.kind.save_label();
    let cancel = ed.kind.cancel_label();
    let tab = if ed.views.len() > 1 { format!("Tab {}  ", next_view_name(ed)) } else { String::new() };
    match ed.view {
        View::Edit => format!("^S {save}  Esc {cancel}  ^F Find  ^R Replace  {tab}F1/^G Help"),
        View::Files => {
            format!("↑↓ Move  Space Cycle  A All  ⌫ Delete  {tab}^S {save}  Esc {cancel}  F1/^G Help")
        }
        View::Rebase => {
            format!("↑↓ Move  Alt↑↓ Reorder  p r e s f d Command  {tab}^S {save}  Esc {cancel}  F1/^G Help")
        }
    }
}

// Names the view Tab switches to.
fn next_view_name(ed: &Editor) -> &'static str {
    let at = ed.views.iter().position(|v| *v == ed.view).unwrap_or(0);
    match ed.views[(at + 1) % ed.views.len()] {
        View::Edit => "Edit",
        View::Files => "Files",
        View::Rebase => "Rebase",
    }
}

// Returns the status line: a prompt, a message, or the line-length counter.
fn status_line(ed: &Editor) -> String {
    if let Prompt::Delete(files) = &ed.prompt {
        let s = if files.len() == 1 { "" } else { "s" };
        return format!("Delete {} file{s} from disk?  {}   y/n", files.len(), files.join(", "));
    }
    if ed.prompt == Prompt::Discard {
        return "Discard your changes?  y/n".to_string();
    }
    if !ed.message.is_empty() {
        return ed.message.clone();
    }
    let Pos { row, col } = ed.buf.cursor;
    let len = ed.buf.lines[row].len();
    match (ed.view, ed.kind.limits(row)) {
        (View::Edit, Some((soft, _))) if row == 0 => format!("subject {len}/{soft}"),
        (View::Edit, Some((_, hard))) => format!("line {} {len}/{hard}", row + 1),
        (View::Edit, None) => format!("Ln {}, Col {}", row + 1, col + 1),
        (View::Files, _) => format!("{} files", ed.files.len()),
        (View::Rebase, _) => format!("{} lines", ed.buf.lines.len()),
    }
}

// Returns the find bar line and the column of its cursor.
fn find_line(ed: &Editor) -> Option<(String, usize)> {
    let bar = ed.find.as_ref()?;
    let find = format!("Find: {}", bar.query);
    if !bar.replace {
        return Some((format!("{find}   Enter Next  Esc Close"), find.chars().count()));
    }
    let with = format!("   Replace: {}", bar.with);
    let cursor = if bar.on_with { find.chars().count() + with.chars().count() } else { find.chars().count() };
    Some((format!("{find}{with}   Enter Replace  Alt+A All  Tab Field  Esc Close"), cursor))
}

// Returns the style of a character in the editing view.
fn char_style(ed: &Editor, row: usize, col: usize, sel: Option<(Pos, Pos)>) -> Style {
    let p = Pos { row, col };
    if sel.is_some_and(|(a, b)| a <= p && p < b) {
        return Style::Selected;
    }
    if ed.kind.is_git() && ed.buf.lines[row].first() == Some(&ed.comment) {
        return Style::Dim;
    }
    match ed.kind.limits(row) {
        Some((_, hard)) if col >= hard => Style::Hard,
        Some((soft, _)) if col >= soft => Style::Soft,
        _ => Style::Normal,
    }
}

// Draws one editable line from the left scroll column, and returns it.
fn text_row(ed: &Editor, row: usize, left: usize, w: usize, sel: Option<(Pos, Pos)>) -> String {
    let mut out = String::new();
    let mut style = Style::Normal;
    let mut used = 0;
    for (col, &c) in ed.buf.lines[row].iter().enumerate().skip(left) {
        used += width(c);
        if used > w {
            break;
        }
        let s = char_style(ed, row, col, sel);
        if s != style {
            out.push_str(RESET);
            out.push_str(sgr(s));
            style = s;
        }
        out.push(if c.is_control() { '?' } else { c });
    }
    out.push_str(RESET);
    out
}

// Adjusts the scroll so the cursor is visible, and returns the screen column of the cursor.
fn scroll_to_cursor(ed: &Editor, scroll: &mut Scroll, rows: usize, w: usize) -> usize {
    let Pos { row, col } = ed.buf.cursor;
    if row < scroll.top {
        scroll.top = row;
    }
    if row >= scroll.top + rows {
        scroll.top = row + 1 - rows;
    }
    let line = &ed.buf.lines[row];
    scroll.left = scroll.left.min(col);
    while line[scroll.left..col].iter().map(|c| width(*c)).sum::<usize>() >= w {
        scroll.left += 1;
    }
    line[scroll.left..col].iter().map(|c| width(*c)).sum()
}

// Returns the content rows and the cursor position for the editing view.
fn edit_rows(
    ed: &Editor,
    scroll: &mut Scroll,
    rows: usize,
    w: usize,
) -> (Vec<String>, Option<(usize, usize)>) {
    let x = scroll_to_cursor(ed, scroll, rows, w);
    let sel = ed.buf.selection();
    let n = ed.buf.lines.len();
    let mut out = Vec::new();
    for i in scroll.top..scroll.top + rows {
        if i < n {
            out.push(text_row(ed, i, scroll.left, w, sel));
        } else if let Some(t) = ed.buf.tail.get(i - n) {
            out.push(format!("{DIM}{}{RESET}", fit(t, w)));
        } else {
            out.push(String::new());
        }
    }
    (out, Some((ed.buf.cursor.row - scroll.top, x)))
}

// Returns the content rows for a list view, with the cursor row highlighted.
fn list_rows(items: &[String], cursor: usize, scroll: &mut Scroll, rows: usize, w: usize) -> Vec<String> {
    if cursor < scroll.top {
        scroll.top = cursor;
    }
    if cursor >= scroll.top + rows {
        scroll.top = cursor + 1 - rows;
    }
    (scroll.top..scroll.top + rows)
        .map(|i| match items.get(i) {
            Some(t) if i == cursor => format!("{REVERSE}{}{RESET}", fit(&format!("› {t}"), w)),
            Some(t) => fit(&format!("  {t}"), w),
            None => String::new(),
        })
        .collect()
}

// Returns the help screen lines for the current view.
fn help_lines(ed: &Editor) -> Vec<String> {
    let mut v = vec![
        "zazzle git message: keys (any key closes this help)".to_string(),
        String::new(),
        format!("Ctrl+S        {}", ed.kind.save_label()),
        format!("Esc           {} (asks first if anything changed)", ed.kind.cancel_label()),
        "Tab, Shift+Tab  switch between editing and the Files or Rebase view".to_string(),
        "F1, Ctrl+G    this help".to_string(),
        String::new(),
        "Arrows        move; double tap: word left/right, page up/down;".to_string(),
        "              triple tap: start/end of line, start/end of text".to_string(),
        "Shift+arrows  select (taps work too); Ctrl+A selects all".to_string(),
        "Ctrl+C/X/V    copy, cut, paste      Ctrl+Z/Y  undo, redo".to_string(),
        "Ctrl+K        delete to end of line  Alt+Up/Down  move the line".to_string(),
        "Ctrl+F        find (Enter: next)     Ctrl+R  replace (Enter: one, Alt+A: all)".to_string(),
        "PgUp/PgDn     page up/down".to_string(),
    ];
    if ed.views.contains(&View::Files) {
        v.push(String::new());
        v.push("Files view: Space cycles [ADD] [IGNORE] [REMOVE] and not in this commit; A toggles all to [ADD];".to_string());
        v.push(
            "            Backspace marks [DELETE] (asked again before committing); Ctrl+Z undoes a mark"
                .to_string(),
        );
    }
    if ed.views.contains(&View::Rebase) {
        v.push(String::new());
        v.push(
            "Rebase view: p pick, r reword, e edit, s squash, f fixup, d drop; Alt+Up/Down reorders"
                .to_string(),
        );
    }
    v.push(String::new());
    v.push(
        "macOS: Ctrl+arrows may switch desktops; turn that off in System Settings > Keyboard > Shortcuts."
            .to_string(),
    );
    v
}

// Builds the whole frame for a terminal of the given size.
pub fn frame(ed: &mut Editor, scroll: &mut Scroll, w: usize, h: usize) -> String {
    let w = w.max(10);
    let h = h.max(3);
    let find = find_line(ed);
    let rows = h - 2 - usize::from(find.is_some());
    ed.page = rows.saturating_sub(1).max(1);
    let (content, cursor) = if ed.prompt == Prompt::Help {
        (help_lines(ed).iter().map(|l| fit(l, w)).collect(), None)
    } else {
        match ed.view {
            View::Edit => edit_rows(ed, scroll, rows, w),
            View::Files => (
                list_rows(
                    &ed.files.iter().map(|e| e.row()).collect::<Vec<_>>(),
                    ed.list_row,
                    scroll,
                    rows,
                    w,
                ),
                None,
            ),
            View::Rebase => {
                let items: Vec<String> =
                    ed.buf.lines.iter().map(|l| rebase::row(&l.iter().collect::<String>())).collect();
                (list_rows(&items, ed.buf.cursor.row, scroll, rows, w), None)
            }
        }
    };
    let mut out = String::from("\x1b[?25l\x1b[H");
    for line in content.iter().take(rows) {
        out.push_str(&format!("{line}\x1b[K\r\n"));
    }
    for _ in content.len()..rows {
        out.push_str("\x1b[K\r\n");
    }
    if let Some((text, _)) = &find {
        out.push_str(&format!("{BOLD}{}{RESET}\x1b[K\r\n", fit(text, w)));
    }
    out.push_str(&format!("{BOLD}{}{RESET}\x1b[K\r\n", fit(&status_line(ed), w)));
    out.push_str(&format!("{REVERSE}{}\x1b[K{RESET}", fit(&key_bar(ed), w)));
    match (find, cursor) {
        (Some((_, col)), _) => out.push_str(&format!("\x1b[{};{}H\x1b[?25h", rows + 1, col.min(w - 1) + 1)),
        (None, Some((r, c))) => out.push_str(&format!("\x1b[{};{}H\x1b[?25h", r + 1, c + 1)),
        (None, None) => {}
    }
    out
}

#[cfg(test)]
#[path = "render_tests.rs"]
mod tests;
