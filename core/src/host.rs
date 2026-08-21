//! The one place the swappable pieces live.
//!
//! Built-ins and extensions reach the same fields: nothing in here is `const`,
//! nothing is private, and a frontend is handed a `&Host` rather than reaching
//! for a global. That is what makes "anything a built-in does, an extension must
//! be able to do too" checkable — if a feature needs a knob that is not on this
//! struct or on one of its fields, it is not extensible yet.
//!
//! It is deliberately small. Command dispatch and the mode stack belong here
//! too and are not written; when they are, they land next to these.

use crate::font::Font;
use crate::syntax::Highlighters;
use crate::theme::Theme;

pub struct Host {
    /// Which highlighter each path gets. Route a language elsewhere, or replace
    /// the fallback for all of them.
    pub syntax: Highlighters,
    /// Every colour the app draws.
    pub theme: Theme,
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
    /// The shipped configuration: the built-in highlighters, the dark theme and
    /// the default face.
    pub fn new() -> Self {
        Self {
            syntax: Highlighters::builtin(),
            theme: Theme::default_dark(),
            font: Font::default(),
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
        // The whole of what an extension does at startup, in seven lines.
        let mut host = Host::new();
        host.syntax.route(&["rs", "Cargo.lock"], Markdown);
        host.theme.set_syntax(Kind::Heading, Style::fg(0x00ff00).bold());
        host.theme.diff.added_bg = 0x001100;
        host.font = crate::font::Font::menlo();

        let got = host.syntax.highlight("a.rs", &["# routed away from the scanner"]);
        assert_eq!(got[0][0].kind, Kind::Heading);
        assert_eq!(host.theme.syntax(Kind::Heading).fg, 0x00ff00);
        assert_eq!(host.theme.diff.added_bg, 0x001100);
        assert_eq!(host.font.family, "Menlo");
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
}
