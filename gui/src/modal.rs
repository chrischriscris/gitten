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
//! The look is the guide-v2 reference's dialog: a 12px radius, 24px of air, the
//! surface colour rather than the strip, and a heading with a `×` on the right.
//! [`heading`], [`close_button`], [`hint`] and [`preview`] are that vocabulary;
//! a panel that reaches for its own border has already stopped matching.
//!
//! Deliberately not a `Modal` struct with slots for headers and footers: a
//! container that dictates content is a container an extension cannot extend.
//! This is plumbing — the paint order and the dim — and the panels keep their
//! own headings, rows and footers. The day an extension needs to stand a
//! panel of its own, this is the function it calls.

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

/// Air inside the border, at each edge — the reference's 24px dialog inset.
pub const PANEL_PAD: f32 = 24.0;

/// The dialog's corner radius. Its own constant, and not [`crate::chrome::RADIUS`]:
/// a control is 4px and a floating panel is 12, and one number for both is how
/// a menu ends up looking like a card.
pub const PANEL_RADIUS: f32 = 12.0;

/// The scrim over the whole window with one bordered box in the middle.
///
/// Painted [`deferred`] at priority 2 — above the context menu's 1 and its
/// backdrop's 0 — and [`occlude`]d twice over: the scrim swallows the wheel
/// and the clicks around the box, the box claims its own. The dim is
/// load-bearing, not decorative: a faint border clears ~1.35:1 against the
/// row tints bare and ~1.7:1 dimmed, so a panel without it dissolves into
/// the diff. The reference's own scrim is black at 55% with a backdrop blur;
/// the alpha is kept, the blur is the platform's and not ours.
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
        .bg(rgb(c.bg))
        .border_1()
        .border_color(rgb(c.border))
        .rounded(px(PANEL_RADIUS))
        .p(px(PANEL_PAD))
        .text_size(px(host.font.size))
        .font_family(host.font.family.clone())
        .text_color(rgb(c.fg))
        .children(children);
    let panel = match width {
        Width::Exact(w) => panel.w(px(w)),
        Width::Max(w) => panel.max_w(px(w)),
    };
    div()
        .absolute()
        .inset_0()
        // The reference's own scrim: black at 55%, not the window tint. A dim
        // deep enough that the border and the fill both clear the diff behind.
        .bg(rgba(0x0000008c))
        .occlude()
        .flex()
        .items_center()
        .justify_center()
        .child(deferred(panel).with_priority(2))
        .into_any_element()
}

/// A dialog's heading: its title at the left, its way out at the right.
///
/// The `close` element is the caller's — [`close_button`] is the standard one —
/// because what closing *means* belongs to the panel that opened.
pub fn heading(host: &Host, title: impl Into<SharedString>, close: Option<AnyElement>) -> Div {
    let c = &host.theme.chrome;
    div()
        .flex_none()
        .flex()
        .items_center()
        .justify_between()
        .gap(px(12.0))
        .mb(px(20.0))
        .child(
            div()
                .min_w_0()
                .truncate()
                .text_size(px(18.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(c.fg))
                .child(title.into()),
        )
        .children(close)
}

/// The standard `×`. The caller attaches the click that closes its own panel.
pub fn close_button(host: &Host, id: impl Into<ElementId>) -> Stateful<Div> {
    let c = &host.theme.chrome;
    div()
        .id(id)
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .px(px(5.0))
        .cursor_pointer()
        .text_size(px(23.0))
        .text_color(rgb(c.dim))
        .hover(|s| s.text_color(rgb(c.fg)))
        .child("\u{00d7}")
}

/// The small print under a dialog's content: what the keyboard does next.
pub fn hint(host: &Host, text: impl Into<SharedString>) -> AnyElement {
    let c = &host.theme.chrome;
    div()
        .flex_none()
        .mt(px(19.0))
        .text_size(px(11.0))
        .text_color(rgb(c.dim))
        .child(text.into())
        .into_any_element()
}

/// A boxed block inside a dialog — a commit preview, a summary. The
/// reference's `.commit-preview`: a quiet surface one step off the dialog's,
/// a hairline and 16px of air.
pub fn preview(host: &Host, children: Vec<AnyElement>) -> AnyElement {
    let c = &host.theme.chrome;
    div()
        .flex_none()
        .v_flex()
        .gap_y(px(6.0))
        .p(px(16.0))
        .bg(rgb(c.raised))
        .border_1()
        .border_color(rgb(c.border))
        .rounded(px(6.0))
        .text_size(px(12.0))
        .children(children)
        .into_any_element()
}
