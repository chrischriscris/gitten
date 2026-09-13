//! A markdown file drawn as a document: variable height, real cells, code
//! blocks with a background of their own.
//!
//! # Why this is a pane and not a taller row
//!
//! [`super::markdown::MarkdownRows`] renders a `.md` *diff* — a `Block` per row
//! of the list, drawn inside `ROW_H` — and it does that because `uniform_list`
//! is the only reason a 714k-row diff scrolls at all. Everything a document
//! wants and cannot have follows from that one number:
//!
//! - a heading may not be much more than 18px, because a glyph needs about 1.2×
//!   its point size of line box and the row is 22px — past that it clips into
//!   the row below, which is why levels 4–6 fall back to the body size;
//! - a fenced block cannot have a background, because a background on a row
//!   means added or removed and a diff may not give that up;
//! - a table is aligned by padding cells with spaces, because a row is one
//!   `StyledText` on a monospaced line — so a cell cannot wrap, and a grid
//!   wider than the window is squeezed or scrolled to;
//! - prose wraps only where the row's own budget says, because a wrapped line
//!   is more rows and never a taller one (decision 0017).
//!
//! This pane gives all four up on purpose. It is not a list, so every element
//! sizes itself, GPUI wraps the text, and the reader scrolls. That is the trade
//! decision 0006 named when it said a rendered preview "wants a pane, and panes
//! do not exist yet": this is that pane, for the one file type it was written
//! about.
//!
//! # What it draws from
//!
//! [`gitten_core::document::Document`] — one `Block` per row, the row's text
//! with its markers already off and its tokens already moved onto it, and every
//! table measured against its own rows with cells as byte ranges. Nothing here
//! decides what a document *is*; the numbers this file brings are how large a
//! heading is and how far one indent step goes, and those are its business
//! because they are pixels.
//!
//! The grouping the renderer needs — a fence and its body becoming one element,
//! a table becoming one grid — happens once, in [`sections_of`], not per frame.
//! A render path that regrouped its own rows every frame would be rule 3's
//! exact complaint, and the grouping cannot change until the file does.
//!
//! # What is deliberately missing
//!
//! - **No syntax highlighting inside a fence.** The `Markdown` highlighter
//!   emits code fences as one `Str` and no colours, and injecting the routed
//!   highlighter into a fence body is decision 0010's own named gap. Code here
//!   is the chrome's text colour on the chrome's raised surface — a pairing the
//!   theme already resolves, which is why no new palette entry was needed.
//! - **No images.** `![alt](diagram.png)` draws its alt text, exactly as a row
//!   presentation draws it.
//! - **No links you can follow.** A link is the accent colour and an underline;
//!   opening one is a verb this pane does not have.

use crate::views::diff::{slice, PAD};
use gitten_core::document::Document;
use gitten_core::font::Font;
use gitten_core::host::Host;
use gitten_core::markdown::{Align, Block};
use gitten_core::runs::{runs, Run};
use gitten_core::syntax::Kind;
use gitten_core::theme::{Rgb, Surface};
use gitten_core::LineKind;
use gpui::*;
use std::ops::Range;

/// How much of a file this pane will read and lay out.
///
/// Four megabytes, and it is a reading limit rather than a memory one: a
/// document somebody reads in a pane is prose, and the largest markdown file in
/// this repository is 31 KB. A file past this is refused with its size, which is
/// a sentence a reader can act on — a `CONTRIBUTING.md` of 40 MB is a mistake,
/// and rendering it would be a pane that hangs on a keypress.
pub const CAP: u64 = 4 << 20;

/// How a document is proportioned.
///
/// A struct and not constants, for rule 1: these are numbers somebody will want
/// to disagree with, and a built-in may not hold a knob an extension cannot
/// reach.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Metrics {
    /// Point size per heading level, `[0]` being `#`.
    ///
    /// **No ceiling on it**, which is half the reason this pane exists: the row
    /// presentation clamps its scale to `ROW_H`, and here a heading is as large
    /// as a document heading should be — an `#` at 1.7× the body, a `######` at
    /// the body size and separated by weight, which is what typographic scales
    /// do at that depth anyway.
    pub heading: [f32; 6],
    /// One step of list indent, in pixels.
    pub indent: f32,
    /// Width of the bar beside a quote and a fenced block.
    pub bar: f32,
    /// A blank row's height: a breath between blocks, not a line of nothing.
    pub gap: f32,
    /// Padding inside a code block, and inside a table cell.
    pub pad: f32,
    /// Bullet glyph per depth; the last one repeats.
    pub bullets: &'static [&'static str],
}

impl Default for Metrics {
    fn default() -> Self {
        Self {
            heading: [24.0, 20.0, 17.0, 15.5, 15.0, 15.0],
            indent: 18.0,
            bar: 3.0,
            gap: 10.0,
            pad: 10.0,
            bullets: &["•", "◦", "▪", "·"],
        }
    }
}

impl Metrics {
    /// The scale relative to a font, so a document looks right at any body size
    /// — the same rule the row presentation's `for_font` follows, without its
    /// ceiling.
    pub fn for_font(font: &Font) -> Self {
        let scale = [1.7, 1.4, 1.2, 1.1, 1.05, 1.0];
        let mut heading = [font.size; 6];
        for (h, factor) in heading.iter_mut().zip(scale) {
            *h = font.scaled(factor);
        }
        Self {
            heading,
            gap: font.scaled(0.7),
            indent: font.scaled(1.2),
            pad: font.scaled(0.7),
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

/// One thing the renderer draws: a row, a fence with its body, or a table.
///
/// The grouping is what makes a code block *one* element with one background
/// instead of a run of rows that happen to share a colour, and it is decided
/// once, when the document arrives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Section {
    /// One row of the file, by index: a heading, prose, a bullet, a quote, a
    /// rule, a blank.
    Row(usize),
    /// A fenced block: its opening line — whose text is the language, if it
    /// named one — and the rows inside it. The closing fence is a line of the
    /// file that draws nothing, which is what the label's absence says.
    Code {
        label: Option<usize>,
        body: Range<usize>,
    },
    /// A measured table, by index into [`Document::tables`].
    Table(usize),
}

/// What drawing borrows and reuses: the run merge's output, and the styles it
/// resolves to. One per pane, cleared per row — a `Vec` per visible row per
/// frame is the thing rule 3 is about.
#[derive(Default)]
pub struct Scratch {
    runs: Vec<Run>,
    styles: Vec<(Range<usize>, HighlightStyle)>,
}

/// The document pane: one file's blocks, drawn at their own heights.
pub struct DocumentPane {
    /// What is being shown, for the strip above the pane and for a report.
    path: String,
    doc: Option<Document>,
    metrics: Metrics,
    /// The rows grouped into draws, built once per document.
    sections: Vec<Section>,
    /// What the diff made of each row, one entry per row of the document, with
    /// the removals spliced back in — so the pane can draw the change *inside*
    /// the document and not only beside it in the diff. Empty for a document
    /// with no diff behind it, which reads as all context.
    kinds: Vec<LineKind>,
    scratch: std::cell::RefCell<Scratch>,
}

impl Default for DocumentPane {
    fn default() -> Self {
        Self::new()
    }
}

impl DocumentPane {
    pub fn new() -> Self {
        Self {
            path: String::new(),
            doc: None,
            metrics: Metrics::default(),
            sections: Vec::new(),
            kinds: Vec::new(),
            scratch: std::cell::RefCell::default(),
        }
    }

    /// Hands the pane a document, which is the only thing that makes it show.
    ///
    /// The metrics are derived from the font here rather than per frame: the
    /// heading scale is a function of the body size, and recomputing six
    /// multiplications per frame for an answer that changes when `gitten.toml`
    /// does is the kind of work rule 3 exists to stop.
    pub fn show(&mut self, path: &str, doc: Document, kinds: Vec<LineKind>, font: &Font) {
        self.path = path.to_string();
        self.metrics = Metrics::for_font(font);
        self.sections = sections_of(&doc);
        self.doc = Some(doc);
        self.kinds = kinds;
    }

    /// Back to nothing: the centre returns to the diff rows.
    pub fn clear(&mut self) {
        self.doc = None;
        self.sections.clear();
        self.path.clear();
        self.kinds.clear();
    }

    /// Whether the pane has a document to draw, which is what the shell asks
    /// before it gives the centre's body to it.
    pub fn is_showing(&self) -> bool {
        self.doc.is_some()
    }

    fn doc(&self) -> Option<&Document> {
        self.doc.as_ref()
    }

    /// What the diff made of one row — context, added or removed.
    ///
    /// Every row the shell hands the pane has an entry. A document with no diff
    /// behind it has none and is all context, which is what the empty case
    /// answers; a row past the end of the list is the same thing said by the
    /// same default rather than a panic in a render path.
    fn kind(&self, row: usize) -> LineKind {
        self.kinds.get(row).copied().unwrap_or(LineKind::Context)
    }

    /// One row's text as a `SharedString`: a refcount bump, never a copy. A
    /// document's rows are `Arc<str>` from the read all the way down, and a
    /// `String` per visible row per frame is the allocation this avoids.
    fn shared(&self, row: usize) -> SharedString {
        match self.doc().and_then(|d| d.line(row)) {
            Some(line) => SharedString::from(line.text.clone()),
            None => SharedString::from(""),
        }
    }

    /// A range of one row as a `SharedString`, borrowing the row's storage.
    fn part(&self, row: usize, at: Range<usize>) -> SharedString {
        match self.doc().and_then(|d| d.line(row)) {
            Some(line) => slice(&line.text, &at),
            None => SharedString::from(""),
        }
    }

    /// One row's inline styling, clipped to `at` and rebased onto it.
    ///
    /// `at` exists for the one caller whose text is a *slice* of a row: a table
    /// cell. A `StyledText` is handed the cell's own text, so a run reaching
    /// past the cell is a range off the end of the string — a panic inside
    /// GPUI's text layout rather than a wrong colour. For every other caller
    /// `at` is the whole row and this is the identity.
    ///
    /// The merge is [`gitten_core::runs`] and not a second walk over the
    /// tokens, because a `.md` row's markup *is* a token — `Strong` over
    /// `**word**`, the delimiters already cut off by the block pass — and one
    /// implementation of "which bytes are one run" is the difference between a
    /// bold word and a bold sentence.
    fn styles<'s>(
        &self,
        row: usize,
        host: &Host,
        at: Range<usize>,
        sc: &'s mut Scratch,
    ) -> &'s [(Range<usize>, HighlightStyle)] {
        let theme = &host.theme;
        // What the diff made of this row decides its ink: a line it added or
        // took away draws in the diff's own colour for it, so the pane and the
        // rows agree on what green means. Prose no highlighter claimed draws in
        // that colour too — for a context row, the page's own text tone, the one
        // a context row of the diff uses, so a document and a diff of the same
        // file read as the same text.
        let kind = self.kind(row);
        let (_, plain, _) = theme.diff.line_colors(kind, false);
        let text = self.doc().and_then(|d| d.text(row)).unwrap_or_default();
        runs(
            0..text.len(),
            self.tokens(row),
            &[],
            LineKind::Context,
            false,
            &mut sc.runs,
        );
        sc.styles.clear();
        for run in sc.runs.iter() {
            // The theme resolves a token against the surface it lands on —
            // colour, weight and slant — and this pane takes all three rather
            // than deciding in a second place what a heading looks like.
            let style = run.kind.map(|kind| theme.syntax_on(kind, Surface::Context));
            // A row the diff touched keeps the diff's colour and takes only
            // weight and slant from the syntax: the change outranks the grammar,
            // or an added heading would be green nowhere on the row.
            let colour = if kind == LineKind::Context {
                style.map_or(plain, |s| s.fg)
            } else {
                plain
            };
            let mut hl = HighlightStyle {
                color: Some(rgb(colour).into()),
                font_weight: style.filter(|s| s.bold).map(|_| FontWeight::BOLD),
                font_style: style.filter(|s| s.italic).map(|_| FontStyle::Italic),
                ..Default::default()
            };
            match run.kind {
                // A link's URL is gone from a rendered row, so the underline is
                // what still says the words are one.
                Some(Kind::Link) => {
                    hl.underline = Some(UnderlineStyle {
                        thickness: px(1.0),
                        color: Some(rgb(colour).into()),
                        wavy: false,
                    })
                }
                // Inline code reads as code: the raised surface behind it is the
                // one a fenced block sits on, so `x = 1` in a sentence and a
                // block of it look like the same thing.
                Some(Kind::Str) => hl.background_color = Some(rgb(theme.chrome.raised).into()),
                _ => {}
            }
            // A line that is gone reads as gone: struck through in its own
            // colour, which is how a document says "this was here" without a
            // second column to say it in.
            if kind == LineKind::Removed {
                hl.strikethrough = Some(gpui::StrikethroughStyle {
                    thickness: px(1.0),
                    color: Some(rgb(plain).into()),
                });
            }
            // Clipped to what is about to be drawn and moved onto it: a run
            // half inside a cell keeps the half that is there.
            let (start, end) = (run.at.start.max(at.start), run.at.end.min(at.end));
            if start < end {
                sc.styles.push((start - at.start..end - at.start, hl));
            }
        }
        &sc.styles
    }

    /// How many bytes a row's text is — what a whole-row style range spans.
    fn row_len(&self, row: usize) -> usize {
        self.doc()
            .and_then(|d| d.text(row))
            .map_or(0, |text| text.len())
    }

    fn tokens(&self, row: usize) -> &[gitten_core::syntax::Token] {
        self.doc().map_or(&[], |d| d.tokens(row))
    }

    /// One row's text, styled, as the body of whatever block holds it.
    fn text(
        &self,
        row: usize,
        host: &Host,
        colour: Rgb,
        size: f32,
        sc: &mut Scratch,
    ) -> AnyElement {
        let styled = StyledText::new(self.shared(row)).with_highlights(
            self.styles(row, host, 0..self.row_len(row), sc)
                .iter()
                .cloned(),
        );
        div()
            .flex_grow(1.0)
            .min_w_0()
            .text_size(px(size))
            .text_color(rgb(colour))
            .child(styled)
            .into_any_element()
    }

    /// One section, drawn.
    ///
    /// Public to the crate so a test can walk every shape without a window —
    /// the same door [`super::markdown`]'s row renderer leaves open, for the
    /// same reason.
    pub(crate) fn section(&self, at: usize, host: &Host, sc: &mut Scratch) -> AnyElement {
        let m = &self.metrics;
        let theme = &host.theme;
        let md = &theme.markdown;
        let (_, prose, _) = theme.diff.line_colors(LineKind::Context, false);
        let body = host.font.size;
        let Some(doc) = self.doc() else {
            return div().into_any_element();
        };
        let Some(section) = self.sections.get(at) else {
            return div().into_any_element();
        };

        match section {
            Section::Row(row) => {
                let Some(block) = doc.block(*row) else {
                    return div().into_any_element();
                };
                match block {
                    // A blank row is a breath and not a line of nothing: two
                    // paragraphs are already separated by the gap under the
                    // first, and a full line between them would double-space
                    // every document written with blank lines in it.
                    Block::Blank => div().h(px(m.gap)).into_any_element(),
                    Block::Rule => div()
                        .w_full()
                        .h(px(1.0))
                        .my(px(m.gap))
                        .bg(rgb(md.rule))
                        .into_any_element(),
                    // A heading is the element's own size, and the run list
                    // cannot carry one — `HighlightStyle` is documented as
                    // "uniformly sized text" — so the size is set here and the
                    // margins are what separate it from the prose around it.
                    Block::Heading(level) => div()
                        .flex()
                        .w_full()
                        .mt(px(if level == 1 { 22.0 } else { 16.0 }))
                        .mb(px(4.0))
                        .child(
                            div()
                                .flex_none()
                                .text_size(px(m.size(level)))
                                .font_weight(FontWeight::BOLD)
                                .text_color(rgb(prose))
                                .font_family(host.font.family.clone())
                                .child(
                                    StyledText::new(self.shared(*row)).with_highlights(
                                        self.styles(*row, host, 0..self.row_len(*row), sc)
                                            .iter()
                                            .cloned(),
                                    ),
                                ),
                        )
                        .into_any_element(),
                    // A quote is a bar and a quieter voice.
                    Block::Quote(depth) => div()
                        .flex()
                        .w_full()
                        .mb(px(m.gap))
                        .pl(px(depth as f32 * m.indent))
                        .child(div().flex_none().w(px(m.bar)).bg(rgb(md.quote_bar)))
                        .child(div().flex_none().w(px(m.pad)))
                        .child(self.text(*row, host, theme.dim_on(Surface::Context), body, sc))
                        .into_any_element(),
                    // A bullet's glyph is furniture: it is the marker that was
                    // removed, drawn again — so a wrapped item continues under
                    // its own text at the same indent and the reader can still
                    // see where the item began.
                    Block::Bullet(depth) => div()
                        .flex()
                        .w_full()
                        .mb(px(4.0))
                        .pl(px(depth as f32 * m.indent))
                        .child(
                            div()
                                .flex_none()
                                .w(px(m.indent))
                                .text_color(rgb(theme.marker_on(Surface::Context)))
                                .child(m.bullet(depth)),
                        )
                        .child(self.text(*row, host, prose, body, sc))
                        .into_any_element(),
                    // An ordered item keeps its number: it is content and not
                    // punctuation, and hiding it would lose which item this is.
                    Block::Ordered(depth) => div()
                        .flex()
                        .w_full()
                        .mb(px(4.0))
                        .pl(px(depth as f32 * m.indent))
                        .child(self.text(*row, host, prose, body, sc))
                        .into_any_element(),
                    // A `Code` row with no fence above it in the file — a file
                    // that begins inside a block — is drawn as its own.
                    Block::Code | Block::Fence => self.code(*row..*row + 1, None, host, sc),
                    // A table row reaching here means the measurement refused
                    // the run, so it has no grid to sit in. Its own text is the
                    // honest answer and an empty row is not.
                    Block::Table | Block::TableRule => div()
                        .flex()
                        .w_full()
                        .child(self.text(*row, host, prose, body, sc))
                        .into_any_element(),
                    Block::Paragraph => div()
                        .flex()
                        .w_full()
                        .mb(px(m.gap))
                        .child(self.text(*row, host, prose, body, sc))
                        .into_any_element(),
                }
            }
            Section::Code { label, body: rows } => self.code(rows.clone(), *label, host, sc),
            Section::Table(which) => self.table(*which, host, sc),
        }
    }

    /// A fenced block: the language as a label, then the body on its own
    /// surface.
    ///
    /// The label is the fence line's text — the block pass leaves the language
    /// and takes the backticks — and it is drawn dim and one size down, because
    /// it is punctuation *about* the block rather than a line of it.
    fn code(
        &self,
        rows: Range<usize>,
        label: Option<usize>,
        host: &Host,
        _sc: &mut Scratch,
    ) -> AnyElement {
        let m = &self.metrics;
        let theme = &host.theme;
        // A file's own text, drawn as one element per line: this is the one
        // place the pane is still line-shaped, because a code block *is* lines
        // — its indentation is content and its blank lines are part of it.
        let code = div()
            .flex()
            .flex_col()
            .w_full()
            .font_family(host.font.family.clone())
            .bg(rgb(theme.chrome.raised))
            .border_l(px(m.bar))
            .border_color(rgb(theme.markdown.code_bar))
            .py(px(m.pad * 0.5))
            .text_size(px(host.font.size))
            .text_color(rgb(theme.chrome.fg))
            .children(rows.map(|row| {
                let text = self.shared(row);
                // An empty line in a fenced block is a line of the block: it
                // takes a line's height and no ink. A `SharedString` of
                // nothing would collapse to no height at all.
                div().px(px(m.pad)).child(match text.is_empty() {
                    true => SharedString::from(" "),
                    false => text,
                })
            }));
        match label.and_then(|l| self.doc().and_then(|d| d.text(l))) {
            Some(language) if !language.is_empty() => div()
                .flex()
                .flex_col()
                .w_full()
                .mt(px(m.gap))
                .child(
                    div()
                        .text_size(px(host.font.size - 2.0))
                        .text_color(rgb(theme.marker_on(Surface::Context)))
                        .mb(px(2.0))
                        .child(SharedString::from(language.to_string())),
                )
                .child(code)
                .into_any_element(),
            _ => div().w_full().mt(px(m.gap)).child(code).into_any_element(),
        }
    }

    /// A table as a grid: one row of cells per line of the file, a hairline
    /// under each, and the columns measured against the whole table.
    ///
    /// **Cells are elements and columns are weighted**, which is the difference
    /// between this and the row presentation. A row there pads cells with
    /// spaces, so it needs a monospaced face and cannot wrap a cell; here each
    /// column takes a share of the width in proportion to what it asked for, so
    /// a narrow window squeezes the long column and leaves a `yes` alone —
    /// water-filling, the policy `core::markdown::flow_table` also applies,
    /// reached differently because the unit here is an element and not a
    /// character.
    fn table(&self, which: usize, host: &Host, sc: &mut Scratch) -> AnyElement {
        let m = &self.metrics;
        let theme = &host.theme;
        let md = &theme.markdown;
        let (_, prose, _) = theme.diff.line_colors(LineKind::Context, false);
        let body = host.font.size;
        let Some(doc) = self.doc() else {
            return div().into_any_element();
        };
        let Some(table) = doc.tables().get(which) else {
            return div().into_any_element();
        };
        let widths = &table.grid.widths;
        let aligns = &table.grid.aligns;
        let columns = widths.len();
        // The header is the table's first row, and it is drawn heavier: the
        // rule beneath it says the grid started, and the weight says which row
        // names the columns. A document's table header is bold everywhere a
        // reader has seen one, and a grid where every row weighs the same makes
        // them re-read the first row to find out what it was.
        let header = table.rows.start;
        // What one character of the pane's face costs, for the columns' floors.
        // The face is monospaced, and the measurement core did is in
        // characters, so this is the only conversion the grid needs.
        let advance = host.font.size * 0.62;

        let mut grid = div().flex().flex_col().w_full().my(px(m.gap));
        for (offset, row) in table.rows.clone().enumerate() {
            // A separator row is punctuation that draws a line: the line is
            // what it meant, and `|---|---|` in a document is noise.
            if doc.block(row) == Some(Block::TableRule) {
                grid = grid.child(
                    div()
                        .w_full()
                        .h(px(1.0))
                        .bg(rgb(md.rule))
                        .mb(px(m.pad * 0.3)),
                );
                continue;
            }
            let empty = table.cells.get(offset).map_or(&[][..], |c| c.as_ref());
            // Whether this row's line is the separator's to draw. The two would
            // otherwise stack — a row's bottom border plus the separator's own
            // hairline, a pixel apart — and the header would carry a rule one
            // pixel heavier than every other rule in the table.
            let separator_below =
                row + 1 < table.rows.end && doc.block(row + 1) == Some(Block::TableRule);
            let mut line = div().flex().w_full().items_start().pb(px(m.pad * 0.3));
            if !separator_below {
                line = line.border_b_1().border_color(rgb(md.rule));
            }
            // A row the diff touched wears its colour across the whole grid row.
            let row_kind = self.kind(row);
            if row_kind != LineKind::Context {
                line = line.bg(rgb(theme.diff.line_colors(row_kind, false).0));
            }
            for k in 0..columns {
                let weight = widths.get(k).copied().unwrap_or(1).max(1) as f32;
                let cell = match empty.get(k) {
                    Some(cell) => {
                        let styled = StyledText::new(self.part(row, cell.clone())).with_highlights(
                            self.styles(row, host, cell.clone(), sc).iter().cloned(),
                        );
                        let drawn = div()
                            .text_size(px(body))
                            .text_color(rgb(prose))
                            .child(styled);
                        if row == header {
                            drawn.font_weight(FontWeight::BOLD)
                        } else {
                            drawn
                        }
                    }
                    // A ragged row still gets its missing columns: a row that
                    // stopped early would shear the grid below it.
                    None => div().child(SharedString::from("")),
                };
                let cell = match aligns.get(k).copied().unwrap_or_default() {
                    Align::Left => cell.flex().justify_start(),
                    Align::Center => cell.flex().justify_center(),
                    Align::Right => cell.flex().justify_end(),
                };
                // One width per column, for the whole table. The basis is
                // zero, so a cell's share comes from the column's weight and
                // never from how long this row's text happens to be: with a
                // content-sized basis a long cell in one row pushes that row's
                // later columns somewhere the row above did not put them, and
                // a grid whose columns wander is not a grid. The floor is the
                // column's own measured width, never this row's cell, for the
                // same reason — and it is what keeps a short column from being
                // squeezed below the text it holds.
                line = line.child(
                    cell.flex_grow(weight)
                        .flex_basis(px(0.0))
                        .min_w(px(weight * advance))
                        .px(px(m.pad * 0.5)),
                );
            }
            grid = grid.child(line);
        }
        grid.into_any_element()
    }

    /// The whole document, top to bottom, one section after another.
    fn column(&self, host: &Host) -> Div {
        let mut sc = self.scratch.borrow_mut();
        let mut column = div().flex().flex_col().w_full().px(px(PAD)).py(px(16.0));
        for at in 0..self.sections.len() {
            let drawn = self.section(at, host, &mut sc);
            // The wash. A line the diff added or took away carries its colour
            // across the whole row, which is what makes a change read as a
            // change in a document and not only as a pair of coloured glyphs.
            // A block section is not washed here: a fence owns its background,
            // and a table's wash belongs to each of its rows.
            let wash = match self.sections.get(at) {
                Some(Section::Row(row)) => {
                    let kind = self.kind(*row);
                    (kind != LineKind::Context).then(|| host.theme.diff.line_colors(kind, false).0)
                }
                _ => None,
            };
            column = column.child(match wash {
                Some(colour) => div()
                    .w_full()
                    .bg(rgb(colour))
                    .child(drawn)
                    .into_any_element(),
                None => drawn,
            });
        }
        column
    }
}

impl Render for DocumentPane {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The host is read on the render path and never captured: a captured
        // clone is a snapshot, and it is what makes `gitten.toml` apply on the
        // next frame instead of the next launch.
        let host = crate::config::host(cx);
        let (page, _, _) = host.theme.diff.line_colors(LineKind::Context, false);
        div()
            // `.id()` before a scroll: those methods live on
            // `StatefulInteractiveElement` and there is no way in without one.
            .id("document-scroll")
            .debug_selector(|| "document-pane".to_string())
            .flex_grow(1.0)
            .min_h_0()
            .w_full()
            .overflow_y_scroll()
            .bg(rgb(page))
            .child(self.column(&host))
    }
}

/// Which rows a measured table of this document is on, for [`sections_of`].
fn table_rows(doc: &Document, which: usize) -> Option<Range<usize>> {
    doc.tables().get(which).map(|t| t.rows.clone())
}

/// Groups a document's rows into what the renderer draws.
///
/// Three rules, and each exists because doing it per frame would be work for an
/// answer that cannot change until the file does:
///
/// - a table's rows are one [`Section::Table`], found through the measured
///   tables rather than by counting pipes a second time;
/// - a fence and every row inside it are one [`Section::Code`], so a code block
///   is one element with one background — and the closing fence belongs to
///   nobody, which is why the label is an `Option`;
/// - everything else is a row, in file order.
///
/// A run of table rows the measurement refused has no `Table` to belong to and
/// is drawn as rows: an empty measurement means the run held nothing but
/// separators, and rows of punctuation are still rows of the file.
fn sections_of(doc: &Document) -> Vec<Section> {
    let mut out = Vec::new();
    let mut row = 0;
    let mut next_table = 0;
    while row < doc.len() {
        if let Some(rows) = table_rows(doc, next_table).filter(|rows| rows.start == row) {
            out.push(Section::Table(next_table));
            next_table += 1;
            row = rows.end;
            continue;
        }
        if doc.block(row) == Some(Block::Fence) {
            let label = row;
            let mut end = row + 1;
            while end < doc.len() && doc.block(end) == Some(Block::Code) {
                end += 1;
            }
            // The closing fence is drawn by nobody: its backticks are
            // punctuation the block pass took off, and what is left of it is
            // an empty string. The row is stepped over, not drawn.
            let closed = end < doc.len() && doc.block(end) == Some(Block::Fence);
            out.push(Section::Code {
                label: Some(label),
                body: label + 1..end,
            });
            row = match closed {
                true => end + 1,
                false => end,
            };
            continue;
        }
        out.push(Section::Row(row));
        row += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    // By name, not a glob: `use gpui::*` in the parent shadows `#[test]`.
    use super::{sections_of, DocumentPane, Metrics, Scratch, Section, CAP};
    use gitten_core::document::Document;
    use gitten_core::host::Host;
    use gitten_core::markdown::Block;
    use gitten_core::syntax::Highlighters;

    const DOC: &str = "\
# gitten

A paragraph with **bold**, *italic*, `code` and [a link](https://example.com/x)
that runs on long enough to wrap in any window it is drawn in.

- a bullet
  - a nested bullet

1. an ordered item

> a quote

```rust
fn main() {
    let x = 1;
}
```

| stage | what it does | cost |
| :--: | --- | --: |
| parse | the log | 466 ms |

---

last paragraph
";

    fn built(src: &str) -> DocumentPane {
        let mut pane = DocumentPane::new();
        let doc = Document::new(src, "README.md", &Highlighters::builtin());
        let host = Host::new();
        pane.show("README.md", doc, Vec::new(), &host.font);
        pane
    }

    /// The same document, with the diff's own answer for each of its first rows.
    fn built_with_kinds(src: &str, kinds: Vec<gitten_core::LineKind>) -> DocumentPane {
        let mut pane = DocumentPane::new();
        let doc = Document::new(src, "README.md", &Highlighters::builtin());
        let host = Host::new();
        pane.show("README.md", doc, kinds, &host.font);
        pane
    }

    #[test]
    fn a_marked_row_draws_its_kind_without_a_window() {
        use gitten_core::LineKind;
        // The redline's own paths: a removed row is struck through and an added
        // one is washed, and both have to draw. A run reaching past the text it
        // was rebased onto is a panic inside GPUI's text layout rather than a
        // wrong colour, so this walks the sections and the column the wash is
        // applied in.
        let pane = built_with_kinds(
            DOC,
            vec![
                LineKind::Added,
                LineKind::Context,
                LineKind::Removed,
                LineKind::Context,
                LineKind::Added,
            ],
        );
        assert_eq!(pane.kind(0), LineKind::Added);
        assert_eq!(pane.kind(2), LineKind::Removed);
        // Past the entries it was handed: a document with no diff behind it is
        // all context, and a document whose diff covers part of it is the same.
        assert_eq!(pane.kind(DOC.lines().count()), LineKind::Context);
        let host = Host::new();
        let mut sc = Scratch::default();
        for at in 0..pane.sections.len() {
            let _ = pane.section(at, &host, &mut sc);
        }
        let _ = pane.column(&host);
    }

    #[test]
    fn a_fence_is_one_element_however_many_lines_are_in_it() {
        // The thing this pane exists for: a code block is one block, so it can
        // have one background and one bar, and a hundred lines of code are one
        // element rather than a hundred rows.
        let pane = built(DOC);
        let code = pane
            .sections
            .iter()
            .find_map(|s| match s {
                Section::Code { body, .. } => Some(body.clone()),
                _ => None,
            })
            .expect("a fenced block");
        assert_eq!(code.len(), 3, "the fence's body: {code:?}");
        assert_eq!(
            pane.sections
                .iter()
                .filter(|s| matches!(s, Section::Code { .. }))
                .count(),
            1,
            "a fence and its body are one draw"
        );
    }

    #[test]
    fn a_table_is_one_element_and_its_rows_have_a_grid() {
        let pane = built(DOC);
        let tables: Vec<&Section> = pane
            .sections
            .iter()
            .filter(|s| matches!(s, Section::Table(_)))
            .collect();
        assert_eq!(tables.len(), 1, "one table, one draw: {tables:?}");
        let doc = pane.doc().expect("a document");
        assert_eq!(
            doc.tables()[0].rows.len(),
            3,
            "a header, a separator and one row"
        );
    }

    #[test]
    fn every_row_of_the_file_is_drawn_by_exactly_one_section() {
        // No row may go missing and none may be drawn twice: a document with a
        // hole in it reads as a document that ended, and a duplicated row reads
        // as a bug in the file.
        let pane = built(DOC);
        let doc = pane.doc().expect("a document");
        let mut seen = vec![false; doc.len()];
        for section in &pane.sections {
            match section {
                Section::Row(row) => seen[*row] = true,
                Section::Table(which) => {
                    for row in doc.tables()[*which].rows.clone() {
                        seen[row] = true;
                    }
                }
                Section::Code { label, body } => {
                    if let Some(label) = label {
                        seen[*label] = true;
                    }
                    for row in body.clone() {
                        seen[row] = true;
                    }
                }
            }
        }
        // The one exception, said out loud: a closing fence is a row of the file
        // that draws nothing, so it is stepped over rather than drawn.
        let fence = (0..doc.len())
            .filter(|r| doc.block(*r) == Some(Block::Fence))
            .collect::<Vec<_>>();
        assert_eq!(
            fence.len(),
            2,
            "one opening fence and one closing: {fence:?}"
        );
        seen[fence[0]] = true;
        let missed: Vec<usize> = (0..doc.len()).filter(|r| !seen[*r]).collect();
        assert_eq!(missed, vec![fence[1]], "rows nobody drew");
    }

    #[test]
    fn every_section_draws_without_a_window() {
        // The shapes that panic in a drawing path: an empty paragraph, a table
        // with a ragged row, a fence with no language. Walked with no window at
        // all, which is the cheapest place to find out.
        let host = Host::new();
        for src in [DOC, "", "\n\n\n", "```\n", "| a |\n|---|\n| b | c |\n"] {
            let pane = built(src);
            let mut sc = Scratch::default();
            for at in 0..pane.sections.len() {
                let _ = pane.section(at, &host, &mut sc);
            }
            // Out of range answers with nothing rather than panicking: a stale
            // index from a reflow must not take the window down.
            let _ = pane.section(pane.sections.len() + 4, &host, &mut sc);
        }
    }

    #[test]
    fn the_pane_shows_only_what_it_was_given() {
        let mut pane = DocumentPane::new();
        assert!(!pane.is_showing());
        assert!(pane.doc.is_none());
        assert!(pane.sections.is_empty());
        assert!(pane.path.is_empty());

        let host = Host::new();
        pane.show(
            "docs/README.md",
            Document::new("# Title\n", "README.md", &Highlighters::builtin()),
            Vec::new(),
            &host.font,
        );
        assert!(pane.is_showing());
        assert_eq!(pane.path, "docs/README.md");
        assert_eq!(pane.doc.as_ref().map(|d| d.len()), Some(1));
        assert_eq!(pane.sections.len(), 1, "one heading, one draw");

        pane.clear();
        assert!(!pane.is_showing());
        assert!(pane.path.is_empty());
        assert!(pane.sections.is_empty());
    }

    #[test]
    fn the_heading_scale_is_monotonic_and_unclamped() {
        // The difference from the row presentation, in one test: an `#` is
        // larger than a row could hold, and nothing above clips into what
        // follows because nothing here is a fixed height.
        let m = Metrics::for_font(&gitten_core::font::Font::default());
        assert!(m.size(1) > 20.0, "h1 is {}", m.size(1));
        assert!(m.size(1) > m.size(2) && m.size(2) > m.size(3));
        assert!(m.size(6) >= gitten_core::font::Font::default().size * 0.99);
        // Out of range answers rather than panicking on a render path.
        assert_eq!(m.size(0), m.size(1));
        assert_eq!(m.size(9), m.size(6));
        assert_eq!(m.bullet(99), "·");
    }

    #[test]
    fn the_cap_is_a_reading_limit_and_not_a_memory_one() {
        // Quoted here so a change to it is a change to a test: four megabytes
        // is prose, and a document past it is refused with its size rather than
        // laid out.
        assert_eq!(CAP, 4 * 1024 * 1024);
    }

    #[test]
    fn a_document_is_grouped_the_same_way_twice() {
        // The grouping is a pure function of the document, which is what makes
        // it safe to do once at load and read for the life of the pane.
        let doc = Document::new(DOC, "README.md", &Highlighters::builtin());
        assert_eq!(sections_of(&doc), sections_of(&doc));
        assert_eq!(sections_of(&doc), built(DOC).sections);
    }
}
