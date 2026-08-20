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
//! row, and a code block cannot have a block background of its own. Anything
//! that genuinely needs to reflow wants a pane, which does not exist — see
//! `docs/decisions/0006-row-seam-without-boxing.md`.
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
    file_header, hunk_header, line_colors, num, number, runs, Rows, ROW_H,
};
use gpui::*;
use plait_core::host::Host;
use plait_core::markdown::{lay_out, Block};
use plait_core::syntax::Token;
use plait_core::theme::{Rgb, Theme};
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
}

impl Default for Metrics {
    fn default() -> Self {
        Self {
            heading: [18.0, 16.5, 15.0, 14.0, 14.0, 14.0],
            indent: 14.0,
            bar: 2.0,
            bullets: &["•", "◦", "▪", "·"],
        }
    }
}

impl Metrics {
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
            let blocks = lay_out(&mut h.lines);
            self.laid_out += blocks.len();
            for (l, block) in h.lines.into_iter().zip(blocks) {
                self.rows.push(Row::Line {
                    block,
                    kind: l.kind,
                    old: number(l.old_no),
                    new: number(l.new_no),
                    text: l.text.into(),
                    spans: l.spans,
                    tokens: l.tokens,
                });
            }
        }
    }

    fn width(&self, index: usize) -> usize {
        match &self.rows[index] {
            // Indent steps cost roughly a character each, and a heading's glyphs
            // are wider than the body's — both approximations, and both only feed
            // the one row `uniform_list` measures to size its scroll width.
            Row::Line { text, block, .. } => {
                let scale = match block {
                    Block::Heading(l) => self.metrics.size(*l) / 14.0,
                    _ => 1.0,
                };
                (text.len() as f32 * scale) as usize + block.depth() as usize + 2
            }
            Row::Hunk(h) => h.len(),
            Row::File { path, .. } => path.len(),
        }
    }

    fn report(&self) -> String {
        if self.laid_out == 0 {
            String::new()
        } else {
            format!("markdown {} rows", self.laid_out)
        }
    }

    fn render(&self, index: usize, host: &Host) -> AnyElement {
        let theme = &host.theme;
        match &self.rows[index] {
            Row::File { path, adds, dels } => file_header(path, *adds, *dels, theme),
            Row::Hunk(header) => hunk_header(header, theme),
            Row::Line { block, kind, old, new, text, spans, tokens } => {
                self.line(*block, *kind, old, new, text, spans, tokens, theme)
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
        old: &SharedString,
        new: &SharedString,
        text: &SharedString,
        spans: &[Span],
        tokens: &[Token],
        theme: &Theme,
    ) -> AnyElement {
        let m = &self.metrics;
        let md = &theme.markdown;
        let (bg, fg, sign) = line_colors(kind, &theme.diff);

        // The gutter is the built-in's, unchanged. Whatever the row does with the
        // text, the two line numbers and the sign have to sit where they sit on
        // every other row of the diff or the eye loses the column.
        let row = div()
            .flex()
            .items_center()
            .h(px(ROW_H))
            .px_4()
            .bg(rgb(bg))
            .child(num(old.clone(), theme.diff.gutter_fg))
            .child(num(new.clone(), theme.diff.gutter_fg))
            .child(div().flex_none().w(px(16.)).text_color(rgb(fg)).child(sign));

        // A rule draws no text: the dashes were the drawing, so they are replaced
        // by the thing they were drawing.
        if block == Block::Rule {
            return row
                .child(
                    div()
                        .flex_none()
                        .w(px(320.))
                        .h(px(1.))
                        .bg(rgb(md.rule))
                        .ml(px(m.indent))
                        .into_any_element(),
                )
                .into_any_element();
        }
        if block == Block::Blank {
            return row.into_any_element();
        }

        let bar = |color: Rgb| {
            div().flex_none().w(px(m.bar)).h(px(ROW_H - 6.0)).mr(px(m.indent - m.bar)).bg(rgb(color))
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
            Block::Bullet(d) => row.child(
                div()
                    .flex_none()
                    .w(px(m.indent))
                    .text_color(rgb(md.marker))
                    .child(m.bullet(d)),
            ),
            _ => row,
        };

        // An empty fence line is a bare ``` with no language: the bar beside it
        // already says a block opened, so there is nothing left to draw.
        if text.is_empty() {
            return row.into_any_element();
        }

        let body = div().flex_none().text_color(rgb(fg)).child(
            StyledText::new(text.clone()).with_highlights(runs(text, tokens, spans, theme, kind)),
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
    use super::{MarkdownRows, Metrics};
    use crate::views::diff::{Diff, Rows, TextRows};
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
        assert!((0..r.len()).all(|i| r.width(i) > 0));
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

    #[test]
    fn a_file_with_no_hunks_still_produces_its_header() {
        let r = built("diff --git a/a.md b/a.md\n");
        assert_eq!(r.len(), 1);
        assert!(r.width(0) > 0);
    }
}
