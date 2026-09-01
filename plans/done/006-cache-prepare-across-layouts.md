# Plan 006: Toggling the diff layout does not re-run the expensive prepare pass

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result. If a "STOP conditions"
> item occurs, stop and report — this plan touches a shared trait and has a real
> chance of rippling; the STOP conditions are how you avoid improvising a design.
> When done, update this plan's row in `plans/README.md`.
>
> **Drift check (run first)**:
> `git diff --stat 3a8b347..HEAD -- shell/src/views/diff.rs core/src/prepared.rs`
> If either changed, compare the excerpts against the live code; on a mismatch, STOP.

## Status

- **Priority**: P2
- **Effort**: M
- **Risk**: MED
- **Depends on**: none (but see 007 — landing shell tests in CI first means the
  reflow/assemble tests that guard this run on every change)
- **Category**: perf
- **Planned at**: commit `3a8b347`, 2026-08-24

## Why this matters

Pressing `s` cycles the diff layout (unified ↔ side-by-side). Each press calls
`assemble`, which unconditionally re-runs `prepare` — the intraline word-diff and
the syntax highlighting of **every line of every file**. But the files, the
differ, and the theme have not changed; only which `Rows` implementation consumes
the result. The code's own comment prices this at "8 ms on a typical diff and
289 ms on the pathological fixture, once, on a keystroke". A 289 ms UI stall on
the single most-pressed key in the diff view is 100% recomputation of an answer
that is a pure function of inputs that did not change.

The comment also states the correct shape of the fix: "Making it instant would
mean the row implementations sharing their text behind a refcount instead of
owning it, and that is a change to `prepared::Line`." This plan does exactly the
narrower version: cache the `Prepared` result and reuse it across layout toggles,
recomputing only the per-presentation claim + order-table work.

## Current state

`shell/src/views/diff.rs`:

- `cycle_layout` (line 983) → `apply_layout(index, host)` (line 998) → `assemble`
  (line 1038) on every `s` press.
- `apply_layout` (lines 998-1022) rebuilds `order`, `renderers`, `widest`, `load`
  from `assemble(&self.files, host, &self.layouts, index)`.
- `assemble` (line 1038):

  ```rust
  fn assemble(files: &[FileDiff], host: &Host, layouts: &Layouts, current: usize) -> Built {
      let t = std::time::Instant::now();
      let mut renderers = match layouts.0.get(current) { ... };
      // ...
      let Prepared { files: prepared, intraline, syntax, threads } =
          prepare(files, &host.syntax, MAX_LINE_CHARS);   // <-- re-run every toggle
      let file_count = prepared.len();
      for f in prepared {
          let owner = renderers.iter().enumerate().rev()
              .find(|(_, r)| r.claims(&f.path)) ...;
          // hands `f` (by value) to the owning renderer's build
      }
      // builds order table, widest, load string
  }
  ```

- `prepare` is defined in `core/src/prepared.rs` and returns
  `Prepared { files: Vec<File>, intraline: Duration, syntax: Duration, threads: usize }`.
  It is a pure function of `(files, &host.syntax, MAX_LINE_CHARS)`.
- The renderers consume each `File` **by value** in the `for f in prepared` loop
  (via `Rows::build`), which is why the result cannot simply be borrowed today.

The `Diff` view struct holds `files: Vec<FileDiff>` (the input) and is rebuilt via
`swap`/`replace` when the underlying diff actually changes (grep `fn swap` and
`fn replace` in the same file).

## Commands you will need

| Purpose | Command | Expected |
|---------|---------|----------|
| Shell tests (headless GPUI) | `cargo test -q -p gitten-shell` | `test result: ok` |
| Core tests | `cargo test -q -p gitten-core` | `test result: ok` |
| Bench the pathological case | `./dev --release dump diff --fixtures` then toggle, or `cargo run -q -p gitten-core --example bench --release` | prints `prepare` timing |
| Lint | `cargo clippy -p gitten-shell --all-targets` | no warnings |

## Scope

**In scope**:
- `shell/src/views/diff.rs` — cache the prepared files on the `Diff` view; reuse
  in `apply_layout`; invalidate in `swap`/`replace`.
- `core/src/prepared.rs` — only if the shared representation needs an `Rc`/clone
  helper; prefer to keep the change shell-side.

**Out of scope**:
- Changing `Rows::build`'s signature across `core`, `tui`, and `web` — if the
  fix seems to require it, that is the design-heavy version the maintainer
  deferred. STOP and report instead (see STOP conditions).
- The `prepare` algorithm itself (intraline, syntax) — leave it.
- The blob-OID diff cache (that is PERF-04, a separate larger item).

## Git workflow

- Branch: `advisor/006-cache-prepare-across-layouts`
- Commit per logical step; imperative messages.

## Steps

### Step 1: Measure the baseline

Confirm the stall exists before changing anything. Run the pathological fixture
and note the `prepare` timing that appears on a layout toggle (`GITTEN_STATS=1`
is the default in dev; the `load` string carries it). Record the number.

**Verify**: you have a before-number for `prepare` on `--fixtures` toggle.

### Step 2: Split `assemble` into "prepare" and "arrange"

Factor `assemble` so the `prepare(...)` call is separable from the
claim + order-table work. Target shape:

- `fn prepare_files(files, host) -> Rc<Prepared>` — just the `prepare` call,
  wrapped in `Rc`.
- `fn arrange(prepared: &Rc<Prepared>, host, layouts, current) -> Built` — the
  renderer selection, the per-file `build`, the order table, `widest`, `load`.

The catch is the `for f in prepared` loop consumes `File` by value. To arrange
from a shared `Rc<Prepared>` without re-preparing, the per-file `build` must take
`&File`. Check `Rows::build`'s signature (grep `fn build` in
`shell/src/views/diff.rs` and `core/src/rows.rs`). If **shell's own** `Rows`
trait/impls can take `&File` with only shell-local edits, do that. If `build`
is `core`'s shared `Present`/`Rows` trait consumed by `tui` and `web` too,
changing it is out of scope — see STOP conditions; instead clone each `File` out
of the cached `Rc<Prepared>` (a `File` clone re-allocates its token/span boxes
but does **no** intraline diff or syntax scan, so it is still far cheaper than a
full `prepare`). Measure both if unsure and take the cheaper that stays in scope.

**Verify**: `cargo build -p gitten-shell` → exit 0; behavior unchanged (manual
toggle still works).

### Step 3: Hold the prepared result on the `Diff` view and reuse it

Add a field to `Diff`, e.g. `prepared: Rc<Prepared>`, populated when the view is
built and when `swap`/`replace` changes `files`. In `apply_layout`, call
`arrange(&self.prepared, host, ...)` instead of `assemble(&self.files, ...)`.

Invalidate (re-`prepare_files`) in exactly the places `self.files` changes —
`swap`/`replace` — and, importantly, when `host.syntax` changes (a theme/syntax
config reload). Check how config reload reaches this view (grep `reload` /
`config::host` in `shell/src`); if the syntax config can change without going
through `swap`, key the cache on it or re-prepare on reload. If you cannot
cheaply detect a syntax-config change, re-prepare on config reload
unconditionally (reload is rare; a toggle is hot).

**Verify**: `cargo build -p gitten-shell` → exit 0.

### Step 4: Confirm the toggle no longer re-prepares

Run the pathological fixture again and toggle the layout. The `prepare` timing on
a toggle should now be ~0 (or absent), while the first load still pays it.

**Verify**: the layout-toggle `prepare` time is dramatically lower than the
Step 1 baseline; the first-load time is unchanged.

### Step 5: Tests

Add/extend a shell test in `shell/src/views/diff.rs`'s `#[cfg(test)]` block. There
are existing headless tests for the order table and reflow (grep
`the_order_table_grows_and_keeps_the_line_you_were_reading` around line 3297).
Add a test that:
- Builds a `Diff` over a fixture with ≥2 layouts.
- Calls `apply_layout` to toggle, and asserts the resulting `order`/rows are the
  same as building fresh at that layout (correctness: the cache must not change
  output).
- If feasible, asserts the cached `Rc<Prepared>` pointer is unchanged across a
  toggle (`Rc::ptr_eq`) but changes across a `swap` (proves the cache is reused
  and invalidated correctly).

**Verify**: `cargo test -q -p gitten-shell` → all pass including the new test.

## Test plan

- New shell test: a layout toggle produces the same rows as a fresh build at that
  layout (cache is transparent), and the prepared result is reused across toggles
  but rebuilt on `swap`.
- Verification: `cargo test -q -p gitten-shell` and `cargo test -q -p gitten-core`
  → all pass.

## Done criteria

ALL must hold:

- [ ] `cargo test -q -p gitten-shell` exits 0; new cache test present and passing
- [ ] `cargo test -q -p gitten-core` exits 0 (unchanged if `core` untouched)
- [ ] A layout toggle on `--fixtures` no longer shows a full `prepare` cost
      (measured before/after)
- [ ] `Rows::build`'s shared signature in `core`/`tui`/`web` is unchanged, OR the
      change was confined to shell (confirm with `git diff --stat`)
- [ ] `cargo clippy -p gitten-shell --all-targets` clean; `cargo fmt --check` clean
- [ ] `plans/README.md` status row updated

## STOP conditions

Stop and report (do not improvise) if:

- Reusing the prepared result requires changing `core`'s shared `Present`/`Rows`
  `build` signature (which `tui` and `web` also implement). That is the larger
  design change the maintainer deferred; report the exact trait and its impls so
  the maintainer can decide, rather than editing three clients.
- Cloning a `File` out of the cache turns out to be as expensive as re-preparing
  (measure it) — report; the answer is then the trait change, which is out of scope.
- The cache produces different rows than a fresh build in the test — the
  invalidation is wrong; report rather than loosening the assertion.

## Maintenance notes

- The cache key is implicitly `(self.files, host.syntax)`. Any future input to
  `prepare` (a new differ override that affects intraline, say) must join the
  invalidation set or the toggle will show a stale diff.
- This is the cheaper half of PERF-04 (the blob-OID diff cache). If that larger
  cache lands, this view-level cache still helps for the pure-presentation toggle
  and they compose.
- A reviewer should scrutinize the invalidation in `swap`/`replace` and on config
  reload — a missed invalidation is a silently-stale diff, which is worse than
  the stall this removes.
