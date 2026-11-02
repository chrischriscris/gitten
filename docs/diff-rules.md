# Diff rules

The rules for the diff path — differs, alignment, layouts and wrapping — that
are easy to get wrong in the plausible direction: each one produces something
that still looks like a working diff. They lived in `AGENTS.md` until it grew
past what every task should have to read; they apply to any change under
`core/src/{differ,align,wrap,markdown,rows}.rs`, the diff views in `gui/` and
`tui/`, and `git/`'s acquisition. How the pipeline fits together is
[diff-pipeline.md](diff-pipeline.md); this page is what not to break in it.

## Diffs

Histogram — not Myers. Histogram anchors on lines appearing exactly once, so
function signatures become anchors and a moved block reads as a move instead of
dissolving into line-soup. Git defaults to Myers. We deliberately don't.

Diffing is a `trait Differ` and the algorithms are written out in `core`, not
pulled in — `core` has no dependencies and that is the rule, not housekeeping.
Histogram, patience and Myers are the first three; semantic and language-aware
differs arrive as extensions. The view never calls a differ directly, and an
implementation returns only the edit script: line numbers, context and hunk
headers are shared.

**Check a differ against git, don't argue about it.** `diffcheck` compares
changed-line counts *and every hunk position* against six git invocations over
four repositories. Three bugs so far were invisible in the totals: two showed up
in the per-file deltas, and the third only in the positions — identical counts,
hunks in places git does not put them. Compare the positions.

Two things in histogram are wrong in the plausible direction, so read them before
touching them: a run is scored by its **rarest** line, not its most common one,
and the threshold **tightens** as the search runs. Getting either backwards costs
hundreds of spurious changed lines and still looks like a working diff.

Both algorithms are quadratic in the number of differing lines in the worst case.
Bound them and degrade to "this region was replaced"; a line-for-line pairing of a
generated file is not worth a visible pause and nobody was reading it. Recurse on
an explicit stack — a file whose every anchor peels off one line is as deep as it
is long, and generated code has that shape.

**Everything except the edit script is shared.** Whitespace normalisation, the
indent-heuristic slide, hunk assembly and move detection all live beside the trait
and run for every implementation, including one an extension compiles in. A
`Differ` decides which lines correspond and nothing else.

Ignoring whitespace is an **equivalence relation, not an algorithm** — normalise
per line, length-preserving, and the script still addresses the original lines
while the hunks show the real text. That is what stops `histogram-ignore-ws` from
having to exist.

A **moved** block is deleted here and added there, and it is the one thing in a
diff a reader may skip. Flag it beside `kind`, never as a fourth `LineKind`: it is
still an addition or a removal, and `align`, `replace_pairs` and the adds/dels
counts must not learn about it. Require three lines — two matching lines are a
coincidence and `}` is everywhere.

**Port git's heuristics; don't approximate them.** The indent heuristic's weights
are xdiff's because an approximation produces boundaries no test can call right.
Its one trap: a position's indentation is compared by *sign*, not magnitude.

**Two contrast floors, because reading and glancing are different.** `min_contrast`
3.5 for token text, `min_furniture` 3.0 for line numbers and hunk coordinates —
resolved through the same `readable` against the same `Surface`, because a single
literal drawn on five row backgrounds measured 2.05:1 on context and 1.60:1 on a
moved line. And a *hairline* has no floor at all: `chrome.border` and `diff.rule`
exist because nothing in a near-black palette is more than ~1.2:1 from anything
else, so a tint cannot carry an edge and one pixel can.

**Fence a code block; never indent one.** The renderer knows fences and nothing
else, so a four-space block is prose — emphasis, links and list markers all get
interpreted inside it. `#` was the worst of it: a `# comment` trailing an
indented command read as an `<h1>` until `heading_level` started counting the
indent, which is CommonMark's rule and now shared by the block pass and the token
pass. Applies to `AGENTS.md` and every page in `docs/` too.

Intraline highlighting is a **second pass**: line diff first, then re-diff only the
changed pairs at word level. Words, not characters — char diffs on code are
confetti. Never diff untokenized text; it degrades badly.

**Which removal pairs with which addition is decided once, in `core::align`.** The
intraline pass and the side-by-side layout both read it. Pair differently in a
renderer and you draw a removal beside an addition whose changed words were
computed against another line — highlighting that corresponds to nothing on
screen.

## Layouts

Unified and side-by-side are two entries in a registry, not two branches of a
`render`. A `Layout` is a name and a closure that builds a set of `Rows`; `s`
cycles them and `[diff] layout` picks the one to open in. The registry is
shell-side because a `Rows` implementation returns a UI element; the *name* is on
`Host`, because a name is data.

Adding a presentation must need no edit to the existing ones. `SplitRows` needed
no new trait, no new argument and no change to `TextRows` — that is the only test
of a seam that counts. A registered layout appears in the title-bar dropdown
without being told to; if a new seam needs a control written for it by hand, the
seam is shaped wrong.

Cache diffs by blob OID. They never change. Acquisition yields both OIDs for
exactly this reason; the cache lives in `core::differ`.

## Wrapping

**A wrapped line is more rows, never a taller one.** `uniform_list` is the only
reason 714k rows scroll, and it needs every row the same height. So a wrap returns
byte ranges into a line and the line stays one line — because the edit script, the
hunk numbers, `replace_pairs`, `align`, the spans and the tokens all address
lines, and splitting one in two before those ran pairs a removal with the wrong
addition.

Two things are wrong in the plausible direction. **Word wrap searches backwards
from the column**, so the row is never wider than the budget; forwards overflows
by however long the next word is and looks right on prose. And **the budget is per
line, not per diff** — a bullet, an indent and an 18px heading each cost
characters, and one number for the whole diff is what makes a presentation write
its own wrap.

**A grid is not prose, so it is not broken — it is laid out again.** A Markdown
table wider than the window has its columns *squeezed* and its cells wrapped
inside them, at reflow, because the width is the one part of a table's layout the
load pass cannot know. Breaking a table row at a column instead is the obvious
thing and it shears the grid: the second half of row three lands under the first
half of row four, in a column that means something else. Squeezing is
water-filling and not proportional — a column that already fits keeps what it
wants, and the paragraph beside it gives — and it has a floor: below one character
a column there is nothing honest to draw, so the table is left whole and scrolled
to. `core::markdown::flow_table` does it and `wrap::Budget::At` is how the rows it
decided survive the wrap.

And the rule *between* two rows of that table is **pixels, not glyphs**. Every
horizontal line a table has as text is a line of the file — the `|---|` separator,
redrawn — so an inner rule drawn the same way is a row of the list that no line
produced, with a blank gutter the numbers either side have to skip. A row count is
not a presentation's to change, so anything between two rows is a hairline: 1px,
as wide as the grid and no wider, under the *last* sub-row of a row so a squeezed
cell keeps its sentence.

`off` is an entry in the registry, not a flag beside one — the pickers are a pure
function of a registry, so that is what puts it in the menu for free. Unlike the
layouts this registry *is* on `Host`: a break point is a property of text, so
`core` can hold the implementations, and what the frontend supplies is the column
count. Everything else is shared — the range partition, the validation, the flat
table — so a `Wrap` decides where a line breaks and nothing else.
