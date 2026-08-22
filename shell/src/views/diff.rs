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

use gpui::*;
use gpui_component::scroll::Scrollbar;
use plait_core::host::Host;
use plait_core::prepared::{prepare, Prepared};
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
const GUTTER_W: f32 = 52.0;
/// The `+`/`-` column.
const SIGN_W: f32 = 16.0;
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
/// approximation `with_width_from_item` and the Markdown table padding already
/// make, and the reason `Font::monospaced` exists to be asked.
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

/// Where a click landed inside a row: which of the row's texts, and which byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit {
    pub part: u16,
    /// A byte offset into the *logical* row's text — see [`Rows::hit`].
    pub off: usize,
}

/// Which byte of `text` a click `x` pixels into it landed on.
///
/// Characters, not bytes and not graphemes, and from `font.advance` rather than a
/// measured glyph: exactly the approximation `columns` and `with_width_from_item`
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
    fn render(&self, index: usize, seg: usize, host: &Host, sel: Option<Selected>) -> AnyElement;

    /// Width of a visual row in characters, for `uniform_list`'s one measured
    /// row.
    fn width(&self, index: usize, seg: usize) -> usize;

    /// Which text a click at `x` pixels from this row's left edge landed in, and
    /// which byte of it.
    ///
    /// The frontend half of a selection, and the only half that needs pixels:
    /// where the text starts depends on the gutters, the bars and the indents
    /// this presentation drew in front of it, and how wide a character is depends
    /// on the face and — in a rendered document — on the row. Nobody outside an
    /// implementation can know either, which is why this is on the trait rather
    /// than a division in the view.
    ///
    /// The offset is into the **logical** row's text, not the visual row's, so a
    /// caret on the third row of a wrapped line is the same kind of thing as one
    /// on an unwrapped line — see [`plait_core::select`]. `None` means the row
    /// takes no part in a selection, and defaults to it: an extension's
    /// presentation compiles unchanged and is simply not selectable until it says
    /// where its text is.
    fn hit(&self, _index: usize, _seg: usize, _x: f32, _host: &Host) -> Option<Hit> {
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
    /// See the note in the commits view: uniform_list sizes its scrollable
    /// width from a single measured row, defaulting to row 0.
    widest: usize,
    scroll: UniformListScrollHandle,
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
    /// runs on a resize and on a wrap change and at no other time. Three ways
    /// out before it does any work, in increasing cost: nothing moved, nothing
    /// *can* move because the wrap never breaks, and no implementation's row
    /// count actually changed.
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
        let same_wrap = self.applied.1 == wrap.name();
        self.applied = (width, wrap.name());
        // A wrap that never breaks has no width to be wrong about. Without this
        // every pixel of a drag rescans the whole diff to be told nothing moved.
        if !wrap.breaks_lines() && same_wrap {
            return;
        }

        let changed = {
            let mut rs = self.renderers.borrow_mut();
            rs.iter_mut().fold(false, |acc, r| r.reflow(width, host, wrap) | acc)
        };
        if !changed {
            return;
        }

        // Anchored to the logical row at the top, not to a proportion: a reflow
        // is the same diff at a different width, so the line you were reading
        // still exists and is the honest thing to keep still. A layout change
        // has no such correspondence, which is why it uses a fraction instead.
        let anchor = self.order.get(self.top.get()).copied();
        let built = expand(&self.order, &self.renderers.borrow(), anchor);
        let logical = self.renderers.borrow().iter().map(|r| r.len()).sum::<usize>();
        *self.note.borrow_mut() = format!(
            "{} · {:.0} px · {} rows / {logical} lines",
            wrap.name(),
            width,
            built.order.len()
        )
        .into();
        self.order = Rc::new(built.order);
        self.widest = built.widest;
        self.total.set(self.order.len());
        self.top.set(built.anchor);
        self.scroll_to(built.anchor);
        // The rows are the same rows at different heights, so the selection
        // survives — but every visual row it cached has moved, which is what
        // `resolve` rebuilds. A drag *through* a reflow cannot happen: the width
        // only changes when the mouse is on the window's edge.
        if let Some(sel) = &mut self.sel {
            if !sel.resolve(&self.order) {
                self.sel = None;
            }
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
        let x = f32::from(pos.x - bounds.origin.x - offset.x);
        let r = self.order[visual];

        let renderers = self.renderers.borrow();
        let rows = renderers.get(r.owner as usize)?;
        let hit = rows.hit(r.index as usize, r.seg as usize, x, host)?;
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
                        .render(r.index as usize, r.seg as usize, &host, at)
                })
                .collect()
        })
        .with_width_from_item(Some(self.widest))
        .track_scroll(&self.scroll)
        .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
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
            // How wide this view actually is, which is the wrap budget. A view
            // is handed its box by whatever assembled it and cannot know it
            // during `render`, so it is read off the paint pass and used on the
            // frame after — the same one-frame trade every measured layout
            // makes. Zero height, so it takes part in nothing.
            .child(self.probe(cx))
            .child(Scrollbar::vertical(&self.scroll))
            .child(Scrollbar::horizontal(&self.scroll));

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
        moved: bool,
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
    fn reflow(&mut self, width: f32, host: &Host, wrap: &dyn Wrap) -> bool {
        let cols = columns(width, TEXT_CHROME, host.font.size, host);
        if cols == self.cols && wrap.name() == self.wrap {
            return false;
        }
        self.cols = cols;
        self.wrap = wrap.name();
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
                    old: number(l.old_no),
                    new: number(l.new_no),
                    text: l.text.into(),
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

    /// The gutters and the sign column, then the text — and for a header, the
    /// page padding and nothing else, because that is what it draws.
    fn hit(&self, index: usize, seg: usize, x: f32, host: &Host) -> Option<Hit> {
        Some(match self.rows.get(index)? {
            Row::Hunk(h) => header_hit(h, x, host),
            Row::File { path, .. } => header_hit(path, x, host),
            Row::Line { text, .. } => {
                let at = self.wrapped.range(index, seg, text);
                // Rebased into the line: a caret addresses the line, and this row
                // is one of the rows the line wrapped onto.
                let off = at.start
                    + column_at(
                        &text[at.clone()],
                        x - (TEXT_CHROME - PAD),
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

    fn render(&self, index: usize, seg: usize, host: &Host, sel: Option<Selected>) -> AnyElement {
        let theme = &host.theme;
        let p = &theme.diff;
        match &self.rows[index] {
            Row::File { path, adds, dels } => file_header(path, *adds, *dels, theme, sel),

            Row::Hunk(header) => hunk_header(header, theme, sel),

            Row::Line { kind, moved, old, new, text, spans, tokens } => {
                let (bg, fg, sign) = line_colors(*kind, *moved, p);
                let at = self.wrapped.range(index, seg, text);
                let piece = slice(text, &at);
                // A continuation carries no number and no sign. The background
                // is what says which line it belongs to, and an empty gutter is
                // what says it is not a line of its own — every real line has at
                // least one number, so there is nothing to confuse it with.
                let blank = seg > 0;
                div()
                    .flex()
                    .items_center()
                    .h(px(ROW_H))
                    .px(px(PAD))
                    .bg(rgb(bg))
                    .child(num(number_or_blank(old, blank), p.gutter_fg))
                    .child(num(number_or_blank(new, blank), p.gutter_fg))
                    .child(
                        div()
                            .flex_none()
                            .w(px(SIGN_W))
                            .text_color(rgb(fg))
                            .child(if blank { " " } else { sign }),
                    )
                    .child(
                        div().flex_none().text_color(rgb(fg)).child(
                            StyledText::new(piece).with_highlights(runs(
                                at.clone(),
                                tokens,
                                spans,
                                theme,
                                *kind,
                                *moved,
                                selected(sel, 0, text.len()),
                            )),
                        ),
                    )
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
/// The common case by far — most lines fit — and it is a refcount bump. A row
/// that *did* wrap copies its slice, which is up to a window's width of bytes on
/// each of the fifty visible rows, per frame. That is smaller than the run list
/// beside it, which is also rebuilt per row per frame and for the same reason:
/// caching either across 714k rows costs far more memory than the rows.
pub(crate) fn slice(text: &SharedString, at: &Range<usize>) -> SharedString {
    match at.start == 0 && at.end == text.len() {
        true => text.clone(),
        false => SharedString::from(text[at.clone()].to_string()),
    }
}

/// A line number, or nothing at all on a continuation row.
pub(crate) fn number_or_blank(n: &SharedString, blank: bool) -> SharedString {
    match blank {
        true => SharedString::default(),
        false => n.clone(),
    }
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
pub(crate) fn header_hit(text: &str, x: f32, host: &Host) -> Hit {
    Hit { part: 0, off: column_at(text, x - PAD, host.font.size, host) }
}

/// A header's text, with whatever a selection covers lit up behind it.
///
/// One run and not the token sweep: a header has no syntax and no changed words,
/// so the only thing that can be true of a stretch of it is that it is selected.
/// The unselected case stays a bare string child — a `StyledText` on every header
/// of every frame is a shaped line for a highlight nobody asked for.
fn header_text(text: &SharedString, sel: Range<usize>, theme: &Theme) -> AnyElement {
    if sel.is_empty() {
        return text.clone().into_any_element();
    }
    StyledText::new(text.clone())
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
pub(crate) fn file_header(
    path: &SharedString,
    adds: usize,
    dels: usize,
    theme: &Theme,
    sel: Option<Selected>,
) -> AnyElement {
    let p = &theme.diff;
    div()
        .flex()
        .items_center()
        .gap_3()
        .h(px(ROW_H))
        .px_4()
        .bg(rgb(p.file_bg))
        .child(
            div()
                .text_color(rgb(p.file_fg))
                .child(header_text(path, selected(sel, 0, path.len()), theme)),
        )
        .child(div().text_color(rgb(p.adds_fg)).child(format!("+{adds}")))
        .child(div().text_color(rgb(p.dels_fg)).child(format!("-{dels}")))
        .into_any_element()
}

pub(crate) fn hunk_header(
    header: &SharedString,
    theme: &Theme,
    sel: Option<Selected>,
) -> AnyElement {
    let p = &theme.diff;
    div()
        .flex()
        .items_center()
        .h(px(ROW_H))
        .px_4()
        .bg(rgb(p.hunk_bg))
        .text_color(rgb(p.hunk_fg))
        .child(header_text(header, selected(sel, 0, header.len()), theme))
        .into_any_element()
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

pub(crate) fn num(n: SharedString, fg: Rgb) -> Div {
    div().flex_none().w(px(GUTTER_W)).text_color(rgb(fg)).child(n)
}

/// Line numbers are drawn, so they are formatted once at load rather than on
/// every frame the row is visible.
pub(crate) fn number(n: Option<u32>) -> SharedString {
    n.map(|n| SharedString::from(n.to_string())).unwrap_or_default()
}

/// Merges three independent sets of byte ranges into the one flat, sorted,
/// non-overlapping run list `StyledText` wants: syntax tokens style the
/// foreground, intraline spans light the background of the words that changed,
/// and `sel` lights the background of whatever the mouse has selected.
///
/// All three arrive sorted and internally non-overlapping, so this is a sweep
/// over their combined edges rather than a sort.
///
/// `at` is the part of the line being drawn, which is the whole of it unless the
/// line wrapped. Tokens, spans and the selection stay in *line* coordinates
/// throughout — they belong to the line, not to one of its rows — and are clamped
/// into `at` on the way in and rebased on the way out. Clipping them into a row's
/// own vectors first is the other way to write this, and it is two allocations per
/// visible row per frame for an answer the sweep already had.
///
/// **A selection outranks a changed word.** Both are backgrounds and only one can
/// be drawn, and the reader already knows which words changed — the thing they do
/// not know, and are about to press a key about, is what is selected.
pub(crate) fn runs(
    at: Range<usize>,
    tokens: &[Token],
    spans: &[Span],
    theme: &Theme,
    kind: LineKind,
    moved: bool,
    sel: Range<usize>,
) -> Vec<(Range<usize>, HighlightStyle)> {
    // Which background each run actually lands on, so the theme can hand back a
    // foreground that reads against it. A changed word sits on a lighter
    // background than the rest of its line and needs a different answer.
    //
    // A moved line is the same text in a different place, so nothing inside it
    // changed and its spans describe a change the detection just said was not
    // one. Dropped here rather than coloured the same as the row: an invisible
    // run is still a run to merge and shape.
    let spans: &[Span] = if moved { &[] } else { spans };

    let (plain_surface, word_surface) = match (kind, moved) {
        (LineKind::Added, false) => (Surface::Added, Surface::AddedWord),
        (LineKind::Added, true) => (Surface::MovedAdded, Surface::MovedAdded),
        (LineKind::Removed, false) => (Surface::Removed, Surface::RemovedWord),
        (LineKind::Removed, true) => (Surface::MovedRemoved, Surface::MovedRemoved),
        (LineKind::Context, _) => (Surface::Context, Surface::Context),
    };
    let word_bg = theme.background(word_surface);
    let selected_bg = theme.background(Surface::Selected);
    if tokens.is_empty() && spans.is_empty() && sel.is_empty() {
        return Vec::new();
    }

    // Clamped rather than filtered: anything wholly outside this row collapses
    // to a zero-length edge pair, which `dedup` removes for free.
    let clamp = |i: usize| i.clamp(at.start, at.end);
    let sel = clamp(sel.start)..clamp(sel.end);
    let mut edges = Vec::with_capacity((tokens.len() + spans.len()) * 2 + 3);
    for t in tokens {
        edges.push(clamp(t.start));
        edges.push(clamp(t.end));
    }
    for s in spans {
        edges.push(clamp(s.start));
        edges.push(clamp(s.end));
    }
    edges.push(sel.start);
    edges.push(sel.end);
    edges.push(at.end);
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
        let on_sel = sel.contains(&cursor);
        let on_word = spans.get(si).is_some_and(|s| s.start <= cursor);
        let surface = match (on_sel, on_word) {
            (true, _) => Surface::Selected,
            (false, true) => word_surface,
            (false, false) => plain_surface,
        };
        let style =
            tokens.get(ti).filter(|t| t.start <= cursor).map(|t| theme.syntax_on(t.kind, surface));
        let bg = match (on_sel, on_word) {
            (true, _) => Some(rgb(selected_bg).into()),
            (false, true) => Some(rgb(word_bg).into()),
            (false, false) => None,
        };
        if style.is_some() || bg.is_some() {
            out.push((
                cursor - at.start..edge - at.start,
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
    use super::{line_colors, Diff, Layouts, Row, Rows, TextRows, PAD, TEXT_CHROME};
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

    fn tok(start: usize, end: usize, kind: Kind) -> Token {
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
        super::runs(at, tokens, spans, theme, kind, moved, 0..0)
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
        assert!(runs(0..12, &[], &[], &theme, LineKind::Context, false).is_empty());
    }

    #[test]
    fn a_token_and_a_span_over_the_same_bytes_split_into_both() {
        // `let` is a keyword and also a changed word: one run carrying a
        // foreground and a background, not two elements fighting over it.
        let theme = Theme::default_dark();
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
        let theme = Theme::default_dark();
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
        let out = runs(all(text), &tokens, &spans, &theme, LineKind::Removed, false);
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
            all(text),
            &[tok(0, 3, Kind::Keyword), tok(quote, text.len() - 1, Kind::Str)],
            &[Span { start: quote, end: text.len() - 1 }],
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
        let mut theme = Theme::default_dark();
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
        let theme = Theme::default_dark();
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
        let mut theme = Theme::default_dark();
        theme.diff.moved_removed_bg = 0xf2ede6;
        theme.rebuild();
        let text = "// a comment that moved";
        let tokens = [tok(0, text.len(), Kind::Comment)];
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
        let theme = Theme::default_dark();
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
        let theme = Theme::default_dark();
        let text = "# Collect every check failure before exiting";
        let out = runs(
            all(text),
            &[tok(0, text.len(), Kind::Comment)],
            &[Span { start: 10, end: text.len() }],
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
            let hit = r.hit(3, 0, x_for(col, &host), &host).expect("a hit");
            assert_eq!((hit.part, hit.off), (0, col), "column {col}");
        }
        // Past the end of the text clamps to the end of it rather than reaching
        // into whatever is at that byte of the next line.
        let hit = r.hit(3, 0, x_for(400, &host), &host).unwrap();
        assert_eq!(hit.off, text.len());
        // And to the left of the text — in the gutter — is the start of it.
        let hit = r.hit(3, 0, 0.0, &host).unwrap();
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
        let first = r.hit(row, 0, x_for(3, &host), &host).unwrap().off;
        let second = r.hit(row, 1, x_for(3, &host), &host).unwrap().off;
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
        let hit = r.hit(0, 0, PAD + 2.5 * host.font.size * host.font.advance, &host).unwrap();
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
        let theme = Theme::default_dark();
        let text = "    let x = 1;";
        let out = super::runs(
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
    fn a_selection_is_clipped_into_the_row_that_draws_it() {
        // Line coordinates in, row coordinates out — the same contract tokens
        // and spans have, and the same off-by-one available to it.
        let theme = Theme::default_dark();
        let text = "aaaa bbbb cccc";
        let first = super::runs(0..5, &[], &[], &theme, LineKind::Context, false, 2..12);
        let second = super::runs(5..text.len(), &[], &[], &theme, LineKind::Context, false, 2..12);
        assert_eq!(first.first().map(|(r, _)| r.clone()), Some(2..5));
        assert_eq!(second.first().map(|(r, _)| r.clone()), Some(0..7));
        // A selection that misses this row entirely leaves no runs at all.
        assert!(super::runs(5..14, &[], &[], &theme, LineKind::Context, false, 0..2).is_empty());
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
        // What replaces the horizontal scrollbar. `width` is what
        // `with_width_from_item` measures, so this is also the assertion that
        // nothing is left to scroll to.
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
        let theme = Theme::default_dark();
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
        let theme = Theme::default_dark();
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
