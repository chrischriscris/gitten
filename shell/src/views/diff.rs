//! The diff view.
//!
//! Everything is flattened to a uniform row list up front — file headers, hunk
//! headers and lines all the same height — so the whole thing virtualizes
//! through one `uniform_list` regardless of how large the diff is.
//!
//! Word-level spans come from `gitten_core::intraline` and syntax tokens from
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
//!
//! # Layouts
//!
//! A named set of those implementations is a [`Layout`], and `s` cycles them
//! live. Unified and side-by-side are two entries in a registry, not two
//! branches: the second one is `SplitRows` claiming every path in place of
//! `TextRows`, and nothing in here knows which of them is loaded.
//!
//! Switching rebuilds the rows, which means running `prepare` again — the parsed
//! diff is kept for exactly that. It is not free on a 700k-line diff and it is
//! not on the render path either; see [`Diff::cycle_layout`] for the trade and
//! what the alternative would cost.
//!
//! # Wrapping
//!
//! A line too wide for the window is drawn on several rows rather than on one
//! tall one, because `uniform_list` needs them all the same height. So there are
//! two row counts in here: [`Rows::len`] counts lines and [`Rows::rows`] counts
//! what is drawn, and the order table carries which of a line's rows an entry is.
//!
//! The budget is this view's own measured width — see [`Diff::probe`] — so it
//! changes on a resize, and [`Diff::reflow`] is what re-expands the rows when it
//! crosses a character boundary. That runs stages 4c and 5 of the pipeline and
//! nothing above them: no clip, no intraline, no syntax. Where a line breaks is
//! `gitten_core::wrap`, which is a registry on `Host` and has `w` and a title-bar
//! control like everything else that is one.
//!
//! # Scrolling sideways
//!
//! With wrapping off a line wider than the window is reached by scrolling, and
//! the line numbers, the sign column and whatever else a presentation drew in
//! front of its text **stay where they are** while the text moves under them.
//! That is what the terminal does with `Pen::scroll`, and it is the same thing
//! here: a row is always exactly as wide as the viewport, the furniture is drawn
//! first, and the text goes in a [`scrolled`] window that clips it.
//!
//! Which is why the offset is this view's — [`Pan`] — and not `uniform_list`'s.
//! The list scrolls a *row*, and a row is the gutter and the text together;
//! nothing outside it can hold one of them still. What the list keeps is the
//! vertical axis, which is the one that has to virtualize.

use super::{
    accept_deferred_scroll, horizontal_scrollbar, track_marks, vertical_scrollbar,
    DeferredScrollbar, PendingScroll,
};
use crate::chrome::gap_l;
pub(crate) use crate::chrome::ROW_BAR;
use gitten_core::font::Font;
use gitten_core::host::Host;
use gitten_core::prepared::{prepare, Prepared};
use gitten_core::rows::{Ordered, RowRef};
use gitten_core::runs::{self, surfaces, Run};
use gitten_core::select::{self, Caret, RowId, Selected, Selection, Text as _};
use gitten_core::syntax::Token;
use gitten_core::theme::{DiffPalette, Rgb, Surface, Theme};
use gitten_core::view::Viewport;
use gitten_core::wrap::{Wrap, Wrapped};
use gitten_core::{FileDiff, LineKind, Span};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::scroll::ScrollbarHandle;
use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::rc::Rc;

pub(crate) const ROW_H: f32 = 22.0;
/// One line-number column, including the air after the digits.
///
/// 56 and not 52 because the digits are right-aligned now and the air is part of
/// the column: 48 pixels of digits is six of them in the shipped face, which is
/// a million-line file, and 8 pixels of gap is what stops a number touching the
/// next column.
const GUTTER_W: f32 = 56.0;
/// The air between the last digit of a gutter and whatever is next to it.
const GUTTER_PAD: f32 = 8.0;
/// The `+`/`-` column.
pub(crate) const SIGN_W: f32 = 16.0;
/// The page padding, at each edge.
pub(crate) const PAD: f32 = 16.0;

/// Everything a text row draws besides its text: the padding at both edges, the
/// two line-number gutters and the sign column. What the wrap budget is measured
/// against — see [`columns`].
pub(crate) const TEXT_CHROME: f32 = 2.0 * PAD + 2.0 * GUTTER_W + SIGN_W;

/// Where a hunk header's text starts: the same x as a code line's text, so the
/// `@@` sits over the code it addresses rather than under the line numbers.
/// `TEXT_CHROME` less the right-hand padding, which is what a line has in front
/// of its first character.
pub(crate) const HUNK_INDENT: f32 = TEXT_CHROME - PAD;

/// Narrowest wrap budget worth having. A window dragged narrower than its own
/// gutters would otherwise ask for one character a row, which is a diff turned
/// into a column of letters; overflowing is the better failure.
pub(crate) const MIN_WRAP_COLS: usize = 8;

/// How many characters fit in `width` pixels once `chrome` is taken out, in the
/// host's face at `size` pixels.
///
/// From `font.advance` rather than a measured glyph, which is what that field is
/// for and is exact in a monospaced face. In a proportional one it is an average,
/// so a row may come out a little short or a little over — the same
/// approximation [`Rows::overflow`] and the Markdown table padding already make,
/// and the reason `Font::monospaced` exists to be asked.
pub(crate) fn columns(width: f32, chrome: f32, size: f32, host: &Host) -> usize {
    let advance = size * host.font.advance;
    if advance <= 0.0 || !width.is_finite() {
        return MIN_WRAP_COLS;
    }
    (((width - chrome) / advance).floor().max(0.0) as usize).max(MIN_WRAP_COLS)
}

/// How wide a row may get before it is clipped — a rendering budget, which is
/// why it is the frontend that owns the number and `core` that applies it. Text
/// layout is linear in length and a 9.6-million-character line was measured in
/// the wild; nobody reads past column 2000 either way.
const MAX_LINE_CHARS: usize = 2000;

/// The expensive client-independent half of building diff rows. Exposed to the
/// shell so repository refresh can run clipping, intraline and syntax work on
/// its background load task before GPUI applies the result.
pub(crate) fn prepare_files(files: &[FileDiff], host: &Host) -> Prepared {
    prepare(files, &host.syntax, MAX_LINE_CHARS)
}

/// Where a click landed inside a row — see [`Rows::hit`].
///
/// `core`'s, since the terminal asks its presentations the same question in
/// cells and got the same answer back.
pub use gitten_core::select::Hit;

/// Which byte of `text` a click `x` pixels into it landed on.
///
/// Characters, not bytes and not graphemes, and from `font.advance` rather than a
/// measured glyph: exactly the approximation `columns` and [`Rows::overflow`]
/// already make, and exact in a monospaced face. In a proportional one a caret
/// drifts along a long line, which is the price of not shaping the text twice —
/// and `Font::monospaced` exists to be asked when that matters.
///
/// **Rounds rather than truncates.** A click on the right half of a character
/// puts the caret after it, which is what every text field does and what makes a
/// drag include the character it started on.
pub(crate) fn column_at(text: &str, x: f32, size: f32, host: &Host) -> usize {
    let advance = size * host.font.advance;
    if advance <= 0.0 || !x.is_finite() {
        return 0;
    }
    let col = (x / advance).round().max(0.0) as usize;
    text.char_indices()
        .nth(col)
        .map(|(i, _)| i)
        .unwrap_or(text.len())
}

// ------------------------------------------------------------------ the seam

/// Everything a presentation needs to know about one row's relationship to
/// the keyboard, beyond what the row itself holds.
///
/// One argument and not three bools because this trait is a documented seam —
/// `docs/extending.md` — and a presentation after the next must not change the
/// signature twice.
#[derive(Clone, Copy, Default)]
pub struct RowState {
    /// The keyboard's row.
    pub current: bool,
    /// Whether this pane holds the keyboard at all: [`row_bar`] picks the
    /// cursor bar's ink by it — accent while the pane holds the keyboard,
    /// faint where the selection is remembered and the keyboard is not.
    pub focused: bool,
    /// An armed destructive question stands over this row's hunk: the gutter
    /// and the sign read it, and tint toward `chrome.error`, so the line a
    /// second press would destroy is named by its own colour and not only by
    /// the band above it.
    pub armed: bool,
    /// The row is inside the hunk the keyboard is on — its header, its lines
    /// and the rows a wrapped line spilled onto. The extent a hunk verb acts
    /// on: the gutter's hairline marks it, row by row, so a hunk that starts
    /// above the viewport is still shown. Computed against the hunk's span
    /// once per frame; a row reads it as two integer compares — see
    /// [`HunkExtent`].
    pub in_hunk: bool,
}

/// Turns one file's diff into rows, and draws them.
///
/// Row height is fixed for the whole list because `uniform_list` is what makes a
/// 700k-row diff scroll at all, so an implementation may draw anything it likes
/// within [`ROW_H`] but cannot ask for more. A presentation that genuinely needs
/// variable height — a rendered Markdown preview, an image diff — is a different
/// plug point: its own view in its own pane, not a row in this list.
///
/// # Rows, and the rows a row is drawn on
///
/// A wrapped line is *n rows of [`ROW_H`]*, never one tall one, for exactly that
/// reason. So there are two indices in here and they are not the same thing:
/// [`Rows::len`] and [`Rows::build`] count **logical** rows — a line, a hunk
/// header, a file header — and `render`/`width` are additionally handed which
/// **visual** row of that logical one they are drawing. `seg` is 0 for
/// everything that fits, which is nearly everything.
///
pub trait Rows {
    /// Whether this implementation wants the file. The built-in claims
    /// everything; the last registered claimant wins, so a specialist can take
    /// `.md` without the generalist having to know it exists.
    fn claims(&self, path: &str) -> bool;

    /// How many rows this implementation currently holds. The list uses it to
    /// address the rows `build` is about to append.
    fn len(&self) -> usize;

    /// Appends the rows for `file`, which arrives clipped, intraline-diffed and
    /// highlighted — see `gitten_core::prepared`. An implementation draws; it does
    /// not redo any of that.
    fn build(&mut self, file: gitten_core::prepared::File);

    /// Whether logical row `index` is a file header — the `path +n -m` band
    /// [`file_header`] draws.
    ///
    /// Asked so the list can leave it out: a diff of exactly one file is named by
    /// the pane header above it, and a second copy of the name two rows down is
    /// furniture. With two files or more the band is the separator and stays.
    /// The row itself is still built and still addressable — only the order
    /// table skips it — so nothing an implementation indexes by row moves.
    /// Defaults to `false`, which is "keep everything" for a presentation that
    /// draws no such row.
    fn is_file_header(&self, _index: usize) -> bool {
        false
    }

    /// How many visual rows logical row `index` occupies at the current wrap.
    /// More than one only when its text wraps.
    fn rows(&self, _index: usize) -> usize {
        1
    }

    /// A new width, in pixels, for everything this presentation draws.
    ///
    /// Returns whether its row expansion changed, which is what tells the list
    /// whether to rebuild its order table — so a resize that does not cross a
    /// character boundary costs a float comparison and nothing else. The
    /// implementation owns the conversion from pixels to columns because it owns
    /// what it draws around the text: see [`columns`] and [`TEXT_CHROME`].
    fn reflow(&mut self, _width: f32, _host: &Host, _wrap: &dyn Wrap) -> bool {
        false
    }

    /// Draws one visual row. `sel` is the part of it the mouse has selected, in
    /// the row's own byte coordinates — `None` for the overwhelming majority of
    /// rows on the overwhelming majority of frames. `state` is the row's
    /// relationship to the keyboard: whether it is the row the keyboard is on,
    /// drawn as a background bar so navigation has a visible cursor — see
    /// [`gitten_core::view::Viewport`] — whether the pane holding it holds the
    /// keyboard, and whether an armed question stands over it.
    ///
    /// `shift` is how many pixels of text a horizontal scroll has pulled off the
    /// left edge. A row is as wide as the viewport whatever it holds, so an
    /// implementation draws its furniture at the left as usual and puts its text
    /// in a [`scrolled`] window: the numbers stay put, the text moves. Ignoring
    /// it draws a presentation that simply does not scroll sideways, which is the
    /// right answer for one whose rows always fit.
    fn render(
        &self,
        index: usize,
        seg: usize,
        host: &Host,
        sel: Option<Selected>,
        state: RowState,
        shift: f32,
    ) -> AnyElement;

    /// Whether logical row `index` is a file header. What `]` and `[` jump
    /// between; the default is no, because only an implementation knows what it
    /// drew as one.
    fn is_header(&self, _index: usize) -> bool {
        false
    }

    /// Which diff hunk logical row `index` belongs to: `(path, hunk)`, where
    /// `hunk` indexes that file's hunks in the loaded diff. The keyboard's
    /// address for hunk-level staging — what space, u and D act on.
    ///
    /// On the trait rather than computed outside because a hunk's row shape
    /// is the implementation's own: split pairs a removal with the addition
    /// that replaced it onto one row, so the same hunk spans a different
    /// number of rows in each presentation. The default is none, which is
    /// what makes "the keyboard is not on a hunk" the honest answer from
    /// anything that draws no hunks — a rendered document, a graph.
    fn hunk_at(&self, _index: usize) -> Option<(&str, usize)> {
        None
    }

    /// The logical rows the hunk under logical row `index` spans — its header
    /// row through its last line, the same address [`Rows::hunk_at`] names but
    /// as a range: what the extent mark and the armed tint are computed
    /// against, once per frame, so a row reads its membership as integer
    /// compares against a precomputed key and never a search apiece. The
    /// default is none, which is what makes "no hunk, no extent" the honest
    /// answer from anything that draws no hunks — a rendered document, a graph.
    fn hunk_span(&self, _index: usize) -> Option<(u32, u32)> {
        None
    }

    /// Width of a visual row in characters. What the widest-row search ranks
    /// rows by, and therefore which row [`Rows::overflow`] is asked about.
    fn width(&self, index: usize, seg: usize) -> usize;

    /// How far the text of one visual row reaches past the right edge of a
    /// `width`-pixel window, in pixels: what bounds the horizontal scroll.
    ///
    /// The presentation answers because only it knows what it drew in front of
    /// the text, how large the text is drawn and — in a two-column layout — which
    /// window the text is even in. Zero when the row fits, which is every row of
    /// a wrapped diff, and the default: a presentation that never overflows never
    /// scrolls sideways and needs no code for it.
    fn overflow(&self, _index: usize, _seg: usize, _width: f32, _host: &Host) -> f32 {
        0.0
    }

    /// Which text a click `x` pixels into this row landed in, and which byte of
    /// it.
    ///
    /// The frontend half of a selection, and the only half that needs pixels:
    /// where the text starts depends on the gutters, the bars and the indents
    /// this presentation drew in front of it, and how wide a character is depends
    /// on the face and — in a rendered document — on the row. Nobody outside an
    /// implementation can know either, which is why this is on the trait rather
    /// than a division in the view.
    ///
    /// `x` is from the left edge of the *window*, and `shift` is how far the text
    /// has been scrolled sideways under the furniture — so the arithmetic is the
    /// terminal's, `(x - chrome).max(0) + shift`, and a click on a line number
    /// lands on the first character there is to see rather than on one scrolled
    /// out of the window.
    ///
    /// The offset is into the **logical** row's text, not the visual row's, so a
    /// caret on the third row of a wrapped line is the same kind of thing as one
    /// on an unwrapped line — see [`gitten_core::select`]. `None` means the row
    /// takes no part in a selection, and defaults to it: an extension's
    /// presentation compiles unchanged and is simply not selectable until it says
    /// where its text is.
    fn hit(&self, _index: usize, _seg: usize, _x: f32, _host: &Host, _shift: f32) -> Option<Hit> {
        None
    }

    /// The text of one of this row's parts: what a selection over it copies.
    ///
    /// `None` for a part that is not there — the empty side of a two-column row,
    /// a row that draws no text — and a copy *skips* those rather than pasting a
    /// blank line for them. The line coordinates are the ones [`Rows::hit`]
    /// returns offsets into.
    fn selectable(&self, _index: usize, _part: u16) -> Option<&str> {
        None
    }

    /// Whatever this implementation wants to say on the stats overlay.
    fn report(&self) -> String {
        String::new()
    }
}

// ---------------------------------------------------------------- the layouts

/// A named set of [`Rows`] implementations: one way of presenting a whole diff.
///
/// `build` is a closure rather than a `Vec` because a layout has to be
/// *rebuildable*. Switching re-runs the pipeline and hands each implementation
/// its files again, and a `Vec` that has already been consumed cannot be handed
/// anything. It takes the `Host` because a presentation is entitled to depend on
/// the font — `MarkdownRows` derives its whole heading scale from it.
pub struct Layout {
    pub name: &'static str,
    #[allow(clippy::type_complexity)]
    pub build: Box<dyn Fn(&Host) -> Vec<Box<dyn Rows>>>,
}

/// Every presentation the diff view can be in, in the order `s` cycles them.
///
/// This registry and the one inside it are both here rather than on `Host` for
/// the same structural reason: a `Rows` implementation returns an `AnyElement`,
/// `Host` lives in `core`, and `core` never knows a UI exists. What *is* on
/// `Host` is `layout` — the name of the one to open in, which is data and
/// therefore configurable.
pub struct Layouts(Vec<Layout>);

impl Default for Layouts {
    fn default() -> Self {
        Self::builtin()
    }
}

impl Layouts {
    /// The two shipped presentations. Both go through `register`, so the shipped
    /// configuration uses the seam rather than going around it.
    pub fn builtin() -> Self {
        let mut l = Self(Vec::new());
        l.register("unified", |host| {
            vec![
                Box::new(TextRows::default()),
                // The Markdown metrics come from the host's font: it decides
                // whether tables can be padded into a grid, and the heading
                // scale is relative to the body size rather than a set of pixel
                // constants.
                Box::new(super::markdown::MarkdownRows::new(
                    super::markdown::Metrics::for_font(&host.font),
                    &["md", "markdown", "mdx"],
                )),
            ]
        });
        // No Markdown specialist here on purpose: a rendered document in a
        // 44-character column is worse than its source, and the two-column
        // presentation is already the answer to "show me both versions".
        l.register("split", |_| {
            vec![Box::new(super::split::SplitRows::default())]
        });
        l
    }

    /// Adds a presentation, replacing any already registered under the same
    /// name — so `unified` can be corrected rather than only added to.
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
}

// The order table's row reference and the table itself are
// `gitten_core::rows`': 8 bytes a row, `logical()` for what survives a reflow,
// and the same `widest`/`anchor` a walk of it computes. Only `expand` below is
// this client's, and only because a `Rows` returns an `AnyElement` — see the
// note there.

/// Expands one entry per *logical* row into one per *visual* row.
///
/// `logical` may already be expanded — consecutive entries with the same owner
/// and index are one logical row, and an index is unique within an owner, so the
/// previous table is its own source of truth. That is the whole reason a reflow
/// needs no second table to remember the unwrapped shape: 8 bytes a row, once,
/// however many times the window is dragged.
///
/// Returns the table plus where the file headers landed in it — what `]` and
/// `[` jump between. Core's [`gitten_core::rows::Ordered`] stays as it is because
/// the terminal indexes headers off its own presentations; this client collects
/// them during the same walk rather than search the table per keypress.
fn expand(
    logical: &[RowRef],
    renderers: &[Box<dyn Rows>],
    anchor: Option<RowRef>,
) -> (Ordered, Vec<usize>) {
    let mut order: Vec<RowRef> = Vec::with_capacity(logical.len());
    let mut headers: Vec<usize> = Vec::new();
    let (mut widest, mut widest_at) = (0usize, 0usize);
    let mut found = 0usize;
    let mut i = 0;
    while i < logical.len() {
        let r = logical[i];
        while i < logical.len() && logical[i].logical() == r.logical() {
            i += 1;
        }
        let Some(rows) = renderers.get(r.owner as usize) else {
            continue;
        };
        if anchor.map(RowRef::logical) == Some(r.logical()) {
            found = order.len();
        }
        // One branch per visual row, once per rebuild: where the file headers
        // are is what `]` and `[` jump between, and no presentation has to know
        // a jump list exists.
        if rows.is_header(r.index as usize) {
            headers.push(order.len());
        }
        let n = rows.rows(r.index as usize).clamp(1, u16::MAX as usize);
        for seg in 0..n {
            let w = rows.width(r.index as usize, seg);
            if w > widest {
                (widest, widest_at) = (w, order.len());
            }
            order.push(RowRef {
                owner: r.owner,
                seg: seg as u16,
                index: r.index,
            });
        }
    }
    (
        Ordered {
            order,
            widest: widest_at,
            anchor: found,
        },
        headers,
    )
}

/// This wheel event's delta with the gesture's axis lock applied: what is left on
/// the x axis is the text's to move, and what is left on the y axis is the list's.
///
/// Two rules, and both are what a browser does.
///
/// **A gesture has one axis for its whole life.** `OngoingScroll` is `gpui`'s own
/// lock — the same one `div` uses for a scroll container — and it holds the axis
/// a flick started on until the fingers lift or the deltas turn hard the other
/// way. Without it a diagonal trackpad swipe is horizontal on some of its events
/// and vertical on others, which is the drift that reads as the text sliding at
/// an angle. Deciding per event instead is a coin flip fifty times a second.
///
/// **`shift` means "this one is horizontal"**, and it is applied *before* the
/// lock rather than after: the delta is on the vertical axis, so a lock that saw
/// it first would call the gesture vertical and hand it to the list. Only when
/// the platform has not already done the swap itself, which macOS does for some
/// mice.
pub(crate) fn locked(
    mut delta: Point<Pixels>,
    shift: bool,
    ongoing: &mut OngoingScroll,
    phase: TouchPhase,
) -> Point<Pixels> {
    if shift && delta.x.is_zero() {
        delta = point(delta.y, px(0.));
    }
    ongoing.filter(&mut delta, phase);
    delta
}

/// How far the text has been scrolled sideways, in pixels, and how far it may go.
///
/// The window's `Pen::scroll`. A handle rather than a field on the view because
/// two things write it from outside `render`: a wheel event, which arrives with
/// no `&mut Diff` in reach of the closure that draws a row, and the scrollbar
/// thumb, which is dragged during paint. One `Cell`, copied in and out — the
/// shape `UniformListScrollHandle` already has for the axis it owns.
///
/// `at` is clamped on the way in, so every reader — the rows, the hit test, the
/// scrollbar — sees a value that exists without having to ask.
#[derive(Clone, Default)]
pub struct Pan(Rc<Cell<Panned>>);

#[derive(Clone, Copy, Default)]
struct Panned {
    at: f32,
    max: f32,
    /// The box the rows are drawn in, so the scrollbar knows where to put itself
    /// and how long the thumb is. Written per frame from the list's own bounds,
    /// which is the only element here that has any.
    viewport: Bounds<Pixels>,
}

impl Pan {
    /// Pixels of text off the left edge of the window.
    pub fn at(&self) -> f32 {
        self.0.get().at
    }

    /// Scrolls to an absolute offset. Returns whether it moved, which is what
    /// decides a redraw: a trackpad delivers events long after the text has
    /// stopped being able to move.
    pub fn set(&self, at: f32) -> bool {
        let mut s = self.0.get();
        let at = at.clamp(0.0, s.max);
        if at == s.at {
            return false;
        }
        s.at = at;
        self.0.set(s);
        true
    }

    pub fn by(&self, dx: f32) -> bool {
        self.set(self.0.get().at + dx)
    }

    /// A new bound, and a re-clamp with it: turning wrapping on, or dragging the
    /// window wider, leaves the text scrolled to somewhere that is no longer
    /// there.
    pub fn set_max(&self, max: f32) {
        let mut s = self.0.get();
        s.max = max.max(0.0);
        s.at = s.at.clamp(0.0, s.max);
        self.0.set(s);
    }

    fn set_viewport(&self, viewport: Bounds<Pixels>) {
        let mut s = self.0.get();
        s.viewport = viewport;
        self.0.set(s);
    }
}

/// What the horizontal scrollbar reads and what its thumb writes. The offset is
/// negative in `gpui`'s convention — content pulled left of its origin — and
/// positive in ours, which is the whole of the conversion.
impl ScrollbarHandle for Pan {
    fn viewport_bounds(&self) -> Bounds<Pixels> {
        self.0.get().viewport
    }

    fn offset(&self) -> Point<Pixels> {
        point(px(-self.0.get().at), px(0.))
    }

    fn set_offset(&self, offset: Point<Pixels>) {
        self.set(-f32::from(offset.x));
    }

    fn content_size(&self) -> Size<Pixels> {
        let s = self.0.get();
        size(s.viewport.size.width + px(s.max), s.viewport.size.height)
    }
}

/// What the pane header says about the file the keyboard is in: its path, the
/// change counts its own header row printed, and which hunk of it holds the
/// cursor — the mock's `5 internal/extension/host.go   +18 −6   hunk 1/3`.
///
/// One shape rather than three arguments because the header redraws whole every
/// frame and wants one question answered. The counts are the loaded diff's own,
/// computed by [`prepare`](gitten_core::prepared::prepare) out of `LineKind`
/// before anything here ran — which is also why they can never disagree with
/// what the file's drawn header shows: there is one copy of the numbers, and
/// both places read it.
///
/// `PartialEq`, because the shell memoises the header's spelled-out strings
/// against the last summary it drew: equal means nothing to re-spell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSummary {
    /// As the loaded diff spells it — the same string hunk staging resolves
    /// by, so a header that copies it acts on exactly what it names.
    pub path: String,
    pub adds: u64,
    pub dels: u64,
    /// 1-based index of the hunk under the keyboard, within its file.
    /// `0` when the file has no hunks.
    pub hunk: usize,
    pub hunks: usize,
}

pub struct Diff {
    /// The parsed diff, kept so a layout change can rebuild the rows.
    ///
    /// This is the memory cost of a live toggle, and it is a real one: on the
    /// 714k-line fixture it is a second copy of every line. The alternatives are
    /// worse — cloning the *prepared* diff pays the same memory plus the clone at
    /// load, whether or not anybody ever presses the key, and re-acquiring means
    /// the view needs a repository, which it does not have and should not.
    files: Rc<Vec<FileDiff>>,
    /// The expensive half, run once per diff and kept: clip, intraline and
    /// syntax behind an `Rc`, so a layout toggle pays [`arrange`] — renderer
    /// selection, the order table, one clone per drawn file — and not the two
    /// passes over every line. Rebuilt only where the diff itself changes:
    /// [`Diff::swap`] and [`Diff::replace_prepared`].
    prepared: Rc<Prepared>,
    layouts: Rc<Layouts>,
    current: usize,
    /// Which entry of `host.wrap` is in use.
    ///
    /// The view's own pick, not the host's, for the same reason `current` is:
    /// `Host` is rebuilt from defaults on every save of the config file, so a
    /// field on it would be reset by an unrelated edit. What the file says is
    /// what this *opens* on — see [`Diff::with_layouts`].
    wrap: usize,
    /// The width and wrap the rows were last expanded for. A resize that does
    /// not cross a character boundary compares equal here and stops.
    applied: (f32, &'static str),
    /// The font the row tables were built against, seeded at construction with
    /// the host the renderers were arranged against — the first settled frame
    /// therefore has nothing to rebuild. `Font` is plain data deriving PartialEq,
    /// so a value comparison is the fingerprint; a mismatch means the metrics
    /// the renderers were built with no longer describe what will be drawn.
    font_applied: Option<Font>,
    /// The view's own width in pixels, written during paint by the probe in
    /// [`Diff::render`] and read on the frame after. There is no way to know it
    /// earlier: a view is handed its box by whatever assembled it, and this one
    /// does not assume it owns the window.
    measured: Rc<Cell<f32>>,
    /// Mutable because a resize reflows in place. `RefCell` and not `Rc::get_mut`
    /// because the render closure holds a clone for as long as the element tree
    /// does; one borrow per *batch* of rows, not per row.
    renderers: Rc<RefCell<Vec<Box<dyn Rows>>>>,
    order: Rc<Vec<RowRef>>,
    /// Where each hunk starts, as a fraction of the row order — the diff
    /// scrollbar's ticks. Computed beside the order table it indexes and
    /// rebuilt exactly where the order is: load, reflow, a layout swap and a
    /// reload. See [`hunk_marks`].
    marks: Rc<Vec<f32>>,
    /// What the mouse has selected, or nothing.
    ///
    /// The model is `gitten_core::select` and this is the only state the window
    /// keeps: a caret is a logical row and a byte, so it survives a reflow, and
    /// the render path turns it into a byte range per visible row in two
    /// comparisons. Cleared by a layout change and by a new diff, because the
    /// rows it was anchored to are then somebody else's.
    sel: Option<Selection>,
    /// True between mouse-down and mouse-up, so a move extends the selection
    /// rather than doing nothing at all.
    dragging: bool,
    /// Index into `order` of the widest visual row: the one row the horizontal
    /// bound is taken from, because it is the one there is furthest to scroll to.
    widest: usize,
    /// The logical row an armed hunk-discard was aimed at, if a question is
    /// standing. Any move of the keyboard, wheel or refresh of the diff
    /// clears it — see [`Diff::confirm_or_arm_discard_hunk`] — so a press
    /// can never spend an arm on a hunk it was not asked about.
    armed_hunk: Option<(u16, u32)>,
    /// Whether this pane holds the keyboard, as the shell last told it. A
    /// row's cursor bar is accent only when its pane is focused, and the view
    /// cannot ask the shell during render — so the shell writes it here when
    /// focus moves, and render reads a flag.
    focused: bool,
    /// Where every file header is, in visual rows — what `]` and `[` jump
    /// between. Collected by [`expand`] while it builds the order table, so it
    /// costs one branch per row at rebuild and nothing per frame.
    headers: Rc<Vec<usize>>,
    scroll: UniformListScrollHandle,
    /// The horizontal axis, which is this view's and not the list's — see the
    /// module note. Bounded from `widest` on every reflow.
    pan: Pan,
    /// The cursor, the top row and the height, and nothing else about them. The
    /// keyboard's position in this diff, from [`gitten_core::view::Viewport`] —
    /// the same model the terminal holds, so a key means the same thing in both.
    ///
    /// Behind a shared cell because the render closure reads it per batch, which
    /// is also why it is not folded into [`Diff::top`]: that one is written *by*
    /// the list, this one is what moves the list.
    view: Rc<Cell<Viewport>>,
    /// The vertical offset this view last wrote. A scrollbar thumb writes the
    /// same offset without coming through here, and that mismatch — not the
    /// position itself — is what [`Diff::reconcile`] treats as "the list moved".
    synced: Rc<Cell<f32>>,
    /// A strict row waiting for prepaint, plus exact wheel pixels meanwhile.
    pending_scroll: PendingScroll,
    pub rendered: Rc<Cell<usize>>,
    /// Rows that exist, live: wrapping changes it on every resize, so an
    /// overlay reading a number taken at load would be describing the diff as it
    /// was one window ago.
    pub total: Rc<Cell<usize>>,
    /// What wrapping is currently doing, for the overlay. Written on reflow.
    pub note: Rc<RefCell<SharedString>>,
    /// First visible row, written on every batch the list asks for. Read by the
    /// session so a restart can put you back on it — see `session.rs`.
    pub top: Rc<Cell<usize>>,
    pub load: String,
}

impl Diff {
    /// Rows that exist right now. The overlay reads [`Diff::total`] through a
    /// cell instead, because wrapping moves this on every resize and nothing
    /// pushes a new number at it; this is for the tests and for anything that
    /// asks once.
    #[allow(dead_code)]
    pub fn total(&self) -> usize {
        self.order.len()
    }

    /// Every wrap registered, in the order `w` cycles them, and which one is on.
    /// What the title-bar control lists — the same shape as the layout picker,
    /// because it is the same control.
    pub fn wrap_names(&self, host: &Host) -> Vec<&'static str> {
        host.wrap.names()
    }

    pub fn wrap_index(&self) -> usize {
        self.wrap
    }

    /// Loads a wrap by index. Out of range is ignored rather than clamped, for
    /// the same reason [`Diff::set_layout`] ignores one.
    pub fn set_wrap(&mut self, index: usize, host: &Host, cx: &mut Context<Self>) {
        if index >= host.wrap.len() || index == self.wrap {
            return;
        }
        self.wrap = index;
        cx.notify();
    }

    /// Re-expands the rows for a new width, keeping the line you were reading at
    /// the top.
    ///
    /// Called from `render` with the width measured on the previous frame, so it
    /// runs on a resize and on a wrap change and at no other time. Two ways out
    /// before it does any work, in increasing cost: nothing moved, and no
    /// implementation's row count actually changed.
    ///
    /// A presentation is told the width **whatever the wrap is doing**, and the
    /// cheap path for a wrap that never breaks lines is the implementation's —
    /// see `TextRows::reflow`. It has to be: with wrapping off the width is still
    /// what decides how wide a side-by-side column is, how long a Markdown rule
    /// is, and how far there is to scroll, and a presentation told nothing
    /// answers all three from the window it had two resizes ago.
    ///
    /// What it costs when it does run is a rescan of the text — 1–3 ms on the
    /// real fixtures and 26 ms on the 714k-line one, at 36–52 ns a line — plus a
    /// new order table. It deliberately does *not* re-run `prepare`: that is
    /// 241 ms on the same fixture, and a resize drag would be a slideshow. See
    /// `docs/measurements.md`.
    fn reflow(&mut self, width: f32, host: &Host) {
        // Before the width exit, not beside it: a font edit reshapes every
        // glyph without moving the width, and would otherwise survive until the
        // next resize happened to cross a boundary. The price is one
        // `Option<Font>` compare — still O(1) on the common path, which is what
        // the resize test below pins.
        if self.font_applied.as_ref() != Some(&host.font) {
            self.font_applied = Some(host.font.clone());
            // Reset first: arrange() has already been given today's host, and the
            // width half of `applied` must re-fire on the rebuilt renderers.
            self.applied = (0.0, "");
            // Same presentation before and after — only the glyph metrics moved — so
            // unlike a layout *change* the selection and the exact cursor row both
            // still mean something. `apply_layout` is written for the change and
            // drops both (a fraction of the old row count, no selection); stash them
            // and hand them back. Sound only because `apply_layout` leaves fresh
            // renderers with `applied` reset, so the `changed` branch below always
            // runs in this same call and re-resolves both against the rebuilt order
            // table — this is not restoring stale state.
            //
            // `armed_hunk` is not carried the same way: it is a pending
            // *destructive* action, and making someone re-arm a discard after a
            // config reload is the safe direction to be wrong in, unlike a
            // selection.
            let keep = self.sel.take();
            let cursor = self.view.get().cursor();
            self.apply_layout(self.current, host);
            let mut v = self.view.get();
            v.go_to(cursor.min(self.order.len().saturating_sub(1)));
            self.view.set(v);
            self.defer_show(v);
            self.sel = keep;
        }
        let wrap = host.wrap.at(self.wrap);
        if (width, wrap.name()) == self.applied || width <= 0.0 {
            return;
        }
        self.applied = (width, wrap.name());

        let changed = {
            let mut rs = self.renderers.borrow_mut();
            rs.iter_mut()
                .fold(false, |acc, r| r.reflow(width, host, wrap) | acc)
        };
        if changed {
            // Anchored to the logical row under the **cursor**, not to a
            // proportion and not to whatever happens to be at the top: a reflow
            // is the same diff at a different width, so every line still exists,
            // and the one being read is the cursor's. A layout change has no
            // such correspondence, which is why it uses a fraction instead.
            let anchor = self.order.get(self.view.get().cursor()).copied();
            let (built, headers) = expand(&self.order, &self.renderers.borrow(), anchor);
            let logical = self
                .renderers
                .borrow()
                .iter()
                .map(|r| r.len())
                .sum::<usize>();
            self.order = Rc::new(built.order);
            self.widest = built.widest;
            self.headers = Rc::new(headers);
            self.marks = Rc::new(hunk_marks(&self.order, &self.renderers.borrow()));
            self.total.set(self.order.len());
            // The line you were reading is wherever the cursor now is — its row
            // number moved with the wrapping, which is what `built.anchor`
            // found — and the viewport follows it, exactly as any cursor move
            // does.
            let mut v = self.view.get();
            v.set_len(self.order.len());
            v.go_to(built.anchor);
            self.view.set(v);
            // Deferred, not written: the list has not laid out the new row
            // count, and its bound is the old shape's. See `defer_show`.
            self.defer_show(v);
            // After the order table, because the bound is the widest row's and
            // that is what just moved. Said out loud when it is not zero: "the
            // diff fits" and "there is a kilometre of it off the right of the
            // screen" look identical until something scrolls.
            let bound = self.bound(width, host);
            *self.note.borrow_mut() = format!(
                "{} · {:.0} px · {} rows / {logical} lines{}",
                wrap.name(),
                width,
                self.order.len(),
                match bound > 0.0 {
                    true => format!(" · {bound:.0} px right"),
                    false => String::new(),
                }
            )
            .into();
            // The rows are the same rows at different heights, so the selection
            // survives — but every visual row it cached has moved, which is what
            // `resolve` rebuilds. A drag *through* a reflow cannot happen: the
            // width only changes when the mouse is on the window's edge.
            if let Some(sel) = &mut self.sel {
                if !sel.resolve(&self.order) {
                    self.sel = None;
                }
            }
        }
        // Last, and on every path: the bound is a function of the width, of the
        // wrap and of which row came out widest, and the row count changing is
        // only one of the three.
        self.pan.set_max(self.bound(width, host));
    }

    /// How far the text may be scrolled sideways, in pixels.
    ///
    /// The widest row and no other, which is the same row `expand` already found
    /// while walking the order table: once its last character is on screen there
    /// is nothing left anywhere in the diff to scroll to.
    fn bound(&self, width: f32, host: &Host) -> f32 {
        let rs = self.renderers.borrow();
        self.order.get(self.widest).map_or(0.0, |r| {
            rs[r.owner as usize].overflow(r.index as usize, r.seg as usize, width, host)
        })
    }

    // -------------------------------------------------------------- commands

    /// The box the row list is drawn in — what a wheel event over the window is
    /// hit-tested against. Zero until the first paint.
    pub fn list_bounds(&self) -> Bounds<Pixels> {
        self.scroll.0.borrow().base_handle.bounds()
    }

    /// Moves the text sideways by `dx` pixels. The wheel's horizontal half,
    /// routed through [`crate::main`]'s axis lock; `h` and `l` arrive as columns
    /// via [`Diff::pan_columns`].
    ///
    /// Returns whether anything moved, which is what decides a redraw.
    pub fn pan_pixels(&self, dx: f32) -> bool {
        self.pan.by(dx)
    }

    /// Moves the text sideways by `columns` characters. `view.left`,
    /// `view.right` — the terminal's eight columns, in this client's unit.
    pub fn pan_columns(&mut self, columns: isize, host: &Host) {
        self.pan_pixels(columns as f32 * host.font.char_width());
    }

    /// The viewport model with everything live folded in: the list's length,
    /// the height last measured, and `[view] scrolloff` as the file has it
    /// *now*. Every path that moves or reads the view starts from here, so a
    /// reloaded config reaches the next keypress instead of the next launch.
    fn live_view(&self, host: &Host) -> Viewport {
        let mut v = self.view.get();
        v.set_len(self.order.len());
        v.set_height(self.rendered.get());
        v.set_scrolloff(host.view.scrolloff);
        v
    }

    /// Moves the list by `dy` pixels without translating it into rows first.
    ///
    /// The wheel reports pixels, not rows, and this is what keeps it *smooth*:
    /// the command it resolves to (`view.scroll-up`, from `[keys]`) says what the
    /// wheel does; the event's own delta says how far. A key repeat has no delta
    /// and uses [`Diff::run_view`] like every other command.
    ///
    /// A glance, not a commitment: the viewport pans and the keyboard
    /// selection stays where it was, exactly as the terminal's `pan_by`
    /// does — the selected row is not necessarily in view.
    pub fn scroll_pixels(&mut self, dy: f32, host: &Host) -> bool {
        let deferred = self.scroll.0.borrow().deferred_scroll_to_item;
        if let Some(request) = deferred {
            if self.pending_scroll.is_awaiting() {
                let pixels = self.pending_scroll.wheel(dy);
                let mut v = self.live_view(host);
                let y = -(request.item_index as f32 * ROW_H) + pixels;
                v.pan_to((-y / ROW_H).round().max(0.0) as usize);
                self.view.set(v);
                self.top.set(v.top());
                // The wheel is also a move of attention.
                self.armed_hunk = None;
                return true;
            }
            // Selection autoscroll parks its own non-strict request. A newer
            // wheel cancels it and follows the ordinary live-pixel path rather
            // than accumulating into state that does not own that request.
            self.scroll.0.borrow_mut().deferred_scroll_to_item = None;
        }
        let (offset, max) = {
            let s = self.scroll.0.borrow();
            (s.base_handle.offset(), s.base_handle.max_offset())
        };
        let y = (f32::from(offset.y) + dy).clamp(-f32::from(max.y), 0.0);
        if y == f32::from(offset.y) {
            return false;
        }
        self.scroll
            .0
            .borrow()
            .base_handle
            .set_offset(point(offset.x, px(y)));
        // The top row the pixels landed on, panned to — the selection stays.
        let mut v = self.live_view(host);
        v.pan_to((-y / ROW_H).round().max(0.0) as usize);
        self.view.set(v);
        self.synced.set(y);
        // The wheel is also a move of attention — same rule the arrow keys keep.
        self.armed_hunk = None;
        true
    }

    /// Meets the list where it actually is: a scrollbar drag moves the offset
    /// without touching the selection, and the next key acts from the offset
    /// now on screen.
    ///
    /// [`Diff::synced`] is what separates "the list moved under us" from "we
    /// moved the list": only a mismatch counts, so two commands in a row do not
    /// fight each other through this method.
    pub fn reconcile(&mut self, host: &Host) {
        if self.scroll.0.borrow().deferred_scroll_to_item.is_some() {
            return;
        }
        let shown_y = f32::from(self.scroll.0.borrow().base_handle.offset().y);
        if (shown_y - self.synced.get()).abs() < 0.5 {
            return;
        }
        self.synced.set(shown_y);
        let shown = (-shown_y / ROW_H).round().max(0.0) as usize;
        let mut v = self.live_view(host);
        if v.top() == shown {
            return;
        }
        v.pan_to(shown);
        self.view.set(v);
    }

    /// Runs one of the `view.*` commands against the viewport, keeping the list
    /// and the saved position honest afterwards. The same names the terminal
    /// dispatches; [`Viewport`] is the part that must not differ. The `diff.*`
    /// family rides the same method: one screen, one place its commands live.
    ///
    /// False is "not one of mine", and the caller says so.
    pub fn run_view(&mut self, command: &str, host: &Host) -> bool {
        // First, meet the list where it actually is: a scrollbar drag moved the
        // offset without touching the cursor, and the next key should act on
        // what is on screen now.
        self.reconcile(host);
        let mut v = self.live_view(host);
        match command {
            "view.down" => v.down(),
            "view.up" => v.up(),
            "view.page-down" => v.page(1),
            "view.page-up" => v.page(-1),
            "view.scroll-down" => v.pan_by(host.view.rows as isize),
            "view.scroll-up" => v.pan_by(-(host.view.rows as isize)),
            "view.top" => v.to_top(),
            "view.bottom" => v.to_bottom(),
            "view.left" => {
                let _ = v;
                self.pan_columns(-8, host);
                return true;
            }
            "view.right" => {
                let _ = v;
                self.pan_columns(8, host);
                return true;
            }
            "diff.next-file" => {
                let _ = v;
                self.jump_file(1, host);
                return true;
            }
            "diff.prev-file" => {
                let _ = v;
                self.jump_file(-1, host);
                return true;
            }
            "diff.cycle-layout" => {
                let _ = v;
                // A single-presentation registry has nothing to cycle to, which
                // is what [`Layouts::len`] says.
                if self.layouts.len() >= 2 {
                    self.apply_layout((self.current + 1) % self.layouts.len(), host);
                }
                // The rows are about to be re-arranged; whatever the question
                // was armed against may land somewhere else in them.
                self.armed_hunk = None;
                return true;
            }
            "diff.cycle-wrap" => {
                let _ = v;
                if host.wrap.len() >= 2 {
                    self.wrap = (self.wrap + 1) % host.wrap.len();
                }
                // The rows are about to re-expand; whatever the question was
                // armed against may land somewhere else in them.
                self.armed_hunk = None;
                return true;
            }
            _ => return false,
        }
        // The keyboard moved. Whatever an armed discard was asked about was
        // where the keyboard used to be — same rule as the working-tree pane.
        self.armed_hunk = None;
        self.view.set(v);
        self.show(v);
        true
    }

    /// Puts row `v.top()` at the top of the viewport — exactly, not "if it is
    /// already visible": the margin arithmetic is [`Viewport::follow`]'s, and
    /// re-doing it here would be doing it differently.
    ///
    /// Direct offset when geometry exists; when a deferred request is still
    /// parked, replace its target instead. Clearing it and writing immediately
    /// would clamp against the old row count that made deferral necessary.
    fn show(&self, v: Viewport) {
        let target = v.top();
        if self.scroll.0.borrow().deferred_scroll_to_item.is_some() {
            self.defer_show(v);
            return;
        }
        let s = self.scroll.0.borrow();
        let cur = s.base_handle.offset();
        let y = -(target as f32 * ROW_H).clamp(0.0, f32::from(s.base_handle.max_offset().y));
        s.base_handle.set_offset(point(cur.x, px(y)));
        self.synced.set(y);
        self.top.set(target);
    }

    /// [`Diff::show`] against geometry that does not exist yet.
    ///
    /// A cursor-preserving reflow has just changed how many rows there are,
    /// which means the list's own bound — what [`Diff::show`] clamps against —
    /// still describes the *old* shape: narrower rows means more of them, and
    /// a deep cursor needs more offset than the old maximum allows, so writing
    /// now would clamp it back on screen-edge and record the wrong place in
    /// [`Diff::synced`]. GPUI's deferred request is the fix: it is consumed by
    /// the list's own prepaint, after it has measured the new row count, and
    /// **strict**, so it lands exactly where the model says even if that row
    /// would have been visible somewhere else.
    ///
    /// The offset itself is deliberately left alone until then; [`Diff::top`]
    /// says where the list is about to sit, and [`Diff::reconcile`] meets the
    /// real number once prepaint has written it.
    fn defer_show(&self, v: Viewport) {
        let target = v.top();
        self.pending_scroll.begin();
        self.scroll
            .scroll_to_item_strict(target, ScrollStrategy::Top);
        self.top.set(target);
    }

    /// The header of the next or previous file. `]` and `[`, tab and backtab.
    pub fn jump_file(&mut self, by: isize, host: &Host) {
        let mut v = self.live_view(host);
        let cursor = v.cursor();
        // Binary search rather than a scan: a 5,953-file diff is a realistic
        // input and this is a keypress. Same walk as the terminal's.
        let target = match by.is_negative() {
            true => self
                .headers
                .partition_point(|&h| h < cursor)
                .checked_sub(1)
                .and_then(|i| self.headers.get(i))
                .copied(),
            false => self
                .headers
                .get(self.headers.partition_point(|&h| h <= cursor))
                .copied(),
        };
        if let Some(t) = target {
            v.go_to(t);
            self.view.set(v);
            // A file jump is a move of the keyboard; see `run_view`'s tail.
            self.armed_hunk = None;
            self.show(v);
        }
    }

    /// Where the keyboard is. What `copy.selection` falls back to and the tests
    /// assert against.
    ///
    /// `dead_code` for the binary — dispatch reads the cursor through the shared
    /// viewport cell — and live in the tests, which is what it is here for. A
    /// binary crate does not count a test as a use.
    #[allow(dead_code)]
    pub fn cursor(&self) -> usize {
        self.view.get().cursor()
    }

    /// The logical row the keyboard is on: `(owner, index)`, the identity a
    /// question is armed against.
    pub(crate) fn cursor_row_id(&self) -> (u16, u32) {
        self.order
            .get(self.view.get().cursor())
            .map(|r| r.logical())
            .unwrap_or((u16::MAX, u32::MAX))
    }

    /// The hunk under the keyboard, as the loaded diff holds it: its file's
    /// path and the [`Hunk`](gitten_core::Hunk) itself, with every line and
    /// both sides' numbers — exactly what patch synthesis needs. `None` when
    /// the keyboard sits on a file header or an empty diff; a presentation
    /// that draws no hunks answers none for the whole view.
    ///
    /// The caller meets the list where the last drag left it first — see
    /// [`Diff::reconcile`].
    pub fn current_hunk(&self) -> Option<(String, gitten_core::Hunk)> {
        let r = *self.order.get(self.view.get().cursor())?;
        let renderers = self.renderers.borrow();
        let (path, hunk_no) = renderers.get(r.owner as usize)?.hunk_at(r.index as usize)?;
        let file = self.files.iter().find(|f| f.path == path)?;
        Some((path.to_string(), file.hunks.get(hunk_no)?.clone()))
    }

    /// Where the keyboard is, as the pane header names it. `None` when nothing
    /// on screen answers: an empty diff, a cursor past the end, or a row the
    /// presentation drew outside both vocabularies — a rendered document's
    /// body rows are nobody's hunk and nobody's header.
    ///
    /// A hunk row answers through [`Rows::hunk_at`], the same address `space`
    /// stages against, so the header and the staging question can never point
    /// at different hunks. A file-header row owns no hunk — the spans open
    /// below it — but it still names its file, through [`Rows::selectable`]:
    /// a header's copyable text *is* its path (that is what makes a selection
    /// across files paste readable), so the accessor every presentation already
    /// implements for copying is the one place they all spell the path. One
    /// lookup against the prepared diff either way; once per frame, not per row.
    pub fn file_summary(&self) -> Option<FileSummary> {
        let r = *self.order.get(self.view.get().cursor())?;
        let index = r.index as usize;
        let renderers = self.renderers.borrow();
        let rows = renderers.get(r.owner as usize)?;

        // Which file the keyboard is over, and which of its hunks is under it.
        // The latter stays absent when no hunk is under it: whether such a
        // file has any first hunk to name is the file's own fact, read below.
        let located = match rows.hunk_at(index) {
            Some((path, n)) => Some((path, Some(n))),
            None if rows.is_header(index) => {
                // Wrapping adds visual rows and never changes the logical one
                // `index` names, so this covers the wrapped tail of a hunk
                // line exactly as it does the line itself.
                rows.selectable(index, 0).map(|p| (p, None))
            }
            _ => None,
        }?;
        let f = self.prepared.files.iter().find(|f| f.path == located.0)?;
        Some(FileSummary {
            path: located.0.to_string(),
            adds: f.adds as u64,
            dels: f.dels as u64,
            // The map addresses hunks from zero because that is how the
            // presentations enumerate them; people count from one.
            hunk: located
                .1
                .map_or_else(|| usize::from(!f.hunks.is_empty()), |n| n + 1),
            hunks: f.hunks.len(),
        })
    }

    /// Arms — or confirms — a discard of the hunk on logical row `id`. The
    /// first call stores it and returns false: ask, don't act. A second call
    /// carrying the same id has the keyboard still sitting where the question
    /// was asked, and spends the arm.
    pub(crate) fn confirm_or_arm_discard_hunk(&mut self, id: (u16, u32)) -> bool {
        match self.armed_hunk {
            Some(armed) if armed == id => {
                self.armed_hunk = None;
                true
            }
            _ => {
                self.armed_hunk = Some(id);
                false
            }
        }
    }

    /// Told by the shell whenever the keyboard moves — never decided here.
    pub(crate) fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }

    /// Whether this pane holds the keyboard. The rows read it for the bar.
    #[allow(dead_code)]
    pub(crate) fn focused(&self) -> bool {
        self.focused
    }

    /// The text of the row the keyboard is on, or nothing past either end. The
    /// fallback half of `copy.selection`.
    pub fn cursor_text(&self) -> String {
        let v = self.view.get();
        let r = self.order.get(v.cursor()).copied();
        let renderers = self.renderers.borrow();
        r.and_then(|r| {
            renderers
                .get(r.owner as usize)
                .and_then(|rows| rows.selectable(r.index as usize, 0).map(str::to_string))
        })
        .unwrap_or_default()
    }

    /// Which presentation is loaded. Read by the tests and by anything that
    /// wants to name it; the control strip asks for the index and the list.
    #[allow(dead_code)]
    pub fn layout(&self) -> &'static str {
        self.layouts
            .names()
            .get(self.current)
            .copied()
            .unwrap_or("custom")
    }

    /// Every presentation registered, in the order `s` cycles them. What a
    /// control strip lists.
    pub fn layout_names(&self) -> Vec<&'static str> {
        self.layouts.names()
    }

    pub fn layout_index(&self) -> usize {
        self.current
    }

    /// Loads a presentation by index, keeping the reading position. Out of range
    /// is ignored rather than clamped: the index came from a list this view
    /// published, so a stale one means the list moved and picking a neighbour
    /// would be a guess.
    pub fn set_layout(&mut self, index: usize, host: &Host, cx: &mut Context<Self>) {
        if index >= self.layouts.len() || index == self.current {
            return;
        }
        self.apply_layout(index, host);
        cx.notify();
    }

    /// Swaps the diff itself, keeping the presentation and the reading position.
    ///
    /// What changing the algorithm does. The rows are rebuilt from stage 3 the
    /// same way a layout change rebuilds them; the only difference is that the
    /// `FileDiff`s underneath are new ones.
    pub fn replace(&mut self, files: Vec<FileDiff>, host: &Host, cx: &mut Context<Self>) {
        self.reconcile(host);
        if self.files.as_slice() == files.as_slice() {
            return;
        }
        let prepared = prepare_files(&files, host);
        self.swap_prepared(files, prepared, host);
        cx.notify();
    }

    /// [`Diff::replace`] with the pure preparation already completed off the
    /// GPUI thread by a pane refresh.
    pub(crate) fn replace_prepared(
        &mut self,
        files: Vec<FileDiff>,
        prepared: Prepared,
        host: &Host,
        cx: &mut Context<Self>,
    ) {
        self.reconcile(host);
        if self.files.as_slice() == files.as_slice() {
            return;
        }
        self.swap_prepared(files, prepared, host);
        cx.notify();
    }

    /// The half of [`Diff::replace`] that needs no window, and therefore the
    /// half with tests.
    #[cfg(test)]
    fn swap(&mut self, files: Vec<FileDiff>, host: &Host) {
        let prepared = prepare_files(&files, host);
        self.swap_prepared(files, prepared, host);
    }

    fn swap_prepared(&mut self, files: Vec<FileDiff>, prepared: Prepared, host: &Host) {
        let old = self.view.get();
        let cursor = old.cursor();
        let top = old.top();
        let pan = self.pan.at();
        self.files = Rc::new(files);
        self.sel = None;
        self.dragging = false;
        // A refresh is the repository saying things moved; an armed discard
        // was a promise about how they were, so it dies here first.
        self.armed_hunk = None;
        self.prepared = Rc::new(prepared);
        let built = arrange(&self.prepared, host, &self.layouts, self.current);
        self.order = Rc::new(built.order);
        *self.renderers.borrow_mut() = built.renderers;
        self.widest = built.widest;
        self.headers = Rc::new(built.headers);
        self.marks = Rc::new(built.marks);
        self.load = built.load;
        self.total.set(self.order.len());
        self.applied = (0.0, "");

        let mut view = old;
        view.set_len(self.order.len());
        view.go_to(cursor);
        view.scroll_to(top);
        self.view.set(view);
        self.pan.set_max(self.bound(self.measured.get(), host));
        self.pan.set(pan);
        if self.order.is_empty() {
            self.pending_scroll.cancel();
            let mut state = self.scroll.0.borrow_mut();
            state.deferred_scroll_to_item = None;
            state.base_handle.set_offset(point(px(0.0), px(0.0)));
            self.synced.set(0.0);
            self.top.set(0);
        } else {
            self.defer_show(view);
        }
    }

    /// Puts a saved row back at the top of the viewport, with the keyboard on
    /// it. Clamped rather than validated: the diff may be shorter than it was
    /// when the position was taken — a rebuild is usually a code change, but
    /// nothing stops the working tree having moved too.
    ///
    /// The viewport model is filled in **first** — length, measured height, and
    /// the live `[view] scrolloff` — because a restore lands on a view that has
    /// never been laid out: without it, `go_to` would clamp a saved row 4,102
    /// against a list the model still believes is empty, and the first frame
    /// would open at row zero no matter what was restored.
    ///
    /// And **strict**, deferred to the list's own prepaint: the non-strict
    /// strategy skips scrolling for a row already inside the initial viewport,
    /// so a saved row 5 of a tall window would leave GPUI parked at row zero
    /// while the model and the session both claimed 5 — and every later
    /// reconcile would then read that lie back as the truth. Strict puts row
    /// `row` at the top, whatever was there.
    pub fn scroll_to(&self, row: usize, host: &Host) {
        if self.order.is_empty() {
            return;
        }
        let row = row.min(self.order.len() - 1);
        let mut v = self.live_view(host);
        v.scroll_to(row);
        self.view.set(v);
        self.defer_show(v);
    }

    pub fn go_to(&self, row: usize, host: &Host) {
        let mut v = self.live_view(host);
        v.go_to(row);
        self.view.set(v);
    }

    /// The shipped set: the registry of presentations, opened on whichever one
    /// the host names. An unknown name falls back to the first rather than
    /// failing — the config layer is what reports it, because it is the layer
    /// that knows it came from a file somebody is editing.
    pub fn new(files: Vec<FileDiff>, host: Rc<Host>, _cx: &mut Context<Self>) -> Self {
        Self::with_layouts(files, &host, Layouts::builtin())
    }

    /// One presentation, pinned: no registry, so nothing to cycle to.
    ///
    /// `renderers[0]` is the fallback and must claim every path; later entries
    /// are specialists and win over earlier ones. This is the entry point an
    /// extension uses to install its own set — see `docs/extending.md`.
    ///
    /// `dead_code` because nothing in the binary calls it: the shipped
    /// configuration goes through `Layouts`, and this is the seam plus the tests
    /// that prove a second set fits. A binary crate does not count a test as a
    /// use.
    #[allow(dead_code)]
    pub fn with_renderers(
        files: Vec<FileDiff>,
        host: Rc<Host>,
        renderers: Vec<Box<dyn Rows>>,
    ) -> Self {
        let mut layouts = Layouts(Vec::new());
        // Moved into the closure through a cell, because `build` may be called
        // more than once in general and here can only be called once. A pinned
        // presentation has nothing to switch to, so once is all it gets — and if
        // it is somehow asked twice, the fallback rather than an empty list,
        // because `assemble` indexes `renderers[0]`.
        let once = std::cell::RefCell::new(Some(renderers));
        layouts.register("custom", move |_| {
            once.borrow_mut()
                .take()
                .filter(|r| !r.is_empty())
                .unwrap_or_else(|| vec![Box::new(TextRows::default()) as Box<dyn Rows>])
        });
        Self::with_layouts(files, &host, layouts)
    }

    /// The general form: a registry, and the host's chosen entry out of it.
    pub fn with_layouts(files: Vec<FileDiff>, host: &Host, layouts: Layouts) -> Self {
        // Reported here and not by the config layer, because this is the layer
        // that holds the registry: `core` cannot see a `Rows` implementation and
        // an extension may have registered a name the config layer never heard
        // of. Falling back rather than failing — a typo in a live-reloaded file
        // must not leave you with no diff.
        let current = match layouts.position(&host.layout) {
            Some(i) => i,
            None => {
                if crate::stats::enabled() {
                    eprintln!(
                        "gitten: unknown diff.layout {:?}; registered: {}",
                        host.layout,
                        layouts.names().join(", ")
                    );
                }
                0
            }
        };
        let files = Rc::new(files);
        let prepared = Rc::new(prepare_files(&files, host));
        let built = arrange(&prepared, host, &layouts, current);
        // The host names the wrap this opens on, exactly as it names the layout.
        // An unknown name is reported by the config layer, which is the layer
        // that knows it came from a file somebody is editing.
        let wrap = host.wrap.selected_index();
        let total = Rc::new(Cell::new(built.order.len()));
        let view = Viewport::new();
        Self {
            files,
            prepared,
            layouts: Rc::new(layouts),
            current,
            wrap,
            applied: (0.0, ""),
            // Arranged above against this very host, so its font is already
            // on the rows — recording anything else makes the first settled
            // reflow pay a redundant second arrange.
            font_applied: Some(host.font.clone()),
            measured: Rc::new(Cell::new(0.0)),
            renderers: Rc::new(RefCell::new(built.renderers)),
            order: Rc::new(built.order),
            marks: Rc::new(built.marks),
            sel: None,
            dragging: false,
            widest: built.widest,
            armed_hunk: None,
            focused: false,
            headers: Rc::new(built.headers),
            scroll: UniformListScrollHandle::new(),
            pan: Pan::default(),
            view: Rc::new(Cell::new(view)),
            synced: Rc::new(Cell::new(0.0)),
            pending_scroll: PendingScroll::default(),
            rendered: Rc::new(Cell::new(0)),
            total,
            note: Rc::new(RefCell::new(SharedString::default())),
            top: Rc::new(Cell::new(0)),
            load: built.load,
        }
    }

    /// Rebuilds the rows for `index`, keeping the reading position. The half of
    /// a layout cycle and [`Diff::replace`] that needs no window, and therefore
    /// the half with tests.
    fn apply_layout(&mut self, index: usize, host: &Host) {
        let fraction = self.view.get().progress();
        self.current = index;
        // Every row about to be replaced, so a selection anchored to one of them
        // would be pointing at whatever now has its index. There is no honest
        // way to carry a selection across two presentations of the same diff —
        // a replace pair is one row here and two there — so it goes.
        self.sel = None;
        // An armed discard rides the same logic: the row it was asked about
        // is about to have a different meaning.
        self.armed_hunk = None;
        let built = arrange(&self.prepared, host, &self.layouts, index);
        self.order = Rc::new(built.order);
        *self.renderers.borrow_mut() = built.renderers;
        self.widest = built.widest;
        self.marks = Rc::new(built.marks);

        self.load = built.load;
        self.total.set(self.order.len());
        // Fresh implementations hold no wrap, so the next frame reflows them.
        // Left to that rather than done here, because the width belongs to the
        // window and this half of a layout change is the half with no window.
        self.applied = (0.0, "");
        let mut v = self.view.get();
        v.set_len(self.order.len());
        v.go_to_fraction(fraction);
        self.view.set(v);
        // A presentation swap is a new row count too — split merges a replace
        // pair onto one row — so the same rule as a reflow: the position lands
        // when the list has measured what it now holds.
        self.defer_show(v);
    }
}

/// What one pass of stages 3–5 produces.
struct Built {
    renderers: Vec<Box<dyn Rows>>,
    order: Vec<RowRef>,
    widest: usize,
    /// Where each file header landed in visual rows — what jump-to-file and
    /// the widest-row search read. Produced by the same [`expand`] that built
    /// `order`, so it can never disagree with it.
    headers: Vec<usize>,
    /// Where each hunk starts, as a fraction of the order — [`Diff::marks`]'s
    /// source. Produced beside the order it indexes, by the same [`expand`]
    /// pass, so the two can never disagree.
    marks: Vec<f32>,
    load: String,
}

///
/// Takes the [`Prepared`] by reference on purpose: the expensive half ran once,
/// somewhere else, and sits behind an `Rc` on the view — this is the cheap half
/// a layout toggle pays, and it must not consume what the toggle wants kept.
fn arrange(prepared: &Prepared, host: &Host, layouts: &Layouts, current: usize) -> Built {
    let t = std::time::Instant::now();
    let mut renderers = match layouts.0.get(current) {
        Some(layout) => (layout.build)(host),
        None => Vec::new(),
    };
    // `renderers[0]` is indexed unconditionally below, so an empty list from a
    // registered builder is a panic rather than an empty diff. The fallback is
    // the built-in, which claims everything.
    if renderers.is_empty() {
        renderers.push(Box::new(TextRows::default()));
    }
    let name = layouts.names().get(current).copied().unwrap_or("custom");
    let mut order: Vec<RowRef> = Vec::new();

    let file_count = prepared.files.len();

    for f in &prepared.files {
        let owner = renderers
            .iter()
            .enumerate()
            .rev()
            .find(|(_, r)| r.claims(&f.path))
            .map_or(0, |(i, _)| i);
        // Cloned out of the shared cache: an allocation and refcount bumps, but
        // neither the intraline diff nor the syntax scan — which is exactly why
        // those two live behind the `Rc` and this pass does not.
        let r = &mut renderers[owner];
        let first = r.len();
        r.build(f.clone());
        for index in first..r.len() {
            // One file: the pane header names it, so its own band is noise. The
            // row stays built — hunk numbering, wrapping and the cursor address
            // rows by index — and only the order table leaves it out. A file
            // with no hunks keeps it: the band is then the only row there is,
            // and an empty pane says less than the name does.
            if file_count == 1 && !f.hunks.is_empty() && r.is_file_header(index) {
                continue;
            }
            order.push(RowRef {
                owner: owner as u16,
                seg: 0,
                index: index as u32,
            });
        }
    }

    // One entry per logical row so far, which is what `expand` wants. Nothing
    // wraps yet — no implementation has been given a width — so this pass only
    // finds the widest row and the file headers; the first frame reflows and
    // runs it again.
    let (Ordered { order, widest, .. }, headers) = expand(&order, &renderers, None);

    // `cpu across N` when the pass fanned out, because these are summed across
    // workers and `build` beside them is wall clock — without the note the two
    // numbers read as a contradiction rather than as a speed-up.
    let cpu = match prepared.threads > 1 {
        true => format!(" cpu across {}", prepared.threads),
        false => String::new(),
    };
    let mut reports: Vec<String> = vec![format!(
        "intraline {:.0?} · syntax {:.0?}{cpu}",
        prepared.intraline, prepared.syntax
    )];
    // Distinct from a renderer's own "invalid breaks" report below: this counts
    // spans and tokens `prepare` threw away at the `Line` boundary — a bad
    // `Differ` or `Highlighter`, not a bad `Wrap`.
    if prepared.rejected() > 0 {
        reports.push(format!("{} spans/tokens rejected", prepared.rejected()));
    }
    reports.extend(
        renderers
            .iter()
            .map(|r| r.report())
            .filter(|s| !s.is_empty()),
    );
    let load = format!(
        "{} rows · {} files · {name} · build {:.0?} ({})",
        order.len(),
        file_count,
        t.elapsed(),
        reports.join(" · "),
    );
    if crate::stats::enabled() {
        eprintln!("{load}");
    }
    let marks = hunk_marks(&order, &renderers);
    Built {
        renderers,
        order,
        widest,
        headers,
        marks,
        load,
    }
}

impl Diff {
    /// A zero-height canvas whose only job is to report how wide this view is.
    ///
    /// Only when it *changes*, and only by half a pixel or more: a redraw per
    /// frame that re-measured the same width would be an animation loop with
    /// nothing animating in it.
    fn probe(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let measured = self.measured.clone();
        let me = cx.entity().downgrade();
        canvas(
            move |bounds, _window, cx| {
                let w = f32::from(bounds.size.width);
                if (w - measured.get()).abs() >= 0.5 {
                    measured.set(w);
                    _ = me.update(cx, |_, cx| cx.notify());
                }
            },
            |_, _, _, _| {},
        )
        // Left and right pinned rather than `w_full`, so the width comes from the
        // parent's box directly instead of from a percentage basis. Absolute, so
        // it takes no part in the layout it is measuring, and zero height so it
        // could not if it wanted to.
        .absolute()
        .top_0()
        .left_0()
        .right_0()
        .h(px(0.))
    }
}

// -------------------------------------------------------------- the selection

/// Where every row's selectable text comes from, for `gitten_core::select`.
///
/// A wrapper rather than an impl on the vector, because the trait and the vector
/// both belong to somebody else. Three lines is what this seam costs.
struct Selectable<'a>(&'a [Box<dyn Rows>]);

impl select::Text for Selectable<'_> {
    fn text(&self, row: RowId, part: u16) -> Option<&str> {
        self.0.get(row.0 as usize)?.selectable(row.1 as usize, part)
    }
}

impl Diff {
    /// Which row and which byte of it a point in the window landed on.
    ///
    /// This is the one piece of a selection that cannot be a presentation's job
    /// and cannot be `core`'s either: `uniform_list` puts row *i* at
    /// `origin + offset + i * ROW_H`, and this is that arithmetic run backwards
    /// against the box and the scroll offset the list wrote during paint.
    ///
    /// **Not clamped to the viewport.** A drag 200 pixels below the window lands
    /// on the row 9 further down, which exists and is exactly what should be
    /// selected — the same as dragging past the bottom of a page in a browser.
    /// Clamped to the *diff*, so it cannot address a row that is not there.
    ///
    /// The third element is the **visual** row clicked — which of a wrapped
    /// line's rows it was — because a cursor lands where the mouse did, not at
    /// the top of whatever line that was.
    fn locate(&self, pos: Point<Pixels>, host: &Host) -> Option<(u16, Caret, usize)> {
        if self.order.is_empty() {
            return None;
        }
        let (bounds, offset) = {
            let s = self.scroll.0.borrow();
            (s.base_handle.bounds(), s.base_handle.offset())
        };
        // Zero before the list has ever been painted, and a click cannot have
        // happened inside something that was never drawn.
        if bounds.size.width <= px(0.) {
            return None;
        }
        let y = f32::from(pos.y - bounds.origin.y - offset.y);
        let visual = ((y / ROW_H).floor().max(0.0) as usize).min(self.order.len() - 1);
        // Not `- offset.x`, because there is none: a row is as wide as the
        // viewport and the horizontal offset is inside it, applied to the text
        // and not to the row. So this is a window coordinate and `hit` is handed
        // the scroll separately — the same two numbers the terminal passes.
        let x = f32::from(pos.x - bounds.origin.x);
        let r = self.order[visual];

        let renderers = self.renderers.borrow();
        let rows = renderers.get(r.owner as usize)?;
        let hit = rows.hit(r.index as usize, r.seg as usize, x, host, self.pan.at())?;
        // The visual rows this logical row occupies. The caret caches them so the
        // render path never searches the order table for them, and they are free
        // here: this row is `seg` into the run and the presentation knows how long
        // the run is.
        let first = visual - r.seg as usize;
        let n = rows.rows(r.index as usize).max(1);
        Some((
            hit.part,
            Caret {
                row: r.logical(),
                off: hit.off,
                at: first..first + n,
            },
            visual,
        ))
    }

    /// A selection over one byte range of one row: what a double or a triple
    /// click makes.
    fn span(&self, part: u16, at: &Caret, bytes: Range<usize>) -> Selection {
        let mut sel = Selection::new(
            part,
            Caret {
                off: bytes.start,
                ..at.clone()
            },
        );
        sel.extend(Caret {
            off: bytes.end,
            ..at.clone()
        });
        sel
    }

    /// The text of one row, for a word or a whole-row selection.
    fn row_text(&self, row: RowId, part: u16) -> Option<String> {
        Selectable(&self.renderers.borrow())
            .text(row, part)
            .map(str::to_string)
    }

    /// Moves the keyboard onto a clicked **visual** row, and keeps the list and
    /// the model agreeing about where that leaves the viewport.
    ///
    /// A click is a place: everything a key does next — copy, jump, open — acts
    /// on the row the cursor is on. On a wrapped line that is the *continuation*
    /// the mouse actually hit, not the top of the line it belongs to; and since
    /// [`Viewport::go_to`] may drag the top for its margin, [`Diff::show`]
    /// writes the list back to where the model now says it is — one write, so
    /// the two cannot disagree about who moved.
    ///
    /// Through [`Diff::live_view`], and not the stored one: a click can be the
    /// first thing that ever happens to this view — no key has navigated, no
    /// frame has reported a height — and against a model that still believes
    /// the list is empty, `go_to` clamps every row onto zero.
    fn click_row(&mut self, visual: usize, host: &Host) {
        let mut v = self.live_view(host);
        v.go_to(visual);
        self.view.set(v);
        self.show(v);
    }

    /// Mouse down: a new selection, a widened one on a repeat click, or an
    /// extension of the existing one when shift is held.
    ///
    /// A press on nothing selectable *clears*, which is the whole reason a fresh
    /// [`Selection`] is empty until something extends it: a click has to be able
    /// to mean "no longer selected".
    fn press(&mut self, ev: &MouseDownEvent, cx: &mut Context<Self>) {
        let host = crate::config::host(cx);
        let Some((part, caret, visual)) = self.locate(ev.position, &host) else {
            self.sel = None;
            cx.notify();
            return;
        };
        self.click_row(visual, &host);
        self.dragging = true;
        // Shift extends whatever is already there, which is how a selection
        // longer than the window gets made without a drag that has to scroll.
        // Only within the same part: across the divider it means nothing.
        let extend = ev.modifiers.shift && self.sel.as_ref().is_some_and(|s| s.part() == part);
        self.sel = match (extend, ev.click_count) {
            (true, _) => {
                let mut sel = self.sel.take().expect("extend implies a selection");
                sel.extend(caret);
                Some(sel)
            }
            // Two clicks take the word under the caret, three take the row. The
            // classes a word is made of are `core`'s: a terminal and a window
            // must not disagree about what `foo(bar,` is.
            (_, 2) => {
                let text = self.row_text(caret.row, part).unwrap_or_default();
                Some(self.span(part, &caret, select::word_at(&text, caret.off)))
            }
            (_, n) if n >= 3 => {
                let len = self.row_text(caret.row, part).map_or(0, |t| t.len());
                Some(self.span(part, &caret, 0..len))
            }
            _ => Some(Selection::new(part, caret)),
        };
        cx.notify();
    }

    /// Mouse move with the button down: moves the free end.
    ///
    /// The **anchor's** part wins. A drag that crosses the divider of a
    /// side-by-side diff stays in the column it started in and runs to that
    /// column's edge — because the alternative is a paste with the old and the
    /// new file interleaved, which is not a thing anybody wants.
    fn drag(&mut self, ev: &MouseMoveEvent, cx: &mut Context<Self>) {
        if !self.dragging || !ev.dragging() {
            return;
        }
        let host = crate::config::host(cx);
        self.autoscroll(ev.position);
        let Some((part, mut caret, _)) = self.locate(ev.position, &host) else {
            return;
        };
        let Some(sel) = &self.sel else { return };
        if part != sel.part() {
            // Parts are laid out left to right, so a part further along than the
            // anchor's means the mouse is past the end of the anchor's text.
            caret.off = match part > sel.part() {
                true => self.row_text(caret.row, sel.part()).map_or(0, |t| t.len()),
                false => 0,
            };
        }
        if let Some(sel) = &mut self.sel {
            sel.extend(caret);
        }
        cx.notify();
    }

    /// Pulls the diff along when a drag reaches an edge, a row per row of
    /// overshoot.
    ///
    /// Deliberately not a clock. Holding the mouse still outside the window does
    /// not keep scrolling, because that needs a timer running for as long as a
    /// button is held and this needs nothing at all — and the selection already
    /// extends past the last visible row without it, so what this buys is being
    /// able to *see* where it got to.
    fn autoscroll(&self, pos: Point<Pixels>) {
        if self.order.is_empty() {
            return;
        }
        let bounds = self.scroll.0.borrow().base_handle.bounds();
        let over = if pos.y < bounds.top() {
            pos.y - bounds.top()
        } else if pos.y > bounds.bottom() {
            pos.y - bounds.bottom()
        } else {
            return;
        };
        // At least one row: an overshoot of three pixels is still an overshoot,
        // and truncating it to nothing is a drag that will not leave the edge.
        let over = f32::from(over);
        let by = match (over / ROW_H) as i64 {
            0 => over.signum() as i64,
            rows => rows,
        };
        let last = self.order.len() as i64 - 1;
        let to = (self.top.get() as i64 + by).clamp(0, last) as usize;
        self.scroll.scroll_to_item(to, ScrollStrategy::Top);
    }

    /// Whatever the mouse is holding, as text. Empty when nothing is selected.
    pub fn selection(&self) -> String {
        match &self.sel {
            Some(sel) => sel.text(&self.order, &Selectable(&self.renderers.borrow())),
            None => String::new(),
        }
    }

    /// `copy.selection`. The selection, or the row the keyboard is on when there
    /// is none — "copy this line" should not need the mouse. A no-op with
    /// neither, rather than a cleared clipboard: losing what somebody copied
    /// elsewhere is worse than a key that did nothing.
    pub fn copy(&self, cx: &mut Context<Self>) {
        let mut text = self.selection();
        if text.is_empty() {
            text = self.cursor_text();
        }
        if !text.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    /// `select.all`.
    pub fn select_all(&mut self, cx: &mut Context<Self>) {
        self.sel = Selection::all(&self.order);
        cx.notify();
    }

    /// `select.none`. Whether there was one, which is what `back` wants to know.
    pub fn select_none(&mut self, cx: &mut Context<Self>) -> bool {
        let had = self.sel.take().is_some();
        if had {
            cx.notify();
        }
        had
    }

    /// While a drag is live the mouse belongs to it, wherever the pointer is.
    ///
    /// An element's own `on_mouse_move` fires only while the pointer is inside
    /// its box, so a selection dragged up into the title bar — or off the side of
    /// the window — would silently stop extending halfway through. A
    /// window-level listener has no box; it has to be registered during *paint*,
    /// which is what the zero-height canvas is for. Registered only while
    /// dragging, so a listener is not walked on every mouse move of every frame.
    fn drag_probe(&self, cx: &mut Context<Self>) -> AnyElement {
        if !self.dragging {
            return div().into_any_element();
        }
        let me = cx.entity().downgrade();
        canvas(
            |_, _, _| {},
            move |_, _, window, _| {
                let me = me.clone();
                window.on_mouse_event(move |ev: &MouseMoveEvent, phase, _, cx| {
                    if phase == DispatchPhase::Bubble {
                        _ = me.update(cx, |this, cx| this.drag(ev, cx));
                    }
                });
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .h(px(0.))
        .into_any_element()
    }
}

impl Render for Diff {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Zero rows is a sentence, not a bare rectangle: nothing selected, a
        // clean tree and an empty projection are indistinguishable from out
        // here, and [`chrome::empty_line`] says so the same way every quiet
        // pane does. The pane header above already names what was selected.
        // A one-file diff with no hunks still keeps its header row — only an
        // empty *order* lands here.
        if self.order.is_empty() {
            return crate::chrome::empty_line(
                &crate::config::host(cx),
                "no changes to show".into(),
            );
        }
        // Whatever the last frame measured this view to be. Reflowing here and
        // not in the probe itself keeps every mutation of the row tables on the
        // one path, and costs one frame of unwrapped rows at startup.
        self.reflow(self.measured.get(), &crate::config::host(cx));

        let pending_scroll = self.pending_scroll.clone();
        let renderers = self.renderers.clone();
        let order = self.order.clone();
        let rendered = self.rendered.clone();
        let top = self.top.clone();
        // Cloned per frame and not held behind a cell: it is two carets, the
        // closure lives for one element tree, and every path that changes a
        // selection notifies — so the copy in here is never the stale one.
        let sel = self.sel.clone();
        // Where the keyboard is, read per batch: commands run between frames and
        // this cell is what they move.
        let view = self.view.clone();
        let scroll = self.scroll.clone();
        let synced = self.synced.clone();
        // The same flag the shell last wrote, copied into the rows with the rest
        // of the frame's reads — a view cannot ask the shell during render.
        let focused = self.focused;
        // The question the shell is holding, if any, copied so the rows of one
        // frame all answer it at the same state of the arm.
        let armed = self.armed_hunk;
        // The hunk ticks' two per-frame reads, taken here like every other
        // setting the frame is drawn from: the offsets, computed at load and
        // rebuilt where the order was, and the ink they are drawn in. A
        // refcount bump and a u32 — nothing per frame.
        let marks = self.marks.clone();
        let ink = crate::config::host(cx).theme.diff.hunk_fg;
        // Where the scrollbar draws itself and how long its thumb is. Last
        // frame's box, like everything else measured here — a view is handed
        // one and cannot ask before.
        self.pan
            .set_viewport(self.scroll.0.borrow().base_handle.bounds());
        // Read once per frame and copied into the rows, so every row of the
        // frame is drawn at the same offset. Reading it per row would be the
        // same number and one `Cell` load each.
        let shift = self.pan.at();

        // The host is read here, per batch, rather than cloned in once when the
        // view was built. That is the whole of what makes a saved config file
        // appear on the next frame instead of the next launch.
        let list = uniform_list("diff", order.len(), move |range, _, cx| {
            rendered.set(range.len());
            top.set(range.start);
            let host = crate::config::host(cx);
            if let Some(accepted) = accept_deferred_scroll(&scroll, &pending_scroll, &synced) {
                if accepted.wheeled {
                    let mut v = view.get();
                    v.set_len(order.len());
                    v.set_height(range.len());
                    v.set_scrolloff(host.view.scrolloff);
                    v.pan_to((-accepted.y / ROW_H).round().max(0.0) as usize);
                    view.set(v);
                    top.set(v.top());
                    cx.refresh_windows();
                }
            }
            let cursor = view.get().cursor();
            // Once per batch of rows, not once per row.
            let renderers = renderers.borrow();
            // The keyboard's hunk and the armed one, as the rows address them:
            // one presentation lookup each, per frame — a row then reads its
            // extent and its arm as integer compares against the two ranges,
            // and never a search apiece. `armed` keys the logical row the
            // question was asked on — see [`Diff::confirm_or_arm_discard_hunk`]
            // — and no cursor move has moved it, so the rows are the same ones
            // the arm was spent on.
            let extent = order
                .get(cursor)
                .and_then(|r| extent_of(&renderers, r.owner, r.index));
            let armed_extent = armed.and_then(|(o, i)| extent_of(&renderers, o, i));
            range
                .map(|i| {
                    let r = order[i];
                    // Two integer comparisons on a row with no selection, which
                    // is every row of every frame until somebody drags.
                    let at = sel.as_ref().and_then(|s| s.at(i, r.logical()));
                    renderers[r.owner as usize].render(
                        r.index as usize,
                        r.seg as usize,
                        &host,
                        at,
                        RowState {
                            current: i == cursor,
                            focused,
                            armed: armed_extent.is_some_and(|e| e.contains(r.owner, r.index)),
                            in_hunk: extent.is_some_and(|e| e.contains(r.owner, r.index)),
                        },
                        shift,
                    )
                })
                .collect()
        })
        .track_scroll(&self.scroll)
        // Constrained, which is the default: a row is exactly as wide as the
        // viewport whatever it holds, and what overflows is clipped inside it by
        // `scrolled` rather than scrolled to by the list. That is what lets the
        // gutter stay put, and it takes the one measured row with it — the list
        // no longer shapes a 2000-character line every frame to decide a
        // scrollable width nothing scrolls.
        .size_full();

        let root = div()
            .relative()
            .size_full()
            // Text, because it is: the whole view is selectable, headers
            // included, and an arrow over a wall of code says it is not.
            .cursor_text()
            // Down starts a selection, up ends the drag, and move extends it —
            // but only the down needs to be on this element. Move is registered
            // on the *window* while a drag is live, so it does not stop at the
            // edge of the box (see `drag_probe`), and `on_mouse_up_out` is what
            // catches a button released somewhere else entirely.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, _, cx| this.press(ev, cx)),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, _| this.dragging = false),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, _| this.dragging = false),
            )
            .child(list)
            .child(self.drag_probe(cx))
            // How wide this view actually is, which is the wrap budget. A view
            // is handed its box by whatever assembled it and cannot know it
            // during `render`, so it is read off the paint pass and used on the
            // frame after — the same one-frame trade every measured layout
            // makes. Zero height, so it takes part in nothing.
            .child(self.probe(cx))
            // `[view] scrollbar`, read per frame like every other setting — the
            // terminal draws its own bar from the same flag.
            .when(crate::config::host(cx).view.scrollbar, |d| {
                // Two handles, because there are two axes and they belong to
                // different things now: the rows to the list, the text to `Pan`.
                d.child(vertical_scrollbar(&DeferredScrollbar::new(
                    &self.scroll,
                    &self.pending_scroll,
                )))
                .child(horizontal_scrollbar(&self.pan))
                // The hunk ticks, last so they share the scrollbar's strip and
                // remain visible above its overlaid thumb. Only when there is a
                // track to mark: a diff that fits the pane is one the widget
                // draws no bar on.
                .when(
                    self.scroll.0.borrow().base_handle.max_offset().y > px(0.),
                    |d| d.child(track_marks(marks, ink)),
                )
            });
        root.into_any_element()
    }
}

// --------------------------------------------------------------- the built-in

/// Rows hold [`Arc<str>`] text and exact-size boxed ranges, not owned copies.
///
/// The text is the *same* allocation `prepare` clipped into — a row takes a
/// refcount bump at load and hands back another one at draw time, which is why
/// cycling layouts on a 714k-line diff costs no second copy of every line.
/// `render` runs for every visible row on every frame that redraws, so what it
/// hands GPUI is `SharedString::from(Arc::clone(..))`: a refcount bump (or an
/// inline copy for the shortest lines), never a heap allocation — see [`slice`].
///
/// The same handle shape for the strings the parsed diff already retains: a
/// file's path and a hunk's header are one [`Arc<str>`] built at load and
/// cloned by handle into the row, not a second copy of a string somebody else
/// is keeping. And the gutter numbers are stored as the [`u32`]s they always
/// were, formatted at draw time — see [`Scratch::number`] — because
/// pre-rendering them put forty-eight bytes of string on every line of a
/// 714k-line diff to describe an integer.
enum Row {
    File {
        path: std::sync::Arc<str>,
        adds: usize,
        dels: usize,
    },
    Hunk(std::sync::Arc<str>),
    Line {
        kind: LineKind,
        moved: bool,
        old: Option<u32>,
        new: Option<u32>,
        text: std::sync::Arc<str>,
        spans: Box<[Span]>,
        tokens: Box<[Token]>,
    },
}

/// Which hunk each drawn row belongs to.
///
/// One entry per hunk — not per row, which is the difference between a table
/// that grows with a 714k-line diff and one that grows with its hunk count.
/// Both shipped presentations build their rows in hunk order (a file header,
/// then each hunk's header and lines), so recording a span at build time is
/// one push per hunk; reading it back is a binary search. The path travels
/// as the loaded diff spells it, which is the key the staging verbs aim
/// [`gitten_core::patch::emit`] with.
#[derive(Default)]
pub(crate) struct HunkMap {
    spans: Vec<SpanEntry>,
}

struct SpanEntry {
    /// First logical row of the hunk, inclusive — its header row.
    start: u32,
    /// How many logical rows the hunk spans, header included.
    rows: u32,
    path: std::sync::Arc<str>,
    hunk: u16,
}

impl HunkMap {
    /// Records one hunk occupying logical rows `at..at+rows`. The path is
    /// interned once per hunk — hunks number in the hundreds where rows
    /// number in the hundreds of thousands.
    pub(crate) fn record(&mut self, at: usize, rows: usize, path: &str, hunk: usize) {
        self.spans.push(SpanEntry {
            start: at as u32,
            rows: rows as u32,
            path: std::sync::Arc::from(path),
            hunk: hunk as u16,
        });
    }

    /// The hunk under logical row `index`, or nothing for the gaps between
    /// hunks — today only the file headers.
    pub(crate) fn at(&self, index: usize) -> Option<(&str, usize)> {
        let i = self.spans.partition_point(|s| s.start as usize <= index);
        let s = self.spans.get(i.checked_sub(1)?)?;
        if index < s.start as usize + s.rows as usize {
            Some((s.path.as_ref(), s.hunk as usize))
        } else {
            None
        }
    }

    /// The logical rows the hunk under `index` spans — first through last,
    /// header included, [`HunkMap::at`]'s own answer as a range. What the
    /// extent mark and the armed tint are computed against, once per frame:
    /// a row then reads its membership as two integer compares against this
    /// range and never a binary search apiece.
    pub(crate) fn span(&self, index: usize) -> Option<(u32, u32)> {
        let i = self.spans.partition_point(|s| s.start as usize <= index);
        let s = self.spans.get(i.checked_sub(1)?)?;
        (index < s.start as usize + s.rows as usize).then_some((s.start, s.start + s.rows))
    }
}

/// The default presentation: one line of text per row, behind a line-number
/// gutter, coloured by the host's theme.
#[derive(Default)]
pub struct TextRows {
    rows: Vec<Row>,
    /// Which hunk every row belongs to — the staging verbs' map from the
    /// keyboard's row back to the loaded diff. One entry per hunk.
    hunks: HunkMap,
    /// How many rows are part of a block that moved, for the overlay. Reported
    /// because move detection is otherwise invisible when it finds nothing, and
    /// "it found nothing" and "it is switched off" look identical on screen.
    moved: usize,
    /// Where each row's text breaks, indexed by row. Headers are in it too, with
    /// no breaks, so nothing has to translate between two numbering schemes.
    wrapped: Wrapped,
    /// The budget and the policy `wrapped` was built for, so a resize that
    /// changes neither costs a comparison.
    cols: usize,
    wrap: &'static str,
    /// What drawing borrows. Cleared per cell, grown once ever — see [`Scratch`].
    scratch: RefCell<Scratch>,
}

impl Rows for TextRows {
    fn claims(&self, _path: &str) -> bool {
        true
    }

    fn report(&self) -> String {
        let mut out = match self.moved {
            0 => String::new(),
            n => format!("{n} moved"),
        };
        // Never silently: a wrap whose breaks were all thrown away looks exactly
        // like a wrap that found nothing to do.
        if self.wrapped.rejected() > 0 {
            out.push_str(&format!(
                "{}{} invalid breaks from {}",
                if out.is_empty() { "" } else { " · " },
                self.wrapped.rejected(),
                self.wrap
            ));
        }
        out
    }

    fn len(&self) -> usize {
        self.rows.len()
    }

    fn is_file_header(&self, index: usize) -> bool {
        matches!(self.rows.get(index), Some(Row::File { .. }))
    }

    fn rows(&self, index: usize) -> usize {
        self.wrapped.rows(index)
    }

    /// The whole of what a presentation owes wrapping: turn its own width into a
    /// column budget, and hand `core` the text.
    ///
    /// The one thing in here that is not about wrapping is the early exit for a
    /// wrap that breaks nothing. The width still has to land — it is what bounds
    /// the horizontal scroll — but there is nothing to scan for: an unbroken
    /// table is the default one, and building it from 714k lines to be told so
    /// again is 26 ms on every five characters of a resize drag.
    fn reflow(&mut self, width: f32, host: &Host, wrap: &dyn Wrap) -> bool {
        let cols = columns(width, TEXT_CHROME, host.font.size, host);
        if cols == self.cols && wrap.name() == self.wrap {
            return false;
        }
        self.cols = cols;
        self.wrap = wrap.name();
        if !wrap.breaks_lines() {
            let broken = self.wrapped.total() > self.wrapped.lines();
            self.wrapped = Wrapped::default();
            return broken;
        }
        self.wrapped = Wrapped::build(self.rows.iter().map(|r| (wrappable(r), cols)), wrap);
        true
    }

    fn build(&mut self, f: gitten_core::prepared::File) {
        // Kept beside the rows, which consume the hunks: the hunk map needs
        // to spell each file the loaded diff spells it.
        let path = std::sync::Arc::from(f.path.as_str());
        self.rows.push(Row::File {
            path: std::sync::Arc::clone(&path),
            adds: f.adds,
            dels: f.dels,
        });
        for (n, h) in f.hunks.into_iter().enumerate() {
            // The hunk's span opens on its header row and closes after its
            // last line, so a cursor anywhere inside it — header included —
            // reads as being *on* the hunk.
            let at = self.rows.len();
            self.rows.push(Row::Hunk(h.header.into()));
            let mut moved = 0;
            for l in h.lines {
                moved += l.moved as usize;
                self.rows.push(Row::Line {
                    kind: l.kind,
                    moved: l.moved,
                    old: l.old_no,
                    new: l.new_no,
                    text: l.text,
                    spans: l.spans,
                    tokens: l.tokens,
                });
            }
            self.moved += moved;
            self.hunks.record(at, self.rows.len() - at, &path, n);
        }
    }

    fn hunk_at(&self, index: usize) -> Option<(&str, usize)> {
        self.hunks.at(index)
    }

    fn hunk_span(&self, index: usize) -> Option<(u32, u32)> {
        self.hunks.span(index)
    }

    /// Characters, not bytes, and after `trim_end`: a line of box drawing is a
    /// third as many columns as it is bytes, and whitespace at the end of a row
    /// is not ink. Both were wrong here in the direction of a scrollable width
    /// wider than anything on screen.
    fn width(&self, index: usize, seg: usize) -> usize {
        match &self.rows[index] {
            Row::Line { text, .. } => text[self.wrapped.range(index, seg, text)]
                .trim_end()
                .chars()
                .count(),
            Row::Hunk(h) => h.chars().count(),
            Row::File { path, .. } => path.chars().count(),
        }
    }

    /// Characters of text past the window, at this presentation's one size.
    ///
    /// `TEXT_CHROME` and not the window's whole width, because the gutters and
    /// the sign do not scroll: what has to reach the right edge is the text, and
    /// the furniture in front of it is space the text never had.
    fn overflow(&self, index: usize, seg: usize, width: f32, host: &Host) -> f32 {
        let text = self.width(index, seg) as f32 * host.font.char_width();
        (text - (width - TEXT_CHROME)).max(0.0)
    }

    /// The gutters and the sign column, then the text — and for a file header,
    /// the page padding and nothing else, because that is what it draws. A hunk
    /// header's text sits where the code does, so it measures from there.
    fn hit(&self, index: usize, seg: usize, x: f32, host: &Host, shift: f32) -> Option<Hit> {
        Some(match self.rows.get(index)? {
            Row::Hunk(h) => hunk_hit(h, x, host, shift),
            Row::File { path, .. } => header_hit(path, x, host, shift),
            Row::Line { text, .. } => {
                let at = self.wrapped.range(index, seg, text);
                // Rebased into the line: a caret addresses the line, and this row
                // is one of the rows the line wrapped onto.
                let off = at.start
                    + column_at(
                        &text[at.clone()],
                        into_text(x, TEXT_CHROME - PAD, shift),
                        host.font.size,
                        host,
                    );
                Hit { part: 0, off }
            }
        })
    }

    /// One part, and it is what the row draws. A header's text is its own — a
    /// selection dragged across three files copies their paths with them, which
    /// is what makes the paste readable.
    fn selectable(&self, index: usize, _part: u16) -> Option<&str> {
        Some(match self.rows.get(index)? {
            Row::Line { text, .. } => text.as_ref(),
            Row::Hunk(h) => h.as_ref(),
            Row::File { path, .. } => path.as_ref(),
        })
    }

    fn is_header(&self, index: usize) -> bool {
        matches!(self.rows.get(index), Some(Row::File { .. }))
    }

    fn render(
        &self,
        index: usize,
        seg: usize,
        host: &Host,
        sel: Option<Selected>,
        state: RowState,
        shift: f32,
    ) -> AnyElement {
        let theme = &host.theme;
        let p = &theme.diff;
        match &self.rows[index] {
            Row::File { path, adds, dels } => {
                file_header(path, *adds, *dels, theme, &host.font, sel, state, shift)
            }

            Row::Hunk(header) => hunk_header(header, theme, sel, state, shift),

            Row::Line {
                kind,
                moved,
                old,
                new,
                text,
                spans,
                tokens,
            } => {
                let (bg, fg, sign) = line_colors(*kind, *moved, p);
                // The keyboard's row is a bar across the whole line, whatever
                // kind of line it is — the same background the terminal draws,
                // so the cursor reads as one thing in both.
                let bg = row_background(state.current, bg, theme);
                // Which background this row's furniture lands on, so the line
                // numbers are resolved against it — see `Theme::gutter_on`. On
                // the keyboard's row that is the wash, and not the line kind's:
                // the row paints `selection_bg` over both, so a number resolved
                // for the line it is sits on a background it never lands on.
                let (plain, _) = surfaces(*kind, *moved);
                let gutter = theme.gutter_on(match state.current {
                    true => Surface::Cursor,
                    false => plain,
                });
                // The question stands over the hunk the second press will
                // spend, and the column that says which hunk that is — the
                // line numbers and the sign — name it in the colour a conflict
                // does: the palette's own "this row ends work" foreground,
                // which a conflict's letters already draw.
                let gutter = match state.armed {
                    true => theme.chrome.error,
                    false => gutter,
                };
                let at = self.wrapped.range(index, seg, text);
                let piece = slice(text, &at);
                // A continuation carries no number and no sign. The background
                // is what says which line it belongs to, and an empty gutter is
                // what says it is not a line of its own — every real line has at
                // least one number, so there is nothing to confuse it with.
                let blank = seg > 0;
                // One borrow per row, held while the row's pieces are built:
                // numbers are formatted into it and the run list swept into it,
                // both copied out by the elements as they take them.
                let mut sc = self.scratch.borrow_mut();
                extent_line(row_frame().items_center().px(px(PAD)), state, bg, theme)
                    // The bar on every row, in the row's own background when
                    // the cursor is elsewhere: the padding gives back what the
                    // border takes, so the text starts where it started and a
                    // move of the cursor shifts no line a pixel — the same
                    // frame the sidebar's rows sit in.
                    .border_l(px(ROW_BAR))
                    .border_color(rgb(row_bar(state, bg, theme)))
                    .pl(px(PAD - ROW_BAR))
                    .bg(rgb(bg))
                    .child(num(sc.number(*old, blank), gutter))
                    .child(num(sc.number(*new, blank), gutter))
                    .child(
                        div()
                            .flex_none()
                            .w(px(SIGN_W))
                            .text_color(rgb(match state.armed {
                                true => theme.chrome.error,
                                false => fg,
                            }))
                            .child(if blank { " " } else { sign }),
                    )
                    .child(scrolled(
                        shift,
                        div().text_color(rgb(fg)).child(
                            StyledText::new(piece).with_highlights(
                                sc.merged(
                                    at.clone(),
                                    tokens,
                                    spans,
                                    theme,
                                    *kind,
                                    *moved,
                                    state.current,
                                    selected(sel, 0, text),
                                )
                                .iter()
                                .cloned(),
                            ),
                        ),
                    ))
                    .into_any_element()
            }
        }
    }
}

/// The text of a row that may wrap. A header does not, and passing an empty
/// string is how [`Wrapped`] is told so — cheaper and less error-prone than a
/// parallel table of which rows are lines.
fn wrappable(row: &Row) -> &str {
    match row {
        Row::Line { text, .. } => text,
        _ => "",
    }
}

/// One row's worth of a line, without copying the line when it is all of it.
///
/// The common case by far — most lines fit — and it is a refcount bump twice
/// over: the `Arc` is cloned and GPUI's `SharedString` adopts it as its own
/// heap representation. Only the shortest lines (23 bytes or fewer) are copied,
/// inline into the string itself. A row that *did* wrap cannot borrow — GPUI's
/// elements are `'static`, so `StyledText` only ever takes owned text — but it
/// goes in by reference all the same: `SharedString::from(&str)` copies into
/// the inline representation for a window-width slice of 23 bytes or less and
/// heap-allocates exactly once past that, where the `to_string()` it replaced
/// allocated twice (the `String`, then the `Arc` adopted from it).
pub(crate) fn slice(text: &std::sync::Arc<str>, at: &Range<usize>) -> SharedString {
    match at.start == 0 && at.end == text.len() {
        true => SharedString::from(text.clone()),
        false => SharedString::from(&text[at.clone()]),
    }
}

/// The frame every row in the list is drawn in: exactly [`ROW_H`] tall, and
/// never narrower than the window.
///
/// The width is what makes a row's background the *row's* — a line's colour runs
/// to the right edge of the view instead of stopping after its last character,
/// which is what every diff viewer worth reading does and what makes a run of
/// additions read as a block rather than as a ragged margin.
///
/// `min_w_full` and not a measured width, because `uniform_list` lays each
/// visible row out as its own root against the viewport's width — so 100% is "to
/// the right edge of the window" and the fill costs nothing to compute. A
/// *minimum* and not a width, because the one row the list measures to decide its
/// item height is laid out against `MaxContent`, where a percentage has no parent
/// width to resolve against: `w_full` there is zero, and a row of zero height is a
/// list that draws nothing.
///
/// What a line wider than the window does is *not* make the row wider — the row
/// is the viewport, always, and the text is clipped inside it by [`scrolled`].
/// See the module note on scrolling sideways.
pub(crate) fn row_frame() -> Div {
    div().flex().h(px(ROW_H)).min_w_full()
}

/// The window a presentation's text is drawn in, `shift` pixels to the left of
/// where it would otherwise start.
///
/// This is `Pen::scroll`: whatever the presentation drew before it — the line
/// numbers, the sign, a quote bar, the other column — stays where it is, and the
/// text moves under it. Two properties do the work and both are load-bearing.
/// **`overflow_x_hidden`**, so a line pulled left is clipped at this window's
/// edge instead of painting over the numbers it slid under; the row is not the
/// clip, because the row is what contains the numbers. It masks the window's
/// whole box and not only its x axis — `Style::overflow_mask` gives the same
/// rectangle either way — so the window is exactly as tall as what it holds, and
/// a row with something to place against its *bottom* edge, like a Markdown
/// table's rule, passes in a `ROW_H` box of its own. And **`min_w(0)`**, because
/// a flex item
/// is otherwise at least as wide as its content, and a 2000-character line would
/// make this window that wide and push the whole row past the viewport.
///
/// A negative margin and not a slice of the text: the syntax tokens and the
/// intraline spans address the *line*, so cutting the string before
/// [`Scratch::merged`] pairs styling with the wrong bytes. The same reason the
/// terminal swallows columns in the pen rather than slicing.
pub(crate) fn scrolled(shift: f32, text: Div) -> Div {
    div()
        .flex()
        .flex_grow(1.)
        .min_w(px(0.))
        .overflow_x_hidden()
        .child(text.flex_none().ml(px(-shift)))
}

/// The bytes of `text` that a selection covers, or nothing at all when the
/// selection is in another of the row's parts — or is not there, which is the
/// case on nearly every row of nearly every frame.
pub(crate) fn selected(sel: Option<Selected>, part: u16, text: &str) -> Range<usize> {
    match sel.filter(|s| s.part() == part) {
        Some(s) => s.range(text),
        None => 0..0,
    }
}

/// Where a click landed in a file header.
///
/// Shared for the same reason the header itself is: whoever owns the lines
/// beneath it, a file header is drawn by [`file_header`] and its text starts at
/// the page padding. Three presentations working that out separately is three
/// places for the caret to be a gutter's width off.
pub(crate) fn header_hit(text: &str, x: f32, host: &Host, shift: f32) -> Hit {
    Hit {
        part: 0,
        off: column_at(text, into_text(x, PAD, shift), host.font.size, host),
    }
}

/// Where a click landed in a hunk header, whose text [`hunk_header`] draws at
/// [`HUNK_INDENT`] — the code column — rather than at the page padding. The same
/// constant on both sides, or the caret is two gutters off.
pub(crate) fn hunk_hit(text: &str, x: f32, host: &Host, shift: f32) -> Hit {
    Hit {
        part: 0,
        off: column_at(text, into_text(x, HUNK_INDENT, shift), host.font.size, host),
    }
}

/// How far into a row's text a click landed: `x` is from the left edge of the
/// window, `chrome` is where the text starts and `shift` is how far it has been
/// scrolled under everything in front of it.
///
/// The `max(0)` is **before** the shift and not after, which is the terminal's
/// arithmetic in `TextRows::hit` and matters as soon as anything is scrolled: a
/// click on a line number is a caret at the first character there is to *see*,
/// not at one somewhere off the left edge, and not at a byte the row cannot
/// address.
pub(crate) fn into_text(x: f32, chrome: f32, shift: f32) -> f32 {
    (x - chrome).max(0.0) + shift
}

/// A header's text, with whatever a selection covers lit up behind it.
///
/// One run and not the token sweep: a header has no syntax and no changed words,
/// so the only thing that can be true of a stretch of it is that it is selected.
/// The unselected case stays a bare string child — a `StyledText` on every header
/// of every frame is a shaped line for a highlight nobody asked for.
fn header_text(text: SharedString, sel: Range<usize>, theme: &Theme) -> AnyElement {
    if sel.is_empty() {
        return text.into_any_element();
    }
    StyledText::new(text)
        .with_highlights([(
            sel,
            HighlightStyle {
                background_color: Some(rgb(theme.chrome.selected_bg).into()),
                ..Default::default()
            },
        )])
        .into_any_element()
}

/// A file's header row. Identical whichever presentation owns the lines beneath
/// it — a `.md` file is still a file — so it is drawn here and shared.
///
/// Two things carry the boundary, because the background alone could not: a rule
/// along the top, and a `file_bg` that is now a real step off a context row
/// rather than the 1.048:1 it was. This is the most important edge in a diff and
/// it was invisible.
///
/// The **directory recedes and the file name does not**. A forty-file diff is
/// scanned by name, and `src/views/` is the same eleven characters on most of
/// them; drawn at one weight the names are the part that has to be hunted for.
///
/// The path arrives as the row's own handle and is borrowed, not copied: the
/// directory/name split below is two subslices of one string, and only what a
/// header actually draws is handed to GPUI — inline for any piece under 23
/// bytes, which is nearly all of them.
#[allow(clippy::too_many_arguments)]
pub(crate) fn file_header(
    path: &std::sync::Arc<str>,
    adds: usize,
    dels: usize,
    theme: &Theme,
    font: &Font,
    sel: Option<Selected>,
    state: RowState,
    shift: f32,
) -> AnyElement {
    let p = &theme.diff;
    let (dir, name) = split_path(path);
    // One range over the whole path, split between the two elements below.
    let sel = selected(sel, 0, path);
    let cut = dir.as_ref().map_or(0, |d| d.len());
    header_gutter(row_frame(), state, p.file_bg, theme)
        // A column, so the rule is part of the row's own 22 pixels rather than
        // added to them: every row in this list is exactly `ROW_H` tall and the
        // list is what makes 714k of them scroll.
        .flex_col()
        .bg(rgb(row_background(state.current, p.file_bg, theme)))
        .child(div().flex_none().h(px(1.)).bg(rgb(p.rule)))
        // A path longer than the window is reached the same way a line is: it is
        // the row's text, and the only thing in front of it is the page padding —
        // which is why the padding is out here and the scroll is inside it.
        .child(
            div().flex().flex_grow(1.0).px(px(PAD)).child(scrolled(
                shift,
                div()
                    .flex()
                    .items_center()
                    .gap(gap_l(font))
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .children(dir.map(|d| {
                                // The furniture colour, resolved against the header's
                                // own background rather than a row's — a header is not
                                // a `Surface`, and `gutter_fg` raw is 1.7:1 on it.
                                // Twice a frame at most: one header per file.
                                let fg = gitten_core::theme::readable(
                                    p.gutter_fg,
                                    p.file_bg,
                                    theme.min_furniture,
                                );
                                let at = clipped(&sel, 0..cut);
                                div().flex_none().text_color(rgb(fg)).child(header_text(
                                    SharedString::from(d),
                                    at,
                                    theme,
                                ))
                            }))
                            .child(div().flex_none().text_color(rgb(p.file_fg)).child(
                                header_text(
                                    match dir {
                                        // A bare name *is* the whole path: adopt the
                                        // row's own handle rather than copying it.
                                        None => SharedString::from(std::sync::Arc::clone(path)),
                                        Some(_) => SharedString::from(name),
                                    },
                                    clipped(&sel, cut..path.len()),
                                    theme,
                                ),
                            )),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_color(rgb(p.adds_fg))
                            .child(format!("+{adds}")),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_color(rgb(p.dels_fg))
                            .child(format!("−{dels}")),
                    ),
            )),
        )
        .into_any_element()
}

/// The part of `sel` that falls inside `at`, rebased to the start of it.
///
/// A file header draws its path as two elements — the directory dim, the name
/// bright — and a selection knows nothing about where that boundary is: it is one
/// range over the whole path, because that is the one string `header_hit` measures
/// a click against. So each element takes the slice of it that it actually covers.
/// Empty for the common case, which is a selection somewhere else entirely.
fn clipped(sel: &Range<usize>, at: Range<usize>) -> Range<usize> {
    let clamp = |i: usize| i.clamp(at.start, at.end) - at.start;
    clamp(sel.start)..clamp(sel.end)
}

/// `src/views/diff.rs` -> (`src/views/`, `diff.rs`). `None` for a bare name.
///
/// Borrows, and is allowed to be that cheap: a header is one row per *file*, so
/// at most a couple are ever on screen, where a line is one of fifty — but the
/// two pieces are subslices of the row's own handle, so there is nothing to
/// allocate for until GPUI is handed a piece it must own.
fn split_path(path: &str) -> (Option<&str>, &str) {
    match path.rfind('/') {
        Some(i) => (Some(&path[..=i]), &path[i + 1..]),
        None => (None, path),
    }
}

/// A hunk's header row: `@@ -41,9 +41,11 @@ fn dispatch() {`.
///
/// Drawn as two things, because it is two things — the split is
/// [`gitten_core::hunk_parts`], so every client agrees where it is. The
/// coordinates are furniture and take the gutter's colour, which is what they
/// are: a line number with a range around it. The declaration git appends is the
/// half a reader wants, and keeps `hunk_fg`.
///
/// The band itself recedes now. It used to be the more prominent of the two
/// headers, which had the hierarchy backwards: a hunk is a place inside a file,
/// and the file is the boundary that matters.
///
/// Its text starts at [`HUNK_INDENT`], where a code line's does, and not at the
/// gutter: the `@@` is a coordinate *of the lines under it*, and flush left it
/// read as a heading over them. Lined up with the code it is a quiet rule
/// between two runs of it. [`hunk_hit`] measures a click from the same place.
pub(crate) fn hunk_header(
    header: &std::sync::Arc<str>,
    theme: &Theme,
    sel: Option<Selected>,
    state: RowState,
    shift: f32,
) -> AnyElement {
    let p = &theme.diff;
    let (marker, _) = gitten_core::hunk_parts(header);
    header_gutter(row_frame(), state, p.hunk_bg, theme)
        .items_center()
        .pl(px(HUNK_INDENT))
        .pr(px(PAD))
        .bg(rgb(row_background(state.current, p.hunk_bg, theme)))
        .text_color(rgb(p.hunk_fg))
        .child(scrolled(
            shift,
            div().child(
                // The row's own handle, adopted rather than copied.
                StyledText::new(SharedString::from(std::sync::Arc::clone(header)))
                    .with_highlights(hunk_runs(marker.len(), selected(sel, 0, header), theme)),
            ),
        ))
        .into_any_element()
}

/// The two colours of a hunk header, and whatever a selection covers, as one run
/// list.
///
/// **One `StyledText` over the whole string**, not two elements side by side,
/// which is how the two-tone version started: `header_hit` maps a click to a byte
/// offset in that one string measured from the page padding, so any gap between
/// two elements puts the caret characters off for everything after it — and a
/// selection spanning the boundary would be two highlights that have to agree
/// about where it is.
///
/// The declaration git appends gets no run at all: it is the row's own
/// `hunk_fg`, and a run that repeats the default is a run to merge for nothing.
fn hunk_runs(
    marker: usize,
    sel: Range<usize>,
    theme: &Theme,
) -> Vec<(Range<usize>, HighlightStyle)> {
    let fg = theme.gutter_on(Surface::Context);
    let bg = theme.chrome.selected_bg;
    // Four edges at most, so this is a sort of four rather than a sweep.
    let mut edges = vec![0, marker, sel.start, sel.end];
    edges.sort_unstable();
    edges.dedup();
    let mut out = Vec::with_capacity(edges.len());
    for w in edges.windows(2) {
        let (start, end) = (w[0], w[1]);
        let in_marker = start < marker;
        let in_sel = sel.contains(&start);
        if !in_marker && !in_sel {
            continue;
        }
        out.push((
            start..end,
            HighlightStyle {
                color: in_marker.then(|| rgb(fg).into()),
                background_color: in_sel.then(|| rgb(bg).into()),
                ..Default::default()
            },
        ));
    }
    out
}

/// Which background a line is drawn on, and the foreground and sign that go with
/// it. Shared by all three presentations so they cannot drift on what "added"
/// looks like.
///
/// `moved` swaps the background and nothing else. The `+` and `-` stay, so a
/// column of signs is still scannable, and the foreground stays so a moved block
/// reads as ordinary text — which it is. Only the hue says "you may skip this",
/// which is how git's `--color-moved` does it too.
pub(crate) fn line_colors(
    kind: LineKind,
    moved: bool,
    p: &DiffPalette,
) -> (Rgb, Rgb, &'static str) {
    match (kind, moved) {
        (LineKind::Added, false) => (p.added_bg, p.added_fg, "+"),
        (LineKind::Added, true) => (p.moved_added_bg, p.added_fg, "+"),
        (LineKind::Removed, false) => (p.removed_bg, p.removed_fg, "-"),
        (LineKind::Removed, true) => (p.moved_removed_bg, p.removed_fg, "-"),
        // Context is never moved: a line that did not change did not go
        // anywhere, and `mark_moved` says so.
        (LineKind::Context, _) => (p.context_bg, p.context_fg, " "),
    }
}

/// The background a row paints: its own kind's, unless the keyboard is on it —
/// then the one bar every presentation shares. Every `render` in this crate
/// goes through here for exactly that reason: a visible cursor that some rows
/// honour and others ignore is a cursor that lies about where it is.
pub(crate) fn row_background(current: bool, base: Rgb, theme: &Theme) -> Rgb {
    match current {
        true => theme.chrome.selection_bg,
        false => base,
    }
}

/// The ink on the bar down a row's left edge, on every row: accent while the
/// row's pane holds the keyboard, faint when the selection is remembered and
/// the keyboard is elsewhere, and the row's own background when the row is not
/// the cursor's — which is what keeps the text from shifting a pixel when the
/// cursor moves. The same rule [`chrome::list_row`] runs; a cursor that some
/// rows honour and others ignore is a cursor that lies about where it is.
pub(crate) fn row_bar(state: RowState, base: Rgb, theme: &Theme) -> Rgb {
    match (state.current, state.focused) {
        (true, true) => theme.chrome.accent,
        (true, false) => theme.chrome.faint,
        (false, _) => base,
    }
}

/// The ink the extent hairline takes on a row: `diff.rule` when the row is in
/// the keyboard's hunk and the keyboard is elsewhere; the row's own background
/// when it is not — the ink the element always carries when it says nothing,
/// which is what keeps a cursor moving between hunks from shifting any row a
/// pixel; and the bar's ink on the keyboard's row, where the 2px bar wins and
/// the hairline hides inside it.
fn extent_ink(state: RowState, base: Rgb, theme: &Theme) -> Rgb {
    match (state.current, state.in_hunk) {
        (true, _) => row_bar(state, base, theme),
        (false, true) => theme.diff.rule,
        (false, false) => base,
    }
}

/// The hairline down a row's left edge that says where the keyboard's hunk
/// starts and ends: one pixel, inside the bar's own 2px column, in
/// [`extent_ink`]'s ink.
///
/// A hairline and not a second tint, because the row backgrounds already
/// spend their tint saying add and remove — a second tint would compete. And
/// in `diff.rule`'s ink specifically because that is the ink a 1px line
/// already proves itself in: the file header's top rule, and the split
/// layout's divider, draw it against the same backgrounds at the same width.
pub(crate) fn extent_line(frame: Div, state: RowState, base: Rgb, theme: &Theme) -> Div {
    frame.relative().child(
        div()
            .absolute()
            .left_0()
            .top_0()
            .bottom_0()
            .w(px(1.))
            .bg(rgb(extent_ink(state, base, theme))),
    )
}

/// A header row's gutter edge: the 2px bar the line rows carry as a border —
/// same [`row_bar`] ink, so the cursor is one shape on a header and on a line
/// — with the extent hairline inside it. Both out of the flow, because a
/// header's geometry was measured to the pixel: the file header's rule across
/// its top, the indent its text starts at — a border would move them both.
pub(crate) fn header_gutter(frame: Div, state: RowState, base: Rgb, theme: &Theme) -> Div {
    extent_line(
        frame.child(
            div()
                .absolute()
                .left_0()
                .top_0()
                .bottom_0()
                .w(px(ROW_BAR))
                .bg(rgb(row_bar(state, base, theme))),
        ),
        state,
        base,
        theme,
    )
}

/// A hunk, as the rows address it once per frame: which presentation owns it
/// and the logical rows it spans there — [`HunkMap::span`]'s shape. The
/// keyboard's hunk and the armed one are computed like this at the top of a
/// frame, and every row then reads its membership as two integer compares —
/// one `u16` for every row of another file, two `u32` for the owner's own —
/// never a string compare, and never a search per row.
#[derive(Clone, Copy)]
pub(crate) struct HunkExtent {
    owner: u16,
    /// First logical row of the hunk, its header included, and one past its
    /// last.
    span: (u32, u32),
}

impl HunkExtent {
    /// Whether a row of the order table is inside this hunk. The owner first:
    /// it is the compare every row of every other file pays, which is all of
    /// them but the hunk's own.
    fn contains(&self, owner: u16, index: u32) -> bool {
        self.owner == owner && self.span.0 <= index && index < self.span.1
    }
}

/// The hunk standing over one row of the order table, as its presentation
/// holds it. Once per frame, not once per row.
fn extent_of(renderers: &[Box<dyn Rows>], owner: u16, index: u32) -> Option<HunkExtent> {
    let span = renderers.get(owner as usize)?.hunk_span(index as usize)?;
    Some(HunkExtent { owner, span })
}

/// Where each hunk starts, as a fraction of the order table — the scrollbar
/// ticks' whole input.
///
/// Computed where the order table is — load, reflow, a layout change — and
/// never per frame. It walks the flat order once, asking each renderer's
/// [`Rows::hunk_span`] for the hunk a row belongs to (the input 051's
/// [`HunkMap`] already serves; nothing here re-derives hunks) and recording
/// `hunk_start_row / total_rows` for a hunk's own first visual row. That is
/// the row a `scroll_to_item` to the hunk's start lands the viewport on, and
/// therefore where the thumb's top sits — a uniform list normalizes a row to
/// a track offset by plain division, which is exactly what a fraction of the
/// row count is.
///
/// Mark data, not hunk data: a fraction per hunk and nothing else. The
/// painter takes offsets and an ink — [`crate::views::track_marks`] — so
/// search marks later are another caller, not another painter.
fn hunk_marks(order: &[RowRef], renderers: &[Box<dyn Rows>]) -> Vec<f32> {
    let total = order.len();
    let mut marks = Vec::new();
    if total == 0 {
        return marks;
    }
    // The hunk the walk is inside of, per owner: the compare every row pays
    // before it may ask its renderer, so the walk is linear in the rows and
    // a search only per hunk.
    let mut open: Option<(u16, u32, u32)> = None;
    for (i, r) in order.iter().enumerate() {
        let span = match open {
            Some((owner, span, end)) if owner == r.owner && r.index < end => (span, end),
            _ => match renderers
                .get(r.owner as usize)
                .and_then(|rows| rows.hunk_span(r.index as usize))
            {
                Some(s) => {
                    open = Some((r.owner, s.0, s.1));
                    s
                }
                None => {
                    open = None;
                    continue;
                }
            },
        };
        // The hunk's header row is its first, and its first visual row is
        // that row's first segment: one tick per hunk, not one per segment.
        if r.seg == 0 && span.0 == r.index {
            marks.push(i as f32 / total as f32);
        }
    }
    marks
}

/// One line-number column.
///
/// **Right-aligned**, which is the whole reason this is a flex row and not a
/// `div` with text in it. A column of left-aligned numbers puts the units digit
/// of `9` and of `1234` four characters apart, so the one thing the eye does with
/// this column — run down it — is the one thing it cannot do. Every diff tool
/// aligns it the other way.
///
/// `fg` comes from [`Theme::gutter_on`] and not from `diff.gutter_fg` directly,
/// because a number is drawn on whatever background its line has and that grey
/// is 2.05:1 on a context row and 1.60:1 on a moved one.
pub(crate) fn num(n: SharedString, fg: Rgb) -> Div {
    div()
        .flex()
        .flex_none()
        .justify_end()
        .w(px(GUTTER_W))
        .pr(px(GUTTER_PAD))
        .text_color(rgb(fg))
        .child(n)
}

/// What drawing borrows: the buffers a presentation reuses across the visible
/// rows of a frame.
///
/// The render path allocates nothing per row. Gutter numbers are formatted
/// into `no` and adopted inline by reference; the run list is swept into
/// `runs` by `core`'s buffer-passing merge and resolved into `hl`. Both clear
/// per cell and grow once ever — after the first frame nothing here touches
/// the heap, which is rule 3 and the reason this replaced an edges-`Vec` plus
/// an output-`Vec` per visible row per frame.
#[derive(Default)]
pub(crate) struct Scratch {
    /// One gutter number at a time.
    no: String,
    /// The sweep's output, in line coordinates.
    runs: Vec<Run>,
    /// The same runs theme-resolved and rebased into row coordinates.
    hl: Vec<(Range<usize>, HighlightStyle)>,
}

impl Scratch {
    /// A number as the row draws it: formatted into the scratch string and
    /// handed over by reference, so GPUI copies it straight into its own
    /// inline representation. Numbers are far under the 23-byte inline
    /// capacity, so this is a stack-format and a short memcpy — never a heap
    /// allocation, which is what pre-rendering them at load cost instead:
    /// forty-eight bytes of string per line of a 714k-line diff to describe
    /// two integers.
    ///
    /// Right-alignment is [`num`]'s (`justify_end` in a fixed-width column)
    /// and padding is no part of that, so a formatted digit reads exactly
    /// where a stored one did.
    pub(crate) fn number(&mut self, n: Option<u32>, blank: bool) -> SharedString {
        match n.filter(|_| !blank) {
            Some(n) => {
                use std::fmt::Write as _;
                self.no.clear();
                let _ = write!(self.no, "{n}");
                SharedString::from(self.no.as_str())
            }
            None => SharedString::default(),
        }
    }

    /// The merged style list one row draws with: `core`'s single sweep — the
    /// same implementation the terminal uses, selection folded in — produces
    /// the runs, and resolving each against the theme is all that is left
    /// here. Plain stretches are dropped, which is exactly what the
    /// edge-sorting version this replaced emitted: only bytes carrying a
    /// colour or a background are listed.
    ///
    /// The returned slice borrows `self.hl`; bind the borrow at the call site
    /// so it outlives `with_highlights`'s copy into its own element.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn merged(
        &mut self,
        at: Range<usize>,
        tokens: &[Token],
        spans: &[Span],
        theme: &Theme,
        kind: LineKind,
        moved: bool,
        current: bool,
        sel: Range<usize>,
    ) -> &[(Range<usize>, HighlightStyle)] {
        runs::runs_selected(at.clone(), tokens, spans, kind, moved, sel, &mut self.runs);
        let selected_bg = rgb(theme.background(Surface::Selected));
        self.hl.clear();
        self.hl.extend(self.runs.iter().filter_map(|r| {
            let on_sel = r.surface == Surface::Selected;
            if r.kind.is_none() && !r.word && !on_sel {
                return None;
            }
            // A token resolves against the surface it lands on, so a selected
            // or changed byte gets a foreground that reads on that background.
            // On the keyboard's row the plain one is the wash — the row paints
            // `selection_bg` over whatever the line was, so a token resolved
            // for it was resolved against a background it never lands on. A
            // changed word keeps its own: the wash is a bar *under* the row and
            // the word is the thing being read on it.
            let surface = match (current, r.word) {
                (true, false) => Surface::Cursor,
                _ => r.surface,
            };
            let style = r.kind.map(|k| theme.syntax_on(k, surface));
            Some((
                r.at.start - at.start..r.at.end - at.start,
                HighlightStyle {
                    color: style.map(|s| rgb(s.fg).into()),
                    background_color: match (on_sel, r.word) {
                        (true, _) => Some(selected_bg.into()),
                        (false, true) => Some(rgb(theme.background(r.surface)).into()),
                        (false, false) => None,
                    },
                    font_weight: style.filter(|s| s.bold).map(|_| FontWeight::BOLD),
                    font_style: style.filter(|s| s.italic).map(|_| FontStyle::Italic),
                    ..Default::default()
                },
            ))
        }));
        &self.hl
    }
}

#[cfg(test)]
mod tests {
    // By name, not a glob: `use gpui::*` in the parent shadows `#[test]` with
    // GPUI's own attribute macro and every test in here fails to expand.
    use super::{
        extent_ink, extent_of, file_header, hunk_header, line_colors, locked, row_background,
        row_bar, Diff, FileSummary, Layouts, Pan, Row, RowState, Rows, TextRows, PAD, ROW_H,
        TEXT_CHROME,
    };
    use gitten_core::font::Font;
    use gitten_core::host::Host;
    use gitten_core::prepared::{prepare, File as PreparedFile};
    use gitten_core::select::{Caret, Selected, Selection};
    use gitten_core::syntax::{Kind, Token};
    use gitten_core::theme::{Style, Surface, Theme};
    use gitten_core::{parse_unified_diff, FileDiff, LineKind, Span};
    use gpui::{
        div, rgb, AnyElement, FontStyle, FontWeight, HighlightStyle, IntoElement, ParentElement,
        ScrollStrategy,
    };
    use std::cell::Cell;
    use std::rc::Rc;

    fn tok(start: u32, end: u32, kind: Kind) -> Token {
        Token { start, end, kind }
    }

    /// The whole line — what every row that did not wrap asks `runs` for.
    fn all(text: &str) -> std::ops::Range<usize> {
        0..text.len()
    }

    /// The merge with nothing selected, which is what everything below but the
    /// selection tests is about. Shadows the real one so those tests read as they
    /// did before a selection was a layer in it.
    fn runs(
        at: std::ops::Range<usize>,
        tokens: &[Token],
        spans: &[Span],
        theme: &Theme,
        kind: LineKind,
        moved: bool,
    ) -> Vec<(std::ops::Range<usize>, HighlightStyle)> {
        runs_sel(at, tokens, spans, theme, kind, moved, 0..0)
    }

    /// The same, over an explicit selection.
    fn runs_sel(
        at: std::ops::Range<usize>,
        tokens: &[Token],
        spans: &[Span],
        theme: &Theme,
        kind: LineKind,
        moved: bool,
        sel: std::ops::Range<usize>,
    ) -> Vec<(std::ops::Range<usize>, HighlightStyle)> {
        let mut sc = super::Scratch::default();
        super::Scratch::merged(&mut sc, at, tokens, spans, theme, kind, moved, false, sel).to_vec()
    }

    fn well_formed(text: &str, runs: &[(std::ops::Range<usize>, HighlightStyle)]) {
        assert!(
            runs.windows(2).all(|w| w[0].0.end <= w[1].0.start),
            "overlapping: {runs:?}"
        );
        for (r, _) in runs {
            assert!(
                r.start < r.end && r.end <= text.len(),
                "{r:?} outside {text:?}"
            );
            assert!(
                text.is_char_boundary(r.start) && text.is_char_boundary(r.end),
                "{r:?}"
            );
        }
    }

    #[test]
    fn plain_text_produces_no_runs_at_all() {
        let theme = Theme::dark();
        assert!(runs(0..12, &[], &[], &theme, LineKind::Context, false).is_empty());
    }

    #[test]
    fn a_token_and_a_span_over_the_same_bytes_split_into_both() {
        // `let` is a keyword and also a changed word: one run carrying a
        // foreground and a background, not two elements fighting over it.
        let theme = Theme::dark();
        let text = "let x = 1;";
        let out = runs(
            all(text),
            &[tok(0, 3, Kind::Keyword)],
            &[Span { start: 0, end: 3 }],
            &theme,
            LineKind::Added,
            false,
        );
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
        let theme = Theme::dark();
        let text = "let x = 1;";
        let out = runs(
            all(text),
            &[tok(0, 3, Kind::Keyword)],
            &[Span { start: 2, end: 7 }],
            &theme,
            LineKind::Added,
            false,
        );
        well_formed(text, &out);
        let shape: Vec<_> = out
            .iter()
            .map(|(r, s)| (r.clone(), s.color.is_some(), s.background_color.is_some()))
            .collect();
        assert_eq!(
            shape,
            vec![(0..2, true, false), (2..3, true, true), (3..7, false, true)]
        );
    }

    #[test]
    fn multi_byte_text_keeps_its_boundaries() {
        let theme = Theme::dark();
        let text = "let s = \"café 😀\";";
        let quote = text.find('"').unwrap();
        let out = runs(
            all(text),
            &[
                tok(0, 3, Kind::Keyword),
                tok(quote as u32, (text.len() - 1) as u32, Kind::Str),
            ],
            &[Span {
                start: quote as u32,
                end: (text.len() - 1) as u32,
            }],
            &theme,
            LineKind::Added,
            false,
        );
        well_formed(text, &out);
    }

    #[test]
    fn weight_and_slant_reach_the_run_list() {
        // A Markdown `**word**` that only changed colour would be wrong, so the
        // theme's bold and italic have to survive the merge.
        let mut theme = Theme::dark();
        theme.set_syntax(Kind::Strong, Style::fg(0xffffff).bold());
        theme.set_syntax(Kind::Emphasis, Style::fg(0xcccccc).italic());
        let text = "**bold** and *thin*";
        let out = runs(
            all(text),
            &[tok(0, 8, Kind::Strong), tok(13, 19, Kind::Emphasis)],
            &[],
            &theme,
            LineKind::Context,
            false,
        );
        well_formed(text, &out);
        assert_eq!(out[0].1.font_weight, Some(FontWeight::BOLD));
        assert_eq!(out[0].1.font_style, None);
        assert_eq!(out[1].1.font_style, Some(FontStyle::Italic));
        assert_eq!(out[1].1.font_weight, None);
    }

    #[test]
    fn a_moved_line_is_drawn_on_its_own_background() {
        // The point of move detection: a moved block has to recede from the
        // add/remove hues, or there is nothing to skip.
        let theme = Theme::dark();
        let p = &theme.diff;
        for kind in [LineKind::Added, LineKind::Removed] {
            let (plain, _, sign) = line_colors(kind, false, p);
            let (moved, _, moved_sign) = line_colors(kind, true, p);
            assert_ne!(
                plain, moved,
                "{kind:?} moved and unmoved share a background"
            );
            assert_eq!(sign, moved_sign, "the sign column must stay scannable");
        }
        // Context is never moved, and asking must not change what it looks like.
        assert_eq!(
            line_colors(LineKind::Context, true, p),
            line_colors(LineKind::Context, false, p)
        );
    }

    #[test]
    fn the_cursor_bar_beats_every_row_background_in_every_presentation() {
        // One helper answers for text rows, split cells, headers and markdown
        // prose alike — this is the assertion that it answers with the bar,
        // and only with the bar. The regression it holds: ordinary markdown
        // lines painted their diff background over the cursor because the
        // decision was written out per presentation and one of them forgot.
        let theme = Theme::dark();
        let p = &theme.diff;
        let kinds = [
            (LineKind::Context, false),
            (LineKind::Context, true),
            (LineKind::Added, false),
            (LineKind::Added, true),
            (LineKind::Removed, false),
            (LineKind::Removed, true),
        ];
        for (kind, moved) in kinds {
            let (base, _, _) = line_colors(kind, moved, p);
            assert_eq!(
                row_background(true, base, &theme),
                theme.chrome.selection_bg,
                "{kind:?} hid the cursor"
            );
            assert_eq!(row_background(false, base, &theme), base);
        }
        // The furniture rows the presentations share.
        assert_eq!(
            row_background(true, p.file_bg, &theme),
            theme.chrome.selection_bg
        );
        assert_eq!(
            row_background(true, p.hunk_bg, &theme),
            theme.chrome.selection_bg
        );
        assert_eq!(
            row_background(true, p.absent_bg, &theme),
            theme.chrome.selection_bg
        );
        assert_eq!(row_background(false, p.file_bg, &theme), p.file_bg);
        // The bar, and nothing else: accent while the row's pane holds the
        // keyboard, faint when the selection is remembered and the keyboard is
        // elsewhere, and the row's own background when the row is not the
        // cursor's — which is what keeps the text from shifting a pixel.
        let state = |current: bool, focused: bool| RowState {
            current,
            focused,
            armed: false,
            in_hunk: false,
        };
        assert_eq!(
            row_bar(state(true, true), p.context_bg, &theme),
            theme.chrome.accent
        );
        assert_eq!(
            row_bar(state(true, false), p.context_bg, &theme),
            theme.chrome.faint
        );
        assert_eq!(
            row_bar(state(false, true), p.context_bg, &theme),
            row_background(false, p.context_bg, &theme)
        );
    }
    #[test]
    fn every_row_of_the_cursors_hunk_reports_the_extent() {
        // The keyboard mid-hunk: every row of the hunk it sits in — its
        // header row, its lines, nothing else — reads in extent. That range
        // is what the hairline in the gutter marks row by row, and the
        // answer to "which lines would `space` stage".
        let host = Rc::new(Host::new());
        let mut d = Diff::with_layouts(parse_unified_diff(THREE_HUNKS), &host, Layouts::builtin());
        with_height(&mut d, 20);

        // Unified layout rows: file header at 0; hunk 0 owns its header row
        // and its four lines (1..=5); hunk 1 its header and three lines
        // (6..=9). A file header owns no hunk — the keyboard parked there
        // reports none.
        let r = d.order[0];
        let renderers = d.renderers.borrow();
        assert!(
            extent_of(&renderers, r.owner, r.index).is_none(),
            "row 0 is a file header"
        );
        drop(renderers);

        // The keyboard onto hunk 0's header row.
        d.run_view("view.down", &host);
        let r = d.order[d.view.get().cursor()];
        let e = extent_of(&d.renderers.borrow(), r.owner, r.index).expect("on a hunk");
        let rows: Vec<usize> = (0..d.order.len())
            .filter(|&i| {
                let r = d.order[i];
                e.contains(r.owner, r.index)
            })
            .collect();
        assert_eq!(rows, vec![1, 2, 3, 4, 5], "the hunk's rows, and only them");
        // The boundaries: the file header above is out, and so is the next
        // hunk's header — and the second file's first hunk is out twice
        // over, its own hunk number 0 notwithstanding.
        assert!(!e.contains(r.owner, 0), "row 0 is above the extent");
        assert!(!e.contains(r.owner, 6), "row 6 is the next hunk's header");
        let two = d.order[12];
        assert!(
            !e.contains(two.owner, two.index),
            "the second file maps to no extent here"
        );
    }

    #[test]
    fn the_armed_tint_covers_exactly_the_armed_hunk() {
        // Armed via the discard verb, the way `D` asks: the tint spans the
        // hunk the second press would spend — its header row through its
        // last line — and nothing outside it. A move of the keyboard
        // disarms, as it always has, so the tint cannot outlive the
        // question it was asked about.
        let host = Rc::new(Host::new());
        let mut d = Diff::with_layouts(parse_unified_diff(THREE_HUNKS), &host, Layouts::builtin());
        with_height(&mut d, 20);
        // Walk to hunk 1's first changed line — row 6 is its header, 7 is
        // `delta` — and arm.
        for _ in 0..7 {
            d.run_view("view.down", &host);
        }
        let id = d.cursor_row_id();
        assert!(!d.confirm_or_arm_discard_hunk(id), "first press asks");
        let armed = d.armed_hunk.expect("armed, and waiting");
        let e = extent_of(&d.renderers.borrow(), armed.0, armed.1)
            .expect("the armed row is a hunk row");
        let rows: Vec<usize> = (0..d.order.len())
            .filter(|&i| {
                let r = d.order[i];
                e.contains(r.owner, r.index)
            })
            .collect();
        assert_eq!(
            rows,
            vec![6, 7, 8, 9],
            "the armed hunk's rows, and only them"
        );
        d.run_view("view.down", &host);
        assert!(d.armed_hunk.is_none(), "a cursor move disarms");
    }

    #[test]
    fn a_header_row_carries_the_cursor_bar() {
        // `file_header` and `hunk_header` take the RowState the line rows do
        // and ask the same `row_bar` for their bar's ink — so the cursor is
        // one shape parked on a header and parked on a line — and the extent
        // hairline inside it takes `extent_ink`'s: `diff.rule` when the row
        // is in the hunk, the header's own background when it is not, and
        // the bar's ink on the keyboard's row, where the 2px bar wins.
        let theme = Theme::dark();
        let p = &theme.diff;
        let header: std::sync::Arc<str> = std::sync::Arc::from("@@ -1,3 +1,3 @@");
        let path: std::sync::Arc<str> = std::sync::Arc::from("one.txt");
        let state = |current: bool, in_hunk: bool| RowState {
            current,
            focused: true,
            armed: false,
            in_hunk,
        };
        // Both headers draw with the keyboard on them — and, one state down,
        // with the hairline carrying the extent.
        let _ = hunk_header(&header, &theme, None, state(true, true), 0.0);
        let _ = file_header(
            &path,
            1,
            2,
            &theme,
            &Font::default(),
            None,
            state(true, true),
            0.0,
        );
        let _ = hunk_header(&header, &theme, None, state(false, true), 0.0);
        let _ = file_header(
            &path,
            1,
            2,
            &theme,
            &Font::default(),
            None,
            state(false, true),
            0.0,
        );
        // The bar's ink is the line rows', whatever the header's own
        // background: accent on the keyboard's row, and the row's own
        // background elsewhere.
        assert_eq!(
            row_bar(state(true, false), p.hunk_bg, &theme),
            theme.chrome.accent
        );
        assert_eq!(row_bar(state(false, false), p.hunk_bg, &theme), p.hunk_bg);
        assert_eq!(row_bar(state(false, false), p.file_bg, &theme), p.file_bg);
        // The hairline's ink: `diff.rule` in the extent, the row's own
        // background out of it, and the bar's ink on the keyboard's row.
        assert_eq!(extent_ink(state(false, true), p.hunk_bg, &theme), p.rule);
        assert_eq!(
            extent_ink(state(false, false), p.file_bg, &theme),
            p.file_bg
        );
        assert_eq!(
            extent_ink(state(true, true), p.hunk_bg, &theme),
            row_bar(state(true, true), p.hunk_bg, &theme)
        );
    }

    #[test]
    fn a_layout_cycle_disarms_before_the_rows_mean_something_else() {
        // The rows are about to be re-arranged; whatever the question was
        // armed against may land somewhere else — the same reason a selection
        // goes, and the same clear every cursor move already does.
        let host = Rc::new(Host::new());
        let mut d = Diff::with_layouts(parse_unified_diff(THREE_HUNKS), &host, Layouts::builtin());
        let id = d.cursor_row_id();
        assert!(!d.confirm_or_arm_discard_hunk(id), "first press asks");
        assert!(d.armed_hunk.is_some(), "armed, and waiting");
        d.apply_layout(1, &host);
        assert!(
            d.armed_hunk.is_none(),
            "the cycle spent nothing: it cleared"
        );
    }

    #[test]
    fn a_token_on_a_moved_line_is_resolved_against_that_background() {
        // The reason `Surface` gained two variants rather than the moved
        // background being painted under an unmoved foreground: the contrast
        // resolver has to see the background the text actually lands on.
        //
        // The shipped theme's moved backgrounds sit at almost the same luminance
        // as the ones they replace, so its greys come out identical — which is
        // the resolver being stable, not the surfaces being ignored. A theme that
        // moves the background properly is what shows the difference.
        let mut theme = Theme::dark();
        theme.diff.moved_removed_bg = 0xf2ede6;
        theme.rebuild();
        let text = "// a comment that moved";
        let tokens = [tok(0, text.len() as u32, Kind::Comment)];
        let plain = runs(all(text), &tokens, &[], &theme, LineKind::Removed, false);
        let moved = runs(all(text), &tokens, &[], &theme, LineKind::Removed, true);
        well_formed(text, &plain);
        well_formed(text, &moved);
        assert_ne!(plain[0].1.color, moved[0].1.color, "the same grey on both");
    }

    #[test]
    fn a_moved_line_lights_up_no_changed_words() {
        // A moved line is the same text somewhere else, so a changed-word
        // background on it would be describing a change the detection just said
        // was not one.
        let theme = Theme::dark();
        let text = "let x = 1;";
        let spans = [Span { start: 0, end: 3 }];
        let out = runs(all(text), &[], &spans, &theme, LineKind::Added, true);
        let unmoved = runs(all(text), &[], &spans, &theme, LineKind::Added, false);
        assert!(unmoved.iter().any(|(_, s)| s.background_color.is_some()));
        assert!(
            out.is_empty(),
            "a moved line produced runs for nothing: {out:?}"
        );
    }

    #[test]
    fn a_comment_on_a_changed_word_is_lifted_off_the_background() {
        // The regression from a screenshot: a whole rewritten comment line sits
        // under the changed-word background, and the comment grey measured
        // 1.15:1 against it — a smear. The run that lands on the word background
        // must not carry the same foreground as the run that does not.
        let theme = Theme::dark();
        let text = "# Collect every check failure before exiting";
        let out = runs(
            all(text),
            &[tok(0, text.len() as u32, Kind::Comment)],
            &[Span {
                start: 10,
                end: text.len() as u32,
            }],
            &theme,
            LineKind::Added,
            false,
        );
        well_formed(text, &out);
        let plain = out.iter().find(|(r, _)| r.start == 0).unwrap();
        let on_word = out.iter().find(|(r, _)| r.start == 10).unwrap();
        assert!(on_word.1.background_color.is_some());
        assert_ne!(
            plain.1.color, on_word.1.color,
            "same grey on both backgrounds"
        );
    }

    // ------------------------------------------------------ the draw scratch

    #[test]
    fn the_draw_scratch_grows_once_and_stops() {
        // Rule 3, on the path every visible row of every frame walks: neither
        // the sweep's buffer nor the resolved run list may grow on a repaint.
        let theme = Theme::dark();
        let text = "let alpha = 1; let beta = 2;";
        let tokens = [tok(0, 3, Kind::Keyword), tok(15, 18, Kind::Keyword)];
        let spans = [Span { start: 4, end: 9 }, Span { start: 19, end: 23 }];
        let mut sc = super::Scratch::default();
        let out = super::Scratch::merged(
            &mut sc,
            all(text),
            &tokens,
            &spans,
            &theme,
            LineKind::Added,
            false,
            false,
            6..20,
        );
        well_formed(text, out);
        let caps = (sc.runs.capacity(), sc.hl.capacity());
        for _ in 0..100 {
            super::Scratch::merged(
                &mut sc,
                all(text),
                &tokens,
                &spans,
                &theme,
                LineKind::Added,
                false,
                false,
                6..20,
            );
        }
        assert_eq!(
            (sc.runs.capacity(), sc.hl.capacity()),
            caps,
            "a repaint grew a buffer"
        );
    }

    #[test]
    fn a_gutter_number_formats_into_the_scratch_and_pads_nowhere() {
        // The integers replaced pre-rendered strings, so what reaches the
        // screen must be exactly what those strings were: bare digits, nothing
        // padded in — right-alignment is the column's, not the text's.
        let mut sc = super::Scratch::default();
        assert_eq!(&*sc.number(Some(9), false), "9");
        assert_eq!(&*sc.number(Some(12345), false), "12345");
        assert_eq!(
            &*sc.number(Some(7), true),
            "",
            "a continuation row draws nothing"
        );
        assert_eq!(
            &*sc.number(None, false),
            "",
            "so does a side with no number"
        );
        let drawn = sc.number(Some(41), false);
        assert_eq!(&*drawn, "41");
        assert_eq!(&*sc.no, "41", "the scratch is the one home of the digits");
    }

    const SAMPLE: &str = "\
diff --git a/a.rs b/a.rs
@@ -1,2 +1,2 @@
 fn main() {
-    let x = 1;
+    let x = 2;
";

    // ------------------------------------------------------------ selection

    /// `x` pixels for a click `col` characters into a line's text, in the
    /// default face. Half a character in, so the rounding in [`column_at`] is
    /// not deciding the test.
    fn x_for(col: usize, host: &Host) -> f32 {
        TEXT_CHROME - PAD + (col as f32 + 0.1) * host.font.size * host.font.advance
    }

    #[test]
    fn a_click_lands_on_the_byte_under_it() {
        let (r, host) = text_rows(SAMPLE);
        // Row 3 is `-    let x = 1;`, stored without its sign.
        let text = r.selectable(3, 0).expect("a line");
        assert_eq!(text, "    let x = 1;");
        for col in [0, 4, 7, 13] {
            let hit = r.hit(3, 0, x_for(col, &host), &host, 0.0).expect("a hit");
            assert_eq!((hit.part, hit.off), (0, col), "column {col}");
        }
        // Past the end of the text clamps to the end of it rather than reaching
        // into whatever is at that byte of the next line.
        let hit = r.hit(3, 0, x_for(400, &host), &host, 0.0).unwrap();
        assert_eq!(hit.off, text.len());
        // And to the left of the text — in the gutter — is the start of it.
        let hit = r.hit(3, 0, 0.0, &host, 0.0).unwrap();
        assert_eq!(hit.off, 0);
    }

    #[test]
    fn a_click_on_a_continuation_row_addresses_the_line_and_not_the_row() {
        // The failure this prevents is the same one `runs` guards against from
        // the other side: a caret in *row* coordinates would select from the
        // start of the line every time you clicked on a wrapped one.
        let (mut r, host) = text_rows(LONG);
        assert!(r.reflow(width_for(40, &host), &host, host.wrap.current()));
        let row = (0..r.len())
            .find(|i| r.rows(*i) > 1)
            .expect("a wrapped line");
        let first = r.hit(row, 0, x_for(3, &host), &host, 0.0).unwrap().off;
        let second = r.hit(row, 1, x_for(3, &host), &host, 0.0).unwrap().off;
        assert_eq!(first, 3);
        assert!(
            second > 30,
            "the second row rebased to {second}, not into the line"
        );
        // And the byte it names is the byte drawn there.
        let text = r.selectable(row, 0).unwrap();
        let at = r.wrapped.range(row, 1, text);
        assert_eq!(second, at.start + 3);
    }

    #[test]
    fn a_header_is_selectable_because_a_paste_of_three_files_needs_its_paths() {
        let (r, host) = text_rows(SAMPLE);
        assert_eq!(r.selectable(0, 0), Some("a.rs"));
        assert_eq!(r.selectable(1, 0), Some("@@ -1,2 +1,2 @@"));
        // Drawn at the page padding and nowhere else, whichever presentation
        // owns the lines beneath it.
        let hit = r
            .hit(
                0,
                0,
                PAD + 2.5 * host.font.size * host.font.advance,
                &host,
                0.0,
            )
            .unwrap();
        assert_eq!(hit.off, 2);
    }

    #[test]
    fn a_selection_over_three_rows_copies_the_lines_between_them() {
        let host = Host::new();
        let mut diff = Diff::with_layouts(parse_unified_diff(SAMPLE), &host, Layouts::builtin());
        // Rows: the hunk header, then three lines — one file, so no file header.
        diff.sel = Some(select(&diff, (1, 0), (3, 9)));
        assert_eq!(
            diff.selection(),
            "fn main() {
    let x = 1;
    let x"
        );
        // The anchor is not the start: the same drag backwards is the same text.
        diff.sel = Some(select(&diff, (3, 9), (1, 0)));
        assert_eq!(
            diff.selection(),
            "fn main() {
    let x = 1;
    let x"
        );
    }

    #[test]
    fn a_wrapped_line_copies_once_and_without_the_break() {
        // The window's width is not part of the text. Pasting the soft breaks
        // back would paste the size of somebody's window into their file.
        let host = Host::new();
        let mut diff = Diff::with_layouts(parse_unified_diff(LONG), &host, Layouts::builtin());
        diff.reflow(width_for(30, &host), &host);
        let long = *diff
            .order
            .iter()
            .find(|r| diff.renderers.borrow()[r.owner as usize].rows(r.index as usize) > 1)
            .expect("a line that wrapped");
        let whole = diff.renderers.borrow()[long.owner as usize]
            .selectable(long.index as usize, 0)
            .unwrap()
            .to_string();
        let at = diff.order.iter().position(|r| *r == long).unwrap();
        let n = diff.renderers.borrow()[long.owner as usize].rows(long.index as usize);
        let mut sel = Selection::new(
            0,
            Caret {
                row: long.logical(),
                off: 0,
                at: at..at + n,
            },
        );
        sel.extend(Caret {
            row: long.logical(),
            off: whole.len(),
            at: at..at + n,
        });
        diff.sel = Some(sel);
        assert_eq!(diff.selection(), whole);
        assert!(!whole.is_empty());
    }

    #[test]
    fn a_selection_survives_a_reflow_and_dies_with_a_layout_change() {
        // The two halves of the rule: a wrap is the same diff at a different
        // width, and a layout is a different diff of the same repository.
        let host = Host::new();
        let mut diff = Diff::with_layouts(parse_unified_diff(LONG), &host, Layouts::builtin());
        diff.reflow(width_for(200, &host), &host);
        diff.sel = Some(select(&diff, (2, 0), (3, 10)));
        let text = diff.selection();

        diff.reflow(width_for(24, &host), &host);
        assert!(diff.sel.is_some(), "a resize threw the selection away");
        assert_eq!(diff.selection(), text, "the same bytes, at a new width");

        diff.apply_layout(1, &host);
        assert!(diff.sel.is_none(), "the rows are somebody else's now");
        assert_eq!(diff.selection(), "");
    }

    #[test]
    fn selecting_everything_reaches_the_end_of_the_last_line() {
        let host = Host::new();
        let mut diff = Diff::with_layouts(parse_unified_diff(SAMPLE), &host, Layouts::builtin());
        diff.sel = Selection::all(&diff.order);
        assert_eq!(
            diff.selection(),
            "@@ -1,2 +1,2 @@\nfn main() {\n    let x = 1;\n    let x = 2;",
            "one file: the pane header names it, so the body has no file header row"
        );
    }

    #[test]
    fn nothing_selected_copies_nothing_rather_than_everything() {
        let host = Host::new();
        let diff = Diff::with_layouts(parse_unified_diff(SAMPLE), &host, Layouts::builtin());
        assert!(diff.sel.is_none());
        assert_eq!(diff.selection(), "");
    }

    #[test]
    fn a_selected_run_carries_a_background_and_outranks_a_changed_word() {
        // Both are backgrounds and only one can be drawn. The reader already
        // knows which words changed; what they are about to press a key about is
        // what is selected.
        let theme = Theme::dark();
        let text = "    let x = 1;";
        let out = runs_sel(
            all(text),
            &[],
            &[Span { start: 8, end: 9 }],
            &theme,
            LineKind::Removed,
            false,
            4..11,
        );
        well_formed(text, &out);
        // The sweep splits at every edge and does not coalesce, so what matters
        // is which *bytes* carry it — not how many runs they arrived in.
        let selected = rgb(theme.background(Surface::Selected));
        let word = rgb(theme.background(Surface::RemovedWord));
        let painted: Vec<usize> = out
            .iter()
            .filter(|(_, s)| s.background_color == Some(selected.into()))
            .flat_map(|(r, _)| r.clone())
            .collect();
        assert_eq!(painted, (4..11).collect::<Vec<_>>());
        assert!(
            !out.iter()
                .any(|(r, s)| r.contains(&8) && s.background_color == Some(word.into())),
            "the changed word kept its background inside the selection"
        );
    }

    #[test]
    fn a_hunk_header_paints_its_coordinates_and_leaves_its_code_alone() {
        // The two-tone header is one `StyledText` with runs, not two elements —
        // `header_hit` measures a click against the whole string. So the split
        // has to come out as bytes, and the declaration git appends has to carry
        // no run at all: it is already the row's own `hunk_fg`.
        let theme = Theme::dark();
        let header = "@@ -41,9 +41,11 @@ fn dispatch() {";
        let (marker, code) = gitten_core::hunk_parts(header);
        let out = super::hunk_runs(marker.len(), 0..0, &theme);
        well_formed(header, &out);
        assert_eq!(out.len(), 1, "one run: the coordinates");
        assert_eq!(out[0].0, 0..marker.len());
        assert_eq!(
            out[0].1.color,
            Some(rgb(theme.gutter_on(Surface::Context)).into())
        );
        assert!(out[0].1.background_color.is_none());
        assert!(!code.is_empty() && out.iter().all(|(r, _)| r.end <= marker.len()));
    }

    #[test]
    fn a_selection_across_a_hunk_header_keeps_both_of_its_colours() {
        // The case two side-by-side elements could not draw: one selection whose
        // ends live in different halves of the header.
        let theme = Theme::dark();
        let header = "@@ -41,9 +41,11 @@ fn dispatch() {";
        let marker = gitten_core::hunk_parts(header).0.len();
        let out = super::hunk_runs(marker, 5..25, &theme);
        well_formed(header, &out);
        let bg = rgb(theme.chrome.selected_bg);
        let painted: Vec<usize> = out
            .iter()
            .filter(|(_, st)| st.background_color == Some(bg.into()))
            .flat_map(|(r, _)| r.clone())
            .collect();
        assert_eq!(
            painted,
            (5..25).collect::<Vec<_>>(),
            "the selection, exactly"
        );
        // ...and the coordinates are still the coordinates inside it.
        let fg = rgb(theme.gutter_on(Surface::Context));
        let coloured: Vec<usize> = out
            .iter()
            .filter(|(_, st)| st.color == Some(fg.into()))
            .flat_map(|(r, _)| r.clone())
            .collect();
        assert_eq!(coloured, (0..marker).collect::<Vec<_>>());
    }

    #[test]
    fn a_header_with_no_coordinates_is_all_coordinates() {
        // `hunk_parts` hands the whole string back when there is no `@@` pair —
        // it is the *tail* that may be missing, not the head — so a malformed
        // header comes out entirely in the coordinates' colour. Which is the
        // right answer for a string that is not a hunk header: quiet, whole, and
        // not half-painted at an offset nothing chose.
        let theme = Theme::dark();
        let header = "not a hunk header";
        let marker = gitten_core::hunk_parts(header).0.len();
        assert_eq!(marker, header.len(), "the whole string is the marker");
        let out = super::hunk_runs(marker, 0..0, &theme);
        well_formed(header, &out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, 0..header.len());
        // A zero-length marker — the one case with nothing to colour — is a
        // selection background and nothing else, rather than an empty run.
        let out = super::hunk_runs(0, 2..6, &theme);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, 2..6);
        assert!(out[0].1.color.is_none(), "no coordinates to colour");
        assert!(
            super::hunk_runs(0, 0..0, &theme).is_empty(),
            "nothing at all"
        );
    }

    #[test]
    fn a_paths_selection_is_split_between_its_directory_and_its_name() {
        // The file header draws `src/views/` and `diff.rs` as two elements, and
        // the selection is one range over the whole path. Each element takes the
        // part of it that it actually covers, rebased to its own start.
        let cut = "src/views/".len();
        let len = "src/views/diff.rs".len();
        // A selection over the whole path.
        assert_eq!(super::clipped(&(0..len), 0..cut), 0..cut);
        assert_eq!(super::clipped(&(0..len), cut..len), 0..len - cut);
        // One that straddles the boundary: three bytes of directory, four of name.
        let sel = cut - 3..cut + 4;
        assert_eq!(super::clipped(&sel, 0..cut), cut - 3..cut);
        assert_eq!(super::clipped(&sel, cut..len), 0..4);
        // One that misses each side entirely is empty, not inverted.
        assert!(super::clipped(&(0..2), cut..len).is_empty());
        assert!(super::clipped(&(cut + 1..len), 0..cut).is_empty());
        // Nothing selected at all, which is nearly every header of every frame.
        assert!(super::clipped(&(0..0), cut..len).is_empty());
    }

    #[test]
    fn a_selection_is_clipped_into_the_row_that_draws_it() {
        // Line coordinates in, row coordinates out — the same contract tokens
        // and spans have, and the same off-by-one available to it.
        let theme = Theme::dark();
        let text = "aaaa bbbb cccc";
        let first = runs_sel(0..5, &[], &[], &theme, LineKind::Context, false, 2..12);
        let second = runs_sel(
            5..text.len(),
            &[],
            &[],
            &theme,
            LineKind::Context,
            false,
            2..12,
        );
        assert_eq!(first.first().map(|(r, _)| r.clone()), Some(2..5));
        assert_eq!(second.first().map(|(r, _)| r.clone()), Some(0..7));
        // A selection that misses this row entirely leaves no runs at all.
        assert!(runs_sel(5..14, &[], &[], &theme, LineKind::Context, false, 0..2).is_empty());
    }

    // ------------------------------------------------------------- wrapping

    /// A diff with one line far too long for any sensible window, and one that
    /// fits, so a test can tell "it wrapped" from "it wrapped everything".
    const LONG: &str = "\
diff --git a/a.rs b/a.rs
@@ -1,2 +1,2 @@
 fn main() {
-    let x = one(alpha) + two(beta) + three(gamma) + four(delta) + five(epsilon);
+    let x = one(alpha) + two(beta) + three(gamma) + four(delta) + six(epsilon);
";

    /// A width in pixels that leaves room for `cols` characters of text in the
    /// unified presentation, in the default face.
    ///
    /// Half a character of slack, deliberately: landing exactly on a boundary
    /// makes the `floor` in [`columns`] a coin toss on the last bit of an `f32`,
    /// and a test that wants 40 columns should ask for 40 columns rather than for
    /// whichever side of 40 the arithmetic came down on.
    fn width_for(cols: usize, host: &Host) -> f32 {
        TEXT_CHROME + (cols as f32 + 0.5) * host.font.size * host.font.advance
    }

    /// A selection between two visual rows of `diff`, at the given byte offsets.
    /// Every row of these fixtures is one visual row, which is what lets a test
    /// name them by index.
    fn select(diff: &Diff, from: (usize, usize), to: (usize, usize)) -> Selection {
        let at = |v: usize| Caret {
            row: diff.order[v].logical(),
            off: 0,
            at: v..v + 1,
        };
        let mut sel = Selection::new(
            0,
            Caret {
                off: from.1,
                ..at(from.0)
            },
        );
        sel.extend(Caret {
            off: to.1,
            ..at(to.0)
        });
        sel
    }

    fn text_rows(src: &str) -> (TextRows, Rc<Host>) {
        let host = Rc::new(Host::new());
        let mut p = prepare(&parse_unified_diff(src), &host.syntax, 2000);
        let mut r = TextRows::default();
        r.build(p.files.remove(0));
        (r, host)
    }

    #[test]
    fn a_wrapped_line_is_several_rows_and_still_one_line() {
        // The whole shape of the feature: `len` counts lines and does not move,
        // `rows` counts what is drawn and does.
        let (mut r, host) = text_rows(LONG);
        let before = r.len();
        assert!(
            (0..r.len()).all(|i| r.rows(i) == 1),
            "nothing wraps before a reflow"
        );

        assert!(r.reflow(width_for(40, &host), &host, host.wrap.current()));
        assert_eq!(r.len(), before, "wrapping changed the line count");
        let rows: Vec<usize> = (0..r.len()).map(|i| r.rows(i)).collect();
        assert_eq!(
            rows,
            [1, 1, 1, 3, 3],
            "headers, a short line, two long ones"
        );
    }

    #[test]
    fn no_row_is_wider_than_the_window_once_it_has_wrapped() {
        // What replaces the horizontal scroll. `width` is what `overflow` is
        // measured from, so this is also the assertion that nothing is left to
        // scroll to.
        let (mut r, host) = text_rows(LONG);
        for cols in [12, 20, 40, 77] {
            r.reflow(width_for(cols, &host), &host, host.wrap.current());
            for i in 0..r.len() {
                // Headers are exempt and stay one row each: a path is not prose,
                // and the `+N -N` after it is not part of the string, so there is
                // nothing to slice. A long path overflows, which is a scrollbar
                // for one row in a file rather than for every line in the diff.
                if !matches!(r.rows[i], Row::Line { .. }) {
                    assert_eq!(r.rows(i), 1);
                    continue;
                }
                for seg in 0..r.rows(i) {
                    assert!(r.width(i, seg) <= cols, "{cols}: row {i}/{seg}");
                }
            }
        }
    }

    #[test]
    fn a_resize_that_crosses_no_character_boundary_costs_nothing() {
        // The reason `reflow` returns a bool: this runs on every frame of a drag.
        let (mut r, host) = text_rows(LONG);
        let w = width_for(40, &host);
        assert!(r.reflow(w, &host, host.wrap.current()));
        assert!(
            !r.reflow(w + 1.0, &host, host.wrap.current()),
            "one pixel rebuilt the table"
        );
        assert!(
            r.reflow(w + 40.0, &host, host.wrap.current()),
            "five characters did not"
        );
    }

    #[test]
    fn turning_it_off_collapses_the_rows_again() {
        let (mut r, host) = text_rows(LONG);
        let narrow = width_for(20, &host);
        r.reflow(narrow, &host, host.wrap.current());
        assert!(r.rows(3) > 1);

        let off = host.wrap.at(host.wrap.position("off").unwrap());
        assert!(r.reflow(narrow, &host, off));
        assert!(
            (0..r.len()).all(|i| r.rows(i) == 1),
            "off still broke something"
        );
        // And the widest row is the whole line again, which is what the
        // horizontal scrollbar is for.
        assert!(r.width(3, 0) > 20);
    }

    // ------------------------------------------------------ scrolling sideways

    #[test]
    fn a_horizontal_scroll_moves_the_text_and_not_the_gutter() {
        // The terminal's test of the same name, in pixels. What proves the
        // furniture stays put is `hit`: the caret arithmetic is the only place
        // the offset and the chrome meet, so if a click at the left edge of the
        // text lands on the character `shift` pixels in, the numbers in front of
        // it did not move.
        let (r, host) = text_rows(SAMPLE);
        let cw = host.font.char_width();
        for col in [0, 4, 13] {
            let plain = r
                .hit(3, 0, x_for(0, &host), &host, col as f32 * cw)
                .unwrap();
            assert_eq!(
                (plain.part, plain.off),
                (0, col),
                "scrolled {col} characters"
            );
        }
        // A click on a line number, scrolled: the first character there is to
        // see, and never a negative offset into the line.
        assert_eq!(r.hit(3, 0, 0.0, &host, 4.0 * cw).unwrap().off, 4);
        assert_eq!(r.hit(3, 0, 0.0, &host, 0.0).unwrap().off, 0);
    }

    #[test]
    fn there_is_nothing_to_scroll_to_once_it_has_wrapped() {
        // Two claims: an unwrapped line is over the edge by exactly the part of
        // it that does not fit, and a wrapped one is not over the edge at all.
        let (mut r, host) = text_rows(LONG);
        let w = width_for(40, &host);
        let off = host.wrap.at(host.wrap.position("off").unwrap());
        r.reflow(w, &host, off);
        let over = r.overflow(3, 0, w, &host);
        let text = r.width(3, 0) as f32 * host.font.char_width();
        assert!((over - (text - (w - TEXT_CHROME))).abs() < 0.001, "{over}");
        assert!(over > 0.0, "a 76-character line fits 40 columns");

        r.reflow(w, &host, host.wrap.current());
        for seg in 0..r.rows(3) {
            assert_eq!(r.overflow(3, seg, w, &host), 0.0, "row 3/{seg}");
        }
    }

    #[test]
    fn a_gesture_keeps_the_axis_it_started_on() {
        // The bug this exists to stop: a flick to the right that also moved the
        // rows down, because the wheel handler and the list each decided the
        // axis for themselves, one event at a time. One lock, one decision.
        use gpui::{point, px, OngoingScroll, TouchPhase};
        let mut lock = OngoingScroll::default();
        let moved = TouchPhase::Moved;

        // Sideways, and nothing on the vertical axis for the list to have.
        let d = locked(
            point(px(-30.), px(0.)),
            false,
            &mut lock,
            TouchPhase::Started,
        );
        assert_eq!((d.x, d.y), (px(-30.), px(0.)));
        // The rest of the same gesture is sideways too, however the fingers
        // wander: this is the drift that read as the text sliding at an angle.
        let d = locked(point(px(-12.), px(4.)), false, &mut lock, moved);
        assert_eq!(d.y, px(0.), "a locked gesture leaked onto the other axis");
        assert!(d.x < px(0.));

        // A vertical gesture is the list's, and this hands back nothing for the
        // text to move by — not even the sideways wobble in it.
        let mut lock = OngoingScroll::default();
        let d = locked(
            point(px(3.), px(-40.)),
            false,
            &mut lock,
            TouchPhase::Started,
        );
        assert_eq!((d.x, d.y), (px(0.), px(-40.)));

        // `shift` is the platform's way of saying "this one is horizontal", and
        // it is applied before the lock — after it, the lock has already called
        // the gesture vertical and given it away.
        let mut lock = OngoingScroll::default();
        let d = locked(
            point(px(0.), px(-40.)),
            true,
            &mut lock,
            TouchPhase::Started,
        );
        assert_eq!((d.x, d.y), (px(-40.), px(0.)));
    }

    #[test]
    fn the_offset_is_clamped_to_what_there_is_to_scroll() {
        // Every reader of `Pan` — the rows, the hit test, the scrollbar thumb —
        // takes the value as given, so this is the one place it can be wrong.
        let pan = Pan::default();
        assert!(!pan.set(200.0), "scrolled a diff that fits");
        assert_eq!(pan.at(), 0.0);

        pan.set_max(100.0);
        assert!(pan.set(200.0));
        assert_eq!(pan.at(), 100.0, "scrolled past the widest row");
        assert!(pan.by(-1000.0));
        assert_eq!(pan.at(), 0.0, "scrolled left of column zero");

        pan.set(60.0);
        // Wrapping turned on, or the window dragged wider: the offset it was at
        // is no longer somewhere that exists.
        pan.set_max(0.0);
        assert_eq!(pan.at(), 0.0);
    }

    /// A `Diff` opened on a named wrap, which is how the host names one.
    fn diff_wrapped(src: &str, wrap: &str) -> (Diff, Rc<Host>) {
        let mut h = Host::new();
        assert!(h.wrap.select(wrap), "no wrap called {wrap}");
        let host = Rc::new(h);
        let diff = Diff::with_layouts(parse_unified_diff(src), &host, Layouts::builtin());
        (diff, host)
    }

    #[test]
    fn the_view_bounds_the_scroll_by_its_widest_row() {
        // End to end: the bound comes off the row `expand` picked, so this is
        // also the assertion that the two agree about which row that is.
        let (mut diff, host) = diff_wrapped(LONG, "off");
        let w = width_for(20, &host);
        diff.reflow(w, &host);
        let widest = diff.order[diff.widest];
        let chars = diff.renderers.borrow()[widest.owner as usize]
            .width(widest.index as usize, widest.seg as usize);
        let expected = chars as f32 * host.font.char_width() - (w - TEXT_CHROME);
        assert!(
            (diff.bound(w, &host) - expected).abs() < 0.001,
            "{}",
            diff.bound(w, &host)
        );
        // The offset the reflow left is inside it, whatever it was before.
        diff.pan.set(1e6);
        assert_eq!(diff.pan.at(), diff.bound(w, &host));

        // And a wrapped diff has nowhere left to go, which is what puts the text
        // back at column zero the moment `w` turns wrapping on.
        let (mut wrapped, host) = diff_wrapped(LONG, "word");
        wrapped.reflow(w, &host);
        assert_eq!(
            wrapped.bound(w, &host),
            0.0,
            "a wrapped row hangs over the edge"
        );
        assert_eq!(wrapped.pan.at(), 0.0);
    }

    #[test]
    fn a_registered_wrap_reaches_the_rows() {
        // The swap test. A policy that breaks every line into single characters
        // is absurd and unmistakable, which is the point.
        struct EveryChar;
        impl gitten_core::wrap::Wrap for EveryChar {
            fn name(&self) -> &'static str {
                "every-char"
            }
            fn breaks(&self, text: &str, _cols: usize, out: &mut Vec<gitten_core::wrap::Break>) {
                for (i, _) in text.char_indices().skip(1) {
                    out.push(gitten_core::wrap::Break::hard(i));
                }
            }
        }
        let mut host = Host::new();
        host.wrap.register(EveryChar);
        assert!(host.wrap.select("every-char"));
        let host = Rc::new(host);

        let mut p = prepare(&parse_unified_diff(LONG), &host.syntax, 2000);
        let mut r = TextRows::default();
        r.build(p.files.remove(0));
        r.reflow(width_for(40, &host), &host, host.wrap.current());
        assert_eq!(r.rows(2), "fn main() {".chars().count());
        assert!((0..r.len()).all(|i| r.width(i, 0) <= 1 || !matches!(r.rows[i], Row::Line { .. })));
    }

    #[test]
    fn the_order_table_grows_and_keeps_the_line_you_were_reading() {
        // What a resize does to the list: more rows, and the same *line* at the
        // top rather than the same row number.
        let host = Host::new();
        let mut diff = Diff::with_layouts(parse_unified_diff(LONG), &host, Layouts::builtin());
        let unwrapped = diff.total();

        diff.reflow(width_for(40, &host), &host);
        assert!(diff.total() > unwrapped, "the order table did not grow");
        // Every entry still addresses a row that exists, and the segments of one
        // line are consecutive and start at zero.
        let mut expect = 0;
        let mut last = None;
        for r in diff.order.iter() {
            if last != Some(r.logical()) {
                expect = 0;
                last = Some(r.logical());
            }
            assert_eq!(r.seg, expect, "segments out of order");
            expect += 1;
        }

        // Park the keyboard on the last line, reflow narrower, and it is still
        // the line under the **cursor** — at a different row number, because
        // rows above it grew.
        let last_line = diff.order.last().unwrap().logical();
        diff.go_to(diff.total() - 1, &host);
        diff.reflow(width_for(20, &host), &host);
        assert_eq!(diff.order[diff.cursor()].logical(), last_line);
        assert_eq!(diff.order[diff.cursor()].seg, 0, "not the top of its line");
    }

    #[test]
    fn a_row_that_wrapped_styles_its_own_slice_and_not_the_line() {
        // The failure this prevents: tokens are in *line* coordinates, so a
        // continuation row handed them unshifted would highlight bytes from the
        // start of the line rather than from the start of the row.
        let theme = Theme::dark();
        let text = "let alpha = 1; let beta = 2;";
        let tokens = [tok(0, 3, Kind::Keyword), tok(15, 18, Kind::Keyword)];
        let second = text.find("let beta").unwrap();

        let out = runs(
            second..text.len(),
            &tokens,
            &[],
            &theme,
            LineKind::Context,
            false,
        );
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(out[0].0, 0..3, "the second `let` is at 0 of the second row");

        // And the first row sees only the first token.
        let out = runs(0..second, &tokens, &[], &theme, LineKind::Context, false);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, 0..3);
    }

    #[test]
    fn a_span_straddling_a_break_is_clipped_to_each_row() {
        // A changed word the wrap cut in half has to light up on both rows, each
        // time only as far as that row goes.
        let theme = Theme::dark();
        let text = "aaaa bbbb cccc";
        let spans = [Span { start: 2, end: 12 }];
        let first = runs(0..5, &[], &spans, &theme, LineKind::Added, false);
        let second = runs(5..text.len(), &[], &spans, &theme, LineKind::Added, false);
        assert_eq!(first[0].0, 2..5);
        assert_eq!(second[0].0, 0..7);
        assert!(first[0].1.background_color.is_some());
        assert!(second[0].1.background_color.is_some());
    }

    #[test]
    fn a_presentation_that_ignores_wrapping_is_untouched_by_it() {
        // The trait defaults, as a promise to an extension: an implementation
        // written before wrapping existed compiles and behaves identically.
        let host = Host::new();
        let mut r = OneLiner::default();
        let mut p = prepare(&parse_unified_diff(TWO_FILES), &host.syntax, 2000);
        r.build(p.files.remove(1));
        assert_eq!(r.rows(0), 1);
        assert!(!r.reflow(100.0, &host, host.wrap.current()));
        assert_eq!(r.rows(0), 1, "a reflow it ignored changed its row count");
    }

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
        assert!((0..r.len()).all(|i| r.width(i, 0) > 0));
    }

    #[test]
    fn a_one_file_diff_leaves_its_file_header_out_of_the_order() {
        // The pane header above the view names the file, so the band inside it
        // would name it twice. The row is still built — indices do not move —
        // but the order table skips it, and the hunk header is the first row.
        let host = Rc::new(Host::new());
        let diff = Diff::with_renderers(
            parse_unified_diff(SAMPLE),
            host,
            vec![Box::new(TextRows::default())],
        );
        let renderers = diff.renderers.borrow();
        assert_eq!(
            renderers[0].len(),
            5,
            "file header, hunk header, three lines built"
        );
        assert_eq!(diff.order.len(), 4, "but the file header is not drawn");
        assert!(diff
            .order
            .iter()
            .all(|r| !renderers[0].is_file_header(r.index as usize)));
        let first = diff.order[0];
        assert_eq!(
            renderers[0].selectable(first.index as usize, 0),
            Some("@@ -1,2 +1,2 @@"),
            "the body opens on the hunk header"
        );
    }

    #[test]
    fn a_one_file_diff_with_no_hunks_keeps_its_only_row() {
        // Nothing opens below the band, so dropping it would draw an empty pane
        // for a file that did change — a mode flip, a binary. The header stays
        // and `file_summary` still has a row to read the name from.
        let host = Rc::new(Host::new());
        let diff = Diff::with_renderers(
            vec![FileDiff {
                path: "bin.dat".into(),
                hunks: Vec::new(),
            }],
            host,
            vec![Box::new(TextRows::default())],
        );
        assert_eq!(diff.order.len(), 1);
        assert_eq!(
            diff.file_summary().map(|s| s.path),
            Some("bin.dat".to_string())
        );
    }

    #[test]
    fn a_two_file_diff_keeps_both_file_headers() {
        // With more than one file the band is the separator between them.
        let host = Rc::new(Host::new());
        let diff = Diff::with_renderers(
            parse_unified_diff(TWO_FILES),
            host,
            vec![Box::new(TextRows::default())],
        );
        let renderers = diff.renderers.borrow();
        let headers = diff
            .order
            .iter()
            .filter(|r| renderers[0].is_file_header(r.index as usize))
            .count();
        assert_eq!(headers, 2);
        assert_eq!(diff.order.len(), renderers[0].len());
    }

    #[test]
    fn a_hunk_header_is_hit_where_the_code_is() {
        // The `@@` is drawn at the code column, so a click measured from the page
        // padding would land two gutters early. `x_for` is the code column.
        let (r, host) = text_rows(SAMPLE);
        let header = r.selectable(1, 0).expect("the hunk header");
        assert!(header.starts_with("@@"));
        for col in [0, 3, 8] {
            let hit = r.hit(1, 0, x_for(col, &host), &host, 0.0).expect("a hit");
            assert_eq!((hit.part, hit.off), (0, col), "column {col}");
        }
        // The file header still measures from the padding.
        let hit = r
            .hit(0, 0, PAD + 0.1 * host.font.char_width(), &host, 0.0)
            .unwrap();
        assert_eq!(hit.off, 0);
        assert_eq!(super::HUNK_INDENT, TEXT_CHROME - PAD);
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
        fn width(&self, index: usize, _seg: usize) -> usize {
            self.rows[index].len()
        }
        fn render(
            &self,
            index: usize,
            _seg: usize,
            _host: &Host,
            _sel: Option<Selected>,
            _state: RowState,
            _shift: f32,
        ) -> AnyElement {
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
    fn the_shipped_registry_has_both_presentations() {
        let l = Layouts::builtin();
        assert_eq!(l.names(), vec!["unified", "split"]);
        assert_eq!(l.position("split"), Some(1));
        assert_eq!(l.position("sidebyside"), None);
    }

    #[test]
    fn the_host_chooses_which_presentation_opens() {
        let mut host = Host::new();
        host.layout = "split".into();
        let diff = Diff::with_layouts(parse_unified_diff(TWO_FILES), &host, Layouts::builtin());
        assert_eq!(diff.layout(), "split");
        // The two-column layout collapses a replace pair onto one row, so it has
        // strictly fewer rows than unified for the same diff.
        let unified = Diff::with_layouts(
            parse_unified_diff(TWO_FILES),
            &Host::new(),
            Layouts::builtin(),
        );
        assert_eq!(unified.layout(), "unified");
        assert!(
            diff.total() < unified.total(),
            "{} vs {}",
            diff.total(),
            unified.total()
        );
    }

    #[test]
    fn an_unknown_layout_name_opens_the_first_rather_than_nothing() {
        let mut host = Host::new();
        host.layout = "sidebyside".into();
        let diff = Diff::with_layouts(parse_unified_diff(SAMPLE), &host, Layouts::builtin());
        assert_eq!(diff.layout(), "unified");
        assert!(
            diff.total() > 0,
            "a typo in a live-reloaded file must not blank the diff"
        );
    }

    #[test]
    fn cycling_returns_to_where_it_started() {
        let host = Host::new();
        let mut diff = Diff::with_layouts(parse_unified_diff(TWO_FILES), &host, Layouts::builtin());
        let (name, total) = (diff.layout(), diff.total());
        diff.apply_layout(1, &host);
        assert_eq!(diff.layout(), "split");
        assert_ne!(diff.total(), total);
        diff.apply_layout(0, &host);
        assert_eq!(diff.layout(), name);
        assert_eq!(
            diff.total(),
            total,
            "a round trip must rebuild the same rows"
        );
    }

    #[test]
    fn swapping_the_diff_keeps_the_presentation() {
        // What changing the algorithm does: new `FileDiff`s underneath, same
        // layout on top. A swap that reset to unified would make the control
        // disagree with the screen.
        let host = Host::new();
        let mut diff = Diff::with_layouts(parse_unified_diff(SAMPLE), &host, Layouts::builtin());
        diff.apply_layout(1, &host);
        assert_eq!(diff.layout(), "split");
        let mut view = diff.view.get();
        view.set_len(diff.total());
        view.go_to(diff.total().saturating_sub(1));
        diff.view.set(view);
        diff.sel = Some(select(&diff, (0, 0), (1, 0)));
        diff.dragging = true;
        diff.pan.set_max(100.0);
        diff.pan.set(50.0);

        diff.swap(parse_unified_diff(TWO_FILES), &host);
        assert_eq!(diff.layout(), "split", "the swap reset the presentation");
        assert!(diff.load.contains("2 files"), "{}", diff.load);
        assert!(diff.load.contains("split"), "{}", diff.load);
        assert!(
            diff.cursor() < diff.total(),
            "the old cursor was not clamped"
        );
        assert!(diff.sel.is_none(), "old rows kept a stale selection");
        assert!(!diff.dragging, "replacement kept a stale mouse drag");

        // And an empty diff is a swap too — a revspec whose changes vanished.
        diff.swap(Vec::new(), &host);
        assert_eq!(diff.total(), 0);
        assert_eq!(diff.layout(), "split");
        assert_eq!((diff.cursor(), diff.view.get().top()), (0, 0));
        assert_eq!(diff.pan.at(), 0.0);
        assert!(diff.scroll.0.borrow().deferred_scroll_to_item.is_none());
    }

    #[test]
    fn a_font_change_rebuilds_the_presentation() {
        // Three claims. Repeated `reflow`s under the same font cost nothing,
        // by width or by fingerprint; a font edit rebuilds through
        // `apply_layout`, because the metrics the renderers were built with —
        // the Markdown heading scale, notably — no longer describe what will be
        // drawn while the width itself has not moved; and the rebuild rides the
        // prepared cache rather than re-running either expensive pass.
        //
        // The presentation is counted at `Layout::build`, which only `arrange`
        // invokes: a rebuild and nothing else bumps it. A layout registers its
        // builders with the live host, so the count is the whole observable.
        let builds = Rc::new(Cell::new(0));
        let counted = builds.clone();
        let mut layouts = Layouts::builtin();
        layouts.register("counting", move |_| {
            counted.set(counted.get() + 1);
            vec![Box::new(TextRows::default())]
        });
        let mut host = Host::new();
        host.layout = "counting".into();
        let mut diff = Diff::with_layouts(parse_unified_diff(SAMPLE), &host, layouts);
        let prepared = diff.prepared.clone();

        // Construction built once and seeded the fingerprint against this very
        // host, so the first `reflow` finds nothing to rebuild.
        let w = width_for(40, &host);
        diff.reflow(w, &host);
        let settled = builds.get();
        assert_eq!(settled, 1);

        // Same font again and again: nothing to do.
        diff.reflow(w, &host);
        diff.reflow(w, &host);
        assert_eq!(
            builds.get(),
            settled,
            "an unchanged frame rebuilt the renderers"
        );

        // A bigger face: every glyph moves, the width does not.
        let mut bigger = Host::new();
        bigger.layout = "counting".into();
        bigger.font.size = 18.0;
        diff.reflow(w, &bigger);
        assert!(
            builds.get() > settled,
            "a font change left the presentation stale"
        );
        assert!(
            Rc::ptr_eq(&prepared, &diff.prepared),
            "a font change re-prepared the diff"
        );

        // And it settles again: frames under the new font are free once more.
        let rebuilt = builds.get();
        diff.reflow(w, &bigger);
        diff.reflow(w, &bigger);
        assert_eq!(
            builds.get(),
            rebuilt,
            "the rebuild fired again on an unchanged frame"
        );
    }

    #[test]
    fn a_font_edit_keeps_the_selection_and_the_row() {
        // A font edit is not a presentation change: same rows, same row count,
        // only the glyph metrics moved. Unlike `apply_layout`'s other callers,
        // the selection and the cursor's logical row both still mean something
        // afterwards, and both should survive.
        let host = Host::new();
        let mut diff = Diff::with_layouts(parse_unified_diff(TWO_FILES), &host, Layouts::builtin());
        diff.reflow(width_for(40, &host), &host);

        diff.sel = Some(select(&diff, (1, 0), (3, 9)));
        let text = diff.selection();
        assert!(!text.is_empty());

        // A known row, past the start, so a fall-back to 0 would not
        // accidentally pass.
        let mut v = diff.view.get();
        v.go_to(3);
        diff.view.set(v);
        let logical_before = diff.order[diff.view.get().cursor()].logical();

        let mut bigger = Host::new();
        bigger.font.size = 18.0;
        diff.reflow(width_for(40, &host), &bigger);

        assert!(diff.sel.is_some(), "a font edit threw the selection away");
        assert_eq!(
            diff.selection(),
            text,
            "the same bytes, at the same width, under a new font"
        );
        // The visual index may legitimately have moved if the new font
        // changed the column budget; the row it names must not have.
        let logical_after = diff.order[diff.view.get().cursor()].logical();
        assert_eq!(
            logical_after, logical_before,
            "the cursor landed on a different logical row"
        );

        // The other half of the rule still holds: an actual presentation
        // change still drops the selection.
        diff.apply_layout(1, &host);
        assert!(
            diff.sel.is_none(),
            "a layout change kept a selection anchored to somebody else's rows"
        );
    }

    #[test]
    fn the_font_fingerprint_compares_by_value() {
        // What the reflow guard leans on: `Font` derives `PartialEq`, so a
        // value comparison is the whole fingerprint. Each field individually has
        // to move the outcome, or a config edit that moved only that field
        // would leave stale metrics behind it.
        assert_eq!(Some(Font::jetbrains_mono()), Some(Font::jetbrains_mono()));
        let resized = Font {
            size: 15.0,
            ..Font::menlo()
        };
        assert_ne!(resized, Font::menlo());
        let retuned = Font {
            advance: 0.5,
            ..Font::menlo()
        };
        assert_ne!(retuned, Font::menlo());
    }

    #[test]
    fn cycling_keeps_you_at_the_same_point_in_the_diff() {
        // Exactly is impossible — the two presentations do not have the same
        // number of rows — so the proportion is what is preserved. The cursor,
        // which is where a proportion comes from now.
        let host = Host::new();
        let mut diff = Diff::with_layouts(long_diff(), &host, Layouts::builtin());
        let total = diff.total();
        let mut v = diff.view.get();
        v.set_len(total);
        v.go_to(total / 2);
        diff.view.set(v);
        diff.apply_layout(1, &host);
        let landed = diff.top.get() as f32 / diff.total() as f32;
        assert!(
            (landed - 0.5).abs() < 0.05,
            "landed {landed} of the way through"
        );
    }

    #[test]
    fn a_registered_presentation_is_cycled_to_like_a_built_in() {
        // Rule 1, as a test: a third presentation needs no edit to the two
        // shipped ones, and `[diff] layout` reaches it.
        let mut layouts = Layouts::builtin();
        layouts.register("one-liner", |_| {
            vec![Box::new(OneLinerEverything::default())]
        });
        assert_eq!(layouts.names(), vec!["unified", "split", "one-liner"]);

        let mut host = Host::new();
        host.layout = "one-liner".into();
        let diff = Diff::with_layouts(parse_unified_diff(TWO_FILES), &host, layouts);
        assert_eq!(diff.layout(), "one-liner");
        assert_eq!(diff.total(), 2, "one row per file and nothing else");
    }

    #[test]
    fn registering_a_name_twice_replaces_the_presentation() {
        let mut layouts = Layouts::builtin();
        layouts.register("unified", |_| vec![Box::new(OneLinerEverything::default())]);
        assert_eq!(
            layouts.names(),
            vec!["unified", "split"],
            "a replacement must not append"
        );
        let diff = Diff::with_layouts(parse_unified_diff(TWO_FILES), &Host::new(), layouts);
        assert_eq!(diff.total(), 2);
    }

    #[test]
    fn a_pinned_set_of_renderers_has_nothing_to_cycle_to() {
        let diff = Diff::with_renderers(
            parse_unified_diff(SAMPLE),
            Rc::new(Host::new()),
            vec![Box::new(TextRows::default())],
        );
        assert_eq!(diff.layouts.len(), 1);
        assert_eq!(diff.layout(), "custom");
    }

    /// A presentation that claims everything, for the registry tests: one row
    /// per file, so a row count identifies which one ran.
    #[derive(Default)]
    struct OneLinerEverything {
        rows: Vec<String>,
    }

    impl Rows for OneLinerEverything {
        fn claims(&self, _: &str) -> bool {
            true
        }
        fn len(&self) -> usize {
            self.rows.len()
        }
        fn build(&mut self, file: PreparedFile) {
            self.rows.push(file.path);
        }
        fn width(&self, index: usize, _seg: usize) -> usize {
            self.rows[index].len()
        }
        fn render(
            &self,
            index: usize,
            _seg: usize,
            _host: &Host,
            _sel: Option<Selected>,
            _state: RowState,
            _shift: f32,
        ) -> AnyElement {
            div().child(self.rows[index].clone()).into_any_element()
        }
    }

    /// Enough rows that a proportional scroll position means something.
    fn long_diff() -> Vec<gitten_core::FileDiff> {
        let mut raw = String::from("diff --git a/big.rs b/big.rs\n@@ -1,200 +1,200 @@\n");
        for i in 0..200 {
            if i % 5 == 0 {
                raw.push_str(&format!(
                    "-    let x{i} = {i};\n+    let x{i} = {};\n",
                    i + 1
                ));
            } else {
                raw.push_str(&format!("     let y{i} = {i};\n"));
            }
        }
        parse_unified_diff(&raw)
    }

    #[test]
    fn every_layout_survives_the_shapes_that_break_things() {
        // Not a rendering test — nothing here opens a window. It is the load
        // path over the inputs that have broken it before: a line far past the
        // clip budget, multi-byte text that measures three times its width in
        // bytes, a markdown file that one layout has a specialist for and the
        // other does not, a file with no extension, and a hunk that is pure
        // addition so one column is empty for all of it.
        let long = "x".repeat(9000);
        let raw = format!(
            "diff --git a/min.js b/min.js\n@@ -1,1 +1,1 @@\n-var a={long};\n+var b={long};\n\
             diff --git a/wide.txt b/wide.txt\n@@ -1,2 +1,2 @@\n-箱の中身は「猫」\n+箱の中身は「犬」\n \
             😀 unchanged\n\
             diff --git a/doc.md b/doc.md\n@@ -1,3 +1,4 @@\n # Heading\n-| a | b |\n+| a | bb |\n+| c | d |\n\
             diff --git a/Makefile b/Makefile\n@@ -1,0 +1,2 @@\n+all:\n+\tcargo build\n"
        );
        let files = parse_unified_diff(&raw);
        assert_eq!(files.len(), 4);
        let host = Host::new();
        for name in Layouts::builtin().names() {
            let mut h = Host::new();
            h.layout = name.into();
            let diff = Diff::with_layouts(files.clone(), &h, Layouts::builtin());
            assert_eq!(diff.layout(), name);
            assert!(diff.total() > 0, "{name} produced no rows");
            // Every row answers a width, and the widest index is one of them.
            for r in diff.order.iter() {
                let _ = diff.renderers.borrow()[r.owner as usize]
                    .width(r.index as usize, r.seg as usize);
            }
            assert!(
                diff.widest < diff.total(),
                "{name}: widest {} of {}",
                diff.widest,
                diff.total()
            );
        }
        // And cycling between them, both ways, over the same input.
        let mut diff = Diff::with_layouts(files, &host, Layouts::builtin());
        for i in [1, 0, 1, 0] {
            diff.apply_layout(i, &host);
            assert!(diff.total() > 0);
        }
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

    // ------------------------------------------------------------ navigation

    /// A viewport of `n` visible rows, as the list would report after a frame.
    fn with_height(diff: &mut Diff, n: usize) {
        diff.rendered.set(n);
        let mut v = diff.view.get();
        v.set_len(diff.order.len());
        v.set_height(n);
        diff.view.set(v);
    }

    #[test]
    fn navigation_moves_the_cursor_and_the_view_follows_with_a_margin() {
        let host = Host::new();
        let mut d = Diff::with_layouts(long_diff(), &host, Layouts::builtin());
        with_height(&mut d, 20);
        assert!(d.run_view("view.down", &host));
        assert!(d.run_view("view.down", &host));
        assert!(d.run_view("view.down", &host));
        assert_eq!(d.cursor(), 3);
        assert_eq!(d.top.get(), 0, "inside the margin, nothing scrolled");
        // Past the margin the view moves one row, not a screenful.
        for _ in 0..20 {
            assert!(d.run_view("view.down", &host));
        }
        assert_eq!(d.cursor(), 23);
        assert!(d.top.get() > 0, "the margin was breached");
    }

    #[test]
    fn pages_move_a_screenful_and_top_bottom_reach_both_ends() {
        let host = Host::new();
        let mut d = Diff::with_layouts(long_diff(), &host, Layouts::builtin());
        let total = d.total();
        with_height(&mut d, 20);
        assert!(d.run_view("view.page-down", &host));
        assert_eq!(d.cursor(), 19, "a screenful less one row");
        assert!(d.run_view("view.page-up", &host));
        assert_eq!(d.cursor(), 0);
        assert!(d.run_view("view.bottom", &host));
        assert_eq!(d.cursor(), total - 1, "clamped to the last row");
        assert!(d.run_view("view.top", &host));
        assert_eq!((d.cursor(), d.view.get().top()), (0, 0));
    }

    #[test]
    fn navigation_clamps_instead_of_leaving_the_diff() {
        let host = Host::new();
        let mut d = Diff::with_layouts(long_diff(), &host, Layouts::builtin());
        let total = d.total();
        with_height(&mut d, 20);
        for _ in 0..5 {
            assert!(d.run_view("view.up", &host));
        }
        assert_eq!(d.cursor(), 0, "clamped at the first row");
        assert!(total > 10);
        assert!(d.run_view("view.scroll-down", &host));
        // A scroll is not a cursor move: the viewport pans a row while the
        // keyboard stays where it was, like the terminal.
        assert_eq!(d.view.get().top(), 1);
        assert_eq!(d.cursor(), 0);
    }

    #[test]
    fn file_jumps_land_on_headers_in_both_directions() {
        let host = Host::new();
        let mut d = Diff::with_layouts(parse_unified_diff(TWO_FILES), &host, Layouts::builtin());
        assert_eq!(d.headers.len(), 2, "two files, two headers");
        assert!(d.headers.windows(2).all(|w| w[0] < w[1]));
        assert!(d.run_view("diff.next-file", &host));
        assert_eq!(d.cursor(), d.headers[1], "skipped to the second header");
        assert!(d.run_view("diff.next-file", &host));
        assert_eq!(d.cursor(), d.headers[1], "no third file: it stayed");
        assert!(d.run_view("diff.prev-file", &host));
        assert_eq!(d.cursor(), d.headers[0]);
        assert!(d.run_view("diff.prev-file", &host));
        assert_eq!(d.cursor(), d.headers[0], "no row above the first header");
    }

    #[test]
    fn cycling_layout_and_wrap_are_commands_too() {
        let host = Host::new();
        let mut d = Diff::with_layouts(long_diff(), &host, Layouts::builtin());
        assert!(d.run_view("diff.cycle-layout", &host));
        assert_eq!(d.layout_index(), 1);
        assert_eq!(d.layout(), "split");
        assert!(d.run_view("diff.cycle-layout", &host));
        assert_eq!(d.layout_index(), 0, "wrapped round");
        assert!(d.run_view("diff.cycle-wrap", &host));
        assert_ne!(d.wrap_index(), host.wrap.selected_index(), "moved on");
    }

    #[test]
    fn a_command_no_screen_owns_is_reported_not_swallowed() {
        let mut host = Host::new();
        let mut d = Diff::with_layouts(long_diff(), &host, Layouts::builtin());
        with_height(&mut d, 20);
        // Registered by an extension somewhere else; nothing here answers it.
        host.commands.register("blame.toggle", "show blame");
        assert!(!d.run_view("blame.toggle", &host));
    }

    #[test]
    fn consecutive_commands_compose_rather_than_resetting_each_other() {
        // The regression this guards against: reconcile reading the scroll
        // handle's position and deciding every previous command had not
        // happened. In a headless view the handle never paints, so its offset
        // stays where `show` left it.
        let host = Host::new();
        let mut d = Diff::with_layouts(long_diff(), &host, Layouts::builtin());
        with_height(&mut d, 20);
        for _ in 0..4 {
            d.run_view("view.down", &host);
        }
        assert_eq!(d.cursor(), 4);
        d.run_view("view.page-down", &host);
        assert_eq!(d.cursor(), 23, "four rows plus a nineteen-row page");
        d.run_view("view.down", &host);
        assert_eq!(d.cursor(), 24, "and one more, not back to zero plus one");
    }

    #[test]
    fn a_thumb_drag_is_reconciled_before_anything_reads_the_cursor() {
        // A scrollbar drag writes the offset and nothing else, so the model
        // has to meet the visible top — while the cursor, the keyboard's
        // row, stays exactly where the keys left it, like the terminal.
        let host = Host::new();
        let mut d = Diff::with_layouts(long_diff(), &host, Layouts::builtin());
        with_height(&mut d, 20);
        d.run_view("view.top", &host);
        // Ten rows of drag, straight into the handle like a paint pass writes
        // it: offset −220 px at 22 px a row.
        d.scroll
            .0
            .borrow()
            .base_handle
            .set_offset(gpui::point(gpui::px(0.), gpui::px(-220.)));
        assert_eq!(d.cursor(), 0, "the stale cursor is what the fix is for");

        d.reconcile(&host);
        let v = d.view.get();
        assert_eq!(v.top(), 10, "the model caught up with the visible top");
        // The drag panned: the selection never left row zero.
        assert_eq!(v.cursor(), 0, "a drag pans; it does not select");

        // Idempotent: meeting the list twice is not moving it twice.
        d.reconcile(&host);
        assert_eq!((d.view.get().top(), d.cursor()), (10, 0));
    }

    #[test]
    fn a_click_on_a_wrapped_continuation_moves_the_cursor_there() {
        // Not to the top of the wrapped line: a click is a place, and the bar
        // belongs under the mouse.
        let host = Host::new();
        let mut d = Diff::with_layouts(parse_unified_diff(LONG), &host, Layouts::builtin());
        d.reflow(width_for(30, &host), &host);
        let continuation = d
            .order
            .iter()
            .position(|r| r.seg > 0)
            .expect("a wrapped line produced a continuation row");
        with_height(&mut d, 20);

        d.click_row(continuation, &host);
        assert_eq!(
            d.cursor(),
            continuation,
            "the keyboard is on the clicked visual row"
        );
        // The model and the list agree about where the viewport now sits.
        let v = d.view.get();
        assert!(
            v.top() <= d.cursor() && d.cursor() < v.top() + 20,
            "the clicked row is visible"
        );
        assert_eq!(
            d.synced.get(),
            f32::from(d.scroll.0.borrow().base_handle.offset().y),
            "the handle and the sync mark disagree"
        );
    }

    #[test]
    fn a_first_click_lands_where_it_pointed_before_any_navigation() {
        // The regression: `click_row` read the *stored* viewport, and a view
        // nothing has navigated yet believes it is empty — so `go_to` clamped
        // every click onto row zero, deep rows included.
        let host = Host::new();
        let mut d = Diff::with_layouts(parse_unified_diff(LONG), &host, Layouts::builtin());
        assert_eq!(d.view.get().len(), 0, "the test means to start fresh");
        assert!(d.total() > 0, "but the rows themselves exist");

        // No key has moved, no frame has reported a height — and the click is
        // on the last row there is.
        let last = d.order.len() - 1;
        d.click_row(last, &host);
        assert_eq!(d.cursor(), last, "row zero is not where I clicked");

        // A wrapped continuation, with the stored model again empty — the
        // state it is in until the first frame fills it.
        d.reflow(width_for(30, &host), &host);
        let continuation = d
            .order
            .iter()
            .position(|r| r.seg > 0)
            .expect("a wrapped continuation to click on");
        d.view.set(gitten_core::view::Viewport::new());
        d.click_row(continuation, &host);
        assert_eq!(d.cursor(), continuation);
        // And the list was written back to meet the model, so they agree.
        assert_eq!(
            d.synced.get(),
            f32::from(d.scroll.0.borrow().base_handle.offset().y),
            "the handle and the sync mark disagree"
        );
    }

    #[test]
    fn a_reflow_keeps_the_line_under_the_cursor_not_the_one_at_the_top() {
        // The regression: anchoring the reflow to the top row moved the
        // *cursor* to wherever that line landed, silently relocating the thing
        // everything above — open-diff, copy — acts on. The cursor's own line
        // is what survives, wherever it sat in the viewport.
        let host = Host::new();
        let mut d = Diff::with_layouts(long_diff(), &host, Layouts::builtin());
        d.reflow(width_for(80, &host), &host);
        with_height(&mut d, 20);
        // Read well past the first screen, then leave the top behind: the
        // cursor must be neither at nor near the top row for this to bite.
        for _ in 0..3 {
            d.run_view("view.page-down", &host);
        }
        d.run_view("view.up", &host);
        let cursor_line = d.order[d.cursor()].logical();
        let top_line = d.order[d.top.get()].logical();
        assert_ne!(
            cursor_line, top_line,
            "the test wants the cursor away from the top"
        );

        d.reflow(width_for(12, &host), &host);
        assert_eq!(
            d.order[d.cursor()].logical(),
            cursor_line,
            "the cursor kept its line"
        );
        assert_ne!(
            d.order[d.top.get()].logical(),
            top_line,
            "the old top row was not held still while the cursor moved"
        );
        let v = d.view.get();
        assert!(
            v.top() <= v.cursor() && v.cursor() < v.top() + 20,
            "and it stayed on screen through the reflow"
        );
    }

    #[test]
    fn a_cursor_preserving_reflow_waits_for_the_new_geometry() {
        // The other half of a reflow: the rows have been rebuilt but the list
        // has not laid them out yet, so its own bound still describes the
        // *wide* shape. `show` clamps against that stale maximum — and a deep
        // cursor needs more offset than the old shape ever had — then writes
        // the clamped lie into the sync mark. The position goes through GPUI's
        // deferred request instead, which the list consumes after measuring
        // what it now holds.
        let host = Host::new();
        let mut d = Diff::with_layouts(long_diff(), &host, Layouts::builtin());
        d.reflow(width_for(80, &host), &host);
        with_height(&mut d, 20);
        let wide_rows = d.total();

        // Read to the very end, so the cursor sits past anything the wide
        // shape's geometry could name.
        assert!(d.run_view("view.bottom", &host));
        let cursor_line = d.order[d.cursor()].logical();

        d.reflow(width_for(12, &host), &host);
        assert!(
            d.total() >= wide_rows * 3 / 2,
            "{} did not materially grow past {wide_rows}",
            d.total()
        );

        let v = d.view.get();
        assert_eq!(
            d.order[v.cursor()].logical(),
            cursor_line,
            "the reflow moved the keyboard"
        );
        let request = d
            .scroll
            .0
            .borrow()
            .deferred_scroll_to_item
            .expect("the position was not parked for layout");
        assert!(request.scroll_strict, "exact, not merely visible");
        assert_eq!(request.strategy, ScrollStrategy::Top);
        assert_eq!(request.item_index, v.top(), "on the cursor's viewport");
        // And the parked row is one the OLD geometry had no offset for — the
        // clamp this test exists to prevent would have pinned it there.
        let row_h = super::ROW_H;
        assert!(
            (v.top() as f32) * row_h > ((wide_rows - 20) as f32) * row_h,
            "top {} was inside the old bound; nothing would have been clamped",
            v.top()
        );

        // Nothing was claimed about pixels the list has not measured: the old
        // code wrote a clamped offset and its own sync mark over it.
        assert_eq!(
            f32::from(d.scroll.0.borrow().base_handle.offset().y),
            0.0,
            "an offset was written against the old shape"
        );
        assert_eq!(d.synced.get(), 0.0);

        // ...and when a command moves before layout, it replaces the deferred
        // target instead of clamping immediately against the old geometry.
        assert!(d.run_view("view.down", &host));
        let request = d
            .scroll
            .0
            .borrow()
            .deferred_scroll_to_item
            .expect("the updated target was not deferred");
        assert_eq!(request.item_index, d.view.get().top());
        assert!(request.scroll_strict);
        assert_eq!(f32::from(d.scroll.0.borrow().base_handle.offset().y), 0.0);

        let before = request.item_index;
        assert!(d.scroll_pixels(0.25, &host));
        let request = d
            .scroll
            .0
            .borrow()
            .deferred_scroll_to_item
            .expect("the wheel discarded the deferred target");
        assert_eq!(request.item_index, before, "the strict baseline moved");
        assert_eq!(d.pending_scroll.0.wheel.get(), 0.25);
        assert_eq!(f32::from(d.scroll.0.borrow().base_handle.offset().y), 0.0);
    }

    #[test]
    fn a_restored_row_inside_the_first_screen_still_moves_the_list() {
        // Session restore: a view constructed, a saved row handed to it,
        // nothing laid out. Row 5 of a tall window is inside the initial
        // viewport, which is precisely where the non-strict strategy declines
        // to scroll — the list would open at row zero while model, session and
        // title all claimed 5.
        let host = Host::new();
        let d = Diff::with_layouts(long_diff(), &host, Layouts::builtin());
        d.scroll_to(5, &host);

        let request = d
            .scroll
            .0
            .borrow()
            .deferred_scroll_to_item
            .expect("no request was parked");
        assert_eq!(request.item_index, 5);
        assert_eq!(request.strategy, ScrollStrategy::Top);
        assert!(request.scroll_strict, "visible-in-range is exactly the bug");
        assert_eq!(d.view.get().top(), 5, "and the model says so too");
    }

    #[test]
    fn a_restore_this_view_accepted_is_not_reconciled_as_a_drag() {
        // The acceptance itself is `views::tests`'; this is the wiring — that
        // the offset lands on *this* view's `synced`, so its `reconcile` reads
        // the list where the restore left it rather than as a thumb drag, which
        // would put the key below on the scrolloff margin.
        let host = Host::new();
        let mut d = Diff::with_layouts(long_diff(), &host, Layouts::builtin());
        d.scroll_to(40, &host);
        d.go_to(40, &host);
        {
            let mut state = d.scroll.0.borrow_mut();
            state.deferred_scroll_to_item = None;
            state
                .base_handle
                .set_offset(gpui::point(gpui::px(0.0), gpui::px(-40.0 * ROW_H)));
        }
        crate::views::accept_deferred_scroll(&d.scroll, &d.pending_scroll, &d.synced)
            .expect("prepaint's offset was not accepted");
        assert_eq!(d.synced.get(), -40.0 * ROW_H, "this view's own sync marker");

        d.rendered.set(20);
        assert!(d.run_view("view.down", &host));
        assert_eq!(d.cursor(), 41);
    }

    #[test]
    fn a_wheel_cancels_selection_autoscroll_instead_of_joining_its_request() {
        let host = Host::new();
        let mut d = Diff::with_layouts(long_diff(), &host, Layouts::builtin());
        with_height(&mut d, 20);

        // The headless handle's bounds end at zero, so this parks a non-strict
        // request below them exactly as a drag beyond the window edge does.
        d.autoscroll(gpui::point(gpui::px(0.0), gpui::px(44.0)));
        assert!(d.scroll.0.borrow().deferred_scroll_to_item.is_some());
        assert!(!d.pending_scroll.is_awaiting());

        // There is no headless scroll bound to move within, but the newer wheel
        // must still cancel the foreign request and leave no orphaned pixels.
        assert!(!d.scroll_pixels(-0.25, &host));
        assert!(d.scroll.0.borrow().deferred_scroll_to_item.is_none());
        assert_eq!(d.pending_scroll.0.wheel.get(), 0.0);
    }

    #[test]
    fn scrolling_by_pixels_is_smooth_and_drags_the_cursor_along() {
        // With a measured viewport of two rows, three rows of pixels push the
        // cursor off the bottom — where it lands is `Viewport`'s rule, shared
        // with the terminal's wheel.
        let host = Host::new();
        let mut d = Diff::with_layouts(long_diff(), &host, Layouts::builtin());
        with_height(&mut d, 2);
        // Headless, the handle has no bounds and reports nowhere to go.
        if d.scroll_pixels(-66.0, &host) {
            assert_eq!(d.view.get().top(), 3, "66 px at 22 px a row");
        }
    }

    #[test]
    fn a_fresh_viewport_restores_a_saved_row_without_preseeding() {
        // The startup path: a view constructed, a saved row handed to it,
        // nothing else. `go_to` once clamped against a list the model believed
        // was empty, so the restore landed on row zero; the viewport has to be
        // filled in before the position is.
        let mut host = Host::new();
        host.view.scrolloff = 5;
        let d = Diff::with_layouts(long_diff(), &host, Layouts::builtin());
        assert_eq!(d.view.get().len(), 0, "the test means to start fresh");
        assert_eq!(d.rendered.get(), 0);

        d.scroll_to(40, &host);
        d.go_to(40, &host);
        let v = d.view.get();
        assert_eq!(v.cursor(), 40, "the keyboard came back where it left off");
        assert_eq!(v.top(), 40);
        assert_eq!(v.len(), d.total(), "and against the real list length");

        // When the first height arrives, the view settles with the *file's*
        // margin above the cursor — not the built-in's three rows.
        let mut v = d.view.get();
        v.set_height(30);
        assert_eq!(
            (v.cursor(), v.top()),
            (40, 35),
            "scrolloff from the file, not the built-in"
        );
    }

    #[test]
    fn key_navigation_uses_the_live_scrolloff() {
        // `[view] scrolloff` reaches every command, not just construction: two
        // hosts identical but for the margin start scrolling at different rows.
        let build = |scrolloff: usize| -> (Diff, Rc<Host>) {
            let mut h = Host::new();
            h.view.scrolloff = scrolloff;
            let host = Rc::new(h);
            let mut d = Diff::with_layouts(long_diff(), &host, Layouts::builtin());
            with_height(&mut d, 20);
            (d, host)
        };
        let (mut tight, tight_host) = build(3);
        let (mut loose, loose_host) = build(8);
        for _ in 0..16 {
            tight.run_view("view.down", &tight_host);
            loose.run_view("view.down", &loose_host);
        }
        assert_eq!(tight.cursor(), loose.cursor());
        assert_eq!(tight.top.get(), 0, "a three-row margin holds at cursor 16");
        assert!(loose.top.get() > 0, "an eight-row margin scrolled already");
    }

    #[test]
    fn the_cursor_row_text_is_what_copy_falls_back_to() {
        let host = Host::new();
        let mut d = Diff::with_layouts(parse_unified_diff(TWO_FILES), &host, Layouts::builtin());
        with_height(&mut d, 10);
        d.run_view("diff.prev-file", &host); // already at the first header
                                             // The path as the diff parsed it: `b/` stripped, which is what the row
                                             // drew and therefore what a copy of it should hold.
        assert_eq!(d.cursor_text(), "a.rs");
    }

    // ------------------------------------------------------- hunk staging

    /// Two files, two hunks in the first and one in the second — enough
    /// addresses to prove the map reads the *keyboard's* hunk, not the first
    /// one it finds.
    const THREE_HUNKS: &str = "\
diff --git a/one.txt b/one.txt
--- a/one.txt
+++ b/one.txt
@@ -1,3 +1,3 @@
 alpha
-beta
+BETA
 gamma
@@ -10,2 +10,2 @@
 delta
-epsilon old
+epsilon new
diff --git a/two.txt b/two.txt
--- a/two.txt
+++ b/two.txt
@@ -5,2 +5,2 @@
 zeta
-eta old
+eta new
";

    /// The hunk under logical row `index`, through the same walk the view
    /// does — owner first, then the implementation's own answer.
    fn hunk_at_row(d: &Diff, index: usize) -> Option<String> {
        let r = *d.order.get(index)?;
        let renderers = d.renderers.borrow();
        let (path, n) = renderers.get(r.owner as usize)?.hunk_at(r.index as usize)?;
        Some(format!("{path}#{}", n))
    }

    #[test]
    fn every_row_of_a_hunk_answers_with_that_hunk() {
        let host = Host::new();
        let d = Diff::with_layouts(parse_unified_diff(THREE_HUNKS), &host, Layouts::builtin());

        // Unified layout rows: file header at 0; hunk 0 owns its header row
        // and its four lines (1..=5); hunk 1 its header and three lines
        // (6..=9); the second file starts at 10.
        assert_eq!(hunk_at_row(&d, 0), None, "a file header is no hunk");
        assert_eq!(hunk_at_row(&d, 1), Some("one.txt#0".into()));
        assert_eq!(hunk_at_row(&d, 3), Some("one.txt#0".into()), "mid-hunk");
        assert_eq!(hunk_at_row(&d, 5), Some("one.txt#0".into()));
        assert_eq!(hunk_at_row(&d, 6), Some("one.txt#1".into()));
        assert_eq!(hunk_at_row(&d, 8), Some("one.txt#1".into()));
        assert_eq!(
            hunk_at_row(&d, 12),
            Some("two.txt#0".into()),
            "the second file maps by its own path"
        );
    }

    #[test]
    fn every_hunk_leaves_a_tick_at_its_start() {
        let host = Host::new();
        let d = Diff::with_layouts(parse_unified_diff(THREE_HUNKS), &host, Layouts::builtin());

        // The same rows `hunk_at_row` names: file header at 0; hunk 0 at 1
        // (header and four lines); hunk 1 at 6 (header and three lines); the
        // second file's header 10, its hunk 11 (header and three lines) — 15
        // rows, and a tick where a `scroll_to_item` to each hunk's header
        // lands the thumb.
        assert_eq!(d.order.len(), 15);
        assert_eq!(d.marks.as_ref(), &[1.0 / 15.0, 6.0 / 15.0, 11.0 / 15.0]);
    }

    #[test]
    fn an_empty_diff_holds_no_marks() {
        let host = Host::new();
        let d = Diff::with_layouts(Vec::new(), &host, Layouts::builtin());
        assert!(d.marks.is_empty(), "nothing to divide, nothing to mark");
    }

    #[test]
    fn the_smallest_hunk_marks_the_top_of_the_track() {
        // One line replaced: in unified layout the removal and the addition
        // are two lines, so the hunk is a header and two lines, the order is
        // three rows — one file with hunks leaves its band out — and the tick
        // sits at row 0. The division the one-row guard protects is the empty
        // order's, tested above; a mark itself can only be a fraction of an
        // order that holds the hunk it came from, so three is the floor.
        let src = "\
diff --git a/one.txt b/one.txt
--- a/one.txt
+++ b/one.txt
@@ -1 +1 @@
-alpha
+beta
";
        let host = Host::new();
        let d = Diff::with_layouts(parse_unified_diff(src), &host, Layouts::builtin());
        assert_eq!(d.order.len(), 3);
        assert_eq!(d.marks.as_ref(), &[0.0]);
    }

    #[test]
    fn marks_follow_the_order_through_a_reflow_and_a_layout_swap() {
        // The marks are rebuilt exactly where the order is, so a width that
        // wraps keeps one tick per hunk at its new start — and a layout
        // change, which renumbers every row, does not leave the old fractions
        // behind. The count is read from the loaded diff itself, not from the
        // marks, so a walk that lost or doubled a hunk would fail here; the
        // reflow is asserted to have moved the rows at all, so a stale cache
        // would fail the changed-fractions compare.
        let host = Host::new();
        let mut d = Diff::with_layouts(parse_unified_diff(THREE_HUNKS), &host, Layouts::builtin());
        let hunks: usize = d.files.iter().map(|f| f.hunks.len()).sum();
        assert_eq!(hunks, 3, "the fixture has hunks to mark");
        assert_eq!(d.marks.len(), hunks);
        let before = d.marks.as_ref().to_vec();

        // Five columns wraps `epsilon old` and its friends, so hunk 1's and
        // hunk 2's starts move down and the fractions move with them.
        d.reflow(width_for(5, &host), &host);
        assert!(d.order.len() > 15, "the width wrapped something");
        assert_eq!(d.marks.len(), hunks, "a wrap renumbers, never re-marks");
        assert!(d.marks.iter().all(|m| (0.0..=1.0).contains(m)));
        assert!(d.marks.windows(2).all(|w| w[0] < w[1]), "ascending");
        assert_ne!(d.marks.as_ref(), before.as_slice());

        d.apply_layout(1, &host);
        assert_eq!(d.layout(), "split");
        assert_eq!(d.marks.len(), hunks);
        assert!(d.marks.iter().all(|m| (0.0..=1.0).contains(m)));
    }

    #[test]
    fn current_hunk_hands_over_the_loaded_diffs_own_hunk() {
        let host = Rc::new(Host::new());
        let mut d = Diff::with_layouts(parse_unified_diff(THREE_HUNKS), &host, Layouts::builtin());
        with_height(&mut d, 20);

        // Walk down to hunk 1's changed line — row 6 is its header, 7 is
        // `delta` — and ask what space would act on.
        for _ in 0..7 {
            d.run_view("view.down", &host);
        }
        let (path, hunk) = d.current_hunk().expect("on a hunk");
        assert_eq!(path, "one.txt");
        assert!(hunk.header.starts_with("@@ -10"), "{}", hunk.header);
        assert_eq!(hunk.lines.len(), 3);

        // Back up onto the second file's header: nothing to act on, said
        // rather than guessed.
        for _ in 0..7 {
            d.run_view("view.up", &host);
        }
        assert!(d.current_hunk().is_none(), "row 0 is the first file header");
    }

    // ---------------------------------------------------------- file summary

    /// Two files, two hunks each, with totals the other file does not share —
    /// so a summary resolved against the wrong one fails loudly. `one.txt` is
    /// +2 −2, `two.txt` is +3 −3.
    const TWO_FILES_TWO_HUNKS: &str = "\
diff --git a/one.txt b/one.txt
--- a/one.txt
+++ b/one.txt
@@ -1,4 +1,4 @@
 alpha
-beta
+BETA
 gamma
 delta
@@ -10,4 +10,4 @@
-epsilon old
 zeta
+inserted
 eta
 theta
diff --git a/two.txt b/two.txt
--- a/two.txt
+++ b/two.txt
@@ -1,3 +1,3 @@
 one
-two old
+two new
 three
@@ -20,4 +21,4 @@
 four
-five old
-five more old
+five new
+six new
 seven
";

    #[test]
    fn the_summary_names_the_file_and_hunk_the_keyboard_is_in() {
        let host = Host::new();
        let mut d = Diff::with_layouts(
            parse_unified_diff(TWO_FILES_TWO_HUNKS),
            &host,
            Layouts::builtin(),
        );
        with_height(&mut d, 30);

        // Unified rows: 0 header; hunk 0 owns 1..=6 (header + five lines);
        // hunk 1 owns 7..=12. Row 8 — eight downs — is inside the *second*
        // hunk of one.txt.
        for _ in 0..8 {
            d.run_view("view.down", &host);
        }
        let s: FileSummary = d.file_summary().expect("on a hunk");
        assert_eq!(s.path, "one.txt");
        assert_eq!((s.adds, s.dels), (2, 2));
        assert_eq!((s.hunk, s.hunks), (2, 2));

        // Twenty-one rows down is still two.txt's second hunk, and its totals
        // are its own: resolve the summary by path and this passes, resolve it
        // against whichever file comes first and it cannot.
        for _ in 8..21 {
            d.run_view("view.down", &host);
        }
        let s = d.file_summary().expect("still on a hunk");
        assert_eq!(s.path, "two.txt");
        assert_eq!((s.adds, s.dels), (3, 3));
        assert_eq!((s.hunk, s.hunks), (2, 2));
    }

    #[test]
    fn a_header_row_says_hunk_one_unless_the_file_has_none() {
        let host = Host::new();

        // A fresh view opens on row 0, which is one.txt's header. The file has
        // hunks, so "first" exists and is 1.
        let d = Diff::with_layouts(
            parse_unified_diff(TWO_FILES_TWO_HUNKS),
            &host,
            Layouts::builtin(),
        );
        let s = d.file_summary().expect("on a header");
        assert_eq!(s.path, "one.txt");
        assert_eq!((s.adds, s.dels), (2, 2));
        assert_eq!((s.hunk, s.hunks), (1, 2));

        // The second file's header answers with the second file's own counts.
        let mut d = Diff::with_layouts(
            parse_unified_diff(TWO_FILES_TWO_HUNKS),
            &host,
            Layouts::builtin(),
        );
        with_height(&mut d, 30);
        for _ in 0..13 {
            d.run_view("view.down", &host);
        }
        let s = d.file_summary().expect("on the second header");
        assert_eq!(s.path, "two.txt");
        assert_eq!((s.adds, s.dels), (3, 3));
        assert_eq!((s.hunk, s.hunks), (1, 2));

        // A file with no hunks at all has no first to point at: 0, and zeroed
        // counts. Built by hand because a unified patch with headers and no
        // hunks is not text the parser owes an opinion about.
        let d = Diff::with_layouts(
            vec![FileDiff {
                path: "bin.dat".into(),
                hunks: Vec::new(),
            }],
            &host,
            Layouts::builtin(),
        );
        let s = d.file_summary().expect("its header is still there");
        assert_eq!(s.path, "bin.dat");
        assert_eq!((s.adds, s.dels), (0, 0));
        assert_eq!((s.hunk, s.hunks), (0, 0));
    }

    #[test]
    fn split_answers_the_same_file_from_fewer_rows() {
        // Collapsing replace pairs onto one row moves every later row; the
        // summary must come out identical anyway, because it reads the map
        // each presentation keeps rather than counting rows itself.
        let mut host = Host::new();
        host.layout = "split".into();
        let mut d = Diff::with_layouts(
            parse_unified_diff(TWO_FILES_TWO_HUNKS),
            &host,
            Layouts::builtin(),
        );
        with_height(&mut d, 30);

        // The same walk that lands on hunk 1's pair in unified lands on it in
        // split too — six downs from the top.
        for _ in 0..6 {
            d.run_view("view.down", &host);
        }
        let s = d.file_summary().expect("on a hunk in split");
        assert_eq!(s.path, "one.txt");
        assert_eq!((s.adds, s.dels), (2, 2));
        assert_eq!((s.hunk, s.hunks), (2, 2));
    }

    #[test]
    fn an_empty_diff_has_nothing_to_name() {
        let host = Host::new();
        let d = Diff::with_layouts(Vec::new(), &host, Layouts::builtin());
        assert!(d.file_summary().is_none());
    }

    #[test]
    fn split_pairs_a_replace_but_not_the_hunks_address() {
        // The whole reason the map lives on the trait: split collapses the
        // beta->BETA replace pair onto one row, so the same hunk is fewer
        // rows than unified drew — and must still answer with the same hunk.
        let mut host = Host::new();
        host.layout = "split".into();
        let host = Rc::new(host);
        let mut d = Diff::with_layouts(parse_unified_diff(THREE_HUNKS), &host, Layouts::builtin());
        with_height(&mut d, 20);

        // Rows: file header 0; hunk 0 = header + [ctx, pair, ctx] = 1..=4;
        // hunk 1 = header + pair = 5..=6. Walk to the pair of hunk 1.
        for _ in 0..6 {
            d.run_view("view.down", &host);
        }
        let (path, hunk) = d.current_hunk().expect("on a hunk");
        assert_eq!(path, "one.txt");
        assert!(hunk.header.starts_with("@@ -10"), "{}", hunk.header);
    }

    #[test]
    fn an_armed_discard_survives_nothing_but_the_same_spot() {
        let host = Rc::new(Host::new());
        let mut d = Diff::with_layouts(parse_unified_diff(THREE_HUNKS), &host, Layouts::builtin());
        with_height(&mut d, 20);

        let id = d.cursor_row_id();
        assert!(!d.confirm_or_arm_discard_hunk(id), "first press asks");
        // The keyboard moves — any move disarms before it can lie.
        d.run_view("view.down", &host);
        assert!(
            !d.confirm_or_arm_discard_hunk(id),
            "the arm died with the cursor move"
        );

        // Arm here, stay put: the second press spends it, and a third asks
        // afresh rather than firing twice off one question.
        let id = d.cursor_row_id();
        assert!(!d.confirm_or_arm_discard_hunk(id));
        assert!(d.confirm_or_arm_discard_hunk(id));
        assert!(!d.confirm_or_arm_discard_hunk(id));
    }
}
