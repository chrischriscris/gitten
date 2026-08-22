//! The title-bar controls.
//!
//! One thing so far: a picker. A label, the value it currently holds, and the
//! registered alternatives — which is the shape every seam in this app already
//! has, so the same control will do for a theme, a font or a highlighter the day
//! those want one.
//!
//! # Why this is not `gpui_component::popover::Popover`
//!
//! AGENTS.md says don't build what the framework already has, and the framework
//! has both a `Popover` and a `Select`. This is a deliberate exception, and the
//! reasoning is short enough to check:
//!
//! - **Every colour in this app comes from `plait_core::theme`**, which the ANSI
//!   painter and a terminal frontend read too. `Popover` draws its surface from
//!   gpui-component's own theme, so matching would mean keeping a second theme
//!   in sync with ours, and *not* matching is what rule 2 forbids —
//!   `appearance(false)` drops the surface and, per its own documentation, also
//!   drops dismiss-on-outside-click, which is the part worth having.
//! - **`Popover::trigger` wants `Selectable`**, a gpui-component trait, so the
//!   trigger cannot be a plain `div` drawn in our palette without a wrapper type
//!   whose only job is to satisfy it.
//! - **The hard part of a dropdown is placement**, and here there is none. This
//!   sits in a fixed 32px strip at the top of the window; it always opens
//!   downward and never needs to flip. `Popover` earns its keep where the anchor
//!   can be anywhere, which is not this.
//!
//! What is *not* hand-rolled is the two things that would have been real bugs:
//! the open list is `gpui::deferred`, so it paints above its ancestors rather
//! than under the sibling that comes after the title bar, and it dismisses
//! through `on_mouse_down_out`. Both are GPUI's, and both are one call.
//!
//! `Select` is more still: a searchable, multi-selectable list behind a delegate
//! trait and its own state entity, for a list of three. If a picker ever needs
//! search, grouping or keyboard navigation, that is the moment to throw this
//! away and take the framework's — and this comment is what should be read
//! first.
//!
//! # State lives with the caller
//!
//! Nothing here is an entity and nothing here holds an open flag. Which picker
//! is open is one field on whoever draws the strip, and the two callbacks are
//! `cx.listener` closures from that same entity. So this file has no lifecycle
//! to get wrong, and it is a pure function of a `Picker` plus a bool.

use gpui::*;
use plait_core::font::Font;
use plait_core::theme::Theme;
use std::rc::Rc;

/// Height of the control, and of the strip it sits in.
const H: f32 = 22.0;
/// The open list's row height. Taller than the trigger: a menu row is a target,
/// not a label, and 22px is uncomfortable to hit.
const ROW_H: f32 = 24.0;

/// A value, and everything it could be instead.
pub struct Picker {
    /// What the value *is*, shown before it in a dimmer colour. Two words at
    /// most; this is a 32-pixel strip.
    pub label: &'static str,
    pub options: Vec<SharedString>,
    pub current: usize,
    /// False draws it dim and inert. Used rather than hiding it, so the control
    /// does not appear and disappear as you change what you are looking at —
    /// a `.diff` fixture has no algorithm to choose and still has a diff.
    pub enabled: bool,
}

impl Picker {
    pub fn new(label: &'static str, options: &[&str], current: usize) -> Self {
        Self {
            label,
            options: options.iter().map(|s| SharedString::from(s.to_string())).collect(),
            current,
            enabled: true,
        }
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    fn value(&self) -> SharedString {
        self.options.get(self.current).cloned().unwrap_or_default()
    }
}

/// The control: a trigger, and the list under it when `open`.
///
/// `on_toggle` is called with the *new* open state, so the caller does not have
/// to track what it just did. `on_pick` gets the chosen index and is responsible
/// for closing — a pick that left the list open would be a second decision, and
/// this control does not make decisions.
pub fn picker(
    id: &'static str,
    p: &Picker,
    open: bool,
    theme: &Theme,
    font: &Font,
    on_toggle: impl Fn(bool, &mut Window, &mut App) + 'static,
    // `on_pick` closes for itself, so a pick is one decision and not two.
    on_pick: impl Fn(usize, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let c = &theme.chrome;
    // Disabled draws the *label* faint and the *value* at what an enabled label
    // gets, rather than putting both on `faint`: that measures 1.95:1 on the
    // title bar, so "dim and inert rather than removed" was in practice removed —
    // and a control still has to say what it is on to be worth leaving there.
    let dim = if p.enabled { c.dim } else { c.faint };
    let fg = if p.enabled { c.fg } else { c.dim };

    // Shared with the list, which dismisses on an outside click.
    let toggle = Rc::new(on_toggle);
    let mut trigger = div()
        // Interactivity requires identity: `.id()` before any of the methods on
        // `StatefulInteractiveElement`, and there is no way in without one.
        //
        // `"trigger"` and not `id`, because the wrapper below carries `id` and
        // an element's identity is its *path*. An unnamed ancestor contributes
        // nothing to that path, so two pickers whose inner elements were both
        // called `"list"` would be the same element as far as GPUI is concerned —
        // one open menu would drive the other's hover state.
        .id("trigger")
        .flex()
        .flex_none()
        .items_center()
        .gap_2()
        .h(px(H))
        .px_2()
        .rounded(px(3.))
        // A border, because the fill cannot do this job: closed, the trigger was
        // `title_bg` on `title_bg`, so four controls were sixty characters of
        // dim text that only became controls when the mouse was already on one.
        .border_1()
        .border_color(rgb(if open { c.faint } else { c.border }))
        .bg(rgb(if open { c.status_bg } else { c.title_bg }))
        .child(div().text_color(rgb(dim)).child(p.label))
        .child(div().text_color(rgb(fg)).child(p.value()))
        // A caret and not a glyph from an icon font: the app ships no icons, and
        // one drawn character is not worth a dependency.
        .child(div().text_color(rgb(dim)).child(if open { "▴" } else { "▾" }));

    if p.enabled {
        trigger = trigger
            .cursor_pointer()
            .hover(|s| s.bg(rgb(c.status_bg)))
            .on_click({
                let toggle = toggle.clone();
                move |_, window, cx| toggle(!open, window, cx)
            });
    }

    // `relative` on the wrapper and `absolute` on the list, so the list is
    // positioned by this control and not by the strip: the strip is a flex row
    // and an absolutely positioned child of it would be measured into the
    // layout and push its neighbours around.
    let mut root = div().id(id).relative().flex_none().child(trigger);

    if open && p.enabled {
        let on_pick = Rc::new(on_pick);
        let dismiss = toggle.clone();
        // Wide enough for the longest option plus its tick, from the font rather
        // than a constant — the same reason `font.advance` exists at all. A
        // stale width here is a menu that clips its own labels.
        let widest = p.options.iter().map(|o| o.chars().count()).max().unwrap_or(0);
        let w = px((widest as f32 + 4.0) * font.advance * font.size + 16.0);

        let list = div()
            .id("list")
            .absolute()
            .top(px(H + 4.0))
            .right_0()
            .w(w)
            .py_1()
            .bg(rgb(c.title_bg))
            .border_1()
            .border_color(rgb(c.faint))
            .rounded(px(4.))
            // Without this the list is drawn but the rows beneath it get the
            // clicks: GPUI hit-tests by paint order, and an absolutely
            // positioned child does not claim the space it covers.
            .occlude()
            // A menu that only closes by choosing something is a menu you cannot
            // change your mind about.
            .on_mouse_down_out({
                let dismiss = dismiss.clone();
                move |_, window, cx| dismiss(false, window, cx)
            })
            .children(p.options.iter().enumerate().map(|(i, option)| {
                let on_pick = on_pick.clone();
                div()
                    .id(("option", i))
                    .flex()
                    .items_center()
                    .justify_between()
                    .h(px(ROW_H))
                    .px_2()
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(c.status_bg)))
                    .child(
                        div()
                            .text_color(rgb(if i == p.current { c.accent } else { c.fg }))
                            .child(option.clone()),
                    )
                    .children((i == p.current).then(|| {
                        div().flex_none().text_color(rgb(c.accent)).child("✓")
                    }))
                    .on_click(move |_, window, cx| on_pick(i, window, cx))
            }));

        // Deferred, and this is the whole reason the list is visible at all. The
        // title bar is the *first* child of the window's column, so anything it
        // overflows downward is painted before — and therefore under — the diff
        // beside it. `deferred` keeps the layout here and moves the paint to
        // after every ancestor.
        root = root.child(deferred(list).with_priority(1));
    }
    root.into_any_element()
}

#[cfg(test)]
mod tests {
    // By name, not a glob: `use gpui::*` in the parent shadows `#[test]` with
    // GPUI's own attribute macro and every test in here fails to expand.
    use super::Picker;

    #[test]
    fn a_picker_shows_the_option_it_is_on() {
        let p = Picker::new("layout", &["unified", "split"], 1);
        assert_eq!(p.value().as_ref(), "split");
        assert!(p.enabled);
    }

    #[test]
    fn an_out_of_range_index_shows_nothing_rather_than_panicking() {
        // The index comes from another component's registry, which may have been
        // rebuilt. Empty is a legible answer; an index panic is not.
        let p = Picker::new("layout", &["unified"], 7);
        assert_eq!(p.value().as_ref(), "");
        let p = Picker::new("algorithm", &[], 0);
        assert_eq!(p.value().as_ref(), "");
    }

    #[test]
    fn disabling_is_a_state_and_not_an_absence() {
        // Drawn dim and inert rather than removed: a control that appears and
        // disappears as you change view is harder to find than one that greys.
        let p = Picker::new("algorithm", &["histogram"], 0).enabled(false);
        assert!(!p.enabled);
        assert_eq!(p.value().as_ref(), "histogram", "it still says what it is on");
    }
}
