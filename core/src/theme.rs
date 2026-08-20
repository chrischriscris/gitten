//! Colour, as data.
//!
//! Every colour the app draws lives here, and nothing here knows what a window
//! is: a theme is a struct of `0xRRGGBB` numbers plus two booleans for weight
//! and slant. The GPUI shell turns them into `Hsla`, the ANSI painter turns the
//! same numbers into escape codes, and a terminal frontend would too. That is
//! the test — a palette that only one frontend can read is a palette living in
//! the wrong crate.
//!
//! Swapping a theme is therefore building one of these. Nothing is `const` and
//! nothing is private except the syntax array, which is indexed rather than
//! matched so a lookup on the render path is one load.

use crate::syntax::Kind;

/// Which background a token is actually drawn on. A single colour per token
/// class is not enough: the same grey that reads as a quiet comment on the
/// near-black context row is illegible on the lighter background a changed word
/// carries, which was measured at 1.15:1 before this existed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    Context,
    Added,
    Removed,
    AddedWord,
    RemovedWord,
}

impl Surface {
    pub const ALL: [Surface; 5] = [
        Surface::Context,
        Surface::Added,
        Surface::Removed,
        Surface::AddedWord,
        Surface::RemovedWord,
    ];
    pub const COUNT: usize = Self::ALL.len();

    pub const fn index(self) -> usize {
        self as usize
    }
}

/// `0xRRGGBB`. Core does not know what a colour is beyond this.
pub type Rgb = u32;

/// How one token class is drawn. Weight and slant are here because emphasis in
/// prose is not a colour — a Markdown `**word**` that only changed hue would be
/// wrong, and the highlighter has no way to say so otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Style {
    pub fg: Rgb,
    pub bold: bool,
    pub italic: bool,
}

impl Style {
    pub const fn fg(fg: Rgb) -> Self {
        Self { fg, bold: false, italic: false }
    }
    pub const fn bold(mut self) -> Self {
        self.bold = true;
        self
    }
    pub const fn italic(mut self) -> Self {
        self.italic = true;
        self
    }
}

/// The diff view's own colours: two backgrounds per line kind (the line and the
/// changed words inside it), plus the furniture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiffPalette {
    pub file_bg: Rgb,
    pub file_fg: Rgb,
    pub adds_fg: Rgb,
    pub dels_fg: Rgb,
    pub hunk_bg: Rgb,
    pub hunk_fg: Rgb,
    pub gutter_fg: Rgb,
    pub context_bg: Rgb,
    pub context_fg: Rgb,
    pub added_bg: Rgb,
    pub added_fg: Rgb,
    pub added_word_bg: Rgb,
    pub removed_bg: Rgb,
    pub removed_fg: Rgb,
    pub removed_word_bg: Rgb,
}

/// The furniture a rendered Markdown row draws in place of the markers it hides.
///
/// Bars rather than backgrounds, deliberately. A row's background in a diff means
/// added, removed or unchanged, and that is the one thing a diff may never give
/// up — so a fenced block and a blockquote are marked by a rule down their left
/// edge instead, which groups a run of rows without touching what the row already
/// says about itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarkdownPalette {
    /// The rule down the left of a fenced code block.
    pub code_bar: Rgb,
    /// The rule down the left of a blockquote.
    pub quote_bar: Rgb,
    /// Bullet glyphs, table pipes, a fence's language label: the punctuation the
    /// renderer draws itself, which should read as structure and not as text.
    pub marker: Rgb,
    /// A thematic break, and a table's separator row.
    pub rule: Rgb,
}

/// Window furniture: the things that are neither a diff nor a graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChromePalette {
    pub bg: Rgb,
    pub fg: Rgb,
    pub dim: Rgb,
    pub faint: Rgb,
    pub accent: Rgb,
    pub title_bg: Rgb,
    pub status_bg: Rgb,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    pub name: String,
    /// Contrast floor for token text against whatever it is drawn on, as a WCAG
    /// 2.1 ratio. 4.5 is the standard for body text; a diff wants its comments to
    /// recede, so the shipped themes sit at 3.5 — legible, still quiet. A colour
    /// that already clears the floor is never touched.
    pub min_contrast: f32,
    /// Indexed by [`Kind::index`], not matched: the render path does this per
    /// token run.
    syntax: [Style; Kind::COUNT],
    pub diff: DiffPalette,
    pub markdown: MarkdownPalette,
    pub chrome: ChromePalette,
    /// Cycled per graph lane, and per author for the initials column. Any
    /// length; the drawing code takes them modulo.
    pub lanes: Vec<Rgb>,
    pub lane_overflow: Rgb,
    pub authors: Vec<Rgb>,
    /// `syntax` resolved against every [`Surface`], filled by [`Theme::rebuild`].
    /// Resolving costs a handful of `powf` per entry and `render` asks for one of
    /// these per run per visible row per frame, so it is computed once.
    resolved: Vec<Style>,
}

impl Default for Theme {
    fn default() -> Self {
        Self::default_dark()
    }
}

impl Theme {
    /// The warm, low-contrast dark theme the app ships with. Desaturated on
    /// purpose: the add/remove background already says a line changed, so
    /// syntax colour is here to give the eye structure, not to compete.
    pub fn default_dark() -> Self {
        use Kind::*;
        let mut syntax = [Style::fg(0xa39c93); Kind::COUNT];
        let mut set = |k: Kind, s: Style| syntax[k.index()] = s;
        set(Comment, Style::fg(0x615a52).italic());
        set(Str, Style::fg(0x9aab7d));
        set(Number, Style::fg(0xc2a06a));
        set(Keyword, Style::fg(0xa88fb5));
        set(Type, Style::fg(0x86a6bd));
        set(Constant, Style::fg(0xd0b48a));
        set(Func, Style::fg(0xd9c98f));
        set(Property, Style::fg(0xb5aea5));
        set(Heading, Style::fg(0xe8e3dc).bold());
        set(Strong, Style::fg(0xd8d2ca).bold());
        set(Emphasis, Style::fg(0xc9c2b9).italic());
        set(Link, Style::fg(0x7fa2bd));

        Self {
            name: "plait dark".into(),
            min_contrast: 3.5,
            syntax,
            diff: DiffPalette {
                file_bg: 0x151312,
                file_fg: 0xe8e3dc,
                adds_fg: 0x6fbf73,
                dels_fg: 0xd4736b,
                hunk_bg: 0x14181d,
                hunk_fg: 0x7d8fa8,
                gutter_fg: 0x4a4540,
                context_bg: 0x0e0d0c,
                context_fg: 0xa39c93,
                added_bg: 0x16241a,
                added_fg: 0x9dc79b,
                added_word_bg: 0x1e3a23,
                removed_bg: 0x2a1917,
                removed_fg: 0xd4a09a,
                removed_word_bg: 0x43201a,
            },
            // Quieter than the syntax palette on purpose: this is punctuation
            // the reader should be able to ignore, standing in for punctuation
            // that is no longer on the row.
            markdown: MarkdownPalette {
                code_bar: 0x35302b,
                quote_bar: 0x4d5f6b,
                marker: 0x6e6862,
                rule: 0x3a352f,
            },
            chrome: ChromePalette {
                bg: 0x0e0d0c,
                fg: 0xe8e3dc,
                dim: 0x6e6862,
                faint: 0x4a4540,
                accent: 0xdfa851,
                title_bg: 0x151312,
                status_bg: 0x131211,
            },
            lanes: vec![0xdfa851, 0x6f9ecf, 0xa983c9, 0x5fa8a0, 0xc97d6f, 0x8fb35e],
            lane_overflow: 0x453f39,
            // Muted enough to stay out of the graph's way — these sit inches
            // from the lane colours and must not be mistaken for them.
            authors: vec![0x9c8a6b, 0x6f8296, 0x8b7a96, 0x6b8f88, 0x9c7f75, 0x7d8a6b],
            resolved: Vec::new(),
        }
        .rebuilt()
    }

    /// Recompute the resolved table. Required after changing `syntax`, `diff` or
    /// `min_contrast` directly; [`Theme::set_syntax`] does it for you.
    pub fn rebuild(&mut self) {
        self.resolved = vec![Style::default(); Kind::COUNT * Surface::COUNT];
        for kind in Kind::ALL {
            for surface in Surface::ALL {
                let base = self.syntax[kind.index()];
                let bg = self.background(surface);
                self.resolved[kind.index() * Surface::COUNT + surface.index()] =
                    Style { fg: readable(base.fg, bg, self.min_contrast), ..base };
            }
        }
    }

    fn rebuilt(mut self) -> Self {
        self.rebuild();
        self
    }

    pub fn background(&self, surface: Surface) -> Rgb {
        match surface {
            Surface::Context => self.diff.context_bg,
            Surface::Added => self.diff.added_bg,
            Surface::Removed => self.diff.removed_bg,
            Surface::AddedWord => self.diff.added_word_bg,
            Surface::RemovedWord => self.diff.removed_word_bg,
        }
    }

    /// The style to draw `kind` in when it lands on `surface`. One index; the
    /// contrast work happened in [`Theme::rebuild`].
    #[inline]
    pub fn syntax_on(&self, kind: Kind, surface: Surface) -> Style {
        self.resolved[kind.index() * Surface::COUNT + surface.index()]
    }

    #[inline]
    pub fn syntax(&self, kind: Kind) -> Style {
        self.syntax[kind.index()]
    }

    pub fn set_syntax(&mut self, kind: Kind, style: Style) {
        self.syntax[kind.index()] = style;
        self.rebuild();
    }

    /// Cycles, so a theme may ship any number of lane colours.
    pub fn lane(&self, i: usize) -> Rgb {
        if self.lanes.is_empty() {
            return self.chrome.fg;
        }
        self.lanes[i % self.lanes.len()]
    }

    /// Stable per author name, so one person's commits clump visibly in a long
    /// list without anyone assigning colours by hand.
    pub fn author(&self, author: &str) -> Rgb {
        if self.authors.is_empty() {
            return self.chrome.dim;
        }
        let hash = author.bytes().fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32));
        self.authors[hash as usize % self.authors.len()]
    }
}

/// Relative luminance, WCAG 2.1.
pub fn luminance(c: Rgb) -> f32 {
    let channel = |shift: u32| {
        let v = ((c >> shift) & 0xff) as f32 / 255.0;
        if v <= 0.03928 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0)
}

/// WCAG 2.1 contrast ratio, 1.0 (identical) to 21.0 (black on white).
pub fn contrast(a: Rgb, b: Rgb) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let ch = |shift: u32| {
        let (x, y) = (((a >> shift) & 0xff) as f32, ((b >> shift) & 0xff) as f32);
        ((x + (y - x) * t).round() as u32).min(255) << shift
    };
    ch(16) | ch(8) | ch(0)
}

/// `fg` if it already clears `target` against `bg`, otherwise `fg` blended
/// toward white on a dark background or black on a light one until it does.
///
/// Blending rather than picking a new colour keeps the hue: a lifted comment is
/// still the same grey-brown, just far enough off the background to read. Themes
/// therefore only have to be *tasteful*, not to enumerate a colour per surface.
pub fn readable(fg: Rgb, bg: Rgb, target: f32) -> Rgb {
    if contrast(fg, bg) >= target {
        return fg;
    }
    let toward = if luminance(bg) < 0.35 { 0xffffff } else { 0x000000 };
    const STEPS: u32 = 24;
    for i in 1..=STEPS {
        let candidate = mix(fg, toward, i as f32 / STEPS as f32);
        if contrast(candidate, bg) >= target {
            return candidate;
        }
    }
    toward
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_has_a_style() {
        let t = Theme::default_dark();
        for kind in Kind::ALL {
            assert_ne!(t.syntax(kind).fg, 0, "{kind:?} was never set");
        }
    }

    #[test]
    fn every_token_is_legible_on_every_surface() {
        // The regression this exists for: a comment on a changed word measured
        // 1.15:1, which is a grey smear on green. Nothing may sit below the
        // theme's own floor, on any background it can be drawn on.
        let t = Theme::default_dark();
        for kind in Kind::ALL {
            for surface in Surface::ALL {
                let style = t.syntax_on(kind, surface);
                let got = contrast(style.fg, t.background(surface));
                assert!(
                    got >= t.min_contrast - 0.01,
                    "{kind:?} on {surface:?} is {got:.2}:1, floor is {:.2}",
                    t.min_contrast
                );
            }
        }
    }

    #[test]
    fn a_colour_that_already_reads_is_left_exactly_alone() {
        // Lifting everything would flatten the palette. Only the failures move.
        let t = Theme::default_dark();
        assert_eq!(t.syntax_on(Kind::Str, Surface::Context).fg, 0x9aab7d);
        assert_eq!(t.syntax_on(Kind::Heading, Surface::AddedWord).fg, 0xe8e3dc);
        // ...and the one that does not read is lifted, not replaced.
        let lifted = t.syntax_on(Kind::Comment, Surface::AddedWord).fg;
        assert_ne!(lifted, t.syntax_on(Kind::Comment, Surface::Context).fg);
        assert!(lifted > 0x615a52, "lifted toward white on a dark surface");
        assert!(t.syntax_on(Kind::Comment, Surface::AddedWord).italic, "style survives");
    }

    #[test]
    fn lifting_goes_the_other_way_on_a_light_theme() {
        let mut t = Theme::default_dark();
        t.diff.added_word_bg = 0xf5f0e8;
        t.set_syntax(Kind::Comment, Style::fg(0xd8d2ca));
        let fg = t.syntax_on(Kind::Comment, Surface::AddedWord).fg;
        assert!(fg < 0xd8d2ca, "dark text on a light background, got {fg:06x}");
        assert!(contrast(fg, 0xf5f0e8) >= t.min_contrast);
    }

    #[test]
    fn raising_the_floor_is_one_field_and_a_rebuild() {
        let mut t = Theme::default_dark();
        t.min_contrast = 7.0;
        t.rebuild();
        for kind in Kind::ALL {
            for surface in Surface::ALL {
                assert!(contrast(t.syntax_on(kind, surface).fg, t.background(surface)) >= 6.99);
            }
        }
    }

    #[test]
    fn contrast_matches_the_wcag_reference_values() {
        assert!((contrast(0xffffff, 0x000000) - 21.0).abs() < 0.01);
        assert!((contrast(0x777777, 0xffffff) - 4.48).abs() < 0.05);
        assert!((contrast(0x123456, 0x123456) - 1.0).abs() < 0.001);
    }

    #[test]
    fn a_theme_can_be_rewritten_field_by_field() {
        // What an extension does: take a theme, change what it likes, hand it
        // back. Nothing here is const and nothing needs a window.
        let mut t = Theme::default_dark();
        t.name = "solarized-ish".into();
        t.set_syntax(Kind::Comment, Style::fg(0x93a1a1));
        t.diff.added_bg = 0x073642;
        t.lanes = vec![0xb58900];
        assert_eq!(t.syntax(Kind::Comment).fg, 0x93a1a1);
        assert!(!t.syntax(Kind::Comment).italic, "the whole style is replaced");
        assert_eq!(t.lane(0), 0xb58900);
        assert_eq!(t.lane(97), 0xb58900, "lane colours cycle");
    }

    #[test]
    fn author_colour_is_stable_and_in_range() {
        let t = Theme::default_dark();
        assert_eq!(t.author("Junio C Hamano"), t.author("Junio C Hamano"));
        assert!(t.authors.contains(&t.author("anyone at all")));
    }

    #[test]
    fn an_empty_palette_falls_back_instead_of_panicking() {
        let mut t = Theme::default_dark();
        t.lanes.clear();
        t.authors.clear();
        assert_eq!(t.lane(3), t.chrome.fg);
        assert_eq!(t.author("x"), t.chrome.dim);
    }
}
