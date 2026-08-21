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
//! Both columns are as wide as the widest line anywhere in the diff, so the
//! divider is one straight vertical line from the first row to the last. The
//! alternative — per-file or per-viewport widths — moves the divider as you
//! scroll, and a boundary that drifts is worse than one that is too far right.
//!
//! Wider than the window is fine and is what the horizontal scrollbar is for. It
//! is also unavoidable: a 2000-character minified line has to go somewhere, and
//! clipping the column would lose text that unified mode shows.
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
//! appears in both columns — is stored once.

use super::diff::{file_header, hunk_header, line_colors, number, runs, Rows, ROW_H};
use gpui::*;
use plait_core::align::align;
use plait_core::host::Host;
use plait_core::syntax::Token;
use plait_core::theme::Theme;
use plait_core::{LineKind, Span};

/// Which side of the divider a cell is being drawn on, and therefore which of
/// the line's two numbers its gutter shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Column {
    Old,
    New,
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

/// One prepared line, ready to draw. Held in a flat table so that a context
/// line, which appears in both columns, is stored once.
///
/// Both numbers, because a context line carries both and which one is shown
/// depends on the column it is being drawn in — the left says where the line was
/// and the right says where it is now, and after an insertion those differ.
struct Line {
    kind: LineKind,
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
                self.lines.push(Line {
                    kind: l.kind,
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
    fn width(&self, index: usize) -> usize {
        match &self.rows[index] {
            Row::Pair { .. } => 2 * (self.widest_chars + 8),
            Row::Hunk(h) => h.chars().count(),
            Row::File { path, .. } => path.chars().count(),
        }
    }

    fn report(&self) -> String {
        match self.rows.is_empty() {
            true => String::new(),
            false => format!("split {} paired · {} cols", self.paired, self.widest_chars),
        }
    }

    fn render(&self, index: usize, host: &Host) -> AnyElement {
        let theme = &host.theme;
        match &self.rows[index] {
            Row::File { path, adds, dels } => file_header(path, *adds, *dels, theme),
            Row::Hunk(header) => hunk_header(header, theme),
            Row::Pair { old, new } => {
                // The column width in pixels, from the host's font rather than a
                // constant: the face is configurable and hot-reloaded, and a
                // stale character width is exactly what `font.advance` exists to
                // stop being possible.
                let col = px((self.widest_chars as f32 + SLACK) * host.font.advance * host.font.size);
                div()
                    .flex()
                    .items_center()
                    .h(px(ROW_H))
                    .child(self.cell(*old, Column::Old, col, theme))
                    .child(
                        div()
                            .flex_none()
                            .w(px(RULE_W))
                            .h(px(ROW_H))
                            .bg(rgb(theme.diff.gutter_fg)),
                    )
                    .child(self.cell(*new, Column::New, col, theme))
                    .into_any_element()
            }
        }
    }
}

impl SplitRows {
    /// One column of one row: gutter, sign, text — the built-in's anatomy at
    /// half the width, so the eye finds the same things in the same order.
    fn cell(
        &self,
        line: Option<u32>,
        column: Column,
        col: Pixels,
        theme: &Theme,
    ) -> AnyElement {
        let p = &theme.diff;
        let Some(line) = line.map(|i| &self.lines[i as usize]) else {
            // Nothing opposite: a flat, darker block, so a run of them reads as
            // a hole in the column rather than as unchanged content.
            return div()
                .flex_none()
                .w(px(GUTTER_W + SIGN_W) + col)
                .h(px(ROW_H))
                .bg(rgb(p.absent_bg))
                .into_any_element();
        };
        let (bg, fg, sign) = line_colors(line.kind, p);
        let no = match column {
            Column::Old => &line.old_no,
            Column::New => &line.new_no,
        };
        div()
            .flex()
            .flex_none()
            .items_center()
            .h(px(ROW_H))
            .w(px(GUTTER_W + SIGN_W) + col)
            .bg(rgb(bg))
            .child(div().flex_none().w(px(GUTTER_W)).pl_2().text_color(rgb(p.gutter_fg)).child(no.clone()))
            .child(div().flex_none().w(px(SIGN_W)).text_color(rgb(fg)).child(sign))
            .child(
                div().flex_none().text_color(rgb(fg)).child(
                    StyledText::new(line.text.clone()).with_highlights(runs(
                        &line.text,
                        &line.tokens,
                        &line.spans,
                        theme,
                        line.kind,
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
    use super::{Row, Rows, SplitRows};
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
            .map(|i| r.width(i))
            .collect();
        assert!(widths.windows(2).all(|w| w[0] == w[1]), "{widths:?}");
        assert!(widths[0] > 0);
    }

    #[test]
    fn it_claims_every_path_because_it_replaces_the_fallback() {
        let r = SplitRows::default();
        for p in ["a.rs", "b.md", "no-extension", "weird.xyz"] {
            assert!(r.claims(p));
        }
    }
}
