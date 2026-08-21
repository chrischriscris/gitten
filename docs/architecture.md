# Architecture

Five crates. The interesting line is between the first two and the rest.

```
                        ┌──────────────────────────────────────┐
                        │  plait-core          zero deps       │
   a repository         │                                      │
        │               │  parse_log      assign_lanes         │
        │               │  differ::{Histogram, Myers, hunks}   │
        ▼               │  prepared::prepare   align::align    │
┌───────────────┐ blobs │  intraline   markdown::lay_out       │
│  plait-git    ├──────►│  rows::{Flat, Present, expand}       │
│  two texts    │       │  runs::runs   graph::{Hues, cap}     │
│  per file     │       │  syntax::{Lexer, Markdown, ...}      │
│  git binary   │       │  wrap::{Word, Char, Wrapped}         │
│  (gix later)  │       │  theme::Theme  font::Font  host::Host │
└───────────────┘       └──┬────────┬────────┬────────┬───────┘
                           │        │        │        │
                     rows, │        │        │        │  rows, colours
                   colours │        │        │        │
              ┌────────────▼─┐ ┌────▼─────┐ ┌▼───────────┐ ┌▼──────────────┐
              │ plait-shell  │ │plait-tui │ │ plait-web  │ │examples/paint │
              │ GPUI window  │ │ cells,   │ │ loopback   │ │ ANSI,         │
              │              │ │ crossterm│ │ HTTP + JS  │ │ headless      │
              └──────────────┘ └──────────┘ └────────────┘ └───────────────┘
```

Three real frontends and one example, and the example is still the cheapest place
to look at a colour. Each door draws and nothing else: what a `Rows`
implementation returns is an `AnyElement`, a row of cells or a JSON payload, and
everything above that line is one implementation in `core`.

## plait-core

Pure. No GPUI, no gitoxide, no I/O, and `[dependencies]` is empty — deliberately,
because it compiles in a second and its tests need no window.

| module | what lives there |
|---|---|
| `lib.rs` | commit and diff parsing, `assign_lanes`, `intraline`, `replace_pairs`, `initials` |
| `graph.rs` | which branch is which colour, the lane cap, the honest lane count |
| `rows.rs` | a diff flattened to rows, the wrap index, the order table, the load path |
| `runs.rs` | syntax tokens × intraline spans → one flat styled run list |
| `differ.rs` | the `Differ` trait, Histogram/Patience/Myers, whitespace relations, the indent heuristic, move detection, hunk assembly, routing |
| `align.rs` | which removal sits opposite which addition, for a two-column view |
| `prepared.rs` | a diff assembled into drawable rows: clip → intraline → syntax |
| `markdown.rs` | a `.md` diff as blocks, with the markers cut and the ranges moved |
| `syntax.rs` | the scanner, the language tables, the `Highlighter` trait, routing, Markdown |
| `theme.rs` | every colour, as `0xRRGGBB` data, plus contrast resolution |
| `font.rs` | the face as data: family, size, and whether a char is a column |
| `host.rs` | the struct that holds the swappable pieces |

Four examples double as the headless test bench: `bench` (timings at fixture
scale), `shape` (topology statistics), `verify` (lane-assignment invariants),
`paint` (the diff view in ANSI).

Three of these modules exist because a third frontend was written. `rows`, `runs`
and `graph::Hues` each had two implementations in two frontends before they had
one here — see [terminal.md](terminal.md) for which copies are still outstanding.

## plait-git

The only crate that talks to a repository. Everything currently shells out to the
`git` binary; reads are meant to move to `gix` while writes stay on the binary
permanently, because shelling out is what gets hooks, credential helpers and
`.gitconfig` semantics exactly right.

Today: `log`, `pairs`, `diff`, `describe`. All behind one surface, so a frontend
never learns which path ran.

**It acquires content, not diffs.** `pairs` returns two lists of lines per changed
file; `diff` is that plus the host's `Differs`. It used to run `git diff` and parse
the unified output back, which meant git chose the algorithm and
`plait_core::differ` could not have existed — see
[decisions/0013](decisions/0013-differs-in-core-not-a-dependency.md). Two
processes for a whole diff whatever the file count: one `git diff --raw`, one `git
cat-file --batch`.

`examples/diffcheck.rs` is the headless check that the differs agree with git on
real history, and is run by `./check.sh`.

## plait-tui

The terminal. A cell grid, the presentations that fill it, and escape codes.

| file | what lives there |
|---|---|
| `screen.rs` | cells, ink, the pen, the two-buffer diff, and `print` |
| `rows.rs` | the `Rows` seam, `Layouts`, `TextRows`, the shared row furniture |
| `split.rs` | `SplitRows`, at half the width and with its own scroll |
| `diff.rs` | the diff view: viewport, reflow, commands |
| `commits.rs` | the commit list, and the graph in box drawing |
| `term.rs` | the only module that touches `crossterm` |

`examples/dump.rs` prints one frame of either view to stdout, which is how it is
looked at without a terminal — and, because `Screen` is a `Vec<Cell>`, it is the
one frontend whose *drawing* is unit-tested. See [terminal.md](terminal.md).

## plait-web

A loopback HTTP server and a page. No third-party dependencies: the server is a
`TcpListener`, the JSON writer is a `String`. Everything above drawing runs
natively in the process you started, so nothing needs a wasm target; the browser
re-implements the drawing.

## plait-shell

GPUI. Drawing and input, and as little else as possible.

| file | what lives there |
|---|---|
| `main.rs` | argument parsing, data loading, the window, the `Host` |
| `views/diff.rs` | the `Rows` seam, `TextRows`, run-list merging, the shared row furniture |
| `views/diff.rs` | …and `Layouts`, the registry of whole-diff presentations |
| `views/markdown.rs` | `MarkdownRows`: the rendered-Markdown presentation, and its metrics |
| `views/split.rs` | `SplitRows`: the two-column presentation |
| `views/commits.rs` | the commit list, author initials, row layout |
| `graph.rs` | lane geometry and painting: quads, paths, one canvas per row |
| `controls.rs` | the title-bar pickers: a label, a value, and the registered alternatives |
| `config.rs` | `plait.toml`: parse, apply, watch, and the live `Host` global |
| `session.rs` | the row you were on, so `./dev.sh` can put you back after a restart |
| `stats.rs` | the counting allocator and the `PLAIT_STATS` overlay |

## Which way data moves

```
  git --raw ──► git cat-file ──► Vec<Pair>        two texts per changed file
                                     │
                    Differs::file    │  Differ::diff, then differ::hunks
                                     ▼
                               Vec<FileDiff>  ◄──── or parse_unified_diff,
                                     │              for a .diff fixture
                   prepared::prepare │  clip, intraline, syntax
                                     ▼
                             Vec<prepared::File>
                                     │
             Rows::build (per file)  │  claimed by path, within a Layout
                                     ▼
                             Vec<RowRef>  ──►  uniform_list  ──►  Rows::render
                               8 bytes/row                          + Theme
```

Nothing flows backwards, but the tail is re-run: cycling the layout replays
everything from `prepare` against the same `Vec<FileDiff>`, which is why the view
keeps it.

A view never mutates what it was handed, which is why every stage above is
testable without a window and why `paint.rs` can join the pipeline one stage from
the end.

## The rule this shape exists to enforce

> `core/` never knows a UI exists.

Two checks, both cheap to run against a diff:

1. **`core` has no dependencies.** `core/Cargo.toml` ends with an empty
   `[dependencies]`. If something needs adding, the thing wanting it belongs in
   another crate.
2. **A second frontend needs no logic of its own.** `examples/paint.rs` draws a
   real diff — clipped, intraline-marked, syntax-coloured, themed — in ANSI, and
   contains no pipeline code. When that stops being true, logic has leaked into
   the shell. It has happened once already; see
   [decisions/0007](decisions/0007-assembly-in-core.md).

## Not built yet

Listed so nobody reads an intention as a description:

- **`cli/`.** Referenced throughout as the second door. `plait-tui` and
  `plait-web` now stand in as the proof that the boundary holds; what `cli/`
  would still add is a non-interactive door — a diff to stdout, an exit status —
  and `tui/examples/dump.rs` is most of it already.
- **Any keyboard beyond scrolling, in the terminal.** `plait-tui`'s views are
  components: every action is a method and nothing in the crate knows what a
  keypress is. There is no `main`, no event loop and no keymap, because a keymap
  written there is one `cli/` would have to duplicate — see the next item.
- **Command dispatch and the mode stack.** `Host` is where they belong. `s`
  cycling the diff layout and `w` cycling the wrap are the only key bindings that
  are not `cmd-q`, and both are shaped so dispatch has something to attach to
  rather than something to replace.
- **Configurable keybindings, and a settings panel.** The title-bar pickers are
  the interim answer: a control per registry, driven by the same names
  `plait.toml` uses. When the panel exists it should read the same registries and
  these should collapse into it.
- **Code hot reload.** The config file reloads *data* live, and `./dev.sh` removes
  everything either side of a code rebuild — but the rebuild itself remains, 3–5 s.
  A dylib swap was investigated and rejected, and the reasons are specific enough
  to be worth keeping: GPUI holds a thread-local element arena
  (`window.rs:321`, a raw pointer, so a second copy allocates into nothing), a
  process-wide entity id counter (`entity_map.rs:672`), and forty uses of
  `TypeId::of` for global and entity lookup. A dylib that statically links its own
  gpui forks all three. The fix is making gpui a real `dylib` and building with
  `-C prefer-dynamic` the way Bevy does — possible, multi-day, and broken by every
  gpui update. Dioxus's `subsecond` hot-*patches* the binary instead and sidesteps
  the ABI problem entirely; that is the direction to watch.
- **Extension loading.** Every seam takes an implementation today; nothing loads
  one from outside the binary yet. See [extending.md](extending.md).
- **Writes.** No commit, push, stage or rebase. Reads only.
- **A diff cache keyed by blob id.** Acquisition now yields the pair of object ids
  that produced a diff, and a blob never changes, so the cache is possible. It is
  not built.
- **`\ No newline at end of file`.** Content is split into lines, so a file with
  and without a trailing newline produce the same list and the distinction is
  lost. Needs a per-side flag on `Pair` and somewhere in `DiffLine` for a note.
- **Semantic diffs.** The seam takes an implementation from outside `core`, so a
  tree-sitter differ has somewhere to live. What it has nowhere to *say* is the
  sub-line part: `Edit` is line ranges, and a tree diff's whole value is "this
  argument was added". The pipeline already has the resolution — `Span`, from the
  intraline pass — but that is computed independently in stage 3b, so a `Differ`
  cannot emit it. Making semantic pay off is a change to the seam, not a new
  implementation behind it. See
  [decisions/0003](decisions/0003-scanner-over-tree-sitter.md) for what
  tree-sitter measured last time it was considered.
- **Move detection across files.** `moves` works within one file's script, so a
  block cut from one file and pasted into another is two unrelated changes. git
  has the same limit by default and lifts it with `--color-moved`\'s
  cross-file modes.
- **`gix`.** All reads still spawn `git`.
- **A rendering test, *in the shell*.** `plait-tui` has one — its screen is a
  cell buffer, so a row's text and its per-cell colour are both assertions, and
  82 tests exercise the real presentations. Nothing equivalent exercises a GPUI
  element tree:
  a panic in `render`, a colliding element id or a floating element painted under
  its sibling are all found by launching. `gpui`'s `test-support` feature would
  fix it and unifies onto every build — proptest and a leak detector on a
  three-second rebuild loop — so it has not been taken. The two GPUI traps in
  AGENTS.md's notes are what that costs.
- **Panes.** One view fills the window, so a presentation needing its own layout
  has nowhere to go yet — the reason the `Rows` seam is row-shaped. A rendered
  Markdown *row* exists ([decisions/0010](decisions/0010-markdown-rendered-rows.md));
  a rendered Markdown *document*, reflowed and variably tall, is what needs the pane.
  The diff view already measures its own box rather than reading the window's, so
  it is a pane's tenant already — see
  [decisions/0017](decisions/0017-wrapping-is-more-rows-not-taller-ones.md).
- **Code-block injection.** A fenced block in a `.md` diff knows it said `rust` and
  is still drawn as one string. See
  [decisions/0010](decisions/0010-markdown-rendered-rows.md).
