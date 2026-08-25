//! The working tree, as a list.
//!
//! lazygit's Files panel, viewer half: what [`Status`] says, grouped into the
//! four sections git's own model names — staged, unstaged, untracked, conflicts
//! — with a section drawn only when it has something in it. Staging is not
//! here yet; what is here is the shape it will plug into, which is why every
//! row keeps its [`PathBytes`] beside its display text and which group it
//! belongs to.
//!
//! The list idioms are [`super::commits`]'s, on purpose: one `Viewport`, one
//! scroll-handle dance, rows flattened **once per refresh** into owned display
//! strings so the render path allocates nothing per frame.

use super::{accept_deferred_scroll, DeferredScrollbar, PendingScroll};
use crate::graph::ROW_H;
use gitten_core::host::Host;
use gitten_core::status::{Change, ConflictKind, PathBytes, Status};
use gitten_core::theme::Rgb;
use gitten_core::view::Viewport;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::scroll::Scrollbar;
use std::cell::Cell;
use std::collections::HashSet;
use std::rc::Rc;

/// One flat row of the pane: a section heading or one file.
///
/// Flattened once per refresh — never per frame. Everything a draw needs that
/// costs allocation (the lossy path text, the spelled-out count) is computed
/// at flatten time; what a draw reads per frame is an enum match and a
/// refcount bump.
pub(crate) enum Entry {
    /// A group heading, drawn only because the group under it is non-empty.
    Heading {
        /// How many files are in the group, spelled out once.
        count: SharedString,
        section: Section,
    },
    File(FileEntry),
}

/// One file of the working tree.
pub(crate) struct FileEntry {
    /// Which group it sits under — what stage/unstage will need to know where
    /// a verb goes, and half of the refresh anchor.
    pub section: Section,
    /// The addressing form, byte for byte. Never decoded in place.
    pub path: PathBytes,
    /// The display form, decoded lossily once at flatten.
    pub path_text: SharedString,
    /// What a rename moved it from, decoded once; `None` otherwise.
    pub renamed_from: Option<SharedString>,
    /// What happened to it — the letter's meaning and its colour in one.
    pub mark: Mark,
    /// The letter(s) themselves, git's own spelling: `A`, `M`, `UU`, `?`.
    pub letters: &'static str,
}

/// The four questions a status panel asks, in draw order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Staged,
    Unstaged,
    Untracked,
    Conflicts,
}

impl Section {
    fn name(self) -> &'static str {
        match self {
            Section::Staged => "staged",
            Section::Unstaged => "unstaged",
            Section::Untracked => "untracked",
            Section::Conflicts => "conflicts",
        }
    }

    fn all() -> [Section; 4] {
        [
            Section::Staged,
            Section::Unstaged,
            Section::Untracked,
            Section::Conflicts,
        ]
    }
}

/// What a status letter means, once you get past which side of the index it is
/// about — which is what decides its colour, and nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    Add,
    Modify,
    Delete,
    Rename,
    TypeChange,
    Untracked,
    Conflict,
}

impl Mark {
    /// From git's change letter set. A rename and a copy both mean "the index
    /// matched content across two paths", and draw alike.
    fn of(change: Change) -> Self {
        match change {
            Change::Added => Mark::Add,
            Change::Modified => Mark::Modify,
            Change::Deleted => Mark::Delete,
            Change::Renamed | Change::Copied => Mark::Rename,
            Change::TypeChanged => Mark::TypeChange,
        }
    }

    /// The single letter git prints. Drawn from the theme, not spelled here.
    fn letter(self) -> &'static str {
        match self {
            Mark::Add => "A",
            Mark::Modify => "M",
            Mark::Delete => "D",
            Mark::Rename => "R",
            Mark::TypeChange => "T",
            // Known to no part of git: git itself prints `??`, and one honest
            // glyph beats two.
            Mark::Untracked => "?",
            Mark::Conflict => "",
        }
    }

    fn color(self, host: &Host) -> Rgb {
        let t = &host.theme;
        match self {
            // Additions and deletions borrow the diff palette, where those two
            // words already have colours a theme has tuned.
            Mark::Add => t.diff.adds_fg,
            Mark::Delete => t.diff.dels_fg,
            Mark::Conflict => t.chrome.error,
            Mark::Rename => t.chrome.accent,
            Mark::Modify => t.chrome.fg,
            // Rare enough not to earn a hue of its own; quieter than any of
            // the above is the right amount of loud for a typechange.
            Mark::TypeChange => t.chrome.dim,
            Mark::Untracked => t.chrome.faint,
        }
    }
}

/// The two-letter state of a conflicted path, exactly as porcelain v2 spells
/// it — who added and who deleted decides what resolving means, so the letters
/// are data and not decoration.
fn conflict_letters(state: ConflictKind) -> &'static str {
    match state {
        ConflictKind::BothDeleted => "DD",
        ConflictKind::AddedByUs => "AU",
        ConflictKind::DeletedByThem => "UD",
        ConflictKind::AddedByThem => "UA",
        ConflictKind::DeletedByUs => "DU",
        ConflictKind::BothAdded => "AA",
        ConflictKind::BothModified => "UU",
    }
}

/// The whole working tree flattened to rows, plus the title-strip line. Pure —
/// this is the unit-tested half of a refresh.
pub(crate) struct Prepared {
    pub(crate) rows: Vec<Entry>,
    /// The title-strip line: who we are and how much changed.
    pub(crate) label: String,
}

/// Flattens a status into display rows: one heading per non-empty section,
/// then that section's files, in [`Section::all`] order.
pub(crate) fn flatten(status: &Status) -> Vec<Entry> {
    use Entry::*;
    let mut rows = Vec::new();
    for section in Section::all() {
        let files: Vec<FileEntry> = match section {
            Section::Staged => status
                .staged
                .iter()
                .map(|e| {
                    let mark = Mark::of(e.change);
                    file(section, &e.path, e.old_path.as_ref(), mark, mark.letter())
                })
                .collect(),
            Section::Unstaged => status
                .unstaged
                .iter()
                .map(|e| {
                    let mark = Mark::of(e.change);
                    file(section, &e.path, None, mark, mark.letter())
                })
                .collect(),
            Section::Untracked => status
                .untracked
                .iter()
                .map(|e| file(section, &e.path, None, Mark::Untracked, "?"))
                .collect(),
            Section::Conflicts => status
                .conflicts
                .iter()
                .map(|e| {
                    file(
                        section,
                        &e.path,
                        None,
                        Mark::Conflict,
                        conflict_letters(e.state),
                    )
                })
                .collect(),
        };
        if files.is_empty() {
            continue;
        }
        rows.push(Heading {
            count: SharedString::from(files.len().to_string()),
            section,
        });
        rows.extend(files.into_iter().map(File));
    }
    rows
}

fn file(
    section: Section,
    path: &PathBytes,
    old_path: Option<&PathBytes>,
    mark: Mark,
    letters: &'static str,
) -> FileEntry {
    FileEntry {
        section,
        path: path.clone(),
        path_text: path.to_string_lossy().into_owned().into(),
        // The arrow baked in here, not per frame: a rename's origin is
        // furniture drawn dim, and no string is built for it on the render
        // path.
        renamed_from: old_path.map(|p| format!("← {}", p.to_string_lossy()).into()),
        mark,
        letters,
    }
}

/// [`flatten`] plus what the title strip says about it. The load line goes to
/// stderr like every other view's, and nothing is stored for an overlay that
/// does not read panes.
///
/// The count is **distinct paths**: one file edited, staged and edited again
/// sits in two lists and is still one change to a person.
pub(crate) fn prepare(status: Status, describe: &str) -> Prepared {
    let t = std::time::Instant::now();
    let rows = flatten(&status);
    let mut seen = HashSet::new();
    let changed = rows
        .iter()
        .filter_map(|r| match r {
            Entry::File(f) => Some(f),
            _ => None,
        })
        .filter(|f| seen.insert(&f.path))
        .count();
    eprintln!("files: {changed} entries · flatten {:.0?}", t.elapsed());
    Prepared {
        rows,
        label: format!("{describe} · {changed} changed"),
    }
}

/// What an armed discard asks, once, in the notice band. An untracked file
/// says *delete* because that is what discarding means when there is no
/// earlier version to go back to — the honest word for the one mechanics
/// where nothing is recoverable.
pub(crate) fn discard_question(section: Section, shown: &str) -> String {
    match section {
        Section::Untracked => format!("delete {shown}? press again to confirm"),
        _ => format!("discard {shown}? press again to confirm"),
    }
}

/// The working-tree pane. Holds flattened rows behind an `Rc`, so a refresh
/// swaps one refcount instead of mutating what a frame may be reading.
///
/// Destructive verbs confirm **in this pane** rather than in a dialog: there
/// is no modal anywhere in the window yet, and arming the keyboard's own row
/// needs none. The first press stores [`Files::armed`] and asks its question
/// through the notice band; the second press on the same row executes; any
/// cursor move, wheel or refresh drops the arm — a stale yes waiting on a
/// file that has already changed under it is exactly the accident the double
/// press exists to prevent.
pub struct Files {
    data: Rc<Vec<Entry>>,
    scroll: UniformListScrollHandle,
    /// The cursor, the top row and the height — [`Viewport`], the same model
    /// every other list holds.
    view: Rc<Cell<Viewport>>,
    synced: Rc<Cell<f32>>,
    pending_scroll: PendingScroll,
    rendered: Rc<Cell<usize>>,
    /// The discard awaiting its second press: the section and path of the
    /// row that asked. One slot — arming a different row moves the question,
    /// it does not queue two. Outliving a switch to another pane and back is
    /// deliberate: the question still sits on the row it was asked about,
    /// and only a cursor move, a wheel or a refresh can make its answer
    /// stale — none of which is a focus change.
    armed: Option<(Section, PathBytes)>,
}

impl Files {
    /// The viewport model with everything live folded in — see
    /// [`super::commits::Commits::live_view`].
    fn live_view(&self, host: &Host) -> Viewport {
        let mut v = self.view.get();
        v.set_len(self.data.len());
        v.set_height(self.rendered.get());
        v.set_scrolloff(host.view.scrolloff);
        v
    }

    pub(crate) fn from_prepared(prepared: Prepared) -> Self {
        let Prepared { rows, .. } = prepared;
        Self {
            data: Rc::new(rows),
            scroll: UniformListScrollHandle::new(),
            view: Rc::new(Cell::new(Viewport::new())),
            synced: Rc::new(Cell::new(0.0)),
            pending_scroll: PendingScroll::default(),
            rendered: Rc::new(Cell::new(0)),
            armed: None,
        }
    }

    /// Whether the working tree had nothing to say — the empty state's trigger.
    pub fn is_clean(&self) -> bool {
        self.data.is_empty()
    }

    /// Replaces repository data while keeping the selection anchored to its
    /// path — the semantic cursor anchor of this pane. A file that vanished
    /// falls back to clamping, like a commit list whose sha left the log.
    #[cfg(test)]
    fn replace(&mut self, status: Status, host: &Host) {
        self.replace_prepared(prepare(status, ""), host);
    }

    pub(crate) fn replace_prepared(&mut self, prepared: Prepared, host: &Host) {
        // A refresh is the repository saying things moved; an armed discard
        // was a promise about how they were, so it dies here first.
        self.armed = None;
        self.reconcile(host);
        let old = self.view.get();
        // Only a file anchors, and on its **section and path together**: the
        // same path can sit in staged *and* unstaged, and anchoring on the
        // bare path would walk the cursor to whichever twin flattens first.
        // A heading is a fact about the last refresh's grouping, not a thing
        // the eye was reading.
        let anchored = match self.data.get(old.cursor()) {
            Some(Entry::File(f)) => Some((f.section, f.path.clone())),
            _ => None,
        };
        let Prepared { rows, .. } = prepared;
        self.data = Rc::new(rows);

        let cursor = anchored
            .and_then(|(section, path)| {
                self.data.iter().position(
                    |e| matches!(e, Entry::File(f) if f.section == section && f.path == path),
                )
            })
            .unwrap_or(old.cursor());
        let mut view = old;
        view.set_len(self.data.len());
        view.go_to(cursor);
        self.view.set(view);

        if self.data.is_empty() {
            self.pending_scroll.cancel();
            let mut state = self.scroll.0.borrow_mut();
            state.deferred_scroll_to_item = None;
            state.base_handle.set_offset(point(px(0.0), px(0.0)));
            self.synced.set(0.0);
        } else {
            self.defer_show(view);
        }
    }

    // -------------------------------------------------------------- commands

    /// The box the list is drawn in, for hit-testing a wheel event.
    pub fn list_bounds(&self) -> Bounds<Pixels> {
        self.scroll.0.borrow().base_handle.bounds()
    }

    /// Nothing off the left edge to reach — paths truncate rather than pan.
    /// Present so the wheel routing can offer the axis to every screen alike.
    pub fn pan_pixels(&self, _dx: f32) -> bool {
        false
    }

    /// Moves the list by `dy` pixels — the wheel, whose command resolves
    /// through `[keys]` but whose delta is pixels. Same dance as the commit
    /// list, for the same reasons.
    pub fn scroll_pixels(&mut self, dy: f32, host: &Host) -> bool {
        let deferred = self.scroll.0.borrow().deferred_scroll_to_item;
        if let Some(request) = deferred {
            if self.pending_scroll.is_awaiting() {
                let pixels = self.pending_scroll.wheel(dy);
                let mut v = self.live_view(host);
                let y = -(request.item_index as f32 * ROW_H) + pixels;
                v.scroll_to((-y / ROW_H).round().max(0.0) as usize);
                self.view.set(v);
                // The wheel is also a move of attention — same rule the
                // arrow keys keep.
                self.armed = None;
                return true;
            }
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
        let mut v = self.live_view(host);
        v.scroll_to((-y / ROW_H).round().max(0.0) as usize);
        self.view.set(v);
        self.synced.set(y);
        self.armed = None;
        true
    }

    /// Meets the list where it actually is after a scrollbar drag — see
    /// [`super::commits::Commits::reconcile`].
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
        v.scroll_to(shown);
        self.view.set(v);
    }

    /// Runs one of the commands this pane answers. The `view.*` family is the
    /// shared list vocabulary — inherited by every mode through [`GLOBAL`] —
    /// onto the same [`Viewport`] arithmetic the commit list runs.
    ///
    /// False is "not one of mine", and the caller says so.
    pub fn run_view(&mut self, command: &str, host: &Host) -> bool {
        self.reconcile(host);
        let mut v = self.live_view(host);
        match command {
            "view.down" => v.down(),
            "view.up" => v.up(),
            "view.page-down" => v.page(1),
            "view.page-up" => v.page(-1),
            "view.scroll-down" => v.scroll_by(host.view.rows as isize),
            "view.scroll-up" => v.scroll_by(-(host.view.rows as isize)),
            "view.top" => v.to_top(),
            "view.bottom" => v.to_bottom(),
            // Answered without doing anything, like the commit graph: a
            // resolved command must not read as a failed one. Nothing moved
            // either, so an armed discard stands.
            "view.left" | "view.right" => return true,
            _ => return false,
        }
        // The keyboard moved — including the two scrolls above, which leave
        // the cursor but not the question's row in view. Whatever was armed
        // was armed to what the keyboard used to be on.
        self.armed = None;
        self.view.set(v);
        self.show(v);
        true
    }

    /// Puts row `v.top()` at the top of the viewport, exactly — see
    /// [`super::commits::Commits::show`].
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
    }

    fn defer_show(&self, v: Viewport) {
        self.pending_scroll.begin();
        self.scroll
            .scroll_to_item_strict(v.top(), ScrollStrategy::Top);
    }

    /// What the keyboard is on: the whole file entry — section and path
    /// together, which is what a stage verb will need to know where its work
    /// goes. `None` on a heading or an empty tree.
    pub(crate) fn current_file(&self) -> Option<&FileEntry> {
        match self.data.get(self.view.get().cursor()) {
            Some(Entry::File(f)) => Some(f),
            _ => None,
        }
    }

    /// Which section the keyboard sits *in* — a heading counts as its own
    /// section's ground, because that is where the eye puts it. What makes
    /// the whole-section verb readable: the side of the index under the
    /// keyboard decides, heading or file alike.
    pub(crate) fn cursor_section(&self) -> Option<Section> {
        match self.data.get(self.view.get().cursor()) {
            Some(Entry::Heading { section, .. }) => Some(*section),
            Some(Entry::File(f)) => Some(f.section),
            None => None,
        }
    }

    /// Every path flattened under one section, in draw order — what a
    /// whole-section verb acts on. Bytes throughout, because these aim
    /// verbs; the display forms live only in the rows.
    pub(crate) fn paths_in(&self, section: Section) -> Vec<PathBytes> {
        self.data
            .iter()
            .filter_map(|e| match e {
                Entry::File(f) if f.section == section => Some(f.path.clone()),
                _ => None,
            })
            .collect()
    }

    /// Arms — or confirms — a discard of this exact row. The first call on a
    /// target stores it and returns false: ask, don't act. A second call on
    /// the same target clears the arm and returns true: act. Anything else
    /// (a different row, after a move or refresh cleared it) re-arms onto
    /// the new target and returns false again, so there is no state here a
    /// caller has to remember.
    pub(crate) fn confirm_or_arm_discard(&mut self, section: Section, path: &PathBytes) -> bool {
        let already = matches!(
            &self.armed,
            Some((armed_section, armed_path))
                if *armed_section == section && armed_path == path
        );
        self.armed = match already {
            true => None,
            false => Some((section, path.clone())),
        };
        already
    }

    /// Whether a discard is waiting for its second press — the render's
    /// tint of the row the question is about.
    #[cfg(test)]
    pub(crate) fn armed_row(&self) -> Option<(Section, PathBytes)> {
        self.armed.clone()
    }

    /// What `copy.selection` copies here: the row the keyboard is on, as git
    /// would spell it — letters, then path. A heading copies nothing, which is
    /// what makes the empty result skip the clipboard entirely.
    pub fn cursor_text(&self) -> String {
        match self.current_file() {
            Some(f) => format!("{} {}", f.letters, f.path),
            None => String::new(),
        }
    }

    /// No drag selection over a file list yet; `select.all` is inert here, the
    /// same answer the commit graph gives.
    pub fn select_all(&mut self) -> bool {
        false
    }

    pub fn select_none(&mut self) -> bool {
        false
    }
}

/// Width of the status column, in characters. Two, because a conflict's XY
/// pair is the widest thing git puts there.
const STATUS_CHARS: f32 = 2.0;
/// Air between the columns, also in characters.
const GAP_CHARS: f32 = 1.5;

impl Render for Files {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = crate::config::host(cx).theme.chrome;
        // A clean tree is a sentence, not an empty box: the pane still exists,
        // it just has nothing to list.
        if let Some(empty) = self.is_clean().then(|| {
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(rgb(c.faint))
                .child("No changes — working tree clean")
                .into_any_element()
        }) {
            return empty;
        }

        let data = self.data.clone();
        let rendered = self.rendered.clone();
        let view = self.view.clone();
        let scroll = self.scroll.clone();
        let synced = self.synced.clone();
        let pending_scroll = self.pending_scroll.clone();
        // The row an armed discard is waiting on, found once per frame —
        // the tint is a property of the question, not of the draw.
        let armed = self.armed.as_ref().and_then(|(section, path)| {
            data.iter().position(
                |e| matches!(e, Entry::File(f) if f.section == *section && f.path == *path),
            )
        });
        let list = uniform_list("files", data.len(), move |range, _, cx| {
            rendered.set(range.len());
            let host = crate::config::host(cx);
            if let Some(accepted) = accept_deferred_scroll(&scroll, &pending_scroll, &synced) {
                if accepted.wheeled {
                    let mut v = view.get();
                    v.set_len(data.len());
                    v.set_height(range.len());
                    v.set_scrolloff(host.view.scrolloff);
                    v.scroll_to((-accepted.y / ROW_H).round().max(0.0) as usize);
                    view.set(v);
                    cx.refresh_windows();
                }
            }
            let cursor = view.get().cursor();
            range
                .map(|i| row(&data[i], &host, i == cursor, Some(i) == armed))
                .collect()
        })
        .track_scroll(&self.scroll)
        .size_full()
        .p_4();

        // The scrollbar overlays the list, so the container must be positioned
        // — and only the vertical one: paths clip rather than pan.
        div()
            .relative()
            .size_full()
            .child(list)
            .when(crate::config::host(cx).view.scrollbar, |d| {
                d.child(Scrollbar::vertical(&DeferredScrollbar::new(
                    &self.scroll,
                    &self.pending_scroll,
                )))
            })
            .into_any_element()
    }
}

/// One row: a dim heading, or a status letter in its colour beside the path.
/// `current` paints the keyboard's row in `chrome.selection_bg`, exactly as
/// the commit list does; `armed` tints that row's letters and path toward
/// `chrome.error`, so the thing a second press will destroy is named by its
/// own colour and not only by the band above it.
fn row(e: &Entry, host: &Host, current: bool, armed: bool) -> AnyElement {
    let ch = host.font.char_width();
    let c = host.theme.chrome;
    let base = div()
        .flex()
        .items_center()
        .h(px(ROW_H))
        .bg(rgb(match current {
            true => c.selection_bg,
            false => c.bg,
        }));
    match e {
        Entry::Heading { count, section } => base
            .child(
                div()
                    .flex_none()
                    .text_color(rgb(c.dim))
                    .child(section.name()),
            )
            .child(
                div()
                    .flex_none()
                    .ml(px(GAP_CHARS * ch))
                    .text_color(rgb(c.faint))
                    .child(count.clone()),
            )
            .into_any_element(),
        Entry::File(f) => {
            base.child(
                div()
                    .flex_none()
                    .w(px(STATUS_CHARS * ch))
                    // The error colour is already this palette's "this row
                    // ends work" foreground — conflicts draw their letters
                    // with it — so the armed tint spends nothing new.
                    .text_color(rgb(match armed {
                        true => c.error,
                        false => f.mark.color(host),
                    }))
                    .child(SharedString::from(f.letters)),
            )
            .child(
                div()
                    .flex_none()
                    .min_w_0()
                    // Unarmed keeps the inherited ink; only the question
                    // repaints the row.
                    .when(armed, |d| d.text_color(rgb(c.error)))
                    .child(f.path_text.clone()),
            )
            .children(f.renamed_from.as_ref().map(|old| {
                div()
                    .flex_none()
                    .ml(px(GAP_CHARS * ch))
                    .text_color(rgb(c.faint))
                    .child(old.clone())
            }))
            .into_any_element()
        }
    }
}

#[cfg(test)]
mod tests {
    // By name, not a glob: `use gpui::*` in the parent shadows `#[test]` with
    // GPUI's own attribute macro and every test in here fails to expand.
    use super::{
        conflict_letters, discard_question, flatten, prepare, Entry, Files, Mark, Section, Status,
    };
    use gitten_core::host::Host;
    use gitten_core::status::{
        Change, ConflictEntry, ConflictKind, Kind, PathBytes, StagedEntry, Submodule,
        UnstagedEntry, UntrackedEntry,
    };

    fn staged(path: &str, change: Change) -> StagedEntry {
        StagedEntry {
            path: PathBytes::from(path),
            change,
            old_path: None,
            kind: Kind::File,
            submodule: Submodule::default(),
        }
    }

    fn unstaged(path: &str, change: Change) -> UnstagedEntry {
        UnstagedEntry {
            path: PathBytes::from(path),
            change,
            kind: Kind::File,
            submodule: Submodule::default(),
        }
    }

    fn untracked(path: &str) -> UntrackedEntry {
        UntrackedEntry {
            path: PathBytes::from(path),
        }
    }

    fn conflict(path: &str, state: ConflictKind) -> ConflictEntry {
        ConflictEntry {
            path: PathBytes::from(path),
            state,
            kind: Kind::File,
            submodule: Submodule::default(),
        }
    }

    /// Headings and paths in draw order — the shape the tests read.
    fn outline(status: &Status) -> Vec<String> {
        flatten(status)
            .iter()
            .map(|e| match e {
                Entry::Heading { count, section } => {
                    format!("[{}·{count}]", section.name())
                }
                Entry::File(f) => f.path.to_string_lossy().into_owned(),
            })
            .collect()
    }

    fn with_height(f: &mut Files, n: usize) {
        f.rendered.set(n);
        let mut v = f.view.get();
        v.set_len(f.data.len());
        v.set_height(n);
        f.view.set(v);
    }

    /// A pane over one status, as a refresh would leave it. The label carries
    /// no repository name, which is what the caller would pass.
    fn files(status: Status) -> Files {
        Files::from_prepared(prepare(status, ""))
    }

    fn sample_status() -> Status {
        Status {
            staged: vec![
                staged("src/main.rs", Change::Modified),
                staged("gone.txt", Change::Deleted),
            ],
            unstaged: vec![unstaged("src/main.rs", Change::Modified)],
            untracked: vec![untracked("notes.md")],
            conflicts: vec![conflict("merged.rs", ConflictKind::BothModified)],
            ignored: vec![],
        }
    }

    #[test]
    fn sections_draw_in_order_and_empty_ones_do_not_draw_at_all() {
        let s = Status {
            untracked: vec![untracked("a.txt")],
            ..Default::default()
        };
        assert_eq!(
            outline(&s),
            vec!["[untracked·1]", "a.txt"],
            "a lone section draws no neighbours"
        );
        assert_eq!(outline(&Status::default()), Vec::<String>::new());
    }

    #[test]
    fn the_full_tree_flattens_into_four_labeled_groups() {
        let rows = outline(&sample_status());
        assert_eq!(
            rows,
            vec![
                "[staged·2]",
                "src/main.rs",
                "gone.txt",
                "[unstaged·1]",
                "src/main.rs",
                "[untracked·1]",
                "notes.md",
                "[conflicts·1]",
                "merged.rs",
            ]
        );
    }

    #[test]
    fn letters_match_gits_own_spelling() {
        assert_eq!(Mark::of(Change::Added).letter(), "A");
        assert_eq!(Mark::of(Change::Modified).letter(), "M");
        assert_eq!(Mark::of(Change::Deleted).letter(), "D");
        assert_eq!(Mark::of(Change::Renamed).letter(), "R");
        assert_eq!(
            Mark::of(Change::Copied).letter(),
            "R",
            "a copy draws as a move"
        );
        assert_eq!(Mark::of(Change::TypeChanged).letter(), "T");
        assert_eq!(Mark::Untracked.letter(), "?");

        // Porcelain v2's unmerged pairs: who added and who deleted is the whole
        // message, so the spelling is git's verbatim.
        for (state, letters) in [
            (ConflictKind::BothDeleted, "DD"),
            (ConflictKind::AddedByUs, "AU"),
            (ConflictKind::DeletedByThem, "UD"),
            (ConflictKind::AddedByThem, "UA"),
            (ConflictKind::DeletedByUs, "DU"),
            (ConflictKind::BothAdded, "AA"),
            (ConflictKind::BothModified, "UU"),
        ] {
            assert_eq!(conflict_letters(state), letters);
        }
    }

    #[test]
    fn a_conflict_row_carries_its_two_letter_state_and_a_path_keeps_its_bytes() {
        let rows = flatten(&sample_status());
        let merged = rows.iter().find_map(|e| match e {
            Entry::File(f) if f.section == Section::Conflicts => Some(f),
            _ => None,
        });
        let f = merged.expect("the conflicted file was flattened");
        assert_eq!(f.letters, "UU");
        assert_eq!(f.mark, Mark::Conflict);
        assert_eq!(
            f.path.as_bytes(),
            b"merged.rs",
            "addressing stays raw bytes"
        );
    }

    #[test]
    fn a_rename_travels_with_the_name_it_had() {
        let s = Status {
            staged: vec![StagedEntry {
                path: PathBytes::from("after.rs"),
                change: Change::Renamed,
                old_path: Some(PathBytes::from("before.rs")),
                kind: Kind::File,
                submodule: Submodule::default(),
            }],
            ..Default::default()
        };
        let rows = flatten(&s);
        let Entry::File(f) = &rows[1] else {
            panic!("row 1 is the file under the heading");
        };
        // The arrow is baked in at flatten, so the render path draws furniture
        // without building a string for it.
        assert_eq!(f.renamed_from.as_deref(), Some("← before.rs"));
        // And a plain modification carries none.
        let Entry::File(m) = &flatten(&sample_status())[1] else {
            panic!();
        };
        assert!(m.renamed_from.is_none());
    }

    #[test]
    fn a_non_utf8_path_keeps_its_bytes_and_displays_lossily() {
        // `café.txt` in Latin-1: git emits bytes no encoding claims, and the
        // model keeps them raw while the display string takes U+FFFD for the
        // one it cannot show.
        let s = Status {
            untracked: vec![UntrackedEntry {
                path: PathBytes::from_bytes(b"caf\xe9.txt"),
            }],
            ..Default::default()
        };
        let rows = flatten(&s);
        let Entry::File(f) = &rows[1] else {
            panic!("row 1 is the file under the heading");
        };
        assert_eq!(
            f.path.as_bytes(),
            b"caf\xe9.txt",
            "addressing keeps the bytes"
        );
        assert!(
            f.path_text.contains('\u{FFFD}'),
            "display decodes lossily instead of failing"
        );
    }

    #[test]
    fn navigation_moves_across_sections_and_clamps_at_both_ends() {
        let host = Host::new();
        let mut f = files(sample_status());
        with_height(&mut f, 4);
        let last = f.data.len() - 1;
        // Row 0 is the staged heading; the next is its first file.
        assert!(f.run_view("view.down", &host));
        assert!(f.current_file().is_some());
        for _ in 0..20 {
            f.run_view("view.down", &host);
        }
        assert_eq!(
            f.view.get().cursor(),
            last,
            "the last row clamps rather than wrapping"
        );
        assert!(f.run_view("view.top", &host));
        assert_eq!(f.view.get().cursor(), 0);
        assert!(f.run_view("view.bottom", &host));
        assert_eq!(f.view.get().cursor(), last);
        assert!(f.run_view("view.up", &host));
        assert_eq!(f.view.get().cursor(), last - 1);
    }

    #[test]
    fn sideways_commands_are_answered_without_doing_anything() {
        let host = Host::new();
        let mut f = files(sample_status());
        with_height(&mut f, 4);
        assert!(f.run_view("view.left", &host));
        assert!(f.run_view("view.right", &host));
        assert!(!f.pan_pixels(40.0));
        // And an unknown command says so rather than pretending. The write
        // verbs (`files.stage`, `files.commit`) are not in that company any
        // more — dispatch answers them before they ever reach this method,
        // because their work belongs to the job queue, not to a view.
        assert!(!f.run_view("files.discard", &host));
    }

    #[test]
    fn copy_falls_back_to_the_row_the_keyboard_is_on_as_git_would_spell_it() {
        let host = Host::new();
        let mut f = files(sample_status());
        with_height(&mut f, 8);
        // Down past the heading to the first file: `M src/main.rs`.
        f.run_view("view.down", &host);
        assert_eq!(f.cursor_text(), "M src/main.rs");
        // Back up onto the heading: nothing to copy, which copy.selection skips.
        f.run_view("view.up", &host);
        assert_eq!(f.cursor_text(), "");
        assert!(!f.select_all());
        assert!(!f.select_none());
    }

    #[test]
    fn replacement_keeps_the_cursor_on_its_file_within_its_section() {
        let host = Host::new();
        let mut f = files(sample_status());
        with_height(&mut f, 9);
        // Cursor onto `notes.md` (untracked): rows 0–5 are the staged heading,
        // its two files, the unstaged heading and file, then untracked's.
        f.run_view("view.top", &host);
        for _ in 0..6 {
            f.run_view("view.down", &host);
        }
        assert_eq!(f.cursor_text(), "? notes.md");

        // A refresh that adds a file above it in the same section shifts
        // notes.md down a row; the keyboard goes with the file.
        let mut next = sample_status();
        next.untracked.insert(0, untracked("aaa.md"));
        f.replace(next, &host);
        assert_eq!(f.cursor_text(), "? notes.md");
    }

    #[test]
    fn an_anchor_does_not_cross_sections_to_a_path_twin() {
        // `src/main.rs` sits in staged *and* unstaged. The cursor is on the
        // unstaged one; a refresh must not walk it up to the staged twin just
        // because that twin flattens first.
        let host = Host::new();
        let mut f = files(sample_status());
        with_height(&mut f, 9);
        f.run_view("view.top", &host);
        for _ in 0..4 {
            f.run_view("view.down", &host);
        }
        let before = f.current_file().expect("a file under the cursor");
        assert_eq!(
            (before.section, before.path.to_string_lossy().as_ref()),
            (Section::Unstaged, "src/main.rs")
        );

        // A staged addition above shifts every staged row; the anchor holds
        // the unstaged copy anyway.
        let mut next = sample_status();
        next.staged.insert(0, staged("aaa.rs", Change::Added));
        f.replace(next, &host);

        let after = f.current_file().expect("still a file");
        assert_eq!(
            after.section,
            Section::Unstaged,
            "the cursor walked across sections"
        );
        assert_eq!(after.path.as_bytes(), b"src/main.rs");
    }

    #[test]
    fn replacement_clamps_a_vanished_anchor_and_accepts_an_empty_tree() {
        let host = Host::new();
        let mut f = files(sample_status());
        with_height(&mut f, 4);
        f.run_view("view.bottom", &host);

        // Every file gone: the tree went clean mid-session.
        f.replace(Status::default(), &host);
        assert!(f.is_clean());
        assert_eq!((f.view.get().cursor(), f.view.get().top()), (0, 0));
        assert!(f.scroll.0.borrow().deferred_scroll_to_item.is_none());

        // And back to something: the cursor starts at the top again.
        f.replace(sample_status(), &host);
        assert_eq!(f.view.get().cursor(), 0);
    }

    #[test]
    fn a_heading_under_the_cursor_does_not_pretend_to_anchor() {
        let host = Host::new();
        let mut f = files(sample_status());
        with_height(&mut f, 9);
        f.run_view("view.top", &host); // cursor on the staged heading
        assert!(f.current_file().is_none());

        // A refresh that reorders nothing still lands the cursor somewhere
        // sane — on the same row index, since no file claimed the anchor.
        let mut next = sample_status();
        next.staged.insert(0, staged("aaa.rs", Change::Added));
        f.replace(next, &host);
        assert!(f.view.get().cursor() < f.data.len());
    }

    #[test]
    fn what_the_keyboard_is_on_names_the_group_and_the_path() {
        // The seam stage/unstage hangs off: both halves of "where a verb goes"
        // in one answer.
        let host = Host::new();
        let mut f = files(sample_status());
        with_height(&mut f, 9);
        for _ in 0..6 {
            f.run_view("view.down", &host);
        }
        let current = f.current_file().expect("a file under the cursor");
        assert_eq!(current.section, Section::Untracked);
        assert_eq!(current.path.as_bytes(), b"notes.md");
    }

    #[test]
    fn the_label_counts_what_changed_and_the_load_line_says_how_long_it_took() {
        let prepared = prepare(sample_status(), "gitten (main)");
        // Five entries, four paths: src/main.rs sits in staged and unstaged
        // and counts once.
        assert_eq!(prepared.label, "gitten (main) · 4 changed");

        let clean = prepare(Status::default(), "gitten (main)");
        assert_eq!(clean.label, "gitten (main) · 0 changed");
        assert!(clean.rows.is_empty(), "a clean tree flattens to nothing");
    }

    #[test]
    fn the_shipped_keymap_resolves_the_files_binding_through_the_registry() {
        use gitten_core::command::{Code, Commands, HelpRow, Key, Keymap, Modes, Resolve};

        // The command exists and the key resolves to it.
        let k = Keymap::builtin();
        assert_eq!(
            k.resolve(&Modes::new(), &[Key::char('2')]),
            Resolve::Run("files.focus")
        );
        assert_eq!(k.keys_for("files.focus"), vec!["2"]);
        assert_eq!(
            Key::parse("2"),
            Some(Key::plain(Code::Char('2'))),
            "the binding round-trips through a config file's spelling"
        );

        // And both registries feed the help projection directly, so the row
        // appears under [global] with no help-specific code anywhere.
        let rows = k.help(&Commands::builtin(), &Modes::new());
        let global = rows
            .iter()
            .position(|r| matches!(r, HelpRow::Mode(m) if m == "global"))
            .unwrap();
        assert!(rows[global..]
            .iter()
            .any(|r| matches!(r, HelpRow::Command { keys, doc } if keys == "2" && doc == "swap the working-tree list into the column")));
    }

    // ------------------------------------------------------- the discard arm

    /// Puts the keyboard on the unstaged `src/main.rs` — row 4 of the
    /// sample, past its heading.
    fn onto_unstaged(f: &mut Files, host: &Host) {
        f.run_view("view.top", host);
        for _ in 0..4 {
            f.run_view("view.down", host);
        }
    }

    /// The keyboard's row, owned — [`Files::current_file`] borrows the pane,
    /// and the arm API needs it mutable.
    fn under(f: &Files) -> (Section, PathBytes) {
        let current = f.current_file().expect("a file under the cursor");
        (current.section, current.path.clone())
    }

    #[test]
    fn a_discard_arms_then_confirms_on_the_second_press_of_the_same_row() {
        let host = Host::new();
        let mut f = files(sample_status());
        with_height(&mut f, 9);
        onto_unstaged(&mut f, &host);
        let (section, path) = under(&f);

        // First press: asked, not acted.
        assert!(!f.confirm_or_arm_discard(section, &path));
        assert_eq!(
            f.armed_row(),
            Some((Section::Unstaged, PathBytes::from("src/main.rs"))),
            "the arm names section and path together"
        );

        // Second press on the same row: act, and the slot is spent.
        assert!(f.confirm_or_arm_discard(section, &path));
        assert_eq!(f.armed_row(), None);

        // And a third press starts over: there is no latched yes.
        assert!(!f.confirm_or_arm_discard(section, &path));
    }

    #[test]
    fn arming_a_different_row_moves_the_question_rather_than_confirming_it() {
        let host = Host::new();
        let mut f = files(sample_status());
        with_height(&mut f, 9);
        onto_unstaged(&mut f, &host);
        let (unstaged_section, unstaged_path) = under(&f);
        assert!(!f.confirm_or_arm_discard(unstaged_section, &unstaged_path));

        // The same path under *staged* is a different row and a different
        // question; pressing there re-arms rather than executes.
        let staged = PathBytes::from("src/main.rs");
        assert!(!f.confirm_or_arm_discard(Section::Staged, &staged));
        assert_eq!(
            f.armed_row(),
            Some((Section::Staged, staged)),
            "one slot, moved — never two questions waiting"
        );
    }

    #[test]
    fn any_cursor_move_disarms_an_armed_discard() {
        let host = Host::new();
        let mut f = files(sample_status());
        with_height(&mut f, 9);
        onto_unstaged(&mut f, &host);
        let (section, path) = under(&f);
        assert!(!f.confirm_or_arm_discard(section, &path));

        // One step down — onto the untracked *heading*, as it happens: the
        // keyboard left the question's row either way.
        assert!(f.run_view("view.down", &host));
        assert!(f.current_file().is_none());
        // So the next press asks again about whatever it lands on instead
        // of executing the stale one.
        f.run_view("view.down", &host);
        let (section, path) = under(&f);
        assert!(!f.confirm_or_arm_discard(section, &path));

        // The scroll family moves attention too, even with the cursor still.
        assert!(f.run_view("view.scroll-down", &host));
        assert_eq!(
            f.armed_row(),
            None,
            "the question did not survive its own scroll away"
        );
    }

    #[test]
    fn a_refresh_disarms_even_when_the_file_survives() {
        let host = Host::new();
        let mut f = files(sample_status());
        with_height(&mut f, 9);
        onto_unstaged(&mut f, &host);
        let (section, path) = under(&f);
        assert!(!f.confirm_or_arm_discard(section, &path));

        // A refresh that changes nothing at all still says "things moved".
        f.replace(sample_status(), &host);
        assert_eq!(f.armed_row(), None);
        // The press after it re-arms rather than executes.
        assert!(!f.confirm_or_arm_discard(section, &path));
    }

    #[test]
    fn an_armed_discard_dies_when_its_file_vanishes_under_it() {
        // The race the double-press exists for: arm on a file, the tree goes
        // clean underneath, and the second press must not delete something
        // chosen against a tree that no longer exists.
        let host = Host::new();
        let mut f = files(Status {
            untracked: vec![untracked("notes.md")],
            ..Default::default()
        });
        with_height(&mut f, 4);
        f.run_view("view.bottom", &host); // onto notes.md
        let notes = PathBytes::from("notes.md");
        assert!(!f.confirm_or_arm_discard(Section::Untracked, &notes));

        f.replace(Status::default(), &host);
        assert_eq!(f.armed_row(), None, "the vanish took the question with it");
        // And the pane's own answer is empty-tree honest: nothing to arm.
        assert!(f.current_file().is_none());
    }

    #[test]
    fn byte_paths_arm_and_confirm_without_decoding() {
        // Latin-1 é in an untracked name: the arm holds the bytes it was
        // given, so the verb that finally runs aims at git's exact file.
        let host = Host::new();
        let mut f = files(Status {
            untracked: vec![UntrackedEntry {
                path: PathBytes::from_bytes(b"caf\xe9.txt"),
            }],
            ..Default::default()
        });
        with_height(&mut f, 4);
        f.run_view("view.bottom", &host);
        let (section, path) = under(&f);
        assert_eq!(path.as_bytes(), b"caf\xe9.txt");

        assert!(!f.confirm_or_arm_discard(section, &path));
        assert!(
            f.confirm_or_arm_discard(Section::Untracked, &PathBytes::from_bytes(b"caf\xe9.txt"))
        );
        assert_eq!(f.armed_row(), None);
    }

    #[test]
    fn whole_section_verbs_enumerate_their_own_side_in_draw_order() {
        let host = Host::new();
        let mut f = files(sample_status());
        with_height(&mut f, 9);
        assert_eq!(f.paths_in(Section::Staged).len(), 2);
        assert_eq!(
            f.paths_in(Section::Unstaged)
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
            vec!["src/main.rs"]
        );
        assert_eq!(
            f.paths_in(Section::Untracked)
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
            vec!["notes.md"]
        );
        assert!(f.paths_in(Section::Conflicts).len() == 1);

        // And the side the keyboard sits in, headings included: the eye
        // reading "[staged]" is in staged for a stage-all's purposes.
        f.run_view("view.top", &host);
        assert_eq!(
            f.cursor_section(),
            Some(Section::Staged),
            "the heading counts as its section"
        );
        f.run_view("view.bottom", &host);
        assert_eq!(f.cursor_section(), Some(Section::Conflicts));
    }

    #[test]
    fn the_arm_question_says_delete_for_a_file_with_nothing_to_go_back_to() {
        assert_eq!(
            discard_question(Section::Untracked, "notes.md"),
            "delete notes.md? press again to confirm",
            "an untracked file has no earlier version; the word is honest"
        );
        assert_eq!(
            discard_question(Section::Unstaged, "src/x.rs"),
            "discard src/x.rs? press again to confirm"
        );
    }
}
