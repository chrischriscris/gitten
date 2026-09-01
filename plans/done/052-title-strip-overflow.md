# Plan 052: The title bar survives a narrow window

> Adopted from draft `hp-03` (authored by session full-44 against
> `635aba8`); renumbered into this pass. The pack's shared base note applies:
> the owner commits the staged design pass first, and `shell/src/main.rs` is
> among the staged files — expect the drift check below to report drift, and
> match on quoted content.

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving on. On
> any STOP condition, stop and report — do not improvise. When done, update
> this plan's row in `plans/high-priority/README.md`.
>
> **Drift check (run first)**:
> `git diff --stat 635aba8..HEAD -- shell/src/main.rs shell/src/controls.rs shell/src/chrome.rs`
> On any in-scope drift, compare the "Current state" excerpts against the
> live code; on a mismatch, STOP. **Pass 8's plan 039 rewrites
> `controls.rs` dispatch** — if `advisor/039-*` has landed, rebase your
> reading on the result before starting.
>
> **Base**: the commit the operator names (the design pass, once committed).
> Branch: `git switch -c advisor/ui-052-titlebar-narrow <base>`. Line numbers
> are against `635aba8`; match on quoted content.
>
> **Shared ground rules**: see `plans/high-priority/README.md`.

## Status

- **Priority**: P1
- **Effort**: M

## Why this matters

The window's declared minimum is 560×320 (`shell/src/main.rs:5458`), and at
that width the title bar cannot hold what it always draws: 72px of traffic
light inset (`LIGHTS_W`, `main.rs:100`), the repo path, the branch chip, the
debug badge, and up to five pickers — every one of them `flex_none`. Only
the repo path can shrink, and it clips *hard*: the no-repo fallback uses
`.text_ellipsis_start()` but the real path does not. So at any width the
constants don't happen to fit, pickers march off the right edge with no cue
— the exact failure the status bar's hint budget was built to prevent, one
bar up.

Two fixes: the path ellipsizes like its own fallback already does, and the
pickers degrade in two steps as the window narrows — labels drop first,
then the five collapse into one.

## Current state

- Title bar assembly: `main.rs` render, the strip beginning near `4349`
  (`.flex_none()` on the bar) — repo path with `flex_shrink(1.0).min_w_0()
  .overflow_hidden()` but no ellipsis; branch chip; debug badge; a
  `flex_grow` spacer; then the pickers.
- Pickers: `shell/src/controls.rs` — trigger is label (`c.dim`) + value
  (`c.fg`) + caret, `flex_none` (`controls.rs:150`, `186`), width derived
  from content. The picker is a pure function of a registry list and an
  index — the property this plan leans on.
- Which pickers show: theme always; layout / wrap / algorithm / whitespace
  only when the main screen is a diff (the `strip` builder in `main.rs`).
- The status bar already solves this class of problem with a character
  budget (`chrome::hints_budget`, `chrome.rs:468`, called at
  `main.rs:4737`) — width arithmetic from `char_width`, not measurement.

## Scope

**In scope**: `shell/src/main.rs` (title strip assembly + tests),
`shell/src/controls.rs` (a label-less trigger variant; a composed picker),
`shell/src/chrome.rs` (budget helper if shared).

**Out of scope**: the picker *menu* rendering, keyboard access to pickers
(that is pass 8's plan 039), the branch chip's content, `window_min_size`
itself, any settings-panel work.

## Git workflow

Branch `advisor/ui-052-titlebar-narrow` from the operator-named base.

## Steps

### Step 1: The path ellipsizes at the start

Add `.text_ellipsis_start()` to the repo-path container, matching the
no-repo fallback a few lines below it. Start-ellipsis, because the *name* —
the last segment — is the part being scanned; the parent is the part to
sacrifice.

**Verify**: build; the fallback and the path now share the same truncation
call (cite both lines in the commit message).

### Step 2: A width budget for the strip

Compute, in characters × `char_width` like the status bar does, what the
strip's right side costs: each picker is `label + value + caret + padding`
chars. Two named thresholds, derived from the budget rather than guessed:

- Below **T1** (the width where five full pickers no longer fit beside a
  40-char path): render every picker trigger *value-only* (`unified ▾`
  instead of `layout unified ▾`). The value is the information; the label
  is recoverable from the open menu.
- Below **T2** (where even value-only triggers do not fit): render one
  trigger — `view ▾` — whose menu is the five pickers' entries as sections.
  The picker is a pure function of a list and an index, so compose: one
  menu, five labeled groups, same dispatch per entry as the standalone
  pickers. No new state machine — selecting an entry does exactly what the
  standalone picker's entry did.

The viewport width is not knowable during `render` from inside a view — but
the title strip is built by the root `DevShell`, which owns the window; use
the same source the status bar's budget uses (`main.rs:4737` vicinity) so
the two bars can never disagree about the window's width.

**Verify**: unit tests on the budget arithmetic: given a path length and
picker set, the tier chosen at 560px is T2, at ~900px T1 or full depending
on the real character counts (compute them in the test, don't hardcode),
at 1400px full. `cargo test -p gitten-shell` green.

### Step 3: Full gate

`./dev check`. Hand the owner `./dev desktop` and suggest dragging the
window to 560px.

## Test plan

- Budget-tier unit tests (Step 2).
- A test that the composed `view ▾` menu contains every entry of the five
  standalone pickers, generated from the same registries — the seam test:
  a sixth registered picker must appear in the composed menu without an
  edit to it.
- `./dev check` green.

## Done criteria

- The repo path start-ellipsizes.
- At no window width ≥ 560px does any title-bar element paint past the
  window edge or under another element.
- The degradation is two named tiers driven by one budget computation, and
  a new registered picker participates in both tiers for free.

## STOP conditions

- Plan 039 has landed and moved picker dispatch in a way that makes the
  composed menu a rewrite rather than a composition — report the new shape.
- The width source the status bar uses is not reachable from the title
  strip's builder without threading new state through `render` — report.
- Drift check fails.
