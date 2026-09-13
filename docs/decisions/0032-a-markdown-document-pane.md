# 0032 — A markdown file's diff is a document, in a pane of its own

**Status** accepted
**Date** 2026-09

## Context

`0010` made a `.md` file's diff show as a *document* rather than as source: the
markers off, a heading a size larger, a table measured into a grid. It is a `Rows`
implementation, so every one of those decisions lives inside a fixed row height,
and everything a reader of prose actually wants is what that height forbids:

| what a document needs | why a row cannot have it |
|---|---|
| an `#` at 1.7× the body | a glyph needs ~1.2× its point size of line box and `ROW_H` is 22px — past about 18px a heading clips into the row below, which is why levels 4–6 fall back to the body size |
| a code block with a background | a row's background in a diff means added or removed, and a diff may not give that up |
| a cell that wraps | a row is one `StyledText` on a monospaced line, aligned by padding cells with spaces |
| text that wraps where the window does | a wrapped line is *more rows*, never a taller one (`0017`) |

`0006` named the shape of the fix when it accepted the seam: "a rendered Markdown
preview … wants a pane, and panes do not exist yet." `0011` was an attempt at one,
parked in a stash rather than deleted, and the number is still held for it.

## Decision

**A markdown file gets a second body: `views::document::DocumentPane`.** It is not
a `Rows` implementation and it is not a list. It is an ordinary view, laid out with
flexbox, scrolled by the platform, and therefore variable in height — which is the
whole point.

**The choice is the reader's, and the selection is the door.** The centre draws the
diff, exactly as before; `m` (`document.flip`) asks for the file as a document.
The key is refused with a sentence — not silently ignored — for a file that has no
document to draw, which is what keeps this from being a *mode*. A pane stands
itself down when it is handed something it cannot draw, so nothing has to clear a
flag when the cursor moves.

**The model is `core`'s and the pixels are the window's.** `core::document::Document`
lays a whole file out through the same block pass, the same marker removal and the
same table measurement a hunk side goes through, and returns rows, blocks and
grids. It adds no parser and no dependency. What is new in it is the *unit*: a
whole file is one `for_each_side` run, because a file has no hunk boundaries:

- a fence opened on line 3 and closed on line 203 is one block — the case `0010`
  documented as impossible to express when the rows arrive as hunks;
- a table is measured against its own whole run, so the same table is the same
  width whatever changed near it.

**The read is bounded, whole, and off the render path.** `Repo::file_lines` is a
new read in the only crate that touches a repository. A working-tree side is
`stat`ed before it is read; an object-database side is asked its length with
`cat-file -s` before `cat-file blob` fetches it — so a file over the cap is refused
with a number instead of being read to find out. It runs in the *preview lane's
own job*, on the same thread and behind the same request guard as the diff, so a
fast cursor run still collapses to one load.

**The pane is grouped once, at load.** `sections_of` turns the rows into what the
renderer draws: a table is one element, a fence and its body are one element, a row
is a row. Regrouping per frame would be rule 3's exact complaint for an answer that
cannot change until the file does.

## Why not the alternatives

**A taller row in the existing presentation.** Every item in the table above is a
consequence of one number, and the number is `uniform_list`'s requirement — which
is what makes a 714k-row diff scroll at all. Trading that for prose means the diff
pane gets slower for the files that are not prose.

**A separate window or tab.** The comparison a reader wants is between the file's
diff and the file itself, one keystroke apart, in the same column, at the same
scroll position. A second window is a second thing to place and dismiss.

**`markdown 1.0`, which is already in the tree through `gpui-component`.** `0010`
answered this for the row presentation and the answer does not change: `core` takes
no dependencies, and a hunk — or a file with fences that a *stash* cut in half — is
not a document in the sense a CommonMark parse means.

**Space-padded table cells here too.** The pane's cells are elements with weighted
widths, so a column takes a share in proportion to what it asked for and a long cell
wraps inside its column. Padding would need a monospaced face and could not wrap;
weighting needs neither, and it is the same water-filling policy `flow_table`
applies, reached with elements instead of characters.

## Evidence

`cargo test`, on this branch:

- `gitten-core`: 506 tests, including eight for `document` — the marker removal, the
  200-line fence that stays one block, the two tables that measure separately, and
  the token/byte-range invariant every renderer depends on.
- `gitten-gui`: 394 tests, including ten new ones: the grouping (a fence is one
  draw, a table is one draw, every row is drawn by exactly one section), the scale
  (unclamped, monotonic), that every shape draws without a window, and two through
  the **real** path — a `.md` file in the rail reaches the pane and back to the rows
  through `document.flip`, and a `.rs` file is a sentence instead of a mode.
- `gitten-git`: the four new read tests — the working tree and the index side
  disagreeing on purpose, the cap refused with its limit from both sides, a Latin-1
  file read rather than refused, and a missing side as an error rather than an empty
  document.

Cost, by construction rather than by stopwatch: one `stat` and one file read per
document a reader asks for, on a job thread; zero processes for a text diff that
nobody flips; and nothing on the render path that allocates beyond GPUI's own
element tree.

**Not measured:** the layout cost of a large document (`AGENTS.md` is 31 KB; the
largest in this repository) and the frame time of a long one. The obvious next
instrument is a stopwatch in `core::document::Document::new` over `md.diff`, the way
`0010` measured its own pass — and until that exists this document claims no number.

## Consequences

- **A `.md` file now has two presentations and two answers to "what is this
  file"**. They are `views::markdown`'s claim list and this pane's — the second asks
  the first (`views::markdown::claims`), so they cannot drift; a file that renders
  as markdown rows gets a document, by construction.
- **Inline markup inside a heading still keeps its markers.** That is `0010`'s
  inherited blind spot — the highlighter emits one whole-line `Heading` token and
  never scans inside it — and the pane inherits it exactly.
- **A fenced block has no syntax highlighting.** Its bytes are drawn as code on the
  raised surface, in the chrome's text colour. Injecting the routed highlighter back
  into a fence body is still `0004`'s named next lever, and it would improve both
  presentations at once.
- **A `Revspec` side is a `rev`-shaped read.** `gitten diff . HEAD~2..HEAD` and then
  `m` asks `cat-file -s HEAD~2..HEAD:path`, which git refuses; the refusal arrives as
  a sentence in the band. A revspec names an aggregate read and not one side, so the
  honest fix is to resolve the side first, not to widen the read.
- **The pane is not a registry.** It is built by the shell, so an extension cannot
  swap it — which is rule 1's shape, unfinished. What *is* shared is the model: a
  second frontend (the terminal) can consume `core::document` without touching this
  file, and a `Docs` registry beside `Layouts` is the step that would let an
  extension draw its own. Named here rather than pretended away.
- **`0011` stays held.** The parked reader is not this: this is a pane reached by a
  key on a file already on screen, and the stash's own text, if it is ever recovered,
  is a reader rather than a body. The number remains reserved for it.
