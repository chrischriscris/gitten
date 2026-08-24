//! A `.md` file's diff, drawn as the document rather than as the source.
//!
//! A [`Rows`] implementation and nothing more: it claims markdown paths, takes
//! the same prepared lines the built-in takes, and draws them with the markers
//! gone — `## ` off the front and the row a size larger, `**` off a word that is
//! now simply bold, a link down to its text. The structural work is
//! [`gitten_core::markdown`]; what is here is pixels.
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
//! are real pixels, and an 18px heading in a 14px body holds a fifth fewer
//! characters.
//!
//! A table is the exception to all of it. Its grid is aligned character by
//! character with the rows around it, so a break at a column shears it — and not
//! breaking it makes it the widest row in the diff, which drags the whole view
//! into a horizontal scroll. So a grid too wide for the window is *laid out
//! again* at the width there is: columns squeezed, cells wrapped inside them, one
//! row becoming as many rows as its tallest cell needs. That is
//! [`MarkdownRows::reflow_tables`] over `core`'s `flow_table`, and the rows it
//! decides reach `Wrapped` as `Budget::At` — the same flat table every other
//! row's rows are in.
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
    column_at, columns, file_header, header_hit, hunk_header, into_text, line_colors, num,
    row_frame, scrolled, selected, slice, slice_shared, Hit, Rows, Scratch, PAD, ROW_H, SIGN_W,
    TEXT_CHROME,
};
use gitten_core::host::Host;
use gitten_core::markdown::{flow_table, lay_out_tables, Block, Grid, Layout, TableRow};
use gitten_core::runs::surfaces;
use gitten_core::select::Selected;
use gitten_core::syntax::Token;
use gitten_core::theme::Rgb;
use gitten_core::wrap::{Break, Budget, Wrap, Wrapped};
use gitten_core::{LineKind, Span};
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
            layout: Layout {
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
}

/// One row. `Copy` fields and shared text only: `render` runs per visible row
/// per redraw, so nothing in here may be worth allocating at that point. The
/// line's text is the prepared line's own `Arc`, not a copy of it; the headers
/// are the parsed diff's own strings by handle, and the gutter numbers stay
/// integers until draw time — see [`Row::Line`](super::diff::Row) for why.
enum Row {
    File {
        path: std::sync::Arc<str>,
        adds: usize,
        dels: usize,
    },
    Hunk(std::sync::Arc<str>),
    Line {
        block: Block,
        kind: LineKind,
        moved: bool,
        old: Option<u32>,
        new: Option<u32>,
        text: std::sync::Arc<str>,
        spans: Box<[Span]>,
        tokens: Box<[Token]>,
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
    /// Which rows are in a table, and which table: `(row, grid)`, ascending by
    /// row. Sparse, because a table is 1–2.5% of the rows of a diff and a `run`
    /// field on every row of a 714k-line one is 2.8 MB to answer "no".
    tables: Vec<(u32, u32)>,
    /// What each of those tables was aligned to at load: the width every column
    /// wants, and which way its cells sit. One per run, so a handful.
    grids: Vec<Grid>,
    /// The table rows whose grid does not fit the current width, re-laid out onto
    /// one that does — `(row, flowed)`, ascending. Empty at any width where every
    /// table fits, and with wrapping off.
    flows: Vec<(u32, Flowed)>,
    /// What drawing borrows. Cleared per row, grown once ever — see [`Scratch`].
    scratch: std::cell::RefCell<Scratch>,
}

/// One table row re-laid-out to the window: the sub-rows of its grid laid end to
/// end, and everything that indexes them.
///
/// The shell's copy of [`gitten_core::markdown::FlowRow`], and it exists for one
/// reason: `SharedString`. A row is sliced per visible row per frame through the
/// same `slice` every other row uses, and that is a refcount bump on a
/// `SharedString` and a `to_string` on a `String`.
struct Flowed {
    text: SharedString,
    breaks: Vec<Break>,
    spans: Vec<Span>,
    tokens: Vec<Token>,
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
            tables: Vec::new(),
            grids: Vec::new(),
            flows: Vec::new(),
            scratch: std::cell::RefCell::default(),
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
    /// A table's budget is the width its *grid* has to fit, and not a column its
    /// text may be broken at: a grid is aligned character by character with the
    /// rows above and below it, so it is re-laid-out by
    /// [`flow_table`] — cells wrapped inside their own columns — or drawn whole
    /// and scrolled to. See [`MarkdownRows::reflow_tables`].
    fn budget(&self, block: Block, width: f32, host: &Host) -> usize {
        columns(
            width,
            TEXT_CHROME + self.furniture(block),
            self.size(block, host),
            host,
        )
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

    /// How many characters one visual row actually draws, after `trim_end`:
    /// trailing space is not ink and a row that is all of it has nothing past
    /// the window. What [`Rows::overflow`] measures, where [`Rows::width`]'s
    /// approximations are not good enough — a bound half a character out is a
    /// diff you cannot scroll to the end of.
    fn chars(&self, index: usize, seg: usize) -> usize {
        let Some(Row::Line { text, .. }) = self.rows.get(index) else {
            return 0;
        };
        let text = self
            .flowed(index)
            .map_or(text.as_ref(), |f| f.text.as_str());
        text[self.wrapped.range(index, seg, text)]
            .trim_end()
            .chars()
            .count()
    }

    /// One character of this block's text, in pixels. `Font::char_width` at the
    /// block's own size rather than the host's, which is the whole of what makes
    /// a heading's caret and a heading's overflow different from a paragraph's.
    fn char_width(&self, block: Block, host: &Host) -> f32 {
        self.size(block, host) * host.font.advance
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

    /// Re-lays every table whose grid does not fit `width`, and forgets the ones
    /// that do.
    ///
    /// The one part of a table's layout that is the window's business. Which rows
    /// are one table, what its columns are and how wide each wants to be were all
    /// settled at load by `lay_out_tables`; how many of those columns' characters
    /// there is room for is not knowable until here, and changes on every drag
    /// that crosses one.
    ///
    /// Runs per reflow over the tables and not over the rows, which is the whole
    /// reason `tables` is sparse: a diff with no table in it does no work here at
    /// any width.
    fn reflow_tables(&mut self, width: f32, host: &Host, wrap: &dyn Wrap) {
        self.flows.clear();
        if self.tables.is_empty() {
            return;
        }
        // One budget for every table, because a table row draws no furniture and
        // no heading: the grid gets the whole column.
        let cols = self.budget(Block::Table, width, host);
        // Two vectors and not a `filter_map` into one: what comes back is one
        // flowed row per row passed in, so a row silently dropped on the way in
        // would attach every flow after it to the wrong row.
        let mut run: Vec<TableRow> = Vec::new();
        let mut of: Vec<u32> = Vec::new();
        let mut i = 0;
        while i < self.tables.len() {
            let grid = self.tables[i].1;
            let start = i;
            while i < self.tables.len() && self.tables[i].1 == grid {
                i += 1;
            }
            run.clear();
            of.clear();
            for (r, _) in &self.tables[start..i] {
                let Row::Line {
                    block,
                    text,
                    spans,
                    tokens,
                    ..
                } = &self.rows[*r as usize]
                else {
                    continue;
                };
                of.push(*r);
                run.push(TableRow {
                    text,
                    block: *block,
                    tokens,
                    spans,
                });
            }
            let Some(flowed) = flow_table(
                &run,
                &self.grids[grid as usize],
                cols,
                &self.metrics.layout,
                wrap,
            ) else {
                continue;
            };
            for (row, f) in of.iter().zip(flowed) {
                self.flows.push((
                    *row,
                    Flowed {
                        text: f.text.into(),
                        breaks: f.breaks,
                        spans: f.spans,
                        tokens: f.tokens,
                    },
                ));
            }
        }
    }

    /// The re-laid-out grid for a row, if this width needed one.
    ///
    /// A binary search and not a field on the row: this is asked once per visible
    /// row per frame over a list that holds a handful of entries, and the
    /// alternative costs four bytes on every row of every diff to say "no".
    fn flowed(&self, index: usize) -> Option<&Flowed> {
        let at = self
            .flows
            .binary_search_by_key(&(index as u32), |(r, _)| *r)
            .ok()?;
        Some(&self.flows[at].1)
    }

    /// Whether a hairline is drawn under this visual row: a rule *between* two
    /// rows of a table, which is not the same thing as a border around one.
    ///
    /// Three ways to answer no, and each is a thing that looked wrong:
    ///
    /// - **The last row of a table.** There is nothing under it to be separated
    ///   from, and a line hanging under an open-bottomed grid reads as a break in
    ///   the document rather than as part of the table.
    /// - **A header.** Its separator row is already a rule, and two of them a
    ///   pixel apart is a double line.
    /// - **Any sub-row but the last.** A squeezed cell wraps, and a rule through
    ///   the middle of its own sentence says the row ended where it did not.
    ///
    /// One method rather than a condition at the call site, because it is a rule
    /// about rows and a test can ask it directly.
    fn ruled(&self, index: usize, seg: usize) -> bool {
        let Some(Row::Line { block, .. }) = self.rows.get(index) else {
            return false;
        };
        *block == Block::Table
            && seg + 1 == self.wrapped.rows(index)
            && matches!(self.rows.get(index + 1), Some(Row::Line { block, .. }) if *block == Block::Table)
    }

    /// What a row draws and what indexes it: its own text, or the grid this width
    /// re-laid it out onto. The [`Source`] keeps which one, because slicing a
    /// row's own `Arc` is a refcount bump and slicing a flowed grid is not.
    fn text_of<'a>(
        &'a self,
        index: usize,
        text: &'a std::sync::Arc<str>,
        spans: &'a [Span],
        tokens: &'a [Token],
    ) -> (&'a str, &'a [Span], &'a [Token], Source<'a>) {
        match self.flowed(index) {
            Some(f) => (
                f.text.as_str(),
                &f.spans,
                &f.tokens,
                Source::Flowed(&f.text),
            ),
            None => (text, spans, tokens, Source::Own(text)),
        }
    }
}

/// Which storage a row's drawn text came from — see [`MarkdownRows::text_of`].
enum Source<'a> {
    Own(&'a std::sync::Arc<str>),
    Flowed(&'a SharedString),
}

impl Source<'_> {
    /// One row's worth of it: whole rows come out as refcount bumps either way.
    fn piece(&self, at: &std::ops::Range<usize>) -> SharedString {
        match self {
            Source::Own(t) => slice(t, at),
            Source::Flowed(t) => slice_shared(t, at),
        }
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
        self.rows.len()
    }

    fn rows(&self, index: usize) -> usize {
        self.wrapped.rows(index)
    }

    fn reflow(&mut self, width: f32, host: &Host, wrap: &dyn Wrap) -> bool {
        let budgets: Vec<usize> = self
            .blocks
            .iter()
            .map(|b| self.budget(*b, width, host))
            .collect();
        if budgets == self.budgets && wrap.name() == self.wrap {
            return false;
        }
        self.budgets = budgets;
        self.wrap = wrap.name();
        self.width = width;
        // Before the wrap and not inside it: a table that no longer fits comes
        // out of this with its rows already decided, and what `Wrapped` does with
        // it is keep them.
        self.reflow_tables(width, host, wrap);
        let wrapped = Wrapped::build_with(
            self.rows.iter().enumerate().map(|(i, r)| match r {
                Row::Line { block, text, .. } => match self.flowed(i) {
                    Some(f) => (f.text.as_ref(), Budget::At(&f.breaks)),
                    // A grid that fits, or one nothing could be done with, is
                    // drawn whole: a break at a column shears it.
                    None if block.is_table() => (text.as_ref(), Budget::Cols(0)),
                    None => (
                        text.as_ref(),
                        Budget::Cols(self.budget(*block, width, host)),
                    ),
                },
                // A header is drawn by the built-in's own function at the built-in's
                // own width, and a path is not prose. One row, always.
                _ => ("", Budget::Cols(0)),
            }),
            wrap,
        );
        self.wrapped = wrapped;
        true
    }

    fn build(&mut self, mut f: gitten_core::prepared::File) {
        self.rows.push(Row::File {
            path: std::mem::take(&mut f.path).into(),
            adds: f.adds,
            dels: f.dels,
        });
        for mut h in f.hunks {
            self.rows
                .push(Row::Hunk(std::mem::take(&mut h.header).into()));
            // Per hunk, because that is the largest unit whose block structure is
            // knowable: a fence opened in one hunk and closed in another has
            // everything between them missing from the diff entirely.
            let (blocks, tables) = lay_out_tables(&mut h.lines, &self.metrics.layout);
            self.laid_out += blocks.len();
            // The grids come along because the window's width is not known here
            // and a grid too wide for it has to be laid out again — off the same
            // runs and the same measurements, or the second answer disagrees with
            // the first. Rebased onto this presentation's own row numbering.
            let (row, grid) = (self.rows.len() as u32, self.grids.len() as u32);
            self.grids.extend(tables.grids);
            self.tables
                .extend(tables.of_line.iter().map(|(l, g)| (row + l, grid + g)));
            for (l, block) in h.lines.into_iter().zip(blocks) {
                if !self.blocks.contains(&block) {
                    self.blocks.push(block);
                }
                self.rows.push(Row::Line {
                    block,
                    kind: l.kind,
                    moved: l.moved,
                    old: l.old_no,
                    new: l.new_no,
                    text: l.text,
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
            // the widest-row contest that decides which row the horizontal bound
            // is taken from.
            Row::Line { text, block, .. } => {
                let scale = match block {
                    Block::Heading(l) => self.metrics.size(*l) / 14.0,
                    _ => 1.0,
                };
                // The re-laid-out grid, when there is one: a table that was
                // squeezed to fit is exactly as wide as the budget, and measuring
                // the one it was squeezed out of would leave the whole list
                // scrolling sideways for a row nothing draws.
                let text = self
                    .flowed(index)
                    .map_or(text.as_ref(), |f| f.text.as_str());
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
            return String::new();
        }
        let mut out = format!("markdown {} rows", self.laid_out);
        // Only when it happened. A table that fits is the common case and saying
        // "0 squeezed" on every diff is noise on a line that is read at a glance.
        if !self.flows.is_empty() {
            out.push_str(&format!(" · {} table rows squeezed", self.flows.len()));
        }
        out
    }

    /// A row's own furniture and a row's own type size, which is what makes this
    /// the one presentation whose overflow is per row rather than per diff: the
    /// same text is a fifth wider as an `#` heading and starts three indent steps
    /// further in as a nested bullet.
    fn overflow(&self, index: usize, seg: usize, width: f32, host: &Host) -> f32 {
        match &self.rows[index] {
            Row::Line { block, .. } => {
                let text = self.chars(index, seg) as f32 * self.char_width(*block, host);
                let room = width - TEXT_CHROME - self.furniture(*block);
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
        Some(match self.rows.get(index)? {
            Row::File { path, .. } => header_hit(path, x, host, shift),
            Row::Hunk(h) => header_hit(h, x, host, shift),
            Row::Line { block, text, .. } => {
                let text = self
                    .flowed(index)
                    .map_or(text.as_ref(), |f| f.text.as_str());
                let at = self.wrapped.range(index, seg, text);
                let from = TEXT_CHROME - PAD + self.furniture(*block);
                let off = at.start
                    + column_at(
                        &text[at.clone()],
                        into_text(x, from, shift),
                        self.size(*block, host),
                        host,
                    );
                Hit { part: 0, off }
            }
        })
    }

    /// The source line, which is also what is drawn: the markers this
    /// presentation replaces were taken off the text by `lay_out`, so a copy
    /// yields what was on screen rather than a bullet nobody can see.
    fn selectable(&self, index: usize, _part: u16) -> Option<&str> {
        Some(match self.rows.get(index)? {
            // The flowed grid, when there is one, because that is what is on
            // screen — and what `hit` returned offsets into.
            Row::Line { text, .. } => self
                .flowed(index)
                .map_or(text.as_ref(), |f| f.text.as_str()),
            Row::Hunk(h) => h.as_ref(),
            Row::File { path, .. } => path.as_ref(),
        })
    }

    fn render(
        &self,
        index: usize,
        seg: usize,
        host: &Host,
        sel: Option<Selected>,
        shift: f32,
    ) -> AnyElement {
        let theme = &host.theme;
        match &self.rows[index] {
            Row::File { path, adds, dels } => file_header(path, *adds, *dels, theme, sel, shift),
            Row::Hunk(header) => hunk_header(header, theme, sel, shift),
            Row::Line {
                block,
                kind,
                moved,
                old,
                new,
                text,
                spans,
                tokens,
            } => {
                let (text, spans, tokens, source) = self.text_of(index, text, spans, tokens);
                let at = self.wrapped.range(index, seg, text);
                let rule = self.ruled(index, seg);
                self.line(
                    *block,
                    *kind,
                    *moved,
                    *old,
                    *new,
                    source.piece(&at),
                    text.len(),
                    at,
                    seg,
                    spans,
                    tokens,
                    rule,
                    host,
                    sel,
                    shift,
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
        old: Option<u32>,
        new: Option<u32>,
        // What this visual row draws of the line — `slice` or `slice_shared`
        // of it, already taken.
        piece: SharedString,
        // Length of the whole line the piece belongs to, which a selection
        // range is measured against.
        full_len: usize,
        at: std::ops::Range<usize>,
        seg: usize,
        spans: &[Span],
        tokens: &[Token],
        // A hairline under this row: a table row with another one under it.
        rule: bool,
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
        let (bg, fg, sign) = line_colors(kind, moved, &theme.diff);
        let surface = surfaces(kind, moved).0;
        // A continuation of a wrapped line: the same furniture, so a wrapped
        // bullet stays indented under its own text and a wrapped quote keeps its
        // bar, and no number and no sign, as everywhere else.
        let blank = seg > 0;
        let text = &piece;
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
            .child(num(sc.number(old, blank), theme.gutter_on(surface)))
            .child(num(sc.number(new, blank), theme.gutter_on(surface)))
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
            let body = div().text_color(rgb(fg)).child(
                StyledText::new(text.clone()).with_highlights(
                    sc.merged(
                        at,
                        tokens,
                        spans,
                        theme,
                        kind,
                        moved,
                        selected(sel, 0, full_len),
                    )
                    .iter()
                    .cloned(),
                ),
            );
            // The grid is structure, not content, and a separator row is nothing
            // but grid.
            let body = if block == Block::TableRule {
                body.text_color(rgb(md.rule))
            } else {
                body
            };
            if !rule {
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
        if block == Block::Rule {
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
        if block == Block::Blank {
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

        let body = div().text_color(rgb(fg)).child(
            StyledText::new(text.clone()).with_highlights(
                sc.merged(
                    at,
                    tokens,
                    spans,
                    theme,
                    kind,
                    moved,
                    selected(sel, 0, full_len),
                )
                .iter()
                .cloned(),
            ),
        );
        let body = match block {
            Block::Heading(level) => body
                .text_size(px(m.size(level)))
                .font_weight(FontWeight::BOLD),
            // A fence's language label is punctuation the reader should be able
            // to skip. A table's pipes are too, but a table is drawn verbatim —
            // see the note on `Block::Table` in `gitten_core::markdown`.
            Block::Fence => body.text_color(rgb(md.marker)),
            _ => body,
        };
        row.child(scrolled(shift, body)).into_any_element()
    }
}

#[cfg(test)]
mod tests {
    // By name, not a glob: `use gpui::*` in the parent shadows `#[test]`.
    use super::{MarkdownRows, Metrics, Row};
    use crate::views::diff::{Diff, Rows, TextRows, PAD, TEXT_CHROME};
    use gitten_core::host::Host;
    use gitten_core::markdown::Block;
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
        let (text, tokens) = r
            .rows
            .iter()
            .find_map(|row| match row {
                super::Row::Line {
                    block: Block::Heading(_),
                    text,
                    tokens,
                    ..
                } => Some((text.clone(), tokens.clone())),
                _ => None,
            })
            .expect("a heading row");
        let t = tokens
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
        for row in &r.rows {
            let super::Row::Line {
                text,
                tokens,
                spans,
                ..
            } = row
            else {
                continue;
            };
            for t in tokens {
                assert!(t.end as usize <= text.len(), "token {t:?} outside {text:?}");
                assert!(
                    text.is_char_boundary(t.start as usize)
                        && text.is_char_boundary(t.end as usize)
                );
            }
            for s in spans {
                assert!(s.end as usize <= text.len(), "span {s:?} outside {text:?}");
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
                super::Row::Line {
                    kind: LineKind::Added,
                    text,
                    spans,
                    ..
                } if !spans.is_empty() => Some(
                    spans
                        .iter()
                        .map(|s| text[s.start as usize..s.end as usize].to_string())
                        .collect::<Vec<_>>(),
                ),
                _ => None,
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
        assert!(r.blocks.len() >= 3, "{:?}", r.blocks);
        for (i, b) in r.blocks.iter().enumerate() {
            assert!(
                !r.blocks[..i].contains(b),
                "{b:?} collected twice: {:?}",
                r.blocks
            );
        }
        for i in 0..r.len() {
            if let Row::Line { block, .. } = &r.rows[i] {
                assert!(r.blocks.contains(block), "{block:?} was never collected");
            }
        }
    }

    /// Every table row of a reflowed document, as `(row, visual row, text)`.
    fn grid(r: &MarkdownRows) -> Vec<(usize, usize, String)> {
        let mut out = Vec::new();
        for i in 0..r.len() {
            let Row::Line {
                block,
                text,
                spans,
                tokens,
                ..
            } = &r.rows[i]
            else {
                continue;
            };
            if !block.is_table() {
                continue;
            }
            let (t, _, _, _) = r.text_of(i, text, spans, tokens);
            for seg in 0..r.rows(i) {
                out.push((i, seg, t[r.wrapped.range(i, seg, t)].to_string()));
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
            if let Row::Line { block, .. } = &r.rows[i] {
                if block.is_table() {
                    tables += 1;
                    assert_eq!(r.rows(i), 1, "a table row wrapped");
                }
            }
        }
        assert_eq!(tables, 3, "the fixture lost its table");
        assert!(
            r.flows.is_empty(),
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
            .filter_map(|i| match &r.rows[i] {
                Row::Line { block, .. } if block.is_table() => Some((i, *block)),
                _ => None,
            })
            .collect();
        assert_eq!(rows.len(), 4, "header, separator and two rows: {rows:?}");
        let (header, sep, first, last) = (rows[0].0, rows[1].0, rows[2].0, rows[3].0);

        assert!(
            !r.ruled(header, 0),
            "a header's separator row is already a rule"
        );
        assert_eq!(rows[1].1, Block::TableRule);
        assert!(
            !r.ruled(sep, 0),
            "the separator row drew a second rule under itself"
        );
        assert!(
            !r.ruled(last, 0),
            "the last row of the table has nothing under it"
        );

        // The wrapped one: under its last sub-row, and none of the others.
        assert!(r.rows(first) > 1, "the fixture stopped wrapping");
        for seg in 0..r.rows(first) - 1 {
            assert!(
                !r.ruled(first, seg),
                "a rule cut through a wrapped cell at {seg}"
            );
        }
        assert!(
            r.ruled(first, r.rows(first) - 1),
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
            let at = r.wrapped.range(row, seg, copied);
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
        let rows: Vec<(usize, Block)> = r
            .rows
            .iter()
            .enumerate()
            .filter_map(|(i, row)| match row {
                Row::Line { block, .. } => Some((i, *block)),
                _ => None,
            })
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
        assert!(r.furniture(Block::Paragraph) < r.furniture(Block::Bullet(1)));

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
            let from = TEXT_CHROME - PAD + r.furniture(*block);
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
        let rows: Vec<(usize, Block)> = r
            .rows
            .iter()
            .enumerate()
            .filter_map(|(i, row)| match row {
                Row::Line { block, .. } => Some((i, *block)),
                _ => None,
            })
            .collect();
        for (i, block) in &rows {
            if r.chars(*i, 0) < 4 {
                continue;
            }
            let from = TEXT_CHROME - PAD + r.furniture(*block);
            let shift = 3.0 * r.char_width(*block, &host);
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
            let Row::Line { block, .. } = r.rows[i] else {
                continue;
            };
            let room = width - TEXT_CHROME - r.furniture(block);
            let text = r.chars(i, 0) as f32 * r.char_width(block, &host);
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
                    r.rows[*i],
                    Row::Line {
                        block: Block::Heading(_),
                        ..
                    }
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
            r.furniture(Block::Bullet(1)) > 0.0,
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
        let heading = r
            .rows
            .iter()
            .position(|row| {
                matches!(
                    row,
                    Row::Line {
                        block: Block::Heading(_),
                        ..
                    }
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
}
