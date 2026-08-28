//! A column of cells that says where you are in a list.
//!
//! Two halves, and the split is the same one everything else in this app has:
//! **where the thumb goes is [`gitten_core::view::Viewport::thumb`]** — arithmetic
//! about a list, shared with every other door — and what it is *made of* is here,
//! because a glyph is a UI.
//!
//! # It rides the container's edge
//!
//! The caller hands [`paint`] the column, and the column is the pane's right
//! boundary — the divider the layout owns for a pane that has one, the
//! screen's edge for one that runs to it. That is where a scrollbar belongs:
//! on lazygit's containers the bar *is* the right border with a brighter
//! segment on it, and a bar one column in from the edge reads as a second,
//! brighter rule floating inside the pane.
//!
//! The boundary column is nearly free. A divider is a column the layout
//! already owns and no pane writes, so the bar costs no text and no reflow —
//! which retires the old trade: the bar used to sit *on* the pane's last
//! column of text, because reserving a column is a different wrap, a
//! different row count, a different scrollbar. The one pane still paying it
//! is the main region, whose right boundary is the screen's edge, with no
//! divider to ride — and with wrapping on nothing reaches that column anyway,
//! and with wrapping off the line is being scrolled sideways underneath it.
//!
//! [`Screen::over`] and not a [`Pen`](crate::screen::Pen), still: whatever the
//! boundary cell holds, the cell's background stays — a removal's red runs to
//! the edge underneath the main pane's bar.
//!
//! # It is an indicator, and the mouse cannot have it
//!
//! The bar was draggable once, and the drag is why it stopped. A thumb's travel
//! is one viewport's worth of cells and the list's is everything else — a
//! seven-thousand-commit log in a forty-row pane moves a hundred and eighty rows
//! for every cell of drag, and a 714k-line diff moves thousands. The window
//! carries the same ratio and wins on pointer resolution alone: twenty-five
//! positions to the cell means its thumb can be aimed. A terminal's cannot, and
//! a control that cannot be aimed is not a control. So the bar says where you
//! are and takes nothing: precision is the keyboard's (`j`, `ctrl-d`, `/`),
//! scrolling is the wheel's, and the bar's column is no pane's business — a
//! press there is a press in no pane at all. The window keeps its
//! draggable bar — a difference between the doors, not a gap in this one.
//!
//! # Half rows
//!
//! A terminal moves a thumb in whole cells, but a cell is not the finest line
//! the grid can draw: `╻` and `╹` each paint half of a vertical stroke, so the
//! thumb is computed by the same [`Viewport::thumb`] arithmetic over a track
//! twice as tall and lands between two rows where that is where it belongs.
//! The bar is a coordinate, and twice the resolution is twice the reading.
//! And the floor holds: a thumb never travels less than a cell, so the finer
//! position never becomes a smaller thumb.
//!
//! # Nothing when there is nothing to say
//!
//! A list shorter than its viewport has no bar at all, because a full-length
//! thumb is furniture that tells you what an empty column already did.
//! `[view] scrollbar = false` turns it off entirely.

use crate::screen::Screen;
use gitten_core::host::Host;
use gitten_core::view::Viewport;

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
    /// The thumb's upper and lower half, when the alphabet can draw them.
    ///
    /// `Some(('╻', '╹'))` lets the thumb land between two rows, which is
    /// where it usually is — see *Half rows* above. `None` rounds back to
    /// whole rows: [`Bar::ascii`], or an alphabet without the glyphs, draws
    /// exactly the bar this module always drew.
    pub halves: Option<(char, char)>,
}

impl Default for Bar {
    fn default() -> Self {
        Self::line()
    }
}

impl Bar {
    /// The shipped set: a light line for the track, a heavy one for the thumb.
    ///
    /// The thumb is a segment of the same stroke the track draws — brighter,
    /// heavier, never a different *kind* of mark. A full block was the
    /// shipped thumb once, and beside a hairline track it read as a blob the
    /// line ran into: two widths touching is a junction, and a scrollbar is
    /// not a junction. The old objection to `┃` — that a line-width thumb
    /// reads as another lane of the graph — was written when the bar floated
    /// inside the pane beside the graph; on the boundary column no lane ever
    /// arrives, and the reference the eye already knows, lazygit's bar, is a
    /// brighter segment of the border line.
    pub fn line() -> Self {
        Self {
            track: '│',
            thumb: '┃',
            halves: Some(('╻', '╹')),
        }
    }

    /// Nothing outside ASCII, for a terminal or a font that cannot draw the rest.
    pub fn ascii() -> Self {
        Self {
            track: '|',
            thumb: '#',
            halves: None,
        }
    }

    /// The thumb at this bar's resolution, or `None` when no bar is drawn —
    /// the config flag says no, or the list fits its viewport.
    ///
    /// Whole rows for an alphabet without half glyphs; twice the track — the
    /// same [`Viewport::thumb`] arithmetic, twice the resolution — for one
    /// with them.
    fn range(&self, view: &Viewport, host: &Host) -> Option<std::ops::Range<usize>> {
        if !host.view.scrollbar {
            return None;
        }
        let track = match self.halves {
            Some(_) => view.height() * 2,
            None => view.height(),
        };
        let mut thumb = view.thumb(track)?;
        // A thumb the share rounds below one cell of ink — any list more than
        // twice its viewport — would draw as half a block, where the row bar
        // never drew less than a whole one. Widen to a cell: the end first,
        // then the start where the end ran out of track, which is what keeps
        // the bottom case touching the bottom. The halves change where the
        // thumb *moves*, never how much of it there is.
        if self.halves.is_some() && thumb.len() < 2 {
            let grow_end = (2 - thumb.len()).min(track - thumb.end);
            thumb = thumb.start.saturating_sub(2 - thumb.len() - grow_end)..thumb.end + grow_end;
        }
        Some(thumb)
    }
}

/// Draws the bar for `view` down column `x`, over the rows `y..y + height`.
///
/// `x` is the pane's right boundary — the divider, or the screen's edge — and
/// is the caller's to choose; see *It rides the container's edge* above. A
/// no-op when the config file says no, when the list fits, or when the view
/// has not been given a height yet. Costs one `over` per visible row and
/// allocates nothing.
pub fn paint(screen: &mut Screen, bar: Bar, x: usize, y: usize, view: &Viewport, host: &Host) {
    let Some(thumb) = bar.range(view, host) else {
        return;
    };
    let c = &host.theme.chrome;
    // The half glyphs, when the alphabet has them. Substituting the thumb for
    // them when it does not is dead code by construction: without halves the
    // two reads per cell below are the same call, so a cell arrives all-thumb
    // or all-track and never half of each.
    let (upper, lower) = bar.halves.unwrap_or((bar.thumb, bar.thumb));
    for cell in 0..view.height() {
        // Which halves of this cell the thumb covers: off the doubled track
        // when there are half glyphs, off the row itself when there are not.
        let (top, bottom) = match bar.halves {
            Some(_) => (thumb.contains(&(cell * 2)), thumb.contains(&(cell * 2 + 1))),
            None => (thumb.contains(&cell), thumb.contains(&cell)),
        };
        let (ch, fg) = match (top, bottom) {
            (false, false) => (bar.track, c.faint),
            (true, true) => (bar.thumb, c.dim),
            (true, false) => (upper, c.dim),
            (false, true) => (lower, c.dim),
        };
        screen.over(x, y + cell, ch, fg);
    }
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
        paint(&mut screen, Bar::line(), 19, 1, &v, &host);
        assert_eq!(screen.char_at(19, 1), Some('┃'), "the thumb is at the top");
        assert_eq!(screen.char_at(19, 9), Some('│'));
        assert_eq!(
            screen.ink(19, 9).unwrap().bg,
            0x330000,
            "it repainted the row"
        );
    }

    #[test]
    fn a_list_that_fits_draws_nothing() {
        let host = Host::new();
        let mut screen = Screen::new(20, 12);
        screen.clear(Ink::new(0xffffff, 0x000000));
        let v = view(5, 10);
        paint(&mut screen, Bar::line(), 19, 1, &v, &host);
        assert_eq!(screen.char_at(19, 1), Some(' '));
    }

    #[test]
    fn turning_it_off_turns_off_the_paint() {
        let mut host = Host::new();
        host.view.scrollbar = false;
        let mut screen = Screen::new(20, 12);
        screen.clear(Ink::new(0xffffff, 0x000000));
        paint(&mut screen, Bar::line(), 19, 1, &view(100, 10), &host);
        assert_eq!(screen.char_at(19, 1), Some(' '));
    }

    #[test]
    fn a_thumb_that_falls_between_two_rows_is_drawn_as_halves_of_each() {
        let host = Host::new();
        // A hundred rows in ten: the half-resolution thumb over `top` 45 lands
        // on half units 9..11 — the bottom half of cell 4 and the top half of
        // cell 5 — where the row-resolution thumb rounds to cell 5 whole.
        let mut v = view(100, 10);
        v.scroll_to(45);
        let mut screen = Screen::new(20, 12);
        screen.clear(Ink::new(0xffffff, 0x000000));
        paint(&mut screen, Bar::line(), 19, 1, &v, &host);
        assert_eq!(
            screen.char_at(19, 3),
            Some('│'),
            "the track above the thumb"
        );
        assert_eq!(
            screen.char_at(19, 5),
            Some('╹'),
            "the thumb's first half row is the bottom of cell 4"
        );
        assert_eq!(
            screen.char_at(19, 6),
            Some('╻'),
            "the thumb's last half row is the top of cell 5"
        );
        assert_eq!(
            screen.char_at(19, 7),
            Some('│'),
            "the track below the thumb"
        );
    }

    #[test]
    fn an_alphabet_without_halves_draws_whole_rows() {
        let host = Host::new();
        let mut v = view(100, 10);
        v.scroll_to(45);
        let mut screen = Screen::new(20, 12);
        screen.clear(Ink::new(0xffffff, 0x000000));
        paint(&mut screen, Bar::ascii(), 19, 1, &v, &host);
        assert_eq!(screen.char_at(19, 6), Some('#'), "cell 5 whole, as always");
        for y in 1..11 {
            let ch = screen.char_at(19, y).unwrap();
            assert!(
                ch == '#' || ch == '|',
                "no half of a cell is ever drawn: {ch}"
            );
        }
    }

    #[test]
    fn the_half_thumb_touches_the_bottom_exactly_when_the_list_does() {
        // The load-bearing property, at the doubled track too: scrolled to the
        // end, the thumb sits on the last row whole — not a half row short of
        // it, which would read as "there is more" when there is not.
        let host = Host::new();
        let mut v = view(100, 10);
        v.scroll_to(90);
        let mut screen = Screen::new(20, 12);
        screen.clear(Ink::new(0xffffff, 0x000000));
        paint(&mut screen, Bar::line(), 19, 1, &v, &host);
        assert_eq!(screen.char_at(19, 10), Some('┃'), "the last row whole");
        assert_eq!(screen.char_at(19, 9), Some('│'), "track above it");
    }
}
