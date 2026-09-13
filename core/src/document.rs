//! A whole markdown file, laid out as a document rather than as a diff side.
//!
//! [`crate::markdown`] answers "what is this hunk" — a block per line of a diff,
//! with the markers off and the tables aligned — and that is the right answer
//! for a diff pane and the wrong one for a reader. Two things a hunk pass cannot
//! do, and both are the reason this exists:
//!
//! **A fence has no end.** The block pass runs per hunk *side*, because a fence
//! opened above a hunk and closed below it has everything between them missing,
//! and classifying the interleaved rows as one document would have a removed
//! ``` closing an added one. A whole file has no such seam: every line belongs
//! to the same document, the fence opens where it opens and closes where it
//! closes, and a hundred-line code block is one block.
//!
//! **A table is measured against the whole table.** Column widths come from a
//! run of rows; a hunk that shows four rows of a twenty-row table measures the
//! four, so the same table is a different width depending on what changed near
//! it. Here the run is the table.
//!
//! What it is *not* is a second markdown parser. The classification, the marker
//! removal, the byte-range remapping and the grid measurement are
//! [`crate::markdown`]'s, reached through the same entry points a diff side
//! uses — because two implementations of "is this line a heading" drift, and
//! the one that drifts is the one nobody is looking at.
//!
//! # What comes out
//!
//! A [`Document`], which is data and knows nothing about a window: a [`Block`]
//! per row, the row's text with its markers gone and its tokens moved to match,
//! and the tables that were measured. A frontend turns that into elements — the
//! window does, in `gui/src/views/document.rs` — and is free to lay it out at
//! any height it likes, which is the whole point: a document pane is not a list
//! of fixed-height rows.

use crate::markdown::{self, Block, Grid, Layout};
use crate::prepared::Line;
use crate::syntax::{Highlighters, Token};
use crate::LineKind;
use std::ops::Range;
use std::sync::Arc;

/// One measured table: its rows, the grid they were measured against, and each
/// row's cells as ranges into that row's own text.
///
/// The cells are ranges and not padded text, which is the difference between
/// this and a diff's grid. A diff aligns a table by inserting spaces, because a
/// row there is one `StyledText` on a monospaced line and padding is the only
/// honest way to line cells up. A document pane draws cells as elements, so
/// what it needs is where each cell's content is *within the row* and how wide
/// each column wants to be — characters, which the pane turns into pixels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    /// The rows of the file this table occupies, header and separator included.
    pub rows: Range<usize>,
    /// What the columns were measured at, and which way they align.
    pub grid: Grid,
    /// One entry per row in [`Table::rows`], in order: that row's cells.
    pub cells: Box<[Box<[Range<usize>]>]>,
}

/// A file laid out as a document. Borrow-free and cheap to hand to a frontend:
/// three boxed slices, one allocation each.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    /// One per row, in file order. Same length as [`Document::lines`].
    blocks: Box<[Block]>,
    /// The rows: text with the markers removed, tokens and spans moved onto it.
    lines: Box<[Line]>,
    /// Every table in the file, in the order they were met.
    tables: Box<[Table]>,
}

impl Document {
    /// Lays one file out. `path` decides the highlighter, exactly as it does for
    /// a diff side: a `.md` file gets [`crate::syntax::Markdown`] and a `.mdx`
    /// one would get whatever that route names.
    ///
    /// The layout is [`Layout::proportional`] and that is not an oversight: a
    /// table's cells here are elements with the width the window gives them, so
    /// padding them into a character grid would be work thrown away, and worse,
    /// it would rewrite a row's text — which is what a cell range indexes.
    pub fn new(text: &str, path: &str, syntax: &Highlighters) -> Self {
        let (blocks, lines) = lay_out_file(text, path, syntax, &Layout::proportional());
        let tables = measure_tables(&lines, &blocks);
        Self {
            blocks: blocks.into(),
            lines: lines.into(),
            tables: tables.into(),
        }
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// What the row structurally is. Never `None` for an in-range row — a file
    /// has no headers, which is what makes this different from the row store.
    pub fn block(&self, row: usize) -> Option<Block> {
        self.blocks.get(row).copied()
    }

    /// The row's text, markers gone, exactly as a row presentation would draw it.
    pub fn text(&self, row: usize) -> Option<&str> {
        self.lines.get(row).map(|l| l.text.as_ref())
    }

    pub fn line(&self, row: usize) -> Option<&Line> {
        self.lines.get(row)
    }

    /// A row's tokens, in the coordinates of [`Document::text`].
    pub fn tokens(&self, row: usize) -> &[Token] {
        self.lines.get(row).map_or(&[], |l| &l.tokens)
    }

    /// Every table, in file order.
    pub fn tables(&self) -> &[Table] {
        &self.tables
    }

    /// The table a row belongs to, if it is in one.
    ///
    /// Binary search and not a field per row: a table is a small fraction of a
    /// document's rows, and a field on every row to answer "no" costs more than
    /// the search costs on the rows that do.
    pub fn table_at(&self, row: usize) -> Option<&Table> {
        let i = self
            .tables
            .binary_search_by(|t| {
                if row < t.rows.start {
                    std::cmp::Ordering::Greater
                } else if row >= t.rows.end {
                    std::cmp::Ordering::Less
                } else {
                    std::cmp::Ordering::Equal
                }
            })
            .ok()?;
        self.tables.get(i)
    }

    /// How many rows were laid out, for a load report. The sentence a shell
    /// prints is its own; this is the number.
    pub fn rows(&self) -> usize {
        self.lines.len()
    }

    /// How many tables were measured. Zero for most documents, and the reason
    /// the report says nothing about tables when there are none.
    pub fn measured(&self) -> usize {
        self.tables.len()
    }
}

/// One file's lines through the diff side's own passes.
///
/// Every line is [`LineKind::Added`], which is the truthful answer — the file
/// is all new content to a viewer that has no old side — and it is what makes
/// the block pass treat the whole file as one document: `for_each_side` splits
/// by kind, and one kind is one run.
///
/// The tokens come from the highlighter the path routes to, once for the whole
/// file: the markdown highlighter is a line walker and takes a slice of lines,
/// so a fence's state crosses rows the way it does in the editor it is imitating.
/// Tokens that do not index their line are dropped rather than trusted — the
/// `Line` boundary is a contract every renderer depends on, and a highlighter
/// that breaks it should cost a colour, not a panic in GPUI's text layout.
pub fn lay_out_file(
    text: &str,
    path: &str,
    syntax: &Highlighters,
    layout: &Layout,
) -> (Vec<Block>, Vec<Line>) {
    let texts: Vec<&str> = text.lines().collect();
    let tokens = syntax.for_path(path).highlight(path, &texts);
    let mut lines: Vec<Line> = texts
        .iter()
        .enumerate()
        .map(|(i, text)| Line {
            kind: LineKind::Added,
            moved: false,
            // A document has no line numbers: there is no hunk header to count
            // from and no second side to disagree with. `None` on both, and the
            // frontend draws no gutter — which is what makes this a document
            // and not a diff with the marks taken off.
            old_no: None,
            new_no: None,
            text: Arc::from(*text),
            spans: Box::default(),
            tokens: tokens
                .get(i)
                .map(|tokens| {
                    tokens
                        .iter()
                        .filter(|t| t.start < t.end && (t.end as usize) <= text.len())
                        .filter(|t| {
                            text.is_char_boundary(t.start as usize)
                                && text.is_char_boundary(t.end as usize)
                        })
                        .copied()
                        .collect::<Box<[Token]>>()
                })
                .unwrap_or_default(),
        })
        .collect();
    let blocks = markdown::lay_out(&mut lines, layout);
    (blocks, lines)
}

/// Every run of table rows in the file, measured against its own rows.
///
/// Runs, not the whole file: two tables separated by a paragraph are two grids
/// with two sets of widths, and measuring them together is how one table's long
/// cell widens the other's columns. This is the same rule the diff pass follows
/// per hunk side, for the same reason.
fn measure_tables(lines: &[Line], blocks: &[Block]) -> Vec<Table> {
    let mut out = Vec::new();
    let mut cells: Vec<Range<usize>> = Vec::new();
    let mut row = 0;
    while row < blocks.len() {
        if !blocks[row].is_table() {
            row += 1;
            continue;
        }
        let start = row;
        while row < blocks.len() && blocks[row].is_table() {
            row += 1;
        }
        let run: Vec<usize> = (start..row).collect();
        let Some(grid) = markdown::measure(lines, blocks, &run) else {
            continue;
        };
        // Each row's cells are split once and kept: the pane draws them as
        // elements on every frame, and re-scanning a row for its pipes per
        // frame is exactly the kind of thing rule 3 is about.
        let mut rows = Vec::with_capacity(run.len());
        for &r in &run {
            markdown::split_cells(&lines[r].text, &mut cells);
            rows.push(cells.clone().into_boxed_slice());
        }
        out.push(Table {
            rows: start..row,
            grid,
            cells: rows.into_boxed_slice(),
        });
    }
    out
}

/// The document's lines with the diff's removals left in place, and what each
/// line is.
///
/// The pane draws the file as it is now, which is a document and not a diff —
/// right for reading, and half an answer for somebody looking at a change. This
/// is the other half: the same whole-file text, with every line the change took
/// away spliced back where it was and both sides marked as context, added or
/// removed. The caller lays *this* out, so a fence that gained a line is still
/// one fence, a table that lost a row is still one table, and the reader sees
/// the difference inside the document rather than in a second view.
///
/// Removals come from `old` — the other side of the same source, read whole —
/// and never from the diff's own rows: a row of a `.md` diff has had its
/// markdown markers taken off by the presentation, so splicing one back in
/// would leave a table row with no pipes that shears the grid it lands in, or a
/// heading with no `#` that reads as prose. The diff says *which* lines went
/// (`old_no`) and *where* they were (`new_no` of the line that followed them);
/// the text comes from the file.
///
/// A removal the diff has no line after — a change at the end of the file —
/// belongs after the last line, and lands there.
pub fn redline(
    new: &[Arc<str>],
    old: &[Arc<str>],
    changed: &[Line],
) -> (Vec<Arc<str>>, Vec<LineKind>) {
    use std::collections::{BTreeMap, HashSet};

    // Before which new line each removal goes, and which new lines are additions.
    let mut removed_at: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    let mut added: HashSet<usize> = HashSet::new();
    let mut pending: Vec<usize> = Vec::new();
    for line in changed {
        match line.kind {
            LineKind::Removed => {
                if let Some(old_no) = line.old_no {
                    pending.push(old_no as usize);
                }
            }
            LineKind::Added | LineKind::Context => {
                if let Some(new_no) = line.new_no {
                    let new_no = new_no as usize;
                    if !pending.is_empty() {
                        removed_at.insert(new_no, std::mem::take(&mut pending));
                    }
                    if line.kind == LineKind::Added {
                        added.insert(new_no);
                    }
                }
            }
        }
    }

    let mut out: Vec<Arc<str>> = Vec::with_capacity(new.len() + pending.len());
    let mut kinds: Vec<LineKind> = Vec::with_capacity(out.capacity());
    let push = |out: &mut Vec<Arc<str>>, kinds: &mut Vec<LineKind>, text: &Arc<str>, kind| {
        out.push(text.clone());
        kinds.push(kind);
    };
    for (i, text) in new.iter().enumerate() {
        let new_no = i + 1;
        for old_no in removed_at.get(&new_no).map_or(&[][..], |v| v.as_slice()) {
            if let Some(text) = old_no.checked_sub(1).and_then(|k| old.get(k)) {
                push(&mut out, &mut kinds, text, LineKind::Removed);
            }
        }
        let kind = if added.contains(&new_no) {
            LineKind::Added
        } else {
            LineKind::Context
        };
        push(&mut out, &mut kinds, text, kind);
    }
    for old_no in pending {
        if let Some(text) = old_no.checked_sub(1).and_then(|k| old.get(k)) {
            push(&mut out, &mut kinds, text, LineKind::Removed);
        }
    }
    (out, kinds)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One row of a diff, as a presentation builds them.
    fn mk(kind: LineKind, old_no: Option<u32>, new_no: Option<u32>, text: &str) -> Line {
        Line {
            kind,
            moved: false,
            old_no,
            new_no,
            text: Arc::from(text),
            spans: Vec::new().into_boxed_slice(),
            tokens: Vec::new().into_boxed_slice(),
        }
    }

    fn file(text: &str) -> Vec<Arc<str>> {
        text.lines().map(Arc::from).collect()
    }

    fn texts(lines: &[Arc<str>]) -> Vec<&str> {
        lines.iter().map(|l| l.as_ref()).collect()
    }

    #[test]
    fn a_removal_comes_back_before_the_line_that_followed_it() {
        let new = file("one\ntwo\nthree");
        let old = file("one\nGONE\ntwo\nthree");
        let changed = vec![
            mk(LineKind::Context, Some(1), Some(1), "one"),
            mk(LineKind::Removed, Some(2), None, "GONE"),
            mk(LineKind::Context, Some(3), Some(2), "two"),
            mk(LineKind::Context, Some(4), Some(3), "three"),
        ];
        let (out, kinds) = redline(&new, &old, &changed);
        assert_eq!(texts(&out), vec!["one", "GONE", "two", "three"]);
        assert_eq!(
            kinds,
            vec![
                LineKind::Context,
                LineKind::Removed,
                LineKind::Context,
                LineKind::Context
            ]
        );
    }

    #[test]
    fn an_addition_is_marked_where_it_lands() {
        let new = file("a\nNEW\nb");
        let old = file("a\nb");
        let changed = vec![
            mk(LineKind::Context, Some(1), Some(1), "a"),
            mk(LineKind::Added, None, Some(2), "NEW"),
            mk(LineKind::Context, Some(2), Some(3), "b"),
        ];
        let (out, kinds) = redline(&new, &old, &changed);
        assert_eq!(texts(&out), vec!["a", "NEW", "b"]);
        assert_eq!(
            kinds,
            vec![LineKind::Context, LineKind::Added, LineKind::Context]
        );
    }

    #[test]
    fn a_removal_at_the_end_of_the_file_lands_at_the_end() {
        let new = file("a\nb");
        let old = file("a\nb\nGONE");
        let changed = vec![
            mk(LineKind::Context, Some(1), Some(1), "a"),
            mk(LineKind::Context, Some(2), Some(2), "b"),
            mk(LineKind::Removed, Some(3), None, "GONE"),
        ];
        let (out, kinds) = redline(&new, &old, &changed);
        assert_eq!(texts(&out), vec!["a", "b", "GONE"]);
        assert_eq!(kinds.last(), Some(&LineKind::Removed));
    }

    #[test]
    fn a_spliced_removal_is_the_files_own_text_and_not_the_rows() {
        // The row's text has had its markdown taken off it by the presentation;
        // splicing that back in would leave a table row with no pipes in it.
        let new = file("x");
        let old = file("x\n| left | 12 |");
        let changed = vec![
            mk(LineKind::Context, Some(1), Some(1), "x"),
            mk(LineKind::Removed, Some(2), None, "left | 12"),
        ];
        let (out, _) = redline(&new, &old, &changed);
        assert_eq!(texts(&out), vec!["x", "| left | 12 |"]);
    }

    #[test]
    fn a_file_with_no_other_side_keeps_what_it_has() {
        // An untracked file: every line an addition, and no removal to place.
        let new = file("a\nb");
        let changed = vec![
            mk(LineKind::Added, None, Some(1), "a"),
            mk(LineKind::Added, None, Some(2), "b"),
            mk(LineKind::Removed, Some(1), None, "was here"),
        ];
        let (out, kinds) = redline(&new, &[], &changed);
        assert_eq!(texts(&out), vec!["a", "b"]);
        assert_eq!(kinds, vec![LineKind::Added, LineKind::Added]);
    }

    fn doc(text: &str) -> Document {
        Document::new(text, "a.md", &Highlighters::builtin())
    }

    #[test]
    fn a_heading_loses_its_hashes_and_keeps_its_level() {
        let d = doc("# Title\n\ntext under it\n");
        assert_eq!(d.block(0), Some(Block::Heading(1)));
        assert_eq!(d.text(0), Some("Title"));
        assert_eq!(d.block(2), Some(Block::Paragraph));
        assert_eq!(d.text(2), Some("text under it"));
    }

    #[test]
    fn a_link_is_down_to_its_text_and_its_url_is_gone() {
        let d = doc("[the docs](https://example.com/a/very/long/path)\n");
        assert_eq!(d.text(0), Some("the docs"));
    }

    #[test]
    fn a_fence_opens_once_and_closes_once_however_long_it_is() {
        // The case a hunk-shaped pass cannot express: 200 body lines between
        // one fence and the other, and every one of them a `Code` row with its
        // bytes untouched. A `#` inside is a comment, a `*` is a dereference,
        // and a backtick is a backtick.
        let body = "  *pointer = `literal`;  # not a heading";
        let text = format!("```rust\n{}\n```\n# After\n", vec![body; 200].join("\n"));
        let d = doc(&text);
        assert_eq!(d.block(0), Some(Block::Fence));
        assert_eq!(d.text(0), Some("rust"), "the fence keeps its language");
        for row in 1..=200 {
            assert_eq!(d.block(row), Some(Block::Code), "row {row}");
            assert_eq!(d.text(row), Some(body), "row {row} was rewritten");
        }
        assert_eq!(d.block(201), Some(Block::Fence));
        assert_eq!(d.block(202), Some(Block::Heading(1)));
        assert_eq!(d.text(202), Some("After"));
    }

    #[test]
    fn an_indented_hash_inside_a_fence_is_not_a_heading() {
        let d = doc("```\n# not a heading\n```\n");
        assert_eq!(d.block(1), Some(Block::Code));
    }

    #[test]
    fn a_table_is_measured_against_its_own_rows_and_its_cells_are_ranges() {
        let d =
            doc("| stage | what | cost |\n| :--: | --- | --: |\n| parse | the log | 466 ms |\n");
        let table = &d.tables()[0];
        assert_eq!(table.rows, 0..3);
        assert_eq!(table.grid.widths, vec![5, 7, 6]);
        assert_eq!(
            table.grid.aligns,
            vec![
                markdown::Align::Center,
                markdown::Align::Left,
                markdown::Align::Right
            ]
        );
        assert_eq!(d.table_at(2).map(|t| t.rows.clone()), Some(0..3));
        assert_eq!(d.table_at(3), None, "the paragraph below it is not a table");
        // The cells index the row's own text, and the separator row is the one
        // whose content is punctuation rather than cells.
        let text = d.text(2).unwrap();
        let cells = &table.cells[2];
        assert_eq!(&text[cells[0].clone()], "parse");
        assert_eq!(&text[cells[1].clone()], "the log");
        assert_eq!(&text[cells[2].clone()], "466 ms");
    }

    #[test]
    fn two_tables_are_two_grids() {
        let d =
            doc("| a |\n|---|\n| 1 |\n\ntext between\n\n| a much longer cell |\n|---|\n| 2 |\n");
        assert_eq!(d.measured(), 2);
        let [first, second] = d.tables() else {
            panic!("two tables")
        };
        assert_eq!(first.grid.widths, vec![1], "measured against the other one");
        assert_eq!(second.grid.widths, vec![18]);
    }

    #[test]
    fn every_token_indexes_the_text_the_row_hands_a_renderer() {
        // The invariant that turns into a panic inside GPUI's text layout
        // rather than into a wrong colour.
        let d = doc("# Title with **bold**\n\n- a bullet\n\n> quote\n\n```\nlet x = 1;\n```\n");
        for row in 0..d.len() {
            let text = d.text(row).unwrap();
            for t in d.tokens(row) {
                assert!(t.end as usize <= text.len(), "{t:?} outside {text:?}");
                assert!(
                    text.is_char_boundary(t.start as usize)
                        && text.is_char_boundary(t.end as usize),
                    "{t:?} off a boundary in {text:?}"
                );
                assert!(t.start < t.end);
            }
        }
    }

    #[test]
    fn a_document_lays_out_every_row_it_was_given() {
        // The row count is the file's, not the interesting subset of it: a
        // frontend draws one element per row and nothing may go missing, or a
        // document that ends mid-sentence looks like a document that ended.
        let text = "# one\n\nparagraph\n\n- bullet\n\n```\ncode\n```\n\n---\n";
        let d = doc(text);
        assert_eq!(d.len(), text.lines().count());
        assert!(!d.is_empty());
        assert_eq!(d.rows(), d.len());
        assert_eq!(d.block(d.len()), None, "out of range answers, never panics");
        assert_eq!(d.text(d.len()), None);
    }
}
