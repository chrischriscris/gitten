//! A `.md` file's diff, drawn as the document rather than as the source.
//!
//! A [`Rows`] implementation and nothing more: it claims markdown paths, takes
//! the same prepared lines the built-in takes, and draws them with the markers
//! gone — `## ` off the front and the row a size larger, `**` off a word that is
//! now simply bold, a link down to its text. The structural work is
//! [`plait_core::markdown`]; what is here is pixels.
//!
//! # What fixed row height buys and costs
//!
//! `uniform_list` is the only reason a 700k-row diff scrolls, and it wants every
//! row the same height. So this is a *rendered row*, not a rendered document: a
//! heading can be bigger but not much bigger, a blank line still costs a full
//! row, and a code block cannot have a block background of its own. A rendered
//! *document*, reflowed and variably tall, wants a pane, which does not exist —
//! see `docs/decisions/0006-row-seam-without-boxing.md`.
//!
//! Prose does wrap, though, and that is the one thing here that gets closest to
//! being a document: a paragraph too wide for the window continues on the row
//! below at the same indent, under its own text rather than under its bullet.
//! What makes it fit inside a fixed row height is that a wrapped line is *more
//! rows*, never a taller one — `docs/decisions/0017`.
//!
//! This is also the only presentation whose column budget differs per row, and
//! [`MarkdownRows::budget`] is why: a bar, three levels of indent and a bullet
//! are real pixels, an 18px heading in a 14px body holds a fifth fewer
//! characters, and a table must not break at all.
//!
//! Within that, three devices do the work:
//!
//! - **Size** for headings, set on the row because a run list cannot carry it —
//!   GPUI's `HighlightStyle` is documented as "uniformly sized text" and has no
//!   font size field. That single fact is why headings scale per row and inline
//!   markup varies only weight, slant, colour and underline.
//! - **A left bar** for the blocks that group rows: a fenced block and a
//!   blockquote. Not a background, because a row's background in a diff means
//!   added or removed and that is the one thing a diff may not give up.
//! - **Furniture** for the markers that were removed: a bullet glyph, a rule,
//!   the fence's language. `&'static str` glyphs and coloured divs, so none of
//!   it allocates on the render path.
//!
//! # Cost
//!
//! The same as the built-in, per frame: one `StyledText` and one run list per
//! visible row, through the same `runs` merge. Everything markdown-specific was
//! decided at load and is a `Copy` field read out of a `Vec`.

use super::diff::{
    column_at, columns, file_header, header_hit, hunk_header, line_colors, num, number,
    number_or_blank, runs, selected, slice, Hit, Rows, ROW_H, PAD, SIGN_W, TEXT_CHROME,
};
use gpui::*;
use plait_core::host::Host;
use plait_core::markdown::{lay_out, Block, Layout};
use plait_core::select::Selected;
use plait_core::syntax::Token;
use plait_core::runs::surfaces;
use plait_core::theme::Rgb;
use plait_core::wrap::{Wrap, Wrapped};
use plait_core::{LineKind, Span};

/// How a rendered markdown row is proportioned. A struct rather than constants
/// because these are the numbers someone will want to disagree with — and rule 1
/// says a built-in may not hold a knob an extension cannot reach.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Metrics {
    /// Point size per heading level, `[0]` being `#`.
    ///
    /// The ceiling is not taste, it is arithmetic: a row is [`ROW_H`] tall and a
    /// glyph needs roughly 1.2× its point size of line box, so anything past
    /// about 18px is drawing outside its row and clipping into its neighbour.
    /// Levels 4–6 land on the body size and separate themselves by weight, which
    /// is what most typographic scales do at that depth anyway.
    pub heading: [f32; 6],
    /// One step of list indent, in pixels.
    pub indent: f32,
    /// Width of the bar drawn beside a quote or a fenced block.
    pub bar: f32,
    /// Bullet glyph per depth; the last one repeats.
    pub bullets: &'static [&'static str],
    /// What `core` is allowed to assume about the face this draws in.
    ///
    /// Derived from `host.font` by [`Metrics::for_font`] rather than assumed,
    /// because it is the font that decides: a monospaced face gets its table
    /// columns padded into a grid, and a proportional one gets its tables left
    /// as written instead of misaligned by a fraction of a glyph per cell.
    pub layout: Layout,
}

impl Default for Metrics {
    fn default() -> Self {
        Self {
            heading: [18.0, 16.5, 15.0, 14.0, 14.0, 14.0],
            indent: 14.0,
            bar: 2.0,
            bullets: &["•", "◦", "▪", "·"],
            layout: Layout::monospaced(),
        }
    }
}

impl Metrics {
    /// The metrics for a given font.
    ///
    /// Two things follow from the face and may not be guessed: whether tables can
    /// be aligned at all, and how large a heading may be. The heading scale is
    /// capped by [`ROW_H`] and not by the font, but it is *relative* to the body
    /// size, so a larger font has to give up the top of the scale rather than
    /// draw outside its row.
    pub fn for_font(font: &plait_core::font::Font) -> Self {
        // A glyph needs roughly 1.2x its point size of line box, so this is the
        // largest a row can hold. At the default 14px body size it lands at 18px,
        // which is where the scale was pinned when it was a constant.
        let ceiling = ROW_H / 1.2;
        let scale = [1.30, 1.18, 1.07, 1.0, 1.0, 1.0];
        let mut heading = [font.size; 6];
        for (h, factor) in heading.iter_mut().zip(scale) {
            *h = font.scaled(factor).min(ceiling);
        }
        Self {
            heading,
            layout: Layout { monospaced: font.monospaced, ..Default::default() },
            ..Self::default()
        }
    }

    fn size(&self, level: u8) -> f32 {
        self.heading[(level.max(1).min(6) - 1) as usize]
    }

    fn bullet(&self, depth: u8) -> &'static str {
        let last = self.bullets.len().saturating_sub(1);
        self.bullets.get(depth as usize).copied().unwrap_or(self.bullets[last])
    }
}

/// One row. `Copy` fields and `SharedString`s only: `render` runs per visible row
/// per redraw, so nothing in here may be worth allocating at that point.
enum Row {
    File { path: SharedString, adds: usize, dels: usize },
    Hunk(SharedString),
    Line {
        block: Block,
        kind: LineKind,
        moved: bool,
        old: SharedString,
        new: SharedString,
        text: SharedString,
        spans: Vec<Span>,
        tokens: Vec<Token>,
    },
}

/// The rendered-markdown presentation. Register it after the built-in and it
/// takes every `.md`, `.markdown` and `.mdx` file in the diff.
pub struct MarkdownRows {
    rows: Vec<Row>,
    metrics: Metrics,
    /// Which extensions to claim. Owned rather than hardcoded so the same
    /// implementation can be pointed at `.mdown` or `.txt` without editing it.
    extensions: Vec<String>,
    laid_out: usize,
    /// Where each row's text breaks. Indexed by row, like the built-in's.
    wrapped: Wrapped,
    /// Every distinct [`Block`] in the rows, collected as they were built.
    ///
    /// This is what makes a resize cheap here. The other two presentations have
    /// one column budget and compare it; this one has a budget per *row*, so
    /// there is no single number — but there are only ever a couple of dozen
    /// distinct blocks in a document, and the budget is a pure function of the
    /// block and the width. Comparing the budgets of these is therefore exactly
    /// as good as comparing all of them, and does not touch a row.
    ///
    /// Comparing the raw width instead is the obvious thing and it rescans every
    /// line of the diff on every pixel of a drag — 3 ms a frame on `md.diff`,
    /// for an answer that did not change.
    blocks: Vec<Block>,
    /// The budgets `blocks` had when `wrapped` was built, and the policy that
    /// built it.
    budgets: Vec<usize>,
    wrap: &'static str,
    /// The width the budgets were computed for, kept so a row can ask what its
    /// own column is. Only one thing needs it — a thematic break is drawn as a
    /// rule and a rule has to be as wide as the text it replaces — and the
    /// alternative is a constant, which was 320 pixels regardless of the window.
    width: f32,
}

impl Default for MarkdownRows {
    fn default() -> Self {
        // The same three the `Markdown` highlighter is routed for, so a file that
        // gets markdown tokens gets a markdown row. Diverging here would give one
        // of them prose colours in a source presentation.
        Self::new(Metrics::default(), &["md", "markdown", "mdx"])
    }
}

impl MarkdownRows {
    /// Both knobs at once: how a row is proportioned, and which paths this
    /// presentation takes. `Default` is the shipped answer to both.
    pub fn new(metrics: Metrics, extensions: &[&str]) -> Self {
        Self {
            rows: Vec::new(),
            metrics,
            extensions: extensions.iter().map(|s| s.to_string()).collect(),
            laid_out: 0,
            wrapped: Wrapped::default(),
            blocks: Vec::new(),
            budgets: Vec::new(),
            wrap: "",
            width: 0.0,
        }
    }

    /// How many characters of `row` fit in `width` pixels.
    ///
    /// Every other presentation has one budget for the whole diff; this one has a
    /// budget per row, and that is the reason [`Wrapped::build`] takes the column
    /// count per line rather than once. Two things move it:
    ///
    /// - **What is drawn in front of the text.** A quote bar, three levels of
    ///   list indent and a bullet are real pixels, and a wrap that ignored them
    ///   would overflow by exactly as many as it ignored.
    /// - **How large the text is.** A `#` heading is drawn at 18px where the body
    ///   is 14, so the same row holds a fifth fewer characters. Nothing else in
    ///   the app has two type sizes in one list.
    ///
    /// A table gets 0, which [`Wrapped`] reads as "never break this". Its grid is
    /// aligned character by character with the rows above and below it, and a
    /// break shears it — the same reason `align_tables` measures per run.
    fn budget(&self, block: Block, width: f32, host: &Host) -> usize {
        if block.is_table() {
            return 0;
        }
        columns(width, TEXT_CHROME + self.furniture(block), self.size(block, host), host)
    }

    /// How many pixels of furniture sit between the sign column and the text: a
    /// bar, some indent steps, a bullet.
    ///
    /// One function because two callers must agree about it — the wrap budget
    /// above and the caret in `hit`. A table gets none of it: its grid is aligned
    /// character by character against the rows around it, so it is drawn with the
    /// gutter and then nothing at all.
    fn furniture(&self, block: Block) -> f32 {
        if block.is_table() {
            return 0.0;
        }
        let m = &self.metrics;
        let bar = matches!(block, Block::Quote(_) | Block::Fence | Block::Code);
        let bullet = matches!(block, Block::Bullet(_));
        m.indent * (bar as u8 + block.depth() + bullet as u8) as f32
    }

    /// How large this block's text is drawn. A heading is the only thing in the
    /// app with a type size of its own, and it is why a column budget and a
    /// caret are both per row here rather than per diff.
    fn size(&self, block: Block, host: &Host) -> f32 {
        match block {
            Block::Heading(level) => self.metrics.size(level),
            _ => host.font.size,
        }
    }
}

impl Rows for MarkdownRows {
    fn claims(&self, path: &str) -> bool {
        // `rsplit` on the whole path, not the file name: a path with no dot in
        // its last segment must not pick up a dot from a parent directory.
        let name = path.rsplit('/').next().unwrap_or(path);
        name.rsplit_once('.').is_some_and(|(_, ext)| self.extensions.iter().any(|e| e == ext))
    }

    fn len(&self) -> usize {
        self.rows.len()
    }

    fn rows(&self, index: usize) -> usize {
        self.wrapped.rows(index)
    }

    fn reflow(&mut self, width: f32, host: &Host, wrap: &dyn Wrap) -> bool {
        let budgets: Vec<usize> =
            self.blocks.iter().map(|b| self.budget(*b, width, host)).collect();
        if budgets == self.budgets && wrap.name() == self.wrap {
            return false;
        }
        self.budgets = budgets;
        self.wrap = wrap.name();
        self.width = width;
        self.wrapped = Wrapped::build(
            self.rows.iter().map(|r| match r {
                Row::Line { block, text, .. } => {
                    (text.as_ref(), self.budget(*block, width, host))
                }
                // A header is drawn by the built-in's own function at the built-in's
                // own width, and a path is not prose. One row, always.
                _ => ("", 0),
            }),
            wrap,
        );
        true
    }

    fn build(&mut self, mut f: plait_core::prepared::File) {
        self.rows.push(Row::File {
            path: std::mem::take(&mut f.path).into(),
            adds: f.adds,
            dels: f.dels,
        });
        for mut h in f.hunks {
            self.rows.push(Row::Hunk(std::mem::take(&mut h.header).into()));
            // Per hunk, because that is the largest unit whose block structure is
            // knowable: a fence opened in one hunk and closed in another has
            // everything between them missing from the diff entirely.
            let blocks = lay_out(&mut h.lines, &self.metrics.layout);
            self.laid_out += blocks.len();
            for (l, block) in h.lines.into_iter().zip(blocks) {
                if !self.blocks.contains(&block) {
                    self.blocks.push(block);
                }
                self.rows.push(Row::Line {
                    block,
                    kind: l.kind,
                    moved: l.moved,
                    old: number(l.old_no),
                    new: number(l.new_no),
                    text: l.text.into(),
                    spans: l.spans,
                    tokens: l.tokens,
                });
            }
        }
    }

    fn width(&self, index: usize, seg: usize) -> usize {
        match &self.rows[index] {
            // Indent steps cost roughly a character each, and a heading's glyphs
            // are wider than the body's — both approximations, and both only feed
            // the one row `uniform_list` measures to size its scroll width.
            Row::Line { text, block, .. } => {
                let scale = match block {
                    Block::Heading(l) => self.metrics.size(*l) / 14.0,
                    _ => 1.0,
                };
                // `chars`, not `len`: a table row is full of three-byte box
                // drawing and would otherwise measure three times too wide and
                // win the widest-row contest for the whole diff.
                let shown = text[self.wrapped.range(index, seg, text)].trim_end();
                (shown.chars().count() as f32 * scale) as usize + block.depth() as usize + 2
            }
            Row::Hunk(h) => h.chars().count(),
            Row::File { path, .. } => path.chars().count(),
        }
    }

    fn report(&self) -> String {
        if self.laid_out == 0 {
            String::new()
        } else {
            format!("markdown {} rows", self.laid_out)
        }
    }

    /// The gutter, then this block's own furniture, then text at this block's own
    /// size. Nothing else in the app has two type sizes in one list, which is why
    /// this is the one presentation whose caret arithmetic is per row.
    fn hit(&self, index: usize, seg: usize, x: f32, host: &Host) -> Option<Hit> {
        Some(match self.rows.get(index)? {
            Row::File { path, .. } => header_hit(path, x, host),
            Row::Hunk(h) => header_hit(h, x, host),
            Row::Line { block, text, .. } => {
                let at = self.wrapped.range(index, seg, text);
                let from = TEXT_CHROME - PAD + self.furniture(*block);
                let off = at.start
                    + column_at(&text[at.clone()], x - from, self.size(*block, host), host);
                Hit { part: 0, off }
            }
        })
    }

    /// The source line, which is also what is drawn: the markers this
    /// presentation replaces were taken off the text by `lay_out`, so a copy
    /// yields what was on screen rather than a bullet nobody can see.
    fn selectable(&self, index: usize, _part: u16) -> Option<&str> {
        Some(match self.rows.get(index)? {
            Row::Line { text, .. } => text.as_ref(),
            Row::Hunk(h) => h.as_ref(),
            Row::File { path, .. } => path.as_ref(),
        })
    }

    fn render(&self, index: usize, seg: usize, host: &Host, sel: Option<Selected>) -> AnyElement {
        let theme = &host.theme;
        match &self.rows[index] {
            Row::File { path, adds, dels } => file_header(path, *adds, *dels, theme, sel),
            Row::Hunk(header) => hunk_header(header, theme, sel),
            Row::Line { block, kind, moved, old, new, text, spans, tokens } => {
                let at = self.wrapped.range(index, seg, text);
                self.line(
                    *block, *kind, *moved, old, new, text, at, seg, spans, tokens, host, sel,
                )
            }
        }
    }
}

impl MarkdownRows {
    #[allow(clippy::too_many_arguments)]
    fn line(
        &self,
        block: Block,
        kind: LineKind,
        moved: bool,
        old: &SharedString,
        new: &SharedString,
        full: &SharedString,
        at: std::ops::Range<usize>,
        seg: usize,
        spans: &[Span],
        tokens: &[Token],
        host: &Host,
        sel: Option<Selected>,
    ) -> AnyElement {
        let theme = &host.theme;
        let m = &self.metrics;
        let md = &theme.markdown;
        let (bg, fg, sign) = line_colors(kind, moved, &theme.diff);
        let surface = surfaces(kind, moved).0;
        // A continuation of a wrapped line: the same furniture, so a wrapped
        // bullet stays indented under its own text and a wrapped quote keeps its
        // bar, and no number and no sign, as everywhere else.
        let blank = seg > 0;
        let text = &slice(full, &at);

        // The gutter is the built-in's, unchanged. Whatever the row does with the
        // text, the two line numbers and the sign have to sit where they sit on
        // every other row of the diff or the eye loses the column.
        let row = div()
            .flex()
            .items_center()
            .h(px(ROW_H))
            .px_4()
            .bg(rgb(bg))
            .child(num(number_or_blank(old, blank), theme.gutter_on(surface)))
            .child(num(number_or_blank(new, blank), theme.gutter_on(surface)))
            .child(
                div()
                    .flex_none()
                    .w(px(SIGN_W))
                    .text_color(rgb(fg))
                    .child(if blank { " " } else { sign }),
            );

        // A table row's grid lives inside its text, aligned character by character
        // against the rows above and below it. Anything drawn in front of one row
        // and not the next would shear the grid, so a table gets the gutter and
        // then nothing: no bar, no indent, no glyph.
        if block.is_table() {
            let body = div().flex_none().text_color(rgb(fg)).child(
                StyledText::new(text.clone()).with_highlights(runs(
                    at,
                    tokens,
                    spans,
                    theme,
                    kind,
                    moved,
                    selected(sel, 0, full.len()),
                )),
            );
            // The grid is structure, not content, and a separator row is nothing
            // but grid.
            let body = if block == Block::TableRule {
                body.text_color(rgb(md.rule))
            } else {
                body
            };
            return row.child(body).into_any_element();
        }

        // A rule draws no text: the dashes were the drawing, so they are replaced
        // by the thing they were drawing.
        if block == Block::Rule {
            // As wide as the text it stands in for, which is the row's own wrap
            // budget: this used to be 320 pixels whatever the window was doing,
            // so a break was a stub in a wide window and an overhang in a narrow
            // one. The budget is already net of everything drawn in front of the
            // text, so it is the column exactly.
            let w = self.budget(block, self.width, host) as f32 * host.font.char_width();
            return row
                .child(
                    div()
                        .flex_none()
                        .w(px(w))
                        .h(px(1.))
                        .bg(rgb(md.rule))
                        .into_any_element(),
                )
                .into_any_element();
        }
        if block == Block::Blank {
            return row.into_any_element();
        }

        // Full row height, so a fenced block of nine lines is one rule nine rows
        // long. At `ROW_H - 6` it was a 16-pixel dash with a 6-pixel gap above
        // and below, which is a ladder rather than a bar — and grouping a run of
        // rows is the whole job.
        let bar = |color: Rgb| {
            div().flex_none().w(px(m.bar)).h(px(ROW_H)).mr(px(m.indent - m.bar)).bg(rgb(color))
        };

        let row = match block {
            Block::Quote(_) => row.child(bar(md.quote_bar)),
            Block::Fence | Block::Code => row.child(bar(md.code_bar)),
            _ => row,
        };

        // Indent, then the marker's replacement, then the text. Separate elements
        // rather than padding inside the `StyledText` so the glyph can carry its
        // own colour without becoming a run in the merge.
        let depth = block.depth();
        let row = if depth > 0 {
            row.child(div().flex_none().w(px(depth as f32 * m.indent)))
        } else {
            row
        };
        let row = match block {
            // The glyph on the first row and its width on every one, so a
            // wrapped item's continuation lines up under its own text rather
            // than under its bullet.
            Block::Bullet(d) => row.child(
                div()
                    .flex_none()
                    .w(px(m.indent))
                    .text_color(rgb(md.marker))
                    .child(if blank { " " } else { m.bullet(d) }),
            ),
            _ => row,
        };

        // An empty fence line is a bare ``` with no language: the bar beside it
        // already says a block opened, so there is nothing left to draw.
        if text.is_empty() {
            return row.into_any_element();
        }

        let body = div().flex_none().text_color(rgb(fg)).child(
            StyledText::new(text.clone()).with_highlights(runs(
                at,
                tokens,
                spans,
                theme,
                kind,
                moved,
                selected(sel, 0, full.len()),
            )),
        );
        let body = match block {
            Block::Heading(level) => body.text_size(px(m.size(level))).font_weight(FontWeight::BOLD),
            // A fence's language label is punctuation the reader should be able
            // to skip. A table's pipes are too, but a table is drawn verbatim —
            // see the note on `Block::Table` in `plait_core::markdown`.
            Block::Fence => body.text_color(rgb(md.marker)),
            _ => body,
        };
        row.child(body).into_any_element()
    }
}

#[cfg(test)]
mod tests {
    // By name, not a glob: `use gpui::*` in the parent shadows `#[test]`.
    use super::{MarkdownRows, Metrics, Row};
    use crate::views::diff::{Diff, Rows, TextRows, PAD, TEXT_CHROME};
    use plait_core::host::Host;
    use plait_core::markdown::Block;
    use plait_core::prepared::prepare;
    use plait_core::syntax::Kind;
    use plait_core::{parse_unified_diff, LineKind};
    use std::rc::Rc;

    /// A real diff body. The lone `\x20` lines are blank *context* lines: a diff
    /// marks those with a single space, and `parse_unified_diff` drops a line
    /// that has no marker at all. Escaped so nothing can strip the space.
    const DOC: &str = "\
diff --git a/README.md b/README.md
@@ -1,9 +1,9 @@
 # plait
\x20
-A git client with **bold** claims and [a link](https://example.com/long/url).
+A git client with **bolder** claims and [a link](https://example.com/other/url).
\x20
 - one
   - nested
\x20
 > quoted
 ```rust
 let x = 1;
 ```
";

    fn built(src: &str) -> MarkdownRows {
        let host = Host::new();
        let mut p = prepare(&parse_unified_diff(src), &host.syntax, 2000);
        let mut r = MarkdownRows::default();
        r.build(p.files.remove(0));
        r
    }

    /// The blocks the built rows ended up with, lines only.
    fn blocks(r: &MarkdownRows) -> Vec<Block> {
        r.rows
            .iter()
            .filter_map(|row| match row {
                super::Row::Line { block, .. } => Some(*block),
                _ => None,
            })
            .collect()
    }

    fn texts(r: &MarkdownRows) -> Vec<String> {
        r.rows
            .iter()
            .filter_map(|row| match row {
                super::Row::Line { text, .. } => Some(text.to_string()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn it_claims_markdown_and_nothing_else() {
        let r = MarkdownRows::default();
        for p in ["README.md", "docs/a.markdown", "x/y/z.mdx", "a.b.md"] {
            assert!(r.claims(p), "{p}");
        }
        for p in ["a.rs", "Cargo.lock", "no-extension", "weird.xyz", "md", ".md.rs"] {
            assert!(!r.claims(p), "{p}");
        }
    }

    #[test]
    fn a_dot_in_a_parent_directory_is_not_an_extension() {
        // `v1.2/CHANGELOG` would claim as `2/CHANGELOG` if the split ran over the
        // whole path instead of the last segment.
        let r = MarkdownRows::default();
        assert!(!r.claims("v1.md/CHANGELOG"));
        assert!(r.claims("v1.2/CHANGELOG.md"));
    }

    #[test]
    fn what_it_claims_is_configurable() {
        let r = MarkdownRows::new(Metrics::default(), &["mdown", "txt"]);
        assert!(r.claims("a.mdown") && r.claims("b.txt"));
        assert!(!r.claims("c.md"), "the default list was replaced, not extended");
    }

    #[test]
    fn the_metrics_are_configurable_too() {
        // Rule 1: the numbers a built-in draws with are not the built-in's to
        // keep. Anything an extension cannot reach is not a knob.
        let tight = Metrics { heading: [14.0; 6], indent: 8.0, ..Metrics::default() };
        let r = MarkdownRows::new(tight, &["md"]);
        assert_eq!(r.metrics.size(1), 14.0);
        assert_eq!(r.metrics.indent, 8.0);
    }

    #[test]
    fn it_builds_a_row_per_line_plus_the_headers() {
        let r = built(DOC);
        // Same row count as the built-in: a presentation may not add or drop
        // rows, because the line numbers in the gutter have to keep adding up.
        let host = Host::new();
        let mut p = prepare(&parse_unified_diff(DOC), &host.syntax, 2000);
        let mut t = TextRows::default();
        t.build(p.files.remove(0));
        assert_eq!(r.len(), t.len());
        assert!((0..r.len()).all(|i| r.width(i, 0) > 0));
    }

    #[test]
    fn the_document_structure_reaches_the_rows() {
        let r = built(DOC);
        let b = blocks(&r);
        assert_eq!(b[0], Block::Heading(1));
        assert_eq!(b[1], Block::Blank);
        assert_eq!(b[2], Block::Paragraph, "the removed prose line");
        assert_eq!(b[3], Block::Paragraph, "the added prose line");
        assert!(b.contains(&Block::Bullet(0)));
        assert!(b.contains(&Block::Bullet(1)));
        assert!(b.contains(&Block::Quote(1)));
        assert!(b.contains(&Block::Fence));
        assert!(b.contains(&Block::Code));
    }

    #[test]
    fn the_markers_are_gone_from_the_text_that_will_be_drawn() {
        let t = texts(&built(DOC));
        assert!(t.contains(&"plait".to_string()), "hashes survived: {t:?}");
        assert!(t.iter().any(|l| l == "one"), "a bullet marker survived: {t:?}");
        assert!(t.iter().any(|l| l == "quoted"), "a quote marker survived: {t:?}");
        assert!(t.iter().any(|l| l == "rust"), "a fence kept more than its language");
        assert!(
            t.iter().any(|l| l.contains("bolder claims and a link")),
            "inline markup survived: {t:?}"
        );
        assert!(!t.iter().any(|l| l.contains("https://")), "a url survived: {t:?}");
        assert!(t.iter().any(|l| l == "let x = 1;"), "a fence body was altered");
    }

    #[test]
    fn a_heading_row_keeps_its_heading_token_over_its_words() {
        // The row draws the token, so if the cut left it pointing at the wrong
        // bytes the title comes out in the body colour at the wrong size.
        let r = built(DOC);
        let (text, tokens) = r
            .rows
            .iter()
            .find_map(|row| match row {
                super::Row::Line { block: Block::Heading(_), text, tokens, .. } => {
                    Some((text.clone(), tokens.clone()))
                }
                _ => None,
            })
            .expect("a heading row");
        let t = tokens.iter().find(|t| t.kind == Kind::Heading).expect("a heading token");
        assert_eq!(&text[t.start..t.end], "plait");
    }

    #[test]
    fn every_range_indexes_the_text_the_row_will_draw() {
        // The one invariant that turns into a panic in GPUI's text layout rather
        // than into a wrong colour.
        let r = built(DOC);
        for row in &r.rows {
            let super::Row::Line { text, tokens, spans, .. } = row else { continue };
            for t in tokens {
                assert!(t.end <= text.len(), "token {t:?} outside {text:?}");
                assert!(text.is_char_boundary(t.start) && text.is_char_boundary(t.end));
            }
            for s in spans {
                assert!(s.end <= text.len(), "span {s:?} outside {text:?}");
            }
        }
    }

    #[test]
    fn the_changed_word_still_marks_the_word_that_changed() {
        // The intraline spans were computed on the source, so they have to have
        // moved with the text. This is the pair from DOC: bold -> bolder.
        let r = built(DOC);
        let marked: Vec<String> = r
            .rows
            .iter()
            .filter_map(|row| match row {
                super::Row::Line { kind: LineKind::Added, text, spans, .. } if !spans.is_empty() => {
                    Some(spans.iter().map(|s| text[s.start..s.end].to_string()).collect::<Vec<_>>())
                }
                _ => None,
            })
            .flatten()
            .collect();
        assert!(!marked.is_empty(), "no changed words survived the layout");
        assert!(
            marked.iter().any(|m| m.contains("bolder") || m.contains("other")),
            "spans point at {marked:?}"
        );
    }

    #[test]
    fn a_table_reaches_the_rows_as_a_grid() {
        let r = built(TABLE);
        let t = texts(&r);
        assert_eq!(t[0], "│ stage        │ time   │");
        assert_eq!(t[1], "├──────────────┼────────┤");
        assert_eq!(t[2], "│ parse log    │ 466 ms │");
        assert_eq!(t[3], "│ assign lanes │ 301 ms │");
        let b = blocks(&r);
        assert_eq!(b[1], Block::TableRule);
        assert!(b[0].is_table() && b[2].is_table());
    }

    #[test]
    fn a_table_row_measures_in_columns_not_bytes() {
        // Box drawing is three bytes a glyph. Measuring a table row by `len`
        // makes it three times too wide and it wins `with_width_from_item` for
        // the whole diff, which scrolls sideways into empty space.
        let r = built(TABLE);
        let widest = (0..r.len()).max_by_key(|i| r.width(*i, 0)).unwrap();
        let table_row = r.width(2, 0);
        assert!(
            table_row < 40,
            "a 25-column table row measured {table_row}; widest row is {widest}"
        );
    }

    #[test]
    fn metrics_derived_from_the_default_font_match_the_constants_they_replaced() {
        // The scale used to be pixel constants. Deriving it must not move it.
        let m = Metrics::for_font(&plait_core::font::Font::default());
        assert!((m.size(1) - 18.0).abs() < 0.35, "h1 moved to {}", m.size(1));
        assert!((m.size(2) - 16.5).abs() < 0.35, "h2 moved to {}", m.size(2));
        assert!((m.size(4) - 14.0).abs() < 0.01, "h4 is not the body size");
        assert!(m.layout.monospaced, "the default font is monospaced");
    }

    #[test]
    fn a_bigger_font_gives_up_the_top_of_the_scale_rather_than_the_row() {
        // The constraint is the row, not the font: at a 20px body size a 1.3x h1
        // would be 26px and clip into the row below, so it is capped instead.
        let big = plait_core::font::Font { size: 20.0, ..plait_core::font::Font::default() };
        let m = Metrics::for_font(&big);
        for level in 1..=6u8 {
            assert!(
                m.size(level) * 1.2 <= super::ROW_H + 0.01,
                "h{level} at {}px does not fit ROW_H",
                m.size(level)
            );
        }
        assert!((1..6u8).all(|l| m.size(l) >= m.size(l + 1)), "scale is not monotonic");
    }

    #[test]
    fn a_proportional_font_turns_table_padding_off() {
        // The whole reason `monospaced` is on the font rather than assumed here.
        let prop = plait_core::font::Font {
            monospaced: false,
            ..plait_core::font::Font::default()
        };
        assert!(!Metrics::for_font(&prop).layout.monospaced);
    }

    #[test]
    fn metrics_stay_inside_a_row() {
        // The constraint that decides the whole design: a glyph needs about 1.2x
        // its point size of line box, and a row is ROW_H tall. A heading scale
        // that breaks this clips into the row below.
        let m = Metrics::default();
        for level in 1..=6u8 {
            assert!(m.size(level) * 1.2 <= super::ROW_H, "h{level} at {}px", m.size(level));
        }
        // Monotonic, or level three outranks level two.
        assert!((1..6u8).all(|l| m.size(l) >= m.size(l + 1)));
        // Out-of-range levels answer rather than panicking on the render path.
        assert_eq!(m.size(0), m.size(1));
        assert_eq!(m.size(9), m.size(6));
        assert_eq!(m.bullet(99), "·");
    }

    const TABLE: &str = "\
diff --git a/docs/m.md b/docs/m.md
@@ -1,4 +1,4 @@
 | stage | time |
 |---|---|
 | parse log | 466 ms |
 | assign lanes | 301 ms |
";

    const MIXED: &str = "\
diff --git a/a.rs b/a.rs
@@ -1,1 +1,1 @@
-let x = 1;
+let x = 2;
diff --git a/README.md b/README.md
@@ -1,1 +1,1 @@
-# old
+# new
";

    #[test]
    fn it_takes_the_markdown_and_leaves_the_code_to_the_built_in() {
        let host = Rc::new(Host::new());
        let diff = Diff::with_renderers(
            parse_unified_diff(MIXED),
            host,
            vec![Box::new(TextRows::default()), Box::new(MarkdownRows::default())],
        );
        // Two headers and a removed/added pair per file, every row accounted for
        // exactly once, and only the markdown file's two lines were laid out.
        assert_eq!(diff.total(), 8);
        assert!(diff.load.contains("markdown 2 rows"), "report missing: {}", diff.load);
    }

    // ------------------------------------------------------------- wrapping

    /// Prose long enough to wrap, at three different budgets: a heading (drawn
    /// larger, so fewer columns), a nested bullet (indented, so fewer again) and
    /// a table (never).
    const PROSE: &str = "\
diff --git a/a.md b/a.md
@@ -1,5 +1,5 @@
-# A heading long enough that it has to break somewhere sensible
+# A heading long enough that it has to break somewhere else instead
   - a nested bullet whose text runs on well past the edge of any window
 | alpha | beta | gamma |
 | --- | --- | --- |
 | one that is long | two | three |
";

    fn reflowed(src: &str, cols: usize) -> (MarkdownRows, std::rc::Rc<Host>) {
        let host = std::rc::Rc::new(Host::new());
        let mut p = prepare(&parse_unified_diff(src), &host.syntax, 2000);
        let mut r = MarkdownRows::new(Metrics::for_font(&host.font), &["md"]);
        r.build(p.files.remove(0));
        let f = &host.font;
        r.reflow(
            crate::views::diff::TEXT_CHROME + (cols as f32 + 0.5) * f.size * f.advance,
            &host,
            host.wrap.current(),
        );
        (r, host)
    }

    #[test]
    fn a_resize_that_changes_no_row_s_budget_costs_nothing() {
        // The reason the distinct blocks are collected at build. Comparing the
        // raw width instead rescans every line of the diff on every pixel of a
        // drag, for an answer that did not change.
        let (mut r, host) = reflowed(PROSE, 40);
        let f = &host.font;
        let w = crate::views::diff::TEXT_CHROME + 40.5 * f.size * f.advance;
        assert!(!r.reflow(w, &host, host.wrap.current()), "the same width rebuilt");
        assert!(!r.reflow(w + 0.4, &host, host.wrap.current()), "half a pixel rebuilt");
        assert!(r.reflow(w + 200.0, &host, host.wrap.current()), "24 characters did not");

        // And the blocks it collected are the ones the document has, once each —
        // the list is what stands in for every row, so a duplicate is wasted work
        // on every frame of a drag and a miss is a row wrapped to a stale budget.
        assert!(r.blocks.len() >= 3, "{:?}", r.blocks);
        for (i, b) in r.blocks.iter().enumerate() {
            assert!(!r.blocks[..i].contains(b), "{b:?} collected twice: {:?}", r.blocks);
        }
        for i in 0..r.len() {
            if let Row::Line { block, .. } = &r.rows[i] {
                assert!(r.blocks.contains(block), "{block:?} was never collected");
            }
        }
    }

    #[test]
    fn a_table_never_wraps_however_narrow_the_window() {
        // Its grid is aligned character by character against the rows above and
        // below, so a break shears it. `budget` returns 0 and `Wrapped` reads
        // that as "leave this line alone".
        let (r, _) = reflowed(PROSE, 12);
        let mut tables = 0;
        for i in 0..r.len() {
            if let Row::Line { block, .. } = &r.rows[i] {
                if block.is_table() {
                    tables += 1;
                    assert_eq!(r.rows(i), 1, "a table row wrapped");
                }
            }
        }
        assert_eq!(tables, 3, "the fixture lost its table");
    }

    #[test]
    fn a_heading_gets_fewer_columns_than_the_body_it_sits_in() {
        // Nothing else in the app draws two type sizes in one list, and it is why
        // the column budget is per row rather than per diff: an 18px heading in a
        // 14px body holds a fifth fewer characters at the same width.
        let (r, host) = reflowed(PROSE, 40);
        let heading = r.budget(Block::Heading(1), 800.0, &host);
        let body = r.budget(Block::Paragraph, 800.0, &host);
        assert!(heading < body, "heading {heading} columns, body {body}");
    }

    #[test]
    fn what_is_drawn_in_front_of_a_line_comes_out_of_its_budget() {
        // A bar, three levels of indent and a bullet are real pixels. A wrap that
        // ignored them would overflow by exactly as many as it ignored.
        let (r, host) = reflowed(PROSE, 40);
        let plain = r.budget(Block::Paragraph, 800.0, &host);
        assert!(r.budget(Block::Bullet(2), 800.0, &host) < plain);
        assert!(r.budget(Block::Quote(1), 800.0, &host) < plain);
        assert!(r.budget(Block::Fence, 800.0, &host) < plain);
    }

    #[test]
    fn wrapping_does_not_change_how_many_lines_a_document_has() {
        // The rule from `docs/extending.md`, still holding: the gutter shows both
        // line numbers and they have to keep adding up, so wrapping may add rows
        // and may not add *lines*.
        let (r, _) = reflowed(PROSE, 20);
        let host = Host::new();
        let mut p = prepare(&parse_unified_diff(PROSE), &host.syntax, 2000);
        let mut t = TextRows::default();
        t.build(p.files.remove(0));
        assert_eq!(r.len(), t.len());
        assert!((0..r.len()).map(|i| r.rows(i)).sum::<usize>() > r.len(), "nothing wrapped");
    }

    #[test]
    fn a_file_with_no_hunks_still_produces_its_header() {
        let r = built("diff --git a/a.md b/a.md\n");
        assert_eq!(r.len(), 1);
        assert!(r.width(0, 0) > 0);
    }

    // ------------------------------------------------------------ selection

    #[test]
    fn the_caret_follows_the_furniture_the_row_drew() {
        // The only presentation whose text does not start at the same x on every
        // row: a bullet, an indent step and a quote bar are real pixels, and a
        // caret that ignored them would be a word or two off on exactly the rows
        // a reader is most likely to select.
        let host = Host::new();
        let r = built(DOC);
        let rows: Vec<(usize, Block)> = r
            .rows
            .iter()
            .enumerate()
            .filter_map(|(i, row)| match row {
                Row::Line { block, .. } => Some((i, *block)),
                _ => None,
            })
            .collect();
        let plain = rows.iter().find(|(_, b)| matches!(b, Block::Paragraph)).unwrap().0;
        let nested = rows.iter().filter(|(_, b)| matches!(b, Block::Bullet(_))).nth(1).unwrap().0;
        assert!(r.furniture(Block::Paragraph) < r.furniture(Block::Bullet(1)));

        // The same pixel column, two rows: the indented one starts later, so the
        // same x is fewer characters into its text.
        let x = TEXT_CHROME - PAD + 20.0 * host.font.size * host.font.advance;
        let flat = r.hit(plain, 0, x, &host).unwrap().off;
        let inset = r.hit(nested, 0, x, &host).unwrap().off;
        assert!(inset < flat, "the indent did not move the caret: {inset} vs {flat}");

        // And a click at the start of a row's own text is byte 0 of it, whatever
        // that row drew in front of itself.
        for (i, block) in &rows {
            let from = TEXT_CHROME - PAD + r.furniture(*block);
            assert_eq!(r.hit(*i, 0, from, &host).unwrap().off, 0, "row {i} {block:?}");
        }
    }

    #[test]
    fn a_heading_is_selected_by_the_size_it_is_drawn_at() {
        // A `#` heading is 18px where the body is 14, so the same pixel is a
        // different character. Measuring it at the body size is a caret that
        // drifts further right the longer the heading is.
        let host = Host::new();
        let r = built(DOC);
        let heading = r
            .rows
            .iter()
            .position(|row| matches!(row, Row::Line { block: Block::Heading(_), .. }))
            .expect("a heading");
        let text = r.selectable(heading, 0).unwrap().to_string();
        let end = TEXT_CHROME - PAD + text.chars().count() as f32 * r.metrics.size(1) * 0.602;
        assert_eq!(r.hit(heading, 0, end, &host).unwrap().off, text.len());
    }

    #[test]
    fn what_it_copies_is_what_it_drew() {
        // The markers are off the text by the time a row holds it, so a copy
        // yields the line as rendered rather than a bullet nobody can see.
        let r = built(DOC);
        let all: Vec<&str> = (0..r.len()).filter_map(|i| r.selectable(i, 0)).collect();
        assert_eq!(all.len(), r.len(), "a row with nothing to copy");
        assert!(all.contains(&"README.md"), "the file header is in it");
        assert!(all.iter().any(|t| t.contains("bolder")));
    }
}
