//! Window chrome: the numbered pane headers and the status bar.
//!
//! The design being followed draws every region with the same furniture: a
//! short header strip that names the pane with **the number of the key that
//! focuses it** — numbered in stack order, so no literal here can go stale —
//! and one status bar across the bottom that says where the keyboard is and
//! what the nearest keys do. Both are drawn here, once, so the two regions
//! cannot drift into two different ideas of a header.
//!
//! Nothing here holds state or makes a decision: a header is a number, a
//! name and a count; the status bar is a badge, a list of `(key, label)`
//! pairs and a version. The *content* of the hints is a projection of the
//! same registries the help panel reads — [`Keymap::help`] for what is live,
//! [`Commands::hint`] for the short label — so a key rebound in `gitten.toml`
//! rewrites the bar on the next frame, and a command the focused pane cannot
//! run is never advertised on it. A bar of keys that would not fire is the
//! one lie a keyboard-first app must not tell.

use gitten_core::command::{HelpRow, Modes};
use gitten_core::host::Host;
use gitten_core::theme::Surface;
use gpui::prelude::FluentBuilder as _;
use gpui::*;

/// Height of a pane's header strip — the same strip the module doc
/// describes, so the numbers live in the pane stack and nowhere else.
/// A shade taller than a list row: a header is a label, not a row, and one
/// pixel of extra air is what keeps it from reading as data.
pub const HEADER_H: f32 = 26.0;

/// Height of the status bar. Same air as a header: the two strips bracket
/// the window and should read as a matched pair.
pub const STATUS_H: f32 = 26.0;

/// The left column's slice of the window's width — one column, lazygit's
/// proportions: the three short lists and the commit list stacked in it, the
/// diff taking the rest. It was 0.21 when the column held only short paths
/// and branch names and the commits had a region of their own; merged, the
/// stack inherits the commits' width, because a sha, initials, a subject and
/// an age all live in a row now.
pub const SIDEBAR_SHARE: f32 = 0.32;

/// Left padding of every list row and section label. One number, because
/// the eye runs down a column of rows and a row that starts a pixel later
/// than its neighbours reads as indented on purpose.
pub const ROW_PAD: f32 = 12.0;

/// The bar on the selected row's left edge — and on the focused pane's
/// header. Two pixels: one is a hairline and reads as an edge, three is a
/// stripe and starts to look like a column of its own.
pub const ROW_BAR: f32 = 2.0;

/// Corner radius for every chip, pill, keycap and floating panel. One value:
/// three radii in one 32px strip read as three design languages.
pub const RADIUS: f32 = 4.0;

/// The frame every list row sits in: a fixed height for `uniform_list`, the
/// selection tint when `current`, and the bar on the left edge — accent when
/// the row's pane holds the keyboard, `faint` when the selection is remembered
/// but the keyboard is elsewhere. The bar is drawn on *every* row, in the
/// row's own background when it is not selected, so `ROW_PAD` is always the
/// same distance and the text never shifts a pixel when the cursor moves.
pub fn list_row(host: &Host, current: bool, focused: bool, h: f32) -> Div {
    let c = host.theme.chrome;
    let bg = match current {
        true => c.selection_bg,
        false => c.bg,
    };
    let bar = match (current, focused) {
        (true, true) => c.accent,
        (true, false) => c.faint,
        (false, _) => bg,
    };
    div()
        .flex()
        .items_center()
        .min_w_full()
        .h(px(h))
        .bg(rgb(bg))
        .border_l(px(ROW_BAR))
        .border_color(rgb(bar))
        .pl(px(ROW_PAD - ROW_BAR))
}

/// A section's label inside a list — `STAGED`, `UNSTAGED` — with an optional
/// count at the right edge. Faint and uppercase so it reads as a heading
/// over the rows and never as one of them: it is not selectable, and a
/// label that looked like a row would be one the cursor skips for no reason
/// the eye can see. Same `ROW_PAD` as the rows, so the column stays a column.
/// `text` arrives already in caps — a static per section — so a heading row
/// costs the frame no string.
pub fn section_label(host: &Host, text: SharedString, count: Option<SharedString>, h: f32) -> Div {
    let c = host.theme.chrome;
    div()
        .flex()
        .items_center()
        .min_w_full()
        .h(px(h))
        .pl(px(ROW_PAD))
        // Text, not a border: raw `faint` is 2.05:1 here — half the furniture
        // floor — so "quiet" was in practice invisible. `quiet_on` lifts it to
        // the floor and no further, so the label still reads as a heading over
        // the rows and never as one of them.
        .text_color(rgb(host.theme.quiet_on(c.bg)))
        .child(div().flex_none().child(text))
        // The auto margin carries the distance to the far end; the right
        // reserve is the scrollbar's track, which overlays this edge — the
        // same constant the bar is configured with, so the two cannot
        // disagree.
        .children(count.map(|count| {
            div()
                .flex_none()
                .ml_auto()
                .pr(px(crate::views::SCROLLBAR_TRACK_W))
                .child(count)
        }))
}

/// A path drawn as the design draws one: directory dim, filename in
/// `bright` — whatever ink the row has earned. The two halves arrive already
/// cut, by [`gitten_core::path::split_dir_name`] at flatten or prepare, so
/// the files pane, the title strip and the diff header agree on where the
/// filename starts and the render path clones two refcounts instead of
/// cutting and copying a string per visible row per frame. No wrapping,
/// because a path is one word to the eye.
///
/// When the row is too narrow for the whole path the **directory's head
/// gives and the filename never does** — `min_w_0` and `flex_shrink` let the
/// dim half shrink under a squeezed row, `text_ellipsis_start` leaves
/// `…views/diff.rs` and not `src/views/d…`. A forty-file list is scanned by
/// name; a path that clipped at its right edge would throw away the one
/// part being scanned.
pub fn path_spans(
    host: &Host,
    dir: SharedString,
    name: SharedString,
    bright: u32,
    surface: Surface,
) -> Div {
    div()
        .flex()
        .items_center()
        .min_w_0()
        .whitespace_nowrap()
        // The directory is read as a path — raw dim is under the text floor on
        // the title strips it lands on and on a selected row, so it resolves
        // against the surface the caller paints.
        .child(
            div()
                .min_w_0()
                .flex_shrink(1.0)
                .overflow_hidden()
                .text_ellipsis_start()
                .text_color(rgb(host.theme.dim_on(surface)))
                .child(dir),
        )
        .child(div().flex_none().text_color(rgb(bright)).child(name))
}

/// An empty pane's answer: one quiet faint line, top-left, where a reader
/// scans — a quiet line, not an empty box, and never a sentence centred in a
/// pane where no row ever sits. Shared here because three sidebar panes had
/// drifted into near-identical copies of the same twelve lines, and one blank
/// pane must not mean three different things.
pub fn empty_line(host: &Host, text: SharedString) -> AnyElement {
    let c = host.theme.chrome;
    div()
        .size_full()
        .pl(px(ROW_PAD))
        .pt_2()
        .flex()
        .items_start()
        // A sentence someone looks for: through `quiet_on`, because raw
        // `faint` is 2.05:1 here and that is not a sentence, it is a gap.
        .text_color(rgb(host.theme.quiet_on(c.bg)))
        .child(text)
        .into_any_element()
}

/// The keycap a pane header starts with: the number of the key that focuses
/// the pane, on a small filled key face. The face is the whole point — a
/// bare numeral reads as a count, a key face reads as *press me* — and it is
/// the one place the header may spend ink, because it is the one thing the
/// header teaches. Filled rather than outlined: a one-pixel box at this size
/// is all corners, and the raised face is what makes it a key rather than a
/// badge. Which pane holds the keyboard is the numeral's ink — accent or dim
/// — never the face's, so the caps sit still while focus moves.
fn keycap(host: &Host, number: &str, focused: bool) -> Div {
    let c = host.theme.chrome;
    let ch = host.font.char_width();
    let ink = match focused {
        true => c.accent,
        false => c.dim,
    };
    // The numeral is text and the box around it is a border, so they part
    // ways here: the box keeps `faint` raw — a hairline has no floor — and
    // the digit goes through `quiet_on`, because raw it is 1.95:1 on the
    // strip and a keycap nobody can read teaches nothing.
    let digit = match focused {
        true => c.accent,
        false => host.theme.quiet_on(c.title_bg),
    };
    div()
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .w(px(ch * 1.6))
        .h(px(ch * 1.6))
        .bg(rgb(c.keycap))
        .border_1()
        .border_color(rgb(ink))
        .rounded(px(RADIUS))
        .text_color(rgb(digit))
        .child(SharedString::from(number.to_string()))
}

/// One pane's header strip: keycap, name, and — pushed to the right edge —
/// whatever the pane counts, or anything else the caller has to say. The
/// focused pane's furniture is drawn in the accent and the rest in the
/// dim inks, so the stacked headers answer "where is the keyboard" at a
/// glance without a single extra pixel of chrome.
///
/// The strip spans its container's width (`w_full`), because the hairline
/// under it is the region's edge and must reach it whatever the name's
/// length. The focused pane's header also carries a [`ROW_BAR`] on its left
/// edge, for the header's height only — the same mark a selected row wears,
/// so "which pane" and "which row" are answered by the same shape. Unfocused
/// headers draw the bar in the background, so the keycap does not move when
/// the keyboard does. The count is right-edge furniture like anything else the caller
/// passes in `right` — the design pins a section's number against its own
/// right edge, where a drifting count next to a drifting name would wobble
/// — and both are dropped when the pane has nothing worth counting.
pub fn pane_header(
    host: &Host,
    number: &str,
    name: SharedString,
    count: Option<SharedString>,
    focused: bool,
    right: Option<AnyElement>,
) -> Div {
    let c = host.theme.chrome;
    let name = div()
        .text_color(rgb(match focused {
            true => c.fg,
            // The header strip is where the pane says what it is; dim raw
            // measures 3.37:1 on `title_bg`, under the text floor, so the
            // name resolves against the strip it lands on.
            false => host.theme.dim_on(Surface::Title),
        }))
        .child(name)
        .into_any_element();
    pane_header_with(host, number, name, count, focused, right)
}

/// [`pane_header`] with the name already drawn. For the one header whose
/// name is not a word but a path — the diff pane's `5 internal/host.go`,
/// directory dim and filename bright through [`path_spans`] — where a single
/// ink for the whole name would throw away the one cut the eye wants. The
/// caller owns the name's colours; the header owns everything around it.
pub fn pane_header_with(
    host: &Host,
    number: &str,
    name: AnyElement,
    count: Option<SharedString>,
    focused: bool,
    right: Option<AnyElement>,
) -> Div {
    let c = host.theme.chrome;
    div()
        .flex_none()
        .w_full()
        .flex()
        .items_center()
        .gap_2()
        .h(px(HEADER_H))
        .relative()
        // Nothing paints outside the strip, ever. The right-edge furniture is
        // `flex_none` after a name that only lately learned to shrink; a deep
        // path used to be able to push `hunk 2/7` — the answer to "which hunk
        // will space stage" — clean out of the window, where no amount of
        // looking would find it. Clipping keeps the loss at the edge, where a
        // squeezed [`path_spans`] gives the directory's head up long before
        // anything gets this far.
        .overflow_hidden()
        .px_2()
        // The focused pane's band is lifted one step; the rest stay flat.
        // With the accent bar and the bright name this is the third voice
        // saying "the keyboard is here", and it is the one that works in
        // peripheral vision, where neither ink nor a 2px bar registers.
        .when(focused, |d| d.bg(rgb(c.raised)))
        .border_b_1()
        .border_color(rgb(c.border))
        .child(
            // The bar is an absolute child rather than the strip's own left
            // border: one `border_color` serves every side and the bottom
            // hairline has to stay `border`. Over the padding, so the keycap
            // sits where it always did.
            div()
                .absolute()
                .left_0()
                .top_0()
                .bottom_0()
                .w(px(ROW_BAR))
                .bg(rgb(match focused {
                    true => c.accent,
                    false => c.bg,
                })),
        )
        .child(keycap(host, number, focused))
        // `min_w_0` and not `flex_none`: the one header whose name is a path
        // — the diff pane's, through [`path_spans`] — has to be able to give
        // its directory's head up when the right-edge furniture crowds it. A
        // `flex_none` wrapper would pin the path at full width and let the
        // overflow clip at the strip instead, which is the loss this pane
        // exists not to make. Plain word names never overflow and never
        // notice the difference.
        .child(div().min_w_0().child(name))
        // Everything after the name is right-edge furniture.
        .child(div().min_w_0().flex_grow(1.0))
        .children(count.map(|count| {
            div()
                .flex_none()
                // A count is read, so it goes through `quiet_on` — raw `faint`
                // is 1.95:1 on the strip it sits on.
                .text_color(rgb(host.theme.quiet_on(c.title_bg)))
                .child(count)
        }))
        .children(right)
}

/// The bar across the bottom: where the keyboard is, and what the nearest
/// keys do.
///
/// The badge is the focused pane's mode, in a filled chip — the one filled
/// element in the chrome, because it is the one thing that changes as you
/// work and the one thing worth finding without scanning. `hints` is
/// `(key, label)` pairs already resolved and capped by the caller; the key
/// draws bright and the label dim, so the eye picks the keys out of the bar
/// and reads labels only when it wants one. `truncated` is [`hints`]'s word
/// for "there were more pairs than the width allowed": one faint `…` after
/// the last pair, because a bar that quietly drops its tail advertises keys
/// that were never there — and an omission the reader cannot see is the one
/// kind of lie a hint bar must not tell.
pub fn status_bar(
    host: &Host,
    badge: SharedString,
    hints: &[(SharedString, SharedString)],
    truncated: bool,
    version: &str,
) -> Div {
    let c = host.theme.chrome;
    let chip_ink = c.status_bg;
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap_3()
        .h(px(STATUS_H))
        .px_2()
        .bg(rgb(c.status_bg))
        .border_t_1()
        .border_color(rgb(c.border))
        // The bar is read for where the keyboard is; raw dim is under the
        // text floor on it (3.40), so the bar's text resolves against it.
        .text_color(rgb(host.theme.dim_on(Surface::Status)))
        .child(
            div()
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .px_2()
                .h(px(host.font.char_width() * 1.7))
                .rounded(px(RADIUS))
                .bg(rgb(c.accent))
                .text_color(rgb(chip_ink))
                .child(badge),
        )
        .children(hints.iter().map(|(key, label)| {
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap_1()
                .child(div().flex_none().text_color(rgb(c.fg)).child(key.clone()))
                .child(
                    div()
                        .flex_none()
                        // Read when wanted, glanced past otherwise — raw dim is
                        // under the floor on the bar, so it resolves against it.
                        .text_color(rgb(host.theme.dim_on(Surface::Status)))
                        .child(label.clone()),
                )
        }))
        .children(truncated.then(|| div().flex_none().text_color(rgb(c.faint)).child("…")))
        .child(div().min_w_0().flex_grow(1.0))
        .child(
            div()
                .flex_none()
                // Read, not glanced at — a version nobody can parse might as
                // well be absent — so it clears the furniture floor.
                .text_color(rgb(host.theme.quiet_on(c.status_bg)))
                .child(SharedString::from(version.to_string())),
        )
}

/// What the status bar advertises, for the pane holding the keyboard.
///
/// A projection and no decision, like [`Keymap::help`]: walk the same rows
/// the help panel walks, keep the commands that carry a *hint* — the short
/// status-bar label — and prefer the focused pane's own mode before the
/// globals, because `stage` means more to a files pane than `push` does.
/// Stops when `max_px` is spent, so the bar fills whatever width the window
/// has and never wraps — and says so: the second return is whether it
/// stopped with hints left over. A bar that silently drops its tail is a
/// lie by omission; the bar draws a faint `…` in that case and the reader
/// knows there are keys it is not being shown.
///
/// `active` is the focused screen's mode name — the same string `[keys]`
/// groups bindings under, and the same one [`Modes`] carries innermost, so
/// a prompt holding the keyboard empties this list honestly: an input has
/// no hints but its own, and those are drawn by the field.
pub fn hints(
    host: &Host,
    modes: &Modes,
    active: &str,
    max_px: f32,
) -> (Vec<(SharedString, SharedString)>, bool) {
    let rows = host.keys.help(&host.commands, modes);
    let ch = host.font.char_width();
    // One pass collects each mode's hinted rows in registry order, so the
    // bar's left-to-right order is the registry's — the order `[keys]` and
    // the help panel already agree on.
    let mut per_mode: Vec<(String, Vec<(SharedString, SharedString)>)> = Vec::new();
    for row in rows {
        match row {
            HelpRow::Mode(name) => per_mode.push((name, Vec::new())),
            HelpRow::Command { name, keys, .. } => {
                let Some(hint) = host.commands.hint(&name) else {
                    continue;
                };
                let entry = (
                    SharedString::from(keys),
                    SharedString::from(hint.to_string()),
                );
                if let Some((_, list)) = per_mode.last_mut() {
                    list.push(entry);
                }
            }
            HelpRow::Blank => {}
        }
    }
    // The focused pane's mode first, then the globals a repository answers
    // from anywhere — push and pull ride with every pane.
    let order = [active, "global"];
    let mut out = Vec::new();
    let mut spent = 0.0;
    for mode in order {
        let Some((_, list)) = per_mode.iter().find(|(m, _)| m == mode) else {
            continue;
        };
        for (key, label) in list {
            // Key, two spaces of air, label, four to the next pair.
            let w = (key.chars().count() + label.chars().count() + 6) as f32 * ch;
            if spent + w > max_px {
                return (out, true);
            }
            spent += w;
            out.push((key.clone(), label.clone()));
        }
    }
    (out, false)
}

/// The version the bar signs itself with. The workspace's own version —
/// one number, bumped when the app ships, not per crate.
pub fn version() -> &'static str {
    concat!("gitten ", env!("CARGO_PKG_VERSION"))
}

/// How wide the hints may draw: the bar's width, less the badge as it will
/// actually render — its characters plus the air around it — the version
/// and their air. The badge is a parameter because its length is the one
/// part that varies (`PROMPT` against a mode name) and a helper that
/// guessed it was a second copy of the arithmetic waiting to drift. One
/// home for the sum, so a caller without a window in hand — a test, a
/// second client — asks instead of duplicating it.
pub fn hints_budget(host: &Host, bar_px: f32, badge: &str) -> f32 {
    let ch = host.font.char_width();
    (bar_px - ch * (badge.chars().count() as f32 + 6.0 + version().len() as f32 + 4.0)).max(0.0)
}

#[cfg(test)]
mod tests {
    use super::hints;
    use gitten_core::command::{Commands, Keymap, Modes};

    #[test]
    fn hints_come_from_the_registry_and_prefer_the_active_mode() {
        let host = gitten_core::host::Host::new();
        let mut modes = Modes::new();
        modes.push("files");
        let (out, truncated) = hints(&host, &modes, "files", 4000.0);
        assert!(
            out.iter()
                .any(|(k, l)| l.as_ref() == "stage" && !k.is_empty()),
            "the files pane's stage hint was not projected: {out:?}"
        );
        // Globals ride along after the pane's own.
        assert!(out.iter().any(|(_, l)| l.as_ref() == "push"));
        assert!(!truncated, "a bar wide enough for everything claimed a cut");
    }

    #[test]
    fn a_tiny_budget_returns_only_what_fits() {
        let host = gitten_core::host::Host::new();
        let mut modes = Modes::new();
        modes.push("files");
        let (out, truncated) = hints(&host, &modes, "files", 1.0);
        assert!(out.len() <= 1, "a one-pixel bar held {} hints", out.len());
        assert!(truncated, "a bar that stopped at one pixel said nothing");
    }

    #[test]
    fn a_bar_that_stopped_with_hints_left_says_so() {
        // Exactly wide enough for the first pair: the second does not fit,
        // the walk stops, and the flag has to say the bar was cut — this is
        // the difference between "these are your keys" and "these are all
        // your keys", and only one of them is true.
        let host = gitten_core::host::Host::new();
        let mut modes = Modes::new();
        modes.push("files");
        let (all, _) = hints(&host, &modes, "files", 4000.0);
        assert!(
            all.len() > 1,
            "the files map had more than one hint to give"
        );
        let (key, label) = &all[0];
        let ch = host.font.char_width();
        let one_pair = (key.chars().count() + label.chars().count() + 6) as f32 * ch;
        let (some, truncated) = hints(&host, &modes, "files", one_pair);
        assert_eq!(some.len(), 1, "the budget held exactly one pair");
        assert!(truncated, "a bar that stopped with hints left said nothing");
    }

    #[test]
    fn an_unknown_mode_still_gets_the_globals() {
        let host = gitten_core::host::Host::new();
        let (out, _) = hints(&host, &Modes::new(), "nowhere", 4000.0);
        assert!(out.iter().any(|(_, l)| l.as_ref() == "push"));
    }

    #[test]
    fn every_hinted_command_is_registered_with_the_keymap_it_rides() {
        // A hint on a command no key can reach is furniture for nothing; the
        // shipped map binds every command that carries one.
        let commands = Commands::builtin();
        let keys = Keymap::builtin();
        for name in [
            "files.stage",
            "files.commit",
            "diff.stage-hunk",
            "repo.push",
            "branches.checkout",
            "stashes.apply",
        ] {
            assert!(commands.hint(name).is_some(), "{name} has no hint");
            assert!(
                keys.keys_for(name).iter().any(|k| !k.is_empty()),
                "{name} is hinted but unbound"
            );
        }
    }
}
