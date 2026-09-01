# Plan 022: Move repository loads and refreshes off the terminal loop

> **Executor instructions**: Follow the plan step by step. Run every verification
> command and confirm the expected result before moving on. If a STOP condition
> fires, stop and report — do not improvise. Do not edit `plans/` in any way:
> the orchestrator owns the index. Do not push.
>
> **Drift check (run first)**: your worktree branches from
> `advisor/021-tui-discard-hunk-parity-guard` (which contains `eb888e1`, a
> `carry:` commit, and plan 021's work). Verify `git -C "$WT" log --oneline`
> shows that history and `git -C "$WT" status --short` is empty. The excerpts
> below are from that state; plan 021's discard verb is expected to exist in
> `hunk_verb`. Where the scrollbar-indicator refactor moved a `main.rs`
> anchor, the **Anchor refresh** block wins. If anything differs, STOP.

## Anchor refresh — 2026-08-28, after the scrollbar-indicator refactor landed

`main.rs` anchors shifted after the carried tree grew to the maintainer's
scrollbar refactor (decision 0027); content verified intact. Carried-tree
anchors: `drain_jobs` :2672, `refresh_stale` :2712, `open_diff` :2315 (inline
`acquire::acquire` :2333), the loop's `self.drain_jobs()` call :1304, `until`
:4634, `app_on_fake`'s acquire :4608. Where inline excerpts disagree, this
block wins; the approach, steps, invariants, and done criteria are unchanged.
`Screens::refresh` and its per-tenant arms sit just above `drain_jobs` in the
same file, unchanged in shape.

## Status

- **Priority**: P1
- **Effort**: L
- **Risk**: HIGH (concurrency, ordering, test determinism)
- **Depends on**: plans/021-tui-discard-hunk-and-parity-guard.md (branch basis)
- **Category**: perf
- **Planned at**: commit `eb888e1f3f3733b6f2020e2877c9d1fa68094f07`, 2026-08-28

## Why this matters

The terminal re-acquires every stale repository pane **synchronously on the
input loop** after each finished write, and loads a commit's diff synchronously
on Enter. Measured window costs for the same work are 48–370 ms per refresh
(one git read + one prepare), and the TUI's refresh is *sequential over all
panes* — so a stage on a repository with files, branches, stashes and commits
panes stalls key handling and repainting for the sum of those reads. The window
solves this with background loads guarded by a request id; the terminal's own
comment calls the synchronous version an accepted tradeoff ("a second terminal
background protocol is not M-sized work"). This plan builds that protocol.

## Current state

- `tui/src/main.rs` (carried tree; plan 021 adds the discard arm inside
  `hunk_verb`):
  - `Screens::refresh` `:372-509` — per-tenant, fully synchronous:
    re-acquires and calls `view.replace(...)` inline, bumps the tenant's
    generation, returns `Some(Result)` (the whole job, load and apply, on the
    loop thread).
  - `App::drain_jobs` `:2656-2683` — on every `Finished` bumps `self.generation`
    then calls `refresh_stale`, and composes the status message
    (`write error · refresh error`).
  - `App::refresh_stale` `:2696-2716` — clones the repo handle, walks every
    registered pane, calls `pane.refresh(...)`, keeps the **first** error,
    never skips later panes after one fails.
  - `App::open_diff` `:2299-2348` — calls
    `acquire::acquire(View::Diff, &source, &self.host, Some(repo.as_ref()))`
    inline on Enter.
  - The loop `App::run` `:1216-1304` calls `self.drain_jobs()` (`:1288`) before
    drawing.
  - Tests: every async assertion polls inside
    `until(Duration::from_secs(2), || { app.drain_jobs(); … })` — e.g.
    `staging_refreshes_focused_and_unfocused_panes` (`:6236`),
    `two_failing_panes_surface_the_first_one_s_error` (`:6306`),
    `a_failed_stash_read_opens_as_unavailable_and_recovers_on_refresh`
    (`:7095`), `stash_finishes_refresh_every_registered_repository_pane`
    (`:7015`). `until` is `:4557`.
- The window's model to mirror — `shell/src/main.rs`:
  - `drain_jobs` `:2710-2762` (events only; refresh spawned per finish),
  - `refresh_stale` `:2764-2811` — collects per-pane `Refresh { load, apply }`,
    runs `load` on `cx.background_spawn`, applies on the loop with a
    generation + `refresh_id` guard, keeps `refresh_error`'s first per batch,
  - `schedule_main_diff` `:3315-3425` — the diff load with a monotonic
    `request` counter; late loads dropped when superseded.
- Threading facts: `gitten_git::Handle` is `Clone` and the `Repo` trait is
  `Send + Sync` (`git/src/lib.rs:338`); `Host` is `Clone`; every snapshot type
  (`Vec<Commit>`, `Vec<FileDiff>`, `Vec<Stash>`, `Status`,
  `tui/src/main.rs::BranchReads`) is plain data.

## Commands you will need

| Purpose | Command | Expected |
|---|---|---|
| Tests | `cargo test -q -p gitten-tui` (with `CARGO_TARGET_DIR=/Users/chus/Projects/gitten.wt/tui/target`) | all pass |
| Lint | `cargo clippy -q -p gitten-tui --all-targets --locked -- -D warnings` | no warnings |
| Format | `cargo fmt --check` | clean |
| Scope | `git -C "$WT" status --short` | only `tui/src/main.rs` (plus, if unavoidable, a doc line in `tui/src/lib.rs`) |

## Scope

**In scope**: `tui/src/main.rs`.

**Out of scope** (do NOT touch):
- `app/src/jobs.rs` — the write `Runner` is correct as is; the loader is a
  TUI-local sibling, not an `app` change.
- `shell/`, `core/`, `git/` — this is the terminal client's protocol only.
- Debouncing cursor-follow preview — the TUI's diff opens on an explicit key;
  no debounce machinery is wanted (the window needs it because it follows the
  cursor; this plan does not add cursor-follow).

## Git workflow

- Branch: `advisor/022-tui-async-loads`, created **from**
  `advisor/021-tui-discard-hunk-parity-guard` (the orchestrator bootstraps this
  after 021's verdict).
- Commit per step; messages like
  `tui: the refresh wave loads off the loop`.
- Do NOT push. Do NOT touch `plans/`.

## Approach

A `LoadJob` is `(token, work)` where `work: Box<dyn FnOnce() -> LoadOutcome + Send>`
runs on one new worker thread ("gitten-loads") and `LoadOutcome` carries the
token plus an enum of per-tenant snapshots (or a diff-open result). The loop
drains outcomes and applies them under the same guards the window uses:
a monotonic refresh-batch id and a monotonic diff-request id, both checked at
apply time so a superseded result is dropped, never applied.

## Steps

### Step 1: Split the tenant refresh into load and apply halves (behavior-neutral)

Rename/replace `Screens::refresh` with:

- `fn acquire_snapshot(&self, target: Generation, host: &Host, repo: &gitten_git::Repo) -> Option<Result<Snapshot, String>>`
  — pure, blocking, `Send` result. `enum Snapshot { Commits(Vec<Commit>),
  Diff(Vec<FileDiff>), Stashes(Vec<Stash>, String /*described*/),
  Files(Status, String /*described*/), Branches(BranchReads) }` — exactly what
  each current arm computes before touching its view.
- `fn apply_snapshot(&mut self, snap: Snapshot, target: Generation, host: &Host) -> Result<(), String>`
  — the current arms' second halves verbatim (`view.replace(...)`, label,
  generation bump, `Err` passthrough).

Keep a thin synchronous `fn refresh(&mut self, ...) -> Option<Result<(), String>>`
wrapper (acquire + apply) so this step changes nothing observable.

**Verify**: `cargo test -q -p gitten-tui` → all pass unchanged.

### Step 2: The loader thread

In `main.rs`, modelled on `app::jobs::Runner`'s shape (mpsc in, mpsc out, one
worker named `"gitten-loads"`, `Drop` stops it):

```rust
struct Loader { /* … */ }
impl Loader {
    fn submit(&self, token: u64, job: Box<dyn FnOnce() -> LoadOutcome + Send>) -> bool;
    fn try_next(&self) -> Option<LoadOutcome>;
}
```

`LoadOutcome` is `{ token: u64, kind: LoadKind }` with
`enum LoadKind { Pane { name: String, batch: u64, target: Generation, result: Result<Snapshot, String> }, Diff { request: u64, result: Result<(Source, Loaded), String> } }`
(shapes may be adjusted; the invariants below may not).

**Verify**: `cargo build -q -p gitten-tui` → exit 0; a small unit test drives
`submit`/`try_next` to round-trip one outcome (the worker is fast; poll with
`until`, `:4557`).

### Step 3: Refresh through the loader

Rework `App`:

- Add `loads: Loader`, `refresh_batch: Option<RefreshBatch>` where
  `RefreshBatch { id: u64, pending: usize, first_error: Option<String> }`, and
  a `refresh_id: u64` counter.
- `refresh_stale(target)` becomes spawn-only: bump `refresh_id`, build the
  batch, and for every stale pane submit a closure that calls
  `pane.acquire_snapshot(...)` with cloned `Handle`/`Host` and returns the
  `LoadKind::Pane` outcome. **Every pane is attempted** exactly as today.
- `App::pump(&mut self)` — drains job events (current `drain_jobs` body,
  minus the inline refresh: a `Finished` bumps the generation and spawns the
  wave), then drains `loads.try_next()`, applying each outcome:
  - drop if `batch != self.refresh_id` or `target < self.generation`
    (superseded);
  - else `pane.apply_snapshot(...)`; on error keep the batch's first error;
  - when `pending` reaches 0, surface the composed message exactly as
    `drain_jobs` does today (`write error · refresh error` order, the job's own
    `done` sentence on a clean write).
- The loop calls `self.pump()` where `:1288` calls `drain_jobs()` today.

Invariants that existing tests pin and must survive verbatim: every registered
pane refreshed to the finish's generation (focused, hidden, unfocused alike);
first refresh error stands; a failed tenant keeps its old generation while
later tenants still reach the target; refusal and success both stale every
pane.

**Verify**: `cargo build` → 0. (Tests still call `drain_jobs` at this point;
they are migrated in step 5, so full-green comes there.)

### Step 4: open_diff through the loader

Rework `App::open_diff` (`:2299-2348`):

- Keep every existing gate (no commit selected, no repository, refusal
  messages) verbatim.
- On the load path: bump a `diff_request: u64` counter, store
  `self.diff_load = Some((req, source.clone()))`, set `self.loading = true`,
  and submit a closure that calls
  `acquire::acquire(View::Diff, &source, &self.host_clone, Some(repo))`
  (clones moved into the closure) returning `LoadKind::Diff`.
- `pump` applies a `Diff` outcome only when `request == self.diff_request`
  current; then exactly the old success path: build `Diff::new`, `set_bar`,
  `ensure_geometry`, resize to the pane rectangle, `panes.register(...)`,
  `gesture = None`, `sync_modes()`; clear `loading`. Older requests are
  dropped, never applied.
- Draw the pending state: while `self.loading`, put the word `loading diff` on
  the status row (faint ink, after the normal status — mirror the window's
  `loading` band; `self.message` is per-keypress and must not be used for this).

**Verify**: `cargo build -q -p gitten-tui` → 0.

### Step 5: Migrate the tests to `pump`

- Make `pump` callable from tests (it is a normal method; `drain_jobs` may
  remain as a private helper pump calls).
- Mechanically replace every `app.drain_jobs()` inside `until(…)` closures with
  `app.pump()` (grep `drain_jobs` under `#[cfg(test)]` — roughly twenty sites
  across the staging/stash/branch/parity test modules).
- Assertions that read `app.message` immediately after a finish must now wait
  for the **batch** to complete, not just the generation to advance: change the
  closure condition to
  `app.pump(); app.generation > gen && app.refresh_batch.is_none()` (expose a
  `#[cfg(test)] fn refresh_settled(&self) -> bool` if cleaner), keeping every
  expected message value identical. The two-panes error test
  (`:6306`) must still end with `app.message == "the log read failed"`; the
  composed `write · refresh` test (`:7015`) must still end with
  `"the stash pop refused · the log read failed"`.
- `enter_replaces_and_focuses_a_persistent_diff_and_back_returns` (`:5532`) and
  the hunk tests that depend on an open diff must pump until `!app.loading`
  before asserting the diff pane's contents.

**Verify**: `cargo test -q -p gitten-tui` → **all pass, no assertion weakened**.
If any test's expected value has to change, that is a STOP, not an edit.

### Step 6: Full gates

**Verify**:
- `cargo test -q -p gitten-tui` → all pass
- `cargo clippy -q -p gitten-tui --all-targets --locked -- -D warnings` → clean
- `cargo fmt --check` → clean
- `grep -n "acquire::acquire\|acquire::reacquire" tui/src/main.rs` → both appear
  only inside closures submitted to the loader (never inline in
  `dispatch`/`press`/`hunk_verb` paths)
- `git -C "$WT" status --short` → only `tui/src/main.rs`

## Test plan

- Loader round-trip unit test (step 2).
- All existing async-behaviour tests migrate to `pump` with identical final
  assertions — that *is* the regression suite for this plan (the contracts:
  all-panes refresh, first-error-stands, refusal-stales, hidden-tenant refresh,
  failed-tenant-keeps-generation, diff-open replaces and focuses).
- New: a supersede test — submit diff load A, then `open_diff` again for commit
  B before A lands (drive by pumping zero times between), pump until settled,
  assert the pane shows B's source and A never applied.
- New: a late-batch test — after wave 1's loads are in flight, a second finish
  bumps the generation; pump until settled; assert wave 1 outcomes whose target
  is stale were dropped (tenant generation equals the newest target).

## Done criteria

- [ ] all gates exit 0
- [ ] no inline `acquire::acquire`/`acquire::reacquire` call outside loader
      closures (grep gate above)
- [ ] `App::run` no longer calls a blocking per-pane refresh; the loop body's
      only blocking calls are `Term::poll`, `screen.flush`, and `pump`'s
      non-blocking drains
- [ ] every migrated test keeps its original final assertion values
- [ ] scope: only `tui/src/main.rs` (plus at most a doc line in
      `tui/src/lib.rs`)

## STOP conditions

Stop and report if:
- A `Snapshot` variant cannot be made `Send` (it should be plain data; if
  something in the acquisition path is not, name it).
- Any existing test cannot keep its expected values (adapt *timing*, never
  assertions — and report the test rather than weakening it).
- Applying an outcome needs more than the pane's own `apply_snapshot` (e.g.
  cross-pane coordination the window doesn't have either).
- The loader and the write `Runner` need to share a thread for correctness —
  they must not; reads and writes are independent by design.

## Maintenance notes

- The loader is deliberately **serial**. If profiling shows one load queuing
  behind another visibly, the next slice is a two-thread pool behind the same
  channel — do not parallelize preemptively.
- The supersede guards (`refresh_id`, `diff_request`) mirror the window's
  `refresh_id`/`request`; keep both sides' comments pointing at each other so a
  fix to one lands in the other.
- Reviewer focus: the apply-time guards (batch id, target generation, request
  id) and the batch-completion message composition. Every dropped-late-result
  path must be unreachable from the user's point of view except as "nothing
  happened yet".
