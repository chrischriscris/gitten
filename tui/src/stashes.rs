//! The stash stack, as a flat list — the terminal's share of the window's
//! stash pane.
//!
//! lazygit's stash panel, and the window's with it, is one flat stack: newest
//! first as the read gives it, each row named `stash@{n}` beside its message,
//! the address first because the address is what every verb on this pane
//! aims at and the message is only what the entry says about itself. The
//! terminal draws exactly that and nothing more — no sections, no dates, no
//! preview — because the read model carries exactly that and nothing more
//! ([`gitten_core::refs::Stash`]).
//!
//! The one subtlety worth a paragraph is **renumbering**. Every drop or pop
//! renumbers everything above it, so an index cannot anchor the cursor
//! across a refresh and cannot be trusted to survive one: each row keeps the
//! entry's full commit ([`Row::commit`]) as its identity, a refresh follows
//! the keyboard's commit to its new row and clamps when it is gone, and an
//! armed drop dies on *every* refresh — a yes addressed to yesterday's
//! numbering is the accident the double press exists to prevent.
//!
//! Selection is the cursor and deliberately nothing else: the verbs act on
//! one stash at a time, so a drag may move the cursor or the scrollbar but
//! never builds a range, and `copy.selection` falls back to the row the
//! keyboard is on — the same answers the window's pane gives.

use crate::screen::{Ink, Screen};
use crate::scrollbar::{self, Bar};
use gitten_core::host::Host;
use gitten_core::refs::{Stash, StashId};
use gitten_core::search::TextIndex;
use gitten_core::view::Viewport;

/// One flat row of the pane: one entry of the stack, flattened once per
/// refresh and never per frame. The address is spelled here, once; what a
/// draw reads is a field away and allocates nothing.
struct Row {
    /// The position on the stack — the `n` of `stash@{n}`, and how every
    /// verb addresses this entry back to git.
    index: usize,
    /// The commit the stash hangs on, kept as this row's identity across a
    /// refresh: indices renumber under a drop, the commits do not.
    commit: String,
    /// `stash@{n}`, spelled once at flatten.
    title: String,
    /// What the entry says about itself.
    message: String,
}

/// Spells the address the way git does, once, at flatten.
fn title(index: usize) -> String {
    format!("stash@{{{index}}}")
}

/// The stash list.
///
/// Holds the flattened stack, the viewport and the drop arm; knows nothing
/// about keys. Every method is a command, exactly as in
/// [`crate::commits::Commits`] and for the same reason.
pub struct Stashes {
    rows: Vec<Row>,
    /// Every row's search text, folded once at load — the message a person
    /// reads, plus the address git spells. See
    /// [`gitten_core::search::TextIndex`].
    search: TextIndex,
    /// The standing query, `None` when the list is whole — always the
    /// *trimmed* text, the same normalization the commit list applies.
    query: Option<String>,
    /// Which source rows the viewport can see, ascending — the one
    /// visible-to-source table every row reader goes through. Unfiltered it
    /// is `0..len`; filtered it is what [`TextIndex::indices`] answered.
    visible: Vec<usize>,
    /// Whether the stack behind these rows was read at all. `false` after a
    /// failed ancillary read: the pane opens anyway — a failed side read
    /// must not abort a launch the main view made good — but it draws as
    /// unavailable, exposes no row to act on, and says something different
    /// from a stack that was successfully read as empty.
    available: bool,
    /// The cursor, the top row and the height. The shared
    /// [`Viewport`], because a scroll rule two views hold separately is a
    /// scroll rule that drifts.
    view: Viewport,
    cols: usize,
    bar: Bar,
    /// The drop awaiting its second press: the *identity* of the row that
    /// asked — the entry's commit, and the position it was at.
    ///
    /// The identity and not the number, because the number is what churns:
    /// a yes addressed to `stash@{1}` must never be spent on whatever
    /// `stash@{1}` became. One slot — arming a different row moves the
    /// question, never queues two. Killed by any cursor move, any moving
    /// scroll, any mouse row change and any refresh; a focus round trip
    /// alone does not touch it, because the question sits on the row it was
    /// asked about.
    armed: Option<StashId>,
    dragging: bool,
}

impl Stashes {
    /// A successfully read stack. An empty vector is one too — nothing
    /// parked is a state, not a failure.
    pub fn new(stashes: Vec<Stash>) -> Self {
        let mut view = Viewport::new();
        view.set_len(stashes.len());
        let mut this = Self {
            rows: flatten(&stashes),
            search: TextIndex::new(Vec::<String>::new()),
            query: None,
            visible: Vec::new(),
            available: true,
            view,
            cols: 0,
            bar: Bar::default(),
            armed: None,
            dragging: false,
        };
        this.reindex();
        this
    }

    /// The pane after a failed read: honest emptiness that is *not* the
    /// empty-stack line, no row to act on, and recoverable — the next
    /// successful refresh replaces it outright.
    pub fn unavailable() -> Self {
        Self {
            rows: Vec::new(),
            search: TextIndex::new(Vec::<String>::new()),
            query: None,
            visible: Vec::new(),
            available: false,
            view: Viewport::new(),
            cols: 0,
            bar: Bar::default(),
            armed: None,
            dragging: false,
        }
    }

    /// The row the viewport names, through the visible table — the cursor is
    /// a row of the *filtered* list, and only the final lookup names a row of
    /// the source.
    fn row_at(&self, visual: usize) -> Option<&Row> {
        self.rows.get(*self.visible.get(visual)?)
    }

    /// Rebuilds the folded search texts against the rows as they stand.
    fn reindex(&mut self) {
        self.search = TextIndex::new(
            self.rows
                .iter()
                .map(|r| format!("{} {}", r.title, r.message)),
        );
        self.refilter();
    }

    /// Rebuilds the visible table against the standing query and re-clamps
    /// the viewport. `apply_query` and `reindex` land here.
    fn refilter(&mut self) {
        self.visible = match &self.query {
            Some(q) => self.search.indices(q),
            None => Vec::from_iter(0..self.rows.len()),
        };
        self.view.set_len(self.visible.len());
    }

    // ----------------------------------------------------------------- search

    /// The live query, for pre-filling a second `/`. `None` when unfiltered.
    pub fn query(&self) -> Option<&str> {
        self.query.as_deref()
    }

    /// The filter while one stands, for a status line: `2/5` — hits over
    /// parked. `None` unfiltered.
    pub fn filter_note(&self) -> Option<String> {
        self.query
            .is_some()
            .then(|| format!("{}/{}", self.visible.len(), self.rows.len()))
    }

    /// Sets the filter — once per keystroke, and never anywhere else. The
    /// keyboard stays on its entry: anchored by the commit under it — the
    /// identity that survives a drop's renumbering — into the next result
    /// set wherever it survives the narrower query, and clamped when it does
    /// not. An empty (or whitespace-only) query is no query, so clearing
    /// restores the whole stack; the same trimmed query twice rebuilds
    /// nothing. An armed drop dies with a result set that changed, like any
    /// other refresh: the question was about a row of yesterday's list.
    pub fn apply_query(&mut self, query: &str) {
        let next = Some(query.trim()).filter(|q| !q.is_empty());
        if self.query.as_deref() == next {
            return;
        }
        let anchored = self.row_at(self.view.cursor()).map(|r| r.commit.clone());
        self.query = next.map(str::to_string);
        self.refilter();
        self.armed = None;
        let cursor = anchored
            .and_then(|c| {
                self.visible
                    .iter()
                    .position(|&r| self.rows.get(r).is_some_and(|row| row.commit == c))
            })
            .unwrap_or_else(|| self.view.cursor());
        self.view.go_to(cursor);
    }

    /// The next — or previous — row of the visible list, wrapping. With a
    /// filter standing the visible list *is* the matches, so this is what
    /// iterating them is; with none standing it says so by doing nothing.
    pub fn next_match(&mut self, by: isize) {
        if self.query.is_none() || self.visible.is_empty() {
            return;
        }
        let len = self.visible.len() as isize;
        let at = (self.view.cursor() as isize + by).rem_euclid(len) as usize;
        self.armed = None;
        self.view.go_to(at);
    }

    /// Takes the filter off. The one door `search.clear` opens, so a search
    /// that is cancelled restores the list it filtered.
    pub fn clear_search(&mut self) {
        self.apply_query("");
    }

    /// Swaps in a refreshed stack, keeping the keyboard on its entry.
    ///
    /// An index cannot anchor — dropping any entry renumbers every later
    /// one — but the commit under an entry is stable, so the cursor follows
    /// its commit to the new row and, when the entry itself is gone, clamps
    /// onto whatever the new stack holds. A refresh is also the repository
    /// saying things moved: an armed drop was a promise about how they were,
    /// so it dies here first, and so does the mouse's hold on a thumb or a
    /// gesture that may no longer mean anything.
    ///
    /// A successful read, empty or not, also clears the unavailable state —
    /// the recovery path of a pane that opened on a failed side read.
    pub fn replace(&mut self, stashes: Vec<Stash>) {
        self.armed = None;
        self.dragging = false;
        self.available = true;
        let (cursor, top) = (self.view.cursor(), self.view.top());
        let anchored = self.rows.get(cursor).map(|r| r.commit.clone());
        self.rows = flatten(&stashes);
        // The folded search texts and the visible table were built against
        // the stack the pane held; a refresh may have added entries under a
        // standing filter, and both are rebuilt with the list.
        self.reindex();
        // The old scroll position first, then the anchor: `go_to` drags the
        // viewport after the cursor, and the surviving commit's row must be
        // the one on screen when it survives.
        self.view.scroll_to(top);
        let at = anchored
            .and_then(|c| {
                self.visible
                    .iter()
                    .position(|&r| self.rows.get(r).is_some_and(|row| row.commit == c))
            })
            .unwrap_or_else(|| cursor.min(self.visible.len().saturating_sub(1)));
        self.view.go_to(at);
    }

    // ------------------------------------------------------------- the viewport

    /// How much lead the cursor keeps at the edge. `[view] scrolloff`.
    pub fn set_scrolloff(&mut self, rows: usize) {
        self.view.set_scrolloff(rows);
    }

    /// The glyphs the scrollbar is drawn with. `--ascii`, or an extension.
    pub fn set_bar(&mut self, bar: Bar) {
        self.bar = bar;
    }

    pub fn resize(&mut self, cols: usize, height: usize) {
        self.cols = cols;
        self.view.set_height(height);
    }

    /// The row the keyboard is on, as the verbs address it: the stack index,
    /// the `n` of `stash@{n}`. `None` on an empty stack — and on an
    /// unavailable one, which exposes no row to act on at all.
    pub fn current(&self) -> Option<usize> {
        if !self.available {
            return None;
        }
        self.row_at(self.view.cursor()).map(|r| r.index)
    }

    /// The entry the keyboard is on, as both its identities: the commit that
    /// survives a drop, and the place on the stack it was at — what every
    /// verb on this pane is aimed with, and what a preview is anchored by.
    /// See [`StashId`] for why both travel.
    pub fn current_id(&self) -> Option<StashId> {
        let index = self.current()?;
        self.rows
            .iter()
            .find(|row| row.index == index)
            .map(|row| StashId {
                index: row.index,
                commit: row.commit.clone(),
            })
    }

    /// What a stash entry says about itself, by its commit — the display
    /// half of a preview's label, addressed by the identity that does not
    /// renumber.
    pub fn message_of(&self, commit: &str) -> Option<&str> {
        self.rows
            .iter()
            .find(|row| row.commit == commit)
            .map(|row| row.message.as_str())
    }

    /// One row down or up. A keyboard move always disarms the drop: the
    /// question was asked about the row that was under the keyboard, and a
    /// disarm that fires once too often costs a second press, while one
    /// that fires once too late costs a stash.
    pub fn move_by(&mut self, by: isize) {
        self.armed = None;
        self.view.move_by(by);
    }

    pub fn down(&mut self) {
        self.move_by(1);
    }

    pub fn up(&mut self) {
        self.move_by(-1);
    }

    pub fn page(&mut self, pages: isize) {
        self.armed = None;
        self.view.page(pages);
    }

    /// The wheel. Disarms only when it actually moved the list — a wheel
    /// spun against the end of a short stack moved nothing, and the
    /// question still sits on the row it was asked about.
    pub fn scroll_y(&mut self, by: isize) {
        let before = self.view.top();
        self.view.pan_by(by);
        if self.view.top() != before {
            self.armed = None;
        }
    }

    pub fn to_top(&mut self) {
        self.armed = None;
        self.view.to_top();
    }

    pub fn to_bottom(&mut self) {
        self.armed = None;
        self.view.to_bottom();
    }

    // ---------------------------------------------------------------- the mouse

    /// A press in the list: the cursor moves there. `extend` is accepted for
    /// the shape every list's press shares and deliberately ignored — the
    /// verbs act on one stash at a time, so shift starts no range here.
    ///
    /// A press that moves the keyboard off the armed row takes the question
    /// with it; a click on the armed row itself is neither an answer nor a
    /// re-ask, and the question stands until its key is pressed again.
    pub fn press(&mut self, _col: usize, row: usize, _extend: bool, _host: &Host) {
        let Some(index) = self.view.row_at(row) else {
            return;
        };
        self.view.go_to(index);
        self.disarm_if_row_moved(index);
    }

    /// The pointer moved with the button down. A row above or below the body
    /// scrolls by the overshoot; a drag never builds a range, so what it can
    /// move is the cursor and nothing else.
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
        self.view.go_to(index);
        self.disarm_if_row_moved(index);
    }

    pub fn release(&mut self) {
        self.dragging = false;
    }

    /// The press or drag landed the keyboard on row `index`; the question,
    /// if one stands, was asked about the row it stood on. Different rows,
    /// no question.
    fn disarm_if_row_moved(&mut self, index: usize) {
        let at = self
            .visible
            .get(index)
            .and_then(|&r| self.rows.get(r))
            .map(|r| StashId {
                index: r.index,
                commit: r.commit.clone(),
            });
        if self.armed.is_some() && self.armed != at {
            self.armed = None;
        }
    }

    /// Arms — or confirms — a drop of this exact entry. First call on a
    /// target stores it and returns false: ask, don't act. Second call on
    /// the same target clears the arm and returns true: act. Anything else
    /// re-arms onto the new target and returns false again.
    ///
    /// The arm holds the whole [`StashId`], commit included, which is what
    /// makes a stale yes impossible rather than merely unlikely: after a drop
    /// or a pop every later number shifts, so an arm keyed on the number
    /// alone could be spent on the entry that inherited it. A refresh
    /// disarms unconditionally on top of that — belt and braces, and the
    /// cheaper of the two to be sure about.
    pub fn confirm_or_arm_drop(&mut self, id: &StashId) -> bool {
        let already = self.armed.as_ref() == Some(id);
        self.armed = match already {
            true => None,
            false => Some(id.clone()),
        };
        already
    }

    // ------------------------------------------------------- copy and selection

    /// What `copy.selection` copies here: the row the keyboard is on, as git
    /// would spell it — the address, then the message. Empty on an empty or
    /// unavailable stack, because there is nothing to name.
    pub fn copy_text(&self) -> String {
        match self
            .current()
            .and_then(|_| self.rows.get(self.view.cursor()))
        {
            Some(r) => format!("{} {}", r.title, r.message),
            None => String::new(),
        }
    }

    /// What the *mouse* has selected: nothing, ever. A gesture may move the
    /// cursor or the scrollbar; a stack is acted on one entry at a time, and
    /// a multi-stash selection would have no verb to hand itself to.
    pub fn selection(&self) -> String {
        String::new()
    }

    /// `select.all`. Inert: there is no range to grow.
    pub fn select_all(&mut self) {}

    /// `select.none`. Says there was no range to drop, so `esc` falls
    /// through to whatever it means next.
    pub fn select_none(&mut self) -> bool {
        false
    }

    // ------------------------------------------------------------- the drawing

    /// Draws the visible rows into `screen`, at `x` of row `y` onward, inside
    /// this pane's own columns.
    ///
    /// Every row is taken through [`Screen::span`], never [`Screen::row`]:
    /// the pane is a guest in the row, and a long message that wrote to the
    /// whole screen would overwrite the divider and whatever sits beside it.
    /// The cursor background draws only while this pane holds the keyboard;
    /// an armed row's message wears the error ink whether focused or not,
    /// because the question stands in both states.
    pub fn paint(&self, screen: &mut Screen, x: usize, y: usize, focused: bool, host: &Host) {
        let theme = &host.theme;
        let blank = Ink::new(theme.chrome.dim, theme.chrome.bg);
        // An empty or unreadable stack is one quiet line, and which line says
        // which: a successful read of an empty stack is `nothing stashed`, a
        // failed read is `stash list unavailable` — never the one drawn as
        // the other, because the first asserts a read that happened and the
        // second admits one that did not.
        let quiet = if !self.available {
            Some((
                "stash list unavailable",
                Ink::new(theme.chrome.error, theme.chrome.bg),
            ))
        } else if self.rows.is_empty() {
            Some((
                "nothing stashed",
                Ink {
                    italic: true,
                    ..Ink::new(theme.chrome.faint, theme.chrome.bg)
                },
            ))
        } else {
            None
        };
        if let Some((text, ink)) = quiet {
            let mut pen = screen.span(y, x, self.cols);
            pen.put(text, ink);
            pen.wash(blank);
        } else {
            for i in 0..self.view.height() {
                let row = y + i;
                let mut pen = screen.span(row, x, self.cols);
                let Some(vis) = self.view.row_at(i) else {
                    pen.wash(blank);
                    continue;
                };
                let Some(r) = self.visible.get(vis).and_then(|&r| self.rows.get(r)) else {
                    pen.wash(blank);
                    continue;
                };
                let bg = match focused && vis == self.view.cursor() {
                    true => theme.chrome.selection_bg,
                    false => theme.chrome.bg,
                };
                let address = Ink::new(theme.chrome.dim, bg);
                let armed = self
                    .armed
                    .as_ref()
                    .is_some_and(|id| id.index == r.index && id.commit == r.commit);
                let body = Ink::new(
                    match armed {
                        true => theme.chrome.error,
                        false => theme.chrome.fg,
                    },
                    bg,
                );
                pen.put(&r.title, address);
                pen.put(" ", address);
                pen.put(&r.message, body);
                // The background runs to the pane's edge, the way every list
                // row here does — a bar that stops after the last character
                // is a ragged margin down the stack.
                pen.wash(body);
            }
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

    /// One line describing the pane, for whatever draws a status bar: where
    /// the keyboard is, over what is parked — or, on the two states that are
    /// not a readable stack, the one word that says which state it is.
    pub fn status(&self) -> String {
        if !self.available {
            return "unavailable".into();
        }
        if self.rows.is_empty() {
            return "0 parked".into();
        }
        let shown = self.visible.len();
        let at = self
            .row_at(self.view.cursor())
            .map(|r| r.title.as_str())
            .unwrap_or("");
        format!("{}/{shown} · {at}", (self.view.cursor() + 1).min(shown),)
    }
}

/// Flattens the stack into display rows, newest first as the read gives it.
fn flatten(stashes: &[Stash]) -> Vec<Row> {
    stashes
        .iter()
        .map(|s| Row {
            index: s.index,
            commit: s.commit.clone(),
            title: title(s.index),
            message: s.message.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two entries, newest first as the read gives them — the shape every
    /// drawing and refresh assertion below is pinned to.
    fn stack() -> Vec<Stash> {
        vec![
            Stash {
                index: 0,
                message: "On main: wip things".into(),
                commit: "aaa".into(),
            },
            Stash {
                index: 1,
                message: "On dev: other work".into(),
                commit: "bbb".into(),
            },
        ]
    }

    /// Twenty entries, so the pane has something a wheel and a thumb can
    /// actually move.
    fn tall_stack() -> Vec<Stash> {
        (0..20)
            .map(|i| Stash {
                index: i,
                message: format!("On branch {i}: parked work"),
                commit: format!("c{i:02}"),
            })
            .collect()
    }

    #[test]
    fn an_empty_stack_is_a_slanted_quiet_line() {
        let host = Host::new();
        let mut v = Stashes::new(Vec::new());
        v.resize(30, 3);
        let mut screen = Screen::new(30, 3);
        screen.clear(Ink::new(host.theme.chrome.fg, host.theme.chrome.bg));
        v.paint(&mut screen, 0, 0, true, &host);
        assert!(screen.row_text(0).contains("nothing stashed"));
        let ink = screen.ink(0, 0).unwrap();
        assert!(ink.italic, "the quiet line lost its slant");
        assert_eq!(ink.fg, host.theme.chrome.faint);
    }

    #[test]
    fn stash_rows_are_address_then_message_and_empty_is_quiet() {
        let host = Host::new();
        let c = &host.theme.chrome;
        // Painted at a nonzero x, in a pane narrower than the screen: the
        // pane is a guest in the row, and whatever it draws stops at its own
        // edge. The sentinel proves nothing outside the span took a cell.
        let (x, cols) = (7, 30);
        let mut screen = Screen::new(44, 6);
        let sentinel = Ink::new(0x112233, 0x445566);
        screen.clear(sentinel);

        let mut v = Stashes::new(stack());
        v.resize(cols, 6);
        v.paint(&mut screen, x, 0, true, &host);

        // Address first, in the furniture ink, then the message in the
        // normal text ink — one row, address ahead of what it names.
        let row = screen.row_text(0);
        assert!(row.contains("stash@{0}"), "{row:?}");
        assert!(row.contains("On main: wip things"), "{row:?}");
        let at = |needle: &str| row.find(needle).expect(needle) + x;
        assert_eq!(screen.ink(at("stash@{0}"), 0).unwrap().fg, c.dim);
        assert_eq!(screen.ink(at("On main"), 0).unwrap().fg, c.fg);
        // The keyboard is on the row, so the cursor background is the row's
        // — and only this row's.
        assert_eq!(screen.ink(x, 0).unwrap().bg, c.selection_bg);
        assert_eq!(screen.ink(x, 1).unwrap().bg, c.bg);
        assert!(screen.row_text(1).contains("stash@{1}"));
        // Nothing outside the pane's span changed.
        assert_eq!(screen.ink(x - 1, 0), Some(sentinel));
        assert_eq!(screen.ink(x + cols, 0), Some(sentinel));
        assert_eq!(v.status(), "1/2 · stash@{0}");
        assert_eq!(v.current(), Some(0));

        // Unfocused, the cursor background goes; the row stays drawn.
        let mut screen = Screen::new(44, 6);
        v.paint(&mut screen, x, 0, false, &host);
        assert_eq!(screen.ink(x, 0).unwrap().bg, c.bg);
        assert!(screen.row_text(0).contains("stash@{0}"));

        // An empty stack is a quiet line — and *only* emptiness says so.
        let mut empty = Stashes::new(Vec::new());
        empty.resize(cols, 6);
        let mut screen = Screen::new(44, 6);
        empty.paint(&mut screen, x, 0, true, &host);
        assert!(
            screen.row_text(0).contains("nothing stashed"),
            "{:?}",
            screen.row_text(0)
        );
        assert!(!screen.row_text(0).contains("unavailable"));
        assert_eq!(empty.status(), "0 parked");
        assert_eq!(empty.current(), None);

        // A read that failed says so, and is never drawn as success.
        let mut failed = Stashes::unavailable();
        failed.resize(cols, 6);
        let mut screen = Screen::new(44, 6);
        failed.paint(&mut screen, x, 0, true, &host);
        assert!(
            screen.row_text(0).contains("stash list unavailable"),
            "{:?}",
            screen.row_text(0)
        );
        assert!(!screen.row_text(0).contains("nothing stashed"));
        assert_eq!(failed.status(), "unavailable");
        assert_eq!(
            failed.current(),
            None,
            "an unavailable stack exposes no row to act on"
        );
        assert_eq!(failed.copy_text(), "");
    }

    #[test]
    fn stash_refresh_follows_commit_identity_and_renumbers_titles() {
        let mut v = Stashes::new(stack());
        v.resize(30, 6);
        // The keyboard on the *second* entry — the one a drop above it will
        // renumber.
        v.view.go_to(1);
        assert_eq!(v.current(), Some(1));

        // The row above the cursor leaves the stack; the cursor follows its
        // commit to the row it now sits at, and the address it draws — and
        // sends to the verbs — is the *new* numbering.
        v.replace(vec![Stash {
            index: 0,
            message: "On dev: other work".into(),
            commit: "bbb".into(),
        }]);
        assert_eq!(v.view.cursor(), 0, "the same commit, at its new row");
        assert_eq!(v.current(), Some(0), "the index renumbered with the stack");
        assert_eq!(v.status(), "1/1 · stash@{0}");

        // The selected commit itself is gone: clamp onto what survives.
        v.replace(vec![Stash {
            index: 0,
            message: "On main: wip things".into(),
            commit: "aaa".into(),
        }]);
        assert_eq!(v.view.cursor(), 0);
        assert_eq!(v.current(), Some(0));

        // And emptied wholesale: cursor and viewport both at the top, and no
        // row for anything to act on.
        v.replace(Vec::new());
        assert_eq!((v.view.cursor(), v.view.top()), (0, 0));
        assert_eq!(v.current(), None);
        assert_eq!(v.status(), "0 parked");
    }

    /// The identity of stack row `i` of a twenty-deep stack — what
    /// [`Stashes::current_id`] would hand a verb standing there.
    fn tall_id(i: usize) -> StashId {
        StashId {
            index: i,
            commit: format!("c{i:02}"),
        }
    }

    #[test]
    fn stash_drop_arm_survives_only_the_same_row() {
        let host = Host::new();
        let mut v = Stashes::new(tall_stack());
        v.resize(30, 6);

        // First press asks; second press on the same row acts, and the act
        // spends the arm.
        assert!(!v.confirm_or_arm_drop(&tall_id(0)));
        assert_eq!(v.armed, Some(tall_id(0)));
        assert!(v.confirm_or_arm_drop(&tall_id(0)));
        assert_eq!(v.armed, None);

        // A different row re-arms rather than inheriting the question.
        assert!(!v.confirm_or_arm_drop(&tall_id(1)));
        assert_eq!(v.armed, Some(tall_id(1)));
        assert!(
            !v.confirm_or_arm_drop(&tall_id(0)),
            "another row asks again"
        );
        assert_eq!(v.armed, Some(tall_id(0)));

        // The same *number* under a different commit is a different entry,
        // and asks again — which is the whole reason the arm holds the
        // identity and not the position. The question standing here is
        // stash@{0}'s, from the line above.
        let inherited = StashId {
            index: 0,
            commit: "somebody-elses".into(),
        };
        assert!(
            !v.confirm_or_arm_drop(&inherited),
            "a stale yes was not spent on the entry that inherited the number"
        );
        assert_eq!(v.armed, Some(inherited));

        // A keyboard move disarms: the question was about the row that was
        // under the keyboard.
        v.confirm_or_arm_drop(&tall_id(0));
        v.down();
        assert_eq!(v.armed, None);
        // ...a wheel that actually moved the list...
        v.confirm_or_arm_drop(&tall_id(0));
        v.scroll_y(3);
        assert_eq!(v.armed, None, "a moving scroll disarms");
        // ...a press on another row...
        v.confirm_or_arm_drop(&tall_id(0));
        v.press(3, 2, false, &host);
        assert_eq!(v.armed, None, "a mouse row change disarms");
        // ...and a refresh, unconditionally: indices renumber under a drop,
        // and belt beats braces on the one question that destroys work.
        v.confirm_or_arm_drop(&tall_id(0));
        v.replace(stack());
        assert_eq!(v.armed, None, "a refresh disarms");
        v.replace(tall_stack());

        // A click on the armed row itself is neither an answer nor a re-ask.
        v.confirm_or_arm_drop(&tall_id(0));
        v.press(3, 0, false, &host);
        assert_eq!(v.armed, Some(tall_id(0)));

        // Merely painting the pane unfocused, then focused — the focus round
        // trip — moves nothing: the question sits on its row.
        let mut screen = Screen::new(44, 6);
        v.paint(&mut screen, 0, 0, false, &host);
        assert_eq!(v.armed, Some(tall_id(0)));
        // The armed row wears the error ink with the keyboard *elsewhere*:
        // the address keeps its furniture ink, the message is the thing
        // being asked about, and neither waits for focus.
        let c = &host.theme.chrome;
        assert_eq!(screen.ink(0, 0).unwrap().fg, c.dim);
        assert_eq!(screen.ink(11, 0).unwrap().fg, c.error);
        v.paint(&mut screen, 0, 0, true, &host);
        assert_eq!(
            v.armed,
            Some(tall_id(0)),
            "the focus round trip moved nothing"
        );
        assert_eq!(screen.ink(11, 0).unwrap().fg, c.error);
    }

    #[test]
    fn the_row_the_keyboard_is_on_answers_with_both_its_identities() {
        // What every verb on this pane is aimed with. The commit is the
        // identity; the number is what the reader saw and what git's own
        // pop and drop insist on. Unavailable and empty both answer with no
        // row at all, which is a different thing from row zero.
        let mut v = Stashes::new(stack());
        v.resize(30, 6);
        assert_eq!(
            v.current_id(),
            Some(StashId {
                index: 0,
                commit: "aaa".into()
            })
        );
        v.down();
        assert_eq!(
            v.current_id(),
            Some(StashId {
                index: 1,
                commit: "bbb".into()
            })
        );
        v.replace(Vec::new());
        assert_eq!(v.current_id(), None);
        assert_eq!(Stashes::unavailable().current_id(), None);
    }
}
