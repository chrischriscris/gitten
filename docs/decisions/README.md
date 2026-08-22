# Decisions

One file per decision that would otherwise be re-argued. Numbered in the order
they were taken, never renumbered, never deleted — a decision that turned out
wrong gets a **Superseded by** line and stays, because the reasoning is the point.

Format, deliberately short:

```
# NNNN — title

**Status** accepted | superseded by NNNN
**Date** YYYY-MM

## Context      what forced a choice
## Decision     what we do
## Why not X    the alternatives, and what ruled them out
## Evidence     numbers, with a pointer to measurements.md
## Consequences what this costs, and what would make us revisit
```

Numbers live in [../measurements.md](../measurements.md). A record quotes the one
figure that decided it and links to the rest, so a stale number has one home
rather than nine.

| | |
|---|---|
| [0001](0001-histogram-not-myers.md) | Histogram diff, not Myers |
| [0002](0002-graph-lane-cap.md) | Cap the graph gutter at 12 lanes |
| [0003](0003-scanner-over-tree-sitter.md) | A table-driven scanner, not tree-sitter |
| [0004](0004-markdown-second-highlighter.md) | Markdown as a second `Highlighter` |
| [0005](0005-theme-in-core.md) | Colour is data, and it lives in `core` |
| [0006](0006-row-seam-without-boxing.md) | Row presentation behind a trait, 8 bytes a row |
| [0007](0007-assembly-in-core.md) | Row assembly belongs in `core`, not the view |
| [0008](0008-intraline-similarity-floor.md) | No word highlighting below 0.4 similarity |
| [0009](0009-contrast-resolution.md) | Resolve foregrounds against their background |
| [0010](0010-markdown-rendered-rows.md) | Markdown renders as rows, and the markers come off in `core` |
| 0011 | *reserved* — see below |
| [0012](0012-config-is-data-behaviour-is-not.md) | Config is data, behaviour is not |
| [0013](0013-differs-in-core-not-a-dependency.md) | The diff algorithms are written in `core`, not pulled in |
| [0014](0014-layouts-are-a-registry.md) | Side-by-side is a registered layout, not a second view |
| [0015](0015-title-bar-controls-are-hand-rolled.md) | The title-bar pickers are hand-rolled |
| [0016](0016-whitespace-moves-and-the-indent-heuristic.md) | Three more kinds of diff, none of them an algorithm |
| [0017](0017-wrapping-is-more-rows-not-taller-ones.md) | Wrapping is more rows, not taller ones |
| [0018](0018-selection-is-a-model-not-a-text-element.md) | A selection is a model, not a text element |

**0011 is deliberately absent.** It was a rendered-Markdown reader that was built,
looked at, judged broken and parked in a `git stash` rather than deleted. The
number is held for it so that recovering the stash needs no renumbering — which
this file promises never happens. If the work is abandoned for good, this line is
what should say so.
