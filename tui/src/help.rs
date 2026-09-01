//! What the keys do, drawn over whatever is underneath.
//!
//! A pure function of [`Keymap`] and [`Commands`], which is the only interesting
//! thing about it: nothing here has a list of keys in it, so a binding added by
//! `gitten.toml` or by an extension appears without being told to. That is the
//! same test the title-bar pickers pass in the GPUI client — a control that is a
//! pure function of a registry is a control nobody has to remember to update.
//!
//! It shows the *active* modes only. A key bound in `commits` is not a key you
//! can press while reading a diff, and listing it would be a lie in the one
//! place that exists to stop you guessing.

use crate::{
    screen::{Ink, Screen},
    scrollbar::{self, Bar},
};
use gitten_core::command::{Commands, HelpRow, Keymap, Modes};
use gitten_core::theme::Theme;
use gitten_core::view::Viewport;

/// Widest the panel gets, in columns. Past this the descriptions are further
/// from their keys than the eye will carry them.
const MAX_W: usize = 96;
/// Tall enough to scan, short enough to remain a modal rather than a screen.
const MAX_H: usize = 30;
/// Columns between the key column and the description.
const GAP: usize = 2;

/// One row of the panel: a mode heading, or a key and what it does.
enum Row {
    Mode(String),
    Key { keys: String, doc: String },
    Blank,
}

fn panel_height(row_count: usize, available: usize) -> usize {
    let preferred = available.saturating_mul(3).saturating_div(4).max(3);
    (row_count + 2).min(preferred).min(MAX_H).min(available)
}

/// Visible rows and the furthest first row for the current terminal.
pub fn scroll_bounds(
    height: usize,
    host: &gitten_core::host::Host,
    modes: &Modes,
) -> (usize, usize) {
    let len = rows(&host.keys, &host.commands, modes).len();
    let visible = panel_height(len, height).saturating_sub(2);
    (visible, len.saturating_sub(visible))
}

/// The rows, straight out of [`Keymap::help`] — `core`'s projection of what the
/// active modes resolve to, which is the part two clients must not say
/// differently. What is left here is only how wide to draw it and in which ink.
fn rows(keys: &Keymap, commands: &Commands, modes: &Modes) -> Vec<Row> {
    keys.help(commands, modes)
        .into_iter()
        .map(|row| match row {
            HelpRow::Mode(name) => Row::Mode(name),
            HelpRow::Command { name: _, keys, doc } => Row::Key { keys, doc },
            HelpRow::Blank => Row::Blank,
        })
        .collect()
}

/// Draws the panel, centred in the rows `top..top + height`.
///
pub fn paint(
    screen: &mut Screen,
    top: usize,
    height: usize,
    host: &gitten_core::host::Host,
    modes: &Modes,
    offset: usize,
    bar: Bar,
) {
    let theme: &Theme = &host.theme;
    let rows = rows(&host.keys, &host.commands, modes);
    let key_w = rows
        .iter()
        .filter_map(|r| match r {
            Row::Key { keys, .. } => Some(crate::screen::width(keys)),
            _ => None,
        })
        .max()
        .unwrap_or(0);
    let doc_w = rows
        .iter()
        .filter_map(|r| match r {
            Row::Key { doc, .. } => Some(crate::screen::width(doc)),
            _ => None,
        })
        .max()
        .unwrap_or(0);

    // Two columns and a border either side, never wider than the screen.
    let inner = (key_w + GAP + doc_w.max(28))
        .min(MAX_W)
        .min(screen.width().saturating_sub(4));
    let w = (inner + 4).min(screen.width());
    let h = panel_height(rows.len(), height);
    let x = screen.width().saturating_sub(w) / 2;
    let y = top + height.saturating_sub(h) / 2;
    let visible = h.saturating_sub(2);
    let offset = offset.min(rows.len().saturating_sub(visible));

    let border = Ink::new(theme.chrome.faint, theme.chrome.title_bg);
    let title = Ink::new(theme.chrome.accent, theme.chrome.title_bg);
    let key = Ink::new(theme.chrome.fg, theme.chrome.title_bg);
    let doc = Ink::new(theme.chrome.dim, theme.chrome.title_bg);
    let mode = Ink::new(theme.chrome.accent, theme.chrome.title_bg);

    for row in 0..h {
        let mut pen = screen.span(y + row, x, w);
        match row {
            0 => {
                pen.put("╭─ ", border);
                pen.put("keys", title);
                pen.put(" ", border);
                let rest = pen.room().saturating_sub(1);
                pen.fill(rest, '─', border);
                pen.put("╮", border);
            }
            r if r == h - 1 => {
                pen.put("╰", border);
                let rest = pen.room().saturating_sub(1);
                pen.fill(rest, '─', border);
                pen.put("╯", border);
            }
            r => {
                pen.put("│ ", border);
                // Keep the right edge outside the content pen. A description
                // that fills its column must clip before the border; otherwise
                // the pane underneath reads as the rest of the sentence.
                let content_w = pen.room().saturating_sub(1);
                let mut content = pen.take(content_w);
                match rows.get(offset + r - 1) {
                    Some(Row::Mode(name)) => {
                        content.put(name, mode);
                    }
                    Some(Row::Key { keys, doc: text }) => {
                        content.put(keys, key);
                        content.fill(
                            key_w + GAP - crate::screen::width(keys).min(key_w + GAP),
                            ' ',
                            doc,
                        );
                        content.put(text, doc);
                    }
                    _ => {}
                }
                content.wash(border);
                pen.put("│", border);
            }
        }
    }

    // The thumb replaces the right border only where it has information to
    // add; above and below it the modal's own edge remains the track.
    let mut view = Viewport::new();
    view.set_len(rows.len());
    view.set_height(visible);
    view.pan_by(offset as isize);
    scrollbar::paint(
        screen,
        bar,
        x + w.saturating_sub(1),
        None,
        y + 1,
        &view,
        host,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use gitten_core::host::Host;

    fn shown(host: &Host, modes: &Modes) -> Vec<String> {
        shown_at(host, modes, 0)
    }

    fn shown_at(host: &Host, modes: &Modes, offset: usize) -> Vec<String> {
        let mut screen = Screen::new(90, 40);
        screen.clear(Ink::new(host.theme.chrome.fg, host.theme.chrome.bg));
        paint(&mut screen, 0, 40, host, modes, offset, Bar::block());
        (0..40).map(|y| screen.row_text(y)).collect()
    }

    fn contains(rows: &[String], text: &str) -> bool {
        rows.iter().any(|r| r.contains(text))
    }

    #[test]
    fn the_shipped_keys_and_what_they_do_are_both_on_screen() {
        let host = Host::new();
        let rows = shown(&host, &Modes::new());
        assert!(contains(&rows, "keys"), "no title");
        assert!(contains(&rows, "one row down"), "no description");
        assert!(contains(&rows, "leave"), "no description for quit");
        assert!(rows.iter().any(|r| r.contains("q / ctrl-c")), "{rows:?}");
        assert!(
            contains(&rows, "ctrl-d"),
            "a modified key was not spelled out"
        );
    }

    #[test]
    fn a_command_with_several_keys_is_one_row() {
        let host = Host::new();
        let rows = shown(&host, &Modes::new());
        let row = rows
            .iter()
            .find(|r| r.contains("one row down"))
            .expect("view.down");
        assert!(
            row.contains("j / down") || row.contains("down / j"),
            "{row}"
        );
        assert_eq!(
            rows.iter().filter(|r| r.contains("one row down")).count(),
            1
        );
    }

    #[test]
    fn only_the_active_modes_are_listed() {
        // A key bound in `commits` is not a key you can press in a diff, and
        // listing it is a lie in the one place that exists to stop you guessing.
        let host = Host::new();
        let global = shown(&host, &Modes::new());
        assert!(!contains(&global, "the next presentation"));

        let mut modes = Modes::new();
        modes.push("diff");
        let in_diff = shown_at(&host, &modes, usize::MAX);
        assert!(contains(&in_diff, "the next presentation"));
        assert!(contains(&in_diff, "diff"), "the mode is not named");
        assert!(
            !contains(&in_diff, "the diff for this commit"),
            "a commits key leaked in"
        );
    }

    #[test]
    fn a_binding_from_the_config_file_appears_without_being_told_to() {
        // The whole point of the panel being a function of the registry.
        let mut host = Host::new();
        host.commands
            .register("blame.toggle", "show blame beside the diff");
        host.keys.bind("global", "b", "blame.toggle").unwrap();
        let rows = shown_at(&host, &Modes::new(), usize::MAX);
        assert!(contains(&rows, "show blame beside the diff"));
    }

    #[test]
    fn an_unbound_key_is_not_listed_and_an_unbinding_removes_it() {
        let mut host = Host::new();
        assert!(contains(&shown(&host, &Modes::new()), "one row down"));
        host.keys.unbind("global", "j");
        host.keys.unbind("global", "down");
        assert!(!contains(&shown(&host, &Modes::new()), "one row down"));
    }

    #[test]
    fn it_is_bordered_on_every_side_and_centred() {
        let mut host = Host::new();
        host.view.scrollbar = false;
        let mut screen = Screen::new(140, 50);
        screen.clear(Ink::new(host.theme.chrome.fg, host.theme.chrome.bg));
        paint(&mut screen, 0, 50, &host, &Modes::new(), 0, Bar::block());
        let rows: Vec<String> = (0..50).map(|y| screen.row_text(y)).collect();
        let first = rows
            .iter()
            .position(|r| r.contains('╭'))
            .expect("a top border");
        let last = rows
            .iter()
            .rposition(|r| r.contains('╰'))
            .expect("a bottom border");
        assert!(last > first + 2);
        let left = rows[first].find('╭').unwrap();
        let right = rows[first].chars().position(|c| c == '╮').unwrap();
        assert!(left > 0, "not centred: {}", rows[first]);
        assert!(right - left + 1 > 68, "the modal did not grow wider");
        assert!(last - first < MAX_H, "the modal grew too tall");
        assert!(last < 49, "the modal is not vertically centred");
        for row in &rows[first + 1..last] {
            assert_eq!(row.chars().nth(left), Some('│'), "no left border: {row:?}");
            assert_eq!(
                row.chars().nth(right),
                Some('│'),
                "no right border: {row:?}"
            );
        }
    }

    #[test]
    fn an_overflowing_panel_moves_through_the_registered_rows() {
        let host = Host::new();
        let mut modes = Modes::new();
        modes.push("commits");
        let (_, max) = scroll_bounds(40, &host, &modes);
        assert!(max > 0, "the fixture unexpectedly fits in one page");

        let first = shown_at(&host, &modes, 0).join("\n");
        let last = shown_at(&host, &modes, max).join("\n");
        assert!(first.contains("global"), "first page lost the global keys");
        assert!(!first.contains("check out this commit"));
        assert!(last.contains("check out this commit"), "{last}");
    }

    #[test]
    fn a_terminal_too_small_clips_rather_than_panicking() {
        let host = Host::new();
        for (w, h) in [(1, 1), (10, 3), (20, 8), (200, 2)] {
            let mut screen = Screen::new(w, h);
            screen.clear(Ink::new(0, 0));
            paint(&mut screen, 0, h, &host, &Modes::new(), 0, Bar::block());
        }
    }
}
