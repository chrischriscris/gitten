//! The repository's remotes, as a flat list — the terminal's share of the
//! remote-management pane.
//!
//! One row per remote: the name verbs address it with, then the URL(s) the
//! config holds. A remote may serve several URLs — an explicit push URL is
//! a real configuration — and every one of them is shown, because "what does
//! this remote point at" is the question the row exists to answer; the
//! verbs aim at the [`name`](gitten_core::refs::Remote::name) and let git
//! resolve whatever the config says by then.
//!
//! The row's identity across a refresh is the remote *name*: URLs are
//! editable config, names are what every verb aims at, so a refresh anchors
//! the cursor by name and an armed removal dies on every refresh — a yes
//! addressed to yesterday's configuration is the accident the double press
//! exists to prevent, and a URL edit that moved the row is not one of them.
//!
//! Selection is the cursor and deliberately nothing else: the verbs act on
//! one remote at a time, so a drag moves the cursor or the scrollbar but
//! never builds a range, and `copy.selection` falls back to the row the
//! keyboard is on — the same answers the stash pane gives.

use crate::screen::{Ink, Screen};
use crate::scrollbar::{self, Bar};
use gitten_core::host::Host;
use gitten_core::list::{self, Armed};
use gitten_core::refs::{RefName, Remote};
use gitten_core::runs::Run;
use gitten_core::search::TextIndex;
use gitten_core::view::Viewport;

/// One flat row of the pane: one remote, flattened once per refresh and
/// never per frame.
struct Row {
    /// The short name every verb addresses the remote with — the row's
    /// identity across a refresh.
    name: RefName,
    /// The URLs the config holds, fetch first, in config order. Display
    /// text only: nothing here is ever aimed at anything.
    urls: Vec<String>,
}

impl Row {
    /// The row as it draws, spelled once at flatten: the name, then every
    /// URL. A remote with no URL says so — a real configuration, and the
    /// honest answer to it.
    fn text(&self) -> String {
        let mut text = self.name.to_string_lossy().into_owned();
        match self.urls.is_empty() {
            true => text.push_str("  (no URL)"),
            false => {
                for url in &self.urls {
                    text.push_str("  ");
                    text.push_str(url);
                }
            }
        }
        text
    }
}

/// The remotes list.
///
/// Holds the flattened list, the viewport and the removal arm; knows nothing
/// about keys. Every method is a command, exactly as in
/// [`crate::stashes::Stashes`] and for the same reason.
pub struct Remotes {
    rows: Vec<Row>,
    search: TextIndex,
    query: Option<String>,
    visible: Vec<usize>,
    /// Whether the remotes were read at all. `false` after a failed read:
    /// the pane opens anyway and draws as unavailable, and the next
    /// successful refresh replaces it outright.
    available: bool,
    view: Viewport,
    cols: usize,
    bar: Bar,
    /// The removal awaiting its second press: the name of the remote that
    /// asked. One slot — the shared [`Armed`] — arming a different row moves
    /// the question, never queues two. Killed by any cursor move, any moving
    /// scroll, any mouse row change and any refresh.
    armed: Armed<RefName>,
    dragging: bool,
}

impl Remotes {
    /// A successfully read list. An empty vector is one too — no remotes is
    /// a state, not a failure.
    pub fn new(remotes: Vec<Remote>) -> Self {
        let mut view = Viewport::new();
        view.set_len(remotes.len());
        let mut this = Self {
            rows: flatten(remotes),
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
    /// no-remotes line, no row to act on, and recoverable — the next
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
    /// keyboard stays on its remote: anchored by name — the identity that
    /// survives a URL edit — into the next result set wherever it survives,
    /// clamped when it does not. An armed removal dies with a result set
    /// that changed, like any other refresh.
    pub fn apply_query(&mut self, query: &str) {
        let next = list::normalize_query(query);
        if self.query == next {
            return;
        }
        let anchored = self.row_at(self.view.cursor()).map(|r| r.name.clone());
        self.query = next;
        self.refilter();
        self.armed.disarm();
        let cursor = anchored
            .and_then(|n| {
                self.visible
                    .iter()
                    .position(|&r| self.rows.get(r).is_some_and(|row| row.name == n))
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

    /// Swaps in a refreshed list, keeping the keyboard on its remote by
    /// name. A refresh is also the configuration saying things moved: an
    /// armed removal dies here first.
    pub fn replace(&mut self, remotes: Vec<Remote>) {
        self.armed.disarm();
        self.dragging = false;
        self.available = true;
        let (cursor, top) = (self.view.cursor(), self.view.top());
        let anchored = self.rows.get(cursor).map(|r| r.name.clone());
        self.rows = flatten(remotes);
        self.reindex();
        self.view.scroll_to(top);
        let at = anchored
            .and_then(|n| {
                self.visible
                    .iter()
                    .position(|&r| self.rows.get(r).is_some_and(|row| row.name == n))
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

    /// The remote the keyboard is on, as the verbs address it: the name.
    /// `None` on an empty list — and on an unavailable one, which exposes no
    /// row to act on at all.
    pub fn current(&self) -> Option<RefName> {
        if !self.available {
            return None;
        }
        self.row_at(self.view.cursor()).map(|r| r.name.clone())
    }

    /// The URLs the selected remote holds, for the edit field's prefill —
    /// the fetch URL first, because that is the address a one-URL remote
    /// means.
    pub fn current_urls(&self) -> Vec<String> {
        self.row_at(self.view.cursor())
            .map(|r| r.urls.clone())
            .unwrap_or_default()
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
        let at = self.row_at(index).map(|r| r.name.clone());
        self.armed.disarm_unless(at.as_ref());
    }

    /// The question standing, if one is — what the tests read to prove an
    /// arm moved, died, or stayed exactly where it was asked.
    pub fn armed(&self) -> Option<RefName> {
        self.armed.get().cloned()
    }

    /// Arms — or confirms — a removal of this exact remote name. First call
    /// on a target stores it and returns false: ask, don't act. Second call
    /// on the same target clears the arm and returns true: act. A refresh
    /// disarms unconditionally — the question was about a configuration
    /// that has since moved.
    pub fn confirm_or_arm_remove(&mut self, name: &RefName) -> bool {
        self.armed.confirm_or_arm(name.clone())
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
                "remotes unavailable",
                Ink::new(theme.chrome.error, theme.chrome.bg),
            ))
        } else if self.rows.is_empty() {
            Some((
                "no remotes — n to add one",
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
                let name = Ink::new(theme.chrome.fg, bg);
                let armed = self.armed.is(&r.name);
                let urls = Ink::new(
                    match armed {
                        true => theme.chrome.error,
                        false => theme.chrome.dim,
                    },
                    bg,
                );
                let text = r.text();
                let name_len = r.name.to_string_lossy().len();
                let (head, rest) = text.split_at(name_len.min(text.len()));
                pen.put(head, name);
                pen.put(rest, urls);
                pen.wash(urls);
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
            return "0 remotes".into();
        }
        let shown = self.visible.len();
        let at = self
            .row_at(self.view.cursor())
            .map(|r| r.name.to_string_lossy().into_owned())
            .unwrap_or_default();
        format!("{}/{shown} · {at}", (self.view.cursor() + 1).min(shown))
    }
}

/// The `view.*` vocabulary — the verbs themselves are the inherent methods
/// above; this impl is what [`run_view_commands`] routes them by name.
///
/// [`run_view_commands`]: gitten_core::view::run_view_commands
impl gitten_core::view::Scrollable for Remotes {
    fn down(&mut self) {
        Remotes::down(self);
    }
    fn up(&mut self) {
        Remotes::up(self);
    }
    fn page(&mut self, pages: isize) {
        Remotes::page(self, pages);
    }
    fn scroll_y(&mut self, rows: isize) {
        Remotes::scroll_y(self, rows);
    }
    fn to_top(&mut self) {
        Remotes::to_top(self);
    }
    fn to_bottom(&mut self) {
        Remotes::to_bottom(self);
    }
}

fn flatten(remotes: Vec<Remote>) -> Vec<Row> {
    remotes
        .into_iter()
        .map(|r| Row {
            name: r.name,
            urls: r.urls,
        })
        .collect()
}
/// The [`Pane`] half of the remotes pane — the tenant contract over the inherent
/// methods above. `view.*` and `search.*` come from the provided `run`;
/// this pane has no verbs of its own to add.
impl crate::pane::Pane for Remotes {
    fn scrollable(&mut self) -> &mut dyn gitten_core::view::Scrollable {
        self
    }

    fn mode(&self) -> &'static str {
        "remotes"
    }

    fn set_scrolloff(&mut self, rows: usize) {
        Remotes::set_scrolloff(self, rows);
    }

    fn resize(&mut self, cols: usize, height: usize, _host: &Host) {
        Remotes::resize(self, cols, height);
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
        Remotes::paint(self, screen, x, y, focused, host);
    }

    fn status(&self, _host: &Host) -> String {
        Remotes::status(self)
    }

    fn paint_bar(
        &self,
        screen: &mut Screen,
        x: usize,
        divider: Option<usize>,
        y: usize,
        host: &Host,
    ) {
        Remotes::paint_bar(self, screen, x, divider, y, host);
    }

    fn press(&mut self, col: usize, row: usize, _clicks: u8, extend: bool, host: &Host) {
        Remotes::press(self, col, row, extend, host);
    }

    fn drag(&mut self, _col: usize, row: isize, host: &Host) {
        Remotes::drag(self, row, host);
    }

    fn release(&mut self) {
        Remotes::release(self);
    }

    fn copy_text(&self) -> String {
        Remotes::copy_text(self)
    }

    fn selection(&self) -> String {
        Remotes::selection(self)
    }

    fn select_all(&mut self) {
        Remotes::select_all(self);
    }

    fn select_none(&mut self) -> bool {
        Remotes::select_none(self)
    }

    fn search_query(&self) -> Option<&str> {
        Remotes::query(self)
    }

    fn search_note(&self) -> Option<String> {
        Remotes::filter_note(self)
    }

    fn search_edit(&mut self, query: &str) {
        Remotes::apply_query(self, query);
    }

    fn search_clear(&mut self) {
        Remotes::clear_search(self);
    }

    fn search_next(&mut self, by: isize) {
        Remotes::next_match(self, by);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remote(name: &str, urls: &[&str]) -> Remote {
        Remote {
            name: RefName::from(name),
            urls: urls.iter().map(|u| u.to_string()).collect(),
        }
    }

    #[test]
    fn rows_are_name_then_urls_and_empty_is_quiet() {
        let host = Host::new();
        let c = &host.theme.chrome;
        let (x, cols) = (7, 55);
        let mut screen = Screen::new(70, 6);
        let sentinel = Ink::new(0x112233, 0x445566);
        screen.clear(sentinel);

        let mut v = Remotes::new(vec![
            remote("origin", &["git@example.com:x.git", "push-only.example"]),
            remote("upstream", &[]),
        ]);
        v.resize(cols, 6);
        v.paint(&mut screen, x, 0, true, &host);

        let row = screen.row_text(0);
        assert!(row.contains("origin"), "{row:?}");
        assert!(row.contains("git@example.com:x.git"), "{row:?}");
        assert!(row.contains("push-only.example"), "{row:?}");
        // The name is the text ink, the URLs the furniture — one row, the
        // address ahead of where it points.
        assert_eq!(screen.ink(x, 0).unwrap().fg, c.fg);
        assert_eq!(screen.ink(x + "origin".len() + 2, 0).unwrap().fg, c.dim);
        // A remote with no URL says so rather than drawing a bare name.
        assert!(
            screen.row_text(1).contains("(no URL)"),
            "{:?}",
            screen.row_text(1)
        );
        // Nothing outside the pane's span changed.
        assert_eq!(screen.ink(x - 1, 0), Some(sentinel));
        assert_eq!(screen.ink(x + cols, 0), Some(sentinel));
        assert_eq!(screen.ink(x, 0).unwrap().bg, c.selection_bg);
        assert_eq!(
            v.current().as_ref().map(|n| n.as_bytes()),
            Some(b"origin".as_slice())
        );

        let mut empty = Remotes::new(Vec::new());
        empty.resize(cols, 6);
        let mut screen = Screen::new(60, 6);
        empty.paint(&mut screen, x, 0, true, &host);
        assert!(
            screen.row_text(0).contains("no remotes"),
            "{:?}",
            screen.row_text(0)
        );
        assert_eq!(empty.status(), "0 remotes");
        assert_eq!(empty.current(), None);

        let mut failed = Remotes::unavailable();
        failed.resize(cols, 6);
        let mut screen = Screen::new(60, 6);
        failed.paint(&mut screen, x, 0, true, &host);
        assert!(
            screen.row_text(0).contains("remotes unavailable"),
            "{:?}",
            screen.row_text(0)
        );
        assert_eq!(failed.current(), None);
    }

    #[test]
    fn refresh_anchors_by_name_and_the_arm_dies_with_it() {
        let host = Host::new();
        let mut v = Remotes::new(vec![
            remote("origin", &["old.example"]),
            remote("fork", &["fork.example"]),
        ]);
        v.resize(30, 6);
        v.view.go_to(1);
        assert_eq!(
            v.current().as_ref().map(|n| n.as_bytes()),
            Some(b"fork".as_slice())
        );

        // The selected remote survives the refresh — possibly re-pointed —
        // and the keyboard follows its name, not its row.
        v.replace(vec![
            remote("fork", &["moved.example"]),
            remote("origin", &["old.example"]),
        ]);
        assert_eq!(
            v.current().as_ref().map(|n| n.as_bytes()),
            Some(b"fork".as_slice())
        );
        assert_eq!(v.current_urls(), vec!["moved.example".to_string()]);

        // Gone wholesale: clamp onto what survives.
        v.replace(vec![remote("origin", &["old.example"])]);
        assert_eq!(
            v.current().as_ref().map(|n| n.as_bytes()),
            Some(b"origin".as_slice())
        );

        // The arm: first press asks, second press on the same row acts, a
        // refresh unconditionally disarms, and a keyboard move disarms.
        assert!(!v.confirm_or_arm_remove(&RefName::from("origin")));
        assert_eq!(
            v.armed.get().map(|n| n.as_bytes()),
            Some(b"origin".as_slice())
        );
        assert!(v.confirm_or_arm_remove(&RefName::from("origin")));
        assert_eq!(v.armed.get(), None);
        assert!(!v.confirm_or_arm_remove(&RefName::from("origin")));
        v.replace(vec![remote("origin", &["old.example"])]);
        assert_eq!(v.armed.get(), None, "a refresh disarms");
        v.confirm_or_arm_remove(&RefName::from("origin"));
        v.down();
        assert_eq!(v.armed.get(), None, "a keyboard move disarms");

        // A URL edit is not a refresh-shaped move of identity: the name
        // survives, and so does the cursor.
        let mut v = Remotes::new(vec![remote("origin", &["old.example"])]);
        v.resize(30, 6);
        v.replace(vec![remote("origin", &["new.example"])]);
        assert_eq!(v.view.cursor(), 0);
        v.press(3, 0, false, &host);
        assert_eq!(v.view.cursor(), 0, "the press landed on the same row");

        // And the empty answer a selection is: no range here, ever.
        assert_eq!(v.selection(), "");
        assert!(!v.select_none());
    }
}
