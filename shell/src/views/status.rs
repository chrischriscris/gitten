//! The status pane: where HEAD sits, and how to move it.
//!
//! Row 0 is the fact — `⎇ full/full · ↑1 ↓0`, the branch and how far it has
//! drifted from its upstream, or a ✓ when it has not. This is the fact
//! lazygit's [1] Status exists to say, and the one fact this window otherwise
//! buries: the title strip names the branch only while no filter is live, and
//! the branches list puts it among sixteen others. A ✓ is furniture ink, not
//! a success green — there is no green in [`ChromePalette`], and inventing
//! one for a tick is a colour that means one thing in one pane.
//!
//! The rows under it are the verbs that belong to exactly these facts —
//! `pull`, `push`, `fetch` — each a command *name* from the registry the
//! keymap and the help overlay read, never a spelled-out second copy. The
//! keyboard moves a visible cursor over them and `enter` dispatches the
//! name through the same path a keybinding takes; the rows teach their own
//! faster path by drawing the bound global key at the right edge, the way
//! the help overlay spells pairs. While one of the verbs runs, its row
//! shows the same line the status band shows — read from the band's cell,
//! not kept twice.
//!
//! The facts themselves are still `branches`' own. It reads `head()` on its
//! refresh wave — the only read of it this window pays for — so this pane
//! reads that model per frame and never disagrees with the list beside it.
//! An extension that took the branches pane over would starve this one of
//! its answer, which is the honest failure: an absent branch, not a stale
//! one. An extension that added a verb to [`ACTIONS`] would render and
//! dispatch with no shell edit at all — a row is data, and the dispatch is
//! by name.

use crate::chrome;
use crate::graph;
use crate::views::branches::Branches;
use gitten_core::command::Modes;
use gitten_core::host::Host;
use gpui::*;
use std::cell::Cell;

/// The mode the pane's own keys bind in — the name the shell's screen table
/// also answers to. Only what is genuinely particular is bound here (`enter`);
/// the list vocabulary rides with the globals, exactly as every other pane's
/// does.
pub const MODE: &str = "status";

/// The action rows, by command name — the registry's own spelling, which is
/// what the dispatch reads. The order is the one the pane's facts suggest:
/// take first (`pull`), then send (`push`), then look (`fetch`) — lazygit's
/// sync verbs, homed where their facts live. The label each row draws comes
/// from the same registry's hint, so a renamed verb renames here without an
/// edit.
const ACTIONS: &[&str] = &["repo.pull", "repo.push", "repo.fetch"];

/// The status pane. `repo` is the repository's own name — the bright half of
/// the fact row — solved once at construction, the way every other pane
/// solves its label. `branches` is who owns the HEAD answer.
pub struct Status {
    repo: SharedString,
    branches: Option<Entity<Branches>>,
    /// The cursor, over [`ACTIONS`] — the fact row is not in its space, so
    /// clamping is clamping to verbs and nothing else.
    cursor: usize,
    focused: bool,
    /// The row a right-click landed on, published for the shell — which opens
    /// the pane's context menu over it. Taken once by whoever opens it: one
    /// right-click, one open.
    menu_row: Cell<Option<usize>>,
    /// The verb job the status band is currently describing, as
    /// `(command name, the band's own line)`. Written by the shell from the
    /// band's cell each frame — the pane keeps no timer and no second copy
    /// of the sentence.
    pub(crate) running: Option<(SharedString, SharedString)>,
}

impl Status {
    pub fn new(repo: impl Into<SharedString>, branches: Option<Entity<Branches>>) -> Self {
        Self {
            repo: repo.into(),
            branches,
            cursor: 0,
            focused: false,
            menu_row: Cell::new(None),
            running: None,
        }
    }

    /// How many rows the pane draws — the fact plus the verbs. What sizes
    /// this pane's sidebar section, and the same count the render walks.
    pub fn rows(&self) -> usize {
        1 + ACTIONS.len()
    }

    /// Where a click lands the keyboard: onto the row the mouse hit, with
    /// exactly the effects a key move has. `index` is the *rendered* row —
    /// 0 is the fact, which is not in the cursor's space, so it snaps to
    /// the nearest verb below, the same rule the other panes' headings run;
    /// anything past the rows clamps.
    /// The row a right-click landed on, published by the row's own handler
    /// beside the left-click one, and taken once by the shell — which opens
    /// the pane's context menu over it. Taken, not read: one right-click,
    /// one open.
    pub fn take_menu_row(&self) -> Option<usize> {
        self.menu_row.take()
    }

    pub fn select_row(&mut self, index: usize) {
        self.cursor = index.min(ACTIONS.len()).saturating_sub(1);
    }

    /// The command the cursor is on, by name — what `enter` dispatches.
    /// The cursor never rests on the fact row, so this is always a verb
    /// while the pane ships its three.
    pub fn current(&self) -> Option<&'static str> {
        ACTIONS.get(self.cursor).copied()
    }

    pub fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }

    /// Runs one of the commands this pane answers: the shared list
    /// vocabulary, over three action rows. There is no viewport here, so
    /// the scrolls are answered without doing anything — a resolved command
    /// must not read as a failed one, the rule the commit graph runs.
    ///
    /// False is "not one of mine", and the caller says so.
    pub fn run_view(&mut self, command: &str, _: &Host) -> bool {
        let last = ACTIONS.len() - 1;
        match command {
            "view.down" => self.cursor = (self.cursor + 1).min(last),
            "view.up" => self.cursor = self.cursor.saturating_sub(1),
            "view.page-down" | "view.bottom" => self.cursor = last,
            "view.page-up" | "view.top" => self.cursor = 0,
            "view.scroll-down" | "view.scroll-up" | "view.left" | "view.right" => return true,
            _ => return false,
        }
        true
    }
}

/// The one row's height, shared with every other list row.
const ROW_H: f32 = graph::ROW_H;

impl Render for Status {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let host = crate::config::host(cx);
        let c = host.theme.chrome;
        let ch = host.font.char_width();
        let info = self.branches.as_ref().and_then(|b| b.read(cx).head_info());

        // Row 0: the fact. Not a list row — it is not selectable, and a
        // frame that looked like one would be one the cursor skips for no
        // reason the eye can see.
        let fact = {
            let row = div()
                .flex()
                .items_center()
                .min_w_full()
                .h(px(ROW_H))
                .pl(px(chrome::ROW_PAD))
                .pr(chrome::gap_m(&host.font))
                .child(
                    div()
                        .flex_none()
                        .text_color(rgb(c.fg))
                        .child(self.repo.clone()),
                );
            match info {
                Some(info) => row
                    .child(
                        div()
                            .flex_none()
                            .px(px(ch * 0.5))
                            // Every quiet ink in this row is read — a direction, a
                            // state, a distance — so all four go through
                            // `quiet_on`: raw `faint` is 2.05:1 on the pane.
                            .text_color(rgb(host.theme.quiet_on(c.bg)))
                            .child("→"),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_color(rgb(c.dim))
                            .child(info.chip.clone()),
                    )
                    .children(match (info.ahead, info.behind) {
                        (Some(0), Some(0)) => Some(
                            div()
                                .flex_none()
                                .pl(px(ch))
                                .text_color(rgb(host.theme.quiet_on(c.bg)))
                                .child("✓"),
                        ),
                        (ahead, behind) => super::branches::drift(ahead, behind).map(|drift| {
                            div()
                                .flex_none()
                                .pl(px(ch))
                                .text_color(rgb(host.theme.quiet_on(c.bg)))
                                .child(SharedString::from(drift))
                        }),
                    }),
                None => row.child(
                    div()
                        .flex_none()
                        .pl(px(ch))
                        .text_color(rgb(host.theme.quiet_on(c.bg)))
                        .child("no branch"),
                ),
            }
        };

        // The keys the rows teach: resolved the way a press is, innermost
        // first — so a key an inner mode took over would not be named here,
        // the same rule the help overlay's pairs run.
        let mut modes = Modes::new();
        modes.push(crate::panes::MODE);
        modes.push(MODE);

        // A click on a row is the keyboard coming back — see [`Self::select_row`].
        // Built as a plain handle, not `cx.listener`: the rows are drawn in a
        // closure over `&mut App`, where no listener can be minted.
        let this = cx.entity().downgrade();
        let cursor = self.cursor;
        let focused = self.focused;
        let running = self.running.clone();
        let actions = ACTIONS
            .iter()
            .enumerate()
            .map(|(i, name)| {
                // The band's line takes the key's place while the verb runs —
                // the same rule the band itself runs: a sentence owed takes
                // the hints' place rather than a band of its own.
                let right = match running.as_ref().filter(|(cmd, _)| cmd == name) {
                    Some((_, text)) => text.clone(),
                    None => host
                        .keys
                        .live_keys_for(name, &modes)
                        .first()
                        .map(|k| SharedString::from(k.as_str()))
                        .unwrap_or_else(|| SharedString::from("")),
                };
                let label = host.commands.hint(name).unwrap_or(name);
                let row = chrome::list_row(&host, i == cursor, focused, ROW_H)
                    .child(
                        div()
                            .flex_none()
                            .text_color(rgb(c.fg))
                            .child(SharedString::from(label)),
                    )
                    .child(
                        div()
                            .flex_none()
                            .ml_auto()
                            .pr(chrome::gap_m(&host.font))
                            .text_color(rgb(c.dim))
                            .child(right),
                    );
                let this = this.clone();
                row.id(("status-row", i))
                    .on_mouse_down(MouseButton::Right, {
                        let this = this.clone();
                        move |_: &MouseDownEvent, _, cx| {
                            let Some(this) = this.upgrade() else {
                                return;
                            };
                            this.update(cx, |s, cx| {
                                s.select_row(i + 1);
                                s.menu_row.set(Some(i + 1));
                                cx.notify();
                            });
                        }
                    })
                    .on_mouse_down(MouseButton::Left, move |_: &MouseDownEvent, _, cx| {
                        let Some(this) = this.upgrade() else {
                            return;
                        };
                        this.update(cx, |s, cx| {
                            s.select_row(i + 1);
                            cx.notify();
                        });
                    })
                    .into_any_element()
            })
            .collect::<Vec<_>>();

        div()
            .flex()
            .flex_col()
            .min_w_full()
            .child(fact)
            .children(actions)
    }
}

#[cfg(test)]
mod tests {
    use super::Status;
    use gitten_core::host::Host;
    use gpui::{AppContext as _, TestAppContext};

    #[gpui::test]
    fn it_renders_without_a_branches_pane(cx: &mut TestAppContext) {
        cx.new(|_| Status::new("repo", None));
    }

    #[gpui::test]
    fn the_cursor_walks_the_verbs_and_never_the_fact(_cx: &mut TestAppContext) {
        let mut s = Status::new("repo", None);
        let host = Host::new();
        assert_eq!(s.rows(), 4, "the fact row plus the three verbs");
        assert_eq!(
            s.current(),
            Some("repo.pull"),
            "the cursor starts on the first verb"
        );

        // `j` clamps at the bottom of three rows...
        for _ in 0..5 {
            s.run_view("view.down", &host);
        }
        assert_eq!(s.current(), Some("repo.fetch"));
        // ...and `k` at the top. The fact row is not in the cursor's space,
        // so clamping is clamping to verbs and nothing else.
        for _ in 0..5 {
            s.run_view("view.up", &host);
        }
        assert_eq!(s.current(), Some("repo.pull"));

        // The ends and the pages spell the same two places.
        s.run_view("view.bottom", &host);
        assert_eq!(s.current(), Some("repo.fetch"));
        s.run_view("view.page-up", &host);
        assert_eq!(s.current(), Some("repo.pull"));
        s.run_view("view.top", &host);
        assert_eq!(s.current(), Some("repo.pull"));

        // A click names the *rendered* row: the fact (0) snaps to the
        // nearest verb below, the same rule the other panes' headings run,
        // and anything past the rows clamps.
        s.select_row(0);
        assert_eq!(s.current(), Some("repo.pull"));
        s.select_row(3);
        assert_eq!(s.current(), Some("repo.fetch"));
        s.select_row(99);
        assert_eq!(s.current(), Some("repo.fetch"), "a click past the rows");

        // The scrolls are answered without doing anything — there is no
        // viewport here — and an unknown verb is said, not swallowed.
        assert!(s.run_view("view.scroll-down", &host));
        assert_eq!(s.current(), Some("repo.fetch"), "the scroll moved nothing");
        assert!(!s.run_view("repo.pull", &host));
    }
}
