# 0002 — Cap the graph gutter at 12 lanes

**Status** accepted
**Date** 2026-08

## Context

git/git runs up to 280 concurrent lanes. At `LANE_W = 14px` that is a 3,920px
gutter, which pushes the commit subject entirely off screen.

## Decision

`MAX_LANES = 12`, hard. Lanes past the cap collapse onto the last column in
`theme.lane_overflow` grey, and collapse in the data too.

## Why not draw them all

Because nobody reads them. git's own `--graph` is unreadable well before a dozen
lanes, and the cost is real: 280 lanes means 280 quads per row queued for a column
of pixels a reader cannot decode anyway.

## Why not scale the lane width down

Sub-pixel lanes stop being crisp — `STROKE = 2` exists so a lane centre lands on
7 and its edges on 6 and 8, sharp at any scale factor — and 280 hairlines is not
more legible than 12 lanes and a grey column.

## Evidence

git/git: p50 126 lanes, p99 226, max 280, only 0.9% of rows at a single lane. See
[../measurements.md](../measurements.md).

## Consequences

Overflow is visible rather than silently misdrawn, but topology past the cap is
genuinely not shown. A reader with a 280-lane repository sees "there is more over
here" and has to go elsewhere for the detail.
