use super::{accept_deferred_scroll, DeferredScrollbar, PendingScroll};
use crate::graph;
use gitten_core::host::Host;
use gitten_core::view::Viewport;
use gitten_core::{assign_lanes, initials, Commit};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::scroll::Scrollbar;
use std::cell::Cell;
use std::rc::Rc;

/// The commit column between the sha and the graph, resolved once at load: two
/// letters and the colour they are drawn in. Not a per-frame job.
struct Who {
    initials: SharedString,
    color: Rgba,
}

struct Data {
    commits: Vec<Commit>,
    draws: Vec<graph::Draw>,
    who: Vec<Who>,
    /// uniform_list measures exactly ONE row to decide how wide the content is,
    /// and by default that is row 0. If row 0 is short there is nothing to
    /// scroll to, however long the rest are. Point it at the real widest row.
    widest: usize,
    /// One line per commit, the way `copy.selection` copies it — the sha and the
    /// subject, and neither the graph nor the initials. Built at load with
    /// everything else that is derived once.
    lines: Vec<String>,
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
}

impl Commits {
    /// The viewport model with everything live folded in: the list's length,
    /// the height last measured, and `[view] scrolloff` as the file has it
    /// *now* — see the diff view's `live_view`.
    fn live_view(&self, host: &Host) -> Viewport {
        let mut v = self.view.get();
        v.set_len(self.data.commits.len());
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
        if self.data.commits.is_empty() {
            return;
        }
        let row = row.min(self.data.commits.len() - 1);
        let mut v = self.live_view(host);
        v.scroll_to(row);
        self.view.set(v);
        self.defer_show(v);
    }

    pub fn total(&self) -> usize {
        self.data.commits.len()
    }

    /// The commit under the keyboard, for whatever opens a diff from it.
    pub fn current(&self) -> Option<&Commit> {
        self.data.commits.get(self.view.get().cursor())
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
            // No sideways half: see `pan_pixels`.
            "view.left" | "view.right" => return true,
            _ => return false,
        }
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
    /// the two fields that name the commit to git and to a person.
    pub fn cursor_text(&self) -> String {
        let v = self.view.get();
        self.data.lines.get(v.cursor()).cloned().unwrap_or_default()
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
}

impl Commits {
    pub fn new(commits: Vec<Commit>, host: Rc<Host>) -> Self {
        let Prepared { data, load } = prepare(commits, &host);
        Self {
            data: Rc::new(data),
            scroll: UniformListScrollHandle::new(),
            view: Rc::new(Cell::new(Viewport::new())),
            synced: Rc::new(Cell::new(0.0)),
            pending_scroll: PendingScroll::default(),
            rendered: Rc::new(Cell::new(0)),
            top: Rc::new(Cell::new(0)),
            load,
        }
    }

    /// Replaces repository data while keeping semantic commit anchors.
    #[cfg(test)]
    pub fn replace(&mut self, commits: Vec<Commit>, host: &Host) {
        let prepared = prepare(commits, host);
        self.replace_prepared(prepared, host);
    }

    pub(crate) fn replace_prepared(&mut self, prepared: Prepared, host: &Host) {
        self.reconcile(host);
        let old = self.view.get();
        let cursor_sha = self.data.commits.get(old.cursor()).map(|c| c.sha.clone());
        let top_sha = self.data.commits.get(old.top()).map(|c| c.sha.clone());
        let old_cursor = old.cursor();
        let old_top = old.top();
        let Prepared { data, load } = prepared;
        let cursor = cursor_sha
            .as_deref()
            .and_then(|sha| data.commits.iter().position(|c| c.sha == sha))
            .unwrap_or(old_cursor);
        let top = top_sha
            .as_deref()
            .and_then(|sha| data.commits.iter().position(|c| c.sha == sha))
            .unwrap_or(old_top);
        self.data = Rc::new(data);
        self.load = load;

        let mut view = old;
        view.set_len(self.data.commits.len());
        view.scroll_to(top);
        view.go_to(cursor);
        self.view.set(view);
        if self.data.commits.is_empty() {
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

pub(crate) fn prepare(commits: Vec<Commit>, host: &Host) -> Prepared {
    let t = std::time::Instant::now();
    let rows = assign_lanes(&commits);
    let t_lanes = t.elapsed();

    let t = std::time::Instant::now();
    let draws = graph::row_draws(&commits, &rows);
    let lanes = graph::lane_count(&rows);
    let t_draws = t.elapsed();

    let who: Vec<Who> = commits
        .iter()
        .map(|c| Who {
            initials: initials(&c.author).into(),
            color: rgb(host.theme.author(&c.author)),
        })
        .collect();

    // The widest row is no longer just the longest subject: every row's
    // graph is only as wide as its own lanes, so a short message behind a
    // wide graph can still out-reach a long one on the trunk.
    //
    // One character's width comes from the host's font rather than a constant
    // measured on whatever the font used to be. It only picks which row
    // `uniform_list` measures, so an approximation is fine — and it is
    // meaningless for a proportional face, which is the honest reason a
    // long subject may then win over a wide graph.
    let char_w = host.font.char_width();
    let widest = draws
        .iter()
        .zip(&commits)
        .map(|(d, c)| graph::row_width(d) + c.subject.len() as f32 * char_w)
        .enumerate()
        .max_by(|(_, a), (_, b)| a.total_cmp(b))
        .map(|(i, _)| i)
        .unwrap_or(0);

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
            commits,
            draws,
            who,
            widest,
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
        let list = uniform_list("commits", data.commits.len(), move |range, _, cx| {
            rendered.set(range.len());
            top.set(range.start);
            let host = crate::config::host(cx);
            if let Some(accepted) = accept_deferred_scroll(&scroll, &pending_scroll, &synced) {
                if accepted.wheeled {
                    let mut v = view.get();
                    v.set_len(data.commits.len());
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
                    row(
                        &data.commits[i],
                        &data.who[i],
                        &data.draws[i],
                        &host,
                        i == cursor,
                    )
                })
                .collect()
        })
        .with_width_from_item(Some(self.data.widest))
        .track_scroll(&self.scroll)
        // Let rows exceed the viewport width instead of being clipped; this is
        // what turns on horizontal scrolling.
        .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
        .size_full()
        .p_4();

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
            .child(Scrollbar::horizontal(&self.scroll))
        })
    }
}

/// The sha and the initials columns, in *characters*.
///
/// Twelve, because `%h` is seven in a young repository and eleven in git/git,
/// plus the air after it. In pixels rather than characters these were 90 and 26,
/// which is 10.7 and 3.1 in the shipped face — so an eleven-character sha
/// overflowed its own column by two pixels while the comment above it said
/// eleven — and 5 and 1.4 at the 18px `font.size` the config file will happily
/// give you. Fixed columns, unlike the graph: the eye scans these vertically, so
/// they have to *be* columns.
const SHA_CHARS: f32 = 12.0;
const WHO_CHARS: f32 = 3.0;

/// lazygit's order — sha, author, graph, subject — and lazygit's spacing: the
/// subject follows its own row's graph immediately, so a commit on the trunk
/// reads from the left instead of starting behind the widest merge in the
/// repository.
///
/// `current` paints the keyboard's row in `chrome.selection_bg`, the one colour
/// the terminal uses for exactly this, so the cursor is visible wherever the
/// keymap moves it.
fn row(c: &Commit, who: &Who, d: &graph::Draw, host: &Rc<Host>, current: bool) -> AnyElement {
    let ch = host.font.char_width();
    div()
        .flex()
        .items_center()
        .h(px(graph::ROW_H))
        .bg(rgb(match current {
            true => host.theme.chrome.selection_bg,
            false => host.theme.chrome.bg,
        }))
        .child(
            div()
                .flex_none()
                .w(px(SHA_CHARS * ch))
                .text_color(rgb(host.theme.chrome.dim))
                .child(c.short.clone()),
        )
        .child(
            div()
                .flex_none()
                .w(px(WHO_CHARS * ch))
                .text_color(who.color)
                .child(who.initials.clone()),
        )
        .child(graph::row_canvas(d.clone(), host.clone()))
        .child(div().flex_none().child(c.subject.clone()))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    // By name, not a glob: `use gpui::*` in the parent shadows `#[test]` with
    // GPUI's own attribute macro and every test in here fails to expand.
    use super::Commits;
    use gitten_core::host::Host;
    use gitten_core::Commit;
    use std::rc::Rc;

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

    fn with_height(c: &mut Commits, n: usize) {
        c.rendered.set(n);
        let mut v = c.view.get();
        v.set_len(c.data.commits.len());
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
}
