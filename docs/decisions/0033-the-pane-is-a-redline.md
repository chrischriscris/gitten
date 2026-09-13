# 0033 — The document pane is a redline, not a preview

**Status** accepted
**Date** 2026-09

## Context

`0032` gave a `.md` file a second body: a pane that lays the whole file out as a
document — a fence that opens on line 3 and closes on line 203 is one element, a
table is measured against every row it has, a heading has no row-height ceiling.
It drew that document with **no mark of the change at all**. The rows beside it
mark everything, and the two bodies are one keystroke apart (`document.flip`), so
the reader who pressed `m` on a file they were reviewing lost the one thing they
were looking at: what the change did.

The obvious seam — lay the *diff's* rows out as a document — is closed by
`0010`: a diff row of a `.md` file has had its markdown markers taken off it by
the presentation. Splicing those rows back into a document would leave a table
row with no pipes in it, which shears the grid it lands in, and a heading with no
`#`, which reads as prose.

## Decision

**The pane draws the change inside the document.** The document is still laid out
whole-file, and each of its rows carries the diff's own answer for it — context,
added or removed — and draws in the diff's colour for that answer:

- an added row takes `added_fg` on the `added_bg` wash, a removed row
  `removed_fg` on `removed_bg` — the same four colours the rows use, so the pane
  and the rows agree on what green means;
- a removed row is **struck through** in its own colour, because a document has no
  second column in which to say "this was here";
- syntax still supplies weight and slant, but a touched row takes its colour from
  the diff and not from the grammar — otherwise an added heading would be green
  nowhere on the row;
- the text of a removal comes from the **other side of the same source**, read
  whole, and never from the diff's rows. `DiffSource::other_side` names it:
  `Unstaged` is the index, `Staged` is `HEAD`, a commit is its first parent. A
  source with nothing before it — an untracked file, a patch, a fixture — has no
  removals to place and reads as all-addition.

`core::document::redline` is the splice: it takes the new file's lines, the old
file's lines and the diff's rows, and returns the document's lines with every
removal put back before the new line that followed it, plus one kind per line.
The pane lays *that* out, so the layout has no idea a diff exists.

## Alternatives considered

- **A second pane for the diff of the document.** Two documents side by side is
  two more scroll positions to keep together, and it answers "what changed" with
  "here are two texts".
- **Splicing the diff's own rows.** Rejected above, and by `0010`'s premise: the
  rows are a presentation, not text.
- **Additions only** — the document as it now is, with what is new in green and
  what is gone invisible. Cheaper by exactly one read, and it answers the review
  question by half: a reader cannot see a deletion inside a document that no
  longer contains it.
- **A marker in the margin** (`+`/`-` in a gutter) instead of a wash. The wash is
  what makes a change read at a glance in a layout that is not a column of lines;
  a gutter would also need a fourth column of width the pane does not have.

## Evidence

- `core`: 512 tests, including the splice's own five — a removal before the line
  that followed it, an addition, a removal at the end of the file, a spliced
  removal carrying the *file's* text rather than the row's, and a file with no
  other side.
- `gui`: 395 tests, including a marked row drawn without a window — the run list
  is rebased per row, and a style reaching past its own text is a panic inside
  GPUI's layout rather than a wrong colour, so that path is walked.
- Measured on the demo document (`/tmp/gitten-md-demo-repo`, one bullet removed
  and one paragraph added): the removed bullet's row carries a continuous rule
  500px wide — the strikethrough — inside a 33px red wash, and the added
  paragraph's two lines carry a 50px green wash beneath it. The table between
  them, which the change did not touch, stays unwashed.

## Consequences

- The pane now needs **two reads** instead of one: the file, and its other side.
  Both are capped, both are on the job thread, and both are behind the same
  request guard as the diff — so a fast cursor run still collapses to the last
  file. `other_side` returns `None` rather than guessing for a source whose other
  side is not one file, and a refusal there is not an error: no before, no
  removals, all-addition.
- A **moved** block draws as a removal *and* an addition rather than as a move,
  because the rows carry `moved` and the document does not. Named, not hidden.
- On a whole-file rewrite the document is mostly green (or mostly red, read
  through the removals), which is the correct answer and not a pleasant picture.
  Nothing about the pane's colours is configurable; the wash is the diff's own
  palette, so a theme that fixes the diff fixes both.
