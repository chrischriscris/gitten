//! The settings panel: every knob, on one surface.
//!
//! The title strip used to carry five pickers — layout, wrap, algorithm,
//! whitespace, theme — each a label, a value and the registered alternatives.
//! Five was already too many for a 44px strip: the tier logic collapsed them
//! into one composed menu on narrow windows, and every sixth knob would have
//! needed the same budget arithmetic again. So the strip carries none of them
//! now, and this panel carries all of them plus the rest of `gitten.toml`'s
//! live knobs: context, move floor, indent heuristic, font, scrolling,
//! sidebar share and the mouse.
//!
//! The rows are built from the same registries the pickers read —
//! [`Host::themes`], [`Differs::names`](gitten_core::differ::Differs::names),
//! [`Wraps::names`](gitten_core::wrap::Wraps::names) and the view's own layout
//! list — so an extension's algorithm or theme is a row here the day it is
//! registered, with no edit to this file. That is the same seam the help
//! overlay and the context menu hold, drawn as a panel instead of a list.
//!
//! Every row applies **live**: choosing is doing, the way the pickers were.
//! There is no next-launch row in here — `font.monospaced`, `font.advance`
//! and the syntax colours stay in the file — and every change is also written
//! back to `gitten.toml` as the new default, so a relaunch opens where the
//! panel left off. A control that quietly rewrote the file would have been a
//! settings panel with no confirmation; this panel *is* the confirmation.
//!
//! Two GPUI facts shape the element, both shared with the help overlay: it is
//! [`deferred`], so it paints above the panes beside it rather than under the
//! sibling that follows them, and it is [`occlude`], so the rows underneath
//! take neither the clicks nor the wheel. And one keyboard fact, also shared:
//! while the panel stands it owns every press, resolved against [`MODE`]
//! alone, so a pane's `D` reads as "not bound" instead of arming a discard
//! behind a screen that is only describing it.

use crate::chrome::{gap_l, gap_m, RADIUS};
use gitten_core::command::Modes;
use gitten_core::differ::Whitespace;
use gitten_core::host::Host;
use gitten_core::theme::Surface;
use gpui::*;
use gpui_component::StyledExt as _;
use std::rc::Rc;

/// The mode the panel pushes, and the only one a press resolves against while
/// it is up. Beside [`crate::help::MODE`] and [`crate::input::MODE`]: the name
/// belongs to whoever pushes it, and `core` holds the bindings.
pub const MODE: &str = "settings";

/// One row's height. Roomier than a menu row: a label, a value and the two
/// chevrons that say the value turns.
const ROW_H: f32 = 28.0;
/// Air inside the border, at each edge — the help overlay's own inset.
const PAD: f32 = 16.0;
/// Widest the panel gets. Past this the values are further from their labels
/// than the eye will carry them.
const MAX_W: f32 = 560.0;

/// Which knob a row edits. Fixed identity for every row the panel holds, so
/// the shell matches on this rather than on a label that might be reworded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setting {
    Layout,
    Wrap,
    Algorithm,
    Whitespace,
    Context,
    Moves,
    IndentHeuristic,
    Theme,
    FontSize,
    FontFamily,
    Scroll,
    Scrolloff,
    Scrollbar,
    Sidebar,
    CopyOnSelect,
}

/// One row of the panel: what it edits, what it is called, what it holds —
/// and whether touching it does anything, which a fixture's algorithm row
/// does not, for the same reason its picker drew inert rather than hiding.
#[derive(Debug, Clone)]
pub struct Row {
    pub setting: Setting,
    pub label: &'static str,
    pub value: String,
    pub enabled: bool,
}

/// One group of rows, under a heading.
#[derive(Debug, Clone)]
pub struct Section {
    pub title: &'static str,
    pub rows: Vec<Row>,
}

/// Every section the panel shows, spelled from the live state.
///
/// `layouts`/`wraps` are the view's own lists with its current index;
/// `algorithm`/`whitespace` are the *effective* values — the live override
/// where there is one, the configured default otherwise — because the panel
/// must agree with what is on screen rather than with a copy of the file.
/// Everything else is read off the host. `from_repo` dims the two rows that
/// only mean something when a repository produced the diff, the way the
/// pickers drew inert rather than hiding.
#[allow(clippy::too_many_arguments)]
pub fn build(
    host: &Host,
    layouts: &[&str],
    layout: usize,
    wraps: &[&str],
    wrap: usize,
    algorithm: &str,
    whitespace: Whitespace,
    from_repo: bool,
) -> Vec<Section> {
    let choice =
        |setting, label: &'static str, options: &[&str], current: usize, enabled: bool| Row {
            setting,
            label,
            value: options.get(current).unwrap_or(&"").to_string(),
            enabled,
        };
    let number = |setting, label: &'static str, value: String| Row {
        setting,
        label,
        value,
        enabled: true,
    };
    let toggle = |setting, label: &'static str, on: bool| Row {
        setting,
        label,
        value: match on {
            true => "on",
            false => "off",
        }
        .into(),
        enabled: true,
    };
    let algorithms = host.differ.names();
    let ws: Vec<&str> = Whitespace::ALL.iter().map(|w| w.name()).collect();
    let themes = host.themes.names();
    let mut families = vec![host.font.family.as_str()];
    for known in ["JetBrainsMono Nerd Font Mono", "Menlo"] {
        if !families.contains(&known) {
            families.push(known);
        }
    }
    vec![
        Section {
            title: "view",
            rows: vec![
                choice(Setting::Layout, "layout", layouts, layout, true),
                choice(Setting::Wrap, "wrap", wraps, wrap, true),
            ],
        },
        Section {
            title: "diff",
            rows: vec![
                choice(
                    Setting::Algorithm,
                    "algorithm",
                    &algorithms,
                    algorithms.iter().position(|n| *n == algorithm).unwrap_or(0),
                    from_repo,
                ),
                choice(
                    Setting::Whitespace,
                    "whitespace",
                    &ws,
                    Whitespace::ALL
                        .iter()
                        .position(|w| *w == whitespace)
                        .unwrap_or(0),
                    from_repo,
                ),
                number(Setting::Context, "context", host.differ.context.to_string()),
                number(
                    Setting::Moves,
                    "moves",
                    match host.differ.min_moved {
                        0 => "off".into(),
                        n => n.to_string(),
                    },
                ),
                toggle(
                    Setting::IndentHeuristic,
                    "indent heuristic",
                    host.differ.indent_heuristic,
                ),
            ],
        },
        Section {
            title: "theme",
            rows: vec![choice(
                Setting::Theme,
                "theme",
                &themes,
                themes
                    .iter()
                    .position(|n| *n == host.theme.name)
                    .unwrap_or(0),
                true,
            )],
        },
        Section {
            title: "text",
            rows: vec![
                number(
                    Setting::FontSize,
                    "font size",
                    format!("{:.0}", host.font.size),
                ),
                choice(Setting::FontFamily, "font", &families, 0, true),
            ],
        },
        Section {
            title: "scrolling",
            rows: vec![
                number(Setting::Scroll, "scroll", host.view.rows.to_string()),
                number(
                    Setting::Scrolloff,
                    "scrolloff",
                    host.view.scrolloff.to_string(),
                ),
                toggle(Setting::Scrollbar, "scrollbar", host.view.scrollbar),
                number(
                    Setting::Sidebar,
                    "sidebar",
                    format!("{:.0}%", host.sidebar_share * 100.0),
                ),
            ],
        },
        Section {
            title: "mouse",
            rows: vec![toggle(
                Setting::CopyOnSelect,
                "copy on select",
                host.mouse.copy_on_select,
            )],
        },
    ]
}

/// How many selectable rows the panel holds. The shell clamps its selection
/// against this, because the sections are rebuilt per frame and the count
/// moves with the registries.
pub fn len(sections: &[Section]) -> usize {
    sections.iter().map(|s| s.rows.len()).sum()
}

/// The row a flat index names. `None` past the end — a stale selection is an
/// unmoved one, never a neighbouring knob.
pub fn at(sections: &[Section], mut index: usize) -> Option<(usize, usize)> {
    for (s, section) in sections.iter().enumerate() {
        if index < section.rows.len() {
            return Some((s, index));
        }
        index -= section.rows.len();
    }
    None
}

/// Step a choice index by `dir`, wrapping. Negative is up, the way `k` reads.
pub fn cycle(count: usize, current: usize, dir: i32) -> usize {
    match count {
        0 => 0,
        n => (current as i32 + dir).rem_euclid(n as i32) as usize,
    }
}

/// The panel itself.
///
/// A pure function of the sections, the selection and the scroll position:
/// nothing here names a knob, which is the whole test the pickers set for a
/// control built on registries. `on_select` moves the highlight,
/// `on_adjust` turns the value by `dir`, and `on_dismiss` walks away — all
/// three are the caller's, through the one dispatch path every key uses.
#[allow(clippy::too_many_arguments)]
pub fn overlay(
    sections: &[Section],
    sel: usize,
    scroll: &ScrollHandle,
    host: &Host,
    modes: &Modes,
    on_select: impl Fn(usize, &mut Window, &mut App) + 'static,
    on_adjust: impl Fn(usize, i32, &mut Window, &mut App) + 'static,
    on_dismiss: impl Fn(&mut Window, &mut App) + 'static,
) -> AnyElement {
    let c = &host.theme.chrome;
    let on_select = Rc::new(on_select);
    let on_adjust = Rc::new(on_adjust);
    let on_dismiss = Rc::new(on_dismiss);
    let font = host.font.family.clone();
    // A flat counter across the sections, because the selection is one number
    // and the rows are grouped for reading rather than for addressing.
    let mut flat = 0;
    let body = sections
        .iter()
        .map(|section| {
            let heading = div()
                .flex_none()
                .flex()
                .items_center()
                .h(px(ROW_H))
                .pt(gap_l(&host.font))
                .text_color(rgb(c.accent))
                .child(SharedString::from(section.title));
            let rows = section.rows.iter().map(|row| {
                let i = flat;
                flat += 1;
                let selected = i == sel;
                let on_select = on_select.clone();
                let on_adjust = on_adjust.clone();
                let value = match row.enabled {
                    true => div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(gap_m(&host.font))
                        .child(
                            div()
                                .id(SharedString::from(format!("settings-less-{i}")))
                                .flex_none()
                                .cursor_pointer()
                                .text_color(rgb(host.theme.dim_on(Surface::Title)))
                                .child("<")
                                .on_click({
                                    let on_adjust = on_adjust.clone();
                                    move |_, window, cx| on_adjust(i, -1, window, cx)
                                }),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_color(rgb(match selected {
                                    true => c.accent,
                                    false => c.fg,
                                }))
                                .child(SharedString::from(row.value.clone())),
                        )
                        .child(
                            div()
                                .id(SharedString::from(format!("settings-more-{i}")))
                                .flex_none()
                                .cursor_pointer()
                                .text_color(rgb(host.theme.dim_on(Surface::Title)))
                                .child(">")
                                .on_click({
                                    let on_adjust = on_adjust.clone();
                                    move |_, window, cx| on_adjust(i, 1, window, cx)
                                }),
                        ),
                    false => div()
                        .flex_none()
                        .text_color(rgb(host.theme.dim_on(Surface::Title)))
                        .child(SharedString::from(row.value.clone())),
                };
                div()
                    .id(SharedString::from(format!("settings-row-{i}")))
                    .flex_none()
                    .flex()
                    .items_center()
                    .h(px(ROW_H))
                    .px(gap_m(&host.font))
                    .rounded(px(RADIUS))
                    .bg(rgb(match selected {
                        true => c.selection_bg,
                        false => c.title_bg,
                    }))
                    .border_l(px(crate::chrome::ROW_BAR))
                    .border_color(rgb(match selected {
                        true => c.accent,
                        false => c.title_bg,
                    }))
                    .cursor_pointer()
                    .child(
                        div()
                            .min_w_0()
                            .flex_grow(1.0)
                            .truncate()
                            .text_color(rgb(match row.enabled {
                                true => c.fg,
                                false => host.theme.dim_on(Surface::Title),
                            }))
                            .child(row.label),
                    )
                    .child(value)
                    .on_click(move |_, window, cx| on_select(i, window, cx))
            });
            div()
                .flex_none()
                .flex()
                .flex_col()
                .child(heading)
                .children(rows)
        })
        .collect::<Vec<_>>();

    div()
        .absolute()
        .inset_0()
        .bg(rgb(c.bg).alpha(0.5))
        .occlude()
        .flex()
        .items_center()
        .justify_center()
        .child(
            deferred(
                div()
                    .occlude()
                    .v_flex()
                    .w(px(MAX_W))
                    .max_h_full()
                    .overflow_hidden()
                    .bg(rgb(c.title_bg))
                    .border_1()
                    .border_color(rgb(c.faint))
                    .rounded(px(RADIUS))
                    .debug_selector(|| "settings-panel".to_string())
                    .p(px(PAD))
                    .text_size(px(host.font.size))
                    .font_family(font)
                    .text_color(rgb(c.dim))
                    .child(
                        div()
                            .flex_none()
                            .pb(gap_m(&host.font))
                            .text_color(rgb(c.accent))
                            .child(SharedString::from(format!(
                                "settings{}",
                                match host.keys.live_keys_for("settings", modes).first() {
                                    Some(k) => format!("  ·  {k} closes"),
                                    None => String::new(),
                                }
                            ))),
                    )
                    .child(
                        div()
                            .id("settings-rows")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .track_scroll(scroll)
                            .children(body),
                    )
                    .child(
                        div()
                            .flex_none()
                            .h(px(ROW_H))
                            .flex()
                            .items_center()
                            .justify_between()
                            .text_color(rgb(host.theme.quiet_on(c.title_bg)))
                            .child("changes apply now and save to gitten.toml")
                            .child(
                                div()
                                    .id("settings-done")
                                    .flex_none()
                                    .cursor_pointer()
                                    .text_color(rgb(c.accent))
                                    .child("done")
                                    .on_click({
                                        let on_dismiss = on_dismiss.clone();
                                        move |_, window, cx| {
                                            cx.stop_propagation();
                                            on_dismiss(window, cx);
                                        }
                                    }),
                            ),
                    ),
            )
            .with_priority(2),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    // By name, not a glob: `use gpui::*` in the parent shadows `#[test]` with
    // GPUI's own attribute macro and every test in here fails to expand.
    use super::{at, build, cycle, len, Setting};
    use gitten_core::differ::Whitespace;
    use gitten_core::host::Host;

    fn sections(host: &Host) -> Vec<super::Section> {
        build(
            host,
            &["unified", "split"],
            0,
            &["off", "word", "char"],
            1,
            "histogram",
            Whitespace::Exact,
            true,
        )
    }

    #[test]
    fn the_panel_holds_every_knob_it_promises() {
        let sells = sections(&Host::new());
        assert_eq!(len(&sells), 15);
        let settings: Vec<Setting> = sells
            .iter()
            .flat_map(|s| s.rows.iter().map(|r| r.setting))
            .collect();
        for want in [
            Setting::Layout,
            Setting::Wrap,
            Setting::Algorithm,
            Setting::Whitespace,
            Setting::Context,
            Setting::Moves,
            Setting::IndentHeuristic,
            Setting::Theme,
            Setting::FontSize,
            Setting::FontFamily,
            Setting::Scroll,
            Setting::Scrolloff,
            Setting::Scrollbar,
            Setting::Sidebar,
            Setting::CopyOnSelect,
        ] {
            assert!(settings.contains(&want), "{want:?} has no row");
        }
    }

    #[test]
    fn the_rows_agree_with_what_is_on_screen() {
        let host = Host::new();
        let sells = sections(&host);
        let row = |w: Setting| {
            sells
                .iter()
                .flat_map(|s| &s.rows)
                .find(|r| r.setting == w)
                .unwrap()
                .value
                .clone()
        };
        assert_eq!(row(Setting::Layout), "unified");
        assert_eq!(row(Setting::Wrap), "word");
        assert_eq!(row(Setting::Algorithm), "histogram");
        assert_eq!(row(Setting::Theme), "dark");
        assert_eq!(row(Setting::Context), "3");
        // A registered-at-test-time algorithm is a row value without an edit here.
        let mut host = Host::new();
        host.differ.select("myers");
        let sells = build(
            &host,
            &["unified"],
            0,
            &["word"],
            0,
            "myers",
            Whitespace::All,
            true,
        );
        let algo = sells
            .iter()
            .flat_map(|s| &s.rows)
            .find(|r| r.setting == Setting::Algorithm)
            .unwrap();
        assert_eq!(algo.value, "myers");
        let ws = sells
            .iter()
            .flat_map(|s| &s.rows)
            .find(|r| r.setting == Setting::Whitespace)
            .unwrap();
        assert_eq!(ws.value, "all");
    }

    #[test]
    fn a_fixture_dims_what_only_a_repository_can_answer() {
        let host = Host::new();
        let sells = build(
            &host,
            &["unified"],
            0,
            &["word"],
            0,
            "histogram",
            Whitespace::Exact,
            false,
        );
        let algo = sells
            .iter()
            .flat_map(|s| &s.rows)
            .find(|r| r.setting == Setting::Algorithm)
            .unwrap();
        assert!(!algo.enabled);
        let theme = sells
            .iter()
            .flat_map(|s| &s.rows)
            .find(|r| r.setting == Setting::Theme)
            .unwrap();
        assert!(theme.enabled, "a palette is the window's, not the diff's");
    }

    #[test]
    fn a_flat_index_names_its_row_and_nothing_past_the_end() {
        let sells = sections(&Host::new());
        assert_eq!(len(&sells), 15);
        assert!(at(&sells, 0).is_some());
        assert!(at(&sells, 14).is_some());
        assert_eq!(at(&sells, 15), None);
        // Sections stay grouped: index 2 is the diff section's first row.
        let (s, r) = at(&sells, 2).unwrap();
        assert_eq!(sells[s].rows[r].setting, Setting::Algorithm);
    }

    #[test]
    fn a_choice_wraps_in_both_directions() {
        assert_eq!(cycle(3, 0, -1), 2);
        assert_eq!(cycle(3, 2, 1), 0);
        assert_eq!(cycle(1, 0, 1), 0);
        assert_eq!(cycle(0, 0, 1), 0);
    }
}
