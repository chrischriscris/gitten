//! A diff drawn as two columns: the old file on the left, the new on the right.
//!
//! A [`Rows`] implementation and nothing more, which is the interesting part —
//! side-by-side sounds like a different view and is a different *presentation*.
//! It claims every path, it takes the same prepared lines the built-in takes,
//! and it draws them in two columns instead of one. No new trait, no new
//! argument, no edit to [`TextRows`](super::diff::TextRows).
//!
//! # Which line sits opposite which
//!
//! Not this file's decision. `plait_core::align` makes it, because
//! `replace_pairs` already makes the same one for the intraline pass and the two
//! answers have to agree — a row showing a removal beside an addition whose
//! changed words were computed against a *different* line highlights fragments
//! that correspond to nothing on screen. See that module for the rule.
//!
//! # One column width for the whole diff
//!
//! One width, whatever it is, so the divider is one straight vertical line from
//! the first row to the last. Per-file widths move the divider as you scroll, and
//! a boundary that drifts is worse than one that is too far right.
//!
//! Which width depends on the wrap. **Wrapping on**, it is half the measured
//! viewport less the gutters: everything fits, and there is nothing left to
//! scroll to horizontally — a line too wide for a column continues on the row
//! below, and a pair row is as tall as its taller side. **Wrapping off**, it is
//! the widest line anywhere in the diff, wider than the window, and the
//! horizontal scrollbar is how a 2000-character minified line is reached.
//!
//! Clipping the column to the window is what neither of those does, because it
//! would lose text unified mode shows.
//!
//! # Row count is not the same as unified's
//!
//! This is the one presentation that legitimately changes it — a removal and the
//! addition that replaced it share a row, so a hunk of N removals and N
//! additions is N rows here and 2N there. That is the whole point, and it is why
//! `docs/extending.md`'s "row count is not yours to change" applies to a
//! presentation of the *same* column and not to a second column.
//!
//! # Cost
//!
//! The same as the built-in per frame: one `StyledText` and one run list per
//! visible *cell*, so two per row rather than one, through the same `runs`
//! merge. Rows hold indices into one flat line table, so a context line — which
//! appears in both columns — is stored once, and so are its wrap points however
//! many rows it takes.

use super::diff::{
    column_at, columns, file_header, header_hit, hunk_header, line_colors, number,
    number_or_blank, runs, selected, slice, Hit, Rows, ROW_H,
};
use gpui::*;
use plait_core::align::align;
use plait_core::host::Host;
use plait_core::select::Selected;
use plait_core::syntax::Token;
use plait_core::theme::Theme;
use plait_core::wrap::{Wrap, Wrapped};
use plait_core::{LineKind, Span};

/// Which side of the divider a cell is being drawn on, and therefore which of
/// the line's two numbers its gutter shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Column {
    Old,
    New,
}

impl Column {
    /// Which of the row's two texts this is, for a selection. The old side is 0
    /// because a row with only one side has that side — see `selectable`.
    fn part(self) -> u16 {
        match self {
            Column::Old => 0,
            Column::New => 1,
        }
    }
}

/// Width of one column's line-number gutter. Narrower than the unified view's,
/// because there is one of them per column rather than two side by side, and the
/// two together may not cost more than the text they are labelling.
const GUTTER_W: f32 = 44.0;
/// The `+`/`-` column. Redundant with the background and kept anyway: colour is
/// not the only channel, and the eye finds a column of signs faster than it
/// distinguishes two dark backgrounds.
const SIGN_W: f32 = 14.0;
/// The rule between the columns.
const RULE_W: f32 = 1.0;
/// Slack on the measured column width, in characters.
///
/// `font.advance` is an approximation by construction — it is a fraction of the
/// point size, not a measured glyph — so a line at exactly the widest measured
/// width can come out a pixel or two over and clip its last character.
const SLACK: f32 = 2.0;

/// Everything drawn besides the two columns of text: a gutter and a sign column
/// on each side, and the rule between them. Half of what is left is one column,
/// which is what makes a wrapped line here break at half the width it would in
/// the unified presentation.
const CHROME: f32 = 2.0 * (GUTTER_W + SIGN_W) + RULE_W;

/// One prepared line, ready to draw. Held in a flat table so that a context
/// line, which appears in both columns, is stored once.
///
/// Both numbers, because a context line carries both and which one is shown
/// depends on the column it is being drawn in — the left says where the line was
/// and the right says where it is now, and after an insertion those differ.
struct Line {
    kind: LineKind,
    moved: bool,
    old_no: SharedString,
    new_no: SharedString,
    text: SharedString,
    spans: Vec<Span>,
    tokens: Vec<Token>,
}

/// `SharedString` throughout, not `String`: `render` runs for every visible row
/// on every frame that redraws, and handing GPUI a `String` there copies the
/// line each time.
enum Row {
    File { path: SharedString, adds: usize, dels: usize },
    Hunk(SharedString),
    /// Indices into [`SplitRows::lines`]. A context row points both columns at
    /// the same line; a lone removal or addition leaves one side `None`.
    Pair { old: Option<u32>, new: Option<u32> },
}

/// The two-column presentation. Register it as `renderers[0]` and it takes the
/// whole diff; register it after the built-in and it takes whatever the built-in
/// would have.
#[derive(Default)]
pub struct SplitRows {
    rows: Vec<Row>,
    lines: Vec<Line>,
    /// Widest line in the diff, in characters — the width both columns get.
    widest_chars: usize,
    /// How many rows carry a pair rather than one side, for the stats overlay.
    /// A diff whose rows are almost all pairs is one this presentation suits.
    paired: usize,
    /// Lines belonging to a block that moved — see the note on `TextRows`.
    moved: usize,
    /// Where each *line* breaks — indexed by line and not by row, because a pair
    /// row draws two of them and a context line is drawn by two columns.
    wrapped: Wrapped,
    /// The budget one column got, the policy it was built with, and whether that
    /// policy breaks anything. The last one is what stops a column being clipped
    /// to the window when wrapping is off, where the horizontal scrollbar is
    /// still how you reach a 2000-character line.
    cols: usize,
    wrap: &'static str,
    wraps: bool,
}

impl Rows for SplitRows {
    /// Everything. This is a presentation of the whole diff, not of one kind of
    /// file, so as `renderers[0]` it is a complete replacement for the built-in
    /// fallback rather than a specialist beside it.
    fn claims(&self, _path: &str) -> bool {
        true
    }

    fn len(&self) -> usize {
        self.rows.len()
    }

    /// A pair row is as tall as its taller side. The shorter one runs out of
    /// text partway down and draws `absent_bg` for the rest, which is the same
    /// thing it already draws opposite a lone addition.
    fn rows(&self, index: usize) -> usize {
        match &self.rows[index] {
            Row::Pair { old, new } => {
                let of = |i: &Option<u32>| i.map_or(1, |i| self.wrapped.rows(i as usize));
                of(old).max(of(new))
            }
            _ => 1,
        }
    }

    fn reflow(&mut self, width: f32, host: &Host, wrap: &dyn Wrap) -> bool {
        // Half of what is left over, less the slack the column already carries,
        // because a column is drawn `SLACK` characters wider than its content.
        let f = &host.font;
        let cols =
            columns((width - CHROME) / 2.0, SLACK * f.size * f.advance, f.size, host);
        if cols == self.cols && wrap.name() == self.wrap {
            return false;
        }
        self.cols = cols;
        self.wrap = wrap.name();
        self.wraps = wrap.breaks_lines();
        self.wrapped =
            Wrapped::build(self.lines.iter().map(|l| (l.text.as_ref(), cols)), wrap);
        true
    }

    fn build(&mut self, f: plait_core::prepared::File) {
        self.rows.push(Row::File { path: f.path.into(), adds: f.adds, dels: f.dels });
        for h in f.hunks {
            self.rows.push(Row::Hunk(h.header.into()));

            // The alignment is computed from the kinds alone, before the lines
            // are moved into the table, because `align` returns indices into
            // this hunk and consuming the lines first would lose them.
            let kinds: Vec<LineKind> = h.lines.iter().map(|l| l.kind).collect();
            let slots = align(&kinds);

            let base = self.lines.len() as u32;
            for l in h.lines {
                // `chars`, not `len`: a line of box drawing would otherwise
                // measure three times too wide and set the column for the whole
                // diff.
                self.widest_chars = self.widest_chars.max(l.text.chars().count());
                self.moved += l.moved as usize;
                self.lines.push(Line {
                    kind: l.kind,
                    moved: l.moved,
                    old_no: number(l.old_no),
                    new_no: number(l.new_no),
                    text: l.text.into(),
                    spans: l.spans,
                    tokens: l.tokens,
                });
            }

            for slot in slots {
                let (old, new) = (slot.old(), slot.new());
                if old.is_some() && new.is_some() {
                    self.paired += 1;
                }
                self.rows.push(Row::Pair {
                    old: old.map(|i| base + i),
                    new: new.map(|i| base + i),
                });
            }
        }
    }

    /// Every pair row is the same width, because both columns are: whichever row
    /// `uniform_list` picks to measure gives the same answer, which is one fewer
    /// thing to get wrong than the unified view's widest-row search.
    fn width(&self, index: usize, _seg: usize) -> usize {
        match &self.rows[index] {
            Row::Pair { .. } => 2 * (self.col_chars() + 8),
            Row::Hunk(h) => h.chars().count(),
            Row::File { path, .. } => path.chars().count(),
        }
    }

    fn report(&self) -> String {
        if self.rows.is_empty() {
            return String::new();
        }
        let mut out = format!("split {} paired · {} cols", self.paired, self.col_chars());
        if self.moved > 0 {
            out.push_str(&format!(" · {} moved", self.moved));
        }
        if self.wrapped.rejected() > 0 {
            out.push_str(&format!(" · {} invalid breaks from {}", self.wrapped.rejected(), self.wrap));
        }
        out
    }

    /// Which column the click was in, then where in that column's text — the
    /// divider is what makes this two answers rather than one, and the reason a
    /// [`Hit`] carries a part at all.
    fn hit(&self, index: usize, seg: usize, x: f32, host: &Host) -> Option<Hit> {
        match self.rows.get(index)? {
            Row::File { path, .. } => Some(header_hit(path, x, host)),
            Row::Hunk(h) => Some(header_hit(h, x, host)),
            Row::Pair { old, new } => {
                let cell = GUTTER_W + SIGN_W + self.col_px(host);
                let (part, from) = match x < cell + RULE_W {
                    true => (Column::Old, 0.0),
                    false => (Column::New, cell + RULE_W),
                };
                let line = match part {
                    Column::Old => *old,
                    Column::New => *new,
                };
                // No line on this side at all: a lone addition opposite a hole.
                // The caret still lands *on* the row, at nothing — which keeps a
                // drag through the hole extending instead of freezing, and copies
                // nothing for it because `selectable` has nothing to give.
                let Some(line) = line else {
                    return Some(Hit { part: part.part(), off: 0 });
                };
                let text = &self.lines[line as usize].text;
                let at = self.wrapped.range(line as usize, seg, text);
                let off = match seg < self.wrapped.rows(line as usize) {
                    true => {
                        at.start
                            + column_at(
                                &text[at.clone()],
                                x - from - GUTTER_W - SIGN_W,
                                host.font.size,
                                host,
                            )
                    }
                    // This side wrapped less far than the other one. The end of
                    // its line is the honest place for the caret.
                    false => text.len(),
                };
                Some(Hit { part: part.part(), off })
            }
        }
    }

    /// A pair row has two texts and a header has one, which it lends to both
    /// parts: a file path is neither the old file's nor the new one's.
    fn selectable(&self, index: usize, part: u16) -> Option<&str> {
        match self.rows.get(index)? {
            Row::File { path, .. } => Some(path.as_ref()),
            Row::Hunk(h) => Some(h.as_ref()),
            Row::Pair { old, new } => {
                let line = match part {
                    0 => *old,
                    _ => *new,
                }?;
                Some(self.lines[line as usize].text.as_ref())
            }
        }
    }

    fn render(&self, index: usize, seg: usize, host: &Host, sel: Option<Selected>) -> AnyElement {
        let theme = &host.theme;
        match &self.rows[index] {
            Row::File { path, adds, dels } => file_header(path, *adds, *dels, theme, sel),
            Row::Hunk(header) => hunk_header(header, theme, sel),
            Row::Pair { old, new } => {
                // The column width in pixels, from the host's font rather than a
                // constant: the face is configurable and hot-reloaded, and a
                // stale character width is exactly what `font.advance` exists to
                // stop being possible.
                let col = px(self.col_px(host));
                div()
                    .flex()
                    .items_center()
                    .h(px(ROW_H))
                    .child(self.cell(*old, seg, Column::Old, col, theme, sel))
                    .child(
                        div()
                            .flex_none()
                            .w(px(RULE_W))
                            .h(px(ROW_H))
                            .bg(rgb(theme.diff.gutter_fg)),
                    )
                    .child(self.cell(*new, seg, Column::New, col, theme, sel))
                    .into_any_element()
            }
        }
    }
}

impl SplitRows {
    /// How wide one column is, in characters.
    ///
    /// The widest line in the diff, or the wrap budget, whichever is narrower —
    /// so with wrapping on the two columns and the rule between them fit the
    /// window exactly and there is nothing left to scroll to. With wrapping
    /// *off* the budget is not a limit at all: a 2000-character line still has
    /// to be reachable, and the horizontal scrollbar is how.
    fn col_chars(&self) -> usize {
        match self.wraps && self.cols > 0 {
            true => self.widest_chars.min(self.cols),
            false => self.widest_chars,
        }
    }

    /// One column of one row: gutter, sign, text — the built-in's anatomy at
    /// half the width, so the eye finds the same things in the same order.
    /// How wide one column of text is, in pixels.
    ///
    /// From the host's font rather than a constant: the face is configurable and
    /// hot-reloaded, and a stale character width is exactly what `font.advance`
    /// exists to stop being possible. Shared by the drawing and the hit test so
    /// the divider is in the same place in both — the click that lands on the
    /// wrong side of it is the whole bug this one function prevents.
    fn col_px(&self, host: &Host) -> f32 {
        (self.col_chars() as f32 + SLACK) * host.font.advance * host.font.size
    }

    #[allow(clippy::too_many_arguments)]
    fn cell(
        &self,
        line: Option<u32>,
        seg: usize,
        column: Column,
        col: Pixels,
        theme: &Theme,
        sel: Option<Selected>,
    ) -> AnyElement {
        let p = &theme.diff;
        // Past the end of *this* side of a pair whose other side wrapped
        // further. The same hole as no line at all, and the same colour: the
        // alternative is a bare row of `context_bg` under a wrapped removal,
        // which reads as an unchanged line that is not there.
        let Some(index) = line.filter(|i| seg < self.wrapped.rows(*i as usize)) else {
            // Nothing opposite: a flat, darker block, so a run of them reads as
            // a hole in the column rather than as unchanged content.
            return div()
                .flex_none()
                .w(px(GUTTER_W + SIGN_W) + col)
                .h(px(ROW_H))
                .bg(rgb(p.absent_bg))
                .into_any_element();
        };
        let line = &self.lines[index as usize];
        let (bg, fg, sign) = line_colors(line.kind, line.moved, p);
        let no = match column {
            Column::Old => &line.old_no,
            Column::New => &line.new_no,
        };
        let at = self.wrapped.range(index as usize, seg, &line.text);
        let blank = seg > 0;
        div()
            .flex()
            .flex_none()
            .items_center()
            .h(px(ROW_H))
            .w(px(GUTTER_W + SIGN_W) + col)
            .bg(rgb(bg))
            .child(
                div()
                    .flex_none()
                    .w(px(GUTTER_W))
                    .pl_2()
                    .text_color(rgb(p.gutter_fg))
                    .child(number_or_blank(no, blank)),
            )
            .child(
                div()
                    .flex_none()
                    .w(px(SIGN_W))
                    .text_color(rgb(fg))
                    .child(if blank { " " } else { sign }),
            )
            .child(
                div().flex_none().text_color(rgb(fg)).child(
                    StyledText::new(slice(&line.text, &at)).with_highlights(runs(
                        at.clone(),
                        &line.tokens,
                        &line.spans,
                        theme,
                        line.kind,
                        line.moved,
                        selected(sel, column.part(), line.text.len()),
                    )),
                ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    // By name, not a glob: `use gpui::*` in the parent shadows `#[test]` with
    // GPUI's own attribute macro and every test in here fails to expand.
    use super::{Column, Row, Rows, SplitRows, GUTTER_W, RULE_W, SIGN_W};
    use plait_core::host::Host;
    use plait_core::prepared::prepare;
    use plait_core::parse_unified_diff;

    const SAMPLE: &str = "\
diff --git a/a.rs b/a.rs
@@ -1,5 +1,5 @@
 fn main() {
-    let x = 1;
-    let y = 2;
+    let x = 11;
+    let y = 22;
+    let z = 33;
 }
";

    fn built() -> SplitRows {
        let host = Host::new();
        let mut p = prepare(&parse_unified_diff(SAMPLE), &host.syntax, 2000);
        let mut r = SplitRows::default();
        r.build(p.files.remove(0));
        r
    }

    #[test]
    fn a_replaced_pair_shares_a_row_and_the_leftover_stands_alone() {
        let r = built();
        // file header, hunk header, then: context, two pairs, one lone
        // addition, context.
        assert_eq!(r.len(), 2 + 5);
        let pairs: Vec<(bool, bool)> = r
            .rows
            .iter()
            .filter_map(|row| match row {
                Row::Pair { old, new, .. } => Some((old.is_some(), new.is_some())),
                _ => None,
            })
            .collect();
        assert_eq!(
            pairs,
            vec![(true, true), (true, true), (true, true), (false, true), (true, true)]
        );
        assert_eq!(r.paired, 4, "two context rows and two replace pairs");
    }

    #[test]
    fn a_context_line_is_stored_once_and_shown_twice() {
        let r = built();
        // Seven diff lines, seven table entries — a context row appears in both
        // columns and is still stored once. Five rows hold those seven lines.
        assert_eq!(r.lines.len(), 7);
        assert_eq!(r.rows.iter().filter(|row| matches!(row, Row::Pair { .. })).count(), 5);
        let context = r
            .rows
            .iter()
            .find_map(|row| match row {
                Row::Pair { old: Some(o), new: Some(n), .. } if o == n => Some(*o),
                _ => None,
            })
            .expect("a context row points both columns at one line");
        assert_eq!(r.lines[context as usize].text.as_ref(), "fn main() {");
    }

    #[test]
    fn each_column_shows_its_own_line_number() {
        // The trap: after an insertion the two files disagree about what line
        // this is, so the left gutter must say where the line *was* and the
        // right where it *is*. One number on the row cannot express that.
        let r = built();
        let numbers: Vec<(String, String)> = r
            .rows
            .iter()
            .filter_map(|row| match row {
                Row::Pair { old, new } => Some((
                    old.map(|i| r.lines[i as usize].old_no.to_string()).unwrap_or_default(),
                    new.map(|i| r.lines[i as usize].new_no.to_string()).unwrap_or_default(),
                )),
                _ => None,
            })
            .collect();
        assert_eq!(numbers[3], ("".to_string(), "4".to_string()), "a lone addition");
        assert_eq!(numbers[4], ("4".to_string(), "5".to_string()), "context after the shift");
    }

    #[test]
    fn every_pair_row_measures_the_same_width() {
        let r = built();
        let widths: Vec<usize> = (0..r.len())
            .filter(|i| matches!(r.rows[*i], Row::Pair { .. }))
            .map(|i| r.width(i, 0))
            .collect();
        assert!(widths.windows(2).all(|w| w[0] == w[1]), "{widths:?}");
        assert!(widths[0] > 0);
    }

    /// One long removal against one short addition, so a pair row has a taller
    /// side and a shorter one.
    const LOPSIDED: &str = "\
diff --git a/a.rs b/a.rs
@@ -1,1 +1,1 @@
-    let x = one(alpha) + two(beta) + three(gamma) + four(delta) + five(eps);
+    let x = 1;
";

    fn wrapped(src: &str, cols: usize) -> (SplitRows, std::rc::Rc<Host>) {
        let host = std::rc::Rc::new(Host::new());
        let mut p = prepare(&parse_unified_diff(src), &host.syntax, 2000);
        let mut r = SplitRows::default();
        r.build(p.files.remove(0));
        // Enough width for `cols` characters in *each* column, plus the chrome.
        let f = &host.font;
        let per = (cols as f32 + super::SLACK + 0.5) * f.size * f.advance;
        r.reflow(super::CHROME + 2.0 * per, &host, host.wrap.current());
        (r, host)
    }

    #[test]
    fn a_pair_row_is_as_tall_as_its_taller_side() {
        // The two columns wrap independently — a long removal beside a short
        // addition — and the row has to hold the longer of them.
        let (r, _) = wrapped(LOPSIDED, 20);
        let pair = (0..r.len()).find(|i| matches!(r.rows[*i], Row::Pair { .. })).unwrap();
        let Row::Pair { old, new } = r.rows[pair] else { unreachable!() };
        let (old, new) = (old.unwrap() as usize, new.unwrap() as usize);
        assert!(r.wrapped.rows(old) > 1, "the long side did not wrap");
        assert_eq!(r.wrapped.rows(new), 1, "the short side wrapped");
        assert_eq!(r.rows(pair), r.wrapped.rows(old));
    }

    #[test]
    fn a_column_narrows_to_the_window_when_wrapping_and_not_when_off() {
        // Wrapping on: both columns and the rule fit, so there is nothing left
        // to scroll to. Off: the column is the widest line in the diff, because
        // a 2000-character line still has to be reachable.
        let (r, host) = wrapped(LOPSIDED, 20);
        assert_eq!(r.col_chars(), 20);

        let mut off = r;
        let f = &host.font;
        let per = (20.0 + super::SLACK + 0.5) * f.size * f.advance;
        let wrap_off = host.wrap.at(host.wrap.position("off").unwrap());
        assert!(off.reflow(super::CHROME + 2.0 * per, &host, wrap_off));
        assert_eq!(off.col_chars(), off.widest_chars);
        assert!(off.widest_chars > 20);
    }

    #[test]
    fn it_claims_every_path_because_it_replaces_the_fallback() {
        let r = SplitRows::default();
        for p in ["a.rs", "b.md", "no-extension", "weird.xyz"] {
            assert!(r.claims(p));
        }
    }

    // ------------------------------------------------------------ selection

    #[test]
    fn the_divider_decides_which_column_a_click_is_in() {
        // The one bug this presentation can have that the others cannot: a click
        // a pixel the wrong side of the rule selects the file you were not
        // reading. `col_px` is shared with the drawing so it cannot drift.
        let host = Host::new();
        let mut r = built();
        r.reflow(900.0, &host, host.wrap.current());
        let pair = (0..r.len()).find(|i| matches!(r.rows[*i], Row::Pair { .. })).unwrap();
        let cell = GUTTER_W + SIGN_W + r.col_px(&host);

        let left = r.hit(pair, 0, GUTTER_W + SIGN_W + 2.0, &host).unwrap();
        assert_eq!(left.part, Column::Old.part());
        let right = r.hit(pair, 0, cell + RULE_W + GUTTER_W + SIGN_W + 2.0, &host).unwrap();
        assert_eq!(right.part, Column::New.part());
        // Either side of the rule itself, and nothing in between.
        assert_eq!(r.hit(pair, 0, cell - 1.0, &host).unwrap().part, 0);
        assert_eq!(r.hit(pair, 0, cell + RULE_W + 1.0, &host).unwrap().part, 1);
    }

    #[test]
    fn each_column_offers_its_own_text_and_a_hole_offers_none() {
        // What makes a drag down one column paste that file: the other side is
        // not a blank line, it is nothing at all.
        let host = Host::new();
        let mut r = built();
        r.reflow(900.0, &host, host.wrap.current());
        let pairs: Vec<usize> =
            (0..r.len()).filter(|i| matches!(r.rows[*i], Row::Pair { .. })).collect();
        let mut both = 0;
        let mut holes = 0;
        for i in pairs {
            match (r.selectable(i, 0), r.selectable(i, 1)) {
                (Some(_), Some(_)) => both += 1,
                (None, Some(_)) | (Some(_), None) => holes += 1,
                (None, None) => panic!("row {i} has no text on either side"),
            }
        }
        // The fixture removes two lines and adds three: two replace pairs, one
        // lone addition.
        assert_eq!((both, holes), (4, 1));
    }

    #[test]
    fn a_header_lends_its_text_to_both_columns() {
        // A path is neither the old file's nor the new one's, and a selection
        // that started in either column has to be able to include it.
        let r = built();
        assert_eq!(r.selectable(0, 0), Some("a.rs"));
        assert_eq!(r.selectable(0, 1), Some("a.rs"));
        assert_eq!(r.selectable(1, 0), r.selectable(1, 1));
    }
}
