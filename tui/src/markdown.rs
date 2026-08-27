//! The rendered Markdown presentation, drawn in cells.
//!
//! [`MarkdownRows`] is [`crate::rows::TextRows`] with the document in place of
//! the source: it claims `.md`, `.markdown` and `.mdx` after the built-in, and
//! draws the same prepared lines with their markers gone — `## ` off the front
//! and the row bold, `**` off a word that is now simply emphasized, a link down
//! to its text. Every structural decision is [`gitten_core::markdown::Document`]'s
//! — the same rows, the same table flow, the same wrap policy, the same
//! furniture description the window draws — so what is here is exactly what a
//! client owns: cell measurement, painting, and hit translation.
//!
//! # The anatomy is the built-in's
//!
//! Right-aligned old/new numbers through
//! [`Theme::gutter_on`](gitten_core::theme::Theme::gutter_on), the sign, this
//! block's furniture, then text. Row backgrounds and signs are
//! [`line_colors`](crate::rows::line_colors); token pieces are
//! [`Theme::syntax_on`](gitten_core::theme::Theme::syntax_on) through the same
//! runs sweep the built-in uses; marker, bar and rule colours come from
//! `theme.markdown`. A heading is bold, because cells cannot change point size
//! and weight is the whole of what says "this is a heading" here.
//!
//! # A table is still a grid
//!
//! The gutter and then nothing: the grid is aligned character by character
//! against the rows above and below it, so anything in front of one row and
//! not the next shears it. When a grid does not fit, core re-lays it out —
//! columns squeezed, cells wrapped inside them — and those rows arrive with
//! their breaks already decided. The hairline between two data rows is drawn
//! on the cells the grid occupies — an underline, never a row of `─` — because
//! a terminal cannot paint between two rows, and a row of the list no line
//! produced would make the gutter's numbering lie.
//!
//! # What wraps
//!
//! Core selects each row's budget: the caller's column count net of this
//! block's furniture, measured at [`STEP`] cells a step out of the semantic
//! description — the terminal's version of the window measuring the same
//! furniture in pixels. One character-count caveat is inherited from
//! `core::wrap` and documented there: budgets are characters, a cell is
//! sometimes two columns, and wide prose is clipped by the pen rather than
//! overflowing the grid.

use crate::rows::{
    col_at, digits, draw_runs, file_header, header_hit, hunk_header, line_colors, number, row_bg,
    Override, Rows, Text, MIN_DIGITS,
};
use crate::screen::{self, Ink, Pen};
use crate::MIN_WRAP_COLS;
use gitten_core::host::Host;
use gitten_core::markdown::{Bar, Block, DocRow, Document};
use gitten_core::prepared::File;
use gitten_core::rows::{Entry, Present};
use gitten_core::select::Hit;
use gitten_core::wrap::Wrap;

/// One indent step, in cells — the terminal's version of the window's
/// `Metrics::indent` at the shipped face, which is about two characters wide.
/// A bar, a depth step and a bullet slot each cost this much, and the same
/// number is what the budget is measured against and what `hit` subtracts.
const STEP: usize = 2;

/// Bullet glyph per depth; the last one repeats. This terminal's own list —
/// every glyph one cell wide — and a seam like the window's `Metrics::bullets`.
const BULLETS: [&str; 4] = ["•", "◦", "▪", "·"];

fn bullet(depth: u8) -> &'static str {
    BULLETS[(depth as usize).min(BULLETS.len() - 1)]
}

/// Columns of furniture a block draws in front of its text, measured out of
/// the semantic description core gives: a bar, each indent step, a bullet's
/// slot — [`STEP`] cells each. A table gets none of it: its grid is its own
/// text, aligned character by character against the rows around it.
///
/// Two callers must agree about this — the wrap budget and the caret — which
/// is why it is one function and not arithmetic at the call sites.
fn furniture(block: Block) -> usize {
    let f = block.furniture();
    STEP * (f.bar.is_some() as usize + f.depth as usize + f.bullet as usize)
}

/// The text budget one block gets at a `cols`-column width, given the
/// presentation's fixed `chrome` (two line-number columns and the sign).
/// Narrower than the floor is the floor: a window dragged narrower than its
/// own gutter must not turn a document into a column of letters.
fn budget(chrome: usize, block: Block, cols: usize) -> usize {
    cols.saturating_sub(chrome + furniture(block))
        .max(MIN_WRAP_COLS)
}

/// The rendered-markdown presentation. Registered after the built-in, it takes
/// every `.md`, `.markdown` and `.mdx` file in the diff.
pub struct MarkdownRows {
    /// The shared model: rows, blocks, tables, flowed grids, wrap ranges.
    doc: Document,
    /// Columns one line-number column needs, from the largest number actually
    /// in the diff — measured, like the built-in's, because a constant wide
    /// enough for a monorepo wastes columns everywhere else.
    digits: usize,
    /// Which extensions to claim. Owned rather than hardcoded so the same
    /// implementation can be pointed elsewhere without editing it.
    extensions: Vec<String>,
}

impl Default for MarkdownRows {
    fn default() -> Self {
        // The same three the `Markdown` highlighter is routed for, exactly as
        // the window's presentation claims them.
        Self::new(&["md", "markdown", "mdx"])
    }
}

impl MarkdownRows {
    pub fn new(extensions: &[&str]) -> Self {
        Self {
            doc: Document::default(),
            digits: 0,
            extensions: extensions.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// Columns one line-number column occupies.
    pub fn gutter(&self) -> usize {
        self.digits.max(MIN_DIGITS)
    }

    /// Everything a text row draws besides its text: two line-number columns,
    /// the sign column, and the single spaces between them.
    pub fn chrome(&self) -> usize {
        2 * self.gutter() + 4
    }
}

impl Present for MarkdownRows {
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

    fn build(&mut self, file: File) {
        let first = self.doc.len();
        self.doc.push(file);
        for i in first..self.doc.len() {
            if let Some(DocRow::Line { line, .. }) = self.doc.row(i) {
                let widest = line.old_no.unwrap_or(0).max(line.new_no.unwrap_or(0));
                self.digits = self.digits.max(digits(widest));
            }
        }
    }

    fn rows(&self, index: usize) -> usize {
        self.doc.rows(index)
    }

    /// Columns of what this visual row draws, furniture excluded — the
    /// furniture does not scroll, so it does not bound the horizontal scroll.
    /// Box drawing is one column a character and `screen::width` knows it;
    /// measuring bytes would put a table in the widest-row contest three
    /// times over.
    fn width(&self, index: usize, seg: usize) -> usize {
        let text = self.doc.text(index).unwrap_or_default();
        screen::width(text[self.doc.range(index, seg)].trim_end())
    }

    fn files(&self) -> &[Entry] {
        self.doc.files()
    }
}

impl Rows for MarkdownRows {
    fn reflow(&mut self, cols: usize, _host: &Host, wrap: &dyn Wrap) -> bool {
        // The unit adapter: one block's budget, in columns, from the terminal
        // width less this presentation's chrome less this block's furniture in
        // cells. Core sees only the number.
        let chrome = self.chrome();
        let budget = move |block: Block| budget(chrome, block, cols);
        self.doc.reflow(&budget, wrap)
    }

    fn render(
        &self,
        index: usize,
        seg: usize,
        at: &crate::rows::Frame,
        pen: &mut Pen,
        out: &mut Vec<gitten_core::runs::Run>,
    ) {
        let theme = at.theme();
        let md = &theme.markdown;
        let Some(row) = self.doc.row(index) else {
            return;
        };
        match row {
            DocRow::File { path, adds, dels } => file_header(path, *adds, *dels, at, pen),
            DocRow::Hunk(header) => hunk_header(header, at, pen),
            DocRow::Line { block, line } => {
                let p = &theme.diff;
                let (own, fg, sign) = line_colors(line.kind, line.moved, p);
                let bg = row_bg(own, at);
                let row_ink = Ink::new(fg, bg);
                let gutter = Ink::new(p.gutter_fg, bg);
                // A continuation of a wrapped row: the same furniture, so a
                // wrapped bullet stays under its own text and a wrapped quote
                // keeps its bar, and no number and no sign, as everywhere else.
                let blank = seg > 0;

                number(line.old_no, blank, self.gutter(), gutter, pen);
                pen.put(" ", gutter);
                number(line.new_no, blank, self.gutter(), gutter, pen);
                pen.put(" ", gutter);
                pen.put(if blank { " " } else { sign }, row_ink);
                pen.put(" ", row_ink);

                let f = block.furniture();
                // A table row's grid is its own text: the gutter and then
                // nothing, or one row of the grid lands a column off the next.
                if !f.table {
                    // The bar repeats on every segment; the bullet reserves
                    // its slot on every segment and draws its glyph on the
                    // first only, so a wrapped item continues under its own
                    // text rather than under another bullet.
                    match f.bar {
                        Some(Bar::Quote) => {
                            pen.put(" ", Ink::new(md.quote_bar, md.quote_bar));
                            pen.put(" ", row_ink);
                        }
                        Some(Bar::Code) => {
                            pen.put(" ", Ink::new(md.code_bar, md.code_bar));
                            pen.put(" ", row_ink);
                        }
                        None => {}
                    }
                    for _ in 0..f.depth {
                        pen.put("  ", row_ink);
                    }
                    if f.bullet {
                        match blank {
                            false => {
                                pen.put(bullet(f.depth), Ink::new(md.marker, bg));
                                pen.put(" ", row_ink);
                            }
                            true => {
                                pen.put("  ", row_ink);
                            }
                        }
                    }
                }

                // A rule draws no text: the dashes were the drawing, so they
                // are replaced by the thing they were drawing — a band of the
                // rule colour across the row's whole text budget, inside the
                // scrolled window, where the window draws its 1px.
                if f.rule {
                    pen.scroll(at.shift);
                    let w = pen.room();
                    let rule_ink = Ink::new(md.rule, md.rule);
                    for _ in 0..w {
                        pen.put(" ", rule_ink);
                    }
                    return;
                }

                let text = self.doc.text(index).unwrap_or_default();
                let span = self.doc.range(index, seg);
                let t = Text {
                    text,
                    tokens: self.doc.tokens(index),
                    spans: self.doc.spans(index),
                    kind: line.kind,
                    moved: line.moved,
                };
                // A heading is bold — a cell cannot change point size, so
                // weight is the whole of what says "this is a heading" here. A
                // fence's language label and a separator row are punctuation
                // the reader should be able to skip, in the marker's colour.
                let over = match block {
                    Block::Heading(_) => Override {
                        bold: true,
                        fg: None,
                    },
                    Block::Fence => Override {
                        bold: false,
                        fg: Some(md.marker),
                    },
                    Block::TableRule => Override {
                        bold: false,
                        fg: Some(md.rule),
                    },
                    _ => Override::default(),
                };
                let from = pen.col();
                draw_runs(
                    &t,
                    span,
                    theme,
                    row_ink,
                    at.shift,
                    at.part(0),
                    over,
                    pen,
                    out,
                );
                let to = pen.col();
                // The row's own background runs to the edge, as everywhere
                // else in the diff.
                pen.wash(row_ink);
                // The hairline: under the grid's own last segment, and only
                // when another data row follows — core's `rule_after`, drawn
                // on the cells the grid occupies and never as a row.
                if self.doc.rule_after(index, seg) {
                    pen.underline(from, to - from, md.rule);
                }
            }
        }
    }

    fn hit(&self, index: usize, seg: usize, col: usize, shift: usize) -> Option<Hit> {
        match self.doc.row(index)? {
            DocRow::File { path, .. } => Some(header_hit(path, col)),
            DocRow::Hunk(header) => Some(header_hit(header, col)),
            DocRow::Line { block, .. } => {
                // Before the text is a click in the gutter or on the
                // furniture — the same columns the budget was measured
                // against — which is a caret at the first character there is
                // to see, and never a byte to the left of the row's own text.
                let text = col.saturating_sub(self.chrome() + furniture(*block)) + shift;
                let span = self.doc.range(index, seg);
                let at = col_at(&self.doc.text(index)?[span.clone()], text);
                Some(Hit {
                    part: 0,
                    off: span.start + at,
                })
            }
        }
    }

    /// What was drawn, which is what a copy takes: the flowed grid when this
    /// width re-laid one out, the line's own text when it did not — the
    /// markers are off the text by then, so a copy yields the rendered row
    /// rather than a bullet nobody can see.
    fn selectable(&self, index: usize, part: u16) -> Option<&str> {
        if part != 0 {
            return None;
        }
        self.doc.text(index)
    }

    fn report(&self) -> String {
        self.doc.report()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::Diff;
    use crate::rows::{Layouts, TextRows};
    use crate::screen::Screen;
    use gitten_core::markdown::Block;
    use gitten_core::parse_unified_diff;
    use gitten_core::prepared::prepare;
    use gitten_core::syntax::Kind;
    use gitten_core::theme::Surface;

    /// The committed slice: small enough to review in full, and the
    /// deterministic stand-in for the ignored `fixtures/real/md.diff` corpus —
    /// no test in here may need that file or the network.
    const RAW: &str = include_str!("../tests/fixtures/md.diff");

    /// The whole frontend, headless: the fixture through `Diff`, reflowed and
    /// painted into a grid. No terminal, no raw mode, no window.
    struct H {
        d: Diff,
        host: Host,
        screen: Screen,
        out: Vec<gitten_core::runs::Run>,
    }

    impl H {
        fn new(raw: &str, cols: usize, height: usize) -> Self {
            Self::with_wrap(raw, cols, height, "word")
        }

        fn with_wrap(raw: &str, cols: usize, height: usize, wrap: &str) -> Self {
            let host = Host::new();
            let mut d = Diff::new(parse_unified_diff(raw), &host);
            d.set_wrap(host.wrap.position(wrap).expect("a shipped wrap"), &host);
            d.resize(cols, height, &host);
            let screen = Screen::new(cols, height);
            let out = Vec::new();
            let mut h = Self {
                d,
                host,
                screen,
                out,
            };
            h.paint();
            h
        }

        fn paint(&mut self) {
            self.d.paint(&mut self.screen, 0, &self.host, &mut self.out);
        }

        fn rows(&self) -> Vec<String> {
            (0..self.d.rows())
                .map(|y| self.screen.row_text(y))
                .collect()
        }

        /// A drag to a body cell, the way the mouse does it: the press above
        /// set the anchor, this extends to the pointer, and the release copies.
        fn drag_to(&mut self, col: usize, row: usize) {
            self.d.drag(col, row as isize, &self.host);
        }
    }

    #[test]
    fn builtin_registry_claims_markdown_only_in_unified() {
        let host = Host::new();
        let layouts = Layouts::builtin();
        assert_eq!(layouts.names(), ["unified", "split"]);

        let unified = layouts.build(layouts.position("unified").unwrap(), &host);
        assert_eq!(unified.len(), 2);
        // The generalist keeps everything until the specialist claims it; the
        // claim order is what makes a registered specialist take `.md` without
        // the built-in knowing it exists.
        assert!(unified[0].claims("a.rs"));
        assert!(!unified[1].claims("a.rs"));
        // The built-in claims everything — that is what makes it the fallback
        // — so the specialist wins by *order*, and the assembled diff shows
        // it: the `.md` files land in owner 1, the `.rs` in owner 0.
        assert!(unified[0].claims("a.md"), "the built-in is the fallback");
        for p in ["a.md", "docs/b.markdown", "x/y/z.mdx"] {
            assert!(unified[1].claims(p), "{p} was not claimed");
        }
        for p in ["a.rs", "Cargo.lock", "no-extension", "md"] {
            assert!(!unified[1].claims(p), "{p}");
        }
        let mut owners = layouts.build(layouts.position("unified").unwrap(), &host);
        let a = crate::rows::assemble(&parse_unified_diff(RAW), &host, &mut owners);
        // Every logical row was handed to exactly one presentation, once.
        assert_eq!(
            a.ordered.len(),
            owners.iter().map(|o| o.len()).sum::<usize>(),
            "a row was claimed twice or dropped"
        );
        // Both owners took rows: the `.rs` file is the generalist's, the `.md`
        // the specialist's.
        assert!(
            owners.iter().all(|o| o.len() > 0),
            "a presentation took nothing"
        );

        // And `split` stays source-only: a rendered document in a 44-character
        // column is worse than its source, and both clients agree about it.
        let split = layouts.build(layouts.position("split").unwrap(), &host);
        assert_eq!(split.len(), 1, "split grew a presentation");

        // The names are the same ones `gitten.toml` opens and `s` cycles.
        let mut d = Diff::new(parse_unified_diff(RAW), &host);
        assert_eq!(d.layout_name(), "unified");
        d.cycle_layout(&host);
        assert_eq!(d.layout_name(), "split");
        d.cycle_layout(&host);
        assert_eq!(d.layout_name(), "unified");
    }

    #[test]
    fn wrapping_adds_visual_rows_and_never_logical_ones() {
        // A presentation may add visual rows and may not add or drop logical
        // ones: the gutter's line numbers have to keep adding up.
        let h = H::new(RAW, 60, 96);
        let host = Host::new();
        let mut owners: Vec<Box<dyn Rows>> = vec![Box::new(TextRows::default())];
        let a = crate::rows::assemble(&parse_unified_diff(RAW), &host, &mut owners);
        assert_eq!(
            a.ordered.len(),
            owners[0].len(),
            "the baseline is one row a line"
        );
        assert_eq!(
            a.ordered.len(),
            owners[0].len(),
            "the built-in is the baseline"
        );
        assert!(
            h.d.rows() > a.ordered.len(),
            "nothing wrapped at 60 columns"
        );
        // Two files, both headers where the jump list says.
        assert_eq!(h.d.headers().len(), 2);
    }

    #[test]
    fn markdown_frame_matches_the_cell_golden() {
        let h = H::new(RAW, 60, 96);
        let rows = h.rows();
        let theme = &h.host.theme;
        let md = &theme.markdown;

        // The `.md` file header and the hunk header, drawn by the built-in's
        // own functions: a document is still a file.
        assert!(rows[0].starts_with(" README.md"), "{:?}", rows[0]);
        assert!(rows[1].starts_with(" @@"), "{:?}", rows[1]);

        // The ATX heading: hashes gone, bold, one row.
        assert_eq!(rows[2], " 1  1   gitten", "{:?}", rows[2]);
        let gitten = rows[2].find("gitten").unwrap();
        assert!(
            h.screen.ink(gitten, 2).unwrap().bold,
            "the heading was not bold"
        );

        // The blank under the heading is a row with a gutter and nothing else.
        assert_eq!(rows[3], " 2  2", "{:?}", rows[3]);

        // The removed prose line: markers and the url gone, the changed word
        // lit on the removal's own word background.
        let removed = &rows[4];
        assert!(removed.starts_with(" 3    - "), "{removed:?}");
        assert_eq!(
            &removed[8..],
            "A terminal diff view with bold claims and a link."
        );
        let y = 4;
        let x = (0..60)
            .find(|x| {
                h.screen.char_at(*x, y) == Some('b')
                    && h.screen.char_at(x + 1, y) == Some('o')
                    && h.screen.char_at(x + 2, y) == Some('l')
                    && h.screen.char_at(x + 3, y) == Some('d')
                    && h.screen.char_at(x + 4, y) == Some(' ')
            })
            .expect("the unmarked word bold");
        assert_eq!(
            h.screen.ink(x, y).unwrap().bg,
            theme.background(Surface::RemovedWord),
            "the changed word lost its background"
        );

        // The added prose line, with its own changed word, on the addition.
        assert!(rows[5].starts_with("    3 + "), "{:?}", rows[5]);
        assert!(
            rows[5].contains("bolder claims and a link"),
            "{:?}",
            rows[5]
        );

        // The blank that follows is a row with a gutter and nothing else.
        assert_eq!(rows[6], " 4  4", "{:?}", rows[6]);

        // Emphasis: the single-asterisk word loses its markers and draws in
        // its own kind's colour and slant, resolved against the addition it
        // sits on — the one token the strong/link pair does not cover.
        let emph = rows
            .iter()
            .position(|r| r.contains("gently"))
            .expect("the emphasis row");
        assert!(
            !rows[emph].contains('*'),
            "an emphasis marker survived: {:?}",
            rows[emph]
        );
        let x = rows[emph].find("gently").unwrap();
        let expected = theme.syntax_on(Kind::Emphasis, Surface::Added);
        let ink = h.screen.ink(x, emph).unwrap();
        assert_eq!(ink.fg, expected.fg, "the emphasis drew in the body colour");
        assert_eq!(ink.italic, expected.italic, "the emphasis lost its slant");

        // The indented `#` command is prose, not a heading: not bold, its
        // indent kept, and the trailing comment where the source had it.
        let command = rows
            .iter()
            .find(|r| r.contains("./dev.sh diff"))
            .expect("the command line");
        assert!(
            command.contains("./dev.sh diff        # rebuild on every save"),
            "{command:?}"
        );
        let at = command.find("./dev.sh").unwrap();
        let y = rows.iter().position(|r| r.contains("./dev.sh")).unwrap();
        assert!(
            !h.screen.ink(at, y).unwrap().bold,
            "an indented command drew as a heading"
        );

        // A bullet: marker gone, the glyph in the marker's colour, and the
        // added side drawn beside its own sign.
        let bullets: Vec<&String> = rows.iter().filter(|r| r.contains("first")).collect();
        assert_eq!(bullets.len(), 2, "{rows:?}");
        assert!(bullets[0].contains(" 8    - • first"), "{:?}", bullets[0]);
        assert!(bullets[1].contains("   10 + • first"), "{:?}", bullets[1]);

        // The nested bullet wrapped: its continuation carries no number and no
        // sign, keeps the indent and the bullet slot, and draws no glyph.
        let nested = rows
            .iter()
            .position(|r| r.contains("◦ nested"))
            .expect("the nested bullet");
        let cont = &rows[nested + 1];
        assert!(cont.starts_with("        "), "no blank gutter: {cont:?}");
        assert!(cont.contains("wrap at any"), "{cont:?}");
        assert!(!cont.contains('•'), "a wrapped bullet drew twice: {cont:?}");

        // A quote keeps its bar, one cell of the quote bar's own colour in
        // front of the text.
        let quoted = rows
            .iter()
            .position(|r| r.contains("quoted prose"))
            .expect("the quote");
        let x = 8;
        assert_eq!(
            h.screen.ink(x, quoted).unwrap().bg,
            md.quote_bar,
            "the quote bar was not drawn"
        );

        // A fence: the language label in the marker colour, the body verbatim,
        // the closing fence an empty row.
        let fence = rows
            .iter()
            .position(|r| r.ends_with("rust") && r.contains('+'))
            .expect("the fence label");
        assert!(rows[fence].ends_with(" rust"), "{:?}", rows[fence]);
        let body = rows
            .iter()
            .position(|r| r.contains("table_flow"))
            .expect("the code body");
        assert!(
            rows[body].contains("squeeze(&grid.widths, cols);"),
            "{:?}",
            rows[body]
        );

        // The thematic rule draws a band and not its dashes.
        // Two-digit numbers fill their gutter exactly, so no leading pad.
        let rule = rows
            .iter()
            .position(|r| r.starts_with("17 19"))
            .expect("the rule");
        assert_eq!(rows[rule], "17 19", "{:?}", rows[rule]);
        assert_eq!(h.screen.ink(8, rule).unwrap().bg, md.rule, "no band");

        // The table: one grid, every row's pipes in the same columns, the wide
        // cell squeezed into sub-rows that still line up.
        let header = rows
            .iter()
            .position(|r| r.contains("stage"))
            .expect("the table header");
        let grid: Vec<&String> = rows[header..header + 6].iter().collect();
        // Char positions, not bytes: the box drawing is three bytes a glyph,
        // and the fill between two pipes is dashes in one row and spaces in
        // another, so byte offsets would call a straight grid sheared.
        let pipes = |t: &str| -> Vec<usize> {
            t.chars()
                .enumerate()
                .filter(|(_, c)| "│├┤┼".contains(*c))
                .map(|(i, _)| i)
                .collect()
        };
        let first = pipes(grid[0]);
        assert_eq!(first.len(), 3, "two columns is three boundaries");
        for (k, r) in grid.iter().enumerate() {
            assert_eq!(pipes(r), first, "row {k} sheared the grid: {r:?}");
        }
        // The separator row is the grid's own rule, in the rule colour.
        let sep = grid.iter().position(|r| r.contains('├')).unwrap();
        let x = grid[sep].find('├').unwrap();
        assert_eq!(h.screen.ink(x, header + sep).unwrap().fg, md.rule);

        // The hairline: under the last segment of the data row that has
        // another one under it, on the cells the grid occupies, and nowhere
        // else — and never as a fabricated row, which would leave a box-drawn
        // line in a gutter whose numbers have to keep adding up.
        let ruled = (header..header + 6)
            .filter(|y| (0..60).any(|x| h.screen.ink(x, *y).unwrap().underline))
            .count();
        assert_eq!(ruled, 1, "exactly one boundary between the data rows");
        let ruled_y = (header..header + 6)
            .find(|y| (0..60).any(|x| h.screen.ink(x, *y).unwrap().underline))
            .unwrap();
        let ruled_x: Vec<usize> = (0..60)
            .filter(|x| h.screen.ink(*x, ruled_y).unwrap().underline)
            .collect();
        assert_eq!(
            *ruled_x.first().unwrap(),
            8,
            "the rule started in the gutter"
        );
        assert!(
            *ruled_x.last().unwrap() < 60,
            "the rule ran to the edge of the window"
        );
        for r in &rows {
            assert!(
                !r.starts_with("        ├"),
                "a hairline was drawn as a row: {r:?}"
            );
        }
    }

    #[test]
    fn fenced_code_scrolls_under_fixed_furniture() {
        // With wrapping off and the text scrolled, the gutter, the sign and
        // the code bar stay where they were and the text moves under them.
        let mut h = H::with_wrap(RAW, 60, 96, "off");
        h.paint();
        let rest_body = h
            .rows()
            .iter()
            .position(|r| r.contains("table_flow"))
            .expect("the code body");

        h.d.scroll_x(4);
        h.paint();
        let scrolled = h.rows();
        let moved_body = scrolled
            .iter()
            .position(|r| r.contains("table_flow"))
            .expect("the code body, scrolled");
        assert_eq!(rest_body, moved_body, "the body moved rows");
        // `Pen::scroll` swallows columns only after it is called, so the four
        // scrolled off are the text's first four — the `let` is gone.
        assert!(
            scrolled[moved_body].contains("table_flow"),
            "{:?}",
            scrolled[moved_body]
        );
        assert!(
            !scrolled[moved_body].contains("let "),
            "{:?}",
            scrolled[moved_body]
        );
        // The row above is the fence label — exactly four columns wide, so
        // the whole label is off and what the row still shows is its bar.
        assert_eq!(
            scrolled[moved_body - 1],
            "   15 +",
            "{:?}",
            scrolled[moved_body - 1]
        );
        assert_eq!(
            h.screen.ink(8, moved_body - 1).unwrap().bg,
            h.host.theme.markdown.code_bar,
            "the bar moved with the text"
        );

        // With word wrap on, the budget is net of the bar: two rows of the
        // same text length, one a code line and one prose, wrap differently,
        // because the code line's bar costs it two of its columns.
        let para = "one two three four five six seven eight nine";
        assert_eq!(para.chars().count(), 44);
        let code = format!("let a = 1; {}", &para[11..]);
        assert_eq!(code.chars().count(), 44, "the pair stopped being equal");
        // The prose is context *before* the added fence: a fence that opens
        // on the added side and never closes would swallow the context line
        // under it, the same way a real half-shown hunk does.
        let src = format!("diff --git a/a.md b/a.md\n@@ -1,1 +1,3 @@\n {para}\n+```\n+{code}\n");
        let host = Host::new();
        let mut p = prepare(&parse_unified_diff(&src), &host.syntax, 2000);
        let mut md = MarkdownRows::default();
        let mut files = p.files.drain(..).collect::<Vec<_>>();
        for f in files.drain(..) {
            md.build(f);
        }
        md.reflow(52, &host, host.wrap.current());
        let code_row = (0..md.doc.len())
            .find(|i| md.doc.block(*i) == Some(Block::Code))
            .expect("a code row");
        let prose_row = (0..md.doc.len())
            .find(|i| md.doc.block(*i) == Some(Block::Paragraph))
            .expect("a prose row");
        assert_eq!(md.rows(prose_row), 1, "the prose row wrapped first");
        assert_eq!(
            md.rows(code_row),
            2,
            "the bar came out of the text budget for free"
        );
    }

    #[test]
    fn selection_hits_transformed_and_flowed_text() {
        let mut h = H::with_wrap(RAW, 44, 96, "word");
        let rows = h.rows();

        // A drag across the bullet pair: the markers are gone from the text,
        // so the copy is what was on the screen and nothing else.
        let removed = rows.iter().position(|r| r.contains("- • first")).unwrap();
        let added = rows.iter().position(|r| r.contains("+ • first")).unwrap();
        let col = rows[removed].find("•").unwrap() + 2;
        h.d.press(col, removed, 1, false, &h.host);
        h.drag_to(col + 4, added);
        h.d.release();
        let copied = h.d.selection();
        assert_eq!(
            copied.lines().count(),
            2,
            "a drag over two bullets copied {copied:?}"
        );
        assert!(
            !copied.contains("-- first") && !copied.contains("+- first"),
            "{copied:?}"
        );
        assert!(copied.lines().all(|l| l.trim() == "first"), "{copied:?}");

        // The flowed table: a drag from its header to its last row copies the
        // rows that were drawn, exactly once each — the wide row's sub-rows
        // with the newlines the screen showed, and no source row twice.
        let header = rows.iter().position(|r| r.contains("stage")).unwrap();
        // `find` is a byte offset and a cell is a column; the grid's box
        // drawing is three bytes a glyph, so the one is not the other.
        let at = rows[header].find("stage").unwrap();
        let col = screen::width(&rows[header][..at]);
        h.d.press(col, header, 1, false, &h.host);
        let last = (header..h.d.rows())
            .find(|y| rows[*y].contains("301 ms"))
            .unwrap();
        // Past the end of the last row: a drag that reaches the row's end
        // takes all of it, which is what a drag down a table does.
        h.drag_to(43, last);
        h.d.release();
        let copied = h.d.selection();
        for what in ["stage", "├", "the log,", "301 ms"] {
            assert_eq!(
                copied.matches(what).count(),
                1,
                "{what:?} copied {} times: {copied:?}",
                copied.matches(what).count()
            );
        }
        for (n, line) in copied.lines().enumerate() {
            assert!(
                rows.iter().any(|r| r.contains(line.trim())),
                "line {n} ({line:?}) was never on the screen"
            );
        }
        // And what the drag selected is what `copy_text` yields, once.
        assert_eq!(h.d.copy_text(), copied);
    }

    #[test]
    fn a_caret_in_the_gutter_is_the_start_of_the_row_s_text() {
        let host = Host::new();
        let mut p = prepare(&parse_unified_diff(RAW), &host.syntax, 2000);
        let mut md = MarkdownRows::default();
        let mut files = p.files.drain(..).collect::<Vec<_>>();
        let mut claimed: Vec<_> = files.drain(..).filter(|f| md.claims(&f.path)).collect();
        for f in claimed.drain(..) {
            md.build(f);
        }
        md.reflow(60, &host, host.wrap.current());

        let heading = (0..md.doc.len())
            .find(|i| md.doc.block(*i) == Some(Block::Heading(1)))
            .expect("a heading");
        // A click in the gutter is byte 0 of the row's text; past the end of a
        // short row is the end of it.
        assert_eq!(md.hit(heading, 0, 1, 0).unwrap().off, 0);
        let text = md.selectable(heading, 0).unwrap();
        assert_eq!(md.hit(heading, 0, 99, 0).unwrap().off, text.len());

        // A click at the start of a bullet's own text is byte 0, whatever the
        // bullet and its slot put in front of it — and the same columns the
        // budget was measured against are the ones the caret subtracts.
        let b = (0..md.doc.len())
            .find(|i| md.doc.block(*i) == Some(Block::Bullet(0)))
            .expect("a bullet");
        let from = md.chrome() + furniture(Block::Bullet(0));
        assert_eq!(md.hit(b, 0, from, 0).unwrap().off, 0);
        assert_eq!(md.hit(b, 0, from, 3).unwrap().off, 3);
        // In the furniture, scrolled: the first character there is to see.
        assert_eq!(md.hit(b, 0, 0, 3).unwrap().off, 3);
        // And the flowed grid, when there is one, is what the caret indexes:
        // every segment's hit lands inside the piece it was clicked on.
        let table: Vec<usize> = (0..md.doc.len())
            .filter(|i| md.doc.block(*i).is_some_and(|b| b.is_table()))
            .collect();
        for i in table {
            for seg in 0..md.rows(i) {
                let span = md.doc.range(i, seg);
                let hit = md.hit(i, seg, 40, 0).unwrap();
                assert!(
                    span.contains(&hit.off) || hit.off == span.end,
                    "row {i}.{seg}: {hit:?} outside {span:?}"
                );
            }
        }
    }
}
