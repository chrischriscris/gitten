//! A `.md` file's diff, drawn as the document rather than as the source.
//!
//! A [`Rows`] implementation and nothing more: it claims markdown paths, takes
//! the same prepared lines the built-in takes, and draws them with the markers
//! gone — `## ` off the front and the row a size larger, `**` off a word that is
//! now simply bold, a link down to its text. The structural work, the row
//! store, the table flow and the wrap policy are [`gitten_core::markdown::Document`]
//! — shared with the terminal, which draws the same model through a pen. What
//! is here is pixels.
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
//! [`Metrics::budget`] is why: a bar, three levels of indent and a bullet are
//! real pixels, and an 18px heading in a 14px body holds a fifth fewer
//! characters. The *policy* — which block costs what, which rows keep their
//! grid whole — is core's; this file turns the semantic furniture into pixels
//! and the budget into a number.
//!
//! A table is the exception to all of it. Its grid is aligned character by
//! character with the rows around it, so a break at a column shears it — and not
//! breaking it makes it the widest row in the diff, which drags the whole view
//! into a horizontal scroll. So a grid too wide for the window is *laid out
//! again* at the width there is: columns squeezed, cells wrapped inside them,
//! one row becoming as many rows as its tallest cell needs. That is
//! `Document::reflow` over `core`'s `flow_table`, and the rows it decides reach
//! `Wrapped` as `Budget::At` — the same flat table every other row's rows are
//! in.
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
//!   it allocates on the render path. The hairline between two rows of a table is
//!   here too, and it is furniture rather than text for a structural reason: as a
//!   row of `─` it would be a row of the list that no line of the file produced,
//!   and the gutter's numbers have to keep adding up.
//!
//! # Cost
//!
//! The same as the built-in, per frame: one `StyledText` and one run list per
//! visible row, through the same `runs` merge. Everything markdown-specific was
//! decided at load and is a `Copy` field read out of a `Vec`.

use super::diff::{
    column_at, columns, file_header, header_hit, hunk_header, hunk_hit, into_text, line_colors,
    num, row_frame, scrolled, selected, slice, Hit, Rows, Scratch, PAD, ROW_H, SIGN_W, TEXT_CHROME,
};
use gitten_core::host::Host;
use gitten_core::markdown::{Bar, Block, DocRow, Document};
use gitten_core::prepared::Line;
use gitten_core::runs::surfaces;
use gitten_core::select::Selected;
use gitten_core::theme::Rgb;
use gitten_core::wrap::Wrap;
use gpui::*;

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
    pub layout: gitten_core::markdown::Layout,
}

impl Default for Metrics {
    fn default() -> Self {
        Self {
            heading: [18.0, 16.5, 15.0, 14.0, 14.0, 14.0],
            indent: 14.0,
            bar: 2.0,
            bullets: &["•", "◦", "▪", "·"],
            layout: gitten_core::markdown::Layout::monospaced(),
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
    pub fn for_font(font: &gitten_core::font::Font) -> Self {
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
            layout: gitten_core::markdown::Layout {
                monospaced: font.monospaced,
                ..Default::default()
            },
            ..Self::default()
        }
    }

    fn size(&self, level: u8) -> f32 {
        self.heading[level.clamp(1, 6) as usize - 1]
    }

    fn bullet(&self, depth: u8) -> &'static str {
        let last = self.bullets.len().saturating_sub(1);
        self.bullets
            .get(depth as usize)
            .copied()
            .unwrap_or(self.bullets[last])
    }

    /// How large this block's text is drawn. A heading is the only thing in the
    /// app with a type size of its own, and it is why a column budget and a
    /// caret are both per row here rather than per diff.
    fn text_size(&self, block: Block, host: &Host) -> f32 {
        match block {
            Block::Heading(level) => self.size(level),
            _ => host.font.size,
        }
    }

    /// One character of a block's text, in pixels. `Font::char_width` at the
    /// block's own size rather than the host's, which is the whole of what makes
    /// a heading's caret and a heading's overflow different from a paragraph's.
    fn char_width(&self, block: Block, host: &Host) -> f32 {
        self.text_size(block, host) * host.font.advance
    }

    /// How many pixels of furniture sit between the sign column and the text: a
    /// bar, some indent steps, a bullet.
    ///
    /// Measured out of the *semantic* furniture core describes, so the budget
    /// and the caret cannot disagree with the drawing about which block costs
    /// what. A table gets none of it: its grid is aligned character by character
    /// against the rows around it, so it is drawn with the gutter and then
    /// nothing at all.
    fn furniture(&self, block: Block) -> f32 {
        let f = block.furniture();
        self.indent * (f.bar.is_some() as u8 + f.depth + f.bullet as u8) as f32
    }

    /// How many characters of `block` fit in `width` pixels.
    ///
    /// Every other presentation has one budget for the whole diff; this one has a
    /// budget per row, and that is the reason core's `Wrapped` takes the column
    /// count per line rather than once. Two things move it:
    ///
    /// - **What is drawn in front of the text.** A quote bar, three levels of
    ///   list indent and a bullet are real pixels, and a wrap that ignored them
    ///   would overflow by exactly as many as it ignored.
    /// - **How large the text is.** A `#` heading is drawn at 18px where the body
    ///   is 14, so the same row holds a fifth fewer characters. Nothing else in
    ///   the app has two type sizes in one list.
    ///
    /// A table's budget is the width its *grid* has to fit, and not a column its
    /// text may be broken at: a grid is aligned character by character with the
    /// rows above and below it, so it is re-laid-out by core's `flow_table` —
    /// cells wrapped inside their own columns — or drawn whole and scrolled to.
    fn budget(&self, block: Block, width: f32, host: &Host) -> usize {
        columns(
            width,
            TEXT_CHROME + self.furniture(block),
            self.text_size(block, host),
            host,
        )
    }
}

/// The rendered-markdown presentation. Register it after the built-in and it
/// takes every `.md`, `.markdown` and `.mdx` file in the diff.
///
/// Rows, blocks, tables, flowed grids and wrap ranges are the core
/// [`Document`]'s; what this holds is the font-derived [`Metrics`], the paths it
/// claims, and the scratch buffers drawing borrows.
pub struct MarkdownRows {
    doc: Document,
    metrics: Metrics,
    /// Which extensions to claim. Owned rather than hardcoded so the same
    /// implementation can be pointed at `.mdown` or `.txt` without editing it.
    extensions: Vec<String>,
    /// The width the budgets were computed for, kept so a row can ask what its
    /// own column is. Only one thing needs it — a thematic break is drawn as a
    /// rule and a rule has to be as wide as the text it replaces — and the
    /// alternative is a constant, which was 320 pixels regardless of the window.
    width: f32,
    /// What drawing borrows. Cleared per row, grown once ever — see [`Scratch`].
    scratch: std::cell::RefCell<Scratch>,
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
            doc: Document::new(metrics.layout),
            metrics,
            extensions: extensions.iter().map(|s| s.to_string()).collect(),
            width: 0.0,
            scratch: std::cell::RefCell::default(),
        }
    }

    fn budget(&self, block: Block, width: f32, host: &Host) -> usize {
        self.metrics.budget(block, width, host)
    }

    /// How many characters one visual row actually draws, after `trim_end`:
    /// trailing space is not ink and a row that is all of it has nothing past
    /// the window. What [`Rows::overflow`] measures, where [`Rows::width`]'s
    /// approximations are not good enough — a bound half a character out is a
    /// diff you cannot scroll to the end of.
    fn chars(&self, index: usize, seg: usize) -> usize {
        let Some(text) = self.doc.text(index) else {
            return 0;
        };
        text[self.doc.range(index, seg)].trim_end().chars().count()
    }
}

impl Rows for MarkdownRows {
    fn claims(&self, path: &str) -> bool {
        // `rsplit` on the whole path, not the file name: a path with no dot in
        // its last segment must not pick up a dot from a parent directory.
        let name = path.rsplit('/').next().unwrap_or(path);
        name.rsplit_once('.')
            .is_some_and(|(_, ext)| self.extensions.iter().any(|e| e == ext))
    }

    fn len(&self) -> usize {
        self.doc.len()
    }

    fn is_file_header(&self, index: usize) -> bool {
        matches!(self.doc.row(index), Some(DocRow::File { .. }))
    }

    fn rows(&self, index: usize) -> usize {
        self.doc.rows(index)
    }

    fn reflow(&mut self, width: f32, host: &Host, wrap: &dyn Wrap) -> bool {
        // The unit adapter: one block's budget, in characters, from the window
        // width less the fixed diff chrome less this block's furniture measured
        // in pixels, at this block's own type size. Core sees only the number.
        let metrics = self.metrics;
        let budget = move |block: Block| metrics.budget(block, width, host);
        match self.doc.reflow(&budget, wrap) {
            true => {
                self.width = width;
                true
            }
            false => false,
        }
    }

    fn build(&mut self, f: gitten_core::prepared::File) {
        self.doc.push(f);
    }

    fn width(&self, index: usize, seg: usize) -> usize {
        match self.doc.row(index) {
            // Indent steps cost roughly a character each, and a heading's glyphs
            // are wider than the body's — both approximations, and both only feed
            // the widest-row contest that decides which row the horizontal bound
            // is taken from.
            Some(DocRow::Line { block, .. }) => {
                let scale = match block {
                    Block::Heading(l) => self.metrics.size(*l) / 14.0,
                    _ => 1.0,
                };
                // The re-laid-out grid, when there is one: a table that was
                // squeezed to fit is exactly as wide as the budget, and measuring
                // the one it was squeezed out of would leave the whole list
                // scrolling sideways for a row nothing draws.
                let text = self.doc.text(index).unwrap_or_default();
                // `chars`, not `len`: a table row is full of three-byte box
                // drawing and would otherwise measure three times too wide and
                // win the widest-row contest for the whole diff.
                let shown = text[self.doc.range(index, seg)].trim_end();
                (shown.chars().count() as f32 * scale) as usize + block.depth() as usize + 2
            }
            Some(DocRow::Hunk(h)) => h.chars().count(),
            Some(DocRow::File { path, .. }) => path.chars().count(),
            None => 0,
        }
    }

    fn report(&self) -> String {
        self.doc.report()
    }

    /// A row's own furniture and a row's own type size, which is what makes this
    /// the one presentation whose overflow is per row rather than per diff: the
    /// same text is a fifth wider as an `#` heading and starts three indent steps
    /// further in as a nested bullet.
    fn overflow(&self, index: usize, seg: usize, width: f32, host: &Host) -> f32 {
        match self.doc.row(index) {
            Some(DocRow::Line { block, .. }) => {
                let text = self.chars(index, seg) as f32 * self.metrics.char_width(*block, host);
                let room = width - TEXT_CHROME - self.metrics.furniture(*block);
                (text - room).max(0.0)
            }
            // A header is the built-in's, drawn behind the page padding and
            // nothing else.
            _ => (self.width(index, seg) as f32 * host.font.char_width() - (width - 2.0 * PAD))
                .max(0.0),
        }
    }

    /// The gutter, then this block's own furniture, then text at this block's own
    /// size. Nothing else in the app has two type sizes in one list, which is why
    /// this is the one presentation whose caret arithmetic is per row.
    fn hit(&self, index: usize, seg: usize, x: f32, host: &Host, shift: f32) -> Option<Hit> {
        Some(match self.doc.row(index)? {
            DocRow::File { path, .. } => header_hit(path, x, host, shift),
            DocRow::Hunk(h) => hunk_hit(h, x, host, shift),
            DocRow::Line { block, .. } => {
                let text = self.doc.text(index)?;
                let at = self.doc.range(index, seg);
                let from = TEXT_CHROME - PAD + self.metrics.furniture(*block);
                let off = at.start
                    + column_at(
                        &text[at.clone()],
                        into_text(x, from, shift),
                        self.metrics.text_size(*block, host),
                        host,
                    );
                Hit { part: 0, off }
            }
        })
    }

    /// The source line, which is also what is drawn: the markers this
    /// presentation replaces were taken off the text by core's `lay_out`, so a
    /// copy yields what was on screen rather than a bullet nobody can see.
    fn selectable(&self, index: usize, _part: u16) -> Option<&str> {
        // The flowed grid, when there is one, because that is what is on
        // screen — and what `hit` returned offsets into.
        self.doc.text(index)
    }

    fn is_header(&self, index: usize) -> bool {
        matches!(self.doc.row(index), Some(DocRow::File { .. }))
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
        match self.doc.row(index) {
            Some(DocRow::File { path, adds, dels }) => {
                file_header(path, *adds, *dels, theme, sel, current, shift)
            }
            Some(DocRow::Hunk(header)) => hunk_header(header, theme, sel, current, shift),
            Some(DocRow::Line { block, line }) => {
                self.line(index, seg, *block, line, current, host, sel, shift)
            }
            // The order table only names rows this presentation built, so this
            // arm is unreachable; a blank row beats a panic if an index ever
            // arrives stale.
            None => div().into_any_element(),
        }
    }
}

impl MarkdownRows {
    #[allow(clippy::too_many_arguments)]
    fn line(
        &self,
        index: usize,
        seg: usize,
        block: Block,
        line: &Line,
        // Whether the keyboard is on this row: the one bar every presentation
        // paints, prose or not — see `row_background`.
        current: bool,
        host: &Host,
        sel: Option<Selected>,
        // Pixels of text scrolled off the left. Everything this row draws in
        // front of its text — the gutter, a quote bar, an indent, a bullet — is
        // furniture and stays put; see `scrolled`.
        shift: f32,
    ) -> AnyElement {
        let theme = &host.theme;
        let m = &self.metrics;
        let md = &theme.markdown;
        // What core says this row draws in place of its markers: a bar, indent
        // steps, a bullet slot, a rule, a grid. The drawing below only turns
        // each into pixels.
        let f = block.furniture();
        let (bg, fg, sign) = line_colors(line.kind, line.moved, &theme.diff);
        // The keyboard's row, on prose exactly as on source: the same helper
        // every presentation goes through, so a paragraph cannot be the one row
        // that hides the cursor.
        let bg = super::diff::row_background(current, bg, theme);
        let surface = surfaces(line.kind, line.moved).0;
        // A continuation of a wrapped line: the same furniture, so a wrapped
        // bullet stays indented under its own text and a wrapped quote keeps its
        // bar, and no number and no sign, as everywhere else.
        let blank = seg > 0;
        // The whole logical row's text — the flowed grid when this width needed
        // one — and the bytes of it this visual row draws.
        let full = self.doc.text(index).unwrap_or_default();
        let at = self.doc.range(index, seg);
        // One borrow per row: the numbers format into it and the run list sweeps
        // through it, both copied out as the elements take them.
        let mut sc = self.scratch.borrow_mut();

        // The gutter is the built-in's, unchanged. Whatever the row does with the
        // text, the two line numbers and the sign have to sit where they sit on
        // every other row of the diff or the eye loses the column.
        let row = row_frame()
            .items_center()
            .px_4()
            .bg(rgb(bg))
            .child(num(sc.number(line.old_no, blank), theme.gutter_on(surface)))
            .child(num(sc.number(line.new_no, blank), theme.gutter_on(surface)))
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
        if f.table {
            let text = slice(self.doc.shared(index).expect("a line has text"), &at);
            let body = div().text_color(rgb(fg)).child(
                StyledText::new(text.clone()).with_highlights(
                    sc.merged(
                        at,
                        self.doc.tokens(index),
                        self.doc.spans(index),
                        theme,
                        line.kind,
                        line.moved,
                        selected(sel, 0, full),
                    )
                    .iter()
                    .cloned(),
                ),
            );
            // The grid is structure, not content, and a separator row is nothing
            // but grid.
            let body = match block {
                Block::TableRule => body.text_color(rgb(md.rule)),
                _ => body,
            };
            if !self.doc.rule_after(index, seg) {
                return row.child(scrolled(shift, body)).into_any_element();
            }
            // A hairline, not a row of `─`: a rule between two rows of a table is
            // not a line of the file, so drawing it as text would need a row the
            // gutter has to skip — and a row count is not this presentation's to
            // change. As wide as the grid and no wider, because a rule that ran
            // to the edge of the window would be a break in the document rather
            // than part of the table. Absolute, so it costs the text no pixel of
            // the row's fixed height.
            let width = text.chars().count() as f32 * host.font.char_width();
            // Inside the scrolled window and not beside it, because the rule is
            // as wide as the grid it belongs to: it has to move with the grid,
            // and be clipped where the grid is clipped rather than run on under
            // the line numbers.
            return row
                .child(scrolled(
                    shift,
                    // `ROW_H` and `relative`, so the hairline's `bottom_0` is the
                    // bottom of the *row* and not of the line of text: two table
                    // rows are contiguous, and a rule a few pixels above the
                    // boundary reads as floating inside one of them.
                    div()
                        .relative()
                        .flex()
                        .items_center()
                        .h(px(ROW_H))
                        .child(body)
                        .child(
                            div()
                                .absolute()
                                .bottom_0()
                                .left_0()
                                .w(px(width))
                                .h(px(1.))
                                .bg(rgb(md.rule)),
                        ),
                ))
                .into_any_element();
        }

        // A rule draws no text: the dashes were the drawing, so they are replaced
        // by the thing they were drawing.
        if f.rule {
            // As wide as the text it stands in for, which is the row's own wrap
            // budget: this used to be 320 pixels whatever the window was doing,
            // so a break was a stub in a wide window and an overhang in a narrow
            // one. The budget is already net of everything drawn in front of the
            // text, so it is the column exactly.
            let w = self.budget(block, self.width, host) as f32 * host.font.char_width();
            return row
                .child(scrolled(shift, div().w(px(w)).h(px(1.)).bg(rgb(md.rule))))
                .into_any_element();
        }
        if f.blank {
            return row.into_any_element();
        }

        // Full row height, so a fenced block of nine lines is one rule nine rows
        // long. At `ROW_H - 6` it was a 16-pixel dash with a 6-pixel gap above
        // and below, which is a ladder rather than a bar — and grouping a run of
        // rows is the whole job.
        let bar = |color: Rgb| {
            div()
                .flex_none()
                .w(px(m.bar))
                .h(px(ROW_H))
                .mr(px(m.indent - m.bar))
                .bg(rgb(color))
        };

        // The bar repeats on every segment; core's `Bar` says which colour.
        let row = match f.bar {
            Some(Bar::Quote) => row.child(bar(md.quote_bar)),
            Some(Bar::Code) => row.child(bar(md.code_bar)),
            None => row,
        };

        // Indent, then the marker's replacement, then the text. Separate elements
        // rather than padding inside the `StyledText` so the glyph can carry its
        // own colour without becoming a run in the merge.
        let depth = f.depth;
        let row = if depth > 0 {
            row.child(div().flex_none().w(px(depth as f32 * m.indent)))
        } else {
            row
        };
        // The glyph on the first row and its width on every one, so a wrapped
        // item's continuation lines up under its own text rather than under its
        // bullet. The slot is reserved on every segment either way — the budget
        // was, so the indent must be.
        let row = if f.bullet {
            row.child(
                div()
                    .flex_none()
                    .w(px(m.indent))
                    .text_color(rgb(md.marker))
                    .child(if blank { " " } else { m.bullet(depth) }),
            )
        } else {
            row
        };

        // An empty fence line is a bare ``` with no language: the bar beside it
        // already says a block opened, so there is nothing left to draw.
        if full.is_empty() && at.is_empty() {
            return row.into_any_element();
        }

        // One borrow of the row's own storage: whole rows come out as refcount
        // bumps, wrapped segments as one heap slice each — never a `String`.
        let text = slice(self.doc.shared(index).expect("a line has text"), &at);
        let body = div().text_color(rgb(fg)).child(
            StyledText::new(text).with_highlights(
                sc.merged(
                    at,
                    self.doc.tokens(index),
                    self.doc.spans(index),
                    theme,
                    line.kind,
                    line.moved,
                    selected(sel, 0, full),
                )
                .iter()
                .cloned(),
            ),
        );
        let body = if let Some(level) = f.heading {
            body.text_size(px(m.size(level)))
                .font_weight(FontWeight::BOLD)
        } else if f.fence_label {
            // A fence's language label is punctuation the reader should be able
            // to skip. A table's pipes are too, but a table is drawn verbatim —
            // see the note on `Block::Table` in `gitten_core::markdown`.
            body.text_color(rgb(md.marker))
        } else {
            body
        };
        row.child(scrolled(shift, body)).into_any_element()
    }
}

#[cfg(test)]
mod tests {
    // By name, not a glob: `use gpui::*` in the parent shadows `#[test]`.
    use super::{MarkdownRows, Metrics};
    use crate::views::diff::{Diff, Rows, TextRows, PAD, TEXT_CHROME};
    use gitten_core::host::Host;
    use gitten_core::markdown::{Block, Document};
    use gitten_core::prepared::prepare;
    use gitten_core::syntax::Kind;
    use gitten_core::{parse_unified_diff, LineKind};
    use std::rc::Rc;

    /// A real diff body. The lone `\x20` lines are blank *context* lines: a diff
    /// marks those with a single space, and `parse_unified_diff` drops a line
    /// that has no marker at all. Escaped so nothing can strip the space.
    const DOC: &str = "\
diff --git a/README.md b/README.md
@@ -1,9 +1,9 @@
 # gitten
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
        (0..r.len()).filter_map(|i| r.doc.block(i)).collect()
    }

    fn texts(r: &MarkdownRows) -> Vec<String> {
        (0..r.len())
            .filter_map(|i| r.doc.block(i).map(|_| r.doc.text(i).unwrap().to_string()))
            .collect()
    }

    #[test]
    fn it_claims_markdown_and_nothing_else() {
        let r = MarkdownRows::default();
        for p in ["README.md", "docs/a.markdown", "x/y/z.mdx", "a.b.md"] {
            assert!(r.claims(p), "{p}");
        }
        for p in [
            "a.rs",
            "Cargo.lock",
            "no-extension",
            "weird.xyz",
            "md",
            ".md.rs",
        ] {
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
        assert!(
            !r.claims("c.md"),
            "the default list was replaced, not extended"
        );
    }

    #[test]
    fn the_metrics_are_configurable_too() {
        // Rule 1: the numbers a built-in draws with are not the built-in's to
        // keep. Anything an extension cannot reach is not a knob.
        let tight = Metrics {
            heading: [14.0; 6],
            indent: 8.0,
            ..Metrics::default()
        };
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
        assert!(t.contains(&"gitten".to_string()), "hashes survived: {t:?}");
        assert!(
            t.iter().any(|l| l == "one"),
            "a bullet marker survived: {t:?}"
        );
        assert!(
            t.iter().any(|l| l == "quoted"),
            "a quote marker survived: {t:?}"
        );
        assert!(
            t.iter().any(|l| l == "rust"),
            "a fence kept more than its language"
        );
        assert!(
            t.iter().any(|l| l.contains("bolder claims and a link")),
            "inline markup survived: {t:?}"
        );
        assert!(
            !t.iter().any(|l| l.contains("https://")),
            "a url survived: {t:?}"
        );
        assert!(
            t.iter().any(|l| l == "let x = 1;"),
            "a fence body was altered"
        );
    }

    #[test]
    fn a_heading_row_keeps_its_heading_token_over_its_words() {
        // The row draws the token, so if the cut left it pointing at the wrong
        // bytes the title comes out in the body colour at the wrong size.
        let r = built(DOC);
        let heading = (0..r.len())
            .find(|i| {
                r.doc
                    .block(*i)
                    .is_some_and(|b| matches!(b, Block::Heading(_)))
            })
            .expect("a heading row");
        let text = r.doc.text(heading).unwrap();
        let t = r
            .doc
            .tokens(heading)
            .iter()
            .find(|t| t.kind == Kind::Heading)
            .expect("a heading token");
        assert_eq!(&text[t.range()], "gitten");
    }

    #[test]
    fn every_range_indexes_the_text_the_row_will_draw() {
        // The one invariant that turns into a panic in GPUI's text layout rather
        // than into a wrong colour.
        let r = built(DOC);
        for i in 0..r.len() {
            let Some(text) = r.doc.text(i) else { continue };
            for t in r.doc.tokens(i) {
                assert!(t.end as usize <= text.len(), "token {t:?} outside {text:?}");
                assert!(
                    text.is_char_boundary(t.start as usize)
                        && text.is_char_boundary(t.end as usize)
                );
            }
            for s in r.doc.spans(i) {
                assert!(s.end as usize <= text.len(), "span {s:?} outside {text:?}");
            }
        }
    }

    #[test]
    fn the_changed_word_still_marks_the_word_that_changed() {
        // The intraline spans were computed on the source, so they have to have
        // moved with the text. This is the pair from DOC: bold -> bolder.
        let r = built(DOC);
        let marked: Vec<String> = (0..r.len())
            .filter_map(|i| {
                let line = r.doc.row(i)?;
                let kind = match line {
                    gitten_core::markdown::DocRow::Line { line, .. } => line.kind,
                    _ => return None,
                };
                if kind != LineKind::Added || r.doc.spans(i).is_empty() {
                    return None;
                }
                let text = r.doc.text(i).unwrap();
                Some(
                    r.doc
                        .spans(i)
                        .iter()
                        .map(|s| text[s.start as usize..s.end as usize].to_string())
                        .collect::<Vec<_>>(),
                )
            })
            .flatten()
            .collect();
        assert!(!marked.is_empty(), "no changed words survived the layout");
        assert!(
            marked
                .iter()
                .any(|m| m.contains("bolder") || m.contains("other")),
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
        // makes it three times too wide and it wins the widest-row contest for
        // the whole diff, which bounds the scroll at empty space.
        let r = built(TABLE);
        let widest = (0..r.len()).max_by_key(|i| r.width(*i, 0)).unwrap();
        let table_row = r.width(2, 0);
        assert!(
            table_row < 40,
            "a 25-column table row measured {table_row}; widest row is {widest}"
        );
    }

    #[test]
    fn every_row_draws_with_the_cursor_bar_on_it() {
        // The bar over prose goes through the same `row_background` helper the
        // source rows use; this walks every row shape in the document —
        // headings, blanks, bullets, fences, tables, rules — with the keyboard
        // on it and off it, so no branch of `line` can quietly skip the bar.
        let host = Host::new();
        let r = built(DOC);
        assert!((0..r.len()).any(|i| r.rows(i) > 0));
        for i in 0..r.len() {
            for seg in 0..r.rows(i) {
                let _ = r.render(i, seg, &host, None, true, 0.0);
                let _ = r.render(i, seg, &host, None, false, 0.0);
            }
        }
    }

    #[test]
    fn metrics_derived_from_the_default_font_match_the_constants_they_replaced() {
        // The scale used to be pixel constants. Deriving it must not move it.
        let m = Metrics::for_font(&gitten_core::font::Font::default());
        assert!((m.size(1) - 18.0).abs() < 0.35, "h1 moved to {}", m.size(1));
        assert!((m.size(2) - 16.5).abs() < 0.35, "h2 moved to {}", m.size(2));
        assert!((m.size(4) - 14.0).abs() < 0.01, "h4 is not the body size");
        assert!(m.layout.monospaced, "the default font is monospaced");
    }

    #[test]
    fn a_bigger_font_gives_up_the_top_of_the_scale_rather_than_the_row() {
        // The constraint is the row, not the font: at a 20px body size a 1.3x h1
        // would be 26px and clip into the row below, so it is capped instead.
        let big = gitten_core::font::Font {
            size: 20.0,
            ..gitten_core::font::Font::default()
        };
        let m = Metrics::for_font(&big);
        for level in 1..=6u8 {
            assert!(
                m.size(level) * 1.2 <= super::ROW_H + 0.01,
                "h{level} at {}px does not fit ROW_H",
                m.size(level)
            );
        }
        assert!(
            (1..6u8).all(|l| m.size(l) >= m.size(l + 1)),
            "scale is not monotonic"
        );
    }

    #[test]
    fn a_proportional_font_turns_table_padding_off() {
        // The whole reason `monospaced` is on the font rather than assumed here.
        let prop = gitten_core::font::Font {
            monospaced: false,
            ..gitten_core::font::Font::default()
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
            assert!(
                m.size(level) * 1.2 <= super::ROW_H,
                "h{level} at {}px",
                m.size(level)
            );
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
            vec![
                Box::new(TextRows::default()),
                Box::new(MarkdownRows::default()),
            ],
        );
        // Two headers and a removed/added pair per file, every row accounted for
        // exactly once, and only the markdown file's two lines were laid out.
        assert_eq!(diff.total(), 8);
        assert!(
            diff.load.contains("markdown 2 rows"),
            "report missing: {}",
            diff.load
        );
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
        assert!(
            !r.reflow(w, &host, host.wrap.current()),
            "the same width rebuilt"
        );
        assert!(
            !r.reflow(w + 0.4, &host, host.wrap.current()),
            "half a pixel rebuilt"
        );
        assert!(
            r.reflow(w + 200.0, &host, host.wrap.current()),
            "24 characters did not"
        );

        // And the blocks it collected are the ones the document has, once each —
        // the list is what stands in for every row, so a duplicate is wasted work
        // on every frame of a drag and a miss is a row wrapped to a stale budget.
        let collected = r.doc.distinct_blocks();
        assert!(collected.len() >= 3, "{collected:?}");
        for (i, b) in collected.iter().enumerate() {
            assert!(
                !collected[..i].contains(b),
                "{b:?} collected twice: {collected:?}"
            );
        }
        for i in 0..r.len() {
            if let Some(block) = r.doc.block(i) {
                assert!(collected.contains(&block), "{block:?} was never collected");
            }
        }
    }

    /// Every table row of a reflowed document, as `(row, visual row, text)`.
    fn grid(r: &MarkdownRows) -> Vec<(usize, usize, String)> {
        let mut out = Vec::new();
        for i in 0..r.len() {
            let Some(block) = r.doc.block(i) else {
                continue;
            };
            if !block.is_table() {
                continue;
            }
            let text = r.doc.text(i).unwrap();
            for seg in 0..r.rows(i) {
                out.push((i, seg, text[r.doc.range(i, seg)].to_string()));
            }
        }
        out
    }

    #[test]
    fn a_table_too_wide_for_the_window_stops_dragging_the_view_sideways_with_it() {
        // The bug: a grid is the widest row in the diff, the widest-row contest
        // picks it, and there is a screenful to scroll sideways through — every
        // row of prose in it wrapped to a window it is no longer looking at.
        let (r, _) = reflowed(PROSE, 30);
        let rows = grid(&r);
        assert!(rows.len() > 3, "the long row did not wrap: {rows:?}");
        for i in 0..r.len() {
            for seg in 0..r.rows(i) {
                // The two the measure adds for the padding it approximates; see
                // `width`.
                assert!(
                    r.width(i, seg) <= 32,
                    "row {i}.{seg} measured {}",
                    r.width(i, seg)
                );
            }
        }
        // And it is still a grid: the same pipes in the same columns, whichever
        // row and whichever sub-row.
        let pipes = |t: &str| -> Vec<usize> {
            t.chars()
                .enumerate()
                .filter(|(_, c)| "│├┤┼".contains(*c))
                .map(|(i, _)| i)
                .collect()
        };
        let first = pipes(&rows[0].2);
        assert_eq!(
            first.len(),
            4,
            "three columns is four boundaries: {:?}",
            rows[0].2
        );
        for (i, seg, t) in &rows {
            assert_eq!(pipes(t), first, "row {i}.{seg} sheared the grid: {t:?}");
        }
    }

    #[test]
    fn a_table_with_no_room_for_a_character_a_column_is_drawn_whole() {
        // Squeezing has a floor: three columns cost ten characters of pipes and
        // padding before a letter is drawn, and a grid made of nothing but grid
        // is worse than a grid you scroll to. So it is left alone, exactly as it
        // was before there was anything else to do — `Budget::Cols(0)`.
        let (r, _) = reflowed(PROSE, 12);
        let mut tables = 0;
        for i in 0..r.len() {
            if r.doc.block(i).is_some_and(|b| b.is_table()) {
                tables += 1;
                assert_eq!(r.rows(i), 1, "a table row wrapped");
            }
        }
        assert_eq!(tables, 3, "the fixture lost its table");
        assert_eq!(
            r.doc.squeezed(),
            0,
            "a table nobody can read a word of was squeezed"
        );
    }

    #[test]
    fn a_hairline_goes_between_two_table_rows_and_nowhere_else() {
        // The rule is drawn as pixels and not as a row of `─` glyphs, because a
        // rule between two rows of a table is not a line of the file: as text it
        // would need a row with no line number, and a row count is not this
        // presentation's to change. So what it draws is a property of a row, and
        // this is that property.
        let src = "\
diff --git a/a.md b/a.md
@@ -1,4 +1,4 @@
 | stage | detail |
 |---|---|
 | parse the log and assign the lanes | 466 ms |
 | assign lanes | 301 ms |
";
        let (r, _) = reflowed(src, 30);
        let rows: Vec<(usize, Block)> = (0..r.len())
            .filter_map(|i| r.doc.block(i).filter(|b| b.is_table()).map(|b| (i, b)))
            .collect();
        assert_eq!(rows.len(), 4, "header, separator and two rows: {rows:?}");
        let (header, sep, first, last) = (rows[0].0, rows[1].0, rows[2].0, rows[3].0);

        assert!(
            !r.doc.rule_after(header, 0),
            "a header's separator row is already a rule"
        );
        assert_eq!(rows[1].1, Block::TableRule);
        assert!(
            !r.doc.rule_after(sep, 0),
            "the separator row drew a second rule under itself"
        );
        assert!(
            !r.doc.rule_after(last, 0),
            "the last row of the table has nothing under it"
        );

        // The wrapped one: under its last sub-row, and none of the others.
        assert!(r.rows(first) > 1, "the fixture stopped wrapping");
        for seg in 0..r.rows(first) - 1 {
            assert!(
                !r.doc.rule_after(first, seg),
                "a rule cut through a wrapped cell at {seg}"
            );
        }
        assert!(
            r.doc.rule_after(first, r.rows(first) - 1),
            "no rule between the two rows"
        );
    }

    #[test]
    fn a_copy_of_a_squeezed_table_yields_its_rows_and_not_one_long_one() {
        // The flowed text is what a copy takes, so the newline between its
        // sub-rows is load-bearing: without it three rows of a grid paste as one
        // 90-character line that was never on the screen.
        let (r, _) = reflowed(PROSE, 30);
        let (row, _, _) = *grid(&r)
            .iter()
            .find(|(i, ..)| r.rows(*i) > 1)
            .expect("a wrapped row");
        let copied = r.selectable(row, 0).expect("a table row copies");
        let lines: Vec<&str> = copied.split('\n').collect();
        assert_eq!(lines.len(), r.rows(row), "{copied:?}");
        for line in &lines {
            assert_eq!(line.chars().count(), 30, "{line:?} is not the grid");
        }
        // And a caret still lands inside the sub-row it was clicked on, in the
        // same coordinates the copy is in.
        for seg in 0..r.rows(row) {
            let at = r.doc.range(row, seg);
            let hit = r
                .hit(row, seg, 60.0, &Host::new(), 0.0)
                .expect("a table row is hittable");
            assert!(at.contains(&hit.off), "{:?} is outside {at:?}", hit.off);
        }
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
        assert!(
            (0..r.len()).map(|i| r.rows(i)).sum::<usize>() > r.len(),
            "nothing wrapped"
        );
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
        let rows: Vec<(usize, Block)> = (0..r.len())
            .filter_map(|i| r.doc.block(i).map(|b| (i, b)))
            .collect();
        let plain = rows
            .iter()
            .find(|(_, b)| matches!(b, Block::Paragraph))
            .unwrap()
            .0;
        let nested = rows
            .iter()
            .filter(|(_, b)| matches!(b, Block::Bullet(_)))
            .nth(1)
            .unwrap()
            .0;
        assert!(r.metrics.furniture(Block::Paragraph) < r.metrics.furniture(Block::Bullet(1)));

        // The same pixel column, two rows: the indented one starts later, so the
        // same x is fewer characters into its text.
        let x = TEXT_CHROME - PAD + 20.0 * host.font.size * host.font.advance;
        let flat = r.hit(plain, 0, x, &host, 0.0).unwrap().off;
        let inset = r.hit(nested, 0, x, &host, 0.0).unwrap().off;
        assert!(
            inset < flat,
            "the indent did not move the caret: {inset} vs {flat}"
        );

        // And a click at the start of a row's own text is byte 0 of it, whatever
        // that row drew in front of itself.
        for (i, block) in &rows {
            let from = TEXT_CHROME - PAD + r.metrics.furniture(*block);
            assert_eq!(
                r.hit(*i, 0, from, &host, 0.0).unwrap().off,
                0,
                "row {i} {block:?}"
            );
        }
    }

    #[test]
    fn a_scroll_moves_the_text_and_leaves_the_furniture() {
        // A bullet, an indent and a quote bar are this presentation's gutter:
        // they say what the row *is*, so they stay while its text moves. The
        // caret is where that shows — a click at the left edge of a row's text
        // is the character `shift` in, at that row's own size.
        let host = Host::new();
        let r = built(DOC);
        let rows: Vec<(usize, Block)> = (0..r.len())
            .filter_map(|i| r.doc.block(i).map(|b| (i, b)))
            .collect();
        for (i, block) in &rows {
            if r.chars(*i, 0) < 4 {
                continue;
            }
            let from = TEXT_CHROME - PAD + r.metrics.furniture(*block);
            let shift = 3.0 * r.metrics.char_width(*block, &host);
            assert_eq!(
                r.hit(*i, 0, from, &host, shift).unwrap().off,
                3,
                "row {i} {block:?}"
            );
            // In the gutter, scrolled: the first character on screen, and never
            // a byte to the left of the row's own text.
            assert_eq!(
                r.hit(*i, 0, 0.0, &host, shift).unwrap().off,
                3,
                "row {i} gutter"
            );
        }
    }

    #[test]
    fn overflow_is_measured_at_the_row_s_own_size_and_indent() {
        // The reason `overflow` is per row here and per diff everywhere else. An
        // 18px heading is a fifth wider than the same characters as body text,
        // and a nested bullet has three fewer characters of room to start with —
        // so one number for the whole diff is short of one row and past the end
        // of another, and the last character of the widest line is unreachable.
        let host = Host::new();
        let mut r = built(DOC);
        let off = host.wrap.at(host.wrap.position("off").unwrap());
        let width = TEXT_CHROME + 4.0 * host.font.char_width();
        r.reflow(width, &host, off);

        let mut checked = 0;
        for i in 0..r.len() {
            let Some(block) = r.doc.block(i) else {
                continue;
            };
            let room = width - TEXT_CHROME - r.metrics.furniture(block);
            let text = r.chars(i, 0) as f32 * r.metrics.char_width(block, &host);
            assert_eq!(
                r.overflow(i, 0, width, &host),
                (text - room).max(0.0),
                "row {i}"
            );
            checked += 1;
        }
        assert!(checked > 6, "only {checked} rows measured");

        // And the two things that make it per row, one at a time.
        let heading = (0..r.len())
            .find(|i| {
                matches!(
                    r.doc.row(*i),
                    Some(gitten_core::markdown::DocRow::Line {
                        block: Block::Heading(_),
                        ..
                    })
                )
            })
            .expect("a heading");
        let chars = r.chars(heading, 0) as f32;
        assert!(
            r.overflow(heading, 0, width, &host)
                > chars * host.font.char_width() - (width - TEXT_CHROME),
            "a heading measured at the body size"
        );
        assert!(
            r.metrics.furniture(Block::Bullet(1)) > 0.0,
            "a nested bullet costs the text nothing"
        );
    }

    #[test]
    fn a_heading_is_selected_by_the_size_it_is_drawn_at() {
        // A `#` heading is 18px where the body is 14, so the same pixel is a
        // different character. Measuring it at the body size is a caret that
        // drifts further right the longer the heading is.
        let host = Host::new();
        let r = built(DOC);
        let heading = (0..r.len())
            .find(|i| {
                matches!(
                    r.doc.row(*i),
                    Some(gitten_core::markdown::DocRow::Line {
                        block: Block::Heading(_),
                        ..
                    })
                )
            })
            .expect("a heading");
        let text = r.selectable(heading, 0).unwrap().to_string();
        let end = TEXT_CHROME - PAD + text.chars().count() as f32 * r.metrics.size(1) * 0.602;
        assert_eq!(r.hit(heading, 0, end, &host, 0.0).unwrap().off, text.len());
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

    // -------------------------------------------------------------- parity

    /// The committed slice the terminal tests with too: this adapter and the
    /// core model must agree about how many rows a file becomes, where its
    /// visual rows break, and where a table's hairline goes. One model, two
    /// doors — if these drift, one client is drawing a different document.
    #[test]
    fn the_shell_and_the_core_model_agree_about_the_committed_slice() {
        let raw = include_str!("../../../tui/tests/fixtures/md.diff");
        let host = Host::new();
        let width = TEXT_CHROME + (60.5) * host.font.size * host.font.advance;
        let wrap = host.wrap.current();

        let mut r = MarkdownRows::default();
        let mut p = prepare(&parse_unified_diff(raw), &host.syntax, 2000);
        let mut claimed: Vec<_> = p.files.drain(..).filter(|f| r.claims(&f.path)).collect();
        for f in claimed.drain(..) {
            r.build(f);
        }
        r.reflow(width, &host, wrap);

        let mut doc = Document::new(r.metrics.layout);
        let mut p = prepare(&parse_unified_diff(raw), &host.syntax, 2000);
        let mut claimed: Vec<_> = p.files.drain(..).filter(|f| r.claims(&f.path)).collect();
        for f in claimed.drain(..) {
            doc.push(f);
        }
        let metrics = r.metrics;
        let budget = |block: Block| metrics.budget(block, width, &host);
        doc.reflow(&budget, wrap);

        assert_eq!(r.len(), doc.len(), "the two models hold different rows");
        for i in 0..doc.len() {
            assert_eq!(r.rows(i), doc.rows(i), "row {i} wraps differently");
            for seg in 0..doc.rows(i) {
                assert_eq!(
                    r.doc.rule_after(i, seg),
                    doc.rule_after(i, seg),
                    "row {i}.{seg} rules differently"
                );
                // And the pieces agree: what the shell measures for its overflow
                // is what the model says the row draws.
                let text = doc.text(i).unwrap();
                let expected = text[doc.range(i, seg)].trim_end().chars().count();
                assert_eq!(r.chars(i, seg), expected, "row {i}.{seg}");
            }
        }
    }
}
