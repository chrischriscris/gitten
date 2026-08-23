//! Type, as data.
//!
//! The same argument as [`theme`](crate::theme): a font is a family name and a
//! handful of numbers, and nothing here knows what a window is. The GPUI shell
//! turns these into `font_family` and `text_size`, the ANSI painter ignores all
//! of it because a terminal owns its own type, and a config file will one day
//! deserialize straight into it.
//!
//! It is on [`Host`](crate::host::Host) rather than being a constant in the shell
//! because of the check in `docs/extending.md`: *if a feature needs a knob that is
//! not on `Host` or one of its fields, that feature is not extensible yet.* The
//! font was a `const` in `main.rs`, and three separate things quietly depended on
//! which font that was.
//!
//! # What depends on this being right
//!
//! More than appearance, which is why the fields are what they are:
//!
//! - **[`Font::monospaced`]** decides whether Markdown tables may be aligned by
//!   padding cells with spaces. `markdown::Layout` is handed this; get it wrong
//!   the optimistic way and every table is misaligned by a fraction of a glyph
//!   per cell.
//! - **[`Font::advance`]** is how wide one character is, as a fraction of the
//!   size. The commit list uses it to guess which row is widest, because
//!   `uniform_list` measures exactly one row to decide its scrollable width.
//! - **[`Font::size`]** is the body size every other measurement is relative to,
//!   including the heading scale in the Markdown row presentation and the
//!   22-pixel row height it has to fit inside.

/// Everything the app needs to know about the face it draws in.
///
/// `advance` rather than a pixel width so that changing [`Font::size`] cannot
/// leave a stale character width behind it — the two were separate numbers once
/// and one of them was a comment saying which font it had been measured on.
#[derive(Debug, Clone, PartialEq)]
pub struct Font {
    /// The family name, as the platform knows it. On macOS that is the
    /// *typographic* family — what Font Book groups under and what
    /// `system_profiler SPFontsDataType` prints as `Family:` — so a Nerd Font
    /// build is `JetBrainsMono Nerd Font Mono` and not the four-letter `NFM`
    /// abbreviation in its name table.
    pub family: String,
    /// Body size in pixels. Everything else is measured against it.
    pub size: f32,
    /// Whether one character occupies one column.
    ///
    /// Not decoration: the Markdown row presentation aligns table columns by
    /// padding cells with spaces, which only lines up in a monospaced face. A
    /// proportional font is a supported answer — the tables are then left as
    /// their source rather than drawn as a grid.
    pub monospaced: bool,
    /// Advance width of one character as a fraction of [`Font::size`], for a
    /// monospaced face. Meaningless when `monospaced` is false, and the only
    /// caller guesses a column width with it, so an approximation is fine.
    pub advance: f32,
}

impl Default for Font {
    fn default() -> Self {
        Self::jetbrains_mono()
    }
}

impl Font {
    /// The shipped face.
    ///
    /// **Ligatures are on**, which is a decision rather than an oversight. The
    /// risk is real and specific: intraline highlighting paints a background over
    /// a *byte range*, so a changed word that begins or ends inside `=>` or `!=`
    /// puts a run boundary inside one shaped glyph. The `NL` cut of this family
    /// exists precisely to avoid that and is one string away —
    /// `JetBrainsMonoNL Nerd Font Mono` — if it turns out to look wrong.
    ///
    /// The `Mono` variant, not `Propo` and not the plain `Nerd Font` build:
    /// `Propo` is proportional, and the plain build gives its icons a
    /// double-width advance. Either would shear every table and every
    /// box-drawing rule the Markdown presentation draws.
    ///
    /// `advance` is measured, not guessed: 600 units on a 1000-unit em, so 0.6
    /// exactly. At 14 px that is 8.4 px, which is the same number Menlo was
    /// giving — Menlo's own ratio is 0.602 — so nothing that depended on the old
    /// constant had to move.
    pub fn jetbrains_mono() -> Self {
        Self {
            family: "JetBrainsMono Nerd Font Mono".into(),
            size: 14.0,
            monospaced: true,
            advance: 0.6,
        }
    }

    /// macOS's default terminal face, and what this shipped with before the font
    /// was a knob. Kept as a fallback worth naming: it is present on every mac.
    pub fn menlo() -> Self {
        Self {
            family: "Menlo".into(),
            size: 14.0,
            monospaced: true,
            advance: 0.602,
        }
    }

    /// Width of one character in pixels. Only meaningful when
    /// [`Font::monospaced`], and only ever used to guess a column width.
    pub fn char_width(&self) -> f32 {
        self.size * self.advance
    }

    /// `size` scaled by `factor`, clamped so a scale of zero cannot make text
    /// that occupies no space and a huge one cannot blow out a row.
    pub fn scaled(&self, factor: f32) -> f32 {
        (self.size * factor).clamp(1.0, 512.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shipped_font_is_monospaced_and_says_so() {
        // The Markdown table alignment reads this and pads with spaces on the
        // strength of it.
        let f = Font::default();
        assert!(f.monospaced);
        assert!(!f.family.is_empty());
    }

    #[test]
    fn char_width_follows_the_size() {
        // The bug this shape prevents: a pixel width measured for one size,
        // left behind when the size changed.
        let mut f = Font::jetbrains_mono();
        assert!((f.char_width() - 8.4).abs() < 0.001, "{}", f.char_width());
        f.size = 28.0;
        assert!((f.char_width() - 16.8).abs() < 0.001, "{}", f.char_width());
    }

    #[test]
    fn the_shipped_advance_matches_what_it_replaced() {
        // 8.4px at 14px was measured on Menlo and hardcoded in the commit list.
        // JetBrains Mono lands on the same number, which is why nothing that
        // depended on it had to move.
        assert!((Font::jetbrains_mono().char_width() - Font::menlo().char_width()).abs() < 0.05);
    }

    #[test]
    fn scaling_is_clamped_at_both_ends() {
        let f = Font::jetbrains_mono();
        assert_eq!(f.scaled(1.0), 14.0);
        assert!(
            f.scaled(0.0) >= 1.0,
            "a scale of zero produced invisible text"
        );
        assert!(f.scaled(1e9) <= 512.0, "a huge scale was not clamped");
    }

    #[test]
    fn a_proportional_font_is_expressible() {
        // Not a hypothetical: it is what turns Markdown table padding off.
        let f = Font {
            family: "Helvetica".into(),
            size: 14.0,
            monospaced: false,
            advance: 0.5,
        };
        assert!(!f.monospaced);
    }
}
