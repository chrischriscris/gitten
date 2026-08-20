# Architecture

Three crates. The interesting line is between the first two and the third.

```
                        ┌──────────────────────────────────────┐
                        │  plait-core          zero deps       │
   a repository         │                                      │
        │               │  parse_log      assign_lanes         │
        │               │  parse_unified_diff   intraline      │
        ▼               │  prepared::prepare                   │
┌───────────────┐ data  │  syntax::{Lexer, Markdown, ...}      │
│  plait-git    ├──────►│  theme::Theme        host::Host      │
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
| `syntax.rs` | the scanner, the language tables, the `Highlighter` trait, routing, Markdown |
| `theme.rs` | every colour, as `0xRRGGBB` data, plus contrast resolution |
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
| `views/diff.rs` | the `Rows` seam, `TextRows`, run-list merging |
| `views/commits.rs` | the commit list, author initials, row layout |
| `graph.rs` | lane geometry and painting: quads, paths, one canvas per row |
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
- **Extension loading.** Every seam takes an implementation today; nothing loads
  one from outside the binary yet. See [extending.md](extending.md).
- **Writes.** No commit, push, stage or rebase. Reads only.
- **`gix`.** All reads still spawn `git`.
- **Panes.** One view fills the window, so a presentation needing its own layout
  has nowhere to go yet — the reason the `Rows` seam is row-shaped.
