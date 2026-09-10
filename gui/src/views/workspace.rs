//! The guide-v2 workspace shell: the window's middle region — a full-height
//! sidebar beside a 76px destination header and diff/inspector row.
//! `"workspace.changes"` shows Changes, `"workspace.history"` shows the
//! in-workspace History timeline beside the selected commit's diff.
//!
//! What lives here is geometry and state only: the sidebar's rows come from
//! [`super::files::Files`]' grouped projection (one selection state, shared),
//! the center is a plain [`super::diff::Diff`] fed one file's rows, and every
//! mouse control resolves through the shell's named dispatch. Widths are
//! fixed px per the spec — 255/266px, 280/295px past [`WIDE_PX`] — read from
//! the viewport once at composition time, never from inside a view.

use super::diff::Diff;
use super::files::Section;
use crate::input::Input;
use gitten_core::status::PathBytes;
use gpui::{Entity, Subscription, UniformListScrollHandle};
use std::cell::Cell;

/// The destination header's height: title, real counts, working-copy status.
pub const HEADER_H: f32 = 76.0;
/// Sidebar and inspector widths at ordinary desktop sizes.
pub const SIDEBAR_W: f32 = 255.0;
pub const INSPECTOR_W: f32 = 266.0;
/// Past this viewport width the spec spends more on both rails.
pub const WIDE_PX: f32 = 1550.0;
pub const SIDEBAR_WIDE_W: f32 = 280.0;
pub const INSPECTOR_WIDE_W: f32 = 295.0;

/// Fixed rail widths for a viewport: the spec's two sizes, picked once at
/// composition time — the one place a viewport read is allowed.
pub fn sidebar_width(viewport_w: f32) -> f32 {
    match viewport_w > WIDE_PX {
        true => SIDEBAR_WIDE_W,
        false => SIDEBAR_W,
    }
}

/// [`sidebar_width`]'s twin for the right rail. Sized here — beside the
/// sidebar's own width — so the center never lays out against a width the
/// inspector will move.
pub fn inspector_width(viewport_w: f32) -> f32 {
    match viewport_w > WIDE_PX {
        true => INSPECTOR_WIDE_W,
        false => INSPECTOR_W,
    }
}

/// One wheel step in grouped-row space: from the mirrored top and the
/// accumulated pixels, the next top and the leftover remainder — or `None`
/// when the pixels haven't filled a row yet and stay banked. Pure, so the
/// settle arithmetic is a test instead of only a gesture.
///
/// Positive pixels scroll up, toward lower indices — the center's own sign
/// convention. A step clamped against a bound forgets its remainder, or
/// the first flick back jumps by the stored distance.
pub fn wheel_step(top: usize, acc: f32, row_h: f32, max: usize) -> Option<(usize, f32)> {
    let step = (-acc / row_h).trunc() as isize;
    if step == 0 {
        return None;
    }
    let next = (top as isize + step).clamp(0, max as isize) as usize;
    let rest = match next == top {
        true => 0.0,
        false => acc + step as f32 * row_h,
    };
    Some((next, rest))
}

/// Reconcile the mirrored top with the rail's actual position, at a scroll
/// decision point — never per frame.
///
/// The mirror is stepped beside every wheel-issued request, but the list
/// also moves by paths that never touch it (the keyboard-follow scroll in
/// `sync_workspace_preview`, a future scrollbar). Reading the handle's
/// settled pixel offset and truncating to rows re-lands the mirror on the
/// row the window actually shows.
///
/// One exception: while a programmatic request is still parked (it lays out
/// on the next frame, not this one), the offset still names the *previous*
/// position while the mirror already names the intent — trusting the offset
/// then would un-step a step. A parked request keeps the mirror
/// authoritative.
pub fn reconcile_top(
    scroll: &UniformListScrollHandle,
    mirror: usize,
    row_h: f32,
    max: usize,
) -> usize {
    let state = scroll.0.borrow();
    if state.deferred_scroll_to_item.is_some() {
        return mirror;
    }
    let y = f32::from(state.base_handle.offset().y);
    (-y / row_h).trunc().clamp(0.0, max as f32) as usize
}

/// Which destination the workspace shows. Changes is the default per the
/// interaction contract; History is the branch timeline beside the selected
/// commit's diff, in the same workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Destination {
    #[default]
    Changes,
    History,
}

/// The workspace's shell-side state: destination, center view and preview
/// guard. The sidebar holds no state of its own — it draws the files pane's
/// grouped projection under the files pane's cursor.
pub struct Workspace {
    /// The destination the header names and the sidebar follows.
    pub destination: Destination,
    /// The center diff: built once on first entry, re-aimed per selection —
    /// never rebuilt, which is what keeps its scroll state and presentation
    /// across files, the same promise the main view keeps across commits.
    pub center: Option<Entity<Diff>>,
    /// Newest file-preview request. A schedule bumps it; a load applies only
    /// if it still equals the value it left with, so a fast cursor run
    /// collapses to exactly one load — the latest row's. Same guard shape
    /// as the main view's `request`, on its own counter.
    pub request: u64,
    /// What the last scheduled preview was of: section, path and the files
    /// pane's refresh generation together. A refresh wave re-lands the same
    /// selection with a newer generation, which is a new key — staging a
    /// hunk re-aims the preview at the side that just moved.
    pub last: Option<(Section, PathBytes, u64)>,
    /// The sidebar list's own scroll handle. The sidebar shares the files
    /// pane's *cursor* but pans its own rows: grouped space has its own
    /// addresses, so the stack list's handle cannot serve it.
    pub sidebar_scroll: UniformListScrollHandle,
    /// The History timeline's scroll handle, for the same reason: the
    /// timeline is a projection of the commits pane with its own row
    /// addresses, so the pane's list handle cannot serve it.
    pub history_scroll: UniformListScrollHandle,
    /// The row the timeline last scrolled to. The commits pane moves its own
    /// cursor; the timeline follows by parking a scroll when this differs —
    /// never by scrolling on every frame.
    pub history_cursor: Cell<usize>,
    /// Sub-row wheel remainder for the rail above: trackpad deltas smaller
    /// than one row accumulate here until they spend. Beside the handle it
    /// feeds, zeroed whenever a step clamps against a bound.
    pub sidebar_px: Cell<f32>,
    /// Mirror of the rail's top index, stepped beside every wheel-issued
    /// request. The handle's own top getter is test-gated upstream
    /// (`#[cfg(any(test, feature = "test-support"))]`), so production reads
    /// this instead — reconciled against the handle's actual offset at each
    /// wheel decision by [`reconcile_top`], because the keyboard-follow
    /// scroll moves the list without stepping it.
    pub sidebar_top: Cell<usize>,
    /// The inspector's two fields, built once on first entry and refilled
    /// from the draft store whenever the repository changes. Owned here —
    /// beside the center view they serve — rather than in the modal prompt
    /// slot, which spends its field on accept while these survive it.
    pub summary: Option<Entity<Input>>,
    pub description: Option<Entity<Input>>,
    /// Which repository key the fields were last filled for. A switch
    /// refills them from that repository's draft instead of leaking the
    /// previous one's unsent words across.
    pub fields_key: Option<String>,
    /// The draft-mirroring subscriptions on the two fields above. Stored —
    /// not detached — so rebuilding the fields (which only a repository
    /// switch does) drops the old pair instead of leaking a writer per
    /// switch onto entities nobody reads again.
    pub field_subs: Vec<Subscription>,
}

impl Default for Workspace {
    fn default() -> Self {
        Self {
            destination: Destination::Changes,
            center: None,
            request: 0,
            last: None,
            sidebar_scroll: UniformListScrollHandle::new(),
            history_scroll: UniformListScrollHandle::new(),
            history_cursor: Cell::new(usize::MAX),
            sidebar_px: Cell::new(0.0),
            sidebar_top: Cell::new(0),
            summary: None,
            description: None,
            fields_key: None,
            field_subs: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, px, ScrollStrategy};

    /// The launch destination per the interaction contract: Changes before
    /// any command runs — no toggle to reach it.
    #[test]
    fn the_workspace_is_the_launch_destination() {
        let ws = Workspace::default();
        assert_eq!(ws.destination, Destination::Changes);
    }

    /// A step onto an already-visible row still moves the mirror: two full
    /// rows down from row 5 walks it to 7 with nothing banked — and the
    /// wheel path parks that step strict, so the window follows instead of
    /// sitting still while the mirror walks away from it.
    #[test]
    fn a_step_onto_an_already_visible_row_still_moves_the_top() {
        assert_eq!(wheel_step(5, -44.0, 22.0, 39), Some((7, 0.0)));
        // Sub-row pixels stay banked: no request, no move.
        assert_eq!(wheel_step(5, -10.0, 22.0, 39), None);
        // Upward pixels walk toward lower indices, keeping the leftover.
        assert_eq!(wheel_step(5, 30.0, 22.0, 39), Some((4, 8.0)));
        // Clamped against the top bound: the top holds and the remainder
        // is forgotten, or the first flick back jumps.
        assert_eq!(wheel_step(0, 44.0, 22.0, 39), Some((0, 0.0)));
    }

    /// The mirror follows a scroll it did not issue: a settled list parked
    /// at row 10 by another path re-lands a stale mirror of 3 — unless a
    /// programmatic request is still parked, in which case the mirror names
    /// the intent and the not-yet-consumed offset must not un-step it.
    #[test]
    fn the_mirror_follows_a_scroll_it_did_not_issue() {
        let scroll = UniformListScrollHandle::new();
        scroll
            .0
            .borrow_mut()
            .base_handle
            .set_offset(point(px(0.), px(-220.0)));
        assert_eq!(reconcile_top(&scroll, 3, 22.0, 39), 10);
        scroll.scroll_to_item_strict(7, ScrollStrategy::Top);
        assert_eq!(reconcile_top(&scroll, 7, 22.0, 39), 7);
    }

    /// Contract pin on the API the wheel path relies on: steps park with
    /// `scroll_to_item_strict`, so an already-visible target still moves.
    /// If GPUI ever changes what strict means, this fails loudly instead
    /// of silently reintroducing the spend-without-moving desync.
    #[test]
    fn sidebar_steps_park_strict_requests() {
        let scroll = UniformListScrollHandle::new();
        scroll.scroll_to_item_strict(7, ScrollStrategy::Top);
        let strict = scroll
            .0
            .borrow()
            .deferred_scroll_to_item
            .as_ref()
            .map(|d| d.scroll_strict)
            .unwrap_or(false);
        assert!(strict, "the parked step must be strict");
    }
}
