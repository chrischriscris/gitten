# 0016 — Three more kinds of diff, none of them an algorithm

**Status** accepted
**Date** 2026-08

## Context

[0013](0013-differs-in-core-not-a-dependency.md) made the algorithm a seam and
shipped three implementations. The obvious next question — "what other kinds of
diff are there?" — has an answer in three parts, and only one of them is another
`Differ`.

## Decision

None of the three is an algorithm. All three are shared machinery beside the
trait, so they compose with every implementation including one an extension
compiles in.

**`Whitespace`** — `exact`, `trailing`, `change`, `all`, matching git's default,
`--ignore-space-at-eol`, `-b` and `-w`. A knob on `Differs`, not three more
implementations.

**`differ::moves`** — a block deleted here and added there is flagged `moved` on
the line. A post-pass over the finished script.

**`differ::compact`** — git's `--indent-heuristic`, ported. On by default, as it
is in git.

## Why whitespace is a knob and not an algorithm

It is a different *equivalence relation* on lines, not a different way of
matching them. As implementations it would have been `histogram-ignore-ws`,
`myers-ignore-ws`, `patience-ignore-ws` — and nothing for an extension's differ,
which is rule 1 failing on the axis this is about.

The trick that makes the knob work: normalising is **per line and
length-preserving**, so the edit script computed over the keys still addresses
the original lines. `hunks` is handed the real text, the implementation never
learns that normalisation happened, and the whole thing is four lines in
`file_using`.

One visible consequence, and it is git's too: a line whose only change was
whitespace becomes context, and context is printed from the *old* file. So its
`new_no` points at bytes that differ from what is on screen. A test pins that,
because it looks like a bug until you know.

## Why `moved` is a flag and not a fourth `LineKind`

A moved line is still an addition or a removal. `align`, `replace_pairs`, the
adds/dels counts and every run-detection loop in the codebase reason about runs
of `Removed` then `Added`, and a fourth variant would have broken all of them for
a property only the drawing cares about.

The drawing swaps the background — `diff.moved_added_bg`, `diff.moved_removed_bg`,
blue-grey rather than a paler green and red, because a moved block has to *recede*
from the change hues to be skippable — and leaves the `+` and `-` alone so the
sign column still scans. That is git's `--color-moved` design and it is proven.

Two new `Surface` variants come with it, because a token's foreground has to be
resolved against the background it actually lands on; that is the entire reason
`Surface` exists ([0009](0009-contrast-resolution.md)).

Three rules keep detection honest, each of which was a bug first:

- **Only lines the script touched.** Indexing the whole file makes every repeated
  line in an unchanged region a move.
- **Three lines minimum.** Two matching lines are a coincidence; `}` and a blank
  line are everywhere. Reporting them costs the feature its whole value.
- **A landing is claimed once.** A block deleted once and added twice otherwise
  marks both, and the moved-line count exceeds the number of lines that exist.

## Why the indent heuristic was ported and not approximated

The readable version of a slide is not a matter of taste that can be
approximated: an approximation produces hunk boundaries that differ from git's in
a way no test can call right or wrong. Porting xdiff's weights — names and values
— means the output is *checkable*.

It was worth it twice over, because the first attempt was wrong in a way only that
check could find. git keeps a position's badness as two numbers, `effective_indent`
and `penalty`, and compares the first with a **three-way compare weighted by
`INDENT_WEIGHT`**. Folding it into one score as `INDENT_WEIGHT * indent` reads
like the same arithmetic and is not: a position one column further left then wins
by as much as one a hundred columns further left. It slid hunks to plausible
places git does not put them, and **every changed-line count was identical**.

That is what made `diffcheck` compare hunk *positions* and not only counts. Five
of its six rows now match git's positions exactly on every input.

Two more traps, both found the same way:

- **Ties go to the later position.** git's comparison is `<=`, ties are common,
  and the two answers are visibly different.
- **Whether a group can slide is the whitespace relation's question; how readable
  the result is, is the text's.** Equality has to use the keys, or `-b` and `-w`
  cannot cross a reindented line where git can. Scoring has to use the text,
  because the keys are what erased the indentation. Using the text for both cost
  two hunks in cmux's history — again with identical line counts.

## Why not semantic diffs

Asked at the same time, and it has two separate blockers.

*Where the code lives* is solved: tree-sitter cannot go in `core`, so it would be
a crate implementing `gitten_core::differ::Differ`, which is what the seam is for.
[0003](0003-scanner-over-tree-sitter.md) measured tree-sitter and rejected it for
highlighting — 7.1 MB/s against the scanner's 104–262, falling to 2.6 MB/s on
hunk-shaped input while losing a fifth of its spans to error recovery, plus
~1.55 MB of engine and 0.21–3.30 MB per grammar.

*What it can say* is not solved, and that is the real blocker. `Edit` is line
ranges, and a tree diff's value is sub-line: "this argument was added". Squashing
that to whole lines discards the thing a parser was bought for. The pipeline has
the resolution already — `Span`, from the intraline pass — but it is computed
independently in stage 3b, so a `Differ` cannot emit one. Semantic needs the
*seam* widened, not another implementation behind it.

## Evidence

[../measurements.md](../measurements.md), the `differs vs git` table. Counts and
positions across four repositories and six git invocations. Five of the six rows
match git's hunk positions exactly on every input; the sixth is `myers`, which
matches every count and places 1–4 hunks per input elsewhere because a minimal
script has one length and not one shape.

Cost: ignoring whitespace is 2–4× the exact relation, all of it the per-line
`String` per side — 108 ms against 28 ms on cmux's 307k lines, paid on a click.
Hashing the keys instead of materialising them is the fix if that changes.

## Consequences

**`whitespace` gets a title-bar picker; `moves` and `indent_heuristic` do not.**
The line is which of them you change mid-review. `-w` is the flag people reach for
constantly; the other two are set once. Four dropdowns in a 32-pixel strip is
worse than three.

**Move detection is within one file.** A block cut from one file and pasted into
another is two unrelated changes. git has the same default.

**A replace never slides.** git slides those per file through machinery this does
not have. Their boundaries are pinned on both sides anyway, so the case is rare —
but it is a real difference from git and is where to look if a position ever
disagrees.
