//! The seam: one file's diff, turned into rows and drawn into a terminal.
//!
//! [`Rows`] is [`plait_core::rows::Present`] plus a `render`, and that split is
//! the whole design. Claiming files, holding rows, counting how many rows a
//! wrapped line takes — none of that knows what a UI is, so none of it is here;
//! what is here is the part whose type is a pen over a row of cells.
//!
//! The consequence is that this file is the shell's `views/diff.rs` with the
//! pipeline taken out. A presentation that exists in one frontend is a `render`
//! away from existing in the other, which is the test rule 1 actually has to
//! pass.
//!
//! # Layouts
//!
//! A named set of implementations is a [`Layout`], and [`Layouts`] is the
//! registry `s` cycles. Unified and side-by-side are two entries in it, not two
//! branches of a `match`: the second is [`crate::split::SplitRows`] claiming
//! every path in place of [`TextRows`], and nothing in the view knows which is
//! loaded. The registry is here rather than on `Host` for the same structural
//! reason it is shell-side there — an implementation draws, and `core` never
//! knows a UI exists. What *is* on `Host` is the *name* of the one to open in,
//! because a name is data.
//!
//! # One row, drawn once
//!
//! `render` is handed a [`Pen`] over exactly the cells it may write and cannot
//! leave them. No allocation is required to draw a row: the run list comes from
//! [`plait_core::runs::runs`] into a buffer the caller owns and reuses, and the
//! text is sliced out of the line rather than copied. That is the terminal's
//! version of "nothing on the render path allocates per frame".

use crate::screen::{self, Ink, Pen};
use crate::{MAX_LINE_CHARS, MIN_WRAP_COLS};
use plait_core::host::Host;
use plait_core::prepared::File;
use plait_core::rows::{Entry, Flat, Present, Row};
use plait_core::runs::{runs, Run};
use plait_core::theme::{DiffPalette, Rgb, Theme};
use plait_core::wrap::Wrap;
use plait_core::LineKind;

/// Everything a row needs to know beyond which row it is.
///
/// A struct rather than three arguments because it will grow — a mode stack and
/// a selection both belong in it — and because every implementation would
/// otherwise thread the same list through by hand.
pub struct Frame<'a> {
    pub host: &'a Host,
    /// Columns of text scrolled off the left edge.
    ///
    /// Only meaningful with wrapping off; a wrapped line has nothing to the left
    /// of the window by construction. A presentation applies it with
    /// [`Pen::scroll`] *after* drawing its gutter, so the line numbers and the
    /// `+`/`-` stay put while the text moves under them.
    pub shift: usize,
    /// Whether this row is the one the keyboard is on. Drawn as a background
    /// bar in `theme.chrome.selection_bg`.
    pub current: bool,
}

impl<'a> Frame<'a> {
    pub fn new(host: &'a Host) -> Self {
        Self { host, shift: 0, current: false }
    }

    pub fn theme(&self) -> &Theme {
        &self.host.theme
    }
}

/// A presentation of one file's diff, in a terminal.
///
/// Everything above `render` is [`Present`], and defaulted where a presentation
/// might not care: one that never wraps is exactly as long as it was before
/// wrapping existed.
pub trait Rows: Present {
    /// A new width, in **columns**, for everything this presentation draws.
    ///
    /// Returns whether its row expansion changed, which is what tells the view
    /// whether to rebuild its order table — so a resize that does not cross a
    /// column boundary costs two comparisons and nothing else.
    ///
    /// Columns and not pixels, which is the one place this seam differs from the
    /// shell's. The implementation still owns the conversion to a *text* budget,
    /// because it owns the furniture it draws around the text: see
    /// [`TextRows::chrome`].
    fn reflow(&mut self, _cols: usize, _host: &Host, _wrap: &dyn Wrap) -> bool {
        false
    }

    /// Draws visual row `seg` of logical row `index`.
    ///
    /// `out` is a scratch buffer for the run list, owned by the caller and
    /// cleared by [`runs`]. Handing it in rather than allocating one is the
    /// difference between one allocation per visible row per frame and none.
    fn render(&self, index: usize, seg: usize, at: &Frame, pen: &mut Pen, out: &mut Vec<Run>);

    /// Whatever this implementation wants to say on the status line.
    fn report(&self) -> String {
        String::new()
    }
}

// ---------------------------------------------------------------- the layouts

/// A named set of [`Rows`] implementations: one way of presenting a whole diff.
///
/// `build` is a closure rather than a `Vec` because a layout has to be
/// *rebuildable* — switching re-runs the pipeline and hands each implementation
/// its files again, and a `Vec` that has already been consumed cannot be handed
/// anything. It takes the `Host` because a presentation is entitled to depend on
/// the theme and on the wrap registry.
pub struct Layout {
    pub name: &'static str,
    #[allow(clippy::type_complexity)]
    pub build: Box<dyn Fn(&Host) -> Vec<Box<dyn Rows>>>,
}

/// Every presentation the diff view can be in, in the order `s` cycles them.
pub struct Layouts(Vec<Layout>);

impl Default for Layouts {
    fn default() -> Self {
        Self::builtin()
    }
}

impl Layouts {
    /// The two shipped presentations. Both go through [`Layouts::register`], so
    /// the shipped configuration uses the seam rather than going around it.
    pub fn builtin() -> Self {
        let mut l = Self(Vec::new());
        l.register("unified", |_| vec![Box::new(TextRows::default())]);
        l.register("side-by-side", |_| vec![Box::new(crate::split::SplitRows::default())]);
        l
    }

    /// Adds one, replacing any already registered under the same name — so a
    /// built-in can be corrected rather than only added to, which is what makes
    /// this a seam instead of a list.
    pub fn register(
        &mut self,
        name: &'static str,
        build: impl Fn(&Host) -> Vec<Box<dyn Rows>> + 'static,
    ) {
        let layout = Layout { name, build: Box::new(build) };
        match self.0.iter().position(|l| l.name == name) {
            Some(i) => self.0[i] = layout,
            None => self.0.push(layout),
        }
    }

    pub fn names(&self) -> Vec<&'static str> {
        self.0.iter().map(|l| l.name).collect()
    }

    pub fn position(&self, name: &str) -> Option<usize> {
        self.0.iter().position(|l| l.name == name)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn name(&self, index: usize) -> &'static str {
        self.0.get(index).map_or("custom", |l| l.name)
    }

    /// Builds one presentation set by index.
    ///
    /// Never empty: an entry whose builder returns nothing falls back to the
    /// built-in, which claims every path. The alternative is a registry entry
    /// that silently shows no diff at all.
    pub fn build(&self, index: usize, host: &Host) -> Vec<Box<dyn Rows>> {
        let mut built = match self.0.get(index) {
            Some(l) => (l.build)(host),
            None => Vec::new(),
        };
        if built.is_empty() {
            built.push(Box::new(TextRows::default()));
        }
        built
    }
}

// ------------------------------------------------------------ shared furniture

/// A file's header row, drawn identically whichever presentation owns the lines
/// beneath it — a `.md` file is still a file.
pub fn file_header(path: &str, adds: usize, dels: usize, at: &Frame, pen: &mut Pen) {
    let p = &at.theme().diff;
    let bg = row_bg(p.file_bg, at);
    pen.put(" ", Ink::new(p.file_fg, bg));
    pen.put(path, Ink::new(p.file_fg, bg).bold());
    pen.put("  ", Ink::new(p.file_fg, bg));
    pen.put(&format!("+{adds}"), Ink::new(p.adds_fg, bg));
    pen.put(" ", Ink::new(p.file_fg, bg));
    pen.put(&format!("-{dels}"), Ink::new(p.dels_fg, bg));
    pen.wash(Ink::new(p.file_fg, bg));
}

pub fn hunk_header(header: &str, at: &Frame, pen: &mut Pen) {
    let p = &at.theme().diff;
    let bg = row_bg(p.hunk_bg, at);
    pen.put(" ", Ink::new(p.hunk_fg, bg));
    pen.put(header, Ink::new(p.hunk_fg, bg));
    pen.wash(Ink::new(p.hunk_fg, bg));
}

/// The background a row actually draws on: its own, or the selection bar.
///
/// One function so that every presentation and every row kind answer it the same
/// way. A selection that only covered the text and not the gutter reads as a
/// highlight on a word rather than as a cursor on a row.
pub fn row_bg(own: Rgb, at: &Frame) -> Rgb {
    match at.current {
        true => at.theme().chrome.selection_bg,
        false => own,
    }
}

/// The background, foreground and sign of a line of this kind.
///
/// The `+` and `-` survive a move, deliberately: a moved block recedes in colour
/// so the eye can skip it, but the columns still have to scan.
pub fn line_colors(kind: LineKind, moved: bool, p: &DiffPalette) -> (Rgb, Rgb, &'static str) {
    match (kind, moved) {
        (LineKind::Added, false) => (p.added_bg, p.added_fg, "+"),
        (LineKind::Added, true) => (p.moved_added_bg, p.added_fg, "+"),
        (LineKind::Removed, false) => (p.removed_bg, p.removed_fg, "-"),
        (LineKind::Removed, true) => (p.moved_removed_bg, p.removed_fg, "-"),
        (LineKind::Context, _) => (p.context_bg, p.context_fg, " "),
    }
}

/// Draws one line's text, styled, from `at.start` of the line to `at.end`.
///
/// The whole of what a presentation needs to draw diff text: the run list comes
/// from `core`, the foreground from the theme resolved against the surface the
/// run lands on, and the background from that surface — which is how a changed
/// word gets a real background in a terminal rather than the underline the ANSI
/// `paint` example settles for.
///
/// `row_ink` is what the *rest* of the row is washed in, so a line shorter than
/// the window still reads as a block of colour.
#[allow(clippy::too_many_arguments)]
pub fn text_run(
    line: &plait_core::prepared::Line,
    span: std::ops::Range<usize>,
    theme: &Theme,
    row_ink: Ink,
    shift: usize,
    pen: &mut Pen,
    out: &mut Vec<Run>,
) {
    runs(span, &line.tokens, &line.spans, line.kind, line.moved, out);
    pen.scroll(shift);
    for run in out.iter() {
        // A selected row keeps its own background: the bar is the cursor, and a
        // changed word inside it is still worth seeing, so the word background
        // is only suppressed when the row already has one of its own.
        let bg = match run.word {
            true => theme.background(run.surface),
            false => row_ink.bg,
        };
        let style = match run.kind {
            Some(kind) => theme.syntax_on(kind, run.surface),
            // Text no highlighter claimed keeps the row's own foreground rather
            // than a syntax colour it was never given.
            None => plait_core::theme::Style { fg: row_ink.fg, bold: false, italic: false },
        };
        pen.put(&line.text[run.at.clone()], Ink::styled(style, bg));
    }
    pen.wash(row_ink);
}

// -------------------------------------------------------------- the built-in

/// The default presentation: one line of text per row, behind a line-number
/// gutter, coloured by the host's theme.
///
/// The unified diff, and the fallback — it claims every path, which is what
/// makes a specialist registered after it able to take `.md` without this having
/// to know that happened.
#[derive(Default)]
pub struct TextRows {
    flat: Flat,
    /// Columns one line-number column needs, from the largest number actually in
    /// the diff.
    ///
    /// Measured rather than fixed, because a terminal has columns to spare in a
    /// four-line repository and none in a kernel. A constant wide enough for
    /// `git/git` wastes eight columns of a 80-column terminal on every other
    /// repo, and one narrow enough for a small one clips the numbers where it
    /// matters most.
    digits: usize,
}

/// Narrowest a line-number column gets, so a two-file diff does not draw a
/// gutter one character wide and lose the column the eye scans down.
const MIN_DIGITS: usize = 2;

impl TextRows {
    /// Columns one line-number column occupies.
    pub fn gutter(&self) -> usize {
        self.digits.max(MIN_DIGITS)
    }

    /// Everything a text row draws besides its text: two line-number columns,
    /// the sign column, and the single spaces between them.
    ///
    /// What a column budget is measured against, and the reason `reflow` takes
    /// the whole width rather than a pre-computed text budget — the furniture is
    /// this implementation's business and nothing else can know it.
    pub fn chrome(&self) -> usize {
        2 * self.gutter() + 4
    }

    fn budget(&self, cols: usize) -> usize {
        cols.saturating_sub(self.chrome()).max(MIN_WRAP_COLS)
    }

    pub fn flat(&self) -> &Flat {
        &self.flat
    }
}

pub fn digits(n: u32) -> usize {
    match n {
        0 => 1,
        _ => n.ilog10() as usize + 1,
    }
}

/// A line number, right-aligned, or nothing at all on a continuation row.
///
/// A continuation carries no number and no sign: the background is what says
/// which line it belongs to, and an empty gutter is what says it is not a line
/// of its own. Every real line has at least one number, so there is nothing to
/// confuse it with.
pub fn number(n: Option<u32>, blank: bool, wide: usize, ink: Ink, pen: &mut Pen) {
    match (blank, n) {
        (false, Some(n)) => pen.put_right(&n.to_string(), wide, ink),
        _ => pen.fill(wide, ' ', ink),
    }
}

impl Present for TextRows {
    fn claims(&self, _path: &str) -> bool {
        true
    }

    fn len(&self) -> usize {
        self.flat.len()
    }

    fn build(&mut self, file: File) {
        let first = self.flat.len();
        self.flat.push(file);
        for row in &self.flat.rows()[first..] {
            if let Some(l) = row.line() {
                let widest = l.old_no.unwrap_or(0).max(l.new_no.unwrap_or(0));
                self.digits = self.digits.max(digits(widest));
            }
        }
    }

    fn rows(&self, index: usize) -> usize {
        self.flat.visual_rows(index)
    }

    /// Columns of *text*, not of the whole row: the gutter does not scroll, so
    /// what a horizontal scroll has to be bounded by is this.
    fn width(&self, index: usize, seg: usize) -> usize {
        screen::width(self.flat.piece(index, seg).trim_end())
    }

    fn files(&self) -> &[Entry] {
        self.flat.files()
    }
}

impl Rows for TextRows {
    fn reflow(&mut self, cols: usize, _host: &Host, wrap: &dyn Wrap) -> bool {
        self.flat.reflow(self.budget(cols), wrap)
    }

    fn report(&self) -> String {
        self.flat.report()
    }

    fn render(&self, index: usize, seg: usize, at: &Frame, pen: &mut Pen, out: &mut Vec<Run>) {
        let theme = at.theme();
        let p = &theme.diff;
        let Some(row) = self.flat.get(index) else { return };
        match row {
            Row::File { path, adds, dels } => file_header(path, *adds, *dels, at, pen),
            Row::Hunk(header) => hunk_header(header, at, pen),
            Row::Line(l) => {
                let (own, fg, sign) = line_colors(l.kind, l.moved, p);
                let bg = row_bg(own, at);
                let row_ink = Ink::new(fg, bg);
                let gutter = Ink::new(p.gutter_fg, bg);
                let blank = seg > 0;

                number(l.old_no, blank, self.gutter(), gutter, pen);
                pen.put(" ", gutter);
                number(l.new_no, blank, self.gutter(), gutter, pen);
                pen.put(" ", gutter);
                pen.put(if blank { " " } else { sign }, row_ink);
                pen.put(" ", row_ink);

                text_run(l, self.flat.range(index, seg), theme, row_ink, at.shift, pen, out);
            }
        }
    }
}

/// Runs the shared pipeline for a set of presentations. A thin alias for
/// [`plait_core::rows::assemble`] with this frontend's clip budget applied, so
/// no caller in here repeats the number.
pub fn assemble(
    files: &[plait_core::FileDiff],
    host: &Host,
    owners: &mut [Box<dyn Rows>],
) -> plait_core::rows::Assembled {
    plait_core::rows::assemble(files, &host.syntax, MAX_LINE_CHARS, owners)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screen::Screen;
    use plait_core::parse_unified_diff;
    use plait_core::wrap::{Off, Word};

    const DIFF: &str = "\
diff --git a/a.rs b/a.rs
--- a/a.rs
+++ b/a.rs
@@ -1,3 +1,3 @@
 fn one() {}
-let x = 1;
+let x = 2;
 fn two() {}
";

    /// The whole frontend, headless: assemble, reflow, draw every row into a
    /// grid. No terminal, no window, no escape codes read back.
    struct Harness {
        host: Host,
        owners: Vec<Box<dyn Rows>>,
        order: Vec<plait_core::rows::RowRef>,
        screen: Screen,
    }

    impl Harness {
        fn new(raw: &str, cols: usize, layout: &str) -> Self {
            let host = Host::new();
            let layouts = Layouts::builtin();
            let mut owners = layouts.build(layouts.position(layout).expect("registered"), &host);
            let a = assemble(&parse_unified_diff(raw), &host, &mut owners);
            let mut h = Harness {
                host,
                owners,
                order: a.ordered.order,
                screen: Screen::new(cols, 64),
            };
            h.reflow(cols, &Word);
            h
        }

        fn reflow(&mut self, cols: usize, wrap: &dyn Wrap) {
            self.screen.resize(cols, 64);
            let host = &self.host;
            for o in self.owners.iter_mut() {
                o.reflow(cols, host, wrap);
            }
            self.order =
                plait_core::rows::expand(&self.order, &self.owners, None).order;
        }

        fn paint(&mut self, shift: usize, current: Option<usize>) {
            let ink = Ink::new(self.host.theme.chrome.fg, self.host.theme.chrome.bg);
            self.screen.clear(ink);
            let mut out = Vec::new();
            for (y, r) in self.order.iter().enumerate() {
                let at = Frame {
                    host: &self.host,
                    shift,
                    current: current == Some(y),
                };
                let mut pen = self.screen.row(y);
                self.owners[r.owner as usize]
                    .render(r.index as usize, r.seg as usize, &at, &mut pen, &mut out);
            }
        }

        fn dump(&self) -> Vec<String> {
            (0..self.order.len()).map(|y| self.screen.row_text(y)).collect()
        }
    }

    #[test]
    fn a_unified_diff_draws_a_gutter_a_sign_and_its_text() {
        let mut h = Harness::new(DIFF, 40, "unified");
        h.paint(0, None);
        let rows = h.dump();
        assert_eq!(rows[0], " a.rs  +1 -1");
        assert_eq!(rows[1], " @@ -1,3 +1,3 @@");
        assert_eq!(rows[2], " 1  1   fn one() {}");
        assert_eq!(rows[3], " 2    - let x = 1;");
        assert_eq!(rows[4], "    2 + let x = 2;");
        assert_eq!(rows[5], " 3  3   fn two() {}");
    }

    #[test]
    fn the_gutter_widens_to_the_largest_line_number_and_no_further() {
        let narrow = Harness::new(DIFF, 40, "unified");
        assert_eq!(narrow.owners[0].width(2, 0), "fn one() {}".len());
        let mut wide = Harness::new(
            "diff --git a/a.rs b/a.rs\n@@ -12000,1 +12000,1 @@\n-a\n+b\n",
            40,
            "unified",
        );
        wide.paint(0, None);
        assert_eq!(wide.dump()[2], "12000       - a");
    }

    #[test]
    fn a_changed_word_gets_a_real_background_and_not_an_underline() {
        // What the ANSI `paint` example cannot do and this can: a terminal cell
        // holds one background, but it is a *different* background per cell.
        let mut h = Harness::new(DIFF, 40, "unified");
        h.paint(0, None);
        let theme = &h.host.theme;
        // Row 3 is `- let x = 1;`. Find the `1`, which is the changed word.
        let y = 3;
        let x = (0..40).find(|x| h.screen.char_at(*x, y) == Some('1')).unwrap();
        assert_eq!(h.screen.ink(x, y).unwrap().bg, theme.diff.removed_word_bg);
        // ...and the character beside it is on the plain removal background.
        assert_eq!(h.screen.ink(x - 1, y).unwrap().bg, theme.diff.removed_bg);
    }

    #[test]
    fn syntax_colour_reaches_the_cells() {
        let mut h = Harness::new(DIFF, 40, "unified");
        h.paint(0, None);
        let theme = &h.host.theme;
        let expected = theme
            .syntax_on(plait_core::syntax::Kind::Keyword, plait_core::theme::Surface::Removed)
            .fg;
        // `let` on the removed line.
        let y = 3;
        let x = (0..40).find(|x| h.screen.char_at(*x, y) == Some('l')).unwrap();
        assert_eq!(h.screen.ink(x, y).unwrap().fg, expected);
    }

    #[test]
    fn a_row_is_washed_to_the_right_edge_in_its_own_background() {
        let mut h = Harness::new(DIFF, 40, "unified");
        h.paint(0, None);
        assert_eq!(h.screen.ink(39, 4).unwrap().bg, h.host.theme.diff.added_bg);
        assert_eq!(h.screen.ink(39, 5).unwrap().bg, h.host.theme.diff.context_bg);
    }

    #[test]
    fn the_selected_row_is_a_bar_across_the_gutter_too() {
        let mut h = Harness::new(DIFF, 40, "unified");
        h.paint(0, Some(3));
        let bar = h.host.theme.chrome.selection_bg;
        assert_eq!(h.screen.ink(0, 3).unwrap().bg, bar, "the gutter was left out");
        assert_eq!(h.screen.ink(39, 3).unwrap().bg, bar, "it stopped at the text");
        assert_ne!(h.screen.ink(0, 4).unwrap().bg, bar, "it leaked downwards");
    }

    #[test]
    fn a_horizontal_scroll_moves_the_text_and_not_the_gutter() {
        let mut h = Harness::new(DIFF, 40, "unified");
        h.reflow(40, &Off);
        h.paint(4, None);
        let rows = h.dump();
        // ` 2     - ` is intact; `let x = 1;` has lost its first four columns.
        assert!(rows[3].starts_with(" 2    - "), "{:?}", rows[3]);
        assert!(rows[3].ends_with("x = 1;"), "{:?}", rows[3]);
        assert!(!rows[3].contains("let"), "{:?}", rows[3]);
    }

    #[test]
    fn a_wrapped_line_carries_no_number_and_no_sign_on_its_continuations() {
        let long = format!(
            "diff --git a/a.rs b/a.rs\n@@ -1,1 +1,1 @@\n-{}\n+b\n",
            "word ".repeat(20)
        );
        let mut h = Harness::new(&long, 30, "unified");
        h.paint(0, None);
        let rows = h.dump();
        assert!(rows[2].starts_with(" 1    - "), "{:?}", rows[2]);
        assert!(rows[3].starts_with("        w"), "{:?}", rows[3]);
        assert!(rows[3].trim().starts_with("word"), "{:?}", rows[3]);
    }

    #[test]
    fn every_registered_layout_draws_something_for_every_row() {
        // The check that a registry entry is real: no panic, no blank screen,
        // and the same logical rows however they are presented.
        let host = Host::new();
        let layouts = Layouts::builtin();
        for (i, name) in layouts.names().iter().enumerate() {
            let mut h = Harness::new(DIFF, 60, name);
            h.paint(0, None);
            let rows = h.dump();
            assert!(!rows.is_empty(), "{name} drew nothing");
            assert!(rows.iter().any(|r| r.contains("a.rs")), "{name} lost the file header");
            assert!(rows.iter().any(|r| r.contains("let x = 2")), "{name} lost an addition");
            assert_eq!(layouts.name(i), *name);
            let _ = &host;
        }
    }

    #[test]
    fn a_registered_layout_that_builds_nothing_falls_back_rather_than_blanking() {
        let mut layouts = Layouts::builtin();
        layouts.register("empty", |_| Vec::new());
        let host = Host::new();
        let built = layouts.build(layouts.position("empty").unwrap(), &host);
        assert_eq!(built.len(), 1);
        assert!(built[0].claims("anything.rs"));
    }

    #[test]
    fn a_third_party_presentation_needs_nothing_from_this_module() {
        // Rule 1, as a test rather than a promise: an extension registers a
        // presentation for one extension, and it takes over those files without
        // the built-in knowing it exists.
        #[derive(Default)]
        struct Shout(Flat);
        impl Present for Shout {
            fn claims(&self, path: &str) -> bool {
                path.ends_with(".rs")
            }
            fn len(&self) -> usize {
                self.0.len()
            }
            fn build(&mut self, file: File) {
                self.0.push(file);
            }
        }
        impl Rows for Shout {
            fn render(
                &self,
                index: usize,
                _seg: usize,
                at: &Frame,
                pen: &mut Pen,
                _out: &mut Vec<Run>,
            ) {
                let text = self.0.get(index).map_or("", Row::text).to_uppercase();
                pen.put(&text, Ink::new(at.theme().chrome.fg, at.theme().chrome.bg));
            }
        }

        let host = Host::new();
        let mut layouts = Layouts::builtin();
        layouts.register("unified", |_| {
            vec![Box::new(TextRows::default()), Box::new(Shout::default())]
        });
        let mut owners = layouts.build(0, &host);
        let a = assemble(&parse_unified_diff(DIFF), &host, &mut owners);
        assert_eq!(owners[0].len(), 0, "the built-in kept a file the specialist claimed");
        assert_eq!(owners[1].len(), 6);

        let mut screen = Screen::new(40, 8);
        let at = Frame::new(&host);
        let mut out = Vec::new();
        for (y, r) in a.ordered.order.iter().enumerate() {
            let mut pen = screen.row(y);
            owners[r.owner as usize].render(r.index as usize, r.seg as usize, &at, &mut pen, &mut out);
        }
        assert_eq!(screen.row_text(0), "A.RS");
        assert_eq!(screen.row_text(3), "LET X = 1;", "a prepared line carries no sign; the presentation draws it");
    }

    #[test]
    fn a_reflow_that_changes_nothing_says_so() {
        let mut h = Harness::new(DIFF, 40, "unified");
        let host = Host::new();
        assert!(!h.owners[0].reflow(40, &host, &Word), "same width, same wrap");
        assert!(h.owners[0].reflow(20, &host, &Word));
    }

    #[test]
    fn the_report_is_the_flat_rows_report_and_not_a_second_one() {
        let h = Harness::new(DIFF, 40, "unified");
        assert_eq!(h.owners[0].report(), "");
    }
}
