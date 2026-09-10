//! Window chrome: shared furniture, row frames, and the status bar.
//!
//! The workspace draws its regions from one vocabulary: hairline borders,
//! compact text scales, and the spacing ladder below — one currency (the
//! live font's advance), so a control cannot drift from its neighbours.
//! Nothing here holds state or makes a decision: a header is a name and a
//! count; the status bar's segments are spelled by the shell from loaded
//! state. The live key-hint projection used to live here; the workspace bar
//! carries sync state, the staging count, and the Commands door instead,
//! and the help panel remains the command registry's reader.

use gitten_core::font::Font;
use gitten_core::host::Host;
use gitten_core::theme::Surface;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::Icon;

/// One of gitten's own stroke glyphs, tinted and sized. The path names an
/// asset the shell's own [`crate::assets::Assets`] embeds under `gitten/`;
/// `currentColor` in the SVG resolves through `text_color`, so a caller
/// picks the ink the same way it picks it for text. A helper and not a
/// literal at each call site: the size/colour pair is the whole contract and
/// a drifted copy is a glyph that does not match its neighbours.
pub fn icon(path: &'static str, size: f32, color: u32) -> AnyElement {
    Icon::empty()
        .path(path)
        .size(px(size))
        .text_color(rgb(color))
        .into_any_element()
}

/// Height of a pane's header strip. The guide's 28px band is deliberately
/// taller than a 22px data row: enough separation to read as chrome without
/// turning each stacked pane into a card.
pub const HEADER_H: f32 = 28.0;

/// Height of the bottom bar. Twenty-nine pixels per the workspace spec — a
/// readout strip, not a second pane: sync state, the staging count, and the
/// Commands door.
pub const STATUS_H: f32 = 29.0;

/// Left padding of every list row and section label. Ten pixels matches the
/// pane header inset at the shipped font, so labels, status marks and names
/// share one quiet vertical axis.
pub const ROW_PAD: f32 = 10.0;

/// The bar on the selected row's left edge. Two pixels: one is a hairline and
/// reads as an edge, three is a stripe and starts to look like a column.
pub const ROW_BAR: f32 = 2.0;

/// Shared corner radius for controls, keycaps and floating panels.
pub const RADIUS: f32 = 4.0;
/// Title-bar controls and branch status: larger, but still below body text.
pub const TOPBAR_TEXT_SCALE: f32 = 0.93;
/// Repository path and other primary title-bar text.
pub const TITLE_TEXT_SCALE: f32 = 1.0;
/// Bottom-bar hints: full body size, so the shortcuts scan continuously.
pub const STATUS_TEXT_SCALE: f32 = 1.0;

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
///
/// The sentence ends with the one verb a blank pane can always advertise: the
/// key that opens the keymap. Key and word both come from the registries —
/// `?` is the help command's binding, "keys" its footer hint — so a
/// rebinding rewrites the sentence and an unbound help draws no suffix. A
/// keyboard-first app spends its idle panes naming the nearest live key, and
/// a blank pane's nearest key is the one that shows the rest.
/// The sentence's tail: ` · ? keys` — the help command's live binding and
/// its own hint word, both from the registries, so a rebinding rewrites the
/// sentence and an unbound help leaves the sentence alone.
fn empty_suffix(host: &Host, text: &str) -> SharedString {
    host.keys
        .keys_for("help")
        .first()
        .zip(host.commands.hint("help"))
        .map(|(key, hint)| SharedString::from(format!("{text} · {key} {hint}")))
        .unwrap_or_else(|| SharedString::from(text.to_string()))
}

pub fn empty_line(host: &Host, text: SharedString) -> AnyElement {
    let c = host.theme.chrome;
    let text = empty_suffix(host, &text);
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

#[cfg(test)]
mod tests {
    use super::{empty_suffix, gap_l, gap_m, gap_s, gap_xl, gap_xxl};
    use gitten_core::command::{Commands, Keymap};
    use gitten_core::font::Font;
    use gpui::px;

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
    fn an_empty_pane_advertises_the_way_to_the_keys() {
        let host = gitten_core::host::Host::new();
        // The suffix is the help command's binding and its own hint word,
        // both from the registries — a rebinding rewrites the sentence, an
        // unbound command leaves it alone.
        let key = host
            .keys
            .keys_for("help")
            .first()
            .cloned()
            .expect("help is bound");
        let hint = host.commands.hint("help").expect("help carries a hint");
        assert_eq!(
            &*empty_suffix(&host, "working tree clean"),
            format!("working tree clean · {key} {hint}")
        );
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
