# Plan 058: One metric system in the chrome

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving on. On
> any STOP condition, stop and report — do not improvise. When done, update
> this plan's row in the high-priority index.
>
> **Run this LAST in the wave** — it sweeps the same files as everything
> else and will conflict with whatever is in flight (pass 8 and pass 9
> included). Rebase your reading on whatever has landed.
>
> **Drift check (run first)**:
> `git diff --stat 635aba8..HEAD -- shell/src/`
> Drift here is *expected*; this plan pins symbols and behavior, not lines.
> On a structural mismatch with "Current state" (a named function or
> constant gone), STOP.
>
> **Base**: `git switch -c advisor/ui-058-metric-unification` from the tip
> the operator names (after the rest of the wave); `origin/full/full`
> (`635aba8`) only if dispatched first anyway.
>
> **Shared ground rules**: see the README in this directory.

## Status

- **Priority**: P2
- **Effort**: L — and deliberately split so Step 1 alone is a shippable S.

## Why this matters

The chrome is sized in two currencies that diverge the moment the user
touches `[font] size`. Character-relative sizes — `px(ch * …)` where `ch`
is the font's advance — scale with the configured font: the keycap box,
the status badge, the files status column, the commit clock column.
Tailwind-style rem shorthands — `px_2`, `pr_2`, `pt_2`, `gap_1/2/3`,
`py_1` — do not: they resolve against GPUI's default rem size, which
nothing in `gitten.toml` reaches. Set `size = 16` and the glyphs grow while
their padding stays at 13px-era values; the keycap gets tight, the gaps go
baggy, and no single edit fixes it because the mismatch is scattered.

Plan 036 already fixed the worst *single* instance of this class (`PAD =
16.0` vs `.px_4()` desynchronizing hit-tests from glyphs). This plan
retires the class in the chrome: one currency, `ch`- and line-height-
derived, so the whole strip scales as one thing.

Colour and font hot-reload on the next frame is a headline behavior
(`CLAUDE.md`, Building) — today it half-works: text reloads, its box does
not.

## Current state (symbols, not lines — expect drift)

- Rem shorthands throughout `shell/src/chrome.rs`, `controls.rs`,
  `main.rs` (title strip, status bar), `views/*.rs` row builders: `px_2`,
  `pr_2`, `gap_1`, `gap_3`, `py_1`, `pt_2` et al.
- Character-derived sizes beside them: keycap `ch * 1.6` (`chrome.rs`,
  `fn keycap`), status badge `ch * 1.7` (`chrome.rs`, `status_bar`),
  `STATUS_CHARS`/`GAP_CHARS` (`files.rs`, `stashes.rs`),
  `WHO_CHARS`/`TIME_CHARS` (`commits.rs`).
- Structural px constants: `TITLE_H = 32`, `HEADER_H = 26`, `STATUS_H =
  26`, `ROW_H = 22` (twice: `graph.rs` and `diff.rs`), `ROW_PAD = 12`,
  picker `H = 22`, menu `ROW_H = 24`, prompt `h(px(34.0))`.
- The host's font (family, size, advance) is read per frame via
  `config::host(cx)` — the values needed are already on the render path.

## Scope

**In scope**: `shell/src/chrome.rs`, `controls.rs`, `input.rs`, `help.rs`,
`main.rs` (title/status strips), `views/*.rs` — *padding and gap* call
sites; a small set of named helpers.

**Out of scope, explicitly**: `ROW_H` and the other structural heights in
Step 3's list until Step 3 itself, which is gated; the diff gutter
arithmetic (`GUTTER_W` — it is already character-reasoned and documented);
the terminal client; any visual redesign — sizes may change by at most
rounding, the *point* is that they stop being frozen.

## Git workflow

Branch `advisor/ui-058-metric-unification`, base per the executor
instructions above.

## Steps

### Step 1: Name the currency

In `chrome.rs`, define the vocabulary once, derived from the live host
font: a small set of spacing helpers (e.g. `gap_s/gap_m`, `pad_s/pad_m` —
follow the existing naming voice, short and flat) returning `Pixels`
computed from `ch` (advance) with the ratios the current 13px defaults
imply, rounded to whole px. Document each with the one sentence that makes
it checkable: "at the default font this is exactly the old value."

**Verify**: unit tests asserting old-value equivalence at the default font
size, and monotonic scaling at size 16.

### Step 2: Sweep the pads and gaps

Replace rem shorthands in the in-scope files with the named helpers,
mechanical and reviewable — one commit per file or per surface, no
behavior change at default font. The magic multipliers get folded into the
vocabulary where they are spacing (`+ 6` hint padding) and left alone
where they are box *content* sizing already in `ch` (keycap `1.6`, badge
`1.7` — they scale correctly today; renaming them is churn, not progress).

**Verify**: `cargo test -p gitten-shell` and `./dev check` green after
each file; at default font size the layout is byte-identical where a test
can see it and visually identical where it cannot (say so).

### Step 3: The heights — gated, SKIP-AND-REPORT by default

Deriving `ROW_H`/`HEADER_H`/`TITLE_H` from line height is the full fix,
and it ripples: `uniform_list` row math, `section_height`, the graph's
dot geometry, hit-tests. **Do not attempt it inside this plan** unless the
operator explicitly widens the scope; instead, report the inventory —
every height constant, its consumers, and which ones would need to move
together — as the input to a future plan.

### Step 4: Full gate

`./dev check`. Report: what scales now, what still does not (the Step 3
inventory), and the one-line answer to "what happens at `size = 16`".

## Test plan

- Old-value equivalence tests (Step 1).
- Full gate green after every sweep commit.

## Done criteria

- No rem-shorthand spacing remains in the swept files; every pad and gap
  is font-derived through the named vocabulary.
- At the default font, nothing visibly moves.
- At `size = 16`, chrome spacing scales with the glyphs.
- Step 3 exists as a written inventory, not as code.

## STOP conditions

- A swept pad turns out to be load-bearing for a hit-test or a
  `uniform_list` measurement (the 036 class) — stop the sweep at that file
  and report it; that site needs its own look.
- The helpers cannot be computed without adding work to the render path
  beyond reading the host (rule 3) — report.
