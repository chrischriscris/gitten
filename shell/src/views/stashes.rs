//! The stash stack, as a list.
//!
//! lazygit's Stash panel, viewer half: what [`Vec<Stash>`] says, newest first
//! as the read gives it, each row named `stash@{n}` beside its message — the
//! address first, because the address is what every verb on this pane aims
//! at, and the message is only what the entry says about itself.
//!
//! The list idioms are [`super::files`]'s, on purpose: one `Viewport`, one
//! scroll-handle dance, rows flattened **once per refresh**, and a destructive
//! verb that confirms in this pane rather than in a dialog — there is no
//! modal anywhere in the window yet.

use super::{accept_deferred_scroll, DeferredScrollbar, PendingScroll};
use crate::graph::ROW_H;
use gitten_core::host::Host;
use gitten_core::refs::Stash;
use gitten_core::view::Viewport;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::scroll::Scrollbar;
use std::cell::Cell;
use std::rc::Rc;

/// One flat row of the pane: one entry of the stack.
///
/// Flattened once per refresh — never per frame. The display strings are
/// built here; what a draw reads is an enum match away from a refcount bump.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Row {
    /// The position on the stack — how every verb addresses this entry.
    pub index: usize,
    /// The commit the stash hangs on, kept as this row's identity across a
    /// refresh: indices renumber under a drop, the commits do not.
    pub commit: String,
    /// `stash@{n}`, spelled once at flatten.
    pub title: SharedString,
    /// What the entry says about itself, decoded lossily once at flatten —
    /// the read already decided messages are display text.
    pub message: SharedString,
}

/// The pane's rows plus the title-strip line. Pure — the unit-tested half of
/// a refresh.
pub(crate) struct Prepared {
    pub(crate) rows: Vec<Row>,
    /// The title-strip line: who we are and how much is parked.
    pub(crate) label: String,
}

/// Spells the address the way git does, once, at flatten.
pub(crate) fn title(index: usize) -> String {
    format!("stash@{{{index}}}")
}

/// Flattens the stack into display rows, newest first as the read gives it.
pub(crate) fn flatten(stashes: &[Stash]) -> Vec<Row> {
    stashes
        .iter()
        .map(|s| Row {
            index: s.index,
            commit: s.commit.clone(),
            title: title(s.index).into(),
            message: s.message.as_str().into(),
        })
        .collect()
}

/// [`flatten`] plus what the title strip says about it. The load line goes to
/// stderr like every other view's.
pub(crate) fn prepare(stashes: &[Stash], describe: &str) -> Prepared {
    let t = std::time::Instant::now();
    let rows = flatten(stashes);
    eprintln!(
        "stashes: {} entries · flatten {:.0?}",
        rows.len(),
        t.elapsed()
    );
    Prepared {
        label: format!("{describe} · {} parked", rows.len()),
        rows,
    }
}

/// What an armed drop asks, once, in the notice band. The address is the
/// honest name here: dropping `stash@{0}` means *the top*, whatever it says
/// about itself.
pub(crate) fn drop_question(shown: &str) -> String {
    format!("drop {shown}? press again to confirm")
}

/// The stash pane. Holds flattened rows behind an `Rc`, so a refresh swaps
/// one refcount instead of mutating what a frame may be reading.
///
/// Dropping confirms **in this pane**, by the same mechanics as a file
/// discard: the first press stores [`Stashes::armed`] and asks through the
/// notice band; the second press on the same row executes; any cursor move,
/// wheel or refresh drops the arm. Outliving a switch to another pane and
/// back is deliberate — the question sits on the row it was asked about.
pub struct Stashes {
    data: Rc<Vec<Row>>,
    scroll: UniformListScrollHandle,
    view: Rc<Cell<Viewport>>,
    synced: Rc<Cell<f32>>,
    pending_scroll: PendingScroll,
    rendered: Rc<Cell<usize>>,
    /// The drop awaiting its second press: the index of the row that asked.
    /// One slot — arming a different row moves the question, never queues two.
    armed: Option<usize>,
    /// Whether this pane holds the keyboard, as the shell last told it. A
    /// row's bar is accent only when its pane is focused, and the view cannot
    /// ask the shell during render — so the shell writes it here when focus
    /// moves, and render reads a flag.
    focused: bool,
}

impl Stashes {
    /// The viewport model with everything live folded in — see
    /// [`super::files::Files::live_view`].
    fn live_view(&self, host: &Host) -> Viewport {
        let mut v = self.view.get();
        v.set_len(self.data.len());
        v.set_height(self.rendered.get());
        v.set_scrolloff(host.view.scrolloff);
        v
    }

    /// Told by the shell whenever the keyboard moves — never decided here.
    pub fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }

    /// Whether this pane holds the keyboard. The rows read it for the bar.
    #[allow(dead_code)]
    pub fn focused(&self) -> bool {
        self.focused
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
            focused: false,
        }
    }

    /// Whether the stack had nothing on it — the empty state's trigger.
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Replaces repository data while keeping the selection anchored to its
    /// entry's commit. An index cannot anchor — dropping any entry renumbers
    /// everything above it — but the commit under an entry is stable, which
    /// is why the row keeps it.
    #[cfg(test)]
    fn replace(&mut self, stashes: &[Stash], host: &Host) {
        self.replace_prepared(prepare(stashes, ""), host);
    }

    pub(crate) fn replace_prepared(&mut self, prepared: Prepared, host: &Host) {
        // A refresh is the repository saying things moved; an armed drop was
        // a promise about how they were, so it dies here first.
        self.armed = None;
        self.reconcile(host);
        let old = self.view.get();
        let anchored = self.data.get(old.cursor()).map(|r| r.commit.clone());
        let Prepared { rows, .. } = prepared;
        self.data = Rc::new(rows);

        let cursor = anchored.and_then(|commit| self.data.iter().position(|r| r.commit == commit));
        let cursor = cursor.unwrap_or_else(|| old.cursor().min(self.data.len().saturating_sub(1)));
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

    /// Nothing off the left edge to reach — messages truncate rather than pan.
    pub fn pan_pixels(&self, _dx: f32) -> bool {
        false
    }

    /// Moves the list by `dy` pixels — the wheel, whose command resolves
    /// through `[keys]` but whose delta is pixels. Same dance as every list.
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
    /// [`super::files::Files::reconcile`].
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

    /// Runs one of the commands this pane answers — the shared `view.*`
    /// family over the same [`Viewport`] arithmetic every other list runs.
    /// False is "not one of mine".
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
            "view.left" | "view.right" => return true,
            _ => return false,
        }
        // The keyboard moved — whatever was armed was armed to what the
        // keyboard used to be on.
        self.armed = None;
        self.view.set(v);
        self.show(v);
        true
    }

    /// Puts row `v.top()` at the top of the viewport, exactly — see
    /// [`super::files::Files::show`].
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

    /// The row the keyboard is on. `None` on an empty stack.
    pub(crate) fn current(&self) -> Option<&Row> {
        self.data.get(self.view.get().cursor())
    }

    /// Arms — or confirms — a drop of this exact row. First call on a target
    /// stores it and returns false: ask, don't act. Second call on the same
    /// target clears the arm and returns true: act. Anything else re-arms
    /// onto the new target and returns false again.
    ///
    /// The arm holds the **index**, the thing a drop aims at — which is also
    /// why a refresh disarms unconditionally below: after any drop the
    /// numbers shift, and a yes addressed to yesterday's numbering is the
    /// accident the double press exists to prevent.
    pub(crate) fn confirm_or_arm_drop(&mut self, index: usize) -> bool {
        let already = self.armed == Some(index);
        self.armed = match already {
            true => None,
            false => Some(index),
        };
        already
    }

    /// Whether a drop is waiting for its second press — the render's tint of
    /// the row the question is about.
    #[cfg(test)]
    pub(crate) fn armed_row(&self) -> Option<usize> {
        self.armed
    }

    /// What `copy.selection` copies here: the row the keyboard is on, as git
    /// would spell it — the address, then the message.
    pub fn cursor_text(&self) -> String {
        match self.current() {
            Some(r) => format!("{} {}", r.title, r.message),
            None => String::new(),
        }
    }

    /// No drag selection over a stack yet; `select.all` is inert here.
    pub fn select_all(&mut self) -> bool {
        false
    }

    pub fn select_none(&mut self) -> bool {
        false
    }
}

/// Air between the address column and the message, in characters.
const GAP_CHARS: f32 = 1.5;

impl Render for Stashes {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = crate::config::host(cx).theme.chrome;
        // An empty stack is a quiet line, not an empty box — the compact twin
        // of the files pane's clean-tree line, sitting top-left because this
        // pane is a short section of the sidebar, not a whole column.
        if let Some(empty) = self.is_empty().then(|| {
            div()
                .size_full()
                .px_3()
                .pt_2()
                .flex()
                .items_start()
                .text_color(rgb(c.faint))
                .child("nothing stashed")
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
        let armed = self.armed;
        let list = uniform_list("stashes", data.len(), move |range, _, cx| {
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
        .px_3();

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

/// One row: the address dim — furniture, not content — then the message in
/// the inherited ink. `current` paints `chrome.selection_bg`; `armed` turns
/// the whole row toward `chrome.error`, the colour the second press spends.
fn row(e: &Row, host: &Host, current: bool, armed: bool) -> AnyElement {
    let ch = host.font.char_width();
    let c = host.theme.chrome;
    div()
        .flex()
        .items_center()
        .min_w_full()
        .h(px(ROW_H))
        .bg(rgb(match current {
            true => c.selection_bg,
            false => c.bg,
        }))
        .child(
            div()
                .flex_none()
                .text_color(rgb(match armed {
                    true => c.error,
                    false => c.dim,
                }))
                .child(e.title.clone()),
        )
        .child(
            div()
                .flex_none()
                .ml(px(GAP_CHARS * ch))
                .min_w_0()
                .when(armed, |d| d.text_color(rgb(c.error)))
                .child(e.message.clone()),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    // By name, not a glob: `use gpui::*` in the parent shadows `#[test]` with
    // GPUI's own attribute macro and every test in here fails to expand.
    use super::{flatten, prepare, title, Stash};
    use gitten_core::host::Host;

    fn stash(index: usize, commit: &str, message: &str) -> Stash {
        Stash {
            index,
            commit: commit.into(),
            message: message.into(),
        }
    }

    fn sample() -> Vec<Stash> {
        vec![
            stash(0, "c0ffee0", "On main: hand written"),
            stash(1, "c0ffee1", "WIP on main: abc1234 seed"),
        ]
    }

    /// The rows as plain text, for assertions.
    fn outline(stashes: &[Stash]) -> Vec<String> {
        flatten(stashes)
            .iter()
            .map(|r| format!("{} {}", r.title, r.message))
            .collect()
    }

    fn pane(stashes: &[Stash]) -> super::Stashes {
        super::Stashes::from_prepared(prepare(stashes, ""))
    }

    fn with_height(f: &mut super::Stashes, n: usize) {
        f.rendered.set(n);
        let mut v = f.view.get();
        v.set_len(f.data.len());
        v.set_height(n);
        f.view.set(v);
    }

    #[test]
    fn rows_are_the_addresses_git_counts_newest_first() {
        assert_eq!(
            outline(&sample()),
            vec![
                "stash@{0} On main: hand written",
                "stash@{1} WIP on main: abc1234 seed",
            ],
            "the read's order is the stack's order"
        );
        // The spelling is git's own, braces and all.
        assert_eq!(title(0), "stash@{0}");
        assert_eq!(title(12), "stash@{12}");
    }

    #[test]
    fn the_label_counts_what_is_parked() {
        let prepared = prepare(&sample(), "gitten (main)");
        assert_eq!(prepared.label, "gitten (main) · 2 parked");
        assert_eq!(
            prepare(&[], "gitten (main)").label,
            "gitten (main) · 0 parked"
        );
        assert!(prepare(&[], "").rows.is_empty());
    }

    #[test]
    fn navigation_clamps_and_the_cursor_names_an_address() {
        let host = Host::new();
        let mut f = pane(&sample());
        with_height(&mut f, 4);
        for _ in 0..20 {
            f.run_view("view.down", &host);
        }
        assert_eq!(f.current().expect("a row").index, 1, "clamped at the end");
        assert!(f.run_view("view.top", &host));
        assert_eq!(f.current().expect("a row").index, 0);

        // And copy spells the row as git would.
        assert_eq!(f.cursor_text(), "stash@{0} On main: hand written");
    }

    #[test]
    fn replacement_anchors_on_the_entrys_commit_not_its_number() {
        // Drop the top and everything above renumbers: the former stash@{1}
        // IS stash@{0} now, and the keyboard follows the *entry* it was on,
        // not the slot it sat in.
        let host = Host::new();
        let mut f = pane(&sample());
        with_height(&mut f, 4);
        f.run_view("view.bottom", &host);
        assert_eq!(f.current().unwrap().index, 1);

        f.replace(&[stash(0, "c0ffee1", "WIP on main: abc1234 seed")], &host);
        assert_eq!(
            f.current().unwrap().commit,
            "c0ffee1",
            "the cursor stayed on the entry that survived"
        );
        assert_eq!(f.current().unwrap().title, "stash@{0}", "renumbered");
    }

    #[test]
    fn an_emptied_stack_lands_the_cursor_at_home_and_says_so() {
        let host = Host::new();
        let mut f = pane(&sample());
        with_height(&mut f, 4);
        f.run_view("view.bottom", &host);
        f.replace(&[], &host);
        assert!(f.is_empty());
        assert!(f.current().is_none());
        assert_eq!((f.view.get().cursor(), f.view.get().top()), (0, 0));

        // Back to something, from the top again.
        f.replace(&sample(), &host);
        assert_eq!(f.view.get().cursor(), 0);
    }

    #[test]
    fn a_drop_arms_then_confirms_on_the_second_press_of_the_same_row() {
        let mut f = pane(&sample());
        with_height(&mut f, 4);

        assert!(!f.confirm_or_arm_drop(0), "first press asks");
        assert_eq!(f.armed_row(), Some(0));

        assert!(f.confirm_or_arm_drop(0), "second press spends");
        assert_eq!(f.armed_row(), None, "no latched yes");

        // And a different row moves the question rather than confirming it.
        assert!(!f.confirm_or_arm_drop(1));
        assert_eq!(f.armed_row(), Some(1), "one slot, moved");
        assert!(!f.confirm_or_arm_drop(0), "a third row asks again");
    }

    #[test]
    fn any_cursor_move_or_refresh_disarms_an_armed_drop() {
        let host = Host::new();
        let mut f = pane(&sample());
        with_height(&mut f, 4);
        assert!(!f.confirm_or_arm_drop(1));

        // One step of the keyboard leaves the question's row.
        assert!(f.run_view("view.down", &host));
        assert_eq!(
            f.armed_row(),
            None,
            "the question did not survive its own move"
        );

        // A refresh that changes nothing still says "things moved" — and a
        // drop renumbers the stack, so a stale index must never survive it.
        assert!(!f.confirm_or_arm_drop(1));
        f.replace(&sample(), &host);
        assert_eq!(f.armed_row(), None);
        // The press after it re-arms rather than executes.
        assert!(!f.confirm_or_arm_drop(1));
    }

    #[test]
    fn the_arm_question_names_the_row_it_was_asked_about() {
        assert_eq!(
            super::drop_question("stash@{0}"),
            "drop stash@{0}? press again to confirm"
        );
    }
}
