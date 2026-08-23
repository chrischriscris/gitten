//! A column of cells that says where you are in a list, and can be dragged.
//!
//! Two halves, and the split is the same one everything else in this app has:
//! **where the thumb goes is [`plait_core::view::Viewport::thumb`]** — arithmetic
//! about a list, shared with every other door — and what it is *made of* is here,
//! because a glyph is a UI.
//!
//! # It is drawn over the rows, not beside them
//!
//! [`Screen::over`] and not a [`Pen`](crate::screen::Pen), so the row underneath
//! keeps its background: a removal's red still runs to the right edge, with the
//! bar on top of it. Reserving a column instead is the obvious alternative and it
//! costs a reflow — one column fewer is a different wrap, which is a different
//! row count, which is a different scrollbar. The window's overlays its list for
//! the same reason.
//!
//! The cost, stated plainly: the last column of text is covered on a list long
//! enough to scroll. With wrapping on nothing reaches it — the budget is the
//! window less the gutters — and with wrapping off the line is being scrolled
//! sideways anyway.
//!
//! # Nothing when there is nothing to say
//!
//! A list shorter than its viewport has no bar at all, because a full-length
//! thumb is furniture that tells you what an empty column already did.
//! `[view] scrollbar = false` turns it off entirely.

use crate::screen::Screen;
use plait_core::host::Host;
use plait_core::view::Viewport;

/// What a scrollbar is made of.
///
/// A struct and not two literals for the same reason [`Glyphs`](crate::commits::Glyphs)
/// is one: `--ascii` has to be able to replace the alphabet, and so does an
/// extension that would rather have a Nerd Font's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bar {
    /// The part of the track the thumb is not on.
    pub track: char,
    pub thumb: char,
}

impl Default for Bar {
    fn default() -> Self {
        Self::block()
    }
}

impl Bar {
    /// The shipped set: a hairline for the track and a full block for the thumb.
    ///
    /// A block and not `▐` or `┃`, because the thumb is the one thing here the
    /// eye has to find while the list moves under it, and a half-width glyph in a
    /// quiet palette reads as another line of the graph.
    pub fn block() -> Self {
        Self {
            track: '│',
            thumb: '█',
        }
    }

    /// Nothing outside ASCII, for a terminal or a font that cannot draw the rest.
    pub fn ascii() -> Self {
        Self {
            track: '|',
            thumb: '#',
        }
    }
}

/// Draws the bar for `view` down column `x`, over the rows `y..y + height`.
///
/// A no-op when the config file says no, when the list fits, or when the view has
/// not been given a height yet. Costs one `over` per visible row and allocates
/// nothing.
pub fn paint(screen: &mut Screen, bar: Bar, x: usize, y: usize, view: &Viewport, host: &Host) {
    let Some(thumb) = thumb(view, host) else {
        return;
    };
    let c = &host.theme.chrome;
    for i in 0..view.height() {
        let on = thumb.contains(&i);
        let (ch, fg) = match on {
            true => (bar.thumb, c.dim),
            false => (bar.track, c.faint),
        };
        screen.over(x, y + i, ch, fg);
    }
}

/// Which rows of the viewport the thumb covers, or `None` when no bar is drawn.
///
/// The one place the config flag is read, so "is there a bar here" is the same
/// question for the paint path and for the hit test — a bar that is invisible and
/// still takes the clicks is worse than either.
pub fn thumb(view: &Viewport, host: &Host) -> Option<std::ops::Range<usize>> {
    if !host.view.scrollbar {
        return None;
    }
    view.thumb(view.height())
}

/// Whether a click at `col` of a view `cols` wide landed on the bar.
pub fn hit(col: usize, cols: usize, view: &Viewport, host: &Host) -> bool {
    thumb(view, host).is_some() && cols > 0 && col + 1 == cols
}

/// What a press on the bar at row `row` grabs, and where it puts the list.
///
/// Two behaviours in one function because they are the same gesture: a press on
/// the thumb *grabs* it where it was taken hold of, and a press anywhere else on
/// the track jumps the thumb to the pointer first and then grabs it in the
/// middle. Returns the grab offset, which the drag then subtracts — without it a
/// thumb snaps its top to the pointer on the first pixel of every drag, which is
/// a scrollbar that jumps whenever it is used.
pub fn grab(view: &mut Viewport, host: &Host, row: usize) -> usize {
    let Some(thumb) = thumb(view, host) else {
        return 0;
    };
    if thumb.contains(&row) {
        return row - thumb.start;
    }
    let offset = thumb.len() / 2;
    drag(view, host, row, offset);
    offset
}

/// Moves the list so the grabbed point of the thumb follows `row`.
pub fn drag(view: &mut Viewport, host: &Host, row: usize, grabbed: usize) {
    if thumb(view, host).is_none() {
        return;
    }
    let track = view.height();
    let top = view.top_at(row.saturating_sub(grabbed), track);
    view.scroll_to(top);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screen::Ink;

    fn view(len: usize, height: usize) -> Viewport {
        let mut v = Viewport::new();
        v.set_len(len);
        v.set_height(height);
        v
    }

    #[test]
    fn the_bar_is_drawn_over_the_rows_and_keeps_their_colour() {
        let host = Host::new();
        let mut screen = Screen::new(20, 12);
        screen.clear(Ink::new(0xffffff, 0x000000));
        for y in 0..10 {
            screen.row(y + 1).wash(Ink::new(0xffffff, 0x330000));
        }
        let v = view(100, 10);
        paint(&mut screen, Bar::block(), 19, 1, &v, &host);
        assert_eq!(screen.char_at(19, 1), Some('█'), "the thumb is at the top");
        assert_eq!(screen.char_at(19, 9), Some('│'));
        assert_eq!(
            screen.ink(19, 9).unwrap().bg,
            0x330000,
            "it repainted the row"
        );
    }

    #[test]
    fn a_list_that_fits_draws_nothing_and_takes_no_clicks() {
        let host = Host::new();
        let mut screen = Screen::new(20, 12);
        screen.clear(Ink::new(0xffffff, 0x000000));
        let v = view(5, 10);
        paint(&mut screen, Bar::block(), 19, 1, &v, &host);
        assert_eq!(screen.char_at(19, 1), Some(' '));
        assert!(!hit(19, 20, &v, &host));
    }

    #[test]
    fn turning_it_off_turns_off_the_hit_test_too() {
        let mut host = Host::new();
        host.view.scrollbar = false;
        let v = view(100, 10);
        assert_eq!(thumb(&v, &host), None);
        assert!(!hit(19, 20, &v, &host));
    }

    #[test]
    fn grabbing_the_thumb_where_it_is_does_not_move_the_list() {
        let host = Host::new();
        let mut v = view(100, 20);
        v.scroll_to(40);
        let thumb = thumb(&v, &host).unwrap();
        assert!(
            thumb.len() > 1,
            "a one-cell thumb has nowhere to be grabbed"
        );
        let grabbed = grab(&mut v, &host, thumb.start + 1);
        assert_eq!(grabbed, 1);
        assert_eq!(v.top(), 40, "a press on the thumb scrolled the list");
        // ...and dragging it one cell down moves the list down, not by a screen.
        drag(&mut v, &host, thumb.start + 2, grabbed);
        assert!(v.top() > 40 && v.top() < 60, "{}", v.top());
    }

    #[test]
    fn a_press_on_the_track_jumps_the_thumb_under_the_pointer() {
        let host = Host::new();
        let mut v = view(1000, 20);
        let grabbed = grab(&mut v, &host, 19);
        assert_eq!(v.top(), 980, "the end of the track is the end of the list");
        assert_eq!(grabbed, 0, "a one-cell thumb is grabbed at its only cell");
        drag(&mut v, &host, 0, grabbed);
        assert_eq!(v.top(), 0);
    }
}
