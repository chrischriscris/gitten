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

Not there yet: command dispatch, the mode stack, and any way to load an
implementation from outside the binary. Today "an extension" means code compiled
in. The seams are shaped so that stops being true without them changing.

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

## 3. A theme

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

## 4. A font

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

## 5. How a file's diff is presented

```rust
pub trait Rows {
    fn claims(&self, path: &str) -> bool;
    fn len(&self) -> usize;
    fn build(&mut self, file: prepared::File);
    fn render(&self, index: usize, host: &Host) -> AnyElement;
    fn width(&self, index: usize) -> usize;
    fn report(&self) -> String { String::new() }
}
```

```rust
Diff::with_renderers(files, host, vec![
    Box::new(TextRows::default()),       // [0] is the fallback; must claim everything
    Box::new(MarkdownRows::default()),   // the second built-in; claims *.md
    Box::new(ImageRows::new()),          // claims *.png, wins over both
]);
```

`Diff::new` is that call with the two built-ins in it, so the shipped
configuration goes through the seam rather than around it. `MarkdownRows` is the
worked example: it draws `.md` as the document instead of the source, and it does
it with no new trait, no new argument and no edit to `TextRows` — which is the
only test of a seam that counts. Read it before writing a second one; the
interesting parts are `Metrics`, and how little of it is markdown.

**This is the one seam whose registry is not on `Host`,** and the reason is
structural rather than an oversight: a `Rows` implementation returns an
`AnyElement`, `Host` lives in `core`, and `core` never knows a UI exists. So the
list is an argument to `Diff::with_renderers` instead. If panes arrive and more
than one view wants a row registry, a shell-side registry is where it goes — not
`Host`.

What arrives in `build` is already clipped, intraline-diffed and highlighted — see
[diff-pipeline.md](diff-pipeline.md). An implementation draws; it does not redo any
of that. It keeps its own row storage and answers `render`/`width` by index, which
is how the list holds 8 bytes per row instead of a box.

**The constraint to design around:** row height is fixed for the whole list,
because `uniform_list` is the only reason a 714k-row diff scrolls at all. You may
draw anything within `ROW_H`, but you cannot ask for more. A presentation that
genuinely needs variable height — a reflowed Markdown *preview*, a side-by-side
image diff — wants a pane of its own, and that plug point does not exist yet.

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
- **Row count is not yours to change.** The gutter shows both line numbers and
  they have to keep adding up, so a blank line still costs a whole row. A test
  asserts `MarkdownRows` and `TextRows` produce the same count for the same file.
- **Anything spanning rows is measured, not laid out.** A table's columns have to
  line up with the rows above and below, which no per-row API can express. Padding
  the text in a monospaced face gets it for free and keeps one `StyledText` per
  row; an element per cell would have cost the render path. If you need this,
  measure across the run first and rewrite each row exactly once — see the
  warning on `syntax::for_each_side` about why "exactly once" is load-bearing.

## What a new seam owes

If you are adding one, match what the existing four do:

1. **A trait or a data structure, not a match arm.** The test is whether a second
   implementation can exist without editing the first.
2. **A registration point on `Host`**, or on something reachable from it.
3. **A default that claims everything**, so behaviour with nothing registered is
   the shipped behaviour.
4. **A test that swaps it.** `Highlighters` has one per routing rule; `Rows` has
   two; `Theme` has the field-by-field rewrite. Counts go stale, so grep for the
   swap rather than trusting this line.
5. **A line in `AGENTS.md`** if it changes the philosophy, and a page here if it
   does not.
