# Architecture

**A core and a set of clients.** Three shared layers, then one crate per client,
and anyone can write another.

```
   ┌──────────────────────────────────────────────────────────────────────┐
   │  plait-core                                              zero deps   │
   │  parse_log  assign_lanes  differ::{Histogram, Myers}  align  wrap    │
   │  prepared::prepare  rows::{Flat, Present, expand}  runs  markdown    │
   │  syntax  graph::Hues  command::{Key, Keymap, Modes}  theme  font     │
   │  host::Host — every swappable piece, in one struct                   │
   └───────────────────────────────┬──────────────────────────────────────┘
   ┌───────────────────────────────┴──────────────────────────────────────┐
   │  plait-git      the only crate that talks to a repository            │
   │  plait-app      plait.toml, the command line, acquisition            │
   └──┬──────────────────┬──────────────────┬─────────────────────────────┘
      │                  │                  │
 ┌────▼───────┐ ┌────────▼─────┐ ┌──────────▼──┐   ┌──────────────────┐
 │plait-shell │ │  plait-tui   │ │  plait-web  │   │ yours            │
 │GPUI window │ │cells, raw tty│ │loopback HTTP│   │ AnyElement, a    │
 │            │ │              │ │  + a page   │   │ cell, a payload  │
 └────────────┘ └──────────────┘ └─────────────┘   └──────────────────┘
```

A client is **drawing and input, and nothing else.** Everything above that line
is one implementation: the same differ, the same rows, the same order table, the
same `plait.toml`, the same keymap. What differs is the type a `Rows`
implementation returns — an `AnyElement`, a row of cells, a JSON payload — and
that is the only reason the `Rows` trait itself cannot live in `core`.

**The three are not equal.** `plait-shell` is the product; `plait-tui` is
planned and built and comes after it; `plait-web` is a proof that the boundary
holds and not a thing anybody asked to ship. A feature asked for without a client
named means the window. See [clients.md](clients.md), and `AGENTS.md` for the
tie-break when a shared seam and a good window disagree.

`core/examples/paint.rs` is a fourth, tiny client: a real diff in ANSI, no
crate of its own, and still the cheapest place to look at a colour.

## plait-core

Pure. No GPUI, no gitoxide, no I/O, and `[dependencies]` is empty — deliberately,
because it compiles in a second and its tests need no window.

| module | what lives there |
|---|---|
| `lib.rs` | commit and diff parsing, `assign_lanes`, `intraline`, `replace_pairs`, `initials` |
| `command.rs` | keys, chords, the mode stack, the keymap, the command registry |
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

Four of these modules exist because a third client was written. `rows`, `runs`
and `graph::Hues` each had two implementations in two clients before they had one
here; `command` had none, and the keymap it replaced was three `match` statements
that could not agree — see [terminal.md](terminal.md) and
[clients.md](clients.md).

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

## plait-app

The config file, the command line, and acquisition — everything a client needs
before it can draw, and nothing that draws.

| module | what lives there |
|---|---|
| `config.rs` | `plait.toml`: parse, apply, write out, watch |
| `cli.rs` | `View`, `Source`, `Request`, the usage text, a client's own flags |
| `acquire.rs` | one view of one source into `Vec<FileDiff>` or `Vec<Commit>` |
| `lib.rs` | `Startup` — the four lines a client's `main` starts with |

It exists because all of that was written twice and about to be written a third
time, and because `config.rs` used to live behind GPUI, which made the window the
only client that could be configured. `toml` and `notify` are here rather than in
`core` for the reason `core` has no dependencies at all: reading a file is I/O.

What is *not* here is how a reload reaches the views. `watch` is shared; what to
do when it fires is a client's, because GPUI swaps a global and a terminal drops
a flag into its event loop.

## plait-tui

The terminal. A cell grid, the presentations that fill it, and escape codes.

| file | what lives there |
|---|---|
| `screen.rs` | cells, ink, the pen, the two-buffer diff, and `print` |
| `rows.rs` | the `Rows` seam, `Layouts`, `TextRows`, the shared row furniture |
| `split.rs` | `SplitRows`, at half the width and with its own scroll |
| `diff.rs` | the diff view: viewport, reflow, commands |
| `commits.rs` | the commit list, and the graph in box drawing |
| `help.rs` | what the keys do, as a pure function of the keymap |
| `term.rs` | the only module that touches `crossterm` |
| `main.rs` | the event loop: a key, a command name, a method |

`examples/dump.rs` prints one frame of either view to stdout, which is how it is
looked at without a terminal — and, because `Screen` is a `Vec<Cell>`, it is the
one frontend whose *drawing* is unit-tested. See [terminal.md](terminal.md).

## plait-web

**A proof, not a product.** It exists to answer one question — can a client
written in a different language, with no access to any of this crate's types,
draw a plait diff? — and the answer being yes is what says `core` has no UI in
it. Nobody asked for a web app and the roadmap does not have one.

Read it that way when deciding whether to invest in it. It still holds its own
row flattening (`rows.rs`) and its own keymap (`ui/app.js`), and those are worth
*knowing about* rather than worth fixing: closing them buys a client nobody
ships. What matters is that it never constrains `core` — if `plait-web` ever
wants something in `core` that the window does not, the window wins.

A loopback HTTP server and a page. No third-party dependencies of its own: the
server is a `TcpListener`, the JSON writer is a `String`. Everything above
drawing runs natively in the process you started, so nothing needs a wasm target;
the browser re-implements the drawing.

| file | what lives there |
|---|---|
| `lib.rs` | the routes, and which view is loaded |
| `api.rs` | the payloads: `meta`, `rows`, `commits`, and the theme resolved |
| `rows.rs` | the diff flattened to rows, and the wrap table |
| `log.rs` | the commit list, with `core::graph`'s plan resolved once |
| `http.rs`, `json.rs` | a server and a writer, both a few hundred lines |
| `ui/` | one page for both views: a virtual list, the theme as custom properties, SVG for the graph |

The graph crosses the wire as `core::graph::plan` — the halves, not a drawing of
them — so the browser's SVG paths and the window's Bézier curves are the same
shape from the same numbers.

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
| `session.rs` | the row you were on, so `./dev desktop` can put you back after a restart |
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
- **Code hot reload.** The config file reloads *data* live, and `./dev` removes
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
