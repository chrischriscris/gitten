# Plan 026: A font edit keeps your selection and your place in the diff

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. Commit your work in the worktree following the
> git workflow section. Do NOT update `plans/README.md` — your reviewer
> maintains the index.
>
> **NEVER launch the desktop client** (no `cargo run -p gitten-shell`, no
> `./dev desktop`). A window appearing unannounced interrupts whoever is at
> the keyboard. The shell's tests are headless GPUI and open no window — they
> are your verification.
>
> **Base: `full/full` (`d53a0c7`).**
> **Drift check (run first)**: `git diff --stat d53a0c7..HEAD -- shell/src/views/diff.rs`

## Status

- **Priority**: P2
- **Effort**: S
- **Risk**: LOW
- **Depends on**: none
- **Category**: bug
- **Planned at**: `d53a0c7` (`full/full`), 2026-08-30
- **History**: v1 of this plan was written against `main` (`87229df`) and had
  a second half — re-ranking the commit list's widest row on a font change.
  **That half is deleted, not deferred**: on `full/full` the commit list no
  longer scrolls sideways, `estimated_row_width` and `with_width_from_item`
  are gone, and `shell/src/views/commits.rs` says so directly — *"the
  widest-row measurement went with the sideways scroll"* and *"Rows are
  exactly the viewport's width — no `Unconstrained` sizing and no widest-row
  measurement."* The bug it fixed cannot occur. **Do not touch
  `shell/src/views/commits.rs`.**

## Why this matters

`gitten.toml` hot-reloads on the next frame — a headline feature. But a font
edit in the diff view **silently throws away the text selection**, and moves
your reading position, because the font branch reuses `apply_layout`, which
was written for a *presentation* change.

For a layout cycle, dropping the selection is right, and `apply_layout`'s own
comment says why: *"a replace pair is one row here and two there"* — there is
no honest correspondence between the old rows and the new ones. A font edit is
not that. It is the same presentation, the same rows, the same row count; only
the glyph metrics moved. Both losses are pure regression: you save a colour
tweak in `gitten.toml` and the text you had selected is gone.

## Current state

All line numbers are `full/full` (`d53a0c7`). Locate by content, not number.

- `shell/src/views/diff.rs:785-800` — `Diff::reflow`, the font branch:

  ```rust
  fn reflow(&mut self, width: f32, host: &Host) {
      // Before the width exit, not beside it: a font edit reshapes every
      // glyph without moving the width, and would otherwise survive until the
      // next resize happened to cross a boundary. The price is one
      // `Option<Font>` compare — still O(1) on the common path, which is what
      // the resize test below pins.
      if self.font_applied.as_ref() != Some(&host.font) {
          self.font_applied = Some(host.font.clone());
          // Reset first: arrange() has already been given today's host, and the
          // width half of `applied` must re-fire on the rebuilt renderers.
          self.applied = (0.0, "");
          self.apply_layout(self.current, host);
      }
      let wrap = host.wrap.at(self.wrap);
      if (width, wrap.name()) == self.applied || width <= 0.0 {
          return;
  ```

- `shell/src/views/diff.rs:1497-1527` — `apply_layout`, which drops both:

  ```rust
  fn apply_layout(&mut self, index: usize, host: &Host) {
      let fraction = self.view.get().progress();
      self.current = index;
      // Every row about to be replaced, so a selection anchored to one of them
      // would be pointing at whatever now has its index. There is no honest
      // way to carry a selection across two presentations of the same diff —
      // a replace pair is one row here and two there — so it goes.
      self.sel = None;
      // An armed discard rides the same logic: the row it was asked about
      // is about to have a different meaning.
      self.armed_hunk = None;
      let built = arrange(&self.prepared, host, &self.layouts, index);
      // ... order / renderers / widest / load / total updated, applied = (0.0, "")
      let mut v = self.view.get();
      v.set_len(self.order.len());
      v.go_to_fraction(fraction);
      self.view.set(v);
      self.defer_show(v);
  }
  ```

  Note `go_to_fraction` — a proportional restore, correct for a presentation
  swap (the row count changes) and lossy for a font edit (it does not).

- `shell/src/views/diff.rs:809-862` — the `changed` branch of `reflow`, which
  is what makes the restore safe. After `apply_layout` the renderers are fresh
  and `self.applied == (0.0, "")`, so for any `width > 0` this branch always
  runs in the same call. It re-anchors on the cursor and re-resolves the
  selection, dropping it only if it genuinely no longer resolves:

  ```rust
  let anchor = self.order.get(self.view.get().cursor()).copied();
  let (built, headers) = expand(&self.order, &self.renderers.borrow(), anchor);
  // ...
  let mut v = self.view.get();
  v.set_len(self.order.len());
  v.go_to(built.anchor);
  self.view.set(v);
  self.defer_show(v);
  // ...
  // The rows are the same rows at different heights, so the selection
  // survives — but every visual row it cached has moved, which is what
  // `resolve` rebuilds.
  if let Some(sel) = &mut self.sel {
      if !sel.resolve(&self.order) {
          self.sel = None;
      }
  }
  ```

- The scroll position is a `Cell<Viewport>` (`gitten_core::view::Viewport`,
  imported at `diff.rs:70`). Relevant methods, in `core/src/view.rs`:
  `cursor()`, `top()`, `set_len(len)` (`:120`), `progress()` (`:167`),
  `go_to(row)` (`:223`, **exact**), `go_to_fraction(at)` (`:259`).
  `Diff::defer_show(v)` is at `diff.rs:1100` — the list has not laid out the
  new row count yet, so the position is deferred rather than written.

- `armed_hunk: Option<(u16, u32)>` (`diff.rs:698`) is a *pending destructive
  action* (an armed hunk discard). `apply_layout` clears it.

## The fix

In the font branch only, stash the selection and the exact cursor row, let
`apply_layout` run, then put them back and let the `changed` branch below
re-resolve them against the rebuilt order table:

```rust
if self.font_applied.as_ref() != Some(&host.font) {
    self.font_applied = Some(host.font.clone());
    self.applied = (0.0, "");
    // Same presentation before and after — only the glyph metrics moved — so
    // unlike a layout *change* the selection and the exact cursor row both
    // still mean something. `apply_layout` is written for the change and
    // drops both (a fraction of the old row count, no selection); stash them
    // and hand them back. Sound only because `apply_layout` leaves fresh
    // renderers with `applied` reset, so the `changed` branch below always
    // runs in this same call and re-resolves both against the rebuilt order
    // table — this is not restoring stale state.
    let keep = self.sel.take();
    let cursor = self.view.get().cursor();
    self.apply_layout(self.current, host);
    let mut v = self.view.get();
    v.go_to(cursor.min(self.order.len().saturating_sub(1)));
    self.view.set(v);
    self.defer_show(v);
    self.sel = keep;
}
```

**Do NOT restore `armed_hunk`.** It is a pending *destructive* action; making
someone re-arm a discard after a config reload is the safe direction, and
that asymmetry deserves a one-line comment saying so. Do not change
`apply_layout` itself — its behaviour is correct for its other callers.

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Shell tests | `cargo test -q -p gitten-shell` | exit 0 |
| Whole workspace | `cargo test -q --workspace` | exit 0 |
| Lint / format | `cargo clippy -q --workspace --all-targets -- -D warnings && cargo fmt --check` | exit 0 |

Run every command in the **foreground** and let it block to completion. A cold
GPUI worktree build is slow; that is expected. Do not dispatch builds as
background tasks and pause.

## Scope

**In scope**: `shell/src/views/diff.rs` only.

**Out of scope** (touching these fails review):
- `shell/src/views/commits.rs` — the widest-row half of this plan is dead; see
  History above.
- `apply_layout`'s own body, beyond leaving it exactly as it is.
- `core/src/view.rs`, `core/` generally.
- `armed_hunk` restoration.

## Git workflow

- Branch off `full/full`: `git switch -c advisor/ff-016-font-hot-reload-parity full/full`
- Commit style: imperative, why-first, no prefix — match `git log --oneline -10`.
  e.g. `Keep the selection and the row when the font changes under the diff`.
- Do NOT push or open a PR.

## Steps

### Step 1: stash and restore in the font branch

Apply the fix above.

**Verify**: `cargo test -q -p gitten-shell` → exit 0, no pre-existing test broken.

### Step 2: a test that fails without the fix

In `diff.rs`'s test module, model on the existing font test (find it with
`grep -n 'font' shell/src/views/diff.rs | grep -i 'fn '`). Add
`a_font_edit_keeps_the_selection_and_the_row`:

- Build a `Diff` over a multi-file fixture using the module's existing
  helpers, reflow at a width.
- Set a selection and capture its text via the same accessor the other
  selection tests use.
- Move the cursor to a known row > 0.
- Reflow again with a host whose `font.size` differs.
- Assert: `sel.is_some()`; the selected **text is byte-identical** (this is
  the assertion that catches a botched restore — not merely that some
  selection exists); and the **logical** row under the cursor is unchanged
  (compare through `self.order`, since the visual index may legitimately move
  if the new font changes the column budget).
- Assert the other half of the rule still holds: an actual `apply_layout` to a
  different index still clears the selection.

**Validate the test by mutation**: temporarily delete the `self.sel = keep;`
line, run the test, confirm it **FAILS**; restore it, confirm it passes.
Report both results. A test that passes either way pins nothing.

**Verify**: `cargo test -q -p gitten-shell a_font_edit_keeps_the_selection_and_the_row` → 1 passed.

### Step 3: gate

**Verify**: `cargo test -q --workspace` → exit 0;
`cargo fmt --check && cargo clippy -q --workspace --all-targets -- -D warnings` → exit 0.

## Done criteria

- [ ] `grep -n 'let keep = self.sel.take' shell/src/views/diff.rs` shows the stash inside the font branch, and nowhere else
- [ ] `grep -n 'armed_hunk' shell/src/views/diff.rs` shows it is NOT restored in the font branch
- [ ] `cargo test -q --workspace` exits 0, including the new test
- [ ] Mutation check reported: test fails without `self.sel = keep;`
- [ ] `cargo fmt --check` and clippy clean
- [ ] `git diff --stat d53a0c7..HEAD` shows **only** `shell/src/views/diff.rs`

## STOP conditions

- The `changed` branch does not run after the font-branch `apply_layout` on
  some path you can construct — the restore would leave an unresolved
  selection. Report the path.
- A pre-existing test asserts a *font* change drops the selection with a
  comment saying that is deliberate.
- You cannot make the mutation check fail.
- `shell/src/views/diff.rs` has drifted from the excerpts above.

## Maintenance notes

- Known, deliberately unfixed: a font edit *and* a layout cycle in the same
  frame batch pays `arrange` twice, because `apply_layout`'s other callers do
  not refresh `font_applied`. One-off and rare; fold into the next `diff.rs`
  pass.
- Reviewers: check the ordering (stash → apply → restore → let the `changed`
  branch resolve), that `armed_hunk` is left cleared, and that nothing new
  allocates per frame — the font check must stay a reference comparison with
  the clone only inside the branch.
