//! The settings surface: every knob, its data, on one registry.
//!
//! The title strip used to carry five pickers — layout, wrap, algorithm,
//! whitespace, theme — each a label, a value and the registered alternatives.
//! Five was already too many for a 44px strip: the tier logic collapsed them
//! into one composed menu on narrow windows, and every sixth knob would have
//! needed the same budget arithmetic again. So the strip carries none of them
//! now, and this module carries all of them plus the rest of `gitten.toml`'s
//! live knobs: context, move floor, indent heuristic, font, scrolling,
//! sidebar share and the mouse.
//!
//! The rows are built from the same registries the pickers read —
//! [`Host::themes`], [`Differs::names`](gitten_core::differ::Differs::names),
//! [`Wraps::names`](gitten_core::wrap::Wraps::names) and the view's own layout
//! list — so an extension's algorithm or theme is a row here the day it is
//! registered, with no edit to this file. That is the same seam the help
//! overlay and the context menu hold.
//!
//! Every row applies **live**: choosing is doing, the way the pickers were.
//! There is no next-launch row in here — `font.monospaced`, `font.advance`
//! and the syntax colours stay in the file — and every change is also written
//! back to `gitten.toml` as the new default, so a relaunch opens where the
//! window left off. A control that quietly rewrote the file would have been a
//! settings surface with no confirmation; this surface *is* the confirmation.
//!
//! What draws the rows — the overlay that stood here, the window that stands
//! here now — is a client of this module, not its content. A row is a setting,
//! a label, a value, one line of teaching and whether touching it does
//! anything. And one keyboard fact travels with it: while settings stand they
//! own every press, resolved against [`MODE`] alone, so a pane's `D` reads as
//! "not bound" instead of arming a discard behind a screen that is only
//! describing it.

use gitten_core::differ::Whitespace;
use gitten_core::host::Host;

/// The mode a press resolves against while settings stand. Beside
/// [`crate::help::MODE`] and [`crate::input::MODE`]: the name belongs to
/// whoever stands it up, and `core` holds the bindings.
pub const MODE: &str = "settings";

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

/// One row of the panel: what it edits, what it is called, what it holds,
/// one line saying what the knob does — and whether touching it does
/// anything, which a fixture's algorithm row does not, for the same reason
/// its picker drew inert rather than hiding.
#[derive(Debug, Clone)]
pub struct Row {
    pub setting: Setting,
    pub label: &'static str,
    pub value: String,
    pub desc: &'static str,
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
    let choice = |setting,
                  label: &'static str,
                  desc: &'static str,
                  options: &[&str],
                  current: usize,
                  enabled: bool| Row {
        setting,
        label,
        value: options.get(current).unwrap_or(&"").to_string(),
        desc,
        enabled,
    };
    let number = |setting, label: &'static str, desc: &'static str, value: String| Row {
        setting,
        label,
        value,
        desc,
        enabled: true,
    };
    let toggle = |setting, label: &'static str, desc: &'static str, on: bool| Row {
        setting,
        label,
        value: match on {
            true => "on",
            false => "off",
        }
        .into(),
        desc,
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
                choice(
                    Setting::Layout,
                    "layout",
                    "unified or side-by-side — s cycles the registry",
                    layouts,
                    layout,
                    true,
                ),
                choice(
                    Setting::Wrap,
                    "wrap",
                    "word, character or off — a wrap is more rows, never a taller one",
                    wraps,
                    wrap,
                    true,
                ),
            ],
        },
        Section {
            title: "diff",
            rows: vec![
                choice(
                    Setting::Algorithm,
                    "algorithm",
                    "which lines correspond — histogram anchors on rare lines",
                    &algorithms,
                    algorithms.iter().position(|n| *n == algorithm).unwrap_or(0),
                    from_repo,
                ),
                choice(
                    Setting::Whitespace,
                    "whitespace",
                    "an equivalence relation, not an algorithm — normalised per line",
                    &ws,
                    Whitespace::ALL
                        .iter()
                        .position(|w| *w == whitespace)
                        .unwrap_or(0),
                    from_repo,
                ),
                number(
                    Setting::Context,
                    "context",
                    "unchanged lines around each hunk",
                    host.differ.context.to_string(),
                ),
                number(
                    Setting::Moves,
                    "moves",
                    "blocks shorter than this are coincidence, not a move",
                    match host.differ.min_moved {
                        0 => "off".into(),
                        n => n.to_string(),
                    },
                ),
                toggle(
                    Setting::IndentHeuristic,
                    "indent heuristic",
                    "slide hunk edges by indentation sign",
                    host.differ.indent_heuristic,
                ),
            ],
        },
        Section {
            title: "theme",
            rows: vec![choice(
                Setting::Theme,
                "theme",
                "the palette the window reads — colour reloads per frame",
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
                    "point size for code and chrome",
                    format!("{:.0}", host.font.size),
                ),
                choice(
                    Setting::FontFamily,
                    "font",
                    "family for code and chrome",
                    &families,
                    0,
                    true,
                ),
            ],
        },
        Section {
            title: "scrolling",
            rows: vec![
                number(
                    Setting::Scroll,
                    "scroll",
                    "multiplier on wheel and smooth-scroll pixels",
                    host.view.rows.to_string(),
                ),
                number(
                    Setting::Scrolloff,
                    "scrolloff",
                    "margin the cursor keeps from the list edges",
                    host.view.scrolloff.to_string(),
                ),
                toggle(
                    Setting::Scrollbar,
                    "scrollbar",
                    "the bar over the last cell — an indicator, not a track",
                    host.view.scrollbar,
                ),
                number(
                    Setting::Sidebar,
                    "sidebar",
                    "share of the window the sidebar keeps",
                    format!("{:.0}%", host.sidebar_share * 100.0),
                ),
            ],
        },
        Section {
            title: "mouse",
            rows: vec![toggle(
                Setting::CopyOnSelect,
                "copy on select",
                "drag-select copies through OSC 52",
                host.mouse.copy_on_select,
            )],
        },
    ]
}

/// How many selectable rows the surface holds. Whoever draws clamps its
/// selection against this, because the sections are rebuilt per frame and the
/// count moves with the registries.
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
    fn every_row_teaches_its_knob() {
        for section in sections(&Host::new()) {
            for row in &section.rows {
                assert!(
                    !row.desc.is_empty(),
                    "{:?} has no one-line teaching",
                    row.setting
                );
            }
        }
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
