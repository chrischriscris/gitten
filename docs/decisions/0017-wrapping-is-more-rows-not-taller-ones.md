# 0017 — Wrapping is more rows, not taller ones

**Status** accepted
**Date** 2026-08

## Context

A long line is the one thing in a diff you cannot read by scrolling. The eye
loses the row on the way back, and in the two-column layout it loses both. Every
editor answers this with soft wrap and the app did not have it: it had a
horizontal scrollbar and a 2000-character clip.

The obvious implementation is the one thing this app cannot do.
[0006](0006-row-seam-without-boxing.md) rests on `uniform_list`, which is the
only reason a 714k-row diff scrolls at all, and it needs **every row the same
height**. A wrapped line drawn as one taller row ends that.

Two more constraints, both from work already done:

- **A wrapped line has to still be one line.** The edit script, the hunk
  numbering, `replace_pairs`, `align`, the intraline spans and the syntax tokens
  all address lines. Splitting a line in two before those ran pairs a removal
  with the wrong addition — the exact failure `align`'s doc comment exists to
  prevent, and the reason that function is in `core`.
- **The budget is the window, and the window moves.** Wrapping to the viewport
  means reflowing on resize, and a resize is a drag: whatever it costs, it costs
  per frame.

## Decision

**A wrapped line is *n* rows of `ROW_H`, and the wrap produces byte ranges into
the line rather than new lines.** The line, its numbers, its tokens and its spans
are one object shared by all of its rows; a renderer draws range `k` of line `i`.
Nothing above stage 4 of the pipeline learns that wrapping exists.

**Where a line breaks is a seam.** `plait_core::wrap::Wrap` returns break points
and nothing else. Three built-ins — `word` (selected), `char`, `off` — and
`Wraps` is the registry, on `Host`. Everything else is shared: `Wrapped` turns
break points into the range partition, validates them, holds them flat and
answers by index.

**`off` is an entry in the registry, not a flag beside it.** The title-bar picker
is a pure function of a registry ([0015](0015-title-bar-controls-are-hand-rolled.md)),
so "off" being registered is what puts it in the menu with nothing written by
hand. It answers `breaks_lines() == false`, which is how a resize skips the whole
reflow rather than rescanning 714k lines to be told nothing moved.

**The column budget is per line, not per diff.** `MarkdownRows` draws a bar, up
to three levels of indent and a bullet in front of its text, and draws a heading
at 18px where the body is 14 — so two rows of the same width hold different
numbers of characters. Handing the budget in per line is what stops that
presentation needing a wrap of its own.

**A reflow re-runs stages 4c–5 only.** Not `prepare`, which is 247 ms on the
pathological fixture. It rescans the text for break points and rebuilds the order
table, and it exits on a float comparison when the width crossed no character
boundary.

## Why not one taller row

`uniform_list` cannot express it, and the alternative to `uniform_list` is a
hand-rolled variable-height list with its own measurement pass — which
`AGENTS.md` names as a day already lost once, and which at 714k rows means
measuring 714k rows to know how tall the content is. The height of a wrapped diff
is not knowable without wrapping all of it anyway, so the measurement is the
work; doing it as *rows* means the answer is an integer and the list stays
virtualized for free.

## Why the wrap is not part of `prepare`

`prepare` is 8–247 ms and produces the text, the spans and the tokens — none of
which depend on the window. The wrap does, and it is 1–26 ms. Putting them in one
pass makes every resize pay for a syntax scan it cannot use.

That split is also what makes the reflow honest: stage 3 owns what a line *is*,
stage 4c owns how it is broken up, and the second one can run again as often as
the window moves.

## Why ranges, and not `Vec<Line>`

Splitting into lines is simpler to render and wrong in three places:

- `align` would see a 3-row removal as three removals and pair the second one
  against an addition whose intraline spans were computed against a different
  line. Highlighted words corresponding to nothing on screen.
- `MarkdownRows` classifies blocks and strips markers per line. A continuation of
  `# heading` is prose, a continuation of a table row shears the grid.
- The gutter shows both line numbers and they have to keep adding up.
  `docs/extending.md` calls this "row count is not yours to change" and a test
  asserts `MarkdownRows` and `TextRows` agree; splitting lines breaks the count
  in a way that looks like working.

## Why the width is measured and not the window's

`views/mod.rs`: *none of them assume they own the window*. A zero-height
`canvas` reports this view's own box during paint and the reflow happens on the
frame after — so the first frame of a session draws unwrapped, for 16 ms. The
alternative is reading `window.viewport_size()`, which is correct exactly until
there are panes.

This **revisits [0014](0014-layouts-are-a-registry.md)'s "why not per viewport"**,
which said a row cannot be told how wide the window is. It cannot be told during
`render`; it can be told on the next frame, and that is enough. With wrapping on,
`SplitRows` narrows both columns to half the measured width, and the divider is
still one straight line from the first row to the last — because it is one width
for the whole diff, just no longer the widest line's. With wrapping *off* the
column is the widest line again, because a 2000-character line still has to be
reachable.

## Why `seg` fits in the order table

`RowRef` was `{ owner: u16, index: u32 }` in eight bytes with two to spare, so
wrapping cost the order table nothing. The cap is 65,535 rows per line;
`MAX_LINE_CHARS` (2000) over `MIN_WRAP_COLS` (8) is 250.

The previous table is also its own record of the unwrapped shape — consecutive
entries with the same owner and index are one logical row, and an index is unique
within an owner — so a reflow needs no second table to remember what it was
expanding. 8 bytes a row, once, however many times the window is dragged.

## Why a slice per visible row per frame is acceptable

Rows hold `SharedString` precisely so that `render` copies nothing, and a wrapped
row breaks that: it hands `StyledText` a substring, which allocates. It is up to
a window's width of bytes on each of ~50 visible rows, and it sits beside the run
list — also rebuilt per row per frame, for the same reason, and *larger*. A row
that did not wrap still clones the whole line and pays nothing. Recorded here
rather than left silent, as the run list already is in
[../diff-pipeline.md](../diff-pipeline.md).

## Evidence

`./check.sh`, the `diffs` section; `WRAP_COLS=n` on `bench` sets the budget.
**36–52 ns a line**: 0.9–3.0 ms on the real fixtures and 26 ms on the 714k-line
one, against a `prepare` of 6.5–247 ms on the same inputs. So the frame during
which a resize crosses a character boundary is the cost, and every other frame of
the drag is a float comparison.

Rows added is smaller than it looks and depends on the budget, not on the
fixture's size: at 150 columns — a 1440px window — **1.00–1.01×** on three of the
four fixtures and 1.02× on the fourth. At 80 columns, 1.04–1.20×. Code lines are
short; the fixtures that wrap are the prose one and the migration one. Full table
in [../measurements.md](../measurements.md).

## Consequences

**A `Rows` implementation opts in.** `rows()` and `reflow()` both default, so a
presentation written before wrapping existed compiles and behaves identically —
and gets no wrapping. Opting in is a column budget and one call to
`Wrapped::build`; all three built-ins do it that way, and a test asserts the
defaults leave an implementation untouched. This is the weakest point of the
design: an extension gets the hard part free but not the whole thing. Making it
free means wrapping before `build`, which is the `Vec<Line>` shape above and its
three bugs.

**Headers do not wrap.** A file header is a path plus `+N -N`, which is not one
string to slice, and a hunk header is short. They stay one row each, so a deeply
nested path can still overflow — a scrollbar for one row per file instead of for
every line in the diff.

**A proportional face wraps approximately.** The budget comes from
`font.advance`, which is an average when `font.monospaced` is false, so a row can
come out slightly short or slightly over. Same approximation
`with_width_from_item` and the Markdown table padding already make.

**Word wrap drops the whitespace it breaks on.** So the range partition of a line
has holes, and only whitespace may fall in one — there is a test. A run reaching
the end of the line is deliberately left on the last row rather than broken
before, because breaking there draws a row of nothing; `width()` trims before
measuring so it costs no scroll width.

**The overlay's row count had to become live.** Wrapping changes how many rows
exist on every resize, and a number taken at load describes the diff as it was
one window ago.
