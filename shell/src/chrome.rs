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
use gitten_core::font::Font;
use gitten_core::host::Host;
use gitten_core::theme::Surface;
use gpui::prelude::FluentBuilder as _;
use gpui::*;

/// Height of a pane's header strip. The guide's 28px band is deliberately
/// taller than a 22px data row: enough separation to read as chrome without
/// turning each stacked pane into a card.
pub const HEADER_H: f32 = 28.0;

/// Height of the status bar. It remains denser than the pane headers: the
/// status bar is transient furniture, while pane headers are navigation.
pub const STATUS_H: f32 = 26.0;

/// Left padding of every list row and section label. Ten pixels matches the
/// pane header inset at the shipped font, so labels, status marks and names
/// share one quiet vertical axis.
pub const ROW_PAD: f32 = 10.0;

/// The bar on the selected row's left edge. Two pixels: one is a hairline and
/// reads as an edge, three is a stripe and starts to look like a column.
pub const ROW_BAR: f32 = 2.0;

/// Corner radius for every chip, pill, keycap and floating panel. One value:
/// mixed radii in one compact strip read as different design languages.
pub const RADIUS: f32 = 4.0;
/// Compact text in dense pane furniture.
pub const COMPACT_TEXT_SCALE: f32 = 0.8;
/// Title-bar controls and branch status: larger, but still below body text.
pub const TOPBAR_TEXT_SCALE: f32 = 0.93;
/// Repository path and other primary title-bar text.
pub const TITLE_TEXT_SCALE: f32 = 1.0;

// The chrome's spacing ladder — the whole vocabulary of distance the strips
// spend, in one currency: the live font's advance ([`Font::char_width`]), not
// GPUI's frozen 16px rem. Each step follows the shipped face — JetBrains Mono
// at 15px with a 0.6em advance — and scales with any configured size. The
// larger default therefore gives the whole interface slightly more breathing
// room rather than enlarging glyphs inside frozen padding.

/// Half a character — 5px at the shipped font.
pub fn gap_s(font: &Font) -> Pixels {
    px((font.char_width() * 0.5).round())
}

/// One character — 9px at the shipped font.
pub fn gap_m(font: &Font) -> Pixels {
    px(font.char_width().round())
}

/// One and two fifths — 13px at the shipped font.
pub fn gap_l(font: &Font) -> Pixels {
    px((font.char_width() * 1.4).round())
}

/// One and nine tenths — 17px at the shipped font.
pub fn gap_xl(font: &Font) -> Pixels {
    px((font.char_width() * 1.9).round())
}

/// Two and four fifths — 25px at the shipped font.
pub fn gap_xxl(font: &Font) -> Pixels {
    px((font.char_width() * 2.8).round())
}

/// One hint pair's air, in characters: two between the key and its label,
/// four to the next pair. [`hints`] spends it per pair and [`hints_budget`]
/// reserves the same six around the badge — one number, so the walk and the
/// budget cannot drift into two ideas of how much air a pair costs.
pub(crate) const HINT_AIR_CHARS: f32 = 6.0;

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
/// count at the right edge. It sits low in its 22px row, giving the label the
/// guide's larger top inset without breaking `uniform_list`'s fixed-row
/// contract. Same `ROW_PAD` as the rows, so the column stays a column.
/// `text` arrives already in caps — a static per section — so a heading row
/// costs the frame no string.
pub fn section_label(host: &Host, text: SharedString, count: Option<SharedString>, h: f32) -> Div {
    let c = host.theme.chrome;
    div()
        .flex()
        .items_end()
        .min_w_full()
        .h(px(h))
        .pb(gap_s(&host.font))
        .pl(px(ROW_PAD))
        .text_size(px((host.font.size * 0.72).round()))
        .font_weight(FontWeight::BOLD)
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
/// `bright` — whatever ink the row has earned. `bold_name` lets the repository
/// name carry title-bar emphasis without making filenames inside data panes
/// heavier. The two halves arrive already cut by
/// [`gitten_core::path::split_dir_name`] at flatten or prepare, so the files
/// pane, title strip and diff header agree on where the filename starts and
/// the render path clones two refcounts instead of cutting and copying a
/// string per visible row per frame. No wrapping, because a path is one word
/// to the eye.
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
    bold_name: bool,
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
        .child(
            div()
                .flex_none()
                .text_color(rgb(bright))
                .when(bold_name, |name| name.font_weight(FontWeight::BOLD))
                .child(name),
        )
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
        .pt(gap_m(&host.font))
        .flex()
        .items_start()
        // A sentence someone looks for: through `quiet_on`, because raw
        // `faint` is 2.05:1 here and that is not a sentence, it is a gap.
        .text_color(rgb(host.theme.quiet_on(c.bg)))
        .child(text)
        .into_any_element()
}

/// The keycap a pane header starts with: the number of the key that focuses
/// it, on a small filled key face. Unfocused caps stay quiet on the dedicated
/// `keycap` surface. Focus fills the face with the accent and reverses the
/// numeral into the window background, matching the guide's strongest and
/// smallest navigation signal without adding another mark to the header.
fn keycap(host: &Host, number: &str, focused: bool) -> Div {
    let c = host.theme.chrome;
    let size = px((host.font.char_width() * 1.8).round());
    let (face, border, digit) = match focused {
        true => (c.accent, c.accent, c.bg),
        false => (c.keycap, c.border, c.fg),
    };
    div()
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .w(size)
        .h(size)
        .bg(rgb(face))
        .border_1()
        .border_color(rgb(border))
        .rounded(px(RADIUS - 1.0))
        .text_color(rgb(digit))
        .when(focused, |d| d.font_weight(FontWeight::BOLD))
        .child(SharedString::from(number.to_string()))
}

/// One pane's header strip: keycap, name, and — pushed to the right edge —
/// whatever the pane counts, or anything else the caller has to say. Every
/// header owns the `title_bg` surface; the focused header steps up to `raised`
/// and reverses its keycap into the accent. This keeps focus visible in
/// peripheral vision without turning the entire pane into a bordered card.
///
/// The strip spans its container's width (`w_full`), because the hairline
/// under it is the region's edge and must reach it whatever the name's
/// length. The count is right-edge furniture like anything else the caller
/// passes in `right` — the design pins a section's number against its own
/// right edge, where a drifting count next to a drifting name would wobble —
/// and both are dropped when the pane has nothing worth counting.
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
    let ch = host.font.char_width();
    let surface = match focused {
        true => c.raised,
        false => c.title_bg,
    };
    div()
        .flex_none()
        .w_full()
        .flex()
        .items_center()
        .gap(px((ch * 0.7).round()))
        .h(px(HEADER_H))
        // Nothing paints outside the strip, ever. The right-edge furniture is
        // `flex_none` after a name that can shrink; a deep path must give its
        // directory up before it can push `hunk 2/7` out of the window.
        .overflow_hidden()
        .px(px((ch * 1.2).round()))
        .text_size(px((host.font.size * COMPACT_TEXT_SCALE).round()))
        .bg(rgb(surface))
        .border_b_1()
        // Focus is an accent-tinted rule, not a second stripe beside the
        // keycap. One navigation signal per axis: keycap for the header,
        // left bar for the selected data row.
        .border_color(match focused {
            true => rgb(c.accent).alpha(0.35),
            false => rgb(c.border),
        })
        .child(keycap(host, number, focused))
        // Pane names are navigation, not furniture: keep them at the full
        // desktop body size while keycaps, counts and right-edge metadata stay
        // compact. The wrapper also lets a diff path surrender its directory.
        .child(div().min_w_0().text_size(px(host.font.size)).child(name))
        // Everything after the name is right-edge furniture.
        .child(div().min_w_0().flex_grow(1.0))
        .children(count.map(|count| {
            div()
                .flex_none()
                .text_color(rgb(host.theme.quiet_on(surface)))
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
        .gap(gap_l(&host.font))
        .h(px(STATUS_H))
        .px(gap_m(&host.font))
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
                .px(gap_m(&host.font))
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
                .gap(gap_s(&host.font))
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
            // Key, two spaces of air, label, four to the next pair —
            // [`HINT_AIR_CHARS`].
            let w = (key.chars().count() + label.chars().count()) as f32 * ch + HINT_AIR_CHARS * ch;
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
    (bar_px - ch * (badge.chars().count() as f32 + HINT_AIR_CHARS + version().len() as f32 + 4.0))
        .max(0.0)
}

#[cfg(test)]
mod tests {
    use super::{gap_l, gap_m, gap_s, gap_xl, gap_xxl, hints, HINT_AIR_CHARS};
    use gitten_core::command::{Commands, Keymap, Modes};
    use gitten_core::font::Font;
    use gpui::px;

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
        let one_pair =
            (key.chars().count() + label.chars().count()) as f32 * ch + HINT_AIR_CHARS * ch;
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

    #[test]
    fn the_ladder_tracks_the_larger_default_font() {
        // JetBrains Mono at 15px and a measured 0.6em advance gives a 9px
        // character. Each rung is the rounded multiple documented above.
        let f = Font {
            size: 15.0,
            ..Font::default()
        };
        assert_eq!(gap_s(&f), px(5.0));
        assert_eq!(gap_m(&f), px(9.0));
        assert_eq!(gap_l(&f), px(13.0));
        assert_eq!(gap_xl(&f), px(17.0));
        assert_eq!(gap_xxl(&f), px(25.0));
    }

    #[test]
    fn the_ladder_scales_with_the_glyphs_at_size_20() {
        let d = Font::default();
        let f = Font {
            size: 20.0,
            ..Font::default()
        };
        for (small, grown) in [
            (gap_s(&d), gap_s(&f)),
            (gap_m(&d), gap_m(&f)),
            (gap_l(&d), gap_l(&f)),
            (gap_xl(&d), gap_xl(&f)),
            (gap_xxl(&d), gap_xxl(&f)),
        ] {
            assert!(
                grown > small,
                "size 20 drew {grown}px where the default drew {small}px"
            );
        }
    }
}
