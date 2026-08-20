//! The diff view.
//!
//! Everything is flattened to a uniform row list up front — file headers, hunk
//! headers and lines all the same height — so the whole thing virtualizes
//! through one `uniform_list` regardless of how large the diff is.
//!
//! Word-level spans come from `plait_core::intraline` and syntax tokens from
//! whichever `Highlighter` the host routed the file to, both computed once at
//! load. Nothing here re-diffs, re-lexes or re-merges during render or scroll.
//!
//! A row is one `StyledText`, not a box per span. Syntax highlighting puts about
//! five tokens on an average line and intraline diffing adds more; as separate
//! elements that is ten boxes a row to lay out and shape, where a run list is
//! one shaped line with colours applied to byte ranges.
//!
//! Rows themselves come from a [`Rows`] implementation chosen per file, so the
//! presentation of a `.md` or a `.png` is a new implementation rather than
//! another arm of a match in here. [`TextRows`] is the built-in one, and it
//! claims every path, which is what makes it the fallback.

use gpui::*;
use gpui_component::scroll::Scrollbar;
use plait_core::host::Host;
use plait_core::prepared::{prepare, Prepared};
use plait_core::syntax::Token;
use plait_core::theme::{DiffPalette, Rgb, Surface, Theme};
use plait_core::{FileDiff, LineKind, Span};
use std::cell::Cell;
use std::ops::Range;
use std::rc::Rc;

pub(crate) const ROW_H: f32 = 22.0;
const GUTTER_W: f32 = 52.0;

/// How wide a row may get before it is clipped — a rendering budget, which is
/// why it is the frontend that owns the number and `core` that applies it. Text
/// layout is linear in length and a 9.6-million-character line was measured in
/// the wild; nobody reads past column 2000 either way.
const MAX_LINE_CHARS: usize = 2000;

// ------------------------------------------------------------------ the seam

/// Turns one file's diff into rows, and draws them.
///
/// Row height is fixed for the whole list because `uniform_list` is what makes a
/// 700k-row diff scroll at all, so an implementation may draw anything it likes
/// within [`ROW_H`] but cannot ask for more. A presentation that genuinely needs
/// variable height — a rendered Markdown preview, an image diff — is a different
/// plug point: its own view in its own pane, not a row in this list.
pub trait Rows {
    /// Whether this implementation wants the file. The built-in claims
    /// everything; the last registered claimant wins, so a specialist can take
    /// `.md` without the generalist having to know it exists.
    fn claims(&self, path: &str) -> bool;

    /// How many rows this implementation currently holds. The list uses it to
    /// address the rows `build` is about to append.
    fn len(&self) -> usize;

    /// Appends the rows for `file`, which arrives clipped, intraline-diffed and
    /// highlighted — see `plait_core::prepared`. An implementation draws; it does
    /// not redo any of that.
    fn build(&mut self, file: plait_core::prepared::File);

    fn render(&self, index: usize, host: &Host) -> AnyElement;

    /// Width of a row in characters, for `uniform_list`'s one measured row.
    fn width(&self, index: usize) -> usize;

    /// Whatever this implementation wants to say on the stats overlay.
    fn report(&self) -> String {
        String::new()
    }
}

/// 8 bytes per row: which implementation owns it, and where in that
/// implementation's own storage it sits. The rows themselves are never boxed —
/// at 700k rows that would be 700k allocations to chase on every scroll.
#[derive(Clone, Copy)]
struct RowRef {
    owner: u16,
    index: u32,
}

pub struct Diff {
    renderers: Rc<Vec<Box<dyn Rows>>>,
    order: Rc<Vec<RowRef>>,
    host: Rc<Host>,
    /// See the note in the commits view: uniform_list sizes its scrollable
    /// width from a single measured row, defaulting to row 0.
    widest: usize,
    scroll: UniformListScrollHandle,
    pub rendered: Rc<Cell<usize>>,
    pub load: String,
}

impl Diff {
    pub fn total(&self) -> usize {
        self.order.len()
    }

    /// The shipped set: the built-in text presentation, plus the rendered
    /// Markdown one registered on top of it through the same call an extension
    /// would use. The same argument as `Highlighters::builtin` routing Markdown
    /// away from the scanner — if a built-in does not go through the seam, the
    /// seam is untested.
    pub fn new(files: Vec<FileDiff>, host: Rc<Host>) -> Self {
        Self::with_renderers(
            files,
            host,
            vec![
                Box::new(TextRows::default()),
                Box::new(super::markdown::MarkdownRows::default()),
            ],
        )
    }

    /// `renderers[0]` is the fallback and must claim every path; later entries
    /// are specialists and win over earlier ones.
    pub fn with_renderers(
        files: Vec<FileDiff>,
        host: Rc<Host>,
        mut renderers: Vec<Box<dyn Rows>>,
    ) -> Self {
        let t = std::time::Instant::now();
        let mut order: Vec<RowRef> = Vec::new();

        // One pass in core, shared with the CLI and the ANSI painter, before any
        // renderer sees a row.
        let Prepared { files, intraline, syntax } =
            prepare(&files, &host.syntax, MAX_LINE_CHARS);
        let file_count = files.len();

        for f in files {
            let owner = renderers
                .iter()
                .enumerate()
                .rev()
                .find(|(_, r)| r.claims(&f.path))
                .map_or(0, |(i, _)| i);
            let r = &mut renderers[owner];
            let first = r.len();
            r.build(f);
            for index in first..r.len() {
                order.push(RowRef { owner: owner as u16, index: index as u32 });
            }
        }

        let widest = order
            .iter()
            .enumerate()
            .max_by_key(|(_, r)| renderers[r.owner as usize].width(r.index as usize))
            .map_or(0, |(i, _)| i);

        let mut reports: Vec<String> =
            vec![format!("intraline {intraline:.0?} · syntax {syntax:.0?}")];
        reports.extend(renderers.iter().map(|r| r.report()).filter(|s| !s.is_empty()));
        let load = format!(
            "{} rows · {} files · build {:.0?} ({})",
            order.len(),
            file_count,
            t.elapsed(),
            reports.join(" · "),
        );
        eprintln!("{load}");

        Self {
            renderers: Rc::new(renderers),
            order: Rc::new(order),
            host,
            widest,
            scroll: UniformListScrollHandle::new(),
            rendered: Rc::new(Cell::new(0)),
            load,
        }
    }
}

impl Render for Diff {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let renderers = self.renderers.clone();
        let order = self.order.clone();
        let host = self.host.clone();
        let rendered = self.rendered.clone();

        let list = uniform_list("diff", order.len(), move |range, _, _| {
            rendered.set(range.len());
            range
                .map(|i| {
                    let r = order[i];
                    renderers[r.owner as usize].render(r.index as usize, &host)
                })
                .collect()
        })
        .with_width_from_item(Some(self.widest))
        .track_scroll(&self.scroll)
        .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
        .size_full();

        div()
            .relative()
            .size_full()
            .child(list)
            .child(Scrollbar::vertical(&self.scroll))
            .child(Scrollbar::horizontal(&self.scroll))
    }
}

// --------------------------------------------------------------- the built-in

/// `SharedString` throughout, not `String`: `render` runs for every visible row
/// on every frame that redraws, and handing GPUI a `String` there copies the
/// line each time. A `SharedString` clone is a refcount bump.
enum Row {
    File {
        path: SharedString,
        adds: usize,
        dels: usize,
    },
    Hunk(SharedString),
    Line {
        kind: LineKind,
        old: SharedString,
        new: SharedString,
        text: SharedString,
        spans: Vec<Span>,
        tokens: Vec<Token>,
    },
}

/// The default presentation: one line of text per row, behind a line-number
/// gutter, coloured by the host's theme.
#[derive(Default)]
pub struct TextRows {
    rows: Vec<Row>,
}

impl Rows for TextRows {
    fn claims(&self, _path: &str) -> bool {
        true
    }

    fn len(&self) -> usize {
        self.rows.len()
    }

    fn build(&mut self, f: plait_core::prepared::File) {
        self.rows.push(Row::File {
            path: f.path.into(),
            adds: f.adds,
            dels: f.dels,
        });
        for h in f.hunks {
            self.rows.push(Row::Hunk(h.header.into()));
            for l in h.lines {
                self.rows.push(Row::Line {
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
            Row::Line { text, .. } => text.len(),
            Row::Hunk(h) => h.len(),
            Row::File { path, .. } => path.len(),
        }
    }

    fn render(&self, index: usize, host: &Host) -> AnyElement {
        let theme = &host.theme;
        let p = &theme.diff;
        match &self.rows[index] {
            Row::File { path, adds, dels } => file_header(path, *adds, *dels, theme),

            Row::Hunk(header) => hunk_header(header, theme),

            Row::Line { kind, old, new, text, spans, tokens } => {
                let (bg, fg, sign) = line_colors(*kind, p);
                div()
                    .flex()
                    .items_center()
                    .h(px(ROW_H))
                    .px_4()
                    .bg(rgb(bg))
                    .child(num(old.clone(), p.gutter_fg))
                    .child(num(new.clone(), p.gutter_fg))
                    .child(div().flex_none().w(px(16.)).text_color(rgb(fg)).child(sign))
                    .child(
                        div().flex_none().text_color(rgb(fg)).child(
                            StyledText::new(text.clone())
                                .with_highlights(runs(text, tokens, spans, theme, *kind)),
                        ),
                    )
                    .into_any_element()
            }
        }
    }
}

/// A file's header row. Identical whichever presentation owns the lines beneath
/// it — a `.md` file is still a file — so it is drawn here and shared.
pub(crate) fn file_header(
    path: &SharedString,
    adds: usize,
    dels: usize,
    theme: &Theme,
) -> AnyElement {
    let p = &theme.diff;
    div()
        .flex()
        .items_center()
        .gap_3()
        .h(px(ROW_H))
        .px_4()
        .bg(rgb(p.file_bg))
        .child(div().text_color(rgb(p.file_fg)).child(path.clone()))
        .child(div().text_color(rgb(p.adds_fg)).child(format!("+{adds}")))
        .child(div().text_color(rgb(p.dels_fg)).child(format!("-{dels}")))
        .into_any_element()
}

pub(crate) fn hunk_header(header: &SharedString, theme: &Theme) -> AnyElement {
    let p = &theme.diff;
    div()
        .flex()
        .items_center()
        .h(px(ROW_H))
        .px_4()
        .bg(rgb(p.hunk_bg))
        .text_color(rgb(p.hunk_fg))
        .child(header.clone())
        .into_any_element()
}

/// Which background a line of `kind` is drawn on, and the surfaces a token
/// lands on there. Shared so the two presentations cannot drift on what "added"
/// looks like.
pub(crate) fn line_colors(
    kind: LineKind,
    p: &DiffPalette,
) -> (Rgb, Rgb, &'static str) {
    match kind {
        LineKind::Added => (p.added_bg, p.added_fg, "+"),
        LineKind::Removed => (p.removed_bg, p.removed_fg, "-"),
        LineKind::Context => (p.context_bg, p.context_fg, " "),
    }
}

pub(crate) fn num(n: SharedString, fg: Rgb) -> Div {
    div().flex_none().w(px(GUTTER_W)).text_color(rgb(fg)).child(n)
}

/// Line numbers are drawn, so they are formatted once at load rather than on
/// every frame the row is visible.
pub(crate) fn number(n: Option<u32>) -> SharedString {
    n.map(|n| SharedString::from(n.to_string())).unwrap_or_default()
}

/// Merges two independent sets of byte ranges into the one flat, sorted,
/// non-overlapping run list `StyledText` wants: syntax tokens style the
/// foreground, intraline spans light the background.
///
/// Both inputs are already sorted and internally non-overlapping, so this is a
/// sweep over their combined edges rather than a sort.
pub(crate) fn runs(
    text: &str,
    tokens: &[Token],
    spans: &[Span],
    theme: &Theme,
    kind: LineKind,
) -> Vec<(Range<usize>, HighlightStyle)> {
    // Which background each run actually lands on, so the theme can hand back a
    // foreground that reads against it. A changed word sits on a lighter
    // background than the rest of its line and needs a different answer.
    let (plain_surface, word_surface) = match kind {
        LineKind::Added => (Surface::Added, Surface::AddedWord),
        LineKind::Removed => (Surface::Removed, Surface::RemovedWord),
        LineKind::Context => (Surface::Context, Surface::Context),
    };
    let word_bg = theme.background(word_surface);
    if tokens.is_empty() && spans.is_empty() {
        return Vec::new();
    }

    let mut edges = Vec::with_capacity((tokens.len() + spans.len()) * 2 + 1);
    for t in tokens {
        edges.push(t.start);
        edges.push(t.end);
    }
    for s in spans {
        edges.push(s.start);
        edges.push(s.end);
    }
    edges.push(text.len());
    edges.sort_unstable();
    edges.dedup();

    let mut out = Vec::with_capacity(edges.len());
    let (mut ti, mut si) = (0usize, 0usize);
    let mut cursor = edges[0];
    for &edge in &edges[1..] {
        while ti < tokens.len() && tokens[ti].end <= cursor {
            ti += 1;
        }
        while si < spans.len() && spans[si].end <= cursor {
            si += 1;
        }
        let on_word = spans.get(si).is_some_and(|s| s.start <= cursor);
        let surface = if on_word { word_surface } else { plain_surface };
        let style =
            tokens.get(ti).filter(|t| t.start <= cursor).map(|t| theme.syntax_on(t.kind, surface));
        let bg = on_word.then(|| rgb(word_bg).into());
        if style.is_some() || bg.is_some() {
            out.push((
                cursor..edge,
                HighlightStyle {
                    color: style.map(|s| rgb(s.fg).into()),
                    background_color: bg,
                    font_weight: style.filter(|s| s.bold).map(|_| FontWeight::BOLD),
                    font_style: style.filter(|s| s.italic).map(|_| FontStyle::Italic),
                    ..Default::default()
                },
            ));
        }
        cursor = edge;
    }
    out
}

#[cfg(test)]
mod tests {
    // By name, not a glob: `use gpui::*` in the parent shadows `#[test]` with
    // GPUI's own attribute macro and every test in here fails to expand.
    use super::{runs, Diff, Rows, TextRows};
    use gpui::{div, AnyElement, FontStyle, FontWeight, HighlightStyle, IntoElement, ParentElement};
    use plait_core::host::Host;
    use plait_core::syntax::{Kind, Token};
    use plait_core::theme::{Style, Theme};
    use plait_core::prepared::{prepare, File as PreparedFile};
    use plait_core::{parse_unified_diff, LineKind, Span};
    use std::rc::Rc;

    fn tok(start: usize, end: usize, kind: Kind) -> Token {
        Token { start, end, kind }
    }

    fn well_formed(text: &str, runs: &[(std::ops::Range<usize>, HighlightStyle)]) {
        assert!(runs.windows(2).all(|w| w[0].0.end <= w[1].0.start), "overlapping: {runs:?}");
        for (r, _) in runs {
            assert!(r.start < r.end && r.end <= text.len(), "{r:?} outside {text:?}");
            assert!(text.is_char_boundary(r.start) && text.is_char_boundary(r.end), "{r:?}");
        }
    }

    #[test]
    fn plain_text_produces_no_runs_at_all() {
        let theme = Theme::default_dark();
        assert!(runs("nothing here", &[], &[], &theme, LineKind::Context).is_empty());
    }

    #[test]
    fn a_token_and_a_span_over_the_same_bytes_split_into_both() {
        // `let` is a keyword and also a changed word: one run carrying a
        // foreground and a background, not two elements fighting over it.
        let theme = Theme::default_dark();
        let text = "let x = 1;";
        let out =
            runs(text, &[tok(0, 3, Kind::Keyword)], &[Span { start: 0, end: 3 }], &theme, LineKind::Added);
        well_formed(text, &out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, 0..3);
        assert!(out[0].1.color.is_some() && out[0].1.background_color.is_some());
    }

    #[test]
    fn a_span_crossing_a_token_edge_splits_at_the_edge() {
        //  text:  let x = 1;
        //  token: ###          keyword
        //  span:    #####      changed
        let theme = Theme::default_dark();
        let text = "let x = 1;";
        let out =
            runs(text, &[tok(0, 3, Kind::Keyword)], &[Span { start: 2, end: 7 }], &theme, LineKind::Added);
        well_formed(text, &out);
        let shape: Vec<_> = out
            .iter()
            .map(|(r, s)| (r.clone(), s.color.is_some(), s.background_color.is_some()))
            .collect();
        assert_eq!(shape, vec![(0..2, true, false), (2..3, true, true), (3..7, false, true)]);
    }

    #[test]
    fn many_tokens_and_spans_stay_sorted_and_disjoint() {
        let theme = Theme::default_dark();
        let text = "fn draw(&self) { self.paint(1); } // later";
        let tokens = vec![
            tok(0, 2, Kind::Keyword),
            tok(3, 7, Kind::Func),
            tok(9, 13, Kind::Keyword),
            tok(22, 27, Kind::Func),
            tok(28, 29, Kind::Number),
            tok(34, 42, Kind::Comment),
        ];
        let spans = vec![Span { start: 3, end: 12 }, Span { start: 28, end: 30 }];
        let out = runs(text, &tokens, &spans, &theme, LineKind::Removed);
        well_formed(text, &out);
        assert!(out
            .iter()
            .any(|(r, s)| *r == (28..29) && s.color.is_some() && s.background_color.is_some()));
    }

    #[test]
    fn multi_byte_text_keeps_its_boundaries() {
        let theme = Theme::default_dark();
        let text = "let s = \"café 😀\";";
        let quote = text.find('"').unwrap();
        let out = runs(
            text,
            &[tok(0, 3, Kind::Keyword), tok(quote, text.len() - 1, Kind::Str)],
            &[Span { start: quote, end: text.len() - 1 }],
            &theme,
            LineKind::Added,
        );
        well_formed(text, &out);
    }

    #[test]
    fn weight_and_slant_reach_the_run_list() {
        // A Markdown `**word**` that only changed colour would be wrong, so the
        // theme's bold and italic have to survive the merge.
        let mut theme = Theme::default_dark();
        theme.set_syntax(Kind::Strong, Style::fg(0xffffff).bold());
        theme.set_syntax(Kind::Emphasis, Style::fg(0xcccccc).italic());
        let text = "**bold** and *thin*";
        let out = runs(
            text,
            &[tok(0, 8, Kind::Strong), tok(13, 19, Kind::Emphasis)],
            &[],
            &theme,
            LineKind::Context,
        );
        well_formed(text, &out);
        assert_eq!(out[0].1.font_weight, Some(FontWeight::BOLD));
        assert_eq!(out[0].1.font_style, None);
        assert_eq!(out[1].1.font_style, Some(FontStyle::Italic));
        assert_eq!(out[1].1.font_weight, None);
    }

    #[test]
    fn a_comment_on_a_changed_word_is_lifted_off_the_background() {
        // The regression from a screenshot: a whole rewritten comment line sits
        // under the changed-word background, and the comment grey measured
        // 1.15:1 against it — a smear. The run that lands on the word background
        // must not carry the same foreground as the run that does not.
        let theme = Theme::default_dark();
        let text = "# Collect every check failure before exiting";
        let out = runs(
            text,
            &[tok(0, text.len(), Kind::Comment)],
            &[Span { start: 10, end: text.len() }],
            &theme,
            LineKind::Added,
        );
        well_formed(text, &out);
        let plain = out.iter().find(|(r, _)| r.start == 0).unwrap();
        let on_word = out.iter().find(|(r, _)| r.start == 10).unwrap();
        assert!(on_word.1.background_color.is_some());
        assert_ne!(plain.1.color, on_word.1.color, "same grey on both backgrounds");
    }

    const SAMPLE: &str = "\
diff --git a/a.rs b/a.rs
@@ -1,2 +1,2 @@
 fn main() {
-    let x = 1;
+    let x = 2;
";

    #[test]
    fn the_built_in_renderer_claims_every_path() {
        let r = TextRows::default();
        for p in ["a.rs", "b.md", "no-extension", "weird.xyz"] {
            assert!(r.claims(p));
        }
    }

    #[test]
    fn building_a_file_yields_a_row_per_line_plus_the_headers() {
        let host = Host::new();
        let mut p = prepare(&parse_unified_diff(SAMPLE), &host.syntax, 2000);
        let mut r = TextRows::default();
        r.build(p.files.remove(0));
        assert_eq!(r.len(), 2 + 3, "file header, hunk header, three lines");
        // Widths are answered for every row it built.
        assert!((0..r.len()).all(|i| r.width(i) > 0));
    }

    /// A specialist: what a Markdown or an image presentation would look like
    /// from the list's side. One row per hunk line, nothing else.
    #[derive(Default)]
    struct OneLiner {
        rows: Vec<String>,
    }

    impl Rows for OneLiner {
        fn claims(&self, path: &str) -> bool {
            path.ends_with(".md")
        }
        fn len(&self) -> usize {
            self.rows.len()
        }
        fn build(&mut self, file: PreparedFile) {
            self.rows.push(format!("rendered {}", file.path));
        }
        fn width(&self, index: usize) -> usize {
            self.rows[index].len()
        }
        fn render(&self, index: usize, _host: &Host) -> AnyElement {
            div().child(self.rows[index].clone()).into_any_element()
        }
    }

    const TWO_FILES: &str = "\
diff --git a/a.rs b/a.rs
@@ -1,2 +1,2 @@
 fn main() {
-    let x = 1;
+    let x = 2;
diff --git a/b.md b/b.md
@@ -1,1 +1,1 @@
-# old heading
+# new heading
";

    #[test]
    fn a_specialist_renderer_takes_only_the_files_it_claims() {
        let host = Rc::new(Host::new());
        let files = parse_unified_diff(TWO_FILES);
        assert_eq!(files.len(), 2);

        let diff = Diff::with_renderers(
            files,
            host,
            vec![Box::new(TextRows::default()), Box::new(OneLiner::default())],
        );

        // a.rs went to the built-in: file header, hunk header, three lines.
        // b.md went to the specialist, which collapsed it to a single row.
        let by_owner = |o: u16| diff.order.iter().filter(|r| r.owner == o).count();
        assert_eq!(by_owner(0), 5);
        assert_eq!(by_owner(1), 1);
        assert_eq!(diff.total(), 6);
        assert!(diff.load.contains("2 files"));
    }

    #[test]
    fn the_fallback_is_used_when_nobody_claims_a_file() {
        let host = Rc::new(Host::new());
        // Only the specialist is registered beyond the fallback, and it wants
        // nothing here, so every row must land on the built-in.
        let diff = Diff::with_renderers(
            parse_unified_diff(SAMPLE),
            host,
            vec![Box::new(TextRows::default()), Box::new(OneLiner::default())],
        );
        assert!(diff.order.iter().all(|r| r.owner == 0));
    }
}
