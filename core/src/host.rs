//! The one place the swappable pieces live.
//!
//! Built-ins and extensions reach the same fields: nothing in here is `const`,
//! nothing is private, and a frontend is handed a `&Host` rather than reaching
//! for a global. That is what makes "anything a built-in does, an extension must
//! be able to do too" checkable — if a feature needs a knob that is not on this
//! struct or on one of its fields, it is not extensible yet.
//!
//! It is deliberately small. Command dispatch landed here as [`Host::keys`] and
//! [`Host::commands`]; the mode stack is [`crate::command::Modes`] and is a
//! client's, because which modes are active is a property of what is on screen
//! rather than of the configuration.

use crate::command::{Commands, Keymap};
use crate::differ::Differs;
use crate::font::Font;
use crate::syntax::Highlighters;
use crate::theme::{Theme, Themes};
use crate::select::Mousing;
use crate::view::Scrolling;
use crate::wrap::Wraps;

pub struct Host {
    /// Which highlighter each path gets. Route a language elsewhere, or replace
    /// the fallback for all of them.
    pub syntax: Highlighters,
    /// Which algorithm turns two files into a diff, and how much context its
    /// hunks carry. Register a new one, select it, or route it to some paths.
    pub differ: Differs,
    /// Which presentation the diff view opens in, by name.
    ///
    /// A `String` and not a type, because `core` never knows a UI exists and a
    /// presentation returns UI elements — the registry of them is a frontend's,
    /// and this is the frontend's *choice* out of it, which is data. Exactly the
    /// same reason `theme.name` is a string: unrecognised is the frontend's
    /// problem to report, not core's to prevent.
    pub layout: String,
    /// Where a line too wide for the window breaks, and whether it breaks at
    /// all — `off` is an entry in this registry rather than a flag beside it.
    ///
    /// A registry here and not just a name, unlike [`Host::layout`], because a
    /// break point is a property of text: `core` can hold the implementations
    /// without knowing a window exists, and a terminal frontend wants the same
    /// three. What the frontend supplies is the column count.
    pub wrap: Wraps,
    /// How far a scroll moves and how much lead the cursor keeps.
    ///
    /// Two numbers rather than a registry, because there is nothing here to swap
    /// — a scroll is arithmetic, and what varies is only how much of it. Read by
    /// whatever runs `view.scroll-down`, so a saved file changes the next notch.
    pub view: Scrolling,
    /// What the mouse does besides select — today, whether a drag copies.
    ///
    /// Its own field and not part of [`Scrolling`]: one is about where a list is
    /// and the other is about what a gesture means, and a config section that
    /// mixes the two is one nobody can guess the shape of.
    pub mouse: Mousing,
    /// Which command each key runs, per mode.
    ///
    /// On `Host` and not in a client for the reason the whole struct exists: a
    /// keybinding is the promise that plait behaves the same in a window, a
    /// browser and a terminal. What `core` resolves is a command *name*; what a
    /// client does with that name is the only part it owns.
    pub keys: Keymap,
    /// Every command name that exists, and one line each.
    ///
    /// Beside the keymap rather than inside it, because a command with no key is
    /// still a command — it is in the help, and a config file can bind it.
    pub commands: Commands,
    /// Every colour the app draws.
    ///
    /// The active theme itself and not a name, unlike [`Host::layout`]: a
    /// palette is data `core` fully understands, and the render path reads a
    /// field off it per run per row per frame. Which of [`Host::themes`] it
    /// started as is its own `name`.
    pub theme: Theme,
    /// Every theme registered, which is what a picker lists and what
    /// [`Host::select_theme`] chooses from.
    ///
    /// Beside the active one rather than holding it, because `theme` is
    /// *edited*: `plait.toml` sets colours on top of whatever it selected, and a
    /// registry that also owned the selection would have to decide whether the
    /// entry or the edit is the truth. This one is the catalogue and `theme` is
    /// the answer.
    pub themes: Themes,
    /// The face it draws in, and the numbers derived from it. More than
    /// appearance — see [`Font`] for what depends on getting it right.
    pub font: Font,
}

impl Default for Host {
    fn default() -> Self {
        Self::new()
    }
}

impl Host {
    /// The shipped configuration: the built-in highlighters and differs, the
    /// dark theme, the default face, the unified presentation.
    pub fn new() -> Self {
        Self {
            syntax: Highlighters::builtin(),
            differ: Differs::builtin(),
            layout: "unified".into(),
            wrap: Wraps::builtin(),
            view: Scrolling::default(),
            mouse: Mousing::default(),
            keys: Keymap::builtin(),
            commands: Commands::builtin(),
            theme: Theme::dark(),
            themes: Themes::builtin(),
            font: Font::default(),
        }
    }

    /// Makes a registered theme the one on screen. False when nothing is
    /// registered under that name, which is what a config file reports back.
    ///
    /// A copy, so the catalogue survives whatever is done to the theme
    /// afterwards — the config file sets colours on top of every selection, and
    /// picking the same theme again has to give back the palette that was
    /// registered rather than the last edit of it.
    pub fn select_theme(&mut self, name: &str) -> bool {
        match self.themes.get(name) {
            Some(theme) => {
                self.theme = theme.clone();
                true
            }
            None => false,
        }
    }

    /// The next theme in the registry, wrapping. What `theme.cycle` runs.
    pub fn cycle_theme(&mut self) {
        if let Some(next) = self.themes.after(&self.theme.name) {
            self.theme = next.clone();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::{Highlighter, Kind, Markdown};
    use crate::theme::Style;

    #[test]
    fn everything_a_built_in_uses_is_reachable_and_replaceable() {
        // The whole of what an extension does at startup, in nine lines.
        let mut host = Host::new();
        host.syntax.route(&["rs", "Cargo.lock"], Markdown);
        assert!(host.differ.select("myers"));
        host.differ.context = 6;
        assert!(host.wrap.select("char"));
        host.themes.register(crate::theme::Theme::slate());
        assert!(host.select_theme("light"));
        // On top of the palette that was just selected, which is the order the
        // config file works in too.
        host.theme.set_syntax(Kind::Heading, Style::fg(0x00ff00).bold());
        host.theme.diff.added_bg = 0x001100;
        host.font = crate::font::Font::menlo();
        host.commands.register("blame.toggle", "show blame beside the diff");
        host.keys.bind("diff", "b", "blame.toggle").unwrap();

        let got = host.syntax.highlight("a.rs", &["# routed away from the scanner"]);
        assert_eq!(got[0][0].kind, Kind::Heading);
        assert_eq!(host.theme.syntax(Kind::Heading).fg, 0x00ff00);
        assert_eq!(host.theme.diff.added_bg, 0x001100);
        assert_eq!(host.theme.name, "light");
        // ...and the catalogue is untouched by any of it: `theme` is a copy, so
        // picking `light` again gives back the palette that was registered.
        assert_eq!(host.themes.names(), vec!["dark", "light", "slate"]);
        assert_ne!(host.themes.get("light").unwrap().diff.added_bg, 0x001100);
        assert_eq!(host.font.family, "Menlo");
        assert_eq!(host.differ.selected(), "myers");
        assert_eq!(host.differ.context, 6);
        assert_eq!(host.wrap.selected(), "char");
        assert!(host.commands.known("blame.toggle"));
        assert_eq!(host.keys.keys_for("blame.toggle"), vec!["b"]);
    }

    #[test]
    fn the_font_is_a_field_and_not_a_constant_somewhere() {
        // The check from `docs/extending.md`, as a test: a knob that is not
        // reachable from `Host` is not a knob. This one was a `const` in
        // `main.rs` with three things quietly depending on it.
        let mut host = Host::new();
        host.font.family = "Iosevka".into();
        host.font.size = 16.0;
        host.font.monospaced = false;
        assert_eq!(host.font.family, "Iosevka");
        assert!(!host.font.monospaced);
    }

    #[test]
    fn the_shipped_diff_defaults_are_the_ones_the_docs_name() {
        let host = Host::new();
        assert_eq!(host.differ.selected(), "histogram");
        assert_eq!(host.differ.context, 3);
        assert_eq!(host.layout, "unified");
        // Wrapping is on out of the box: a diff you have to scroll sideways to
        // read is the problem it exists to solve.
        assert_eq!(host.wrap.selected(), "word");
        // Every shipped binding names a command that exists — the check the
        // config layer runs against the file, applied to the defaults.
        for b in host.keys.bindings() {
            assert!(host.commands.known(&b.command), "{} is not a command", b.command);
        }
    }
}
