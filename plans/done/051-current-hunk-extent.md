# Plan 051: The current hunk shows its extent, and the armed tint covers it

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update this plan's row in
> `plans/high-priority/README.md`.
>
> **Drift check (run first)**: `grep -n "fn row_bar" shell/src/views/diff.rs`
> must hit (the design pass is on your base). Line refs were taken at
> `00842dc` + the staged design pass; match on quoted content where a ref
> drifted; STOP on a structural mismatch. Plans 031 and 038 shaped this
> region — read their landed forms before starting.
>
> **Build cost**: `export CARGO_TARGET_DIR=/tmp/gitten-target`. Never launch
> `./dev desktop` or `./dev tui`. `./dev dump` is the eyes.
>
> **Note (cross-plan)**: plan 057 (diff scrollbar hunk ticks) also touches
> `diff.rs`; the README sequences you.

## Status

- **Priority**: P2
- **Effort**: M
- **Risk**: MED (render-path code; per-row cost rules apply)
- **Depends on**: none (031/038 are on the base; verify, don't assume)
- **Category**: UX — "what will `space` stage" is answered by a counter,
  not by the screen

## Why this matters

Hunk staging acts on *the hunk the cursor is in* (`diff.stage-hunk` /
`unstage-hunk` / `discard-hunk`, `shell/src/main.rs:1782-1884` vicinity),
and the only on-screen answer to "which lines is that" is `hunk 3/5` in the
pane header. The hunk's extent is never marked in the body: a hunk that
starts above the viewport stages lines the user cannot see and was never
shown. Two adjacent defects in the same region:

1. **Header rows fall out of the cursor vocabulary.** Line rows draw the
   2px cursor bar via `row_bar` (`shell/src/views/diff.rs:2950`); file and
   hunk header rows take only the background wash (`file_header`,
   `diff.rs:2717`, has no bar parameter), so the cursor looks subtly
   different parked on a header than on a line.
2. **The armed tint may cover less than the blast radius.** Whatever 031/038
   landed, verify: arm a discard (`D`), and check whether every row of the
   doomed hunk carries `RowState.armed`, or only the row the cursor sat on.
   The review at `00842dc` found the armed key taken from the cursor's
   single logical row (`armed_at`, `diff.rs:~2960`). The second press
   destroys the *hunk*; the tint must say so.

After this plan: every row of the cursor's hunk carries a quiet extent mark
in the gutter; header rows join the cursor-bar rule; the armed tint spans
the armed hunk's rows exactly.

## Current state

- `RowState { current, focused, armed }` is threaded through `Rows::render`
  (plan 031's seam; the trait is a documented extension point in
  `docs/extending.md` — do **not** change its signature again; anything new
  rides in `RowState` or in the presentations' own lookups).
- `hunk_at(index) -> Option<(&str, usize)>` exists on the `Rows` trait
  (`diff.rs:303` default, `diff.rs:2375` on `TextRows`) — the per-row "which
  hunk am I in" answer the extent mark needs.
- `row_bar(state, base, theme)` (`diff.rs:2950`) — the line rows' bar.
- House rules that bind the drawing (CLAUDE.md): nothing on the render path
  allocates per frame; a hairline carries an edge where a tint cannot
  (`chrome.border` / `diff.rule` exist because near-black tints don't read);
  the moved-flag rule — never grow `LineKind`, flag beside it.
- Three presentations implement `Rows` (`TextRows`, `SplitRows`,
  `MarkdownRows`) plus three test-local implementors; the extent mark is
  shared *logic* (which rows are in the cursor's hunk) with per-presentation
  *drawing*, the same split `row_bar` already has.

## Commands you will need

| Purpose | Command | Expected |
|---|---|---|
| Shell tests | `cargo test -q -p gitten-shell` | exit 0 |
| Everything | `./dev check` | exit 0 |
| One frame | `COLS=100 ./dev dump diff . 2>/dev/null \| head -30` | a diff |

## Scope

**In scope**: `shell/src/views/diff.rs`, `split.rs`, `markdown.rs` (drawing
+ the shared extent computation); `shell/src/main.rs` only if the armed key
needs re-shaping; `docs/extending.md` only if `RowState` gains a field
(field addition, not signature change).

**Out of scope**: the hunk header's own styling (its bg/`readable` call is a
known ad-hoc site owned by a 031 follow-up note); the scrollbar (plan 057);
stage/unstage verb behaviour; any `core` change — hunk extents are already
a `Rows` fact.

## Git workflow

- Branch: `advisor/ui-051-hunk-extent`
- Commits per step, e.g. `shell: the cursor's hunk says where it starts and
  ends`
- No push, no PR, unless the operator instructed it.

## Steps

### Step 1: Verify what 031/038 actually landed

Read the landed `RowState` computation in `Diff::render` (how `armed` is
derived per row) and `armed_at`'s key shape. Answer in the commit message:
does the armed tint already span the hunk? If yes, Step 3 shrinks to a
regression test; say so and move on.

### Step 2: The extent mark

Compute once per frame (not per row): the cursor row's hunk key via
`hunk_at(cursor)`. A row is *in extent* when its `hunk_at` equals that key.
Add `in_hunk: bool` to `RowState` (a field addition — existing implementors
gain a `..Default::default()`-compatible field; update the three real and
three test-local implementors and the `docs/extending.md` excerpt).

Draw it as a 1px vertical hairline at the gutter's left edge (inside the
2px bar's column, in the row's own background when not in extent — the
`list_row` trick, so nothing shifts), in `diff.rule`'s ink: a hairline,
because the row backgrounds already spend their tint saying add/remove and
a second tint would compete (the two-floors note in CLAUDE.md). On the
cursor row itself the 2px bar wins; the hairline continues above and below
through every row of the hunk, including its hunk-header row.

Per-row cost: one `u16`/`u32` compare against a precomputed key — no
allocation, no string compare per row (intern the key comparison; if
`hunk_at` returns `(&str, usize)`, compare the `usize` and the file index,
not the `&str`, or precompute the row range).

**Verify**: `cargo test -q -p gitten-shell` → exit 0;
`COLS=100 ./dev dump diff . 2>/dev/null | head -30` unchanged shape.

### Step 3: The armed tint spans the hunk; headers join the bar

- If Step 1 found the armed tint keyed to one row: re-key `armed` so every
  row whose `hunk_at` matches the armed key carries it (the same compare as
  Step 2 against the armed hunk instead of the cursor's).
- Give `file_header` and the hunk-header row the `RowState`-driven bar the
  line rows have (`row_bar` applied to their frames), so the cursor is one
  shape everywhere.

**Verify**: `cargo test -q -p gitten-shell` → exit 0.

### Step 4: Tests + gate

- `every_row_of_the_cursors_hunk_reports_the_extent` (drive the per-row
  computation over a two-hunk fixture; assert the range and its boundaries).
- `the_armed_tint_covers_exactly_the_armed_hunk` (arm via the discard verb,
  assert the flagged row range; assert a cursor move disarms — the existing
  rule).
- `a_header_row_carries_the_cursor_bar` (call the header render fn with
  `current: true`, assert the bar ink).

**Verify**: `./dev check` → exit 0.

## Done criteria

- [ ] `./dev check` exits 0
- [ ] `RowState` carries the extent; all six implementors compile without a
      second trait-signature change
- [ ] The three tests above pass
- [ ] Per-row extent cost is one integer compare (no per-row string ops —
      cite the line in the report)
- [ ] `docs/extending.md`'s `RowState` excerpt is current
- [ ] No files outside the in-scope list modified (`git status`)
- [ ] `plans/high-priority/README.md` row updated

## STOP conditions

- `hunk_at`'s return shape forces a per-row string comparison and no cheap
  precomputed range exists — report the shape; the fix may belong in the
  flatten, which is `core`'s and out of scope.
- 031's landed form differs so much from this plan's assumptions that
  `RowState`/`row_bar` don't exist under these names — re-read, then report
  with the actual names before adapting.
- The hairline is invisible at 1px against every diff background in the
  shipped themes (check `diff.rule`'s value first; if it was tuned for the
  table rule only, adding a floor is plan 034/040 territory — report, don't
  retune).
