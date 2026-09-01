# Plan 014: Stage and unstage the selected hunk from the terminal diff

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving on. If a
> "STOP conditions" item occurs, stop and report; do not improvise. Do not
> update `plans/README.md` — the integrator owns the index for this pass.
>
> **Drift check (run first)**: `git diff --stat 67fee3d..HEAD -- core/src/rows.rs app/src/acquire.rs tui/src/diff.rs tui/src/rows.rs tui/src/split.rs tui/src/main.rs tui/src/term.rs`
> If any listed file changed, compare the facts and named functions below with
> live code before editing. A mismatch in command names, hunk addressing,
> `App::repo`, or refresh ownership is a STOP condition.

## Status

- **Priority**: P1
- **Effort**: M
- **Risk**: MEDIUM
- **Depends on**: none
- **Category**: terminal feature / shared-seam completion
- **Planned at**: commit `67fee3d`, 2026-08-27
- **Known scope boundary**: selected-hunk stage/unstage is implementable now;
  a discoverable whole-file action is not, because the terminal has no Files
  screen and the shared default for `files.stage` is deliberately scoped to
  mode `files`. The follow-up must add that screen rather than give the diff
  mode a second, terminal-only meaning for `space`.

## Why this matters

The window can already move one hunk between the working tree and index through
the full write pipeline. The terminal draws the same prepared hunks, resolves
the same configured command names, and holds the same repository `Handle`, but
its `Screens::run` stops at navigation and presentation commands. Consequently
the shared help can advertise `space`/`u` while those resolved commands end in
`"diff.stage-hunk does nothing here"` or its unstage twin.

The fix is not a terminal-specific git command. It is the missing client
adapter: map the cursor's visual row back to the loaded `Hunk`, call
`gitten_core::patch::emit`, construct `gitten_app::verbs::Write`, submit it to
the shared job runner, and re-acquire repository-backed screens when the
runner's invalidation generation advances.

This plan deliberately does **not** invent a whole-file shortcut in the diff.
`files.stage` and `files.stage-all` exist, with defaults in the `files` mode,
but `Screens` has no Files variant. A Files screen is the correct separately
planned route to whole-file staging, including untracked files whose mode a
hunk patch cannot carry.

## Commands you will need

| Purpose | Command | Expected |
|---|---|---|
| Core geometry | `cargo test -p gitten-core rows::tests` | new hunk-span tests pass |
| Headless terminal + app | `cargo test -p gitten-tui -p gitten-app` | all pass; no tty is entered |
| Lint | `cargo clippy -p gitten-tui -p gitten-app -p gitten-core --all-targets -- -D warnings` | no warnings |
| Format | `cargo fmt --check` | clean |
| Scope | `git status --short` | only the files named under Changes are modified |

Do not run `./dev tui`, `./dev desktop`, or any command that enters a tty or
opens a window. A test frame is built in memory with `Screen`; that is enough.

## Scope

In scope:

- Shared logical-row-to-hunk geometry in dependency-free `core`.
- Terminal dispatch for `diff.stage-hunk` and `diff.unstage-hunk`.
- Submission through `gitten_app::verbs::Write` and `gitten_app::jobs::Runner`.
- Generation-driven re-acquisition of every repository-backed screen in the
  terminal stack, preserving useful viewport anchors.
- Honest refusals for non-working-tree sources, absent repositories, cursor
  rows outside hunks, empty patches, untracked file creations, queue shutdown,
  and git/apply failures.

Out of scope:

- A terminal Files screen, `files.stage`, `files.stage-all`, discard, line-level
  staging, multi-hunk selection, or confirmation UI.
- Any new key, command name, or `gitten.toml` field.
- Any shell, web, git-backend, patch-emission, or dependency change.
- Direct `Command::new("git")` in production or test code. The real-repository
  round trip extends the existing `app/src/acquire.rs` `Scratch::git` fixture;
  the write under test itself must enter through `Write` and the `Repo` handle.

## Baseline facts (provenance)

### Shared commands and keys already exist

- `core/src/command.rs` `Keymap::builtin` binds `space` to
  `diff.stage-hunk` and `u` to `diff.unstage-hunk` in mode `diff`. It also binds
  `space` to `files.stage` and `a` to `files.stage-all` in mode `files`.
- `core/src/command.rs` `Commands::builtin` registers all four names:
  `diff.stage-hunk`, `diff.unstage-hunk`, `files.stage`, and
  `files.stage-all`.
- Therefore the first change layer is **not** command or config. The terminal
  already reads `Host::keys` in `tui/src/main.rs` `App::press`; user overrides
  under `[keys.diff]` reach it through the same `gitten.toml`. Adding a binding
  in `tui` would fork the shipped keyboard.
- `tui/src/term.rs` `translate` maps crossterm `KeyCode::Char(' ')` and
  `KeyCode::Char('u')` to `Code::Char`, then `Key::new`. No terminal protocol
  change is needed. `translate_event` ignores `KeyEventKind::Release`, which is
  the kitty-protocol guard against firing either command twice. Keep
  `tui/src/term.rs` unchanged; add only a resolver assertion if existing test
  coverage does not already name both keys.

### Patch emission takes a path and hunks, not rows or OIDs

- `core/src/patch.rs` `emit(path, chosen)` takes `&str` plus `&[&Hunk]` and
  returns patch bytes. It recomputes coordinates from `DiffLine.old_no`,
  `DiffLine.new_no`, and `LineKind`; it receives neither a visual-row range nor
  blob OIDs. Empty selection or line-empty hunks return an empty `Vec<u8>`.
- `core/src/patch.rs` `emit` names a side `/dev/null` when the chosen lines have
  no rows on that side. This describes content but not a file mode.
- `git/src/lib.rs` `Pair` carries `old_oid`/`new_oid`, but
  `gitten_git::diff` turns it into `core::FileDiff`, whose definition in
  `core/src/lib.rs` contains only `path` and `hunks`. By the time the terminal
  sees a diff, absence of an old OID is not observable. Do not add OIDs to
  `FileDiff` for this feature.
- `git/src/lib.rs` `Repo::stage_patch` documents the exact unsupported case:
  a pure-addition patch for a brand-new file lacks the file mode needed by
  `git apply --cached`. The binary implementation's `stage_patch` calls
  `run_stdin` with `apply --cached --whitespace=nowarn -`; `unstage_patch`
  adds `--reverse`. `run_stdin` waits synchronously for that child.
- Follow the window's `shell/src/main.rs` `DevShell::act_hunk`: only when a
  hunk has no old-side line number, read `Repo::status` and compare its path
  against `Status::untracked`. Refuse an untracked creation with the exact
  terminal message:

  ```text
  that hunk adds a new file — stage or unstage it whole from the files pane
  ```

  Do not infer “untracked” merely from all-addition geometry: at context zero,
  a tracked mid-file insertion has the same shape. A failed status read is not
  proof of creation; emit the patch and let git return its own refusal, as the
  window does.
- `app/src/verbs.rs` `Write::stage_patch(&Handle, Vec<u8>)` and
  `Write::unstage_patch(&Handle, Vec<u8>)` return `Result<Write, String>` and
  refuse empty input as `"an empty patch stages nothing"` and
  `"an empty patch unstages nothing"`. Whole-file fallbacks already exist as
  `Write::stage` and `Write::unstage`, but using them needs an exact status path
  and a Files-screen side (staged versus unstaged). This plan must not guess
  either from `FileDiff.path`, which can be a display label.

### The terminal lacks the row-to-hunk address

- `core/src/rows.rs` `Flat` currently exposes logical `Row` values and
  `Entry { path, adds, dels, row }`, where `row` is only the file-header row.
  It exposes no file index on a row, no hunk index, and no hunk logical-row
  range. `Ordered::order` and `RowRef::logical` map visual rows back to an owner
  and logical row, but stop there.
- `tui/src/diff.rs` `Diff` retains the original `Vec<FileDiff>` and its current
  `RowRef`, so no patch data is missing; only the address from logical row to
  `(file index, hunk index)` is missing.
- The window solved that presentation-dependent address with
  `shell/src/views/diff.rs` `HunkMap::record`, `HunkMap::at`,
  `Rows::hunk_at`, and `Diff::current_hunk`; split implements the same method in
  `shell/src/views/split.rs` `SplitRows::hunk_at`. The hunk's span is in logical
  rows, so wrapping does not alter it. The terminal needs the same seam, placed
  in `core::rows` because both terminal presentations need it and `core` can
  model row geometry without knowing how either client draws.

### Writes, generations, and blocking behavior

- `app/src/jobs.rs` `Runner::new` owns a FIFO worker. `Submitter::submit`
  returns without waiting; the worker calls `Job::run`, catches panics, and
  emits `Event::Finished` with a monotonically advancing `Generation` for both
  success and refusal. `Runner::try_next` is non-blocking.
- Consequently the planned `git apply` is synchronous **inside the shared job
  worker**, not on the terminal event thread. Calling `Job::run` directly from
  `App::dispatch` would freeze input and would bypass the only code allowed to
  mint invalidation generations; that is a STOP-worthy implementation drift,
  not an accepted shortcut.
- The accepted terminal tradeoff is narrower: after a finish, re-acquisition
  is synchronous on the terminal loop. `shell/src/main.rs` documents diff
  acquisition plus preparation in `DevShell::load_diff` as roughly 48–370 ms
  (40–120 ms acquisition plus 8–250 ms prepare); small working trees are
  usually one process-floor-scale read, but large/prose diffs can visibly pause.
  Keep this M-sized plan synchronous on refresh rather than introduce a second
  terminal background protocol. The screen remains drawn while blocked.
- `tui/src/main.rs` `TICK` is 150 ms and `App::run` redraws before each
  `Term::poll(TICK)`. Drain job events before drawing on every iteration. With
  no input, a completed write is noticed within at most one tick; with input,
  the loop wakes earlier. The tick does not make re-acquisition non-blocking—it
  only bounds completion-notice latency. State this plainly in code comments.
- `shell/src/main.rs` `DevShell::drain_jobs` advances its generation on every
  finish and calls `DevShell::refresh_stale`; `Screen::refresh` uses
  `gitten_app::acquire::reacquire`, and `DevShell::refresh_stale` includes panes
  plus the main diff. The terminal must likewise refresh every repository
  screen in `App::stack`, not only the visible top.

### Window comparison and deliberate terminal omissions

The window's hunk flow touches these shell files:

- `shell/src/dispatch.rs`: `translate` converts a GPUI keystroke to shared
  `Key` candidates. Omit from terminal changes because `tui/src/term.rs`
  `translate` already converts space/u correctly and already handles kitty
  release events.
- `shell/src/main.rs`: `DevShell::run_command` routes the shared name;
  `DevShell::act_hunk` validates source/status, emits the patch, builds `Write`,
  and submits it; `DevShell::drain_jobs`, `DevShell::refresh_stale`, and
  `Screen::refresh` propagate generations and re-acquire. This behavior belongs
  in `tui/src/main.rs`, the terminal assembly, not in a view.
- `shell/src/views/diff.rs`: `Rows::hunk_at`, `HunkMap`,
  `Diff::current_hunk`, and `Diff::replace_prepared` provide geometry and
  position-preserving replacement. Omit shell edits: put the reusable geometry
  in `core/src/rows.rs`, then consume it in `tui/src/diff.rs`; migrating the
  already-working desktop map is a separate no-behavior-change cleanup.
- `shell/src/views/split.rs`: `SplitRows::hunk_at` maps its paired row shape to
  a hunk. Omit shell edits because the terminal's split implementation will use
  the new core map; desktop behavior is already correct.

No shell config file participates. Both clients receive `Host::keys` from
`gitten_app::config`, so there is nothing window-local to copy.

## Approach

1. Add a dependency-free logical hunk-span map to `core::rows`, keyed by file
   index and hunk index, and default `Present::hunk_at` to no answer. Both
   terminal built-ins record their own presentation-specific row spans into
   that shared type.
2. Give terminal `Diff` two data operations: `current_hunk`, which turns the
   cursor's `RowRef` into the original loaded `Hunk`, and `replace`, which swaps
   refreshed `FileDiff`s while retaining layout/wrap and restoring cursor/top
   as far as the new row count permits.
3. Retain a `Source`, label, and generation beside each terminal screen. Add the
   shared `Runner`/`Submitter` to `App`; intercept the two hunk command names in
   `App::dispatch`, validate like the window, emit/build/submit, and never call
   a `Repo` writer directly.
4. Drain `JobEvent`s in the 150 ms loop. Every `Finished`, including errors,
   advances `App`'s target generation and re-acquires **all** stale
   repository-backed screens with `acquire::reacquire`. Apply each result even
   if another screen failed, then report the write error ahead of any refresh
   error.
5. Preserve the commits screen semantically by SHA and the diff screen
   positionally. The commit replacement should capture `Commits::current().sha`,
   rebuild graph rows, find the same SHA, then use `Viewport::go_to` and
   `Viewport::scroll_to`; if the SHA vanished, clamp the previous cursor/top.
   The diff has no stable hunk identity after a write, so use the window's
   numeric fallback: capture `Viewport::cursor`/`top`, then `set_len`, `go_to`,
   and `scroll_to`. These are the exact invariant-preserving helpers in
   `core/src/view.rs`; do not assign raw indices.

## Changes by layer

### Core

**`core/src/rows.rs`**

- Add a public, dependency-free hunk geometry type (for example `Hunks` plus a
  borrowed `HunkRef`) storing one entry per hunk: inclusive logical start,
  logical row count, file index, and hunk index. Use `usize` unless measured
  memory justifies checked narrowing; never silently truncate a large hunk.
- Implement `record(start, rows, file, hunk)` and `at(logical_row)` using the
  same sorted-span binary-search shape as the window's verified `HunkMap::at`.
  Refuse/ignore zero-length spans in a documented way and test boundaries.
- Add default `Present::hunk_at` returning `None`. This keeps extension
  presentations source-compatible and makes a presentation that draws no
  hunks honestly non-actionable.
- Have `Flat::push` record the file index and the logical range from each hunk
  header through its last line; expose it through `Present` for `Flat`-backed
  terminal rows. Do not change `RowRef`, wrapping, or add a dependency.

No change to `core/src/patch.rs`, `core/src/command.rs`, or `core/Cargo.toml`.

### App

**`app/src/acquire.rs` (tests only)**

- Extend the existing `Scratch`-repository test fixture with a named staging
  round trip. Set up one committed file with two distant edits, acquire the
  working-tree diff through `acquire`, emit one selected hunk, submit
  `Write::stage_patch` through `Runner`, wait for its `Event::Finished`, and
  assert generation 1 plus exactly that hunk in the cached diff. Re-acquire,
  emit the staged hunk, submit `Write::unstage_patch`, and assert generation 2
  plus an empty cached diff and the original working-tree edits still present.
- Reuse `Scratch::git` only for repository setup and read-only oracle commands;
  the stage/unstage actions under test must never call it.

No production app change is expected: `app/src/jobs.rs`, `app/src/verbs.rs`,
and `app/src/acquire.rs` already expose the required runner, writes, and
`reacquire`. If production app code must change to serve the terminal, STOP—the
client boundary was misunderstood.

### TUI

**`tui/src/rows.rs`**

- Forward the new `Present::hunk_at` from `TextRows` to its `Flat` map. Do not
  put repository data, patch bytes, or command names in the renderer.

**`tui/src/split.rs`**

- Add the shared core hunk-span map beside split's presentation-specific rows.
  In `SplitRows::build`, record the span starting at the hunk header and ending
  after the aligned pair rows; implement `Present::hunk_at` by forwarding to
  it. This is why the map cannot be inferred from `Flat`: split collapses a
  removal/addition pair onto one logical row.

**`tui/src/diff.rs`**

- Add `Diff::current_hunk() -> Option<(String, Hunk)>`: read the cursor's
  `RowRef`, ask its owner for `(file, hunk)`, then clone the matching hunk from
  retained `self.files`. Return `None` on a file header, empty diff, invalid
  extension geometry, or any out-of-range index.
- Add `Diff::replace(files, host)` that clears selection/drag state, rebuilds
  the current presentation without resetting layout/wrap, and restores the old
  cursor/top through `Viewport` helpers. Preserve horizontal shift only up to
  the refreshed bound; cancel any scrollbar grab.
- Add tests for unified and split rows, hunk header/middle/last row, wrapped
  segments resolving to one hunk, file headers returning none, malformed
  extension geometry returning none, and replacement clamping a vanished hunk
  while keeping a still-valid viewport position.

**`tui/src/commits.rs`**

- Add a replacement method used by invalidation refresh. Recompute lanes/draws,
  anchor the cursor by the selected commit SHA when it survives, restore top
  with `Viewport::scroll_to`, clamp when it does not, and clear stale mouse
  selection/drag/grab state. Preserve glyph choice and dimensions.

**`tui/src/main.rs`**

- Change each `Screens` entry to retain the `Source`, label, and last applied
  `Generation` beside its view. Seed them from `Started.view/source/loaded` and
  from the source created by `App::open_diff`. Keep `App::repo` as the verified
  `Option<(PathBuf, Handle)>`; do not open another handle.
- Add `Runner`, `Submitter`, and current `Generation` to `App`. Build them once
  in `App::new` and submit boxed `Write` jobs.
- Route `diff.stage-hunk` and `diff.unstage-hunk` in `App::dispatch` before
  `Screens::run`, because the action needs the source and repository while the
  view must remain drawing/input only. Match `DevShell::act_hunk`'s source
  refusals exactly:
  - non-empty repository revspec: `only the working-tree diff can act on hunks — this one is between commits`;
  - fixtures: `a fixture has no repository behind it`;
  - patch input: `a patch file has no repository behind it`;
  - absent handle: `no repository is open`;
  - cursor outside a hunk: `the keyboard is not on a hunk`;
  - untracked creation: the exact files-pane message stated above.
- Build bytes only with `gitten_core::patch::emit`, jobs only with
  `Write::stage_patch`/`Write::unstage_patch`, and submit only through the
  shared `Submitter`. Surface constructor errors and queue shutdown in
  `App::message`.
- Add `App::drain_jobs` and call it before each frame. On `Started`, show
  `running {name}`. On every `Finished`, retain its generation and refresh all
  stale repository screens, even when `outcome` is `Err`; a refusal may have
  left repository state. Preserve the write error if refresh also fails.
- Add a pure/headless helper for refreshing the stack so tests can inject a
  fake `Repo` and assert both a commits screen and diff screen re-acquire once.
  No `Term::enter` in a test.
- Add an in-memory `Screen` frame assertion: after a successful staged-hunk
  completion, the refreshed diff no longer draws that hunk and the cursor/top
  remain clamped and visible. Assert the status message on refusal as well.

**`tui/src/term.rs`**

- Expected untouched. Its existing `translate` and release filtering already
  carry space/u correctly. If a regression assertion is missing, add only the
  test that space/u resolve through `Keymap::builtin` in `diff` mode; do not add
  escape sequences or special-case commands.

No `tui/Cargo.toml` change is expected; `gitten-app`, `gitten-core`, and
`gitten-git` are already dependencies.

## Test list

1. **`flat_records_exact_hunk_logical_ranges`** — core fixture with two files
   and two hunks; assert header, first line, and last line map to the correct
   `(file, hunk)`, while file headers and the first row after a span do not.
2. **`hunk_lookup_survives_visual_wrapping`** — expand a wrapped logical line
   into multiple `RowRef`s and assert every segment resolves through its one
   logical row to the same hunk.
3. **`an_unaware_presentation_has_no_actionable_hunk`** — custom `Present`
   uses the default and returns none, proving extension compatibility.
4. **`the_terminal_finds_the_hunk_under_the_cursor_in_both_layouts`** — TUI
   unified and split fixtures; assert file headers return none and hunk
   header/body/tail return the original loaded `Hunk` and path.
5. **`refresh_replaces_a_diff_without_losing_a_valid_viewport`** — start below
   row zero, replace with a changed diff, and assert cursor/top use
   `Viewport` clamping rather than reset or out-of-bounds indices.
6. **`commit_refresh_anchors_by_sha`** — insert/remove commits above the cursor;
   assert a surviving SHA stays selected and a vanished one clamps safely.
7. **`shared_defaults_reach_terminal_dispatch`** — `Keymap::builtin` in mode
   `diff` resolves translated space/u to the two existing names; a
   config-overridden binding reaches the same dispatch with no local key table.
8. **`non_working_tree_and_untracked_hunks_are_refused_before_submission`** —
   fake repo/source table asserts exact messages and zero writes; include a
   context-zero tracked insertion to prove all-addition geometry alone is not
   refused.
9. **`every_finished_generation_refreshes_both_stacked_screens`** — fake
   repository, stacked commits + diff, one successful and one refused job;
   assert each finish advances generation and invokes `acquire::reacquire` for
   both screens, including the hidden commits screen.
10. **`a_hunk_stages_and_unstages_round_trip_in_a_throwaway_repository`** —
    existing app `Scratch`, real git repository with two separated edits;
    stage exactly one emitted hunk through `Write` + `Runner`, verify cached
    content and working tree, re-acquire, unstage through the mirror verb, and
    verify index/worktree restoration. No tty/window.
11. **`a_refreshed_frame_is_drawable_headlessly`** — in-memory `Screen` frame
    after replacement; assert selected-row ink remains within the body and no
    stale row is drawn.
12. Run `cargo test -p gitten-tui -p gitten-app`, the focused core tests,
    clippy, and format checks listed above.

## Stop conditions

Stop and escalate if any occurs:

- Any of the four command names or their documented default bindings no longer
  exists in `core/src/command.rs`; do not recreate it in `tui`.
- `FileDiff` has gained reliable status/OID identity, or `core::rows` already
  exposes hunk spans under another name; reconcile with that live seam rather
  than adding a second one.
- Hunk lookup requires presentation-specific text/paint inspection instead of
  a logical-row span. That indicates the `Present` seam is insufficient; do not
  special-case unified/split in `Diff`.
- Implementing the requested whole-file UX would require binding
  `files.stage` in mode `diff`, changing `diff.stage-hunk` to act on file
  headers, or adding a Files screen. Report the follow-up; do not fork command
  semantics or expand this plan.
- An untracked creation can only be detected by treating “no old line number”
  as proof. Keep the status read or stop; the context-zero counterexample makes
  the shortcut incorrect.
- The implementation calls `Job::run` on the terminal thread, directly calls a
  `Repo` writer, or adds `Command::new("git")`. Writers must remain
  `Write` jobs behind the retained `Handle` and shared runner.
- A `Finished` error does not carry a generation or `acquire::reacquire`
  rejects an empty post-write answer. Both are required for honest invalidation.
- Refreshing one stacked screen prevents the other from being attempted. A
  stale hidden commits screen is not acceptable.
- The real-git round trip cannot distinguish the selected hunk from its distant
  neighbour, or depends on ambient git identity/signing/configuration.
- Any test enters raw mode, grabs a tty, or opens a window.
- The change requires a new dependency, especially in `core`.

## Risks

- **Whole-file expectation remains unmet in this slice.** The command and verbs
  exist, but the terminal lacks the `files` mode that owns their default keys
  and staged/unstaged row semantics. This is an explicit follow-up/blocker, not
  a hidden partial implementation.
- **Refresh pauses interaction.** `git apply` itself runs on the shared worker,
  but terminal re-acquisition remains synchronous and can pause roughly
  50–370 ms on measured window workloads, longer on pathological repositories.
  The 150 ms tick bounds notice latency, not refresh cost. If this is
  unacceptable in measurement, stop and plan an async terminal refresh queue.
- **Numeric diff anchoring is approximate.** A staged hunk disappears, so its
  old semantic row has no exact successor. Preserving/clamping cursor and top
  is predictable and matches the window fallback, but may land on adjacent
  context. Do not invent fuzzy hunk matching in this plan.
- **Display labels are not always write paths.** Rename labels and lossy paths
  are why this plan emits patches exactly as the existing window does and does
  not reuse `FileDiff.path` for whole-file `Write::stage`/`unstage`.
- **Shared geometry can be misrecorded by extensions.** Default `None`, checked
  lookup, and malformed-map tests ensure a bad extension yields “not on a hunk”
  rather than indexing the wrong loaded hunk.
- **Error precedence can hide useful context.** Preserve the write refusal as
  primary, append at most one refresh failure, and still attempt every screen.
