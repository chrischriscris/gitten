//! The seam: one file's diff, turned into rows and drawn into a terminal.
//!
//! [`Rows`] is [`gitten_core::rows::Present`] plus a `render`, and that split is
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
//! [`gitten_core::runs::runs`] into a buffer the caller owns and reuses, and the
//! text is sliced out of the line rather than copied. That is the terminal's
//! version of "nothing on the render path allocates per frame".

use crate::screen::{self, Ink, Pen};
use crate::{MAX_LINE_CHARS, MIN_WRAP_COLS};
use gitten_core::host::Host;
use gitten_core::prepared::File;
use gitten_core::rows::{Entry, Flat, Present, Row};
use gitten_core::runs::{runs, Run};
use gitten_core::select::{Hit, Selected};
use gitten_core::theme::{DiffPalette, Rgb, Style, Surface, Theme};
use gitten_core::wrap::Wrap;
use gitten_core::LineKind;
use std::ops::Range;

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
    /// Which bytes of this row the *mouse* has selected, in the row's own
    /// coordinates — `None` for nearly every row on nearly every frame.
    ///
    /// A different thing from `current` and a different colour: that one is a
    /// bar under the row the keyboard is on, and this one is chosen text. The
    /// model behind it is [`gitten_core::select`], shared with the window, so a
    /// wrapped line copies once in both.
    pub sel: Option<Selected>,
}

impl<'a> Frame<'a> {
    pub fn new(host: &'a Host) -> Self {
        Self {
            host,
            shift: 0,
            current: false,
            sel: None,
        }
    }

    /// The selection over one of this row's texts, or `None` if the selection is
    /// in another. What a presentation drawing two columns asks per column.
    pub fn part(&self, part: u16) -> Option<Selected> {
        self.sel.filter(|s| s.part() == part)
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

    /// Which text a click `col` columns into this row landed in, and which byte.
    ///
    /// The frontend half of a selection, and the only half that needs to know
    /// what was drawn: where the text starts depends on the gutters and the signs
    /// this presentation put in front of it, and `shift` is how far it has been
    /// scrolled sideways under them. Nobody outside an implementation can know
    /// either, which is why this is on the trait rather than a subtraction in the
    /// view.
    ///
    /// The offset is into the **logical** row's text, not the visual row's, so a
    /// caret on the third row of a wrapped line is the same kind of thing as one
    /// on an unwrapped line — see [`gitten_core::select`]. `None` means the row
    /// takes no part in a selection, and defaults to it: an extension's
    /// presentation compiles unchanged and is simply not selectable until it says
    /// where its text is.
    fn hit(&self, _index: usize, _seg: usize, _col: usize, _shift: usize) -> Option<Hit> {
        None
    }

    /// The text of one of this row's parts: what a selection over it copies.
    ///
    /// `None` for a part that is not there — the empty side of a two-column row,
    /// a row that draws no text — and a copy *skips* those rather than pasting a
    /// blank line for them. The coordinates are the ones [`Rows::hit`] returns
    /// offsets into.
    fn selectable(&self, _index: usize, _part: u16) -> Option<&str> {
        None
    }

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
        // The same name the desktop registers and `gitten.toml` documents:
        // `diff.layout` is data, and one value has to open this presentation
        // from every client that reads the file.
        l.register("split", |_| {
            vec![Box::new(crate::split::SplitRows::default())]
        });
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
        let layout = Layout {
            name,
            build: Box::new(build),
        };
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

/// Columns a header's text is indented by. One space, and the number a hit test
/// has to subtract — shared so the caret and the glyph cannot disagree.
pub const HEADER_PAD: usize = 1;

/// A file's header row, drawn identically whichever presentation owns the lines
/// beneath it — a `.md` file is still a file.
pub fn file_header(path: &str, adds: usize, dels: usize, at: &Frame, pen: &mut Pen) {
    let p = &at.theme().diff;
    let bg = row_bg(p.file_bg, at);
    pen.fill(HEADER_PAD, ' ', Ink::new(p.file_fg, bg));
    selected_text(
        path,
        Ink::new(p.file_fg, bg).bold(),
        at.part(0),
        at.theme(),
        pen,
    );
    pen.put("  ", Ink::new(p.file_fg, bg));
    pen.put(&format!("+{adds}"), Ink::new(p.adds_fg, bg));
    pen.put(" ", Ink::new(p.file_fg, bg));
    pen.put(&format!("-{dels}"), Ink::new(p.dels_fg, bg));
    pen.wash(Ink::new(p.file_fg, bg));
}

pub fn hunk_header(header: &str, at: &Frame, pen: &mut Pen) {
    let p = &at.theme().diff;
    let bg = row_bg(p.hunk_bg, at);
    pen.fill(HEADER_PAD, ' ', Ink::new(p.hunk_fg, bg));
    selected_text(header, Ink::new(p.hunk_fg, bg), at.part(0), at.theme(), pen);
    pen.wash(Ink::new(p.hunk_fg, bg));
}

/// Where a click landed in a file or hunk header.
///
/// Shared for the same reason the headers themselves are: whoever owns the lines
/// beneath them, a header is drawn by [`file_header`] or [`hunk_header`] and its
/// text starts at [`HEADER_PAD`]. Two presentations working that out separately
/// is two places for the caret to be a column off.
pub fn header_hit(text: &str, col: usize) -> Hit {
    Hit {
        part: 0,
        off: col_at(text, col.saturating_sub(HEADER_PAD)),
    }
}

/// Which byte of `text` is drawn in column `col` of it.
///
/// Columns and not characters, so a caret lands where the pointer is on a line of
/// CJK — the same measure [`screen::cols`] gives the grid, run backwards. Past
/// the end is the end: a click in the blank beyond a short line selects to the
/// end of it, which is what a drag across a ragged block of text has to do.
///
/// **Lands on the character the pointer is over**, not on the nearest boundary.
/// A cell is a cell and there is no right half of one to round away from; a drag
/// that includes the character it started on is [`crate::diff::Diff`]'s doing,
/// because only it knows which way the drag is going.
pub fn col_at(text: &str, col: usize) -> usize {
    let mut at = 0;
    for (i, c) in text.char_indices() {
        if at >= col {
            return i;
        }
        at += screen::cols(c);
    }
    text.len()
}

/// Writes text with whatever a selection covers of it lit up behind it.
///
/// For the rows that have no runs to speak of — a file path, a hunk header. A
/// line of a diff goes through [`text_run`] instead, which has tokens and changed
/// words to keep as well.
pub fn selected_text(text: &str, ink: Ink, sel: Option<Selected>, theme: &Theme, pen: &mut Pen) {
    let Some(range) = selection(sel, text.len()) else {
        pen.put(text, ink);
        return;
    };
    let on = ink.on(theme.background(Surface::Selected));
    pen.put(&text[..range.start], ink);
    pen.put(&text[range.clone()], on);
    pen.put(&text[range.end..], ink);
}

/// The bytes of a text `len` long that are selected, or `None` when none are.
///
/// Empty is `None` and not `0..0`: a zero-length highlight is a colour change
/// nobody asked for, and every caller would otherwise have to check.
fn selection(sel: Option<Selected>, len: usize) -> Option<Range<usize>> {
    sel.map(|s| s.range(len)).filter(|r| !r.is_empty())
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
    line: &gitten_core::prepared::Line,
    span: std::ops::Range<usize>,
    theme: &Theme,
    row_ink: Ink,
    shift: usize,
    sel: Option<Selected>,
    pen: &mut Pen,
    out: &mut Vec<Run>,
) {
    runs(span, &line.tokens, &line.spans, line.kind, line.moved, out);
    pen.scroll(shift);
    // Bytes of the *line*, which is what the runs address too — a wrapped row
    // draws its own slice of both.
    let sel = selection(sel, line.text.len());
    for run in out.iter() {
        // A run is cut into at most three pieces by the selection, and nearly
        // always into one: the loop below runs on every visible row of every
        // frame, so the unselected case must not cost a comparison per byte.
        let (a, b) = match &sel {
            Some(s) => (
                s.start.clamp(run.at.start, run.at.end),
                s.end.clamp(run.at.start, run.at.end),
            ),
            None => (run.at.end, run.at.end),
        };
        piece(line, run, run.at.start..a, false, theme, row_ink, pen);
        piece(line, run, a..b, true, theme, row_ink, pen);
        piece(line, run, b..run.at.end, false, theme, row_ink, pen);
    }
    pen.wash(row_ink);
}

/// One stretch of a run, selected or not.
///
/// Selected text is resolved against [`Surface::Selected`] and not merely given
/// a different background: a comment's grey was chosen to recede on a dark
/// removal, and left alone on the selection's own colour it disappears. That is
/// `core`'s contrast table doing what it is for, and the window resolves the same
/// way.
fn piece(
    line: &gitten_core::prepared::Line,
    run: &Run,
    at: Range<usize>,
    on: bool,
    theme: &Theme,
    row_ink: Ink,
    pen: &mut Pen,
) {
    if at.is_empty() {
        return;
    }
    let surface = match on {
        true => Surface::Selected,
        false => run.surface,
    };
    // A selected row keeps its own background: the bar is the cursor, and a
    // changed word inside it is still worth seeing, so the word background is
    // only suppressed when the row already has one of its own.
    let bg = match (on, run.word) {
        (true, _) => theme.background(Surface::Selected),
        (false, true) => theme.background(run.surface),
        (false, false) => row_ink.bg,
    };
    let style = match run.kind {
        Some(kind) => theme.syntax_on(kind, surface),
        // Text no highlighter claimed keeps the row's own foreground rather
        // than a syntax colour it was never given.
        None => Style {
            fg: row_ink.fg,
            bold: false,
            italic: false,
        },
    };
    pen.put(&line.text[at], Ink::styled(style, bg));
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
        let Some(row) = self.flat.get(index) else {
            return;
        };
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

                let span = self.flat.range(index, seg);
                text_run(l, span, theme, row_ink, at.shift, at.part(0), pen, out);
            }
        }
    }

    fn hit(&self, index: usize, seg: usize, col: usize, shift: usize) -> Option<Hit> {
        match self.flat.get(index)? {
            Row::File { path, .. } => Some(header_hit(path, col)),
            Row::Hunk(header) => Some(header_hit(header, col)),
            Row::Line(l) => {
                // Before the text is a click in the gutter, which is a caret at
                // the start of the line and not nothing: dragging from a line
                // number is how a whole line gets selected.
                let text = col.saturating_sub(self.chrome()) + shift;
                let span = self.flat.range(index, seg);
                let at = col_at(&l.text[span.clone()], text);
                Some(Hit {
                    part: 0,
                    off: span.start + at,
                })
            }
        }
    }

    fn selectable(&self, index: usize, part: u16) -> Option<&str> {
        if part != 0 {
            return None;
        }
        Some(self.flat.get(index)?.text())
    }
}

/// Runs the shared pipeline for a set of presentations. A thin alias for
/// [`gitten_core::rows::assemble`] with this frontend's clip budget applied, so
/// no caller in here repeats the number.
pub fn assemble(
    files: &[gitten_core::FileDiff],
    host: &Host,
    owners: &mut [Box<dyn Rows>],
) -> gitten_core::rows::Assembled {
    gitten_core::rows::assemble(files, &host.syntax, MAX_LINE_CHARS, owners)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screen::Screen;
    use gitten_core::parse_unified_diff;
    use gitten_core::wrap::{Off, Word};

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
        order: Vec<gitten_core::rows::RowRef>,
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
            self.order = gitten_core::rows::expand(&self.order, &self.owners, None).order;
        }

        fn paint(&mut self, shift: usize, current: Option<usize>) {
            self.paint_sel(shift, current, None);
        }

        /// The same, with a selection over the whole diff resolved against the
        /// order table — what a drag produces, without a drag.
        fn paint_sel(
            &mut self,
            shift: usize,
            current: Option<usize>,
            sel: Option<&gitten_core::select::Selection>,
        ) {
            let ink = Ink::new(self.host.theme.chrome.fg, self.host.theme.chrome.bg);
            self.screen.clear(ink);
            let mut out = Vec::new();
            for (y, r) in self.order.iter().enumerate() {
                let at = Frame {
                    host: &self.host,
                    shift,
                    current: current == Some(y),
                    sel: sel.and_then(|s| s.at(y, r.logical())),
                };
                let mut pen = self.screen.row(y);
                self.owners[r.owner as usize].render(
                    r.index as usize,
                    r.seg as usize,
                    &at,
                    &mut pen,
                    &mut out,
                );
            }
        }

        fn dump(&self) -> Vec<String> {
            (0..self.order.len())
                .map(|y| self.screen.row_text(y))
                .collect()
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
        let x = (0..40)
            .find(|x| h.screen.char_at(*x, y) == Some('1'))
            .unwrap();
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
            .syntax_on(
                gitten_core::syntax::Kind::Keyword,
                gitten_core::theme::Surface::Removed,
            )
            .fg;
        // `let` on the removed line.
        let y = 3;
        let x = (0..40)
            .find(|x| h.screen.char_at(*x, y) == Some('l'))
            .unwrap();
        assert_eq!(h.screen.ink(x, y).unwrap().fg, expected);
    }

    #[test]
    fn a_row_is_washed_to_the_right_edge_in_its_own_background() {
        let mut h = Harness::new(DIFF, 40, "unified");
        h.paint(0, None);
        assert_eq!(h.screen.ink(39, 4).unwrap().bg, h.host.theme.diff.added_bg);
        assert_eq!(
            h.screen.ink(39, 5).unwrap().bg,
            h.host.theme.diff.context_bg
        );
    }

    #[test]
    fn the_selected_row_is_a_bar_across_the_gutter_too() {
        let mut h = Harness::new(DIFF, 40, "unified");
        h.paint(0, Some(3));
        let bar = h.host.theme.chrome.selection_bg;
        assert_eq!(
            h.screen.ink(0, 3).unwrap().bg,
            bar,
            "the gutter was left out"
        );
        assert_eq!(
            h.screen.ink(39, 3).unwrap().bg,
            bar,
            "it stopped at the text"
        );
        assert_ne!(h.screen.ink(0, 4).unwrap().bg, bar, "it leaked downwards");
    }

    #[test]
    fn a_click_lands_on_the_byte_under_it_and_the_gutter_is_the_start_of_the_line() {
        let h = Harness::new(DIFF, 40, "unified");
        let rows = &h.owners[0];
        // Row 3 is `- let x = 1;`, drawn after ` 2    - `: the chrome is 8
        // columns, so column 12 is the `x`.
        let hit = rows.hit(3, 0, 12, 0).expect("a line is selectable");
        assert_eq!((hit.part, hit.off), (0, 4));
        assert_eq!(&rows.selectable(3, 0).unwrap()[hit.off..], "x = 1;");
        // Anywhere in the gutter is the start of the line, so a drag from a line
        // number selects the whole of it rather than nothing.
        assert_eq!(rows.hit(3, 0, 1, 0).unwrap().off, 0);
        // Past the end of a short line is the end of it.
        assert_eq!(rows.hit(3, 0, 99, 0).unwrap().off, "let x = 1;".len());
        // A header is selectable too, and its text starts one column in.
        assert_eq!(rows.hit(0, 0, 1, 0).unwrap().off, 0);
        assert_eq!(rows.selectable(0, 0), Some("a.rs"));
    }

    #[test]
    fn a_click_follows_the_text_when_it_is_scrolled_sideways() {
        let mut h = Harness::new(DIFF, 40, "unified");
        h.reflow(40, &Off);
        // The same cell, with four columns swallowed: four bytes further in.
        let plain = h.owners[0].hit(3, 0, 12, 0).unwrap().off;
        let shifted = h.owners[0].hit(3, 0, 12, 4).unwrap().off;
        assert_eq!(shifted, plain + 4);
    }

    #[test]
    fn a_selection_is_painted_on_the_bytes_it_covers_and_no_others() {
        use gitten_core::select::{Caret, Selection};
        let mut h = Harness::new(DIFF, 40, "unified");
        // `let x = 1;` on row 3: select `x = 1`.
        let mut sel = Selection::new(0, Caret::new((0, 3), 4, 3));
        sel.extend(Caret::new((0, 3), 9, 3));
        h.paint_sel(0, None, Some(&sel));
        let bg = h.host.theme.background(Surface::Selected);
        let x = (0..40)
            .find(|x| h.screen.char_at(*x, 3) == Some('x'))
            .unwrap();
        assert_eq!(h.screen.ink(x, 3).unwrap().bg, bg);
        assert_eq!(
            h.screen.ink(x + 4, 3).unwrap().bg,
            bg,
            "the last selected byte"
        );
        assert_ne!(h.screen.ink(x - 1, 3).unwrap().bg, bg, "it started early");
        assert_ne!(
            h.screen.ink(x + 5, 3).unwrap().bg,
            bg,
            "it ran past the end"
        );
        // The row it is not on keeps its own colour.
        assert_eq!(h.screen.ink(x, 4).unwrap().bg, h.host.theme.diff.added_bg);
    }

    #[test]
    fn a_selected_word_keeps_the_word_highlight_it_landed_on() {
        // Both are backgrounds and only one can win. The selection does, because
        // it is the thing that just moved — but the row's changed word is still
        // read against the *selected* surface, not left in a colour chosen for a
        // removal it is no longer drawn on.
        use gitten_core::select::{Caret, Selection};
        let mut h = Harness::new(DIFF, 40, "unified");
        let mut sel = Selection::new(0, Caret::new((0, 3), 0, 3));
        sel.extend(Caret::new((0, 3), 10, 3));
        h.paint_sel(0, None, Some(&sel));
        let bg = h.host.theme.background(Surface::Selected);
        let one = (0..40)
            .find(|x| h.screen.char_at(*x, 3) == Some('1'))
            .unwrap();
        assert_eq!(
            h.screen.ink(one, 3).unwrap().bg,
            bg,
            "the changed word kept its own"
        );
    }

    #[test]
    fn a_header_lights_up_under_a_selection_too() {
        use gitten_core::select::{Caret, Selection};
        let mut h = Harness::new(DIFF, 40, "unified");
        let mut sel = Selection::new(0, Caret::new((0, 0), 0, 0));
        sel.extend(Caret::new((0, 0), 4, 0));
        h.paint_sel(0, None, Some(&sel));
        let bg = h.host.theme.background(Surface::Selected);
        assert_eq!(h.screen.ink(1, 0).unwrap().bg, bg, "the path was not lit");
        assert_ne!(h.screen.ink(6, 0).unwrap().bg, bg, "the +1 -1 was");
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
            assert!(
                rows.iter().any(|r| r.contains("a.rs")),
                "{name} lost the file header"
            );
            assert!(
                rows.iter().any(|r| r.contains("let x = 2")),
                "{name} lost an addition"
            );
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
        assert_eq!(
            owners[0].len(),
            0,
            "the built-in kept a file the specialist claimed"
        );
        assert_eq!(owners[1].len(), 6);

        let mut screen = Screen::new(40, 8);
        let at = Frame::new(&host);
        let mut out = Vec::new();
        for (y, r) in a.ordered.order.iter().enumerate() {
            let mut pen = screen.row(y);
            owners[r.owner as usize].render(
                r.index as usize,
                r.seg as usize,
                &at,
                &mut pen,
                &mut out,
            );
        }
        assert_eq!(screen.row_text(0), "A.RS");
        assert_eq!(
            screen.row_text(3),
            "LET X = 1;",
            "a prepared line carries no sign; the presentation draws it"
        );
    }

    #[test]
    fn a_reflow_that_changes_nothing_says_so() {
        let mut h = Harness::new(DIFF, 40, "unified");
        let host = Host::new();
        assert!(
            !h.owners[0].reflow(40, &host, &Word),
            "same width, same wrap"
        );
        assert!(h.owners[0].reflow(20, &host, &Word));
    }

    #[test]
    fn the_report_is_the_flat_rows_report_and_not_a_second_one() {
        let h = Harness::new(DIFF, 40, "unified");
        assert_eq!(h.owners[0].report(), "");
    }
}
