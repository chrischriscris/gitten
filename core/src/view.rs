//! Where a list is scrolled to, and which row the keyboard is on.
//!
//! Every view in this app is a list — commits, a diff, a help screen — and each
//! of them needs the same two numbers and the same rule relating them. It was
//! written twice before it was written once, which is the test
//! `docs/architecture.md` sets for something belonging here, and the second copy
//! had already drifted: the terminal's diff named its margin `SCROLLOFF` and its
//! commit list spelled the same number `3` in two places.
//!
//! # Two positions, and why the cursor is the anchor
//!
//! [`Viewport::cursor`] is the row the keyboard is on; [`Viewport::top`] is the
//! first row drawn. Moving the cursor drags the viewport after it, which is
//! lazygit's model and the reason a keyboard-first client has something to act
//! *on*: stage this hunk, open this commit, copy this line all need a row, and a
//! pane that only knows where it is scrolled to has none.
//!
//! It also gives a reflow something honest to keep still. Row 4,102 at 120
//! columns is not row 4,102 at 90, but the *line you are on* exists at either
//! width — so a resize re-finds the cursor and anchoring `top` instead would
//! preserve a position nobody was looking at.
//!
//! # And why scrolling is not the same verb
//!
//! [`Viewport::scroll_by`] moves `top` directly and drags the *cursor* along
//! only as far as it must. [`Viewport::pan_by`] is the deliberately disjoint
//! alternative: it changes visibility and never selection. A client chooses
//! between those contracts rather than reimplementing either arithmetic.

use std::ops::Range;

/// Rows of context kept between the cursor and the edge when it moves.
///
/// Three, because a cursor pinned to the last row gives you no idea what you are
/// scrolling into — the same reason `scrolloff` exists in every editor. A field
/// rather than a constant so `gitten.toml` can hold it, and so a view with three
/// rows of its own can say zero.
pub const SCROLLOFF: usize = 3;

/// How a view scrolls. `[view]` in `gitten.toml`, and the same two numbers in
/// every client.
///
/// Here rather than in a client because both of them were a constant somebody
/// disagreed with the moment they saw it, which is the definition of a setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scrolling {
    /// Rows one [`Viewport::scroll_by`] moves — a wheel notch, `ctrl-e`,
    /// `ctrl-y`.
    ///
    /// **One, because the emulator has already multiplied.** A terminal reports
    /// the wheel as one event per *line* of the platform's scroll delta, so
    /// three rows an event is three times whatever the user set for every other
    /// app on the machine — nine rows a notch on a mouse with macOS's default,
    /// which reads as a page. One row an event makes gitten scroll at exactly the
    /// speed the terminal's own scrollback does, which is the number the hand
    /// already knows. A window gets pixel deltas and does its own arithmetic;
    /// this is the multiplier it applies afterwards.
    pub rows: usize,
    /// Rows of lead kept between the cursor and the edge. See [`SCROLLOFF`].
    pub scrolloff: usize,
    /// Whether to draw a scrollbar beside a list longer than its viewport.
    ///
    /// Data and not a registry, because there is nothing to choose *between*: a
    /// client draws one or it does not. What it looks like is the client's, and
    /// [`Viewport::thumb`] is the part that must not be — a thumb that reached
    /// the bottom before the list did would say the wrong thing in three doors
    /// instead of one.
    pub scrollbar: bool,
}

impl Default for Scrolling {
    fn default() -> Self {
        Self {
            rows: 1,
            scrolloff: SCROLLOFF,
            scrollbar: true,
        }
    }
}

/// A scroll position, a cursor, and the rule relating them.
///
/// Holds no rows: `len` is all it knows about them, because a viewport over a
/// commit list and one over a wrapped diff differ in nothing else. Every method
/// leaves both positions valid — clamped into the list and, except for an
/// explicit [`Viewport::pan_by`], into each other — so there is no state to
/// repair afterwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Viewport {
    len: usize,
    height: usize,
    top: usize,
    cursor: usize,
    scrolloff: usize,
}

impl Default for Viewport {
    fn default() -> Self {
        Self::new()
    }
}

impl Viewport {
    pub fn new() -> Self {
        Self {
            len: 0,
            height: 0,
            top: 0,
            cursor: 0,
            scrolloff: SCROLLOFF,
        }
    }

    // ----------------------------------------------------------------- the shape

    /// How many rows the list has. Clamps the cursor, so a diff that shrank
    /// under it does not leave it pointing past the end.
    pub fn set_len(&mut self, len: usize) {
        self.len = len;
        self.cursor = self.cursor.min(len.saturating_sub(1));
        self.follow();
    }

    /// How many rows are drawn at once.
    pub fn set_height(&mut self, height: usize) {
        if self.height == height {
            return;
        }
        self.height = height;
        self.follow();
    }

    /// The margin, from `[view] scrolloff`. Cheap to call every frame, which is
    /// how a saved config file reaches a view that is already on screen.
    pub fn set_scrolloff(&mut self, rows: usize) {
        if self.scrolloff == rows {
            return;
        }
        self.scrolloff = rows;
        self.follow();
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn height(&self) -> usize {
        self.height
    }

    pub fn top(&self) -> usize {
        self.top
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// How far down the list the cursor is, in `0.0..=1.0`.
    ///
    /// What a layout change keeps when there is no row correspondence to keep:
    /// side-by-side puts a removal and its replacement on one row, so row 900 of
    /// one presentation is not row 900 of another.
    pub fn progress(&self) -> f32 {
        match self.len {
            0 => 0.0,
            n => self.cursor as f32 / n as f32,
        }
    }

    /// The row at `at` rows down the screen, if the list reaches that far.
    ///
    /// The whole of what a paint loop needs, and it is a method so that "is
    /// there a row here" is one question rather than an addition and a bounds
    /// check written per client.
    pub fn row_at(&self, at: usize) -> Option<usize> {
        Some(self.top + at).filter(|n| *n < self.len)
    }

    // -------------------------------------------------------------- the cursor

    /// Moves the cursor `by` rows, clamping at both ends.
    ///
    /// Signed, so one method is `j`, `k`, `ctrl-d` and `ctrl-u`. Clamping rather
    /// than wrapping: a list that jumps from the last row to the first loses
    /// your place by the whole list.
    pub fn move_by(&mut self, by: isize) {
        let last = self.len.saturating_sub(1);
        self.cursor = match by.is_negative() {
            true => self.cursor.saturating_sub(by.unsigned_abs()),
            false => (self.cursor + by as usize).min(last),
        };
        self.follow();
    }

    pub fn down(&mut self) {
        self.move_by(1);
    }

    pub fn up(&mut self) {
        self.move_by(-1);
    }

    /// A screenful, less one row of overlap so the eye has something to land on.
    pub fn page(&mut self, pages: isize) {
        let step = self.height.saturating_sub(1).max(1) as isize;
        self.move_by(pages.saturating_mul(step));
    }

    pub fn to_top(&mut self) {
        self.go_to(0);
    }

    pub fn to_bottom(&mut self) {
        self.go_to(self.len.saturating_sub(1));
    }

    /// Puts a particular row under the cursor: a file header, a search hit, a
    /// row saved across a restart.
    pub fn go_to(&mut self, row: usize) {
        self.cursor = row.min(self.len.saturating_sub(1));
        self.follow();
    }

    /// Moves the cursor off a row that cannot hold it — a section heading, a
    /// separator — and onto the nearest row `selectable` says can, continuing
    /// in the direction the cursor was travelling from `from` and turning back
    /// only when that direction has nothing left. So `j` from the last file of
    /// one section lands on the first file of the next, `k` the reverse, and
    /// `G` onto a trailing heading walks back up to the last real row rather
    /// than stopping on furniture.
    ///
    /// Here and not in a view because every list with headings has this rule
    /// and the predicate is the only part that differs. A list with no such
    /// rows never calls it; a list of nothing but unselectable rows is left
    /// where it was, which is the one honest answer when no row qualifies.
    pub fn settle(&mut self, from: usize, selectable: impl Fn(usize) -> bool) {
        if self.len == 0 || selectable(self.cursor) {
            return;
        }
        let below = (self.cursor + 1..self.len).find(|&i| selectable(i));
        let above = (0..self.cursor).rev().find(|&i| selectable(i));
        let target = match self.cursor >= from {
            true => below.or(above),
            false => above.or(below),
        };
        if let Some(row) = target {
            self.go_to(row);
        }
    }

    /// Puts the cursor at the row nearest `at` of the way down the list.
    ///
    /// Rounds *down* and clamps, so `1.0` is the last row rather than one past
    /// it, and an empty list is row zero rather than a division by zero.
    pub fn go_to_fraction(&mut self, at: f32) {
        self.go_to((self.len as f32 * at) as usize);
    }

    // ------------------------------------------------------------ the viewport

    /// Scrolls the view `by` rows without treating it as a cursor move.
    ///
    /// The cursor comes along only when it would otherwise leave the screen.
    pub fn scroll_by(&mut self, by: isize) {
        let to = match by.is_negative() {
            true => self.top.saturating_sub(by.unsigned_abs()),
            false => self.top.saturating_add(by as usize),
        };
        self.scroll_to(to);
    }

    /// Scrolls so that row `top` is the first one drawn.
    ///
    /// What a dragged scrollbar thumb calls, and what [`scroll_by`](Self::scroll_by)
    /// is written in terms of — a drag is a *position* and a wheel notch is a
    /// delta, and only one of them can be expressed as the other without the
    /// clamping drifting.
    pub fn scroll_to(&mut self, top: usize) {
        let max_top = self.max_top();
        self.top = top.min(max_top);
        let last = self.len.saturating_sub(1);
        let pad = self.pad();
        let low = match self.top == 0 {
            true => 0,
            false => self.top + pad,
        };
        let high = match self.top >= max_top {
            true => last,
            false => (self.top + self.height.saturating_sub(1)).saturating_sub(pad),
        };
        self.cursor = self.cursor.clamp(low.min(high), high.max(low)).min(last);
    }

    /// Pans the visible rows by `by` without changing the selected row.
    ///
    /// This is the terminal wheel's contract: the pointer may inspect any pane
    /// while the keyboard selection remains exactly where it was. A later
    /// cursor move calls [`follow`](Self::follow) and reveals that selection.
    pub fn pan_by(&mut self, by: isize) {
        let top = match by.is_negative() {
            true => self.top.saturating_sub(by.unsigned_abs()),
            false => self.top.saturating_add(by as usize),
        };
        self.top = top.min(self.max_top());
    }

    // ----------------------------------------------------------- the scrollbar

    /// Which cells of a `track`-cell scrollbar the thumb covers, or `None` when
    /// there is nothing to scroll.
    ///
    /// Here rather than in a client because a scrollbar is *arithmetic about a
    /// list* and only its glyphs are a UI — three doors would otherwise each
    /// decide for themselves whether a thumb reaching the bottom means the last
    /// row is visible, and two of them would be wrong.
    ///
    /// Two properties, and both are load-bearing. The thumb is **never shorter
    /// than one cell**, so a 714k-row diff still has something to grab. And it
    /// touches the end of the track **exactly** when the list is scrolled to the
    /// end — which is why the position is measured against
    /// [`max_top`](Self::max_top) and the free travel rather than against the
    /// row count: proportional-to-`len` leaves a thumb short of the bottom on a
    /// list scrolled all the way down, which reads as "there is more" when there
    /// is not.
    ///
    /// `None` and not an empty range: a list shorter than its viewport has no
    /// scrollbar at all, and a track drawn with the thumb filling it is furniture
    /// that says nothing.
    pub fn thumb(&self, track: usize) -> Option<Range<usize>> {
        let max_top = self.max_top();
        if track == 0 || max_top == 0 {
            return None;
        }
        let len = self.thumb_len(track);
        let travel = track - len;
        let start = match travel {
            0 => 0,
            _ => (self.top * travel + max_top / 2) / max_top,
        };
        Some(start..start + len)
    }

    /// The `top` that puts the thumb's first cell at `cell` of a `track`-cell
    /// scrollbar: [`thumb`](Self::thumb) run backwards, for a drag.
    ///
    /// Run backwards rather than approximated, so that grabbing a thumb and
    /// putting it back where it was leaves the list where it was — a scrollbar
    /// that drifts by a row per drag is one nobody uses twice.
    pub fn top_at(&self, cell: usize, track: usize) -> usize {
        let max_top = self.max_top();
        if track == 0 || max_top == 0 {
            return 0;
        }
        let travel = track - self.thumb_len(track);
        match travel {
            0 => 0,
            _ => ((cell.min(travel) * max_top) + travel / 2) / travel,
        }
    }

    /// How many cells of `track` the thumb takes: its share of the list, never
    /// nothing and never the whole track — a thumb with nowhere to travel cannot
    /// say where you are.
    fn thumb_len(&self, track: usize) -> usize {
        let share = (self.height * track).div_ceil(self.len.max(1));
        share.clamp(1, track.saturating_sub(1).max(1))
    }

    /// Keeps the cursor on screen, with [`SCROLLOFF`] rows of lead where there
    /// is room for it.
    ///
    /// Private, and every method above ends in it: a caller that has to remember
    /// to re-establish the invariant is a caller that will forget once.
    fn follow(&mut self) {
        if self.height == 0 {
            self.top = self.cursor;
            return;
        }
        let pad = self.pad();
        self.top = self.top.min(self.len.saturating_sub(1));
        if self.cursor < self.top + pad {
            self.top = self.cursor.saturating_sub(pad);
        }
        let bottom = self.top + self.height.saturating_sub(1);
        if self.cursor + pad > bottom {
            self.top = (self.cursor + pad + 1).saturating_sub(self.height);
        }
        // Never scrolled past the end: a screen of blank rows below a short list
        // is a scroll position that says nothing.
        self.top = self.top.min(self.max_top());
    }

    /// The margin actually in force.
    ///
    /// Dropped rather than scaled on a viewport of half a screen or less: the
    /// margin would pin the cursor to the middle and make every keypress scroll.
    fn pad(&self) -> usize {
        match self.height > 2 * self.scrolloff + 1 {
            true => self.scrolloff,
            false => 0,
        }
    }

    fn max_top(&self) -> usize {
        self.len.saturating_sub(self.height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(len: usize, height: usize) -> Viewport {
        let mut v = Viewport::new();
        v.set_len(len);
        v.set_height(height);
        v
    }

    #[test]
    fn settle_skips_unselectable_rows_in_the_direction_of_travel() {
        // Rows 0 and 3 are headings; 1, 2, 4, 5 are files.
        let heading = |i: usize| i == 0 || i == 3;
        let file = move |i: usize| !heading(i);
        let mut v = view(6, 10);
        v.settle(0, file);
        assert_eq!(v.cursor(), 1, "a list opens on its first real row");
        v.down();
        v.settle(1, file);
        assert_eq!(v.cursor(), 2);
        v.down();
        v.settle(2, file);
        assert_eq!(v.cursor(), 4, "down over a heading lands past it");
        v.up();
        v.settle(4, file);
        assert_eq!(v.cursor(), 2, "up over a heading lands before it");
    }

    #[test]
    fn settle_turns_back_at_the_ends_and_leaves_a_list_of_nothing_alone() {
        // A trailing heading at row 5: `G` lands on it and must walk back.
        let file = |i: usize| i != 0 && i != 5;
        let mut v = view(6, 10);
        v.to_bottom();
        v.settle(1, file);
        assert_eq!(v.cursor(), 4);
        // `gg` onto the leading heading walks forward.
        v.to_top();
        v.settle(4, file);
        assert_eq!(v.cursor(), 1);
        // Nothing selectable: stay put rather than invent a row.
        let mut v = view(3, 10);
        v.go_to(2);
        v.settle(0, |_| false);
        assert_eq!(v.cursor(), 2);
        let mut empty = view(0, 10);
        empty.settle(0, |_| true);
        assert_eq!(empty.cursor(), 0);
    }

    #[test]
    fn the_cursor_clamps_rather_than_wrapping_at_both_ends() {
        let mut v = view(100, 20);
        v.up();
        assert_eq!(v.cursor(), 0);
        v.move_by(1000);
        assert_eq!(v.cursor(), 99);
        v.down();
        assert_eq!(v.cursor(), 99);
    }

    #[test]
    fn the_viewport_follows_the_cursor_and_keeps_a_margin() {
        let mut v = view(100, 20);
        // Down to just inside the margin: nothing has scrolled yet.
        v.move_by(16);
        assert_eq!(v.top(), 0);
        // One more and the margin is breached, so the top follows by one.
        v.down();
        assert_eq!(v.cursor(), 17);
        assert_eq!(v.top(), 1);
        // And back up the same way.
        v.move_by(-14);
        assert_eq!(v.cursor(), 3);
        assert_eq!(v.top(), 0);
    }

    #[test]
    fn the_ends_of_the_list_have_no_margin_to_keep() {
        let mut v = view(100, 20);
        v.to_bottom();
        assert_eq!(v.cursor(), 99);
        assert_eq!(v.top(), 80, "a screen of blank rows below the last one");
        v.to_top();
        assert_eq!((v.cursor(), v.top()), (0, 0));
    }

    #[test]
    fn a_viewport_shorter_than_twice_the_margin_drops_it() {
        let mut v = view(100, 5);
        v.down();
        assert_eq!(
            v.top(),
            0,
            "a margin here would scroll on the first keypress"
        );
        v.move_by(3);
        assert_eq!((v.cursor(), v.top()), (4, 0));
        v.down();
        assert_eq!((v.cursor(), v.top()), (5, 1));
    }

    #[test]
    fn a_scroll_moves_the_view_and_leaves_the_cursor_where_it_is() {
        let mut v = view(100, 20);
        v.go_to(50);
        let cursor = v.cursor();
        v.scroll_by(3);
        assert_eq!(v.cursor(), cursor, "the wheel is not a selection");
        assert_eq!(v.top(), 37);
        v.scroll_by(-3);
        assert_eq!((v.cursor(), v.top()), (50, 34));
    }

    #[test]
    fn a_scroll_drags_the_cursor_rather_than_leaving_it_off_screen() {
        let mut v = view(100, 20);
        v.go_to(50);
        v.scroll_by(30);
        assert_eq!(v.top(), 64);
        assert_eq!(v.cursor(), 67, "the top row plus the margin");
        v.scroll_by(-60);
        assert_eq!(v.top(), 4);
        assert_eq!(v.cursor(), 20, "the bottom row less the margin");
    }

    #[test]
    fn a_pan_never_changes_the_cursor_even_when_it_leaves_the_screen() {
        let mut v = view(100, 20);
        v.go_to(50);
        v.pan_by(30);
        assert_eq!((v.cursor(), v.top()), (50, 64));
        v.set_height(20);
        assert_eq!(
            (v.cursor(), v.top()),
            (50, 64),
            "an unchanged layout snapped the pan back to selection"
        );
        v.pan_by(-60);
        assert_eq!((v.cursor(), v.top()), (50, 4));
    }

    #[test]
    fn a_scroll_stops_at_both_ends() {
        let mut v = view(100, 20);
        v.scroll_by(1000);
        assert_eq!(v.top(), 80, "not one row past the last");
        assert_eq!(v.cursor(), 83);
        v.scroll_by(-1000);
        assert_eq!(v.top(), 0);
        assert_eq!(v.cursor(), 16);
    }

    #[test]
    fn the_last_row_is_reachable_with_the_view_against_the_end() {
        let mut v = view(100, 20);
        v.to_bottom();
        assert_eq!((v.top(), v.cursor()), (80, 99));
        // Scrolling into the end it is already at moves nothing.
        v.scroll_by(5);
        assert_eq!((v.top(), v.cursor()), (80, 99));
    }

    #[test]
    fn a_scroll_of_a_list_shorter_than_the_screen_does_nothing() {
        let mut v = view(5, 20);
        v.scroll_by(10);
        assert_eq!((v.top(), v.cursor()), (0, 0));
    }

    #[test]
    fn a_page_is_a_screenful_less_one_row() {
        let mut v = view(100, 20);
        v.page(1);
        assert_eq!(v.cursor(), 19);
        v.page(-1);
        assert_eq!(v.cursor(), 0);
    }

    #[test]
    fn a_shorter_list_takes_the_cursor_with_it() {
        let mut v = view(100, 20);
        v.to_bottom();
        v.set_len(10);
        assert_eq!(v.cursor(), 9);
        assert_eq!(v.top(), 0);
        v.set_len(0);
        assert_eq!((v.cursor(), v.top()), (0, 0));
        assert!(v.row_at(0).is_none());
    }

    #[test]
    fn a_fraction_lands_on_a_row_that_exists() {
        let mut v = view(100, 20);
        v.go_to_fraction(0.5);
        assert_eq!(v.cursor(), 50);
        v.go_to_fraction(1.0);
        assert_eq!(v.cursor(), 99, "rounded down, not one past the end");
        assert!((v.progress() - 0.99).abs() < 1e-6);
    }

    #[test]
    fn a_scrollbar_thumb_is_proportional_and_never_disappears() {
        let v = view(100, 20);
        // A fifth of the list is on screen, so a fifth of a 20-cell track.
        assert_eq!(v.thumb(20), Some(0..4));
        // A 714k-row diff still has something to grab.
        let big = view(714_000, 40);
        let thumb = big.thumb(38).expect("a scrollable list");
        assert_eq!(thumb.len(), 1);
    }

    #[test]
    fn a_thumb_touches_the_bottom_exactly_when_the_list_does() {
        // The property proportional-to-`len` gets wrong: with 20 of 100 rows on
        // screen the last row is reached at top 80, and a thumb still short of
        // the end there says there is more when there is not.
        let mut v = view(100, 20);
        v.to_bottom();
        assert_eq!(v.top(), 80);
        let thumb = v.thumb(20).unwrap();
        assert_eq!(thumb.end, 20, "the thumb stopped short of the end");
        v.to_top();
        assert_eq!(v.thumb(20).unwrap().start, 0);
    }

    #[test]
    fn a_list_that_fits_has_no_thumb_at_all() {
        assert_eq!(view(10, 20).thumb(20), None);
        assert_eq!(view(100, 20).thumb(0), None, "no track, no thumb");
        assert_eq!(Viewport::new().thumb(20), None);
    }

    #[test]
    fn dragging_a_thumb_back_where_it_was_leaves_the_list_where_it_was() {
        // What `top_at` being the exact inverse buys: a scrollbar that does not
        // drift by a row every time it is grabbed.
        let mut v = view(1000, 25);
        for top in [0, 1, 137, 500, 974, 975] {
            v.scroll_to(top);
            let cell = v.thumb(25).unwrap().start;
            // Several tops share a cell on a short track, so the round trip is
            // through the *thumb* and not through the row: put it back and it
            // lands on the same cell.
            let mut back = v;
            back.scroll_to(v.top_at(cell, 25));
            assert_eq!(
                back.thumb(25).unwrap().start,
                cell,
                "the thumb moved at top {top}"
            );
        }
    }

    #[test]
    fn a_thumb_dragged_past_either_end_clamps_rather_than_wrapping() {
        let v = view(1000, 25);
        assert_eq!(v.top_at(0, 25), 0);
        assert_eq!(v.top_at(9999, 25), 975, "past the end of the track");
    }

    #[test]
    fn scrolling_to_a_row_is_the_same_as_scrolling_by_the_difference() {
        let mut a = view(100, 20);
        let mut b = a;
        a.scroll_by(7);
        b.scroll_to(7);
        assert_eq!(a, b);
        a.scroll_to(9999);
        assert_eq!(a.top(), 80, "past the end of the list");
    }

    #[test]
    fn a_viewport_with_no_height_yet_still_answers() {
        let mut v = Viewport::new();
        v.set_len(100);
        v.go_to(40);
        assert_eq!((v.cursor(), v.top()), (40, 40));
        v.set_height(20);
        assert_eq!(v.top(), 37, "the first size drags the view onto the cursor");
    }
}
