//! Markdown as a document rather than as source.
//!
//! The diff view's default presentation shows a `.md` file the way git does:
//! the characters, coloured. This turns the same lines into something closer to
//! what the file *means* — `## ` gone and the row a size larger, `**` gone and
//! the word bold, `[text](url)` down to `text`. Two things come out of here: a
//! [`Block`] per line saying what the line structurally is, and the line's text
//! with its markers removed and every token and span range moved to match.
//!
//! Nothing in here knows what a window is, so what it produces is a *description*
//! of a rendered row and not a rendered row. The shell turns a [`Block`] into a
//! font size, an indent and a bullet glyph; the ANSI painter could turn the same
//! one into escape codes. That split is the reason this is in `core` at all —
//! marker removal and range remapping are logic, and a second frontend would
//! otherwise have to get them right a second time.
//!
//! # Why this re-parses nothing
//!
//! The markers are already located. By the time a row gets here the
//! [`Markdown`](crate::syntax::Markdown) highlighter has run and emitted a
//! `Strong` token over `**word**`, a `Link` over `[text](url)`, a `Str` over
//! `` `code` `` — delimiters included, because a token is a range of the source.
//! So the set of bytes to hide is derivable from the tokens by *looking at the
//! bytes they cover*, which costs a handful of comparisons per token and no
//! second scan of the line. A real CommonMark parse would be the obvious
//! alternative and is the wrong shape twice over: `core` takes no dependencies,
//! and a hunk is not a document — it hands you the middle of a list and half of
//! a fenced block, constantly.
//!
//! Deriving the cuts from the bytes rather than from the token's provenance is
//! also what keeps this working when `.md` is routed somewhere else. A
//! tree-sitter highlighter emits `Strong` over different ranges; the check is
//! "does this token start with two asterisks", so the answer stays right.
//!
//! It also means this inherits the token pass's blind spots exactly, which is the
//! honest trade. Markup the highlighter does not locate keeps its markers: inline
//! emphasis inside a heading, and a delimiter run that opens on one line and
//! closes on the next. Both come to under half a percent of rows. Both are
//! measured in `docs/measurements.md` and both are deliberate on the
//! highlighter's side — see `docs/decisions/0010-markdown-rendered-rows.md`.

use crate::prepared::Line;
use crate::syntax::{
    column_indent, fence_marker, for_each_side, heading_level, is_break, is_setext, list_marker,
    Kind, Token,
};
use crate::wrap::{Break, Wrap, Wrapped};
use crate::{LineKind, Span};
use std::ops::Range;

/// What a line is, structurally. One per row, computed per hunk side.
///
/// `u8` payloads rather than a nesting `Vec`: the deepest thing anyone writes is
/// a list inside a quote, and a renderer only ever wants "how far in does this
/// sit" — which is a number, and a number that fits in a row's worth of pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Block {
    /// Body text. The common case, and the reason this is the default.
    #[default]
    Paragraph,
    /// Nothing on the line. Draws as nothing, but still occupies a row — it is a
    /// line of the diff and the line numbers either side of it have to add up.
    Blank,
    /// `#` through `######`, or a line underlined with `===` / `---`. Levels are
    /// clamped to 1–6 by the syntax that produces them.
    Heading(u8),
    /// `-`, `*` or `+`, with its indent depth.
    Bullet(u8),
    /// `1.` or `1)`, with its indent depth. The number stays in the text: it is
    /// content, not punctuation, and hiding it would lose which item this is.
    Ordered(u8),
    /// `>`, carrying how many deep.
    Quote(u8),
    /// The ``` or ~~~ line itself. Its text is the language, if it named one.
    Fence,
    /// A line inside a fenced block.
    Code,
    /// A `|` row, aligned to the columns of the run it belongs to.
    Table,
    /// A table's separator row (`|---|:-:|`), which also carries the column
    /// alignments for its run. Its text is *regenerated* as a rule sized to the
    /// columns, so unlike every other block this one does not draw the bytes it
    /// arrived with.
    TableRule,
    /// A thematic break: a line whose only content is punctuation drawing a
    /// line. The renderer draws an actual rule and does not draw the text.
    Rule,
}

impl Block {
    /// How far in this line sits, in indent steps. The renderer multiplies by
    /// whatever one step is worth in pixels.
    pub fn depth(self) -> u8 {
        match self {
            Block::Bullet(d) | Block::Ordered(d) => d,
            // A table row draws its own grid and sits flush left, whatever the
            // source indented it by.
            Block::Table | Block::TableRule => 0,
            // The bar the renderer draws already sits one step in, so the first
            // level of quoting costs no indent of its own.
            Block::Quote(d) => d.saturating_sub(1),
            _ => 0,
        }
    }

    /// Whether the line's text is code, and so should not have inline markup
    /// read out of it. A `*` inside a fenced block is a dereference, not
    /// emphasis, and the fence line's own backticks are not a code span.
    pub fn is_code(self) -> bool {
        matches!(self, Block::Code | Block::Fence)
    }

    /// Whether this line is part of a table, and so has to be measured against
    /// its neighbours rather than laid out on its own.
    pub fn is_table(self) -> bool {
        matches!(self, Block::Table | Block::TableRule)
    }
}

/// The glyphs a table's grid is drawn with.
///
/// Configurable because box-drawing characters are an assumption about the font:
/// they are single-width in Menlo and most terminal faces, and a face that draws
/// them wider would knock every column out of line. Swap in `"|"`, `"-"`, `"|"`,
/// `"|"` for pure ASCII.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TableGlyphs {
    /// Between cells, and at both ends of a row.
    pub vertical: &'static str,
    /// The body of a separator row.
    pub horizontal: &'static str,
    /// Where a separator row crosses a column boundary.
    pub cross: &'static str,
    /// Both ends of a separator row.
    pub end: (&'static str, &'static str),
}

impl Default for TableGlyphs {
    fn default() -> Self {
        Self {
            vertical: "│",
            horizontal: "─",
            cross: "┼",
            end: ("├", "┤"),
        }
    }
}

/// What [`lay_out`] may assume about the frontend that will draw the result.
///
/// One field, and it exists because of one fact: **table alignment is done by
/// padding cells with spaces**, which only lines up if a character is a column.
/// `core` cannot see a font, so the frontend says. With `monospaced: false` a
/// table is left exactly as it was written, which is the honest answer rather
/// than a grid that is wrong by a fraction of a glyph per cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Layout {
    pub monospaced: bool,
    pub table: TableGlyphs,
}

impl Layout {
    /// The shipped assumption: a monospaced face, box-drawing glyphs. The GPUI
    /// shell sets `font_family("Menlo")` and the ANSI painter inherits whatever
    /// the terminal uses, so both are monospaced.
    pub fn monospaced() -> Self {
        Self {
            monospaced: true,
            table: TableGlyphs::default(),
        }
    }

    /// Everything except the table grid, for a frontend whose text is
    /// proportionally spaced.
    pub fn proportional() -> Self {
        Self {
            monospaced: false,
            table: TableGlyphs::default(),
        }
    }
}

/// How a table column is aligned, from its separator row: `:-:` centres, `-:`
/// goes right. Left when the separator says nothing, or is not in this hunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
}

/// Lays out one hunk's worth of already-prepared lines, in place.
///
/// Returns a [`Block`] per line and mutates each line so its text carries no
/// markers and its tokens and spans still index it. In place because that is
/// free: marker removal is deletion only, so [`String::drain`] reuses the
/// buffer the line already owns and nothing here allocates per row.
///
/// Runs per hunk *side* — see [`for_each_side`]. A fence opens on one side and
/// not the other all the time, and classifying the interleaved rows as one
/// document would have a removed ``` closing an added one.
pub fn lay_out(lines: &mut [Line], layout: &Layout) -> Vec<Block> {
    lay_out_tables(lines, layout).0
}

/// [`lay_out`], and also the table grids it measured.
///
/// For the one caller that has something to do with them: a frontend that knows
/// how wide the window is, and has found a grid wider than it — see
/// [`flow_table`]. Everything else wants [`lay_out`] and the [`Block`]s.
pub fn lay_out_tables(lines: &mut [Line], layout: &Layout) -> (Vec<Block>, Tables) {
    let mut blocks = vec![Block::Paragraph; lines.len()];
    let kinds: Vec<LineKind> = lines.iter().map(|l| l.kind).collect();

    for_each_side(&kinds, |rows| {
        let mut fence: Option<&'static str> = None;
        let mut prev: Option<(usize, Block)> = None;
        for &i in rows {
            let block = classify(&lines[i].text, &mut fence, prev.map(|p| p.1));
            // A setext underline is not a row of its own — it is the previous
            // row's heading level, arriving one line late. Promote that row and
            // leave this one blank: the underline characters are punctuation
            // describing a line that is already on screen, and drawing them
            // under a heading that is already sized like one is noise.
            if let (Block::Rule, Some((p, Block::Paragraph))) = (block, prev) {
                if !lines[i].text.is_empty() && is_setext(lines[i].text.trim_start()) {
                    blocks[p] = Block::Heading(if lines[i].text.trim_start().starts_with('=') {
                        1
                    } else {
                        2
                    });
                    blocks[i] = Block::Blank;
                    prev = Some((i, Block::Blank));
                    continue;
                }
            }
            blocks[i] = block;
            prev = Some((i, block));
        }
    });

    let mut cuts: Vec<Range<usize>> = Vec::with_capacity(8);
    for (line, &block) in lines.iter_mut().zip(&blocks) {
        mark(line, block, &mut cuts);
        apply(line, &cuts);
    }

    // Third, and only if there is a table at all: a cell's width is a property
    // of the *run* of rows it sits in, not of its own line, so this cannot join
    // either pass above. It runs after the cuts because it measures what will be
    // drawn — a cell holding `` `code` `` is two backticks narrower by now.
    //
    // Per side again, for the same reason as the block pass: the removed rows and
    // the added rows of a table are two different tables and must not be measured
    // against each other, or a column widens on one side because the other side
    // has a long cell in it.
    let tables = match layout.monospaced && blocks.iter().any(|b| b.is_table()) {
        true => align_tables(lines, &blocks, &kinds, layout),
        false => Tables::default(),
    };
    (blocks, tables)
}

// -------------------------------------------------------------------- tables

/// One table's measured columns: how wide each one wants to be, and which way
/// its cells sit in it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Grid {
    /// Widest cell in each column, in characters.
    pub widths: Vec<usize>,
    /// From the separator row, when the hunk contains one.
    pub aligns: Vec<Align>,
}

impl Grid {
    /// How wide the whole grid is, in characters: a pipe at each edge and one
    /// between every pair of columns, and a space either side of every cell.
    pub fn width(&self) -> usize {
        self.widths.iter().sum::<usize>() + 3 * self.widths.len() + 1
    }
}

/// The tables in one hunk: what each was aligned to, and which lines are in it.
///
/// Empty for the hunks that contain no table, which is most of them. It exists
/// so the grid can be laid out a second time at a width `lay_out` cannot know —
/// see [`flow_table`]. Re-deriving the grouping there would be a second answer to
/// "which rows are one table", and two answers drifting apart is a column
/// measured against the wrong neighbours.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tables {
    /// One per run of table rows, in the order the runs were met.
    pub grids: Vec<Grid>,
    /// `(line, grid)` for every line that is in a measured table, ascending by
    /// line. Sparse because a table is 1–2.5% of the rows of a diff.
    pub of_line: Vec<(u32, u32)>,
}

/// Aligns every table in the hunk to its own columns.
///
/// **Measure everything first, then rewrite each row once.** Not an optimisation
/// — a correctness requirement, and a sharp edge worth understanding before
/// adding a third caller to [`for_each_side`]. A context row belongs to both
/// sides and so is visited twice. The token pass does not care, because it
/// *assigns* `out[row]` and assigning twice is the same as assigning once. This
/// pass *mutates* the row, and padding an already-padded row is not idempotent:
/// the second visit measured a grid that the first visit had already drawn, and
/// tripped over the three-byte `│` it had just written.
///
/// So the two phases are separated. Measurement reads only original text;
/// `of_row` records which grid each row belongs to, and because the added side is
/// visited last it wins for context rows — which is the same rule the token pass
/// follows for the same reason.
fn align_tables(
    lines: &mut [Line],
    blocks: &[Block],
    kinds: &[LineKind],
    layout: &Layout,
) -> Tables {
    let mut grids: Vec<Grid> = Vec::new();
    let mut of_row: Vec<Option<usize>> = vec![None; lines.len()];

    for_each_side(kinds, |rows| {
        let mut i = 0;
        while i < rows.len() {
            if !blocks[rows[i]].is_table() {
                i += 1;
                continue;
            }
            // A run, not the whole hunk: two tables separated by a paragraph are
            // two grids with two sets of widths, and a diff shows that often.
            let start = i;
            while i < rows.len() && blocks[rows[i]].is_table() {
                i += 1;
            }
            let run = &rows[start..i];
            if let Some(grid) = measure(lines, blocks, run) {
                grids.push(grid);
                let g = grids.len() - 1;
                for &r in run {
                    of_row[r] = Some(g);
                }
            }
        }
    });

    let mut cells: Vec<Range<usize>> = Vec::new();
    let mut of_line: Vec<(u32, u32)> = Vec::with_capacity(of_row.len() / 32);
    for (r, grid) in of_row.iter().enumerate() {
        let Some(g) = *grid else { continue };
        of_line.push((r as u32, g as u32));
        let grid = &grids[g];
        if blocks[r] == Block::TableRule {
            rule_row(&mut lines[r], &grid.widths, layout);
        } else {
            split_cells(&lines[r].text, &mut cells);
            grid_row(&mut lines[r], &cells, grid, layout);
        }
    }
    Tables { grids, of_line }
}

/// Column widths and alignments for one run. `None` when the run is nothing but
/// separators and so has no content to align to.
fn measure(lines: &[Line], blocks: &[Block], run: &[usize]) -> Option<Grid> {
    let mut widths: Vec<usize> = Vec::new();
    let mut aligns: Vec<Align> = Vec::new();
    let mut cells: Vec<Range<usize>> = Vec::new();

    for &r in run {
        split_cells(&lines[r].text, &mut cells);
        // A separator contributes alignment but no width: its own dashes are
        // punctuation about to be replaced, so measuring them would let
        // `|:----------:|` set the column instead of the content.
        if blocks[r] == Block::TableRule {
            if aligns.len() < cells.len() {
                aligns.resize(cells.len(), Align::Left);
            }
            for (k, c) in cells.iter().enumerate() {
                aligns[k] = alignment(&lines[r].text[c.clone()]);
            }
            continue;
        }
        if widths.len() < cells.len() {
            widths.resize(cells.len(), 0);
        }
        for (k, c) in cells.iter().enumerate() {
            // Characters, not bytes: the padding is counted in columns and a
            // cell saying "café" is four of them.
            widths[k] = widths[k].max(lines[r].text[c.clone()].chars().count());
        }
    }
    (!widths.is_empty()).then_some(Grid { widths, aligns })
}

/// Content ranges of a row's cells, pipes and surrounding spaces excluded.
///
/// A `\|` is a pipe in a cell, not a cell boundary — the one escape that matters
/// here, because a table of operators is full of them.
fn split_cells(text: &str, out: &mut Vec<Range<usize>>) {
    out.clear();
    let b = text.as_bytes();
    let start = text.len() - text.trim_start().len();
    // The row begins with a pipe — `classify` required it — so the first field
    // before it is empty and is not a cell.
    let mut from = start + 1;
    let mut i = from;
    while i < b.len() {
        if b[i] == b'|' && (i == 0 || b[i - 1] != b'\\') {
            out.push(trimmed_range(text, from..i));
            from = i + 1;
        }
        i += 1;
    }
    // Anything after the last pipe. A row may or may not close with one; if it
    // does, what follows is the empty field after it and not a cell.
    let tail = trimmed_range(text, from..text.len());
    if tail.start < tail.end {
        out.push(tail);
    }
}

fn trimmed_range(text: &str, r: Range<usize>) -> Range<usize> {
    let slice = &text[r.clone()];
    let lead = slice.len() - slice.trim_start().len();
    let trail = slice.len() - slice.trim_end().len();
    r.start + lead..r.end - trail.min(slice.len() - lead)
}

fn alignment(cell: &str) -> Align {
    let c = cell.trim();
    match (c.starts_with(':'), c.ends_with(':')) {
        (true, true) => Align::Center,
        (false, true) => Align::Right,
        _ => Align::Left,
    }
}

/// Rewrites a data row onto the measured grid.
fn grid_row(line: &mut Line, cells: &[Range<usize>], grid: &Grid, layout: &Layout) {
    let (widths, aligns) = (&grid.widths, &grid.aligns);
    let g = &layout.table;
    let mut out = String::with_capacity(line.text.len() + widths.len() * 8);
    // Where each cell's content, copied verbatim, now begins. This is the whole
    // correspondence between the old text and the new one — see `remap`.
    let mut map: Vec<(Range<usize>, usize)> = Vec::with_capacity(cells.len());
    out.push_str(g.vertical);
    for (k, &w) in widths.iter().enumerate() {
        // A row with fewer cells than the widest one still gets the missing
        // columns, empty. Ragged tables are normal in hand-written markdown and
        // a short row that stopped early would break the grid below it.
        let content = cells.get(k).map_or("", |c| &line.text[c.clone()]);
        let pad = w.saturating_sub(content.chars().count());
        let (before, after) = match aligns.get(k).copied().unwrap_or_default() {
            Align::Left => (0, pad),
            Align::Right => (pad, 0),
            Align::Center => (pad / 2, pad - pad / 2),
        };
        out.push(' ');
        for _ in 0..before {
            out.push(' ');
        }
        if let Some(c) = cells.get(k) {
            map.push((c.clone(), out.len()));
        }
        out.push_str(content);
        for _ in 0..after {
            out.push(' ');
        }
        out.push(' ');
        out.push_str(g.vertical);
    }

    remap(line, &map, out.len());
    line.text = out.into();
}

/// Replaces a separator row's dashes with a rule sized to the columns.
///
/// The only row whose text is generated rather than trimmed, so it is also the
/// only one whose tokens and spans are dropped: they describe dashes that are no
/// longer there. Nothing is lost that a reader wanted — a separator row differing
/// between two revisions says nothing the columns either side of it do not.
fn rule_row(line: &mut Line, widths: &[usize], layout: &Layout) {
    line.text = rule_text(widths, layout).into();
    // Empty boxed slices: nothing allocates to say "no ranges".
    line.tokens = Box::default();
    line.spans = Box::default();
}

/// The rule itself, which [`flow_table`] draws again at the widths it settled on.
fn rule_text(widths: &[usize], layout: &Layout) -> String {
    let g = &layout.table;
    let mut out = String::with_capacity(widths.iter().sum::<usize>() + widths.len() * 8);
    out.push_str(g.end.0);
    for (k, w) in widths.iter().enumerate() {
        for _ in 0..w + 2 {
            out.push_str(g.horizontal);
        }
        out.push_str(if k + 1 == widths.len() {
            g.end.1
        } else {
            g.cross
        });
    }
    out
}

/// Moves tokens and spans onto a rewritten line.
///
/// `map` is the correspondence, and it is piecewise-linear: each entry says that
/// a range of the old text now begins at a new offset with its bytes unchanged.
/// A position inside a piece keeps its offset within it; a position outside every
/// piece was punctuation or padding the row no longer draws in the same place,
/// and collapses to the start of the next piece it can reach — which makes any
/// token that described only punctuation collapse to nothing and be dropped.
///
/// This is the general form of [`apply`], which the rest of the module uses
/// instead. Both exist on purpose: `apply` handles deletion only, and because
/// deletion only it works in place on the buffer the line already owns and costs
/// no allocation, which matters when it runs on every row of a 71k-row diff.
/// A table is 1–2.5% of rows and needs insertion, so it pays for the general one.
fn remap(line: &mut Line, map: &[(Range<usize>, usize)], new_len: usize) {
    let at = |p: usize| -> usize {
        for (old, new) in map {
            if p < old.start {
                return *new;
            }
            if p <= old.end {
                return new + (p - old.start);
            }
        }
        new_len
    };
    // Rebuilt rather than edited: ranges live in exact-size boxed slices, and a
    // table is a small enough fraction of rows that one collection is nothing.
    line.tokens = line
        .tokens
        .iter()
        .map(|t| (at(t.start as usize), at(t.end as usize), t.kind))
        .filter(|(s, e, _)| s < e)
        .map(|(s, e, k)| Token {
            start: s as u32,
            end: e as u32,
            kind: k,
        })
        .collect::<Vec<_>>()
        .into_boxed_slice();
    line.spans = line
        .spans
        .iter()
        .map(|s| (at(s.start as usize), at(s.end as usize)))
        .filter(|(s, e)| s < e)
        .map(|(s, e)| Span {
            start: s as u32,
            end: e as u32,
        })
        .collect::<Vec<_>>()
        .into_boxed_slice();
}

// ------------------------------------------------- fitting a grid to a window

/// One row of a table run as the frontend holds it: the gridded text it is
/// drawing, and the ranges that index it.
///
/// Borrowed, because this runs per resize over every table on the screen: a flow
/// that copied its input would allocate twice for every row it kept.
#[derive(Debug, Clone, Copy)]
pub struct TableRow<'a> {
    pub text: &'a str,
    pub block: Block,
    pub tokens: &'a [Token],
    pub spans: &'a [Span],
}

/// What one row becomes when its grid has to fit a budget: every sub-row laid end
/// to end, and the breaks that partition them.
///
/// One string and not a `Vec<String>`, for the reason
/// [`Wrapped`](crate::wrap::Wrapped) is one table and not a `Vec<Vec<Break>>`:
/// the rows of a wrapped line are ranges into the line everywhere else in the
/// pipeline, and a table row three rows tall is either that same shape or a
/// second one for a renderer to get wrong.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FlowRow {
    /// Every sub-row, each exactly as wide as the new grid, joined by newlines —
    /// so a range out of `Wrapped` is one row of the table and nothing else.
    ///
    /// The newline is the one in the whole pipeline: a line of a diff has none.
    /// It is here because this text is also what a *copy* of the row yields, and
    /// three rows of a grid run together into one 90-character line is not what
    /// was on the screen. The breaks drop it, exactly as word wrap drops the
    /// space it broke on, so nothing ever draws it.
    pub text: String,
    /// Where the sub-rows end, and where the next begins: the newline between
    /// them falls in the gap and is not drawn.
    pub breaks: Vec<Break>,
    /// The row's tokens, moved onto `text` — and *split*, because a token whose
    /// text wrapped covers two pieces that are no longer next to each other.
    pub tokens: Vec<Token>,
    /// The row's intraline spans, the same way.
    pub spans: Vec<Span>,
}

/// Re-lays gridded table rows onto a grid that fits `cols` columns, wrapping each
/// cell inside its own column.
///
/// `run` is the rows that share `grid` — a whole run of them or any part of one,
/// since the widths come from the grid and not from what is passed in. One flowed
/// row comes back per row passed in, in the same order.
///
/// The width is the one part of a table's layout that depends on the window, so
/// it is the one part that cannot happen at load: which rows are one table, what
/// its columns are and how wide each wants to be were all settled by
/// [`lay_out_tables`] and arrive in [`Tables`].
///
/// **A wrapped table is more rows, not a truncated one.** Squeezing the columns
/// alone does not fit a paragraph into 40 characters, and cutting the cell off
/// hides content with no way to reach it — so a row becomes as many rows as its
/// tallest cell needs, which is the same answer `docs/decisions/0017` gives for
/// prose and keeps `uniform_list`'s fixed row height.
///
/// `None` when there is nothing to do, or nothing that can be done: the grid
/// already fits, the face is not monospaced so there is no grid to begin with,
/// the wrap is `off` so a cell cannot be broken and scrolling is the honest
/// answer, or the budget is narrower than the pipes and padding themselves.
pub fn flow_table(
    run: &[TableRow],
    grid: &Grid,
    cols: usize,
    layout: &Layout,
    wrap: &dyn Wrap,
) -> Option<Vec<FlowRow>> {
    if !layout.monospaced || !wrap.breaks_lines() || grid.width() <= cols {
        return None;
    }
    let widths = squeeze(&grid.widths, cols)?;
    let mut cells: Vec<Range<usize>> = Vec::new();
    let mut pieces: Vec<Vec<Range<usize>>> = Vec::new();
    let mut out = Vec::with_capacity(run.len());
    for row in run {
        out.push(match row.block {
            // A separator row is nothing but grid, so it is drawn again at the
            // widths that were settled on — the same regeneration `rule_row`
            // does at load, for the same reason: its dashes were punctuation
            // about to be replaced.
            Block::TableRule => FlowRow {
                text: rule_text(&widths, layout),
                ..Default::default()
            },
            _ => flow_row(row, &widths, grid, layout, wrap, &mut cells, &mut pieces),
        });
    }
    Some(out)
}

/// One data row onto the narrower grid.
fn flow_row(
    row: &TableRow,
    widths: &[usize],
    grid: &Grid,
    layout: &Layout,
    wrap: &dyn Wrap,
    cells: &mut Vec<Range<usize>>,
    pieces: &mut Vec<Vec<Range<usize>>>,
) -> FlowRow {
    grid_cells(row.text, layout, cells);
    // Grown and cleared per column rather than `pieces.clear()`, which drops the
    // buffers themselves — this runs on every row of every table on every drag
    // that crosses a character.
    if pieces.len() < widths.len() {
        pieces.resize(widths.len(), Vec::new());
    }
    for (k, w) in widths.iter().enumerate() {
        pieces[k].clear();
        let cell = cells.get(k).cloned().unwrap_or(0..0);
        break_cell(
            &row.text[cell.clone()],
            cell.start,
            *w,
            wrap,
            &mut pieces[k],
        );
    }
    let pieces = &pieces[..widths.len()];
    // As tall as the cell that needed the most rows, and never zero: a row of
    // empty cells is still a row of the table.
    let tall = pieces.iter().map(|p| p.len()).max().unwrap_or(1).max(1);

    let g = &layout.table;
    let mut out = FlowRow {
        text: String::with_capacity(tall * (widths.iter().sum::<usize>() + widths.len() * 8)),
        ..Default::default()
    };
    for r in 0..tall {
        // Between the sub-rows, not after them: the last one ends where the text
        // does, and a break there is a row with nothing in it.
        if r > 0 {
            let end = out.text.len() as u32;
            out.text.push('\n');
            out.breaks.push(Break {
                end,
                next: out.text.len() as u32,
            });
        }
        out.text.push_str(g.vertical);
        for (k, w) in widths.iter().enumerate() {
            // A column this row has no cell for still gets drawn, empty — a
            // ragged table is normal by hand and a row that stopped early would
            // break the grid below it.
            let piece = pieces[k].get(r).cloned().unwrap_or(0..0);
            let content = &row.text[piece.clone()];
            let pad = w.saturating_sub(content.chars().count());
            let (before, after) = match grid.aligns.get(k).copied().unwrap_or_default() {
                Align::Left => (0, pad),
                Align::Right => (pad, 0),
                Align::Center => (pad / 2, pad - pad / 2),
            };
            out.text.push(' ');
            for _ in 0..before {
                out.text.push(' ');
            }
            carry(row, &piece, out.text.len(), &mut out.tokens, &mut out.spans);
            out.text.push_str(content);
            for _ in 0..after {
                out.text.push(' ');
            }
            out.text.push(' ');
            out.text.push_str(g.vertical);
        }
    }
    out
}

/// One cell's content, broken into the pieces that fit `cols` columns, in the
/// coordinates of the row it came out of.
///
/// Through the same [`Wrapped`](crate::wrap::Wrapped) a line goes through, and
/// deliberately not by hand: what breaks prose everywhere else in the diff is
/// whichever [`Wrap`] is selected, an extension's included, and a table cell is
/// prose. It also means the validation is the same validation.
fn break_cell(cell: &str, at: usize, cols: usize, wrap: &dyn Wrap, out: &mut Vec<Range<usize>>) {
    let wrapped = Wrapped::build(std::iter::once((cell, cols.max(1))), wrap);
    out.extend((0..wrapped.rows(0)).map(|r| {
        let p = wrapped.range(0, r, cell);
        at + p.start..at + p.end
    }));
}

/// Copies the tokens and spans covering one piece of the old text onto where that
/// piece landed, clipped to it.
///
/// Clipped, not moved: a `**phrase**` whose text wrapped is two pieces on two
/// rows now, and one range spanning them would paint everything between — the
/// rest of the row's columns included. Two ranges is what it is.
fn carry(
    row: &TableRow,
    piece: &Range<usize>,
    at: usize,
    tokens: &mut Vec<Token>,
    spans: &mut Vec<Span>,
) {
    let clip = |from: usize, to: usize| -> Option<Range<usize>> {
        let (start, end) = (from.max(piece.start), to.min(piece.end));
        (start < end).then(|| at + (start - piece.start)..at + (end - piece.start))
    };
    for t in row.tokens {
        if let Some(r) = clip(t.start as usize, t.end as usize) {
            tokens.push(Token {
                start: r.start as u32,
                end: r.end as u32,
                kind: t.kind,
            });
        }
    }
    for s in row.spans {
        if let Some(r) = clip(s.start as usize, s.end as usize) {
            spans.push(Span {
                start: r.start as u32,
                end: r.end as u32,
            });
        }
    }
}

/// Column widths that fit `cols`, from the widths the content wants.
///
/// **Water-filling, not proportional.** Shrinking every column by the same
/// fraction takes the `Yes` out of a three-character column to save a paragraph
/// four characters it will not notice. So every column that already fits its
/// share keeps the width it wants, and what is left over is split between the
/// ones that do not — repeatedly, because settling one raises the share for the
/// rest.
///
/// `None` when the pipes and the padding are already wider than the budget, or
/// when what is left will not give every column a character. A table with no
/// room to draw a word of is worse than a table you have to scroll.
fn squeeze(natural: &[usize], cols: usize) -> Option<Vec<usize>> {
    let n = natural.len();
    let available = cols.checked_sub(3 * n + 1)?;
    if n == 0 || available < n {
        return None;
    }
    let mut widths = vec![0usize; n];
    let mut fixed = vec![false; n];
    loop {
        let free: Vec<usize> = (0..n).filter(|&k| !fixed[k]).collect();
        if free.is_empty() {
            return Some(widths);
        }
        let spent: usize = (0..n).filter(|&k| fixed[k]).map(|k| widths[k]).sum();
        // At least 1: a column settles for no more than its share, so the budget
        // left over is never less than one character for each column left.
        let share = (available - spent) / free.len();
        let mut settled = false;
        for &k in &free {
            if natural[k] <= share {
                widths[k] = natural[k];
                fixed[k] = true;
                settled = true;
            }
        }
        if !settled {
            // Nothing left fits its share, so the share is what they all get.
            // The remainder goes to the widest of them, because that is where
            // another character does the most good.
            let mut order = free;
            order.sort_by_key(|&k| std::cmp::Reverse(natural[k]));
            let mut rest = available - spent;
            for (i, &k) in order.iter().enumerate() {
                widths[k] = rest / (order.len() - i);
                rest -= widths[k];
            }
            return Some(widths);
        }
    }
}

/// Content ranges of a *gridded* row's cells — the job [`split_cells`] does on
/// source markdown, done on the text [`grid_row`] wrote.
///
/// Two differences, both of which are why it is a second function. The boundary
/// is the configured glyph and not a pipe; and there is no escape to honour,
/// because whatever `\|` meant in the source is inside a cell by now. A cell
/// whose *content* holds the glyph itself splits wrongly, which is the same
/// class of thing as `split_cells` and a table drawing its own box glyphs.
fn grid_cells(text: &str, layout: &Layout, out: &mut Vec<Range<usize>>) {
    out.clear();
    let v = layout.table.vertical;
    let Some(open) = text.find(v) else { return };
    let mut from = open + v.len();
    while let Some(i) = text[from..].find(v) {
        let end = from + i;
        out.push(trimmed_range(text, from..end));
        from = end + v.len();
    }
}

/// One line's structural role. `fence` carries across lines and is the reason
/// this cannot be answered per row; `prev` is only consulted to tell a thematic
/// break from a table separator.
fn classify(line: &str, fence: &mut Option<&'static str>, prev: Option<Block>) -> Block {
    let trimmed = line.trim_start();

    // Fences first and unconditionally: inside a block, a `#` is a comment and a
    // `-` is a minus sign, and nothing else in here may look at them.
    match (*fence, fence_marker(trimmed)) {
        (Some(open), Some(found)) if open == found => {
            *fence = None;
            return Block::Fence;
        }
        (Some(_), _) => return Block::Code,
        (None, Some(found)) => {
            *fence = Some(found);
            return Block::Fence;
        }
        (None, None) => {}
    }

    if trimmed.is_empty() {
        return Block::Blank;
    }

    // The whole line, not the trimmed one: past three columns of indent a `#` is
    // a comment inside an indented code block, not a heading. Shared with the
    // token pass so the two cannot disagree about it.
    if let Some(level) = heading_level(line) {
        return Block::Heading(level);
    }
    // A table separator is a rule *and* the row that says how its columns are
    // aligned, so it is its own block rather than a thematic break that happens
    // to sit inside a table.
    if is_table_separator(trimmed) {
        return Block::TableRule;
    }
    // `---` is a thematic break on its own, and `is_setext` also catches the
    // underline case that `lay_out` resolves by looking back at the row above.
    // Both draw as a rule, so they are one arm.
    if is_break(trimmed) || is_setext(trimmed) {
        return Block::Rule;
    }
    if trimmed.starts_with('>') {
        return Block::Quote(quote_depth(trimmed));
    }
    if trimmed.starts_with('|') {
        return Block::Table;
    }

    let indent = column_indent(line);
    let marker = list_marker(trimmed);
    if marker > 0 {
        // Two columns per step is the common convention and four is the other
        // one; halving splits the difference and either way the eye only needs
        // the ordering, not the exact measure. Capped because a row is 22 pixels
        // tall and a runaway indent walks the text off the side.
        let depth = ((indent / 2) as u8).min(3);
        return if trimmed.as_bytes()[0].is_ascii_digit() {
            Block::Ordered(depth)
        } else {
            Block::Bullet(depth)
        };
    }
    // An indented run with no marker is a continuation line, and the previous
    // row's depth is what keeps it under its bullet instead of snapping left.
    if indent >= 2 {
        if let Some(Block::Bullet(d) | Block::Ordered(d)) = prev {
            return Block::Ordered(d);
        }
    }
    if prev == Some(Block::Blank) && !is_prose_start(trimmed) {
        return Block::Paragraph;
    }
    Block::Paragraph
}

/// `|---|:--:|` — punctuation only between the pipes, and at least one dash.
///
/// The dash is the load-bearing part. Without it `| | |` qualifies, and an empty
/// header row is a real thing people write: `docs/README.md` in this repository
/// opens a table with one, and it was drawn as a thematic break until this said
/// so. A separator is a row that draws a line; a row of empty cells is a row.
fn is_table_separator(trimmed: &str) -> bool {
    trimmed.starts_with('|')
        && trimmed.trim_end().len() > 2
        && trimmed.bytes().any(|b| b == b'-')
        && trimmed
            .bytes()
            .all(|b| matches!(b, b'|' | b'-' | b':' | b' ' | b'\t'))
}

fn quote_depth(trimmed: &str) -> u8 {
    let mut depth = 0u8;
    for b in trimmed.bytes() {
        match b {
            b'>' => depth = depth.saturating_add(1),
            b' ' | b'\t' => {}
            _ => break,
        }
    }
    depth.max(1)
}

/// Kept deliberately trivial: it exists so `classify` reads as a list of rules
/// rather than falling off the end into an unexplained default.
fn is_prose_start(_trimmed: &str) -> bool {
    true
}

// ------------------------------------------------------------------- the cuts

/// Byte ranges to hide, appended to `out` sorted and disjoint.
///
/// Two sources. The block's own prefix — the `## `, the `- `, the `> ` — comes
/// from the block, which is the only thing that knows the line is a heading. The
/// inline delimiters come from the tokens, by checking the bytes at each end of
/// the token rather than trusting which highlighter produced it.
fn mark(line: &Line, block: Block, out: &mut Vec<Range<usize>>) {
    out.clear();
    // Most rows have nothing to cut and leave before anything is scanned. On
    // `fixtures/real/md.diff` blank lines are 28% of rows and paragraphs 32%,
    // and a paragraph with no inline markup carries no tokens — so the majority
    // of a markdown diff exits here on a discriminant check.
    if matches!(
        block,
        Block::Blank | Block::Rule | Block::TableRule | Block::Code
    ) {
        return;
    }
    if line.tokens.is_empty()
        && matches!(block, Block::Paragraph | Block::Ordered(_) | Block::Table)
    {
        return;
    }
    let text = &line.text;
    let b = text.as_bytes();
    let indent = text.len() - text.trim_start().len();

    let prefix_end = match block {
        // The indent goes too: a heading is not indented, whatever the source did.
        // Only ATX headings wear hashes — a setext one was promoted from a
        // paragraph by the line *under* it and has no marker of its own, so the
        // bytes decide rather than the level.
        Block::Heading(n) => Some(if b.get(indent) == Some(&b'#') {
            skip_space(b, indent + n as usize)
        } else {
            indent
        }),
        // The marker goes; the depth comes back as furniture and an indent, so
        // the text still lines up under its bullet without carrying a `- `.
        Block::Bullet(_) => Some(indent + list_marker(text.trim_start())),
        // Every `>` and the spaces between them. The bar the renderer draws in
        // their place says the same thing in one column instead of four.
        Block::Quote(_) => {
            let mut i = indent;
            while i < b.len() && matches!(b[i], b'>' | b' ' | b'\t') {
                i += 1;
            }
            Some(i)
        }
        // ``` or ~~~, leaving the language name behind as the row's only text.
        Block::Fence => {
            let run = b[indent..]
                .iter()
                .take_while(|c| matches!(c, b'`' | b'~'))
                .count();
            Some(skip_space(b, indent + run))
        }
        // Ordered items keep their number, tables keep their pipes, code keeps
        // every byte it has, and a rule and a blank have no text to draw.
        _ => None,
    };
    if let Some(end) = prefix_end {
        if end > 0 && end <= b.len() {
            out.push(0..end);
        }
    }

    // Rules and blanks draw no text at all, so there is nothing to trim inside
    // them, and code is code: a `*` in a fenced block is not emphasis.
    if block.is_code() || matches!(block, Block::Rule | Block::Blank) {
        return;
    }

    // A closed ATX heading — `## Title ##` — wears its markers at both ends.
    if let Block::Heading(_) = block {
        let end = text.trim_end().len();
        let hashes = b[..end].iter().rev().take_while(|c| **c == b'#').count();
        if hashes > 0 && b[..end - hashes].last() == Some(&b' ') {
            let from = end - hashes - 1;
            if out.last().is_none_or(|c| c.end <= from) {
                out.push(from..end);
            }
        }
    }

    for t in &line.tokens {
        let (ts, te) = (t.start as usize, t.end as usize);
        if te > b.len() || ts >= te {
            continue;
        }
        let delim = match t.kind {
            // Two bytes a side by construction, but checked rather than assumed
            // so a differently-shaped token from another highlighter is left
            // alone instead of losing two bytes of its content.
            Kind::Strong => wrapped(b, ts, te, 2, |c| c == b'*' || c == b'_'),
            Kind::Emphasis => wrapped(b, ts, te, 1, |c| c == b'*' || c == b'_'),
            // A backtick run of any length, matched at both ends: ``a `b` c``
            // is one code span delimited by two.
            Kind::Str => {
                let n = b[ts..te].iter().take_while(|c| **c == b'`').count();
                (n > 0)
                    .then(|| wrapped(b, ts, te, n, |c| c == b'`'))
                    .flatten()
            }
            Kind::Link => {
                link_cuts(b, ts, te, out);
                None
            }
            _ => None,
        };
        if let Some(n) = delim {
            push_sorted(out, ts..ts + n);
            push_sorted(out, te - n..te);
        }
    }

    debug_assert!(
        out.windows(2).all(|w| w[0].end <= w[1].start),
        "cuts overlap or are unsorted: {out:?} in {text:?}"
    );
    debug_assert!(
        out.iter()
            .all(|c| text.is_char_boundary(c.start) && text.is_char_boundary(c.end)),
        "cut off a char boundary: {out:?} in {text:?}"
    );
}

/// `n` if the token wears `n` matching delimiter bytes at each end and has
/// anything left in between, otherwise `None`. The last condition is what stops
/// `****` from collapsing to nothing.
fn wrapped(
    b: &[u8],
    start: usize,
    end: usize,
    n: usize,
    is_delim: impl Fn(u8) -> bool,
) -> Option<usize> {
    (end - start > 2 * n
        && b[start..start + n].iter().all(|c| is_delim(*c))
        && b[end - n..end].iter().all(|c| is_delim(*c)))
    .then_some(n)
}

/// `[text](url)` down to `text`, and `![alt](url)` down to `alt`.
///
/// The URL is the half of a link nobody reads and three quarters of its width.
/// It is not thrown away as far as the user is concerned — the row underneath is
/// the source, one keystroke away in the default presentation — but on a rendered
/// row it is what pushes the sentence off the screen.
fn link_cuts(b: &[u8], start: usize, end: usize, out: &mut Vec<Range<usize>>) {
    if b.get(start) != Some(&b'[') {
        return;
    }
    // Rightmost `](` inside the token: the text half may contain brackets of its
    // own, the URL half may not contain the pair.
    let close = (start + 1..end.saturating_sub(1))
        .rev()
        .find(|&i| b[i] == b']' && b.get(i + 1) == Some(&b'('));
    let Some(close) = close else { return };
    if close == start + 1 {
        return; // `[](url)` — nothing to show, so show the source.
    }
    // The `!` of an image sits outside the token; taken with the `[` as one cut
    // so the list stays sorted.
    let from = if start > 0 && b[start - 1] == b'!' {
        start - 1
    } else {
        start
    };
    push_sorted(out, from..start + 1);
    push_sorted(out, close..end);
}

fn skip_space(b: &[u8], from: usize) -> usize {
    let mut i = from.min(b.len());
    while i < b.len() && b[i] == b' ' {
        i += 1;
    }
    i
}

/// Appends only if it lands after everything already there. Cuts arrive in order
/// because tokens do, so this is a guard against a malformed token list rather
/// than a sort — one bad range would otherwise corrupt every offset after it.
fn push_sorted(out: &mut Vec<Range<usize>>, r: Range<usize>) {
    if r.start < r.end && out.last().is_none_or(|last| last.end <= r.start) {
        out.push(r);
    }
}

// ------------------------------------------------------------------- applying

/// Removes `cuts` from the line and moves every token and span onto the result.
///
/// The ranges are moved *before* the text, because both have to be described in
/// the same coordinates while the mapping is computed. The text is rebuilt
/// rather than drained: it is an `Arc<str>` shared with the parsed diff and the
/// rows other presentations hold, so the kept bytes go into a fresh buffer and
/// the line takes it. One allocation per cut row, on `.md` files only.
fn apply(line: &mut Line, cuts: &[Range<usize>]) {
    if cuts.is_empty() {
        return;
    }
    // Bytes cut strictly before `p`, with a position inside a cut collapsing to
    // where that cut began — which is what makes a token that covered a marker
    // shrink to cover only what is left of it.
    let shift = |p: usize| -> usize {
        let mut gone = 0;
        for c in cuts {
            if c.end <= p {
                gone += c.end - c.start;
            } else if c.start < p {
                gone += p - c.start;
            } else {
                break;
            }
        }
        p - gone
    };

    line.tokens = line
        .tokens
        .iter()
        .map(|t| (shift(t.start as usize), shift(t.end as usize), t.kind))
        .filter(|(s, e, _)| s < e)
        .map(|(s, e, k)| Token {
            start: s as u32,
            end: e as u32,
            kind: k,
        })
        .collect::<Vec<_>>()
        .into_boxed_slice();
    line.spans = line
        .spans
        .iter()
        .map(|s| (shift(s.start as usize), shift(s.end as usize)))
        .filter(|(s, e)| s < e)
        .map(|(s, e)| Span {
            start: s as u32,
            end: e as u32,
        })
        .collect::<Vec<_>>()
        .into_boxed_slice();

    // Cuts are sorted and disjoint — `push_sorted` guarantees it — so one walk
    // copies everything between them.
    let mut out = String::with_capacity(line.text.len());
    let mut cursor = 0usize;
    for c in cuts {
        out.push_str(&line.text[cursor..c.start]);
        cursor = c.end;
    }
    out.push_str(&line.text[cursor..]);
    line.text = out.into();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prepared::prepare;
    use crate::syntax::Highlighters;
    use crate::{parse_unified_diff, Span};

    /// Runs the real pipeline — parse, prepare, lay out — over a diff body, so
    /// the tokens under test are the ones the app actually produces.
    fn run(body: &str) -> (Vec<Block>, Vec<Line>) {
        let raw = format!("diff --git a/d.md b/d.md\n@@ -1,1 +1,1 @@\n{body}");
        let hl = Highlighters::builtin();
        let mut p = prepare(&parse_unified_diff(&raw), &hl, 2000);
        let mut lines = std::mem::take(&mut p.files[0].hunks[0].lines);
        let blocks = lay_out(&mut lines, &Layout::monospaced());
        (blocks, lines)
    }

    /// The same, and the tables it measured — for the flow, which is what a
    /// window too narrow for a grid asks for.
    fn run_tables(body: &str) -> (Vec<Block>, Vec<Line>, Tables) {
        let raw = format!("diff --git a/d.md b/d.md\n@@ -1,1 +1,1 @@\n{body}");
        let hl = Highlighters::builtin();
        let mut p = prepare(&parse_unified_diff(&raw), &hl, 2000);
        let mut lines = std::mem::take(&mut p.files[0].hunks[0].lines);
        let (blocks, tables) = lay_out_tables(&mut lines, &Layout::monospaced());
        (blocks, lines, tables)
    }

    /// Flows the one table in `body` at `cols`, and returns the rows it drew:
    /// every sub-row of every row, in order.
    fn flowed(body: &str, cols: usize) -> Vec<String> {
        let (blocks, lines, tables) = run_tables(body);
        let run: Vec<TableRow> = (0..lines.len())
            .filter(|i| blocks[*i].is_table())
            .map(|i| TableRow {
                text: &lines[i].text,
                block: blocks[i],
                tokens: &lines[i].tokens,
                spans: &lines[i].spans,
            })
            .collect();
        let grid = &tables.grids[0];
        let Some(flow) = flow_table(&run, grid, cols, &Layout::monospaced(), &crate::wrap::Word)
        else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for f in &flow {
            let w = Wrapped::build_with(
                std::iter::once((f.text.as_str(), crate::wrap::Budget::At(&f.breaks))),
                &crate::wrap::Word,
            );
            for r in 0..w.rows(0) {
                out.push(f.text[w.range(0, r, &f.text)].to_string());
            }
        }
        out
    }

    /// One context line, which every side sees.
    /// One context line, which every side sees.
    fn one(text: &str) -> (Block, Line) {
        let (mut b, mut l) = run(&format!(" {text}\n"));
        (b.remove(0), l.remove(0))
    }

    fn text(t: &str) -> String {
        one(t).1.text.to_string()
    }

    #[test]
    fn every_range_still_indexes_its_line_after_the_markers_go() {
        // The invariant the renderer depends on, and the one this whole module is
        // at risk of breaking: cutting bytes out from under a token.
        let body = "\
 # Heading with **bold** and `code`
-- a bullet with [a link](https://example.com/very/long/path)
+- a bullet with [a link](https://example.com/other)
 > quoted **text**
 ```rust
 let x = **not_emphasis;
 ```
 | a | b |
 |---|---|
 1. ordered *item*
";
        let (blocks, lines) = run(body);
        assert_eq!(blocks.len(), lines.len());
        for l in &lines {
            for t in &l.tokens {
                assert!(
                    t.end as usize <= l.text.len(),
                    "token {t:?} outside {:?}",
                    l.text
                );
                assert!(
                    l.text.is_char_boundary(t.start as usize)
                        && l.text.is_char_boundary(t.end as usize),
                    "token {t:?} off a boundary in {:?}",
                    l.text
                );
                assert!(t.start < t.end, "empty token {t:?} in {:?}", l.text);
            }
            for s in &l.spans {
                assert!(
                    s.end as usize <= l.text.len(),
                    "span {s:?} outside {:?}",
                    l.text
                );
                assert!(s.start < s.end);
            }
        }
    }

    #[test]
    fn a_heading_loses_its_hashes_and_keeps_its_level() {
        assert_eq!(one("# Top").0, Block::Heading(1));
        assert_eq!(text("# Top"), "Top");
        assert_eq!(one("###### Deep").0, Block::Heading(6));
        assert_eq!(text("###### Deep"), "Deep");
        // Three columns of indent is still a heading; four is an indented code
        // block, and a `#` in one is a shell comment. This repository's own
        // `AGENTS.md` has a block of commands with trailing `# comments`, and
        // every one of them rendered as a full-width bold heading.
        assert_eq!(one("   # three spaces").0, Block::Heading(1));
        assert_ne!(one("    # four spaces").0, Block::Heading(1));
        assert_eq!(
            one("    ./dev.sh diff        # rebuild on every save").0,
            Block::Paragraph,
            "a command with a trailing comment is not a heading"
        );
        assert_ne!(one("\t# a tab is four columns").0, Block::Heading(1));

        // Closed form, and an indented one.
        assert_eq!(text("## Middle ##"), "Middle");
        assert_eq!(text("   ## Indented"), "Indented");
        // Seven is not a heading in any dialect.
        assert_eq!(one("####### Seven").0, Block::Paragraph);
        assert_eq!(text("####### Seven"), "####### Seven");
        // Nor is a hash with no space after it.
        assert_eq!(one("#hashtag").0, Block::Paragraph);
    }

    #[test]
    fn the_heading_token_shrinks_onto_the_text_it_still_covers() {
        // The highlighter puts one Heading token over the whole source line.
        // After the cut it has to cover the words and nothing else, or the row
        // draws its title in the body colour.
        let (block, line) = one("## A title");
        assert_eq!(block, Block::Heading(2));
        assert_eq!(line.text.as_ref(), "A title");
        let t = line
            .tokens
            .iter()
            .find(|t| t.kind == Kind::Heading)
            .expect("heading token");
        assert_eq!(&line.text[t.range()], "A title");
    }

    #[test]
    fn inline_emphasis_loses_its_delimiters_and_keeps_its_kind() {
        let (_, line) = one("a **strong** and an *emphatic* word");
        assert_eq!(line.text.as_ref(), "a strong and an emphatic word");
        let by = |k: Kind| {
            line.tokens
                .iter()
                .find(|t| t.kind == k)
                .map(|t| line.text[t.range()].to_string())
        };
        assert_eq!(by(Kind::Strong).as_deref(), Some("strong"));
        assert_eq!(by(Kind::Emphasis).as_deref(), Some("emphatic"));
    }

    #[test]
    fn a_code_span_loses_its_backticks_however_many_there_were() {
        let (_, line) = one("call `f()` please");
        assert_eq!(line.text.as_ref(), "call f() please");
        let t = line.tokens.iter().find(|t| t.kind == Kind::Str).unwrap();
        assert_eq!(&line.text[t.range()], "f()");
        // A doubled fence around a span containing a backtick.
        assert_eq!(text("use ``a `b` c`` here"), "use a `b` c here");
    }

    #[test]
    fn an_empty_delimiter_pair_is_left_alone() {
        // `****` has nothing between its markers; cutting both ends would leave
        // an empty token pointing at nothing.
        assert_eq!(text("**** and ``"), "**** and ``");
    }

    #[test]
    fn a_link_keeps_its_text_and_drops_its_url() {
        let (_, line) = one("see [the docs](https://example.com/a/b) for more");
        assert_eq!(line.text.as_ref(), "see the docs for more");
        let t = line.tokens.iter().find(|t| t.kind == Kind::Link).unwrap();
        assert_eq!(&line.text[t.range()], "the docs");
    }

    #[test]
    fn an_image_loses_its_bang_as_well_as_its_brackets() {
        assert_eq!(text("![a diagram](x.png) follows"), "a diagram follows");
    }

    #[test]
    fn a_link_with_no_text_is_left_as_source() {
        // Nothing to show, so showing the source beats showing a blank row.
        assert_eq!(text("[](https://example.com)"), "[](https://example.com)");
    }

    #[test]
    fn a_bullet_loses_its_marker_and_reports_its_depth() {
        assert_eq!(one("- top").0, Block::Bullet(0));
        assert_eq!(text("- top"), "top");
        assert_eq!(one("  - nested").0, Block::Bullet(1));
        assert_eq!(one("    - deeper").0, Block::Bullet(2));
        assert_eq!(one("* star").0, Block::Bullet(0));
        assert_eq!(one("+ plus").0, Block::Bullet(0));
        // A tab is four columns, so this is one step in.
        assert_eq!(one("\t- tabbed").0, Block::Bullet(2));
    }

    #[test]
    fn an_ordered_item_keeps_its_number() {
        assert_eq!(one("3. third").0, Block::Ordered(0));
        assert_eq!(text("3. third"), "3. third");
        assert_eq!(one("12) twelfth").0, Block::Ordered(0));
    }

    #[test]
    fn a_quote_loses_its_angle_brackets_and_counts_them() {
        assert_eq!(one("> said").0, Block::Quote(1));
        assert_eq!(text("> said"), "said");
        assert_eq!(one("> > deeper").0, Block::Quote(2));
        assert_eq!(text("> > deeper"), "deeper");
    }

    #[test]
    fn a_fence_keeps_only_its_language_and_its_body_stays_verbatim() {
        // The leading space of each body line is the diff's own context marker;
        // what follows it is the file. The indent inside the block is part of the
        // code and has to survive, which is why nothing is cut from a Code row.
        let (blocks, lines) = run(" ```rust\n     let x = *p;\n ```\n text\n");
        assert_eq!(blocks[0], Block::Fence);
        assert_eq!(lines[0].text.as_ref(), "rust");
        assert_eq!(blocks[1], Block::Code);
        // Inside a fence nothing is markup: the text is untouched, indent included.
        assert_eq!(lines[1].text.as_ref(), "    let x = *p;");
        assert_eq!(blocks[2], Block::Fence);
        assert_eq!(lines[2].text.as_ref(), "");
        assert_eq!(blocks[3], Block::Paragraph);
    }

    #[test]
    fn an_unlabelled_fence_leaves_an_empty_row_rather_than_backticks() {
        let (blocks, lines) = run(" ```\n body\n ```\n");
        assert_eq!(blocks[0], Block::Fence);
        assert_eq!(lines[0].text.as_ref(), "");
        assert_eq!(blocks[1], Block::Code);
    }

    #[test]
    fn a_hash_inside_a_fence_is_not_a_heading() {
        let (blocks, _) = run(" ```sh\n # not a heading\n - not a bullet\n ```\n");
        assert_eq!(blocks[1], Block::Code);
        assert_eq!(blocks[2], Block::Code);
    }

    #[test]
    fn a_fence_opened_on_one_side_does_not_close_on_the_other() {
        // The reason this runs per side. The removed ``` must not pair with the
        // added one; each side opens and closes its own block.
        let (blocks, _) = run("-```rust\n-let a = 1;\n-```\n+```rust\n+let b = 2;\n+```\n");
        assert_eq!(
            blocks,
            vec![
                Block::Fence,
                Block::Code,
                Block::Fence,
                Block::Fence,
                Block::Code,
                Block::Fence
            ]
        );
    }

    #[test]
    fn an_unclosed_fence_does_not_leak_past_its_hunk() {
        // Every hunk is laid out on its own, so a block left open at the end of
        // one cannot swallow the next. Half-open fences are the norm in diffs.
        let raw = "\
diff --git a/d.md b/d.md
@@ -1,2 +1,2 @@
 ```rust
 let x = 1;
@@ -9,2 +9,2 @@
 # A heading
 body
";
        let hl = Highlighters::builtin();
        let mut p = prepare(&parse_unified_diff(raw), &hl, 2000);
        let f = &mut p.files[0];
        let first = lay_out(&mut f.hunks[0].lines, &Layout::monospaced());
        let second = lay_out(&mut f.hunks[1].lines, &Layout::monospaced());
        assert_eq!(first, vec![Block::Fence, Block::Code]);
        assert_eq!(second[0], Block::Heading(1), "a fence leaked across hunks");
    }

    #[test]
    fn a_setext_underline_promotes_the_line_above_it() {
        let (blocks, lines) = run(" Title\n =====\n body\n");
        assert_eq!(blocks[0], Block::Heading(1));
        assert_eq!(lines[0].text.as_ref(), "Title");
        assert_eq!(blocks[1], Block::Blank, "the underline draws as nothing");
        assert_eq!(blocks[2], Block::Paragraph);

        let (blocks, _) = run(" Sub\n ---\n");
        assert_eq!(blocks[0], Block::Heading(2));
    }

    #[test]
    fn a_rule_with_nothing_above_it_stays_a_rule() {
        let (blocks, _) = run(" para\n \n ---\n after\n");
        assert_eq!(
            blocks[2],
            Block::Rule,
            "a break after a blank is not a setext"
        );
        let (blocks, _) = run(" ***\n");
        assert_eq!(blocks[0], Block::Rule);
    }

    #[test]
    fn a_table_row_and_its_separator_are_told_apart() {
        let (blocks, _) = run(" | a | b |\n |---|:-:|\n | 1 | 2 |\n");
        assert_eq!(blocks[0], Block::Table);
        assert_eq!(blocks[1], Block::TableRule);
        assert_eq!(blocks[2], Block::Table);
    }

    #[test]
    fn an_empty_header_row_is_a_row_and_not_a_break() {
        // From `docs/README.md` in this repository, which opens a table with one.
        // A separator draws a line; a row of empty cells does not, and a thematic
        // break is neither.
        let (blocks, _) = run(" | | |\n |---|---|\n | a | b |\n");
        assert_eq!(
            blocks[0],
            Block::Table,
            "an empty header row drew as a break"
        );
        assert_eq!(blocks[1], Block::TableRule);
        assert_eq!(blocks[2], Block::Table);
    }

    #[test]
    fn a_table_is_padded_to_its_widest_cell_in_each_column() {
        let (_, lines) = run(" | stage | time |\n |---|---|\n | assign lanes | 301 ms |\n");
        assert_eq!(lines[0].text.as_ref(), "│ stage        │ time   │");
        assert_eq!(lines[1].text.as_ref(), "├──────────────┼────────┤");
        assert_eq!(lines[2].text.as_ref(), "│ assign lanes │ 301 ms │");
        // Every row of a grid is the same width, which is the whole point.
        let w: Vec<usize> = lines.iter().map(|l| l.text.chars().count()).collect();
        assert!(w.windows(2).all(|p| p[0] == p[1]), "ragged: {w:?}");
    }

    #[test]
    fn the_separator_says_how_its_columns_align() {
        let (_, lines) =
            run(" | l | c | r |\n |:--|:-:|--:|\n | 1 | 2 | 3 |\n | xxxx | yyyy | zzzz |\n");
        assert_eq!(lines[2].text.as_ref(), "│ 1    │  2   │    3 │");
    }

    #[test]
    fn a_short_row_still_gets_the_missing_columns() {
        // Ragged tables are normal by hand, and a row that stopped early would
        // break the grid under it.
        let (_, lines) = run(" | a | b | c |\n | d |\n");
        assert_eq!(lines[0].text.as_ref(), "│ a │ b │ c │");
        assert_eq!(lines[1].text.as_ref(), "│ d │   │   │");
    }

    #[test]
    fn two_tables_separated_by_prose_get_their_own_columns() {
        // A run, not the hunk: the second table's long cell must not widen the
        // first table's column.
        let (_, lines) = run(" | a |\n\x20\n text\n\x20\n | wiiiiiide |\n");
        assert_eq!(lines[0].text.as_ref(), "│ a │");
        assert_eq!(lines[4].text.as_ref(), "│ wiiiiiide │");
    }

    #[test]
    fn the_two_sides_of_a_table_are_measured_separately() {
        // Otherwise a long cell on the added side pads out the removed side, and
        // the removed rows stop lining up with anything.
        let (_, lines) = run("-| a | b |\n+| a | bbbbbbbbbb |\n");
        let removed = lines.iter().find(|l| l.kind == LineKind::Removed).unwrap();
        let added = lines.iter().find(|l| l.kind == LineKind::Added).unwrap();
        assert_eq!(removed.text.as_ref(), "│ a │ b │");
        assert_eq!(added.text.as_ref(), "│ a │ bbbbbbbbbb │");
    }

    #[test]
    fn a_cell_keeps_its_markup_and_its_token_after_alignment() {
        // The hard part: the cell was trimmed of backticks by the cut pass and
        // then moved by the padding, so its token has been through both.
        let (_, lines) = run(" | `code` | x |\n | aaaaaaaa | y |\n");
        assert_eq!(lines[0].text.as_ref(), "│ code     │ x │");
        let t = lines[0]
            .tokens
            .iter()
            .find(|t| t.kind == Kind::Str)
            .expect("a code span");
        assert_eq!(&lines[0].text[t.range()], "code");
    }

    #[test]
    fn an_escaped_pipe_is_not_a_column_boundary() {
        let (_, lines) = run(r" | a \| b | c |");
        assert_eq!(lines[0].text.as_ref(), r"│ a \| b │ c │");
    }

    #[test]
    fn a_proportional_frontend_gets_its_table_untouched() {
        // Padding with spaces only aligns in a monospaced face, so the honest
        // answer for anything else is to leave the row alone.
        let raw = "diff --git a/d.md b/d.md\n@@ -1,1 +1,1 @@\n | a | bbbb |\n | cccc | d |\n";
        let hl = Highlighters::builtin();
        let mut p = prepare(&parse_unified_diff(raw), &hl, 2000);
        let mut lines = std::mem::take(&mut p.files[0].hunks[0].lines);
        let blocks = lay_out(&mut lines, &Layout::proportional());
        assert_eq!(blocks[0], Block::Table);
        assert_eq!(
            lines[0].text.as_ref(),
            "| a | bbbb |",
            "a table was padded anyway"
        );
    }

    #[test]
    fn a_table_cut_in_half_by_a_hunk_aligns_to_what_is_on_screen() {
        // A hunk shows three rows out of twenty and has no header and no
        // separator. Aligning to what is present beats refusing to align.
        let (blocks, lines) = run(" | mid | row |\n | another | one |\n");
        assert!(blocks.iter().all(|b| *b == Block::Table));
        assert_eq!(lines[0].text.as_ref(), "│ mid     │ row │");
        assert_eq!(lines[1].text.as_ref(), "│ another │ one │");
    }

    #[test]
    fn a_context_row_in_a_table_is_aligned_exactly_once() {
        // The regression. A context row belongs to both sides, so `for_each_side`
        // hands it to the caller twice; padding it twice widened it by a whole
        // grid and then panicked splitting the `│` it had just written.
        let (blocks, lines) = run(" | keep | this |\n-| old | x |\n+| new | y |\n");
        assert!(blocks.iter().all(|b| *b == Block::Table));
        let context = lines.iter().find(|l| l.kind == LineKind::Context).unwrap();
        assert_eq!(context.text.as_ref(), "│ keep │ this │");
        // Every row of the grid the context row belongs to is the same width.
        let w: Vec<usize> = lines.iter().map(|l| l.text.chars().count()).collect();
        assert!(
            w.windows(2).all(|p| p[0] == p[1]),
            "ragged: {w:?} in {lines:#?}"
        );
    }

    #[test]
    fn a_context_row_takes_the_added_sides_grid() {
        // It can only have one, and the added side is the one the reader is
        // looking at — the same rule the token pass uses for context lines.
        let (_, lines) = run(" | a | b |\n-| c | d |\n+| e | ffffffffff |\n");
        let context = lines.iter().find(|l| l.kind == LineKind::Context).unwrap();
        let added = lines.iter().find(|l| l.kind == LineKind::Added).unwrap();
        assert_eq!(context.text.chars().count(), added.text.chars().count());
    }

    #[test]
    fn glyphs_are_configurable_for_a_font_without_box_drawing() {
        let raw = "diff --git a/d.md b/d.md\n@@ -1,1 +1,1 @@\n | a | b |\n |---|---|\n";
        let hl = Highlighters::builtin();
        let mut p = prepare(&parse_unified_diff(raw), &hl, 2000);
        let mut lines = std::mem::take(&mut p.files[0].hunks[0].lines);
        let ascii = Layout {
            monospaced: true,
            table: TableGlyphs {
                vertical: "|",
                horizontal: "-",
                cross: "+",
                end: ("+", "+"),
            },
        };
        lay_out(&mut lines, &ascii);
        assert_eq!(lines[0].text.as_ref(), "| a | b |");
        assert_eq!(lines[1].text.as_ref(), "+---+---+");
    }

    #[test]
    fn a_blank_line_is_a_blank_block() {
        let (blocks, _) = run(" text\n \n more\n");
        assert_eq!(blocks[1], Block::Blank);
    }

    #[test]
    fn an_intraline_span_moves_with_the_text_it_marked() {
        // A word-level span is computed on the source, so a heading's span sits
        // two bytes right of where the rendered row wants it. It has to move
        // with everything else or the wrong word lights up.
        let (_, lines) = run("-## alpha beta\n+## alpha gamma\n");
        let added = lines.iter().find(|l| l.kind == LineKind::Added).unwrap();
        assert_eq!(added.text.as_ref(), "alpha gamma");
        assert!(!added.spans.is_empty(), "expected a changed word");
        let marked: Vec<&str> = added
            .spans
            .iter()
            .map(|s| &added.text[s.start as usize..s.end as usize])
            .collect();
        assert!(
            marked.iter().any(|m| m.contains("gamma")),
            "spans point at {marked:?}"
        );
    }

    #[test]
    fn a_span_over_a_marker_that_is_cut_disappears_rather_than_dangling() {
        // If the only thing that changed is punctuation the renderer hides, there
        // is nothing left to light up — and a span of zero width would draw as a
        // stray block.
        let (_, lines) = run("-# title\n+## title\n");
        for l in &lines {
            assert!(l
                .spans
                .iter()
                .all(|s| s.start < s.end && s.end as usize <= l.text.len()));
        }
    }

    #[test]
    fn multi_byte_text_survives_the_cut() {
        let (_, line) = one("**Café** naïve 😀 [ok](u)");
        assert_eq!(line.text.as_ref(), "Café naïve 😀 ok");
        for t in &line.tokens {
            assert!(
                line.text.is_char_boundary(t.start as usize)
                    && line.text.is_char_boundary(t.end as usize)
            );
        }
    }

    #[test]
    fn markup_inside_a_heading_keeps_its_markers() {
        // Not an oversight — a limitation inherited from the token pass, pinned
        // here so it is a known quantity rather than a surprise. The Markdown
        // highlighter marks a heading as one whole-line `Heading` token and never
        // scans inside it, so the delimiters within are not located and there is
        // nothing to cut. Locating them would mean splitting that token around
        // them, and tokens must stay non-overlapping.
        //
        // Measured on both markdown fixtures: 21.3% of a programming book's
        // headings carry inline markup and 3.4% of a technical-docs tree's, and
        // because their heading counts are inverse both come to under half a
        // percent of changed rows. Cheap to leave; see the decision record.
        let (block, line) = one("## A `code` heading");
        assert_eq!(block, Block::Heading(2));
        assert_eq!(
            line.text.as_ref(),
            "A `code` heading",
            "the hashes still go"
        );
    }

    #[test]
    fn nothing_to_cut_means_nothing_is_touched() {
        let before = "just some prose with no markup at all";
        let (block, line) = one(before);
        assert_eq!(block, Block::Paragraph);
        assert_eq!(line.text.as_ref(), before);
    }

    #[test]
    fn depth_and_code_answer_for_every_block() {
        // Both are read on the render path for every visible row; neither may
        // panic on a variant added later.
        for b in [
            Block::Paragraph,
            Block::Blank,
            Block::Heading(3),
            Block::Bullet(2),
            Block::Ordered(1),
            Block::Quote(2),
            Block::Fence,
            Block::Code,
            Block::Table,
            Block::TableRule,
            Block::Rule,
        ] {
            let _ = b.depth();
            let _ = b.is_code();
        }
        assert_eq!(Block::Bullet(2).depth(), 2);
        assert_eq!(Block::Quote(2).depth(), 1);
        assert_eq!(Block::Heading(1).depth(), 0);
        assert!(Block::Code.is_code() && Block::Fence.is_code());
        assert!(!Block::Paragraph.is_code());
    }

    #[test]
    fn a_token_list_from_a_different_highlighter_is_not_corrupted() {
        // The cuts are derived from the bytes, not from which highlighter ran.
        // A Strong token that is not wearing asterisks keeps every byte it has.
        let mut line = Line {
            kind: LineKind::Context,
            moved: false,
            old_no: Some(1),
            new_no: Some(1),
            text: "plain strong words".into(),
            spans: Box::new([Span { start: 6, end: 12 }]),
            tokens: Box::new([Token {
                start: 6,
                end: 12,
                kind: Kind::Strong,
            }]),
        };
        let mut cuts = Vec::new();
        mark(&line, Block::Paragraph, &mut cuts);
        assert!(
            cuts.is_empty(),
            "cut {cuts:?} out of a token with no delimiters"
        );
        apply(&mut line, &cuts);
        assert_eq!(line.text.as_ref(), "plain strong words");
        assert_eq!(&line.text[line.tokens[0].range()], "strong");
    }

    // ------------------------------------------- fitting a grid to a window

    /// Two columns, one of them a paragraph. 61 characters of grid.
    const WIDE: &str = "\
 | Resource | Why |
 |---|---|
 | ECR repository | push target for the build, and a rollback tag |
 | Security group | ingress 3000 from the ALB only |
";

    #[test]
    fn a_grid_wider_than_the_window_is_squeezed_to_fit_it() {
        // The bug this exists for: a table is the widest row in the diff, so
        // `with_width_from_item` picks it and the *whole* view scrolls sideways —
        // 852 rows of prose dragged along by one row of a table.
        let (_, _, tables) = run_tables(WIDE);
        assert!(
            tables.grids[0].width() > 44,
            "the fixture stopped being too wide"
        );
        let rows = flowed(WIDE, 44);
        assert!(!rows.is_empty(), "nothing was flowed");
        for row in &rows {
            assert_eq!(
                row.chars().count(),
                44,
                "a row of the grid is not the budget: {row:?}"
            );
        }
    }

    #[test]
    fn every_row_of_a_squeezed_table_puts_its_pipes_in_the_same_place() {
        // The whole point of keeping the grid: a column that lines up with the
        // rows above and below it. A sub-row that put its own pipes wherever its
        // text ended would be a wrapped line, not a table.
        let rows = flowed(WIDE, 44);
        let pipes = |row: &String| -> Vec<usize> {
            row.chars()
                .enumerate()
                .filter(|(_, c)| "│├┤┼".contains(*c))
                .map(|(i, _)| i)
                .collect()
        };
        let first = pipes(&rows[0]);
        assert_eq!(
            first.len(),
            3,
            "two columns is three boundaries: {:?}",
            rows[0]
        );
        for row in &rows {
            assert_eq!(pipes(row), first, "{row:?} sheared the grid");
        }
    }

    #[test]
    fn a_squeezed_cell_wraps_rather_than_losing_its_text() {
        // More rows, not a truncated one: everything the cell said is still on
        // the screen, which is the difference between this and an ellipsis.
        let rows = flowed(WIDE, 44);
        let drawn: String = rows.join(" ");
        for word in "push target for the build, and a rollback tag".split(' ') {
            assert!(drawn.contains(word), "{word:?} was dropped: {drawn}");
        }
        // And it needed the rows to say it: a 44-column grid cannot hold that
        // cell in one.
        assert!(rows.len() > 4, "four rows means nothing wrapped: {rows:?}");
    }

    #[test]
    fn the_narrow_column_keeps_its_width_and_the_wide_one_gives() {
        // Water-filling, not proportional. `Resource` wants 14 and gets it; the
        // paragraph beside it gives up what has to be given up.
        let widths = squeeze(&[14, 44], 44).expect("a two-column table fits in 44");
        assert_eq!(
            widths[0], 14,
            "the narrow column was shrunk too: {widths:?}"
        );
        assert_eq!(
            widths.iter().sum::<usize>(),
            44 - 7,
            "the budget was not spent"
        );
    }

    #[test]
    fn a_column_that_fits_is_never_widened_to_fill_the_row() {
        // The other direction of the same rule: squeezing is not stretching, and
        // a table narrower than the window keeps the shape it was written in.
        assert_eq!(squeeze(&[3, 4], 40), Some(vec![3, 4]));
    }

    #[test]
    fn a_grid_that_fits_is_left_exactly_as_it_was() {
        // Nothing to do, and saying so is what keeps a table one row: the flow is
        // asked on every resize and most tables fit.
        assert!(
            flowed(WIDE, 200).is_empty(),
            "a table that fits was rebuilt"
        );
    }

    #[test]
    fn wrapping_off_scrolls_rather_than_squeezing() {
        // `off` is the reader saying they would rather scroll. A cell cannot be
        // broken without breaking it, so there is nothing honest to do here.
        let (blocks, lines, tables) = run_tables(WIDE);
        let run: Vec<TableRow> = (0..lines.len())
            .filter(|i| blocks[*i].is_table())
            .map(|i| TableRow {
                text: &lines[i].text,
                block: blocks[i],
                tokens: &lines[i].tokens,
                spans: &lines[i].spans,
            })
            .collect();
        let off = flow_table(
            &run,
            &tables.grids[0],
            20,
            &Layout::monospaced(),
            &crate::wrap::Off,
        );
        assert!(off.is_none(), "wrapping off squeezed a table anyway");
    }

    #[test]
    fn a_budget_with_no_room_for_a_character_a_column_gives_up() {
        // Three columns cost ten characters of pipes and padding before a letter
        // is drawn. A grid of nothing but grid is worse than one you scroll.
        assert_eq!(squeeze(&[8, 8, 8], 12), None);
        assert_eq!(squeeze(&[8, 8, 8], 8), None);
        // And one character a column is where it starts saying yes.
        assert_eq!(squeeze(&[8, 8, 8], 13), Some(vec![1, 1, 1]));
    }

    #[test]
    fn the_separator_is_drawn_again_at_the_widths_that_were_settled_on() {
        // It is the one row whose text is generated rather than trimmed, at load
        // and here: a rule sized to a column it no longer has is the grid
        // visibly not lining up.
        let rows = flowed(WIDE, 44);
        let rule = rows
            .iter()
            .find(|r| r.starts_with('├'))
            .expect("a separator row");
        assert_eq!(rule.chars().count(), 44);
        assert!(rule.chars().all(|c| "├─┼┤".contains(c)), "{rule:?}");
    }

    #[test]
    fn a_token_that_wrapped_is_two_tokens_and_neither_covers_the_gap() {
        // A `**phrase**` broken across two sub-rows is two ranges. One range
        // spanning them would paint everything in between — the pipes, the
        // padding and the whole of the other column.
        let body = " | k | a very **emphatic phrase** here |\n |---|---|\n";
        let (blocks, lines, tables) = run_tables(body);
        let run: Vec<TableRow> = (0..lines.len())
            .filter(|i| blocks[*i].is_table())
            .map(|i| TableRow {
                text: &lines[i].text,
                block: blocks[i],
                tokens: &lines[i].tokens,
                spans: &lines[i].spans,
            })
            .collect();
        let flow = flow_table(
            &run,
            &tables.grids[0],
            22,
            &Layout::monospaced(),
            &crate::wrap::Word,
        )
        .expect("22 columns is too narrow for that cell");
        let row = &flow[0];
        let strong: Vec<&str> = row
            .tokens
            .iter()
            .filter(|t| t.kind == Kind::Strong)
            .map(|t| &row.text[t.start as usize..t.end as usize])
            .collect();
        assert_eq!(strong, vec!["emphatic", "phrase"], "in {:?}", row.text);
    }

    #[test]
    fn a_row_with_fewer_cells_than_the_table_still_draws_the_grid() {
        // Ragged tables are normal by hand, and a short row that stopped early
        // would break the grid under it — the same rule `grid_row` follows.
        let body = " | a | b | c |\n |---|---|---|\n | only one cell that is quite long |\n";
        let rows = flowed(body, 26);
        assert!(!rows.is_empty(), "nothing was flowed");
        for row in &rows {
            assert_eq!(row.chars().count(), 26, "{row:?}");
            // Three columns is four boundaries, on the short row too.
            assert_eq!(
                row.chars().filter(|c| "│├┤┼".contains(*c)).count(),
                4,
                "{row:?}"
            );
        }
    }
}
