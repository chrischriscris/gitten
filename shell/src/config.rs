//! The live [`Host`], as a GPUI global.
//!
//! Everything about the *file* — parsing it, applying it, writing it out,
//! watching it — is `plait_app::config`, shared with every other client. What is
//! left here is the one thing that cannot be shared: how a reloaded host reaches
//! the views, which in GPUI is a global and in a terminal is a flag in an event
//! loop.
//!
//! The two functions this crate actually calls are re-exported, so the call
//! sites did not have to move when the rest of the file did; everything else is
//! `plait_app::config::` at the one place that wants it.

pub use plait_app::config::{load, watch};

use gpui::{px, App, Global, Hsla};
use plait_core::host::Host;
use plait_core::theme::Rgb;
use std::rc::Rc;

/// The current configuration, replaced wholesale on reload rather than mutated
/// in place — so no view can ever see half of a new theme.
pub struct Active(pub Rc<Host>);

impl Global for Active {}

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

/// Hands plait's palette to the one thing in the window that is not drawn from
/// it: `gpui_component`'s scrollbars.
///
/// `gpui_component::init` sets its theme to **Light** and nothing here ever
/// changed it, so the two scrollbars — the only widgets from that library the
/// app uses — were painting a light track and an accent-coloured thumb over a
/// 0x0e0d0c window. That is the objection `controls.rs` documents about
/// `Popover`, except live and on screen.
///
/// Two writes per colour, because the library reads the track off `ThemeColor`
/// and the thumb off `ThemeTokens`, and one of those without the other is half a
/// scrollbar in the wrong palette. `sync_base` is what pushes any of it down to
/// the layer that actually paints — see its own documentation; writing the
/// fields alone does nothing.
///
/// Called again on every config reload, so a scrollbar follows a saved
/// `plait.toml` the way every other colour in the window does.
pub fn sync_widgets(host: &Host, cx: &mut App) {
    let c = host.theme.chrome;
    let hsla = |v: Rgb| Hsla::from(gpui::rgb(v));
    // Dark first: it is the half of the library's own palette that our colours
    // belong to, so anything from it we have *not* named lands somewhere
    // defensible rather than white.
    gpui_component::Theme::change(gpui_component::ThemeMode::Dark, None, cx);
    let t = gpui_component::Theme::global_mut(cx);
    // The track is the window: a scrollbar should be a thumb over the content,
    // not a channel drawn beside it.
    t.scrollbar = hsla(c.bg);
    t.scrollbar_thumb = hsla(c.faint);
    t.scrollbar_thumb_hover = hsla(c.dim);
    t.tokens.scrollbar_thumb = hsla(c.faint).into();
    t.tokens.scrollbar_thumb_hover = hsla(c.dim).into();
    // 2px, matching the controls in the title bar. The library's default is its
    // own radius and reads as a different app's widget.
    t.radius = px(2.);
    gpui_component::Theme::sync_base(cx);
}
