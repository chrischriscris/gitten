use super::{accept_deferred_scroll, DeferredScrollbar, PendingScroll};
use crate::chrome;
use crate::graph;
use gitten_core::host::Host;
use gitten_core::search;
use gitten_core::view::Viewport;
use gitten_core::{assign_lanes, initials, Commit};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::scroll::Scrollbar;
use std::cell::Cell;
use std::rc::Rc;

/// The commit column between the sha and the graph, resolved once at load: two
/// letters. Not the colour: that follows the live theme like everything else
/// on the row.
struct Who {
    initials: SharedString,
}

struct Data {
    commits: Vec<Commit>,
    draws: Vec<graph::Draw>,
    /// The age column, one string per commit and solved once per load:
    /// `3h`, `2d`, `6mo`. Clock work belongs to load work twice over — a
    /// frame would reformat all 82k answers for pixels that did not ask,
    /// and a clock read per frame disagrees with itself row to row where a
    /// load-time answer reads as one timestamp. See [`rel_time`] for the
    /// bands.
    times: Vec<SharedString>,
    /// One line per commit, the way `copy.selection` copies it — the sha and the
    /// subject, and neither the graph nor the clock beside them. Built at load
    /// with everything else that is derived once.
    lines: Vec<String>,
    /// The two-letter author initials per commit, solved once at load — see
    /// [`Who`]. The colour beside them reads the theme per frame on purpose.
    who: Vec<Who>,
    /// The same list folded for substring search — see
    /// [`gitten_core::search::Index`] for why this is load work and not
    /// keystroke work.
    search: search::Index,
}

/// Repository data with every view-independent graph and row derivation already
/// completed. Pane refresh builds this on the background executor; applying it
/// only restores semantic viewport anchors and swaps one `Rc`.
pub(crate) struct Prepared {
    data: Data,
    load: String,
}

pub struct Commits {
    data: Rc<Data>,
    /// Row → index into `data.commits`, ascending. Identity until a query
    /// filters it, rebuilt only when the query changes — never per frame —
    /// which is why the full vector is kept and this is all the list draws
    /// from.
    visible: Rc<Vec<usize>>,
    /// The live filter, as the prompt last left it. `None` — or an empty
    /// string, which [`Commits::apply_query`] folds into `None` — is every
    /// row; kept so a second `/` edits the query rather than starting over,
    /// and so clearing restores in one keystroke.
    query: Option<String>,
    scroll: UniformListScrollHandle,
    /// The cursor, the top row and the height — [`gitten_core::view::Viewport`],
    /// the same model the terminal's commit list holds. Behind a shared cell so
    /// the render closure can read which row is the cursor's without a second
    /// source of truth.
    view: Rc<Cell<Viewport>>,
    /// The vertical offset this view last wrote — see the diff view's note.
    synced: Rc<Cell<f32>>,
    /// A strict row waiting for prepaint, plus exact wheel pixels meanwhile.
    pending_scroll: PendingScroll,
    /// Instrumentation the view owns and anyone may read. The view does not
    /// know the stats overlay exists.
    pub rendered: Rc<Cell<usize>>,
    /// First visible row, for the session — see the note in the diff view.
    pub top: Rc<Cell<usize>>,
    pub load: String,
    /// The hard reset awaiting its second press: the sha of the commit that
    /// asked. One slot — arming a different row moves the question. Any
    /// cursor move, wheel or refresh drops it, because a stale yes waiting
    /// on a sha the list may no longer hold is exactly the accident the
    /// double press exists to prevent.
    armed: Option<String>,
    /// Whether this pane holds the keyboard, as the shell last told it. A
    /// row's bar is accent only when its pane is focused, and the view cannot
    /// ask the shell during render — so the shell writes it here when focus
    /// moves, and render reads a flag.
    focused: bool,
}

impl Commits {
    /// Told by the shell whenever the keyboard moves — never decided here.
    pub fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }

    /// Whether this pane holds the keyboard. The rows read it for the bar.
    #[allow(dead_code)]
    pub fn focused(&self) -> bool {
        self.focused
    }

    /// The viewport model with everything live folded in: the list's length,
    /// the height last measured, and `[view] scrolloff` as the file has it
    /// *now* — see the diff view's `live_view`.
    fn live_view(&self, host: &Host) -> Viewport {
        let mut v = self.view.get();
        v.set_len(self.visible.len());
        v.set_height(self.rendered.get());
        v.set_scrolloff(host.view.scrolloff);
        v
    }

    /// Puts a saved row back at the top of the viewport. Clamped — see the
    /// diff view's note: the model is filled in first, because a restore lands
    /// on a view that has never been laid out and must not clamp a saved row
    /// against a list it believes is empty.
    ///
    /// Strict, like the diff view's: the non-strict strategy skips a row that
    /// is already inside the initial viewport, which is exactly where a saved
    /// row near the top of the graph lands — GPUI would stay at row zero while
    /// everything else claims the restore worked.
    pub fn scroll_to(&self, row: usize, host: &Host) {
        if self.visible.is_empty() {
            return;
        }
        let row = row.min(self.visible.len() - 1);
        let mut v = self.live_view(host);
        v.scroll_to(row);
        self.view.set(v);
        self.defer_show(v);
    }

    /// Every commit loaded — the filter narrows what is *shown*, never what
    /// the repository holds. What the stats overlay calls total.
    pub fn total(&self) -> usize {
        self.data.commits.len()
    }

    /// Rows the list draws right now: [`Commits::total`] until a query
    /// narrows it, and what every viewport number addresses.
    ///
    /// Read by tests and, one day, by a second client's status line; nothing
    /// in this window needs it yet.
    #[allow(dead_code)]
    pub fn rows(&self) -> usize {
        self.visible.len()
    }

    /// The commit under the keyboard, for whatever opens a diff from it.
    ///
    /// Through the indirection and not `data.commits[cursor]`: the cursor is
    /// a row of the *visible* list, and under a query those are not the same
    /// vector position. Everything that acts on "this commit" — open-diff,
    /// copy — reads through here, which is why filtering cannot desync them.
    pub fn current(&self) -> Option<&Commit> {
        self.data
            .commits
            .get(*self.visible.get(self.view.get().cursor())?)
    }

    /// The live query, for pre-filling an edit of it. Empty means none.
    pub fn query(&self) -> Option<&str> {
        self.query.as_deref()
    }

    /// What the pane's label appends while filtered: shown over loaded,
    /// `"12/4173"`. `None` unfiltered — the label then stays exactly what
    /// acquisition named it.
    pub fn filter_note(&self) -> Option<String> {
        self.query
            .is_some()
            .then(|| format!("{}/{}", self.visible.len(), self.data.commits.len()))
    }

    // -------------------------------------------------------------- commands

    /// The box the list is drawn in, for hit-testing a wheel event.
    pub fn list_bounds(&self) -> Bounds<Pixels> {
        self.scroll.0.borrow().base_handle.bounds()
    }

    /// A commit list has nothing off the left edge to reach; the terminal says
    /// the same by ignoring `view.left` and `view.right`. Present so the shell's
    /// wheel routing can offer the axis to every screen alike.
    pub fn pan_pixels(&self, _dx: f32) -> bool {
        false
    }

    /// Moves the list by `dy` pixels — the wheel, whose command resolves through
    /// `[keys]` but whose delta is pixels. The cursor comes along when pushed
    /// off screen, exactly as [`Viewport::scroll_by`] does in the terminal.
    pub fn scroll_pixels(&mut self, dy: f32, host: &Host) -> bool {
        let deferred = self.scroll.0.borrow().deferred_scroll_to_item;
        if let Some(request) = deferred {
            if self.pending_scroll.is_awaiting() {
                let pixels = self.pending_scroll.wheel(dy);
                let mut v = self.live_view(host);
                let y = -(request.item_index as f32 * graph::ROW_H) + pixels;
                v.scroll_to((-y / graph::ROW_H).round().max(0.0) as usize);
                self.view.set(v);
                self.top.set(v.top());
                // The wheel is also a move of attention — same rule the
                // arrow keys keep.
                self.armed = None;
                return true;
            }
            // A request not paired with our marker belongs to another
            // interaction. The newer wheel wins rather than donating pixels to
            // a request `accept_deferred_scroll` will deliberately ignore.
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
        v.scroll_to((-y / graph::ROW_H).round().max(0.0) as usize);
        self.view.set(v);
        self.synced.set(y);
        self.armed = None;
        true
    }

    /// Meets the list where it actually is: a scrollbar drag moves the offset
    /// without touching anything else, and the next key should act on what is on
    /// screen now. [`Commits::synced`] separates "the list moved under us" from
    /// "we moved the list" — see the diff view's `reconcile`.
    pub fn reconcile(&mut self, host: &Host) {
        if self.scroll.0.borrow().deferred_scroll_to_item.is_some() {
            return;
        }
        let shown_y = f32::from(self.scroll.0.borrow().base_handle.offset().y);
        if (shown_y - self.synced.get()).abs() < 0.5 {
            return;
        }
        self.synced.set(shown_y);
        let shown = (-shown_y / graph::ROW_H).round().max(0.0) as usize;
        let mut v = self.live_view(host);
        if v.top() == shown {
            return;
        }
        v.scroll_to(shown);
        self.view.set(v);
    }

    /// Runs one of the `view.*` commands. The same names the terminal
    /// dispatches, onto the same [`Viewport`] arithmetic — a key scrolls every
    /// list this app has, which is what makes them bindable in `GLOBAL`.
    ///
    /// False is "not one of mine", and the caller says so: an unknown command
    /// that resolves is worth naming rather than swallowing.
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
            // No sideways half: see `pan_pixels`. Answered without moving
            // anything, so an armed reset stands.
            "view.left" | "view.right" => return true,
            _ => return false,
        }
        // The keyboard moved — including the two scrolls above, which leave
        // the cursor but not the question's row in view. Whatever was armed
        // was armed to what the keyboard used to be on.
        self.armed = None;
        self.view.set(v);
        self.show(v);
        true
    }

    /// Puts row `v.top()` at the top of the viewport, exactly. If the list still
    /// has a deferred position, its geometry is not current yet; replace that
    /// target instead of clamping against stale bounds.
    fn show(&self, v: Viewport) {
        let target = v.top();
        if self.scroll.0.borrow().deferred_scroll_to_item.is_some() {
            self.defer_show(v);
            return;
        }
        let s = self.scroll.0.borrow();
        let cur = s.base_handle.offset();
        let y = -(target as f32 * graph::ROW_H).clamp(0.0, f32::from(s.base_handle.max_offset().y));
        s.base_handle.set_offset(point(cur.x, px(y)));
        self.synced.set(y);
        self.top.set(target);
    }

    fn defer_show(&self, v: Viewport) {
        let target = v.top();
        self.pending_scroll.begin();
        self.scroll
            .scroll_to_item_strict(target, ScrollStrategy::Top);
        self.top.set(target);
    }

    /// What `copy.selection` copies here: the dragged range or, until this view
    /// grows a drag of its own, the commit the keyboard is on — sha and subject,
    /// the two fields that name the commit to git and to a person. Through the
    /// indirection, like [`Commits::current`].
    pub fn cursor_text(&self) -> String {
        let v = self.view.get();
        self.visible
            .get(v.cursor())
            .and_then(|i| self.data.lines.get(*i))
            .cloned()
            .unwrap_or_default()
    }

    /// Whether this view took part in `select.all` / `select.none`. It did not:
    /// there is no selection model over a commit graph yet, and a command that
    /// does nothing here is said, not swallowed.
    pub fn select_all(&mut self) -> bool {
        false
    }

    pub fn select_none(&mut self) -> bool {
        false
    }

    /// The hard reset's two-press dance, shared with every destructive verb:
    /// first press on a row arms it and asks through the notice band; the
    /// second press on the *same* commit executes. True means the press was
    /// the second one and the arm is spent. Addressed by sha — the thing
    /// `current` hands the shell and git will be aimed at.
    pub(crate) fn confirm_or_arm_reset(&mut self, sha: &str) -> bool {
        self.arm(sha)
    }

    /// The same dance for the history rewrites — squash-up, fixup-up,
    /// drop-commit — over the one arm slot a pane holds. Arming anything
    /// else moves the question rather than queueing a second one.
    pub(crate) fn confirm_or_arm_rewrite(&mut self, sha: &str) -> bool {
        self.arm(sha)
    }

    fn arm(&mut self, sha: &str) -> bool {
        let already = self.armed.as_deref() == Some(sha);
        self.armed = match already {
            true => None,
            false => Some(sha.to_string()),
        };
        already
    }

    /// Whether a reset is waiting for its second press — the render's tint of
    /// the row the question is about.
    #[cfg(test)]
    pub(crate) fn armed_sha(&self) -> Option<String> {
        self.armed.clone()
    }

    // ----------------------------------------------------------------- search

    /// Sets the filter — once per keystroke, and never anywhere else. The
    /// visible-index vec is rebuilt here and read everywhere else.
    ///
    /// The keyboard stays on the same commit it was on: anchored by sha into
    /// the next result set wherever that commit survives the narrower query,
    /// clamped to a neighbouring row when it does not. An empty query (a
    /// trimmed-empty one too) is no query: identity indices, so clearing
    /// restores exactly what was on screen before.
    ///
    /// A strict deferred scroll parks the viewport the way every other jump
    /// does — the list's geometry still describes the previous length until
    /// the next prepaint, and writing an offset against it would clamp in the
    /// wrong place.
    pub fn apply_query(&mut self, query: &str) {
        let next = Some(query.trim()).filter(|q| !q.is_empty());
        if self.query.as_deref() == next {
            return;
        }
        // A changed filter can move the cursor by clamping, and a question
        // aimed at yesterday's row is the thing the arm exists to prevent.
        self.armed = None;
        // Anchor first: named by sha, like every other re-anchor in this file,
        // because row numbers are about to stop meaning anything.
        let anchored = self
            .visible
            .get(self.view.get().cursor())
            .and_then(|i| self.data.commits.get(*i))
            .map(|c| c.sha.clone());

        self.query = next.map(str::to_string);
        self.visible = Rc::new(match &self.query {
            Some(q) => self.data.search.indices(q),
            None => Vec::from_iter(0..self.data.commits.len()),
        });

        let mut v = self.view.get();
        let cursor = anchored
            .as_deref()
            .and_then(|sha| {
                self.visible
                    .iter()
                    .position(|i| self.data.commits[*i].sha == sha)
            })
            .unwrap_or_else(|| v.cursor().min(self.visible.len().saturating_sub(1)));
        v.set_len(self.visible.len());
        v.go_to(cursor);
        self.view.set(v);
        if self.visible.is_empty() {
            // Nothing survives the query; park nothing and leave no stale
            // offset for a later keystroke to reconcile against.
            self.pending_scroll.cancel();
            let mut state = self.scroll.0.borrow_mut();
            state.deferred_scroll_to_item = None;
            state.base_handle.set_offset(point(px(0.0), px(0.0)));
            self.synced.set(0.0);
            self.top.set(0);
        } else {
            self.defer_show(v);
        }
    }
}

/// One row's reach, roughly: the graph gutter plus the subject. An estimate,
/// because this number only decides which single row `uniform_list` measures
/// to learn the true scrollable width — see the note in `Data`.
///
/// Characters, never `.len()`: a byte-lengthed CJK subject counted itself
/// three times too wide and could dethrone a genuinely wider ASCII row,
/// leaving that row clipped past the last reachable column forever.
///
impl Commits {
    pub fn new(commits: Vec<Commit>, host: Rc<Host>) -> Self {
        let Prepared { data, load } = prepare(commits, &host);
        Self {
            visible: Rc::new(Vec::from_iter(0..data.commits.len())),
            query: None,
            data: Rc::new(data),
            scroll: UniformListScrollHandle::new(),
            view: Rc::new(Cell::new(Viewport::new())),
            synced: Rc::new(Cell::new(0.0)),
            pending_scroll: PendingScroll::default(),
            rendered: Rc::new(Cell::new(0)),
            top: Rc::new(Cell::new(0)),
            load,
            armed: None,
            focused: false,
        }
    }

    /// Replaces repository data while keeping semantic commit anchors.
    #[cfg(test)]
    pub fn replace(&mut self, commits: Vec<Commit>, host: &Host) {
        let prepared = prepare(commits, host);
        self.replace_prepared(prepared, host);
    }

    pub(crate) fn replace_prepared(&mut self, prepared: Prepared, host: &Host) {
        // A refresh is the repository saying things moved; an armed reset was
        // a promise about how they were, so it dies here first.
        self.armed = None;
        self.reconcile(host);
        let old = self.view.get();
        let cursor_sha = self
            .visible
            .get(old.cursor())
            .and_then(|i| self.data.commits.get(*i))
            .map(|c| c.sha.clone());
        let top_sha = self
            .visible
            .get(old.top())
            .and_then(|i| self.data.commits.get(*i))
            .map(|c| c.sha.clone());
        let old_cursor = old.cursor();
        let old_top = old.top();
        let Prepared { data, load } = prepared;
        // The new rows under the *current* query — a refresh must not drop the
        // filter the user is looking through, and the anchors below are found
        // in this space, not in the full list's.
        let visible = Rc::new(match &self.query {
            Some(q) => data.search.indices(q),
            None => Vec::from_iter(0..data.commits.len()),
        });
        let find = |sha: &str| visible.iter().position(|i| data.commits[*i].sha == sha);
        let cursor = cursor_sha
            .as_deref()
            .and_then(find)
            .unwrap_or_else(|| old_cursor.min(visible.len().saturating_sub(1)));
        let top = top_sha
            .as_deref()
            .and_then(find)
            .unwrap_or_else(|| old_top.min(visible.len().saturating_sub(1)));
        self.data = Rc::new(data);
        self.visible = visible;
        self.load = load;

        let mut view = old;
        view.set_len(self.visible.len());
        view.scroll_to(top);
        view.go_to(cursor);
        self.view.set(view);
        if self.visible.is_empty() {
            self.pending_scroll.cancel();
            let mut state = self.scroll.0.borrow_mut();
            state.deferred_scroll_to_item = None;
            state.base_handle.set_offset(point(px(0.0), px(0.0)));
            self.synced.set(0.0);
            self.top.set(0);
        } else {
            self.defer_show(view);
        }
    }

    /// Puts the keyboard on a row, as a restore does: the row at the top of the
    /// viewport when you left it, which is where the cursor belongs too.
    pub fn go_to(&self, row: usize, host: &Host) {
        let mut v = self.live_view(host);
        v.go_to(row);
        self.view.set(v);
    }
}

/// The host rides along for the day a load-time derivation reads the font or
/// the theme again; nothing does today, since the widest-row measurement
/// went with the sideways scroll.
pub(crate) fn prepare(commits: Vec<Commit>, _host: &Host) -> Prepared {
    let t = std::time::Instant::now();
    let rows = assign_lanes(&commits);
    let t_lanes = t.elapsed();

    let t = std::time::Instant::now();
    let draws = graph::row_draws(&commits, &rows);
    let lanes = graph::lane_count(&rows);
    let t_draws = t.elapsed();

    // One clock read per load and one [`rel_time`] per commit. The answer is
    // as stale as any snapshot of the log is — the next refresh recomputes it
    // here, the same pass that recomputes everything else — and never on a
    // frame.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let times: Vec<SharedString> = commits
        .iter()
        .map(|c| rel_time(c.timestamp, now).into())
        .collect();

    let load = format!(
        "{} commits · {} lanes · lanes {:.0?} draws {:.0?}",
        commits.len(),
        lanes,
        t_lanes,
        t_draws
    );
    eprintln!("{load}");

    Prepared {
        data: Data {
            lines: commits
                .iter()
                .map(|c| format!("{} {}", c.short, c.subject))
                .collect(),
            search: search::Index::new(&commits),
            who: commits
                .iter()
                .map(|c| Who {
                    initials: initials(&c.author).into(),
                })
                .collect(),
            commits,
            draws,
            times,
        },
        load,
    }
}

impl Render for Commits {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let data = self.data.clone();
        let rendered = self.rendered.clone();
        let top = self.top.clone();
        let view = self.view.clone();
        let scroll = self.scroll.clone();
        let synced = self.synced.clone();
        let pending_scroll = self.pending_scroll.clone();
        // Read per batch, not captured at construction — see the note in the
        // diff view: this is what makes a saved config apply on the next frame.
        //
        // The list's length is the *visible* length, and every row goes through
        // the indirection before it touches a commit: under a query, row
        // numbers are positions in `visible`, not in `data.commits`.
        let visible = self.visible.clone();
        // The row an armed reset is waiting on, found once per frame — the
        // same tint a discard and a drop wear.
        let armed = self
            .armed
            .as_ref()
            .and_then(|sha| visible.iter().position(|i| data.commits[*i].sha == *sha));
        let focused = self.focused;
        let list = uniform_list("commits", visible.len(), move |range, _, cx| {
            rendered.set(range.len());
            top.set(range.start);
            let host = crate::config::host(cx);
            if let Some(accepted) = accept_deferred_scroll(&scroll, &pending_scroll, &synced) {
                if accepted.wheeled {
                    let mut v = view.get();
                    v.set_len(visible.len());
                    v.set_height(range.len());
                    v.set_scrolloff(host.view.scrolloff);
                    v.scroll_to((-accepted.y / graph::ROW_H).round().max(0.0) as usize);
                    view.set(v);
                    top.set(v.top());
                    cx.refresh_windows();
                }
            }
            let cursor = view.get().cursor();
            range
                .map(|i| {
                    // `visible` is ascending into the full vec, so the arrays
                    // derived at load index directly by it.
                    let c = visible[i];
                    row(c, &data, &host, i == cursor, focused, Some(i) == armed)
                })
                .collect()
        })
        // Rows are exactly the viewport's width — no `Unconstrained` sizing
        // and no widest-row measurement. The column is a list, not a diff: a
        // subject that does not fit ends in an ellipsis, and the clock at the
        // right edge is always on screen. No padding on the list either:
        // `ROW_PAD` is the row's own, so the cursor bar sits on the region's
        // edge.
        .track_scroll(&self.scroll)
        .size_full();

        // The scrollbar overlays the list, so the container must be positioned.
        // `[view] scrollbar` is read per frame like every other setting: the
        // terminal draws its own bar from the same flag, and a knob that means
        // two things in two clients is a knob nobody trusts.
        let bars = crate::config::host(cx).view.scrollbar;
        div().relative().size_full().child(list).when(bars, |d| {
            d.child(Scrollbar::vertical(&DeferredScrollbar::new(
                &self.scroll,
                &self.pending_scroll,
            )))
        })
    }
}

/// Compact relative age, the way a list glances at it: `now`, `5m`, `3h`,
/// `2d`, `4w`, `6mo`, `1y`.
///
/// Pure on purpose: the caller owns what "now" is, and here that caller is
/// [`prepare`], which reads the clock once per load — a read per row would
/// disagree with itself down the list. A timestamp in the future still reads
/// as `now`: a committer's clock running ahead of ours must not print a minus
/// sign into a column meant to be glanced at. The bands are what the eye
/// already groups — minutes until they stop mattering, hours until a day
/// starts it, days until a week does, then thirty to the month and three
/// hundred sixty-five to the year, each folded with integer division because
/// rough is the point: the alternative is a calendar.
fn rel_time(timestamp: i64, now: i64) -> String {
    let secs = now - timestamp;
    if secs < 60 {
        return "now".to_string();
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{mins}m");
    }
    let hours = mins / 60;
    if hours < 24 {
        return format!("{hours}h");
    }
    let days = hours / 24;
    if days < 7 {
        return format!("{days}d");
    }
    if days < 30 {
        return format!("{}w", days / 7);
    }
    if days < 365 {
        return format!("{}mo", days / 30);
    }
    format!("{}y", days / 365)
}

/// The sha column, in *characters*: twelve, because `%h` is seven in a young
/// repository and eleven in git/git, plus the air after it. In pixels rather
/// than characters this was 90, which is 10.7 in the shipped face — so an
/// eleven-character sha overflowed its own column by two pixels while the
/// comment above it said eleven. Fixed, unlike the graph: the eye scans it
/// vertically, so it has to *be* a column.
const SHA_CHARS: f32 = 12.0;
/// Two letters of initials beside the sha — see [`Who`].
const WHO_CHARS: f32 = 3.0;

/// The clock column, in *characters*: four covers the whole vocabulary —
/// thirteen months still labels itself at most `12mo` — and right-alignment
/// within it keeps a column of ages running straight down the screen instead
/// of trailing their subjects' ragged ends. Fixed like the sha, for the same
/// reason.
const TIME_CHARS: f32 = 4.0;

/// The mock's order — graph, sha, subject, clock — with lazygit's spacing kept
/// where it earned its place: the subject follows its own row's graph
/// immediately, so a commit on the trunk reads from the left instead of
/// starting behind the widest merge in the repository.
///
/// On [`chrome::list_row`]'s furniture, so the cursor bar sits left of the
/// graph like every other list's. The subject is the one thing that gives:
/// `min_w_0` and `truncate` end it in an ellipsis, so the clock — a fixed
/// four-character cell, right-aligned inside it and pushed to the row's
/// edge by `ml_auto` — is always on screen and the ages sit in one vertical
/// line no matter where their subjects stop.
///
/// `current` is the keyboard's row and `focused` picks its bar's ink.
/// `armed` tints the sha, subject and clock toward `chrome.error`, so the
/// commit a second press would reset to is named by its own colour and not
/// only by the band above it.
fn row(
    i: usize,
    data: &Data,
    host: &Rc<Host>,
    current: bool,
    focused: bool,
    armed: bool,
) -> AnyElement {
    // Every per-commit answer indexes straight into what load derived — see
    // `visible`'s comment: no lookup, no hashing, one array read each.
    let c = &data.commits[i];
    let time = &data.times[i];
    let who = &data.who[i];
    let d = &data.draws[i];
    let ch = host.font.char_width();
    chrome::list_row(host, current, focused, graph::ROW_H)
        .child(graph::row_canvas(d.clone(), host.clone()))
        .child(
            div()
                .flex_none()
                .w(px(SHA_CHARS * ch))
                // The error colour is already this palette's "this row ends
                // work" foreground — conflicts and armed destructions draw
                // with it — so the tint spends nothing new.
                .text_color(rgb(match armed {
                    true => host.theme.chrome.error,
                    false => host.theme.chrome.dim,
                }))
                .child(c.short.clone()),
        )
        .child(
            div()
                .flex_none()
                .w(px(WHO_CHARS * ch))
                // The colour resolves here, not at construction, so it reads
                // the live theme like the dim sha and the character width
                // beside it. Deliberate cost: one byte-fold hash of the author
                // name per visible row per frame. A memo HashMap arrives only
                // if profiling ever demands one.
                .text_color(rgb(host.theme.author(&c.author)))
                .child(who.initials.clone()),
        )
        .child(
            div()
                .min_w_0()
                .flex_shrink(1.0)
                .truncate()
                // Unarmed keeps the inherited ink; only the question
                // repaints the row.
                .when(armed, |d| d.text_color(rgb(host.theme.chrome.error)))
                .child(c.subject.clone()),
        )
        .child(
            div()
                .flex_none()
                .ml_auto()
                // A character of air before the cell is the floor under the
                // auto margin — what a squeezed subject still leaves.
                .pl(px(ch))
                .pr_2()
                .w(px((TIME_CHARS + 1.0) * ch))
                .flex()
                .justify_end()
                // Faint rather than dim, and below it in the palette on
                // purpose: furniture looked up once per glance and not text
                // to be read — the same floor reasoning that lets the diff's
                // gutter recede. Armed reaches here too, so the whole row
                // asks together.
                .text_color(rgb(match armed {
                    true => host.theme.chrome.error,
                    false => host.theme.chrome.faint,
                }))
                .child(time.clone()),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    // By name, not a glob: `use gpui::*` in the parent shadows `#[test]` with
    // GPUI's own attribute macro and every test in here fails to expand.
    use super::graph;
    use super::rel_time;
    use super::Commits;
    use gitten_core::host::Host;
    use gitten_core::Commit;
    use std::rc::Rc;
    use std::sync::Arc;

    /// One commit with everything the view needs, and nothing it reads.
    fn commit(n: usize) -> Commit {
        Commit {
            sha: format!("{n:040x}"),
            short: format!("abc00{n}"),
            parents: Box::new([]),
            author: "someone".into(),
            timestamp: 1_700_000_000 + n as i64,
            subject: format!("the {n}th change"),
        }
    }

    fn commits(n: usize) -> Vec<Commit> {
        (0..n).map(commit).collect()
    }

    /// A history whose subjects alternate between two words, so half of it
    /// survives any query the tests type and the rest does not.
    fn mixed_history() -> Vec<Commit> {
        (0..30usize)
            .map(|n| {
                let even = n.is_multiple_of(2);
                Commit {
                    author: Arc::from(if even { "ada" } else { "grace" }),
                    subject: if even {
                        format!("engine note {n}")
                    } else {
                        format!("compiler pass {n}")
                    },
                    ..commit(n)
                }
            })
            .collect()
    }

    fn with_height(c: &mut Commits, n: usize) {
        c.rendered.set(n);
        let mut v = c.view.get();
        v.set_len(c.visible.len());
        v.set_height(n);
        c.view.set(v);
    }

    #[test]
    fn navigation_moves_the_cursor_and_the_view_follows_with_a_margin() {
        let mut c = Commits::new(commits(100), Rc::new(Host::new()));
        with_height(&mut c, 20);
        assert!(c.run_view("view.down", &Host::new()));
        assert_eq!(c.view.get().cursor(), 1);
        for _ in 0..19 {
            c.run_view("view.down", &Host::new());
        }
        assert_eq!(c.view.get().cursor(), 20);
        assert!(c.top.get() > 0, "the margin pushed the viewport");
    }

    #[test]
    fn top_and_bottom_reach_both_ends_and_clamp() {
        let mut c = Commits::new(commits(100), Rc::new(Host::new()));
        with_height(&mut c, 20);
        assert!(c.run_view("view.bottom", &Host::new()));
        assert_eq!(c.view.get().cursor(), 99);
        assert_eq!(c.top.get(), 80, "no screen of blank rows below");
        for _ in 0..3 {
            assert!(c.run_view("view.up", &Host::new()));
        }
        assert_eq!(c.view.get().cursor(), 96);
        assert!(c.run_view("view.top", &Host::new()));
        assert_eq!((c.view.get().cursor(), c.view.get().top()), (0, 0));
        assert_eq!(c.total(), 100);
    }

    #[test]
    fn sideways_commands_are_answered_without_doing_anything() {
        // A commit graph has nothing off the left edge to reach; `h` and `l`
        // are still answered — a command that resolves must not read as one
        // that failed.
        let mut c = Commits::new(commits(10), Rc::new(Host::new()));
        with_height(&mut c, 5);
        assert!(c.run_view("view.left", &Host::new()));
        assert!(c.run_view("view.right", &Host::new()));
        assert!(!c.pan_pixels(40.0));
    }

    #[test]
    fn copy_falls_back_to_the_row_the_keyboard_is_on() {
        let mut c = Commits::new(commits(30), Rc::new(Host::new()));
        with_height(&mut c, 20);
        c.run_view("view.down", &Host::new());
        c.run_view("view.down", &Host::new());
        let text = c.cursor_text();
        assert!(
            text.contains("abc002") && text.contains("the 2th change"),
            "{text:?}"
        );
        // And what it copies is what `select.all` would have to work from,
        // which here is nothing: no selection model over a graph yet.
        assert!(!c.select_all());
        assert!(!c.select_none());
    }

    #[test]
    fn a_fresh_viewport_restores_a_saved_row_without_preseeding() {
        // The startup path on this screen too: a view constructed, a saved row
        // handed over, nothing measured yet.
        let mut host = Host::new();
        host.view.scrolloff = 5;
        let host = Rc::new(host);
        let c = Commits::new(commits(100), host.clone());
        assert_eq!(c.view.get().len(), 0);

        c.scroll_to(40, &host);
        c.go_to(40, &host);
        let v = c.view.get();
        assert_eq!(v.cursor(), 40, "the keyboard came back where it left off");
        assert_eq!(v.top(), 40);
        assert_eq!(v.len(), 100);
        // First real height settles with the file's margin above the cursor,
        // not the built-in's.
        let mut v = c.view.get();
        v.set_height(30);
        assert_eq!((v.cursor(), v.top()), (40, 35));
    }

    #[test]
    fn replacement_follows_commit_identity_across_insertions() {
        let host = Rc::new(Host::new());
        let mut c = Commits::new(commits(30), host.clone());
        with_height(&mut c, 10);
        let selected = c.data.commits[12].sha.clone();
        let visible = c.data.commits[8].sha.clone();
        let mut view = c.view.get();
        view.scroll_to(8);
        view.go_to(12);
        c.view.set(view);

        let mut refreshed = vec![commit(99)];
        refreshed.extend(commits(30));
        c.replace(refreshed, &host);

        let view = c.view.get();
        assert_eq!(c.data.commits[view.cursor()].sha, selected);
        assert_eq!(c.data.commits[view.top()].sha, visible);
    }

    #[test]
    fn replacement_clamps_missing_anchors_and_accepts_empty_history() {
        let host = Rc::new(Host::new());
        let mut c = Commits::new(commits(30), host.clone());
        with_height(&mut c, 10);
        let mut view = c.view.get();
        view.go_to(25);
        c.view.set(view);

        c.replace(commits(3), &host);
        assert_eq!(c.view.get().cursor(), 2);
        c.replace(Vec::new(), &host);
        assert_eq!(c.total(), 0);
        assert_eq!((c.view.get().cursor(), c.view.get().top()), (0, 0));
        assert!(c.scroll.0.borrow().deferred_scroll_to_item.is_none());
    }

    #[test]
    fn a_restored_row_inside_the_first_screen_still_moves_the_list() {
        // The non-strict strategy skips any row already inside the initial
        // viewport — which is where a saved row near the top of the graph
        // lands — so GPUI would open at row zero while everything else claimed
        // the restore worked. The parked request has to be strict.
        let host = Rc::new(Host::new());
        let mut c = Commits::new(commits(100), host.clone());
        c.scroll_to(5, &host);

        let request = c
            .scroll
            .0
            .borrow()
            .deferred_scroll_to_item
            .expect("no request was parked");
        assert_eq!(request.item_index, 5);
        assert_eq!(request.strategy, gpui::ScrollStrategy::Top);
        assert!(request.scroll_strict, "visible-in-range is exactly the bug");
        assert_eq!(c.view.get().top(), 5, "and the model says so too");

        // A command before layout replaces the target. Writing immediately
        // would clamp against geometry that still describes the empty list.
        assert!(c.run_view("view.down", &host));
        let request = c
            .scroll
            .0
            .borrow()
            .deferred_scroll_to_item
            .expect("the updated target was not deferred");
        assert_eq!(request.item_index, c.view.get().top());
        assert!(request.scroll_strict);
        assert_eq!(f32::from(c.scroll.0.borrow().base_handle.offset().y), 0.0);

        let before = request.item_index;
        assert!(c.scroll_pixels(-0.25, &host));
        let request = c
            .scroll
            .0
            .borrow()
            .deferred_scroll_to_item
            .expect("the wheel discarded the deferred target");
        assert_eq!(request.item_index, before, "the strict baseline moved");
        assert_eq!(c.pending_scroll.0.wheel.get(), -0.25);
        assert_eq!(f32::from(c.scroll.0.borrow().base_handle.offset().y), 0.0);
    }

    #[test]
    fn consuming_a_deferred_restore_is_not_reconciled_as_a_drag() {
        let host = Rc::new(Host::new());
        let mut c = Commits::new(commits(100), host.clone());
        c.scroll_to(40, &host);
        c.go_to(40, &host);

        // Measurement calls the row callback before prepaint takes the request;
        // that must not accept the old offset.
        assert!(
            crate::views::accept_deferred_scroll(&c.scroll, &c.pending_scroll, &c.synced).is_none()
        );
        assert!(c.pending_scroll.0.awaiting.get());
        assert_eq!(c.synced.get(), 0.0);

        // What strict prepaint does: consume the request and write its offset.
        {
            let mut state = c.scroll.0.borrow_mut();
            state.deferred_scroll_to_item = None;
            state
                .base_handle
                .set_offset(gpui::point(gpui::px(0.0), gpui::px(-40.0 * 22.0)));
        }
        let accepted =
            crate::views::accept_deferred_scroll(&c.scroll, &c.pending_scroll, &c.synced)
                .expect("prepaint's offset was not accepted");
        assert!(!accepted.wheeled);
        assert!(!c.pending_scroll.0.awaiting.get());
        assert_eq!(c.synced.get(), -40.0 * 22.0);

        // Without accepting the offset, reconcile treats row 40 as a thumb drag
        // and moves the cursor to the scrolloff margin before applying this key.
        c.rendered.set(20);
        assert!(c.run_view("view.down", &host));
        assert_eq!(c.view.get().cursor(), 41);
    }

    #[test]
    fn a_thumb_drag_cancels_a_parked_strict_position() {
        let host = Rc::new(Host::new());
        let mut c = Commits::new(commits(100), host.clone());
        c.scroll_to(40, &host);
        assert!(c.scroll_pixels(-0.25, &host));

        let bar = crate::views::DeferredScrollbar::new(&c.scroll, &c.pending_scroll);
        gpui_component::scroll::ScrollbarHandle::set_offset(
            &bar,
            gpui::point(gpui::px(0.0), gpui::px(-9.5)),
        );

        assert!(c.scroll.0.borrow().deferred_scroll_to_item.is_none());
        assert!(!c.pending_scroll.0.awaiting.get());
        assert_eq!(c.pending_scroll.0.wheel.get(), 0.0);
        assert_eq!(f32::from(c.scroll.0.borrow().base_handle.offset().y), -9.5);
    }

    #[test]
    fn key_navigation_uses_the_live_scrolloff() {
        let build = |scrolloff: usize| -> (Commits, Rc<Host>) {
            let mut h = Host::new();
            h.view.scrolloff = scrolloff;
            let host = Rc::new(h);
            let mut c = Commits::new(commits(100), host.clone());
            with_height(&mut c, 20);
            (c, host)
        };
        let (mut tight, tight_host) = build(3);
        let (mut loose, loose_host) = build(8);
        for _ in 0..16 {
            tight.run_view("view.down", &tight_host);
            loose.run_view("view.down", &loose_host);
        }
        assert_eq!(tight.view.get().cursor(), loose.view.get().cursor());
        assert_eq!(tight.top.get(), 0, "a three-row margin holds at cursor 16");
        assert!(loose.top.get() > 0, "an eight-row margin scrolled already");
    }

    #[test]
    fn a_thumb_drag_is_reconciled_before_anything_reads_the_cursor() {
        // What `commits.open-diff` reads through `current`, and what
        // `copy.selection` falls back to: both mean the commit being *looked
        // at*, so a scrollbar drag has to be met first.
        let host = Rc::new(Host::new());
        let mut c = Commits::new(commits(100), host.clone());
        with_height(&mut c, 20);
        // Ten rows of drag, written straight into the handle the way a paint
        // pass writes it: −220 px at 22 px a row.
        c.scroll
            .0
            .borrow()
            .base_handle
            .set_offset(gpui::point(gpui::px(0.), gpui::px(-220.)));
        assert_eq!(c.view.get().cursor(), 0, "the stale cursor is the bug");

        c.reconcile(&host);
        let v = c.view.get();
        assert_eq!(v.top(), 10);
        assert_eq!(v.cursor(), 13, "top ten plus the three-row margin");
        // And the commit under that cursor is the one open/copy now name.
        let text = c.cursor_text();
        assert!(text.contains("abc0013"), "{text:?}");
        assert_eq!(c.current().map(|cm| cm.short.as_str()), Some("abc0013"));

        // Meeting the list twice is not moving it twice.
        c.reconcile(&host);
        assert_eq!((c.view.get().top(), c.view.get().cursor()), (10, 13));
    }

    // ----------------------------------------------------------------- search

    #[test]
    fn a_query_filters_live_and_the_keyboard_stays_on_its_commit() {
        let host = Rc::new(Host::new());
        let mut c = Commits::new(mixed_history(), host.clone());
        with_height(&mut c, 10);
        // The keyboard sits on an *even* commit — one that survives "ENGINE".
        for _ in 0..4 {
            c.run_view("view.down", &host);
        }
        let anchored_sha = c.current().expect("a commit under the cursor").sha.clone();

        c.apply_query("  ENGINE  ");
        let v = c.view.get();
        assert_eq!(v.len(), 15, "half the history matches, folded");
        assert_eq!(c.filter_note().as_deref(), Some("15/30"));
        // Through the indirection: `current` is the anchored commit, not
        // whatever now happens to sit at row 4 of a shorter list.
        assert_eq!(
            c.current().map(|cm| cm.sha.as_str()),
            Some(anchored_sha.as_str())
        );
        // And what copy falls back to is still that same commit.
        assert!(
            c.cursor_text().contains("engine note"),
            "{:?}",
            c.cursor_text()
        );
    }

    #[test]
    fn narrowing_past_the_anchor_clamps_instead_of_pointing_nowhere() {
        let host = Rc::new(Host::new());
        let mut c = Commits::new(mixed_history(), host.clone());
        with_height(&mut c, 10);
        // Bottom row under "compiler": odd commits only, so the anchor below…
        c.run_view("view.bottom", &host);
        // …then a query no compiler row survives.
        c.apply_query("engine");
        let v = c.view.get();
        assert!(v.cursor() < v.len(), "the cursor outlived its own list");
        assert!(c.current().is_some());
        assert_eq!(c.filter_note().as_deref(), Some("15/30"));
    }

    #[test]
    fn a_query_matching_nothing_empties_the_list_and_holds_together() {
        let host = Rc::new(Host::new());
        let mut c = Commits::new(commits(20), host.clone());
        with_height(&mut c, 10);
        c.apply_query("nothing matches this");
        assert_eq!(
            c.total(),
            20,
            "the filter narrows what is shown, never what is loaded"
        );
        assert_eq!(c.view.get().len(), 0);
        assert!(c.current().is_none());
        assert_eq!(c.filter_note().as_deref(), Some("0/20"));

        // Clearing puts every row back where it started.
        c.apply_query("");
        assert_eq!(c.view.get().len(), 20);
        assert!(c.query().is_none());
        assert_eq!(c.filter_note(), None);
    }

    #[test]
    fn clearing_restores_the_full_list_under_the_same_commit() {
        let host = Rc::new(Host::new());
        let mut c = Commits::new(mixed_history(), host.clone());
        with_height(&mut c, 10);
        for _ in 0..6 {
            c.run_view("view.down", &host);
        }
        let before = c.current().map(|cm| cm.sha.clone()).unwrap();

        c.apply_query("ada");
        assert_eq!(c.view.get().len(), 15);
        c.apply_query("");
        assert_eq!(c.view.get().len(), 30);
        assert_eq!(
            c.current().map(|cm| cm.sha.as_str()),
            Some(before.as_str()),
            "empty restores instantly, cursor included"
        );
    }

    #[test]
    fn a_second_search_finds_the_query_standing() {
        let host = Rc::new(Host::new());
        let mut c = Commits::new(mixed_history(), host.clone());
        with_height(&mut c, 10);
        assert!(c.query().is_none());

        c.apply_query("compiler");
        assert_eq!(
            c.query(),
            Some("compiler"),
            "the prompt pre-fills from here"
        );

        // The same query again is no change at all — and rebuilds nothing.
        let before = Rc::as_ptr(&c.visible);
        let rows = c.view.get().len();
        c.apply_query("compiler ");
        assert_eq!(c.query(), Some("compiler"), "trimmed before comparing");
        assert_eq!(c.view.get().len(), rows);
        assert_eq!(
            Rc::as_ptr(&c.visible),
            before,
            "a no-op query did not rebuild the index"
        );
    }

    #[test]
    fn a_refresh_keeps_the_filter_and_reanchors_within_it() {
        let host = Rc::new(Host::new());
        let mut c = Commits::new(mixed_history(), host.clone());
        with_height(&mut c, 10);
        c.apply_query("engine");
        let anchored = c.current().map(|cm| cm.sha.clone()).unwrap();

        // A refresh prepends one new engine commit; the query re-runs against
        // the *new* data rather than dropping what the user looks through.
        let mut refreshed = mixed_history();
        refreshed.insert(
            0,
            Commit {
                subject: "engine note fresh".into(),
                ..commit(99)
            },
        );
        c.replace(refreshed, &host);

        assert_eq!(c.view.get().len(), 16, "still filtered, over the new data");
        assert_eq!(
            c.current().map(|cm| cm.sha.as_str()),
            Some(anchored.as_str())
        );
        assert_eq!(c.filter_note().as_deref(), Some("16/31"));
    }

    // ----------------------------------------------------------------- clock

    /// A fixed "now", so every age under test is a function of seconds alone
    /// — the same purity [`rel_time`]'s caller hands it a timestamp for.
    const NOW: i64 = 1_700_000_000;

    #[test]
    fn rel_time_reads_now_for_the_recent_and_the_future_alike() {
        assert_eq!(rel_time(NOW, NOW), "now");
        assert_eq!(rel_time(NOW - 59, NOW), "now", "59 seconds is still now");
        assert_eq!(
            rel_time(NOW + 120, NOW),
            "now",
            "a committer's clock ahead of ours prints no minus sign"
        );
    }

    #[test]
    fn rel_time_climbs_through_minutes_hours_and_days() {
        assert_eq!(
            rel_time(NOW - 60, NOW),
            "1m",
            "the minute band opens at 60s"
        );
        assert_eq!(
            rel_time(NOW - 3_599, NOW),
            "59m",
            "the last minute before an hour"
        );
        assert_eq!(rel_time(NOW - 3_600, NOW), "1h");
        assert_eq!(rel_time(NOW - 86_400, NOW), "1d");
        assert_eq!(
            rel_time(NOW - 6 * 86_400, NOW),
            "6d",
            "the last day inside the week band"
        );
    }

    #[test]
    fn rel_time_folds_weeks_months_and_years_to_whole_numbers() {
        assert_eq!(
            rel_time(NOW - 7 * 86_400, NOW),
            "1w",
            "the week band opens at 7d"
        );
        assert_eq!(
            rel_time(NOW - 29 * 86_400, NOW),
            "4w",
            "the last week inside the month band"
        );
        assert_eq!(
            rel_time(NOW - 30 * 86_400, NOW),
            "1mo",
            "the month band opens at 30d"
        );
        assert_eq!(
            rel_time(NOW - 364 * 86_400, NOW),
            "12mo",
            "and `12mo` is four characters wide"
        );
        assert_eq!(
            rel_time(NOW - 365 * 86_400, NOW),
            "1y",
            "the year band opens at 365d"
        );
        assert_eq!(
            rel_time(NOW - 800 * 86_400, NOW),
            "2y",
            "past three hundred sixty-five days the calendar stops being consulted"
        );
    }

    // ------------------------------------------------------------ arming

    #[test]
    fn a_hard_reset_arms_its_row_and_spends_on_the_same_commit() {
        let host = Rc::new(Host::new());
        let mut c = Commits::new(commits(4), host.clone());
        with_height(&mut c, 10);
        c.run_view("view.top", &host);
        let sha = c.current().expect("a commit").sha.clone();

        // First press: asked, not acted. The arm names the commit by sha.
        assert!(!c.confirm_or_arm_reset(&sha));
        assert_eq!(c.armed_sha(), Some(sha.clone()));

        // Second press on the same row: act, and the slot is spent.
        assert!(c.confirm_or_arm_reset(&sha));
        assert_eq!(c.armed_sha(), None);

        // And a third press starts over: there is no latched yes.
        assert!(!c.confirm_or_arm_reset(&sha));
    }

    #[test]
    fn arming_a_different_commit_moves_the_question_rather_than_confirming_it() {
        let host = Rc::new(Host::new());
        let mut c = Commits::new(commits(4), Rc::clone(&host));
        with_height(&mut c, 10);
        let (first, second) = (c.data.commits[0].sha.clone(), c.data.commits[1].sha.clone());

        assert!(!c.confirm_or_arm_reset(&first));
        assert!(!c.confirm_or_arm_reset(&second));
        assert_eq!(c.armed_sha(), Some(second), "one slot: the question moved");
        assert!(
            !c.confirm_or_arm_reset(&first),
            "the old arm was already gone"
        );
    }

    #[test]
    fn a_cursor_move_a_wheel_and_a_refresh_all_disarm_a_reset() {
        let host = Rc::new(Host::new());
        let mut c = Commits::new(commits(20), host.clone());
        with_height(&mut c, 5);
        c.run_view("view.top", &host);
        let sha0 = c.data.commits[0].sha.clone();
        let armed = |c: &Commits| c.armed_sha().is_some();

        // The keyboard moved.
        assert!(!c.confirm_or_arm_reset(&sha0));
        c.run_view("view.down", &host);
        assert!(!armed(&c), "the cursor move disarmed");

        // view.left/right move nothing, so the question stands — and a
        // wheel over a list that cannot move changes nothing either, so
        // neither gesture spends what is armed.
        assert!(!c.confirm_or_arm_reset(&sha0));
        c.run_view("view.right", &host);
        c.scroll_pixels(-3.0 * graph::ROW_H, &host);
        assert!(
            armed(&c),
            "gestures that moved nothing must not spend the question"
        );

        // The repository moved too.
        c.replace(mixed_history(), &host);
        assert!(!armed(&c), "a refresh disarmed");

        // And a changed filter is a movement of attention as well.
        assert!(!c.confirm_or_arm_reset(&sha0));
        c.apply_query("engine");
        assert!(!armed(&c), "a changed query disarmed");
    }
}
