# 0008 — No word highlighting below 0.4 similarity

**Status** accepted
**Date** 2026-08

## Context

From a screenshot: a diff hunk with one removed line and three added ones.
`replace_pairs` matches a run of removals to the additions after it *by position*,
so it paired

```
-     - bash cicd/pipeline/run-all-checks.sh;
+     # Collect every check failure before exiting so one bad check does
```

Two unrelated lines. The word diff then marked nearly the whole comment as
"changed", which both reported a rewrite that never happened and dragged the text
under the lighter changed-word background where it stopped being legible.

## Decision

`MIN_INTRALINE_SIMILARITY = 0.4`, as Dice coefficient over tokens
(`2·LCS / (len_a + len_b)`). Below it, the pair gets no word highlighting at all;
the line keeps its add/remove background.

The LCS table is already built, so its corner gives the similarity for free.

## Why not fix the pairing instead

Better pairing is a real improvement and a bigger change — it means matching
removals to additions by content rather than position, i.e. a second diff inside
the hunk. The floor is 6 lines and removes the damage now. Both can be true.

## Why 0.4

Measured, not chosen. Of 9,447 pairs in the zig→rust fixture **none** fall below
0.60, and the lowest is a genuine rewrite (`#define ZIG_DECL` → `#define
RUST_DECL`). In the deletion-heavy fixture 15.6% fall below 0.4 and every one is
junk by inspection — `/**` against `// Historical note: …` at 0.0 similarity.

0.4 sits below every legitimate pair measured and above the noise. See
[../measurements.md](../measurements.md).

## Consequences

Some genuine edits between 0.4 and 0.6 similarity in repositories unlike the
fixtures will lose word highlighting. The line is still marked changed, so the
information loss is small, and the failure direction is the right one: no
highlight is a smaller lie than a wrong highlight.

A test pins the 0.60 case, so raising the floor cannot silently stop highlighting
real renames.

Spans also now close over whitespace-only gaps, from the same screenshot: the LCS
matches the spaces between changed words, and each match punched a hole in the
highlight, so a rewritten sentence drew as a row of separate blocks.
