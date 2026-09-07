//! The repository's tags, as a flat list — the terminal's share of the
//! tag-management pane.
//!
//! One row per tag: the name verbs address it with, the commit it names,
//! and the annotated message's subject when it carries one. A lightweight
//! tag says so — a bare ref is a real configuration, and the honest answer
//! to it — because "annotated or not" is the question the row exists to
//! answer beside the name; the verbs aim at the
//! [`name`](gitten_core::refs::Tag::name) either way and let git resolve
//! whatever the ref says by then.
//!
//! The row's identity across a refresh is the tag *name*: names are what
//! every verb aims at, so a refresh anchors the cursor by name and an armed
//! deletion dies on every refresh — a yes addressed to yesterday's ref is
//! the accident the double press exists to prevent.
//!
//! Selection is the cursor and deliberately nothing else: the verbs act on
//! one tag at a time, so a drag moves the cursor or the scrollbar but never
//! builds a range, and `copy.selection` falls back to the row the keyboard
//! is on — the same answers the remotes pane gives.

use crate::screen::{Ink, Screen};
use crate::scrollbar::{self, Bar};
use gitten_core::host::Host;
use gitten_core::refs::{RefName, Tag};
use gitten_core::search::TextIndex;
use gitten_core::view::Viewport;

/// One flat row of the pane: one tag, flattened once per refresh and never
/// per frame.
struct Row {
    /// The short name every verb addresses the tag with — the row's
    /// identity across a refresh.
    name: RefName,
    /// The commit the tag ultimately names, full object id. Verbs aim at
    /// the *name*; this rides along so the row can say where it points.
    commit: String,
    /// The commit abbreviated for the row's text. Seven hex digits, the
    /// way git abbreviates when it has room — display text only, never
    /// aimed at anything (verbs take the name).
    short: String,
    /// Whether git stored a tag object (`-a`), rather than a bare ref.
    annotated: bool,
    /// The tag message's subject line, when it carries one.
    subject: Option<String>,
}

impl Row {
    /// The row as it draws, spelled once at flatten: the name, the commit
    /// it names, and the subject — or the lightweight word, which is a
    /// state and not a failure.
    fn text(&self) -> String {
        let mut text = self.name.to_string_lossy().into_owned();
        text.push_str("  ");
        text.push_str(&self.short);
        text.push_str("  ");
        // An annotated tag with an empty message is still annotated — only
        // a bare ref is lightweight, and the row must not confuse the two.
        match (self.annotated, self.subject.as_deref()) {
            (_, Some(subject)) => text.push_str(subject),
            (true, None) => text.push_str("(annotated, no message)"),
            (false, None) => text.push_str("(lightweight)"),
        }
        text
    }
}

/// The tags list.
///
/// Holds the flattened list, the viewport and the deletion arm; knows
/// nothing about keys. Every method is a command, exactly as in
/// [`crate::remotes::Remotes`] and for the same reason.
pub struct Tags {
    rows: Vec<Row>,
    search: TextIndex,
    query: Option<String>,
    visible: Vec<usize>,
    /// Whether the tags were read at all. `false` after a failed read:
    /// the pane opens anyway and draws as unavailable, and the next
    /// successful refresh replaces it outright.
    available: bool,
    view: Viewport,
    cols: usize,
    bar: Bar,
    /// The deletion awaiting its second press: the name of the tag that
    /// asked. One slot — arming a different row moves the question, never
    /// queues two. Killed by any cursor move, any moving scroll, any mouse
    /// row change and any refresh.
    armed: Option<RefName>,
    dragging: bool,
}

impl Tags {
    /// A successfully read list. An empty vector is one too — no tags is a
    /// state, not a failure.
    pub fn new(tags: Vec<Tag>) -> Self {
        let mut view = Viewport::new();
        view.set_len(tags.len());
        let mut this = Self {
            rows: flatten(tags),
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
    /// no-tags line, no row to act on, and recoverable — the next
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
    /// keyboard stays on its tag: anchored by name into the next result set
    /// wherever it survives, clamped when it does not. An armed deletion
    /// dies with a result set that changed, like any other refresh.
    pub fn apply_query(&mut self, query: &str) {
        let next = Some(query.trim()).filter(|q| !q.is_empty());
        if self.query.as_deref() == next {
            return;
        }
        let anchored = self.row_at(self.view.cursor()).map(|r| r.name.clone());
        self.query = next.map(str::to_string);
        self.refilter();
        self.armed = None;
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
        let len = self.visible.len() as isize;
        let at = (self.view.cursor() as isize + by).rem_euclid(len) as usize;
        self.armed = None;
        self.view.go_to(at);
    }

    pub fn clear_search(&mut self) {
        self.apply_query("");
    }

    /// Swaps in a refreshed list, keeping the keyboard on its tag by name.
    /// A refresh is also the ref namespace saying things moved: an armed
    /// deletion dies here first.
    pub fn replace(&mut self, tags: Vec<Tag>) {
        self.armed = None;
        self.dragging = false;
        self.available = true;
        let (cursor, top) = (self.view.cursor(), self.view.top());
        let anchored = self.rows.get(cursor).map(|r| r.name.clone());
        self.rows = flatten(tags);
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

    /// The tag the keyboard is on, as the verbs address it: the name.
    /// `None` on an empty list — and on an unavailable one, which exposes
    /// no row to act on at all.
    pub fn current(&self) -> Option<RefName> {
        if !self.available {
            return None;
        }
        self.row_at(self.view.cursor()).map(|r| r.name.clone())
    }

    /// The commit the selected tag names, full object id — for the rare
    /// caller that needs the target rather than the name.
    pub fn current_commit(&self) -> Option<String> {
        self.row_at(self.view.cursor()).map(|r| r.commit.clone())
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
            .map(|r| r.name.clone());
        if self.armed.is_some() && self.armed != at {
            self.armed = None;
        }
    }

    /// The question standing, if one is — what the tests read to prove an
    /// arm moved, died, or stayed exactly where it was asked.
    pub fn armed(&self) -> Option<RefName> {
        self.armed.clone()
    }

    /// Arms — or confirms — a deletion of this exact tag name. First call
    /// on a target stores it and returns false: ask, don't act. Second call
    /// on the same target clears the arm and returns true: act. A refresh
    /// disarms unconditionally — the question was about a ref namespace
    /// that has since moved.
    pub fn confirm_or_arm_delete(&mut self, name: &RefName) -> bool {
        let already = self.armed.as_ref() == Some(name);
        self.armed = match already {
            true => None,
            false => Some(name.clone()),
        };
        already
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
                "tags unavailable",
                Ink::new(theme.chrome.error, theme.chrome.bg),
            ))
        } else if self.rows.is_empty() {
            Some((
                "no tags — n to name one",
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
                let armed = self.armed.as_ref() == Some(&r.name);
                let rest = Ink::new(
                    match armed {
                        true => theme.chrome.error,
                        false => theme.chrome.dim,
                    },
                    bg,
                );
                let text = r.text();
                let name_len = r.name.to_string_lossy().len();
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
            return "0 tags".into();
        }
        let shown = self.visible.len();
        let at = self
            .row_at(self.view.cursor())
            .map(|r| r.name.to_string_lossy().into_owned())
            .unwrap_or_default();
        format!("{}/{shown} · {at}", (self.view.cursor() + 1).min(shown))
    }
}

fn flatten(tags: Vec<Tag>) -> Vec<Row> {
    tags.into_iter()
        .map(|t| {
            let short = t.commit.chars().take(7).collect();
            Row {
                name: t.name,
                commit: t.commit,
                short,
                annotated: t.annotated,
                subject: t.subject,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tag(name: &str, commit: &str, annotated: bool, subject: Option<&str>) -> Tag {
        Tag {
            name: RefName::from(name),
            commit: commit.into(),
            annotated,
            subject: subject.map(str::to_string),
        }
    }

    #[test]
    fn rows_are_name_then_commit_then_subject_and_empty_is_quiet() {
        let host = Host::new();
        let c = &host.theme.chrome;
        let (x, cols) = (7, 55);
        let mut screen = Screen::new(70, 6);
        let sentinel = Ink::new(0x112233, 0x445566);
        screen.clear(sentinel);

        let mut v = Tags::new(vec![
            tag("v2.0", "aaa111bbb222", true, Some("release two")),
            tag("v1", "ccc333ddd444", false, None),
        ]);
        v.resize(cols, 6);
        v.paint(&mut screen, x, 0, true, &host);

        let row = screen.row_text(0);
        assert!(row.contains("v2.0"), "{row:?}");
        assert!(row.contains("aaa111b"), "{row:?}");
        assert!(row.contains("release two"), "{row:?}");
        assert_eq!(screen.ink(x, 0).unwrap().fg, c.fg);
        // A lightweight tag says so rather than drawing a bare commit.
        let bare = screen.row_text(1);
        assert!(bare.contains("v1"), "{bare:?}");
        assert!(bare.contains("(lightweight)"), "{bare:?}");
        // Nothing outside the pane's span changed.
        assert_eq!(screen.ink(x - 1, 0), Some(sentinel));
        assert_eq!(screen.ink(x + cols, 0), Some(sentinel));
        assert_eq!(
            v.current().as_ref().map(|n| n.as_bytes()),
            Some(b"v2.0".as_slice())
        );

        let mut empty = Tags::new(Vec::new());
        empty.resize(cols, 6);
        let mut screen = Screen::new(60, 6);
        empty.paint(&mut screen, x, 0, true, &host);
        assert!(
            screen.row_text(0).contains("no tags"),
            "{:?}",
            screen.row_text(0)
        );
        assert_eq!(empty.status(), "0 tags");
        assert_eq!(empty.current(), None);

        let mut failed = Tags::unavailable();
        failed.resize(cols, 6);
        let mut screen = Screen::new(60, 6);
        failed.paint(&mut screen, x, 0, true, &host);
        assert!(
            screen.row_text(0).contains("tags unavailable"),
            "{:?}",
            screen.row_text(0)
        );
        assert_eq!(failed.current(), None);
    }

    #[test]
    fn refresh_anchors_by_name_and_the_arm_dies_with_it() {
        let mut v = Tags::new(vec![
            tag("v1", "aaa111bbb222", false, None),
            tag("v2", "ccc333ddd444", true, Some("two")),
        ]);
        v.resize(30, 6);
        v.view.go_to(1);
        assert_eq!(
            v.current().as_ref().map(|n| n.as_bytes()),
            Some(b"v2".as_slice())
        );

        v.replace(vec![
            tag("v2", "ccc333ddd444", true, Some("two")),
            tag("v1", "aaa111bbb222", false, None),
        ]);
        assert_eq!(
            v.current().as_ref().map(|n| n.as_bytes()),
            Some(b"v2".as_slice())
        );

        v.replace(vec![tag("v1", "aaa111bbb222", false, None)]);
        assert_eq!(
            v.current().as_ref().map(|n| n.as_bytes()),
            Some(b"v1".as_slice())
        );

        assert!(!v.confirm_or_arm_delete(&RefName::from("v1")));
        assert!(v.confirm_or_arm_delete(&RefName::from("v1")));
        assert_eq!(v.armed, None);
        assert!(!v.confirm_or_arm_delete(&RefName::from("v1")));
        v.replace(vec![tag("v1", "aaa111bbb222", false, None)]);
        assert_eq!(v.armed, None, "a refresh disarms");
        v.confirm_or_arm_delete(&RefName::from("v1"));
        v.down();
        assert_eq!(v.armed, None, "a keyboard move disarms");

        assert_eq!(v.selection(), "");
        assert!(!v.select_none());
    }
}
