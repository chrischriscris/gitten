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
use gpui::{
    point, px, Bounds, Entity, Pixels, SpringAnimation, SpringConfig, Subscription,
    UniformListScrollHandle,
};
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

/// The motion every rail shares: one spring, one target vocabulary — a rail
/// is either present (`1.0`) or gone (`0.0`), and the fraction is resolved
/// against the live width inside each animator, so a window dragged while a
/// rail is sliding still lands on the spec's rung exactly. Slightly
/// underdamped (ζ ≈ 0.9): quick, with a whisper of give rather than a dead
/// stop — a rail's own width is the thing being stepped, so the same curve
/// reads the same at 152px and at 280px.
pub const RAIL_SPRING: SpringConfig = SpringConfig::new(280.0, 30.0, 1.0);

/// A rail's spring aimed at its flag. The element the spring drives is
/// never removed from the tree — collapsed is a width of zero, not an
/// absent child — which is what lets a re-opened rail pick the spring's
/// velocity up mid-flight instead of restarting, and what makes a first
/// mount (launched, or a destination entered with the rail already hidden)
/// start *at* the target and stay still rather than playing a slide nobody
/// asked for.
pub fn rail_spring(collapsed: bool) -> SpringAnimation<f32> {
    SpringAnimation::new(RAIL_SPRING).to(if collapsed { 0.0 } else { 1.0 })
}

/// The branch timeline's spec width — History's inner rail. It lives beside
/// the other rails' sizes so the drag helpers read one table.
pub const TIMELINE_W: f32 = 310.0;

/// The drag limits per rail. The floor is the smallest size the rail's own
/// content is laid out for — the sidebar's spec already steps down to it on
/// the narrowest windows — and the cap is only a sanity bound; the drawn
/// width is further limited to half the window, so a width remembered from a
/// bigger one can never swallow this one.
pub const SIDEBAR_MIN_W: f32 = SIDEBAR_SMALLEST_W;
pub const SIDEBAR_MAX_W: f32 = 460.0;
pub const INSPECTOR_MIN_W: f32 = INSPECTOR_COMPACT_W;
pub const INSPECTOR_MAX_W: f32 = 460.0;
pub const TIMELINE_MIN_W: f32 = 200.0;
pub const TIMELINE_MAX_W: f32 = 520.0;

/// How many pixels of a rail's receding edge answer to a drag — the strip
/// the cursor handle paints and the band the probe reads.
pub const RAIL_GRAB_W: f32 = 5.0;

/// A rail's drawn width from a remembered drag: the override clamped to the
/// rail's floor and the lesser of its cap and half the window, or the spec
/// width when nothing was dragged. The override is kept as dragged — the
/// clamp is applied at draw time, so the preference survives the window that
/// cannot currently afford it.
pub fn rail_draw_width(over: Option<f32>, min: f32, max: f32, viewport_w: f32, spec: f32) -> f32 {
    let max = max.min(viewport_w * 0.5).max(min);
    over.unwrap_or(spec).clamp(min, max)
}

/// [`rail_draw_width`] for the left rail, on the spec's rungs.
pub fn sidebar_draw_width(viewport_w: f32, over: Option<f32>) -> f32 {
    rail_draw_width(
        over,
        SIDEBAR_MIN_W,
        SIDEBAR_MAX_W,
        viewport_w,
        sidebar_width(viewport_w),
    )
}

/// [`rail_draw_width`] for the right rail, on the spec's rungs.
pub fn inspector_draw_width(viewport_w: f32, over: Option<f32>) -> f32 {
    rail_draw_width(
        over,
        INSPECTOR_MIN_W,
        INSPECTOR_MAX_W,
        viewport_w,
        inspector_width(viewport_w),
    )
}

/// [`rail_draw_width`] for History's timeline, on its one spec size.
pub fn timeline_draw_width(viewport_w: f32, over: Option<f32>) -> f32 {
    rail_draw_width(over, TIMELINE_MIN_W, TIMELINE_MAX_W, viewport_w, TIMELINE_W)
}

/// One of the drag-edges the workspace exposes. The shared left rail
/// answers to the same drag in both destinations — it is one piece of chrome
/// — while the inspector and the timeline belong to their own. `Split` is
/// not a rail but the diff's own rule, dragged through the same probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rail {
    Sidebar,
    Inspector,
    Timeline,
    Split,
}

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

/// Whether the launch's own diff is what the centre is showing.
///
/// A `diff <repo> <revspec>` launch asks for rows by name, so the command
/// line — not the files cursor — is what picked them, and the preview must
/// not re-aim the centre at a file nobody chose yet. The hold is armed at
/// launch and captures its baseline the first time a file sits under the
/// cursor — a skeleton's tree arrives with its wave, so there is no position
/// to read at launch. While the cursor still names that file the launch rows
/// stand; naming another — or the deliberate `workspace.preview` a row click
/// dispatches — spends it. The baseline is section and path only, never the
/// preview key's generation: a refresh wave re-lands the same selection under
/// a newer one, and a wave is not a pick.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum LaunchHold {
    /// A `commits` launch, `diff` of the working tree, or a hold the pick
    /// already spent.
    #[default]
    Off,
    /// Set at launch; the first preview pass that finds a file under the
    /// cursor captures it as the baseline.
    Armed,
    /// The file the cursor launched on, while it still is.
    Holding(Section, PathBytes),
}

impl LaunchHold {
    /// True while the centre is the launch's rows rather than a preview.
    pub fn held(&self) -> bool {
        !matches!(self, Self::Off)
    }
}

/// The workspace's shell-side state: destination, center view and preview
/// guard. The sidebar holds no state of its own — it draws the files pane's
/// grouped projection under the files pane's cursor.
pub struct Workspace {
    /// Whether the left rail is hidden — one flag for both destinations,
    /// because the rail is the same piece of chrome in Changes and History:
    /// a reader who wants it gone wants it gone, not once per view. The
    /// title strip keeps the toggle that brings it back.
    pub sidebar_collapsed: bool,
    /// Whether the Changes inspector is hidden. History never draws that
    /// rail, so the flag means nothing there — it applies on the next entry.
    /// Collapsing while a composer field holds the keyboard hands the focus
    /// back to the root: a field in a rail nobody draws must not keep it.
    pub inspector_collapsed: bool,
    /// Whether History's branch timeline is hidden, leaving the commit's
    /// detail the full width of the destination. The commits pane the
    /// timeline projects keeps its cursor either way — the rail is a
    /// projection, not the list itself.
    pub timeline_collapsed: bool,
    /// The widths the pointer last dragged each rail to — `None` is the
    /// spec's rung for the current window, which is also what a double-click
    /// on the edge restores. The session file remembers them; a width taken
    /// on a bigger window is clamped by the draw helpers rather than thrown
    /// away, so the preference survives the window that cannot afford it.
    pub sidebar_w: Option<f32>,
    /// See `sidebar_w` — the Changes rail the composer lives in.
    pub inspector_w: Option<f32>,
    /// See `sidebar_w` — History's branch timeline.
    pub timeline_w: Option<f32>,
    /// Where the side-by-side diff's rule stands, as a fraction of the row —
    /// [`views::split::SPLIT_HALF`] until a drag says otherwise. A fraction
    /// rather than a width because the share is the preference: the same 60%
    /// is the right answer on every window. Kept here and not on the diffs
    /// themselves because the centre and the commit detail are two entities
    /// that should never disagree about it.
    pub split_fraction: f32,
    /// The destination the header names and the sidebar follows.
    pub destination: Destination,
    /// The center diff: built once on first entry, re-aimed per selection —
    /// never rebuilt, which is what keeps its scroll state and presentation
    /// across files, the same promise the main view keeps across commits.
    pub center: Option<Entity<Diff>>,
    /// The centre's other body: the pane a file with no lines to show gets.
    /// `Some` while the selection is a blob — a picture, a document, anything
    /// git calls binary — and `None` the rest of the time, which is what makes
    /// the centre's body a choice between two views rather than a mode flag
    /// inside one of them.
    pub blob: Option<Entity<crate::views::blob::Blob>>,
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
    /// The launch's own hold on the centre — see [`LaunchHold`]. `Off`
    /// everywhere a `diff <repo> <revspec>` launch did not ask for rows by
    /// name.
    pub launch: LaunchHold,
    /// The centre's other body, for a markdown file: the rendered document.
    /// Built once and kept, like the centre itself, and `None` until something
    /// asks for one.
    pub document: Option<Entity<crate::views::document::DocumentPane>>,
    /// Whether the centre is showing documents rather than rows.
    ///
    /// The reader's choice and not the selection's — with one exception the
    /// pane makes for itself: a file it cannot draw leaves it showing nothing,
    /// so the body falls back to the rows without the flag having to be
    /// cleared by whoever moved the cursor.
    pub document_wanted: bool,
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
            sidebar_collapsed: false,
            inspector_collapsed: false,
            timeline_collapsed: false,
            sidebar_w: None,
            inspector_w: None,
            timeline_w: None,
            split_fraction: super::split::SPLIT_HALF,
            destination: Destination::Changes,
            center: None,
            blob: None,
            request: 0,
            last: None,
            launch: LaunchHold::Off,
            document: None,
            document_wanted: false,
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
