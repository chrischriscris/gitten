# Plan 059: Nits bundle — six one-sitting fixes

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving on. On
> any STOP condition, stop and report — do not improvise. When done, update
> this plan's row in `plans/high-priority/README.md`.
>
> Each item is independent: land what verifies, SKIP-AND-REPORT what does
> not. One commit per item, summary line stating the fact it makes true.
>
> **Drift check (run first)**:
> `git diff --stat 635aba8..HEAD -- shell/src/input.rs shell/src/help.rs shell/src/main.rs shell/src/chrome.rs shell/src/views/`
> Findings were verified against `635aba8`; drift is expected (passes 8 and
> 9 are in flight) — match on quoted content, and re-verify each item still
> exists before fixing it. **Dedupe note**: plan 053 (same wave) owns the
> sidebar hover tint, the pane-header counts for BRANCHES/STASH, the prompt
> height rhythm, and the stale RowState/branches doc comments — those are
> NOT here; do not fix them twice.
>
> **Base**: `git switch -c advisor/ui-059-nits` from the commit the operator
> names (see the README's base convention).
>
> **Shared ground rules**: see `plans/high-priority/README.md`.

## Status

- **Priority**: P1 (cheap, visible, zero-risk)
- **Effort**: S × 6

## The items

### 1. The prompt selects text with the wrong colour

`shell/src/input.rs:777`:

```rust
selection: rgb(chrome.selection_bg),
```

`core/src/theme.rs` documents the split this violates: `selection_bg`
(`theme.rs:227`) is "the row the keyboard is on" — a full-width cursor
bar — while `selected_bg` (`theme.rs:230-235`) is "its own colour and not
`selection_bg`" for text a drag has selected. The diff view already obeys
(`diff.rs` uses `selected_bg` for drag selection). In the shipped dark
theme the difference is 0x241f1a vs 0x2f3b4a — the prompt's drag selection
is currently near-invisible *and* semantically the wrong field.

**Fix**: `selection_bg` → `selected_bg` at that site.
**Verify**: grep — no other `selection_bg` use styles a *text-range*
selection; build green.

### 2. The help backdrop says "dim" and isn't

`shell/src/help.rs` (~`:83-86`): the full-window backdrop is
`.inset_0().occlude()` with **no fill**, while its own comment calls it
"the dim space around it" and the panel sits on `title_bg` — which the
palette docs place within ~1.05:1 of content, i.e. held off a live diff by
a hairline alone. A modal that doesn't dim reads as a floating rectangle.

**Fix**: give the backdrop a scrim — `chrome.bg` at partial alpha (an
`Rgba` built from the existing palette field; **no new palette field**,
per the wave's palette note). Pick the alpha by measurement: the panel's
border must clear ~1.5:1 against the scrimmed diff behind it where it
cleared nothing before; note the value and the measured before/after in
the commit.
**Verify**: build; the comment and the paint now agree.

### 3. One spelling for "minus"

The diff pane header spells deletions with U+2212 (`main.rs:144`:
`format!("−{}", s.dels)`); the in-diff file header row spells them ASCII
(`diff.rs:2789`: `format!("-{dels}")`). Same fact, two glyphs, one screen.

**Fix**: unify on U+2212 `−` — it is the typographic sign, and the header
already chose it. Update the `diff.rs` site and any test string pinning
the ASCII form.
**Verify**: `grep -rn '"-{' shell/src/views/diff.rs` finds no dels
formatting; tests green.

### 4. FILES doesn't count to zero

`main.rs:4255-4260`: the FILES header prints
`view.read(cx).changed().to_string()` unconditionally — `0` on a clean
tree, directly above the empty-state line that already says "working tree
clean". A zero count is the empty state said twice, and no other pane
prints one.

**Fix**: `Some(count)` only when the count is nonzero.
**Verify**: existing header tests; add the zero case. (Plan 053 may add
counts to BRANCHES/STASH — this item only governs the zero; no conflict.)

### 5. The chrome doc names panes that moved

`chrome.rs:5`: "…focuses it — `1 FILES`, `4 COMMITS`, `5 <file>`…" — stale
twice over: status is `1`, files is `2`, the diff keycap is `6`
(`main.rs:158-163` `STACK_TOP`, `:166` `STACK_FOOT`). A module doc that
misstates the keymap is worse than none.

**Fix**: correct the doc to the live table — or better, drop the literal
numbers and say "numbered in [`STACK_TOP`] order", which cannot go stale.
**Verify**: read it; done.

### 6. The right edge belongs to two things

The overlay scrollbar's track and the panes' right-aligned furniture
occupy the same pixels: section counts (`chrome.rs:104`, `.pr_2()`),
branch drift counts (`branches.rs:857`), commit clocks (`commits.rs:836`)
— all 8px from the edge, under a track of about that width. When a pane
scrolls, the thumb sits on the column the eye reads.

**Fix**: measure the actual track width at the call sites (it is our
`DeferredScrollbar` under `gpui_component`'s widget — read the configured
width, don't guess), and give scrollable panes' right-edge furniture a
reserve of track-width instead of `pr_2`. One named constant next to the
scrollbar setup, used by all three sites, so the two can never disagree
again.
**Verify**: the constant equals the track width by construction (same
source); build + tests green.

## Test plan

Per item, as above; then the full gate `./dev check` once at the end.

## Done criteria

Each landed item's "Verify" holds; skipped items are reported with the
reason; one commit per item.

## STOP conditions

- An item's site has been restructured by a landed pass-8/9 plan such that
  the finding no longer reproduces — skip it and report, don't chase it.
- Item 6: the track width is not readable from our side of the widget
  seam — report what the seam exposes instead.
