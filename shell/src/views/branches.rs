//! The repository's branches, as a list.
//!
//! lazygit's Branches panel, viewer half: local branches first — each opening
//! with a one-character dot that says what it *is* (HEAD in the accent, every
//! other local in a lane ink of its own, remote-tracking copies hollow and
//! faint) — with each tracking pair's **distance** spelled compactly
//! `↑n`/`↓n` where git can measure it. The upstream ref is not repeated on
//! the row: those refs sit below as rows of their own. Detached HEAD draws
//! as its own top row rather than hiding: half a bisect, a rebase in
//! progress and "just looking at yesterday" are states worth seeing named,
//! and [`HeadState`] already carries them as data.
//!
//! The list idioms are [`super::files`]'s, on purpose: one `Viewport`, one
//! scroll-handle dance, rows flattened **once per refresh** into owned display
//! strings so the render path allocates nothing per frame.

use super::{accept_deferred_scroll, DeferredScrollbar, PendingScroll};
use crate::chrome;
use crate::graph::ROW_H;
use gitten_core::host::Host;
use gitten_core::refs::{Branch, HeadState, RemoteBranch, Upstream};
use gitten_core::status::PathBytes;
use gitten_core::theme::{Rgb, Theme};
use gitten_core::view::Viewport;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::scroll::Scrollbar;
use std::cell::Cell;
use std::rc::Rc;

/// One flat row of the pane.
///
/// Flattened once per refresh — never per frame. Everything a draw needs that
/// costs allocation (the lossy names, the tracking distance, the spelled-out
/// counts) or a decision (each dot's ink) is computed at flatten time; what a
/// draw reads per frame is an enum match and a refcount bump.
#[derive(Debug)]
pub(crate) enum Row {
    /// Detached HEAD, its own top row: the honest state, not hidden.
    Detached {
        /// The row's dot — [`Dot`] so the state draws in its own ink like
        /// every other ref, dim where a branch would glow.
        dot: Dot,
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

/// The coloured mark that opens every ref row: one character wide, decided
/// entirely at flatten — glyph **and** `Rgb` stored on the row — so the draw
/// never consults the theme, cycles nothing and allocates nothing for it.
///
/// The design's grammar: a filled ● is a ref living locally, tinted by what
/// the row *is* — HEAD alone wears the accent, other locals borrow graph-lane
/// inks so each branch keeps one colour across the app — while a hollow ○
/// marks a remote-tracking copy, faint because it names what a fetch already
/// fetched, not anything checked out here.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Dot {
    /// `●` locally, `○` for the remote copies.
    pub glyph: &'static str,
    /// Handled out at flatten with everything else a draw needs to be free.
    pub color: Rgb,
}

/// Which group a row sits under — the two halves of the ref namespace a
/// panel draws, and the half of the refresh anchor a verb needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Section {
    Local,
    Remote,
}

impl Section {
    /// The lowercase spelling the tests outline rows with; the drawn heading
    /// is [`Section::label`].
    #[cfg(test)]
    fn name(self) -> &'static str {
        match self {
            Section::Local => "local",
            Section::Remote => "remote",
        }
    }

    /// The heading drawn over the group, in caps. Static, so the row's frame
    /// spells nothing.
    fn label(self) -> &'static str {
        match self {
            Section::Local => "LOCAL",
            Section::Remote => "REMOTE",
        }
    }
}

/// One local branch.
#[derive(Debug)]
pub(crate) struct LocalRow {
    /// The addressing form, byte for byte. Never decoded in place.
    pub name: PathBytes,
    /// The display form, decoded lossily once at flatten.
    pub name_text: SharedString,
    /// What its tracking pair says, pre-rendered as **distance only** — see
    /// [`upstream_counts`]. `None` draws no cell at all: the branch tracks
    /// nothing, or is in sync with what it tracks.
    pub counts: Option<SharedString>,
    /// True when a pair exists but cannot be compared — the state the word
    /// *gone* names, drawn faint so it never reads as "in sync".
    pub gone: bool,
    /// The row's dot, decided once — HEAD accent, otherwise lane ink.
    pub dot: Dot,
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
    /// The row's dot — hollow and faint, the grammar for "a fetched copy".
    pub dot: Dot,
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

/// The distance half of one local row, rendered once.
///
/// Zeros stay silent — an in-sync branch reads as a bare name, and `↑0 ↓0`
/// is furniture nobody reads past the first time; both zeros collapse to
/// `None`, so the pane draws no cell at all. Unknowable is the other word:
/// a pair configured against a ref that is no longer there gets `(gone)`,
/// faint, because a missing number must not dress up as a zero. A `None`
/// on either side means the comparison failed, not half of it, so the word
/// covers both.
///
/// The upstream **ref** is deliberately absent — `origin/main ↑1 ↓2` here is
/// exactly what the design takes away — because the remote-tracking branch
/// already sits below as its own row: naming it twice spends the row's width
/// to say nothing new, and what remains is the only part a glance reads.
fn upstream_counts(u: &Upstream) -> (Option<SharedString>, bool) {
    let mut text = String::new();
    for (count, arrow) in [(u.ahead, "↑"), (u.behind, "↓")] {
        let Some(n) = count else {
            return (Some(SharedString::from("(gone)")), true);
        };
        if n > 0 {
            // Joined by a single space; the first arrow comes alone.
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(arrow);
            text.push_str(&n.to_string());
        }
    }
    (
        (!text.is_empty()).then_some(text).map(SharedString::from),
        false,
    )
}

/// Flattens the repository's refs into display rows: detached HEAD first,
/// then the local branches, then the remote group. Pure — the unit-tested
/// half of a refresh. The theme rides along because each dot's ink is a
/// flatten-time decision: it lands on the row, not on the render path.
pub(crate) fn flatten(
    local: &[Branch],
    remotes: &[RemoteBranch],
    head: Option<&HeadState>,
    theme: &Theme,
) -> Vec<Row> {
    let mut rows = Vec::new();
    if let Some(HeadState::Detached { commit }) = head {
        // Eight characters is what `git log --oneline` abbreviates to and
        // what every git UI shows; the full OID stays in the model.
        rows.push(Row::Detached {
            dot: Dot {
                glyph: "●",
                color: theme.chrome.dim,
            },
            text: format!("(detached at {}…)", &commit[..commit.len().min(8)]).into(),
        });
    }
    if !local.is_empty() {
        rows.push(Row::Heading {
            count: SharedString::from(local.len().to_string()),
            section: Section::Local,
        });
        rows.extend(local.iter().enumerate().map(|(i, b)| {
            let (counts, gone) = b.upstream.as_ref().map_or((None, false), upstream_counts);
            // HEAD's branch alone wears the accent; every other local keeps
            // one lane ink for its whole life in this pane. The index is the
            // row's place among locals whether or not HEAD marks it, so a
            // checkout that moves paints only the one dot it moved.
            let dot = match b.head {
                true => Dot {
                    glyph: "●",
                    color: theme.chrome.accent,
                },
                false => Dot {
                    glyph: "●",
                    color: theme.lane(i),
                },
            };
            Row::Local(LocalRow {
                name: b.name.clone(),
                name_text: b.display().into_owned().into(),
                counts,
                gone,
                dot,
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
                // Hollow and faint: a fetched copy of elsewhere, never a
                // state of this checkout. Faint through `quiet_on` rather
                // than raw — the dot is read as a state, and raw `faint` is
                // under the furniture floor on the row it lands on.
                dot: Dot {
                    glyph: "○",
                    color: theme.quiet_on(theme.chrome.bg),
                },
            })
        }));
    }
    rows
}

/// What the title strip names about HEAD: the attached branch and its
/// tracking distance. The distance is passed through verbatim rather than
/// re-spelled, because core has already decided what an unknowable means
/// and dressing that up as a zero here would be wrong exactly where it
/// matters — a push/pull badge reading "nothing to do" when it cannot know.
///
/// Attached without a matching local row (an unborn branch's honest state)
/// yields `None`: nothing is invented to fill the slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadInfo {
    /// The branch HEAD sits on, display form, decoded once.
    pub branch: SharedString,
    /// Commits to push. `None` while git cannot compare — including gone.
    pub ahead: Option<u32>,
    /// Commits to pull. `None` under the same conditions as [`HeadInfo::ahead`].
    pub behind: Option<u32>,
    /// `⎇ main` — the chip's bright half, spelled once here so the title
    /// strip clones a refcount per frame instead of formatting a string.
    pub chip: SharedString,
    /// ` · ↑2 ↓0` — the chip's dim half, [`drift`] run once; `None` when
    /// there is nothing to say.
    pub drift: Option<SharedString>,
}

/// How far HEAD has drifted from its upstream, for the title chip: ` · ↑2 ↓0`
/// when either count is non-zero, nothing when both are zero or unknown. Both
/// arrows once either shows, because `↑2` alone leaves the reader wondering
/// whether the pull side was zero or unread.
pub(crate) fn drift(ahead: Option<u32>, behind: Option<u32>) -> Option<String> {
    let (up, down) = (ahead.unwrap_or(0), behind.unwrap_or(0));
    (up > 0 || down > 0).then(|| format!(" · ↑{up} ↓{down}"))
}

/// Reads [`HeadInfo`] off the model. Pure — the unit-tested half of what the
/// title strip asks about this pane.
fn head_info(head: Option<&HeadState>, local: &[Branch]) -> Option<HeadInfo> {
    match head {
        Some(HeadState::Branch { .. }) => {}
        _ => return None,
    }
    local.iter().find(|b| b.head).map(|b| {
        let branch: SharedString = b.display().into_owned().into();
        let ahead = b.upstream.as_ref().and_then(|u| u.ahead);
        let behind = b.upstream.as_ref().and_then(|u| u.behind);
        HeadInfo {
            chip: format!("⎇ {branch}").into(),
            drift: drift(ahead, behind).map(SharedString::from),
            branch,
            ahead,
            behind,
        }
    })
}

/// [`flatten`] plus what the title strip says about it. The load line goes to
/// stderr like every other view's, and nothing is stored for an overlay that
/// does not read panes.
pub(crate) fn prepare(
    local: Vec<Branch>,
    remotes: Vec<RemoteBranch>,
    head: Option<HeadState>,
    theme: &Theme,
    describe: &str,
) -> Prepared {
    let t = std::time::Instant::now();
    let label = format!(
        "{describe} · {} local · {} remote",
        local.len(),
        remotes.len()
    );
    let rows = flatten(&local, &remotes, head.as_ref(), theme);
    let head = head_info(head.as_ref(), &local);
    eprintln!("branches: {label} · flatten {:.0?}", t.elapsed());
    Prepared { rows, label, head }
}

/// The whole branches panel flattened to rows, plus the title-strip line and
/// who HEAD is.
pub(crate) struct Prepared {
    pub(crate) rows: Vec<Row>,
    /// The title-strip line: who we are and how much there is.
    pub(crate) label: String,
    /// Who HEAD is, read by the window's title strip. `None` while detached.
    pub(crate) head: Option<HeadInfo>,
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
    /// Who HEAD is as of the last refresh, for the window's title strip:
    /// the attached branch and its tracking distance. `None` while detached,
    /// which is a state worth reading on the row above instead of inventing
    /// a branch to name here.
    head: Option<HeadInfo>,
    /// Whether this pane holds the keyboard, as the shell last told it. A
    /// row's bar is accent only when its pane is focused, and the view cannot
    /// ask the shell during render — so the shell writes it here when focus
    /// moves, and render reads a flag.
    focused: bool,
}

/// Where the cursor comes to rest after a move that landed it on `at`.
///
/// A heading is a fact about the grouping and not a thing a verb can aim
/// at, so the keyboard never stops on one: it steps on in the direction it
/// was going, and only when the heading is the list's edge in that direction
/// — `k` from the first branch onto `LOCAL` — does it settle the other way,
/// which keeps `k` on row zero's heading from reading as "nothing happened"
/// and `G` from resting on a `REMOTE` heading with an empty group under it.
/// `dir` is the sign of the move; zero counts as forward.
fn settle(rows: &[Row], at: usize, dir: isize) -> usize {
    let heading = |i: usize| matches!(rows.get(i), Some(Row::Heading { .. }));
    if !heading(at) {
        return at;
    }
    let forward = (at + 1..rows.len()).find(|&i| !heading(i));
    let back = (0..at).rev().find(|&i| !heading(i));
    match dir.is_negative() {
        false => forward.or(back),
        true => back.or(forward),
    }
    .unwrap_or(at)
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
        let Prepared { rows, head, .. } = prepared;
        // Row zero is usually the `LOCAL` heading; the keyboard starts on
        // the first branch under it instead.
        let mut view = Viewport::new();
        view.set_len(rows.len());
        view.go_to(settle(&rows, 0, 1));
        Self {
            data: Rc::new(rows),
            scroll: UniformListScrollHandle::new(),
            view: Rc::new(Cell::new(view)),
            synced: Rc::new(Cell::new(0.0)),
            pending_scroll: PendingScroll::default(),
            rendered: Rc::new(Cell::new(0)),
            armed: None,
            focused: false,
            head,
        }
    }

    /// Whether the repository had nothing to say — the empty state's trigger.
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// The flattened rows of the last refresh, read-only: what the tests
    /// read to see what a refresh actually landed.
    #[cfg(test)]
    pub(crate) fn row_slice(&self) -> &[Row] {
        &self.data
    }

    /// How many rows the list draws — what sizes this pane's sidebar section.
    pub fn rows(&self) -> usize {
        self.data.len()
    }

    /// Who HEAD is, for anything outside this pane: the window's title strip
    /// reads this at most once per frame. Cloned rather than borrowed because
    /// readers sit across an entity boundary; it is one small struct.
    pub fn head_info(&self) -> Option<HeadInfo> {
        self.head.clone()
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
        let Prepared { rows, head, .. } = prepared;
        self.head = head;
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
        // A refresh that lands the cursor on a heading — the branch it was
        // on is gone — steps forward, the way a fresh open does.
        view.go_to(settle(
            &self.data,
            cursor.min(self.data.len().saturating_sub(1)),
            1,
        ));
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
        // Each move carries its sign, so a landing on a heading knows which
        // way to step off it — see [`settle`].
        let dir = match command {
            "view.down" => {
                v.down();
                1
            }
            "view.up" => {
                v.up();
                -1
            }
            "view.page-down" => {
                v.page(1);
                1
            }
            "view.page-up" => {
                v.page(-1);
                -1
            }
            "view.scroll-down" => {
                v.scroll_by(host.view.rows as isize);
                1
            }
            "view.scroll-up" => {
                v.scroll_by(-(host.view.rows as isize));
                -1
            }
            "view.top" => {
                v.to_top();
                1
            }
            "view.bottom" => {
                v.to_bottom();
                -1
            }
            // Answered without doing anything, like the commit graph: a
            // resolved command must not read as a failed one.
            "view.left" | "view.right" => return true,
            _ => return false,
        };
        let settled = settle(&self.data, v.cursor(), dir);
        if settled != v.cursor() {
            v.go_to(settled);
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

impl Render for Branches {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let host = crate::config::host(cx);
        let c = host.theme.chrome;
        // No refs at all is a sentence, not an empty box — an unborn
        // repository's honest answer. Top-left at `ROW_PAD`, like the files
        // and stash panes' lines: this is a short section of the sidebar, and
        // a sentence centred in it would sit where no row ever does.
        if let Some(empty) = self.is_empty().then(|| {
            div()
                .size_full()
                .pl(px(chrome::ROW_PAD))
                .pt_2()
                .flex()
                .items_start()
                // A sentence someone looks for: through `quiet_on`, because
                // raw `faint` is 2.05:1 here and that is not a sentence, it
                // is a gap.
                .text_color(rgb(host.theme.quiet_on(c.bg)))
                .child("no branches yet")
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
        let focused = self.focused;
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
                .map(|i| row(&data[i], &host, i == cursor, focused, Some(i) == armed))
                .collect()
        })
        .track_scroll(&self.scroll)
        // No padding on the list: `ROW_PAD` is the row's own, so the cursor
        // bar sits on the region's edge and the background runs to it.
        .size_full();

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

/// One row, on [`chrome::list_row`]'s furniture: `current` is the keyboard's
/// row and `focused` says whether its bar is the accent or the faint ink.
/// `armed` tints the text toward `chrome.error`, so the thing a second press
/// will destroy is named by its own colour and not only by the band above it.
///
/// A ref row is dot, one character of air, name, and — pushed to the right
/// edge — the tracking distance. The name is the one thing that gives:
/// `min_w_0` and `truncate` let it end in an ellipsis rather than shove the
/// distance out of the pane, because `↑2` is the fact a narrow sidebar is
/// being glanced at for and a name's tail is not. A heading is
/// [`chrome::section_label`] and never the cursor's — see [`settle`].
fn row(e: &Row, host: &Host, current: bool, focused: bool, armed: bool) -> AnyElement {
    let ch = host.font.char_width();
    let c = host.theme.chrome;
    // The dot was decided beside the text, at flatten; the draw only paints
    // it. One character wide plus one of air, so every name aligns.
    let dot = |d: &Dot| {
        div()
            .flex_none()
            .w(px(ch))
            .mr(px(ch))
            .text_color(rgb(d.color))
            .child(SharedString::from(d.glyph))
    };
    let name = |text: SharedString, ink: Option<Rgb>| {
        div()
            .min_w_0()
            .truncate()
            .when_some(ink, |d, ink| d.text_color(rgb(ink)))
            .when(armed, |d| d.text_color(rgb(c.error)))
            .child(text)
    };
    match e {
        Row::Heading { count, section } => {
            chrome::section_label(host, section.label().into(), Some(count.clone()), ROW_H)
                .into_any_element()
        }
        Row::Detached { dot: d, text } => chrome::list_row(host, current, focused, ROW_H)
            .child(dot(d))
            .child(name(text.clone(), Some(c.dim)))
            .into_any_element(),
        Row::Local(l) => chrome::list_row(host, current, focused, ROW_H)
            .child(dot(&l.dot))
            .child(name(l.name_text.clone(), None))
            .children(l.counts.clone().map(|text| {
                div()
                    .flex_none()
                    // The auto margin carries the distance to the row's far
                    // end however wide its name ran; the padding is the floor
                    // under that — the air a squeezed name still leaves.
                    .ml_auto()
                    .pl(px(ch))
                    .pr_2()
                    .text_color(rgb(match l.gone {
                        // "gone" is read — it is why the upstream is not shown
                        // — so quiet through `quiet_on`, not invisible.
                        true => host.theme.quiet_on(c.bg),
                        false => c.dim,
                    }))
                    .child(text)
            }))
            .into_any_element(),
        Row::Remote(r) => chrome::list_row(host, current, focused, ROW_H)
            .child(dot(&r.dot))
            .child(name(r.label.clone(), Some(c.dim)))
            .into_any_element(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        drift, flatten, head_info, prepare, row_target, Branches, HeadInfo, Prepared, Row, Target,
    };
    use gitten_core::host::Host;
    use gitten_core::refs::{Branch, HeadState, RefName, RemoteBranch, Upstream};
    use gitten_core::theme::Theme;

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

    /// Headings and rows in draw order — the shape the tests read. The dots'
    /// glyphs ride along because a column is only useful while it aligns;
    /// each row's *colour* argument stays beside that row, in its own test.
    fn outline(rows: &[Row]) -> Vec<String> {
        rows.iter()
            .map(|r| match r {
                Row::Detached { dot, text } => format!("{}[detached {text}]", dot.glyph),
                Row::Heading { count, section } => {
                    format!("[{}·{count}]", section.name())
                }
                Row::Local(l) => match &l.counts {
                    Some(c) => format!("{}{} {c}", l.dot.glyph, l.name_text),
                    None => format!("{}{}", l.dot.glyph, l.name_text),
                },
                Row::Remote(r) => format!("{}{}", r.dot.glyph, r.label),
            })
            .collect()
    }

    #[test]
    fn locals_come_first_and_the_remote_group_is_quiet_but_there() {
        let t = Theme::dark();
        let rows = flatten(
            &[local("feature", false), local("main", true)],
            &[remote("main"), remote("wip")],
            None,
            &t,
        );
        assert_eq!(
            outline(&rows),
            vec![
                "[local·2]",
                "●feature",
                "●main",
                "[remote·2]",
                "○origin/main",
                "○origin/wip",
            ]
        );
        // The dots say what every row is: HEAD's branch alone wears the
        // accent, another local keeps the lane ink for its place among the
        // locals, and a fetched copy draws hollow and faint.
        match (&rows[1], &rows[2]) {
            (Row::Local(feature), Row::Local(main)) => {
                assert_eq!(
                    feature.dot.color,
                    t.lane(0),
                    "the first local takes the first lane ink"
                );
                assert_eq!(
                    main.dot.color, t.chrome.accent,
                    "HEAD alone earns the accent"
                );
            }
            other => panic!("two local rows expected, got {other:?}"),
        }
        for row in [&rows[4], &rows[5]] {
            match row {
                Row::Remote(r) => assert_eq!(
                    (r.dot.glyph, r.dot.color),
                    ("○", t.quiet_on(t.chrome.bg)),
                    "a remote copy is hollow and quiet — quiet, not invisible"
                ),
                other => panic!("a remote row expected, got {other:?}"),
            }
        }
        // An empty group draws no heading at all.
        assert_eq!(
            outline(&flatten(&[local("main", true)], &[], None, &t)),
            vec!["[local·1]", "●main"]
        );
    }

    #[test]
    fn a_detached_head_is_its_own_top_row_not_a_hidden_state() {
        let t = Theme::dark();
        let rows = flatten(
            &[local("main", false)],
            &[],
            Some(&HeadState::Detached { commit: sha() }),
            &t,
        );
        assert_eq!(
            outline(&rows)[0],
            "●[detached (detached at 01234567…)]",
            "abbreviated once, at flatten"
        );
        // A detached state is real but not alive: dim where a checked-out
        // branch would glow.
        match &rows[0] {
            Row::Detached { dot, .. } => assert_eq!(dot.color, t.chrome.dim),
            other => panic!("the detached row expected, got {other:?}"),
        }
        // And attached heads put no such row anywhere.
        let attached = flatten(
            &[local("main", true)],
            &[],
            Some(&HeadState::Branch {
                name: RefName::from("main"),
                commit: None,
            }),
            &t,
        );
        assert!(!outline(&attached).iter().any(|r| r.contains("detached")));
    }

    #[test]
    fn tracking_speaks_in_arrows_and_zeros_stay_silent() {
        let t = Theme::dark();
        let rows = flatten(
            &[
                tracked("synced", false, Some(0), Some(0)),
                tracked("ahead", false, Some(2), Some(0)),
                tracked("behind", true, Some(0), Some(3)),
                tracked("both", false, Some(1), Some(4)),
            ],
            &[],
            None,
            &t,
        );
        assert_eq!(
            outline(&rows),
            vec![
                "[local·4]",
                "●synced",
                "●ahead ↑2",
                "●behind ↓3",
                "●both ↑1 ↓4",
            ],
            "distance only — the ref itself is the remote group's job"
        );
        // Fully in sync draws nothing at all: `None`, not an empty string.
        match &rows[1] {
            Row::Local(l) => assert_eq!(l.counts, None, "an in-sync branch is bare"),
            other => panic!("the synced row expected, got {other:?}"),
        }
    }

    #[test]
    fn a_gone_upstream_is_named_gone_rather_than_reading_as_zero() {
        // ahead/behind `None` with the pair still configured: the ref the
        // branch tracks no longer exists locally. A `0` here would invite a
        // push that fixes nothing.
        let t = Theme::dark();
        let rows = flatten(&[tracked("old", false, None, None)], &[], None, &t);
        assert_eq!(outline(&rows), vec!["[local·1]", "●old (gone)"]);
        // The row remembers why, for the faint ink the draw gives it.
        match &rows[1] {
            Row::Local(l) => {
                assert!(l.gone);
                assert_eq!(l.counts.as_deref(), Some("(gone)"));
            }
            other => panic!("the tracked row expected, got {other:?}"),
        }
    }

    #[test]
    fn names_keep_their_bytes_and_display_lossily_once() {
        // Latin-1 é and ø: legal ref bytes, illegal UTF-8.
        let t = Theme::dark();
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
            &t,
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
        let t = Theme::dark();
        let rows = flatten(
            &[local("main", true)],
            &[remote("main")],
            Some(&HeadState::Detached { commit: sha() }),
            &t,
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
            &host.theme,
            "",
        ));
        b.rendered.set(3);
        let mut v = b.view.get();
        v.set_len(b.data.len());
        v.set_height(3);
        b.view.set(v);
        // The keyboard opens on `feature`, past the heading; down is `main`.
        assert!(b.run_view("view.down", &host));
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
            &host.theme,
            "",
        ));
        b.rendered.set(3);
        let mut v = b.view.get();
        v.set_len(b.data.len());
        v.set_height(3);
        b.view.set(v);
        // The keyboard opens on `feature`, past the heading; down is `main`.
        assert!(b.run_view("view.down", &host));
        let target = b.current().expect("a branch under the keyboard");
        assert!(!b.confirm_or_arm_delete(&target));
        assert_eq!(b.armed_row(), Some(target.clone()));

        // A refresh that changes nothing at all still says "things moved".
        b.replace_prepared(
            prepare(
                vec![local("feature", false), local("main", true)],
                Vec::new(),
                None,
                &host.theme,
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
            prepare(vec![local("main", true)], Vec::new(), None, &host.theme, ""),
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
        let host = Host::new();
        let p = prepare(
            vec![local("main", true)],
            vec![remote("main")],
            None,
            &host.theme,
            "gitten (main)",
        );
        assert_eq!(p.label, "gitten (main) · 1 local · 1 remote");

        let empty = prepare(Vec::new(), Vec::new(), None, &host.theme, "gitten");
        assert_eq!(empty.label, "gitten · 0 local · 0 remote");
        assert_eq!(empty.rows.len(), 0);
        assert_eq!(empty.head, None, "no branch, nothing to name");

        // And the prepared type is what a refresh hands the pane.
        let p: Prepared = p;
        assert!(!p.rows.is_empty());
    }

    #[test]
    fn head_info_names_the_attached_branch_and_hands_through_its_distance() {
        let host = Host::new();
        let attached = HeadState::Branch {
            name: RefName::from("main"),
            commit: None,
        };
        let p = prepare(
            vec![tracked("main", true, Some(1), Some(2))],
            Vec::new(),
            Some(attached.clone()),
            &host.theme,
            "",
        );
        assert_eq!(
            p.head,
            Some(HeadInfo {
                branch: "main".into(),
                ahead: Some(1),
                behind: Some(2),
                chip: "⎇ main".into(),
                drift: Some(" · ↑1 ↓2".into()),
            }),
            "the numbers core measured, verbatim — and the chip spelled once"
        );
        // And the pane hands it on for the title strip.
        let b = Branches::from_prepared(p);
        let hi = b.head_info().expect("attached HEAD has a name");
        assert_eq!(&*hi.branch, "main");
        assert_eq!((hi.ahead, hi.behind), (Some(1), Some(2)));

        // A gone upstream stays honest: unknowable is not zero, and the
        // title strip must not invent a badge off a missing ref.
        let hi = head_info(Some(&attached), &[tracked("main", true, None, None)])
            .expect("gone still names its branch");
        assert_eq!(
            (hi.ahead, hi.behind),
            (None, None),
            "a vanished ref measures to nothing"
        );
        assert_eq!(hi.drift, None, "and the chip invents no zeros for it");
    }

    #[test]
    fn drift_shows_both_arrows_once_either_is_non_zero_and_nothing_otherwise() {
        assert_eq!(drift(Some(2), Some(0)).as_deref(), Some(" · ↑2 ↓0"));
        assert_eq!(drift(None, Some(3)).as_deref(), Some(" · ↑0 ↓3"));
        assert_eq!(drift(Some(0), Some(0)), None);
        assert_eq!(drift(None, None), None);
    }

    #[test]
    fn head_info_says_nothing_while_detached_or_unmarked() {
        let host = Host::new();
        // Detached: there is no branch to name, and the row above says so
        // better than an invented one would.
        let p = prepare(
            vec![local("main", false)],
            Vec::new(),
            Some(HeadState::Detached { commit: sha() }),
            &host.theme,
            "",
        );
        assert_eq!(p.head, None);

        // Attached to a name no local row claims — the unborn-branch shape:
        // nothing invented to fill the slot either way.
        let bare = prepare(
            vec![local("other", false)],
            Vec::new(),
            Some(HeadState::Branch {
                name: RefName::from("ghost"),
                commit: None,
            }),
            &host.theme,
            "",
        );
        assert_eq!(bare.head, None);

        let none: Option<HeadInfo> = None;
        assert_eq!(Branches::from_prepared(bare).head_info(), none);
    }

    #[test]
    fn lane_inks_follow_a_locals_place_among_locals_head_excluded() {
        // The colour a branch carries through the pane belongs to the *row*,
        // not to HEAD: inserting a branch above shifts the ink down with it,
        // and HEAD never borrows the cycle whatever its place.
        let t = Theme::dark();
        let rows = flatten(
            &[
                tracked("held", true, Some(3), Some(0)),
                local("second", false),
                local("third", false),
            ],
            &[],
            None,
            &t,
        );
        match (&rows[1], &rows[2], &rows[3]) {
            (Row::Local(head), Row::Local(second), Row::Local(third)) => {
                assert_eq!(second.name.as_bytes(), b"second");
                assert_eq!(
                    head.dot.color, t.chrome.accent,
                    "HEAD sits out of the cycle"
                );
                assert_eq!(
                    second.dot.color,
                    t.lane(1),
                    "its index counts from the first local, marked or not"
                );
                assert_eq!(third.dot.color, t.lane(2));
            }
            other => panic!("three local rows expected, got {other:?}"),
        }
    }

    /// A pane over `[LOCAL·2] a b [REMOTE·1] origin/a`, three rows tall.
    fn pane() -> (Branches, Host) {
        let host = Host::new();
        let b = Branches::from_prepared(prepare(
            vec![local("a", true), local("b", false)],
            vec![remote("a")],
            None,
            &host.theme,
            "",
        ));
        b.rendered.set(3);
        let mut v = b.view.get();
        v.set_len(b.data.len());
        v.set_height(3);
        b.view.set(v);
        (b, host)
    }

    fn at(b: &Branches) -> usize {
        b.view.get().cursor()
    }

    #[test]
    fn the_cursor_opens_on_the_first_branch_and_never_rests_on_a_heading() {
        let (mut b, host) = pane();
        // Row 0 is `LOCAL`; the keyboard starts under it.
        assert_eq!(at(&b), 1);
        assert_eq!(b.current(), Some(Target::Local(RefName::from("a"))));

        // `j` twice: b, then over the `REMOTE` heading onto origin/a.
        assert!(b.run_view("view.down", &host));
        assert_eq!(at(&b), 2);
        assert!(b.run_view("view.down", &host));
        assert_eq!(at(&b), 4, "down skipped the heading in its own direction");

        // `k` back: over the heading again, landing on b.
        assert!(b.run_view("view.up", &host));
        assert_eq!(at(&b), 2, "up skipped the heading in its own direction");

        // `k` from the first branch lands on `LOCAL`, which is the edge —
        // so it settles forward and the cursor stays where it was.
        assert!(b.run_view("view.up", &host));
        assert!(b.run_view("view.up", &host));
        assert_eq!(at(&b), 1, "the top heading is not a resting place");

        // Jumps obey the same rule: `gg` is the first branch, `G` the last.
        assert!(b.run_view("view.bottom", &host));
        assert_eq!(at(&b), 4);
        assert!(b.run_view("view.top", &host));
        assert_eq!(at(&b), 1);
    }

    #[test]
    fn a_refresh_that_strands_the_cursor_on_a_heading_steps_off_it() {
        let (mut b, host) = pane();
        // Onto b.
        assert!(b.run_view("view.down", &host));
        // b vanishes and a remote arrives at its place in the list: the
        // clamped row is now the `REMOTE` heading, and the cursor may not
        // stay there.
        b.replace_prepared(
            prepare(
                vec![local("a", true)],
                vec![remote("a"), remote("b")],
                None,
                &host.theme,
                "",
            ),
            &host,
        );
        assert!(
            b.current().is_some(),
            "the cursor rests on a heading after a refresh: row {}",
            at(&b)
        );
    }

    #[test]
    fn settle_is_the_identity_off_a_heading_and_survives_a_heading_only_list() {
        let t = Theme::dark();
        let rows = flatten(&[local("a", true)], &[remote("a")], None, &t);
        for i in [1, 3] {
            assert_eq!(super::settle(&rows, i, 1), i);
            assert_eq!(super::settle(&rows, i, -1), i);
        }
        // Nothing to settle onto: the input comes back, and nothing panics.
        let only = vec![Row::Heading {
            count: "0".into(),
            section: super::Section::Local,
        }];
        assert_eq!(super::settle(&only, 0, 1), 0);
        assert_eq!(super::settle(&only, 0, -1), 0);
        assert_eq!(super::settle(&[], 0, 1), 0);
    }
}
