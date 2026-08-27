//! The diff view: a viewport over the order table, and the state a keyboard
//! moves.
//!
//! This is `shell/src/views/diff.rs`'s `Diff` with GPUI taken out, and it turns
//! out to be much smaller — because `uniform_list` is replaced by a `for` over
//! the visible rows, and because the pipeline it used to drive now lives in
//! [`gitten_core::rows`].
//!
//! # No events in here
//!
//! Every method is a *command*: [`Diff::down`], [`Diff::page`],
//! [`Diff::jump_file`], [`Diff::cycle_layout`]. Nothing in this file knows what
//! a keypress is, and the keymap is not here — because a keymap written here
//! would be one `cli/` would have to duplicate. `main.rs` resolves a key to a
//! command name through `core::command` and calls the method it names.
//!
//! The mouse is the same rule with one more step. [`Diff::press`],
//! [`Diff::drag`] and [`Diff::release`] take a column and a row of the *body*
//! and no event type at all: the assembly owns the title bar and the status
//! line, so it subtracts them, and what arrives here is already a place in this
//! view. Below that, which text and which byte is the presentation's — see
//! [`crate::rows::Rows::hit`] — and everything a selection *means* is
//! [`gitten_core::select`], shared with the window.
//!
//! # Where it is scrolled to is not this file's
//!
//! [`gitten_core::view::Viewport`] holds the cursor and the top row and the rule
//! relating them, because the commit list needs exactly the same pair and had
//! its own copy. What is left here is the part that is genuinely a *diff*: the
//! order table the rows are counted from, and the anchor a reflow keeps — the
//! *line* the cursor is on, which exists at any width when row 4,102 does not.
//!
//! # What a resize costs
//!
//! A rescan of the text and a new order table, and nothing above them: no clip,
//! no intraline pass, no highlighting. On the 714k-line fixture the shell
//! measures that at 26 ms against 241 ms for a full `prepare`, which is why a
//! width change re-expands and a *layout* change rebuilds.

use crate::rows::{assemble, Frame, Layouts, Rows};
use crate::screen::{Ink, Screen};
use crate::scrollbar::{self, Bar};
use gitten_core::host::Host;
use gitten_core::rows::{expand, RowRef};
use gitten_core::runs::Run;
use gitten_core::select::{self, Caret, RowId, Selection, Text as _};
use gitten_core::view::Viewport;
use gitten_core::FileDiff;
use std::time::Duration;

pub struct Diff {
    /// The parsed diff, kept so a layout change can rebuild the rows.
    ///
    /// The memory cost of a live toggle, and a real one: on the 714k-line
    /// fixture it is a second copy of every line. The alternatives are worse —
    /// cloning the *prepared* diff pays the same memory plus the clone at load
    /// whether or not anybody presses the key, and re-acquiring means the view
    /// needs a repository, which it does not have and should not.
    files: Vec<FileDiff>,
    layouts: Layouts,
    current: usize,
    /// Which entry of `host.wrap` is in use.
    ///
    /// The view's own pick, not the host's: `Host` is rebuilt from defaults
    /// whenever a config file is reloaded, so a field on it would be reset by an
    /// unrelated edit. What the file says is what this *opens* on.
    wrap: usize,
    owners: Vec<Box<dyn Rows>>,
    /// One entry per visual row. Re-expanded on a resize and on a wrap change,
    /// rebuilt from scratch on a layout change.
    order: Vec<RowRef>,
    /// Index into `order` of the widest row, which is what a horizontal scroll
    /// is bounded by.
    widest: usize,
    /// Where each file's header landed in `order`, ascending.
    ///
    /// Cached because it moves only when the order table does, and finding it on
    /// demand is a scan of `order` per file: 5,953 files against a million rows
    /// is six billion comparisons for a keypress. One scan per reflow instead.
    headers: Vec<usize>,
    /// The width and wrap the rows were last expanded for. A resize that does
    /// not cross a column boundary compares equal here and stops.
    applied: (usize, &'static str),
    cols: usize,
    /// The cursor, the top row and the height, and nothing else about them.
    view: Viewport,
    /// Columns of text scrolled off the left edge. Only reachable with wrapping
    /// off, because with it on there is nothing off the edge to reach.
    shift: usize,
    intraline: Duration,
    syntax: Duration,
    file_count: usize,
    /// What the mouse has selected, or nothing.
    ///
    /// The model is [`gitten_core::select`] and this is the only state this view
    /// keeps for it: which rows are covered, what a wrapped line copies and where
    /// a word ends are all answers about a *diff* and are `core`'s, shared with
    /// the window.
    sel: Option<Selection>,
    /// True between a press and a release on the text, so a motion event that
    /// belongs to nothing does not extend a selection made minutes ago.
    dragging: bool,
    /// Where in the scrollbar's thumb it was taken hold of, while it is held.
    grabbed: Option<usize>,
    bar: Bar,
}

/// Where every row's selectable text comes from, for [`gitten_core::select`].
///
/// A wrapper rather than an impl on the vector, because the trait and the vector
/// both belong to somebody else. Three lines is what this seam costs — the same
/// three the window pays.
struct Selectable<'a>(&'a [Box<dyn Rows>]);

impl select::Text for Selectable<'_> {
    fn text(&self, row: RowId, part: u16) -> Option<&str> {
        self.0.get(row.0 as usize)?.selectable(row.1 as usize, part)
    }
}

impl Diff {
    pub fn new(files: Vec<FileDiff>, host: &Host) -> Self {
        Self::with_layouts(files, host, Layouts::builtin())
    }

    /// The constructor an extension uses: its own registry, with the built-ins
    /// still in it unless it replaced them by name.
    pub fn with_layouts(files: Vec<FileDiff>, host: &Host, layouts: Layouts) -> Self {
        // Reported here and not by a config layer, because this is the layer
        // that holds the registry: `core` cannot see a `Rows` implementation and
        // an extension may have registered a name nothing else has heard of.
        // Falling back rather than failing — a typo must not leave you with no
        // diff.
        let current = match layouts.position(&host.layout) {
            Some(i) => i,
            None => {
                eprintln!(
                    "gitten: unknown diff.layout {:?}; registered: {}",
                    host.layout,
                    layouts.names().join(", ")
                );
                0
            }
        };
        let mut view = Self {
            files,
            layouts,
            current,
            wrap: host.wrap.selected_index(),
            owners: Vec::new(),
            order: Vec::new(),
            widest: 0,
            headers: Vec::new(),
            applied: (usize::MAX, ""),
            cols: 0,
            view: Viewport::new(),
            shift: 0,
            intraline: Duration::ZERO,
            syntax: Duration::ZERO,
            file_count: 0,
            sel: None,
            dragging: false,
            grabbed: None,
            bar: Bar::default(),
        };
        view.rebuild(host, 0.0);
        view
    }

    /// Rebuilds the rows from the parsed diff, keeping your place proportionally.
    ///
    /// Proportionally and not by row, because a layout change has no row
    /// correspondence to keep: side-by-side puts a removal and its replacement
    /// on one row, so row 900 of one presentation is not row 900 of the other.
    /// A reflow *does* have that correspondence, which is why it anchors on the
    /// cursor's logical row instead.
    fn rebuild(&mut self, host: &Host, at: f32) {
        // The rows are about to be somebody else's, so a selection anchored to
        // one of them has nowhere to go: `resolve` would say so and there is no
        // honest repair, because a layout change has no row correspondence.
        self.sel = None;
        self.owners = self.layouts.build(self.current, host);
        let built = assemble(&self.files, host, &mut self.owners);
        self.order = built.ordered.order;
        self.widest = built.ordered.widest;
        self.index_headers();
        self.file_count = built.files;
        self.intraline = built.intraline;
        self.syntax = built.syntax;
        // The width has to be re-applied: the presentations are new objects and
        // have never been told how wide they are.
        self.applied = (usize::MAX, "");
        self.view.set_len(self.order.len());
        self.view.go_to_fraction(at);
        self.reflow(host);
    }

    // ------------------------------------------------------------------ layout

    /// A new size, in columns and rows. Call it before [`Diff::paint`] on any
    /// frame the terminal may have changed size on; it is two comparisons when
    /// nothing did.
    pub fn resize(&mut self, cols: usize, height: usize, host: &Host) {
        self.cols = cols;
        self.view.set_height(height);
        self.reflow(host);
    }

    /// Re-expands the rows for the current width and wrap, keeping the line the
    /// cursor is on.
    ///
    /// Three ways out before any work, in increasing cost: nothing moved,
    /// nothing *can* move because the wrap never breaks, and no presentation's
    /// row count actually changed.
    fn reflow(&mut self, host: &Host) {
        let wrap = host.wrap.at(self.wrap);
        if (self.cols, wrap.name()) == self.applied || self.cols == 0 {
            return;
        }
        let same_wrap = self.applied.1 == wrap.name();
        self.applied = (self.cols, wrap.name());
        // A wrap that never breaks has no width to be wrong about. Without this,
        // every column of a drag rescans the whole diff to be told nothing moved.
        if !wrap.breaks_lines() && same_wrap {
            return;
        }

        let cols = self.cols;
        let changed = self
            .owners
            .iter_mut()
            .fold(false, |acc, o| o.reflow(cols, host, wrap) | acc);
        if !changed {
            return;
        }
        let anchor = self.order.get(self.view.cursor()).copied();
        let built = expand(&self.order, &self.owners, anchor);
        self.order = built.order;
        self.widest = built.widest;
        self.index_headers();
        self.view.set_len(self.order.len());
        self.view.go_to(built.anchor);
        // Every caret caches the visual rows its line occupies, and a reflow
        // moved all of them. Dropped rather than repaired when the row is gone.
        if self.sel.as_mut().is_some_and(|s| !s.resolve(&self.order)) {
            self.sel = None;
        }
        // A wrapped diff has nothing off the left edge to reach.
        if wrap.breaks_lines() {
            self.shift = 0;
        }
    }

    /// One pass over the order table, marking the rows that are file headers.
    ///
    /// A binary search per row against that owner's own header list, which is
    /// sorted because [`gitten_core::rows::assemble`] builds it in file order.
    /// Only the first visual row of a logical one can be a header, so a wrapped
    /// diff costs no more than an unwrapped one.
    fn index_headers(&mut self) {
        let per_owner: Vec<Vec<u32>> = self
            .owners
            .iter()
            .map(|o| o.files().iter().map(|f| f.row as u32).collect())
            .collect();
        self.headers.clear();
        for (at, r) in self.order.iter().enumerate() {
            if r.seg != 0 {
                continue;
            }
            let Some(rows) = per_owner.get(r.owner as usize) else {
                continue;
            };
            if rows.binary_search(&r.index).is_ok() {
                self.headers.push(at);
            }
        }
    }

    // ----------------------------------------------------------------- commands

    pub fn rows(&self) -> usize {
        self.order.len()
    }

    pub fn cursor(&self) -> usize {
        self.view.cursor()
    }

    pub fn top(&self) -> usize {
        self.view.top()
    }

    pub fn height(&self) -> usize {
        self.view.height()
    }

    pub fn shift(&self) -> usize {
        self.shift
    }

    pub fn move_by(&mut self, by: isize) {
        self.view.move_by(by);
    }

    pub fn down(&mut self) {
        self.view.down();
    }

    pub fn up(&mut self) {
        self.view.up();
    }

    pub fn page(&mut self, pages: isize) {
        self.view.page(pages);
    }

    /// Scrolls without moving the cursor further than it has to go. The wheel.
    pub fn scroll_y(&mut self, by: isize) {
        self.view.scroll_by(by);
    }

    /// How much lead the cursor keeps at the edge. `[view] scrolloff`.
    pub fn set_scrolloff(&mut self, rows: usize) {
        self.view.set_scrolloff(rows);
    }

    pub fn to_top(&mut self) {
        self.view.to_top();
    }

    pub fn to_bottom(&mut self) {
        self.view.to_bottom();
    }

    /// Moves the cursor to the header of the next or previous file.
    ///
    /// The jump list is [`gitten_core::rows::Present::files`], so a presentation
    /// that is not file-shaped offers no jumps rather than wrong ones — and this
    /// works for a presentation registered by an extension without it doing
    /// anything but hold a [`gitten_core::rows::Flat`].
    pub fn jump_file(&mut self, by: isize) {
        // Binary search rather than a scan: a 5,953-file diff is a realistic
        // input and this is a keypress.
        let cursor = self.view.cursor();
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
            self.view.go_to(t);
        }
    }

    /// Where every file header is, in visual rows. What a sidebar or a jump
    /// picker lists.
    pub fn headers(&self) -> &[usize] {
        &self.headers
    }

    /// Scrolls sideways. A no-op with wrapping on, where nothing is off the edge.
    pub fn scroll_x(&mut self, by: isize) {
        let bound = self.order.get(self.widest).map_or(0, |r| {
            self.owners[r.owner as usize].width(r.index as usize, r.seg as usize)
        });
        self.shift = match by.is_negative() {
            true => self.shift.saturating_sub(by.unsigned_abs()),
            false => (self.shift + by as usize).min(bound),
        };
    }

    // ---------------------------------------------------------------- the mouse

    /// The glyphs the scrollbar is drawn with. `--ascii`, or an extension.
    pub fn set_bar(&mut self, bar: Bar) {
        self.bar = bar;
    }

    /// Which row and which byte of it a cell of the body is over.
    ///
    /// The one piece of a selection that is neither `core`'s nor a
    /// presentation's: `core` does not know how tall the body is and a
    /// presentation does not know where it starts. Everything below that — which
    /// text, which byte — is [`Rows::hit`], because only the presentation knows
    /// what it drew in front of its text.
    fn locate(&self, col: usize, visual: usize) -> Option<(u16, Caret)> {
        let r = *self.order.get(visual)?;
        let rows = self.owners.get(r.owner as usize)?;
        let hit = rows.hit(r.index as usize, r.seg as usize, col, self.shift)?;
        // The visual rows this logical row occupies. The caret caches them so the
        // render path never searches the order table, and they are free here:
        // this row is `seg` into the run and the presentation knows how long the
        // run is.
        let first = visual - r.seg as usize;
        let n = rows.rows(r.index as usize).max(1);
        Some((
            hit.part,
            Caret {
                row: r.logical(),
                off: hit.off,
                at: first..first + n,
            },
        ))
    }

    /// A caret for the *free* end of a drag, which is one character further on
    /// than the caret for a click.
    ///
    /// A cell has no right half to round away from, so pressing on a character
    /// and dragging right would otherwise select up to but not including the
    /// character under the pointer — off by one, all the way along, and visible.
    /// Backwards it is the other way round, so the direction is asked first.
    fn head(&self, col: usize, visual: usize) -> Option<(u16, Caret)> {
        let (part, caret) = self.locate(col, visual)?;
        let anchor = self.sel.as_ref()?.anchor();
        match (caret.at.start, caret.off) >= (anchor.at.start, anchor.off) {
            true => self.locate(col + 1, visual).or(Some((part, caret))),
            false => Some((part, caret)),
        }
    }

    /// A press in the body: the cursor moves there, and a selection starts.
    ///
    /// `row` is a row of the body, not of the terminal — whoever assembles the
    /// screen owns the title bar and the status line and subtracts them. `clicks`
    /// is 2 for a word and 3 for a whole row; `extend` is shift, which moves the
    /// free end of what is already selected rather than starting again.
    ///
    /// A press on nothing selectable *clears*, which is the whole reason a fresh
    /// [`Selection`] is empty until something extends it: a click has to be able
    /// to mean "no longer selected".
    pub fn press(&mut self, col: usize, row: usize, clicks: u8, extend: bool, host: &Host) {
        if scrollbar::hit(col, self.cols, &self.view, host) {
            let row = row.min(self.view.height().saturating_sub(1));
            self.grabbed = Some(scrollbar::grab(&mut self.view, host, row));
            return;
        }
        let Some(visual) = self.view.row_at(row) else {
            self.sel = None;
            return;
        };
        // The cursor follows the mouse: a click is a place, and everything a key
        // does next — copy, jump, open — acts on the row the cursor is on.
        self.view.go_to(visual);
        let Some((part, caret)) = self.locate(col, visual) else {
            self.sel = None;
            return;
        };
        self.dragging = true;
        let extend = extend && self.sel.as_ref().is_some_and(|s| s.part() == part);
        // Before the selection is taken out of `self`: the free end of a shift
        // click is one character further on than the caret, and which way that
        // is depends on the anchor that is still in there.
        let head = extend
            .then(|| self.head(col, visual))
            .flatten()
            .map(|(_, c)| c);
        self.sel = match (extend, clicks) {
            (true, _) => {
                let mut sel = self.sel.take().expect("extend implies a selection");
                sel.extend(head.unwrap_or(caret));
                Some(sel)
            }
            // Two clicks take the word under the caret, three take the row. What
            // a word is made of is `core`'s: a terminal and a window must not
            // disagree about what `foo(bar,` is.
            (_, 2) => {
                let text = self.row_text(caret.row, part).unwrap_or_default();
                Some(span(part, &caret, select::word_at(&text, caret.off)))
            }
            (_, n) if n >= 3 => {
                let len = self.row_text(caret.row, part).map_or(0, |t| t.len());
                Some(span(part, &caret, 0..len))
            }
            _ => Some(Selection::new(part, caret)),
        };
    }

    /// The pointer moved with the button down: the free end follows it.
    ///
    /// `row` is signed because a drag does not stop at the edge of the body — a
    /// row above it scrolls up by that much and keeps selecting, which is what
    /// dragging past the top of a page does everywhere else. Deliberately not a
    /// clock: holding the pointer still outside the body does not keep scrolling,
    /// because that needs a timer and this needs nothing.
    ///
    /// The **anchor's** part wins. A drag that crosses the divider of a
    /// side-by-side diff stays in the column it started in and runs to that
    /// column's edge, because the alternative is a paste with the old and the new
    /// file interleaved.
    pub fn drag(&mut self, col: usize, row: isize, host: &Host) {
        if let Some(grabbed) = self.grabbed {
            scrollbar::drag(&mut self.view, host, row.max(0) as usize, grabbed);
            return;
        }
        if !self.dragging {
            return;
        }
        let height = self.view.height() as isize;
        let row = match row {
            r if r < 0 => {
                self.view.scroll_by(r);
                0
            }
            r if r >= height => {
                self.view.scroll_by(r - height + 1);
                height.saturating_sub(1).max(0)
            }
            r => r,
        };
        let Some(visual) = self.view.row_at(row as usize) else {
            return;
        };
        let Some((part, mut caret)) = self.head(col, visual) else {
            return;
        };
        let Some(sel) = &self.sel else { return };
        if part != sel.part() {
            // Parts are laid out left to right, so a part further along than the
            // anchor's means the pointer is past the end of the anchor's text.
            caret.off = match part > sel.part() {
                true => self.row_text(caret.row, sel.part()).map_or(0, |t| t.len()),
                false => 0,
            };
        }
        if let Some(sel) = &mut self.sel {
            sel.extend(caret);
        }
    }

    /// The button came up, wherever it came up. Ends a drag and lets go of the
    /// scrollbar; the selection itself stays, because that is what it is for.
    pub fn release(&mut self) {
        self.dragging = false;
        self.grabbed = None;
    }

    /// The text of one row, for a word or a whole-row selection.
    fn row_text(&self, row: RowId, part: u16) -> Option<String> {
        Selectable(&self.owners).text(row, part).map(str::to_string)
    }

    /// Whatever the mouse is holding, as text. Empty when nothing is selected.
    pub fn selection(&self) -> String {
        match &self.sel {
            Some(sel) => sel.text(&self.order, &Selectable(&self.owners)),
            None => String::new(),
        }
    }

    /// What `copy.selection` copies: the selection, or the row the cursor is on
    /// when there is none.
    ///
    /// The fallback is the point. A keyboard-first client where the copy key does
    /// nothing until you have used the mouse is a client that does not copy, and
    /// "this line" is the only other thing the key could sensibly mean.
    pub fn copy_text(&self) -> String {
        let text = self.selection();
        if !text.is_empty() {
            return text;
        }
        let src = Selectable(&self.owners);
        self.order
            .get(self.view.cursor())
            .and_then(|r| src.text(r.logical(), 0))
            .unwrap_or_default()
            .to_string()
    }

    /// `select.all`.
    pub fn select_all(&mut self) {
        self.sel = Selection::all(&self.order);
    }

    /// `select.none`. Says whether there was anything to drop, so `esc` can fall
    /// through to whatever it means next.
    pub fn select_none(&mut self) -> bool {
        self.sel.take().is_some()
    }

    // ------------------------------------------------------------- the registries

    pub fn layout_names(&self) -> Vec<&'static str> {
        self.layouts.names()
    }

    pub fn layout_index(&self) -> usize {
        self.current
    }

    pub fn layout_name(&self) -> &'static str {
        self.layouts.name(self.current)
    }

    /// Loads a layout by index. Out of range is ignored rather than clamped: a
    /// stale index means a menu moved, and redrawing the diff the way it is
    /// already drawn surprises nobody.
    pub fn set_layout(&mut self, index: usize, host: &Host) {
        if index >= self.layouts.len() || index == self.current {
            return;
        }
        let at = self.progress();
        self.current = index;
        self.rebuild(host, at);
    }

    pub fn cycle_layout(&mut self, host: &Host) {
        if self.layouts.len() < 2 {
            return;
        }
        self.set_layout((self.current + 1) % self.layouts.len(), host);
    }

    pub fn wrap_names(&self, host: &Host) -> Vec<&'static str> {
        host.wrap.names()
    }

    pub fn wrap_index(&self) -> usize {
        self.wrap
    }

    pub fn set_wrap(&mut self, index: usize, host: &Host) {
        if index >= host.wrap.len() || index == self.wrap {
            return;
        }
        self.wrap = index;
        self.reflow(host);
    }

    /// Moves to the next wrap.
    ///
    /// Unlike a layout change this rebuilds nothing above the break table: the
    /// lines, their tokens and their spans are the same objects, and only where
    /// they break moves. That is why it is a keystroke and the algorithm is a
    /// menu.
    pub fn cycle_wrap(&mut self, host: &Host) {
        if host.wrap.len() < 2 {
            return;
        }
        self.set_wrap((self.wrap + 1) % host.wrap.len(), host);
    }

    // ------------------------------------------------------------------ writes

    /// The hunk under the keyboard, as the loaded diff holds it: its file's
    /// path and the [`Hunk`] itself, with every line and both sides' numbers —
    /// exactly what [`gitten_core::patch::emit`] aims a patch with. `None` when
    /// the keyboard sits on a file header, on an empty diff, or on a
    /// presentation whose hunk map answers nothing for the row — which is the
    /// honest answer for a presentation that draws no hunks. A span claiming a
    /// row it does not own is trusted as claimed, the window's own hunk map
    /// running on the same contract; presentation authors record exact spans.
    ///
    /// The address crosses two lists. The presentation answers `(file, hunk)`
    /// in its own file order, which its own [`Present::files`] spells; that
    /// file's path is the key the loaded diff was acquired under, so the hunk
    /// handed over is the one the diff drew — however many presentations
    /// claimed their share of the files on the way to the screen.
    pub fn current_hunk(&self) -> Option<(String, gitten_core::Hunk)> {
        let r = *self.order.get(self.view.cursor())?;
        let rows = self.owners.get(r.owner as usize)?;
        let (file, hunk) = rows.hunk_at(r.index as usize)?;
        let path = rows.files().get(file)?.path.clone();
        let loaded = self.files.iter().find(|f| f.path == path)?;
        Some((path, loaded.hunks.get(hunk)?.clone()))
    }

    /// Swaps in a refreshed diff, keeping the reading position numerically.
    ///
    /// A write leaves no hunk identity to anchor to — the hunk just staged may
    /// be gone, and nothing honest can be said about where it went — so the
    /// cursor and the top row are kept as numbers and clamped into whatever the
    /// new diff can hold. That is the window's fallback, and it is deliberately
    /// not a fuzzy match: landing on adjacent context is predictable, landing
    /// on a guess is not. Layout and wrap are untouched; the presentation is
    /// rebuilt in place, because the refreshed diff has never met either.
    pub fn replace(&mut self, files: Vec<FileDiff>, host: &Host) {
        if self.files == files {
            return;
        }
        let (cursor, top, shift) = (self.view.cursor(), self.view.top(), self.shift);
        // A refresh is the repository saying things moved: a selection was
        // anchored to how they were, a drag was holding rows that may not be
        // there, and the scrollbar was held against a thumb that just moved.
        self.sel = None;
        self.dragging = false;
        self.grabbed = None;
        self.files = files;
        self.owners = self.layouts.build(self.current, host);
        let built = assemble(&self.files, host, &mut self.owners);
        self.order = built.ordered.order;
        self.widest = built.ordered.widest;
        self.index_headers();
        self.file_count = built.files;
        self.intraline = built.intraline;
        self.syntax = built.syntax;
        // The presentations are new objects and have never been told the
        // width; the next reflow re-applies it.
        self.applied = (usize::MAX, "");
        self.view.set_len(self.order.len());
        self.reflow(host);
        // The numeric fallback: the same rows if they still exist, clamped
        // where they do not — `Viewport` does the clamping, and nothing here
        // assigns an index the row count has not blessed.
        self.view.go_to(cursor);
        self.view.scroll_to(top);
        // The horizontal shift survives up to the refreshed bound. With
        // wrapping on there is nothing off the edge to be shifted to, and
        // `reflow` has already zeroed it — restoring a number there would
        // scroll a diff that cannot scroll.
        if !host.wrap.at(self.wrap).breaks_lines() {
            let bound = self.order.get(self.widest).map_or(0, |r| {
                self.owners[r.owner as usize].width(r.index as usize, r.seg as usize)
            });
            self.shift = shift.min(bound);
        }
    }

    fn progress(&self) -> f32 {
        self.view.progress()
    }

    // ------------------------------------------------------------------ drawing

    /// Draws the visible rows into `screen`, starting at row `y`.
    ///
    /// Only the visible rows are ever built or measured, which is what
    /// `uniform_list` does for the window and what a `for` over a range does
    /// here. `out` is the run-list scratch buffer, owned by the caller across
    /// frames so that drawing a row allocates nothing.
    pub fn paint(&self, screen: &mut Screen, y: usize, host: &Host, out: &mut Vec<Run>) {
        let blank = Ink::new(host.theme.chrome.dim, host.theme.chrome.bg);
        for i in 0..self.view.height() {
            let row = y + i;
            match self
                .view
                .row_at(i)
                .and_then(|n| self.order.get(n).map(|r| (n, *r)))
            {
                Some((n, r)) => {
                    let at = Frame {
                        host,
                        shift: self.shift,
                        current: n == self.view.cursor(),
                        // Two integer comparisons per visible row, and no search
                        // of the order table: see `gitten_core::select::Caret::at`.
                        sel: self.sel.as_ref().and_then(|s| s.at(n, r.logical())),
                    };
                    let mut pen = screen.row(row);
                    self.owners[r.owner as usize].render(
                        r.index as usize,
                        r.seg as usize,
                        &at,
                        &mut pen,
                        out,
                    );
                }
                // Past the end of a diff shorter than the screen. Washed in the
                // chrome's background rather than left as whatever the last
                // frame drew there.
                None => screen.row(row).wash(blank),
            }
        }
        // Last, and over the rows rather than beside them: a row's colour still
        // runs to the right edge underneath it.
        scrollbar::paint(
            screen,
            self.bar,
            self.cols.saturating_sub(1),
            y,
            &self.view,
            host,
        );
    }

    /// One line describing what is on screen, for whatever draws a status bar.
    ///
    /// Assembled here because every number in it belongs to this view, and
    /// because a frontend asking six accessors and formatting them is six
    /// chances to describe a different frame than the one that was drawn.
    pub fn status(&self, host: &Host) -> String {
        let mut out = format!(
            "{}/{} · {} files · {} · {}",
            (self.view.cursor() + 1).min(self.rows()),
            self.rows(),
            self.file_count,
            self.layout_name(),
            host.wrap.at(self.wrap).name(),
        );
        if self.shift > 0 {
            out.push_str(&format!(" · +{}c", self.shift));
        }
        for report in self
            .owners
            .iter()
            .map(|o| o.report())
            .filter(|r| !r.is_empty())
        {
            out.push_str(" · ");
            out.push_str(&report);
        }
        out
    }

    /// What the two expensive passes cost, for a stats line. Measured once at
    /// load by `core`, not timed again here — and **CPU time summed across
    /// `prepare`'s workers**, not wall clock, so it does not add up to how long
    /// the load took. See `gitten_core::prepared::Prepared::intraline`.
    pub fn timings(&self) -> (Duration, Duration) {
        (self.intraline, self.syntax)
    }
}

/// A selection over one byte range of one row: what a double or a triple click
/// makes.
fn span(part: u16, at: &Caret, bytes: std::ops::Range<usize>) -> Selection {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rows::TextRows;
    use gitten_core::parse_unified_diff;
    use gitten_core::prepared::File;
    use gitten_core::rows::Present;

    fn diff(lines: usize) -> Vec<FileDiff> {
        let mut raw = String::from("diff --git a/a.rs b/a.rs\n@@ -1,1 +1,1 @@\n");
        for i in 0..lines {
            raw.push_str(&format!(" line {i}\n"));
        }
        parse_unified_diff(&raw)
    }

    fn two_files() -> Vec<FileDiff> {
        let mut raw = String::new();
        for name in ["a.rs", "b.rs", "c.rs"] {
            raw.push_str(&format!(
                "diff --git a/{name} b/{name}\n@@ -1,2 +1,2 @@\n-one\n+two\n"
            ));
        }
        parse_unified_diff(&raw)
    }

    fn view(files: Vec<FileDiff>, cols: usize, height: usize) -> (Diff, Host) {
        let host = Host::new();
        let mut d = Diff::new(files, &host);
        d.resize(cols, height, &host);
        (d, host)
    }

    #[test]
    fn the_cursor_clamps_rather_than_wrapping_at_both_ends() {
        let (mut d, _) = view(diff(10), 60, 6);
        d.up();
        assert_eq!(d.cursor(), 0);
        d.move_by(9999);
        assert_eq!(d.cursor(), d.rows() - 1);
    }

    #[test]
    fn the_viewport_follows_the_cursor_and_keeps_a_margin() {
        let (mut d, _) = view(diff(40), 60, 12);
        assert_eq!(d.top(), 0);
        // Down to just inside the margin: nothing has scrolled yet.
        d.move_by(8);
        assert_eq!(d.top(), 0);
        d.down();
        assert_eq!(d.top(), 1, "the margin did not push the viewport");
        d.to_bottom();
        assert_eq!(d.top(), d.rows() - 12, "scrolled past the end");
    }

    #[test]
    fn a_screen_too_short_for_a_margin_drops_it_rather_than_pinning_the_cursor() {
        let (mut d, _) = view(diff(40), 60, 4);
        d.down();
        assert_eq!(d.top(), 0);
        d.move_by(2);
        assert_eq!(d.cursor(), 3);
        assert_eq!(
            d.top(),
            0,
            "a four-row screen scrolled on the first keypress"
        );
    }

    #[test]
    fn a_diff_shorter_than_the_screen_never_scrolls() {
        let (mut d, _) = view(diff(3), 60, 40);
        d.to_bottom();
        assert_eq!(d.top(), 0);
    }

    #[test]
    fn a_page_keeps_one_row_of_overlap() {
        let (mut d, _) = view(diff(100), 60, 20);
        d.page(1);
        assert_eq!(d.cursor(), 19);
        d.page(-1);
        assert_eq!(d.cursor(), 0);
    }

    #[test]
    fn a_file_jump_lands_on_a_header_and_stops_at_the_ends() {
        let (mut d, _) = view(two_files(), 60, 20);
        assert_eq!(d.cursor(), 0);
        d.jump_file(1);
        let second = d.cursor();
        assert!(second > 0);
        d.jump_file(1);
        assert!(d.cursor() > second);
        let last = d.cursor();
        d.jump_file(1);
        assert_eq!(d.cursor(), last, "jumped past the last file");
        d.jump_file(-1);
        assert_eq!(d.cursor(), second);
    }

    #[test]
    fn a_file_jump_is_a_search_and_not_a_scan_of_the_whole_diff() {
        // 200 files is not the interesting number; 200 × the row count is. This
        // is the shape of the input that made the naive version quadratic.
        let mut raw = String::new();
        for i in 0..200 {
            raw.push_str(&format!(
                "diff --git a/f{i}.rs b/f{i}.rs\n@@ -1,20 +1,20 @@\n{}",
                (0..20).map(|l| format!(" line {l}\n")).collect::<String>()
            ));
        }
        let (mut d, _) = view(parse_unified_diff(&raw), 60, 20);
        assert_eq!(d.headers().len(), 200);
        // Forward through every file, then back through every file.
        let mut seen = vec![d.cursor()];
        for _ in 0..199 {
            d.jump_file(1);
            seen.push(d.cursor());
        }
        assert_eq!(seen, *d.headers(), "a jump landed off a header");
        d.jump_file(1);
        assert_eq!(
            d.cursor(),
            *d.headers().last().unwrap(),
            "jumped past the last file"
        );
        for _ in 0..199 {
            d.jump_file(-1);
        }
        assert_eq!(d.cursor(), 0);
        d.jump_file(-1);
        assert_eq!(d.cursor(), 0, "jumped above the first file");
    }

    #[test]
    fn a_jump_from_the_middle_of_a_file_goes_to_that_files_header() {
        let (mut d, _) = view(two_files(), 60, 20);
        d.jump_file(1);
        let second = d.cursor();
        d.move_by(2);
        d.jump_file(-1);
        assert_eq!(
            d.cursor(),
            second,
            "landed on the previous file, not this one"
        );
    }

    #[test]
    fn a_presentation_with_no_jump_list_offers_no_jumps() {
        // Defaulted on the trait, so this is what an extension gets for free
        // rather than a special case in the view.
        #[derive(Default)]
        struct Bare(usize);
        impl Present for Bare {
            fn claims(&self, _: &str) -> bool {
                true
            }
            fn len(&self) -> usize {
                self.0
            }
            fn build(&mut self, f: File) {
                self.0 += 1 + f.hunks.iter().map(|h| 1 + h.lines.len()).sum::<usize>();
            }
        }
        impl Rows for Bare {
            fn render(
                &self,
                _: usize,
                _: usize,
                _: &Frame,
                _: &mut crate::screen::Pen,
                _: &mut Vec<Run>,
            ) {
            }
        }
        let host = Host::new();
        let mut layouts = Layouts::builtin();
        layouts.register("bare", |_| vec![Box::new(Bare::default())]);
        let mut d = Diff::with_layouts(two_files(), &host, layouts);
        d.set_layout(2, &host);
        d.resize(60, 20, &host);
        d.jump_file(1);
        assert_eq!(d.cursor(), 0, "a jump list appeared out of nowhere");
    }

    #[test]
    fn a_reflow_keeps_the_line_the_cursor_is_on() {
        let long = format!(
            "diff --git a/a.rs b/a.rs\n@@ -1,1 +1,1 @@\n{}",
            (0..12)
                .map(|i| format!(" line {i} {}\n", "padding ".repeat(6)))
                .collect::<String>()
        );
        let (mut d, host) = view(parse_unified_diff(&long), 100, 20);
        d.move_by(8);
        let before = d.order[d.cursor()].logical();
        d.resize(30, 20, &host);
        assert_eq!(
            d.order[d.cursor()].logical(),
            before,
            "the cursor left its line"
        );
        assert_eq!(d.order[d.cursor()].seg, 0, "it landed mid-line");
        assert!(d.rows() > 14, "nothing wrapped at 30 columns");
    }

    #[test]
    fn a_resize_that_changes_nothing_costs_nothing() {
        let (mut d, host) = view(diff(20), 60, 10);
        let before = d.rows();
        d.resize(60, 10, &host);
        assert_eq!(d.rows(), before);
    }

    #[test]
    fn switching_layout_keeps_your_place_proportionally() {
        // A layout change has no row correspondence to keep: side-by-side puts a
        // removal and its replacement on one row.
        let (mut d, host) = view(two_files(), 60, 8);
        d.to_bottom();
        let before = d.cursor() as f32 / d.rows() as f32;
        d.cycle_layout(&host);
        assert_eq!(d.layout_name(), "split");
        let after = d.cursor() as f32 / d.rows() as f32;
        assert!((before - after).abs() < 0.2, "{before} -> {after}");
        assert!(d.cursor() < d.rows());
    }

    #[test]
    fn cycling_the_layout_returns_to_where_it_started() {
        let (mut d, host) = view(diff(10), 60, 8);
        let first = d.layout_name();
        for _ in 0..d.layout_names().len() {
            d.cycle_layout(&host);
        }
        assert_eq!(d.layout_name(), first);
    }

    #[test]
    fn cycling_the_wrap_changes_the_row_count_and_off_is_one_of_them() {
        let long = format!(
            "diff --git a/a.rs b/a.rs\n@@ -1,1 +1,1 @@\n-{}\n",
            "word ".repeat(40)
        );
        let (mut d, host) = view(parse_unified_diff(&long), 40, 10);
        let mut seen = Vec::new();
        for _ in 0..host.wrap.len() {
            seen.push((host.wrap.at(d.wrap_index()).name(), d.rows()));
            d.cycle_wrap(&host);
        }
        assert!(seen.iter().any(|(n, _)| *n == "off"));
        let off = seen.iter().find(|(n, _)| *n == "off").unwrap().1;
        let word = seen.iter().find(|(n, _)| *n == "word").unwrap().1;
        assert!(word > off, "{seen:?}");
    }

    #[test]
    fn a_horizontal_scroll_is_bounded_by_the_widest_row_and_reset_by_wrapping() {
        let long =
            "diff --git a/a.rs b/a.rs\n@@ -1,1 +1,1 @@\n-".to_string() + &"x".repeat(200) + "\n";
        let (mut d, host) = view(parse_unified_diff(&long), 40, 10);
        d.set_wrap(host.wrap.position("off").unwrap(), &host);
        d.scroll_x(-5);
        assert_eq!(d.shift(), 0, "scrolled left of column zero");
        d.scroll_x(9999);
        assert_eq!(d.shift(), 200, "the bound was not the widest row");
        d.set_wrap(host.wrap.position("word").unwrap(), &host);
        assert_eq!(d.shift(), 0, "a wrapped diff kept a horizontal offset");
    }

    #[test]
    fn every_visible_row_is_drawn_and_nothing_below_the_diff_is_stale() {
        let (mut d, host) = view(diff(4), 40, 10);
        let mut screen = Screen::new(40, 12);
        let mut out = Vec::new();
        screen.clear(Ink::new(0xffffff, 0x000000));
        // Row 0 is reserved for a title bar the assembly owns; the view starts
        // at 1 and must not touch the row above it.
        d.paint(&mut screen, 1, &host, &mut out);
        assert_eq!(
            screen.ink(0, 0).unwrap().bg,
            0x000000,
            "the view wrote above its box"
        );
        assert!(screen.row_text(1).contains("a.rs"));
        assert!(screen.row_text(4).contains("line 1"));
        // Past the end of the diff: the chrome's background, not the last frame.
        assert_eq!(screen.ink(0, 9).unwrap().bg, host.theme.chrome.bg);
        d.to_bottom();
    }

    #[test]
    fn the_status_line_describes_the_frame_that_was_drawn() {
        let (mut d, host) = view(two_files(), 60, 20);
        let s = d.status(&host);
        assert!(s.starts_with("1/"), "{s}");
        assert!(s.contains("3 files"), "{s}");
        assert!(s.contains("unified"), "{s}");
        assert!(s.contains("word"), "{s}");
        d.cycle_layout(&host);
        assert!(d.status(&host).contains("split"), "{}", d.status(&host));
    }

    #[test]
    fn an_unknown_layout_name_falls_back_and_says_so() {
        let mut host = Host::new();
        host.layout = "nonexistent".into();
        let d = Diff::new(diff(4), &host);
        assert_eq!(d.layout_index(), 0);
    }

    // ------------------------------------------------------------------- mouse

    /// Three lines of one file, so a drag has somewhere to go.
    fn text_diff() -> Vec<FileDiff> {
        parse_unified_diff(
            "diff --git a/a.rs b/a.rs\n@@ -1,3 +1,3 @@\n one two\n three four\n five six\n",
        )
    }

    #[test]
    fn a_click_moves_the_cursor_and_a_drag_selects_the_text_between() {
        let (mut d, host) = view(text_diff(), 40, 20);
        // Rows: 0 file, 1 hunk, 2..5 the lines. The text starts at column 8.
        d.press(12, 2, 1, false, &host);
        assert_eq!(d.cursor(), 2, "the cursor did not follow the click");
        // Down to the middle of the next line: `two\nthree` — and the character
        // under the pointer is included, which is the thing a cell grid gets
        // wrong by default.
        d.drag(12, 3, &host);
        d.release();
        assert_eq!(d.selection(), "two\nthree");
    }

    #[test]
    fn a_drag_backwards_selects_the_same_text_as_a_drag_forwards() {
        let (mut d, host) = view(text_diff(), 40, 20);
        d.press(12, 2, 1, false, &host);
        d.drag(16, 2, &host);
        let forwards = d.selection();
        assert_eq!(forwards, "two");
        d.press(16, 2, 1, false, &host);
        d.drag(12, 2, &host);
        assert_eq!(d.selection(), forwards, "the anchor is not the start");
    }

    #[test]
    fn two_clicks_take_a_word_and_three_take_the_row() {
        let (mut d, host) = view(text_diff(), 40, 20);
        d.press(10, 2, 2, false, &host);
        assert_eq!(d.selection(), "one");
        d.press(10, 2, 3, false, &host);
        assert_eq!(d.selection(), "one two");
    }

    #[test]
    fn shift_extends_what_is_already_selected() {
        let (mut d, host) = view(text_diff(), 40, 20);
        d.press(8, 2, 1, false, &host);
        d.press(11, 3, 1, true, &host);
        assert_eq!(d.selection(), "one two\nthre");
    }

    #[test]
    fn a_press_on_nothing_selectable_clears_the_selection() {
        let (mut d, host) = view(text_diff(), 40, 20);
        d.press(10, 2, 2, false, &host);
        assert!(!d.selection().is_empty());
        // Past the end of a three-line diff: there is no row there.
        d.press(10, 15, 1, false, &host);
        assert_eq!(d.selection(), "");
    }

    #[test]
    fn a_drag_past_the_bottom_scrolls_and_keeps_selecting() {
        let (mut d, host) = view(diff(60), 40, 10);
        d.press(10, 1, 1, false, &host);
        let before = d.top();
        d.drag(10, 14, &host);
        assert!(d.top() > before, "the view did not follow the drag");
        assert!(d.selection().lines().count() > 5, "{:?}", d.selection());
        // ...and back up above the top.
        d.drag(10, -20, &host);
        assert_eq!(d.top(), 0);
    }

    #[test]
    fn copying_with_nothing_selected_copies_the_row_the_cursor_is_on() {
        // The fallback that makes `y` worth binding at all.
        let (mut d, host) = view(text_diff(), 40, 20);
        d.move_by(3);
        assert_eq!(d.copy_text(), "three four");
        d.press(10, 2, 2, false, &host);
        assert_eq!(d.copy_text(), "one", "a selection wins over the cursor");
    }

    #[test]
    fn a_click_selects_nothing_and_a_gesture_that_does_is_what_copy_on_select_sees() {
        // The rule the whole feature rests on: `selection` is what the mouse
        // holds and is empty after a click, so pointing at a line does not
        // clobber the clipboard. `copy_text` is the key's fallback and is not.
        let (mut d, host) = view(text_diff(), 40, 20);
        d.press(12, 2, 1, false, &host);
        d.release();
        assert_eq!(d.selection(), "");
        assert_eq!(d.copy_text(), "one two");
        // A drag does select, and so does a double click without one.
        d.press(12, 2, 1, false, &host);
        d.drag(16, 2, &host);
        d.release();
        assert_eq!(d.selection(), "two");
        d.press(9, 3, 2, false, &host);
        d.release();
        assert_eq!(d.selection(), "three");
    }

    #[test]
    fn select_all_takes_the_whole_diff_and_none_gives_it_back() {
        let (mut d, _) = view(text_diff(), 40, 20);
        d.select_all();
        let all = d.selection();
        assert!(all.starts_with("a.rs"), "{all:?}");
        assert!(all.ends_with("five six"), "{all:?}");
        assert!(d.select_none());
        assert_eq!(d.selection(), "");
        assert!(!d.select_none(), "there was nothing left to drop");
    }

    #[test]
    fn a_selection_survives_a_reflow_and_not_a_layout_change() {
        // A reflow moves every visual row and the carets cache them, so this is
        // the one place a stale selection would highlight the wrong line.
        let long = format!(
            "diff --git a/a.rs b/a.rs\n@@ -1,1 +1,1 @@\n{}",
            (0..12)
                .map(|i| format!(" line {i} {}\n", "padding ".repeat(6)))
                .collect::<String>()
        );
        let (mut d, host) = view(parse_unified_diff(&long), 100, 20);
        d.press(10, 6, 3, false, &host);
        let before = d.selection();
        assert!(!before.is_empty());
        d.resize(40, 20, &host);
        assert_eq!(
            d.selection(),
            before,
            "the selection followed the wrong line"
        );
        d.cycle_layout(&host);
        assert_eq!(d.selection(), "", "a selection outlived the rows it was on");
    }

    #[test]
    fn a_click_on_the_scrollbar_scrolls_and_does_not_select() {
        let (mut d, host) = view(diff(200), 40, 10);
        // The last column, half way down the track.
        d.press(39, 5, 1, false, &host);
        assert!(d.top() > 0, "the bar did not take the click");
        assert_eq!(d.selection(), "", "the bar started a selection");
        assert_eq!(d.cursor(), d.cursor().min(d.rows()));
        d.drag(39, 9, &host);
        assert_eq!(
            d.top(),
            d.rows() - 10,
            "the end of the track is the end of the list"
        );
        d.release();
        // Once released, the bar is not holding the pointer any more.
        let top = d.top();
        d.drag(39, 0, &host);
        assert_eq!(d.top(), top);
    }

    #[test]
    fn the_scrollbar_is_only_where_the_config_file_says_it_is() {
        let mut host = Host::new();
        host.view.scrollbar = false;
        let mut d = Diff::new(diff(200), &host);
        d.resize(40, 10, &host);
        d.press(39, 5, 1, false, &host);
        assert_eq!(d.top(), 0, "a bar that is not drawn took a click");
        // ...and the click reached the text instead.
        assert_eq!(d.cursor(), 5);
    }

    #[test]
    fn an_empty_diff_draws_nothing_and_panics_nowhere() {
        let (mut d, host) = view(Vec::new(), 40, 6);
        assert_eq!(d.rows(), 0);
        d.down();
        d.page(1);
        d.to_bottom();
        d.jump_file(1);
        assert_eq!(d.cursor(), 0);
        let mut screen = Screen::new(40, 6);
        let mut out = Vec::new();
        d.paint(&mut screen, 0, &host, &mut out);
        assert_eq!(screen.dump().trim(), "");
    }

    #[test]
    fn a_zero_width_terminal_does_not_reflow_into_a_row_per_character() {
        // A budget floor is what stops this; without it a diff becomes a column
        // of letters and the row count explodes.
        let (mut d, host) = view(diff(4), 0, 6);
        d.resize(0, 6, &host);
        assert!(d.rows() <= 32, "{} rows at zero columns", d.rows());
        let mut owners: Vec<Box<dyn Rows>> = vec![Box::new(TextRows::default())];
        owners[0].reflow(1, &host, host.wrap.current());
        let _ = &owners;
    }

    // ------------------------------------------------------------------ hunks

    /// Two files, one hunk each, with content that names which file a hunk
    /// came from.
    fn two_hunks() -> Vec<FileDiff> {
        parse_unified_diff(
            "diff --git a/one.rs b/one.rs\n@@ -1,3 +1,3 @@\n keep\n-was\n+now\n tail\n\
             diff --git a/two.rs b/two.rs\n@@ -1,2 +1,2 @@\n-head\n+tail\n",
        )
    }

    fn split_view(files: Vec<FileDiff>, cols: usize, height: usize) -> (Diff, Host) {
        let host = Host::new();
        let layouts = Layouts::builtin();
        let split = layouts.position("split").unwrap();
        let mut d = Diff::with_layouts(files, &host, layouts);
        d.set_layout(split, &host);
        d.resize(cols, height, &host);
        (d, host)
    }

    #[test]
    fn the_terminal_finds_the_hunk_under_the_cursor_in_both_layouts() {
        let loaded = two_hunks();
        // Split pairs a removal with its addition, so its row numbers differ
        // from unified's; the hunk under each row must not.
        let rows = |split: bool| match split {
            false => (vec![1, 3, 5], 6, vec![7, 9]),
            true => (vec![1, 3, 4], 5, vec![6, 7]),
        };
        for (split, built) in [
            (false, view(two_hunks(), 60, 20)),
            (true, split_view(two_hunks(), 80, 20)),
        ] {
            let (mut d, _) = built;
            let (first, gap, second) = rows(split);
            // A file header is nobody's hunk.
            d.to_top();
            assert_eq!(d.current_hunk(), None, "cursor on a file header");
            // The hunk's header row, its middle and its tail all answer for
            // the one hunk the loaded diff holds.
            for row in first {
                d.to_top();
                d.move_by(row as isize);
                let (path, hunk) = d.current_hunk().expect("the keyboard is on a hunk");
                assert_eq!(path, "one.rs");
                assert_eq!(hunk, loaded[0].hunks[0], "row {row}");
            }
            // The second file's header is a gap; its hunk answers its own.
            d.to_top();
            d.move_by(gap as isize);
            assert_eq!(d.current_hunk(), None, "the second file's header");
            for row in second {
                d.to_top();
                d.move_by(row as isize);
                let (path, hunk) = d.current_hunk().expect("the second hunk");
                assert_eq!(path, "two.rs");
                assert_eq!(hunk, loaded[1].hunks[0], "row {row}");
            }
        }
    }

    #[test]
    fn a_wrapped_line_resolves_to_its_one_hunk_on_every_segment() {
        // The address is a logical row, so however many rows a wrapped line
        // takes, every one of them is the same hunk — the one a verb must
        // act on when the cursor sits halfway down the line's second row.
        let long = format!(
            "diff --git a/a.rs b/a.rs\n@@ -1,1 +1,1 @@\n-{}\n+b\n",
            "word ".repeat(20)
        );
        let loaded = parse_unified_diff(&long);
        let (mut d, _) = view(loaded.clone(), 30, 20);
        for row in 0..d.rows() {
            d.to_top();
            d.move_by(row as isize);
            match row {
                0 => assert_eq!(d.current_hunk(), None, "the file header"),
                _ => {
                    let (path, hunk) = d.current_hunk().expect("row {row} is on a hunk");
                    assert_eq!(path, "a.rs");
                    assert_eq!(hunk, loaded[0].hunks[0], "row {row}");
                }
            }
        }
        assert!(d.rows() > 3, "the line did not wrap at 30 columns");
    }

    #[test]
    fn a_misrecorded_hunk_map_answers_nothing_rather_than_the_wrong_hunk() {
        // An extension records its own spans; a bad one must degrade to
        // "the keyboard is not on a hunk" rather than hand over some other
        // file's hunk. Three ways to be bad: a file index past the
        // presentation's own list, a path the loaded diff never heard of,
        // and a hunk index past the file's hunks.
        #[derive(Default)]
        struct Bogus {
            rows: usize,
            entries: Vec<gitten_core::rows::Entry>,
            answer: Option<(usize, usize)>,
            path: String,
        }
        impl Present for Bogus {
            fn claims(&self, _: &str) -> bool {
                true
            }
            fn len(&self) -> usize {
                self.rows
            }
            fn build(&mut self, f: File) {
                self.entries.push(gitten_core::rows::Entry {
                    path: self.path.clone(),
                    adds: f.adds,
                    dels: f.dels,
                    row: self.rows,
                });
                self.rows += 1 + f.hunks.iter().map(|h| 1 + h.lines.len()).sum::<usize>();
            }
            fn files(&self) -> &[gitten_core::rows::Entry] {
                &self.entries
            }
            fn hunk_at(&self, _: usize) -> Option<(usize, usize)> {
                self.answer
            }
        }
        impl Rows for Bogus {
            fn render(
                &self,
                _: usize,
                _: usize,
                _: &Frame,
                _: &mut crate::screen::Pen,
                _: &mut Vec<Run>,
            ) {
            }
        }

        let bogus = |answer: Option<(usize, usize)>, path: &'static str| {
            let host = Host::new();
            let mut layouts = Layouts::builtin();
            layouts.register("bogus", move |_| {
                vec![Box::new(Bogus {
                    answer,
                    path: path.to_string(),
                    ..Default::default()
                })]
            });
            let at = layouts.position("bogus").unwrap();
            let mut d = Diff::with_layouts(two_hunks(), &host, layouts);
            d.set_layout(at, &host);
            d.resize(60, 20, &host);
            d
        };

        for (answer, path) in [
            (Some((99, 0)), "one.rs"),  // no such file in this presentation
            (Some((0, 0)), "ghost.rs"), // a path the loaded diff never held
            (Some((0, 99)), "one.rs"),  // no such hunk in that file
            (None, "one.rs"),           // honestly off the hunks
        ] {
            let mut d = bogus(answer, path);
            d.move_by(2);
            assert_eq!(
                d.current_hunk(),
                None,
                "{answer:?} against {path:?} acted on something"
            );
        }
    }

    #[test]
    fn refresh_replaces_a_diff_without_losing_a_valid_viewport() {
        let (mut d, host) = view(two_files(), 60, 8);
        d.move_by(6);
        assert!(d.top() > 0, "the test wants a view below row zero");
        let (cursor, top) = (d.cursor(), d.top());
        // A refresh whose answer is shorter: the rows the cursor was on may
        // not exist any more, and the viewport cannot point past the end.
        let refreshed = vec![two_files()[0].clone()];
        d.replace(refreshed, &host);
        assert_eq!(d.cursor(), cursor.min(d.rows() - 1), "clamped, not reset");
        assert_eq!(d.top(), top.min(d.rows().saturating_sub(d.height().max(1))));
        assert!(d.cursor() < d.rows());
        assert!(d.top() <= d.cursor());
        // And a refresh that answers the same diff moves nothing at all.
        d.replace(two_files(), &host);
        let (cursor, top) = (d.cursor(), d.top());
        d.replace(two_files(), &host);
        assert_eq!((d.cursor(), d.top()), (cursor, top));
    }

    #[test]
    fn a_vanished_hunk_clamps_and_a_live_row_survives_the_swap() {
        let (mut d, host) = view(two_hunks(), 60, 8);
        // Onto the second file's hunk, then take that file out from under
        // the keyboard: the hunk vanished and the row no longer exists.
        d.move_by(7);
        // The mouse was holding rows that the swap takes away. It moved the
        // keyboard with it, so the keyboard goes back to the hunk that is
        // about to vanish before the swap is measured.
        d.press(12, 2, 2, false, &host);
        d.release();
        assert!(!d.selection().is_empty(), "the test wants a selection");
        d.move_by(3);
        let refreshed = vec![two_hunks()[0].clone()];
        d.replace(refreshed, &host);
        assert_eq!(d.cursor(), d.rows() - 1, "clamped, not wrapped");
        assert_eq!(d.top(), 0);
        assert_eq!(d.selection(), "", "a selection outlived the rows it held");
        // A row that survives lands where it was: the numeric fallback.
        let (mut d, host) = view(two_hunks(), 60, 8);
        d.move_by(2);
        let (cursor, top) = (d.cursor(), d.top());
        d.replace(vec![two_hunks()[1].clone()], &host);
        assert_eq!(d.cursor(), cursor);
        assert!(d.top() <= top);
    }
}
