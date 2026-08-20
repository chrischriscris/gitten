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
}
```

Built once in `main.rs` before any view exists, handed to each view as
`Rc<Host>`. **If a feature needs a knob that is not on `Host` or one of its
fields, that feature is not extensible yet** — that is the check, and it is cheap
to run against a diff.

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

## 4. How a file's diff is presented

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
    Box::new(TextRows::default()),   // [0] is the fallback; must claim everything
    Box::new(ImageRows::new()),      // claims *.png, wins over the fallback
]);
```

What arrives in `build` is already clipped, intraline-diffed and highlighted — see
[diff-pipeline.md](diff-pipeline.md). An implementation draws; it does not redo any
of that. It keeps its own row storage and answers `render`/`width` by index, which
is how the list holds 8 bytes per row instead of a box.

**The constraint to design around:** row height is fixed for the whole list,
because `uniform_list` is the only reason a 714k-row diff scrolls at all. You may
draw anything within `ROW_H`, but you cannot ask for more. A presentation that
genuinely needs variable height — a rendered Markdown preview, a side-by-side
image diff — wants a pane of its own, and that plug point does not exist yet.

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
