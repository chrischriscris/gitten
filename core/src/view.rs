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
//! only as far as it must. That is the wheel, and a wheel that moved the
//! selection would be a mouse editing your place in the file. What it must not
//! do is leave the cursor off screen: everything above still anchors to it, so a
//! resize would yank the view back to a row scrolled past minutes ago. So the
//! cursor is pushed to the near edge and stops there — vim's behaviour, arrived
//! at from the same constraint.

/// Rows of context kept between the cursor and the edge when it moves.
///
/// Three, because a cursor pinned to the last row gives you no idea what you are
/// scrolling into — the same reason `scrolloff` exists in every editor. A field
/// rather than a constant so `plait.toml` can hold it, and so a view with three
/// rows of its own can say zero.
pub const SCROLLOFF: usize = 3;

/// How a view scrolls. `[view]` in `plait.toml`, and the same two numbers in
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
    /// which reads as a page. One row an event makes plait scroll at exactly the
    /// speed the terminal's own scrollback does, which is the number the hand
    /// already knows. A window gets pixel deltas and does its own arithmetic;
    /// this is the multiplier it applies afterwards.
    pub rows: usize,
    /// Rows of lead kept between the cursor and the edge. See [`SCROLLOFF`].
    pub scrolloff: usize,
}

impl Default for Scrolling {
    fn default() -> Self {
        Self { rows: 1, scrolloff: SCROLLOFF }
    }
}

/// A scroll position, a cursor, and the rule relating them.
///
/// Holds no rows: `len` is all it knows about them, because a viewport over a
/// commit list and one over a wrapped diff differ in nothing else. Every method
/// leaves both positions valid — clamped into the list and into each other — so
/// there is no order to call them in and no state to repair afterwards.
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
        Self { len: 0, height: 0, top: 0, cursor: 0, scrolloff: SCROLLOFF }
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
    /// The cursor comes along only when it would otherwise leave the screen, and
    /// stops at the same margin [`follow`](Self::follow) keeps — except at the
    /// ends of the list, where there is nothing beyond the edge to preview and
    /// the margin would just make the first and last rows unreachable.
    pub fn scroll_by(&mut self, by: isize) {
        let max_top = self.max_top();
        self.top = match by.is_negative() {
            true => self.top.saturating_sub(by.unsigned_abs()),
            false => (self.top + by as usize).min(max_top),
        };
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
        // `low` first, so a viewport shorter than twice the margin cannot invert
        // the two and panic in `clamp`.
        self.cursor = self.cursor.clamp(low.min(high), high.max(low)).min(last);
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
        assert_eq!(v.top(), 0, "a margin here would scroll on the first keypress");
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
        // Far enough that the cursor cannot stay: it lands on the margin.
        v.scroll_by(30);
        assert_eq!(v.top(), 64);
        assert_eq!(v.cursor(), 67, "the top row plus the margin");
        v.scroll_by(-60);
        assert_eq!(v.top(), 4);
        assert_eq!(v.cursor(), 20, "the bottom row less the margin");
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
        // Scrolling into the end it is already at moves nothing: a margin here
        // would drag the cursor *off* the last row for no reason.
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
    fn a_viewport_with_no_height_yet_still_answers() {
        let mut v = Viewport::new();
        v.set_len(100);
        v.go_to(40);
        assert_eq!((v.cursor(), v.top()), (40, 40));
        v.set_height(20);
        assert_eq!(v.top(), 37, "the first size drags the view onto the cursor");
    }
}
