//! The commit list, and the graph gutter drawn in box-drawing characters.
//!
//! Topology is [`gitten_core::assign_lanes`] and colour is
//! [`gitten_core::graph::Hues`], both untouched — this file decides only what a
//! lane *looks like* in a cell grid, which is the same division
//! `shell/src/graph.rs` has with its Bézier curves. Both frontends therefore
//! agree about which branch is amber and where the overflow begins.
//!
//! # One row per commit, and what that costs
//!
//! The window has real curves: a branch changing lanes is an S spanning a whole
//! row, painted in halves that meet on the boundary. A cell grid has no halves —
//! a row is one row of characters — so a merge and a fork are drawn *on the
//! commit's own row* as a horizontal run with a corner at the far end, which is
//! what `git log --graph` and lazygit both do.
//!
//! The honest cost: a merge and a fork in the same row of the same lane collapse
//! into one glyph, and a lane that both arrives and departs on a curve is drawn
//! as a crossing rather than as a swerve. Both are readable and neither is a lie
//! about what merged into what.
//!
//! # Which way is down
//!
//! `git log` is newest-first, so a row below is *older*. A lane converging on a
//! commit ([`GraphRow::merges`]) came from above, and a lane forked out of it
//! ([`GraphRow::forks`]) continues below. That is why a merge's corner points up
//! and a fork's points down, and getting it backwards draws a history where
//! branches merge into their own children.
//!
//! # Two columns per lane
//!
//! One for the glyph, one for the connector between it and the next lane. `git
//! --graph`'s spacing, and the reason a diagonal is expressible at all: with one
//! column per lane there is nowhere to put the `─` that says two lanes are
//! joined.
//!
//! # Filtering, and what a row number names
//!
//! `/` filters the list through [`gitten_core::search::Index`], and the full
//! commits stay resident: the viewport's row numbers address the *visible*
//! table, and one lookup at the end of every reader — cursor, copy, mouse,
//! paint — maps back to the source vector. That is the window's order table
//! (`shell/src/views/commits.rs`) reduced to a terminal's shape, and it is why
//! filtering cannot desync what the cursor names from what is drawn, copied or
//! opened. Search itself runs on edits only, never in a frame.

use crate::screen::{Ink, Pen, Screen};
use crate::scrollbar::{self, Bar};
use gitten_core::graph::{lane_count, Hues, MAX_LANES};
use gitten_core::host::Host;
use gitten_core::search::Index;
use gitten_core::theme::{Rgb, Theme};
use gitten_core::view::Viewport;
use gitten_core::{assign_lanes, initials, Commit, GraphRow};

/// Columns per lane: the glyph, then the gap a connector runs through.
const LANE_W: usize = 2;

/// The sha column, and a space after it. Fixed, unlike the graph: the eye scans
/// it vertically, so it has to be a column and not a ragged edge.
///
/// Clipped from the *right*, unlike a line number: an abbreviated sha is a
/// prefix and git resolves a prefix, so the last characters are the ones to
/// lose. `put_right` here would produce something that looks like a sha and
/// names no commit.
const SHA_W: usize = 8;
/// Two letters and a space.
const WHO_W: usize = 3;

/// What a lane looks like in one cell.
///
/// Named rather than inlined because every one of them is a decision the eye
/// reads, and because the terminal set has to survive a font without box
/// drawing: an extension replacing this struct is how a plain-ASCII fallback
/// happens, which is the only way it can happen without a second `render`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Glyphs {
    /// A lane passing straight through.
    pub through: char,
    /// An ordinary commit, and one with two or more parents. The merge is
    /// heavier so a join is findable while scrolling.
    pub dot: char,
    pub merge_dot: char,
    /// A lane crossing a horizontal connector.
    pub cross: char,
    /// The horizontal connector itself.
    pub run: char,
    /// A lane arriving from above and turning toward the dot: to its left, to
    /// its right.
    pub up_left: char,
    pub up_right: char,
    /// A lane leaving downward, likewise.
    pub down_left: char,
    pub down_right: char,
}

impl Default for Glyphs {
    fn default() -> Self {
        Self::rounded()
    }
}

impl Glyphs {
    /// The shipped set. Rounded corners, because a square `┘` beside a `│` reads
    /// as a table and the graph is not one.
    pub fn rounded() -> Self {
        Self {
            through: '│',
            dot: '●',
            merge_dot: '◉',
            cross: '┼',
            run: '─',
            up_left: '╰',
            up_right: '╯',
            down_left: '╭',
            down_right: '╮',
        }
    }

    /// Nothing outside ASCII, for a terminal or a font that cannot draw the
    /// rest. `git log --graph`'s own alphabet.
    pub fn ascii() -> Self {
        Self {
            through: '|',
            dot: '*',
            merge_dot: '@',
            cross: '+',
            run: '-',
            up_left: '\\',
            up_right: '/',
            down_left: '/',
            down_right: '\\',
        }
    }
}

/// One commit, flattened to what drawing needs: resolved once at load, never per
/// frame.
struct Draw {
    lane: u16,
    hue: u16,
    merge: bool,
    /// `(lane, hue)` for every lane passing straight through this row.
    through: Vec<(u16, u16)>,
    /// `(lane, hue)` for lanes converging on this commit from above.
    merges: Vec<(u16, u16)>,
    /// `(lane, hue)` for lanes forked out of it, continuing below.
    forks: Vec<(u16, u16)>,
    /// How many lanes this row's own graph needs, capped. Per row, so a commit
    /// alone on the trunk gets nearly the whole terminal for its subject
    /// instead of starting behind the widest merge in the repository.
    lanes: usize,
    /// Whether this row actually *has* lanes past the cap.
    ///
    /// Not `lanes == MAX_LANES`, which is the plausible wrong answer: a
    /// repository with exactly twelve lanes hides nothing, and dimming its last
    /// column would say there is more history over there when there is not.
    capped: bool,
    initials: String,
}

/// The commit list.
///
/// Holds the data, the viewport and the cursor; knows nothing about keys. Every
/// method is a command, exactly as in [`crate::diff::Diff`] and for the same
/// reason.
pub struct Commits {
    commits: Vec<Commit>,
    draws: Vec<Draw>,
    /// Every commit's search text, folded once at load — see
    /// [`gitten_core::search::Index`]. Keystrokes fold only their own needle
    /// and scan; nothing here lowercases on the way to a frame.
    search: Index,
    /// The standing query, `None` when the list is whole. Always the *trimmed*
    /// text, so a query of only spaces is no query — the same normalisation
    /// [`Commits::apply_query`] applies.
    query: Option<String>,
    /// Which source rows the viewport can see, ascending: the one
    /// visible-to-source table every row reader goes through. Unfiltered it is
    /// `0..len` and each lookup costs one indirection; filtered it is what
    /// [`Index::indices`] answered for the query.
    ///
    /// Row numbers below this table — the cursor, the selection, the
    /// scrollbar, everything [`Viewport`] holds — are rows of the *visible*
    /// list, and only the final lookup names a commit. That is what keeps
    /// open-diff, copy, the mouse and painting all agreeing with the cursor
    /// under a filter, and it is the same shape the window keeps in
    /// `shell/src/views/commits.rs`.
    visible: Vec<usize>,
    glyphs: Glyphs,
    /// The honest lane count, uncapped, for the status line.
    lanes: usize,
    /// Widest gutter of any row — the lane cap's yardstick, and nothing
    /// more: the subject hugs each row's own graph, the window's layout,
    /// so a row's text starts right after the lanes that row actually
    /// drew. The cost — a subject column that wanders row to row — is the
    /// owner's accepted trade for never shipping a run of dead cells
    /// between a narrow graph and its name.
    gutter: usize,
    cols: usize,
    /// The cursor, the top row and the height. [`gitten_core::view::Viewport`]
    /// and not a pair of fields, because the diff view needs the same ones and
    /// two copies of a scroll rule drift — this one had already lost the name of
    /// its own margin.
    view: Viewport,
    bar: Bar,
    /// The anchor and the free end of a dragged range, while there is one.
    ///
    /// A **range of rows** and not a range of bytes, which is where this view
    /// legitimately differs from the diff: a commit row is a sha, a name, a graph
    /// and a subject drawn in fixed columns, and half of that is furniture nobody
    /// wants pasted. So the unit of selection is the commit, and
    /// [`Commits::selection`] decides what a commit copies as.
    ///
    /// `None` means "just the row the cursor is on", which is what makes the copy
    /// key useful before the mouse has been touched at all. A dragged range is
    /// held here rather than inferred again, and a keyboard *move* clears it.
    sel: Option<(usize, usize)>,
    dragging: bool,
}

impl Commits {
    /// No `Host`, deliberately: nothing here is resolved at load that a theme
    /// could change. A lane's colour is `theme.lane(hue)` and an author's is a
    /// hash and an index, both read on the frame that draws them — so editing
    /// `gitten.toml` recolours the list without rebuilding it. The shell resolves
    /// its author colours once at construction and does not.
    pub fn new(commits: Vec<Commit>) -> Self {
        Self::with_glyphs(commits, Glyphs::default())
    }

    /// The constructor an extension uses to draw the gutter in its own alphabet
    /// — `Glyphs::ascii()`, or a Nerd Font set.
    pub fn with_glyphs(commits: Vec<Commit>, glyphs: Glyphs) -> Self {
        let rows = assign_lanes(&commits);
        let lanes = lane_count(&rows);
        let draws = draws(&commits, &rows);
        // Widest *row*, not the widest lane index: a row's gutter is only as
        // wide as its own lanes, and the trunk-only rows of a busy repository
        // are the majority.
        let gutter = draws.iter().map(|d| d.lanes * LANE_W).max().unwrap_or(0);
        // Search text folded beside the rest of the load work, and the whole
        // list visible to start.
        let search = Index::new(&commits);
        let visible = Vec::from_iter(0..commits.len());
        let mut view = Viewport::new();
        view.set_len(visible.len());
        Self {
            commits,
            draws,
            search,
            visible,
            query: None,
            glyphs,
            lanes,
            gutter,
            cols: 0,
            view,
            bar: Bar::default(),
            sel: None,
            dragging: false,
        }
    }

    /// How many commits were loaded, whatever a filter shows. Not the count the
    /// viewport addresses — that is the visible list's length, and one number
    /// meaning both would be ambiguous under a query.
    pub fn len(&self) -> usize {
        self.commits.len()
    }

    pub fn is_empty(&self) -> bool {
        self.commits.is_empty()
    }

    pub fn cursor(&self) -> usize {
        self.view.cursor()
    }

    pub fn top(&self) -> usize {
        self.view.top()
    }

    /// The commit under the cursor, for whatever opens a diff from it.
    ///
    /// Through the visible table and not `commits[cursor]`: the cursor is a row
    /// of the *filtered* list, and under a query those are not the same
    /// position. Everything that acts on "this commit" — open-diff, copy —
    /// reads through here, which is why filtering cannot desync them.
    pub fn current(&self) -> Option<&Commit> {
        self.commits.get(*self.visible.get(self.view.cursor())?)
    }

    pub fn resize(&mut self, cols: usize, height: usize) {
        self.cols = cols;
        self.view.set_height(height);
    }

    // ----------------------------------------------------------------- search

    /// The live query, for pre-filling a second `/`. `None` when unfiltered.
    pub fn query(&self) -> Option<&str> {
        self.query.as_deref()
    }

    /// The filter while one stands, for a status line: `15/30` — hits over
    /// loaded. `None` unfiltered, so a note is only drawn when there is one.
    pub fn filter_note(&self) -> Option<String> {
        self.query
            .is_some()
            .then(|| format!("{}/{}", self.visible.len(), self.commits.len()))
    }

    /// Sets the filter — once per keystroke, and never anywhere else. The
    /// visible table is rebuilt here and read everywhere else.
    ///
    /// The keyboard stays on the commit it was on: anchored by full sha into
    /// the next result set wherever it survives the narrower query, and left
    /// clamped onto the last surviving row when it does not. An empty (or
    /// whitespace-only) query is no query, so clearing restores the whole list;
    /// the same trimmed query twice does nothing, so a keystroke that changes
    /// nothing rebuilds nothing.
    ///
    /// A selection is dropped whenever the result set changes: its ends are
    /// rows of the *visible* list, and a range named against yesterday's table
    /// would read as different commits under today's.
    pub fn apply_query(&mut self, query: &str) {
        let next = Some(query.trim()).filter(|q| !q.is_empty());
        if self.query.as_deref() == next {
            return;
        }
        // Anchor first: named by sha, because row numbers are about to stop
        // meaning anything.
        let anchored = self
            .visible
            .get(self.view.cursor())
            .and_then(|&i| self.commits.get(i))
            .map(|c| c.sha.clone());

        self.query = next.map(str::to_string);
        self.visible = match &self.query {
            Some(q) => self.search.indices(q),
            None => Vec::from_iter(0..self.commits.len()),
        };
        self.sel = None;
        self.dragging = false;
        // `set_len` clamps the cursor onto the surviving rows; the anchor, where
        // it survived, is put back by name.
        self.view.set_len(self.visible.len());
        let cursor = anchored
            .as_deref()
            .and_then(|sha| {
                self.visible
                    .iter()
                    .position(|&i| self.commits[i].sha == sha)
            })
            .unwrap_or_else(|| self.view.cursor());
        self.view.go_to(cursor);
    }

    pub fn move_by(&mut self, by: isize) {
        self.sel = None;
        self.view.move_by(by);
    }

    pub fn down(&mut self) {
        self.move_by(1);
    }

    pub fn up(&mut self) {
        self.move_by(-1);
    }

    pub fn page(&mut self, pages: isize) {
        self.sel = None;
        self.view.page(pages);
    }

    /// Scrolls the viewport without moving the cursor. The wheel.
    pub fn scroll_y(&mut self, by: isize) {
        self.view.pan_by(by);
    }

    /// How much lead the cursor keeps at the edge. `[view] scrolloff`.
    pub fn set_scrolloff(&mut self, rows: usize) {
        self.view.set_scrolloff(rows);
    }

    pub fn to_top(&mut self) {
        self.sel = None;
        self.view.to_top();
    }

    pub fn to_bottom(&mut self) {
        self.sel = None;
        self.view.to_bottom();
    }

    /// Puts a saved row on screen, for a session restored across a restart.
    pub fn go_to(&mut self, row: usize) {
        self.sel = None;
        self.view.go_to(row);
    }

    /// Swaps in a refreshed list, keeping the selection by identity.
    ///
    /// A write moves rows — a commit added above shifts every row down one —
    /// but the commit the keyboard is on survives more often than its row does,
    /// so the cursor is anchored by **sha** and not by number. When it survives,
    /// the view follows it; when it does not, the previous position is clamped
    /// into whatever the new list can hold. That is the shared
    /// [`Viewport`] doing the clamping, and nothing here assigns an index the
    /// list has not blessed.
    ///
    /// Glyphs, dimensions and the scrollbar are untouched: the refresh changes
    /// what the list holds, never how it draws.
    pub fn replace(&mut self, commits: Vec<Commit>) {
        let sha = self.current().map(|c| c.sha.clone());
        let (cursor, top) = (self.view.cursor(), self.view.top());
        // A drag's range was a promise about rows the refresh may have
        // renumbered. It is the mouse's, and the mouse has let go.
        self.sel = None;
        self.dragging = false;
        self.commits = commits;
        let rows = assign_lanes(&self.commits);
        self.lanes = lane_count(&rows);
        self.draws = draws(&self.commits, &rows);
        self.gutter = self
            .draws
            .iter()
            .map(|d| d.lanes * LANE_W)
            .max()
            .unwrap_or(0);
        // The old scroll position first, then the anchor: `go_to` drags the
        // viewport after the cursor, and the surviving sha's row must be the
        // one on screen when it survives — restoring the top last could drag
        // the cursor off the very row it was kept for.
        self.view.set_len(self.commits.len());
        self.view.scroll_to(top);
        let at = sha
            .and_then(|s| self.commits.iter().position(|c| c.sha == s))
            .unwrap_or(cursor);
        self.view.go_to(at);
    }

    // ---------------------------------------------------------------- the mouse

    /// The glyphs the scrollbar is drawn with. `--ascii`, or an extension.
    pub fn set_bar(&mut self, bar: Bar) {
        self.bar = bar;
    }

    /// Which commits are selected: the drag's range, or the row the cursor is on.
    ///
    /// Never empty on a non-empty list, which is what makes `y` copy this commit
    /// before anything has been dragged. Rows of the visible list — [`Commits::lines`]
    /// maps them through the table, so a filtered list copies only what is shown.
    pub fn selected(&self) -> std::ops::Range<usize> {
        if self.visible.is_empty() {
            return 0..0;
        }
        let cursor = self.view.cursor();
        let (a, b) = self.sel.unwrap_or((cursor, cursor));
        a.min(b)..a.max(b) + 1
    }

    /// A press in the list: the cursor moves there, and a range starts.
    ///
    /// `row` is a row of the body, and `extend` is shift — which moves the free
    /// end of the range rather than starting a new one, so a range longer than
    /// the screen needs no drag that has to scroll.
    pub fn press(&mut self, _col: usize, row: usize, extend: bool, _host: &Host) {
        let Some(index) = self.view.row_at(row) else {
            return;
        };
        let anchor = match (extend, self.sel) {
            (true, Some((a, _))) => a,
            _ => index,
        };
        self.view.go_to(index);
        self.sel = Some((anchor, index));
        self.dragging = true;
    }

    /// The pointer moved with the button down. A row above or below the body
    /// scrolls by the overshoot and keeps selecting, exactly as in the diff.
    pub fn drag(&mut self, row: isize, _host: &Host) {
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
        let Some(index) = self.view.row_at(row as usize) else {
            return;
        };
        // The cursor follows the free end, so the row a command would act on is
        // the row the pointer is over.
        self.view.go_to(index);
        self.sel = Some((self.sel.map_or(index, |(a, _)| a), index));
    }

    pub fn release(&mut self) {
        self.dragging = false;
    }

    /// `select.all`.
    pub fn select_all(&mut self) {
        if self.visible.is_empty() {
            return;
        }
        let last = self.visible.len() - 1;
        self.view.go_to(last);
        self.sel = Some((0, last));
    }

    /// `select.none`. Says whether there was a range to drop, so `esc` can fall
    /// through to whatever it means next — the cursor's own row is not a range
    /// and there is nothing to clear when that is all there is.
    pub fn select_none(&mut self) -> bool {
        self.sel.take().is_some()
    }

    /// What the *mouse* has selected, and nothing else.
    ///
    /// Empty for a click, which is the rule copy-on-select rests on: a click is a
    /// cursor move, and a clipboard that changed every time you pointed at a
    /// commit would be a clipboard you could not keep anything in. A drag across
    /// two rows is a selection; a press and a release on one row is not.
    pub fn selection(&self) -> String {
        match self.sel {
            Some((a, b)) if a != b => self.lines(self.selected()),
            _ => String::new(),
        }
    }

    /// What `copy.selection` copies: the dragged range, or the commit the cursor
    /// is on when there is none.
    ///
    /// The fallback is the point of binding the key at all — "copy this commit"
    /// is the thing you want nine times out of ten, and it should not need the
    /// mouse.
    pub fn copy_text(&self) -> String {
        self.lines(self.selected())
    }

    /// One line per commit: the sha and the subject, and neither the graph nor
    /// the initials — a column of box drawing is not a thing anybody pastes, and
    /// the two fields left are the ones that name the commit to git and to a
    /// person.
    ///
    /// `rows` names visible rows, mapped through the table here — the one
    /// lookup that turns a viewport row back into a commit, so a copy under a
    /// query names the commits on screen and nothing that was filtered out.
    fn lines(&self, rows: std::ops::Range<usize>) -> String {
        self.visible[rows.start..rows.end.min(self.visible.len())]
            .iter()
            .map(|&i| {
                let c = &self.commits[i];
                format!("{} {}", c.short, c.subject)
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Draws the visible rows into `screen`, at `x` of row `y` onward, inside
    /// this pane's own columns.
    ///
    /// Every row is taken through [`Screen::span`], never [`Screen::row`]:
    /// the pane is a guest in the row, and a long subject that wrote to the
    /// whole screen would overwrite the divider and whatever sits beside it.
    /// draws only when this pane holds the keyboard — `focused` is the
    /// caller's answer, not something the view knows — while
    /// a dragged selection keeps its ink either way, because
    /// focus moves the caret and never what the mouse is holding.
    pub fn paint(&self, screen: &mut Screen, x: usize, y: usize, focused: bool, host: &Host) {
        let theme = &host.theme;
        let blank = Ink::new(theme.chrome.dim, theme.chrome.bg);
        // A range of one is the cursor and nothing more, so a click looks exactly
        // like a keypress and only a real drag lights up.
        let selected = match self.sel {
            Some(_) => self.selected(),
            None => 0..0,
        };
        for i in 0..self.view.height() {
            let row = y + i;
            // Two lookups, and both have to be read: `row_at` is the viewport
            // row, which the cursor and the selection address, and `visible`
            // names the commit it stands for.
            let Some(vis) = self.view.row_at(i) else {
                screen.span(row, x, self.cols).wash(blank);
                continue;
            };
            let Some(&index) = self.visible.get(vis) else {
                screen.span(row, x, self.cols).wash(blank);
                continue;
            };
            let bg = match (
                focused && vis == self.view.cursor(),
                selected.contains(&vis),
            ) {
                (true, _) => theme.chrome.selection_bg,
                (false, true) => theme.chrome.selected_bg,
                (false, false) => theme.chrome.bg,
            };
            let mut pen = screen.span(row, x, self.cols);
            self.row(&mut pen, index, bg, theme);
        }
    }

    /// The bar at the edge geometry [`App::paint_scrollbar`] hands it. The
    /// pane does not choose.
    pub fn paint_bar(
        &self,
        screen: &mut Screen,
        x: usize,
        divider: Option<usize>,
        y: usize,
        host: &Host,
    ) {
        scrollbar::paint(screen, self.bar, x, divider, y, &self.view, host);
    }

    /// lazygit's order — sha, author, graph, subject — because the graph is the
    /// column that changes width and putting it last would move the subject.
    ///
    /// `index` is a *source* index: [`Commits::paint`] has already consulted the
    /// visible table and hands over the commit it names.
    fn row(&self, pen: &mut Pen, index: usize, bg: Rgb, theme: &Theme) {
        let c = &self.commits[index];
        let d = &self.draws[index];
        // A sub-pen per column, so neither can push the graph sideways however
        // long the sha or the name is — a fixed column that a long value moves
        // is not a column.
        let dim = Ink::new(theme.chrome.dim, bg);
        pen.take(SHA_W - 1).put(&c.short, dim);
        pen.put(" ", dim);
        pen.take(WHO_W - 1)
            .put(&d.initials, Ink::new(theme.author(&c.author), bg));
        pen.put(" ", dim);

        // The subject hugs the row's own graph — the window's per-row
        // layout. One space after the lanes this row drew, however wide
        // the busiest row in the history is.
        self.gutter(pen, d, bg, theme);
        pen.put(" ", Ink::new(theme.chrome.fg, bg));
        pen.put(&c.subject, Ink::new(theme.chrome.fg, bg));
        pen.wash(Ink::new(theme.chrome.fg, bg));
    }

    /// One row of the graph.
    ///
    /// Painted in one left-to-right pass over the *cells*, not over the lanes,
    /// because a connector occupies cells between two lanes and a lane may be
    /// crossed by one. Each cell asks what belongs in it and the answer is
    /// local, which is what keeps this a `for` over at most 24 columns rather
    /// than a sort of segments.
    fn gutter(&self, pen: &mut Pen, d: &Draw, bg: Rgb, theme: &Theme) {
        let g = &self.glyphs;
        let dot = (d.lane as usize).min(MAX_LANES - 1);
        // Where the horizontal connectors have to reach: the furthest lane that
        // joins this dot, on each side.
        let reach = |ends: &[(u16, u16)]| {
            ends.iter()
                .map(|(l, _)| (*l as usize).min(MAX_LANES - 1))
                .fold((dot, dot), |(lo, hi), l| (lo.min(l), hi.max(l)))
        };
        let (m_lo, m_hi) = reach(&d.merges);
        let (f_lo, f_hi) = reach(&d.forks);
        let (lo, hi) = (m_lo.min(f_lo), m_hi.max(f_hi));

        let hue_of = |lane: usize| -> Option<u16> {
            d.through
                .iter()
                .chain(&d.merges)
                .chain(&d.forks)
                .find(|(l, _)| (*l as usize).min(MAX_LANES - 1) == lane)
                .map(|(_, h)| *h)
        };

        for cell in 0..d.lanes * LANE_W {
            let lane = cell / LANE_W;
            let on_lane = cell % LANE_W == 0;
            let overflow = d.capped && lane == MAX_LANES - 1;

            let (ch, hue) = if on_lane && lane == dot {
                (if d.merge { g.merge_dot } else { g.dot }, Some(d.hue))
            } else if on_lane {
                match (
                    d.merges
                        .iter()
                        .find(|(l, _)| (*l as usize).min(MAX_LANES - 1) == lane),
                    d.forks
                        .iter()
                        .find(|(l, _)| (*l as usize).min(MAX_LANES - 1) == lane),
                    d.through
                        .iter()
                        .find(|(l, _)| (*l as usize).min(MAX_LANES - 1) == lane),
                ) {
                    // A lane that arrives from above and turns toward the dot.
                    // Its corner points *up* because the row above is newer.
                    (Some((_, h)), _, _) => {
                        (if lane < dot { g.up_left } else { g.up_right }, Some(*h))
                    }
                    // A lane born here, continuing into older history below.
                    (None, Some((_, h)), _) => (
                        if lane < dot {
                            g.down_left
                        } else {
                            g.down_right
                        },
                        Some(*h),
                    ),
                    // A lane minding its own business — crossed by a connector
                    // if one passes over it.
                    (None, None, Some((_, h))) => {
                        let crossed = lane > lo && lane < hi;
                        (if crossed { g.cross } else { g.through }, Some(*h))
                    }
                    (None, None, None) => (' ', None),
                }
            } else {
                // Between two lanes: connector, or nothing.
                let between = cell > lo * LANE_W && cell < hi * LANE_W;
                (
                    if between { g.run } else { ' ' },
                    hue_of(lane).or(Some(d.hue)),
                )
            };

            // Past the cap every lane shares one column, so they share one grey
            // and stop pretending to be individuals.
            let fg = match (overflow, hue) {
                (true, _) => theme.lane_overflow,
                (false, Some(h)) => theme.lane(h as usize),
                (false, None) => theme.chrome.bg,
            };
            pen.put(&ch.to_string(), Ink::new(fg, bg));
        }
    }

    /// One line describing the list, for whatever draws a status bar. The lane
    /// count is the uncapped one: "280 lanes" is worth knowing when twelve are
    /// drawn. Position is counted over the *visible* rows — what the cursor
    /// addresses — while `filter_note` is what says the list is narrower than
    /// what was loaded.
    pub fn status(&self) -> String {
        let mut out = format!(
            "{}/{} · {} lanes",
            (self.view.cursor() + 1).min(self.visible.len()),
            self.visible.len(),
            self.lanes,
        );
        if self.lanes > MAX_LANES {
            out.push_str(&format!(" · {MAX_LANES} drawn"));
        }
        out
    }
}

/// Walks the history once, resolving every row's lanes to hues.
///
/// The order of the claims and releases is not incidental — see
/// [`gitten_core::graph::Hues`], which documents it, because getting it wrong
/// wastes a colour per merge and a busy repository then runs out.
fn draws(commits: &[Commit], rows: &[GraphRow]) -> Vec<Draw> {
    let mut hues = Hues::new();
    let mut out = Vec::with_capacity(rows.len());
    for (c, r) in commits.iter().zip(rows) {
        let hue = hues.claim(r.lane);
        let merges: Vec<(u16, u16)> = r
            .merges
            .iter()
            .map(|&m| (m as u16, hues.claim(m)))
            .collect();
        let through: Vec<(u16, u16)> = r
            .through
            .iter()
            .map(|&t| (t as u16, hues.claim(t)))
            .collect();

        // Branches that end here give their colour back, and a root gives up its
        // own lane, *before* the forks below claim theirs.
        for &m in &r.merges {
            hues.release(m);
        }
        if c.parents.is_empty() {
            hues.release(r.lane);
        }
        let forks: Vec<(u16, u16)> = r.forks.iter().map(|&f| (f as u16, hues.claim(f))).collect();

        let widest = r
            .through
            .iter()
            .chain(&r.merges)
            .chain(&r.forks)
            .copied()
            .chain(std::iter::once(r.lane))
            .max()
            .unwrap_or(0);

        out.push(Draw {
            lane: r.lane as u16,
            hue,
            merge: c.parents.len() > 1,
            through,
            merges,
            forks,
            lanes: widest.min(MAX_LANES - 1) + 1,
            capped: widest >= MAX_LANES,
            initials: initials(&c.author),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use gitten_core::parse_log;

    /// A merge of two branches off one root, newest first:
    ///
    /// ```text
    ///   m   merge of a and b
    ///   a   on the trunk
    ///   b   on a branch
    ///   r   root
    /// ```
    const LOG: &str = "\
m\x1fmmmmmmm\x1fa b\x1fAda Lovelace\x1f1700000400\x1fMerge branch\x1e\
a\x1faaaaaaa\x1fr\x1fAda Lovelace\x1f1700000300\x1fOn the trunk\x1e\
b\x1fbbbbbbb\x1fr\x1fGrace Hopper\x1f1700000200\x1fOn a branch\x1e\
r\x1frrrrrrr\x1f\x1fAda Lovelace\x1f1700000100\x1fRoot\x1e";

    fn view(log: &str, cols: usize, height: usize) -> (Commits, Host) {
        let host = Host::new();
        let mut c = Commits::new(parse_log(log));
        c.resize(cols, height);
        (c, host)
    }

    fn painted(c: &Commits, host: &Host) -> Vec<String> {
        let mut screen = Screen::new(c.cols, c.view.height() + 2);
        screen.clear(Ink::new(host.theme.chrome.fg, host.theme.chrome.bg));
        c.paint(&mut screen, 0, 0, true, host);
        (0..c.view.height()).map(|y| screen.row_text(y)).collect()
    }

    /// A list long enough to scroll, so the scrollbar has something to say.
    fn many(n: usize) -> String {
        (0..n)
            .map(|i| {
                let parent = match i + 1 < n {
                    true => format!("c{}", i + 1),
                    false => String::new(),
                };
                format!(
                    "c{i}\x1fc{i:06}\x1f{parent}\x1fAda Lovelace\x1f17000000{i:02}\x1fCommit {i}\x1e"
                )
            })
            .collect()
    }

    /// A linear history of `n` commits alternating half by author and half by
    /// subject — even rows are Ada/engine, odd rows Grace/compiler — so one
    /// query hits exactly half of either. The window's search fixture, spelled
    /// for [`parse_log`].
    fn mixed(n: usize) -> String {
        (0..n)
            .map(|i| {
                let even = i % 2 == 0;
                let sha = format!("{i:08}");
                let parent = match i + 1 < n {
                    true => format!("{:08}", i + 1),
                    false => String::new(),
                };
                format!(
                    "{sha}\x1f{sha}\x1f{parent}\x1f{}\x1f1\x1f{}\x1e",
                    if even { "Ada Lovelace" } else { "Grace Hopper" },
                    if even {
                        format!("engine note {i}")
                    } else {
                        format!("compiler pass {i}")
                    },
                )
            })
            .collect()
    }

    #[test]
    fn a_click_moves_the_cursor_and_a_drag_selects_the_commits_between() {
        let (mut c, host) = view(LOG, 60, 4);
        c.press(20, 2, false, &host);
        assert_eq!(c.cursor(), 2);
        // A click selects nothing — so copy-on-select does not fire on one —
        // but the copy key still has a commit to act on.
        assert_eq!(c.selection(), "");
        assert_eq!(c.copy_text(), "bbbbbbb On a branch");
        c.drag(0, &host);
        c.release();
        assert_eq!(c.selected(), 0..3);
        assert_eq!(
            c.selection(),
            "mmmmmmm Merge branch\naaaaaaa On the trunk\nbbbbbbb On a branch"
        );
    }

    #[test]
    fn the_wheel_keeps_a_dragged_range_and_a_keyboard_move_drops_it() {
        // The viewport and cursor are independent, and an explicit dragged
        // range stays independent of both.
        let (mut c, host) = view(&many(50), 60, 6);
        c.press(20, 0, false, &host);
        c.drag(2, &host);
        let range = c.selected();
        assert_eq!(range, 0..3);
        c.scroll_y(20);
        assert_eq!(
            c.selected(),
            range,
            "scrolling changed what would be copied"
        );
    }

    #[test]
    fn a_keyboard_move_drops_a_dragged_range_rather_than_growing_it() {
        // Otherwise scrolling after a drag silently extends what `y` would copy.
        let (mut c, host) = view(LOG, 60, 4);
        c.press(20, 0, false, &host);
        c.drag(2, &host);
        assert_eq!(c.selected().len(), 3);
        c.down();
        assert_eq!(c.selected().len(), 1);
        assert_eq!(
            c.selection(),
            "",
            "a range outlived the keypress that dropped it"
        );
        assert_eq!(c.copy_text(), "rrrrrrr Root");
    }

    #[test]
    fn a_selected_range_is_lit_and_a_single_click_looks_like_a_keypress() {
        let (mut c, host) = view(LOG, 60, 4);
        c.press(20, 0, false, &host);
        let mut screen = Screen::new(c.cols, 4);
        screen.clear(Ink::new(host.theme.chrome.fg, host.theme.chrome.bg));
        c.paint(&mut screen, 0, 0, true, &host);
        assert_eq!(screen.ink(0, 0).unwrap().bg, host.theme.chrome.selection_bg);
        assert_eq!(
            screen.ink(0, 1).unwrap().bg,
            host.theme.chrome.bg,
            "one row lit two"
        );
        // Now a real drag: the rows behind the cursor take the selected colour.
        c.drag(2, &host);
        c.paint(&mut screen, 0, 0, true, &host);
        assert_eq!(screen.ink(0, 1).unwrap().bg, host.theme.chrome.selected_bg);
        assert_eq!(
            screen.ink(0, 2).unwrap().bg,
            host.theme.chrome.selection_bg,
            "the cursor"
        );
    }

    #[test]
    fn select_all_takes_every_commit_and_none_gives_them_back() {
        let (mut c, _) = view(LOG, 60, 4);
        c.select_all();
        assert_eq!(c.selected(), 0..4);
        assert!(c.select_none());
        assert_eq!(c.selected(), 3..4, "the cursor is still a selection of one");
        assert!(!c.select_none());
    }

    #[test]
    fn the_bar_column_is_the_row_under_it_and_the_list_does_not_jump() {
        let (mut c, host) = view(&many(200), 60, 10);
        // The bar is an indicator: a press on its column is a press on the
        // row underneath, like any other column, and nothing about the bar
        // itself scrolls or grabs.
        let top = c.top();
        c.press(59, 4, false, &host);
        assert_eq!(c.top(), top, "a press on the bar column did not scroll");
        assert_eq!(
            c.selected(),
            4..5,
            "it selected the row under the bar, as a column of text"
        );
        c.release();
    }

    #[test]
    fn a_row_is_sha_initials_graph_then_subject() {
        let (c, host) = view(LOG, 60, 4);
        let rows = painted(&c, &host);
        assert!(rows[0].starts_with("mmmmmmm AL "), "{:?}", rows[0]);
        assert!(rows[0].ends_with("Merge branch"), "{:?}", rows[0]);
        assert!(
            rows[2].contains("GH"),
            "the author column is per commit: {:?}",
            rows[2]
        );
    }

    #[test]
    fn the_subject_hugs_the_rows_own_graph() {
        // The window's layout: one space after the lanes this row drew,
        // so a trunk row's subject sits left of a merge row's — and no
        // row ships dead cells between its graph and its name.
        let (c, host) = view(LOG, 60, 4);
        let rows = painted(&c, &host);
        // Display columns, not bytes: box drawing is three bytes a glyph, so a
        // `find` compares the wrong thing and passes or fails by the graph's
        // shape rather than by its width.
        let at =
            |row: &String, word: &str| crate::screen::width(&row[..row.find(word).expect(word)]);
        // SHA_W + WHO_W of furniture, then the row's own lanes × 2, then
        // the separating space — whatever each row's graph needs, with no
        // fixed column and no dead cells between graph and name.
        for (i, row) in rows.iter().enumerate() {
            let word = match i {
                0 => "Merge branch",
                1 => "On the trunk",
                2 => "On a branch",
                _ => "Root",
            };
            assert_eq!(
                at(row, word),
                11 + c.draws[i].lanes * LANE_W + 1,
                "row {i} ({:?}): {:?}",
                c.draws[i].lanes,
                row
            );
        }
    }

    #[test]
    fn a_merge_commit_is_drawn_heavier_than_an_ordinary_one() {
        let (c, host) = view(LOG, 60, 4);
        let rows = painted(&c, &host);
        assert!(rows[0].contains('◉'), "{:?}", rows[0]);
        assert!(
            rows[1].contains('●') && !rows[1].contains('◉'),
            "{:?}",
            rows[1]
        );
    }

    #[test]
    fn a_forked_lane_points_down_and_a_converging_one_points_up() {
        // The orientation trap: `git log` is newest-first, so a row below is
        // older. Backwards, this draws branches merging into their children.
        let (c, host) = view(LOG, 60, 4);
        let rows = painted(&c, &host);
        // The merge forks lane 1 for its second parent: it continues downward.
        assert!(
            rows[0].contains('╮'),
            "the fork did not point down: {:?}",
            rows[0]
        );
        // The root has both branches converging on it from above.
        assert!(
            rows[3].contains('╯'),
            "the merge did not point up: {:?}",
            rows[3]
        );
    }

    #[test]
    fn a_lane_passing_through_is_a_vertical_and_the_connector_crosses_it() {
        // A fork that has to reach *over* an occupied lane. `t` puts `b` in lane
        // 1, so `m`'s second parent lands in lane 2 and the run from `m`'s dot
        // crosses a lane that has nothing to do with it.
        let log = "\
t\x1ft\x1fm b\x1fA\x1f1\x1ftip\x1e\
m\x1fm\x1fa c\x1fA\x1f1\x1fmerge\x1e\
b\x1fb\x1fr\x1fA\x1f1\x1fother\x1e\
a\x1fa\x1fr\x1fA\x1f1\x1ftrunk\x1e\
c\x1fc\x1fr\x1fA\x1f1\x1fthird\x1e\
r\x1fr\x1f\x1fA\x1f1\x1froot\x1e";
        let (c, host) = view(log, 70, 6);
        let rows = painted(&c, &host);
        assert!(rows[1].contains('─'), "no connector: {:?}", rows[1]);
        assert!(
            rows[1].contains('┼'),
            "the connector broke a lane: {:?}",
            rows[1]
        );
    }

    #[test]
    fn a_branch_keeps_its_colour_and_a_recycled_lane_does_not() {
        let (c, host) = view(LOG, 60, 4);
        let mut screen = Screen::new(60, 4);
        screen.clear(Ink::new(host.theme.chrome.fg, host.theme.chrome.bg));
        c.paint(&mut screen, 0, 0, true, &host);
        // The trunk is the first colour the wheel hands out.
        let dot = (0..60)
            .find(|x| screen.char_at(*x, 1) == Some('●'))
            .unwrap();
        assert_eq!(screen.ink(dot, 1).unwrap().fg, host.theme.lane(0));
        // The branch is not.
        let branch = (0..60)
            .find(|x| screen.char_at(*x, 2) == Some('●'))
            .unwrap();
        assert_ne!(screen.ink(branch, 2).unwrap().fg, host.theme.lane(0));
    }

    #[test]
    fn lanes_past_the_cap_share_one_dim_column() {
        // git/git runs 280 concurrent lanes; a gutter that wide pushes the
        // subject off the screen entirely.
        let mut log = String::from("h\x1fh\x1f");
        let parents: Vec<String> = (0..30).map(|i| format!("p{i}")).collect();
        log.push_str(&parents.join(" "));
        log.push_str("\x1fA\x1f1\x1foctopus\x1e");
        for p in &parents {
            log.push_str(&format!("{p}\x1f{p}\x1f\x1fA\x1f1\x1fparent\x1e"));
        }
        let (c, host) = view(&log, 120, 8);
        assert!(c.lanes > MAX_LANES, "{} lanes", c.lanes);
        assert!(c.status().contains("drawn"), "{}", c.status());
        assert!(
            c.gutter <= MAX_LANES * LANE_W,
            "the gutter blew past the cap"
        );
        let mut screen = Screen::new(120, 8);
        screen.clear(Ink::new(host.theme.chrome.fg, host.theme.chrome.bg));
        c.paint(&mut screen, 0, 0, true, &host);
        let last = SHA_W + WHO_W + (MAX_LANES - 1) * LANE_W;
        assert_eq!(screen.ink(last, 0).unwrap().fg, host.theme.lane_overflow);
    }

    #[test]
    fn exactly_the_cap_is_not_dimmed() {
        // The plausible wrong answer is `lanes == MAX_LANES`. A repository with
        // exactly twelve lanes hides nothing, and dimming its last column says
        // there is more history over there when there is not.
        let mut log = String::from("h\x1fh\x1f");
        let parents: Vec<String> = (0..MAX_LANES).map(|i| format!("p{i}")).collect();
        log.push_str(&parents.join(" "));
        log.push_str("\x1fA\x1f1\x1foctopus\x1e");
        for p in &parents {
            log.push_str(&format!("{p}\x1f{p}\x1f\x1fA\x1f1\x1fparent\x1e"));
        }
        let (c, host) = view(&log, 120, 4);
        assert_eq!(c.lanes, MAX_LANES);
        assert!(!c.status().contains("drawn"), "{}", c.status());
        let mut screen = Screen::new(120, 4);
        screen.clear(Ink::new(host.theme.chrome.fg, host.theme.chrome.bg));
        c.paint(&mut screen, 0, 0, true, &host);
        let last = SHA_W + WHO_W + (MAX_LANES - 1) * LANE_W;
        assert_ne!(screen.ink(last, 0).unwrap().fg, host.theme.lane_overflow);
    }

    #[test]
    fn a_replaceable_glyph_set_reaches_the_screen() {
        // Rule 1 for the one thing in here that is purely appearance: an
        // extension swaps the alphabet without touching `paint`.
        let host = Host::new();
        let mut c = Commits::with_glyphs(parse_log(LOG), Glyphs::ascii());
        c.resize(60, 4);
        let rows = painted(&c, &host);
        assert!(rows[0].contains('@'), "{:?}", rows[0]);
        assert!(rows[1].contains('*'), "{:?}", rows[1]);
        assert!(
            !rows.iter().any(|r| r.contains('│')),
            "box drawing survived: {rows:?}"
        );
    }

    #[test]
    fn the_selected_row_is_a_bar_across_the_whole_width() {
        let (mut c, host) = view(LOG, 60, 4);
        c.down();
        let mut screen = Screen::new(60, 4);
        screen.clear(Ink::new(host.theme.chrome.fg, host.theme.chrome.bg));
        c.paint(&mut screen, 0, 0, true, &host);
        let bar = host.theme.chrome.selection_bg;
        assert_eq!(screen.ink(0, 1).unwrap().bg, bar);
        assert_eq!(screen.ink(59, 1).unwrap().bg, bar);
        assert_ne!(screen.ink(0, 0).unwrap().bg, bar);
    }

    /// The clamp, the page and the margin are
    /// [`gitten_core::view::Viewport`]'s and are tested there, over a viewport
    /// and no commits at all. What is this list's is the row the rules land on
    /// meaning a *commit* — everything that opens a diff, copies a sha or names
    /// a rebase target reads `current`, and a cursor that is right while
    /// `current` is off by one is the bug none of those would survive.
    #[test]
    fn each_pane_clips_to_its_span_and_owns_its_scrollbar() {
        // The pane is a guest in the row: it draws from column 20 for 30
        // columns, and the divider on either side of it — and the pane that
        // owns the rest of the screen — must survive a subject far longer
        // than the pane is wide. Long enough to scroll, so the bar is there
        // to be owned.
        let long = "subject ".repeat(12);
        let mut log = format!("l\x1flllllll\x1fc1\x1fAda Lovelace\x1f1700000000\x1f{long}\x1e");
        for i in 1..200 {
            let parent = format!("c{}", i + 1);
            log.push_str(&format!(
                "c{i}\x1fc{i:06}\x1f{parent}\x1fAda Lovelace\x1f17000000{i:02}\x1fCommit {i}\x1e"
            ));
        }
        let (mut c, host) = view(&log, 30, 4);
        c.press(10, 0, false, &host); // a cursor, to paint ink
        let mut screen = Screen::new(60, 5);
        screen.clear(Ink::new(host.theme.chrome.fg, host.theme.chrome.bg));
        // Sentinels on the divider columns, so anything painted outside the
        // pane's span is visible rather than merely absent.
        for y in 0..5 {
            screen.over(19, y, '╎', host.theme.chrome.accent);
            screen.over(50, y, '╎', host.theme.chrome.accent);
        }
        c.paint(&mut screen, 20, 1, true, &host);
        for y in 0..5 {
            assert_eq!(
                screen.char_at(19, y),
                Some('╎'),
                "the pane wrote over the left divider at row {y}"
            );
            assert_eq!(
                screen.char_at(50, y),
                Some('╎'),
                "the pane wrote over the neighbour at row {y}"
            );
        }
        assert!(
            screen.row_text(0).chars().all(|c| c == ' ' || c == '╎'),
            "the pane wrote above its box: {:?}",
            screen.row_text(0)
        );
        // Everything it drew, it drew inside its own columns: nothing past
        // the pane's right edge, and content inside it.
        for x in 51..60 {
            assert_eq!(
                screen.char_at(x, 1),
                Some(' '),
                "the pane drew at column {x}"
            );
        }
        assert!(
            (20..50).any(|x| screen.char_at(x, 1).is_some_and(|c| c != ' ')),
            "nothing was drawn inside the pane"
        );
        // The pane paints no bar of its own — the column past its span is
        // the app's, and the pane's paint leaves it exactly as it found it.
        assert_eq!(
            screen.char_at(49, 1),
            Some(' '),
            "the pane painted a bar into its own last column"
        );
        let under = screen.ink(49, 1).unwrap().bg;
        let divider_under = screen.ink(50, 1).unwrap().bg;
        // The app overlays the bar on the pane's last cell, whose right edge
        // meets the divider, and keeps the row's background underneath it.
        c.paint_bar(&mut screen, 49, Some(50), 1, &host);
        assert_eq!(
            screen.char_at(49, 1),
            Some('▐'),
            "the bar's inner half is missing"
        );
        assert_eq!(
            screen.ink(49, 1).unwrap().bg,
            under,
            "the bar replaced the row background underneath it"
        );
        assert_eq!(
            screen.char_at(50, 1),
            Some('▌'),
            "the bar's divider half is missing"
        );
        assert_eq!(
            screen.ink(50, 1).unwrap().bg,
            divider_under,
            "the bar replaced the divider background"
        );
    }

    #[test]
    fn an_unfocused_pane_draws_no_cursor_bar_but_keeps_its_selection() {
        let (mut c, host) = view(LOG, 60, 4);
        c.press(10, 0, false, &host);
        c.drag(2, &host);
        c.release();
        let mut screen = Screen::new(60, 4);
        screen.clear(Ink::new(host.theme.chrome.fg, host.theme.chrome.bg));
        c.paint(&mut screen, 0, 0, false, &host);
        assert_ne!(
            screen.ink(0, 0).unwrap().bg,
            host.theme.chrome.selection_bg,
            "an unfocused pane drew the cursor bar"
        );
        assert_eq!(
            screen.ink(0, 1).unwrap().bg,
            host.theme.chrome.selected_bg,
            "focus took the dragged selection's ink with it"
        );
    }

    #[test]
    fn the_cursor_and_the_viewport_behave_as_the_diff_views_do() {
        let log: String = (0..100)
            .map(|i| format!("{i:040}\x1f{i:07}\x1f\x1fA\x1f1\x1fc{i}\x1e"))
            .collect();
        let (mut c, _) = view(&log, 60, 10);
        assert_eq!(c.current().map(|x| x.subject.as_str()), Some("c0"));

        c.page(1);
        assert_eq!(
            c.current().map(|x| x.subject.as_str()),
            Some(format!("c{}", c.cursor()).as_str())
        );
        c.to_bottom();
        assert_eq!(c.cursor(), 99, "the viewport is sized to the log");
        assert_eq!(c.current().map(|x| x.subject.as_str()), Some("c99"));
        // Past the end is the end, and `current` follows it rather than
        // answering about a commit that is not there.
        c.go_to(9999);
        assert_eq!(c.cursor(), 99);
        assert_eq!(c.current().map(|x| x.subject.as_str()), Some("c99"));
    }

    #[test]
    fn commit_refresh_anchors_by_sha() {
        let (mut c, _host) = view(&many(50), 60, 10);
        c.move_by(25);
        let sha = c.current().expect("the list has the commit").sha.clone();
        assert_eq!(sha, "c25");
        // A commit inserted above: every row moved down one, and the cursor
        // follows the commit it was on rather than the number it was at.
        let mut inserted = parse_log(&many(50));
        inserted.insert(
            0,
            Commit {
                sha: "new".into(),
                short: "newest".into(),
                parents: Box::from(&["c0".to_string()][..]),
                author: "Ada Lovelace".into(),
                timestamp: 1,
                subject: "newest".into(),
            },
        );
        c.replace(inserted);
        assert_eq!(c.current().map(|x| x.sha.as_str()), Some("c25"));
        // A commit removed above: rows moved up, and the anchor holds.
        let mut removed = parse_log(&many(50));
        removed.remove(0);
        c.replace(removed);
        assert_eq!(c.current().map(|x| x.sha.as_str()), Some("c25"));
        // The anchored commit vanishes entirely: the previous position
        // clamps into what the shorter list can hold, and nothing panics.
        c.replace(parse_log(&many(10)));
        assert_eq!(c.cursor(), 9, "clamped, not wrapped");
        assert!(c.current().is_some());
        // A drag's range was a promise about rows the refresh renumbered.
        let (mut c, host) = view(&many(50), 60, 10);
        c.move_by(5);
        c.press(20, 2, false, &host);
        c.drag(6, &host);
        assert!(c.selection().len() > 1);
        c.replace(parse_log(&many(50)));
        assert_eq!(c.selection(), "", "a range outlived the rows it held");
        // Glyphs and dimensions survive: the refresh changed what, not how.
        let (c, host) = view(&many(50), 60, 10);
        let rows = painted(&c, &host);
        assert!(rows[0].starts_with("c000000 AL "), "{:?}", rows[0]);
    }

    #[test]
    fn an_empty_log_draws_nothing_and_panics_nowhere() {
        let (mut c, host) = view("", 40, 6);
        assert!(c.is_empty());
        c.down();
        c.page(-1);
        c.to_bottom();
        assert_eq!(c.cursor(), 0);
        assert_eq!(c.current(), None);
        let rows = painted(&c, &host);
        assert!(rows.iter().all(|r| r.is_empty()), "{rows:?}");
        assert_eq!(c.status(), "0/0 · 1 lanes");
    }

    #[test]
    fn a_narrow_terminal_clips_the_subject_rather_than_the_graph() {
        // The graph is the part that cannot be reconstructed by reading; a
        // truncated subject still says what it is.
        let (c, host) = view(LOG, 20, 4);
        let rows = painted(&c, &host);
        assert!(
            rows[0].contains('◉'),
            "the graph was clipped first: {:?}",
            rows[0]
        );
    }

    // ----------------------------------------------------------------- search

    #[test]
    fn a_query_filters_all_three_fields_in_source_order() {
        // Four commits so each of the three fields has its own hit: a sha, an
        // author and a subject, plus one commit nothing below matches. Shas are
        // written whole — the index folds `sha`, and a short is a prefix of it.
        let log = "\
1111aaaa\x1f1111aaa\x1f2222beef\x1fAda Lovelace\x1f1\x1fthe engine, first\x1e\
2222beef\x1f2222bee\x1fcafe3333\x1fGrace Hopper\x1f2\x1fnothing interesting\x1e\
cafe3333\x1fcafe333\x1f4444dddd\x1fÉmile Zola\x1f3\x1fa compiler pass\x1e\
4444dddd\x1f4444ddd\x1f\x1fAda Lovelace\x1f4\x1fnothing else\x1e";
        let (mut c, host) = view(log, 60, 4);

        c.apply_query("engine");
        assert_eq!(c.query(), Some("engine"));
        assert_eq!(c.filter_note().as_deref(), Some("1/4"));
        assert_eq!(
            c.current().map(|x| x.sha.as_str()),
            Some("1111aaaa"),
            "a subject hit"
        );

        c.apply_query("hopper");
        assert_eq!(c.filter_note().as_deref(), Some("1/4"));
        assert_eq!(
            c.current().map(|x| x.sha.as_str()),
            Some("2222beef"),
            "an author hit"
        );

        c.apply_query("333");
        assert_eq!(c.filter_note().as_deref(), Some("1/4"));
        assert_eq!(
            c.current().map(|x| x.sha.as_str()),
            Some("cafe3333"),
            "a sha hit, interior of the hash and all"
        );

        // Two hits, and what is drawn keeps core's order — which is the
        // source's, ascending, never a search's own idea of relevance.
        c.apply_query("ada");
        assert_eq!(c.filter_note().as_deref(), Some("2/4"));
        let rows = painted(&c, &host);
        assert!(rows[0].contains("engine, first"), "{:?}", rows[0]);
        assert!(rows[1].contains("nothing else"), "{:?}", rows[1]);
        assert!(rows[2].is_empty(), "filtered rows are not drawn: {rows:?}");
    }

    #[test]
    fn filtering_anchors_the_cursor_by_sha_and_a_miss_clamps() {
        let (mut c, host) = view(&mixed(30), 60, 10);
        // The keyboard sits on an *even* commit — one that survives "ENGINE".
        for _ in 0..4 {
            c.down();
        }
        let anchored = c.current().expect("a commit under the cursor").sha.clone();
        // Trimmed like the window trims it: spaces around the needle.
        c.apply_query("  ENGINE  ");
        assert_eq!(c.filter_note().as_deref(), Some("15/30"));
        assert_eq!(
            c.current().map(|x| x.sha.as_str()),
            Some(anchored.as_str()),
            "the cursor left its commit because a row number moved"
        );

        // Now the anchor cannot survive: the query names the other half, and
        // the cursor clamps onto the surviving rows instead of pointing past
        // the end of a list that shrank under it.
        c.apply_query("compiler");
        assert_eq!(c.filter_note().as_deref(), Some("15/30"));
        let vis = c.view.len();
        assert!(c.cursor() < vis, "the cursor outlived its own list");
        assert!(c.current().is_some());

        // Zero hits: nothing is current, the numbers stay honest and nothing
        // panics — the list is empty, not broken.
        c.apply_query("nothing matches this");
        assert_eq!(c.filter_note().as_deref(), Some("0/30"));
        assert_eq!(
            c.len(),
            30,
            "the filter narrows what is shown, never what is loaded"
        );
        assert_eq!(c.current(), None);
        assert_eq!(c.cursor(), 0);
        assert!(painted(&c, &host).iter().all(|r| r.is_empty()));
    }

    #[test]
    fn clearing_a_query_restores_every_row_and_copy_uses_visible_rows() {
        let (mut c, host) = view(&mixed(30), 60, 10);
        c.apply_query("engine");
        assert_eq!(c.filter_note().as_deref(), Some("15/30"));

        // A drag over three visible rows copies three *engine* rows: the
        // selection speaks visible order, and nothing a query removed can leak
        // into it through a source-row slice.
        c.press(20, 0, false, &host);
        c.drag(2, &host);
        c.release();
        let text = c.selection();
        assert_eq!(text.lines().count(), 3, "{text:?}");
        assert!(text.contains("engine note 0"), "{text:?}");
        assert!(
            !text.contains("compiler"),
            "a filtered-out row was copied: {text:?}"
        );

        // Whitespace is as good as empty, and empty is the whole list again.
        // Clearing anchors by sha: the keyboard keeps its commit — which was
        // visible row two of the filter and is the history's fifth row whole —
        // and the copy key names that commit, read through the restored table.
        let under = c.current().map(|x| x.sha.clone());
        c.apply_query("   ");
        assert_eq!(c.query(), None);
        assert_eq!(c.filter_note(), None);
        assert_eq!(c.view.len(), 30);
        assert_eq!(
            c.current().map(|x| x.sha.as_str()),
            under.as_deref(),
            "clearing moved the cursor off its commit"
        );
        assert_eq!(c.copy_text(), "00000004 engine note 4");
    }

    #[test]
    fn a_filtered_cursor_keeps_the_existing_selection_bar() {
        // Filtering is the hit marker, exactly as in the window: the cursor's
        // own row keeps the bar it always had, the surviving rows stay plain —
        // they are hits, but every row on screen is a hit and a wall of colour
        // would say nothing — and the drag colour stays the drag's.
        let (mut c, host) = view(LOG, 60, 4);
        c.apply_query("branch");
        assert_eq!(c.filter_note().as_deref(), Some("2/4"));
        let mut screen = Screen::new(c.cols, 4);
        screen.clear(Ink::new(host.theme.chrome.fg, host.theme.chrome.bg));
        c.paint(&mut screen, 0, 0, true, &host);
        assert_eq!(screen.ink(0, 0).unwrap().bg, host.theme.chrome.selection_bg);
        assert_eq!(
            screen.ink(59, 0).unwrap().bg,
            host.theme.chrome.selection_bg,
            "the bar runs the whole width"
        );
        assert_eq!(
            screen.ink(0, 1).unwrap().bg,
            host.theme.chrome.bg,
            "an ordinary hit is not lit twice"
        );
        assert_ne!(
            screen.ink(0, 1).unwrap().bg,
            host.theme.chrome.selected_bg,
            "nothing dragged, nothing selected"
        );
    }
}
