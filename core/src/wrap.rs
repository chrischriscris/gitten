//! Where a line breaks when it is wider than what is there to draw it in.
//!
//! A long line is the one thing in a diff you cannot read by scrolling: the eye
//! loses the row on the way back. Wrapping is the answer, and it is a *policy* —
//! break at a space, break at the column, break after a comma the way a code
//! formatter would — so it is a seam rather than a function.
//!
//! # A wrapped line is still one line
//!
//! It has to be. Every earlier stage addresses lines: the edit script, the hunk
//! numbering, `replace_pairs`, `align`, the intraline spans and the syntax
//! tokens. If wrapping split a line in two before those ran, a removal would
//! pair with the wrong addition and its highlighted words would describe a line
//! that is not beside it — the failure `align`'s doc comment exists to prevent.
//!
//! So wrapping is the *last* thing that happens, after everything that decides
//! what a line means, and it produces **byte ranges into the line** rather than
//! new lines. A renderer draws range `k` of line `i`; the line, its numbers, its
//! tokens and its spans are untouched and shared by all of its rows.
//!
//! # Why not one taller row
//!
//! Because `uniform_list` is the only reason a 714k-row diff scrolls at all, and
//! it needs every row the same height. A wrapped line is therefore *n rows*, not
//! one tall one — which is also what keeps the whole thing virtualized: only the
//! visible ranges are ever sliced.
//!
//! # What is shared
//!
//! Everything except the break points. A [`Wrap`] returns where it would break
//! and nothing else; [`Wrapped`] turns those into the range partition, validates
//! them, holds them flat and answers by index. So an implementation cannot
//! produce a range that points past its line — the same guarantee, for the same
//! reason, as clipping before the intraline pass in
//! [`prepared`](crate::prepared).

use std::ops::Range;

/// One break: where the row being drawn ends, and where the next one starts.
///
/// Two offsets and not one, so an implementation can *drop* what it broke on.
/// Word wrap breaks on a run of spaces and nobody wants those spaces drawn down
/// the left edge of the continuation, nor counted against its width. `end` is
/// the last byte of the row above, `next` the first byte of the row below, and
/// everything between them is thrown away.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Break {
    /// End of the row that stops here, exclusive.
    pub end: u32,
    /// Start of the row that continues, inclusive. Never less than `end`.
    pub next: u32,
}

impl Break {
    /// A break that keeps every byte — what a hard break at a column is.
    pub fn hard(at: usize) -> Self {
        Self { end: at as u32, next: at as u32 }
    }
}

/// Where a line breaks. The seam.
///
/// An implementation is a pure function of one line and a column budget. It gets
/// no theme, no font and no view, because a break point is a property of the
/// text: the frontend has already turned pixels into columns by the time this is
/// called, which is what lets the same implementation serve the window, a
/// terminal and a test.
pub trait Wrap {
    fn name(&self) -> &'static str;

    /// Whether this implementation ever breaks anything.
    ///
    /// [`Off`] answers false, and that is worth a method rather than a name
    /// comparison: a frontend reflows on every window resize, and without this
    /// it would rescan every line of a 714k-line diff on each pixel of a drag to
    /// be told there was nothing to do.
    fn breaks_lines(&self) -> bool {
        true
    }

    /// Appends where `text` breaks, given a budget of `cols` characters per row.
    ///
    /// Ascending, inside the line, on character boundaries. Anything else is
    /// dropped by [`Wrapped::build`] rather than trusted — a bad range would be
    /// a panic on the render path, and a wrap is exactly the kind of thing an
    /// extension writes in an afternoon.
    ///
    /// `cols` is at least 1. `out` may already hold another line's breaks: this
    /// runs once per line of a diff, so the buffer is reused and appending is
    /// the contract.
    fn breaks(&self, text: &str, cols: usize, out: &mut Vec<Break>);
}

/// Never breaks. What "wrapping off" is, and the reason it is an implementation
/// rather than a flag beside one: the title-bar picker is a pure function of a
/// registry, so "off" being in the registry is what gets it into the menu with
/// nothing written by hand — see `docs/decisions/0015`.
pub struct Off;

impl Wrap for Off {
    fn name(&self) -> &'static str {
        "off"
    }

    fn breaks_lines(&self) -> bool {
        false
    }

    fn breaks(&self, _text: &str, _cols: usize, _out: &mut Vec<Break>) {}
}

/// Breaks exactly at the column, mid-word.
///
/// Nothing is ever dropped and nothing is ever wider than the budget, which
/// makes it the honest choice for a minified bundle or a base64 blob — text with
/// no words in it, where [`Word`]'s search for a space is a scan that finds
/// nothing.
pub struct Char;

impl Wrap for Char {
    fn name(&self) -> &'static str {
        "char"
    }

    fn breaks(&self, text: &str, cols: usize, out: &mut Vec<Break>) {
        let mut n = 0;
        for (i, _) in text.char_indices() {
            if n == cols {
                out.push(Break::hard(i));
                n = 0;
            }
            n += 1;
        }
    }
}

/// Breaks at the last run of whitespace that fits, and drops it.
///
/// The shipped default. Three things about it are decisions rather than details:
///
/// - **A row is never wider than the budget.** The break is searched for
///   *backwards* from the column, so the whitespace it lands on is inside the
///   row rather than past it. Searching forwards is the other way to write this
///   and it overflows by however long the next word is.
/// - **The whitespace is dropped, not drawn.** Twelve spaces at the end of a row
///   are invisible; the same twelve down the left edge of the continuation are
///   an indent that means nothing, and they eat the budget of every row after
///   the first.
/// - **No space means a hard break.** A 2000-character base64 line has no break
///   opportunity anywhere, and the alternative to breaking mid-word is a row
///   that overflows the window — which is the thing wrapping is for. Same when
///   the only whitespace is the line's own leading indent: breaking there would
///   emit an empty row and say nothing.
pub struct Word;

impl Wrap for Word {
    fn name(&self) -> &'static str {
        "word"
    }

    fn breaks(&self, text: &str, cols: usize, out: &mut Vec<Break>) {
        let b = text.as_bytes();
        let mut start = 0usize;
        loop {
            // The byte the budget runs out at, from `start`. Walking characters
            // rather than bytes: a line of box drawing is a third as many
            // columns as it is bytes.
            let mut limit = text.len();
            let mut n = 0;
            for (i, _) in text[start..].char_indices() {
                if n == cols {
                    limit = start + i;
                    break;
                }
                n += 1;
            }
            if limit == text.len() {
                return;
            }

            // Backwards from the budget for a run of whitespace to break on. A
            // byte scan is safe here however the line is encoded: no byte of a
            // multi-byte character can be a space or a tab.
            //
            // The run may *start* at the budget — `limit` is inclusive, and that
            // off-by-one is the difference between "the quick" and "the". It may
            // also end past it, which costs nothing: what is dropped is not
            // drawn, so only its start is measured.
            let ws = |i: usize| b[i] == b' ' || b[i] == b'\t';
            let mut cut = None;
            let mut i = limit;
            while i > start {
                if ws(i) {
                    let mut s = i;
                    while s > start && ws(s - 1) {
                        s -= 1;
                    }
                    let mut e = i + 1;
                    while e < text.len() && ws(e) {
                        e += 1;
                    }
                    // `s == start` is a line whose own indent is longer than the
                    // budget; breaking there draws an empty row. `e` at the end
                    // is a line with nothing after the run but more whitespace,
                    // and a break before it would draw one too.
                    if s > start && e < text.len() {
                        cut = Some(Break { end: s as u32, next: e as u32 });
                    }
                    break;
                }
                i -= 1;
            }

            let br = cut.unwrap_or_else(|| Break::hard(limit));
            start = br.next as usize;
            out.push(br);
        }
    }
}

/// Every registered [`Wrap`], and which one is in use.
///
/// Shaped like [`Differs`](crate::differ::Differs) on purpose: register,
/// select by name, and ask for the names so an error message and a menu both
/// describe what is actually there rather than what was there when they were
/// written.
pub struct Wraps {
    impls: Vec<Box<dyn Wrap>>,
    selected: usize,
}

impl Default for Wraps {
    fn default() -> Self {
        Self::builtin()
    }
}

impl Wraps {
    /// The three shipped policies, with `word` selected: wrapping is on out of
    /// the box, because a diff you have to scroll sideways to read is the
    /// problem this exists to solve.
    ///
    /// `off` first, so the menu reads from least to most aggressive.
    pub fn builtin() -> Self {
        let mut w = Self { impls: Vec::new(), selected: 0 };
        w.register(Off);
        w.register(Word);
        w.register(Char);
        w.select("word");
        w
    }

    /// Adds one, replacing any already registered under the same name — so a
    /// built-in can be corrected rather than only added to.
    pub fn register(&mut self, wrap: impl Wrap + 'static) {
        match self.impls.iter().position(|w| w.name() == wrap.name()) {
            Some(i) => self.impls[i] = Box::new(wrap),
            None => self.impls.push(Box::new(wrap)),
        }
    }

    /// False when nothing is registered under that name, which is what a config
    /// file reports back rather than failing over.
    pub fn select(&mut self, name: &str) -> bool {
        match self.position(name) {
            Some(i) => {
                self.selected = i;
                true
            }
            None => false,
        }
    }

    pub fn position(&self, name: &str) -> Option<usize> {
        self.impls.iter().position(|w| w.name() == name)
    }

    pub fn names(&self) -> Vec<&'static str> {
        self.impls.iter().map(|w| w.name()).collect()
    }

    pub fn selected(&self) -> &'static str {
        self.impls[self.selected].name()
    }

    pub fn selected_index(&self) -> usize {
        self.selected
    }

    pub fn len(&self) -> usize {
        self.impls.len()
    }

    pub fn is_empty(&self) -> bool {
        self.impls.is_empty()
    }

    /// One by index, for a frontend whose control published the list. Out of
    /// range falls back to the selected one rather than to `off`: a stale index
    /// means the menu moved, and drawing the diff the way it was already being
    /// drawn is the answer that surprises nobody.
    pub fn at(&self, index: usize) -> &dyn Wrap {
        self.impls.get(index).unwrap_or(&self.impls[self.selected]).as_ref()
    }

    pub fn current(&self) -> &dyn Wrap {
        self.impls[self.selected].as_ref()
    }
}

/// Where every line breaks, in one flat table.
///
/// A `Vec<Vec<Break>>` is the obvious shape and the wrong one: at 714k lines it
/// is 714k allocations to chase, for a table that is mostly empty because most
/// lines fit. This is the same trade as the order table in the diff view — one
/// contiguous buffer, indexed.
///
/// Empty is the useful default. A presentation that has not been reflowed yet —
/// or one whose width is not known until the first frame — asks for row counts
/// and ranges before this holds anything, and gets "one row, the whole line",
/// which is exactly what it should draw.
#[derive(Debug, Default, Clone)]
pub struct Wrapped {
    /// Every line's breaks, concatenated.
    breaks: Vec<Break>,
    /// `at[i]..at[i + 1]` is line `i`'s slice of `breaks`. One longer than the
    /// number of lines, so the last line needs no special case.
    at: Vec<u32>,
    /// Breaks thrown away for breaking the rules in [`Wrap::breaks`].
    ///
    /// Counted rather than asserted, and reported rather than swallowed. An
    /// assertion would make the validation itself untestable and would turn a
    /// third-party wrap's bug into a crash of the whole app; saying nothing
    /// would leave a presentation quietly not wrapping with no way to tell.
    rejected: usize,
}

impl Wrapped {
    /// Wraps every line, each to its own budget.
    ///
    /// Per-line and not one column count for the table, because a budget is not
    /// always a property of the window: a rendered Markdown row draws a bullet,
    /// an indent and a bar before its text and a heading draws its text larger,
    /// so two rows of the same width have different numbers of columns in them.
    /// Passing the budget in per line is what stops that presentation having to
    /// implement its own wrap.
    ///
    /// A budget of 0 means "never break this line" — what a table row wants,
    /// where a break would shear a grid that lines up character by character
    /// with the rows above and below it.
    pub fn build<'a>(lines: impl Iterator<Item = (&'a str, usize)>, wrap: &dyn Wrap) -> Self {
        let mut out = Self { breaks: Vec::new(), at: vec![0], rejected: 0 };
        let mut scratch = Vec::new();
        for (text, cols) in lines {
            if cols > 0 && !text.is_empty() {
                scratch.clear();
                wrap.breaks(text, cols, &mut scratch);
                out.take(text, &scratch);
            }
            out.at.push(out.breaks.len() as u32);
        }
        out
    }

    /// Keeps the breaks that describe a range of `text` and drops the rest.
    ///
    /// Not defensiveness for its own sake: a range that points past its line is
    /// a slice panic on the render path, and `Wrap` is a seam an extension
    /// reaches. The rules are the ones the trait documents — ascending, inside
    /// the line, on character boundaries, and each one strictly past the last,
    /// because a break that does not advance is an empty row and, repeated, a
    /// row count that does not terminate.
    fn take(&mut self, text: &str, breaks: &[Break]) {
        let mut prev = 0usize;
        for br in breaks {
            let (end, next) = (br.end as usize, br.next as usize);
            let ok = end >= prev
                && next >= end
                && next > prev
                && next < text.len()
                && text.is_char_boundary(end)
                && text.is_char_boundary(next);
            if ok {
                self.breaks.push(*br);
                prev = next;
            } else {
                self.rejected += 1;
            }
        }
    }

    /// How many breaks were thrown away as invalid. Zero for everything shipped;
    /// a frontend reports it so a wrap that is quietly not working says so.
    pub fn rejected(&self) -> usize {
        self.rejected
    }

    /// How many rows line `index` occupies. Never zero: a line nothing was
    /// computed for, and an empty line, are both one row.
    pub fn rows(&self, index: usize) -> usize {
        match self.at.get(index + 1) {
            Some(end) => (end - self.at[index]) as usize + 1,
            None => 1,
        }
    }

    /// Total rows, for a frontend that wants to say how much wrapping cost.
    pub fn total(&self) -> usize {
        self.lines() + self.breaks.len()
    }

    pub fn lines(&self) -> usize {
        self.at.len().saturating_sub(1)
    }

    /// The bytes of `text` that row `row` of line `index` draws.
    ///
    /// `text` is passed rather than stored: the caller is holding the line
    /// anyway, and a length per line is a second table the size of the first for
    /// something already in hand. Out of range in either argument is the whole
    /// line, which is what an unwrapped presentation asks for on every row.
    pub fn range(&self, index: usize, row: usize, text: &str) -> Range<usize> {
        let Some(&end) = self.at.get(index + 1) else {
            return 0..text.len();
        };
        let first = self.at[index] as usize;
        let count = end as usize - first;
        if row > count {
            return 0..text.len();
        }
        let start = if row == 0 { 0 } else { self.breaks[first + row - 1].next as usize };
        let stop =
            if row == count { text.len() } else { self.breaks[first + row].end as usize };
        start..stop.max(start)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The text each row of `text` draws, which is the only thing about a wrap
    /// anybody can check by eye.
    fn rows(w: &dyn Wrap, text: &str, cols: usize) -> Vec<String> {
        let t = Wrapped::build(std::iter::once((text, cols)), w);
        (0..t.rows(0)).map(|r| text[t.range(0, r, text)].to_string()).collect()
    }

    #[test]
    fn word_wrap_breaks_on_a_space_and_drops_it() {
        assert_eq!(
            rows(&Word, "the quick brown fox jumps", 10),
            ["the quick", "brown fox", "jumps"]
        );
    }

    #[test]
    fn no_row_is_ever_wider_than_the_budget() {
        // The trap: searching forwards for the next space overflows by however
        // long the next word is, and it looks like it works on prose.
        //
        // Measured after `trim_end`, which is the honest budget: whitespace at
        // the end of a row is not ink, and a run that reaches the end of the
        // line is deliberately left on the last row rather than broken before —
        // breaking there would draw a row of nothing.
        for text in ["a bb ccc dddd eeeee ffffff ggggggg", "short   ", "a b\t \t  "] {
            for cols in 1..40 {
                for row in rows(&Word, text, cols) {
                    assert!(row.trim_end().chars().count() <= cols, "{cols}: {row:?}");
                }
            }
        }
    }

    #[test]
    fn a_word_longer_than_the_budget_is_broken_rather_than_overflowed() {
        // What a minified bundle and a base64 blob are made of. The alternative
        // is a row wider than the window, which is what wrapping is for.
        assert_eq!(rows(&Word, "abcdefghij", 4), ["abcd", "efgh", "ij"]);
        assert_eq!(rows(&Word, "hi abcdefghij", 4), ["hi", "abcd", "efgh", "ij"]);
    }

    #[test]
    fn an_indent_longer_than_the_budget_does_not_produce_an_empty_row() {
        // Breaking on the leading whitespace is a break at offset 0: a row that
        // draws nothing, and then the same decision again on what is left.
        let rows = rows(&Word, "        deeply.indented(call)", 4);
        assert!(rows.iter().all(|r| !r.is_empty()), "{rows:?}");
        assert_eq!(rows.concat().trim(), "deeply.indented(call)".trim());
    }

    #[test]
    fn every_byte_of_a_line_is_either_drawn_or_whitespace() {
        // The partition is allowed to have holes — that is how dropping the
        // space it broke on works — but only whitespace may fall in one.
        let text = "  let x = compute(alpha, beta) + gamma;   // and a trailing note";
        for cols in [4, 7, 13, 20, 61] {
            let t = Wrapped::build(std::iter::once((text, cols)), &Word);
            let mut seen = vec![false; text.len()];
            for r in 0..t.rows(0) {
                for i in t.range(0, r, text) {
                    seen[i] = true;
                }
            }
            for (i, drawn) in seen.iter().enumerate() {
                assert!(
                    *drawn || text.as_bytes()[i].is_ascii_whitespace(),
                    "{cols}: byte {i} ({:?}) is drawn nowhere",
                    &text[i..i + 1]
                );
            }
        }
    }

    #[test]
    fn char_wrap_keeps_every_byte_and_breaks_on_the_column() {
        assert_eq!(rows(&Char, "the quick brown", 5), ["the q", "uick ", "brown"]);
        let text = "the quick brown";
        let t = Wrapped::build(std::iter::once((text, 5)), &Char);
        let joined: String = (0..t.rows(0)).map(|r| &text[t.range(0, r, text)]).collect();
        assert_eq!(joined, text, "char wrap dropped something");
    }

    #[test]
    fn a_multibyte_line_is_measured_in_columns_not_bytes() {
        // Box drawing is three bytes a character. Measured as bytes, a rule
        // wraps at a third of the width it should — and slicing mid-character
        // is a panic, not a cosmetic bug.
        let text = "─".repeat(12);
        let got = rows(&Char, &text, 5);
        assert_eq!(got, ["─────", "─────", "──"]);
    }

    #[test]
    fn an_empty_line_is_one_row() {
        let t = Wrapped::build(std::iter::once(("", 10)), &Word);
        assert_eq!(t.rows(0), 1);
        assert_eq!(t.range(0, 0, ""), 0..0);
    }

    #[test]
    fn a_budget_of_zero_never_breaks() {
        // What a Markdown table row asks for: its grid lines up character by
        // character with the rows above and below, and a break shears it.
        assert_eq!(rows(&Word, "a b c d e f g h", 0), ["a b c d e f g h"]);
    }

    #[test]
    fn off_is_the_old_behaviour_exactly() {
        assert!(!Off.breaks_lines());
        assert_eq!(rows(&Off, "a line long enough to wrap several times over", 5).len(), 1);
    }

    #[test]
    fn a_table_holds_many_lines_and_answers_each_by_index() {
        let lines = ["short", "the quick brown fox", "", "tiny"];
        let t = Wrapped::build(lines.iter().map(|l| (*l, 9)), &Word);
        assert_eq!(t.lines(), 4);
        assert_eq!((0..4).map(|i| t.rows(i)).collect::<Vec<_>>(), [1, 2, 1, 1]);
        assert_eq!(t.total(), 5);
        assert_eq!(&lines[1][t.range(1, 1, lines[1])], "brown fox");
        // A line that did not wrap still answers with the whole of itself.
        assert_eq!(&lines[0][t.range(0, 0, lines[0])], "short");
    }

    #[test]
    fn an_unbuilt_table_says_one_row_and_the_whole_line() {
        // What every presentation asks before the first frame has measured the
        // window. "Nothing wraps" has to be the answer, not a panic.
        let t = Wrapped::default();
        assert_eq!(t.rows(0), 1);
        assert_eq!(t.rows(9999), 1);
        assert_eq!(t.range(0, 0, "a line"), 0..6);
        assert_eq!(t.range(7, 3, "a line"), 0..6);
    }

    #[test]
    fn a_row_past_the_end_is_the_whole_line_rather_than_a_panic() {
        // The index comes from an order table that may have been built against
        // a different width. Legible beats correct-or-crash.
        let t = Wrapped::build(std::iter::once(("the quick brown fox", 9)), &Word);
        assert_eq!(t.range(0, 99, "the quick brown fox"), 0..19);
    }

    // ---------------------------------------------------------- the registry

    struct Silly;
    impl Wrap for Silly {
        fn name(&self) -> &'static str {
            "silly"
        }
        /// Every rule broken at once — past the end, backwards, inside a
        /// character, at the very end — with one legal break in the middle, so
        /// the test can tell "validated" from "threw the line away".
        fn breaks(&self, text: &str, _cols: usize, out: &mut Vec<Break>) {
            out.push(Break::hard(text.len() + 50));
            out.push(Break { end: 9, next: 2 });
            out.push(Break::hard(3));
            out.push(Break::hard(1));
            out.push(Break::hard(text.len()));
        }
    }

    #[test]
    fn a_wrap_that_lies_cannot_produce_a_range_that_panics() {
        // The whole reason `Wrapped` validates rather than trusts: these ranges
        // index a line on the render path, and this is a seam an extension
        // reaches.
        let text = "─── some text ───";
        let t = Wrapped::build(std::iter::once((text, 4)), &Silly);
        for r in 0..t.rows(0) {
            let range = t.range(0, r, text);
            assert!(range.end <= text.len() && range.start <= range.end);
            let _ = &text[range]; // would panic on a bad boundary
        }
        // And it says so, rather than looking like a wrap that found nothing.
        assert_eq!(t.rejected(), 4);
        assert_eq!(t.rows(0), 2, "the one legal break survived");
        assert_eq!(&text[t.range(0, 0, text)], "─");
    }

    #[test]
    fn a_wrap_can_be_added_selected_and_reached_by_index() {
        // The swap test: a fourth policy is registered, selected by name, and
        // handed back by the index a menu would publish.
        let mut w = Wraps::builtin();
        assert_eq!(w.names(), ["off", "word", "char"]);
        assert_eq!(w.selected(), "word");

        w.register(Silly);
        assert!(w.select("silly"));
        assert_eq!(w.selected(), "silly");
        assert_eq!(w.at(w.selected_index()).name(), "silly");
        assert!(!w.select("nothing-registered-under-this"));
        assert_eq!(w.selected(), "silly", "a failed select changed the selection");
    }

    #[test]
    fn registering_a_name_twice_replaces_it() {
        struct NotWord;
        impl Wrap for NotWord {
            fn name(&self) -> &'static str {
                "word"
            }
            fn breaks(&self, _: &str, _: usize, _: &mut Vec<Break>) {}
        }
        let mut w = Wraps::builtin();
        w.register(NotWord);
        assert_eq!(w.names(), ["off", "word", "char"], "it was added rather than replaced");
        assert_eq!(rows(w.current(), "a b c d e", 3).len(), 1, "the built-in still ran");
    }

    #[test]
    fn an_out_of_range_index_falls_back_to_the_selection() {
        let w = Wraps::builtin();
        assert_eq!(w.at(99).name(), "word");
    }
}
