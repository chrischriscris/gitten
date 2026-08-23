//! The help overlay: what the keys do, over whatever is underneath.
//!
//! The rows are [`Keymap::help`]'s — `core`'s projection of the *active* modes
//! against the keymap and the command registry, which is the part two clients
//! must not say differently. What is here is only how wide to draw it and in
//! which ink; a binding added by `plait.toml` or by an extension appears in both
//! without either being told.
//!
//! Two GPUI facts shape the element. It is [`deferred`], because it is painted
//! after every ancestor — as a plain child of the window's column it would be
//! under the diff beside it and visible nowhere (the same trap the picker menus
//! dodge). And it is [`occlude`], because hit-testing is paint order too: an
//! overlay that lets clicks fall through is a menu you act on through a hole.

use gpui::*;
use gpui_component::StyledExt as _;
use plait_core::command::HelpRow;
use plait_core::host::Host;

/// One bound row of the panel. Taller than a diff row: a menu row is a target,
/// even one nobody clicks.
const ROW_H: f32 = 24.0;
/// Air inside the border, at each edge.
const PAD: f32 = 16.0;
/// Widest the panel gets. Past this the descriptions are further from their keys
/// than the eye will carry them.
const MAX_W: f32 = 560.0;
/// Narrowest, so a tiny window clips instead of collapsing to a column of
/// unspaced words.
const MIN_W: f32 = 280.0;

/// The overlay, sized from what the registries say is in it.
///
/// A pure function of the host and the modes: nothing here has a list of keys in
/// it, which is the whole test the title-bar pickers set for a control built on
/// a registry.
pub fn overlay(host: &Host, modes: &plait_core::command::Modes) -> AnyElement {
    let c = &host.theme.chrome;
    let rows = host.keys.help(&host.commands, modes);

    // Keys right up to their descriptions, then air, then the description —
    // never wider than `MAX_W`, never narrower than fits the keys alone.
    // Computed in one place so a test can ask it without a window.
    let w = panel_width(&rows, host);

    let font = host.font.family.clone();
    div()
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .child(
            deferred(
                div()
                    .occlude()
                    .v_flex()
                    .w(px(w))
                    .max_h_full()
                    .overflow_hidden()
                    .bg(rgb(c.title_bg))
                    .border_1()
                    .border_color(rgb(c.faint))
                    .rounded(px(4.))
                    .p(px(PAD))
                    .text_size(px(host.font.size))
                    .font_family(font)
                    .text_color(rgb(c.dim))
                    // The heading, and why there is no second list of keys in
                    // it: every chord below came out of the same map this panel
                    // resolves presses against. The close hint is *live* keys
                    // for `help` — through the same walk that decides what a
                    // press means right now — because a key an inner mode takes
                    // over would close nothing, and naming a dead key here is
                    // the one lie a panel of keys must never tell. No live key,
                    // no hint.
                    .child(div().flex_none().pb_2().text_color(rgb(c.accent)).child(
                        SharedString::from(format!(
                            "keys{}",
                            match host.keys.live_keys_for("help", modes).first() {
                                Some(k) => format!("  ·  {k} closes"),
                                None => String::new(),
                            }
                        )),
                    ))
                    .children(rows.into_iter().map(move |row| {
                        match row {
                            HelpRow::Mode(name) => div()
                                .h(px(ROW_H))
                                .flex()
                                .items_center()
                                .pt_1()
                                .text_color(rgb(c.accent))
                                .child(name),
                            HelpRow::Blank => div().h(px(ROW_H / 2.0)),
                            HelpRow::Command { keys, doc } => div()
                                .h(px(ROW_H))
                                .flex()
                                .items_center()
                                .gap_3()
                                .child(
                                    div()
                                        .flex_none()
                                        .justify_end()
                                        .text_color(rgb(c.fg))
                                        .child(SharedString::from(keys)),
                                )
                                .child(
                                    div().min_w_0().truncate().text_color(rgb(c.dim)).child(doc),
                                ),
                        }
                    })),
            )
            .with_priority(2),
        )
        .into_any_element()
}

/// How wide the panel draws, given what the registries projected into it. The
/// one decision the overlay makes that a test can ask without a window: keys
/// up to their descriptions, then air, then the description — never wider than
/// [`MAX_W`], never narrower than [`MIN_W`].
pub(crate) fn panel_width(rows: &[HelpRow], host: &Host) -> f32 {
    let mut key_w = 0.0_f32;
    let mut doc_w = 0.0_f32;
    for row in rows {
        if let HelpRow::Command { keys, doc } = row {
            key_w = key_w.max(str_px(keys, host));
            doc_w = doc_w.max(str_px(doc, host));
        }
    }
    (key_w + str_px(" · ", host).max(12.) + doc_w + PAD).clamp(MIN_W - 2.0 * PAD, MAX_W - 2.0 * PAD)
        + 2.0 * PAD
}

/// How many pixels wide a run of text draws, in the host's face. Exact in a
/// monospaced face and the usual approximation otherwise — the same trade
/// [`crate::views::diff::columns`] makes, for the same reason.
fn str_px(s: &str, host: &Host) -> f32 {
    s.chars().count() as f32 * host.font.char_width()
}

#[cfg(test)]
mod tests {
    // By name, not a glob: `use gpui::*` in the parent shadows `#[test]` with
    // GPUI's own attribute macro and every test in here fails to expand.
    use super::panel_width;
    use plait_core::command::{Commands, Keymap, Modes};
    use plait_core::host::Host;

    const MIN_W: f32 = super::MIN_W;
    const MAX_W: f32 = super::MAX_W;

    #[test]
    fn the_panel_is_wide_enough_for_the_shipped_keys_and_never_unbounded() {
        let host = Host::new();
        // Global mode only, as the overlay shows it before anything is pushed.
        let rows = Keymap::builtin().help(&Commands::builtin(), &Modes::new());
        assert!(!rows.is_empty(), "nothing was projected");
        let w = panel_width(&rows, &host);
        assert!(w >= MIN_W, "{w} below the floor");
        assert!(w <= MAX_W, "{w} past the ceiling");
    }

    #[test]
    fn a_long_description_clamps_rather_than_pushing_the_border_out() {
        let host = Host::new();
        let mut commands = Commands::builtin();
        commands.register(
            "blame.toggle",
            std::iter::repeat_n("x", 400).collect::<String>(),
        );
        let mut keys = Keymap::builtin();
        keys.bind("global", "b", "blame.toggle").unwrap();
        let rows = keys.help(&commands, &Modes::new());
        assert_eq!(panel_width(&rows, &host), MAX_W);
    }

    #[test]
    fn an_empty_projection_still_has_a_floor() {
        let host = Host::new();
        assert_eq!(
            panel_width(
                &Keymap::empty().help(&Commands::empty(), &Modes::new()),
                &host
            ),
            MIN_W
        );
    }
}
