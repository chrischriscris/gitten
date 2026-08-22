# Theming

A theme is a struct of `0xRRGGBB` numbers plus two booleans. No GPUI, no `Hsla`,
nothing a terminal could not read — which is the test: a palette only one frontend
can read is a palette in the wrong crate.

```rust
pub struct Theme {
    pub name: String,
    pub min_contrast: f32,        // WCAG 2.1 ratio, the floor for token text
    syntax: [Style; 12],          // per Kind, private because it is resolved
    pub diff: DiffPalette,        // 18 colours: the rows, the words, the furniture
    pub chrome: ChromePalette,    // 10 colours: window, titles, status, selection
    pub lanes: Vec<Rgb>,          // cycled per branch
    pub lane_overflow: Rgb,       // past the 12-lane cap
    pub authors: Vec<Rgb>,        // cycled per author name
    resolved: Vec<Style>,         // syntax × Surface, precomputed
}

pub struct Style { pub fg: Rgb, pub bold: bool, pub italic: bool }
```

Weight and slant are in `Style` because emphasis in prose is not a colour. A
Markdown `**word**` that only changed hue would be wrong.

## Three of them, and where a fourth comes from

`Themes` is the registry and `host.theme` is the one on screen:

```rust
pub struct Themes(Vec<Theme>);        // dark, light, slate

host.select_theme("light");           // a copy, so the registry is not edited
host.cycle_theme();                   // what `T` runs
host.themes.register(mine);           // replaces any entry with the same name
host.themes.names();                  // what the picker lists
```

Unlike `Differs` and `Wraps` the registry holds **no selection**, and that is the
one thing worth reading twice about it. A theme is the only seam whose
implementation is *data the config file edits*: `plait.toml` sets colours on top
of whatever it selected, so a registry that also owned the selection would have to
decide whether the entry or the edit is the truth. The catalogue is `themes`, the
answer is `theme`, and `theme.name` is which.

The consequence is the good one: **a theme written in `plait.toml` is a theme.**
The config layer applies the file to whatever `name` selected and then registers
the result back under that name, so a palette somebody tuned by hand is in the
same registry — and therefore the same title-bar menu and the same `T` — as the
three that ship. A `name` nobody registered is a new entry rather than an error;
a `name` that *is* registered corrects that entry rather than adding a second one
called the same thing, exactly as registering a differ does.

The three shipped are `dark` (warm, near-black), `light` (the same palette on
paper) and `slate` (cool). Two are there to make a point the first cannot: warm
dark is a taste and not a default, and a registry with one dark theme in it
proves nothing about the seam.

### A second palette is a port of the first, not a new one

Every ratio in `light` and `slate` is `dark`'s, hue for hue: `added_bg` sits
1.20:1 from its context row in all three, `file_bg` 1.18:1, the changed-word
background 1.29:1 from the line it is inside, the gutter 2.05:1 before it is
lifted. That is what makes the second theme feel like the first — a floor keeps a
palette legible, but *hierarchy* is what a reader actually learns, and hierarchy
is a set of ratios rather than a set of colours.

```sh
cargo run -q -p plait-core --example contrast          # every theme, every ratio
cargo run -q -p plait-core --example contrast light
```

That is the tool the two new palettes were built with, and it is the one to run
before adding a fourth: take dark's column as the target, pick the hue, and solve
for the tint that lands on the number. Two ratios could not be carried across and
both are the same point about a light background — the accent is 5.2:1 rather than
9.1:1, because an amber taken to 9:1 against paper is a brown, and `absent_bg` is
1.51:1 from its context row rather than 1.04:1, because on paper there is no room
left *above* the background and the step has to come from below. The comparison
that decides it is unchanged: 1.25:1 against the row opposite, in every theme.

## Surfaces, and why one colour per class is not enough

A token is not drawn on "the background". It is drawn on one of eight:

```
  Context       #0e0d0c   the near-black body of the file
  Added         #16241a   an added line
  Removed       #2a1917   a removed line
  AddedWord     #1e3a23   a changed word inside an added line
  RemovedWord   #43201a   a changed word inside a removed line
  MovedRemoved  #191d28   the two halves of a block that moved rather than
  MovedAdded    #1d2636     changed — blue-grey, so they recede from the hues
  Selected      #2f3b4a   text the mouse is holding
```

The last one is why a selection is a surface and not a colour the view applies:
it covers a comment as readily as a keyword, and `comment` at #615a52 on #2f3b4a
is the one run in the diff nobody could read.

The comment grey that reads as pleasantly quiet on `Context` measured **1.15:1**
against the old changed-word background — a grey smear on green, which is how this
was found. Full matrix in [measurements.md](measurements.md).

So every class is resolved against every surface:

```rust
theme.syntax_on(Kind::Comment, Surface::AddedWord)   // one index, no maths
```

## Contrast resolution

```rust
pub fn luminance(c: Rgb) -> f32              // WCAG 2.1 relative luminance
pub fn contrast(a: Rgb, b: Rgb) -> f32       // 1.0 … 21.0
pub fn readable(fg: Rgb, bg: Rgb, target: f32) -> Rgb
```

`readable` returns `fg` untouched if it already clears the target. Otherwise it
blends toward white — or black, if the background is light — in 24 steps, stopping
at the first that clears. Blending rather than substituting keeps the hue: a
lifted comment is the same grey-brown, just far enough off the background to read.

The shipped floor is **3.5**, not the WCAG body-text 4.5. A diff wants its comments
to recede, and lifting comments to 4.5 makes them louder than the code around
them. It is one public field; raise it and call `rebuild()`.

The lift alone was not enough. Data said darken the changed-word backgrounds too,
because taking the comment all the way to `#b7b3b0` made it the loudest thing on
the line. Both halves are [decisions/0009](decisions/0009-contrast-resolution.md).

Themes therefore only have to be *tasteful*. They never enumerate a colour per
surface; the floor guarantees the rest.

## Two floors, because reading and glancing are different jobs

`min_contrast` is for text that is read: token colour, 3.5. `min_furniture` is for
text that is *looked up* — line numbers, the `@@ -41,9 +41,11 @@` half of a hunk
header — and it is **3.0**, the WCAG floor for anything that is not body copy.

It exists because the resolution above ran for syntax tokens and nothing else, and
the furniture was measured at **2.05:1** on a context row and **1.60:1** on a moved
one. A line number nobody can read is a column wide enough to matter and no use at
all. Same machinery, same `readable`, one more table:

```rust
theme.gutter_on(Surface::MovedAdded)     // one index, no maths
```

Which surface a row is, incidentally, is not a renderer's decision either:

```rust
runs::surfaces(kind, moved)   // -> (the row's surface, its changed-words' surface)
```

Three presentations and two frontends ask that question, and a client that
answered it locally would be resolving a token against a background it is not
drawn on.

## What a hairline is for

Three colours in the palette are never text and never a surface — `chrome.border`,
`diff.rule` and the Markdown bars. They exist because a *tint* cannot carry an
edge in a dark theme: `chrome.bg`, `title_bg` and `status_bg` are within **1.05:1**
of each other, which is invisible as a boundary, and pulling them apart far enough
to see would make the window three competing panels. One pixel reads at any tint.

`diff.rule` is separate from `gutter_fg` for the reason every split field in here
is separate: that colour has to clear a *text* floor against five row backgrounds,
and a full-height line held to a text floor is a bright seam down the middle of
the window. It was that colour once.

## Cost, and where it is paid

`readable` costs six `powf` per call and `render` asks for a style per run per
visible row per frame. So resolution happens once, in `rebuild()`, into a
`12 × 5` table of `Style`. Render is one index.

The consequence to remember: **after mutating `syntax`, `diff` or `min_contrast`
directly, call `rebuild()`.** `set_syntax` does it for you. There is no way to
enforce this in the type system without closing the struct, and open fields are
worth more here.

## Writing one

```rust
let mut theme = Theme::dark();                                   // or light, or slate
theme.name = "solarized-ish".into();
theme.set_syntax(Kind::Comment, Style::fg(0x93a1a1).italic());   // rebuilds
theme.diff.added_bg = 0x073642;
theme.diff.added_word_bg = 0x0a4a56;
theme.min_contrast = 4.5;
theme.lanes = vec![0xb58900, 0x268bd2, 0xd33682];                 // any length
theme.rebuild();                                                 // after direct edits

host.themes.register(theme);          // in the picker, and in `T`
host.select_theme("solarized-ish");   // and on screen
```

Register *and* select, because they are different claims: one adds a theme to the
menu and the other says what to draw now. An extension that only registers has
added an option, which is usually what it meant.

Lane and author colours cycle, so a theme may ship three or twelve. An empty list
falls back to chrome colours rather than panicking.

Type follows the same pattern in `core::font` and sits beside this on `Host`:
`Font { family, size, monospaced, advance }`. It is not part of the theme because
a palette and a face are swapped independently — and because two of those fields
are load-bearing rather than decorative. See
[extending.md](extending.md#4-a-font).

## Every colour the app draws

If a colour is not here, it is a bug — there were 35 hex literals across four
shell files before this existed.

| group | fields |
|---|---|
| `diff` | `file_bg` `file_fg` `adds_fg` `dels_fg` `hunk_bg` `hunk_fg` `gutter_fg` `rule` `context_bg` `context_fg` `added_bg` `added_fg` `added_word_bg` `removed_bg` `removed_fg` `removed_word_bg` `moved_removed_bg` `moved_added_bg` `absent_bg` |
| `markdown` | `code_bar` `quote_bar` `marker` `rule` (also the table grid) |
| `chrome` | `bg` `fg` `dim` `faint` `accent` `title_bg` `status_bg` `border` `selection_bg` (the row the keyboard is on) `selected_bg` (the text the mouse is holding) `error` |
| graph | `lanes` `lane_overflow` |
| commits | `authors` |
| syntax | 12 `Style`s, resolved across 8 surfaces |
| furniture | `gutter_fg`, resolved across the same 8 |

One colour never means two things, which is why the list is long: `file_bg` and
`title_bg` were the same value, and a theme cannot retune a file header without
moving its own title bar.

## Changing it without a rebuild

```
plait config > plait.toml     # a complete, correct starting file
```

`plait.toml` (or `$PLAIT_CONFIG`) is re-read every time it is saved, and colours,
the font family and the font size land **on the next frame** — no rebuild, no
relaunch, no lost scroll position. That is the payoff for colour having been data
in a dependency-free crate all along.

```toml
[font]
family = "JetBrainsMono Nerd Font Mono"
size = 14.0

[theme]
name = "light"                  # dark, light, slate — or a name of your own

[theme.diff]
added_bg = "#dde5d7"

[theme.syntax]
comment = "#9b9186 italic"      # colour, then any of bold and italic
```

`name` is read **before** everything under it, whatever order the file is in,
because it selects the palette the rest is applied to — TOML hands a table back
alphabetically, and a `name` applied in its turn would land after `[theme.diff]`
and silently throw it away. The result is registered under that name when the
file is done, which is what puts a theme written here in the picker beside the
shipped ones.

So there is only one case worth a warning, and it is the typo: a `[theme]` table
holding an unknown `name` and nothing else changed nothing at all. One that also
sets colours is a definition, and is registered rather than complained about.

Two fields cannot reload live and say so when you change them:
`font.monospaced`, because Markdown table padding rewrites the row *text* during
`prepare`, and `font.advance`, because the widest-row guess is made once at load.
Both still apply on the next launch.

The file is read forgivingly, because it is re-read on every save of something you
are in the middle of typing: an unparseable file leaves the theme exactly as it
was, and a single bad line is named and skipped while the rest applies. A warning
only appears when a value actually *changed* — one that fires on an unchanged
value teaches you to ignore the ones that matter.

The file holds **data only** — see
[decisions/0012](decisions/0012-config-is-data-behaviour-is-not.md). No
expressions and no computed colours, because a settings panel has to be able to
rewrite the file in place and cannot round-trip a function. When a colour is only
ever "that one, but lighter", derive it in `Theme` instead of asking the file for
it; `rebuild` already does exactly that for every syntax colour on every surface.

Implementation is `app/src/config.rs`. It is there and not in `core` because
reading a file is I/O and `core` does none, and not in a client because every
client reads the same file. `apply` is a pure function of a string, which is
why all of it is tested without a disk or a watcher — including a round-trip
asserting that what `plait config` writes reads back identically, so the two
directions cannot drift.

## Changing it without touching the file

The title bar has a **theme** picker — a pure function of the registry, like the
other four — and `T` cycles the same list. Both go through the same reload a save
does: the host is rebuilt from the defaults and the file, and then the pick is
applied on top. One path, because two would be two orders in which a theme and a
colour can disagree, and it is what makes a pick survive the next save the way
the view's own layout and wrap indices do.

The file still says what the window *opens* on. That is the same division as
`[diff] layout` and `[diff] wrap`, for the same reason.

## Seeing it without a window

```
cargo run -q -p plait-core --example paint    --release 40    # THEME=light
cargo run -q -p plait-core --example contrast --release       # every ratio
./dev dump diff --fixtures                                    # THEME=slate
```

`paint` prints a real diff in 24-bit ANSI from this exact theme and a legend of
all twelve classes; `contrast` prints the numbers behind it. The tests do the
rest, and they run over **every registered theme**: each class on each surface
clears the floor, each sign clears it on both rows it can land on, and each file
header out-reads its hunk headers. A new palette cannot ship illegible.
