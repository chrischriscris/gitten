# The terminal frontend

`plait-tui`. The third door, after the GPUI window and the browser.

It exists to be the cheap check on the boundary. `docs/architecture.md` names the
test — *a second frontend needs no logic of its own* — and a terminal is the
hardest place to cheat: it has no layout engine to hide work in, no container
that scrolls itself, and no element tree. If something a view needs is not in
`core`, it is immediately obvious, because there is nothing else there.

Writing it moved three things out of the frontends. That is the interesting part
of this page.

## What is shared, and what each door still owns

```
                        core                          per frontend
  ─────────────────────────────────────────────  ────────────────────────
  prepare        clip, intraline, syntax
  rows::Flat     File/Hunk/Line rows, wrap index
  rows::Present  claims, len, build, rows, width  + render → a UI type
  rows::expand   visual ↔ logical order table
  rows::assemble the whole load path
  runs::runs     tokens × spans → styled runs     → HighlightStyle / SGR / JSON
  graph::Hues    which branch is which colour     → curves / box drawing
  graph::MAX_LANES, lane_count
  theme, font, wrap, differ, syntax
```

Everything on the left had at least two implementations before this crate
existed and now has one. `web/src/rows.rs` and `shell/src/views/diff.rs` have not
been migrated onto `core::rows` yet — see [Still to do](#still-to-do) — so the
row flattening currently has one canonical implementation and two copies that
predate it. `runs` and `graph` are canonical everywhere except `plait-web`.

## The one thing a `Rows` implementation owns

`render`, and its return type is the reason the trait cannot live in `core`:

| frontend | `render` produces |
|---|---|
| `plait-shell` | `AnyElement` |
| `plait-web` | text pieces on the wire |
| `plait-tui` | cells, through a `Pen` |

Everything above it is `core::rows::Present`, which the frontend trait extends.
So a presentation that exists in one door is a `render` away from existing in
another, and `SplitRows` is the proof: `tui/src/split.rs` is
`shell/src/views/split.rs` with the GPUI taken out and *no* pipeline code, no
second alignment rule, no second wrap table.

## Two dependencies, and where the line is

`crossterm`, for what `gpui_platform` does for the shell: raw mode, the alternate
screen, and **parsing a keypress out of a byte stream**. That last one is a
decade of terminal archaeology — a terminal reports `Shift-F5` differently
depending on the emulator, the terminfo entry and whether the kitty protocol is
on — and it is exactly the "don't build what the framework already has" case. It
is confined to `term.rs`; nothing else in the crate imports it, which is what
makes the views testable.

`unicode-width`, for how many columns a character occupies. `Font::advance` is
the window's answer to the same question and has none here. Already in the lock
file as a gpui dependency, so it costs no download and no compile.

Drawing is *not* on that list. Cell diffing and 24-bit colour are forty lines,
and owning them is what gives the views a headless render target — see below.

## The screen is ours, and that is the point

`screen::Screen` is a `Vec<Cell>`. Presentations draw into it; `flush` compares
it against what is on the terminal and writes escape codes only for the runs that
differ.

**This is the only frontend whose drawing is tested.**
`docs/architecture.md`'s *Not built yet* lists "a rendering test" as the shell's
one untested stage: a panic in `render`, a colliding element id or a floating
element painted under its sibling are all found by launching. Here,
"the second row is a removal, red on dark red, with the changed word lit" is an
assertion:

```rust
let x = (0..40).find(|x| screen.char_at(*x, 3) == Some('1')).unwrap();
assert_eq!(screen.ink(x, y).unwrap().bg, theme.diff.removed_word_bg);
assert_eq!(screen.ink(x - 1, y).unwrap().bg, theme.diff.removed_bg);
```

`Screen::print` is the other half: the whole grid as lines, with no cursor
positioning, so `examples/dump.rs` prints a real frame of the real views to
stdout. No window appears — which `AGENTS.md` asks for explicitly, and which is
also how a colour gets checked in a code review.

## What is genuinely different in a terminal

Four things, and only these. Everything else is the same code.

**A column is a cell, not a fraction of an em.** So `Rows::reflow` takes columns
where the shell's takes pixels, and `screen::cols` replaces `Font::advance`.

**A view knows its own size before it draws.** GPUI hands a view its box during
paint, which is why the shell's wrapping lands one frame late
([decisions/0017](decisions/0017-wrapping-is-more-rows-not-taller-ones.md)); a
terminal is queried, so a resize is a method call.

**Nothing scrolls itself.** No `uniform_list`, no scroll container. Vertical
scrolling is a `for` over `top..top + height`, which virtualizes for free.
Horizontal scrolling is `Pen::scroll(n)`, which swallows the first *n* columns of
everything written after it — so the line numbers and the `+`/`-` stay put while
the text moves under them. Slicing the text instead is the obvious alternative
and it is wrong: the tokens and the intraline spans address the *line*, so a
slice taken before `runs` pairs styling with the wrong bytes.

This is also why side-by-side differs. In the window, wrapping off makes each
column as wide as the widest line in the diff and the scrollbar reaches it. A
terminal has no container to be wider than itself: a column wider than half the
screen puts the right-hand gutter off the edge. So the columns stay half the
screen and their contents scroll.

**A wide character claims two cells.** Getting that wrong does not misalign one
row, it shears every row below it, because the cursor ends up somewhere the grid
does not agree with. A lead cell plus a continuation cell, and `flush` extends a
changed run backwards to the lead cell before positioning the cursor.

## The graph, in box drawing

Topology is `assign_lanes` and colour is `graph::Hues`, both untouched. What this
decides is the alphabet: two columns per lane — `git log --graph`'s spacing, and
the only way a `─` between two lanes is expressible at all.

`git log` is newest-first, so a row below is *older*: a lane converging on a
commit came from **above** and its corner points up (`╯`), and a lane forked out
of it continues **below** and points down (`╮`). Backwards, this draws a history
where branches merge into their own children.

The honest cost of one row per commit: the window paints a branch changing lanes
as an S spanning a whole row, in halves that meet on the boundary. A cell grid
has no halves, so a merge and a fork are drawn on the commit's own row as a
horizontal run with a corner at the far end — which is what `git --graph` and
lazygit both do. A merge and a fork in the same lane on the same row collapse
into one glyph.

`Glyphs` is a struct and not a set of literals, so `Glyphs::ascii()` is the
whole of what a terminal without box drawing needs, and a Nerd Font set is one
more constructor.

**The gutter is one width for the whole list**, unlike the window's per-row one.
The window can scroll a container wider than itself; a terminal cannot, and a
subject starting in a different column on every row is a list the eye cannot
scan.

## Cost

Loading is `core`'s and is the same number in every frontend. Drawing is
per-visible-row and independent of the diff, which is the whole claim:

| fixture | rows | load | frame |
|---|---|---|---|
| `pr30683.diff` | 740,383 | 421 ms | 12 µs |
| `pr30683.diff`, side-by-side | 973,394 | 476 ms | 13 µs |
| `md.diff` | 74,467 | 118 ms | 10 µs |
| `md.diff`, side-by-side | 90,963 | 103 ms | 12 µs |
| `git/git`, 82k commits | — | 138 ms | 26 µs |

```sh
COLS=120 ROWS=40 cargo run -q -p plait-tui --example dump --release -- diff --fixtures
```
prints the frame and the timings; `FRAMES=n` sets how many repaints to average.
Release only — a debug build measures a different program, exactly as the
window's overlay says.

Nothing in a frame allocates. The run-list buffer is the caller's and is reused
across frames, a row's text is sliced out of the line rather than copied, and
`core::runs` walks the tokens and the spans together instead of collecting their
combined edges — that last one was worth 2 µs of the 14 the first version cost.

## Known wrong, and deliberately

**Wrap budgets are counted in characters, not columns.** `core::wrap` has no
dependencies and cannot ask how wide a glyph is, so a line of CJK wraps a little
wide and the pen clips it — a visible truncation rather than a broken grid.
Fixing it properly means a way for a frontend to tell `core` how it measures a
column, which is a change to the `Wrap` seam and not a new implementation behind
it. The window has the same approximation through `Font::advance`, where it is
also wrong and much less visible.

**A combining mark at the start of a row is dropped**, having nothing to attach
to. A terminal does the same.

## Still to do

- **Assembly.** The views are components: `Diff` and `Commits` hold state and
  expose commands (`down`, `page`, `jump_file`, `cycle_layout`), and neither
  knows what a keypress is. There is no `main`, no event loop and no keymap —
  deliberately, because command dispatch and the mode stack belong on `Host` and
  are not built, and a keymap written in `tui/` is one `cli/` would have to
  duplicate.
- **Migrating `shell` and `web` onto `core::rows`.** Both still hold their own
  row flattening and order table. `shell`'s is the harder one: `TextRows` stores
  `SharedString` so GPUI is handed a refcount bump rather than a copy per frame,
  so it wants `Flat` plus a parallel table rather than `Flat` alone.
- **`web` onto `core::runs`.** `web/src/rows.rs::pieces` is `runs` with the gap
  handling that `runs` now has. This is a deletion.
- **`MarkdownRows`.** `core/examples/paint.rs` already draws the furniture in
  ANSI, so the terminal version is that function and a `Rows` impl — and the
  furniture itself is then a fourth thing to lift into `core`.
- **A config file.** `plait.toml` is read by `shell/src/config.rs`, which is
  where the I/O lives; the terminal reads none of it yet, including
  `chrome.selection_bg` and the `SCROLLOFF` that should be in it.
- **`selection_bg` in the shell.** It was added to `ChromePalette` for this
  frontend because a hardcoded selection colour is not a seam. No GPUI view
  draws a selection yet.
