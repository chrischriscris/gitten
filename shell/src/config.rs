//! The live [`Host`], as a GPUI global.
//!
//! Everything about the *file* — parsing it, applying it, writing it out,
//! watching it — is `gitten_app::config`, shared with every other client. What is
//! left here is the one thing that cannot be shared: how a reloaded host reaches
//! the views, which in GPUI is a global and in a terminal is a flag in an event
//! loop.
//!
//! The two functions this crate actually calls are re-exported, so the call
//! sites did not have to move when the rest of the file did; everything else is
//! `gitten_app::config::` at the one place that wants it.

pub use gitten_app::config::{load, watch};

use gitten_core::host::Host;
use gitten_core::theme::Rgb;
use gpui::{App, Global, Hsla};
use std::path::Path;
use std::rc::Rc;

/// The current configuration, replaced wholesale on reload rather than mutated
/// in place — so no view can ever see half of a new theme.
pub struct Active(pub Rc<Host>);

impl Global for Active {}

/// Desktop opening size. The shared host stays at 14px for terminal and web
/// clients; the window starts one pixel larger unless `[font] size` overrides it.
const DESKTOP_FONT_SIZE: f32 = 15.0;

fn desktop_defaults() -> Host {
    let mut host = Host::new();
    host.font.size = DESKTOP_FONT_SIZE;
    host
}

/// The theme the title bar picked, if anything has.
///
/// Client state, and it belongs here for the same reason the diff view keeps its
/// own layout index: `gitten.toml` says what the window *opens* on, and a control
/// in the strip says what it is showing now. The file is rebuilt from defaults on
/// every save — that is what makes deleting a line fall back — so without
/// somewhere outside the host to keep this, saving a colour would silently throw
/// away the theme you are looking at.
///
/// `None` means the file's, which is what the panel shows until somebody
/// touches it.
pub struct Chosen(pub Option<String>);

impl Global for Chosen {}

/// Rebuilds the host from the defaults and the file, re-applies the picked theme
/// and hands the result to every window.
///
/// One path for a save and for a pick, deliberately: two would be two orders in
/// which a theme and a colour can disagree.
///
/// **From defaults every time**, never from the live host — otherwise deleting a
/// line from the file would leave the old value in place and the file would stop
/// describing what is on screen.
pub fn reload(path: &Path, cx: &mut App) -> Vec<String> {
    let mut next = desktop_defaults();
    let mut warnings = load(&mut next, path);
    let chosen = cx.try_global::<Chosen>().and_then(|c| c.0.clone());
    if let Some(name) = chosen {
        if !next.select_theme(&name) {
            // The file renamed or removed it while it was on screen. Fall back
            // to what the file says rather than to nothing.
            warnings.push(format!("the picked theme {name:?} is no longer registered"));
            cx.set_global(Chosen(None));
        }
    }
    let next = Rc::new(next);
    cx.set_global(Active(next.clone()));
    // The scrollbars live in another crate's theme, so they only follow a saved
    // file — or a pick — if something pushes it at them.
    sync_widgets(&next, cx);
    cx.refresh_windows();
    warnings
}

/// Patches the live host in place — a clone, mutated, swapped wholesale — for
/// a knob whose route *is* the host.
///
/// The settings panel's path for everything `gitten.toml` carries that no
/// view owns: the window reads these fields on the render path, so the patch
/// lands on the next frame, and the panel's file write lands them on the next
/// launch. Wholesale rather than mutated in place, so no view can ever see
/// half of a new configuration — the same promise [`reload`] keeps.
pub fn patch(cx: &mut App, tune: impl FnOnce(&mut Host)) {
    let mut next = (*host(cx)).clone();
    tune(&mut next);
    let next = Rc::new(next);
    cx.set_global(Active(next.clone()));
    sync_widgets(&next, cx);
    cx.refresh_windows();
}

/// The current host.
///
/// Views call this **on the render path** rather than holding a clone captured
/// when they were built: a captured `Rc` is a snapshot, and the whole point of a
/// watched config file is that it stops being one. It is a refcount bump.
///
/// This has been got wrong once. `DevShell` held an `Rc<Host>` from startup, so
/// the window chrome and the font for the whole window silently did not
/// hot-reload while every view inside them did.
pub fn host(cx: &App) -> Rc<Host> {
    cx.global::<Active>().0.clone()
}

/// Hands gitten's palette to the one thing in the window that is not drawn from
/// it: `gpui_component`'s scrollbars.
///
/// `gpui_component::init` sets its theme to **Light** and nothing here ever
/// changed it, so its thumb used to paint an unrelated accent over gitten's
/// palette. The track is explicitly transparent: an overlay scrollbar must
/// preserve each row's own background rather than cut a dark rail through
/// additions, removals and the cursor.
///
/// The library reads the track off `ThemeColor` and the thumb off both
/// `ThemeColor` and `ThemeTokens`; `Theme::sync_base` is what reaches the
/// painted layer. Called on startup and every config reload.
pub fn sync_widgets(host: &Host, cx: &mut App) {
    let c = host.theme.chrome;
    let hsla = |v: Rgb| Hsla::from(gpui::rgb(v));
    // Dark first: it is the half of the library's own palette that our colours
    // belong to, so anything from it we have *not* named lands somewhere
    // defensible rather than white.
    gpui_component::Theme::change(gpui_component::ThemeMode::Dark, None, cx);
    let t = gpui_component::Theme::global_mut(cx);
    // An overlay, not a channel: the row's own background continues underneath.
    t.scrollbar = gpui::transparent_black();
    t.scrollbar_thumb = hsla(c.faint);
    t.scrollbar_thumb_hover = hsla(c.dim);
    t.tokens.scrollbar_thumb = hsla(c.faint).into();
    t.tokens.scrollbar_thumb_hover = hsla(c.dim).into();
    gpui_component::Theme::sync_base(cx);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_desktop_opens_one_pixel_larger_without_changing_shared_defaults() {
        assert_eq!(desktop_defaults().font.size, 15.0);
        assert_eq!(Host::new().font.size, 14.0);
    }
}
