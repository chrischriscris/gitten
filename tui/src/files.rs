//! The working tree, as a list of cells.
//!
//! The terminal's tenant over [`gitten_core::status::Status`] — the same
//! porcelain-v2 facts the window's files pane draws, flattened the same way
//! and drawn in a different medium: a section only when it has something in
//! it, one heading with its count, then that section's files, in
//! staged → unstaged → untracked → conflicts order. What a verb needs of a
//! row — which side of the index, and the path byte for byte — travels beside
//! what an eye needs of it, because the same path may sit in two sections at
//! once and a verb aimed at the displayed spelling of a Latin-1 filename
//! would hit nothing.
//!
//! The list idioms are [`crate::commits`]'s, on purpose: one
//! [`gitten_core::view::Viewport`], the rows flattened **once per refresh**
//! into owned display strings so the render path allocates nothing per frame,
//! the scrollbar over the pane's own last column, and the cursor never
//! resting on a heading. What is this pane's alone is the armed discard —
//! the one destructive verb confirms on the keyboard, the exact pattern the
//! window runs — and the honest empty states: a clean tree and a status read
//! that failed are two different things here, said in two different ways.

use crate::screen::{width, Ink, Pen, Screen};
use crate::scrollbar::{self, Bar};
use gitten_core::host::Host;
use gitten_core::status::{Change, ConflictKind, PathBytes, Status};
use gitten_core::view::Viewport;
use std::collections::HashSet;

/// The four questions a status panel asks, in draw order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Staged,
    Unstaged,
    Untracked,
    Conflicts,
}

impl Section {
    /// The heading drawn over the group, in the caps the design gives it.
    /// Static, so the row's frame spells nothing.
    pub fn label(self) -> &'static str {
        match self {
            Section::Staged => "STAGED",
            Section::Unstaged => "UNSTAGED",
            Section::Untracked => "UNTRACKED",
            Section::Conflicts => "CONFLICTS",
        }
    }

    /// Draw order — [`Status`]'s own listing order, oldest question first.
    fn all() -> [Section; 4] {
        [
            Section::Staged,
            Section::Unstaged,
            Section::Untracked,
            Section::Conflicts,
        ]
    }
}

/// What a status letter means, once you get past which side of the index it
/// is about — which is what decides its colour, and nothing else. The same
/// mapping the window's files pane makes; a theme field is a client decision
/// and this is the terminal's copy of it.
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

    /// The ink each state draws in — the window's own mapping: adds and
    /// deletes borrow the diff palette where those words already have
    /// colours, modify takes the chrome accent, a rename steps onto the
    /// graph's first lane, and the rest take the quiet furniture inks.
    fn color(self, host: &Host) -> gitten_core::theme::Rgb {
        let t = &host.theme;
        match self {
            Mark::Add => t.diff.adds_fg,
            Mark::Delete => t.diff.dels_fg,
            Mark::Conflict => t.chrome.error,
            Mark::Modify => t.chrome.accent,
            Mark::Rename => t.lanes.first().copied().unwrap_or(t.chrome.accent),
            Mark::TypeChange => t.chrome.dim,
            Mark::Untracked => t.chrome.faint,
        }
    }
}

/// The two-letter state of a conflicted path, exactly as porcelain v2 spells
/// it — who added and who deleted decides what resolving means, so the
/// letters are data and not decoration.
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

/// One flat row of the pane: a section heading or one file.
///
/// Flattened once per refresh — never per frame. Everything a draw needs that
/// costs allocation (the lossy path text, the rename arrow, the spelled-out
/// count) is computed at flatten time; what a draw reads per frame is an enum
/// match and a live theme lookup.
pub enum Entry {
    /// A group heading, drawn only because the group under it is non-empty.
    Heading {
        section: Section,
        /// How many files are in the group, spelled out once.
        count: String,
    },
    File(FileRow),
}

/// One file of the working tree.
#[derive(Debug, Clone)]
pub struct FileRow {
    /// Which group it sits under — what a verb needs to know where its work
    /// goes, and half of the refresh anchor.
    pub section: Section,
    /// The addressing form, byte for byte. Never decoded in place: this is
    /// what a stage, a discard or an ignore is aimed at.
    pub path: PathBytes,
    /// The display form, decoded lossily once at flatten. Drawing's copy, and
    /// nobody else's.
    pub text: String,
    /// What a rename moved it from, arrow baked in at flatten — furniture
    /// drawn dim, and no string built for it on the render path.
    pub origin: Option<String>,
    /// The letter(s) themselves, git's own spelling: `A`, `M`, `UU`, `?`.
    pub letters: &'static str,
    /// What the letters mean and what colour they draw in, as one.
    pub mark: Mark,
    /// Where this row sits among the pane's files, from one — what the status
    /// line says without counting anything per frame.
    pub n: usize,
}

/// [`Status`] flattened to rows, plus the header label. Pure — this is the
/// unit-tested half of a refresh, and everything the pane stores beside its
/// viewport comes out of it.
pub struct Prepared {
    pub rows: Vec<Entry>,
    /// The header strip: who we are and how much changed. The count is
    /// **distinct paths** — one file staged and edited again sits in two
    /// lists and is still one change to a person, the same number the
    /// window's label spells.
    pub label: String,
}

/// Flattens a status into display rows: one heading per non-empty section,
/// then that section's files, in [`Section::all`] order.
pub fn prepare(status: &Status, describe: &str) -> Prepared {
    let mut rows: Vec<Entry> = Vec::new();
    for section in Section::all() {
        let files: Vec<FileRow> = match section {
            Section::Staged => status
                .staged
                .iter()
                .map(|e| {
                    let mark = Mark::of(e.change);
                    file_row(section, &e.path, e.old_path.as_ref(), mark, mark.letter())
                })
                .collect(),
            Section::Unstaged => status
                .unstaged
                .iter()
                .map(|e| {
                    let mark = Mark::of(e.change);
                    file_row(section, &e.path, None, mark, mark.letter())
                })
                .collect(),
            Section::Untracked => status
                .untracked
                .iter()
                .map(|e| {
                    file_row(
                        section,
                        &e.path,
                        None,
                        Mark::Untracked,
                        Mark::Untracked.letter(),
                    )
                })
                .collect(),
            Section::Conflicts => status
                .conflicts
                .iter()
                .map(|e| {
                    file_row(
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
        rows.push(Entry::Heading {
            section,
            count: files.len().to_string(),
        });
        rows.extend(files.into_iter().map(Entry::File));
    }
    // The file ordinal each row carries, in draw order — the status line's
    // `3/15 files` costs one lookup per frame and nothing more.
    let mut n = 0;
    for row in &mut rows {
        if let Entry::File(f) = row {
            n += 1;
            f.n = n;
        }
    }
    let mut seen = HashSet::new();
    let changed = rows
        .iter()
        .filter_map(|r| match r {
            Entry::File(f) => Some(&f.path),
            Entry::Heading { .. } => None,
        })
        .filter(|p| seen.insert(*p))
        .count();
    Prepared {
        rows,
        label: format!("{describe} · {changed} changed"),
    }
}

fn file_row(
    section: Section,
    path: &PathBytes,
    old: Option<&PathBytes>,
    mark: Mark,
    letters: &'static str,
) -> FileRow {
    FileRow {
        section,
        path: path.clone(),
        text: path.to_string_lossy().into_owned(),
        origin: old.map(|p| format!("← {}", p.to_string_lossy())),
        letters,
        mark,
        n: 0,
    }
}

/// What an armed discard asks, once, on the status line. An untracked file
/// says *delete* because that is what discarding means when there is no
/// earlier version to go back to — the honest word for the one mechanics
/// where nothing is recoverable.
pub fn discard_question(section: Section, shown: &str) -> String {
    match section {
        Section::Untracked => format!("delete {shown}? press again to confirm"),
        _ => format!("discard {shown}? press again to confirm"),
    }
}

/// The header label of a files pane whose first status read failed.
///
/// Deliberately not `· 0 changed`: a read that did not come back must never
/// be drawn where a clean tree goes. The tenant is still registered and
/// still refreshes — the next successful read replaces both the rows and
/// this sentence.
pub fn unavailable_label(describe: &str) -> String {
    format!("{describe} · status unavailable")
}

/// The working-tree pane: flattened rows, a viewport, and the discard that
/// is waiting for its second press.
///
/// Knows nothing about keys or jobs — every method is a command or a read,
/// exactly as in [`crate::commits`]. The verbs themselves live in the app,
/// which reads [`Files::current_file`] and [`Files::paths_in`] and builds
/// the write from those; this pane holds the *confirmation* state, because
/// what the second press confirms is a row of this list.
pub struct Files {
    rows: Vec<Entry>,
    /// The cursor, the top row and the height — [`Viewport`], the same model
    /// every other list holds.
    view: Viewport,
    cols: usize,
    bar: Bar,
    /// Whether the rows came from a successful status read. `false` only for
    /// the tenant a failed *initial* read registers: it draws `status
    /// unavailable` where a clean tree draws `working tree clean`, and the
    /// first successful refresh stands it up.
    available: bool,
    /// The discard awaiting its second press: the section and path of the row
    /// that asked. One slot — arming a different row moves the question, it
    /// does not queue two. Section **and** path, because the same path can
    /// sit in staged and unstaged and the question is about one of them.
    /// Outliving a switch to another pane and back is deliberate: the
    /// question still sits on the row it was asked about, and only a cursor
    /// move, a wheel or a refresh can make its answer stale — none of which
    /// is a focus change.
    armed: Option<(Section, PathBytes)>,
    /// Where in the scrollbar's thumb it was taken hold of, while it is held.
    grabbed: Option<usize>,
    /// How many of the rows are files — the status line's denominator, kept
    /// by the same pass that numbers the rows.
    total: usize,
    /// Whether the pane has had its opening settle. It cannot happen at
    /// construction: a viewport with no height keeps its top on its cursor,
    /// so settling before the first `resize` would open the pane scrolled
    /// past the heading it is meant to show. The first size is when there is
    /// a viewport to open onto.
    opened: bool,
}

impl Files {
    /// A pane over a successful read — a clean tree included, which is rows
    /// and nothing else. The cursor opens on the first file, past the first
    /// heading: a heading is a label over rows, and a verb aimed at one has
    /// nowhere to go.
    pub fn new(rows: Vec<Entry>) -> Self {
        let mut view = Viewport::new();
        view.set_len(rows.len());
        // A non-empty flatten always puts a heading at row 0 and its first
        // file at row 1, so opening on the first file is a `go_to`, not a
        // search — and it happens here, not at the first resize, because a
        // verb dispatched before the pane has ever been drawn still needs a
        // cursor that names a row it can act on. A heading is a label over
        // rows, and a verb aimed at one has nowhere to go.
        if !rows.is_empty() {
            view.go_to(1);
        }
        let total = rows.iter().filter(|r| matches!(r, Entry::File(_))).count();
        Self {
            rows,
            view,
            cols: 0,
            bar: Bar::default(),
            available: true,
            armed: None,
            grabbed: None,
            total,
            opened: false,
        }
    }

    /// The pane a failed initial read registers: retryable, registered, and
    /// honest about having nothing. [`Files::replace`] stands it up on the
    /// first read that comes back.
    pub fn unavailable() -> Self {
        Self {
            rows: Vec::new(),
            view: Viewport::new(),
            cols: 0,
            bar: Bar::default(),
            available: false,
            armed: None,
            grabbed: None,
            total: 0,
            opened: false,
        }
    }

    /// Whether this pane holds a successful read at all — the tests' name for
    /// the clean-versus-unavailable split the paint makes.
    pub fn is_available(&self) -> bool {
        self.available
    }

    /// Whether the working tree had nothing to say. A failed read is *not*
    /// clean, however empty its rows are.
    pub fn is_clean(&self) -> bool {
        self.available && self.rows.is_empty()
    }

    /// How many columns the pane draws into, and how many rows it shows.
    /// The first size with rows under it also opens the pane: the cursor is
    /// already on its first file (it was settled at construction), so the
    /// opening is a matter of putting the heading above it back on screen —
    /// which a viewport sized later cannot do on its own when the height is
    /// too small to keep a scroll margin. A one-row pane has room for the
    /// cursor and nothing else.
    pub fn resize(&mut self, cols: usize, height: usize) {
        self.cols = cols;
        self.view.set_height(height);
        if !self.opened && height > 0 {
            self.opened = true;
            self.settle(0);
            if height > 1 {
                self.view.scroll_to(0);
            }
        }
    }

    /// How much lead the cursor keeps at the edge. `[view] scrolloff`.
    pub fn set_scrolloff(&mut self, rows: usize) {
        self.view.set_scrolloff(rows);
    }

    /// The glyphs the scrollbar is drawn with. `--ascii`, or an extension.
    pub fn set_bar(&mut self, bar: Bar) {
        self.bar = bar;
    }

    /// The cursor's row, for whatever reads it.
    pub fn cursor(&self) -> usize {
        self.view.cursor()
    }

    /// The first row drawn.
    pub fn top(&self) -> usize {
        self.view.top()
    }

    /// Moves the cursor off a row that cannot hold it — a heading — onward
    /// from `from`, in the direction it was travelling. Called after every
    /// cursor move and every refresh, so the cursor never rests on a
    /// heading.
    fn settle(&mut self, from: usize) {
        self.view
            .settle(from, |i| matches!(self.rows.get(i), Some(Entry::File(_))));
    }

    /// Swaps in refreshed rows, keeping the cursor anchored to its file.
    ///
    /// Only a file anchors, and on its **section and path together**: the
    /// same path can sit in staged *and* unstaged, and anchoring on the bare
    /// path would walk the cursor to whichever twin flattens first. A
    /// heading is a fact about the last refresh's grouping, not a thing the
    /// eye was reading. A file that vanished falls back to clamping, like a
    /// commit list whose sha left the log.
    ///
    /// A refresh is the repository saying things moved; an armed discard was
    /// a promise about how they were, so it dies here first.
    pub fn replace(&mut self, rows: Vec<Entry>) {
        self.armed = None;
        let old = self.view;
        let anchored = match self.rows.get(old.cursor()) {
            Some(Entry::File(f)) => Some((f.section, f.path.clone())),
            _ => None,
        };
        self.rows = rows;
        self.total = self
            .rows
            .iter()
            .filter(|r| matches!(r, Entry::File(_)))
            .count();
        // A read came back: whatever the pane said before, it says no longer.
        self.available = true;
        let mut view = old;
        view.set_len(self.rows.len());
        let cursor = anchored
            .and_then(|(section, path)| {
                self.rows.iter().position(
                    |e| matches!(e, Entry::File(f) if f.section == section && f.path == path),
                )
            })
            .unwrap_or_else(|| view.cursor());
        view.go_to(cursor);
        self.view = view;
        // A vanished anchor can leave the cursor on whatever heading took its
        // row; the direction is "where it was", so it walks on to the next
        // file rather than back to the previous section's last.
        self.settle(old.cursor());
    }

    // ------------------------------------------------------------------ verbs

    /// What the keyboard is on: the whole file row — section and path
    /// together, which is what a stage verb needs to know where its work
    /// goes. `None` only on an empty or unavailable tree, since the cursor
    /// never rests on a heading.
    pub fn current_file(&self) -> Option<&FileRow> {
        match self.rows.get(self.view.cursor()) {
            Some(Entry::File(f)) => Some(f),
            _ => None,
        }
    }

    /// Which section the keyboard sits *in* — the side of the index under the
    /// keyboard decides where a whole-section verb goes.
    pub fn cursor_section(&self) -> Option<Section> {
        match self.rows.get(self.view.cursor()) {
            Some(Entry::Heading { section, .. }) => Some(*section),
            Some(Entry::File(f)) => Some(f.section),
            None => None,
        }
    }

    /// Every path flattened under one section, in draw order — what a
    /// whole-section verb acts on. Bytes throughout, because these aim
    /// verbs; the display forms live only in the rows.
    pub fn paths_in(&self, section: Section) -> Vec<PathBytes> {
        self.rows
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
    /// (a different row, after a move or refresh cleared it) re-arms onto the
    /// new target and returns false again, so there is no state here a
    /// caller has to remember.
    pub fn confirm_or_arm_discard(&mut self, section: Section, path: &PathBytes) -> bool {
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

    /// Whether a discard is waiting for its second press — the paint's tint
    /// of the row the question is about, and the tests' window on it.
    pub fn armed_row(&self) -> Option<(Section, PathBytes)> {
        self.armed.clone()
    }

    /// The row an armed discard sits on, found per frame — the tint is a
    /// property of the question, not of the draw.
    fn armed_index(&self) -> Option<usize> {
        self.armed.as_ref().and_then(|(section, path)| {
            self.rows.iter().position(
                |e| matches!(e, Entry::File(f) if f.section == *section && f.path == *path),
            )
        })
    }

    // -------------------------------------------------------------- commands

    /// One move of the cursor, past whatever heading it lands on, and the
    /// arm it drops. Every keyboard move is a move of attention: whatever
    /// was armed was armed to what the keyboard used to be on.
    fn move_by(&mut self, by: isize) {
        let from = self.view.cursor();
        self.view.move_by(by);
        self.settle(from);
        self.armed = None;
    }

    pub fn down(&mut self) {
        self.move_by(1);
    }

    pub fn up(&mut self) {
        self.move_by(-1);
    }

    pub fn page(&mut self, pages: isize) {
        let from = self.view.cursor();
        self.view.page(pages);
        self.settle(from);
        self.armed = None;
    }

    /// Scrolls without moving the cursor further than it has to — the wheel.
    /// Also a move of attention, and it disarms like one.
    pub fn scroll_y(&mut self, by: isize) {
        let from = self.view.cursor();
        self.view.scroll_by(by);
        self.settle(from);
        self.armed = None;
    }

    pub fn to_top(&mut self) {
        self.view.to_top();
        self.settle(0);
        self.armed = None;
    }

    pub fn to_bottom(&mut self) {
        self.view.to_bottom();
        self.settle(self.rows.len().saturating_sub(1));
        self.armed = None;
    }

    /// A press in the list: the cursor moves there, unless the press landed
    /// on the scrollbar — its own last column, not the screen's.
    ///
    /// A press on another row takes the armed question's row out from under
    /// it; a press on the same row leaves the question standing. There is no
    /// drag selection over a file list: the mouse moves the cursor and
    /// nothing else.
    pub fn press(&mut self, col: usize, row: usize, _clicks: u8, _extend: bool, host: &Host) {
        if scrollbar::hit(col, self.cols, &self.view, host) {
            let row = row.min(self.view.height().saturating_sub(1));
            self.grabbed = Some(scrollbar::grab(&mut self.view, host, row));
            return;
        }
        let Some(index) = self.view.row_at(row) else {
            return;
        };
        let armed_on = self.armed_index();
        let from = self.view.cursor();
        self.view.go_to(index);
        self.settle(from);
        if self.armed.is_some() && Some(self.view.cursor()) != armed_on {
            self.armed = None;
        }
    }

    /// The pointer moved with the button down. Only the scrollbar's own grab
    /// means anything — a file list has no drag selection to grow.
    pub fn drag(&mut self, _col: usize, row: isize, host: &Host) {
        if let Some(grabbed) = self.grabbed {
            scrollbar::drag(&mut self.view, host, row.max(0) as usize, grabbed);
        }
    }

    pub fn release(&mut self) {
        self.grabbed = None;
    }

    /// What `copy.selection` copies here: the row the keyboard is on, as git
    /// would spell it — letters, then path. A heading copies nothing, which
    /// is what makes the empty result skip the clipboard entirely.
    pub fn copy_text(&self) -> String {
        match self.current_file() {
            Some(f) => format!("{} {}", f.letters, f.path),
            None => String::new(),
        }
    }

    /// What the *mouse* has selected — nothing, ever: a file list has no
    /// drag selection, so copy-on-select has nothing to fire on.
    pub fn selection(&self) -> String {
        String::new()
    }

    /// `select.all` is inert here, the same answer the commit graph gives.
    pub fn select_all(&mut self) {}

    pub fn select_none(&mut self) -> bool {
        false
    }

    /// One line describing where the keyboard is, for the status row. The
    /// ordinal and the denominator are both file counts, decided by the
    /// same flatten that numbered the rows — nothing here counts per frame.
    pub fn status(&self) -> String {
        if !self.available {
            return "unavailable".into();
        }
        if self.total == 0 {
            return "clean".into();
        }
        let at = match self.rows.get(self.view.cursor()) {
            Some(Entry::File(f)) => f.n,
            _ => 0,
        };
        format!("{at}/{} files", self.total)
    }

    // ------------------------------------------------------------------ draw

    /// Draws the visible rows into `screen`, at `x` of row `y` onward, inside
    /// this pane's own columns.
    ///
    /// Every row goes through [`Screen::span`], never [`Screen::row`]: the
    /// pane is a guest in the row, and a long path that wrote to the whole
    /// screen would overwrite the divider and whatever sits beside it. The
    /// cursor background runs the full width only when this pane holds the
    /// keyboard — `focused` is the caller's answer, not something the view
    /// knows.
    pub fn paint(&self, screen: &mut Screen, x: usize, y: usize, focused: bool, host: &Host) {
        let c = &host.theme.chrome;
        let plain = Ink::new(c.fg, c.bg);
        // An empty pane is a quiet line, not an empty box — and *which* quiet
        // line is the honesty this pane owes: a clean tree and a read that
        // did not come back are not the same sentence.
        if self.rows.is_empty() {
            let mut pen = screen.span(y, x, self.cols);
            pen.put(
                match self.available {
                    true => "working tree clean",
                    false => "status unavailable",
                },
                Ink::new(c.faint, c.bg),
            );
            return;
        }
        let armed = self.armed_index();
        for i in 0..self.view.height() {
            let row = y + i;
            let Some(index) = self.view.row_at(i) else {
                screen.span(row, x, self.cols).wash(plain);
                continue;
            };
            let bg = match focused && index == self.view.cursor() {
                true => c.selection_bg,
                false => c.bg,
            };
            let mut pen = screen.span(row, x, self.cols);
            self.row(&mut pen, index, bg, host, armed == Some(index));
        }
        if self.cols > 0 {
            // Last, and over the rows rather than beside them — at this
            // pane's own last column, which is not the screen's.
            scrollbar::paint(screen, self.bar, x + self.cols - 1, y, &self.view, host);
        }
    }

    /// One row: a quiet caps heading with its count at the right, or a
    /// two-cell status column in its mark's colour beside the path — and the
    /// whole thing in `chrome.error` while this is the row an armed discard
    /// is waiting on, so the thing a second press will destroy is named by
    /// its own colour and not only by the band above it.
    ///
    /// The row's background travels in every piece's ink, the way the commit
    /// list draws — `wash` is the *last* write, running whatever background
    /// the row earned out to the edge, not a first coat the text goes on
    /// top of.
    fn row(
        &self,
        pen: &mut Pen,
        index: usize,
        bg: gitten_core::theme::Rgb,
        host: &Host,
        armed: bool,
    ) {
        let c = &host.theme.chrome;
        match &self.rows[index] {
            Entry::Heading { section, count } => {
                pen.put(section.label(), Ink::new(c.faint, bg));
                // The count at the pane's right edge, clear of the scrollbar
                // column, and never pushed backwards by a label that does not
                // fit.
                let at = self.cols.saturating_sub(1 + width(count));
                pen.fill(at.saturating_sub(pen.col()), ' ', Ink::new(c.fg, bg));
                pen.seek(at.max(pen.col()));
                pen.put(count, Ink::new(c.faint, bg));
                pen.wash(Ink::new(c.fg, bg));
            }
            Entry::File(f) => {
                let letters = match armed {
                    true => Ink::new(c.error, bg),
                    false => Ink::new(f.mark.color(host), bg),
                };
                let text = match armed {
                    true => Ink::new(c.error, bg),
                    false => Ink::new(c.fg, bg),
                };
                // Two cells: a conflict's XY pair is the widest thing git
                // puts there.
                pen.take(2).put(f.letters, letters);
                pen.put(" ", text);
                pen.put(&f.text, text);
                if let Some(origin) = &f.origin {
                    pen.put(" ", Ink::new(c.fg, bg));
                    pen.put(origin, Ink::new(c.faint, bg));
                }
                pen.wash(text);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gitten_core::status::{
        ConflictEntry, Kind, StagedEntry, Submodule, UnstagedEntry, UntrackedEntry,
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

    fn renamed(path: &str, from: &str, change: Change) -> StagedEntry {
        StagedEntry {
            old_path: Some(PathBytes::from(from)),
            ..staged(path, change)
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

    /// Every `Change`, all seven conflict states, a rename, a path in two
    /// sections at once, an untracked file, and a path no encoding claims —
    /// `café.txt` in Latin-1, the byte sequence a lossy boundary mangles.
    fn full_status() -> Status {
        Status {
            staged: vec![
                staged("added.rs", Change::Added),
                staged("gone.rs", Change::Deleted),
                renamed("moved.rs", "before.rs", Change::Renamed),
                staged("typed.rs", Change::TypeChanged),
                renamed("copied.rs", "origin.rs", Change::Copied),
            ],
            unstaged: vec![
                unstaged("gone.rs", Change::Deleted),
                unstaged("edited.rs", Change::Modified),
            ],
            untracked: vec![
                untracked("notes.md"),
                UntrackedEntry {
                    path: PathBytes::from_bytes(b"caf\xe9.txt"),
                },
            ],
            conflicts: vec![
                conflict("dd.rs", ConflictKind::BothDeleted),
                conflict("au.rs", ConflictKind::AddedByUs),
                conflict("ud.rs", ConflictKind::DeletedByThem),
                conflict("ua.rs", ConflictKind::AddedByThem),
                conflict("du.rs", ConflictKind::DeletedByUs),
                conflict("aa.rs", ConflictKind::BothAdded),
                conflict("uu.rs", ConflictKind::BothModified),
            ],
            ignored: vec![],
        }
    }

    /// The small tree the navigation tests walk: the same twin-path shape,
    /// fewer rows.
    fn twin_status() -> Status {
        Status {
            staged: vec![
                staged("added.rs", Change::Added),
                staged("gone.rs", Change::Deleted),
            ],
            unstaged: vec![
                unstaged("gone.rs", Change::Deleted),
                unstaged("edited.rs", Change::Modified),
            ],
            untracked: vec![untracked("notes.md")],
            conflicts: vec![conflict("uu.rs", ConflictKind::BothModified)],
            ignored: vec![],
        }
    }

    /// Headings and files in draw order — the shape the tests read.
    fn outline(rows: &[Entry]) -> Vec<String> {
        rows.iter()
            .map(|e| match e {
                Entry::Heading { section, count } => format!("[{}·{count}]", section.label()),
                Entry::File(f) => format!("{} {}", f.letters, f.text),
            })
            .collect()
    }

    /// A pane over a status, at a size, as a refresh would leave it.
    fn view(status: &Status, cols: usize, height: usize) -> (Files, Host) {
        let prepared = prepare(status, "test (main)");
        let mut f = Files::new(prepared.rows);
        f.resize(cols, height);
        (f, Host::new())
    }

    /// What the pane drew, at `x`, one string per visible row.
    fn painted(f: &Files, host: &Host, x: usize, w: usize) -> Vec<String> {
        let mut screen = Screen::new(w, f.view.height().max(1));
        screen.clear(Ink::new(host.theme.chrome.fg, host.theme.chrome.bg));
        f.paint(&mut screen, x, 0, true, host);
        (0..f.view.height()).map(|y| screen.row_text(y)).collect()
    }
    /// The keyboard's row, owned.
    fn under(f: &Files) -> (Section, PathBytes) {
        let current = f.current_file().expect("a file under the cursor");
        (current.section, current.path.clone())
    }

    #[test]
    fn sections_render_in_porcelain_order_with_counts() {
        let prepared = prepare(&full_status(), "repo (main)");
        assert_eq!(
            outline(&prepared.rows),
            vec![
                "[STAGED·5]",
                "A added.rs",
                "D gone.rs",
                "R moved.rs",
                "T typed.rs",
                "R copied.rs",
                "[UNSTAGED·2]",
                "D gone.rs",
                "M edited.rs",
                "[UNTRACKED·2]",
                "? notes.md",
                "? caf\u{FFFD}.txt",
                "[CONFLICTS·7]",
                "DD dd.rs",
                "AU au.rs",
                "UD ud.rs",
                "UA ua.rs",
                "DU du.rs",
                "AA aa.rs",
                "UU uu.rs",
            ],
            "one heading per non-empty section, then its files, git's letters"
        );
        // The label counts **distinct paths**: gone.rs sits in two sections
        // and is one change.
        assert_eq!(prepared.label, "repo (main) · 15 changed");
        // An empty tree flattens to nothing and says zero.
        let clean = prepare(&Status::default(), "repo (main)");
        assert!(clean.rows.is_empty());
        assert_eq!(clean.label, "repo (main) · 0 changed");
    }

    #[test]
    fn a_rename_travels_with_the_name_it_had_and_a_path_keeps_its_bytes() {
        let rows = prepare(&full_status(), "").rows;
        let moved = rows
            .iter()
            .find_map(|e| match e {
                Entry::File(f) if f.text == "moved.rs" => Some(f),
                _ => None,
            })
            .expect("the rename was flattened");
        assert_eq!(moved.origin.as_deref(), Some("← before.rs"));
        let copied = rows
            .iter()
            .find_map(|e| match e {
                Entry::File(f) if f.text == "copied.rs" => Some(f),
                _ => None,
            })
            .expect("the copy was flattened");
        assert_eq!(copied.origin.as_deref(), Some("← origin.rs"));
        // A plain modification carries no origin.
        let edited = rows
            .iter()
            .find_map(|e| match e {
                Entry::File(f) if f.text == "edited.rs" => Some(f),
                _ => None,
            })
            .expect("the edit was flattened");
        assert!(edited.origin.is_none());

        // The Latin-1 path: addressing keeps the bytes, display decodes
        // lossily, and the verb's byte form is the raw one end to end.
        let caf = rows
            .iter()
            .find_map(|e| match e {
                Entry::File(f) if f.path.as_bytes() == b"caf\xe9.txt" => Some(f),
                _ => None,
            })
            .expect("the raw path was flattened by bytes, not by text");
        assert!(
            caf.text.contains('\u{FFFD}'),
            "display decodes lossily instead of failing"
        );
        let untracked = rows
            .iter()
            .find_map(|e| match e {
                Entry::File(f) if f.section == Section::Untracked && f.text == "notes.md" => {
                    Some(f)
                }
                _ => None,
            })
            .expect("notes.md under untracked");
        assert_eq!(untracked.letters, "?");
        assert_eq!(untracked.mark, Mark::Untracked);
    }

    #[test]
    fn verbs_aim_at_raw_bytes_and_the_rows_are_distinct_pairs() {
        let mut f = Files::new(prepare(&full_status(), "").rows);
        f.resize(40, 24);
        // Same path, two sections: two rows, each naming its own side.
        let staged_list = f.paths_in(Section::Staged);
        let unstaged_list = f.paths_in(Section::Unstaged);
        let staged = staged_list
            .iter()
            .find(|p| p.as_bytes() == b"gone.rs")
            .expect("staged twin");
        let unstaged = unstaged_list
            .iter()
            .find(|p| p.as_bytes() == b"gone.rs")
            .expect("unstaged twin");
        assert_eq!(staged.as_bytes(), b"gone.rs");
        assert_eq!(unstaged.as_bytes(), b"gone.rs");
        assert_ne!(
            staged_list.len(),
            unstaged_list.len(),
            "the sections are separate lists"
        );
        // Conflicts are their own list and never a stage-all target by
        // accident of flattening.
        assert_eq!(f.paths_in(Section::Conflicts).len(), 7);
        // The raw bytes ride through to whoever builds a verb.
        assert!(f
            .paths_in(Section::Untracked)
            .iter()
            .any(|p| p.as_bytes() == b"caf\xe9.txt"));
    }

    #[test]
    fn the_marks_draw_in_the_theme_colours_a_frame_resolved() {
        let (f, host) = view(&full_status(), 40, 14);
        // Row 0 is the staged heading; row 1 the first file — the cursor's
        // own row, so its background is the selection's and its letters are
        // still the mark's.
        let rows = painted(&f, &host, 0, 40);
        assert!(rows[0].contains("STAGED"), "{:?}", rows[0]);
        let added_ink = {
            let mut screen = Screen::new(40, 14);
            screen.clear(Ink::new(host.theme.chrome.fg, host.theme.chrome.bg));
            f.paint(&mut screen, 0, 0, false, &host);
            screen.ink(0, 1).unwrap()
        };
        assert_eq!(
            added_ink.fg, host.theme.diff.adds_fg,
            "an addition draws green"
        );
        let mut screen = Screen::new(40, 14);
        screen.clear(Ink::new(host.theme.chrome.fg, host.theme.chrome.bg));
        f.paint(&mut screen, 0, 0, false, &host);
        assert_eq!(
            screen.ink(0, 2).unwrap().fg,
            host.theme.diff.dels_fg,
            "a deletion draws the diff's red"
        );
        // And the ink is live at paint: the same pane over a host whose
        // accent moved draws the modify mark in the new accent, with no
        // rebuild.
        let mut hot = Host::new();
        hot.theme.chrome.accent = 0x112233;
        let mut screen = Screen::new(40, 14);
        screen.clear(Ink::new(hot.theme.chrome.fg, hot.theme.chrome.bg));
        // edited.rs sits under the unstaged heading, a few rows down.
        f.paint(&mut screen, 0, 0, false, &hot);
        let m = (0..14)
            .find(|y| screen.row_text(*y).contains("edited.rs"))
            .expect("the modified row was drawn");
        assert_eq!(
            screen.ink(0, m).unwrap().fg,
            0x112233,
            "the mark's ink was frozen at flatten"
        );
    }

    #[test]
    fn rows_clip_inside_a_nonzero_span_and_own_their_scrollbar() {
        let (f, host) = view(&full_status(), 20, 6);
        let mut screen = Screen::new(40, 6);
        screen.clear(Ink::new(host.theme.chrome.fg, host.theme.chrome.bg));
        // Sentinels on both sides of the pane, so anything painted outside
        // its span is visible rather than merely absent.
        for y in 0..6 {
            screen.over(9, y, '╎', host.theme.chrome.accent);
            screen.over(31, y, '╎', host.theme.chrome.accent);
        }
        f.paint(&mut screen, 10, 0, true, &host);
        for y in 0..6 {
            assert_eq!(
                screen.char_at(9, y),
                Some('╎'),
                "row {y} crossed the left edge"
            );
            assert_eq!(
                screen.char_at(31, y),
                Some('╎'),
                "row {y} crossed the right edge"
            );
        }
        assert!(
            (10..31).any(|x| screen.char_at(x, 1).is_some_and(|c| c != ' ')),
            "nothing was drawn inside the pane"
        );
        // A path longer than the pane clips rather than spilling.
        let long = Status {
            untracked: vec![untracked(&"x".repeat(80))],
            ..Default::default()
        };
        let (f, host) = view(&long, 20, 4);
        let rows = painted(&f, &host, 0, 20);
        assert!(
            rows[1].chars().all(|c| c == 'x' || c == '?' || c == ' '),
            "the row drew something but the path: {:?}",
            rows[1]
        );
        assert_eq!(crate::screen::width(&rows[1]), 20, "the row overflowed");
    }

    #[test]
    fn the_cursor_never_rests_on_a_heading_and_clamps_at_both_ends() {
        let (mut f, _host) = view(&full_status(), 40, 8);
        // Row 0 is a heading; the pane opens on the file under it.
        assert_eq!(f.cursor(), 1);
        for _ in 0..24 {
            f.down();
            assert!(
                f.current_file().is_some(),
                "down parked on a heading at row {}",
                f.cursor()
            );
        }
        let bottom = f.cursor();
        f.down();
        assert_eq!(f.cursor(), bottom, "the last row wrapped");
        // And back up the same way, across every heading.
        for _ in 0..24 {
            f.up();
            assert!(
                f.current_file().is_some(),
                "up parked on a heading at row {}",
                f.cursor()
            );
        }
        assert_eq!(f.cursor(), 1, "up past the first heading stopped on it");
        // `gg` lands on the heading and walks forward; `G` walks back off a
        // trailing one.
        f.to_top();
        assert_eq!(f.cursor(), 1);
        f.to_bottom();
        assert!(f.current_file().is_some());
        // Pages and scrolls settle too.
        f.page(1);
        assert!(f.current_file().is_some(), "a page parked on a heading");
        f.page(-1);
        assert!(f.current_file().is_some());
        f.scroll_y(3);
        assert!(f.current_file().is_some(), "a scroll parked on a heading");
        f.scroll_y(-3);
        assert!(f.current_file().is_some());
    }

    #[test]
    fn the_scrollbar_is_pane_local_and_takes_a_drag() {
        let mut status = Status::default();
        for i in 0..40 {
            status
                .staged
                .push(staged(&format!("file-{i:02}.rs"), Change::Modified));
        }
        let (mut f, host) = view(&status, 30, 6);
        assert_eq!(f.top(), 0);
        // The bar lives on the pane's *last local column*, not the screen's:
        // a press at local col 29 of 30 is a press on the bar.
        f.press(29, 5, 1, false, &host);
        assert!(
            f.top() > 0,
            "the end of the track is not the end of the list"
        );
        let top = f.top();
        f.drag(29, 0, &host);
        assert!(f.top() < top, "the drag did not follow the thumb");
        f.release();
        // A body press moves the cursor instead of the viewport, past the
        // heading under it.
        f.press(10, 0, 1, false, &host);
        assert_eq!(
            f.cursor(),
            1,
            "a press past the heading took the file under it"
        );
        // Painted, the bar sits on the pane's last column, and it keeps
        // whatever background the row it sits on earned — the cursor row's
        // selection tint runs under it, a plain row's quiet one does.
        let mut screen = Screen::new(60, 6);
        screen.clear(Ink::new(host.theme.chrome.fg, host.theme.chrome.bg));
        f.paint(&mut screen, 10, 0, true, &host);
        assert_eq!(
            screen.char_at(39, 0),
            Some('█'),
            "the bar is not at x + cols - 1"
        );
        assert_eq!(
            screen.char_at(39, 5),
            Some('│'),
            "the track is not under the thumb"
        );
        assert_eq!(
            screen.ink(39, 1).unwrap().bg,
            host.theme.chrome.selection_bg,
            "the bar repainted the row it sits on"
        );
        assert_eq!(
            screen.ink(39, 4).unwrap().bg,
            host.theme.chrome.bg,
            "the bar invented a background of its own"
        );
    }

    #[test]
    fn a_refresh_anchors_to_its_section_and_path() {
        let (mut f, _host) = view(&twin_status(), 40, 12);
        // Onto the *unstaged* twin of gone.rs: two steps down from the
        // first staged file, the heading between them skipped.
        f.to_top();
        for _ in 0..2 {
            f.down();
        }
        let (section, path) = under(&f);
        assert_eq!(section, Section::Unstaged);
        assert_eq!(path.as_bytes(), b"gone.rs");

        // A staged addition above shifts every staged row; the keyboard goes
        // with the file, and does not walk up to the staged twin.
        let mut next = twin_status();
        next.staged.insert(0, staged("aaa.rs", Change::Added));
        f.replace(prepare(&next, "").rows);
        let (section, path) = under(&f);
        assert_eq!(section, Section::Unstaged, "the cursor crossed sections");
        assert_eq!(path.as_bytes(), b"gone.rs");

        // The staged twin leaves: row 2 becomes the unstaged heading, and
        // the cursor — travelling down — walks on past the furniture to the
        // first file below rather than resting on it.
        f.replace(prepare(&twin_status(), "").rows);
        f.to_top();
        f.down(); // row 2: the staged gone.rs
        let (section, path) = under(&f);
        assert_eq!(
            (section, path.as_bytes()),
            (Section::Staged, &b"gone.rs"[..])
        );
        let mut next = twin_status();
        next.staged.retain(|e| e.path.as_bytes() != b"gone.rs");
        f.replace(prepare(&next, "").rows);
        let (section, path) = under(&f);
        assert_eq!(
            (section, path.as_bytes()),
            (Section::Unstaged, &b"gone.rs"[..]),
            "a vanished anchor did not settle onto a file"
        );
        // And the anchor's own section emptying clamps onto the nearest row
        // that can hold the cursor.
        let mut next = twin_status();
        next.unstaged.clear();
        f.replace(prepare(&next, "").rows);
        assert!(
            f.current_file().is_some(),
            "the cursor was left on a heading"
        );
    }

    #[test]
    fn a_clean_refresh_clears_the_viewport_and_the_arm() {
        let (mut f, _host) = view(&twin_status(), 40, 12);
        f.to_bottom();
        let (section, path) = under(&f);
        assert!(!f.confirm_or_arm_discard(section, &path));
        assert!(f.armed_row().is_some());
        f.replace(prepare(&Status::default(), "").rows);
        assert!(f.is_clean());
        assert!(f.is_available(), "a successful clean read is still a read");
        assert_eq!(
            (f.cursor(), f.top()),
            (0, 0),
            "the viewport kept a position"
        );
        assert_eq!(f.armed_row(), None, "the arm survived its own refresh");
        assert!(f.current_file().is_none());
        assert_eq!(f.copy_text(), "");

        // And back to something: the cursor opens on the first file again.
        f.replace(prepare(&twin_status(), "").rows);
        assert_eq!(f.cursor(), 1);
        assert!(f.current_file().is_some());
    }

    #[test]
    fn a_discard_arms_on_its_row_and_a_move_asks_again() {
        let (mut f, host) = view(&twin_status(), 40, 12);
        f.to_top();
        for _ in 0..3 {
            f.down();
        }
        let (section, path) = under(&f);
        // First press arms and asks; the identical second spends the arm.
        assert!(!f.confirm_or_arm_discard(section, &path));
        assert_eq!(f.armed_row(), Some((section, path.clone())));
        assert!(f.confirm_or_arm_discard(section, &path));
        assert_eq!(f.armed_row(), None, "the confirmed arm was not spent");

        // A different target arms a different row — it does not inherit the
        // first question's answer.
        f.to_top();
        let (s2, p2) = under(&f);
        assert_ne!((s2, p2.clone()), (section, path.clone()));
        assert!(!f.confirm_or_arm_discard(s2, &p2));
        assert_eq!(f.armed_row(), Some((s2, p2.clone())));

        // A keyboard move disarms; the wheel disarms; a page disarms.
        f.down();
        assert_eq!(f.armed_row(), None, "a keyboard move kept the arm");
        assert!(!f.confirm_or_arm_discard(s2, &p2));
        f.scroll_y(2);
        assert_eq!(f.armed_row(), None, "the wheel kept the arm");
        assert!(!f.confirm_or_arm_discard(s2, &p2));
        f.page(1);
        assert_eq!(f.armed_row(), None, "a page kept the arm");

        // A mouse press on another row disarms; the same row keeps its
        // question standing.
        let armed_row = f.armed_on_row_of(s2, &p2).expect("the row is in the list");
        assert!(!f.confirm_or_arm_discard(s2, &p2));
        let local = armed_row.saturating_sub(f.top());
        let other = if local + 1 < f.view.height() {
            local + 1
        } else {
            local.saturating_sub(1)
        };
        f.press(5, other, 1, false, &host);
        assert_eq!(f.armed_row(), None, "a press on another row kept the arm");
        f.press(5, local, 1, false, &host);
        assert!(!f.confirm_or_arm_discard(s2, &p2));
        f.press(5, local, 1, false, &host);
        assert_eq!(
            f.armed_row(),
            Some((s2, p2.clone())),
            "a press on the armed row dropped it"
        );
        // And a different discard target asked by the keyboard moves the
        // question rather than queueing two.
        f.to_bottom();
        let (s3, p3) = under(&f);
        assert!(!f.confirm_or_arm_discard(s3, &p3));
        assert_eq!(f.armed_row(), Some((s3, p3)), "the arm did not move");
    }

    impl Files {
        /// The row index of an armed target — a test's way to click on it.
        fn armed_on_row_of(&self, section: Section, path: &PathBytes) -> Option<usize> {
            self.rows.iter().position(
                |e| matches!(e, Entry::File(f) if f.section == section && f.path == *path),
            )
        }
    }

    #[test]
    fn copy_spells_the_row_as_git_would_and_selection_stays_inert() {
        let (mut f, _host) = view(&twin_status(), 40, 12);
        f.to_top();
        assert_eq!(f.copy_text(), "A added.rs");
        f.down();
        assert_eq!(f.copy_text(), "D gone.rs");
        // Down once more crosses the unstaged heading — onto the *unstaged*
        // twin, which spells the same because it is the same path on the
        // other side of the index.
        f.down();
        assert_eq!(f.copy_text(), "D gone.rs");
        f.down();
        assert_eq!(f.copy_text(), "M edited.rs");
        f.down();
        assert_eq!(f.copy_text(), "? notes.md");
        f.down();
        assert_eq!(f.copy_text(), "UU uu.rs");
        // The empty answer is only an empty tree's.
        assert_eq!(Files::new(Vec::new()).copy_text(), "");
        // No drag selection, and the select verbs say so.
        assert_eq!(f.selection(), "");
        f.select_all();
        assert_eq!(f.selection(), "");
        assert!(!f.select_none());
    }

    #[test]
    fn the_empty_states_are_two_sentences_and_the_status_says_where() {
        let host = Host::new();
        // A clean tree and a failed read draw different quiet lines at the
        // top-left, and the status line says which is which.
        let mut clean = Files::new(prepare(&Status::default(), "").rows);
        clean.resize(40, 3);
        let mut screen = Screen::new(40, 3);
        screen.clear(Ink::new(host.theme.chrome.fg, host.theme.chrome.bg));
        clean.paint(&mut screen, 0, 0, true, &host);
        assert!(screen.row_text(0).contains("working tree clean"));
        assert!(clean.is_available());
        assert_eq!(clean.status(), "clean");

        let mut broken = Files::unavailable();
        broken.resize(40, 3);
        let mut screen = Screen::new(40, 3);
        screen.clear(Ink::new(host.theme.chrome.fg, host.theme.chrome.bg));
        broken.paint(&mut screen, 0, 0, true, &host);
        assert!(screen.row_text(0).contains("status unavailable"));
        assert!(!broken.is_available());
        assert!(!broken.is_clean(), "a failed read is not a clean tree");
        assert_eq!(broken.status(), "unavailable");
        assert_eq!(
            unavailable_label("repo (main)"),
            "repo (main) · status unavailable"
        );

        // A read that came back says where the keyboard is, in files.
        let (mut f, _host) = view(&twin_status(), 40, 12);
        assert_eq!(f.status(), "1/6 files");
        f.to_bottom();
        assert_eq!(f.status(), "6/6 files");
    }

    #[test]
    fn degenerate_dimensions_are_survivable() {
        let host = Host::new();
        let mut f = Files::new(prepare(&full_status(), "").rows);
        // No columns: nothing draws, nothing panics.
        f.resize(0, 6);
        let mut screen = Screen::new(10, 6);
        screen.clear(Ink::new(host.theme.chrome.fg, host.theme.chrome.bg));
        f.paint(&mut screen, 0, 0, true, &host);
        assert!(screen.row_text(1).trim().is_empty());
        // A one-row pane over a many-row list draws its one row — the one
        // the cursor is on, which is the only row a viewport of one can
        // honestly show.
        f.resize(20, 1);
        let mut screen = Screen::new(20, 1);
        screen.clear(Ink::new(host.theme.chrome.fg, host.theme.chrome.bg));
        f.paint(&mut screen, 0, 0, true, &host);
        assert!(
            screen.row_text(0).contains("added.rs"),
            "{:?}",
            screen.row_text(0)
        );
        // No height: no rows, no bar.
        f.resize(20, 0);
        f.paint(&mut screen, 0, 0, true, &host);
        // Presses into an empty pane are no-ops, and so is a click at a row
        // the list does not have.
        let mut clean = Files::unavailable();
        clean.resize(20, 4);
        clean.press(5, 1, 1, false, &host);
        clean.drag(5, 1, &host);
        clean.release();
        clean.down();
        assert_eq!(clean.cursor(), 0);
        // A pane whose heading and count cannot both fit keeps the label.
        let mut f = Files::new(prepare(&twin_status(), "").rows);
        f.resize(10, 4);
        let mut screen = Screen::new(10, 4);
        screen.clear(Ink::new(host.theme.chrome.fg, host.theme.chrome.bg));
        f.paint(&mut screen, 0, 0, true, &host);
        assert!(
            screen.row_text(0).contains("STAGED"),
            "{:?}",
            screen.row_text(0)
        );
    }
}
