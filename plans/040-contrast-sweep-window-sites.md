# Plan 040: Every text resolves against the surface it is drawn on

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving on. If a
> STOP condition occurs, stop and report. When done, update this plan's row in
> `plans/README.md`.
>
> **Drift check (run first)**: `git diff --stat 635aba8..HEAD -- core/src/theme.rs shell/src/chrome.rs shell/src/help.rs shell/src/input.rs shell/src/main.rs shell/src/views/`
> Written against `635aba8`. On a mismatch with the excerpts below, STOP.

## Status

- **Priority**: P2
- **Effort**: S (eight call-sites, one each)
- **Risk**: LOW (resolved values shift slightly darker/lighter; the contrast
  example pins the result)
- **Depends on**: none
- **Category**: bug (contrast floors)
- **Planned at**: commit `635aba8`, 2026-08-31

## Why this matters

Plan 034 established the rule: text ink resolves against the `Surface` it is
drawn on, with floors 3.5 (`min_contrast`) for text and 3.0 (`min_furniture`)
for furniture, and shipped `dim_on(Surface)` / `quiet_on(bg)` for exactly this.
Eight sites missed the sweep. The worst draw the app's quietest text on the
one row being read: `quiet_on(chrome.bg)` resolved against `chrome.bg` but
painted on the cursor wash (`chrome.selection_bg`), measuring **2.58:1** in
all three shipped themes — under even the furniture floor. The others draw
the app's attentive-reading surfaces (help body, error overlay body, prompt
exit labels) below the text floor at 3.37–3.40:1, and two raw-`faint` texts
at ~1.97:1.

## Current state

The repo's own docs state the numbers (`core/src/theme.rs`): raw `dim` on
`title_bg` is 3.37:1 and on `status_bg` 3.40:1 — "under the text floor" (doc
comments at lines ~57 and ~64); raw `dim` on the cursor wash is 2.97:1; the
cursor wash is `chrome.selection_bg`, painted by `chrome::list_row` for the
current row (`shell/src/chrome.rs:63-64`).

The eight sites:

1. `shell/src/views/files.rs:179` — `Mark::Untracked => t.quiet_on(t.chrome.bg)`,
   drawn on the current row's wash. The sibling arm (lines 170-176) shows the
   pattern:

   ```rust
   Mark::TypeChange => {
       if current {
           host.theme.dim_on(Surface::Cursor)
       } else {
           t.chrome.dim
       }
   }
   ```

2. `shell/src/views/commits.rs:846-849` — the time cell, same shape:

   ```rust
   .text_color(rgb(match armed {
       true => host.theme.chrome.error,
       false => host.theme.quiet_on(host.theme.chrome.bg),
   }))
   ```

3. `shell/src/views/branches.rs:861` — the `(gone)` text, `quiet_on(c.bg)`
   unconditional, drawn via `list_row(host, current, …)` (line ~835).
4. `shell/src/views/branches.rs:204` — `Row::Detached`'s dot takes
   `theme.chrome.dim` **at flatten time**; drawn on the wash where the text
   beside it resolves `dim_on(Surface::Cursor)` (line ~840). A test pins the
   raw value (line ~1021) — flip it with the fix. Resolve at draw time in
   `row()`: flatten cannot know `current`.
5. `shell/src/help.rs:107` — the panel's whole text ink is
   `.text_color(rgb(c.dim))` on `c.title_bg` (line 100).
6. `shell/src/help.rs:175` — each row's description, same raw `c.dim`.
7. `shell/src/main.rs:4854` — the message overlay's body (`error.full`),
   raw `c.dim` on `c.title_bg` (line 4847).
8. `shell/src/input.rs:806` — the prompt's exit labels, raw `c.dim` on the
   prompt band (`chrome.status_bg`, line 739), while its own doc (lines
   789-790) claims "the same pairing the status bar draws its hints in" —
   and the status bar resolves `dim_on(Surface::Status)`
   (`shell/src/chrome.rs:345,371`).

Two taste-flagged `faint`-as-text sites (same rule, maintainer may rule
furniture): `shell/src/chrome.rs:375` — the status bar's truncation `…`,
raw `c.faint` on `status_bg`, while the version string beside it resolves
`quiet_on(c.status_bg)` (line 382); `shell/src/main.rs:4773` — the error
band's exits (`· esc dismiss · …`), raw `c.faint` on the same `status_bg`
(band at 4763). `core/src/theme.rs:655-669` states the rule: `faint` as
text goes through `quiet_on`; only borders keep it raw.

Helpers: `Theme::dim_on(Surface) -> Rgb`, `Theme::quiet_on(bg: Rgb) -> Rgb`
(`core/src/theme.rs`). The contrast example gates the matrix:
`cargo run -q -p gitten-core --example contrast` prints the table and its
tests pin it.

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Theme tests | `cargo test -p gitten-core` | all pass |
| Window tests | `cargo test -p gitten-shell` | all pass |
| Contrast report | `cargo run -q -p gitten-core --example contrast` | matrix printed, no line under its floor |
| Lint/fmt | `cargo clippy --workspace -- -D warnings` / `cargo fmt --all -- --check` | exit 0 |

## Scope

**In scope**: the eight sites above; the two taste-flagged sites; the
`branches.rs` test assertion; new rows in `core/examples/contrast.rs` for
any newly-resolved pair the example enumerates.

**Out of scope**:
- Palette hex values — themes are built to matched ratios; resolution is
  the fix, retuning is not (034's own precedent: the owner chose the
  structural fix over hex retuning).
- `chrome.raised`/`chrome.keycap` — the owner's uncommitted design pass;
  if the fields exist in `ChromePalette`, leave them alone entirely.
- The TUI's quiet sites — plan 044.
- Any `Surface` enum change (promoting `file_bg`/`hunk_bg`) — recorded,
  separately decided.

## Git workflow

- Branch: `advisor/ui-040-contrast-sweep`, from `635aba8`.
- Commit style: `shell: quiet text resolves like the row it lands on`.

## Steps

### Step 1: The three quiet-on-wrong-bg sites (files, commits, branches-gone)

At each site, resolve against the row's real background:

```rust
// files.rs:179 — same shape as the TypeChange arm above it
Mark::Untracked => if current {
    t.quiet_on(t.chrome.selection_bg)
} else {
    t.quiet_on(t.chrome.bg)
},
```

`commits.rs:848` and `branches.rs:861` take the same conditional (the
`armed` arm in commits stays `chrome.error`).

**Verify**: `cargo test -p gitten-shell` → all pass.

### Step 2: The detached-HEAD dot resolves at draw time

In `shell/src/views/branches.rs`, move the `Row::Detached` ink decision from
`flatten` (line 204) into `row()`'s draw, resolving `dim_on(Surface::Cursor)`
when the row is current — the same call the text beside it makes (~840).
Flip the raw-value assertion in the test (~1021).

**Verify**: `cargo test -p gitten-shell` → all pass.

### Step 3: The four prose surfaces through `dim_on`

`help.rs:107` and `help.rs:175` →
`rgb(host.theme.dim_on(theme::Surface::Title))`; `main.rs:4854` → same;
`input.rs:806` → `dim_on(Surface::Status)` (the band is `status_bg`).

**Verify**: `cargo test -p gitten-shell` → all pass.

### Step 4: The two taste-flagged `faint` sites

`chrome.rs:375` and `main.rs:4773` →
`quiet_on(c.status_bg)` / `dim_on(Surface::Status)`. Each gets a comment
naming the ruling (text resolves; furniture keeps raw only for borders).

**Verify**: `cargo test -p gitten-shell` → all pass.

### Step 5: The contrast example stays honest

Run `cargo run -q -p gitten-core --example contrast`. If its table
enumerates the affected pairs, add/adjust rows so every newly-resolved
pair appears and clears its floor; if it is surface-keyed and already
covers them, no change.

**Verify**: example runs clean; `cargo test -p gitten-core` → all pass.

## Test plan

- The existing resolution tests in `core/src/theme.rs` (the rebuild tables)
  need no change — resolution is per-surface already.
- Flip the one raw-value assertion (`branches.rs` ~1021).
- Eyeball gate: `./dev dump commits` — the quiet texts must still read as
  quiet, not loud (the values move slightly darker).

## Done criteria

- [ ] `grep -n 'quiet_on(t.chrome.bg)' shell/src/views/` finds only
      non-current-conditional uses (or none)
- [ ] `cargo run -q -p gitten-core --example contrast` prints no pair under its floor
- [ ] `cargo test -p gitten-core -p gitten-shell` exits 0
- [ ] clippy `-D warnings` + fmt clean
- [ ] No palette hex values changed (`git diff core/src/theme.rs` shows no literal changes)
- [ ] `plans/README.md` status row updated

## STOP conditions

- A cited line's code no longer matches the excerpt.
- Resolving a site makes it indistinguishable from its neighbours in the
  `./dev dump` eyeball gate — report which site; a floor override decision
  is the maintainer's, not the executor's.
- `quiet_on`/`dim_on` signatures differ from those described here.

## Maintenance notes

- New views must resolve per-surface from day one; the reviewer's check is
  `grep -n 'text_color(rgb(c\.' shell/src/views/` — raw palette text colours
  in a view are a finding.
- The `file_bg`/`hunk_bg` Surface promotion (recorded in the pass-6 ledger)
  would move two more sites; do not pre-do it here.
