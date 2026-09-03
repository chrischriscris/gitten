//! The help overlay: what the keys do, over whatever is underneath.
//!
//! The rows are [`Keymap::help`]'s — `core`'s projection of the *active* modes
//! against the keymap and the command registry, which is the part two clients
//! must not say differently. What is here is only how wide to draw it and in
//! which ink; a binding added by `gitten.toml` or by an extension appears in both
//! without either being told.
//!
//! Two GPUI facts shape the element. It is [`deferred`], because it is painted
//! after every ancestor — as a plain child of the window's column it would be
//! under the diff beside it and visible nowhere (the same trap the settings
//! panel dodges). And it is [`occlude`], because hit-testing is paint order too: an
//! overlay that lets clicks fall through is a menu you act on through a hole.
//!
//! And one keyboard fact. While the panel stands it owns every press: the shell
//! resolves against [`MODE`] alone, the way it does against `input` for a
//! focused field, so a pane's `D` reads as "not bound" instead of arming a
//! discard behind a screen that is only *describing* it. Which is why the way
//! out and the panel's own scroll are bound in that mode in `core` — a key is
//! data, and the panel is not allowed a `match` of its own.

use crate::chrome::{gap_l, gap_m, gap_s, RADIUS};
use gitten_core::command::HelpRow;
use gitten_core::host::Host;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::StyledExt as _;

/// The mode the panel pushes, and the only one a press resolves against while
/// it is up. Beside [`crate::input::MODE`] and [`crate::panes::MODE`]: the name
/// belongs to whoever pushes it, and `core` holds the bindings.
pub const MODE: &str = "help";

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
/// Most of the panel's inner width the key column may take. A key is a chord and
/// not a sentence, so this never binds on a real map — it is what keeps a
/// 400-character binding out of `gitten.toml` from pushing every description off
/// the panel.
const KEY_SHARE: f32 = 0.6;

/// The overlay, sized from what the registries say is in it.
///
/// A pure function of the host, the modes and the scroll position: nothing here
/// has a list of keys in it, which is the whole test the settings panel sets
/// for a control built on a registry. `scroll` is the shell's — the panel is
/// taller than a laptop window and the keyboard has to be able to reach the
/// tail, which is the one piece of state a pure element cannot hold.
pub fn overlay(
    host: &Host,
    modes: &gitten_core::command::Modes,
    scroll: &ScrollHandle,
) -> AnyElement {
    let c = &host.theme.chrome;
    let rows = host.keys.help(&host.commands, modes);

    // Keys right up to their descriptions, then air, then the description —
    // never wider than `MAX_W`, never narrower than fits the keys alone, and
    // the key column exactly as wide as the widest chord in it. Computed in one
    // place so a test can ask it without a window.
    let m = metrics(&rows, host);

    // How much of the stack is still under the fold, from the frame that drew
    // it: a view cannot know its own size during `render`, so the footer is one
    // frame late and correct — the same trade wrapping makes. `max_offset` is
    // the distance left to scroll and `offset` is negative going down.
    let below = (f32::from(scroll.max_offset().y) + f32::from(scroll.offset().y)).max(0.0);
    let hidden = hidden_below(&rows, below);
    let cut = below > 0.5;

    let font = host.font.family.clone();
    div()
        .absolute()
        .inset_0()
        // The whole overlay, not only its panel: a wheel in the dim space
        // around it must not find the list underneath. And the space really
        // is dim — a scrim of the window colour at half alpha, so the diff
        // behind recedes and the panel's border clears ~1.7:1 against every
        // row background where it cleared as little as 1.35:1 bare.
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
                    .w(px(m.w))
                    .max_h_full()
                    // The panel's own box, not the row stack's: what leaves
                    // through here is a rounded corner, and the rows scroll.
                    .overflow_hidden()
                    .bg(rgb(c.title_bg))
                    .border_1()
                    .border_color(rgb(c.faint))
                    .rounded(px(RADIUS))
                    .p(px(PAD))
                    .text_size(px(host.font.size))
                    .font_family(font)
                    .text_color(rgb(c.dim))
                    // The heading, and why there is no second list of keys in
                    // it: every chord below came out of the same map this panel
                    // resolves presses against. The close hint is *live* keys
                    // for `help` — through the same walk that decides what a
                    // press means right now — because a key an inner mode took
                    // over would close nothing, and naming a dead key here is
                    // the one lie a panel of keys must never tell. No live key,
                    // no hint. Fixed above the scroll, so the way out is on
                    // screen wherever the rows are.
                    .child(
                        div()
                            .flex_none()
                            .pb(gap_m(&host.font))
                            .text_color(rgb(c.accent))
                            .child(SharedString::from(format!(
                                "keys{}",
                                match host.keys.live_keys_for("help", modes).first() {
                                    Some(k) => format!("  ·  {k} closes"),
                                    None => String::new(),
                                }
                            ))),
                    )
                    // The rows, and the only part that scrolls. `id` first —
                    // there is no way into `overflow_y_scroll` without one —
                    // and `min_h_0`, or a flex child is never shorter than its
                    // content and a 40-row map draws straight past the window
                    // with nothing to say it did. The wheel arrives here on its
                    // own: the shell's capture interceptor stands aside while
                    // the panel is up precisely so a handler on it can see the
                    // event.
                    .child(
                        div()
                            .id("help-rows")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .track_scroll(scroll)
                            .children(rows.iter().map(|row| {
                                match row {
                                    HelpRow::Mode(name) => div()
                                        .h(px(ROW_H))
                                        .flex()
                                        .items_center()
                                        .pt(gap_s(&host.font))
                                        .text_color(rgb(c.accent))
                                        .child(name.clone()),
                                    HelpRow::Blank => div().h(px(ROW_H / 2.0)),
                                    HelpRow::Command { keys, doc, .. } => div()
                                        .h(px(ROW_H))
                                        .flex()
                                        .items_center()
                                        .gap(gap_l(&host.font))
                                        // A column and not a run of text: the width
                                        // is the widest chord's, measured once, and
                                        // right-aligned against it — which is what
                                        // the ` · ` gap in the width formula always
                                        // assumed and what a content-sized cell
                                        // never gave, one x per row instead.
                                        .child(
                                            div()
                                                .flex_none()
                                                .w(px(m.key_w))
                                                .flex()
                                                .justify_end()
                                                .text_color(rgb(c.fg))
                                                .child(SharedString::from(keys.clone())),
                                        )
                                        .child(
                                            div()
                                                .min_w_0()
                                                .truncate()
                                                .text_color(rgb(c.dim))
                                                .child(doc.clone()),
                                        ),
                                }
                            })),
                    )
                    // Content cut is said, never silent: a panel that ends mid
                    // list looks exactly like a map with nothing else in it,
                    // and the tail bindings are the ones nobody has learnt.
                    .when(cut, |d| {
                        d.child(
                            div()
                                .flex_none()
                                .h(px(ROW_H))
                                .flex()
                                .items_center()
                                .text_color(rgb(c.faint))
                                .child(SharedString::from(match hidden {
                                    0 => "…".to_string(),
                                    n => format!("…  {n} more below"),
                                })),
                        )
                    }),
            )
            .with_priority(2),
        )
        .into_any_element()
}

/// How far one press moves the panel: a row, in the direction the command
/// names. The shell's `view.scroll-up` / `view.scroll-down` while help is up.
pub fn scroll_by(scroll: &ScrollHandle, rows: f32) {
    let at = scroll.offset();
    let max = f32::from(scroll.max_offset().y);
    // Down is negative, and GPUI clamps this again at prepaint — but a handle
    // that is asked for a position past the end and read back before the next
    // frame would report it, so the clamp is here too.
    let y = (f32::from(at.y) - rows * ROW_H).clamp(-max, 0.0);
    scroll.set_offset(point(at.x, px(y)));
}

/// The two ends. `view.top` / `view.bottom` while help is up.
pub fn scroll_to_end(scroll: &ScrollHandle, bottom: bool) {
    match bottom {
        true => scroll.scroll_to_bottom(),
        false => scroll.set_offset(point(scroll.offset().x, px(0.))),
    }
}

/// What the panel measures before it draws: how wide it is, and how wide the
/// key column inside it is.
///
/// The one decision the overlay makes that a test can ask without a window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct PanelMetrics {
    /// The panel's outer width, padding included.
    pub(crate) w: f32,
    /// The key column's width — the widest chord in the projection, bounded by
    /// [`KEY_SHARE`] of what the panel has inside its padding.
    pub(crate) key_w: f32,
}

/// How wide the panel draws, given what the registries projected into it: keys
/// up to their descriptions, then air, then the description — never wider than
/// [`MAX_W`], never narrower than [`MIN_W`].
pub(crate) fn metrics(rows: &[HelpRow], host: &Host) -> PanelMetrics {
    let mut key_w = 0.0_f32;
    let mut doc_w = 0.0_f32;
    for row in rows {
        if let HelpRow::Command { keys, doc, .. } = row {
            key_w = key_w.max(str_px(keys, host));
            doc_w = doc_w.max(str_px(doc, host));
        }
    }
    let w = (key_w + str_px(" · ", host).max(12.) + doc_w + PAD)
        .clamp(MIN_W - 2.0 * PAD, MAX_W - 2.0 * PAD)
        + 2.0 * PAD;
    PanelMetrics {
        w,
        key_w: key_w.min((w - 2.0 * PAD) * KEY_SHARE),
    }
}

/// How many *bound* rows are entirely under the fold, given how many pixels of
/// the stack are still below it.
///
/// Walked from the end against the heights the rows are drawn at, so the number
/// in the footer counts bindings and not blanks — the footer's whole job is to
/// say that something you would want is down there.
pub(crate) fn hidden_below(rows: &[HelpRow], below: f32) -> usize {
    let mut acc = 0.0;
    let mut hidden = 0;
    for row in rows.iter().rev() {
        acc += match row {
            HelpRow::Blank => ROW_H / 2.0,
            _ => ROW_H,
        };
        if acc > below {
            break;
        }
        if matches!(row, HelpRow::Command { .. }) {
            hidden += 1;
        }
    }
    hidden
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
    use super::{hidden_below, metrics, str_px, ROW_H};
    use gitten_core::command::{Commands, HelpRow, Keymap, Modes};
    use gitten_core::host::Host;

    const MIN_W: f32 = super::MIN_W;
    const MAX_W: f32 = super::MAX_W;

    #[test]
    fn the_panel_is_wide_enough_for_the_shipped_keys_and_never_unbounded() {
        let host = Host::new();
        // Global mode only, as the overlay shows it before anything is pushed.
        let rows = Keymap::builtin().help(&Commands::builtin(), &Modes::new());
        assert!(!rows.is_empty(), "nothing was projected");
        let w = metrics(&rows, &host).w;
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
        assert_eq!(metrics(&rows, &host).w, MAX_W);
    }

    #[test]
    fn an_empty_projection_still_has_a_floor() {
        let host = Host::new();
        assert_eq!(
            metrics(
                &Keymap::empty().help(&Commands::empty(), &Modes::new()),
                &host
            )
            .w,
            MIN_W
        );
    }

    #[test]
    fn the_key_column_is_as_wide_as_the_widest_chord_in_it() {
        let host = Host::new();
        let rows = vec![
            HelpRow::Command {
                name: "a".into(),
                keys: "c".into(),
                doc: "commit what the index holds".into(),
            },
            HelpRow::Command {
                name: "b".into(),
                keys: "ctrl+shift+d".into(),
                doc: "the other thing".into(),
            },
        ];
        // One x for every description, and it is the long chord's — which is
        // the whole of the alignment the draw cannot be asked about.
        assert_eq!(metrics(&rows, &host).key_w, str_px("ctrl+shift+d", &host));
    }

    #[test]
    fn the_footer_counts_the_bindings_under_the_fold_and_not_the_air() {
        let rows = vec![
            HelpRow::Mode("global".into()),
            HelpRow::Command {
                name: "quit".into(),
                keys: "q".into(),
                doc: "quit".into(),
            },
            HelpRow::Blank,
            HelpRow::Mode("files".into()),
            HelpRow::Command {
                name: "files.stage".into(),
                keys: "space".into(),
                doc: "stage".into(),
            },
        ];
        // Nothing under the fold, nothing to say.
        assert_eq!(hidden_below(&rows, 0.0), 0);
        // The last row alone, and it is a binding.
        assert_eq!(hidden_below(&rows, ROW_H), 1);
        // Its heading and the blank above it come with it, and neither counts:
        // a heading with no rows under it is not something you are missing.
        assert_eq!(hidden_below(&rows, ROW_H * 2.5), 1);
        // Everything below, both bindings.
        assert_eq!(hidden_below(&rows, ROW_H * 100.0), 2);
    }
}
