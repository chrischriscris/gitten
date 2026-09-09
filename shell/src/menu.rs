//! Floating surfaces: the transparent backdrop behind an open menu.
//!
//! The project menu paints at deferred priority 1. This paints first at
//! priority 0, occluding the rest of the window so a wheel outside the menu
//! cannot reach the diff underneath, while the menu remains the target
//! inside its own bounds. The row menus themselves — the context menu, the
//! project menu — build their own rows where they stand; what is shared is
//! only this dim, occluding floor.

use gpui::*;

/// Menu rows stay compact; only the title-bar trigger needed the larger target.
pub(crate) const ROW_H: f32 = 24.0;

/// Where a menu draws: below-right of the pointer, clamped so it never
/// paints past the window edge. **Clamp, don't flip** — a menu that flips
/// under the pointer puts the rows the finger is on somewhere else exactly
/// when the finger is at an edge, which is the one place the mistake is
/// cheapest to make.
pub(crate) fn clamped(at: Point<Pixels>, viewport: Size<Pixels>, w: f32, h: f32) -> Point<Pixels> {
    point(
        px(f32::from(at.x).clamp(0.0, (f32::from(viewport.width) - w).max(0.0))),
        px(f32::from(at.y).clamp(0.0, (f32::from(viewport.height) - h).max(0.0))),
    )
}

/// The transparent surface behind an open menu.
pub fn backdrop() -> AnyElement {
    deferred(div().absolute().inset_0().occlude())
        .with_priority(0)
        .into_any_element()
}
