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
//! `plait_core::wrap`, which is a registry on `Host` and has `w` and a title-bar
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

use gpui::*;
use gpui::prelude::FluentBuilder as _;
use gpui_component::scroll::{Scrollbar, ScrollbarHandle};
use plait_core::host::Host;
use plait_core::prepared::{prepare, Prepared};
use plait_core::runs::{self, surfaces, Run};
use plait_core::rows::{Ordered, RowRef};
use plait_core::select::{self, Caret, RowId, Selected, Selection, Text as _};
use plait_core::syntax::Token;
use plait_core::theme::{DiffPalette, Rgb, Surface, Theme};
use plait_core::wrap::{Wrap, Wrapped};
use plait_core::{FileDiff, LineKind, Span};
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

/// Where a click landed inside a row — see [`Rows::hit`].
///
/// `core`'s, since the terminal asks its presentations the same question in
/// cells and got the same answer back.
pub use plait_core::select::Hit;

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
    text.char_indices().nth(col).map(|(i, _)| i).unwrap_or(text.len())
}

// ------------------------------------------------------------------ the seam

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
/// [`Rows::rows`] and [`Rows::reflow`] both default, so a presentation that does
/// not wrap is exactly as long as it was and an extension's compiles unchanged.
/// A presentation that does wrap gets the hard part —
/// [`plait_core::wrap::Wrapped`] — from `core`; see `TextRows::reflow` for what
/// is left, which is six lines and a column budget.
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
    /// rows on the overwhelming majority of frames.
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
        shift: f32,
    ) -> AnyElement;

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
    /// on an unwrapped line — see [`plait_core::select`]. `None` means the row
    /// takes no part in a selection, and defaults to it: an extension's
    /// presentation compiles unchanged and is simply not selectable until it says
    /// where its text is.
    fn hit(
        &self,
        _index: usize,
        _seg: usize,
        _x: f32,
        _host: &Host,
        _shift: f32,
    ) -> Option<Hit> {
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
        l.register("split", |_| vec![Box::new(super::split::SplitRows::default())]);
        l
    }

    /// Adds a presentation, replacing any already registered under the same
    /// name — so `unified` can be corrected rather than only added to.
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


}

// Cycle to the next presentation. Bound to `s` in `main.rs`.
//
// The first real action in the app, and deliberately shaped like the last one
// will be: the view owns a focus handle, the binding is global, and the handler
// is a method. When command dispatch and the mode stack land in `core` this
// becomes a named command they can reach — see `docs/extending.md`.
actions!(plait, [CycleLayout, CycleWrap, CopySelection, SelectAll, SelectNone]);

// The order table's row reference and the table itself are
// `plait_core::rows`': 8 bytes a row, `logical()` for what survives a reflow,
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
fn expand(logical: &[RowRef], renderers: &[Box<dyn Rows>], anchor: Option<RowRef>) -> Ordered {
    let mut order: Vec<RowRef> = Vec::with_capacity(logical.len());
    let (mut widest, mut widest_at) = (0usize, 0usize);
    let mut found = 0usize;
    let mut i = 0;
    while i < logical.len() {
        let r = logical[i];
        while i < logical.len() && logical[i].logical() == r.logical() {
            i += 1;
        }
        let Some(rows) = renderers.get(r.owner as usize) else { continue };
        if anchor.map(RowRef::logical) == Some(r.logical()) {
            found = order.len();
        }
        let n = rows.rows(r.index as usize).clamp(1, u16::MAX as usize);
        for seg in 0..n {
            let w = rows.width(r.index as usize, seg);
            if w > widest {
                (widest, widest_at) = (w, order.len());
            }
            order.push(RowRef { owner: r.owner, seg: seg as u16, index: r.index });
        }
    }
    Ordered { order, widest: widest_at, anchor: found }
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
fn locked(
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

pub struct Diff {
    /// The parsed diff, kept so a layout change can rebuild the rows.
    ///
    /// This is the memory cost of a live toggle, and it is a real one: on the
    /// 714k-line fixture it is a second copy of every line. The alternatives are
    /// worse — cloning the *prepared* diff pays the same memory plus the clone at
    /// load, whether or not anybody ever presses the key, and re-acquiring means
    /// the view needs a repository, which it does not have and should not.
    files: Rc<Vec<FileDiff>>,
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
    /// What the mouse has selected, or nothing.
    ///
    /// The model is `plait_core::select` and this is the only state the window
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
    scroll: UniformListScrollHandle,
    /// The horizontal axis, which is this view's and not the list's — see the
    /// module note. Bounded from `widest` on every reflow.
    pan: Pan,
    /// Which axis the wheel gesture in flight belongs to. `gpui`'s own lock, kept
    /// here because the events arrive at a window handler rather than at a scroll
    /// container — see [`sideways`].
    ongoing: Cell<OngoingScroll>,
    /// Absent in the headless tests, which build a `Diff` with no window and no
    /// `Context` to take a handle from. Present in the app, where it is what
    /// puts this view in the key dispatch path at all.
    focus: Option<FocusHandle>,
    focused: bool,
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

    /// Moves to the next wrap. Bound to `w` in `main.rs`.
    ///
    /// Unlike a layout change this rebuilds nothing below stage 5: the lines,
    /// their tokens and their spans are the same objects, and only where they
    /// break moves. That is why it is a keystroke and the algorithm is a menu.
    pub fn cycle_wrap(&mut self, cx: &mut Context<Self>) {
        let host = crate::config::host(cx);
        if host.wrap.len() < 2 {
            return;
        }
        self.wrap = (self.wrap + 1) % host.wrap.len();
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
        let wrap = host.wrap.at(self.wrap);
        if (width, wrap.name()) == self.applied || width <= 0.0 {
            return;
        }
        self.applied = (width, wrap.name());

        let changed = {
            let mut rs = self.renderers.borrow_mut();
            rs.iter_mut().fold(false, |acc, r| r.reflow(width, host, wrap) | acc)
        };
        if changed {
            // Anchored to the logical row at the top, not to a proportion: a
            // reflow is the same diff at a different width, so the line you were
            // reading still exists and is the honest thing to keep still. A
            // layout change has no such correspondence, which is why it uses a
            // fraction instead.
            let anchor = self.order.get(self.top.get()).copied();
            let built = expand(&self.order, &self.renderers.borrow(), anchor);
            let logical = self.renderers.borrow().iter().map(|r| r.len()).sum::<usize>();
            self.order = Rc::new(built.order);
            self.widest = built.widest;
            self.total.set(self.order.len());
            self.top.set(built.anchor);
            self.scroll_to(built.anchor);
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

    /// A wheel or a trackpad, sideways — and the decision about whether *this*
    /// gesture is sideways at all.
    ///
    /// **A sideways gesture never reaches the list**, and that is the whole of
    /// this method. `uniform_list` scrolls one axis, so `overflow.x` on it is
    /// visible, and `gpui`'s scroll handler reads that as permission to use a
    /// horizontal delta for vertical movement — the arm is
    /// `Overflow::Scroll if !restrict_scroll_to_axis && overflow.x != Scroll => delta.x`.
    /// With the text panning from the same event, a flick to the right came out
    /// diagonal. So this runs in the **capture** phase and stops the event dead
    /// when the gesture is horizontal: one component decides the axis, and the
    /// one that decides is the one that owns the axis it decided on.
    ///
    /// Whether it *could* move does not come into it. A page with nothing to the
    /// right does nothing when you swipe right; it does not start scrolling down.
    fn wheel(&mut self, ev: &ScrollWheelEvent, window: &mut Window, cx: &mut Context<Self>) {
        // Over the rows, and not over the title bar or a dropdown above them.
        // A capture-phase handler is registered on the window, so it is outside
        // the hit test a bubble-phase one gets for free.
        if !self.scroll.0.borrow().base_handle.bounds().contains(&ev.position) {
            return;
        }
        let mut ongoing = self.ongoing.get();
        let delta = locked(
            ev.delta.pixel_delta(window.line_height()),
            ev.modifiers.shift,
            &mut ongoing,
            ev.touch_phase,
        );
        self.ongoing.set(ongoing);
        if delta.x.is_zero() {
            return;
        }
        // Ours *alone* only when the lock says so. A gesture that unlocked
        // mid-flick — swipe left, then up, without lifting — carries both axes
        // for the rest of its life, and eating it would be a diff that stops
        // scrolling down until the fingers come off the glass.
        if delta.y.is_zero() {
            cx.stop_propagation();
        }
        // A scroll to the right moves the content left, which is further into
        // the line: the sign is the one thing to get right in here.
        if self.pan.by(-f32::from(delta.x)) {
            cx.notify();
        }
    }

    /// Which presentation is loaded. Read by the tests and by anything that
    /// wants to name it; the control strip asks for the index and the list.
    #[allow(dead_code)]
    pub fn layout(&self) -> &'static str {
        self.layouts.names().get(self.current).copied().unwrap_or("custom")
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
        self.swap(files, host);
        cx.notify();
    }

    /// The half of [`Diff::replace`] that needs no window, and therefore the
    /// half with tests.
    fn swap(&mut self, files: Vec<FileDiff>, host: &Host) {
        self.files = Rc::new(files);
        self.apply_layout(self.current, host);
    }

    /// Puts a saved row back at the top of the viewport.
    ///
    /// Clamped rather than validated: the diff may be shorter than it was when
    /// the position was taken — a rebuild is usually a code change, but nothing
    /// stops the working tree having moved too.
    pub fn scroll_to(&self, row: usize) {
        if self.order.is_empty() {
            return;
        }
        self.scroll.scroll_to_item(row.min(self.order.len() - 1), ScrollStrategy::Top);
    }

    /// The shipped set: the registry of presentations, opened on whichever one
    /// the host names. An unknown name falls back to the first rather than
    /// failing — the config layer is what reports it, because it is the layer
    /// that knows it came from a file somebody is editing.
    pub fn new(files: Vec<FileDiff>, host: Rc<Host>, cx: &mut Context<Self>) -> Self {
        let mut d = Self::with_layouts(files, &host, Layouts::builtin());
        d.focus = Some(cx.focus_handle());
        d
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
                eprintln!(
                    "plait: unknown diff.layout {:?}; registered: {}",
                    host.layout,
                    layouts.names().join(", ")
                );
                0
            }
        };
        let files = Rc::new(files);
        let built = assemble(&files, host, &layouts, current);
        // The host names the wrap this opens on, exactly as it names the layout.
        // An unknown name is reported by the config layer, which is the layer
        // that knows it came from a file somebody is editing.
        let wrap = host.wrap.selected_index();
        let total = Rc::new(Cell::new(built.order.len()));
        Self {
            files,
            layouts: Rc::new(layouts),
            current,
            wrap,
            applied: (0.0, ""),
            measured: Rc::new(Cell::new(0.0)),
            renderers: Rc::new(RefCell::new(built.renderers)),
            order: Rc::new(built.order),
            sel: None,
            dragging: false,
            widest: built.widest,
            scroll: UniformListScrollHandle::new(),
            pan: Pan::default(),
            ongoing: Cell::default(),
            focus: None,
            focused: false,
            rendered: Rc::new(Cell::new(0)),
            total,
            note: Rc::new(RefCell::new(SharedString::default())),
            top: Rc::new(Cell::new(0)),
            load: built.load,
        }
    }

    /// Moves to the next presentation and rebuilds the rows, keeping you roughly
    /// where you were reading.
    ///
    /// **Roughly, and not exactly.** The two presentations do not have the same
    /// number of rows — a replace pair is one row in the two-column layout and
    /// two in the unified one — so a row index means something different in each
    /// and there is nothing to preserve exactly. The proportion through the diff
    /// is preserved instead, which lands you on the same screenful.
    ///
    /// The whole pipeline from stage 3 runs again. That is 8 ms on a typical diff
    /// and 289 ms on the pathological fixture, once, on a keystroke — which is
    /// the right place to spend it. Making it instant would mean the row
    /// implementations sharing their text behind a refcount instead of owning
    /// it, and that is a change to `prepared::Line`, not to this function.
    pub fn cycle_layout(&mut self, cx: &mut Context<Self>) {
        if self.layouts.len() < 2 {
            return;
        }
        // The live host, not one captured when this view was built — the same
        // reason `render` reads it per batch. A layout rebuilt from a stale font
        // would quietly disagree with the row it replaced.
        let host = crate::config::host(cx);
        self.apply_layout((self.current + 1) % self.layouts.len(), &host);
        cx.notify();
    }

    /// Rebuilds the rows for `index`, keeping the reading position. The half of
    /// [`Diff::cycle_layout`] and [`Diff::replace`] that needs no window, and
    /// therefore the half with tests.
    fn apply_layout(&mut self, index: usize, host: &Host) {
        let fraction = match self.order.len() {
            0 => 0.0,
            n => self.top.get() as f32 / n as f32,
        };
        self.current = index;
        // Every row about to be replaced, so a selection anchored to one of them
        // would be pointing at whatever now has its index. There is no honest
        // way to carry a selection across two presentations of the same diff —
        // a replace pair is one row here and two there — so it goes.
        self.sel = None;
        let built = assemble(&self.files, host, &self.layouts, index);
        self.order = Rc::new(built.order);
        *self.renderers.borrow_mut() = built.renderers;
        self.widest = built.widest;
        self.load = built.load;
        self.total.set(self.order.len());
        // Fresh implementations hold no wrap, so the next frame reflows them.
        // Left to that rather than done here, because the width belongs to the
        // window and this half of a layout change is the half with no window.
        self.applied = (0.0, "");
        let row = (fraction * self.order.len() as f32) as usize;
        self.top.set(row);
        self.scroll_to(row);
    }
}

/// What one pass of stages 3–5 produces.
struct Built {
    renderers: Vec<Box<dyn Rows>>,
    order: Vec<RowRef>,
    widest: usize,
    load: String,
}

/// Prepare the diff, hand each file to the implementation that claims it, and
/// build the order table.
///
/// A free function rather than a method because it runs before a `Diff` exists
/// and again after one does, and both callers want exactly this.
fn assemble(files: &[FileDiff], host: &Host, layouts: &Layouts, current: usize) -> Built {
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

    // One pass in core, shared with the CLI and the ANSI painter, before any
    // renderer sees a row.
    let Prepared { files: prepared, intraline, syntax } =
        prepare(files, &host.syntax, MAX_LINE_CHARS);
    let file_count = prepared.len();

    for f in prepared {
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
            order.push(RowRef { owner: owner as u16, seg: 0, index: index as u32 });
        }
    }

    // One entry per logical row so far, which is what `expand` wants. Nothing
    // wraps yet — no implementation has been given a width — so this pass only
    // finds the widest row; the first frame reflows and runs it again.
    let Ordered { order, widest, .. } = expand(&order, &renderers, None);

    let mut reports: Vec<String> =
        vec![format!("intraline {intraline:.0?} · syntax {syntax:.0?}")];
    reports.extend(renderers.iter().map(|r| r.report()).filter(|s| !s.is_empty()));
    let load = format!(
        "{} rows · {} files · {name} · build {:.0?} ({})",
        order.len(),
        file_count,
        t.elapsed(),
        reports.join(" · "),
    );
    eprintln!("{load}");
    Built { renderers, order, widest, load }
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

/// Where every row's selectable text comes from, for `plait_core::select`.
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
    fn locate(&self, pos: Point<Pixels>, host: &Host) -> Option<(u16, Caret)> {
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
        Some((hit.part, Caret { row: r.logical(), off: hit.off, at: first..first + n }))
    }

    /// A selection over one byte range of one row: what a double or a triple
    /// click makes.
    fn span(&self, part: u16, at: &Caret, bytes: Range<usize>) -> Selection {
        let mut sel = Selection::new(part, Caret { off: bytes.start, ..at.clone() });
        sel.extend(Caret { off: bytes.end, ..at.clone() });
        sel
    }

    /// The text of one row, for a word or a whole-row selection.
    fn row_text(&self, row: RowId, part: u16) -> Option<String> {
        Selectable(&self.renderers.borrow()).text(row, part).map(str::to_string)
    }

    /// Mouse down: a new selection, a widened one on a repeat click, or an
    /// extension of the existing one when shift is held.
    ///
    /// A press on nothing selectable *clears*, which is the whole reason a fresh
    /// [`Selection`] is empty until something extends it: a click has to be able
    /// to mean "no longer selected".
    fn press(&mut self, ev: &MouseDownEvent, cx: &mut Context<Self>) {
        let host = crate::config::host(cx);
        let Some((part, caret)) = self.locate(ev.position, &host) else {
            self.sel = None;
            cx.notify();
            return;
        };
        self.dragging = true;
        // Shift extends whatever is already there, which is how a selection
        // longer than the window gets made without a drag that has to scroll.
        // Only within the same part: across the divider it means nothing.
        let extend =
            ev.modifiers.shift && self.sel.as_ref().is_some_and(|s| s.part() == part);
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
        let Some((part, mut caret)) = self.locate(ev.position, &host) else { return };
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

    /// `copy.selection`. A no-op with nothing selected rather than a cleared
    /// clipboard — losing what somebody copied elsewhere is worse than a key
    /// that did nothing.
    pub fn copy(&self, cx: &mut Context<Self>) {
        let text = self.selection();
        if !text.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    /// `select.all`.
    pub fn select_all(&mut self, cx: &mut Context<Self>) {
        self.sel = Selection::all(&self.order);
        cx.notify();
    }

    /// `select.none`.
    pub fn select_none(&mut self, cx: &mut Context<Self>) {
        if self.sel.take().is_some() {
            cx.notify();
        }
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
        canvas(|_, _, _| {}, move |_, _, window, _| {
            let me = me.clone();
            window.on_mouse_event(move |ev: &MouseMoveEvent, phase, _, cx| {
                if phase == DispatchPhase::Bubble {
                    _ = me.update(cx, |this, cx| this.drag(ev, cx));
                }
            });
        })
        .absolute()
        .top_0()
        .left_0()
        .h(px(0.))
        .into_any_element()
    }

    /// The same trick as [`Diff::drag_probe`], for the wheel, and for a different
    /// reason: not to hear about events outside the box, but to hear about them
    /// **first**. A `div`'s `on_scroll_wheel` is bubble-phase only, and the list
    /// is a child — so by the time it fired, the list had already turned a
    /// sideways flick into vertical scrolling. See [`Diff::wheel`].
    fn wheel_probe(&self, cx: &mut Context<Self>) -> AnyElement {
        let me = cx.entity().downgrade();
        canvas(|_, _, _| {}, move |_, _, window, _| {
            let me = me.clone();
            window.on_mouse_event(move |ev: &ScrollWheelEvent, phase, window, cx| {
                if phase == DispatchPhase::Capture {
                    _ = me.update(cx, |this, cx| this.wheel(ev, window, cx));
                }
            });
        })
        .absolute()
        .top_0()
        .left_0()
        .h(px(0.))
        .into_any_element()
    }
}

impl Render for Diff {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Whatever the last frame measured this view to be. Reflowing here and
        // not in the probe itself keeps every mutation of the row tables on the
        // one path, and costs one frame of unwrapped rows at startup.
        self.reflow(self.measured.get(), &crate::config::host(cx));

        let renderers = self.renderers.clone();
        let order = self.order.clone();
        let rendered = self.rendered.clone();
        let top = self.top.clone();
        // Cloned per frame and not held behind a cell: it is two carets, the
        // closure lives for one element tree, and every path that changes a
        // selection notifies — so the copy in here is never the stale one.
        let sel = self.sel.clone();
        // Where the scrollbar draws itself and how long its thumb is. Last
        // frame's box, like everything else measured here — a view is handed
        // one and cannot ask before.
        self.pan.set_viewport(self.scroll.0.borrow().base_handle.bounds());
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
            // Once per batch of rows, not once per row.
            let renderers = renderers.borrow();
            range
                .map(|i| {
                    let r = order[i];
                    // Two integer comparisons on a row with no selection, which
                    // is every row of every frame until somebody drags.
                    let at = sel.as_ref().and_then(|s| s.at(i, r.logical()));
                    renderers[r.owner as usize]
                        .render(r.index as usize, r.seg as usize, &host, at, shift)
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

        let mut root = div()
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
            .child(self.wheel_probe(cx))
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
                d.child(Scrollbar::vertical(&self.scroll))
                    .child(Scrollbar::horizontal(&self.pan))
            });

        // Key dispatch runs down the focus path, so an action handler on an
        // element nothing has focused is never reached. Taking focus on the
        // first frame is what puts this view in that path; there is no other
        // focusable thing in the window yet, and when there is, a mode stack is
        // what should be deciding.
        if let Some(focus) = self.focus.clone() {
            if !self.focused {
                self.focused = true;
                window.focus(&focus, cx);
            }
            root = root
                .track_focus(&focus)
                .on_action(cx.listener(|this, _: &CycleLayout, _, cx| this.cycle_layout(cx)))
                .on_action(cx.listener(|this, _: &CycleWrap, _, cx| this.cycle_wrap(cx)))
                .on_action(cx.listener(|this, _: &CopySelection, _, cx| this.copy(cx)))
                .on_action(cx.listener(|this, _: &SelectAll, _, cx| this.select_all(cx)))
                .on_action(cx.listener(|this, _: &SelectNone, _, cx| this.select_none(cx)));
        }
        root
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

/// The default presentation: one line of text per row, behind a line-number
/// gutter, coloured by the host's theme.
#[derive(Default)]
pub struct TextRows {
    rows: Vec<Row>,
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

    fn build(&mut self, f: plait_core::prepared::File) {
        self.rows.push(Row::File {
            path: f.path.into(),
            adds: f.adds,
            dels: f.dels,
        });
        for h in f.hunks {
            self.rows.push(Row::Hunk(h.header.into()));
            for l in h.lines {
                self.moved += l.moved as usize;
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
        }
    }

    /// Characters, not bytes, and after `trim_end`: a line of box drawing is a
    /// third as many columns as it is bytes, and whitespace at the end of a row
    /// is not ink. Both were wrong here in the direction of a scrollable width
    /// wider than anything on screen.
    fn width(&self, index: usize, seg: usize) -> usize {
        match &self.rows[index] {
            Row::Line { text, .. } => {
                text[self.wrapped.range(index, seg, text)].trim_end().chars().count()
            }
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

    /// The gutters and the sign column, then the text — and for a header, the
    /// page padding and nothing else, because that is what it draws.
    fn hit(&self, index: usize, seg: usize, x: f32, host: &Host, shift: f32) -> Option<Hit> {
        Some(match self.rows.get(index)? {
            Row::Hunk(h) => header_hit(h, x, host, shift),
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

    fn render(
        &self,
        index: usize,
        seg: usize,
        host: &Host,
        sel: Option<Selected>,
        shift: f32,
    ) -> AnyElement {
        let theme = &host.theme;
        let p = &theme.diff;
        match &self.rows[index] {
            Row::File { path, adds, dels } => file_header(path, *adds, *dels, theme, sel, shift),

            Row::Hunk(header) => hunk_header(header, theme, sel, shift),

            Row::Line { kind, moved, old, new, text, spans, tokens } => {
                let (bg, fg, sign) = line_colors(*kind, *moved, p);
                // Which background this row's furniture lands on, so the line
                // numbers are resolved against it — see `Theme::gutter_on`.
                let gutter = theme.gutter_on(surfaces(*kind, *moved).0);
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
                row_frame()
                    .items_center()
                    .px(px(PAD))
                    .bg(rgb(bg))
                    .child(num(sc.number(*old, blank), gutter))
                    .child(num(sc.number(*new, blank), gutter))
                    .child(
                        div()
                            .flex_none()
                            .w(px(SIGN_W))
                            .text_color(rgb(fg))
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
                                    selected(sel, 0, text.len()),
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

/// The same, for text GPUI already owns — a re-flowed Markdown table row.
/// Whole-row clones stay refcount bumps there too, and wrapped segments take
/// the by-reference path [`slice`] does.
pub(crate) fn slice_shared(text: &SharedString, at: &Range<usize>) -> SharedString {
    match at.start == 0 && at.end == text.len() {
        true => text.clone(),
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

/// The bytes of a `len`-long text that a selection covers, or nothing at all
/// when the selection is in another of the row's parts — or is not there, which
/// is the case on nearly every row of nearly every frame.
pub(crate) fn selected(sel: Option<Selected>, part: u16, len: usize) -> Range<usize> {
    match sel.filter(|s| s.part() == part) {
        Some(s) => s.range(len),
        None => 0..0,
    }
}

/// Where a click landed in a file or hunk header.
///
/// Shared for the same reason the two headers themselves are: whoever owns the
/// lines beneath them, a header is drawn by [`file_header`] or [`hunk_header`] and
/// its text starts at the page padding. Three presentations working that out
/// separately is three places for the caret to be a gutter's width off.
pub(crate) fn header_hit(text: &str, x: f32, host: &Host, shift: f32) -> Hit {
    Hit { part: 0, off: column_at(text, into_text(x, PAD, shift), host.font.size, host) }
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
pub(crate) fn file_header(
    path: &std::sync::Arc<str>,
    adds: usize,
    dels: usize,
    theme: &Theme,
    sel: Option<Selected>,
    shift: f32,
) -> AnyElement {
    let p = &theme.diff;
    let (dir, name) = split_path(path);
    // One range over the whole path, split between the two elements below.
    let sel = selected(sel, 0, path.len());
    let cut = dir.as_ref().map_or(0, |d| d.len());
    row_frame()
        // A column, so the rule is part of the row's own 22 pixels rather than
        // added to them: every row in this list is exactly `ROW_H` tall and the
        // list is what makes 714k of them scroll.
        .flex_col()
        .bg(rgb(p.file_bg))
        .child(div().flex_none().h(px(1.)).bg(rgb(p.rule)))
        // A path longer than the window is reached the same way a line is: it is
        // the row's text, and the only thing in front of it is the page padding —
        // which is why the padding is out here and the scroll is inside it.
        .child(div().flex().flex_grow(1.0).px_4().child(scrolled(
            shift,
            div()
                .flex()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .children(dir.map(|d| {
                            // The furniture colour, resolved against the header's
                            // own background rather than a row's — a header is not
                            // a `Surface`, and `gutter_fg` raw is 1.7:1 on it.
                            // Twice a frame at most: one header per file.
                            let fg = plait_core::theme::readable(
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
                        .child(
                            div().flex_none().text_color(rgb(p.file_fg)).child(header_text(
                                match dir {
                                    // A bare name *is* the whole path: adopt the
                                    // row's own handle rather than copying it.
                                    None => SharedString::from(std::sync::Arc::clone(path)),
                                    Some(_) => SharedString::from(name),
                                },
                                clipped(&sel, cut..path.len()),
                                theme,
                            )),
                        ),
                )
                .child(div().flex_none().text_color(rgb(p.adds_fg)).child(format!("+{adds}")))
                .child(div().flex_none().text_color(rgb(p.dels_fg)).child(format!("-{dels}"))),
        )))
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
/// [`plait_core::hunk_parts`], so every client agrees where it is. The
/// coordinates are furniture and take the gutter's colour, which is what they
/// are: a line number with a range around it. The declaration git appends is the
/// half a reader wants, and keeps `hunk_fg`.
///
/// The band itself recedes now. It used to be the more prominent of the two
/// headers, which had the hierarchy backwards: a hunk is a place inside a file,
/// and the file is the boundary that matters.
pub(crate) fn hunk_header(
    header: &std::sync::Arc<str>,
    theme: &Theme,
    sel: Option<Selected>,
    shift: f32,
) -> AnyElement {
    let p = &theme.diff;
    let (marker, _) = plait_core::hunk_parts(header);
    row_frame()
        .items_center()
        .px_4()
        .bg(rgb(p.hunk_bg))
        .text_color(rgb(p.hunk_fg))
        .child(scrolled(
            shift,
            div().child(
                // The row's own handle, adopted rather than copied.
                StyledText::new(SharedString::from(std::sync::Arc::clone(header)))
                    .with_highlights(hunk_runs(
                        marker.len(),
                        selected(sel, 0, header.len()),
                        theme,
                    )),
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
            let style = r.kind.map(|k| theme.syntax_on(k, r.surface));
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
    use super::{line_colors, locked, Diff, Layouts, Pan, Row, Rows, TextRows, PAD, TEXT_CHROME};
    use gpui::{
        div, rgb, AnyElement, FontStyle, FontWeight, HighlightStyle, IntoElement, ParentElement,
    };
    use plait_core::host::Host;
    use plait_core::syntax::{Kind, Token};
    use plait_core::theme::{Style, Surface, Theme};
    use plait_core::prepared::{prepare, File as PreparedFile};
    use plait_core::select::{Caret, Selected, Selection};
    use plait_core::{parse_unified_diff, LineKind, Span};
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
        super::Scratch::merged(&mut sc, at, tokens, spans, theme, kind, moved, sel).to_vec()
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
        let theme = Theme::dark();
        assert!(runs(0..12, &[], &[], &theme, LineKind::Context, false).is_empty());
    }

    #[test]
    fn a_token_and_a_span_over_the_same_bytes_split_into_both() {
        // `let` is a keyword and also a changed word: one run carrying a
        // foreground and a background, not two elements fighting over it.
        let theme = Theme::dark();
        let text = "let x = 1;";
        let out =
            runs(all(text), &[tok(0, 3, Kind::Keyword)], &[Span { start: 0, end: 3 }], &theme, LineKind::Added, false);
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
        let out =
            runs(all(text), &[tok(0, 3, Kind::Keyword)], &[Span { start: 2, end: 7 }], &theme, LineKind::Added, false);
        well_formed(text, &out);
        let shape: Vec<_> = out
            .iter()
            .map(|(r, s)| (r.clone(), s.color.is_some(), s.background_color.is_some()))
            .collect();
        assert_eq!(shape, vec![(0..2, true, false), (2..3, true, true), (3..7, false, true)]);
    }

    #[test]
    fn many_tokens_and_spans_stay_sorted_and_disjoint() {
        let theme = Theme::dark();
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
        let out = runs(all(text), &tokens, &spans, &theme, LineKind::Removed, false);
        well_formed(text, &out);
        assert!(out
            .iter()
            .any(|(r, s)| *r == (28..29) && s.color.is_some() && s.background_color.is_some()));
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
            &[Span { start: quote as u32, end: (text.len() - 1) as u32 }],
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
            assert_ne!(plain, moved, "{kind:?} moved and unmoved share a background");
            assert_eq!(sign, moved_sign, "the sign column must stay scannable");
        }
        // Context is never moved, and asking must not change what it looks like.
        assert_eq!(
            line_colors(LineKind::Context, true, p),
            line_colors(LineKind::Context, false, p)
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
        assert!(out.is_empty(), "a moved line produced runs for nothing: {out:?}");
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
            &[Span { start: 10, end: text.len() as u32 }],
            &theme,
            LineKind::Added,
            false,
        );
        well_formed(text, &out);
        let plain = out.iter().find(|(r, _)| r.start == 0).unwrap();
        let on_word = out.iter().find(|(r, _)| r.start == 10).unwrap();
        assert!(on_word.1.background_color.is_some());
        assert_ne!(plain.1.color, on_word.1.color, "same grey on both backgrounds");
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
        let out =
            super::Scratch::merged(&mut sc, all(text), &tokens, &spans, &theme, LineKind::Added, false, 6..20);
        well_formed(text, out);
        let caps = (sc.runs.capacity(), sc.hl.capacity());
        for _ in 0..100 {
            super::Scratch::merged(&mut sc, all(text), &tokens, &spans, &theme, LineKind::Added, false, 6..20);
        }
        assert_eq!((sc.runs.capacity(), sc.hl.capacity()), caps, "a repaint grew a buffer");
    }

    #[test]
    fn a_gutter_number_formats_into_the_scratch_and_pads_nowhere() {
        // The integers replaced pre-rendered strings, so what reaches the
        // screen must be exactly what those strings were: bare digits, nothing
        // padded in — right-alignment is the column's, not the text's.
        let mut sc = super::Scratch::default();
        assert_eq!(&*sc.number(Some(9), false), "9");
        assert_eq!(&*sc.number(Some(12345), false), "12345");
        assert_eq!(&*sc.number(Some(7), true), "", "a continuation row draws nothing");
        assert_eq!(&*sc.number(None, false), "", "so does a side with no number");
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
        let row = (0..r.len()).find(|i| r.rows(*i) > 1).expect("a wrapped line");
        let first = r.hit(row, 0, x_for(3, &host), &host, 0.0).unwrap().off;
        let second = r.hit(row, 1, x_for(3, &host), &host, 0.0).unwrap().off;
        assert_eq!(first, 3);
        assert!(second > 30, "the second row rebased to {second}, not into the line");
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
        let hit = r.hit(0, 0, PAD + 2.5 * host.font.size * host.font.advance, &host, 0.0).unwrap();
        assert_eq!(hit.off, 2);
    }

    #[test]
    fn a_selection_over_three_rows_copies_the_lines_between_them() {
        let host = Host::new();
        let mut diff = Diff::with_layouts(parse_unified_diff(SAMPLE), &host, Layouts::builtin());
        // Rows: the file header, the hunk header, then three lines.
        diff.sel = Some(select(&diff, (2, 0), (4, 9)));
        assert_eq!(diff.selection(), "fn main() {
    let x = 1;
    let x");
        // The anchor is not the start: the same drag backwards is the same text.
        diff.sel = Some(select(&diff, (4, 9), (2, 0)));
        assert_eq!(diff.selection(), "fn main() {
    let x = 1;
    let x");
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
        let mut sel = Selection::new(0, Caret { row: long.logical(), off: 0, at: at..at + n });
        sel.extend(Caret { row: long.logical(), off: whole.len(), at: at..at + n });
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
            "a.rs\n@@ -1,2 +1,2 @@\nfn main() {\n    let x = 1;\n    let x = 2;"
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
            !out.iter().any(|(r, s)| r.contains(&8) && s.background_color == Some(word.into())),
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
        let (marker, code) = plait_core::hunk_parts(header);
        let out = super::hunk_runs(marker.len(), 0..0, &theme);
        well_formed(header, &out);
        assert_eq!(out.len(), 1, "one run: the coordinates");
        assert_eq!(out[0].0, 0..marker.len());
        assert_eq!(out[0].1.color, Some(rgb(theme.gutter_on(Surface::Context)).into()));
        assert!(out[0].1.background_color.is_none());
        assert!(!code.is_empty() && out.iter().all(|(r, _)| r.end <= marker.len()));
    }

    #[test]
    fn a_selection_across_a_hunk_header_keeps_both_of_its_colours() {
        // The case two side-by-side elements could not draw: one selection whose
        // ends live in different halves of the header.
        let theme = Theme::dark();
        let header = "@@ -41,9 +41,11 @@ fn dispatch() {";
        let marker = plait_core::hunk_parts(header).0.len();
        let out = super::hunk_runs(marker, 5..25, &theme);
        well_formed(header, &out);
        let bg = rgb(theme.chrome.selected_bg);
        let painted: Vec<usize> = out
            .iter()
            .filter(|(_, st)| st.background_color == Some(bg.into()))
            .flat_map(|(r, _)| r.clone())
            .collect();
        assert_eq!(painted, (5..25).collect::<Vec<_>>(), "the selection, exactly");
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
        let marker = plait_core::hunk_parts(header).0.len();
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
        assert!(super::hunk_runs(0, 0..0, &theme).is_empty(), "nothing at all");
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
        let second = runs_sel(5..text.len(), &[], &[], &theme, LineKind::Context, false, 2..12);
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
        let at = |v: usize| Caret { row: diff.order[v].logical(), off: 0, at: v..v + 1 };
        let mut sel = Selection::new(0, Caret { off: from.1, ..at(from.0) });
        sel.extend(Caret { off: to.1, ..at(to.0) });
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
        assert!((0..r.len()).all(|i| r.rows(i) == 1), "nothing wraps before a reflow");

        assert!(r.reflow(width_for(40, &host), &host, host.wrap.current()));
        assert_eq!(r.len(), before, "wrapping changed the line count");
        let rows: Vec<usize> = (0..r.len()).map(|i| r.rows(i)).collect();
        assert_eq!(rows, [1, 1, 1, 3, 3], "headers, a short line, two long ones");
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
        assert!(!r.reflow(w + 1.0, &host, host.wrap.current()), "one pixel rebuilt the table");
        assert!(r.reflow(w + 40.0, &host, host.wrap.current()), "five characters did not");
    }

    #[test]
    fn turning_it_off_collapses_the_rows_again() {
        let (mut r, host) = text_rows(LONG);
        let narrow = width_for(20, &host);
        r.reflow(narrow, &host, host.wrap.current());
        assert!(r.rows(3) > 1);

        let off = host.wrap.at(host.wrap.position("off").unwrap());
        assert!(r.reflow(narrow, &host, off));
        assert!((0..r.len()).all(|i| r.rows(i) == 1), "off still broke something");
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
            let plain = r.hit(3, 0, x_for(0, &host), &host, col as f32 * cw).unwrap();
            assert_eq!((plain.part, plain.off), (0, col), "scrolled {col} characters");
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
        let d = locked(point(px(-30.), px(0.)), false, &mut lock, TouchPhase::Started);
        assert_eq!((d.x, d.y), (px(-30.), px(0.)));
        // The rest of the same gesture is sideways too, however the fingers
        // wander: this is the drift that read as the text sliding at an angle.
        let d = locked(point(px(-12.), px(4.)), false, &mut lock, moved);
        assert_eq!(d.y, px(0.), "a locked gesture leaked onto the other axis");
        assert!(d.x < px(0.));

        // A vertical gesture is the list's, and this hands back nothing for the
        // text to move by — not even the sideways wobble in it.
        let mut lock = OngoingScroll::default();
        let d = locked(point(px(3.), px(-40.)), false, &mut lock, TouchPhase::Started);
        assert_eq!((d.x, d.y), (px(0.), px(-40.)));

        // `shift` is the platform's way of saying "this one is horizontal", and
        // it is applied before the lock — after it, the lock has already called
        // the gesture vertical and given it away.
        let mut lock = OngoingScroll::default();
        let d = locked(point(px(0.), px(-40.)), true, &mut lock, TouchPhase::Started);
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
        let diff =
            Diff::with_layouts(parse_unified_diff(src), &host, Layouts::builtin());
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
        assert!((diff.bound(w, &host) - expected).abs() < 0.001, "{}", diff.bound(w, &host));
        // The offset the reflow left is inside it, whatever it was before.
        diff.pan.set(1e6);
        assert_eq!(diff.pan.at(), diff.bound(w, &host));

        // And a wrapped diff has nowhere left to go, which is what puts the text
        // back at column zero the moment `w` turns wrapping on.
        let (mut wrapped, host) = diff_wrapped(LONG, "word");
        wrapped.reflow(w, &host);
        assert_eq!(wrapped.bound(w, &host), 0.0, "a wrapped row hangs over the edge");
        assert_eq!(wrapped.pan.at(), 0.0);
    }

    #[test]
    fn a_registered_wrap_reaches_the_rows() {
        // The swap test. A policy that breaks every line into single characters
        // is absurd and unmistakable, which is the point.
        struct EveryChar;
        impl plait_core::wrap::Wrap for EveryChar {
            fn name(&self) -> &'static str {
                "every-char"
            }
            fn breaks(&self, text: &str, _cols: usize, out: &mut Vec<plait_core::wrap::Break>) {
                for (i, _) in text.char_indices().skip(1) {
                    out.push(plait_core::wrap::Break::hard(i));
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

        // Park on the last line, reflow narrower, and it is still the line at
        // the top — at a different row number, because rows above it grew.
        let last_line = diff.order.last().unwrap().logical();
        diff.top.set(diff.total() - 1);
        diff.reflow(width_for(20, &host), &host);
        assert_eq!(diff.order[diff.top.get()].logical(), last_line);
        assert_eq!(diff.order[diff.top.get()].seg, 0, "not the top of its line");
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

        let out = runs(second..text.len(), &tokens, &[], &theme, LineKind::Context, false);
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
        let diff = Diff::with_layouts(
            parse_unified_diff(TWO_FILES),
            &host,
            Layouts::builtin(),
        );
        assert_eq!(diff.layout(), "split");
        // The two-column layout collapses a replace pair onto one row, so it has
        // strictly fewer rows than unified for the same diff.
        let unified =
            Diff::with_layouts(parse_unified_diff(TWO_FILES), &Host::new(), Layouts::builtin());
        assert_eq!(unified.layout(), "unified");
        assert!(diff.total() < unified.total(), "{} vs {}", diff.total(), unified.total());
    }

    #[test]
    fn an_unknown_layout_name_opens_the_first_rather_than_nothing() {
        let mut host = Host::new();
        host.layout = "sidebyside".into();
        let diff = Diff::with_layouts(parse_unified_diff(SAMPLE), &host, Layouts::builtin());
        assert_eq!(diff.layout(), "unified");
        assert!(diff.total() > 0, "a typo in a live-reloaded file must not blank the diff");
    }

    #[test]
    fn cycling_returns_to_where_it_started() {
        let host = Host::new();
        let mut diff =
            Diff::with_layouts(parse_unified_diff(TWO_FILES), &host, Layouts::builtin());
        let (name, total) = (diff.layout(), diff.total());
        diff.apply_layout(1, &host);
        assert_eq!(diff.layout(), "split");
        assert_ne!(diff.total(), total);
        diff.apply_layout(0, &host);
        assert_eq!(diff.layout(), name);
        assert_eq!(diff.total(), total, "a round trip must rebuild the same rows");
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

        diff.swap(parse_unified_diff(TWO_FILES), &host);
        assert_eq!(diff.layout(), "split", "the swap reset the presentation");
        assert!(diff.load.contains("2 files"), "{}", diff.load);
        assert!(diff.load.contains("split"), "{}", diff.load);

        // And an empty diff is a swap too — a revspec whose changes vanished.
        diff.swap(Vec::new(), &host);
        assert_eq!(diff.total(), 0);
        assert_eq!(diff.layout(), "split");
    }

    #[test]
    fn cycling_keeps_you_at_the_same_point_in_the_diff() {
        // Exactly is impossible — the two presentations do not have the same
        // number of rows — so the proportion is what is preserved.
        let host = Host::new();
        let mut diff = Diff::with_layouts(long_diff(), &host, Layouts::builtin());
        let total = diff.total();
        diff.top.set(total / 2);
        diff.apply_layout(1, &host);
        let landed = diff.top.get() as f32 / diff.total() as f32;
        assert!((landed - 0.5).abs() < 0.05, "landed {landed} of the way through");
    }

    #[test]
    fn a_registered_presentation_is_cycled_to_like_a_built_in() {
        // Rule 1, as a test: a third presentation needs no edit to the two
        // shipped ones, and `[diff] layout` reaches it.
        let mut layouts = Layouts::builtin();
        layouts.register("one-liner", |_| vec![Box::new(OneLinerEverything::default())]);
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
        assert_eq!(layouts.names(), vec!["unified", "split"], "a replacement must not append");
        let diff =
            Diff::with_layouts(parse_unified_diff(TWO_FILES), &Host::new(), layouts);
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
            _shift: f32,
        ) -> AnyElement {
            div().child(self.rows[index].clone()).into_any_element()
        }
    }

    /// Enough rows that a proportional scroll position means something.
    fn long_diff() -> Vec<plait_core::FileDiff> {
        let mut raw = String::from("diff --git a/big.rs b/big.rs\n@@ -1,200 +1,200 @@\n");
        for i in 0..200 {
            if i % 5 == 0 {
                raw.push_str(&format!("-    let x{i} = {i};\n+    let x{i} = {};\n", i + 1));
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
            assert!(diff.widest < diff.total(), "{name}: widest {} of {}", diff.widest, diff.total());
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
}

