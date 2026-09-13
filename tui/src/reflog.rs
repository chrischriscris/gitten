//! Where HEAD has been, newest first — the terminal's share of the
//! reflog browser.
//!
//! One row per reflog entry: the commit HEAD pointed at, the selector that
//! addresses the entry back to git (`HEAD@{3}`), and the message naming what
//! moved it. The selector is git's own addressing and the only thing a
//! recovery verb aims at; the message is display text that also tells the
//! reader *what kind* of move each row was — a commit, a checkout, a reset —
//! because "what would this put back" is the question the row exists to
//! answer.
//!
//! The row's identity across a refresh is the commit *and* the message
//! together: selectors renumber (`HEAD@{1}` becomes `HEAD@{2}`) every time
//! anything moves HEAD, so a refresh anchors the cursor on what the entry
//! *was* rather than where it sat. An armed recovery dies on every refresh
//! for the same reason every other arm does — a yes addressed to a
//! renumbered row is the accident the double press exists to prevent — and
//! the arm itself is keyed on the selector *and* the commit, so a row that
//! slid under the cursor cannot spend it.
//!
//! Selection is the cursor and deliberately nothing else: recovery acts on
//! one entry at a time, so a drag moves the cursor or the scrollbar but
//! never builds a range, and `copy.selection` falls back to the row the
//! keyboard is on — the same answers the tags pane gives.

use crate::screen::{Ink, Screen};
use crate::scrollbar::{self, Bar};
use gitten_core::host::Host;
use gitten_core::list::{self, Armed};
use gitten_core::refs::ReflogEntry;
use gitten_core::runs::Run;
use gitten_core::search::TextIndex;
use gitten_core::view::Viewport;

/// One flat row of the pane: one reflog entry, flattened once per refresh
/// and never per frame.
struct Row {
    /// The commit HEAD pointed at, abbreviated as git abbreviates it.
    commit: String,
    /// The address of this entry, e.g. `HEAD@{3}` — what recovery aims at.
    selector: String,
    /// What moved HEAD — `commit: …`, `checkout: …`, `rebase …`.
    message: String,
}

impl Row {
    /// The row as it draws, spelled once at flatten: the selector the
    /// verbs address, the commit it names, and the message saying what
    /// kind of move it was.
    fn text(&self) -> String {
        format!("{}  {}  {}", self.selector, self.commit, self.message)
    }
}

/// The reflog list.
///
/// Holds the flattened list, the viewport and the recovery arm; knows
/// nothing about keys. Every method is a command, exactly as in
/// [`crate::tags::Tags`] and for the same reason.
pub struct Reflog {
    rows: Vec<Row>,
    search: TextIndex,
    query: Option<String>,
    visible: Vec<usize>,
    /// Whether the reflog was read at all. `false` after a failed read:
    /// the pane opens anyway and draws as unavailable, and the next
    /// successful refresh replaces it outright. An unborn branch reads as
    /// *empty*, not failed — there is simply nowhere HEAD has been yet.
    available: bool,
    view: Viewport,
    cols: usize,
    bar: Bar,
    /// The recovery awaiting its second press: the selector and commit it
    /// was asked over. One slot — the shared [`Armed`] — arming a different
    /// row moves the question, never queues two. Killed by any cursor move,
    /// any moving scroll, any mouse row change and any refresh.
    armed: Armed<(String, String)>,
    dragging: bool,
}

impl Reflog {
    /// A successfully read list. An empty vector is one too — an unborn
    /// branch has no entries, which is a state, not a failure.
    pub fn new(entries: Vec<ReflogEntry>) -> Self {
        let mut view = Viewport::new();
        view.set_len(entries.len());
        let mut this = Self {
            rows: flatten(entries),
            search: TextIndex::new(Vec::<String>::new()),
            query: None,
            visible: Vec::new(),
            available: true,
            view,
            cols: 0,
            bar: Bar::default(),
            armed: Armed::new(),
            dragging: false,
        };
        this.reindex();
        this
    }

    /// The pane after a failed read: honest emptiness that is *not* the
    /// empty-reflog line, no row to act on, and recoverable — the next
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
            armed: Armed::new(),
            dragging: false,
        }
    }

    fn row_at(&self, visual: usize) -> Option<&Row> {
        list::shown(&self.rows, &self.visible, visual)
    }

    fn reindex(&mut self) {
        self.search = TextIndex::new(self.rows.iter().map(|r| r.text()));
        self.refilter();
    }

    fn refilter(&mut self) {
        self.visible = match &self.query {
            Some(q) => self.search.indices(q),
            None => list::identity(&self.rows),
        };
        self.view.set_len(self.visible.len());
    }

    // ----------------------------------------------------------------- search

    pub fn query(&self) -> Option<&str> {
        self.query.as_deref()
    }

    pub fn filter_note(&self) -> Option<String> {
        list::filter_note(self.query.as_deref(), self.visible.len(), self.rows.len())
    }

    /// Sets the filter — once per keystroke, never anywhere else. The
    /// keyboard stays on its entry: anchored by commit and message into
    /// the next result set wherever it survives, clamped when it does
    /// not. An armed recovery dies with a result set that changed, like
    /// any other refresh.
    pub fn apply_query(&mut self, query: &str) {
        let next = list::normalize_query(query);
        if self.query == next {
            return;
        }
        let anchored = self
            .row_at(self.view.cursor())
            .map(|r| (r.commit.clone(), r.message.clone()));
        self.query = next;
        self.refilter();
        self.armed.disarm();
        let cursor = anchored
            .and_then(|(commit, message)| {
                self.visible.iter().position(|&r| {
                    self.rows
                        .get(r)
                        .is_some_and(|row| row.commit == commit && row.message == message)
                })
            })
            .unwrap_or_else(|| self.view.cursor());
        self.view.go_to(cursor);
    }

    pub fn next_match(&mut self, by: isize) {
        if self.query.is_none() || self.visible.is_empty() {
            return;
        }
        let at = list::wrap_index(self.view.cursor(), by, self.visible.len());
        self.armed.disarm();
        self.view.go_to(at);
    }

    pub fn clear_search(&mut self) {
        self.apply_query("");
    }

    /// Swaps in a refreshed list, keeping the keyboard on its entry by
    /// commit and message — selectors renumber, so the cursor follows what
    /// the entry *was*. An armed recovery dies here first.
    pub fn replace(&mut self, entries: Vec<ReflogEntry>) {
        self.armed.disarm();
        self.dragging = false;
        self.available = true;
        let (cursor, top) = (self.view.cursor(), self.view.top());
        let anchored = self
            .rows
            .get(cursor)
            .map(|r| (r.commit.clone(), r.message.clone()));
        self.rows = flatten(entries);
        self.reindex();
        self.view.scroll_to(top);
        let at = anchored
            .and_then(|(commit, message)| {
                self.visible.iter().position(|&r| {
                    self.rows
                        .get(r)
                        .is_some_and(|row| row.commit == commit && row.message == message)
                })
            })
            .unwrap_or_else(|| cursor.min(self.visible.len().saturating_sub(1)));
        self.view.go_to(at);
    }

    // ------------------------------------------------------------- the viewport

    pub fn set_scrolloff(&mut self, rows: usize) {
        self.view.set_scrolloff(rows);
    }

    pub fn set_bar(&mut self, bar: Bar) {
        self.bar = bar;
    }

    pub fn resize(&mut self, cols: usize, height: usize) {
        self.cols = cols;
        self.view.set_height(height);
    }

    /// The entry the keyboard is on, whole — selector, commit and message,
    /// the three things recovery needs. `None` on an empty list — and on
    /// an unavailable one, which exposes no row to act on at all.
    pub fn current(&self) -> Option<ReflogEntry> {
        if !self.available {
            return None;
        }
        self.row_at(self.view.cursor()).map(|r| ReflogEntry {
            commit: r.commit.clone(),
            selector: r.selector.clone(),
            message: r.message.clone(),
        })
    }

    pub fn move_by(&mut self, by: isize) {
        self.armed.disarm();
        self.view.move_by(by);
    }

    pub fn down(&mut self) {
        self.move_by(1);
    }

    pub fn up(&mut self) {
        self.move_by(-1);
    }

    pub fn page(&mut self, pages: isize) {
        self.armed.disarm();
        self.view.page(pages);
    }

    pub fn scroll_y(&mut self, by: isize) {
        let before = self.view.top();
        self.view.pan_by(by);
        if self.view.top() != before {
            self.armed.disarm();
        }
    }

    pub fn to_top(&mut self) {
        self.armed.disarm();
        self.view.to_top();
    }

    pub fn to_bottom(&mut self) {
        self.armed.disarm();
        self.view.to_bottom();
    }

    // ---------------------------------------------------------------- the mouse

    pub fn press(&mut self, _col: usize, row: usize, _extend: bool, _host: &Host) {
        let Some(index) = self.view.row_at(row) else {
            return;
        };
        self.view.go_to(index);
        self.disarm_if_row_moved(index);
    }

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

    fn disarm_if_row_moved(&mut self, index: usize) {
        let at = self
            .row_at(index)
            .map(|r| (r.selector.clone(), r.commit.clone()));
        self.armed.disarm_unless(at.as_ref());
    }

    /// The question standing, if one is — what the tests read to prove an
    /// arm moved, died, or stayed exactly where it was asked.
    pub fn armed(&self) -> Option<(String, String)> {
        self.armed.get().cloned()
    }

    /// Arms — or confirms — a recovery of the entry under the keyboard.
    /// Both halves must match the standing arm: the selector that was
    /// asked over *and* the commit it named, so a row that slid under the
    /// cursor between the presses cannot spend it. First call stores and
    /// returns false; second call on the same pair clears and returns
    /// true. A refresh disarms unconditionally.
    pub fn confirm_or_arm_recover(&mut self, selector: &str, commit: &str) -> bool {
        self.armed
            .confirm_or_arm((selector.to_string(), commit.to_string()))
    }

    // ------------------------------------------------------- copy and selection

    pub fn copy_text(&self) -> String {
        self.row_at(self.view.cursor())
            .map(|r| r.text())
            .unwrap_or_default()
    }

    pub fn selection(&self) -> String {
        String::new()
    }

    pub fn select_all(&mut self) {}

    pub fn select_none(&mut self) -> bool {
        false
    }

    // ------------------------------------------------------------- the drawing

    pub fn paint(&self, screen: &mut Screen, x: usize, y: usize, focused: bool, host: &Host) {
        let theme = &host.theme;
        let blank = Ink::new(theme.chrome.dim, theme.chrome.bg);
        let quiet = if !self.available {
            Some((
                "reflog unavailable",
                Ink::new(theme.chrome.error, theme.chrome.bg),
            ))
        } else if self.rows.is_empty() {
            Some((
                "no reflog entries — nowhere HEAD has been yet",
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
                let selector = Ink::new(theme.chrome.fg, bg);
                let armed = self.armed.get().is_some_and(|(s, c)| {
                    s.as_str() == r.selector.as_str() && c.as_str() == r.commit.as_str()
                });
                let rest = Ink::new(
                    match armed {
                        true => theme.chrome.error,
                        false => theme.chrome.dim,
                    },
                    bg,
                );
                let text = r.text();
                let selector_len = r.selector.len();
                let (head, tail) = text.split_at(selector_len.min(text.len()));
                pen.put(head, selector);
                pen.put(tail, rest);
                pen.wash(rest);
            }
        }
    }

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

    pub fn status(&self) -> String {
        if !self.available {
            return "unavailable".into();
        }
        if self.rows.is_empty() {
            return "0 entries".into();
        }
        let shown = self.visible.len();
        let at = self
            .row_at(self.view.cursor())
            .map(|r| r.selector.clone())
            .unwrap_or_default();
        format!("{}/{shown} · {at}", (self.view.cursor() + 1).min(shown))
    }
}

/// The `view.*` vocabulary — the verbs themselves are the inherent methods
/// above; this impl is what [`run_view_commands`] routes them by name.
///
/// [`run_view_commands`]: gitten_core::view::run_view_commands
impl gitten_core::view::Scrollable for Reflog {
    fn down(&mut self) {
        Reflog::down(self);
    }
    fn up(&mut self) {
        Reflog::up(self);
    }
    fn page(&mut self, pages: isize) {
        Reflog::page(self, pages);
    }
    fn scroll_y(&mut self, rows: isize) {
        Reflog::scroll_y(self, rows);
    }
    fn to_top(&mut self) {
        Reflog::to_top(self);
    }
    fn to_bottom(&mut self) {
        Reflog::to_bottom(self);
    }
}

fn flatten(entries: Vec<ReflogEntry>) -> Vec<Row> {
    entries
        .into_iter()
        .map(
            |ReflogEntry {
                 commit,
                 selector,
                 message,
             }| Row {
                commit,
                selector,
                message,
            },
        )
        .collect()
}
/// The [`Pane`] half of the reflog pane — the tenant contract over the inherent
/// methods above. `view.*` and `search.*` come from the provided `run`;
/// this pane has no verbs of its own to add.
impl crate::pane::Pane for Reflog {
    fn scrollable(&mut self) -> &mut dyn gitten_core::view::Scrollable {
        self
    }

    fn mode(&self) -> &'static str {
        "reflog"
    }

    fn set_scrolloff(&mut self, rows: usize) {
        Reflog::set_scrolloff(self, rows);
    }

    fn resize(&mut self, cols: usize, height: usize, _host: &Host) {
        Reflog::resize(self, cols, height);
    }

    fn paint(
        &self,
        screen: &mut Screen,
        x: usize,
        y: usize,
        focused: bool,
        host: &Host,
        _out: &mut Vec<Run>,
    ) {
        Reflog::paint(self, screen, x, y, focused, host);
    }

    fn status(&self, _host: &Host) -> String {
        Reflog::status(self)
    }

    fn paint_bar(
        &self,
        screen: &mut Screen,
        x: usize,
        divider: Option<usize>,
        y: usize,
        host: &Host,
    ) {
        Reflog::paint_bar(self, screen, x, divider, y, host);
    }

    fn press(&mut self, col: usize, row: usize, _clicks: u8, extend: bool, host: &Host) {
        Reflog::press(self, col, row, extend, host);
    }

    fn drag(&mut self, _col: usize, row: isize, host: &Host) {
        Reflog::drag(self, row, host);
    }

    fn release(&mut self) {
        Reflog::release(self);
    }

    fn copy_text(&self) -> String {
        Reflog::copy_text(self)
    }

    fn selection(&self) -> String {
        Reflog::selection(self)
    }

    fn select_all(&mut self) {
        Reflog::select_all(self);
    }

    fn select_none(&mut self) -> bool {
        Reflog::select_none(self)
    }

    fn search_query(&self) -> Option<&str> {
        Reflog::query(self)
    }

    fn search_note(&self) -> Option<String> {
        Reflog::filter_note(self)
    }

    fn search_edit(&mut self, query: &str) {
        Reflog::apply_query(self, query);
    }

    fn search_clear(&mut self) {
        Reflog::clear_search(self);
    }

    fn search_next(&mut self, by: isize) {
        Reflog::next_match(self, by);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(commit: &str, selector: &str, message: &str) -> ReflogEntry {
        ReflogEntry {
            commit: commit.into(),
            selector: selector.into(),
            message: message.into(),
        }
    }

    #[test]
    fn rows_are_selector_then_commit_then_message_and_empty_is_quiet() {
        let host = Host::new();
        let c = &host.theme.chrome;
        let (x, cols) = (7, 60);
        let mut screen = Screen::new(75, 6);
        let sentinel = Ink::new(0x112233, 0x445566);
        screen.clear(sentinel);

        let mut v = Reflog::new(vec![
            entry("bbb222", "HEAD@{0}", "commit: third"),
            entry(
                "aaa111",
                "HEAD@{1}",
                "checkout: moving from main to feature",
            ),
        ]);
        v.resize(cols, 6);
        v.paint(&mut screen, x, 0, true, &host);

        let row = screen.row_text(0);
        assert!(row.contains("HEAD@{0}"), "{row:?}");
        assert!(row.contains("bbb222"), "{row:?}");
        assert!(row.contains("commit: third"), "{row:?}");
        assert_eq!(screen.ink(x, 0).unwrap().fg, c.fg);
        let checkout = screen.row_text(1);
        assert!(checkout.contains("moving from main"), "{checkout:?}");
        assert_eq!(screen.ink(x - 1, 0), Some(sentinel));
        assert_eq!(screen.ink(x + cols, 0), Some(sentinel));
        assert_eq!(
            v.current().as_ref().map(|e| e.selector.clone()),
            Some("HEAD@{0}".to_string())
        );

        let mut empty = Reflog::new(Vec::new());
        empty.resize(cols, 6);
        let mut screen = Screen::new(70, 6);
        empty.paint(&mut screen, x, 0, true, &host);
        assert!(
            screen.row_text(0).contains("nowhere HEAD has been"),
            "{:?}",
            screen.row_text(0)
        );
        assert_eq!(empty.status(), "0 entries");
        assert_eq!(empty.current(), None);

        let mut failed = Reflog::unavailable();
        failed.resize(cols, 6);
        let mut screen = Screen::new(70, 6);
        failed.paint(&mut screen, x, 0, true, &host);
        assert!(
            screen.row_text(0).contains("reflog unavailable"),
            "{:?}",
            screen.row_text(0)
        );
        assert_eq!(failed.current(), None);
    }

    #[test]
    fn refresh_anchors_by_commit_and_message_and_the_arm_needs_both() {
        let mut v = Reflog::new(vec![
            entry("bbb222", "HEAD@{0}", "commit: third"),
            entry("aaa111", "HEAD@{1}", "commit: second"),
        ]);
        v.resize(30, 6);
        v.view.go_to(1);
        assert_eq!(
            v.current().as_ref().map(|e| e.commit.clone()),
            Some("aaa111".to_string())
        );

        // A new move renumbers every selector; the keyboard follows what
        // the entry was, not where it sat.
        v.replace(vec![
            entry("ccc333", "HEAD@{0}", "commit: fourth"),
            entry("bbb222", "HEAD@{1}", "commit: third"),
            entry("aaa111", "HEAD@{2}", "commit: second"),
        ]);
        assert_eq!(
            v.current().as_ref().map(|e| e.selector.clone()),
            Some("HEAD@{2}".to_string())
        );

        // The arm needs both halves: the selector alone is not enough once
        // rows slide.
        assert!(!v.confirm_or_arm_recover("HEAD@{2}", "aaa111"));
        assert!(v.confirm_or_arm_recover("HEAD@{2}", "aaa111"));
        assert_eq!(v.armed.get(), None);
        // A pair that matches nothing standing re-arms and asks again —
        // it can never spend: only the exact standing pair confirms.
        assert!(!v.confirm_or_arm_recover("HEAD@{2}", "bbb222"));
        assert_eq!(
            v.armed.get().cloned(),
            Some(("HEAD@{2}".to_string(), "bbb222".to_string())),
            "a moved row re-arms, never spends"
        );
        assert!(v.confirm_or_arm_recover("HEAD@{2}", "bbb222"));
        v.replace(vec![entry("aaa111", "HEAD@{0}", "commit: second")]);
        assert_eq!(v.armed.get(), None, "a refresh disarms");
        v.confirm_or_arm_recover("HEAD@{0}", "aaa111");
        v.down();
        assert_eq!(v.armed.get(), None, "a keyboard move disarms");

        assert_eq!(v.selection(), "");
        assert!(!v.select_none());
    }
}
