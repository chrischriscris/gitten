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
use gitten_core::refs::Stash;
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

/// What an armed drop asks, once, in the status line: the address, spoken
/// the way git spells it, because dropping `stash@{0}` means *the top*,
/// whatever the entry says about itself. The command's own name
/// (`stashes.drop`) belongs to the keymap and the help panel; the question
/// speaks the thing being dropped.
pub fn drop_question(index: usize) -> String {
    format!("drop stash@{{{index}}}? press again to confirm")
}

/// The stash list.
///
/// Holds the flattened stack, the viewport and the drop arm; knows nothing
/// about keys. Every method is a command, exactly as in
/// [`crate::commits::Commits`] and for the same reason.
pub struct Stashes {
    rows: Vec<Row>,
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
    /// The drop awaiting its second press: the stack index of the row that
    /// asked. One slot — arming a different row moves the question, never
    /// queues two. Killed by any cursor move, any moving scroll, any mouse
    /// row change and any refresh; a focus round trip alone does not touch
    /// it, because the question sits on the row it was asked about.
    armed: Option<usize>,
    dragging: bool,
    /// Where in the scrollbar's thumb it was taken hold of, while it is held.
    grabbed: Option<usize>,
}

impl Stashes {
    /// A successfully read stack. An empty vector is one too — nothing
    /// parked is a state, not a failure.
    pub fn new(stashes: Vec<Stash>) -> Self {
        let mut view = Viewport::new();
        view.set_len(stashes.len());
        Self {
            rows: flatten(&stashes),
            available: true,
            view,
            cols: 0,
            bar: Bar::default(),
            armed: None,
            dragging: false,
            grabbed: None,
        }
    }

    /// The pane after a failed read: honest emptiness that is *not* the
    /// empty-stack line, no row to act on, and recoverable — the next
    /// successful refresh replaces it outright.
    pub fn unavailable() -> Self {
        Self {
            rows: Vec::new(),
            available: false,
            view: Viewport::new(),
            cols: 0,
            bar: Bar::default(),
            armed: None,
            dragging: false,
            grabbed: None,
        }
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
        self.grabbed = None;
        self.available = true;
        let (cursor, top) = (self.view.cursor(), self.view.top());
        let anchored = self.rows.get(cursor).map(|r| r.commit.clone());
        self.rows = flatten(&stashes);
        // The old scroll position first, then the anchor: `go_to` drags the
        // viewport after the cursor, and the surviving commit's row must be
        // the one on screen when it survives.
        self.view.set_len(self.rows.len());
        self.view.scroll_to(top);
        let at = anchored
            .and_then(|c| self.rows.iter().position(|r| r.commit == c))
            .unwrap_or_else(|| cursor.min(self.rows.len().saturating_sub(1)));
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
        self.rows.get(self.view.cursor()).map(|r| r.index)
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
        self.view.scroll_by(by);
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
    pub fn press(&mut self, col: usize, row: usize, _extend: bool, host: &Host) {
        if scrollbar::hit(col, self.cols, &self.view, host) {
            let row = row.min(self.view.height().saturating_sub(1));
            let before = self.view.top();
            self.grabbed = Some(scrollbar::grab(&mut self.view, host, row));
            // A press on the track can jump the thumb: a moving scrollbar is
            // a moving list, and the question was asked about a row of the
            // list that was.
            if self.view.top() != before {
                self.armed = None;
            }
            return;
        }
        let Some(index) = self.view.row_at(row) else {
            return;
        };
        self.view.go_to(index);
        self.disarm_if_row_moved(index);
    }

    /// The pointer moved with the button down. A row above or below the body
    /// scrolls by the overshoot; a drag never builds a range, so what it can
    /// move is the cursor or the scrollbar and nothing else.
    pub fn drag(&mut self, row: isize, host: &Host) {
        if let Some(grabbed) = self.grabbed {
            let before = self.view.top();
            scrollbar::drag(&mut self.view, host, row.max(0) as usize, grabbed);
            if self.view.top() != before {
                self.armed = None;
            }
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
        let Some(index) = self.view.row_at(row as usize) else {
            return;
        };
        self.view.go_to(index);
        self.disarm_if_row_moved(index);
    }

    pub fn release(&mut self) {
        self.dragging = false;
        self.grabbed = None;
    }

    /// The press or drag landed the keyboard on row `index`; the question,
    /// if one stands, was asked about the row it stood on. Different rows,
    /// no question.
    fn disarm_if_row_moved(&mut self, index: usize) {
        if self
            .armed
            .is_some_and(|a| self.rows.iter().position(|r| r.index == a) != Some(index))
        {
            self.armed = None;
        }
    }

    /// Arms — or confirms — a drop of this exact stack index. First call on
    /// a target stores it and returns false: ask, don't act. Second call on
    /// the same target clears the arm and returns true: act. Anything else
    /// re-arms onto the new target and returns false again.
    ///
    /// The arm holds the **index**, the thing a drop aims at — which is also
    /// why a refresh disarms unconditionally: after any drop or pop every
    /// later number shifts, and a yes addressed to yesterday's numbering is
    /// the accident the double press exists to prevent.
    pub fn confirm_or_arm_drop(&mut self, index: usize) -> bool {
        let already = self.armed == Some(index);
        self.armed = match already {
            true => None,
            false => Some(index),
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
                Ink::new(theme.chrome.faint, theme.chrome.bg),
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
                let Some(r) = self.rows.get(vis) else {
                    pen.wash(blank);
                    continue;
                };
                let bg = match focused && vis == self.view.cursor() {
                    true => theme.chrome.selection_bg,
                    false => theme.chrome.bg,
                };
                let address = Ink::new(theme.chrome.dim, bg);
                let armed = self.armed == Some(r.index);
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
        if self.cols > 0 {
            // Last, and over the rows rather than beside them — at this
            // pane's own last column, which is not the screen's.
            scrollbar::paint(screen, self.bar, x + self.cols - 1, y, &self.view, host);
        }
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
        format!(
            "{}/{} · {}",
            (self.view.cursor() + 1).min(self.rows.len()),
            self.rows.len(),
            self.rows[self.view.cursor().min(self.rows.len() - 1)].title,
        )
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

    #[test]
    fn stash_drop_arm_survives_only_the_same_row() {
        let host = Host::new();
        let mut v = Stashes::new(tall_stack());
        v.resize(30, 6);

        // First press asks; second press on the same row acts, and the act
        // spends the arm.
        assert!(!v.confirm_or_arm_drop(0));
        assert_eq!(v.armed, Some(0));
        assert!(v.confirm_or_arm_drop(0));
        assert_eq!(v.armed, None);

        // A different row re-arms rather than inheriting the question.
        assert!(!v.confirm_or_arm_drop(1));
        assert_eq!(v.armed, Some(1));
        assert!(!v.confirm_or_arm_drop(0), "another row asks again");
        assert_eq!(v.armed, Some(0));

        // A keyboard move disarms: the question was about the row that was
        // under the keyboard.
        v.down();
        assert_eq!(v.armed, None);
        // ...a wheel that actually moved the list...
        v.confirm_or_arm_drop(0);
        v.scroll_y(3);
        assert_eq!(v.armed, None, "a moving scroll disarms");
        // ...a press on another row...
        v.confirm_or_arm_drop(0);
        v.press(3, 2, false, &host);
        assert_eq!(v.armed, None, "a mouse row change disarms");
        // ...and a refresh, unconditionally: indices renumber under a drop,
        // and the arm holds an index.
        v.confirm_or_arm_drop(0);
        v.replace(stack());
        assert_eq!(v.armed, None, "a refresh disarms");

        // A click on the armed row itself is neither an answer nor a re-ask.
        v.confirm_or_arm_drop(0);
        v.press(3, 0, false, &host);
        assert_eq!(v.armed, Some(0));

        // Merely painting the pane unfocused, then focused — the focus round
        // trip — moves nothing: the question sits on its row.
        let mut screen = Screen::new(44, 6);
        v.paint(&mut screen, 0, 0, false, &host);
        assert_eq!(v.armed, Some(0));
        // The armed row wears the error ink with the keyboard *elsewhere*:
        // the address keeps its furniture ink, the message is the thing
        // being asked about, and neither waits for focus.
        let c = &host.theme.chrome;
        assert_eq!(screen.ink(0, 0).unwrap().fg, c.dim);
        assert_eq!(screen.ink(11, 0).unwrap().fg, c.error);
        v.paint(&mut screen, 0, 0, true, &host);
        assert_eq!(v.armed, Some(0), "the focus round trip moved nothing");
        assert_eq!(screen.ink(11, 0).unwrap().fg, c.error);

        // And the question is the address, spoken once, exactly.
        assert_eq!(drop_question(0), "drop stash@{0}? press again to confirm");
    }
}
