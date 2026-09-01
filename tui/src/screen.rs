//! The cell grid, and the escape codes that put it on a terminal.
//!
//! This is what `AnyElement` is to the GPUI shell: the thing a presentation
//! draws into. It is ours rather than a framework's for one reason worth stating
//! plainly — **it makes the views testable without a terminal.** A [`Screen`] is
//! a `Vec<Cell>` and a [`Screen::dump`], so "the second row is a removal, red on
//! dark red, with the changed word lit" is an assertion in a unit test. The GPUI
//! shell has no equivalent and `docs/architecture.md` lists that as its one
//! untested stage; here it is the default.
//!
//! # Only what changed is written
//!
//! Two buffers. Presentations draw into the back one, [`Screen::flush`] compares
//! it against what is already on screen and emits escape codes for the runs that
//! differ. A full-screen repaint of a 200×60 terminal is about 40 kB of SGR
//! sequences; a keystroke that moves the selection one row is about 200 bytes.
//! Over ssh that is the difference between usable and not, and it is the same
//! trade as `uniform_list` building only the visible rows.
//!
//! The whole thing is wrapped in the synchronized-output private mode
//! (`?2026`), so a terminal that understands it presents the frame at once
//! rather than mid-repaint. Terminals that do not, ignore it.
//!
//! # Columns, not characters
//!
//! A terminal cell holds one column, and a CJK ideograph occupies two of them. A
//! character count is therefore the wrong measure here, and getting it wrong
//! does not misalign one row — it shears every row below it, because the cursor
//! ends up somewhere the grid does not agree with. So [`cols`] is
//! `unicode-width`, a wide character claims a lead cell and a continuation cell,
//! and a zero-width mark is folded onto the cell before it.
//!
//! One thing is knowingly inconsistent, and it is `core`'s: `wrap` budgets are
//! counted in *characters*, because `core` has no dependencies and cannot ask
//! how wide a glyph is. A line of ideographs therefore wraps a little wide and
//! is clipped by the pen rather than overflowing the grid — a visible truncation
//! instead of a broken screen. `docs/` has the note on what fixing it properly
//! would take.

use gitten_core::theme::{Rgb, Style};
use std::io::{self, Write};
use unicode_width::UnicodeWidthChar;

/// How many columns a character occupies: 0 for a combining mark, 2 for a wide
/// or fullwidth one, 1 for everything else.
///
/// Control characters answer 1 rather than `None`. A diff can contain any byte
/// git handed us, and a row that silently loses a character is worse than one
/// that shows a placeholder — see [`Pen::put`], which substitutes.
pub fn cols(c: char) -> usize {
    c.width().unwrap_or(1)
}

/// How many columns a string occupies.
pub fn width(s: &str) -> usize {
    s.chars().map(cols).sum()
}

/// Everything about how one cell is drawn except which character is in it.
///
/// Flattened rather than holding a [`Style`]: this is compared per cell on every
/// flush, and a `Copy` struct of two `u32`s and three flags is one comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ink {
    pub fg: Rgb,
    pub bg: Rgb,
    pub bold: bool,
    pub italic: bool,
    /// Not used by the diff view, which has real backgrounds for the words that
    /// changed. Here because a terminal has few other ways to mark a span, and
    /// an extension drawing into a row is entitled to one.
    pub underline: bool,
    /// The colour of that underline, when it should not be the terminal's own.
    ///
    /// `None` keeps the plain SGR 4 everything else uses; `Some` adds SGR 58
    /// beside it, and a terminal that does not know 58 colours the underline
    /// with its default and simply ignores the sequence — the boundary stays
    /// visible either way. This is what a hairline *between* two rows of a
    /// table becomes here, because a terminal cannot paint between two rows
    /// and a row of `─` would be a row of the list no line of the file produced.
    pub underline_color: Option<Rgb>,
}

impl Ink {
    pub const fn new(fg: Rgb, bg: Rgb) -> Self {
        Self {
            fg,
            bg,
            bold: false,
            italic: false,
            underline: false,
            underline_color: None,
        }
    }

    /// A [`Style`] from the theme, on a background the theme resolved it
    /// against. The two travel together everywhere in the diff view — see
    /// [`Theme::syntax_on`](gitten_core::theme::Theme::syntax_on).
    pub const fn styled(style: Style, bg: Rgb) -> Self {
        Self {
            fg: style.fg,
            bg,
            bold: style.bold,
            italic: style.italic,
            underline: false,
            underline_color: None,
        }
    }

    pub const fn bold(mut self) -> Self {
        self.bold = true;
        self
    }

    pub const fn underline(mut self) -> Self {
        self.underline = true;
        self
    }

    /// An underline in `colour` rather than the terminal's default. The
    /// Ink-level way to ask for what [`Pen::underline`] writes onto cells that
    /// are already there.
    pub const fn underlined(mut self, colour: Rgb) -> Self {
        self.underline = true;
        self.underline_color = Some(colour);
        self
    }

    pub const fn on(mut self, bg: Rgb) -> Self {
        self.bg = bg;
        self
    }
}

/// The character in a cell that a wide character to its left already covers.
///
/// A real `char`, so a cell stays `Copy` and 12 bytes. `\0` cannot appear in a
/// line of a diff: [`Pen::put`] substitutes for control characters.
const CONTINUED: char = '\0';

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Cell {
    ch: char,
    ink: Ink,
}

impl Cell {
    const fn blank(ink: Ink) -> Self {
        Self { ch: ' ', ink }
    }
}

/// A grid of cells, and what is currently on the terminal.
pub struct Screen {
    w: usize,
    h: usize,
    /// What the next flush will put on screen.
    back: Vec<Cell>,
    /// What is on screen now. Empty before the first flush, which is what makes
    /// that one a full repaint.
    front: Vec<Cell>,
}

impl Screen {
    pub fn new(w: usize, h: usize) -> Self {
        let blank = Cell::blank(Ink::new(0, 0));
        Self {
            w,
            h,
            back: vec![blank; w * h],
            front: Vec::new(),
        }
    }

    /// Resizes, discarding both buffers.
    ///
    /// The front buffer goes too, deliberately: a terminal that was just resized
    /// has reflowed or truncated its own scrollback and nothing can be assumed
    /// about what is on it, so the next flush must be a full repaint. Returns
    /// whether the size actually changed, so a `SIGWINCH` storm costs two
    /// comparisons.
    pub fn resize(&mut self, w: usize, h: usize) -> bool {
        if (w, h) == (self.w, self.h) {
            return false;
        }
        self.w = w;
        self.h = h;
        self.back = vec![Cell::blank(Ink::new(0, 0)); w * h];
        self.front.clear();
        true
    }

    pub fn size(&self) -> (usize, usize) {
        (self.w, self.h)
    }

    pub fn width(&self) -> usize {
        self.w
    }

    pub fn height(&self) -> usize {
        self.h
    }

    /// Fills every cell with a blank in `ink`. What a frame starts with.
    pub fn clear(&mut self, ink: Ink) {
        self.back.fill(Cell::blank(ink));
    }

    /// A pen over one whole row. Out of range returns a pen with nowhere to
    /// write, so a view that draws one row too many clips instead of panicking —
    /// an off-by-one in a viewport is a bug to see, not to crash on.
    pub fn row(&mut self, y: usize) -> Pen<'_> {
        let cells = match y < self.h {
            true => &mut self.back[y * self.w..(y + 1) * self.w],
            false => &mut [][..],
        };
        Pen {
            cells,
            x: 0,
            skip: 0,
        }
    }

    /// A pen over part of a row, for a view that owns a column range — a
    /// sidebar, or one half of a split.
    pub fn span(&mut self, y: usize, x: usize, cols: usize) -> Pen<'_> {
        let start = x.min(self.w);
        let end = (start + cols).min(self.w);
        let cells = match y < self.h {
            true => &mut self.back[y * self.w + start..y * self.w + end],
            false => &mut [][..],
        };
        Pen {
            cells,
            x: 0,
            skip: 0,
        }
    }

    /// Writes the difference between the back buffer and the screen.
    ///
    /// Returns how many cells it wrote, which is what a stats overlay reports:
    /// on a scroll that is a screenful, and on a keypress that moves a cursor it
    /// should be a handful. A number that is always the whole grid means
    /// something is rebuilding ink it did not need to.
    pub fn flush(&mut self, out: &mut impl Write) -> io::Result<usize> {
        // Begin synchronized update: the frame appears at once or not at all.
        out.write_all(b"\x1b[?2026h")?;
        let mut written = 0;
        let mut ink: Option<Ink> = None;

        for y in 0..self.h {
            let row = y * self.w;
            let mut x = 0;
            while x < self.w {
                if self.unchanged(row + x) {
                    x += 1;
                    continue;
                }
                // A run that begins on a continuation cell has to be redrawn
                // from the wide character that owns it, or the cursor lands
                // inside a glyph and the terminal draws half of one.
                let mut start = x;
                while start > 0 && self.back[row + start].ch == CONTINUED {
                    start -= 1;
                }
                let mut end = x + 1;
                while end < self.w && !self.unchanged(row + end) {
                    end += 1;
                }
                // ...and it has to end after one, for the same reason.
                while end < self.w && self.back[row + end].ch == CONTINUED {
                    end += 1;
                }

                write!(out, "\x1b[{};{}H", y + 1, start + 1)?;
                for cell in &self.back[row + start..row + end] {
                    if cell.ch == CONTINUED {
                        continue;
                    }
                    if ink != Some(cell.ink) {
                        sgr(out, cell.ink)?;
                        ink = Some(cell.ink);
                    }
                    let mut buf = [0u8; 4];
                    out.write_all(cell.ch.encode_utf8(&mut buf).as_bytes())?;
                    written += 1;
                }
                x = end;
            }
        }

        out.write_all(b"\x1b[0m\x1b[?2026l")?;
        out.flush()?;
        self.front.clear();
        self.front.extend_from_slice(&self.back);
        Ok(written)
    }

    fn unchanged(&self, i: usize) -> bool {
        self.front.get(i) == Some(&self.back[i])
    }

    /// Writes the whole grid as lines, with no cursor positioning.
    ///
    /// The other half of [`Screen::flush`], and the reason it exists at all: a
    /// grid that can be *printed* is a grid that can be looked at without
    /// entering raw mode or the alternate screen. `examples/dump.rs` is a real
    /// frame of the real views piped into a pager, which is how a colour or a
    /// glyph is checked without a window appearing and interrupting whoever is
    /// at the keyboard.
    ///
    /// Every row is emitted whole, since there is nothing on screen to diff
    /// against, and each line is reset at its end so a pager cannot inherit the
    /// last cell's background.
    pub fn print(&self, out: &mut impl Write) -> io::Result<()> {
        for y in 0..self.h {
            let mut ink: Option<Ink> = None;
            for cell in &self.back[y * self.w..(y + 1) * self.w] {
                if cell.ch == CONTINUED {
                    continue;
                }
                if ink != Some(cell.ink) {
                    sgr(out, cell.ink)?;
                    ink = Some(cell.ink);
                }
                let mut buf = [0u8; 4];
                out.write_all(cell.ch.encode_utf8(&mut buf).as_bytes())?;
            }
            out.write_all(b"\x1b[0m\n")?;
        }
        out.flush()
    }

    /// The text of one row, as it would appear, with trailing blanks removed.
    /// For tests and for `--dump`.
    pub fn row_text(&self, y: usize) -> String {
        if y >= self.h {
            return String::new();
        }
        let mut out = String::with_capacity(self.w);
        for cell in &self.back[y * self.w..(y + 1) * self.w] {
            if cell.ch != CONTINUED {
                out.push(cell.ch);
            }
        }
        out.trim_end().to_string()
    }

    /// The whole grid as text, one row per line. What a test asserts against and
    /// what a `--dump` flag prints instead of opening a terminal.
    pub fn dump(&self) -> String {
        (0..self.h)
            .map(|y| self.row_text(y))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Draws one character over whatever is already in a cell, keeping that
    /// cell's background.
    ///
    /// What a scrollbar is: furniture that sits *on* the rows rather than beside
    /// them, so a removal's red runs to the edge underneath it exactly as it does
    /// everywhere else. A pen cannot do this — it writes a whole [`Ink`], and a
    /// scrollbar drawn with one would punch a column of chrome-coloured holes
    /// down a wall of colour.
    ///
    /// Out of range writes nothing, and a continuation cell takes the character
    /// anyway: half a wide glyph is already lost the moment something is drawn
    /// over the other half, and leaving `\0` on screen would be worse.
    pub fn over(&mut self, x: usize, y: usize, ch: char, fg: Rgb) {
        if x >= self.w || y >= self.h {
            return;
        }
        let cell = &mut self.back[y * self.w + x];
        cell.ch = ch;
        cell.ink.fg = fg;
        cell.ink.bold = false;
        cell.ink.italic = false;
        cell.ink.underline = false;
        cell.ink.underline_color = None;
    }

    /// The ink of one cell, for a test that cares about colour rather than text.
    pub fn ink(&self, x: usize, y: usize) -> Option<Ink> {
        (x < self.w && y < self.h).then(|| self.back[y * self.w + x].ink)
    }

    /// The character in one cell, or `None` if a wide character to its left
    /// covers it.
    pub fn char_at(&self, x: usize, y: usize) -> Option<char> {
        if x >= self.w || y >= self.h {
            return None;
        }
        Some(self.back[y * self.w + x].ch).filter(|c| *c != CONTINUED)
    }
}

/// 24-bit colour and the three attributes, reset first.
///
/// The leading `0` costs two bytes a run and buys statelessness: without it,
/// turning bold *off* needs its own code and a missed one bleeds down the rest
/// of the screen. Runs are per changed span, not per cell, so the two bytes are
/// noise.
fn sgr(out: &mut impl Write, ink: Ink) -> io::Result<()> {
    let (fr, fg, fb) = (ink.fg >> 16 & 0xff, ink.fg >> 8 & 0xff, ink.fg & 0xff);
    let (br, bg, bb) = (ink.bg >> 16 & 0xff, ink.bg >> 8 & 0xff, ink.bg & 0xff);
    write!(out, "\x1b[0;38;2;{fr};{fg};{fb};48;2;{br};{bg};{bb}")?;
    if ink.bold {
        out.write_all(b";1")?;
    }
    if ink.italic {
        out.write_all(b";3")?;
    }
    if ink.underline {
        out.write_all(b";4")?;
        // SGR 58, the underline colour: a terminal without it ignores the
        // sequence and draws the plain SGR 4 underline in its own colour, so
        // the boundary stays visible either way.
        if let Some(c) = ink.underline_color {
            let (r, g, b) = (c >> 16 & 0xff, c >> 8 & 0xff, c & 0xff);
            write!(out, ";58;2;{r};{g};{b}")?;
        }
    }
    out.write_all(b"m")
}

/// Writes across one row, left to right, and cannot leave it.
///
/// Clipping rather than wrapping, always: a row is a row, and a presentation
/// that wanted two asks for two. That is the same constraint the GPUI shell has
/// for the same reason — a wrapped line is *more rows*, decided before anything
/// is drawn, by [`gitten_core::wrap`].
pub struct Pen<'a> {
    cells: &'a mut [Cell],
    x: usize,
    /// Columns still to be swallowed by a horizontal scroll. See [`Pen::scroll`].
    skip: usize,
}

impl Pen<'_> {
    /// Which column the next write lands in.
    pub fn col(&self) -> usize {
        self.x
    }

    /// Columns left before the end of the row.
    pub fn room(&self) -> usize {
        self.cells.len().saturating_sub(self.x)
    }

    pub fn full(&self) -> bool {
        self.room() == 0
    }

    /// Moves to a column, without drawing anything on the way.
    pub fn seek(&mut self, col: usize) {
        self.x = col.min(self.cells.len());
    }

    pub fn skip(&mut self, cols: usize) {
        self.seek(self.x + cols);
    }

    /// Swallows the next `cols` columns of everything written after this, so
    /// content scrolls sideways under whatever was drawn before it.
    ///
    /// This is how a diff scrolls horizontally with wrapping off while its line
    /// numbers and its `+`/`-` stay put — the presentation draws its gutter,
    /// calls this, then draws the text, and the pen does the rest. Doing it by
    /// slicing the text instead is the obvious alternative and it is wrong: the
    /// syntax tokens and the intraline spans address the *line*, so a slice
    /// taken before [`gitten_core::runs::runs`] runs pairs styling with the wrong
    /// bytes.
    pub fn scroll(&mut self, cols: usize) {
        self.skip = cols;
    }

    /// A pen over the next `cols` columns, advancing this one past them.
    ///
    /// What splits a row between two presentations of the same diff: the
    /// side-by-side layout takes a column, draws a rule, takes another. A
    /// sub-pen cannot reach outside its slice, so neither half can overrun the
    /// divider however long its line is — which is the property that keeps the
    /// rule a straight vertical line from the first row to the last.
    pub fn take(&mut self, cols: usize) -> Pen<'_> {
        let start = self.x;
        let end = (start + cols).min(self.cells.len());
        self.x = end;
        Pen {
            cells: &mut self.cells[start..end],
            x: 0,
            skip: 0,
        }
    }

    /// Fills `cols` columns with `ch`.
    pub fn fill(&mut self, cols: usize, ch: char, ink: Ink) {
        for _ in 0..cols {
            if self.full() {
                return;
            }
            self.cells[self.x] = Cell { ch, ink };
            self.x += 1;
        }
    }

    /// Fills the rest of the row with blanks in `ink`. What paints a diff row's
    /// background all the way to the right edge, which is what makes a run of
    /// removals read as a block rather than as ragged text.
    pub fn wash(&mut self, ink: Ink) {
        let room = self.room();
        self.fill(room, ' ', ink);
    }

    /// Underlines the cells from column `from` for `cols` columns, in
    /// `colour`, leaving every character and every background exactly as they
    /// were written.
    ///
    /// The terminal's stand-in for the window's 1px table hairline: a terminal
    /// cannot paint between two rows of a grid, and drawing the boundary as a
    /// row of `─` would invent a visual row no line of the file produced — a
    /// row the gutter's numbers have to skip. So the boundary is drawn on the
    /// cells the grid itself occupies, under its last visual segment: the
    /// glyphs and the colours stay, and the underline says a boundary is here.
    /// A wide character's continuation cell is underlined with its lead, as
    /// the glyph's own underline would be; cells past the row's end are
    /// clipped, and the cells already written are not touched beyond the two
    /// underline fields.
    pub fn underline(&mut self, from: usize, cols: usize, colour: Rgb) {
        let len = self.cells.len();
        let from = from.min(len);
        let to = from.saturating_add(cols).min(len);
        for cell in &mut self.cells[from..to] {
            cell.ink.underline = true;
            cell.ink.underline_color = Some(colour);
        }
    }

    /// Writes as much of `s` as fits, and returns the columns it took.
    ///
    /// Three substitutions, each because the alternative is worse than a
    /// placeholder:
    ///
    /// - A **control character** — a stray `\r` or a vertical tab, both of which
    ///   turn up in real diffs — becomes `·`. Writing it would move the cursor
    ///   and desynchronise the grid from the screen.
    /// - A **zero-width mark** is merged onto the cell before it, so a combining
    ///   accent draws on its base rather than eating a column.
    /// - A **wide character with one column left** becomes a space. Half a glyph
    ///   is not a thing a terminal can draw.
    pub fn put(&mut self, s: &str, ink: Ink) -> usize {
        let from = self.x;
        for c in s.chars() {
            let c = if c.is_control() { '·' } else { c };
            let w = cols(c);
            if w == 0 {
                // Nothing to attach it to at the start of a row: dropped, which
                // is what a terminal would do with it anyway.
                continue;
            }
            if self.skip >= w {
                self.skip -= w;
                continue;
            }
            // A wide character straddling the scroll boundary: its left half is
            // off screen, so a space stands in for the half that is not.
            if self.skip > 0 {
                self.skip = 0;
                if !self.full() {
                    self.cells[self.x] = Cell { ch: ' ', ink };
                    self.x += 1;
                }
                continue;
            }
            if self.room() < w {
                if self.room() == 1 {
                    self.cells[self.x] = Cell { ch: ' ', ink };
                    self.x += 1;
                }
                break;
            }
            self.cells[self.x] = Cell { ch: c, ink };
            for i in 1..w {
                self.cells[self.x + i] = Cell { ch: CONTINUED, ink };
            }
            self.x += w;
        }
        self.x - from
    }

    /// Writes `s` right-aligned in `cols` columns, padding on the left.
    ///
    /// What a line-number gutter is: numbers that do not line up on their last
    /// digit are numbers the eye has to read rather than scan.
    pub fn put_right(&mut self, s: &str, cols_wide: usize, ink: Ink) {
        let w = width(s);
        self.fill(cols_wide.saturating_sub(w), ' ', ink);
        // Longer than its column is clipped from the left, so the digits that
        // survive are the ones that distinguish it.
        match w > cols_wide {
            true => {
                let skip = w - cols_wide;
                let start = s
                    .char_indices()
                    .scan(0, |acc, (i, c)| {
                        let before = *acc;
                        *acc += cols(c);
                        Some((i, before))
                    })
                    .find(|(_, before)| *before >= skip)
                    .map_or(s.len(), |(i, _)| i);
                self.put(&s[start..], ink);
            }
            false => {
                self.put(s, ink);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FG: Rgb = 0xe8e3dc;
    const BG: Rgb = 0x0e0d0c;

    fn screen(w: usize, h: usize) -> Screen {
        let mut s = Screen::new(w, h);
        s.clear(Ink::new(FG, BG));
        s
    }

    /// What a flush actually put on the wire.
    fn flushed(s: &mut Screen) -> String {
        let mut out = Vec::new();
        s.flush(&mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn a_row_is_written_and_read_back() {
        let mut s = screen(20, 3);
        s.row(1).put("hello", Ink::new(FG, BG));
        assert_eq!(s.row_text(1), "hello");
        assert_eq!(s.dump(), "\nhello\n");
    }

    #[test]
    fn nothing_written_can_leave_its_row() {
        let mut s = screen(8, 2);
        let took = s
            .row(0)
            .put("far too long for eight columns", Ink::new(FG, BG));
        assert_eq!(took, 8);
        assert_eq!(s.row_text(0), "far too");
        assert_eq!(s.row_text(1), "", "it did not spill downwards");
    }

    #[test]
    fn a_row_past_the_bottom_clips_instead_of_panicking() {
        let mut s = screen(8, 2);
        s.row(99).put("nowhere", Ink::new(FG, BG));
        assert_eq!(s.dump(), "\n");
    }

    #[test]
    fn a_wide_character_takes_two_cells_and_the_grid_stays_square() {
        // The failure this prevents: counting characters, so the row is one cell
        // short and every row below it is offset by one.
        let mut s = screen(10, 1);
        let took = s.row(0).put("日本語ab", Ink::new(FG, BG));
        assert_eq!(took, 8);
        assert_eq!(s.char_at(0, 0), Some('日'));
        assert_eq!(
            s.char_at(1, 0),
            None,
            "the continuation cell holds no character"
        );
        assert_eq!(s.char_at(6, 0), Some('a'));
        assert_eq!(s.row_text(0), "日本語ab");
    }

    #[test]
    fn a_wide_character_with_one_column_left_becomes_a_space() {
        let mut s = screen(2, 1);
        s.row(0).put("a日", Ink::new(FG, BG));
        assert_eq!(s.char_at(0, 0), Some('a'));
        assert_eq!(s.char_at(1, 0), Some(' '), "half a glyph was drawn");
    }

    #[test]
    fn a_control_character_is_a_placeholder_and_not_a_moved_cursor() {
        let mut s = screen(8, 1);
        s.row(0).put("a\rb\tc", Ink::new(FG, BG));
        assert_eq!(s.row_text(0), "a·b·c");
    }

    #[test]
    fn a_combining_mark_draws_on_its_base_rather_than_taking_a_column() {
        let mut s = screen(6, 1);
        let took = s.row(0).put("e\u{301}f", Ink::new(FG, BG));
        assert_eq!(took, 2, "the accent claimed a column");
        assert_eq!(s.char_at(1, 0), Some('f'));
    }

    #[test]
    fn a_scroll_swallows_columns_of_text_and_leaves_the_gutter_alone() {
        let mut s = screen(12, 1);
        {
            let mut pen = s.row(0);
            pen.put(">>", Ink::new(FG, BG));
            pen.scroll(3);
            pen.put("abcdefghijkl", Ink::new(FG, BG));
        }
        assert_eq!(s.row_text(0), ">>defghijkl");
    }

    #[test]
    fn a_wide_character_straddling_the_scroll_boundary_becomes_a_space() {
        let mut s = screen(8, 1);
        {
            let mut pen = s.row(0);
            pen.scroll(1);
            pen.put("日本", Ink::new(FG, BG));
        }
        // One column of the first ideograph is off screen, so its remaining
        // half is a space rather than half a glyph.
        assert_eq!(s.row_text(0), " 本");
    }

    #[test]
    fn a_gutter_lines_up_on_its_last_digit() {
        let mut s = screen(10, 2);
        s.row(0).put_right("7", 4, Ink::new(FG, BG));
        s.row(1).put_right("1234", 4, Ink::new(FG, BG));
        assert_eq!(s.row_text(0), "   7");
        assert_eq!(s.row_text(1), "1234");
    }

    #[test]
    fn a_number_wider_than_its_gutter_keeps_the_digits_that_distinguish_it() {
        let mut s = screen(10, 1);
        s.row(0).put_right("123456", 3, Ink::new(FG, BG));
        assert_eq!(s.row_text(0), "456");
    }

    #[test]
    fn wash_paints_the_rest_of_the_row() {
        let mut s = screen(6, 1);
        {
            let mut pen = s.row(0);
            pen.put("ab", Ink::new(FG, 0x111111));
            pen.wash(Ink::new(FG, 0x222222));
        }
        assert_eq!(s.ink(1, 0).unwrap().bg, 0x111111);
        assert_eq!(s.ink(5, 0).unwrap().bg, 0x222222);
    }

    #[test]
    fn the_first_flush_writes_everything_and_the_second_writes_nothing() {
        let mut s = screen(10, 2);
        s.row(0).put("hello", Ink::new(FG, BG));
        let first = flushed(&mut s);
        assert!(first.contains("hello"));
        assert!(
            first.starts_with("\x1b[?2026h"),
            "no synchronized update: {first:?}"
        );
        assert!(first.ends_with("\x1b[0m\x1b[?2026l"));

        let mut out = Vec::new();
        let n = s.flush(&mut out).unwrap();
        assert_eq!(n, 0, "an unchanged frame wrote cells");
        assert_eq!(out.len(), "\x1b[?2026h\x1b[0m\x1b[?2026l".len());
    }

    #[test]
    fn only_the_changed_span_of_a_row_is_written() {
        // The whole reason there are two buffers. A selection moving one row
        // must not cost a screenful of escape codes.
        let mut s = screen(40, 1);
        s.row(0).put("the quick brown fox", Ink::new(FG, BG));
        flushed(&mut s);
        s.row(0).put("the QUICK brown fox", Ink::new(FG, BG));
        let mut out = Vec::new();
        let n = s.flush(&mut out).unwrap();
        assert_eq!(n, 5, "wrote more than the word that changed");
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("QUICK"));
        assert!(!text.contains("brown"));
        // Positioned at the changed column, one-based.
        assert!(text.contains("\x1b[1;5H"), "{text:?}");
    }

    #[test]
    fn a_changed_continuation_cell_is_redrawn_from_the_character_that_owns_it() {
        // Otherwise the cursor lands inside a glyph and the terminal draws half
        // of one — the one flush bug a character-counting grid cannot even have.
        let mut s = screen(6, 1);
        s.row(0).put("日本", Ink::new(FG, BG));
        flushed(&mut s);
        s.row(0).put("日語", Ink::new(FG, BG));
        let out = flushed(&mut s);
        assert!(
            out.contains("\x1b[1;3H"),
            "cursor was not put on the lead cell: {out:?}"
        );
        assert!(out.contains('語'), "{out:?}");
    }

    #[test]
    fn a_resize_forces_a_full_repaint() {
        let mut s = screen(10, 1);
        s.row(0).put("hi", Ink::new(FG, BG));
        flushed(&mut s);
        assert!(s.resize(12, 2));
        assert!(!s.resize(12, 2), "the same size resized again");
        s.clear(Ink::new(FG, BG));
        s.row(0).put("hi", Ink::new(FG, BG));
        let mut out = Vec::new();
        // 24 cells, because nothing may be assumed about a terminal that has
        // just reflowed its own screen.
        assert_eq!(s.flush(&mut out).unwrap(), 24);
    }

    #[test]
    fn ink_is_emitted_once_per_run_and_not_once_per_cell() {
        let mut s = screen(20, 1);
        {
            let mut pen = s.row(0);
            pen.put("aaaa", Ink::new(0x111111, BG));
            pen.put("bbbb", Ink::new(0x222222, BG));
        }
        let out = flushed(&mut s);
        assert_eq!(out.matches("38;2;17;17;17").count(), 1);
        assert_eq!(out.matches("38;2;34;34;34").count(), 1);
    }

    #[test]
    fn every_attribute_reaches_the_wire() {
        let mut s = screen(4, 1);
        let ink = Ink {
            fg: 0xff0000,
            bg: 0x00ff00,
            bold: true,
            italic: true,
            underline: true,
            underline_color: None,
        };
        s.row(0).put("x", ink);
        let out = flushed(&mut s);
        assert!(
            out.contains("\x1b[0;38;2;255;0;0;48;2;0;255;0;1;3;4m"),
            "{out:?}"
        );
    }

    #[test]
    fn a_coloured_bottom_rule_preserves_cells_and_sgr_state() {
        // The table hairline's whole trick: the boundary is drawn *on* the cells
        // the grid already occupies — no character changes, no background
        // changes, and the next run of cells after it is completely untouched.
        let mut s = screen(20, 1);
        let rule = 0xabcdef;
        {
            let mut pen = s.row(0);
            pen.put("grid cells", Ink::new(FG, BG));
            pen.underline(0, 10, rule);
            pen.put("plain text", Ink::new(FG, 0x112233));
        }
        // The characters and the backgrounds are what they were.
        assert_eq!(s.row_text(0), "grid cellsplain text");
        assert_eq!(s.ink(0, 0).unwrap().bg, BG);
        assert_eq!(s.ink(9, 0).unwrap().bg, BG);
        // The rule is on the cells it covers and nowhere else.
        for x in 0..10 {
            let ink = s.ink(x, 0).unwrap();
            assert!(ink.underline, "cell {x} missed the rule");
            assert_eq!(ink.underline_color, Some(rule));
        }
        for x in 10..20 {
            let ink = s.ink(x, 0).unwrap();
            assert!(!ink.underline, "the rule bled into cell {x}");
            assert_eq!(ink.underline_color, None);
        }

        // On the wire: reset first, then the underline and its colour on the
        // ruled run; the run after it starts from the reset again, so nothing
        // bleeds.
        let out = flushed(&mut s);
        let ruled = "\x1b[0;38;2;232;227;220;48;2;14;13;12;4;58;2;171;205;239m";
        assert!(out.contains(ruled), "{out:?}");
        let after = out.split(ruled).nth(1).expect("the run after the rule");
        assert!(
            after.contains("\x1b[0;38;2;"),
            "the next run did not reset: {after:?}"
        );
        assert!(!after.contains("58;2;"), "the colour bled: {after:?}");
        assert!(out.ends_with("\x1b[0m\x1b[?2026l"), "{out:?}");
    }

    #[test]
    fn a_plain_underline_still_carries_no_colour() {
        // The pre-existing SGR 4 semantics are untouched: no 58 beside it when
        // no colour was asked for, and `underlined` is the only way in.
        let mut s = screen(4, 2);
        s.row(0).put("x", Ink::new(FG, BG).underline());
        s.row(1).put("y", Ink::new(FG, BG).underlined(0xabcdef));
        let out = flushed(&mut s);
        assert!(out.contains(";4m"), "{out:?}");
        assert!(out.contains(";4;58;2;171;205;239m"), "{out:?}");
    }

    #[test]
    fn a_theme_style_becomes_ink_without_losing_its_emphasis() {
        let style = Style::fg(0xabcdef).bold().italic();
        let ink = Ink::styled(style, 0x123456);
        assert_eq!((ink.fg, ink.bg), (0xabcdef, 0x123456));
        assert!(ink.bold && ink.italic && !ink.underline);
    }

    #[test]
    fn a_sub_pen_stops_at_its_own_edge_and_advances_the_one_it_came_from() {
        let mut s = screen(14, 1);
        {
            let mut pen = s.row(0);
            pen.take(6).put("abcdefghij", Ink::new(FG, BG));
            pen.put("|", Ink::new(FG, BG));
            pen.take(6).put("xy", Ink::new(FG, BG));
        }
        assert_eq!(s.row_text(0), "abcdef|xy");
    }

    #[test]
    fn printing_emits_every_row_with_no_cursor_positioning() {
        let mut s = screen(10, 2);
        s.row(0).put("one", Ink::new(FG, BG));
        s.row(1).put("two", Ink::new(FG, BG));
        let mut out = Vec::new();
        s.print(&mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("one"));
        assert!(text.contains("two"));
        assert!(
            !text.contains("\x1b[1;1H"),
            "a printable dump moved the cursor"
        );
        assert_eq!(
            text.matches("\x1b[0m\n").count(),
            2,
            "a row did not reset: {text:?}"
        );
    }

    #[test]
    fn drawing_over_a_cell_keeps_the_background_it_landed_on() {
        // The scrollbar's whole trick: a row's colour still runs to the edge.
        let mut s = screen(6, 2);
        s.row(0).wash(Ink::new(FG, 0x330000));
        s.over(5, 0, '\u{2588}', 0xabcdef);
        assert_eq!(s.char_at(5, 0), Some('\u{2588}'));
        assert_eq!(s.ink(5, 0).unwrap().bg, 0x330000, "it repainted the row");
        assert_eq!(s.ink(5, 0).unwrap().fg, 0xabcdef);
        // Off the grid is a no-op rather than a panic: a bar is drawn from a
        // height the caller worked out, and an off-by-one is a bug to see.
        s.over(99, 0, 'x', FG);
        s.over(0, 99, 'x', FG);
    }

    #[test]
    fn a_span_pen_cannot_reach_outside_its_columns() {
        let mut s = screen(12, 1);
        s.span(0, 4, 4).put("abcdefgh", Ink::new(FG, BG));
        assert_eq!(s.row_text(0), "    abcd");
        // ...and one that starts past the edge writes nothing.
        s.span(0, 99, 4).put("zzz", Ink::new(FG, BG));
        assert_eq!(s.row_text(0), "    abcd");
    }
}
