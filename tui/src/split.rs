//! A diff drawn as two columns: the old file on the left, the new on the right.
//!
//! A [`Rows`] implementation and nothing more, which is the interesting part —
//! side-by-side sounds like a different view and is a different *presentation*.
//! It claims every path, it takes the same prepared lines the built-in takes,
//! and it draws them in two columns instead of one. No new trait, no new
//! argument, no edit to [`TextRows`](crate::rows::TextRows).
//!
//! # Which line sits opposite which
//!
//! Not this file's decision. [`plait_core::align`] makes it, because
//! `replace_pairs` already makes the same one for the intraline pass and the two
//! answers have to agree — a row showing a removal beside an addition whose
//! changed words were computed against a *different* line highlights fragments
//! that correspond to nothing on screen.
//!
//! # One column width for the whole diff
//!
//! Half of what is left after the two gutters and the rule, always, so the
//! divider is one straight vertical line from the first row to the last.
//!
//! This is where the terminal legitimately differs from the window. GPUI scrolls
//! a container wider than itself, so there the column is the *widest line in the
//! diff* with wrapping off and the scrollbar is how a 2000-character minified
//! line is reached. A terminal has no container: a column wider than half the
//! screen would put the right-hand gutter off the edge entirely. So the columns
//! stay half the screen and the *text inside them* scrolls, under gutters that
//! do not — which reaches the same line and keeps both sides visible while doing
//! it. [`Pen::scroll`](crate::screen::Pen::scroll) is what makes that a shared
//! two lines rather than a slice taken at the wrong stage.
//!
//! # Row count is not the same as unified's
//!
//! This is the one presentation that legitimately changes it — a removal and the
//! addition that replaced it share a row, so a hunk of N removals and N
//! additions is N rows here and 2N there. A pair row is as tall as its *taller*
//! side; the shorter one runs out of text partway down and draws `absent_bg` for
//! the rest, which is the same thing it already draws opposite a lone addition.

use crate::rows::{
    col_at, digits, file_header, header_hit, hunk_header, line_colors, number, row_bg, text_run,
    Frame, Rows,
};
use crate::screen::{self, Ink, Pen};
use crate::MIN_WRAP_COLS;
use plait_core::align::align;
use plait_core::host::Host;
use plait_core::prepared::{File, Line};
use plait_core::rows::{Entry, Present};
use plait_core::runs::Run;
use plait_core::select::Hit;
use plait_core::wrap::{Wrap, Wrapped};
use plait_core::LineKind;

/// Which side of the divider a cell is on, and therefore which of the line's two
/// numbers its gutter shows.
///
/// A context line carries both: the left says where the line was and the right
/// says where it is now, and after an insertion those differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Column {
    Old,
    New,
}

impl Column {
    /// Which of the row's two texts this column draws, for a selection. The old
    /// side is 0 because a row with only one side has that side — see
    /// [`SplitRows::selectable`].
    fn part(self) -> u16 {
        match self {
            Column::Old => 0,
            Column::New => 1,
        }
    }
}

/// The rule between the columns. A single box-drawing character, so it joins up
/// vertically into one continuous line.
const RULE: &str = "│";

enum Row {
    File {
        path: String,
        adds: usize,
        dels: usize,
    },
    Hunk(String),
    /// Indices into [`SplitRows::lines`]. A context row points both columns at
    /// the same line; a lone removal or addition leaves one side `None`.
    Pair {
        old: Option<u32>,
        new: Option<u32>,
    },
}

/// The two-column presentation.
///
/// Register it as `owners[0]` and it takes the whole diff; register it after the
/// built-in and it takes whatever the built-in would have.
#[derive(Default)]
pub struct SplitRows {
    rows: Vec<Row>,
    /// One flat table, so a context line — which appears in *both* columns — is
    /// stored once, and so are its wrap points however many rows it takes.
    lines: Vec<Line>,
    /// How many rows carry a pair rather than one side, for the status line. A
    /// diff whose rows are almost all pairs is one this presentation suits.
    paired: usize,
    moved: usize,
    /// Where each *line* breaks — indexed by line and not by row, because a pair
    /// row draws two of them and a context line is drawn by two columns.
    wrapped: Wrapped,
    digits: usize,
    /// Where each file starts, for a jump list. Built here rather than reused
    /// from a `Flat`, because this presentation's rows are pairs and not lines.
    files: Vec<Entry>,
    /// The text budget one column got, and the policy it was built with.
    cols: usize,
    wrap: &'static str,
}

impl SplitRows {
    /// Columns one line-number gutter occupies.
    ///
    /// One per column here rather than two side by side, so it may be as wide as
    /// the diff needs: the two together still cost less than the unified
    /// presentation's pair.
    fn gutter(&self) -> usize {
        self.digits.max(2)
    }

    /// What one side draws before its text: a gutter, a space, a sign, a space.
    fn side_chrome(&self) -> usize {
        self.gutter() + 3
    }

    /// Everything drawn besides the two columns of text: a gutter, a space, a
    /// sign and a space on each side, and the rule between them.
    pub fn chrome(&self) -> usize {
        2 * self.side_chrome() + screen::width(RULE)
    }

    /// How wide one column's text is. Half of what is left, which is what makes
    /// a wrapped line here break at half the width it would unified.
    pub fn col(&self) -> usize {
        self.cols
    }

    fn budget(&self, cols: usize) -> usize {
        cols.saturating_sub(self.chrome()).max(2 * MIN_WRAP_COLS) / 2
    }

    /// How many rows one side of a pair takes. One for an absent side, so a lone
    /// addition is one row and not none.
    fn side_rows(&self, line: Option<u32>) -> usize {
        line.map_or(1, |i| self.wrapped.rows(i as usize))
    }

    /// One column of one row: gutter, sign, text — the built-in's anatomy at
    /// half the width, so the eye finds the same things in the same order.
    fn cell(
        &self,
        line: Option<u32>,
        seg: usize,
        column: Column,
        at: &Frame,
        pen: &mut Pen,
        out: &mut Vec<Run>,
    ) {
        let theme = at.theme();
        let p = &theme.diff;
        // `None` is nothing opposite; past the end is *this* side of a pair whose
        // other side wrapped further. The same hole, and the same colour: a bare
        // row of `context_bg` under a wrapped removal reads as an unchanged line
        // that is not there.
        let Some(index) = line.filter(|i| seg < self.wrapped.rows(*i as usize)) else {
            pen.wash(Ink::new(p.gutter_fg, row_bg(p.absent_bg, at)));
            return;
        };
        let l = &self.lines[index as usize];
        let (own, fg, sign) = line_colors(l.kind, l.moved, p);
        let bg = row_bg(own, at);
        let row_ink = Ink::new(fg, bg);
        let gutter = Ink::new(p.gutter_fg, bg);
        let no = match column {
            Column::Old => l.old_no,
            Column::New => l.new_no,
        };

        number(no, seg > 0, self.gutter(), gutter, pen);
        pen.put(" ", gutter);
        pen.put(if seg > 0 { " " } else { sign }, row_ink);
        pen.put(" ", row_ink);
        let span = self.wrapped.range(index as usize, seg, &l.text);
        text_run(
            l,
            span,
            theme,
            row_ink,
            at.shift,
            at.part(column.part()),
            pen,
            out,
        );
    }
}

impl Present for SplitRows {
    /// Everything. This is a presentation of the whole diff, not of one kind of
    /// file, so as `owners[0]` it replaces the built-in fallback rather than
    /// standing beside it.
    fn claims(&self, _path: &str) -> bool {
        true
    }

    fn len(&self) -> usize {
        self.rows.len()
    }

    fn build(&mut self, f: File) {
        self.files.push(Entry {
            path: f.path.clone(),
            adds: f.adds,
            dels: f.dels,
            row: self.rows.len(),
        });
        self.rows.push(Row::File {
            path: f.path,
            adds: f.adds,
            dels: f.dels,
        });
        for h in f.hunks {
            self.rows.push(Row::Hunk(h.header));

            // The alignment is computed from the kinds alone, *before* the lines
            // are moved into the table: `align` returns indices into this hunk,
            // and consuming the lines first would lose them.
            let kinds: Vec<LineKind> = h.lines.iter().map(|l| l.kind).collect();
            let slots = align(&kinds);

            let base = self.lines.len() as u32;
            for l in h.lines {
                self.moved += l.moved as usize;
                self.digits = self
                    .digits
                    .max(digits(l.old_no.unwrap_or(0).max(l.new_no.unwrap_or(0))));
                self.lines.push(l);
            }

            for slot in slots {
                let (old, new) = (slot.left(), slot.right());
                self.paired += (old.is_some() && new.is_some()) as usize;
                self.rows.push(Row::Pair {
                    old: old.map(|i| base + i),
                    new: new.map(|i| base + i),
                });
            }
        }
    }

    fn rows(&self, index: usize) -> usize {
        match self.rows.get(index) {
            Some(Row::Pair { old, new }) => self.side_rows(*old).max(self.side_rows(*new)),
            _ => 1,
        }
    }

    /// Widest *text* in either column, which is what a horizontal scroll is
    /// bounded by. The columns themselves are always the same width, so unlike
    /// the unified presentation there is no widest *row* to find.
    fn width(&self, index: usize, seg: usize) -> usize {
        match self.rows.get(index) {
            Some(Row::Pair { old, new }) => [old, new]
                .into_iter()
                .flatten()
                .map(|i| {
                    let l = &self.lines[*i as usize];
                    let span = self.wrapped.range(*i as usize, seg, &l.text);
                    screen::width(l.text[span].trim_end())
                })
                .max()
                .unwrap_or(0),
            Some(Row::Hunk(h)) => screen::width(h),
            Some(Row::File { path, .. }) => screen::width(path),
            None => 0,
        }
    }

    fn files(&self) -> &[Entry] {
        &self.files
    }
}

impl Rows for SplitRows {
    fn reflow(&mut self, cols: usize, _host: &Host, wrap: &dyn Wrap) -> bool {
        let cols = self.budget(cols);
        if cols == self.cols && wrap.name() == self.wrap {
            return false;
        }
        self.cols = cols;
        self.wrap = wrap.name();
        // A wrap that breaks nothing needs no scan: the unbroken table is the
        // default one, and building it to be told so is a pass over every line
        // in the diff on every column of a resize drag.
        if !wrap.breaks_lines() {
            let broken = self.wrapped.total() > self.wrapped.lines();
            self.wrapped = Wrapped::default();
            return broken;
        }
        self.wrapped = Wrapped::build(self.lines.iter().map(|l| (l.text.as_ref(), cols)), wrap);
        true
    }

    fn report(&self) -> String {
        if self.rows.is_empty() {
            return String::new();
        }
        let mut out = format!("split {} paired · {} cols", self.paired, self.cols);
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

    /// Which side of the rule the pointer is on, and which byte of that side's
    /// line it is over.
    ///
    /// The column width is the one the last reflow settled on rather than the pen's,
    /// because a hit test has no pen — they differ only by the odd column an odd
    /// terminal width leaves over, which `render` washes at the right-hand edge and
    /// which no text is drawn in.
    fn hit(&self, index: usize, seg: usize, col: usize, shift: usize) -> Option<Hit> {
        match self.rows.get(index)? {
            Row::File { path, .. } => Some(header_hit(path, col)),
            Row::Hunk(h) => Some(header_hit(h, col)),
            Row::Pair { old, new } => {
                let side = self.side_chrome() + self.cols;
                let (column, from) = match col < side + screen::width(RULE) {
                    true => (Column::Old, 0),
                    false => (Column::New, side + screen::width(RULE)),
                };
                let line = match column {
                    Column::Old => *old,
                    Column::New => *new,
                };
                // No line on this side at all: a lone addition opposite a hole.
                // The caret still lands *on* the row, at nothing — which keeps a
                // drag through the hole extending instead of freezing, and copies
                // nothing for it because `selectable` has nothing to give.
                let Some(line) = line else {
                    return Some(Hit {
                        part: column.part(),
                        off: 0,
                    });
                };
                let text = &self.lines[line as usize].text;
                let at = self.wrapped.range(line as usize, seg, text);
                let off = match seg < self.wrapped.rows(line as usize) {
                    true => {
                        let within = col.saturating_sub(from + self.side_chrome()) + shift;
                        at.start + col_at(&text[at.clone()], within)
                    }
                    // This side wrapped less far than the other one. The end of
                    // its line is the honest place for the caret.
                    false => text.len(),
                };
                Some(Hit {
                    part: column.part(),
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

    fn render(&self, index: usize, seg: usize, at: &Frame, pen: &mut Pen, out: &mut Vec<Run>) {
        match self.rows.get(index) {
            Some(Row::File { path, adds, dels }) => file_header(path, *adds, *dels, at, pen),
            Some(Row::Hunk(header)) => hunk_header(header, at, pen),
            Some(Row::Pair { old, new }) => {
                // Both halves are the same width, computed from what the pen
                // actually has rather than from the last reflow: a row drawn
                // into a narrower pen than the diff was reflowed for clips
                // symmetrically instead of pushing the rule off the edge.
                let side = (pen.room().saturating_sub(screen::width(RULE))) / 2;
                {
                    let mut left = pen.take(side);
                    self.cell(*old, seg, Column::Old, at, &mut left, out);
                }
                let rule_bg = row_bg(at.theme().diff.context_bg, at);
                pen.put(RULE, Ink::new(at.theme().diff.gutter_fg, rule_bg));
                {
                    let mut right = pen.take(side);
                    self.cell(*new, seg, Column::New, at, &mut right, out);
                }
                // Whatever an odd column count left over, in the right-hand
                // side's own background rather than in the chrome's.
                pen.wash(Ink::new(at.theme().diff.gutter_fg, rule_bg));
            }
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rows::{assemble, Layouts};
    use crate::screen::Screen;
    use plait_core::parse_unified_diff;
    use plait_core::wrap::{Off, Word};

    const DIFF: &str = "\
diff --git a/a.rs b/a.rs
--- a/a.rs
+++ b/a.rs
@@ -1,4 +1,4 @@
 fn one() {}
-let x = 1;
+let x = 2;
+let z = 3;
 fn two() {}
";

    struct Harness {
        host: Host,
        owners: Vec<Box<dyn Rows>>,
        order: Vec<plait_core::rows::RowRef>,
        screen: Screen,
        cols: usize,
    }

    impl Harness {
        fn new(raw: &str, cols: usize, wrap: &dyn Wrap) -> Self {
            let host = Host::new();
            let layouts = Layouts::builtin();
            let mut owners = layouts.build(layouts.position("split").unwrap(), &host);
            let a = assemble(&parse_unified_diff(raw), &host, &mut owners);
            for o in owners.iter_mut() {
                o.reflow(cols, &host, wrap);
            }
            let order = plait_core::rows::expand(&a.ordered.order, &owners, None).order;
            Harness {
                host,
                owners,
                order,
                screen: Screen::new(cols, 64),
                cols,
            }
        }

        fn paint(&mut self, shift: usize) -> Vec<String> {
            let ink = Ink::new(self.host.theme.chrome.fg, self.host.theme.chrome.bg);
            self.screen.clear(ink);
            let mut out = Vec::new();
            for (y, r) in self.order.iter().enumerate() {
                let at = Frame {
                    host: &self.host,
                    shift,
                    current: false,
                    sel: None,
                };
                let mut pen = self.screen.row(y);
                self.owners[r.owner as usize].render(
                    r.index as usize,
                    r.seg as usize,
                    &at,
                    &mut pen,
                    &mut out,
                );
            }
            (0..self.order.len())
                .map(|y| self.screen.row_text(y))
                .collect()
        }
    }

    #[test]
    fn a_click_lands_in_the_column_it_is_over_and_selects_that_side_only() {
        let h = Harness::new(DIFF, 60, &Word);
        let rows = &h.owners[0];
        // Row 3 is the pair `-let x = 1;` / `+let x = 2;`. The left column's text
        // starts after its gutter and sign; the right one is past the rule.
        let split = h.owners[0]
            .hit(3, 0, 5, 0)
            .expect("the old side is selectable");
        assert_eq!(split.part, 0);
        assert_eq!(rows.selectable(3, 0), Some("let x = 1;"));
        let right = rows.hit(3, 0, h.cols - 5, 0).unwrap();
        assert_eq!(right.part, 1, "the pointer was past the rule");
        assert_eq!(rows.selectable(3, 1), Some("let x = 2;"));
        // A row with nothing opposite: the caret still lands on it, so a drag
        // through the hole keeps going, and it copies nothing.
        let hole = rows.hit(4, 0, 5, 0).unwrap();
        assert_eq!(hole.part, 0);
        assert_eq!(rows.selectable(4, 0), None, "a hole had text to copy");
        assert_eq!(rows.selectable(4, 1), Some("let z = 3;"));
        // A header belongs to both columns: a path is neither file's.
        assert_eq!(rows.selectable(0, 0), rows.selectable(0, 1));
    }

    #[test]
    fn a_replacement_shares_one_row_and_a_lone_addition_gets_its_own() {
        let mut h = Harness::new(DIFF, 60, &Word);
        let rows = h.paint(0);
        // Two headers, then one context, one pair, one lone addition, one
        // context: six rows where the unified presentation has seven.
        assert_eq!(rows.len(), 6);
        assert!(
            rows[3].contains("let x = 1;") && rows[3].contains("let x = 2;"),
            "{:?}",
            rows[3]
        );
        assert!(rows[4].contains("let z = 3;"), "{:?}", rows[4]);
    }

    #[test]
    fn the_rule_is_one_straight_line_down_every_pair_row() {
        // The property one column width for the whole diff exists to give. A
        // divider that drifts as you scroll is worse than one too far right.
        let mut h = Harness::new(DIFF, 60, &Word);
        h.paint(0);
        let at = (0..h.cols)
            .find(|x| h.screen.char_at(*x, 3) == Some('│'))
            .expect("a rule");
        for y in [2, 3, 4, 5] {
            assert_eq!(h.screen.char_at(at, y), Some('│'), "row {y} lost the rule");
        }
    }

    #[test]
    fn the_left_column_shows_the_old_number_and_the_right_the_new() {
        let mut h = Harness::new(DIFF, 60, &Word);
        let rows = h.paint(0);
        // The lone addition is line 3 on the new side and nothing on the old.
        let cells: Vec<&str> = rows[4].split('│').collect();
        assert!(
            cells[0].trim().is_empty(),
            "the old side of an insertion was not empty"
        );
        assert!(cells[1].trim_start().starts_with('3'), "{:?}", cells[1]);
    }

    #[test]
    fn the_empty_half_of_a_row_is_its_own_colour_and_not_context() {
        // `absent_bg` means "there is no line here"; `context_bg` means "this
        // line did not change". Drawing one as the other is a lie about the diff.
        let mut h = Harness::new(DIFF, 60, &Word);
        h.paint(0);
        let p = &h.host.theme.diff;
        assert_eq!(h.screen.ink(0, 4).unwrap().bg, p.absent_bg);
        assert_ne!(p.absent_bg, p.context_bg);
    }

    #[test]
    fn a_pair_row_is_as_tall_as_its_taller_side() {
        let raw = format!(
            "diff --git a/a.rs b/a.rs\n@@ -1,1 +1,1 @@\n-{}\n+short\n",
            "word ".repeat(20)
        );
        let h = Harness::new(&raw, 60, &Word);
        // One header, one hunk header, then a pair taking several rows.
        assert!(
            h.order.len() > 3,
            "the pair did not grow with its longer side"
        );
        let segs: Vec<u16> = h
            .order
            .iter()
            .filter(|r| r.index == 2)
            .map(|r| r.seg)
            .collect();
        assert_eq!(segs, (0..segs.len() as u16).collect::<Vec<_>>());
    }

    #[test]
    fn the_shorter_side_of_a_wrapped_pair_becomes_a_hole_rather_than_a_context_row() {
        let raw = format!(
            "diff --git a/a.rs b/a.rs\n@@ -1,1 +1,1 @@\n-{}\n+short\n",
            "word ".repeat(20)
        );
        let mut h = Harness::new(&raw, 60, &Word);
        h.paint(0);
        let p = &h.host.theme.diff;
        // Row 2 is the pair's first row: both sides have text.
        assert_eq!(h.screen.ink(0, 2).unwrap().bg, p.removed_bg);
        let rule = (0..h.cols)
            .find(|x| h.screen.char_at(*x, 2) == Some('│'))
            .unwrap();
        assert_eq!(h.screen.ink(rule + 1, 2).unwrap().bg, p.added_bg);
        // Row 3 continues the removal, and the addition has run out.
        assert_eq!(h.screen.ink(0, 3).unwrap().bg, p.removed_bg);
        assert_eq!(h.screen.ink(rule + 1, 3).unwrap().bg, p.absent_bg);
    }

    #[test]
    fn a_column_wraps_at_half_the_width_the_unified_presentation_would() {
        let raw = format!(
            "diff --git a/a.rs b/a.rs\n@@ -1,1 +1,1 @@\n-{}\n+b\n",
            "word ".repeat(30)
        );
        let split = Harness::new(&raw, 80, &Word);
        let host = Host::new();
        let layouts = Layouts::builtin();
        let mut unified = layouts.build(layouts.position("unified").unwrap(), &host);
        let a = assemble(&parse_unified_diff(&raw), &host, &mut unified);
        for o in unified.iter_mut() {
            o.reflow(80, &host, &Word);
        }
        let flat = plait_core::rows::expand(&a.ordered.order, &unified, None);
        assert!(
            split.order.len() > flat.order.len(),
            "split {} rows, unified {}",
            split.order.len(),
            flat.order.len()
        );
    }

    #[test]
    fn text_scrolls_under_gutters_that_do_not() {
        // Where the terminal differs from the window on purpose: no container to
        // scroll, so the columns stay put and their contents move.
        let mut h = Harness::new(DIFF, 40, &Off);
        let rows = h.paint(4);
        let cells: Vec<&str> = rows[3].split('│').collect();
        assert!(
            cells[0].contains(" 2 - "),
            "the gutter moved: {:?}",
            cells[0]
        );
        assert!(cells[0].contains("x = 1;"), "{:?}", cells[0]);
        assert!(!cells[0].contains("let"), "{:?}", cells[0]);
    }

    #[test]
    fn neither_half_can_write_past_the_divider() {
        let mut h = Harness::new(DIFF, 34, &Off);
        h.paint(0);
        let at = (0..h.cols)
            .find(|x| h.screen.char_at(*x, 3) == Some('│'))
            .unwrap();
        for y in 2..6 {
            assert_eq!(
                h.screen.char_at(at, y),
                Some('│'),
                "row {y} overran the rule"
            );
        }
    }

    #[test]
    fn the_report_says_how_well_this_presentation_suits_the_diff() {
        let h = Harness::new(DIFF, 60, &Word);
        let report = h.owners[0].report();
        // Every row using both columns, context included: three of the four.
        assert!(report.starts_with("split 3 paired"), "{report}");
        assert!(report.contains("cols"), "{report}");
    }

    #[test]
    fn a_reflow_that_changes_nothing_says_so() {
        // The answer is "did the row expansion change", not "did the policy":
        // switching to a wrap that joins no rows back is not a change, and
        // saying so would rebuild the order table for nothing.
        let raw = format!(
            "diff --git a/a.rs b/a.rs\n@@ -1,1 +1,1 @@\n-{}\n+b\n",
            "word ".repeat(30)
        );
        let mut h = Harness::new(&raw, 80, &Word);
        let host = Host::new();
        assert!(
            !h.owners[0].reflow(80, &host, &Word),
            "same width, same policy"
        );
        // Off pulls the wrapped line back together — the order table must hear.
        assert!(h.owners[0].reflow(80, &host, &Off));
        // And off to off changed nothing at all.
        assert!(!h.owners[0].reflow(80, &host, &Off));
        assert!(h.owners[0].reflow(80, &host, &Word), "and apart again");

        // A diff with nothing long enough to wrap never comes apart, so neither
        // direction of the switch is a structural change.
        let mut h = Harness::new(DIFF, 60, &Word);
        assert!(!h.owners[0].reflow(60, &host, &Off));
    }
}
