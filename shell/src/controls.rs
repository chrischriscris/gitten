//! The title-bar controls.
//!
//! One thing so far: a picker. A label, the value it currently holds, and the
//! registered alternatives — which is the shape every seam in this app already
//! has, so the same control will do for a theme, a font or a highlighter the day
//! those want one.
//!
//! The strip's pickers degrade in two tiers as the window narrows — the
//! labels drop first, then the five collapse into one `view ▾` — computed,
//! never guessed, by [`tier`] from the same character arithmetic the
//! status bar's hint budget spends.
//!
//! # Why this is not `gpui_component::popover::Popover`
//!
//! AGENTS.md says don't build what the framework already has, and the framework
//! has both a `Popover` and a `Select`. This is a deliberate exception, and the
//! reasoning is short enough to check:
//!
//! - **Every colour in this app comes from `gitten_core::theme`**, which the ANSI
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

use crate::chrome::{gap_m, gap_s, RADIUS};
use gitten_core::font::Font;
use gitten_core::theme::{Surface, Theme};
use gpui::*;
use std::rc::Rc;

/// Height of the control, and of the strip it sits in.
const H: f32 = 22.0;
/// The open list's row height. Taller than the trigger: a menu row is a target,
/// not a label, and 22px is uncomfortable to hit.
pub(crate) const ROW_H: f32 = 24.0;

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
            options: options
                .iter()
                .map(|s| SharedString::from(s.to_string()))
                .collect(),
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

/// How much of their labels the strip's picker triggers keep as the window
/// narrows. Two steps, computed — never guessed — from the same character
/// arithmetic the status bar's hint budget spends: the labels drop first
/// (the value is the information; the label is recoverable from the open
/// menu), then the five collapse into one `view ▾`.
#[derive(Debug, PartialEq, Eq)]
pub enum Tier {
    /// Every trigger draws label and value: `layout unified ▾`.
    Full,
    /// The width where the full triggers no longer fit: value-only
    /// triggers — `unified ▾`, not `layout unified ▾`.
    ValueOnly,
    /// Even value-only triggers do not fit: one `view ▾` whose menu is
    /// the five pickers' entries as sections.
    Composed,
}

/// The tier the title strip's pickers draw at. `strip_px` is the window's
/// width; `left_px` is everything on the strip left of the pickers but the
/// repo path — the lights inset, the paddings, the chip, the badge, their
/// gaps — and `path_chars` is the repo path at the length it renders. Each
/// picker costs `label + value + caret` characters plus the pixels around
/// it, in characters × [`Font::char_width`], so a picker registered
/// tomorrow is budgeted the day it appears — and the thresholds are
/// derived from the budget, not named and then maintained by hand.
pub fn tier(
    font: &Font,
    strip_px: f32,
    left_px: f32,
    path_chars: usize,
    pickers: &[&Picker],
) -> Tier {
    let ch = font.char_width();
    // What one trigger costs around its text: its own padding, its border,
    // and the gap the strip puts before it — `px_2` twice, `border_1`, and
    // `gap_2`, through the spacing ladder so the budget moves when the font
    // does and not only when a label changes. The borders are absolute: a
    // hairline has no floor to scale from.
    let trigger_px = f32::from(gap_m(font)) + 2.0 + f32::from(gap_m(font)) + f32::from(gap_m(font));
    // The air between the trigger's own halves — label, value, caret.
    let gap_px = f32::from(gap_m(font));
    let cost = |labelled: bool| -> f32 {
        pickers
            .iter()
            .map(|p| {
                let text = if labelled {
                    p.label.chars().count() + p.value().chars().count() + 1
                } else {
                    p.value().chars().count() + 1
                };
                text as f32 * ch + gap_px * if labelled { 2.0 } else { 1.0 } + trigger_px
            })
            .sum()
    };
    let room = strip_px - left_px - path_chars as f32 * ch;
    if room >= cost(true) {
        Tier::Full
    } else if room >= cost(false) {
        Tier::ValueOnly
    } else {
        Tier::Composed
    }
}

/// The control: a trigger, and the list under it when `open`.
///
/// `on_toggle` is called with the *new* open state, so the caller does not have
/// to track what it just did. `on_pick` gets the chosen index and is responsible
/// for closing — a pick that left the list open would be a second decision, and
/// this control does not make decisions.
#[allow(clippy::too_many_arguments)]
pub fn picker(
    id: &'static str,
    p: &Picker,
    open: bool,
    // The trigger drops its label and says only the value — the strip's
    // second tier, drawn here so the narrowing has one home.
    value_only: bool,
    theme: &Theme,
    font: &Font,
    on_toggle: impl Fn(bool, &mut Window, &mut App) + 'static,
    // `on_pick` closes for itself, so a pick is one decision and not two.
    on_pick: impl Fn(usize, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let c = &theme.chrome;
    // Disabled draws the *label* quiet and the *value* at what an enabled label
    // gets, rather than putting both on `faint`: raw, that measures 1.95:1 on
    // the title bar, so "dim and inert rather than removed" was in practice
    // removed. The label now goes through `quiet_on` — still the quietest ink
    // on the strip, but at the furniture floor instead of under it — and a
    // control still has to say what it is on to be worth leaving there.
    let dim = if p.enabled {
        // The trigger's background is its own to know: closed it sits on the
        // title strip, open on the status tint — raw dim is under the text
        // floor on both (3.37, 3.40), so it resolves against the one painted.
        theme.dim_on(if open {
            Surface::Status
        } else {
            Surface::Title
        })
    } else {
        theme.quiet_on(c.title_bg)
    };
    let fg = if p.enabled {
        c.fg
    } else {
        // The value is read when it is there; raw dim is under the text floor
        // on the title bar, so it resolves against the strip it floats over.
        theme.dim_on(Surface::Title)
    };

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
        .gap(gap_m(font))
        .h(px(H))
        .px(gap_m(font))
        .rounded(px(RADIUS))
        // A border, because the fill cannot do this job: closed, the trigger was
        // `title_bg` on `title_bg`, so four controls were sixty characters of
        // dim text that only became controls when the mouse was already on one.
        .border_1()
        .border_color(rgb(if open { c.faint } else { c.border }))
        .bg(rgb(if open { c.status_bg } else { c.title_bg }))
        .children((!value_only).then(|| div().text_color(rgb(dim)).child(p.label)))
        .child(div().text_color(rgb(fg)).child(p.value()))
        .child(
            div()
                .text_color(rgb(dim))
                .child(if open { "▴" } else { "▾" }),
        );

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
        let widest = p
            .options
            .iter()
            .map(|o| o.chars().count())
            .max()
            .unwrap_or(0);
        let w = px((widest as f32 + 4.0) * font.advance * font.size + 16.0);

        let list = div()
            .id("list")
            .absolute()
            .top(px(H) + gap_s(font))
            .right_0()
            .w(w)
            .py(gap_s(font))
            .bg(rgb(c.title_bg))
            .border_1()
            .border_color(rgb(c.faint))
            .rounded(px(RADIUS))
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
                    .px(gap_m(font))
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(c.status_bg)))
                    .child(
                        div()
                            .text_color(rgb(if i == p.current { c.accent } else { c.fg }))
                            .child(option.clone()),
                    )
                    .children(
                        (i == p.current)
                            .then(|| div().flex_none().text_color(rgb(c.accent)).child("✓")),
                    )
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

/// One group of the composed menu: what one picker would have offered.
pub struct Section {
    /// The picker's label, naming the group.
    pub label: &'static str,
    pub options: Vec<SharedString>,
    pub current: usize,
    /// False draws the group's rows quiet and deaf — the same "dim and
    /// inert rather than removed" rule the standalone trigger follows.
    pub enabled: bool,
}

impl Section {
    /// The composed menu's groups, straight off the same registries the
    /// standalone triggers read: a pure walk over the pickers, so a picker
    /// registered tomorrow is a section the day it appears, with no edit
    /// here. This is the seam the tests hold.
    pub fn sections(pickers: &[&Picker]) -> Vec<Section> {
        pickers
            .iter()
            .map(|p| Section {
                label: p.label,
                options: p.options.clone(),
                current: p.current,
                enabled: p.enabled,
            })
            .collect()
    }
}

/// The five-in-one trigger: one `view ▾`, its menu the pickers' entries as
/// labeled sections. Selecting an entry does exactly what the standalone
/// picker's entry did — `on_pick` gets `(section, index)` and the caller
/// routes it; nothing here is a second decision.
pub fn composed_picker(
    id: &'static str,
    sections: &[Section],
    open: bool,
    theme: &Theme,
    font: &Font,
    on_toggle: impl Fn(bool, &mut Window, &mut App) + 'static,
    on_pick: impl Fn(usize, usize, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let c = &theme.chrome;
    // The same chrome the standalone trigger wears — the label is
    // "view" and there is no value, because the value is five menus'
    // worth of information the open list holds better.
    let dim = theme.dim_on(if open {
        Surface::Status
    } else {
        Surface::Title
    });
    let toggle = Rc::new(on_toggle);
    let trigger = div()
        .id("trigger")
        .flex()
        .flex_none()
        .items_center()
        .gap(gap_m(font))
        .h(px(H))
        .px(gap_m(font))
        .rounded(px(RADIUS))
        .border_1()
        .border_color(rgb(if open { c.faint } else { c.border }))
        .bg(rgb(if open { c.status_bg } else { c.title_bg }))
        .child(div().text_color(rgb(dim)).child("view"))
        .child(
            div()
                .text_color(rgb(dim))
                .child(if open { "▴" } else { "▾" }),
        )
        .cursor_pointer()
        .hover(|s| s.bg(rgb(c.status_bg)))
        .on_click({
            let toggle = toggle.clone();
            move |_, window, cx| toggle(!open, window, cx)
        });

    let mut root = div().id(id).relative().flex_none().child(trigger);

    if open {
        let on_pick = Rc::new(on_pick);
        // Wide enough for the longest option plus its tick, from the font
        // rather than a constant — the same reason the standalone list is.
        let widest = sections
            .iter()
            .flat_map(|s| s.options.iter().map(|o| o.chars().count()))
            .max()
            .unwrap_or(0);
        let w = px((widest as f32 + 4.0) * font.advance * font.size) + gap_m(font) + gap_m(font);

        let list = div()
            .id("list")
            .absolute()
            .top(px(H) + gap_s(font))
            .right_0()
            .w(w)
            .py(gap_s(font))
            .bg(rgb(c.title_bg))
            .border_1()
            .border_color(rgb(c.faint))
            .rounded(px(RADIUS))
            .occlude()
            .on_mouse_down_out({
                let dismiss = toggle.clone();
                move |_, window, cx| dismiss(false, window, cx)
            })
            .children(sections.iter().enumerate().map(|(s, section)| {
                let header = div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .h(px(ROW_H))
                    .px(gap_m(font))
                    .text_color(rgb(theme.dim_on(Surface::Title)))
                    .child(SharedString::from(section.label));
                div()
                    .id(("section", s))
                    .flex()
                    .flex_col()
                    .child(header)
                    .children(section.options.iter().enumerate().map(|(i, option)| {
                        let row =
                            div()
                                .id(("option", i))
                                .flex()
                                .items_center()
                                .justify_between()
                                .h(px(ROW_H))
                                .px(gap_m(font))
                                .text_color(rgb(if !section.enabled {
                                    theme.dim_on(Surface::Title)
                                } else if i == section.current {
                                    c.accent
                                } else {
                                    c.fg
                                }))
                                .child(option.clone())
                                .children((i == section.current && section.enabled).then(|| {
                                    div().flex_none().text_color(rgb(c.accent)).child("✓")
                                }));
                        if section.enabled {
                            let on_pick = on_pick.clone();
                            row.cursor_pointer()
                                .hover(|s| s.bg(rgb(c.status_bg)))
                                .on_click(move |_, window, cx| on_pick(s, i, window, cx))
                        } else {
                            row
                        }
                    }))
            }));

        // Deferred at priority 1, for the same reason the standalone list
        // is: an ancestor's paint order would otherwise bury it.
        root = root.child(deferred(list).with_priority(1));
    }
    root.into_any_element()
}

/// The transparent surface behind an open picker.
///
/// Picker lists paint at deferred priority 1. This paints first at priority 0,
/// occluding the rest of the window so a wheel outside the list cannot reach the
/// diff underneath, while the list remains the target inside its own bounds.
pub fn picker_backdrop() -> AnyElement {
    deferred(div().absolute().inset_0().occlude())
        .with_priority(0)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    // By name, not a glob: `use gpui::*` in the parent shadows `#[test]` with
    // GPUI's own attribute macro and every test in here fails to expand.
    use super::{tier, Picker, Section, Tier};

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
        assert_eq!(
            p.value().as_ref(),
            "histogram",
            "it still says what it is on"
        );
    }

    /// The strip's pickers as the strip builds them: every registry the
    /// diff screen reads, minus the two that live on a view (`layout`
    /// and `wrap` name what that view publishes, so a test spells the
    /// shipped values). The order is the strip's own.
    fn strip_pickers(host: &gitten_core::host::Host) -> Vec<Picker> {
        let whitespace: Vec<&str> = gitten_core::differ::Whitespace::ALL
            .iter()
            .map(|w| w.name())
            .collect();
        vec![
            Picker::new("layout", &["unified", "split"], 0),
            Picker::new("wrap", &host.wrap.names(), 0),
            Picker::new("algorithm", &host.differ.names(), 0),
            Picker::new("whitespace", &whitespace, 0),
            Picker::new("theme", &host.themes.names(), 0),
        ]
    }

    #[test]
    fn the_tier_degrades_exactly_at_the_computed_boundaries() {
        // The thresholds are derived, not named: the test computes what five
        // full and five value-only triggers cost from the real character
        // counts and holds the tier to those widths — a picker registered
        // tomorrow moves the boundaries and this test moves with it.
        let host = gitten_core::host::Host::new();
        let pickers = strip_pickers(&host);
        let refs: Vec<&Picker> = pickers.iter().collect();
        let font = gitten_core::font::Font::default();
        let ch = font.char_width();

        // The strip's furniture at the widths the plan names: the lights
        // inset, the paddings, the border, the chip and its drift, the
        // gaps — and a 40-character repository path, the length the
        // threshold is defined against.
        let left_px = 72.0 + 12.0 + 1.0 + 8.0 + (14.0 * ch + 16.0 + 2.0 + 8.0);
        let path_chars = 40;
        let full: f32 = refs
            .iter()
            .map(|p| {
                (p.label.chars().count() + p.value().chars().count() + 1) as f32 * ch + 16.0 + 26.0
            })
            .sum();
        let value: f32 = refs
            .iter()
            .map(|p| (p.value().chars().count() + 1) as f32 * ch + 8.0 + 26.0)
            .sum();
        let t1 = left_px + path_chars as f32 * ch + full;
        let t2 = left_px + path_chars as f32 * ch + value;

        assert_eq!(
            tier(&font, t1, left_px, path_chars, &refs),
            Tier::Full,
            "exactly the width five full pickers fit beside a 40-char path"
        );
        assert_eq!(
            tier(&font, t1 - 0.5, left_px, path_chars, &refs),
            Tier::ValueOnly,
            "half a character short, the labels give"
        );
        assert_eq!(
            tier(&font, t2, left_px, path_chars, &refs),
            Tier::ValueOnly,
            "exactly the width five value-only triggers fit"
        );
        assert_eq!(
            tier(&font, t2 - 0.5, left_px, path_chars, &refs),
            Tier::Composed,
            "half a character short of even that, the five compose"
        );
    }

    #[test]
    fn a_560px_window_composes_and_a_1400px_one_is_full() {
        // The plan's two named widths, held against the shipped registries.
        let host = gitten_core::host::Host::new();
        let pickers = strip_pickers(&host);
        let refs: Vec<&Picker> = pickers.iter().collect();
        let font = gitten_core::font::Font::default();
        let left_px = 72.0 + 12.0 + 1.0 + 8.0 + (14.0 * font.char_width() + 26.0);
        assert_eq!(
            tier(&font, 560.0, left_px, 40, &refs),
            Tier::Composed,
            "at the declared minimum, five pickers are one"
        );
        assert_eq!(
            tier(&font, 1400.0, left_px, 40, &refs),
            Tier::Full,
            "at a wide window, every trigger carries its label"
        );
    }

    #[test]
    fn the_composed_menu_holds_every_entry_of_every_picker() {
        // The seam test: the composed menu is generated from the same
        // registries, so a sixth registered picker appears in it without
        // an edit here.
        let host = gitten_core::host::Host::new();
        let mut pickers = strip_pickers(&host);
        pickers.push(Picker::new("sixth", &["a", "b"], 1));
        let refs: Vec<&Picker> = pickers.iter().collect();
        let sections = Section::sections(&refs);

        assert_eq!(sections.len(), 6, "a sixth picker is a sixth section");
        for (p, s) in pickers.iter().zip(&sections) {
            assert_eq!(s.label, p.label);
            let wanted: Vec<_> = p.options.iter().map(|o| o.as_ref()).collect();
            let got: Vec<_> = s.options.iter().map(|o| o.as_ref()).collect();
            assert_eq!(got, wanted, "section {} lost an entry", p.label);
            assert_eq!(s.current, p.current);
            assert_eq!(s.enabled, p.enabled);
        }
    }
}
