# Plan 057: Hunk ticks on the diff scrollbar

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving on. On
> any STOP condition, stop and report — do not improvise. When done, update
> this plan's row in the high-priority index.
>
> **Drift check (run first)**:
> `git diff --stat 635aba8..HEAD -- shell/src/views/mod.rs shell/src/views/diff.rs core/`
> On any in-scope drift, compare the "Current state" excerpts against the
> live code; on a mismatch, STOP. **Plan 051 (pass 9) works in `diff.rs`**
> — if it has landed, re-read the hunk data structures before starting.
>
> **Base**: `git switch -c advisor/ui-057-hunk-ticks origin/full/full`
> (`635aba8`). Line numbers are against that commit; match on quoted content.
>
> **Shared ground rules**: see the README in this directory. The palette
> note matters here: no new theme fields — `diff.rule` and the existing
> hunk/furniture inks are the ink budget.

## Status

- **Priority**: P2
- **Effort**: M (Step 1 is an investigation with an honest bail-out)

## Why this matters

The diff header answers "which hunk am I in" (`hunk 3/5`) but nothing
answers "where are the other four". In a 714k-row fixture — or just a 108-
line hunk span — the scrollbar is the only spatial summary of the file, and
today it summarizes nothing. Marks on the track at each hunk's offset turn
the bar into a map: five ticks, one glance, and `space`-stage-next-hunk
stops being navigation by faith. Every serious diff surface (VS Code,
IntelliJ) puts change marks on the scroll track for exactly this reason.

The same seam, once built, is what search-match marks ride on later — build
it as *marks at normalized offsets*, not as *hunks on a scrollbar*.

## Current state

- The scrollbar is already ours: `DeferredScrollbar` in
  `shell/src/views/mod.rs:94-108` implements `gpui_component`'s public
  `ScrollbarHandle` trait ("four methods over your own `Cell`", per the
  GPUI notes) — the widget draws the track and thumb and drags for us.
- The diff view owns a flat row order; hunk boundaries are known to it (the
  header's `hunk i/n` is computed from the cursor row against hunk starts —
  find `FileSummary`/the hunk-count source in `diff.rs` and reuse its
  input, do not recompute hunks).
- Row count is uniform (`uniform_list`), so a row index normalizes to a
  track offset by plain division — no measurement.

## Scope

**In scope**: `shell/src/views/mod.rs` (the mark-painting device),
`shell/src/views/diff.rs` (supplying `Vec` of normalized mark offsets at
load), tests.

**Out of scope**: minimap, search marks themselves (only the seam), any
`gpui_component` fork, sidebar scrollbars, colors beyond existing palette
fields.

## Git workflow

Branch `advisor/ui-057-hunk-ticks` from `origin/full/full`.

## Steps

### Step 1: Find the paint seam (investigation, timeboxed)

Determine whether marks can be painted with, not against, the widget:

a. Does `gpui_component`'s scrollbar expose any decoration/child hook on
   the track? (Read its source at the locked commit — `Cargo.lock` holds
   the pin; do not `cargo update`.)
b. If not: the track's geometry is knowable (the diff pane's box minus the
   track width used at the call site), so a zero-width overlay strip drawn
   by *our* code, aligned to the track and painted before it (so the thumb
   rides above the ticks), does not touch the widget at all. `canvas()` +
   `paint_path` per the custom-drawing note; `deferred` is not needed if it
   is a sibling painted in order.

Pick (a) if it exists, else (b). **If neither can align to the real track
geometry without guessing pixel constants from the widget's internals,
STOP and report** — a tick strip that drifts from the thumb is worse than
none.

### Step 2: Marks as data

The diff view computes, at load/prepare time (never per frame), a
`Vec<f32>` of normalized offsets — `hunk_start_row / total_rows` — for the
current file order. Cache it beside the order; it changes only when the
order does. The type is *marks*, not *hunks*: the painter takes offsets
and an ink.

**Verify**: unit test — a synthetic order with hunks at known rows yields
the expected normalized offsets; an empty diff yields none; a one-row file
does not divide by zero.

### Step 3: Paint

Ticks are furniture: 1px-to-2px tall marks, full track width, in an
existing furniture ink that clears `min_furniture` against the track
(compute with the repo's own `contrast()`; the hunk-header ink and
`diff.rule` are the candidates — pick by measurement, not taste, and note
the measured ratios in the commit). The cursor's own hunk is not
special-cased in this plan (that is presentation polish; the seam first).

**Verify**: `cargo test -p gitten-shell`; `./dev check` green. Note in the
report which ink won and its measured ratio on the track color.

### Step 4: The seam check

A second caller must be possible without touching the painter: write a
doc comment on the mark-painting device stating its contract (offsets +
ink), and a test that feeds it two mark sets. That is the whole
extensibility ask — no registry needed yet.

## Test plan

- Offset math tests (Step 2).
- Contract test on the painter (Step 4).
- Full gate.

## Done criteria

- Every hunk in the current diff shows a tick at its position on the diff
  scrollbar track; ticks land where the thumb lands when scrolled to that
  hunk.
- Mark computation is load-time; zero per-frame allocation (rule 3).
- Painter takes generic offsets — search marks later are a new caller, not
  a new painter.

## STOP conditions

- Step 1 finds no honest alignment to the widget's track — report both
  paths' blockers.
- Hunk starts are not available to the view without recomputation — report
  where they live.
- Drift check fails / plan 051 has restructured the hunk data.
