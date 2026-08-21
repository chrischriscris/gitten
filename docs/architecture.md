# Architecture

Three crates. The interesting line is between the first two and the third.

```
                        ┌──────────────────────────────────────┐
                        │  plait-core          zero deps       │
   a repository         │                                      │
        │               │  parse_log      assign_lanes         │
        │               │  parse_unified_diff   intraline      │
        ▼               │  prepared::prepare   markdown::lay_out│
┌───────────────┐ data  │  syntax::{Lexer, Markdown, ...}      │
│  plait-git    ├──────►│  theme::Theme  font::Font  host::Host │
│               │       │                                      │
│  git binary   │       └───────────┬──────────────┬───────────┘
│  (gix later)  │                   │              │
└───────────────┘          rows,    │              │  rows, colours
                           colours  │              │
                        ┌───────────▼──────┐  ┌────▼─────────────────┐
                        │  plait-shell     │  │  examples/paint.rs   │
                        │  GPUI window     │  │  ANSI, headless      │
                        └──────────────────┘  └──────────────────────┘
                                               (and a cli/, one day)
```

## plait-core

Pure. No GPUI, no gitoxide, no I/O, and `[dependencies]` is empty — deliberately,
because it compiles in a second and its tests need no window.

| module | what lives there |
|---|---|
| `lib.rs` | commit and diff parsing, `assign_lanes`, `intraline`, `replace_pairs`, `initials` |
| `prepared.rs` | a diff assembled into drawable rows: clip → intraline → syntax |
| `markdown.rs` | a `.md` diff as blocks, with the markers cut and the ranges moved |
| `syntax.rs` | the scanner, the language tables, the `Highlighter` trait, routing, Markdown |
| `theme.rs` | every colour, as `0xRRGGBB` data, plus contrast resolution |
| `font.rs` | the face as data: family, size, and whether a char is a column |
| `host.rs` | the struct that holds the swappable pieces |

Four examples double as the headless test bench: `bench` (timings at fixture
scale), `shape` (topology statistics), `verify` (lane-assignment invariants),
`paint` (the diff view in ANSI).

## plait-git

The only crate that talks to a repository. Everything currently shells out to the
`git` binary; reads are meant to move to `gix` while writes stay on the binary
permanently, because shelling out is what gets hooks, credential helpers and
`.gitconfig` semantics exactly right.

Today: `log`, `diff`, `describe`. Both behind one surface, so a frontend never
learns which path ran.

## plait-shell

GPUI. Drawing and input, and as little else as possible.

| file | what lives there |
|---|---|
| `main.rs` | argument parsing, data loading, the window, the `Host` |
| `views/diff.rs` | the `Rows` seam, `TextRows`, run-list merging, the shared row furniture |
| `views/markdown.rs` | `MarkdownRows`: the rendered-Markdown presentation, and its metrics |
| `views/commits.rs` | the commit list, author initials, row layout |
| `graph.rs` | lane geometry and painting: quads, paths, one canvas per row |
| `config.rs` | `plait.toml`: parse, apply, watch, and the live `Host` global |
| `session.rs` | the row you were on, so `./dev.sh` can put you back after a restart |
| `stats.rs` | the counting allocator and the `PLAIT_STATS` overlay |

## Which way data moves

```
  git binary ──► bytes ──► parse_unified_diff ──► Vec<FileDiff>
                                                      │
                                    prepared::prepare │  clip, intraline, syntax
                                                      ▼
                                                Vec<prepared::File>
                                                      │
                              Rows::build (per file)  │  claimed by path
                                                      ▼
                              Vec<RowRef>  ──►  uniform_list  ──►  Rows::render
                                8 bytes/row                          + Theme
```

Nothing flows back. A view never mutates what it was handed, which is why every
stage above is testable without a window and why `paint.rs` can join the pipeline
one stage from the end.

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

- **`cli/`.** Referenced throughout as the second door. `paint.rs` currently
  stands in for it as the proof that the boundary holds.
- **Command dispatch and the mode stack.** `Host` is where they belong.
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
- **`gix`.** All reads still spawn `git`.
- **Panes.** One view fills the window, so a presentation needing its own layout
  has nowhere to go yet — the reason the `Rows` seam is row-shaped. A rendered
  Markdown *row* exists ([decisions/0010](decisions/0010-markdown-rendered-rows.md));
  a rendered Markdown *document*, reflowed and variably tall, is what needs the pane.
- **Code-block injection.** A fenced block in a `.md` diff knows it said `rust` and
  is still drawn as one string. See
  [decisions/0010](decisions/0010-markdown-rendered-rows.md).
