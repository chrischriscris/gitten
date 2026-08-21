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
actions!(plait, [CycleLayout]);

/// 8 bytes per row: which implementation owns it, and where in that
/// implementation's own storage it sits. The rows themselves are never boxed —
/// at 700k rows that would be 700k allocations to chase on every scroll.
#[derive(Clone, Copy)]
struct RowRef {
    owner: u16,
    index: u32,
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
    renderers: Rc<Vec<Box<dyn Rows>>>,
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
    /// First visible row, written on every batch the list asks for. Read by the
    /// session so a restart can put you back on it — see `session.rs`.
    pub top: Rc<Cell<usize>>,
    pub load: String,
}

impl Diff {
    pub fn total(&self) -> usize {
        self.order.len()
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
        Self {
            files,
            layouts: Rc::new(layouts),
            current,
            renderers: Rc::new(built.renderers),
            order: Rc::new(built.order),
            widest: built.widest,
            scroll: UniformListScrollHandle::new(),
            focus: None,
            focused: false,
            rendered: Rc::new(Cell::new(0)),
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
        self.renderers = Rc::new(built.renderers);
        self.widest = built.widest;
        self.load = built.load;
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
        "{} rows · {} files · {name} · build {:.0?} ({})",
        order.len(),
        file_count,
        t.elapsed(),
        reports.join(" · "),
    );
    eprintln!("{load}");
    Built { renderers, order, widest, load }
}

impl Render for Diff {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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

        let mut root = div()
            .relative()
            .size_full()
            .child(list)
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
                .on_action(cx.listener(|this, _: &CycleLayout, _, cx| this.cycle_layout(cx)));
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
}

impl Rows for TextRows {
    fn claims(&self, _path: &str) -> bool {
        true
    }

    fn report(&self) -> String {
        match self.moved {
            0 => String::new(),
            n => format!("{n} moved"),
        }
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

            Row::Line { kind, moved, old, new, text, spans, tokens } => {
                let (bg, fg, sign) = line_colors(*kind, *moved, p);
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
                                .with_highlights(runs(text, tokens, spans, theme, *kind, *moved)),
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
pub(crate) fn runs(
    text: &str,
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
    use super::{line_colors, runs, Diff, Layouts, Rows, TextRows};
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
        assert!(runs("nothing here", &[], &[], &theme, LineKind::Context, false).is_empty());
    }

    #[test]
    fn a_token_and_a_span_over_the_same_bytes_split_into_both() {
        // `let` is a keyword and also a changed word: one run carrying a
        // foreground and a background, not two elements fighting over it.
        let theme = Theme::default_dark();
        let text = "let x = 1;";
        let out =
            runs(text, &[tok(0, 3, Kind::Keyword)], &[Span { start: 0, end: 3 }], &theme, LineKind::Added, false);
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
            runs(text, &[tok(0, 3, Kind::Keyword)], &[Span { start: 2, end: 7 }], &theme, LineKind::Added, false);
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
        let out = runs(text, &tokens, &spans, &theme, LineKind::Removed, false);
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
            text,
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
        let plain = runs(text, &tokens, &[], &theme, LineKind::Removed, false);
        let moved = runs(text, &tokens, &[], &theme, LineKind::Removed, true);
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
        let out = runs(text, &[], &spans, &theme, LineKind::Added, true);
        let unmoved = runs(text, &[], &spans, &theme, LineKind::Added, false);
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
            text,
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
        fn width(&self, index: usize) -> usize {
            self.rows[index].len()
        }
        fn render(&self, index: usize, _host: &Host) -> AnyElement {
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
                let _ = diff.renderers[r.owner as usize].width(r.index as usize);
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
