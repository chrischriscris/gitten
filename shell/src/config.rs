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

use gpui::{App, Global};
use plait_core::host::Host;
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
