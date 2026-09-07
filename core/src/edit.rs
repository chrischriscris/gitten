//! A text field: cursor, selection, and edit — the one input model.
//!
//! The terminal's prompt, the window's input panel and any third client's
//! field are the same editing problem, and the first prompt solved it as an
//! append-only `String` because it stood on a status line. A cursor made the
//! two clients' answers drift by construction, so the model is here, in the
//! crate with no dependencies and no UI: what a character lands as, what the
//! two delete keys each mean, where the arrow keys, Home and End go, and what
//! a paste is sanitized into. A client draws the field; nobody else decides
//! what editing means.
//!
//! # Coordinates
//!
//! The cursor is a **byte offset**, always on a scalar-value boundary — the
//! same unit the edit script, the tokens and the spans address lines by. A
//! character index would need converting before every insert, and a UTF-16
//! index belongs to a client that thinks in UTF-16, not to this model.
//!
//! # The selected state
//!
//! `selected` means *the whole text is selected* — the rename prefill's shape,
//! where the first edit replaces the whole value and a move just lets go. Not
//! a range: a prompt edits one value, and a range would drag a second
//! coordinate system in for one sentence of behavior. The state is the
//! client's to spend; [`Field::edit`] spends it exactly once per edit.

/// One text field: the value, the cursor inside it, and whether the value is
/// wholly selected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    text: String,
    cursor: usize,
    selected: bool,
}

impl Default for Field {
    fn default() -> Self {
        Self::new()
    }
}

impl Field {
    /// An empty field, cursor at the start.
    pub fn new() -> Self {
        Self {
            text: String::new(),
            cursor: 0,
            selected: false,
        }
    }

    /// A field holding `text`, cursor at the end.
    pub fn with(text: impl Into<String>) -> Self {
        let text = text.into();
        let cursor = text.len();
        Self {
            text,
            cursor,
            selected: false,
        }
    }

    /// A field holding `text` wholly selected — the prefill whose first edit
    /// replaces it. The cursor is where the selection ends: the text's end,
    /// so letting go without editing lands at the same place typing would
    /// have appended to.
    pub fn with_selected(text: impl Into<String>) -> Self {
        let mut this = Self::with(text);
        this.selected = !this.text.is_empty();
        this
    }

    /// The value, whole.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Where the cursor is, in bytes into `text`. Always on a scalar-value
    /// boundary.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Whether the whole value is selected — the next edit replaces it.
    pub fn is_selected(&self) -> bool {
        self.selected
    }

    /// Replaces the value, cursor at the end, selection spent. What a prompt
    /// is seeded with mid-flight — a second `/` finds the standing query.
    pub fn set(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.cursor = self.text.len();
        self.selected = false;
    }

    /// The value, taking the field apart.
    pub fn take(self) -> String {
        self.text
    }

    /// Lands one edit. The single door every keystroke and paste comes
    /// through, so a client cannot invent a second meaning for a key without
    /// naming it here.
    pub fn edit(&mut self, edit: Edit) {
        match edit {
            Edit::Char(c) => self.insert_char(c),
            Edit::Newline => self.insert_char('\n'),
            Edit::Backspace => self.backspace(),
            Edit::Delete => self.delete(),
            Edit::Paste(pasted) => self.paste(&pasted, false),
            Edit::PasteMultiline(pasted) => self.paste(&pasted, true),
            Edit::Left => self.move_left(),
            Edit::Right => self.move_right(),
            Edit::WordLeft => self.move_word_left(),
            Edit::WordRight => self.move_word_right(),
            Edit::Home => self.line_home(),
            Edit::End => self.line_end(),
            Edit::Up => self.move_vertical(-1),
            Edit::Down => self.move_vertical(1),
        }
    }

    /// Inserts one character at the cursor. A selected field is replaced
    /// first — the selection is spent by whichever edit lands first.
    pub fn insert_char(&mut self, c: char) {
        if self.selected {
            self.text.clear();
            self.cursor = 0;
            self.selected = false;
        }
        self.text.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    /// Removes the scalar before the cursor; a selected field empties instead.
    pub fn backspace(&mut self) {
        if self.selected {
            self.text.clear();
            self.cursor = 0;
            self.selected = false;
            return;
        }
        let Some(prev) = self.text[..self.cursor]
            .char_indices()
            .next_back()
            .map(|(i, _)| i)
        else {
            return;
        };
        self.text.replace_range(prev..self.cursor, "");
        self.cursor = prev;
    }

    /// Removes the scalar at the cursor. The forward twin of
    /// [`Field::backspace`], and the reason both delete keys exist as
    /// separate edits now that there is a position to differ at.
    pub fn delete(&mut self) {
        if self.selected {
            self.text.clear();
            self.cursor = 0;
            self.selected = false;
            return;
        }
        let Some(next) = self.text[self.cursor..].chars().next().map(char::len_utf8) else {
            return;
        };
        self.text.replace_range(self.cursor..self.cursor + next, "");
    }

    /// A paste's worth of text, inserted whole. One paste, one edit, and
    /// never a transcript of keypresses: line breaks and tabs become spaces
    /// and other control characters are dropped, because that is what fits
    /// the one line a single-line field owns — pass `true` for a field that
    /// holds multiline text, which keeps line breaks (and normalizes `\r\n`)
    /// instead. Nothing here can execute, whatever the paste held.
    pub fn paste(&mut self, pasted: &str, multiline: bool) {
        let clean: String = pasted
            .chars()
            .filter_map(|c| match c {
                '\r' => None,
                '\t' if multiline => Some('\t'),
                '\n' if multiline => Some('\n'),
                '\n' | '\t' => Some(' '),
                c if c.is_control() => None,
                c => Some(c),
            })
            .collect();
        self.insert_str(&clean);
    }

    /// Inserts a string at the cursor, without sanitizing — the private door
    /// [`Field::paste`] and [`Field::insert_char`] land through, and the one
    /// place the boundary arithmetic lives.
    fn insert_str(&mut self, s: &str) {
        if self.selected {
            self.text.clear();
            self.cursor = 0;
            self.selected = false;
        }
        self.text.insert_str(self.cursor, s);
        self.cursor += s.len();
    }

    /// One scalar left. Letting go of a selection costs nothing — the value
    /// is still there, the cursor moves within it.
    pub fn move_left(&mut self) {
        self.selected = false;
        self.cursor = self.text[..self.cursor]
            .char_indices()
            .next_back()
            .map(|(i, _)| i)
            .unwrap_or(0);
    }

    /// One scalar right, clamped at the end.
    pub fn move_right(&mut self) {
        self.selected = false;
        let next = self.text[self.cursor..]
            .chars()
            .next()
            .map(char::len_utf8)
            .unwrap_or(0);
        self.cursor = (self.cursor + next).min(self.text.len());
    }

    /// To the start of the previous word: past the trailing whitespace, then
    /// to the start of the run of word characters — or non-word characters —
    /// the cursor ends in or sits behind. A word is what
    /// `char::is_alphanumeric` answers for; punctuation separates, letters
    /// and digits hold.
    pub fn move_word_left(&mut self) {
        self.selected = false;
        let skipped = self.text[..self.cursor].trim_end();
        let Some(last) = skipped.chars().next_back() else {
            self.cursor = 0;
            return;
        };
        let word = last.is_alphanumeric();
        let mut target = 0;
        for (i, c) in skipped.char_indices().rev() {
            if c.is_alphanumeric() != word {
                target = i + c.len_utf8();
                break;
            }
        }
        self.cursor = target;
    }

    /// To the end of the next word — [`Field::move_word_left`], forward.
    pub fn move_word_right(&mut self) {
        self.selected = false;
        let tail = &self.text[self.cursor..];
        let trimmed = tail.trim_start();
        let offset = self.cursor + (tail.len() - trimmed.len());
        let word = trimmed.chars().next().is_some_and(|c| c.is_alphanumeric());
        for (i, c) in self.text[offset..].char_indices() {
            if c.is_alphanumeric() != word {
                self.cursor = offset + i;
                return;
            }
        }
        self.cursor = self.text.len();
    }

    /// To the start of the line the cursor is on — a multiline field edits
    /// one line at a time in the arrow keys, and Home stays line-scoped.
    pub fn line_home(&mut self) {
        self.selected = false;
        self.cursor = self.line_bounds().0;
    }

    /// To the end of the line the cursor is on.
    pub fn line_end(&mut self) {
        self.selected = false;
        self.cursor = self.line_bounds().1;
    }

    /// One line up or down, keeping the column the cursor had — clamped into
    /// the target line when it is shorter, because a status field has no
    /// second viewport to remember a goal column in.
    pub fn move_vertical(&mut self, by: isize) {
        self.selected = false;
        let (start, end) = self.line_bounds();
        let column = self.text[start..self.cursor].chars().count();
        let (before, this): (&str, &str) = (self.text[..start].as_ref(), self.text[end..].as_ref());
        let target = match by {
            by if by < 0 => before,
            _ => this,
        };
        if target.is_empty() || (by < 0 && before.is_empty()) {
            return;
        }
        let lines: Vec<&str> = match by {
            by if by < 0 => target.lines().collect(),
            _ => target.lines().collect(),
        };
        let line = match by {
            by if by < 0 => lines.last().copied(),
            _ => lines.first().copied(),
        };
        let Some(line) = line else {
            return;
        };
        let start = match by {
            by if by < 0 => start - line.len() - 1,
            _ => end + 1,
        };
        let at = start
            + line
                .char_indices()
                .nth(column)
                .map(|(i, _)| i)
                .unwrap_or(line.len());
        self.cursor = at;
    }

    /// The byte range of the line the cursor is on. What drawing reads to
    /// show the one line a status row can hold, and what the line-number
    /// indicator is computed from.
    pub fn line_bounds(&self) -> (usize, usize) {
        let start = self.text[..self.cursor]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0);
        let end = self.text[self.cursor..]
            .find('\n')
            .map(|i| self.cursor + i)
            .unwrap_or(self.text.len());
        (start, end)
    }

    /// Which line the cursor is on and how many the field holds, both from
    /// one — `2/3` above a message that grew past its status row.
    pub fn line_of(&self) -> (usize, usize) {
        let line = self.text[..self.cursor].matches('\n').count() + 1;
        let lines = self.text.matches('\n').count() + 1;
        (line, lines)
    }

    /// The window to draw: at most `room` scalars of the cursor's line, with
    /// the cursor inside it, plus where the cursor sits in the window. The
    /// end of the text is where the next character goes, so the window
    /// anchors there when the tail is what fits — the old tail-drawing rule,
    /// generalized to a cursor in the middle.
    pub fn window(&self, room: usize) -> (String, usize) {
        let (start, end) = self.line_bounds();
        let line = &self.text[start..end];
        let chars: Vec<(usize, char)> = line.char_indices().collect();
        if chars.len() <= room {
            let at = line[..self.cursor - start].chars().count();
            return (line.to_string(), at);
        }
        // The cursor's own position in the line, in scalars.
        let at = line[..self.cursor - start].chars().count();
        let from = at.saturating_sub(room / 2).min(chars.len() - room);
        let text: String = chars[from..from + room].iter().map(|(_, c)| *c).collect();
        (text, at - from)
    }
}

/// One edit to a field, already reduced by whoever routed the event: the
/// prompt never sees an event, so a pasted `q` is the character `q` and the
/// keymap never learns a paste happened.
#[derive(Debug, Clone)]
pub enum Edit {
    Char(char),
    /// A line break, inserted as text — the key that carries it is a
    /// command's to name, because Enter is accept.
    Newline,
    /// Backspace or Delete, backwards — the single-line prompt's shape.
    Backspace,
    /// Delete, forwards, at the cursor.
    Delete,
    /// A bracketed paste, sanitized to one line.
    Paste(String),
    /// A bracketed paste, keeping line breaks — a message field's shape.
    PasteMultiline(String),
    Left,
    Right,
    WordLeft,
    WordRight,
    Home,
    End,
    Up,
    Down,
}

#[cfg(test)]
mod tests {
    use super::{Edit, Field};

    fn edited(field: &str, edit: Edit) -> String {
        let mut f = Field::with(field);
        f.edit(edit);
        f.take()
    }

    #[test]
    fn edits_land_at_the_cursor_and_not_at_the_end() {
        let mut f = Field::with("abc");
        f.edit(Edit::Left);
        f.edit(Edit::Left);
        f.edit(Edit::Char('X'));
        assert_eq!(f.take(), "aXbc");
    }

    #[test]
    fn a_cursor_never_splits_a_multibyte_scalar() {
        // ß is two bytes; left of it is its own boundary, never its middle.
        let mut f = Field::with("aßc");
        f.edit(Edit::Left);
        f.edit(Edit::Left);
        assert_eq!(f.cursor(), 1);
        f.edit(Edit::Backspace);
        assert_eq!(f.text(), "ßc");
        f.edit(Edit::End);
        f.edit(Edit::Char('ß'));
        assert_eq!(f.take(), "ßcß");
    }

    #[test]
    fn delete_removes_forward_and_backspace_removes_back() {
        let mut f = Field::with("abc");
        f.edit(Edit::Home);
        f.edit(Edit::Delete);
        assert_eq!(f.text(), "bc");
        f.edit(Edit::End);
        f.edit(Edit::Backspace);
        assert_eq!(f.take(), "b");
    }

    #[test]
    fn home_end_and_the_arrows_walk_the_line() {
        let mut f = Field::with("hello");
        f.edit(Edit::Home);
        f.edit(Edit::Char('>'));
        assert_eq!(f.take(), ">hello");
        let mut f = Field::with("hello");
        f.edit(Edit::Home);
        f.edit(Edit::Right);
        f.edit(Edit::Right);
        f.edit(Edit::End);
        f.edit(Edit::Char('!'));
        assert_eq!(f.text(), "hello!");
    }

    #[test]
    fn word_moves_skip_punctuation_and_stop_at_runs() {
        let mut f = Field::with("foo.bar baz");
        f.edit(Edit::End);
        f.edit(Edit::WordLeft);
        assert_eq!(f.cursor(), 8, "the cursor stops at the start of `baz`");
        f.edit(Edit::WordLeft);
        assert_eq!(f.cursor(), 4, "past `.bar` as one run of word characters");
        f.edit(Edit::WordLeft);
        assert_eq!(f.cursor(), 3, "the start of the `.` run");
        f.edit(Edit::WordLeft);
        assert_eq!(f.cursor(), 0);
        f.edit(Edit::WordRight);
        assert_eq!(f.cursor(), 3);
    }

    #[test]
    fn a_paste_is_one_edit_and_single_line_flattens_it() {
        assert_eq!(edited("", Edit::Paste("a\nb\tc".into())), "a b c");
        assert_eq!(
            edited("", Edit::PasteMultiline("a\r\nb\tc\n".into())),
            "a\nb\tc\n"
        );
        // Control characters other than the ones text is made of never land.
        assert_eq!(edited("", Edit::Paste("a\u{1b}q".into())), "aq");
    }

    #[test]
    fn multiline_lines_the_cursor_walks() {
        let mut f = Field::with("one\ntwo\nthree");
        f.edit(Edit::Home);
        assert_eq!(f.line_of(), (3, 3), "Home is line-scoped, not document");
        f.edit(Edit::Up);
        assert_eq!(f.line_of(), (2, 3));
        assert!(f.text()[f.cursor()..].starts_with("two"));
        f.edit(Edit::Up);
        assert_eq!(f.line_of(), (1, 3));
        f.edit(Edit::Up);
        assert_eq!(f.line_of(), (1, 3), "clamped at the first line");
        f.edit(Edit::Down);
        f.edit(Edit::Down);
        assert_eq!(f.line_of(), (3, 3), "clamped at the last line");
        f.edit(Edit::Up);
        assert_eq!(f.line_of(), (2, 3));
    }

    #[test]
    fn a_selected_field_replaces_on_the_first_edit_and_lets_arrows_keep_it() {
        let mut f = Field::with_selected("old name");
        assert!(f.is_selected());
        f.edit(Edit::Left);
        assert!(!f.is_selected(), "a move lets go, it does not replace");
        assert_eq!(f.text(), "old name");
        f.edit(Edit::Right);
        f.edit(Edit::Char('x'));
        assert_eq!(f.take(), "old namex");
        // The replace shape: the first character lands whole.
        let mut f = Field::with_selected("old name");
        f.edit(Edit::Char('n'));
        assert_eq!(f.take(), "n");
        // Backspace empties it, the way the rename question always worked.
        let mut f = Field::with_selected("old name");
        f.edit(Edit::Backspace);
        assert_eq!(f.take(), "");
        // A paste replaces too.
        let mut f = Field::with_selected("old name");
        f.edit(Edit::Paste("new".into()));
        assert_eq!(f.take(), "new");
    }

    #[test]
    fn the_window_shows_the_cursor_and_anchors_on_the_tail() {
        let f = Field::with("abcdefgh");
        let (text, at) = f.window(4);
        assert_eq!((text.as_str(), at), ("efgh", 4), "the tail is what fits");
        let mut f = Field::with("abcdefgh");
        f.edit(Edit::Left);
        f.edit(Edit::Left);
        let (text, at) = f.window(4);
        assert_eq!(text, "efgh");
        assert_eq!(at, 2, "the cursor sits where it is, inside the window");
        // A short line is drawn whole.
        let (text, at) = Field::with("ab").window(4);
        assert_eq!((text.as_str(), at), ("ab", 2));
    }
}
