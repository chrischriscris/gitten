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

use super::{accept_deferred_scroll, vertical_scrollbar, DeferredScrollbar, PendingScroll};
use crate::chrome::{empty_line, list_row};
use crate::graph::ROW_H;
use gitten_core::host::Host;
use gitten_core::refs::Stash;
use gitten_core::theme;
use gitten_core::view::Viewport;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
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
    if crate::stats::enabled() {
        eprintln!(
            "stashes: {} entries · flatten {:.0?}",
            rows.len(),
            t.elapsed()
        );
    }
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
/// The rows a query keeps, as indices into `rows`: a stash whose message
/// matches. The list is flat — no headings to carry.
fn search_rows(rows: &[Row], query: &str) -> Vec<usize> {
    rows.iter()
        .enumerate()
        .filter_map(|(i, r)| matches(r.message.as_ref(), query).then_some(i))
        .collect()
}

/// The one matcher, where the rows live: a query matches when the row's
/// text contains it, folded — exactly what the commit list's search does.
fn matches(haystack: &str, needle: &str) -> bool {
    haystack.to_lowercase().contains(&needle.to_lowercase())
}

/// Dropping confirms **in this pane**, by the same mechanics as a file
/// discard: the first press stores [`Stashes::armed`] and asks through the
/// notice band; the second press on the same row executes; any cursor move,
/// wheel or refresh drops the arm. Outliving a switch to another pane and
/// back is deliberate — the question sits on the row it was asked about.
pub struct Stashes {
    data: Rc<Vec<Row>>,
    /// Rows the list draws right now: indices into `data`, ascending.
    /// Identity until a query filters it, rebuilt only when the query
    /// changes — never per frame — which is why the full vector is kept
    /// and this is all the list draws from.
    visible: Rc<Vec<usize>>,
    /// The live filter, as the prompt last left it. `None` — or an empty
    /// string, which [`Stashes::apply_query`] folds into `None` — is
    /// every row; kept so a second `/` edits the query rather than
    /// starting over, and so clearing restores in one keystroke.
    filter: Option<String>,
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
    /// The row a right-click landed on, published for the shell — which opens
    /// the pane's context menu over it. Taken once by whoever opens it: one
    /// right-click, one open.
    menu_row: Cell<Option<usize>>,
}

impl Stashes {
    /// The viewport model with everything live folded in — see
    /// [`super::files::Files::live_view`].
    fn live_view(&self, host: &Host) -> Viewport {
        let mut v = self.view.get();
        v.set_len(self.visible.len());
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
        let visible = Rc::new(Vec::from_iter(0..rows.len()));
        Self {
            data: Rc::new(rows),
            visible,
            filter: None,
            scroll: UniformListScrollHandle::new(),
            view: Rc::new(Cell::new(Viewport::new())),
            synced: Rc::new(Cell::new(0.0)),
            pending_scroll: PendingScroll::default(),
            rendered: Rc::new(Cell::new(0)),
            armed: None,
            focused: false,
            menu_row: Cell::new(None),
        }
    }

    /// How many rows the list draws — the shown ones, a query having
    /// narrowed the stack without replacing it. What sizes this pane's
    /// sidebar section.
    pub fn rows(&self) -> usize {
        self.visible.len()
    }

    /// The live query, for pre-filling an edit of it. Empty means none.
    pub fn query(&self) -> Option<&str> {
        self.filter.as_deref()
    }

    /// What the pane's label appends while filtered: shown over loaded —
    /// `"1/3"`. `None` unfiltered — the header then stays exactly what
    /// acquisition named it.
    pub fn filter_note(&self) -> Option<String> {
        self.filter
            .is_some()
            .then(|| format!("{}/{}", self.visible.len(), self.data.len()))
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
        // The cursor addresses the *shown* rows, so the anchor is read
        // through the visible index.
        let anchored = self
            .visible
            .get(old.cursor())
            .and_then(|&d| self.data.get(d))
            .map(|r| r.commit.clone());
        let Prepared { rows, .. } = prepared;
        self.data = Rc::new(rows);
        // The new rows under the *current* query — a refresh must not drop
        // the filter the user is looking through, and the anchor below is
        // found in this space, not in the full list's.
        self.visible = Rc::new(match &self.filter {
            Some(q) => search_rows(&self.data, q),
            None => Vec::from_iter(0..self.data.len()),
        });

        let cursor = anchored
            .and_then(|commit| {
                self.visible
                    .iter()
                    .position(|&d| self.data[d].commit == commit)
            })
            .unwrap_or_else(|| old.cursor().min(self.visible.len().saturating_sub(1)));
        let mut view = old;
        view.set_len(self.visible.len());
        view.go_to(cursor);
        self.view.set(view);

        if self.visible.is_empty() {
            self.pending_scroll.cancel();
            let mut state = self.scroll.0.borrow_mut();
            state.deferred_scroll_to_item = None;
            state.base_handle.set_offset(point(px(0.0), px(0.0)));
            self.synced.set(0.0);
        } else {
            self.defer_show(view);
        }
    }

    /// Sets the filter — once per keystroke, and never anywhere else. The
    /// visible-index vec is rebuilt here and read everywhere else.
    ///
    /// The keyboard stays on the same entry it was on: anchored by the row's
    /// commit into the next result set wherever that entry survives the
    /// narrower query, clamped to a neighbouring row when it does not. An
    /// empty query (a trimmed-empty one too) is no query: identity indices,
    /// so clearing restores exactly what was on screen before.
    ///
    /// A strict deferred scroll parks the viewport the way every other jump
    /// does — the list's geometry still describes the previous length until
    /// the next prepaint, and writing an offset against it would clamp in
    /// the wrong place.
    pub fn apply_query(&mut self, query: &str) {
        let next = Some(query.trim()).filter(|q| !q.is_empty());
        if self.filter.as_deref() == next {
            return;
        }
        // A changed filter can move the cursor by clamping, and a question
        // aimed at yesterday's row is the thing the arm exists to prevent.
        self.armed = None;
        // Anchor first, named by the row's commit like every other re-anchor
        // in this file, because row numbers are about to stop meaning
        // anything.
        let anchored = self
            .visible
            .get(self.view.get().cursor())
            .and_then(|&d| self.data.get(d))
            .map(|r| r.commit.clone());

        self.filter = next.map(str::to_string);
        self.visible = Rc::new(match &self.filter {
            Some(q) => search_rows(&self.data, q),
            None => Vec::from_iter(0..self.data.len()),
        });

        let mut view = self.view.get();
        let cursor = anchored
            .as_ref()
            .and_then(|commit| {
                self.visible
                    .iter()
                    .position(|&d| &self.data[d].commit == commit)
            })
            .unwrap_or_else(|| view.cursor().min(self.visible.len().saturating_sub(1)));
        view.set_len(self.visible.len());
        view.go_to(cursor);
        self.view.set(view);
        if self.visible.is_empty() {
            // Nothing survives the query; park nothing and leave no stale
            // offset for a later keystroke to reconcile against.
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

    /// Nothing off the left edge to reach — a squeezed message ends in an
    /// ellipsis rather than pan. Present so the wheel routing can offer the
    /// axis to every screen alike.
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

    /// Where a click lands the keyboard: onto the row the mouse hit, with
    /// exactly the side effects a key move has — see [`Self::run_view`]. The
    /// row clamps like [`Viewport::go_to`] does; this list has no headings.
    /// The row a right-click landed on, published by the row's own handler
    /// beside the left-click one, and taken once by the shell — which opens
    /// the pane's context menu over it. Taken, not read: one right-click,
    /// one open.
    pub fn take_menu_row(&self) -> Option<usize> {
        self.menu_row.take()
    }

    pub fn select_row(&mut self, index: usize, host: &Host) {
        self.reconcile(host);
        let mut v = self.live_view(host);
        v.go_to(index);
        // The mouse moved — whatever was armed was armed to what the mouse
        // used to be on.
        self.armed = None;
        self.view.set(v);
        self.show(v);
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
        self.data.get(*self.visible.get(self.view.get().cursor())?)
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
        // An empty stack is a quiet line, not an empty box —
        // [`chrome::empty_line`], the compact twin of the files pane's
        // clean-tree sentence.
        if self.is_empty() {
            return empty_line(&crate::config::host(cx), "nothing stashed".into());
        }

        let data = self.data.clone();
        let visible = self.visible.clone();
        let rendered = self.rendered.clone();
        let view = self.view.clone();
        let scroll = self.scroll.clone();
        let synced = self.synced.clone();
        let pending_scroll = self.pending_scroll.clone();
        let armed = self.armed;
        let focused = self.focused;
        // A click on a row is the keyboard coming back — see [`Self::select_row`].
        // Built as a plain handle, not `cx.listener`: the rows are drawn in the
        // list's closure over `&mut App`, where no listener can be minted.
        let this = cx.entity().downgrade();
        let list = uniform_list("stashes", visible.len(), move |range, _, cx| {
            rendered.set(range.len());
            let host = crate::config::host(cx);
            if let Some(accepted) = accept_deferred_scroll(&scroll, &pending_scroll, &synced) {
                if accepted.wheeled {
                    let mut v = view.get();
                    v.set_len(visible.len());
                    v.set_height(range.len());
                    v.set_scrolloff(host.view.scrolloff);
                    v.scroll_to((-accepted.y / ROW_H).round().max(0.0) as usize);
                    view.set(v);
                    cx.refresh_windows();
                }
            }
            let cursor = view.get().cursor();
            range
                .map(|i| {
                    let this = this.clone();
                    row(
                        &data[visible[i]],
                        &host,
                        i == cursor,
                        focused,
                        Some(i) == armed,
                    )
                    .id(("row", i))
                    // Hover says clickable — but the selection tint outranks it,
                    // so the cursor row keeps its own background. `hover` needs
                    // identity, so it rides this id (plan 045); `chrome::list_row`
                    // has none.
                    .cursor_pointer()
                    .when(i != cursor, |r| {
                        r.hover(|s| s.bg(rgb(host.theme.chrome.fg).alpha(0.03)))
                    })
                    .on_mouse_down(MouseButton::Right, {
                        let this = this.clone();
                        move |_: &MouseDownEvent, _, cx| {
                            let Some(this) = this.upgrade() else { return };
                            let host = crate::config::host(cx);
                            this.update(cx, |s, cx| {
                                s.select_row(i, &host);
                                s.menu_row.set(Some(i));
                                cx.notify();
                            });
                        }
                    })
                    .on_mouse_down(MouseButton::Left, move |_: &MouseDownEvent, _, cx| {
                        let Some(this) = this.upgrade() else { return };
                        let host = crate::config::host(cx);
                        this.update(cx, |s, cx| {
                            s.select_row(i, &host);
                            cx.notify();
                        });
                    })
                    .into_any_element()
                })
                .collect()
        })
        .track_scroll(&self.scroll)
        .size_full();

        div()
            .relative()
            .size_full()
            .child(list)
            .when(crate::config::host(cx).view.scrollbar, |d| {
                d.child(vertical_scrollbar(&DeferredScrollbar::new(
                    &self.scroll,
                    &self.pending_scroll,
                )))
            })
            .into_any_element()
    }
}

/// One row: the address dim — furniture, not content — then the message in
/// the pane's own ink. The frame is [`list_row`]'s: selection tint, the bar
/// on the left edge in accent while this pane is `focused`. `armed` turns the
/// whole row toward `chrome.error`, the colour the second press spends.
fn row(e: &Row, host: &Host, current: bool, focused: bool, armed: bool) -> Div {
    let ch = host.font.char_width();
    let c = host.theme.chrome;
    list_row(host, current, focused, ROW_H)
        .child(
            div()
                .flex_none()
                .text_color(rgb(match armed {
                    true => c.error,
                    false => host.theme.dim_on(if current {
                        theme::Surface::Cursor
                    } else {
                        theme::Surface::Context
                    }),
                }))
                .child(e.title.clone()),
        )
        .child(
            div()
                .ml(px(GAP_CHARS * ch))
                // The one thing that gives: `min_w_0` and `truncate` let a long
                // message end in an ellipsis rather than shove itself out of
                // the pane — the rule the branches pane's names already run.
                // The address beside it is `flex_none` and never moves.
                .min_w_0()
                .truncate()
                .text_color(rgb(match armed {
                    true => c.error,
                    false => c.fg,
                }))
                .child(e.message.clone()),
        )
}

#[cfg(test)]
mod tests {
    // By name, not a glob: `use gpui::*` in the parent shadows `#[test]` with
    // GPUI's own attribute macro and every test in here fails to expand.
    use super::{flatten, prepare, title, Stash};
    use gitten_core::host::Host;
    use std::rc::Rc;

    #[test]
    fn a_query_filters_live_and_the_keyboard_stays_on_its_entry() {
        let _host = Host::new();
        let mut f = pane(&sample());
        with_height(&mut f, 4);
        // The keyboard opens on stash@{0} — a message that survives "HAND",
        // folded, while "WIP on main" does not.
        let anchored = f
            .current()
            .expect("a row under the keyboard")
            .commit
            .clone();

        f.apply_query("  hand  ");
        let v = f.view.get();
        assert_eq!(v.len(), 1, "one message matches, folded");
        assert_eq!(f.filter_note().as_deref(), Some("1/2"));
        assert_eq!(f.rows(), 1);
        // Through the indirection: `current` is the anchored entry, not
        // whatever now happens to sit at row 0 of a shorter list — and the
        // stack's addresses stay git's, so the match is still stash@{0}.
        assert_eq!(
            f.current().map(|r| r.commit.clone()).as_ref(),
            Some(&anchored)
        );
        assert_eq!(f.current().map(|r| r.index), Some(0));

        // The same query again is no change at all — and rebuilds nothing.
        let before = Rc::as_ptr(&f.visible);
        f.apply_query("hand ");
        assert_eq!(f.query(), Some("hand"), "trimmed before comparing");
        assert_eq!(
            Rc::as_ptr(&f.visible),
            before,
            "a no-op query did not rebuild the index"
        );

        // A changed filter is a movement of attention: the keyboard clamps
        // into what survives, and an armed drop dies with the question's row.
        assert!(!f.confirm_or_arm_drop(0));
        f.apply_query("nothing matches this");
        let v = f.view.get();
        assert_eq!(v.len(), 0);
        assert!(f.current().is_none());
        assert_eq!(f.filter_note().as_deref(), Some("0/2"));
        assert_eq!(f.armed_row(), None, "a changed query disarmed");

        // Clearing puts every entry back under the same commit.
        f.apply_query("");
        assert_eq!(f.rows(), 2);
        assert_eq!(f.query(), None);
        assert_eq!(f.filter_note(), None);
        assert_eq!(
            f.current().map(|r| r.commit.clone()).as_ref(),
            Some(&anchored),
            "empty restores instantly, cursor included"
        );
    }

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

    #[test]
    fn a_click_moves_the_cursor_to_the_row_it_hit_and_disarms_a_drop() {
        let host = Host::new();
        let mut f = pane(&sample());
        with_height(&mut f, 4);

        // A click is a place: the keyboard lands on the row the mouse hit.
        f.select_row(1, &host);
        assert_eq!(
            f.view.get().cursor(),
            1,
            "the keyboard is on the clicked row"
        );

        // And the click is a cursor move like any other: an armed drop dies.
        assert!(!f.confirm_or_arm_drop(1));
        assert_eq!(f.armed_row(), Some(1));
        f.select_row(0, &host);
        assert_eq!(f.armed_row(), None, "the click disarms the question");
    }
}
