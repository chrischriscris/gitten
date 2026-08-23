//! What the mouse has selected, and what copying it yields.
//!
//! A browser and a terminal both hand a selection over for free, and the window
//! is the one door where nobody does: GPUI's `StyledText` paints glyphs and has
//! no notion of a selected byte. So this is the model the window drives, and it
//! is here rather than beside the renderer for the usual reason — three clients
//! would otherwise each decide for themselves where a selection starts, whether
//! a wrapped line copies once or twice, and what a side-by-side diff does when
//! you drag down one column. Those are answers about a *diff*, not about a UI.
//!
//! # What a frontend still owns
//!
//! Two things, both of them pixels. **Where a click landed** — which row and
//! which byte of it — because that needs the font, the gutters and whatever else
//! the presentation drew in front of the text. And **how a selected run is
//! painted**, which is a background colour on a run list.
//!
//! Everything else is here: which rows lie between two carets, which bytes of
//! each are covered, what survives a reflow, where a word ends and how the
//! whole thing turns into a string.
//!
//! # Coordinates
//!
//! A caret names a **logical** row — the `(owner, index)` pair
//! [`RowRef::logical`] returns — and a byte offset into that row's own text. Not
//! a visual row, and not a character index. Both of those were tried in the
//! reading position first and are wrong for the same two reasons: a reflow moves
//! every visual row, and a byte offset is what the edit script, the tokens and
//! the spans already address, so a selection in any other unit would need
//! converting before it could be painted.
//!
//! What a caret *also* carries is the visual rows its logical row currently
//! occupies, and that is a cache — see [`Caret::at`]. The render path asks "is
//! this row selected" once per visible row per frame and may not answer it by
//! searching the order table.
//!
//! # Parts
//!
//! A row may draw more than one text: a side-by-side row draws two. A selection
//! is fixed to the one its anchor landed in, so dragging down the left column of
//! a two-column diff selects the left column and nothing else. That is not a
//! simplification — a selection that wandered across the divider would paste the
//! old and the new file interleaved, which is not a thing anybody wants, and it
//! is why `part` sits on the [`Selection`] and not on each [`Caret`].

use crate::rows::RowRef;
use std::ops::Range;

/// What the mouse does besides select. `[mouse]` in `plait.toml`.
///
/// One field, and it is here rather than beside the renderer because it is a
/// question about a *selection* — what finishing one means — and a client that
/// answered it with a literal would be a client nobody could change it in.
///
/// **A client it does not apply to ignores it**, the same way one that cannot
/// `quit` ignores `quit`: the window's own `cmd-c` works, because nothing there
/// took the platform's copy away, and a desktop app that rewrote the clipboard
/// on every drag would be breaking a convention rather than restoring one. This
/// is the terminal's knob for the terminal's problem, in the file every client
/// reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mousing {
    /// Whether finishing a selection with the mouse puts it on the clipboard.
    ///
    /// **On**, because a terminal that has taken the drag has taken
    /// select-then-`cmd-c` with it, and giving nothing back for it is the worst
    /// of both. X11 has had this since 1987 and every terminal on it still does
    /// it; what is different here is only that the clipboard is reached by
    /// asking the emulator (OSC 52) rather than by owning a selection on a
    /// display server.
    ///
    /// A **drag** copies and a **click** does not, which is the whole of the
    /// rule: a click is a cursor move and copying on one would clobber the
    /// clipboard every time you pointed at something. A double or a triple click
    /// is a selection and does copy.
    ///
    /// Off for anyone who wants the clipboard to change only when they say so —
    /// the copy key still works, and so does the emulator's own drag with
    /// `shift` held.
    pub copy_on_select: bool,
}

impl Default for Mousing {
    fn default() -> Self {
        Self {
            copy_on_select: true,
        }
    }
}

/// A logical row, the way [`RowRef::logical`] names one: which presentation owns
/// it, and where in that presentation's own storage it sits.
pub type RowId = (u16, u32);

/// Where a click landed inside a row: which of the row's texts, and which byte.
///
/// The answer a presentation gives when it is asked to hit-test itself, and the
/// one piece of a selection that needs a frontend at all — a pixel in a window,
/// a cell in a terminal. It is *here* rather than beside either of them because
/// both were about to define the same pair, and a caret built from two different
/// spellings of it is a caret that only one door can carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit {
    pub part: u16,
    /// A byte offset into the **logical** row's text, not the visual row's — so
    /// a caret on the third row of a wrapped line is the same kind of thing as
    /// one on an unwrapped line.
    pub off: usize,
}

/// One end of a selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caret {
    pub row: RowId,
    /// A byte offset into the row's own text. Always on a character boundary:
    /// a frontend derives it from a column count, so there is nowhere for a
    /// half-character offset to come from.
    pub off: usize,
    /// The visual rows that row occupies right now, half-open.
    ///
    /// A cache of the order table and nothing more — [`Selection::resolve`]
    /// rebuilds it after a reflow. It is here because the alternative is a
    /// linear search of a 714k-entry table per visible row per frame, for a
    /// question that is two integer comparisons once it is answered.
    pub at: Range<usize>,
}

impl Caret {
    /// A caret on a row that occupies one visual row, which is nearly every row.
    pub fn new(row: RowId, off: usize, at: usize) -> Self {
        Self {
            row,
            off,
            at: at..at + 1,
        }
    }

    /// Draw order, with the offset breaking a tie inside one logical row. A
    /// wrapped line's later rows carry later offsets, so the two agree.
    fn key(&self) -> (usize, usize) {
        (self.at.start, self.off)
    }
}

/// Which bytes of one row a selection covers.
///
/// The end is deliberately not exposed raw: "to the end of this row" has to
/// travel from here to a presentation that has not been asked how long its text
/// is, so it is carried as a sentinel and [`Selected::range`] is the only way
/// out. A caller that reached past the end of a line would be indexing a string
/// it does not own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selected {
    part: u16,
    from: usize,
    to: usize,
}

impl Selected {
    /// Which of the row's texts this covers. A presentation drawing one text
    /// ignores anything but 0.
    pub fn part(self) -> u16 {
        self.part
    }

    /// The bytes of a text `len` long that are selected, clamped into it.
    pub fn range(self, len: usize) -> Range<usize> {
        self.from.min(len)..self.to.min(len)
    }
}

/// An anchor, a head, and which of a row's texts they live in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    part: u16,
    anchor: Caret,
    head: Caret,
}

impl Selection {
    /// A fresh selection with both ends where the mouse went down. Empty until
    /// something [`extend`](Selection::extend)s it, which is what makes a plain
    /// click a *clear* rather than a one-byte selection.
    pub fn new(part: u16, at: Caret) -> Self {
        Self {
            part,
            anchor: at.clone(),
            head: at,
        }
    }

    /// Everything, in draw order, or `None` if there is nothing to select.
    ///
    /// The head's offset is past the end of any line on purpose:
    /// [`Selected::range`] clamps it, and asking the last row how long its text
    /// is would mean this function needing a presentation.
    pub fn all(order: &[RowRef]) -> Option<Self> {
        let (first, last) = (order.first()?, order.last()?);
        let mut sel = Self::new(
            0,
            Caret {
                row: first.logical(),
                off: 0,
                at: 0..1,
            },
        );
        sel.head = Caret {
            row: last.logical(),
            off: usize::MAX,
            at: order.len() - 1..order.len(),
        };
        sel.resolve(order);
        Some(sel)
    }

    pub fn part(&self) -> u16 {
        self.part
    }

    pub fn anchor(&self) -> &Caret {
        &self.anchor
    }

    /// Moves the free end. The anchor stays put, which is what makes a drag back
    /// past its own start select backwards rather than collapse.
    pub fn extend(&mut self, to: Caret) {
        self.head = to;
    }

    /// Nothing between the two ends — a click that did not drag.
    pub fn is_empty(&self) -> bool {
        self.anchor.key() == self.head.key()
    }

    /// The two ends in draw order, earlier first.
    pub fn ends(&self) -> (&Caret, &Caret) {
        match self.anchor.key() <= self.head.key() {
            true => (&self.anchor, &self.head),
            false => (&self.head, &self.anchor),
        }
    }

    /// The visual rows this selection touches, half-open. Empty when it is.
    pub fn rows(&self) -> Range<usize> {
        if self.is_empty() {
            return 0..0;
        }
        let (a, b) = self.ends();
        a.at.start..b.at.end
    }

    /// Which bytes of visual row `visual`, whose logical row is `row`, are
    /// selected — the one question the render path asks, once per visible row.
    ///
    /// Two comparisons and two more, and no allocation: a selection over a 714k
    /// row diff costs the same per frame as one over three.
    ///
    /// The logical row is compared rather than the visual index because a
    /// wrapped line is several visual rows sharing one identity, and it is the
    /// *line's* offsets the carets hold. A row whose selected range comes out
    /// empty — the selection ends exactly where it begins — answers `None`, so a
    /// presentation never has to think about a zero-length highlight.
    pub fn at(&self, visual: usize, row: RowId) -> Option<Selected> {
        if self.is_empty() {
            return None;
        }
        let (a, b) = self.ends();
        if visual < a.at.start || visual >= b.at.end {
            return None;
        }
        let from = if row == a.row { a.off } else { 0 };
        let to = if row == b.row { b.off } else { usize::MAX };
        (from < to).then_some(Selected {
            part: self.part,
            from,
            to,
        })
    }

    /// Rebuilds both carets' visual ranges from the order table.
    ///
    /// Called after a reflow, and only then: a wrap change moves every visual row
    /// and the cached ranges would otherwise describe the window one drag ago.
    /// `false` means a row the selection was anchored to is no longer in the
    /// table at all — a layout change, or a fresh diff — and the caller should
    /// drop the selection rather than repair it, because there is no honest
    /// answer to *where did that line go*.
    pub fn resolve(&mut self, order: &[RowRef]) -> bool {
        let (mut a, mut b) = (None, None);
        for (i, r) in order.iter().enumerate() {
            let id = r.logical();
            if id == self.anchor.row {
                let at = a.get_or_insert(i..i);
                at.end = i + 1;
            }
            // Not `else`: both ends are on the same row whenever a selection
            // fits on one line, which is most of them.
            if id == self.head.row {
                let at = b.get_or_insert(i..i);
                at.end = i + 1;
            }
        }
        match (a, b) {
            (Some(a), Some(b)) => {
                self.anchor.at = a;
                self.head.at = b;
                true
            }
            _ => false,
        }
    }

    /// The selection as text, one row per line.
    ///
    /// A wrapped line copies **once**, as the line it is: the soft breaks were a
    /// property of the window, and pasting them back would be pasting the width
    /// of somebody's window into their file.
    ///
    /// A row whose part has no text at all is *skipped* rather than emitted as a
    /// blank line — which is what makes dragging down the new side of a
    /// side-by-side diff yield the new file, holes and all removed, instead of a
    /// paste full of gaps. A row that has text and it is empty is still a line.
    pub fn text(&self, order: &[RowRef], src: &dyn Text) -> String {
        let mut out = String::new();
        let mut last: Option<RowId> = None;
        let rows = self.rows();
        for (visual, row) in order
            .iter()
            .enumerate()
            .take(rows.end.min(order.len()))
            .skip(rows.start)
        {
            let id = row.logical();
            if last == Some(id) {
                continue;
            }
            last = Some(id);
            let Some(text) = src.text(id, self.part) else {
                continue;
            };
            let Some(sel) = self.at(visual, id) else {
                continue;
            };
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&text[sel.range(text.len())]);
        }
        out
    }
}

/// Where a row's selectable text comes from.
///
/// A trait and not a closure because [`Selection::text`] takes it as `&dyn` and
/// a borrowing closure will not go through one. A frontend implements it over
/// whatever holds its presentations — three lines — and `None` means the row has
/// no such text: a decoration, an image, or the empty side of a two-column row.
pub trait Text {
    fn text(&self, row: RowId, part: u16) -> Option<&str>;
}

/// The word around `off`, for a double-click.
///
/// Three classes, not two: letters and digits and `_` are a word, whitespace is
/// a run of whitespace, and everything else is a run of punctuation. Lumping
/// punctuation in with letters selects `foo(bar,` as one word, and lumping it
/// with whitespace makes a double-click on `=>` select nothing — both of which
/// are worse than either on their own.
///
/// `off` past the end, or inside a character, clamps to the nearest boundary
/// rather than panicking: it arrives from a pixel measurement.
pub fn word_at(text: &str, off: usize) -> Range<usize> {
    #[derive(PartialEq)]
    enum Class {
        Word,
        Space,
        Other,
    }
    fn class(c: char) -> Class {
        if c.is_alphanumeric() || c == '_' {
            Class::Word
        } else if c.is_whitespace() {
            Class::Space
        } else {
            Class::Other
        }
    }

    let mut off = off.min(text.len());
    while off > 0 && !text.is_char_boundary(off) {
        off -= 1;
    }
    // The character *under* the caret, or the one before it at the end of a
    // line — a double-click past the last word should still select that word.
    let Some(here) = text[off..]
        .chars()
        .next()
        .or_else(|| text[..off].chars().next_back())
    else {
        return 0..0;
    };
    let want = class(here);
    let start = text[..off]
        .char_indices()
        .rev()
        .take_while(|(_, c)| class(*c) == want)
        .map(|(i, _)| i)
        .last()
        .unwrap_or(off);
    let end = text[off..]
        .char_indices()
        .take_while(|(_, c)| class(*c) == want)
        .map(|(i, c)| off + i + c.len_utf8())
        .last()
        .unwrap_or(off);
    start..end.max(start)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn order(spec: &[(u16, u32, u16)]) -> Vec<RowRef> {
        spec.iter()
            .map(|&(owner, index, seg)| RowRef { owner, seg, index })
            .collect()
    }

    /// One row per line, no wrapping: the shape of nearly every diff.
    fn flat(n: u32) -> Vec<RowRef> {
        (0..n)
            .map(|i| RowRef {
                owner: 0,
                seg: 0,
                index: i,
            })
            .collect()
    }

    struct Lines(Vec<&'static str>);

    impl Text for Lines {
        fn text(&self, row: RowId, part: u16) -> Option<&str> {
            // Part 1 is the second column and only the first two rows have one,
            // which is what a lone removal looks like.
            match part {
                0 => self.0.get(row.1 as usize).copied(),
                _ => self.0.get(row.1 as usize).copied().filter(|_| row.1 < 2),
            }
        }
    }

    fn sel(from: (u32, usize), to: (u32, usize)) -> Selection {
        let mut s = Selection::new(0, Caret::new((0, from.0), from.1, from.0 as usize));
        s.extend(Caret::new((0, to.0), to.1, to.0 as usize));
        s
    }

    #[test]
    fn a_click_that_did_not_drag_selects_nothing() {
        // The reason this matters: a click is how you *clear* a selection, and a
        // one-byte selection that painted a sliver would make that impossible.
        let s = Selection::new(0, Caret::new((0, 3), 5, 3));
        assert!(s.is_empty());
        assert_eq!(s.at(3, (0, 3)), None);
        assert_eq!(s.rows(), 0..0);
        assert_eq!(s.text(&flat(9), &Lines(vec!["one", "two"])), "");
    }

    #[test]
    fn one_row_selects_the_bytes_between_the_two_offsets() {
        let s = sel((1, 2), (1, 6));
        let got = s.at(1, (0, 1)).expect("the row the carets are on");
        assert_eq!(got.range(100), 2..6);
        assert_eq!(s.at(0, (0, 0)), None, "the row above");
        assert_eq!(s.at(2, (0, 2)), None, "the row below");
    }

    #[test]
    fn dragging_backwards_selects_the_same_bytes_as_dragging_forwards() {
        // The anchor is not the start: it is the end that did not move.
        let forwards = sel((1, 4), (3, 2));
        let backwards = sel((3, 2), (1, 4));
        for v in 0..5u32 {
            let a = forwards.at(v as usize, (0, v)).map(|s| s.range(20));
            let b = backwards.at(v as usize, (0, v)).map(|s| s.range(20));
            assert_eq!(a, b, "row {v}");
        }
        assert_eq!(forwards.rows(), 1..4);
    }

    #[test]
    fn a_row_in_the_middle_is_selected_whole_and_the_ends_are_not() {
        let s = sel((1, 4), (3, 2));
        assert_eq!(
            s.at(1, (0, 1)).unwrap().range(10),
            4..10,
            "from the anchor to the end"
        );
        assert_eq!(s.at(2, (0, 2)).unwrap().range(10), 0..10, "all of it");
        assert_eq!(s.at(3, (0, 3)).unwrap().range(10), 0..2, "up to the head");
    }

    #[test]
    fn a_selection_ending_at_the_start_of_a_row_does_not_highlight_it() {
        // Drag from the middle of one line to the very start of the next: the
        // second line contributes nothing, and a zero-length highlight on it
        // would be a stray one-pixel block.
        let s = sel((1, 4), (2, 0));
        assert_eq!(s.at(1, (0, 1)).unwrap().range(10), 4..10);
        assert_eq!(s.at(2, (0, 2)), None);
    }

    #[test]
    fn a_wrapped_line_is_one_row_however_many_it_is_drawn_on() {
        // Line 1 wraps onto three visual rows, 1..4. The carets address the
        // *line*, so a selection inside it is one range and the three rows each
        // take the part of it they draw.
        let table = order(&[(0, 0, 0), (0, 1, 0), (0, 1, 1), (0, 1, 2), (0, 2, 0)]);
        let mut s = Selection::new(0, Caret::new((0, 1), 3, 1));
        s.extend(Caret::new((0, 1), 40, 3));
        assert!(s.resolve(&table));
        assert_eq!(s.anchor.at, 1..4, "the whole run of visual rows");
        assert_eq!(s.rows(), 1..4);
        for v in 1..4 {
            assert_eq!(
                s.at(v, (0, 1)).unwrap().range(60),
                3..40,
                "row {v} of the line"
            );
        }
        // And it copies once, not three times.
        assert_eq!(
            s.text(&table, &Lines(vec!["a", "0123456789", "c"])),
            "3456789"
        );
    }

    #[test]
    fn resolve_finds_a_row_that_a_reflow_moved() {
        // The same two lines, wrapped harder: line 1 now takes four rows, so
        // line 2 has moved down two. A selection anchored to the visual row
        // would now be pointing at the middle of line 1.
        let before = order(&[(0, 0, 0), (0, 1, 0), (0, 1, 1), (0, 2, 0)]);
        let mut s = Selection::new(0, Caret::new((0, 2), 0, 3));
        s.extend(Caret::new((0, 2), 4, 3));
        assert!(s.resolve(&before));
        assert_eq!(s.rows(), 3..4);

        let after = order(&[
            (0, 0, 0),
            (0, 1, 0),
            (0, 1, 1),
            (0, 1, 2),
            (0, 1, 3),
            (0, 2, 0),
        ]);
        assert!(s.resolve(&after));
        assert_eq!(s.rows(), 5..6);
        assert_eq!(s.at(5, (0, 2)).unwrap().range(9), 0..4);
    }

    #[test]
    fn resolve_says_no_when_the_rows_are_gone() {
        // A layout change: the rows are somebody else's now. Repairing this
        // would mean guessing, so the caller drops the selection.
        let mut s = sel((1, 0), (2, 3));
        assert!(!s.resolve(&order(&[(1, 0, 0), (1, 1, 0)])));
    }

    #[test]
    fn copying_joins_rows_with_newlines_and_never_a_trailing_one() {
        let table = flat(4);
        let s = sel((0, 1), (2, 2));
        assert_eq!(
            s.text(&table, &Lines(vec!["abcd", "efgh", "ijkl", "mnop"])),
            "bcd\nefgh\nij"
        );
    }

    #[test]
    fn a_part_with_no_text_is_skipped_rather_than_pasted_as_a_blank_line() {
        // Dragging down the new side of a side-by-side diff past a lone
        // removal. The removal has no new-side text, and a blank line there
        // would be a paste that does not compile.
        let table = flat(4);
        let mut s = sel((0, 0), (3, 4));
        s.part = 1;
        assert_eq!(
            s.text(&table, &Lines(vec!["aaaa", "bbbb", "cccc", "dddd"])),
            "aaaa\nbbbb"
        );
    }

    #[test]
    fn select_all_covers_every_row_and_all_of_the_last_one() {
        let table = flat(3);
        let s = Selection::all(&table).expect("a non-empty diff");
        assert_eq!(s.rows(), 0..3);
        assert_eq!(
            s.at(2, (0, 2)).unwrap().range(4),
            0..4,
            "to the end of the last line"
        );
        assert_eq!(
            s.text(&table, &Lines(vec!["one", "two", "three"])),
            "one\ntwo\nthree"
        );
        assert_eq!(Selection::all(&[]), None);
    }

    #[test]
    fn a_word_is_letters_or_punctuation_or_whitespace_and_never_two_of_them() {
        let text = "let x = foo(bar_baz, 2);";
        assert_eq!(&text[word_at(text, 0)], "let");
        assert_eq!(&text[word_at(text, 2)], "let");
        assert_eq!(&text[word_at(text, 3)], " ");
        assert_eq!(&text[word_at(text, 12)], "bar_baz");
        assert_eq!(&text[word_at(text, 8)], "foo");
        assert_eq!(&text[word_at(text, 11)], "(", "punctuation is its own run");
        assert_eq!(
            &text[word_at(text, 22)],
            ");",
            "a run of punctuation, together"
        );
    }

    #[test]
    fn a_word_survives_the_ends_and_the_middle_of_a_character() {
        assert_eq!(word_at("", 0), 0..0);
        assert_eq!(
            word_at("ab", 99),
            0..2,
            "past the end clamps back onto the word"
        );
        let text = "héllo wörld";
        // Offset 2 is inside the two-byte `é`.
        assert_eq!(&text[word_at(text, 2)], "héllo");
        assert_eq!(&text[word_at(text, 8)], "wörld");
    }
}
