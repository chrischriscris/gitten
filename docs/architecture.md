# Architecture

**A core and a set of clients.** Three shared layers, then one crate per client,
and anyone can write another.

```
   ┌──────────────────────────────────────────────────────────────────────┐
   │  gitten-core                                              zero deps   │
   │  parse_log  assign_lanes  differ::{Histogram, Myers}  align  wrap    │
   │  prepared::prepare  rows::{Flat, Present, expand}  runs  markdown    │
   │  syntax  graph::Hues  command::{Key, Keymap, Modes}  status  select │
   │  refs  search  patch  font  host::Host — every swappable piece      │
   └───────────────────────────────┬──────────────────────────────────────┘
   ┌───────────────────────────────┴──────────────────────────────────────┐
   │  gitten-git      the only crate that talks to a repository            │
   │  gitten-app      gitten.toml, the command line, acquisition            │
   └──┬──────────────────┬──────────────────┬─────────────────────────────┘
      │                  │                  │
 ┌────▼───────┐ ┌────────▼─────┐ ┌──────────▼──┐   ┌──────────────────┐
 │gitten-shell │ │  gitten-tui   │ │  gitten-web  │   │ yours            │
 │GPUI window │ │cells, raw tty│ │loopback HTTP│   │ AnyElement, a    │
 │            │ │              │ │  + a page   │   │ cell, a payload  │
 └────────────┘ └──────────────┘ └─────────────┘   └──────────────────┘
```

A client is **drawing and input, and nothing else.** Everything above that line
is one implementation: the same differ, the same rows, the same order table, the
same `gitten.toml`, the same keymap. What differs is the type a `Rows`
implementation returns — an `AnyElement`, a row of cells, a JSON payload — and
that is the only reason the `Rows` trait itself cannot live in `core`.

**The three are not equal.** `gitten-shell` is the product; `gitten-tui` is
planned and built and comes after it; `gitten-web` is a proof that the boundary
holds and not a thing anybody asked to ship. A feature asked for without a client
named means the window. See [clients.md](clients.md), and `AGENTS.md` for the
tie-break when a shared seam and a good window disagree.

`core/examples/paint.rs` is a fourth, tiny client: a real diff in ANSI, no
crate of its own, and still the cheapest place to look at a colour.

## gitten-core

Pure. No GPUI, no gitoxide, no I/O, and `[dependencies]` is empty — deliberately,
because it compiles in a second and its tests need no window.

| module | what lives there |
|---|---|
| `lib.rs` | commit and diff parsing, `assign_lanes`, `intraline`, `replace_pairs`, `initials` |
| `command.rs` | keys, chords, the mode stack, the keymap, the command registry |
| `graph.rs` | which branch is which colour, the lane cap, the honest lane count |
| `rows.rs` | a diff flattened to rows, the wrap index, the order table, the load path |
| `runs.rs` | syntax tokens × intraline spans → one flat styled run list |
| `select.rs` | what the mouse has selected: carets, the rows between them, and the copy |
| `view.rs` | `Viewport`: the cursor, the top row, the margin, scrolling that is not a cursor move, and where a scrollbar's thumb goes |
| `differ.rs` | the `Differ` trait, Histogram/Patience/Myers, whitespace relations, the indent heuristic, move detection, hunk assembly, routing |
| `align.rs` | which removal sits opposite which addition, for a two-column view |
| `prepared.rs` | a diff assembled into drawable rows: clip → intraline → syntax |
| `markdown.rs` | a `.md` diff as blocks, with the markers cut and the ranges moved |
| `syntax.rs` | the scanner, the language tables, the `Highlighter` trait, routing, Markdown |
| `status.rs` | staged, unstaged, untracked and conflicted entries with byte-preserving paths |
| `refs.rs` | branches, remote branches, HEAD, stashes, remotes, tags, reflog — names as bytes, absence as data |
| `search.rs` | commit-list search: the index folded once per load, substring per keystroke |
| `patch.rs` | chosen hunks synthesized into one unified patch, for the stage/unstage/discard-hunk verbs |
| `theme.rs` | every colour as `0xRRGGBB` data, the seven shipped palettes, contrast resolution |
| `font.rs` | the face as data: family, size, and whether a char is a column |
| `host.rs` | the struct that holds the swappable pieces |

Four examples double as the headless test bench: `bench` (timings at fixture
scale), `shape` (topology statistics), `verify` (lane-assignment invariants),
`paint` (the diff view in ANSI).

Five of these modules exist because a third client was written. `rows`, `runs`
and `graph::Hues` each had two implementations in two clients before they had one
here, and `view` had two in the *same* client — the terminal's diff and its
commit list, one of which had already lost the name of its own margin. `command`
had none, and the keymap it replaced was three `match` statements
that could not agree — see [terminal.md](terminal.md) and
[clients.md](clients.md).

## gitten-git

The only crate that talks to a repository. Everything — reads and writes both —
shells out to the `git` binary, because shelling out is what gets hooks,
credential helpers, SSH agents and `.gitconfig` semantics exactly right. `gix`
remains the intended destination for the hot read path (see *Not built yet*);
writes stay on the binary permanently.

Everything enters through `Repo`, one object-safe trait held behind a
`Handle`, so one opened repository serves every view, reload and re-acquire
without threading a concrete type through them. Reads: `log`, `pairs`,
`status`, `describe`, plus branches, remote branches, HEAD, stashes, remotes,
tags and the reflog — the optional ones default to an error, so an
implementation carries only what it serves. Writes are verbs on the same
trait — stage and unstage (one path, many paths, or a synthesized hunk patch),
commit and amend, discard, remove-untracked, ignore, checkout/create/delete/
rename branch, stash push/apply/pop/drop, reset soft/mixed/hard, revert, push,
pull, fetch — and no frontend learns which process ran. A fake implementation
stands in for the binary throughout the tests.

`status` parses porcelain v2 into staged, unstaged, untracked and conflicted
lists. Paths remain bytes through the model and repository layer; lossy decoding
is a display operation, never the address handed back to git.

**Untracked files come from `git status`, not `git diff`.** `git diff` compares
the index and the working tree against a commit, and an untracked file is in none
of the three — so it has nothing to diff and never reports one. Every client that
shows them asks `status` separately, and `pairs` does that for an empty revspec
and synthesises a pair with an empty old side. Without it, "show me my
uncommitted work" silently omits every file you just created, which on a real
branch is most of what you are looking for.

**It acquires content, not diffs.** `pairs` returns two lists of lines per changed
file; `diff` is that plus the host's `Differs`. It used to run `git diff` and parse
the unified output back, which meant git chose the algorithm and
`gitten_core::differ` could not have existed — see
[decisions/0013](decisions/0013-differs-in-core-not-a-dependency.md). Two
processes for a whole diff whatever the file count: one `git diff --raw`, one
long-lived `git cat-file --batch`. The batch streams (`BlobStream`): each
file's blobs are parsed and handed over as they arrive, and dropped before the
next file's are read, so memory holds one file's content rather than the whole
diff and a thousand-file request cannot fill both pipes and block forever.

`examples/diffcheck.rs` is the headless check that the differs agree with git on
real history, and is run by `./check.sh`.

## gitten-app

The config file, the command line, and acquisition — the whole of a client's
startup, and nothing that draws. Startup has one seam in it:
[`Startup::configure`](clients.md) is everything `Startup::go` does but the
acquisition, for a client with something to draw first — the desktop — and the
client schedules that acquisition itself. What is shared is the chain, not the
ordering.

| module | what lives there |
|---|---|
| `config.rs` | `gitten.toml`: parse, apply, write out, watch |
| `cli.rs` | `View`, `Source`, `Request`, the usage text, a client's own flags |
| `acquire.rs` | one view of one source into `Vec<FileDiff>` or `Vec<Commit>`, and re-acquisition after writes |
| `jobs.rs` | serial blocking jobs and lifecycle events; the finish-counting generation — a refusal advances it too, because git can answer nonzero having left work behind (a conflicted revert) |
| `verbs.rs` | the write verbs as jobs: each captures a `Handle` clone plus its arguments and calls the trait; an extension composes these exact words |
| `lib.rs` | `Startup` — the lines a client's `main` starts with; `configure` stops before acquisition for a client that draws first |

It exists because all of that was written twice and about to be written a third
time, and because `config.rs` used to live behind GPUI, which made the window the
only client that could be configured. `toml` and `notify` are here rather than in
`core` for the reason `core` has no dependencies at all: reading a file is I/O.

What is *not* here is how a reload reaches the views. `watch` is shared; what to
do when it fires is a client's, because GPUI swaps a global and a terminal drops
a flag into its event loop.

## gitten-tui

The terminal. A cell grid, the presentations that fill it, and escape codes.

| file | what lives there |
|---|---|
| `screen.rs` | cells, ink, the pen, the two-buffer diff, and `print` |
| `rows.rs` | the `Rows` seam, `Layouts`, `TextRows`, the shared row furniture |
| `split.rs` | `SplitRows`, at half the width and with its own scroll |
| `diff.rs` | the diff view: the order table, reflow, commands |
| `commits.rs` | the commit list, and the graph in box drawing |
| `help.rs` | what the keys do, as a pure function of the keymap |
| `term.rs` | the only module that touches `crossterm` |
| `main.rs` | the event loop: a key, a command name, a method |

`examples/dump.rs` prints one frame of either view to stdout, which is how it is
looked at without a terminal — and, because `Screen` is a `Vec<Cell>`, it is the
one frontend whose *drawing* is unit-tested. See [terminal.md](terminal.md).

## gitten-web

**A proof, not a product.** It exists to answer one question — can a client
written in a different language, with no access to any of this crate's types,
draw a gitten diff? — and the answer being yes is what says `core` has no UI in
it. Nobody asked for a web app and the roadmap does not have one.

Read it that way when deciding whether to invest in it. It still holds its own
row flattening (`rows.rs`) and its own keymap (`ui/app.js`), and those are worth
*knowing about* rather than worth fixing: closing them buys a client nobody
ships. What matters is that it never constrains `core` — if `gitten-web` ever
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

## gitten-shell

GPUI. Drawing and input, and as little else as possible.

| file | what lives there |
|---|---|
| `main.rs` | the window, its `Launch` seam (`Skeleton` opens before acquiring), named command dispatch, pane assembly and job-event draining |
| `input.rs` | native GPUI text input: IME composition, UTF-16 selection and grapheme editing |
| `panes.rs` | stable pane registration, replacement and logical focus |
| `views/diff.rs` | the `Rows` seam, `TextRows`, run-list merging, the shared row furniture |
| `views/diff.rs` | …and `Layouts`, the registry of whole-diff presentations |
| `views/markdown.rs` | `MarkdownRows`: the rendered-Markdown presentation, and its metrics |
| `views/split.rs` | `SplitRows`: the two-column presentation |
| `views/commits.rs` | the commit list, author initials, row layout |
| `views/files.rs` | the working tree: staged/unstaged/untracked/conflicted sections over `core::status` |
| `views/branches.rs` | local and remote-tracking branches, ahead/behind counts, detached HEAD named |
| `views/stashes.rs` | the stash stack, newest first, `stash@{n}` beside its message |
| `dispatch.rs` | GPUI keystroke → `command::Key`: the window's half of input, and how it reads `[keys]` |
| `input.rs` | native text input: IME composition, UTF-16 selection, grapheme editing; feeds the prompt slot |
| `panes.rs` | tenants registered under stable names, replaced in place; logical focus — no GPUI in it |
| `graph.rs` | lane geometry and painting: quads, paths, one canvas per row |
| `settings.rs` | the settings panel: every knob as rows built from the registries |
| `config.rs` | config reload wiring, widget-theme sync and the live `Host` global |
| `session.rs` | the row you were on, so `./dev desktop` can put you back after a restart |
| `stats.rs` | the counting allocator and the `GITTEN_STATS` overlay |
| `assets/icon.svg` | the mark: three lanes weaving. `./dev bundle` renders the iconset from it |

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

A finished background job advances `app::jobs::Generation` — a refusal as much
as a success, because git can answer nonzero with work already left behind (a
conflicted revert leaves its unmerged paths in the index). Every visible
repository pane re-acquires through the retained `Repo` handle and replaces its
data in place while preserving its semantic cursor anchors. Each pane supplies a
type-erased `Refresh`: blocking acquisition and pure row/graph preparation run on
the background executor, then a generation-guarded apply touches its GPUI entity.
A failed or panicked write emits an error and schedules the same re-acquire wave,
so the panes show the state the refusal left rather than the state they remember.

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

- **`cli/`.** Referenced throughout as the second door. `gitten-tui` and
  `gitten-web` now stand in as the proof that the boundary holds; what `cli/`
  would still add is a non-interactive door — a diff to stdout, an exit status —
  and `tui/examples/dump.rs` is most of it already.
- **A settings panel.** Configurable keybindings and the shared command/help
  registries exist. The panel reads those same names and holds every live knob
  in one surface — see
  [decisions/0028](decisions/0028-settings-live-in-a-panel.md); what it does
  not edit is colours and the two next-launch font fields, which stay in the
  file.
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
- **A Linux build.** CI compiles the workspace and runs every crate's tests on
  Linux, and the tree keeps itself portable — no `cfg(target_os)`, no
  macOS-only crate, OSC 52 for the clipboard — under that gate. What nobody
  has done is *develop* there, and `./dev bundle` is macOS-only on purpose,
  with no counterpart.
- **A diff cache keyed by blob id.** Acquisition now yields the pair of object ids
  that produced a diff, and a blob never changes, so the cache is possible. It is
  not built.
- **`\ No newline at end of file`.** Content is split into lines, so a file with
  and without a trailing newline produce the same list and the distinction is
  lost. Needs a per-side flag on `Pair` and somewhere in `DiffLine` for a note.
  Meanwhile the limit is pinned: a synthesized patch touching such a last line
  is refused by git at all three hunk verbs, and a test holds both sides
  byte-identical.
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
- **Code-block injection.** A fenced block in a `.md` diff knows it said `rust` and
  is still drawn as one string. See
  [decisions/0010](decisions/0010-markdown-rendered-rows.md).
