//! The repository's branches, as a list.
//!
//! lazygit's Branches panel, viewer half: local branches first — HEAD's own
//! row marked, each tracking pair's distance spelled `↑n`/`↓n` where git can
//! measure it — then a quiet group of the remote-tracking refs the last fetch
//! left behind. Detached HEAD draws as its own top row rather than hiding:
//! half a bisect, a rebase in progress and "just looking at yesterday" are
//! states worth seeing named, and [`HeadState`] already carries them as data.
//!
//! The list idioms are [`super::files`]'s, on purpose: one `Viewport`, one
//! scroll-handle dance, rows flattened **once per refresh** into owned display
//! strings so the render path allocates nothing per frame.

use super::{accept_deferred_scroll, DeferredScrollbar, PendingScroll};
use crate::graph::ROW_H;
use gitten_core::host::Host;
use gitten_core::refs::{Branch, HeadState, RemoteBranch, Upstream};
use gitten_core::status::PathBytes;
use gitten_core::view::Viewport;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::scroll::Scrollbar;
use std::cell::Cell;
use std::rc::Rc;

/// One flat row of the pane.
///
/// Flattened once per refresh — never per frame. Everything a draw needs that
/// costs allocation (the lossy names, the upstream line, the spelled-out
/// counts) is computed at flatten time; what a draw reads per frame is an
/// enum match and a refcount bump.
#[derive(Debug)]
pub(crate) enum Row {
    /// Detached HEAD, its own top row: the honest state, not hidden.
    Detached {
        /// `(detached at abc12345…)` — abbreviated once, at flatten.
        text: SharedString,
    },
    /// A group heading, drawn only because the group under it is non-empty.
    Heading {
        /// How many branches are in the group, spelled out once.
        count: SharedString,
        section: Section,
    },
    Local(LocalRow),
    Remote(RemoteRow),
}

/// Which group a row sits under — the two halves of the ref namespace a
/// panel draws, and the half of the refresh anchor a verb needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Section {
    Local,
    Remote,
}

impl Section {
    fn name(self) -> &'static str {
        match self {
            Section::Local => "local",
            Section::Remote => "remote",
        }
    }
}

/// One local branch.
#[derive(Debug)]
pub(crate) struct LocalRow {
    /// HEAD is attached here — the one row that earns its marker.
    pub head: bool,
    /// The addressing form, byte for byte. Never decoded in place.
    pub name: PathBytes,
    /// The display form, decoded lossily once at flatten.
    pub name_text: SharedString,
    /// What its tracking pair says, pre-rendered: `origin/main ↑1 ↓2` when
    /// git can measure, the pair plus `(gone)` when the upstream's ref has
    /// vanished, `None` when the branch tracks nothing.
    pub upstream: Option<SharedString>,
    /// True when that pair exists but cannot be compared — the state the
    /// word *gone* names, drawn faint so it never reads as "in sync".
    pub gone: bool,
}

/// One remote-tracking branch, as the last fetch left it.
#[derive(Debug)]
pub(crate) struct RemoteRow {
    /// The remote it came from, as named locally.
    pub remote: PathBytes,
    /// The branch name on that remote.
    pub branch: PathBytes,
    /// `origin/main` — the display form, joined once at flatten. The two
    /// halves above stay separate because the join loses information: a
    /// remote's name may contain a slash.
    pub label: SharedString,
}

/// What the keyboard is on, as verbs aim at it: bytes, never display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Target {
    /// A local branch, named relative to `refs/heads`.
    Local(PathBytes),
    /// A remote-tracking branch. Checkout may aim here — git detaches onto
    /// the fetched commit — but rename and delete refuse tonight, on purpose.
    Remote {
        remote: PathBytes,
        branch: PathBytes,
    },
    /// The detached-HEAD row: a place, not a branch, and every branch verb
    /// says so rather than guessing which branch was meant.
    Detached,
}

/// The upstream half of one local row, rendered once.
///
/// Zeros stay silent — an in-sync branch reads as a bare name, and `↑0 ↓0`
/// is furniture nobody reads past the first time. Unknowable is the other
/// word: a pair configured against a ref that is no longer there gets
/// `(gone)` beside it, faint, because a missing number must not dress up as
/// a zero.
fn upstream_line(u: &Upstream) -> (SharedString, bool) {
    let mut text = format!(
        "{}/{}",
        u.remote.to_string_lossy(),
        u.branch.to_string_lossy()
    );
    match (u.ahead, u.behind) {
        (Some(ahead), Some(behind)) => {
            if ahead > 0 {
                text.push_str(&format!(" ↑{ahead}"));
            }
            if behind > 0 {
                text.push_str(&format!(" ↓{behind}"));
            }
            (text.into(), false)
        }
        _ => {
            text.push_str(" (gone)");
            (text.into(), true)
        }
    }
}

/// Flattens the repository's refs into display rows: detached HEAD first,
/// then the local branches, then the remote group. Pure — the unit-tested
/// half of a refresh.
pub(crate) fn flatten(
    local: &[Branch],
    remotes: &[RemoteBranch],
    head: Option<&HeadState>,
) -> Vec<Row> {
    let mut rows = Vec::new();
    if let Some(HeadState::Detached { commit }) = head {
        // Eight characters is what `git log --oneline` abbreviates to and
        // what every git UI shows; the full OID stays in the model.
        rows.push(Row::Detached {
            text: format!("(detached at {}…)", &commit[..commit.len().min(8)]).into(),
        });
    }
    if !local.is_empty() {
        rows.push(Row::Heading {
            count: SharedString::from(local.len().to_string()),
            section: Section::Local,
        });
        rows.extend(local.iter().map(|b| {
            let (upstream, gone) = b.upstream.as_ref().map_or((None, false), |u| {
                let (text, gone) = upstream_line(u);
                (Some(text), gone)
            });
            Row::Local(LocalRow {
                head: b.head,
                name: b.name.clone(),
                name_text: b.display().into_owned().into(),
                upstream,
                gone,
            })
        }));
    }
    if !remotes.is_empty() {
        rows.push(Row::Heading {
            count: SharedString::from(remotes.len().to_string()),
            section: Section::Remote,
        });
        rows.extend(remotes.iter().map(|r| {
            let label = format!(
                "{}/{}",
                r.remote.to_string_lossy(),
                r.branch.to_string_lossy()
            );
            Row::Remote(RemoteRow {
                remote: r.remote.clone(),
                branch: r.branch.clone(),
                label: label.into(),
            })
        }));
    }
    rows
}

/// [`flatten`] plus what the title strip says about it. The load line goes to
/// stderr like every other view's, and nothing is stored for an overlay that
/// does not read panes.
pub(crate) fn prepare(
    local: Vec<Branch>,
    remotes: Vec<RemoteBranch>,
    head: Option<HeadState>,
    describe: &str,
) -> Prepared {
    let t = std::time::Instant::now();
    let label = format!(
        "{describe} · {} local · {} remote",
        local.len(),
        remotes.len()
    );
    let rows = flatten(&local, &remotes, head.as_ref());
    eprintln!("branches: {label} · flatten {:.0?}", t.elapsed());
    Prepared { rows, label }
}

/// The whole branches panel flattened to rows, plus the title-strip line.
pub(crate) struct Prepared {
    pub(crate) rows: Vec<Row>,
    /// The title-strip line: who we are and how much there is.
    pub(crate) label: String,
}

/// The branches pane. Holds flattened rows behind an `Rc`, so a refresh swaps
/// one refcount instead of mutating what a frame may be reading.
///
/// Deletion confirms **in this pane** rather than in a dialog, exactly as the
/// working tree's discard does: the first press stores the target and asks
/// through the notice band, the second press on the same target executes, and
/// any cursor move, wheel or refresh drops the arm.
pub struct Branches {
    data: Rc<Vec<Row>>,
    scroll: UniformListScrollHandle,
    /// The cursor, the top row and the height — [`Viewport`], the same model
    /// every other list holds.
    view: Rc<Cell<Viewport>>,
    synced: Rc<Cell<f32>>,
    pending_scroll: PendingScroll,
    rendered: Rc<Cell<usize>>,
    /// The delete awaiting its second press. One slot — arming a different
    /// row moves the question, it does not queue two.
    armed: Option<Target>,
}

impl Branches {
    /// The viewport model with everything live folded in.
    fn live_view(&self, host: &Host) -> Viewport {
        let mut v = self.view.get();
        v.set_len(self.data.len());
        v.set_height(self.rendered.get());
        v.set_scrolloff(host.view.scrolloff);
        v
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
        }
    }

    /// Whether the repository had nothing to say — the empty state's trigger.
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// The flattened rows of the last refresh, read-only: what the tests
    /// read to see what a refresh actually landed.
    #[cfg(test)]
    pub(crate) fn rows(&self) -> &[Row] {
        &self.data
    }

    pub(crate) fn replace_prepared(&mut self, prepared: Prepared, host: &Host) {
        // A refresh is the repository saying things moved; an armed delete
        // was a promise about how they were, so it dies here first.
        self.armed = None;
        self.reconcile(host);
        let old = self.view.get();
        // Only a branch anchors, and on what a verb aims at — the bytes. A
        // heading is a fact about the last refresh's grouping, not a thing
        // the eye was reading.
        let anchored = self.data.get(old.cursor()).and_then(row_target);
        let Prepared { rows, .. } = prepared;
        self.data = Rc::new(rows);

        let cursor = anchored
            .and_then(|target| {
                self.data
                    .iter()
                    .position(|r| row_target(r).is_some_and(|t| t == target))
            })
            .unwrap_or(old.cursor());
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

    /// Nothing off the left edge to reach — names truncate rather than pan.
    /// Present so the wheel routing can offer the axis to every screen alike.
    pub fn pan_pixels(&self, _dx: f32) -> bool {
        false
    }

    /// Moves the list by `dy` pixels — the wheel. Same dance as every list,
    /// for the same reasons.
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

    /// Meets the list where it actually is after a scrollbar drag.
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
    /// vocabulary onto the same [`Viewport`] arithmetic every list runs.
    ///
    /// False is "not one of mine", and the caller says so.
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
            // Answered without doing anything, like the commit graph: a
            // resolved command must not read as a failed one.
            "view.left" | "view.right" => return true,
            _ => return false,
        }
        // The keyboard moved; whatever was armed was armed to what it used
        // to be on.
        self.armed = None;
        self.view.set(v);
        self.show(v);
        true
    }

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

    /// What the keyboard is on, as verbs aim at it. `None` on a heading or
    /// in an empty pane.
    pub(crate) fn current(&self) -> Option<Target> {
        row_target(self.data.get(self.view.get().cursor())?)
    }

    /// Arms — or confirms — a delete of this exact target. First call asks
    /// (returns false); second call on the same target spends the arm and
    /// acts (returns true); anything else re-arms and asks again.
    pub(crate) fn confirm_or_arm_delete(&mut self, target: &Target) -> bool {
        self.arm(target)
    }

    /// The same dance for `commits.rebase-onto`, over the one arm slot the
    /// pane holds: arming anything else moves the question.
    pub(crate) fn confirm_or_arm_rebase(&mut self, target: &Target) -> bool {
        self.arm(target)
    }

    fn arm(&mut self, target: &Target) -> bool {
        let already = self.armed.as_ref() == Some(target);
        self.armed = match already {
            true => None,
            false => Some(target.clone()),
        };
        already
    }

    /// Whether a delete is waiting for its second press — the render's tint
    /// of the row the question is about.
    #[cfg(test)]
    pub(crate) fn armed_row(&self) -> Option<Target> {
        self.armed.clone()
    }

    /// What `copy.selection` copies here: the row the keyboard is on, as git
    /// would spell it — the bare refname. Headings and the detached row copy
    /// nothing, which is what makes an empty result skip the clipboard.
    pub fn cursor_text(&self) -> String {
        match self.current() {
            Some(Target::Local(name)) => name.to_string_lossy().into_owned(),
            Some(Target::Remote { remote, branch }) => {
                format!("{}/{}", remote.to_string_lossy(), branch.to_string_lossy())
            }
            Some(Target::Detached) | None => String::new(),
        }
    }

    /// No drag selection over a ref list yet; `select.all` is inert here.
    pub fn select_all(&mut self) -> bool {
        false
    }

    pub fn select_none(&mut self) -> bool {
        false
    }
}

/// A row's verb target, when it has one — headings do not.
fn row_target(row: &Row) -> Option<Target> {
    match row {
        Row::Detached { .. } => Some(Target::Detached),
        Row::Local(l) => Some(Target::Local(l.name.clone())),
        Row::Remote(r) => Some(Target::Remote {
            remote: r.remote.clone(),
            branch: r.branch.clone(),
        }),
        Row::Heading { .. } => None,
    }
}

/// Air around the head marker, in characters.
const MARK_CHARS: f32 = 1.5;

impl Render for Branches {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = crate::config::host(cx).theme.chrome;
        // No refs at all is a sentence, not an empty box — an unborn
        // repository's honest answer.
        if let Some(empty) = self.is_empty().then(|| {
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(rgb(c.faint))
                .child("No branches yet")
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
        // The row an armed delete is waiting on, found once per frame — the
        // tint is a property of the question, not of the draw.
        let armed = self.armed.as_ref().and_then(|target| {
            data.iter()
                .position(|r| row_target(r).as_ref() == Some(target))
        });
        let list = uniform_list("branches", data.len(), move |range, _, cx| {
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
        .p_4();

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

/// One row. `current` paints the keyboard's row in `chrome.selection_bg`;
/// `armed` tints it toward `chrome.error`, so the thing a second press will
/// destroy is named by its own colour and not only by the band above it.
fn row(e: &Row, host: &Host, current: bool, armed: bool) -> AnyElement {
    let ch = host.font.char_width();
    let c = host.theme.chrome;
    let base = div()
        .min_w_full()
        .flex()
        .items_center()
        .h(px(ROW_H))
        .bg(rgb(match current {
            true => c.selection_bg,
            false => c.bg,
        }));
    match e {
        Row::Detached { text } => base
            .child(
                div()
                    .flex_none()
                    .when(armed, |d| d.text_color(rgb(c.error)))
                    .text_color(rgb(c.dim))
                    .child(text.clone()),
            )
            .into_any_element(),
        Row::Heading { count, section } => base
            .child(
                div()
                    .flex_none()
                    .text_color(rgb(c.dim))
                    .child(section.name()),
            )
            .child(
                div()
                    .flex_none()
                    .ml(px(MARK_CHARS * ch))
                    .text_color(rgb(c.faint))
                    .child(count.clone()),
            )
            .into_any_element(),
        Row::Local(l) => base
            .child(
                // A fixed-width marker column, empty where HEAD is not: the
                // names align whether or not their row owns the mark.
                div()
                    .flex_none()
                    .w(px(ch))
                    .child(match l.head {
                        true => SharedString::from("*"),
                        false => SharedString::default(),
                    })
                    .text_color(rgb(c.accent)),
            )
            .child(
                div()
                    .flex_none()
                    .when(armed, |d| d.text_color(rgb(c.error)))
                    .child(l.name_text.clone()),
            )
            .children(l.upstream.clone().map(|text| {
                div()
                    .flex_none()
                    .ml(px(MARK_CHARS * ch))
                    .text_color(rgb(match l.gone {
                        true => c.faint,
                        false => c.dim,
                    }))
                    .child(text)
            }))
            .into_any_element(),
        Row::Remote(r) => base
            // No marker of its own — remotes cannot be where HEAD is — but
            // the same empty column keeps both namespaces aligned.
            .child(div().flex_none().w(px(ch)))
            .child(
                div()
                    .flex_none()
                    .when(armed, |d| d.text_color(rgb(c.error)))
                    .text_color(rgb(c.dim))
                    .child(r.label.clone()),
            )
            .into_any_element(),
    }
}

#[cfg(test)]
mod tests {
    use super::{flatten, prepare, row_target, Branches, Prepared, Row, Target};
    use gitten_core::host::Host;
    use gitten_core::refs::{Branch, HeadState, RefName, RemoteBranch, Upstream};

    /// A full-length OID-looking commit id, for shapes rather than values.
    fn sha() -> String {
        "0123456789abcdef0123456789abcdef01234567".to_string()
    }

    fn local(name: &str, head: bool) -> Branch {
        Branch {
            name: RefName::from(name),
            commit: sha(),
            upstream: None,
            head,
        }
    }

    fn tracked(name: &str, head: bool, ahead: Option<u32>, behind: Option<u32>) -> Branch {
        Branch {
            upstream: Some(Upstream {
                remote: RefName::from("origin"),
                branch: RefName::from(name),
                ahead,
                behind,
            }),
            ..local(name, head)
        }
    }

    fn remote(name: &str) -> RemoteBranch {
        RemoteBranch {
            remote: RefName::from("origin"),
            branch: RefName::from(name),
            commit: sha(),
        }
    }

    /// Headings and rows in draw order — the shape the tests read.
    fn outline(rows: &[Row]) -> Vec<String> {
        rows.iter()
            .map(|r| match r {
                Row::Detached { text } => format!("[detached {text}]"),
                Row::Heading { count, section } => {
                    format!("[{}·{count}]", section.name())
                }
                Row::Local(l) => {
                    let star = if l.head { "*" } else { "" };
                    match &l.upstream {
                        Some(u) => format!("{star}{} {u}", l.name_text),
                        None => format!("{star}{}", l.name_text),
                    }
                }
                Row::Remote(r) => r.label.to_string(),
            })
            .collect()
    }

    #[test]
    fn locals_come_first_and_the_remote_group_is_quiet_but_there() {
        let rows = flatten(
            &[local("feature", false), local("main", true)],
            &[remote("main"), remote("wip")],
            None,
        );
        assert_eq!(
            outline(&rows),
            vec![
                "[local·2]",
                "feature",
                "*main",
                "[remote·2]",
                "origin/main",
                "origin/wip",
            ]
        );
        // An empty group draws no heading at all.
        assert_eq!(
            outline(&flatten(&[local("main", true)], &[], None)),
            vec!["[local·1]", "*main"]
        );
    }

    #[test]
    fn a_detached_head_is_its_own_top_row_not_a_hidden_state() {
        let rows = flatten(
            &[local("main", false)],
            &[],
            Some(&HeadState::Detached { commit: sha() }),
        );
        assert_eq!(
            outline(&rows)[0],
            "[detached (detached at 01234567…)]",
            "abbreviated once, at flatten"
        );
        // And attached heads put no such row anywhere.
        let attached = flatten(
            &[local("main", true)],
            &[],
            Some(&HeadState::Branch {
                name: RefName::from("main"),
                commit: None,
            }),
        );
        assert!(!outline(&attached).iter().any(|r| r.contains("detached")));
    }

    #[test]
    fn tracking_speaks_in_arrows_and_zeros_stay_silent() {
        let rows = flatten(
            &[
                tracked("synced", false, Some(0), Some(0)),
                tracked("ahead", false, Some(2), Some(0)),
                tracked("behind", true, Some(0), Some(3)),
                tracked("both", false, Some(1), Some(4)),
            ],
            &[],
            None,
        );
        assert_eq!(
            outline(&rows),
            vec![
                "[local·4]",
                "synced origin/synced",
                "ahead origin/ahead ↑2",
                "*behind origin/behind ↓3",
                "both origin/both ↑1 ↓4",
            ]
        );
    }

    #[test]
    fn a_gone_upstream_is_named_gone_rather_than_reading_as_zero() {
        // ahead/behind `None` with the pair still configured: the ref the
        // branch tracks no longer exists locally. A `0` here would invite a
        // push that fixes nothing.
        let rows = flatten(&[tracked("old", false, None, None)], &[], None);
        assert_eq!(outline(&rows), vec!["[local·1]", "old origin/old (gone)"]);
        // The row remembers why, for the faint ink the draw gives it.
        match &rows[1] {
            Row::Local(l) => assert!(l.gone),
            other => panic!("the tracked row expected, got {other:?}"),
        }
    }

    #[test]
    fn names_keep_their_bytes_and_display_lossily_once() {
        // Latin-1 é and ø: legal ref bytes, illegal UTF-8.
        let rows = flatten(
            &[Branch {
                name: RefName::from_bytes(b"f\xe9ature"),
                ..local("unused", false)
            }],
            &[RemoteBranch {
                remote: RefName::from("origin"),
                branch: RefName::from_bytes(b"w\xf8rk"),
                commit: sha(),
            }],
            None,
        );
        match &rows[1] {
            Row::Local(l) => {
                assert_eq!(l.name.as_bytes(), b"f\xe9ature", "addressing keeps bytes");
                assert!(
                    l.name_text.contains('\u{FFFD}'),
                    "display decodes lossily instead of failing"
                );
            }
            other => panic!("the local row expected, got {other:?}"),
        }
        match &rows[3] {
            Row::Remote(r) => {
                assert_eq!(r.branch.as_bytes(), b"w\xf8rk");
                assert!(r.label.contains('\u{FFFD}'));
            }
            other => panic!("the remote row expected, got {other:?}"),
        }
    }

    #[test]
    fn targets_are_what_verbs_aim_at_bytes_included() {
        let rows = flatten(
            &[local("main", true)],
            &[remote("main")],
            Some(&HeadState::Detached { commit: sha() }),
        );
        assert_eq!(
            row_target(&rows[0]),
            Some(Target::Detached),
            "the detached row is a place verbs can refuse honestly"
        );
        assert_eq!(
            row_target(&rows[2]),
            Some(Target::Local(RefName::from("main")))
        );
        assert_eq!(
            row_target(&rows[4]),
            Some(Target::Remote {
                remote: RefName::from("origin"),
                branch: RefName::from("main"),
            })
        );
        assert_eq!(row_target(&rows[1]), None, "a heading aims at nothing");
        assert_eq!(row_target(&rows[3]), None);
    }

    #[test]
    fn a_delete_arms_then_confirms_and_any_cursor_move_disarms() {
        let host = Host::new();
        let mut b = Branches::from_prepared(prepare(
            vec![local("feature", false), local("main", true)],
            Vec::new(),
            None,
            "",
        ));
        b.rendered.set(3);
        let mut v = b.view.get();
        v.set_len(b.data.len());
        v.set_height(3);
        b.view.set(v);
        assert!(b.run_view("view.down", &host)); // onto feature
        let target = b.current().expect("a branch under the keyboard");

        // First press: asked, not acted.
        assert!(!b.confirm_or_arm_delete(&target));
        assert_eq!(b.armed_row(), Some(target.clone()));

        // Second press on the same row: act, and the slot is spent.
        assert!(b.confirm_or_arm_delete(&target));
        assert_eq!(b.armed_row(), None);

        // A third press starts over — there is no latched yes.
        assert!(!b.confirm_or_arm_delete(&target));

        // The keyboard moving drops the question before it can lie.
        assert!(b.run_view("view.down", &host));
        assert_eq!(
            b.armed_row(),
            None,
            "the arm did not survive its own cursor move"
        );
    }

    #[test]
    fn a_refresh_disarms_an_armed_delete_even_when_the_branch_survives() {
        // The mirror of the working tree's rule: a refresh is the repository
        // saying things moved, and an armed delete was a promise about how
        // they were.
        let host = Host::new();
        let mut b = Branches::from_prepared(prepare(
            vec![local("feature", false), local("main", true)],
            Vec::new(),
            None,
            "",
        ));
        b.rendered.set(3);
        let mut v = b.view.get();
        v.set_len(b.data.len());
        v.set_height(3);
        b.view.set(v);
        assert!(b.run_view("view.down", &host)); // onto feature
        let target = b.current().expect("a branch under the keyboard");
        assert!(!b.confirm_or_arm_delete(&target));
        assert_eq!(b.armed_row(), Some(target.clone()));

        // A refresh that changes nothing at all still says "things moved".
        b.replace_prepared(
            prepare(
                vec![local("feature", false), local("main", true)],
                Vec::new(),
                None,
                "",
            ),
            &host,
        );
        assert_eq!(b.armed_row(), None, "the question did not survive");
        // And the press after it re-arms rather than executes — there was
        // never a latched yes to lose.
        assert!(!b.confirm_or_arm_delete(&target));

        // The same when the branch itself is gone under the arm.
        assert!(b.confirm_or_arm_delete(&target));
        b.replace_prepared(
            prepare(vec![local("main", true)], Vec::new(), None, ""),
            &host,
        );
        assert_eq!(b.armed_row(), None);
        // The cursor clamped onto the branch that remains — a real row,
        // never the ghost of the one the question was about.
        assert_eq!(
            b.current(),
            Some(Target::Local(RefName::from("main"))),
            "the vanished anchor fell back to clamping"
        );
    }

    #[test]
    fn the_label_counts_both_groups_and_an_empty_repository_flattens_to_nothing() {
        let p = prepare(
            vec![local("main", true)],
            vec![remote("main")],
            None,
            "gitten (main)",
        );
        assert_eq!(p.label, "gitten (main) · 1 local · 1 remote");

        let empty = prepare(Vec::new(), Vec::new(), None, "gitten");
        assert_eq!(empty.label, "gitten · 0 local · 0 remote");
        assert_eq!(empty.rows.len(), 0);

        // And the prepared type is what a refresh hands the pane.
        let p: Prepared = p;
        assert!(!p.rows.is_empty());
    }
}
