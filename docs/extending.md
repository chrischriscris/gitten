# Extending

Rule 1 says anything a built-in does, an extension must be able to do too. This
page is the list of what that currently means, with a worked example each.

Every seam is exercised by a test that swaps the implementation. One
implementation and no test is a promise, not a seam — so if you add a seam, add
the test that proves a second implementation fits.

## Where the pieces live

```rust
pub struct Host {
    pub syntax: Highlighters,   // which highlighter each path gets
    pub differ: Differs,        // which algorithm turns two files into a diff
    pub layout: String,         // which diff presentation opens, by name
    pub wrap: Wraps,            // where a line too wide for the window breaks
    pub keys: Keymap,           // which command each key runs, per mode
    pub commands: Commands,     // every command name that exists, and one line each
    pub theme: Theme,           // every colour the app draws
    pub font: Font,             // the face, and the numbers derived from it
}
```

Built once in `main.rs` before any view exists and held in a GPUI global.
**If a feature needs a knob that is not on `Host` or one of its fields, that
feature is not extensible yet** — that is the check, and it is cheap to run
against a diff.

Views read it through `config::host(cx)` on the render path rather than capturing
an `Rc` when they are built, and that is not incidental: a captured clone is a
snapshot, and it is what makes `plait.toml` apply on the next frame instead of the
next launch. A new view that captures the host instead will work, and will quietly
not hot-reload.

Built once in a client's `main` by `plait_app::Startup`, which reads
`plait.toml` into it before any view exists.

Not there yet: any way to load an implementation from outside the binary. Today
"an extension" means code compiled in. The seams are shaped so that stops being
true without them changing.

`layout` is a name and `wrap` is a whole registry, and the difference is the
boundary rather than taste: a `Rows` implementation returns UI elements, so its
registry cannot be in `core`; a break point is a property of text, so its registry
can be.

There is now one `Host` and three frontends reading it, and it is the same struct
in all three — which is what makes "not on `Host`" a real failure rather than a
style note. `chrome.selection_bg` exists because the terminal needed a colour for
the row the keyboard is on and a literal in a view is not a seam; no GPUI view
draws *that* yet. `chrome.selected_bg` is the other one — the text a drag
selected — and it is a whole `Surface`, so a theme retunes the syntax colours that
land on it rather than accepting whatever they were.

`s` and `w` are the first real key bindings and are deliberately shaped like the
last one will be — the view owns a focus handle, the binding is global, the handler is a
method — so that when dispatch arrives they have something to attach to rather
than something to replace. Until then the title-bar pickers are how a registry is
reachable without editing a file; they read the same names `plait.toml` does, and
should collapse into a settings panel when there is one.

## 1. A language

Data, not code. Nothing about the scanner changes.

```rust
host.syntax.languages().unwrap().register(&["nim", "nims"], Syntax::new()
    .line(&["#"])
    .block(&[("#[", "]#")]).nested_block()
    .strings(&[("\"\"\"", "\"\"\"", false, true), ("\"", "\"", true, false)])
    .keywords(&["proc", "var", "let", "type", "import", "if", "else", "return"])
    .capitalized_types()
    .call_heuristic());
```

Keys are extensions or whole filenames (`Cargo.lock`). A later registration
replaces an earlier one, so a built-in table can be corrected rather than only
added to. `languages()` returns `None` once the fallback has been swapped for
something that is not the scanner — there are no tables to register with then.

Read [syntax-highlighting.md](syntax-highlighting.md) first — particularly the
part about which languages this model cannot do. A wrong table is worse than none.

## 2. A highlighter

When the scanner's model does not fit — markup, anything needing scope or
injections — implement the trait and route the paths to it:

```rust
struct TreeSitter { /* grammars, queries, a blob cache */ }

impl Highlighter for TreeSitter {
    fn highlight(&self, path: &str, lines: &[&str]) -> Vec<Vec<Token>> {
        // one Vec<Token> per line, ranges relative to that line,
        // sorted, non-overlapping, on char boundaries
    }
}

host.syntax.route(&["html", "php", "vue"], TreeSitter::new());
```

Last route wins. `set_fallback` replaces the scanner for everything unrouted.
The crate holding your implementation carries its own dependencies; none of them
reach `core`.

`Markdown` in `core/syntax.rs` is exactly this, and is the built-in proof the
route is real.

The contract, in full: ranges index their own line, are sorted, never overlap, and
land on char boundaries. Break the last one and a debug build panics in GPUI's
text layout.

## 3. A diff algorithm

Which lines correspond is a judgement, so it is a trait. An implementation returns
only the edit script — line numbers, context and hunk headers are
`differ::hunks`, shared by all of them, because that bookkeeping is identical and
a second copy of it is a hunk header that quietly disagrees with its lines.

```rust
struct TreeSitterDiff { /* parsers, a blob cache */ }

impl Differ for TreeSitterDiff {
    fn name(&self) -> &'static str { "tree-sitter" }

    fn diff(&self, path: &str, old: &[&str], new: &[&str]) -> Vec<Edit> {
        // sorted by old_start, non-overlapping, none empty, none adjacent
    }
}

host.differ.register(TreeSitterDiff::new());
host.differ.select("tree-sitter");            // for everything…
host.differ.route(&["json", "lock"], "myers"); // …or all but these
host.differ.context = 6;
```

Selection is by **name**, not by value, and that is the whole point: `[diff]
algorithm = "tree-sitter"` in `plait.toml` reaches a registered implementation the
day it exists, and an unknown name reports the ones that do — from the registry,
so the message cannot go stale. `route` matches extensions or whole filenames the
way `Highlighters::route` does, and a later route wins.

`path` is in the signature for exactly this: a language-aware differ needs to know
what it is looking at. If you need the *blob* rather than the split lines,
acquisition is what would have to change; see
[decisions/0013](decisions/0013-differs-in-core-not-a-dependency.md).

The contract, in full: edits are sorted by `old_start`, never overlap, are never
empty, and no two are adjacent — two touching edits describe one change and must
be one. `verify` in `differ.rs`'s tests checks every clause of that and every
built-in is run through it. If your implementation is meant to be *minimal*, check
it against `git diff --minimal` with `git/examples/diffcheck.rs`; a minimal script
has exactly one length, so that is a real test and not a comparison.

**Four things you do not implement**, because they are shared and would compose
wrongly if they were not:

| | |
|---|---|
| `Whitespace` | how much whitespace must match. Normalised before your `diff` is called, per line and length-preserving, so you never see it |
| `differ::compact` | git's indent heuristic, sliding each change to a readable boundary |
| `differ::hunks` | context, line numbers, `@@` headers, the function-name suffix |
| `differ::moves` | blocks deleted here and added there, flagged on the line |

All four are knobs on `Differs` (`whitespace`, `indent_heuristic`, `context`,
`min_moved`) and all four apply to your implementation the day it is registered.
That is the shape to preserve: a `Differ` decides which lines correspond, and
nothing else.

## 4. A theme

```rust
let mut theme = Theme::default_dark();
theme.set_syntax(Kind::Comment, Style::fg(0x93a1a1).italic());
theme.diff.added_bg = 0x073642;
theme.min_contrast = 4.5;
theme.rebuild();          // required after touching fields directly
host.theme = theme;
```

Plain `0xRRGGBB` throughout, so the ANSI painter and the GPUI window read the same
one. Details and the contrast machinery: [theming.md](theming.md).

## 5. A font

```rust
host.font = Font {
    family: "Iosevka Term".into(),
    size: 15.0,
    monospaced: true,
    advance: 0.5,          // as a fraction of `size`
};
```

Type as data, the same way colour is: a family name the platform can match, and
three numbers. On macOS the family is the *typographic* one — what Font Book
groups under, so `JetBrainsMono Nerd Font Mono` rather than the `NFM` in its name
table.

Two fields are not decoration, and this is the reason the font is on `Host` rather
than a `const` in `main.rs`, where it was:

- **`monospaced`** decides whether Markdown tables get padded into a grid.
  Setting it true for a proportional face misaligns every table by a fraction of a
  glyph per cell; setting it false is a supported answer and leaves tables as
  their source.
- **`advance`** is one character's width as a fraction of `size`, used to guess
  which commit row is widest — `uniform_list` measures exactly one row to decide
  its scrollable width. A fraction rather than a pixel count so changing `size`
  cannot leave a stale width behind it, which is what happened before: an `8.4`
  with a comment naming the font it had been measured on.

Everything else derived from the face is derived, not restated:
`markdown::Metrics::for_font` builds the heading scale relative to `size` and caps
it at `ROW_H / 1.2`, so a larger body size gives up the top of the scale instead
of drawing outside its row.

## 6. How a file's diff is presented

```rust
pub trait Rows {
    fn claims(&self, path: &str) -> bool;
    fn len(&self) -> usize;
    fn build(&mut self, file: prepared::File);
    fn render(&self, index: usize, seg: usize, host: &Host, sel: Option<Selected>) -> AnyElement;
    fn width(&self, index: usize, seg: usize) -> usize;

    // Wrapping. Both default, so an implementation that ignores them is exactly
    // as long as it was — see 8.
    fn rows(&self, index: usize) -> usize { 1 }
    fn reflow(&mut self, width: f32, host: &Host, wrap: &dyn Wrap) -> bool { false }

    // Selection. Both default too: `hit` to `None`, which means "not
    // selectable" — see 11.
    fn hit(&self, index: usize, seg: usize, x: f32, host: &Host) -> Option<Hit> { None }
    fn selectable(&self, index: usize, part: u16) -> Option<&str> { None }

    fn report(&self) -> String { String::new() }
}
```

**`len` counts lines; `seg` says which row of one.** A wrapped line is *n* rows of
`ROW_H` and still one entry in `len`, so the two indices are not the same thing.
`seg` is 0 for everything that fits, which is nearly everything.

```rust
Diff::with_renderers(files, host, vec![
    Box::new(TextRows::default()),       // [0] is the fallback; must claim everything
    Box::new(MarkdownRows::default()),   // the second built-in; claims *.md
    Box::new(ImageRows::new()),          // claims *.png, wins over both
]);
```

That pins one presentation with nothing to cycle to. The general form is a
[layout](#7-a-whole-diff-presentation), and `Layouts::builtin` builds the two
shipped ones through it, so the shipped configuration goes through the seam rather
than around it.

`MarkdownRows` is the worked example: it draws `.md` as the document instead of the
source, and it does it with no new trait, no new argument and no edit to
`TextRows` — which is the only test of a seam that counts. Read it before writing
a second one; the interesting parts are `Metrics`, and how little of it is
markdown. `SplitRows` is the second, and it is the one to read for a presentation
of the whole diff rather than of one kind of file.

**This is the one seam whose registry is not on `Host`,** and the reason is
structural rather than an oversight: a `Rows` implementation returns an
`AnyElement`, `Host` lives in `core`, and `core` never knows a UI exists. So the
registry is shell-side and `Host` carries only the *name* of the entry to open,
which is data. If panes arrive and more than one view wants one, that is where a
second shell-side registry goes — not `Host`.

What arrives in `build` is already clipped, intraline-diffed and highlighted — see
[diff-pipeline.md](diff-pipeline.md). An implementation draws; it does not redo any
of that. It keeps its own row storage and answers `render`/`width` by index, which
is how the list holds 8 bytes per row instead of a box.

**The constraint to design around:** row height is fixed for the whole list,
because `uniform_list` is the only reason a 714k-row diff scrolls at all. You may
draw anything within `ROW_H`, but you cannot ask for more. A presentation that
genuinely needs variable height — a reflowed Markdown *preview*, a side-by-side
image diff — wants a pane of its own, and that plug point does not exist yet.

What you *can* have is more rows. That is what wrapping is: a line too wide for
the window is drawn on several rows of `ROW_H` rather than on one tall one, which
is why it needed no change to this constraint. See [8](#8-where-a-line-breaks).

More fits inside `ROW_H` than it looks like. What `MarkdownRows` found:

- **Font size is a row property, not a run property.** GPUI's `HighlightStyle`
  carries colour, weight, slant, background, underline, strikethrough and fade,
  and is documented as *"a single font, uniformly sized and spaced text."* There
  is no size on a run. So a heading scales by `.text_size()` on the row and never
  within one — and the ceiling is `ROW_H / 1.2`, past which it clips into its
  neighbour.
- **Leave the row background alone.** In a diff it means added or removed, and
  that is the one thing a presentation may not spend. Group rows with a bar down
  their left edge instead.
- **Punctuation you draw yourself is furniture, not text.** A bullet glyph as part
  of the string becomes a run in the merge and takes the text's colour; as its own
  `div` it carries its own. It also keeps every text rewrite a pure deletion,
  which is what lets the range remapping stay one-directional.
- **Row count is not yours to change — within a column.** The gutter shows both
  line numbers and they have to keep adding up, so a blank line still costs a
  whole row. A test asserts `MarkdownRows` and `TextRows` produce the same count
  for the same file. A *second column* is a different claim, and `SplitRows` is
  the presentation that makes it: a removal and the addition that replaced it
  share a row, so it has strictly fewer rows than unified for the same diff.
- **Anything spanning rows is measured, not laid out.** A table's columns have to
  line up with the rows above and below, which no per-row API can express. Padding
  the text in a monospaced face gets it for free and keeps one `StyledText` per
  row; an element per cell would have cost the render path. If you need this,
  measure across the run first and rewrite each row exactly once — see the
  warning on `syntax::for_each_side` about why "exactly once" is load-bearing.

## 7. A whole-diff presentation

A [`Rows`](#6-how-a-files-diff-is-presented) implementation presents one kind of
file. A `Layout` is a named set of them: one way of presenting the whole diff, and
what `s` cycles.

```rust
let mut layouts = Layouts::builtin();          // unified, split
layouts.register("inline-words", |host| {
    vec![Box::new(InlineWordRows::new(&host.font))]
});

// and then, in plait.toml:  [diff] layout = "inline-words"
```

`build` is a closure and not a `Vec`, because a layout has to be *rebuildable*:
switching re-runs the pipeline from stage 3 and hands each implementation its
files again, and a `Vec` that has already been consumed cannot be handed anything.
It takes the `Host` because a presentation is entitled to depend on the font —
`MarkdownRows` derives its whole heading scale from it.

`renderers[0]` is the fallback for that layout and must claim every path. `split`
has exactly one entry for that reason: `SplitRows` claims everything, because it
is a presentation of the diff rather than of `.md`.

Registering a name twice replaces it, so `unified` can be corrected rather than
only added to. An unknown `[diff] layout` opens the first entry and says so —
falling back rather than failing, because the file is live-reloaded and a typo must
not leave you with no diff.

**A registered layout appears in the title-bar picker with no further work**, and
so does a registered algorithm. That is the property to preserve: the pickers in
`controls.rs` are a pure function of a list of names and an index, so a seam with
a registry gets a control for free. If a new seam needs a control written for it
by hand, the seam is the wrong shape — see
[decisions/0015](decisions/0015-title-bar-controls-are-hand-rolled.md).

Two things to know before writing one:

- **The alignment is `core`'s, not yours.** `align::align` decides which removal
  sits opposite which addition, and it is the same function `replace_pairs` is
  built on. Pair differently and you will draw a removal beside an addition whose
  changed words were computed against another line — highlighted fragments
  corresponding to nothing on screen.
- **Switching costs a `prepare`.** 8 ms typically, 247 ms on the pathological
  fixture. That is the price of a keystroke and it is paid to keep the memory
  cost off the load path; see
  [decisions/0014](decisions/0014-layouts-are-a-registry.md).

## 8. Where a line breaks

```rust
pub trait Wrap {
    fn name(&self) -> &'static str;
    fn breaks_lines(&self) -> bool { true }
    fn breaks(&self, text: &str, cols: usize, out: &mut Vec<Break>);
}
```

```rust
host.wrap.register(Sentence);       // and then: [diff] wrap = "sentence"
host.wrap.select("sentence");       // or pick it in the title bar, or press `w`
```

Three built-ins: `word` (selected), `char`, and `off` — which is an *entry in the
registry* and not a flag beside it, because the pickers are a pure function of a
registry and that is what puts it in the menu for free.

An implementation is a pure function of one line and a column budget. No theme, no
font, no view: a break point is a property of the text, the frontend has already
turned pixels into columns by the time this is called, and that is what lets the
same three serve the window, the ANSI `paint` example and a test.

**Everything except the break points is shared.** `wrap::Wrapped` turns them into
the range partition, validates them, holds them flat and answers by index — so an
implementation cannot produce a range that points past its line, and its bugs are
counted and reported on the overlay rather than crashing the app.

Two obvious ones that do not exist yet and should: a code-aware wrap that breaks
after `,` and `(` the way a formatter does, and one that keeps a continuation
aligned under the opening bracket. Both are `breaks` and nothing else.

### What a presentation owes it

The two `Rows` methods in [6](#6-how-a-files-diff-is-presented), and they are six
lines. `TextRows` is the whole of it:

```rust
fn rows(&self, index: usize) -> usize {
    self.wrapped.rows(index)
}

fn reflow(&mut self, width: f32, host: &Host, wrap: &dyn Wrap) -> bool {
    let cols = columns(width, TEXT_CHROME, host.font.size, host);
    if cols == self.cols && wrap.name() == self.wrap {
        return false;      // a resize that crossed no character boundary
    }
    self.cols = cols;
    self.wrap = wrap.name();
    self.wrapped = Wrapped::build(self.rows.iter().map(|r| (wrappable(r), cols)), wrap);
    true
}
```

Three things about that are the whole design:

- **You own the pixels-to-columns conversion**, because you own what is drawn
  around the text. `TEXT_CHROME` is two gutters, a sign column and the padding;
  `SplitRows` halves what is left because it has two columns; `MarkdownRows`
  computes a budget *per row*, because a bullet, three levels of indent and an
  18px heading all cost characters. That is why `Wrapped::build` takes the budget
  per line rather than once.
- **Return whether the row count moved.** This runs on every frame of a resize
  drag, and `false` is what makes all but one of those frames a float comparison.
- **A budget of 0 means "never break this line."** What a Markdown table row
  asks for: its grid lines up character by character with the rows above and
  below, and a break shears it.

Then `render` and `width` are handed `seg` and ask `Wrapped` for the byte range.
`runs` takes that range and clips the tokens and spans into it, so nothing is
re-derived and nothing extra is allocated per frame.

**Ignoring all of this is a supported answer**, and there is a test asserting it:
an implementation written before wrapping existed keeps one row per line and
behaves identically. It is also the weakest point of the seam — see
[decisions/0017](decisions/0017-wrapping-is-more-rows-not-taller-ones.md) for why
wrapping cannot happen before `build` and hand it to everybody free.

## 9. The same presentation, in the terminal

`plait-tui` has the same two registries — a `Rows` trait claimed per path and a
`Layouts` that `s` cycles — and the split between them and `core` is where the
extension story got sharper.

The half of a presentation that has nothing to do with a UI is now a trait in
`core`:

```rust
pub trait Present {          // plait_core::rows
    fn claims(&self, path: &str) -> bool;
    fn len(&self) -> usize;
    fn build(&mut self, file: prepared::File);
    fn rows(&self, index: usize) -> usize;              // defaulted to 1
    fn width(&self, index: usize, seg: usize) -> usize; // defaulted to 0
    fn files(&self) -> &[Entry];                        // defaulted to none
}
```

A frontend's trait is that plus a `render`, and `render`'s return type is the only
reason the frontend's trait exists at all — an `AnyElement`, a row of cells, a
JSON payload. So the whole of the terminal's built-in unified presentation is:

```rust
#[derive(Default)]
pub struct TextRows { flat: Flat, digits: usize }

impl Present for TextRows {
    fn claims(&self, _: &str) -> bool { true }
    fn len(&self) -> usize { self.flat.len() }
    fn build(&mut self, f: File) { self.flat.push(f); /* …widest number */ }
    fn rows(&self, i: usize) -> usize { self.flat.visual_rows(i) }
    fn width(&self, i: usize, seg: usize) -> usize { screen::width(self.flat.piece(i, seg)) }
    fn files(&self) -> &[Entry] { self.flat.files() }
}

impl Rows for TextRows {
    fn reflow(&mut self, cols: usize, _: &Host, w: &dyn Wrap) -> bool {
        self.flat.reflow(self.budget(cols), w)      // the whole of what it owes wrapping
    }
    fn render(&self, i: usize, seg: usize, at: &Frame, pen: &mut Pen, out: &mut Vec<Run>) { … }
}
```

`Flat` is doing the work: the rows, the wrap index, the reflow early-out, the
`n moved · k invalid breaks` report. A presentation that holds one gets all of it,
which is what makes an extension's `render` the only thing it has to think about.

Two differences from the window's seam, and only two. `reflow` takes **columns**,
because a terminal has no pixels — the implementation still owns the conversion to
a text budget, because it owns the furniture it draws around the text. And
`render` is handed a `&mut Vec<Run>` scratch buffer it must not allocate,
because a frame is 50 rows and a scroll is a frame per keypress.

### The graph's alphabet

`commits::Glyphs` is a struct of `char`s, not literals in a `match`:

```rust
Commits::with_glyphs(commits, Glyphs::ascii())   // git --graph's own set
```

Nine characters — the vertical, the two dots, the crossing, the run and the four
corners. A terminal without box drawing, a Nerd Font set and a
one-column-per-lane experiment are all constructors, and none of them touch
`paint`.

## 10. A key, and a command

`core::command`, and the shape is the whole point: **a key is data and a command
is a name.** Nothing in the chain is a function pointer, which is what lets
`plait.toml` hold it and a settings panel rewrite it.

```rust
host.commands.register("blame.toggle", "show blame beside the diff");
host.keys.bind("diff", "b", "blame.toggle")?;
```

Two lines, and: `b` works in a diff, `?` lists it with its description, and
`plait config` writes it back out. A client that does not know what
`blame.toggle` is treats it as an unbound key, which is the honest answer.

The registry is validated *against itself* — `apply_keys` refuses a binding whose
command is not registered — so a typo is named and a key is never silently bound
to nothing. That is the same trick `[diff] algorithm` plays against `Differs`.

Two things it will not do, both because the alternative needs a clock `core` does
not have:

- **A prefix is refused.** Binding `g g` when `g` exists, or the other way round,
  is an error and the binding is not added — so the map is never in a state that
  cannot resolve without a timeout.
- **Shift on a character is dropped.** Every platform reports `Shift-a` as `A`; a
  binding on `shift-a` would never fire and one written both ways would fire
  twice. `Key::new` enforces it, so no client can get it wrong.

What a client writes is a translation from its own platform's event to
`command::Key` — `plait-tui`'s is `term.rs`, and it is the only file in that
crate that imports `crossterm`. See [clients.md](clients.md).

## 11. A selection your presentation takes part in

```rust
/// Where a click landed inside a row.
pub struct Hit { pub part: u16, pub off: usize }

fn hit(&self, index: usize, seg: usize, x: f32, host: &Host) -> Option<Hit>;
fn selectable(&self, index: usize, part: u16) -> Option<&str>;
```

Two methods, and everything else about a selection is `core::select`: which rows
lie between two carets, which bytes of each, what survives a reflow, where a word
ends for a double-click, and what the whole thing is as a string. See
[decisions/0018](decisions/0018-selection-is-a-model-not-a-text-element.md).

**`off` is a byte offset into the *logical* row's text**, not the visual row's, so
a caret on the third row of a wrapped line is the same kind of thing as one on a
line that fits. Rebase it: `wrapped.range(index, seg, text).start + column_at(..)`.
Getting this wrong selects from the start of the line every time somebody clicks a
wrapped one.

**`part` is which of the row's texts.** Almost always one, so almost always 0.
`SplitRows` has two — the old side and the new — and a selection is fixed to the
one its anchor landed in, so a drag never crosses the divider. Parts are laid out
left to right, which is the only thing the view assumes about them.

**`None` from `selectable` is a hole, not an empty line.** A copy skips the row
entirely, which is what makes dragging down one column of a two-column diff paste
that file with the gaps closed rather than a blank line per lone change.

**`hit` returning `None` means the whole presentation is not selectable**, and that
is the default — so an implementation written before any of this compiles and
behaves exactly as it did. Opting in is the two methods above and reading `sel` in
`render`; all three built-ins do it in about fifteen lines each, and
`MarkdownRows` is the one to read because its text starts at a different x on
every row.

Painting it is one more layer in the run merge, not an overlay:

```rust
StyledText::new(piece).with_highlights(runs(
    at, tokens, spans, theme, kind, moved,
    selected(sel, /* part */ 0, text.len()),   // <- the byte range, clamped
))
```

`column_at`, `header_hit` and `selected` are shared helpers in `views::diff`, for
the same reason `file_header` is: a header's text starts at the page padding
whoever owns the lines beneath it, and three presentations working that out
separately is three places for the caret to be a gutter's width off.

## What a new seam owes

If you are adding one, match what the existing four do:

1. **A trait or a data structure, not a match arm.** The test is whether a second
   implementation can exist without editing the first.
2. **A registration point on `Host`**, or on something reachable from it.
3. **A default that claims everything**, so behaviour with nothing registered is
   the shipped behaviour.
4. **A test that swaps it.** `Highlighters` has one per routing rule; `Rows` has
   two; `Differs` has `an_algorithm_can_be_added_selected_and_routed` and a config
   test that reaches a registered one by name; `Layouts` has
   `a_registered_presentation_is_cycled_to_like_a_built_in`; `Theme` has the
   field-by-field rewrite. Counts go stale, so grep for the swap rather than
   trusting this line.
5. **A line in `AGENTS.md`** if it changes the philosophy, and a page here if it
   does not.
6. **Reachable from a client's `main` without that client being special.**
   `plait_app::Startup` hands back the `Host`; if a seam needs something else,
   it is not a seam three clients can use.
