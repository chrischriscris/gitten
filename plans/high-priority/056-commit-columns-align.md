# Plan 056: The commit columns align

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving on. On
> any STOP condition, stop and report — do not improvise. When done, update
> this plan's row in the high-priority index.
>
> **Drift check (run first)**:
> `git diff --stat 635aba8..HEAD -- shell/src/graph.rs shell/src/views/commits.rs`
> On any in-scope drift, compare the "Current state" excerpts against the
> live code; on a mismatch, STOP.
>
> **Base**: `git switch -c advisor/ui-056-commit-columns origin/full/full`
> (`635aba8`). Line numbers are against that commit; match on quoted content.
>
> **Shared ground rules**: see the README in this directory.

## Status

- **Priority**: P2
- **Effort**: S

## Why this matters

A column of anything is aligned or it is not a column. The commit row's
graph canvas is sized per row — `row_width(d) = d.lanes * LANE_W + GAP`
(`shell/src/graph.rs:88-89`) — so the sha, the author initials and the
subject start at a different x on every row whose lane count differs from
its neighbour's. The sha cell is fixed-width and the initials cell is
`WHO_CHARS` wide *precisely so they align vertically*, and the per-row
canvas throws that away: the one thing the eye does with a sha column is
run down it (the codebase's own words about number columns).

The repo already caps lanes at `MAX_LANES = 12` (`graph.rs:83` clamps to
it), so the fix cannot reintroduce the 280-lane gutter problem: the widest
possible gutter is 12 lanes.

There is a comment near the row builder in `commits.rs` (the `row_width`
call site, ~`commits.rs:768` at an earlier base) explaining why the width
is per-row — **read it first**. If its reason is per-row paint cost, the
fix below preserves that (the canvas still paints only its own lanes). If
its reason is something else, STOP and report the reason instead of
overruling it.

## Current state

- `graph.rs:88-89`:

  ```rust
  pub fn row_width(d: &Draw) -> f32 {
      d.lanes as f32 * LANE_W + GAP
  }
  ```

- `commits.rs` calls it per row to size the canvas; the text cells follow
  the canvas.
- The commits load pass already walks every row once (lane assignment), so
  a max-lanes-in-view or max-lanes-loaded figure is available without a new
  pass — the same idiom as "compute the widest index at load" from the GPUI
  notes.

## Scope

**In scope**: `shell/src/graph.rs` (or its caller) — one shared gutter
width; `shell/src/views/commits.rs` — use it; tests.

**Out of scope**: lane assignment, colours, the 12-lane cap, the graph's
drawing itself, any other pane.

## Git workflow

Branch `advisor/ui-056-commit-columns` from `origin/full/full`.

## Steps

### Step 1: One gutter width per load

At load (not per frame), compute `gutter_lanes = max lanes across loaded
rows, clamped to MAX_LANES`, store it beside the rows, and size every row's
canvas box to `gutter_lanes * LANE_W + GAP`. The canvas *paints* exactly
what it painted before — only its box widens; lanes the row does not have
are empty gutter, which is what a graph gutter is. Filtered projections
(`/` search) may keep the full set's width — a stable gutter across a
filter toggle is calmer than a reflow, and cheaper; note the choice in a
comment.

**Verify**: a unit test — rows with lanes [1, 3, 2] all report the same
row width, equal to the 3-lane width; a 300-lane synthetic row still clamps
to `MAX_LANES`.

### Step 2: The columns prove it

Extend an existing commits view test (or add one) asserting the x-offset
invariant the cells were built for: with mixed lane counts, the sha cell's
start offset is identical across rows. If offsets aren't testable in the
harness, assert `row_width` equality — that is the invariant carrier.

**Verify**: `cargo test -p gitten-shell` green; `./dev check` green.

## Test plan

As embedded in the steps; nothing else moves.

## Done criteria

- Sha, initials and subject start at one x for every visible commit row.
- Worst case gutter is unchanged (12 lanes); typical repos pay only their
  own max.
- No per-frame computation added; the width is a load-time fact.

## STOP conditions

- The `commits.rs` comment justifies per-row width for a reason this plan
  does not answer — report the reason verbatim.
- The width figure is not reachable at load without a second pass over the
  rows — report where lane counts become known.
- Drift check fails.
