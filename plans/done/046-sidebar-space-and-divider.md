# Plan 046: The sidebar spends its pixels where the user is

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update this plan's row in
> `plans/high-priority/README.md`.
>
> **Drift check (run first)**: `grep -n "fn row_bar" shell/src/views/diff.rs`
> must hit. Line refs were taken at `00842dc` + the staged design pass; match
> on quoted content where a ref drifted; STOP on a structural mismatch.
>
> **Build cost**: `export CARGO_TARGET_DIR=/tmp/gitten-target`. Never launch
> `./dev desktop` or `./dev tui`.

## Status

- **Priority**: P1
- **Effort**: L
- **Risk**: MED (layout arithmetic; one-frame-late measurement)
- **Depends on**: none
- **Category**: UX — fixed space allocation fights the user three ways

## Why this matters

Three compounding problems, all visible in one screenshot of a real repo:

1. **The pane you are working in gets the leftovers.** Section heights are a
   pure function of row *counts* (`section_height`, `shell/src/main.rs:114`),
   so a BRANCHES list with 16 locals (11 of them machine-named worktree
   branches) takes 16 rows while COMMITS — the heart of a git client — is
   squeezed to 2. Focus changes nothing about the split.
2. **A squeezed section clips a row in half.** Sections take
   `flex_shrink(1.0)` with a `min_h` floor (`main.rs:4272-4277`), so when the
   window is short, flex compresses a section to a height that is not a
   whole-row multiple and the boundary bisects a line of text mid-glyph.
3. **The sidebar/diff split is a constant.** `SIDEBAR_SHARE = 0.32`
   (`shell/src/chrome.rs:39`, applied at `main.rs:4350`) — not draggable, not
   configurable.

After this plan: unfocused list sections cap at a content maximum and the
focused section takes the slack (lazygit's accordion, quieter); a squeezed
section always shows whole rows; and the split is a `gitten.toml` knob plus a
draggable divider that adjusts it live for the session.

## Current state

- `SECTION_MIN_H` (`main.rs:107`) = header + 2 rows; `section_height(rows)`
  (`main.rs:114`) = `HEADER_H + rows * graph::ROW_H`; `section_floor`
  (`main.rs:118-122`) caps the floor at the natural height. Tests at
  `main.rs:9710-9770` pin these — extend, don't fight them.
- Sections: `STACK_TOP` (status/files/branches, `main.rs:158-162`), then
  `commits_section` (flex `flex_basis(0)` + `flex_grow`, `main.rs:~4155`),
  then `STACK_FOOT` (stashes, `main.rs:168`). Each section:
  `flex_shrink(1.0).h(px(section_height(rows))).min_h(px(section_floor(rows)))
  .overflow_hidden()` (`main.rs:4268-4277`).
- The sidebar column is `w(relative(chrome::SIDEBAR_SHARE))` at
  `main.rs:4350`; a test at `main.rs:6060` asserts the share — update it to
  read the new source of truth.
- **GPUI fact (CLAUDE.md)**: a view cannot know its own size during render.
  The established in-repo pattern is a zero-height `canvas` that reports its
  box during paint into a `Cell`, consumed next frame — the diff's wrap
  reflow already works this way ("correct and one frame late").
- Config lives in `gitten-app` (`app/src/config.rs`), hot-reloads, and every
  knob has a `./dev config` line. `[view]` already exists (scrollbar,
  scroll).

## Commands you will need

| Purpose | Command | Expected |
|---|---|---|
| Shell tests | `cargo test -q -p gitten-shell` | exit 0 |
| Config tests | `cargo test -q -p gitten-app` | exit 0 |
| Everything | `./dev check` | exit 0 |
| A complete config | `./dev config \| grep -A3 "\[view\]"` | shows the new knob |

## Scope

**In scope**: `shell/src/main.rs`, `shell/src/chrome.rs` (the share constant
becomes a default), `app/src/config.rs` (+ its `./dev config` template),
`docs/` only where a doc states the 0.32 share.

**Out of scope**: pane *collapse* (hiding sections); persisting the dragged
share back into `gitten.toml` (config is read-only by design — the knob sets
the opening share, the drag adjusts the session); the min-window-size clip
(plan 053 item 8); any change to `uniform_list` row heights.

## Git workflow

- Branch: `advisor/ui-046-sidebar-space`
- Commits per step, e.g. `shell: a squeezed section shows whole rows`
- No push, no PR, unless the operator instructed it.

## Steps

### Step 1: Cap unfocused sections; the focused one takes the slack

Introduce `SECTION_MAX_ROWS: usize = 8` (a named constant with a doc comment
in the house register: nobody reads sixteen branches while committing; the
focused pane is the one being read). In the section builders:

- An **unfocused** list section's height uses
  `section_height(rows.min(SECTION_MAX_ROWS))`.
- The **focused** section keeps its natural `section_height(rows)` (still
  bounded by the flex squeeze below).
- COMMITS keeps `flex_grow` as the flexible middle; when COMMITS is focused
  nothing changes (it already grows).

The focus change already `cx.notify()`s, so heights re-resolve on the next
frame for free. Scroll positions must survive the resize — the views'
viewport/reconcile model already handles a height change; confirm with the
existing reconcile tests.

**Verify**: `cargo test -q -p gitten-shell` → exit 0. Extend the
`section_height`/`section_floor` test module with the cap rule
(sentence-named, e.g. `an_unfocused_section_never_exceeds_the_cap`).

### Step 2: Whole rows or nothing

Quantize what a squeezed section shows. The section's *content* wrapper
(the `div().min_h_0().flex_grow(1.0).overflow_hidden()` under the header,
`main.rs:4291-4296`) gets a measured height via the canvas-probe pattern:
paint reports the box; next frame the section rounds the content height
**down** to a whole multiple of `graph::ROW_H` and pads the remainder below
the last full row (background `chrome.bg`), so the boundary always lands
between rows. One frame late is fine — that is the house rule for measured
layout.

Keep the arithmetic in one function (`fn quantized(content_h: f32) -> f32`)
with unit tests; the probe wiring stays thin.

**Verify**: `cargo test -q -p gitten-shell` → exit 0; new test
`a_squeezed_section_shows_only_whole_rows` on the pure function.

### Step 3: The share is a knob with a default

- `chrome::SIDEBAR_SHARE` stays as the *default*.
- Add `[view] sidebar = 0.32` to `app/src/config.rs` (clamped to a sane band,
  say `0.20..=0.50` — clamp in the parser, the way other knobs validate) and
  to the `./dev config` template with a one-line comment.
- `main.rs:4350` reads the config value; the test at `main.rs:6060` reads the
  default through the config path.

**Verify**: `cargo test -q -p gitten-app && cargo test -q -p gitten-shell` →
exit 0. `./dev config | grep sidebar` → the line with its comment.

### Step 4: The divider drags

A 5px-wide hit strip straddling the sidebar/diff border (the border itself
stays the 1px `c.border` hairline — a hairline carries the edge, the hit
strip is invisible): `.id("divider")`, `cursor_col_resize()`, and a drag that
writes a session-local share (a `Cell<f32>` on `DevShell`, initialized from
the config knob, same clamp). `on_drag`/drag-move per GPUI's
`StatefulInteractiveElement`; re-render per frame is a relative-width write,
no reflow code of ours.

**Verify**: `cargo test -q -p gitten-shell` → exit 0. `./dev check` → exit 0.

## Done criteria

- [ ] `./dev check` exits 0
- [ ] Unfocused sections cap at `SECTION_MAX_ROWS`; the focused section is
      uncapped (tests prove both)
- [ ] The quantize function is unit-tested; no section boundary can bisect a
      row
- [ ] `./dev config` emits `[view] sidebar` with a comment
- [ ] Dragging state is session-only; the config file is never written
- [ ] No files outside the in-scope list modified (`git status`)
- [ ] `plans/high-priority/README.md` row updated

## STOP conditions

- The canvas-probe pattern cannot reach the section content wrapper without
  restructuring the section builders into entities — report the shape first.
- Step 1's cap makes any existing reconcile/scroll test fail in a way one
  clamp does not fix — the viewport model may assume content-height sections.
- GPUI's drag API requires an element to own app-level state in a way
  `cx.listener` + a `Cell` cannot express (check how the diff's scrollbar
  drag is wired first — `views/mod.rs` `DeferredScrollbar` is the exemplar).
