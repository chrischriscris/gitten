//! The guide-v2 workspace shell (Phase 2 strangler).
//!
//! Beside the numbered stack, not instead of it yet: when
//! [`Workspace::enabled`] the window's middle region is this workspace — a
//! 76px destination header over a sidebar + center-diff + inspector-stub row
//! — and the old stack is hidden but fully alive underneath (its panes keep
//! their cursors, its refresh wave keeps landing, its commands keep their
//! names). `"workspace.changes"` enters, `"workspace.history"` leaves for the
//! full stack, which is the History destination until it moves in here.
//!
//! What lives here is geometry and state only: the sidebar's rows come from
//! [`super::files::Files`]' grouped projection (one selection state, shared),
//! the center is a plain [`super::diff::Diff`] fed one file's rows, and every
//! mouse control resolves through the shell's named dispatch. Widths are
//! fixed px per the spec — 255/266px, 280/295px past [`WIDE_PX`] — read from
//! the viewport once at composition time, never from inside a view.

use super::diff::Diff;
use super::files::Section;
use gitten_core::status::PathBytes;
use gpui::{Entity, UniformListScrollHandle};

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

/// [`sidebar_width`]'s twin for the right rail. The inspector's content is
/// Phase 3; the slot is reserved here so the center never lays out against
/// a width Phase 3 will move.
pub fn inspector_width(viewport_w: f32) -> f32 {
    match viewport_w > WIDE_PX {
        true => INSPECTOR_WIDE_W,
        false => INSPECTOR_W,
    }
}

/// Which destination the workspace shows. Changes is the default per the
/// interaction contract; History leaves the workspace for the full stack
/// (commits column included) until the timeline moves in here in a later
/// phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Destination {
    #[default]
    Changes,
    History,
}

/// The workspace's shell-side state: the toggle, the center view and the
/// preview guard. The sidebar holds no state of its own — it draws the
/// files pane's grouped projection under the files pane's cursor.
pub struct Workspace {
    /// Strangler flag: middle region shows the workspace instead of the stack.
    pub enabled: bool,
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
}

impl Default for Workspace {
    fn default() -> Self {
        Self {
            enabled: false,
            destination: Destination::Changes,
            center: None,
            request: 0,
            last: None,
            sidebar_scroll: UniformListScrollHandle::new(),
        }
    }
}
