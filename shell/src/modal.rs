//! The centered panel: one scrim, one box, whoever's content.
//!
//! The help overlay and the message overlay drew the same shape twice — a dim
//! scrim over the window, a bordered box in the middle, painted late and
//! occluding — and the settings overlay drew it a third time before it moved
//! out. Three copies of the shape is how a fourth panel invents a fifth
//! border. So the shape lives here, once: the scrim's alpha, the deferred
//! priority, the padding, the radius and the border. What differs per panel is
//! the width and the content, which is why those are the arguments.
//!
//! Deliberately not a `Modal` struct with slots for headers and footers: a
//! container that dictates content is a container an extension cannot extend.
//! This is plumbing — the paint order and the dim — and the panels keep their
//! own headings, rows and footers. The day an extension needs to stand a
//! panel of its own, this is the function it calls.

use crate::chrome::RADIUS;
use gitten_core::host::Host;
use gpui::*;
use gpui_component::StyledExt as _;

/// How wide the box draws. Help sizes to its projection — keys up to their
/// descriptions, never wider than its ceiling — while the message takes the
/// room git's answer needs. Both ceilings ride along here so a new panel
/// picks a bound rather than inventing a width.
pub enum Width {
    /// Exactly this wide, padding included.
    Exact(f32),
    /// No wider than this; narrower content draws narrower.
    Max(f32),
}

/// Air inside the border, at each edge — the help overlay's old inset, now
/// every centered panel's.
pub const PANEL_PAD: f32 = 16.0;

/// The scrim over the whole window with one bordered box in the middle.
///
/// Painted [`deferred`] at priority 2 — above the context menu's 1 and its
/// backdrop's 0 — and [`occlude`]d twice over: the scrim swallows the wheel
/// and the clicks around the box, the box claims its own. The dim is
/// load-bearing, not decorative: a faint border clears ~1.35:1 against the
/// row tints bare and ~1.7:1 dimmed, so a panel without it dissolves into
/// the diff.
///
/// One child per panel section — heading, scrolling rows, footer — laid as
/// the box's own column, so a scrolling middle keeps its flex like it did
/// when each panel built the box itself.
pub fn centered(host: &Host, width: Width, children: Vec<AnyElement>) -> AnyElement {
    let c = &host.theme.chrome;
    let panel = div()
        .occlude()
        .v_flex()
        .max_h_full()
        .overflow_hidden()
        .bg(rgb(c.title_bg))
        .border_1()
        .border_color(rgb(c.faint))
        .rounded(px(RADIUS))
        .p(px(PANEL_PAD))
        .text_size(px(host.font.size))
        .font_family(host.font.family.clone())
        .text_color(rgb(c.dim))
        .children(children);
    let panel = match width {
        Width::Exact(w) => panel.w(px(w)),
        Width::Max(w) => panel.max_w(px(w)),
    };
    div()
        .absolute()
        .inset_0()
        .bg(rgb(c.bg).alpha(0.5))
        .occlude()
        .flex()
        .items_center()
        .justify_center()
        .child(deferred(panel).with_priority(2))
        .into_any_element()
}
