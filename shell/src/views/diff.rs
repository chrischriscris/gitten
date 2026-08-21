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
const PAD: f32 = 16.0;

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

    fn render(&self, index: usize, seg: usize, host: &Host) -> AnyElement;

    /// Width of a visual row in characters, for `uniform_list`'s one measured
    /// row.
    fn width(&self, index: usize, seg: usize) -> usize;

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
actions!(plait, [CycleLayout, CycleWrap]);

/// 8 bytes per row: which implementation owns it, where in that implementation's
/// own storage it sits, and which of that row's wrapped lines this one is. The
/// rows themselves are never boxed — at 700k rows that would be 700k allocations
/// to chase on every scroll.
///
/// `seg` fits in the two bytes `owner` and `index` left over, so wrapping cost
/// the order table nothing. It caps a line at 65,535 rows, which
/// [`MAX_LINE_CHARS`] and [`MIN_WRAP_COLS`] together put out of reach by a factor
/// of 260.
#[derive(Clone, Copy, PartialEq, Eq)]
struct RowRef {
    owner: u16,
    seg: u16,
    index: u32,
}

impl RowRef {
    /// The logical row this one is part of — what survives a reflow, and
    /// therefore what a reading position is anchored to.
    fn logical(self) -> (u16, u32) {
        (self.owner, self.index)
    }
}

/// An order table, and the two things computed while walking it.
struct Ordered {
    order: Vec<RowRef>,
    /// Widest row, for `uniform_list`'s one measured item.
    widest: usize,
    /// Where the anchor's logical row landed, so a reflow keeps your place.
    anchor: usize,
}

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
                    renderers[r.owner as usize].render(r.index as usize, r.seg as usize, &host)
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
            .child(list)
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
                .on_action(cx.listener(|this, _: &CycleWrap, _, cx| this.cycle_wrap(cx)));
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

    fn render(&self, index: usize, seg: usize, host: &Host) -> AnyElement {
        let theme = &host.theme;
        let p = &theme.diff;
        match &self.rows[index] {
            Row::File { path, adds, dels } => file_header(path, *adds, *dels, theme),

            Row::Hunk(header) => hunk_header(header, theme),

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
                            StyledText::new(piece)
                                .with_highlights(runs(at, tokens, spans, theme, *kind, *moved)),
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

/// Merges two independent sets of byte ranges into the one flat, sorted,
/// non-overlapping run list `StyledText` wants: syntax tokens style the
/// foreground, intraline spans light the background.
///
/// Both inputs are already sorted and internally non-overlapping, so this is a
/// sweep over their combined edges rather than a sort.
///
/// `at` is the part of the line being drawn, which is the whole of it unless the
/// line wrapped. Tokens and spans stay in *line* coordinates throughout — they
/// belong to the line, not to one of its rows — and are clamped into `at` on the
/// way in and rebased on the way out. Clipping them into a row's own vectors
/// first is the other way to write this, and it is two allocations per visible
/// row per frame for an answer the sweep already had.
pub(crate) fn runs(
    at: Range<usize>,
    tokens: &[Token],
    spans: &[Span],
    theme: &Theme,
    kind: LineKind,
    moved: bool,
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
    if tokens.is_empty() && spans.is_empty() {
        return Vec::new();
    }

    // Clamped rather than filtered: anything wholly outside this row collapses
    // to a zero-length edge pair, which `dedup` removes for free.
    let clamp = |i: usize| i.clamp(at.start, at.end);
    let mut edges = Vec::with_capacity((tokens.len() + spans.len()) * 2 + 1);
    for t in tokens {
        edges.push(clamp(t.start));
        edges.push(clamp(t.end));
    }
    for s in spans {
        edges.push(clamp(s.start));
        edges.push(clamp(s.end));
    }
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
        let on_word = spans.get(si).is_some_and(|s| s.start <= cursor);
        let surface = if on_word { word_surface } else { plain_surface };
        let style =
            tokens.get(ti).filter(|t| t.start <= cursor).map(|t| theme.syntax_on(t.kind, surface));
        let bg = on_word.then(|| rgb(word_bg).into());
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
    use super::{line_colors, runs, Diff, Layouts, Row, Rows, TextRows, TEXT_CHROME};
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

    /// The whole line — what every row that did not wrap asks `runs` for.
    fn all(text: &str) -> std::ops::Range<usize> {
        0..text.len()
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
        fn render(&self, index: usize, _seg: usize, _host: &Host) -> AnyElement {
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
        fn render(&self, index: usize, _seg: usize, _host: &Host) -> AnyElement {
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
