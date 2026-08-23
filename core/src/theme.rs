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
    /// The two halves of a block that moved. Their own surfaces because their
    /// backgrounds are their own colours, and a token's foreground has to be
    /// resolved against whatever it actually lands on — that is the whole reason
    /// this enum exists.
    MovedRemoved,
    MovedAdded,
    /// Text the mouse has selected.
    ///
    /// A surface and not a colour the frontend applies itself, because a
    /// selection covers a comment as readily as a keyword and the whole point of
    /// this enum is that a token's foreground is resolved against whatever it
    /// actually lands on. Without it, `comment` on the selection background is
    /// the one run in the diff nobody can read.
    Selected,
}

impl Surface {
    pub const ALL: [Surface; 8] = [
        Surface::Context,
        Surface::Added,
        Surface::Removed,
        Surface::AddedWord,
        Surface::RemovedWord,
        Surface::MovedRemoved,
        Surface::MovedAdded,
        Surface::Selected,
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
        Self {
            fg,
            bold: false,
            italic: false,
        }
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
    /// The part of a hunk header that is *not* the `@@ … @@` — the enclosing
    /// function git puts after it, which is the half worth reading. The markers
    /// themselves are drawn in the gutter's colour, because a coordinate is
    /// furniture in exactly the way a line number is.
    pub hunk_fg: Rgb,
    /// Line numbers, and the base for every furniture colour resolved against a
    /// row background — see [`Theme::gutter_on`], which is what is actually
    /// drawn.
    pub gutter_fg: Rgb,
    /// A hairline *inside* a diff: the rule between the two halves of a
    /// side-by-side row, and the one above a file header.
    ///
    /// Its own colour and not `gutter_fg`, which it was: that one has to stay
    /// legible as text against five different row backgrounds, and a 1px rule
    /// held to a text floor is a bright line down the middle of the window.
    pub rule: Rgb,
    pub context_bg: Rgb,
    pub context_fg: Rgb,
    pub added_bg: Rgb,
    pub added_fg: Rgb,
    pub added_word_bg: Rgb,
    pub removed_bg: Rgb,
    pub removed_fg: Rgb,
    pub removed_word_bg: Rgb,
    /// The two halves of a block that moved rather than changed.
    ///
    /// Blue-grey rather than a paler red and green, and that is the point: a
    /// moved block is the one thing in a diff you are allowed to *skip*, so it
    /// has to recede from the add/remove hues instead of joining them. The `+`
    /// and `-` stay, so the columns still scan.
    pub moved_removed_bg: Rgb,
    pub moved_added_bg: Rgb,
    /// The half of a side-by-side row with no line in it.
    ///
    /// Its own colour and not `context_bg`, because those two mean opposite
    /// things: context is a line that did not change, and this is the absence of
    /// a line.
    ///
    /// The comparison that decides the value is not the one against a context
    /// row — on a near-black theme *nothing* is more than 1.08:1 darker than
    /// that, so darkness alone could never carry it. It is the comparison
    /// against the row **opposite**, which is the only place an absent cell ever
    /// appears: 1.25:1 against an addition and 1.20:1 against a removal, which
    /// is the same step the changed rows themselves get against context.
    pub absent_bg: Rgb,
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
    /// The hairline between one chrome surface and the next.
    ///
    /// Needed because the surfaces cannot carry the boundary themselves: `bg`,
    /// `title_bg` and `status_bg` are within 1.05:1 of each other in the shipped
    /// theme — a tint that quiet is invisible as an edge, and one loud enough to
    /// be seen would be three competing panels instead of one dark window. A rule
    /// is one pixel and reads at any tint.
    pub border: Rgb,
    /// The row the keyboard is on.
    ///
    /// A background bar and not a foreground change, because the row underneath
    /// already spends its foreground saying what it is — an added line, an
    /// author, a lane colour — and a selection that repainted text would erase
    /// that. Its own field and not `title_bg`: those are the same colour in the
    /// shipped theme and mean unrelated things, and a palette where one colour
    /// means two things is a palette a theme cannot retune.
    pub selection_bg: Rgb,
    /// The text a drag has selected.
    ///
    /// Its own colour and not `selection_bg`: that one is a full-width bar under
    /// a row nobody picked out, and this one has to read as *chosen text* on top
    /// of all six diff backgrounds at once. So it is cool where the diff palette
    /// is warm, and brighter than any row it can land on — a selection you have
    /// to look for is a selection you will keep re-making.
    pub selected_bg: Rgb,
    /// Something the app tried and could not do — a diff that failed to
    /// re-acquire, say. Its own colour and not `diff.dels_fg`: that red means
    /// "this line was removed", and a palette where one colour means two things
    /// is a palette a theme cannot retune.
    pub error: Rgb,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    pub name: String,
    /// Contrast floor for token text against whatever it is drawn on, as a WCAG
    /// 2.1 ratio. 4.5 is the standard for body text; a diff wants its comments to
    /// recede, so the shipped themes sit at 3.5 — legible, still quiet. A colour
    /// that already clears the floor is never touched.
    pub min_contrast: f32,
    /// Contrast floor for the furniture — line numbers and the hunk markers —
    /// against whatever row background they land on.
    ///
    /// Lower than [`Theme::min_contrast`] on purpose, and it is the difference
    /// between reading and glancing: body text is read continuously and takes
    /// the 3.5 floor, whereas a line number is looked up once and should recede
    /// the rest of the time. 3.0 is the WCAG floor for non-body text, and it is
    /// what stops the previous state of this: `gutter_fg` measured **2.05:1** on
    /// a context row and **1.60:1** on a moved one, because the resolution ran
    /// for syntax tokens and nothing else.
    pub min_furniture: f32,
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
    /// `diff.gutter_fg` resolved against every [`Surface`], for the same reason
    /// and by the same code. Indexed by [`Surface::index`].
    gutter: [Rgb; Surface::COUNT],
}

impl Default for Theme {
    fn default() -> Self {
        Self::dark()
    }
}

impl Theme {
    /// The warm, low-contrast dark theme the app opens in. Desaturated on
    /// purpose: the add/remove background already says a line changed, so
    /// syntax colour is here to give the eye structure, not to compete.
    pub fn dark() -> Self {
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
            name: "dark".into(),
            min_contrast: 3.5,
            min_furniture: 3.0,
            syntax,
            diff: DiffPalette {
                file_bg: 0x231e1a,
                file_fg: 0xe8e3dc,
                adds_fg: 0x6fbf73,
                dels_fg: 0xd4736b,
                hunk_bg: 0x111417,
                hunk_fg: 0x7d8fa8,
                gutter_fg: 0x4a4540,
                rule: 0x332c28,
                context_bg: 0x0e0d0c,
                context_fg: 0xa39c93,
                added_bg: 0x16241a,
                added_fg: 0x9dc79b,
                added_word_bg: 0x1e3a23,
                removed_bg: 0x2a1917,
                removed_fg: 0xd4a09a,
                removed_word_bg: 0x43201a,
                moved_removed_bg: 0x191d28,
                moved_added_bg: 0x1d2636,
                absent_bg: 0x070605,
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
                border: 0x282320,
                selection_bg: 0x241f1a,
                selected_bg: 0x2f3b4a,
                error: 0xd4736b,
            },
            lanes: vec![0xdfa851, 0x6f9ecf, 0xa983c9, 0x5fa8a0, 0xc97d6f, 0x8fb35e],
            lane_overflow: 0x453f39,
            // Muted enough to stay out of the graph's way — these sit inches
            // from the lane colours and must not be mistaken for them.
            authors: vec![0x9c8a6b, 0x6f8296, 0x8b7a96, 0x6b8f88, 0x9c7f75, 0x7d8a6b],
            resolved: Vec::new(),
            gutter: [0; Surface::COUNT],
        }
        .rebuilt()
    }

    /// The same theme on paper.
    ///
    /// Not an inversion — a palette that reads well on near-black inverts into a
    /// set of pastels nothing can be drawn on. What is carried across is the
    /// *ratios*: every background here sits the same WCAG step from its context
    /// row as the dark theme's does, and every foreground the same step from what
    /// it lands on, so the hierarchy a reader learns in one is the hierarchy they
    /// get in the other. `docs/measurements.md` has the table both were built to.
    ///
    /// Two numbers are deliberately not carried across, and both are the same
    /// point about a light background:
    ///
    /// - **The accent is 5.2:1, not 9.1:1.** Contrast against paper is darkness,
    ///   and an amber taken to 9:1 is a brown. Loud enough to find, saturated
    ///   enough to still be the accent.
    /// - **`absent_bg` is 1.51:1 from the context row**, where the dark theme's
    ///   is 1.04:1. The comparison that decides it is the one against the row
    ///   *opposite* — 1.25:1 on an addition, in both themes — and on paper there
    ///   is no room left above the background, so it has to come from below.
    pub fn light() -> Self {
        use Kind::*;
        let mut syntax = [Style::fg(0x58524c); Kind::COUNT];
        let mut set = |k: Kind, s: Style| syntax[k.index()] = s;
        set(Comment, Style::fg(0x9b9186).italic());
        set(Str, Style::fg(0x26662f));
        set(Number, Style::fg(0x814e0c));
        set(Keyword, Style::fg(0x734ca2));
        set(Type, Style::fg(0x245e87));
        set(Constant, Style::fg(0x754c16));
        set(Func, Style::fg(0x644c0e));
        set(Property, Style::fg(0x514c44));
        set(Heading, Style::fg(0x2a2722).bold());
        set(Strong, Style::fg(0x3c3730).bold());
        set(Emphasis, Style::fg(0x474138).italic());
        set(Link, Style::fg(0x245e87));

        Self {
            name: "light".into(),
            min_contrast: 3.5,
            min_furniture: 3.0,
            syntax,
            diff: DiffPalette {
                file_bg: 0xe9e5dc,
                file_fg: 0x23201c,
                adds_fg: 0x26642f,
                dels_fg: 0xaa3623,
                hunk_bg: 0xf3f2ed,
                hunk_fg: 0x4c627b,
                gutter_fg: 0xb4afa7,
                rule: 0xd8d0c5,
                context_bg: 0xfaf7f1,
                context_fg: 0x58524c,
                added_bg: 0xdde5d7,
                added_fg: 0x1a4520,
                added_word_bg: 0xb8cfb5,
                removed_bg: 0xf3e4dc,
                removed_fg: 0x82291a,
                removed_word_bg: 0xecd0c7,
                // Lavender, for the reason the dark theme's are blue-grey: the
                // two hues a diff already owns are green and red, and a moved
                // block has to recede from both rather than join one.
                moved_removed_bg: 0xe9e6ed,
                moved_added_bg: 0xdcdaea,
                absent_bg: 0xd0cbc3,
            },
            markdown: MarkdownPalette {
                code_bar: 0xd3ccc0,
                quote_bar: 0x6a97b6,
                marker: 0x89827a,
                rule: 0xcec5b8,
            },
            chrome: ChromePalette {
                bg: 0xfaf7f1,
                fg: 0x23201c,
                dim: 0x89827a,
                faint: 0xb4afa7,
                accent: 0x9a5809,
                title_bg: 0xf5f1eb,
                status_bg: 0xf6f3ec,
                border: 0xe4ded5,
                selection_bg: 0xede4d0,
                selected_bg: 0xaac2e4,
                error: 0xab3623,
            },
            lanes: vec![0x9a5c0e, 0x2c709f, 0x7e5aaa, 0x29776e, 0xa6533b, 0x57732c],
            lane_overflow: 0xbbb7b0,
            authors: vec![0x8a7040, 0x577891, 0x816b99, 0x447e73, 0x95695b, 0x6a7a40],
            resolved: Vec::new(),
            gutter: [0; Surface::COUNT],
        }
        .rebuilt()
    }

    /// Cool where [`Theme::dark`] is warm, and built to the same ratios.
    ///
    /// It exists because "warm dark" is a taste and not a default, and because a
    /// registry with one dark theme in it proves nothing. The one structural
    /// difference is the moved block: blue-grey is what the *background* is here,
    /// so a moved row that borrowed it would be invisible, and it is violet
    /// instead. That is the whole argument for a theme being data — the hue a
    /// colour has to avoid is a property of the theme, not of the diff.
    pub fn slate() -> Self {
        use Kind::*;
        let mut syntax = [Style::fg(0x95a2ae); Kind::COUNT];
        let mut set = |k: Kind, s: Style| syntax[k.index()] = s;
        set(Comment, Style::fg(0x555f6c).italic());
        set(Str, Style::fg(0x79b692));
        set(Number, Style::fg(0xc4a377));
        set(Keyword, Style::fg(0xb08dd1));
        set(Type, Style::fg(0x7babcf));
        set(Constant, Style::fg(0xd5b88a));
        set(Func, Style::fg(0xcbd393));
        set(Property, Style::fg(0xaab3bd));
        set(Heading, Style::fg(0xe3e9ef).bold());
        set(Strong, Style::fg(0xcfd8e0).bold());
        set(Emphasis, Style::fg(0xbfc8d2).italic());
        set(Link, Style::fg(0x78a6ca));

        Self {
            name: "slate".into(),
            min_contrast: 3.5,
            min_furniture: 3.0,
            syntax,
            diff: DiffPalette {
                file_bg: 0x1d242b,
                file_fg: 0xe3e9ef,
                adds_fg: 0x39ae7a,
                dels_fg: 0xf06671,
                hunk_bg: 0x131820,
                hunk_fg: 0x7d92ab,
                gutter_fg: 0x404a54,
                rule: 0x2a313b,
                context_bg: 0x0f1319,
                context_fg: 0x95a2ae,
                added_bg: 0x152826,
                added_fg: 0x78d2aa,
                added_word_bg: 0x1b3d33,
                removed_bg: 0x2c1d23,
                removed_fg: 0xf4969d,
                removed_word_bg: 0x42252b,
                moved_removed_bg: 0x1e1e32,
                moved_added_bg: 0x282542,
                absent_bg: 0x0b0f13,
            },
            markdown: MarkdownPalette {
                code_bar: 0x2c353e,
                quote_bar: 0x36657f,
                marker: 0x616d79,
                rule: 0x303944,
            },
            chrome: ChromePalette {
                bg: 0x0f1319,
                fg: 0xe3e9ef,
                dim: 0x616d79,
                faint: 0x404a54,
                accent: 0x77bfdf,
                title_bg: 0x14181f,
                status_bg: 0x13171e,
                border: 0x212730,
                selection_bg: 0x18252e,
                selected_bg: 0x20405b,
                error: 0xef6470,
            },
            lanes: vec![0x77bfdf, 0x66ab87, 0xaa88ca, 0x55ada7, 0xf06873, 0xa9af78],
            lane_overflow: 0x3a444d,
            authors: vec![0x6d94b1, 0x658b84, 0x887f9f, 0x619494, 0xa28087, 0x798f6c],
            resolved: Vec::new(),
            gutter: [0; Surface::COUNT],
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
                self.resolved[kind.index() * Surface::COUNT + surface.index()] = Style {
                    fg: readable(base.fg, bg, self.min_contrast),
                    ..base
                };
            }
        }
        for surface in Surface::ALL {
            let bg = self.background(surface);
            self.gutter[surface.index()] = readable(self.diff.gutter_fg, bg, self.min_furniture);
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
            Surface::MovedRemoved => self.diff.moved_removed_bg,
            Surface::MovedAdded => self.diff.moved_added_bg,
            Surface::Selected => self.chrome.selected_bg,
        }
    }

    /// The style to draw `kind` in when it lands on `surface`. One index; the
    /// contrast work happened in [`Theme::rebuild`].
    #[inline]
    pub fn syntax_on(&self, kind: Kind, surface: Surface) -> Style {
        self.resolved[kind.index() * Surface::COUNT + surface.index()]
    }

    /// The colour to draw a line number in on `surface`. One index; the contrast
    /// work happened in [`Theme::rebuild`].
    ///
    /// Per surface and not one value, for the reason [`Surface`] exists at all: a
    /// grey that recedes politely on a near-black context row is 1.6:1 on the
    /// blue-grey of a moved block, and the number stops being there.
    #[inline]
    pub fn gutter_on(&self, surface: Surface) -> Rgb {
        self.gutter[surface.index()]
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
        let hash = author
            .bytes()
            .fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32));
        self.authors[hash as usize % self.authors.len()]
    }
}

/// Every theme that exists, and what a picker lists.
///
/// A catalogue and nothing else: unlike [`Differs`](crate::differ::Differs) and
/// [`Wraps`](crate::wrap::Wraps) it holds no selection, because a theme is the
/// one seam whose implementation is *data the config file edits*. What is on
/// screen is [`Host::theme`](crate::host::Host::theme) — a copy taken from here
/// and then given whatever `plait.toml` said on top of it — and two answers to
/// "which theme" would be one too many.
///
/// So a file that names a theme nobody registered is not an error: it is a theme
/// *definition*, and the config layer registers it, which is what puts a palette
/// somebody wrote by hand in the same menu as the shipped ones.
#[derive(Debug, Clone, PartialEq)]
pub struct Themes(Vec<Theme>);

impl Default for Themes {
    fn default() -> Self {
        Self::builtin()
    }
}

impl Themes {
    /// The three shipped palettes, in the order a picker shows them.
    pub fn builtin() -> Self {
        Self(vec![Theme::dark(), Theme::light(), Theme::slate()])
    }

    pub fn empty() -> Self {
        Self(Vec::new())
    }

    /// Adds one, replacing any already registered under the same name — so a
    /// built-in can be *corrected* rather than only added to, exactly as a
    /// differ or a highlighter can. That is what a `[theme]` block naming
    /// `"dark"` does: it is not a second dark, it is this one.
    pub fn register(&mut self, theme: Theme) {
        match self.0.iter().position(|t| t.name == theme.name) {
            Some(i) => self.0[i] = theme,
            None => self.0.push(theme),
        }
    }

    pub fn get(&self, name: &str) -> Option<&Theme> {
        self.0.iter().find(|t| t.name == name)
    }

    pub fn at(&self, i: usize) -> Option<&Theme> {
        self.0.get(i)
    }

    pub fn index_of(&self, name: &str) -> Option<usize> {
        self.0.iter().position(|t| t.name == name)
    }

    /// Every registered name, for a picker and for an error message that says
    /// what the options actually are rather than what they were when it was
    /// written.
    pub fn names(&self) -> Vec<&str> {
        self.0.iter().map(|t| t.name.as_str()).collect()
    }

    /// The one after `name`, wrapping — what a `theme.cycle` key runs. Falls to
    /// the first when the name is not registered, which is what a theme defined
    /// in the file and then renamed leaves behind.
    pub fn after(&self, name: &str) -> Option<&Theme> {
        match self.index_of(name) {
            Some(i) => self.0.get((i + 1) % self.0.len()),
            None => self.0.first(),
        }
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
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
    let toward = if luminance(bg) < 0.35 {
        0xffffff
    } else {
        0x000000
    };
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
        for t in Themes::builtin().0 {
            for kind in Kind::ALL {
                assert_ne!(t.syntax(kind).fg, 0, "{}: {kind:?} was never set", t.name);
            }
        }
    }

    #[test]
    fn every_token_is_legible_on_every_surface() {
        // The regression this exists for: a comment on a changed word measured
        // 1.15:1, which is a grey smear on green. Nothing may sit below the
        // theme's own floor, on any background it can be drawn on.
        //
        // Over the whole registry, and that is the point of running it there: a
        // fourth palette cannot ship illegible, and neither can a third one
        // somebody tuned by hand this afternoon.
        for t in Themes::builtin().0 {
            for kind in Kind::ALL {
                for surface in Surface::ALL {
                    let style = t.syntax_on(kind, surface);
                    let got = contrast(style.fg, t.background(surface));
                    assert!(
                        got >= t.min_contrast - 0.01,
                        "{}: {kind:?} on {surface:?} is {got:.2}:1, floor is {:.2}",
                        t.name,
                        t.min_contrast
                    );
                }
            }
        }
    }

    #[test]
    fn a_colour_that_already_reads_is_left_exactly_alone() {
        // Lifting everything would flatten the palette. Only the failures move.
        let t = Theme::dark();
        assert_eq!(t.syntax_on(Kind::Str, Surface::Context).fg, 0x9aab7d);
        assert_eq!(t.syntax_on(Kind::Heading, Surface::AddedWord).fg, 0xe8e3dc);
        // ...and the one that does not read is lifted, not replaced.
        let lifted = t.syntax_on(Kind::Comment, Surface::AddedWord).fg;
        assert_ne!(lifted, t.syntax_on(Kind::Comment, Surface::Context).fg);
        assert!(lifted > 0x615a52, "lifted toward white on a dark surface");
        assert!(
            t.syntax_on(Kind::Comment, Surface::AddedWord).italic,
            "style survives"
        );
    }

    #[test]
    fn lifting_goes_the_other_way_on_a_light_theme() {
        // The shipped light theme, not a synthetic one: every surface in it has
        // to sit above the luminance where `readable` turns around, and a
        // palette with one surface on the wrong side of that line has its
        // comments lifted *toward* the background they are drawn on.
        let t = Theme::light();
        let base = t.syntax(Kind::Comment).fg;
        let fg = t.syntax_on(Kind::Comment, Surface::AddedWord).fg;
        assert!(
            fg < base,
            "text should darken on paper, got {fg:06x} from {base:06x}"
        );
        assert!(contrast(fg, t.background(Surface::AddedWord)) >= t.min_contrast);
        assert!(
            t.syntax_on(Kind::Comment, Surface::AddedWord).italic,
            "style survives"
        );
    }

    #[test]
    fn a_theme_is_registered_by_name_and_replaced_by_it() {
        let mut r = Themes::builtin();
        assert_eq!(r.names(), vec!["dark", "light", "slate"]);
        assert_eq!(r.get("light").map(|t| t.chrome.bg), Some(0xfaf7f1));

        // What a `[theme]` block naming a built-in does: correct it, rather than
        // add a second entry called the same thing.
        let mut mine = Theme::dark();
        mine.chrome.bg = 0x010203;
        r.register(mine);
        assert_eq!(r.len(), 3, "registering a known name added an entry");
        assert_eq!(r.get("dark").map(|t| t.chrome.bg), Some(0x010203));

        // And one nobody shipped is simply another theme.
        let mut theirs = Theme::slate();
        theirs.name = "solarized-ish".into();
        r.register(theirs);
        assert_eq!(r.names(), vec!["dark", "light", "slate", "solarized-ish"]);
    }

    #[test]
    fn cycling_wraps_and_survives_a_name_nobody_registered() {
        let r = Themes::builtin();
        assert_eq!(r.after("dark").map(|t| t.name.as_str()), Some("light"));
        assert_eq!(r.after("slate").map(|t| t.name.as_str()), Some("dark"));
        // A theme defined in the file and then renamed leaves this behind, and
        // the answer has to be a theme rather than nothing.
        assert_eq!(r.after("gone").map(|t| t.name.as_str()), Some("dark"));
        assert_eq!(Themes::empty().after("dark"), None);
    }

    #[test]
    fn raising_the_floor_is_one_field_and_a_rebuild() {
        let mut t = Theme::dark();
        t.min_contrast = 7.0;
        t.rebuild();
        for kind in Kind::ALL {
            for surface in Surface::ALL {
                assert!(contrast(t.syntax_on(kind, surface).fg, t.background(surface)) >= 6.99);
            }
        }
    }

    #[test]
    fn a_sign_and_its_line_are_legible_on_every_row_they_can_land_on() {
        // `line_colors` draws the `+` and the unhighlighted text of a row in
        // one of three foregrounds, and two of those land on *two* backgrounds
        // each — a moved addition keeps `added_fg` on `moved_added_bg`. None of
        // it goes through `rebuild`, because none of it is a token, so this is
        // the only thing standing between a theme and a `+` nobody can see.
        for t in Themes::builtin().0 {
            let d = &t.diff;
            for (what, fg, bgs) in [
                ("added_fg", d.added_fg, [d.added_bg, d.moved_added_bg]),
                (
                    "removed_fg",
                    d.removed_fg,
                    [d.removed_bg, d.moved_removed_bg],
                ),
                ("context_fg", d.context_fg, [d.context_bg, d.context_bg]),
            ] {
                for bg in bgs {
                    let got = contrast(fg, bg);
                    assert!(
                        got >= t.min_contrast,
                        "{}: {what} on {bg:06x} is {got:.2}:1, floor is {:.2}",
                        t.name,
                        t.min_contrast
                    );
                }
            }
        }
    }

    #[test]
    fn an_absent_cell_is_read_against_the_row_opposite_it() {
        // The comparison that decides `absent_bg` is never the one against a
        // context row: on a near-black theme nothing is more than 1.08:1 darker
        // than that, and on paper nothing is lighter. An absent cell only ever
        // appears *beside* a change, so that is what it has to differ from —
        // and this is the number both shipped directions were built to.
        for t in Themes::builtin().0 {
            let d = &t.diff;
            for (what, bg) in [("added", d.added_bg), ("removed", d.removed_bg)] {
                let got = contrast(d.absent_bg, bg);
                assert!(
                    got >= 1.15,
                    "{}: absent is {got:.2}:1 against {what}",
                    t.name
                );
            }
        }
    }

    #[test]
    fn a_line_number_clears_the_furniture_floor_on_every_surface() {
        // The regression this exists for: `gutter_fg` was 2.05:1 on a context
        // row and 1.60:1 on a moved one, because `rebuild` resolved syntax
        // tokens and nothing else. A line number nobody can read is a column of
        // pixels wide enough to matter and no use at all.
        for t in Themes::builtin().0 {
            for surface in Surface::ALL {
                let got = contrast(t.gutter_on(surface), t.background(surface));
                assert!(
                    got >= t.min_furniture - 0.01,
                    "{}: the gutter on {surface:?} is {got:.2}:1, floor is {:.2}",
                    t.name,
                    t.min_furniture
                );
            }
        }
    }

    #[test]
    fn furniture_recedes_further_than_body_text() {
        // Both floors exist because they are different jobs. If the furniture
        // ever came out as loud as the text it labels, one of them is wrong.
        for t in Themes::builtin().0 {
            assert!(t.min_furniture < t.min_contrast);
            let gutter = contrast(t.gutter_on(Surface::Context), t.diff.context_bg);
            let body = contrast(t.diff.context_fg, t.diff.context_bg);
            assert!(
                gutter < body,
                "{}: gutter {gutter:.2} is not quieter than text {body:.2}",
                t.name
            );
        }
    }

    #[test]
    fn a_file_header_is_a_step_and_a_hunk_header_is_not() {
        // The hierarchy this fixes was inverted: `file_bg` was 1.048:1 against a
        // context row — invisible — while the hunk header, which matters less,
        // was the more prominent band of the two. A file boundary is the most
        // important edge in a diff.
        for t in Themes::builtin().0 {
            let file = contrast(t.diff.file_bg, t.diff.context_bg);
            let hunk = contrast(t.diff.hunk_bg, t.diff.context_bg);
            assert!(
                file > hunk,
                "{}: file {file:.3} does not out-read hunk {hunk:.3}",
                t.name
            );
            assert!(
                file >= 1.15,
                "{}: a file header at {file:.3} is not a boundary",
                t.name
            );
            assert_ne!(
                t.diff.file_bg, t.chrome.title_bg,
                "{}: one colour, two meanings",
                t.name
            );
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
        let mut t = Theme::dark();
        t.name = "solarized-ish".into();
        t.set_syntax(Kind::Comment, Style::fg(0x93a1a1));
        t.diff.added_bg = 0x073642;
        t.diff.moved_added_bg = 0x002b36;
        t.lanes = vec![0xb58900];
        assert_eq!(t.syntax(Kind::Comment).fg, 0x93a1a1);
        assert!(
            !t.syntax(Kind::Comment).italic,
            "the whole style is replaced"
        );
        assert_eq!(t.lane(0), 0xb58900);
        assert_eq!(t.lane(97), 0xb58900, "lane colours cycle");
    }

    #[test]
    fn author_colour_is_stable_and_in_range() {
        let t = Theme::dark();
        assert_eq!(t.author("Junio C Hamano"), t.author("Junio C Hamano"));
        assert!(t.authors.contains(&t.author("anyone at all")));
    }

    #[test]
    fn an_empty_palette_falls_back_instead_of_panicking() {
        let mut t = Theme::dark();
        t.lanes.clear();
        t.authors.clear();
        assert_eq!(t.lane(3), t.chrome.fg);
        assert_eq!(t.author("x"), t.chrome.dim);
    }
}
