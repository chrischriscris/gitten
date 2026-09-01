# Plan 010: Pick the commit list's widest row in characters, not bytes

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat 2dfcb82..HEAD -- shell/src/views/commits.rs`
> If the file changed since this plan was written, compare the "Current state"
> excerpts against the live code before proceeding; on a mismatch, treat it as
> a STOP condition.

## Status

- **Priority**: P2
- **Effort**: S
- **Risk**: LOW
- **Depends on**: none
- **Category**: bug
- **Planned at**: commit `2dfcb82`, 2026-08-26

## Why this matters

`uniform_list` measures exactly **one** row to decide how wide its scrollable
content is (see the doc comment at `shell/src/views/commits.rs:21-24`), so the
choice of that row is load-bearing. The commit view ranks candidates by
`c.subject.len()` — a **byte** length. A CJK/full-width subject costs 3 bytes
per character but draws ~2 cells wide, so multibyte subjects inflate to ~1.5×
their real width and can dethrone a genuinely wider ASCII row. The wrong row
gets measured, and every longer row clips at the right edge with no way to
reach it: this view has no horizontal pan of its own beyond what that measured
width enables. Any repository with Japanese/Chinese/Korean commit messages hits
this.

The fix ranks by an estimated *display* width in characters, matching the
convention the sibling presentations already follow (`chars()`, not `len()`)
and keeping the documented approximation level — one extra non-ASCII byte per
extra display cell is fine here; being inverted is not.

## Current state

- `shell/src/views/commits.rs` (~190 lines) — the GPUI commit column.
  - The widest-row computation in `Commits::new`, :73–90:
    ```rust
    let char_w = host.font.char_width();
    let widest = draws
        .iter()
        .zip(&commits)
        .map(|(d, c)| graph::row_width(d) + c.subject.len() as f32 * char_w)
        .enumerate()
        .max_by(|(_, a), (_, b)| a.total_cmp(b))
        .map(|(i, _)| i)
        .unwrap_or(0);
    ```
    consumed once at :131 `.with_width_from_item(Some(self.data.widest))`.
  - The comment above it (:73–81) already blesses approximation ("It only picks
    which row `uniform_list` measures") but does not cover byte-vs-char.
- The precedent to match, `shell/src/views/split.rs:272-275`:
    ```rust
    // `chars`, not `len`: a line of box drawing would otherwise
    // measure three times too wide and set the column for the whole
    // diff.
    self.widest_chars = self.widest_chars.max(l.text.chars().count());
    ```
  (`markdown.rs:616-620` repeats the same idea.)
- `shell/src/graph.rs`: `pub fn row_width(d: &Draw) -> f32` at :88 — lane-count
  estimate for the graph gutter, already display-shaped. Do not change it.
- Conventions: views read the host on the render path, never capture clones;
  comments explain the *why* in prose; tests are inline `#[cfg(test)] mod
  tests` beside the code they cover.
- Known adjacent issue deliberately NOT part of this plan: per-row clones of a
  `Draw` + two `String`s per visible row per frame (`row()` at :167–190) are a
  tracked perf item from the previous audit; do not touch them.

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Crate tests | `cargo test -p gitten-shell` | all pass |
| Full gate | `./check.sh` | exit 0 |

## Scope

**In scope** (the only files you should modify):
- `shell/src/views/commits.rs`

**Out of scope**:
- `graph::row_width` and anything in `graph.rs`.
- The `row()` painter, `Who`, or selection plumbing.
- The tui/web commit renderers (they have their own width logic; only this file
  has the measured-row trap).
- Any real display-width dependency (e.g. unicode-width crates): core must stay
  dependency-free and this estimate is explicitly allowed to be coarse.

## Git workflow

- Branch: `advisor/010-widest-row-in-characters`
- Commit style: sentence-case imperative like `Join working-tree paths onto the
  repo top level`. Do NOT push or open a PR unless instructed.

## Steps

### Step 1: Extract the estimate into a pure function

Replace the inline closure body with a named helper so it is unit-testable
(place it just above `Commits::new` or below the `impl` block, private):

```rust
/// One row's reach, roughly: the graph gutter plus the subject. An estimate,
/// because this number only decides which single row `uniform_list` measures
/// to learn the true scrollable width — see the note in `Data`.
///
/// Characters, never `.len()`: a byte-lengthed CJK subject counted itself
/// three times too wide and could dethrone a genuinely wider ASCII row,
/// leaving that row clipped past the last reachable column forever.
fn estimated_row_width(gutter: &graph::Draw, subject: &str, char_w: f32) -> f32 {
    graph::row_width(gutter) + subject.chars().count() as f32 * char_w
}
```

Call it from `Commits::new`'s iterator. Keep everything else identical.

**Verify**: `cargo build -p gitten-shell` → exit 0.

### Step 2: Unit test the inversion

Add an inline `mod tests` (none exists yet in this file) with one focused test:

- Build two commits whose subjects are contrived so that bytes invert against
  characters: `A` = ASCII, 45 chars wide; `B` = `"日"` repeated 20 times
  (20 chars, 60 UTF-8 bytes). Give both identical `Commit.author/short` fields
  (`Commit` comes from `gitten_core`; check its constructor requirements in
  `core/src/lib.rs` / wherever `Commit` is defined and satisfy them minimally).
- Produce `draws` through the real functions —
  `gitten_core::assign_lanes(&commits)` then `graph::row_draws(&commits, &rows)`
  (both already imported at :3 and used in `new`) — with empty parents so each
  gutter width is small and constant.
- Assert the chosen index equals the ASCII row's index under
  `estimated_row_width`, e.g. compute maxima the same way `new` does over your
  own `char_w` value like `12.0`. Before this plan the byte-based expression
  picks B (60 > 45); after, A (45 > 20). Assert the after-behavior by calling
  the new helper directly (no need to construct `Commits`, whose constructor
  also formats/eprintln!s load stats).

A second assertion worth one line: a 2× factor guard — a CJK string of n chars
must never out-rank an ASCII string wider than 3n chars under the *old*
expression but may legitimately lose under the new one when shorter; keep this
only if it reads naturally, otherwise drop it. One clear inversion test is the
deliverable.

**Verify**: `cargo test -p gitten-shell widest_row` (or the test's name) → pass;
`cargo test -p gitten-shell` → all pass.

### Step 3: Full gate

**Verify**: `./check.sh` → exit 0.

## Test plan

- New inline test in `shell/src/views/commits.rs` covering the byte-vs-char
  inversion (the specific regression fixed).
- Pattern: inline `mod tests` with plain assertions, as in
  `shell/src/views/markdown.rs` (e.g. its `Metrics::for_font` tests around
  :1252–1288 show constructor-free unit testing conventions).

## Done criteria

All must hold:

- [ ] `grep -n 'subject.len()' shell/src/views/commits.rs` returns nothing
- [ ] `grep -n 'fn estimated_row_width' shell/src/views/commits.rs` finds the helper
- [ ] `cargo test -p gitten-shell` exits 0 including the new inversion test
- [ ] `./check.sh` exits 0
- [ ] No files outside the in-scope list modified (`git status --short`)
- [ ] `plans/README.md` status row updated

## STOP conditions

Stop and report back (do not improvise) if:

- `Commit` cannot be constructed headlessly without spawning git (report what
  fields block construction instead of stubbing them behind unsafe or defaults).
- `graph::row_draws` signature changed or moved since this plan (drift check
  will catch it; verify the excerpt above matches first).
- You find another `.len()`-based width ranking in `tui/src/commits.rs` or
  `web/` during Step 1's build — report it; do not expand scope unilaterally.

## Maintenance notes

- If a real display-width function ever lands (the tui's cell-width story, a
  tracked known finding), thread it here too — this is one more consumer the
  same way `split.rs` would be.
- Reviewer focus: the estimate comment must keep saying "approximation", so a
  future reader doesn't "fix" it into a dependency-bearing exact measurement.
