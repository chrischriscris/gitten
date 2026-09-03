//! The context menu: what the keymap says a right-click may do here.
//!
//! There are no entries here. The rows are [`Keymap::help`]'s — `core`'s
//! projection of a mode stack against the command registry — taken for the
//! one pane the click landed over, so a command an extension registers in a
//! pane's mode is in that pane's menu without a line here changing. The same
//! seam the help overlay and the status bar already pass, drawn at the
//! pointer instead of the bottom of the window.
//!
//! What is here is only placement and ink. Placement: below-right of the
//! pointer, **clamped, never flipped**, so the menu never paints past the
//! window edge — the one placement decision the picker never needed, because
//! a picker sits in a fixed strip and always opens downward. Ink: the status
//! bar's own rule, the key drawn bright and the label dim, so the eye picks
//! the keys out of the menu and reads labels only when it wants one.
//!
//! Two GPUI facts it shares with the picker menus and the help overlay: it
//! is [`deferred`], so it paints above the panes beside it rather than under
//! the sibling that follows it, and it is [`occlude`] with an
//! `on_mouse_down_out` dismissal, so it claims the clicks it covers and can
//! be walked away from. And one more it inherits from the status bar's
//! honest-hints rule: a command the registry projects is runnable by
//! definition, so there is no disabled row to draw.

use crate::chrome::RADIUS;
use gitten_core::command::{Commands, HelpRow};
use gitten_core::font::Font;
use gitten_core::theme::Theme;
use gpui::*;
use std::rc::Rc;

/// Menu rows stay compact; only the title-bar trigger needed the larger target.
pub(crate) const ROW_H: f32 = 24.0;

/// Air inside the border, top and bottom — the picker list's own `py_1`.
const PAD_Y: f32 = 8.0;
/// Air between the two columns, in characters — the help overlay's `" · "`,
/// for the same reason: the columns are read as two.
const GAP_CHARS: f32 = 3.0;

/// One row of the menu: the command's registry name — what a pick dispatches,
/// and how a client filters without re-walking the projection — and the
/// `(keys, label)` pair the row draws. The label is the registry's short hint
/// where there is one, its doc where there is not: the status bar's own
/// choice, made for the same kind of column.
pub struct Row {
    pub(crate) name: String,
    keys: String,
    label: String,
}

/// The pane's own rows, out of the projection: every `HelpRow::Command` bound
/// in `mode`, and nothing from any other mode — no globals section in a
/// context menu, because a menu that answers "what may I do *here*" has one
/// subject. Reusing `help` rather than asking `core` for a narrower walk,
/// because the projection's row shape is already exactly what a menu row is.
pub fn rows(help: &[HelpRow], mode: &str, commands: &Commands) -> Vec<Row> {
    let mut out = Vec::new();
    let mut mine = false;
    for row in help {
        match row {
            // A heading: everything under it belongs to the mode it names.
            HelpRow::Mode(name) => mine = name == mode,
            HelpRow::Command { name, keys, doc } if mine => out.push(Row {
                name: name.clone(),
                keys: keys.clone(),
                label: commands.hint(name).unwrap_or(doc).into(),
            }),
            _ => {}
        }
    }
    out
}

/// How wide the menu draws: the longest key and the longest label, measured
/// in the host's face — from the font rather than a constant, the same
/// reason the picker list is. A stale width here is a menu that clips its
/// own labels.
///
/// The one decision the menu makes that a test can ask without a window.
pub(crate) fn width(rows: &[Row], font: &Font) -> f32 {
    let keys = rows
        .iter()
        .map(|r| r.keys.chars().count())
        .max()
        .unwrap_or(0);
    let labels = rows
        .iter()
        .map(|r| r.label.chars().count())
        .max()
        .unwrap_or(0);
    (keys as f32 + labels as f32 + GAP_CHARS + 4.0) * font.char_width() + 16.0
}

/// How tall the menu draws, border included — what the clamp needs that a
/// test can also ask without a window.
pub(crate) fn height(row_count: usize) -> f32 {
    row_count as f32 * ROW_H + 2.0 * PAD_Y + 2.0
}

/// Where the menu draws: below-right of the pointer, clamped so it never
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

/// The menu itself. `on_pick` gets the chosen command's *registry name* —
/// dispatch is the caller's, through the one path every key uses — and is
/// responsible for closing, so a pick is one decision and not two.
/// `on_dismiss` closes without picking: a menu that only ends by choosing
/// something is a menu you cannot change your mind about.
pub fn context_menu(
    rows: &[Row],
    theme: &Theme,
    font: &Font,
    at: Point<Pixels>,
    viewport: Size<Pixels>,
    on_pick: impl Fn(&str, &mut Window, &mut App) + 'static,
    on_dismiss: impl Fn(&mut Window, &mut App) + 'static,
) -> AnyElement {
    let c = &theme.chrome;
    let at = clamped(at, viewport, width(rows, font), height(rows.len()));
    let on_pick = Rc::new(on_pick);
    let dismiss = Rc::new(on_dismiss);

    let menu = div()
        .id("context-menu")
        .absolute()
        .top(at.y)
        .left(at.x)
        .w(px(width(rows, font)))
        .py_1()
        .bg(rgb(c.title_bg))
        .border_1()
        .border_color(rgb(c.faint))
        .rounded(px(RADIUS))
        .text_size(px(font.size))
        .font_family(font.family.clone())
        // Without this the menu is drawn but the rows beneath it get the
        // clicks: GPUI hit-tests by paint order, and an absolutely
        // positioned child does not claim the space it covers.
        .occlude()
        .on_mouse_down_out(move |_, window, cx| dismiss(window, cx))
        .children(rows.iter().map(|row| {
            let on_pick = on_pick.clone();
            let name = row.name.clone();
            div()
                .id(SharedString::from(format!("context-row-{name}")))
                .flex()
                .items_center()
                .justify_between()
                .h(px(ROW_H))
                .px_2()
                .cursor_pointer()
                .hover(|s| s.bg(rgb(c.status_bg)))
                // The status bar's ink rule: the key bright, the label dim,
                // so the eye finds the keys and reads labels only when it
                // wants one.
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_color(rgb(c.dim))
                        .child(SharedString::from(row.label.clone())),
                )
                .child(
                    div()
                        .flex_none()
                        .text_color(rgb(c.fg))
                        .child(SharedString::from(row.keys.clone())),
                )
                .on_click(move |_, window, cx| on_pick(&name, window, cx))
        }));

    // Deferred, and this is the whole reason the menu is visible at all —
    // the same reason the picker lists and the help overlay are: painted
    // after every ancestor, where a plain child of the window's column
    // would be under the panes beside it. At priority 1, under only the
    // help panel's own priority 2.
    deferred(menu).with_priority(1).into_any_element()
}

/// The transparent surface behind an open menu.
///
/// Menus paint at deferred priority 1. This paints first at priority 0,
/// occluding the rest of the window so a wheel outside the menu cannot reach
/// the diff underneath, while the menu remains the target inside its own
/// bounds.
pub fn backdrop() -> AnyElement {
    deferred(div().absolute().inset_0().occlude())
        .with_priority(0)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    // By name, not a glob: `use gpui::*` in the parent shadows `#[test]` with
    // GPUI's own attribute macro and every test in here fails to expand.
    use super::{height, rows, width};
    use gitten_core::command::Modes;

    #[test]
    fn a_context_menus_rows_are_the_keymaps_own() {
        // The files pane's stack, as the shell builds it — globals under
        // everything, then the pane's own mode.
        let host = gitten_core::host::Host::new();
        let mut modes = Modes::new();
        modes.push("files");
        let menu = rows(
            &host.keys.help(&host.commands, &modes),
            "files",
            &host.commands,
        );

        // The menu is the keymap's own: every row is a command bound in the
        // pane's mode — none from any other, nothing hardcoded — and each row
        // says exactly what the registry says about its command. The test
        // walks the map rather than quoting it, which is also the seam held:
        // a command an extension registers in the pane's mode is on the menu
        // the day it appears, without an edit to the menu.
        assert!(!menu.is_empty(), "the shipped files mode has verbs");
        for row in &menu {
            assert!(
                host.keys
                    .bindings()
                    .iter()
                    .any(|b| b.mode == "files" && b.command == row.name),
                "{} is bound in the pane's own mode",
                row.name
            );
            let registered = host.commands.get(&row.name).unwrap_or_else(|| {
                panic!("{} came out of the registry, not out of the menu", row.name)
            });
            assert_eq!(
                row.label,
                registered.hint.as_deref().unwrap_or(&registered.doc),
                "the label is the registry's own sentence"
            );
        }
        let no_globals = menu.iter().all(|r| r.name != "quit");
        assert!(
            no_globals,
            "no globals section: quit is bound in global, not here"
        );

        // The seam, said outright: register in the pane's mode at test time.
        let mut host = gitten_core::host::Host::new();
        host.commands.register("ext.verb", "does the thing");
        host.keys.bind("files", "e", "ext.verb").unwrap();
        let mut modes = Modes::new();
        modes.push("files");
        let menu = rows(
            &host.keys.help(&host.commands, &modes),
            "files",
            &host.commands,
        );
        assert!(
            menu.iter().any(|r| r.name == "ext.verb"),
            "a registered-at-test-time command appears without an edit here"
        );
        assert!(
            !menu.iter().any(|r| r.name == "quit"),
            "still no globals section"
        );
    }

    #[test]
    fn the_menu_is_as_wide_as_its_widest_columns_and_tall_as_its_rows() {
        // Computed from the same character arithmetic the element draws at:
        // the two columns are independent — the widest key and the widest
        // label need not sit on one row — the gap between them is the help
        // overlay's ` · `, the picker's four characters of air and sixteen of
        // padding ride along, and the height is the rows' own.
        let font = gitten_core::font::Font::default();
        let ch = font.char_width();
        let row = |keys: &str, label: &str| super::Row {
            name: "a".into(),
            keys: keys.into(),
            label: label.into(),
        };
        let menu = vec![row("space", "stage"), row("c", "commit message here")];
        let expected = ("space".chars().count() + "commit message here".chars().count()) as f32
            * ch
            + (3.0 + 4.0) * ch
            + 16.0;
        assert!((width(&menu, &font) - expected).abs() < 0.001);
        assert!((height(menu.len()) - (menu.len() as f32 * 24.0 + 18.0)).abs() < 0.001);

        // An empty menu is a zero the clamp can hold, not a negative width.
        assert!((width(&[], &font) - (7.0 * ch + 16.0)).abs() < 0.001);
        assert_eq!(height(0), 18.0);
    }
}
