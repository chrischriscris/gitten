//! The repository's worktrees, as one flat list — the terminal's share of
//! the worktree-management pane.
//!
//! One row per checkout: the path verbs address it with, what it holds,
//! and the states git would refuse to touch — locked, prunable, or the
//! checkout this client is itself standing in. A path is bytes end to end:
//! verbs aim at the row's bytes exactly as the listing spelled them, and
//! only the drawing decodes lossily.
//!
//! The row's identity across a refresh is the *path*: names are what every
//! verb aims at, so a refresh anchors the cursor by path and an armed
//! removal dies on every refresh — a yes addressed to yesterday's checkout
//! is the accident the double press exists to prevent.
//!
//! Selection is the cursor and deliberately nothing else, exactly as in
//! [`crate::tags::Tags`] and for the same reason: the verbs act on one
//! checkout at a time.

use crate::screen::{Ink, Screen};
use crate::scrollbar::{self, Bar};
use gitten_core::host::Host;
use gitten_core::search::TextIndex;
use gitten_core::view::Viewport;
use gitten_core::worktrees::Worktree;

/// One flat row of the pane: one checkout, flattened once per refresh and
/// never per frame.
struct Row {
    /// Where the checkout lives — the row's identity, and what every verb
    /// addresses it with.
    path: Vec<u8>,
    /// The commit its HEAD names, full object id.
    head: String,
    /// The branch it holds, without `refs/heads/` — `None` for a detached
    /// checkout and for a bare repository.
    branch: Option<Vec<u8>>,
    /// Whether this is the bare repository rather than a checkout.
    bare: bool,
    /// The lock reason, when the entry is locked.
    lock: Option<String>,
    /// The prune reason, when git considers the entry gone.
    prunable: Option<String>,
    /// Whether this is the checkout the client itself stands in — the one
    /// row a removal must never aim at, because git refuses it and the
    /// pane should say so first.
    here: bool,
}

impl Row {
    /// The row as it draws, spelled once at flatten: the path, what it
    /// holds, and the states — locked, prunable, this checkout — that
    /// change what the verbs may do.
    fn text(&self) -> String {
        let mut text = String::from_utf8_lossy(&self.path).into_owned();
        text.push_str("  ");
        match (self.bare, self.branch.as_deref()) {
            (true, _) => text.push_str("(bare)"),
            (false, Some(branch)) => text.push_str(&String::from_utf8_lossy(branch)),
            (false, None) => {
                text.push_str("(detached ");
                text.push_str(&self.head.chars().take(7).collect::<String>());
                text.push(')');
            }
        }
        if let Some(reason) = self.lock.as_deref() {
            text.push_str("  (locked");
            if !reason.is_empty() {
                text.push_str(": ");
                text.push_str(reason);
            }
            text.push(')');
        }
        if let Some(reason) = self.prunable.as_deref() {
            text.push_str("  (prunable");
            if !reason.is_empty() {
                text.push_str(": ");
                text.push_str(reason);
            }
            text.push(')');
        }
        if self.here {
            text.push_str("  (this checkout)");
        }
        text
    }
}

/// The worktree list.
///
/// Holds the flattened list, the viewport and the removal arm; knows
/// nothing about keys. Every method is a command, exactly as in
/// [`crate::tags::Tags`] and for the same reason.
pub struct Worktrees {
    rows: Vec<Row>,
    search: TextIndex,
    query: Option<String>,
    visible: Vec<usize>,
    /// Whether the list was read at all. `false` after a failed read:
    /// the pane opens anyway and draws as unavailable, and the next
    /// successful refresh replaces it outright.
    available: bool,
    view: Viewport,
    cols: usize,
    bar: Bar,
    /// The removal awaiting its second press: the path that asked, and
    /// whether the force spelling is what the next press runs. One slot —
    /// arming a different row moves the question, never queues two. Killed
    /// by any cursor move, any moving scroll, any mouse row change and any
    /// refresh.
    armed: Option<(Vec<u8>, bool)>,
    dragging: bool,
}

impl Worktrees {
    /// A successfully read list. An empty vector is one too — but a
    /// repository always lists at least itself, so emptiness here is the
    /// shape a backend that cannot answer takes, and it draws as such.
    pub fn new(worktrees: Vec<Worktree>, here: &[u8]) -> Self {
        let mut view = Viewport::new();
        view.set_len(worktrees.len());
        let mut this = Self {
            rows: flatten(worktrees, here),
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
    /// empty line, no row to act on, and recoverable — the next successful
    /// refresh replaces it outright.
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

    fn row_at(&self, visual: usize) -> Option<&Row> {
        self.rows.get(*self.visible.get(visual)?)
    }

    fn reindex(&mut self) {
        self.search = TextIndex::new(self.rows.iter().map(|r| r.text()));
        self.refilter();
    }

    fn refilter(&mut self) {
        self.visible = match &self.query {
            Some(q) => self.search.indices(q),
            None => Vec::from_iter(0..self.rows.len()),
        };
        self.view.set_len(self.visible.len());
    }

    // ----------------------------------------------------------------- search

    pub fn query(&self) -> Option<&str> {
        self.query.as_deref()
    }

    pub fn filter_note(&self) -> Option<String> {
        self.query
            .is_some()
            .then(|| format!("{}/{}", self.visible.len(), self.rows.len()))
    }

    /// Sets the filter — once per keystroke, never anywhere else. The
    /// keyboard stays on its checkout: anchored by path into the next
    /// result set wherever it survives, clamped when it does not. An armed
    /// removal dies with a result set that changed, like any other refresh.
    pub fn apply_query(&mut self, query: &str) {
        let next = Some(query.trim()).filter(|q| !q.is_empty());
        if self.query.as_deref() == next {
            return;
        }
        let anchored = self.row_at(self.view.cursor()).map(|r| r.path.clone());
        self.query = next.map(str::to_string);
        self.refilter();
        self.armed = None;
        let cursor = anchored
            .and_then(|p| {
                self.visible
                    .iter()
                    .position(|&r| self.rows.get(r).is_some_and(|row| row.path == p))
            })
            .unwrap_or_else(|| self.view.cursor());
        self.view.go_to(cursor);
    }

    pub fn next_match(&mut self, by: isize) {
        if self.query.is_none() || self.visible.is_empty() {
            return;
        }
        let len = self.visible.len() as isize;
        let at = (self.view.cursor() as isize + by).rem_euclid(len) as usize;
        self.armed = None;
        self.view.go_to(at);
    }

    pub fn clear_search(&mut self) {
        self.apply_query("");
    }

    /// Swaps in a refreshed list, keeping the keyboard on its checkout by
    /// path. A refresh is also the worktree namespace saying things moved:
    /// an armed removal dies here first.
    pub fn replace(&mut self, worktrees: Vec<Worktree>, here: &[u8]) {
        self.armed = None;
        self.dragging = false;
        self.available = true;
        let (cursor, top) = (self.view.cursor(), self.view.top());
        let anchored = self.rows.get(cursor).map(|r| r.path.clone());
        self.rows = flatten(worktrees, here);
        self.reindex();
        self.view.scroll_to(top);
        let at = anchored
            .and_then(|p| {
                self.visible
                    .iter()
                    .position(|&r| self.rows.get(r).is_some_and(|row| row.path == p))
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

    /// The checkout the keyboard is on, as the verbs address it: the path.
    /// `None` on an empty list — and on an unavailable one, which exposes
    /// no row to act on at all.
    pub fn current(&self) -> Option<Vec<u8>> {
        if !self.available {
            return None;
        }
        self.row_at(self.view.cursor()).map(|r| r.path.clone())
    }

    /// Whether the keyboard sits on the checkout this client stands in —
    /// the row a removal must refuse before git does.
    pub fn current_is_here(&self) -> bool {
        self.row_at(self.view.cursor()).is_some_and(|r| r.here)
    }

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
            .visible
            .get(index)
            .and_then(|&r| self.rows.get(r))
            .map(|r| (r.path.clone(), self.armed.as_ref().is_some_and(|(_, f)| *f)));
        if self.armed.is_some() && self.armed != at {
            self.armed = None;
        }
    }

    /// The removal standing, if one is — what the tests read to prove an
    /// arm moved, died, upgraded to force, or stayed exactly where asked.
    pub fn armed(&self) -> Option<(Vec<u8>, bool)> {
        self.armed.clone()
    }

    /// Arms — or spends — a removal of this exact path. First call stores
    /// `(path, force)` and returns false: ask, don't act. A second call on
    /// the same pair clears the arm and returns true: act. A different
    /// pair re-arms — the force upgrade a dirty refusal offers arrives as
    /// a new question, never as a spent old one — and a refresh disarms
    /// unconditionally.
    pub fn confirm_or_arm_remove(&mut self, path: &[u8], force: bool) -> bool {
        let already = self
            .armed
            .as_ref()
            .is_some_and(|(p, f)| p == path && *f == force);
        self.armed = match already {
            true => None,
            false => Some((path.to_vec(), force)),
        };
        already
    }

    /// Re-arms the standing question with the force spelling after a plain
    /// removal came back refused — the App's half of the dirty upgrade.
    /// A no-op unless the arm stands on this exact path without force.
    pub fn upgrade_to_force(&mut self, path: &[u8]) {
        if self.armed.as_ref().is_some_and(|(p, f)| p == path && !f) {
            self.armed = Some((path.to_vec(), true));
        }
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
                "worktrees unavailable",
                Ink::new(theme.chrome.error, theme.chrome.bg),
            ))
        } else if self.rows.is_empty() {
            Some((
                "no worktrees",
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
                let armed = self.armed.as_ref().is_some_and(|(p, _)| p == &r.path);
                let rest = Ink::new(
                    match armed || r.lock.is_some() || r.prunable.is_some() {
                        true => theme.chrome.error,
                        false => theme.chrome.dim,
                    },
                    bg,
                );
                let text = r.text();
                let name_len = String::from_utf8_lossy(&r.path).len();
                let (head, tail) = text.split_at(name_len.min(text.len()));
                pen.put(head, name);
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
            return "0 worktrees".into();
        }
        let shown = self.visible.len();
        let at = self
            .row_at(self.view.cursor())
            .map(|r| String::from_utf8_lossy(&r.path).into_owned())
            .unwrap_or_default();
        format!("{}/{shown} · {at}", (self.view.cursor() + 1).min(shown))
    }
}

fn flatten(worktrees: Vec<Worktree>, here: &[u8]) -> Vec<Row> {
    // Exact bytes: the listing spells paths symlink-resolved, so the
    // caller canonicalizes its own root first. Exactness here is what
    // keeps a removal from ever aiming at the wrong checkout.
    worktrees
        .into_iter()
        .map(|w| {
            let here = w.path == here;
            Row {
                path: w.path,
                head: w.head,
                branch: w.branch,
                bare: w.bare,
                lock: w.lock,
                prunable: w.prunable,
                here,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wt(path: &str, branch: Option<&str>, lock: bool) -> Worktree {
        Worktree {
            path: path.as_bytes().to_vec(),
            head: "abcdef0123456789".into(),
            branch: branch.map(|b| b.as_bytes().to_vec()),
            bare: false,
            lock: lock.then(|| "held".into()),
            prunable: None,
        }
    }

    #[test]
    fn rows_name_path_state_and_this_checkout_and_empty_is_quiet() {
        let host = Host::new();
        let (x, cols) = (7, 60);
        let mut screen = Screen::new(75, 6);

        let mut v = Worktrees::new(
            vec![
                wt("/repo", Some("main"), false),
                wt("/repo/feature", Some("feature"), false),
                wt("/repo/held", None, true),
            ],
            b"/repo",
        );
        v.resize(cols, 6);
        v.paint(&mut screen, x, 0, true, &host);

        let row = screen.row_text(0);
        assert!(row.contains("/repo"), "{row:?}");
        assert!(row.contains("main"), "{row:?}");
        assert!(row.contains("(this checkout)"), "{row:?}");
        let bare = screen.row_text(1);
        assert!(bare.contains("/repo/feature"), "{bare:?}");
        let held = screen.row_text(2);
        assert!(held.contains("(locked: held)"), "{held:?}");
        assert!(v.current_is_here());
        v.down();
        assert!(!v.current_is_here());
        assert_eq!(v.current(), Some(b"/repo/feature".to_vec()));

        assert!(!v.confirm_or_arm_remove(b"/repo/feature", false));
        assert!(v.confirm_or_arm_remove(b"/repo/feature", false));
        assert_eq!(v.armed(), None, "spending clears");
        assert!(!v.confirm_or_arm_remove(b"/repo/feature", false));
        v.upgrade_to_force(b"/repo/feature");
        assert_eq!(
            v.armed(),
            Some((b"/repo/feature".to_vec(), true)),
            "the dirty upgrade"
        );
        assert!(!v.confirm_or_arm_remove(b"/repo/feature", true));
        assert!(v.confirm_or_arm_remove(b"/repo/feature", true));
        v.replace(vec![wt("/repo", Some("main"), false)], b"/repo");
        assert_eq!(v.armed(), None, "a refresh disarms");
        assert_eq!(v.status(), "1/1 · /repo");

        let mut empty = Worktrees::new(Vec::new(), b"/repo");
        empty.resize(cols, 6);
        let mut screen = Screen::new(75, 6);
        empty.paint(&mut screen, x, 0, true, &host);
        assert!(
            screen.row_text(0).contains("no worktrees"),
            "{:?}",
            screen.row_text(0)
        );
        assert_eq!(empty.current(), None);
    }
}
