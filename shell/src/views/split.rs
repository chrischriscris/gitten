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
//! Not this file's decision. `gitten_core::align` makes it, because
//! `replace_pairs` already makes the same one for the intraline pass and the two
//! answers have to agree — a row showing a removal beside an addition whose
//! changed words were computed against a *different* line highlights fragments
//! that correspond to nothing on screen. See that module for the rule.
//!
//! # One column width for the whole diff
//!
//! Half of what is left after the page padding and the rule between them,
//! whatever the diff holds and whatever the wrap is doing — so the divider is
//! one straight vertical line from the first row to the last and it is in the
//! same place in every diff. Per-file widths move the divider as you scroll, and
//! a boundary that drifts is worse than one that is too far right.
//!
//! **Wrapping on**, everything fits: a line too wide for a column continues on
//! the row below and a pair row is as tall as its taller side. **Wrapping off**,
//! a line runs past its column's edge and is clipped there — and reached by
//! scrolling, which moves the text of *both* columns and leaves the two gutters
//! and the divider where they are. That is the terminal's behaviour and the
//! reason a column is not sized to the widest line in the diff: a column wide
//! enough for a 2000-character minified line puts the new file off the right of
//! the screen for every row of every file, to make one line of one of them
//! reachable without scrolling.
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
    column_at, columns, file_header, header_hit, hunk_header, hunk_hit, into_text, line_colors,
    row_frame, scrolled, selected, slice, Hit, Rows, Scratch, PAD, ROW_H,
};
use gitten_core::align::align;
use gitten_core::host::Host;
use gitten_core::runs::surfaces;
use gitten_core::select::Selected;
use gitten_core::syntax::Token;
use gitten_core::theme::{Surface, Theme};
use gitten_core::wrap::{Wrap, Wrapped};
use gitten_core::{LineKind, Span};
use gpui::*;
use std::cell::RefCell;

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

    /// The name, for a debug selector.
    fn name(self) -> &'static str {
        match self {
            Column::Old => "old",
            Column::New => "new",
        }
    }
}

/// Width of one column's line-number gutter, including the air after the digits.
/// Narrower than the unified view's, because there is one of them per column
/// rather than two side by side, and the two together may not cost more than the
/// text they are labelling — but not so narrow that a five-digit line number in a
/// right-aligned column runs into the sign beside it, which 44 was.
const GUTTER_W: f32 = 52.0;
/// The air between the last digit and the sign column. The unified view's number,
/// because the two presentations are the same anatomy at two widths and a reader
/// switching with `s` should find the columns where they were.
const GUTTER_PAD: f32 = 8.0;
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

/// Everything drawn besides the two columns of text: the page padding at each
/// edge, a gutter and a sign column on each side, and the rule between them.
/// Half of what is left is one column, which is what makes a wrapped line here
/// break at half the width it would in the unified presentation.
///
/// The padding is the unified view's [`PAD`] and not zero, for the same reasons
/// that view has it: the headers this presentation shares are drawn at `PAD`,
/// and a gutter starting left of them reads as a mistake; and the vertical
/// scrollbar overlays the right edge, so an unwrapped right-hand column without
/// a right pad runs under it. The divider stays one straight line — every row
/// loses the same two strips.
const CHROME: f32 = 2.0 * PAD + 2.0 * (GUTTER_W + SIGN_W) + RULE_W;

/// One prepared line, ready to draw. Held in a flat table so that a context
/// line, which appears in both columns, is stored once.
///
/// Both numbers as the integers they are — formatted at draw time into the
/// presentation's scratch, like every presentation's gutter — because which one
/// is shown depends on the column it is being drawn in: the left says where the
/// line was and the right says where it is now, and after an insertion those
/// differ.
struct Line {
    kind: LineKind,
    moved: bool,
    old_no: Option<u32>,
    new_no: Option<u32>,
    /// The prepared line's own allocation, shared rather than copied.
    text: std::sync::Arc<str>,
    spans: Box<[Span]>,
    tokens: Box<[Token]>,
}

/// `SharedString` throughout, not `String`: `render` runs for every visible row
/// on every frame that redraws, and handing GPUI a `String` there copies the
/// line each time. The headers take the row's own `Arc<str>` handles for the
/// same reason [`Row::Line`](super::diff::Row) does — see that enum.
enum Row {
    File {
        path: std::sync::Arc<str>,
        adds: usize,
        dels: usize,
    },
    Hunk(std::sync::Arc<str>),
    /// Indices into [`SplitRows::lines`]. A context row points both columns at
    /// the same line; a lone removal or addition leaves one side `None`.
    Pair {
        old: Option<u32>,
        new: Option<u32>,
    },
}

/// The two-column presentation. Register it as `renderers[0]` and it takes the
/// whole diff; register it after the built-in and it takes whatever the built-in
/// would have.
#[derive(Default)]
pub struct SplitRows {
    rows: Vec<Row>,
    lines: Vec<Line>,
    /// Which hunk every row belongs to — see [`super::diff::HunkMap`]. One
    /// entry per hunk, recorded as the rows are built.
    hunks: super::diff::HunkMap,
    /// Widest line in the diff, in characters. Not a width any more — a column is
    /// half the window — but it is what says on the overlay how much of the diff
    /// is off the right of it.
    widest_chars: usize,
    /// How many rows carry a pair rather than one side, for the stats overlay.
    /// A diff whose rows are almost all pairs is one this presentation suits.
    paired: usize,
    /// Lines belonging to a block that moved — see the note on `TextRows`.
    moved: usize,
    /// Where each *line* breaks — indexed by line and not by row, because a pair
    /// row draws two of them and a context line is drawn by two columns.
    wrapped: Wrapped,
    /// The budget one column got and the policy it was built with.
    cols: usize,
    wrap: &'static str,
    /// The window, in pixels. What the columns are halves of, and therefore the
    /// one number the divider's position and a click's column both come from —
    /// a click that lands on the wrong side of the rule is the whole bug one
    /// field prevents.
    width: f32,
    /// What drawing borrows. Cleared per cell, grown once ever — see [`Scratch`].
    scratch: RefCell<Scratch>,
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

    fn is_file_header(&self, index: usize) -> bool {
        matches!(self.rows.get(index), Some(Row::File { .. }))
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
        let cols = columns(
            (width - CHROME) / 2.0,
            SLACK * f.size * f.advance,
            f.size,
            host,
        );
        // The width lands whatever else is true: it is what the divider and the
        // hit test are halves of, and a stale one puts the rule somewhere the
        // click does not agree with. No row count depends on it, which is why it
        // is not on its own a reason to rebuild the order table.
        self.width = width;
        if cols == self.cols && wrap.name() == self.wrap {
            return false;
        }
        self.cols = cols;
        self.wrap = wrap.name();
        // A wrap that breaks nothing needs no scan: the unbroken table is the
        // default one, and building it to be told so is a pass over every line
        // in the diff on every few characters of a resize drag.
        if !wrap.breaks_lines() {
            let broken = self.wrapped.total() > self.wrapped.lines();
            self.wrapped = Wrapped::default();
            return broken;
        }
        self.wrapped = Wrapped::build(self.lines.iter().map(|l| (l.text.as_ref(), cols)), wrap);
        true
    }

    fn build(&mut self, f: gitten_core::prepared::File) {
        // Kept beside the rows, which consume the hunks: the hunk map needs
        // to spell each file the loaded diff spells it.
        let path = std::sync::Arc::from(f.path.as_str());
        self.rows.push(Row::File {
            path: std::sync::Arc::clone(&path),
            adds: f.adds,
            dels: f.dels,
        });
        for (n, h) in f.hunks.into_iter().enumerate() {
            let at = self.rows.len();
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
                    old_no: l.old_no,
                    new_no: l.new_no,
                    text: l.text,
                    spans: l.spans,
                    tokens: l.tokens,
                });
            }

            for slot in slots {
                let (old, new) = (slot.left(), slot.right());
                if old.is_some() && new.is_some() {
                    self.paired += 1;
                }
                self.rows.push(Row::Pair {
                    old: old.map(|i| base + i),
                    new: new.map(|i| base + i),
                });
            }
            self.hunks.record(at, self.rows.len() - at, &path, n);
        }
    }

    fn hunk_at(&self, index: usize) -> Option<(&str, usize)> {
        self.hunks.at(index)
    }

    /// The longer of a pair row's two sides, because that is the side that
    /// decides how far there is left to scroll — both columns move together and
    /// they are the same width, so the wider text is the bound for the row.
    fn width(&self, index: usize, seg: usize) -> usize {
        match &self.rows[index] {
            Row::Pair { old, new } => self.chars(*old, seg).max(self.chars(*new, seg)),
            Row::Hunk(h) => h.chars().count(),
            Row::File { path, .. } => path.chars().count(),
        }
    }

    /// A column's text and not the row's: what has to reach the edge of the
    /// window is the widest line, and the window it has to reach the edge of is
    /// one column of two.
    fn overflow(&self, index: usize, seg: usize, width: f32, host: &Host) -> f32 {
        let text = self.width(index, seg) as f32 * host.font.char_width();
        let room = match &self.rows[index] {
            Row::Pair { .. } => self.col_px(width),
            // A header is the built-in's, drawn across the whole row behind the
            // page padding and nothing else.
            _ => width - 2.0 * PAD,
        };
        (text - room).max(0.0)
    }

    fn report(&self) -> String {
        if self.rows.is_empty() {
            return String::new();
        }
        let mut out = format!(
            "split {} paired · {} cols · widest {}",
            self.paired, self.cols, self.widest_chars
        );
        if self.moved > 0 {
            out.push_str(&format!(" · {} moved", self.moved));
        }
        if self.wrapped.rejected() > 0 {
            out.push_str(&format!(
                " · {} invalid breaks from {}",
                self.wrapped.rejected(),
                self.wrap
            ));
        }
        out
    }

    /// Which column the click was in, then where in that column's text — the
    /// divider is what makes this two answers rather than one, and the reason a
    /// [`Hit`] carries a part at all.
    fn hit(&self, index: usize, seg: usize, x: f32, host: &Host, shift: f32) -> Option<Hit> {
        match self.rows.get(index)? {
            Row::File { path, .. } => Some(header_hit(path, x, host, shift)),
            Row::Hunk(h) => Some(hunk_hit(h, x, host, shift)),
            Row::Pair { old, new } => {
                let cell = PAD + self.cell_px(self.width);
                let (part, from) = match x < cell + RULE_W {
                    true => (Column::Old, PAD),
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
                    return Some(Hit {
                        part: part.part(),
                        off: 0,
                    });
                };
                let text = &self.lines[line as usize].text;
                let at = self.wrapped.range(line as usize, seg, text);
                let off = match seg < self.wrapped.rows(line as usize) {
                    true => {
                        at.start
                            + column_at(
                                &text[at.clone()],
                                into_text(x, from + GUTTER_W + SIGN_W, shift),
                                host.font.size,
                                host,
                            )
                    }
                    // This side wrapped less far than the other one. The end of
                    // its line is the honest place for the caret.
                    false => text.len(),
                };
                Some(Hit {
                    part: part.part(),
                    off,
                })
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

    fn is_header(&self, index: usize) -> bool {
        matches!(self.rows.get(index), Some(Row::File { .. }))
    }

    fn render(
        &self,
        index: usize,
        seg: usize,
        host: &Host,
        sel: Option<Selected>,
        current: bool,
        shift: f32,
    ) -> AnyElement {
        let theme = &host.theme;
        match &self.rows[index] {
            Row::File { path, adds, dels } => {
                file_header(path, *adds, *dels, theme, sel, current, shift)
            }
            Row::Hunk(header) => hunk_header(header, theme, sel, current, shift),
            Row::Pair { old, new } => {
                // Page padding, then two columns of *measured* width and a
                // fixed rule. The width is `cell_px` — the number the hit test
                // divides clicks at — and not a flex share: as a direct child
                // of a `uniform_list` item, two `flex_1` halves do not come out
                // equal (the list measures its items against their content, and
                // the distribution goes content-driven; measured, not guessed —
                // see `list_layout_tests`). Fixed pixels cannot drift, and they
                // make drawing and hit test the same number by construction.
                let cell = px(self.cell_px(self.width));
                row_frame()
                    .items_center()
                    .px(px(PAD))
                    .bg(rgb(match current {
                        true => theme.chrome.selection_bg,
                        false => theme.chrome.bg,
                    }))
                    .child(self.cell(
                        *old,
                        seg,
                        Column::Old,
                        theme,
                        sel,
                        current,
                        shift,
                        index,
                        cell,
                    ))
                    .child(
                        div()
                            .flex_none()
                            .w(px(RULE_W))
                            .h(px(ROW_H))
                            // `diff.rule` and not `gutter_fg`, which this was:
                            // that colour has to stay legible as *text* on five
                            // row backgrounds, and a full-height line held to a
                            // text floor is a bright seam down the window.
                            .bg(rgb(theme.diff.rule)),
                    )
                    .child(self.cell(
                        *new,
                        seg,
                        Column::New,
                        theme,
                        sel,
                        current,
                        shift,
                        index,
                        cell,
                    ))
                    .into_any_element()
            }
        }
    }
}

impl SplitRows {
    /// How wide one column is, gutter and sign included: half of what is left
    /// of the window after the page padding and the rule.
    ///
    /// Shared by the drawing and the hit test so the divider is in the same place
    /// in both — the click that lands on the wrong side of it is the whole bug
    /// this one function prevents. The drawing takes it as a literal pixel
    /// width, so the divider *is* this number rather than the same arithmetic
    /// done twice.
    fn cell_px(&self, width: f32) -> f32 {
        ((width - 2.0 * PAD - RULE_W) / 2.0).max(0.0)
    }

    /// How wide one column's *text* is, in pixels: the column less its own gutter
    /// and sign, which do not scroll and are not the text's to use.
    fn col_px(&self, width: f32) -> f32 {
        (self.cell_px(width) - GUTTER_W - SIGN_W).max(0.0)
    }

    /// How many characters one side of a pair draws on visual row `seg`, after
    /// `trim_end` — trailing space is not ink, and a row that is all of it is a
    /// row with nothing to scroll to.
    fn chars(&self, line: Option<u32>, seg: usize) -> usize {
        self.present(line, seg).map_or(0, |i| {
            let text = &self.lines[i as usize].text;
            text[self.wrapped.range(i as usize, seg, text)]
                .trim_end()
                .chars()
                .count()
        })
    }

    /// The line this side of a pair draws on visual row `seg`, if it draws one
    /// at all: absent from the pair entirely, or past the end of a side whose
    /// opposite wrapped further. Asked by the cell and by the widest-row
    /// measurement, so the two cannot disagree about which rows are holes.
    fn present(&self, line: Option<u32>, seg: usize) -> Option<u32> {
        line.filter(|i| seg < self.wrapped.rows(*i as usize))
    }

    #[allow(clippy::too_many_arguments)]
    fn cell(
        &self,
        line: Option<u32>,
        seg: usize,
        column: Column,
        theme: &Theme,
        sel: Option<Selected>,
        current: bool,
        shift: f32,
        row: usize,
        width: gpui::Pixels,
    ) -> AnyElement {
        let p = &theme.diff;
        // Past the end of *this* side of a pair whose other side wrapped
        // further. The same hole as no line at all, and the same colour: the
        // alternative is a bare row of `context_bg` under a wrapped removal,
        // which reads as an unchanged line that is not there.
        let Some(index) = self.present(line, seg) else {
            // Nothing opposite: a flat, darker block, so a run of them reads as
            // a hole in the column rather than as unchanged content. The
            // keyboard's bar runs across it too, so the cursor reads as one bar.
            let bg = super::diff::row_background(current, p.absent_bg, theme);
            return cell_frame(width)
                .debug_selector(move || format!("cell-{}-{row}", column.name()))
                .bg(rgb(bg))
                .into_any_element();
        };
        let line = &self.lines[index as usize];
        let (bg, fg, sign) = line_colors(line.kind, line.moved, p);
        // The keyboard's row, on this side and on the other: one bar, not two.
        let bg = super::diff::row_background(current, bg, theme);
        // The same substitution the unified view makes: the row paints the
        // wash over whatever the line was, so a number resolved for the line
        // kind was resolved against a background it never lands on.
        let (plain, _) = surfaces(line.kind, line.moved);
        let gutter = theme.gutter_on(match current {
            true => Surface::Cursor,
            false => plain,
        });
        let no = match column {
            Column::Old => line.old_no,
            Column::New => line.new_no,
        };
        let at = self.wrapped.range(index as usize, seg, &line.text);
        let blank = seg > 0;
        // One borrow per cell: the number formats into it and the run list
        // sweeps through it, both copied out as the elements take them.
        let mut sc = self.scratch.borrow_mut();
        cell_frame(width)
            .debug_selector(move || format!("cell-{}-{row}", column.name()))
            .items_center()
            .bg(rgb(bg))
            // Right-aligned, for the reason the unified view's is: a column of
            // numbers is read down, and left-aligning puts the units digits four
            // characters apart.
            .child(
                div()
                    .flex()
                    .flex_none()
                    .justify_end()
                    .w(px(GUTTER_W))
                    .pr(px(GUTTER_PAD))
                    .text_color(rgb(gutter))
                    .child(sc.number(no, blank)),
            )
            .child(
                div()
                    .flex_none()
                    .w(px(SIGN_W))
                    .text_color(rgb(fg))
                    .child(if blank { " " } else { sign }),
            )
            .child(scrolled(
                shift,
                div().text_color(rgb(fg)).child(
                    StyledText::new(slice(&line.text, &at)).with_highlights(
                        sc.merged(
                            at.clone(),
                            &line.tokens,
                            &line.spans,
                            theme,
                            line.kind,
                            line.moved,
                            current,
                            selected(sel, column.part(), &line.text),
                        )
                        .iter()
                        .cloned(),
                    ),
                ),
            ))
            .into_any_element()
    }
}

/// One column of one row: its measured half, whatever is in it.
///
/// A pixel width and not `flex_1`: as a direct child of a `uniform_list` item,
/// two flex halves of the same row do not come out equal — the list measures
/// its items against their content and the distribution goes content-driven,
/// which staggered the right-hand column by whatever the left-hand text was.
/// Measured repeatedly in [`list_layout_tests`]; fixed widths also make the
/// drawn divider *be* the number `hit` divides clicks at, rather than the same
/// arithmetic done twice. The clipping is [`scrolled`]'s, one level in: the
/// gutter and the sign are in here too and they are the things that must not
/// move.
fn cell_frame(width: gpui::Pixels) -> Div {
    div().flex().flex_none().w(width).min_w(px(0.)).h(px(ROW_H))
}

#[cfg(test)]
mod tests {
    // By name, not a glob: `use gpui::*` in the parent shadows `#[test]` with
    // GPUI's own attribute macro and every test in here fails to expand.
    use super::{Column, Row, Rows, SplitRows, GUTTER_W, PAD, RULE_W, SIGN_W};
    use gitten_core::host::Host;
    use gitten_core::parse_unified_diff;
    use gitten_core::prepared::prepare;

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
            vec![
                (true, true),
                (true, true),
                (true, true),
                (false, true),
                (true, true)
            ]
        );
        assert_eq!(r.paired, 4, "two context rows and two replace pairs");
    }

    #[test]
    fn a_context_line_is_stored_once_and_shown_twice() {
        let r = built();
        // Seven diff lines, seven table entries — a context row appears in both
        // columns and is still stored once. Five rows hold those seven lines.
        assert_eq!(r.lines.len(), 7);
        assert_eq!(
            r.rows
                .iter()
                .filter(|row| matches!(row, Row::Pair { .. }))
                .count(),
            5
        );
        let context = r
            .rows
            .iter()
            .find_map(|row| match row {
                Row::Pair {
                    old: Some(o),
                    new: Some(n),
                    ..
                } if o == n => Some(*o),
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
                    old.and_then(|i| r.lines[i as usize].old_no)
                        .map(|n| n.to_string())
                        .unwrap_or_default(),
                    new.and_then(|i| r.lines[i as usize].new_no)
                        .map(|n| n.to_string())
                        .unwrap_or_default(),
                )),
                _ => None,
            })
            .collect();
        assert_eq!(
            numbers[3],
            ("".to_string(), "4".to_string()),
            "a lone addition"
        );
        assert_eq!(
            numbers[4],
            ("4".to_string(), "5".to_string()),
            "context after the shift"
        );
    }

    #[test]
    fn a_pair_row_measures_its_longer_side() {
        // Both columns move together under one offset, so what bounds the scroll
        // for a row is whichever of its two lines is longer.
        let r = built();
        for i in (0..r.len()).filter(|i| matches!(r.rows[*i], Row::Pair { .. })) {
            let Row::Pair { old, new } = r.rows[i] else {
                unreachable!()
            };
            let of = |line: Option<u32>| {
                line.map_or(0, |l| r.lines[l as usize].text.trim_end().chars().count())
            };
            assert_eq!(r.width(i, 0), of(old).max(of(new)), "row {i}");
        }
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
        let pair = (0..r.len())
            .find(|i| matches!(r.rows[*i], Row::Pair { .. }))
            .unwrap();
        let Row::Pair { old, new } = r.rows[pair] else {
            unreachable!()
        };
        let (old, new) = (old.unwrap() as usize, new.unwrap() as usize);
        assert!(r.wrapped.rows(old) > 1, "the long side did not wrap");
        assert_eq!(r.wrapped.rows(new), 1, "the short side wrapped");
        assert_eq!(r.rows(pair), r.wrapped.rows(old));
    }

    #[test]
    fn a_column_is_half_the_window_whatever_the_wrap_is_doing() {
        // The divider does not move when the wrap changes, and a column is never
        // sized to the widest line in the diff: what a line too long for its
        // column gets is somewhere to be scrolled to.
        let (mut r, host) = wrapped(LOPSIDED, 20);
        let f = &host.font;
        let width = super::CHROME + 2.0 * (20.0 + super::SLACK + 0.5) * f.size * f.advance;
        let half = r.cell_px(width);
        assert_eq!(r.cols, 20);
        let pair = (0..r.len())
            .find(|i| matches!(r.rows[*i], Row::Pair { .. }))
            .unwrap();
        // Wrapping on, every row fits its column, so there is nothing to the
        // right of the window at all.
        assert_eq!(r.overflow(pair, 0, width, &host), 0.0);

        let off = host.wrap.at(host.wrap.position("off").unwrap());
        assert!(
            r.reflow(width, &host, off),
            "the rows did not come back together"
        );
        assert_eq!(r.cell_px(width), half, "the divider moved with the wrap");
        // And now the long side runs past its column's edge by however much of
        // it did not fit.
        let over = r.overflow(pair, 0, width, &host);
        assert!(over > 0.0, "a 74-character line fits a 20-character column");
        let text = r.widest_chars as f32 * host.font.char_width();
        assert!((over - (text - r.col_px(width))).abs() < 0.001, "{over}");
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
        let pair = (0..r.len())
            .find(|i| matches!(r.rows[*i], Row::Pair { .. }))
            .unwrap();
        let rule_at = PAD + r.cell_px(900.0);

        let left = r
            .hit(pair, 0, PAD + GUTTER_W + SIGN_W + 2.0, &host, 0.0)
            .unwrap();
        assert_eq!(left.part, Column::Old.part());
        let right = r
            .hit(
                pair,
                0,
                rule_at + RULE_W + GUTTER_W + SIGN_W + 2.0,
                &host,
                0.0,
            )
            .unwrap();
        assert_eq!(right.part, Column::New.part());
        // Either side of the rule itself, and nothing in between.
        assert_eq!(r.hit(pair, 0, rule_at - 1.0, &host, 0.0).unwrap().part, 0);
        assert_eq!(
            r.hit(pair, 0, rule_at + RULE_W + 1.0, &host, 0.0)
                .unwrap()
                .part,
            1
        );
        // The page padding is the left column's too: a click on it is a click in
        // the old half, not a miss.
        assert_eq!(r.hit(pair, 0, PAD - 1.0, &host, 0.0).unwrap().part, 0);
    }

    #[test]
    fn each_column_offers_its_own_text_and_a_hole_offers_none() {
        // What makes a drag down one column paste that file: the other side is
        // not a blank line, it is nothing at all.
        let host = Host::new();
        let mut r = built();
        r.reflow(900.0, &host, host.wrap.current());
        let pairs: Vec<usize> = (0..r.len())
            .filter(|i| matches!(r.rows[*i], Row::Pair { .. }))
            .collect();
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
    fn a_scroll_moves_both_columns_and_neither_gutter() {
        // The point of the whole thing: the two gutters and the divider are where
        // they were, and a click at the left edge of a column's text lands on the
        // character that is drawn there rather than on the one that was before
        // anything scrolled.
        let host = Host::new();
        let mut r = built();
        r.reflow(
            900.0,
            &host,
            host.wrap.at(host.wrap.position("off").unwrap()),
        );
        let pair = (0..r.len())
            .find(|i| matches!(r.rows[*i], Row::Pair { .. }))
            .unwrap();
        let cw = host.font.char_width();
        let shift = 4.0 * cw;
        // A fifth of a character in, and not half: `column_at` rounds, so a
        // click exactly on a boundary is a coin toss between two answers and
        // neither of them is what this test is about.
        let into = GUTTER_W + SIGN_W + 0.2 * cw;

        assert_eq!(r.hit(pair, 0, PAD + into, &host, 0.0).unwrap().off, 0);
        assert_eq!(r.hit(pair, 0, PAD + into, &host, shift).unwrap().off, 4);
        // The right-hand column too, from its own edge — past the padding and
        // the rule — and still part 1: the divider did not move.
        let across = PAD + r.cell_px(900.0) + RULE_W + into;
        let right = r.hit(pair, 0, across, &host, shift).unwrap();
        assert_eq!((right.part, right.off), (1, 4));
        // A click on a line number is the first character there is to see, not
        // one that scrolled out of the window.
        assert_eq!(r.hit(pair, 0, 2.0, &host, shift).unwrap().off, 4);
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

/// The presentation laid out by the real list, measured.
///
/// The isolated row lays out correctly (see `tests/pair_layout.rs`), so when
/// real use staggers rows across the window, what is left is this: the
/// presentation through [`uniform_list`], at the widths and in the sequence the
/// view actually drives it. This opens a headless window, paints both the
/// pre-reflow frame the first paint gets and the reflowed one after, and reads
/// every visible row's bounds back.
#[cfg(test)]
mod list_layout_tests {
    use std::rc::Rc;

    use super::{Rows, SplitRows, PAD, RULE_W};
    use gitten_core::host::Host;
    use gitten_core::parse_unified_diff;
    use gitten_core::prepared::prepare;
    use gpui::{
        px, size, AppContext, Bounds, Context, IntoElement, Render, Styled, WindowBounds,
        WindowOptions,
    };

    // The parent's `use gpui::*` shadows `#[test]` with GPUI's own macro; these
    // tests are named through it on purpose and keep it fully qualified.

    const W: f32 = 1536.0;

    // check.yml's shape: a hunk of long additions (each wraps), then a hunk of
    // pairs and holes. Whatever makes rows wander should wander here too.
    const SRC: &str = "\
diff --git a/.github/workflows/check.yml b/.github/workflows/check.yml
@@ -0,0 +1,4 @@
+name: check
+# Everything that can be checked without opening a window - the CI half of ./check.sh. Two jobs, deliberately small:
+jobs:
+  check:
@@ -10,3 +14,6 @@
 context one
-old line that is quite long indeed and will not fit into half of any reasonable window width without wrapping somewhere
+new line that is also quite long indeed and will not fit into half of any reasonable window width either
+lone addition opposite nothing at all
 context two
";

    struct Probe {
        rows: Rc<SplitRows>,
        host: Rc<Host>,
        flat: Rc<Vec<(usize, u16)>>,
    }

    impl Render for Probe {
        fn render(
            &mut self,
            _window: &mut gpui::Window,
            _cx: &mut Context<Self>,
        ) -> impl IntoElement {
            let rows = self.rows.clone();
            let host = self.host.clone();
            let flat = self.flat.clone();
            gpui::uniform_list("probe", flat.len(), move |range, _, _| {
                range
                    .map(|k| {
                        let (i, seg) = flat[k];
                        rows.render(i, seg as usize, &host, None, false, 0.0)
                    })
                    .collect::<Vec<_>>()
            })
            .size_full()
        }
    }

    /// Build → build files → **reflow**, which is where every real session
    /// converges after the probe reports the width.
    fn ready() -> (Rc<SplitRows>, Rc<Host>, Vec<(usize, u16)>) {
        let host = Host::new();
        let mut r = SplitRows::default();
        let mut p = prepare(&parse_unified_diff(SRC), &host.syntax, 2000);
        for f in p.files.drain(..) {
            r.build(f);
        }
        r.reflow(W, &host, host.wrap.current());
        let mut flat = Vec::new();
        for i in 0..r.len() {
            for seg in 0..r.rows(i) {
                flat.push((i, seg as u16));
            }
        }
        assert!(
            flat.len() > 6,
            "nothing wrapped; the fixture is not exercising the bug"
        );
        (Rc::new(r), Rc::new(host), flat)
    }

    #[gpui::test]
    fn every_row_spans_the_viewport_from_x_zero(cx: &mut gpui::TestAppContext) {
        let (rows, host, flat) = ready();
        let n = flat.len().min(9);
        let handle = cx.update(|cx| {
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: Default::default(),
                        size: size(px(W), px(300.)),
                    })),
                    ..Default::default()
                },
                |_window, cx| {
                    cx.new(|_| Probe {
                        rows,
                        host,
                        flat: Rc::new(flat[..n].to_vec()),
                    })
                },
            )
            .unwrap()
        });
        let mut cx = gpui::VisualTestContext::from_window(handle.into(), cx);

        // The cells inside the rows. With no wrapper between list and row this
        // is exactly how the real view lays them out, and it is where the
        // flex halves went unequal — asserted equal now that they are pixels.
        let half = (W - 2.0 * PAD - RULE_W) / 2.0;
        let mut checked = 0;
        for k in 0..n {
            let o_sel: &'static str = Box::leak(format!("cell-old-{k}").into_boxed_str());
            let n_sel: &'static str = Box::leak(format!("cell-new-{k}").into_boxed_str());
            let (Some(o), Some(nw)) = (cx.debug_bounds(o_sel), cx.debug_bounds(n_sel)) else {
                continue;
            };
            checked += 1;
            assert!(
                (f32::from(o.size.width) - half).abs() < 1.0,
                "row {k} old cell {} != {half}",
                o.size.width
            );
            assert!(
                (f32::from(nw.size.width) - half).abs() < 1.0,
                "row {k} new cell {} != {half}",
                nw.size.width
            );
            assert_eq!(
                nw.origin.x,
                px(PAD + half + RULE_W),
                "row {k} new column drifted"
            );
        }
        assert!(checked >= 3, "only {checked} rows laid out cells");
    }

    /// The whole view — [`Diff::new`], its list, its probe, its reflow — with
    /// nothing hand-rolled. If real use staggers the columns, this sees it.
    #[gpui::test]
    fn the_real_view_holds_two_columns(cx: &mut gpui::TestAppContext) {
        let handle = cx.update(|cx| {
            gpui_component::init(cx);
            let mut host = Host::new();
            host.layout = "split".into();
            let host = Rc::new(host);
            cx.set_global(crate::config::Active(host.clone()));
            let files = parse_unified_diff(SRC);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: Default::default(),
                        size: size(px(W), px(600.)),
                    })),
                    ..Default::default()
                },
                |_window, cx| cx.new(|cx| crate::views::diff::Diff::new(files, host, cx)),
            )
            .unwrap()
        });
        // The probe reports the width one frame late and notifies; park until
        // the reflow that follows has been painted.
        let mut cx = gpui::VisualTestContext::from_window(handle.into(), cx);
        cx.run_until_parked();

        let half = (W - 2.0 * PAD - RULE_W) / 2.0;
        let mut checked = 0;
        for k in 0..13 {
            let old: &'static str = Box::leak(format!("cell-old-{k}").into_boxed_str());
            let new: &'static str = Box::leak(format!("cell-new-{k}").into_boxed_str());
            let (Some(o), Some(n)) = (cx.debug_bounds(old), cx.debug_bounds(new)) else {
                continue; // past what the window painted
            };
            checked += 1;
            let drift = |actual: f32, expected: f32, what: String| {
                assert!(
                    (actual - expected).abs() < 1.5,
                    "{what}: {actual} != {expected}"
                );
            };
            drift(
                f32::from(o.origin.x),
                PAD,
                format!("row {k} old column drifted"),
            );
            drift(
                f32::from(n.origin.x),
                PAD + half + RULE_W,
                format!("row {k} new column drifted"),
            );
            drift(
                f32::from(o.size.width),
                half,
                format!("row {k} old cell width"),
            );
            drift(
                f32::from(n.size.width),
                half,
                format!("row {k} new cell width"),
            );
        }
        assert!(
            checked >= 5,
            "only {checked} rows painted; the fixture shrank"
        );
    }
}
