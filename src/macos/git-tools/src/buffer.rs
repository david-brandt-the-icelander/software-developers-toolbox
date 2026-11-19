// The text being edited: editable lines, a read-only tail (git's comment block), the cursor, the selection and undo history.

// Most undo steps kept.
const UNDO_LIMIT: usize = 200;

// A position in the editable lines: row and character column.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pos {
    pub row: usize,
    pub col: usize,
}

// One undo step: the lines and cursor before an edit.
#[derive(Clone, Debug)]
struct Snapshot {
    lines: Vec<Vec<char>>,
    cursor: Pos,
}

// The edit buffer.
#[derive(Debug)]
pub struct Buffer {
    pub lines: Vec<Vec<char>>,
    pub tail: Vec<String>,
    pub cursor: Pos,
    pub anchor: Option<Pos>,
    original: Vec<Vec<char>>,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    typing: bool,
}

// Returns true for characters that make up words for word movement.
fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

impl Buffer {
    // Creates a buffer from editable lines and a read-only tail.
    pub fn new(editable: Vec<String>, tail: Vec<String>) -> Buffer {
        let mut lines: Vec<Vec<char>> = editable.iter().map(|l| l.chars().collect()).collect();
        if lines.is_empty() {
            lines.push(Vec::new());
        }
        Buffer {
            original: lines.clone(),
            lines,
            tail,
            cursor: Pos::default(),
            anchor: None,
            undo: Vec::new(),
            redo: Vec::new(),
            typing: false,
        }
    }

    // Returns the whole text, editable lines then the tail, each line ending in a newline.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for l in self.lines.iter().map(|l| l.iter().collect::<String>()).chain(self.tail.iter().cloned()) {
            out.push_str(&l);
            out.push('\n');
        }
        out
    }

    // Returns only the editable lines, joined with newlines.
    pub fn editable_text(&self) -> String {
        self.lines.iter().map(|l| l.iter().collect::<String>()).collect::<Vec<_>>().join("\n")
    }

    // Returns true if the editable lines differ from what was loaded.
    pub fn changed(&self) -> bool {
        self.lines != self.original
    }

    // Returns one line as a string.
    pub fn line(&self, row: usize) -> String {
        self.lines[row].iter().collect()
    }

    // Records an undo step before an edit; consecutive typing shares one step.
    pub fn checkpoint(&mut self, typing: bool) {
        if typing && self.typing {
            return;
        }
        self.typing = typing;
        self.undo.push(Snapshot { lines: self.lines.clone(), cursor: self.cursor });
        if self.undo.len() > UNDO_LIMIT {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    // Ends a run of typing so the next edit gets its own undo step.
    pub fn break_typing(&mut self) {
        self.typing = false;
    }

    // Undoes the last edit; returns false if there was none.
    pub fn undo(&mut self) -> bool {
        self.swap_history(true)
    }

    // Redoes the last undone edit; returns false if there was none.
    pub fn redo(&mut self) -> bool {
        self.swap_history(false)
    }

    // Moves one snapshot between the undo and redo stacks, restoring it.
    fn swap_history(&mut self, undo: bool) -> bool {
        let (from, to) =
            if undo { (&mut self.undo, &mut self.redo) } else { (&mut self.redo, &mut self.undo) };
        let Some(snap) = from.pop() else { return false };
        to.push(Snapshot { lines: std::mem::take(&mut self.lines), cursor: self.cursor });
        self.lines = snap.lines;
        self.cursor = snap.cursor;
        self.anchor = None;
        self.typing = false;
        true
    }

    // Returns the ordered selection range, if any text is selected.
    pub fn selection(&self) -> Option<(Pos, Pos)> {
        let a = self.anchor?;
        match a.cmp(&self.cursor) {
            std::cmp::Ordering::Less => Some((a, self.cursor)),
            std::cmp::Ordering::Greater => Some((self.cursor, a)),
            std::cmp::Ordering::Equal => None,
        }
    }

    // Returns the selected text.
    pub fn selected_text(&self) -> Option<String> {
        let (a, b) = self.selection()?;
        Some(self.slice(a, b))
    }

    // Returns the text between two ordered positions.
    fn slice(&self, a: Pos, b: Pos) -> String {
        if a.row == b.row {
            return self.lines[a.row][a.col..b.col].iter().collect();
        }
        let mut out: String = self.lines[a.row][a.col..].iter().collect();
        for line in &self.lines[a.row + 1..b.row] {
            out.push('\n');
            out.extend(line.iter());
        }
        out.push('\n');
        out.extend(self.lines[b.row][..b.col].iter());
        out
    }

    // Removes the text between two ordered positions and puts the cursor at the start.
    fn remove(&mut self, a: Pos, b: Pos) {
        let tail: Vec<char> = self.lines[b.row][b.col..].to_vec();
        self.lines[a.row].truncate(a.col);
        self.lines[a.row].extend(tail);
        self.lines.drain(a.row + 1..=b.row);
        self.cursor = a;
        self.anchor = None;
    }

    // Deletes the selection; returns false if nothing was selected.
    pub fn delete_selection(&mut self) -> bool {
        let Some((a, b)) = self.selection() else {
            self.anchor = None;
            return false;
        };
        self.remove(a, b);
        true
    }

    // Selects all editable text.
    pub fn select_all(&mut self) {
        self.anchor = Some(Pos::default());
        let row = self.lines.len() - 1;
        self.cursor = Pos { row, col: self.lines[row].len() };
    }

    // Inserts text at the cursor, replacing any selection; newlines split lines.
    pub fn insert(&mut self, text: &str) {
        self.delete_selection();
        let Pos { row, col } = self.cursor;
        let rest: Vec<char> = self.lines[row].split_off(col);
        let mut parts = text.split('\n');
        self.lines[row].extend(parts.next().unwrap_or("").chars());
        let mut r = row;
        for part in parts {
            r += 1;
            self.lines.insert(r, part.chars().collect());
        }
        let col = self.lines[r].len();
        self.lines[r].extend(rest);
        self.cursor = Pos { row: r, col };
    }

    // Deletes the selection or the character before the cursor, joining lines at a line start.
    pub fn backspace(&mut self) {
        if self.delete_selection() {
            return;
        }
        let Pos { row, col } = self.cursor;
        if col > 0 {
            self.remove(Pos { row, col: col - 1 }, self.cursor);
        } else if row > 0 {
            let prev = Pos { row: row - 1, col: self.lines[row - 1].len() };
            self.remove(prev, self.cursor);
        }
    }

    // Deletes the selection or the character after the cursor, joining lines at a line end.
    pub fn delete(&mut self) {
        if self.delete_selection() {
            return;
        }
        let end = self.right_of(self.cursor);
        if end != self.cursor {
            self.remove(self.cursor, end);
        }
    }

    // Deletes from the cursor to the end of the line, or joins the next line when already at the end.
    pub fn kill_to_end(&mut self) {
        self.anchor = None;
        let Pos { row, col } = self.cursor;
        let end = if col < self.lines[row].len() {
            Pos { row, col: self.lines[row].len() }
        } else {
            self.right_of(self.cursor)
        };
        if end != self.cursor {
            self.remove(self.cursor, end);
        }
    }

    // Swaps the cursor's line with the one above or below, keeping the cursor on it.
    pub fn move_line(&mut self, up: bool) -> bool {
        let row = self.cursor.row;
        let other = if up { row.checked_sub(1) } else { Some(row + 1).filter(|r| *r < self.lines.len()) };
        let Some(other) = other else { return false };
        self.lines.swap(row, other);
        self.cursor.row = other;
        self.anchor = None;
        true
    }

    // Replaces a whole line.
    pub fn set_line(&mut self, row: usize, text: &str) {
        self.lines[row] = text.chars().collect();
        self.cursor.col = self.cursor.col.min(self.lines[self.cursor.row].len());
    }

    // Returns the position one character to the left, moving to the previous line end at a line start.
    pub fn left_of(&self, p: Pos) -> Pos {
        match (p.col, p.row) {
            (0, 0) => p,
            (0, r) => Pos { row: r - 1, col: self.lines[r - 1].len() },
            (c, r) => Pos { row: r, col: c - 1 },
        }
    }

    // Returns the position one character to the right, moving to the next line start at a line end.
    pub fn right_of(&self, p: Pos) -> Pos {
        if p.col < self.lines[p.row].len() {
            return Pos { row: p.row, col: p.col + 1 };
        }
        if p.row + 1 < self.lines.len() {
            return Pos { row: p.row + 1, col: 0 };
        }
        p
    }

    // Returns the start of the word before the position.
    pub fn word_left(&self, p: Pos) -> Pos {
        if p.col == 0 {
            return self.left_of(p);
        }
        let line = &self.lines[p.row];
        let mut c = p.col;
        while c > 0 && !is_word(line[c - 1]) {
            c -= 1;
        }
        while c > 0 && is_word(line[c - 1]) {
            c -= 1;
        }
        Pos { row: p.row, col: c }
    }

    // Returns the start of the next word after the position, or the line end.
    pub fn word_right(&self, p: Pos) -> Pos {
        let line = &self.lines[p.row];
        if p.col >= line.len() {
            return self.right_of(p);
        }
        let mut c = p.col;
        while c < line.len() && is_word(line[c]) {
            c += 1;
        }
        while c < line.len() && !is_word(line[c]) {
            c += 1;
        }
        Pos { row: p.row, col: c }
    }

    // Returns the position the given number of rows up or down, keeping the column where it fits.
    pub fn vertical(&self, p: Pos, rows: isize) -> Pos {
        let last = self.lines.len() as isize - 1;
        let row = (p.row as isize + rows).clamp(0, last) as usize;
        Pos { row, col: p.col.min(self.lines[row].len()) }
    }

    // Returns the start or end of the position's line.
    pub fn line_edge(&self, p: Pos, end: bool) -> Pos {
        Pos { row: p.row, col: if end { self.lines[p.row].len() } else { 0 } }
    }

    // Returns the start or end of the editable text.
    pub fn text_edge(&self, end: bool) -> Pos {
        if !end {
            return Pos::default();
        }
        let row = self.lines.len() - 1;
        Pos { row, col: self.lines[row].len() }
    }

    // Moves the cursor, extending the selection when selecting and clearing it otherwise.
    pub fn move_to(&mut self, p: Pos, select: bool) {
        if select && self.anchor.is_none() {
            self.anchor = Some(self.cursor);
        }
        if !select {
            self.anchor = None;
        }
        self.cursor = p;
        self.typing = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Builds a buffer from text lines with no tail.
    fn buf(lines: &[&str]) -> Buffer {
        Buffer::new(lines.iter().map(|s| s.to_string()).collect(), Vec::new())
    }

    // Checks text output with a tail, editable text, and an empty buffer.
    #[test]
    fn test_buffer_text_and_tail() {
        let b = Buffer::new(vec!["a".into(), "b".into()], vec!["# c".into()]);
        assert_eq!(b.text(), "a\nb\n# c\n");
        assert_eq!(b.editable_text(), "a\nb");
        let e = Buffer::new(Vec::new(), Vec::new());
        assert_eq!(e.lines.len(), 1);
        assert!(!e.changed());
    }

    // Checks inserting single and multi-line text, and replacing a selection.
    #[test]
    fn test_buffer_insert() {
        let mut b = buf(&["hello world"]);
        b.cursor = Pos { row: 0, col: 5 };
        b.insert(",\nnew");
        assert_eq!(b.text(), "hello,\nnew world\n");
        assert_eq!(b.cursor, Pos { row: 1, col: 3 });
        b.anchor = Some(Pos { row: 0, col: 0 });
        b.insert("X");
        assert_eq!(b.text(), "X world\n");
        assert!(b.changed());
    }

    // Checks backspace and delete inside lines, across line breaks and at the edges.
    #[test]
    fn test_buffer_backspace_delete() {
        let mut b = buf(&["ab", "cd"]);
        b.cursor = Pos { row: 1, col: 0 };
        b.backspace();
        assert_eq!(b.text(), "abcd\n");
        b.backspace();
        assert_eq!(b.text(), "acd\n");
        b.cursor = Pos::default();
        b.backspace();
        assert_eq!(b.text(), "acd\n");
        b.delete();
        assert_eq!(b.text(), "cd\n");
        b.cursor = Pos { row: 0, col: 2 };
        b.delete();
        assert_eq!(b.text(), "cd\n");
        let mut s = buf(&["abc", "def"]);
        s.anchor = Some(Pos { row: 0, col: 1 });
        s.cursor = Pos { row: 1, col: 1 };
        assert_eq!(s.selected_text().as_deref(), Some("bc\nd"));
        s.delete();
        assert_eq!(s.text(), "aef\n");
        s.anchor = Some(Pos { row: 0, col: 2 });
        s.cursor = Pos { row: 0, col: 1 };
        s.backspace();
        assert_eq!(s.text(), "af\n");
    }

    // Checks selection order, selecting all, and multi-line slices.
    #[test]
    fn test_buffer_selection() {
        let mut b = buf(&["one", "two", "three"]);
        assert_eq!(b.selection(), None);
        b.anchor = Some(Pos::default());
        assert_eq!(b.selection(), None);
        b.select_all();
        assert_eq!(b.selected_text().as_deref(), Some("one\ntwo\nthree"));
        b.anchor = Some(Pos { row: 2, col: 2 });
        b.cursor = Pos { row: 0, col: 1 };
        assert_eq!(b.selected_text().as_deref(), Some("ne\ntwo\nth"));
        assert!(!buf(&["x"]).delete_selection());
    }

    // Checks deleting to the end of the line and joining at the end.
    #[test]
    fn test_buffer_kill_to_end() {
        let mut b = buf(&["abc", "def"]);
        b.cursor = Pos { row: 0, col: 1 };
        b.kill_to_end();
        assert_eq!(b.text(), "a\ndef\n");
        b.kill_to_end();
        assert_eq!(b.text(), "adef\n");
        b.cursor = Pos { row: 0, col: 4 };
        b.kill_to_end();
        assert_eq!(b.text(), "adef\n");
    }

    // Checks moving lines up and down and the edges where they cannot move.
    #[test]
    fn test_buffer_move_line() {
        let mut b = buf(&["a", "b", "c"]);
        assert!(!b.move_line(true));
        assert!(b.move_line(false));
        assert_eq!(b.text(), "b\na\nc\n");
        assert_eq!(b.cursor.row, 1);
        b.cursor.row = 2;
        assert!(!b.move_line(false));
        assert!(b.move_line(true));
        assert_eq!(b.text(), "b\nc\na\n");
        b.set_line(0, "pick");
        assert_eq!(b.line(0), "pick");
    }

    // Checks undo and redo, typing coalescing and the history limit.
    #[test]
    fn test_buffer_undo_redo() {
        let mut b = buf(&[""]);
        assert!(!b.undo());
        for c in ["a", "b"] {
            b.checkpoint(true);
            b.insert(c);
        }
        b.break_typing();
        b.checkpoint(false);
        b.insert("!");
        assert!(b.undo());
        assert_eq!(b.text(), "ab\n");
        assert!(b.undo());
        assert_eq!(b.text(), "\n");
        assert!(b.redo());
        assert_eq!(b.text(), "ab\n");
        assert!(b.redo());
        assert!(!b.redo());
        for _ in 0..UNDO_LIMIT + 5 {
            b.checkpoint(false);
        }
        assert_eq!(b.undo.len(), UNDO_LIMIT);
    }

    // Checks character, word, line, page and text movement.
    #[test]
    fn test_buffer_movement() {
        let b = buf(&["foo, bar_baz", "", "x"]);
        let p = |row, col| Pos { row, col };
        assert_eq!(b.left_of(p(0, 0)), p(0, 0));
        assert_eq!(b.left_of(p(1, 0)), p(0, 12));
        assert_eq!(b.left_of(p(0, 3)), p(0, 2));
        assert_eq!(b.right_of(p(0, 12)), p(1, 0));
        assert_eq!(b.right_of(p(2, 1)), p(2, 1));
        assert_eq!(b.word_right(p(0, 0)), p(0, 5));
        assert_eq!(b.word_right(p(0, 5)), p(0, 12));
        assert_eq!(b.word_right(p(0, 12)), p(1, 0));
        assert_eq!(b.word_left(p(0, 12)), p(0, 5));
        assert_eq!(b.word_left(p(0, 5)), p(0, 0));
        assert_eq!(b.word_left(p(1, 0)), p(0, 12));
        assert_eq!(buf(&["  x"]).word_left(p(0, 2)), p(0, 0));
        assert_eq!(b.vertical(p(0, 10), 2), p(2, 1));
        assert_eq!(b.vertical(p(2, 0), -10), p(0, 0));
        assert_eq!(b.line_edge(p(0, 3), true), p(0, 12));
        assert_eq!(b.line_edge(p(0, 3), false), p(0, 0));
        assert_eq!(b.text_edge(true), p(2, 1));
        assert_eq!(b.text_edge(false), p(0, 0));
        let mut m = buf(&["abc"]);
        m.move_to(p(0, 2), true);
        assert_eq!(m.selected_text().as_deref(), Some("ab"));
        m.move_to(p(0, 1), false);
        assert_eq!(m.anchor, None);
    }
}
