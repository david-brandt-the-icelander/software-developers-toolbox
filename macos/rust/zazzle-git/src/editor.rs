// The editor's state and what each key does in the editing view, the Files and Rebase views, the find bar and the prompts.

use crate::assist::{Assist, Status};
use crate::buffer::{Buffer, Pos};
use crate::clipboard::Clipboard;
use crate::files::{self, Entry, Mark};
use crate::find;
use crate::keys::{Key, KeyEvent};
use crate::kind::Kind;
use crate::rebase;
use crate::tap::{Dir, Taps};

// The views Tab and Shift+Tab cycle through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Edit,
    Files,
    Rebase,
}

// How editing ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Save,
    Cancel,
}

// A question or screen shown over the view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Prompt {
    None,
    Discard,
    Delete(Vec<String>),
    Help,
}

// The find bar: the query, the replacement, and whether replacing and which field has focus.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FindBar {
    pub query: String,
    pub with: String,
    pub replace: bool,
    pub on_with: bool,
    pub origin: Pos,
}

// The whole editor state.
pub struct Editor {
    pub buf: Buffer,
    pub kind: Kind,
    pub comment: char,
    pub files: Vec<Entry>,
    files_start: Vec<Mark>,
    files_undo: Vec<Vec<Mark>>,
    pub views: Vec<View>,
    pub view: View,
    pub list_row: usize,
    pub prompt: Prompt,
    pub find: Option<FindBar>,
    pub message: String,
    pub page: usize,
    pub osc52: Option<String>,
    taps: Taps,
    tap_from: Pos,
    tap_row: usize,
    clipboard: Box<dyn Clipboard>,
    pub assist: Option<Assist>,
    now: u64,
}

impl Editor {
    // Creates an editor for a buffer; a Files column is offered when files are given, a Rebase view for todo lists, and an agent starts composing when the message is empty.
    pub fn new(
        buf: Buffer,
        kind: Kind,
        comment: char,
        files: Vec<Entry>,
        clipboard: Box<dyn Clipboard>,
        assist: Option<Assist>,
    ) -> Editor {
        let mut views = vec![View::Edit];
        if !files.is_empty() {
            views.push(View::Files);
        }
        if kind == Kind::Rebase {
            views.push(View::Rebase);
        }
        let view = if kind == Kind::Rebase { View::Rebase } else { View::Edit };
        let mut ed = Editor {
            buf,
            kind,
            comment,
            files_start: files.iter().map(|e| e.mark).collect(),
            files,
            files_undo: Vec::new(),
            views,
            view,
            list_row: 0,
            prompt: Prompt::None,
            find: None,
            message: String::new(),
            page: 10,
            osc52: None,
            taps: Taps::default(),
            tap_from: Pos::default(),
            tap_row: 0,
            clipboard,
            assist,
            now: 0,
        };
        if ed.message_empty() {
            if let Some(a) = ed.assist.as_mut() {
                a.start(&ed.files, 0);
            }
        }
        ed
    }

    // Returns true if the editable text has nothing but blank and comment lines.
    pub fn message_empty(&self) -> bool {
        self.buf.lines.iter().all(|l| l.first() == Some(&self.comment) || l.iter().all(|c| c.is_whitespace()))
    }

    // Returns the time of the last key press or tick, in milliseconds since the editor started.
    pub fn now(&self) -> u64 {
        self.now
    }

    // Returns true while the agent works or a save waits for it, so the screen keeps updating without key presses.
    pub fn busy(&self) -> bool {
        self.assist.as_ref().is_some_and(|a| a.busy() || a.save_waiting)
    }

    // Advances the agent at the given time; returns whether the screen changed and how editing ended, if a waiting save finished.
    pub fn tick(&mut self, now_ms: u64) -> (bool, Option<Outcome>) {
        self.now = now_ms;
        let Some(a) = self.assist.as_mut() else { return (false, None) };
        let changed = a.tick(&self.files, now_ms);
        if !a.save_waiting {
            return (changed, None);
        }
        match &a.status {
            Status::Ready(_) => {
                a.save_waiting = false;
                (true, self.save())
            }
            Status::Failed(_) => {
                a.save_waiting = false;
                self.message = "No suggestion: write a message (Esc cancels)".to_string();
                (true, None)
            }
            _ => (changed, None),
        }
    }

    // Returns true if the text or any file mark changed since loading.
    pub fn changed(&self) -> bool {
        self.buf.changed() || self.files.iter().map(|e| e.mark).ne(self.files_start.iter().copied())
    }

    // Handles one key press at the given time; returns how editing ended, if it did.
    pub fn handle(&mut self, ev: KeyEvent, now_ms: u64) -> Option<Outcome> {
        if !matches!(ev.key, Key::Up | Key::Down | Key::Left | Key::Right) {
            self.taps.reset();
        }
        self.message.clear();
        self.now = now_ms;
        match self.prompt.clone() {
            Prompt::Help => {
                self.prompt = Prompt::None;
                return None;
            }
            Prompt::Discard => return self.answer(&ev, Outcome::Cancel),
            Prompt::Delete(_) => return self.answer(&ev, Outcome::Save),
            Prompt::None => {}
        }
        if let Some(bar) = self.find.take() {
            self.find_key(bar, ev);
            return None;
        }
        match ev.key {
            Key::Ctrl('s') => return self.save(),
            Key::Esc => return self.cancel(),
            Key::F1 | Key::Ctrl('g') => self.prompt = Prompt::Help,
            Key::Tab => self.cycle_view(1),
            Key::BackTab => self.cycle_view(self.views.len() - 1),
            Key::Ctrl('f') => self.open_find(false),
            Key::Ctrl('r') => self.open_find(true),
            Key::Ctrl('n') | Key::Ctrl('e') => self.cycle_assist(ev.key == Key::Ctrl('n')),
            _ => match self.view {
                View::Edit => self.edit_key(ev, now_ms),
                View::Files => self.files_key(ev, now_ms),
                View::Rebase => self.rebase_key(ev, now_ms),
            },
        }
        None
    }

    // Handles y/n for a prompt: y ends editing with the outcome, anything else closes the prompt.
    fn answer(&mut self, ev: &KeyEvent, outcome: Outcome) -> Option<Outcome> {
        self.prompt = Prompt::None;
        matches!(ev.key, Key::Char('y') | Key::Char('Y')).then_some(outcome)
    }

    // Switches the agent or its effort, which starts composing again.
    fn cycle_assist(&mut self, agent: bool) {
        let (files, now) = (&self.files, self.now);
        self.message = match self.assist.as_mut() {
            Some(a) if agent => a.cycle_agent(files, now),
            Some(a) => a.cycle_effort(files, now),
            None => "Agents only suggest commit messages".to_string(),
        };
    }

    // Fills an empty commit message with the agent's suggestion, or waits for it; returns false while waiting.
    fn use_suggestion(&mut self) -> bool {
        if self.kind != Kind::Commit || !self.message_empty() {
            return true;
        }
        let Some(a) = self.assist.as_mut() else { return true };
        match a.status.clone() {
            Status::Ready(s) => {
                self.buf.checkpoint(false);
                self.buf.lines = s.lines().map(|l| l.chars().collect()).collect();
                self.buf.move_to(Pos::default(), false);
                true
            }
            Status::Composing { .. } | Status::Idle => {
                if a.status == Status::Idle {
                    a.start(&self.files, self.now);
                }
                a.save_waiting = true;
                let name = a.agent().map_or("the agent", |g| g.name);
                self.message = format!("Saving when {name} is done (Esc stops waiting)");
                false
            }
            _ => true,
        }
    }

    // Saves unless the content would be rejected; an empty commit message takes the agent's suggestion; asks first when files are marked for deletion.
    fn save(&mut self) -> Option<Outcome> {
        if !self.use_suggestion() {
            return None;
        }
        if let Some(problem) = self.problem() {
            self.message = problem;
            return None;
        }
        let deletions = files::deletions(&self.files);
        if !deletions.is_empty() {
            self.prompt = Prompt::Delete(deletions);
            return None;
        }
        Some(Outcome::Save)
    }

    // Returns why saving now would fail: an empty commit message or a todo list git rejects.
    fn problem(&self) -> Option<String> {
        match self.kind {
            Kind::Commit => self.message_empty().then(|| "The message is empty (Esc cancels)".to_string()),
            Kind::Rebase => {
                rebase::warning(&self.buf.lines.iter().map(|l| l.iter().collect()).collect::<Vec<String>>())
            }
            _ => None,
        }
    }

    // Stops a save that waits for the agent; otherwise cancels at once if nothing changed, or asks first.
    fn cancel(&mut self) -> Option<Outcome> {
        if let Some(a) = self.assist.as_mut().filter(|a| a.save_waiting) {
            a.save_waiting = false;
            self.message = "Stopped waiting; not saved".to_string();
            return None;
        }
        if !self.changed() {
            return Some(Outcome::Cancel);
        }
        self.prompt = Prompt::Discard;
        None
    }

    // Switches to the view `step` places further along the cycle.
    fn cycle_view(&mut self, step: usize) {
        let at = self.views.iter().position(|v| *v == self.view).unwrap_or(0);
        self.view = self.views[(at + step) % self.views.len()];
    }

    // Opens the find bar, or the find-and-replace bar while editing.
    fn open_find(&mut self, replace: bool) {
        if replace && self.view != View::Edit {
            self.message = "Replace works while editing (Tab)".to_string();
            return;
        }
        let query = self.buf.selected_text().filter(|s| !s.contains('\n')).unwrap_or_default();
        let origin = self.buf.selection().map_or(self.buf.cursor, |(a, _)| a);
        self.find = Some(FindBar { query, replace, origin, ..FindBar::default() });
    }

    // Returns the rows a list view shows, for finding in them.
    fn list_rows(&self) -> Vec<String> {
        match self.view {
            View::Files => self.files.iter().map(Entry::row).collect(),
            _ => self.buf.lines.iter().map(|l| l.iter().collect()).collect(),
        }
    }

    // Handles a key while the find bar is open.
    fn find_key(&mut self, mut bar: FindBar, ev: KeyEvent) {
        let field = if bar.on_with { &mut bar.with } else { &mut bar.query };
        let mut search = false;
        let mut next = false;
        match ev.key {
            Key::Esc => return,
            Key::Char(c) => {
                field.push(c);
                search = !bar.on_with;
            }
            Key::Paste(s) => {
                field.push_str(&s.replace('\n', " "));
                search = !bar.on_with;
            }
            Key::Backspace => {
                field.pop();
                search = !bar.on_with;
            }
            Key::Tab | Key::BackTab if bar.replace => bar.on_with = !bar.on_with,
            Key::Alt('a') if bar.replace => self.replace_all(&bar),
            Key::Enter if bar.replace => self.replace_one(&bar),
            Key::Enter => next = true,
            Key::Ctrl('f') => bar.replace = false,
            Key::Ctrl('r') if self.view == View::Edit => bar.replace = true,
            _ => {}
        }
        if search || next {
            self.search(&bar, next);
        }
        self.find = Some(bar);
    }

    // Moves to the next match: from where the bar opened while typing, past the current match on Enter.
    fn search(&mut self, bar: &FindBar, next: bool) {
        if self.view != View::Edit {
            let rows = self.list_rows();
            let from = self.current_row() + usize::from(next);
            match find::find_row(&rows, &bar.query, from % rows.len().max(1)) {
                Some(r) => self.set_row(r),
                None => self.message = "Not found".to_string(),
            }
            return;
        }
        let from = if next { self.buf.cursor } else { bar.origin };
        match find::find_next(&self.buf.lines, &bar.query, from) {
            Some((a, b)) => {
                self.buf.anchor = Some(a);
                self.buf.cursor = b;
            }
            None => self.message = "Not found".to_string(),
        }
    }

    // Replaces the selected match and moves to the next one, or just finds the next match.
    fn replace_one(&mut self, bar: &FindBar) {
        let sel = self.buf.selection();
        let at = sel.map_or(self.buf.cursor, |(a, _)| a);
        if sel.is_some() && sel == find::find_next(&self.buf.lines, &bar.query, at) {
            self.buf.checkpoint(false);
            self.buf.insert(&bar.with);
        }
        self.search(bar, true);
    }

    // Replaces every match as one undo step.
    fn replace_all(&mut self, bar: &FindBar) {
        self.buf.checkpoint(false);
        let n = find::replace_all(&mut self.buf.lines, &bar.query, &bar.with);
        self.buf.anchor = None;
        let row = self.buf.cursor.row;
        self.buf.cursor.col = self.buf.cursor.col.min(self.buf.lines[row].len());
        self.message = format!("Replaced {n}");
    }

    // Returns the cursor row of the current list view.
    fn current_row(&self) -> usize {
        match self.view {
            View::Files => self.list_row,
            _ => self.buf.cursor.row,
        }
    }

    // Puts the cursor of the current list view on a row.
    fn set_row(&mut self, row: usize) {
        match self.view {
            View::Files => self.list_row = row,
            _ => self.buf.move_to(Pos { row, col: 0 }, false),
        }
    }

    // Returns the direction of an arrow key.
    fn dir(key: &Key) -> Option<Dir> {
        match key {
            Key::Up => Some(Dir::Up),
            Key::Down => Some(Dir::Down),
            Key::Left => Some(Dir::Left),
            Key::Right => Some(Dir::Right),
            _ => None,
        }
    }

    // Handles a key in the editing view.
    fn edit_key(&mut self, ev: KeyEvent, now_ms: u64) {
        if let Some(dir) = Self::dir(&ev.key) {
            return self.edit_arrow(dir, &ev, now_ms);
        }
        let typing = matches!(ev.key, Key::Char(_));
        if !typing {
            self.buf.break_typing();
        }
        match ev.key {
            Key::Char(c) => {
                self.buf.checkpoint(true);
                self.buf.insert(&c.to_string());
            }
            Key::Enter => self.edit(|b| b.insert("\n")),
            Key::Paste(s) => self.edit(|b| b.insert(&s)),
            Key::Backspace => self.edit(Buffer::backspace),
            Key::Delete => self.edit(Buffer::delete),
            Key::Ctrl('k') => self.edit(Buffer::kill_to_end),
            Key::Ctrl('z') => self.history(true),
            Key::Ctrl('y') => self.history(false),
            Key::Ctrl('a') => self.buf.select_all(),
            Key::Ctrl('c') => self.copy(false),
            Key::Ctrl('x') => self.copy(true),
            Key::Ctrl('v') => self.paste(),
            Key::PageUp => self.page_move(-1, ev.shift),
            Key::PageDown => self.page_move(1, ev.shift),
            _ => {}
        }
    }

    // Runs an edit as its own undo step.
    fn edit(&mut self, f: impl FnOnce(&mut Buffer)) {
        self.buf.checkpoint(false);
        f(&mut self.buf);
    }

    // Undoes or redoes, saying so when there is nothing to undo or redo.
    fn history(&mut self, undo: bool) {
        let done = if undo { self.buf.undo() } else { self.buf.redo() };
        if !done {
            self.message = if undo { "Nothing to undo" } else { "Nothing to redo" }.to_string();
        }
    }

    // Copies, or cuts, the selection to the clipboard.
    fn copy(&mut self, cut: bool) {
        let Some(text) = self.buf.selected_text() else {
            self.message = "Nothing selected".to_string();
            return;
        };
        self.clipboard.copy(&text);
        self.osc52 = Some(text);
        if cut {
            self.edit(|b| {
                b.delete_selection();
            });
        }
    }

    // Pastes the clipboard at the cursor.
    fn paste(&mut self) {
        match self.clipboard.paste() {
            Some(text) => self.edit(|b| b.insert(&text)),
            None => self.message = "The clipboard is empty".to_string(),
        }
    }

    // Moves a page up or down.
    fn page_move(&mut self, sign: isize, select: bool) {
        let p = self.buf.vertical(self.buf.cursor, sign * self.page as isize);
        self.buf.move_to(p, select);
    }

    // Moves the cursor for an arrow press: one step, a word or page on a double tap, the line or text edge on a triple tap.
    fn edit_arrow(&mut self, dir: Dir, ev: &KeyEvent, now_ms: u64) {
        if ev.alt && matches!(dir, Dir::Up | Dir::Down) {
            self.buf.checkpoint(false);
            self.buf.move_line(dir == Dir::Up);
            return;
        }
        let count = self.taps.press(dir, now_ms, ev.repeat);
        if count == 1 {
            self.tap_from = self.buf.cursor;
        }
        let b = &self.buf;
        let p = self.tap_from;
        let page = self.page as isize;
        let target = match (dir, count) {
            (Dir::Left, 1) => b.left_of(p),
            (Dir::Right, 1) => b.right_of(p),
            (Dir::Left, 2) => b.word_left(p),
            (Dir::Right, 2) => b.word_right(p),
            (Dir::Left, _) => b.line_edge(p, false),
            (Dir::Right, _) => b.line_edge(p, true),
            (Dir::Up, 1) => b.vertical(p, -1),
            (Dir::Down, 1) => b.vertical(p, 1),
            (Dir::Up, 2) => b.vertical(p, -page),
            (Dir::Down, 2) => b.vertical(p, page),
            (Dir::Up, _) => b.text_edge(false),
            (Dir::Down, _) => b.text_edge(true),
        };
        self.buf.move_to(target, ev.shift);
    }

    // Returns the row a list cursor moves to for an up/down press with taps, or None for left/right.
    fn list_arrow(&mut self, ev: &KeyEvent, now_ms: u64, rows: usize) -> Option<usize> {
        let dir = Self::dir(&ev.key).filter(|d| matches!(d, Dir::Up | Dir::Down))?;
        let count = self.taps.press(dir, now_ms, ev.repeat);
        if count == 1 {
            self.tap_row = self.current_row();
        }
        let last = rows.saturating_sub(1) as isize;
        let step = match count {
            1 => 1,
            2 => self.page as isize,
            _ => last + 1,
        };
        let sign = if dir == Dir::Up { -1 } else { 1 };
        Some((self.tap_row as isize + sign * step).clamp(0, last) as usize)
    }

    // Handles a key in the Files view.
    fn files_key(&mut self, ev: KeyEvent, now_ms: u64) {
        let n = self.files.len();
        if let Some(row) = self.list_arrow(&ev, now_ms, n) {
            self.list_row = row;
            return;
        }
        if ev.key == Key::Ctrl('z') {
            match self.files_undo.pop() {
                Some(marks) => {
                    self.files.iter_mut().zip(marks).for_each(|(e, m)| e.mark = m);
                    self.marks_changed();
                }
                None => self.message = "Nothing to undo".to_string(),
            }
            return;
        }
        let snapshot: Vec<Mark> = self.files.iter().map(|e| e.mark).collect();
        let row = self.list_row;
        match ev.key {
            Key::Char(' ') => self.files[row].cycle(),
            Key::Char('a') | Key::Char('A') => files::toggle_all(&mut self.files),
            Key::Backspace => self.files[row].mark = Mark::Delete,
            Key::PageUp => self.list_row = row.saturating_sub(self.page),
            Key::PageDown => self.list_row = (row + self.page).min(n - 1),
            _ => {}
        }
        if self.files.iter().map(|e| e.mark).ne(snapshot.iter().copied()) {
            self.files_undo.push(snapshot);
            self.marks_changed();
        }
    }

    // Tells the agent the marks changed, so it composes again for the new set of files.
    fn marks_changed(&mut self) {
        if let Some(a) = self.assist.as_mut() {
            a.marks_changed(self.now);
        }
    }

    // Handles a key in the Rebase view.
    fn rebase_key(&mut self, ev: KeyEvent, now_ms: u64) {
        let n = self.buf.lines.len();
        if ev.alt && matches!(ev.key, Key::Up | Key::Down) {
            self.buf.checkpoint(false);
            self.buf.move_line(ev.key == Key::Up);
            return;
        }
        if let Some(row) = self.list_arrow(&ev, now_ms, n) {
            self.buf.move_to(Pos { row, col: 0 }, false);
            return;
        }
        let row = self.buf.cursor.row;
        match ev.key {
            Key::Char(c) => {
                let line = self.buf.line(row);
                match rebase::command_for_key(c).and_then(|cmd| rebase::set_command(&line, cmd)) {
                    Some(new) => self.edit(|b| b.set_line(row, &new)),
                    None => self.message = "p r e s f d set the command of a commit line".to_string(),
                }
            }
            Key::Ctrl('z') => self.history(true),
            Key::Ctrl('y') => self.history(false),
            Key::PageUp => self.buf.move_to(self.buf.vertical(self.buf.cursor, -(self.page as isize)), false),
            Key::PageDown => self.buf.move_to(self.buf.vertical(self.buf.cursor, self.page as isize), false),
            _ => {}
        }
    }
}

#[cfg(test)]
#[path = "editor_tests.rs"]
mod tests;
