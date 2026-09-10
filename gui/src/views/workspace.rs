//! The guide-v2 workspace shell: the window's middle region — a full-height
//! sidebar beside a 76px destination header and diff/inspector row.
//! `"workspace.changes"` shows Changes, `"workspace.history"` shows the
//! in-workspace History timeline beside the selected commit's diff.
//!
//! What lives here is geometry and state only: the sidebar's rows come from
//! [`super::files::Files`]' grouped projection (one selection state, shared),
//! the center is a plain [`super::diff::Diff`] fed one file's rows, and every
//! mouse control resolves through the shell's named dispatch. Widths are
//! fixed px per the spec — 255/266px at an ordinary size, 280/295px past
//! [`WIDE_PX`], and narrower below the three `max-width` steps, where the
//! reference spends less on a rail rather than on the diff — read from the
//! viewport once at composition time, never from inside a view.
//!
//! The reference stacks the composer under the diff below 850px; this window
//! deliberately does not. The spec calls that breakpoint "a narrow-width
//! reference, not a mandate to degrade desktop usability", and a window that
//! rearranges itself at 849px is a worse window on a desktop that is merely
//! narrow. The rails give instead, which costs neither pane its reason.

use super::diff::Diff;
use super::files::Section;
use crate::input::Input;
use gitten_core::status::PathBytes;
use gpui::{point, px, Bounds, Entity, Pixels, Subscription, UniformListScrollHandle};
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
/// Below these the reference narrows the rails rather than the diff, which is
/// the right way round: a file list is scanned by name and a diff is read a
/// line at a time, so the pane that loses is the one that can still be read.
/// The numbers are `style.css`'s `max-width` steps against the same
/// breakpoints, held here as one ladder so no view can invent a rung.
pub const COMPACT_PX: f32 = 1150.0;
pub const NARROW_PX: f32 = 850.0;
pub const SMALLEST_PX: f32 = 650.0;
pub const SIDEBAR_COMPACT_W: f32 = 225.0;
pub const SIDEBAR_NARROW_W: f32 = 195.0;
pub const SIDEBAR_SMALLEST_W: f32 = 152.0;
pub const INSPECTOR_COMPACT_W: f32 = 230.0;

/// Fixed rail widths for a viewport: the spec's five sizes, picked once at
/// composition time — the one place a viewport read is allowed. The top step
/// is the spec's own wording ("above 1550px"); the rest are its `max-width`
/// steps, checked from the narrow end so the smallest viewport cannot fall
/// through to the widest rung.
pub fn sidebar_width(viewport_w: f32) -> f32 {
    if viewport_w > WIDE_PX {
        SIDEBAR_WIDE_W
    } else if viewport_w <= SMALLEST_PX {
        SIDEBAR_SMALLEST_W
    } else if viewport_w <= NARROW_PX {
        SIDEBAR_NARROW_W
    } else if viewport_w <= COMPACT_PX {
        SIDEBAR_COMPACT_W
    } else {
        SIDEBAR_W
    }
}

/// [`sidebar_width`]'s twin for the right rail. Sized here — beside the
/// sidebar's own width — so the center never lays out against a width the
/// inspector will move. The inspector has one rung fewer than the sidebar:
/// the reference gives up on the diff's line numbers sooner than on its
/// file names, so the composer stops stepping where the sidebar keeps going.
pub fn inspector_width(viewport_w: f32) -> f32 {
    if viewport_w > WIDE_PX {
        INSPECTOR_WIDE_W
    } else if viewport_w <= COMPACT_PX {
        INSPECTOR_COMPACT_W
    } else {
        INSPECTOR_W
    }
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
            summary: None,
            description: None,
            fields_key: None,
            field_subs: Vec::new(),
        }
    }
}

/// The box a `uniform_list` was last painted in — what the wheel hit-tests a
/// list by before the list's own body runs. Zero until the first prepaint, so
/// nothing can be scrolled through a region that has never been drawn; and the
/// *stale* bounds of a list that has stopped being drawn are why the wheel
/// picks its region by destination and not by whichever handle answers first.
pub fn list_bounds(scroll: &UniformListScrollHandle) -> Bounds<Pixels> {
    scroll.0.borrow().base_handle.bounds()
}

/// Moves a list by `dy` pixels — the wheel's own pixels, never a row step.
///
/// A rail row and a timeline row are both taller than the center's, and the
/// platform reports points, so a row is the wrong unit twice over; and a list
/// that moves in whole rows cannot show the half-row a slow flick is asking
/// for. The offset convention is the diff's, so the arithmetic is too:
/// positive pixels move up the document, and the handle clamps to what exists.
///
/// The newer intent wins. A request parked by a keyboard-follow scroll but not
/// yet consumed by the prepaint is dropped, exactly as a wheel drops the
/// diff's own parked request — otherwise the row lands a frame later and drags
/// the list back out from under the finger. Returns whether anything moved,
/// which is what decides a redraw.
pub fn wheel_pixels(scroll: &UniformListScrollHandle, dy: f32) -> bool {
    let mut state = scroll.0.borrow_mut();
    state.deferred_scroll_to_item = None;
    let offset = state.base_handle.offset();
    let y = (f32::from(offset.y) + dy).clamp(-f32::from(state.base_handle.max_offset().y), 0.0);
    if y == f32::from(offset.y) {
        return false;
    }
    state.base_handle.set_offset(point(offset.x, px(y)));
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::ScrollStrategy;

    /// The rail ladder at every rung and at both of its edges, plus the
    /// property a ladder is for: a viewport that grows never gets a narrower
    /// rail. Both edges are checked because a rung is a `<=` on the way down
    /// and a `>` on the way up, and only one of those two mistakes is a size
    /// nobody would notice.
    #[test]
    fn the_rails_step_with_the_viewport_and_never_grow_downward() {
        for (viewport, sidebar, inspector) in [
            (400.0, SIDEBAR_SMALLEST_W, INSPECTOR_COMPACT_W),
            (SMALLEST_PX, SIDEBAR_SMALLEST_W, INSPECTOR_COMPACT_W),
            (SMALLEST_PX + 1.0, SIDEBAR_NARROW_W, INSPECTOR_COMPACT_W),
            (NARROW_PX, SIDEBAR_NARROW_W, INSPECTOR_COMPACT_W),
            (NARROW_PX + 1.0, SIDEBAR_COMPACT_W, INSPECTOR_COMPACT_W),
            (COMPACT_PX, SIDEBAR_COMPACT_W, INSPECTOR_COMPACT_W),
            (COMPACT_PX + 1.0, SIDEBAR_W, INSPECTOR_W),
            (WIDE_PX, SIDEBAR_W, INSPECTOR_W),
            (WIDE_PX + 1.0, SIDEBAR_WIDE_W, INSPECTOR_WIDE_W),
        ] {
            assert_eq!(sidebar_width(viewport), sidebar, "sidebar at {viewport}");
            assert_eq!(
                inspector_width(viewport),
                inspector,
                "inspector at {viewport}"
            );
        }
        let mut last = 0.0;
        for step in 0..300 {
            let viewport = 400.0 + step as f32 * 10.0;
            let side = sidebar_width(viewport);
            assert!(side >= last, "the sidebar shrank at {viewport}");
            assert!(side <= SIDEBAR_WIDE_W, "the sidebar overgrew at {viewport}");
            last = side;
        }
    }

    /// The launch destination per the interaction contract: Changes before
    /// any command runs — no toggle to reach it.
    #[test]
    fn the_workspace_is_the_launch_destination() {
        let ws = Workspace::default();
        assert_eq!(ws.destination, Destination::Changes);
    }

    /// The rail's wheel is the newer intent, so it drops a keyboard-follow
    /// request that prepaint has not consumed yet — the way the diff's wheel
    /// drops its own. Left standing, that request lands a frame later and
    /// drags the list back out from under the finger that just moved it.
    #[test]
    fn a_wheel_drops_a_parked_keyboard_request() {
        let ws = Workspace::default();
        assert!(
            !wheel_pixels(&ws.sidebar_scroll, -12.5),
            "nowhere to scroll, nowhere moved"
        );

        ws.sidebar_scroll.scroll_to_item(3, ScrollStrategy::Nearest);
        assert!(ws
            .sidebar_scroll
            .0
            .borrow()
            .deferred_scroll_to_item
            .is_some());
        // Headless, the handle has no bounds and reports nowhere to go; the
        // request must be gone either way.
        assert!(!wheel_pixels(&ws.sidebar_scroll, -12.5));
        assert!(ws
            .sidebar_scroll
            .0
            .borrow()
            .deferred_scroll_to_item
            .is_none());
    }
}
