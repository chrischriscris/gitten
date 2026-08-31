# Plan 044: The terminal's quiet text resolves like the window's

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving on. If a
> STOP condition occurs, stop and report. When done, update this plan's row in
> `plans/README.md`.
>
> **Drift check (run first)**: `git diff --stat 635aba8..HEAD -- tui/src/ core/src/theme.rs`
> Written against `635aba8`. On a mismatch with the excerpts below, STOP.

## Status

- **Priority**: P2
- **Effort**: S
- **Risk**: LOW (lifted values must stay quiet-looking — eyeball via `./dev tui`)
- **Depends on**: none
- **Category**: bug (client parity / contrast)
- **Planned at**: commit `635aba8`, 2026-08-31

## Why this matters

The window's identical sentences resolve through `Theme::quiet_on` — its
empty states name the number outright: raw `faint` is **2.05:1** on
`chrome.bg`, under every floor (`shell/src/chrome.rs:161-163`). The terminal
draws the same sentences raw: "nothing stashed", the files pane's empty
line, "no branches yet", and the status band's note are all
`Ink::new(theme.chrome.faint, theme.chrome.bg)` (or `faint` on `status_bg`)
— the least legible text in either client. `core::Theme::quiet_on(bg)` is
crate-agnostic and the tui already adopts the theme's resolution everywhere
else (`tui/src/rows.rs:512-522`): the shared fix exists and was applied to
one client only.

## Current state

- `tui/src/stashes.rs:353-357`:

  ```rust
  } else if self.rows.is_empty() {
      Some((
          "nothing stashed",
          Ink::new(theme.chrome.faint, theme.chrome.bg),
      ))
  ```

- `tui/src/files.rs:701` — files-pane empty-state line, same ink pair.
- `tui/src/branches.rs:632` — "no branches yet", same.
- `tui/src/main.rs:3027` — the status band's note, raw `c.faint` on
  `c.status_bg`.
- The window's equivalents: `shell/src/chrome.rs:161-163` (the 2.05:1 doc)
  and the status bar's version string resolving
  `quiet_on(c.status_bg)` (`chrome.rs:382`).
- `core/src/theme.rs` — `quiet_on(bg: Rgb) -> Rgb`; `Surface`-keyed
  resolution tables exist for `dim` (used by the tui at `tui/src/rows.rs:512-522`
  via `theme.background(Surface::…)`-style calls — read that block for the
  tui's exact accessor spelling).

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Terminal tests | `cargo test -p gitten-tui` | all pass |
| Core tests | `cargo test -p gitten-core` | all pass |
| Lint/fmt | `cargo clippy --workspace -- -D warnings` / `cargo fmt --all -- --check` | exit 0 |
| Eyeball | `./dev tui` | empty states readable, still quiet |

## Scope

**In scope**: the four tui sites above; tests asserting the resolved ink.

**Out of scope**:
- Same-role section headings (`tui/src/files.rs:755,762`,
  `tui/src/branches.rs:681,688`) — a taste call; listed as an optional
  Step 3, execute only if the resolved values keep them visibly quieter
  than their rows.
- The window's sites (plan 040), any `Surface` enum change, the tui's
  cursor-bar half of the armed tint (resolved; in the pass-6 ledger).

## Git workflow

- Branch: `advisor/ui-044-tui-quiet-adoption`, from `635aba8`.
- Commit style: `tui: the quiet text resolves like the window's`.

## Steps

### Step 1: The four sites through `quiet_on`

Replace `Ink::new(theme.chrome.faint, theme.chrome.bg)` with
`Ink::new(theme.quiet_on(theme.chrome.bg), theme.chrome.bg)` at
`stashes.rs:356`, `files.rs:701`, `branches.rs:632`; and the status note at
`main.rs:3027` with `theme.quiet_on(theme.chrome.status_bg)` on the
`status_bg` pair — matching the accessor spelling the tui already uses
(`rows.rs:512-522`).

**Verify**: `cargo test -p gitten-tui` → all pass (some tests may pin raw
ink values — flip those assertions with the fix, naming the floor in the
test comment, as the window's tests do).

### Step 2: Pin the resolution

Add one test per pane asserting the empty-state ink equals
`theme.quiet_on(theme.chrome.bg)` (not the raw `faint`), modelled on the
existing tui ink assertions (grep `Ink::new` in `tui/src/*/#[cfg(test)]`).

**Verify**: `cargo test -p gitten-tui` → all pass including the new tests.

### Step 3 (optional, taste): the section headings

Run `./dev tui` and eyeball the four empty states and the neighbouring
section headings. If a heading now reads louder than the quiet sentence
beside it, apply the same resolution to the four heading sites listed out
of scope — and say so in the commit message. If unsure, do nothing and
note it in the PR/commit body.

**Verify**: `cargo test -p gitten-tui` → all pass.

## Test plan

As in Steps 1-2. Deterministic: no fixture or repo dependency.

## Done criteria

- [ ] `grep -n 'Ink::new(theme.chrome.faint, theme.chrome.bg)' tui/src/` finds nothing
- [ ] `cargo test -p gitten-tui` exits 0 with the new assertions
- [ ] clippy `-D warnings` + fmt clean
- [ ] No files outside `tui/src/` modified
- [ ] `plans/README.md` status row updated

## STOP conditions

- `quiet_on` does not exist on the theme the tui holds (report what it has).
- A cited site no longer matches the excerpt.
- The tui draws empty states through a shared helper the excerpts missed —
  fix the helper (same rule), and say so.

## Maintenance notes

- New tui quiet sentences must go through `quiet_on`; the reviewer's grep
  is `theme.chrome.faint` in `tui/src/` — raw uses of it as *text* are a
  finding (borders excepted).
- The CJK-in-squeezed-cells seam (pass-3 follow-up) is unrelated; do not
  touch width math here.
