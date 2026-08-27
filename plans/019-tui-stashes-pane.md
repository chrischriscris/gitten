# Plan 019: Register a first-class terminal stashes pane

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving on. If a
> STOP condition occurs, stop and report; do not alter the pane registry or
> invent terminal-only commands or keys. Do not update `plans/README.md`: the
> integrator owns the index.
>
> **Drift check (run first)**:
> `git diff --stat 7b7ec51..HEAD -- shell/src/views/stashes.rs shell/src/main.rs core/src/command.rs core/src/refs.rs app/src/acquire.rs app/src/verbs.rs tui/src/lib.rs tui/src/main.rs tui/src/panes.rs tui/src/stashes.rs`
> If any listed path changed, compare every cited symbol and behavior below
> against the live code before editing. A mismatch in `Panes<T>`, canonical
> placement, stash command names, `Write` signatures, `Runner` outcome handling,
> or registration-wide refresh is a STOP condition.

## Status

- **Priority**: P1
- **Effort**: S–M (the thinnest of the three repository-list panes: one flat
  list, four already-existing jobs, no sections, text prompt, branch topology,
  or new command vocabulary)
- **Risk**: MED
- **Depends on**: plan 016 only, already landed at this baseline; this plan must
  compile and pass independently of plans 017 and 018
- **Category**: terminal feature / client parity
- **Planned at**: commit `7b7ec51`, 2026-08-27
- **Confidence**: HIGH on the data, verbs, confirmation, registry integration,
  and refresh behavior; MED on ancillary-read failure copy until fixed-cell
  tests establish the quietest honest wording

## Why this matters

The terminal now has plan 016's extensible pane registry, canonical sidebar
order, focus routing, and wide/narrow geometry, but only `commits` and `diff`
register. The shared keymap already promises `5`/`stashes.focus`, the stash-mode
apply/pop/drop bindings, and `files.stash`; today the terminal answers the focus
key with `no stashes pane` and the verb names fall through.

This pass fills the existing tenant slot without reopening the foundation. A
repository-backed terminal launch gets a flat stash list, the selected row is
addressed through the existing `Write` constructors, and every job completion
re-reads the stack through the same registration-wide generation rail as the
other panes. Fixture and patch launches remain repository-free. No part of this
plan depends on a files or branches pane existing: `files.stash` is dispatched
now through its existing command name, so a future files tenant gains its
already-configured `s` binding simply by registering.

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Required headless gate | `cargo test -p gitten-tui -p gitten-app` | all existing and new tests pass; no raw mode, alternate screen, tty, or window |
| Format | `cargo fmt --check` | exit 0 |
| Lint | `cargo clippy -p gitten-tui -p gitten-app --all-targets -- -D warnings` | no warnings |
| Foundation regression | `cargo test -p gitten-core` | shared keymap, refs, viewport, rows, and diff tests remain green |
| Full repository gate | `./check.sh` | exit 0; no client is launched |
| Scope | `git status --short` | only files listed under Changes by layer are modified |

Do not run `./dev tui`, `./dev desktop`, or anything that enters a tty or opens
a window. Render into in-memory `Screen` values and drive the real `Runner`
against fakes or temporary repositories.

## Scope

In scope:

- One shared app acquisition helper for the repository's stash stack; an empty
  stack is a successful read, not an error and not a missing pane.
- A terminal-owned flat `Stashes` view with one `Viewport`, pane-local drawing,
  mouse cursor placement, scrollbar support, copy-current-row, refresh anchoring,
  and the drop arm.
- A repository-only `Screens::Stashes` tenant registered with
  `Placement::sidebar("stashes")` on both commits and diff launches.
- Dispatch of the existing `stashes.apply`, `stashes.pop`, `stashes.drop`, and
  `files.stash` names through `Write` and the existing `Submitter`/`Runner`.
- Exact drop confirmation, conflict/refusal reporting, and registration-wide
  refresh after both successful and refused jobs.
- Headless rendering, focus-ring, verb, refusal, refresh, and pass-016 survival
  tests.

Explicitly out of scope:

- Any edit to `tui/src/panes.rs`. Its generic registry, canonical rank 4,
  equal-height sidebar slicing, cached geometry, and narrow focused-only policy
  are the law for this pass.
- A terminal files or branches pane, or any dependency on plans 017/018. Handle
  `files.stash` even when no files tenant is registered; do not create a fake
  files pane to make its default mode reachable.
- A new CLI `stashes` view, `View::Stashes`, or `Data::Stashes` variant. Stashes
  are an ancillary repository tenant, not a new startup view; adding a `Data`
  variant would force unrelated client matches to change.
- New command names, aliases, default bindings, terminal-local key tables, or a
  new `gitten.toml` table. The existing names and `[keys]` modes are sufficient.
- A stash-message prompt. `files.stash` passes `None`, exactly as the window
  does; naming a stash is future prompt work.
- Stash diff preview, automatic changes to the main diff when the stash cursor
  moves, multiple selection, drag-to-copy ranges, stash branching, or include-
  untracked options.
- Any `core/`, `git/`, shell, web, dependency, or `Cargo.toml` change.

## Baseline facts (provenance)

### The desktop reference is one flat, identity-anchored stack

- `shell/src/views/stashes.rs:25-67` flattens each `Stash` once into a row with
  `index`, full `commit`, preformatted `stash@{n}` title, and message. The address
  is the verb target; the commit is retained only to anchor the cursor when a
  drop renumbers indices.
- `shell/src/views/stashes.rs:69-83` labels the pane as
  `<describe> · <N> parked`. `shell/src/views/stashes.rs:370-464` draws the
  address first in dim furniture ink, then the message, and uses
  `nothing stashed` as the quiet empty line.
- `shell/src/views/stashes.rs:163-197` clears an armed drop on refresh, follows
  the selected entry by commit when it survives, and otherwise clamps the old
  cursor. `shell/src/views/stashes.rs:318-355` defines the current row, the drop
  arm, and copy text as `stash@{n} <message>`; there is deliberately no range
  selection in this view.
- `shell/src/views/stashes.rs:85-99,323-338` contains the exact destructive
  pattern: first press on an index arms it and asks
  `drop stash@{n}? press again to confirm`; the second press on the same index
  clears the arm and acts. A cursor move, moving wheel, or refresh disarms it;
  merely focusing another pane and returning does not.

### Apply, pop, drop, and push already have shared names and semantics

- `core/src/command.rs:373-384` globally binds `5` to `stashes.focus`.
  `core/src/command.rs:414-426` binds `s` in `files` mode to `files.stash`, and
  space/`g`/`d` in `stashes` mode to apply/pop/drop. The mode override is
  load-bearing: `g` remains `view.top` outside the stash pane.
- `core/src/command.rs:1044-1061` registers all five names. Its descriptions make
  the user-visible distinction explicit: apply keeps the entry; pop drops it
  only when apply is clean; only drop says `asked twice`. The terminal must not
  add a second confirmation to pop or disclose an internal alias in the prompt.
- `shell/src/main.rs:2243-2282` proves the window's exact command path. It reads
  the current row's index, refuses an empty stack or missing repository, confirms
  **only** `stashes.drop`, then builds `Write::stash_apply`, `stash_pop`, or
  `stash_drop` and sends the job. `shell/src/main.rs:7228-7272` pins pop as one
  press and drop as two presses on the same row.
- `shell/src/main.rs:1756-1771` sends `Write::stash_push(&repo, None)` for
  `files.stash`. There is no message field or prompt: git supplies its normal
  `WIP on …` text.
- `app/src/verbs.rs:355-385` exposes the exact constructors this pass must call:
  `stash_push(&Handle, Option<String>) -> Write`,
  `stash_apply(&Handle, usize) -> Write`,
  `stash_pop(&Handle, usize) -> Write`, and
  `stash_drop(&Handle, usize) -> Write`. None pre-refuses based on UI state;
  drop's caller confirms before construction, while repository refusals come
  back from the queued operation.

### The read model is already sufficient, but app acquisition has no stash door

- `core/src/refs.rs:16-19,165-180` says absence is data: no stashes is an empty
  list. Each entry contains exactly the stack index (newest is zero), display
  message, and full commit identity. There is no richer stash tree, diff, date,
  branch, or prompt model to invent in the client.
- `app/src/acquire.rs:27-53` currently models startup `Loaded` data as only
  `Commits` or `Diff`; `app/src/acquire.rs:127-224` matches only those two CLI
  views and their sources. Therefore this pass needs a narrow repository-pane
  helper, not a third startup `Data` variant and not direct `Repo::stashes()` I/O
  in `tui`.
- `app/src/acquire.rs:113-124` already distinguishes a refresh from startup and
  accepts empty results after writes. The stash helper should make emptiness
  valid on both first read and refresh, because an empty stack is normal before
  the first push and after the last pop/drop.

### Plan 016 already provides every pane seam this feature needs

- `tui/src/panes.rs:63-97` reserves canonical sidebar rank 4 for `stashes` and
  provides `Placement::sidebar(name)`. `tui/src/panes.rs:306-383` registers or
  replaces generic tenants by stable name and exposes named/focused iteration.
  The API is not blocked and must not be rewritten.
- `tui/src/panes.rs:203-297` divides the existing sidebar equally among all
  registered lists in wide mode and shows only the focused pane below 96
  columns. Registering stashes therefore automatically gives it the sidebar
  foot, wide slice, mouse rectangle, narrow full body, and cached geometry.
- `tui/src/main.rs:157-186,197-300` has a `Screens` adapter whose variants own a
  view, label, source where needed, and `Generation`; its `refresh` method is the
  per-tenant re-acquisition seam. `tui/src/main.rs:312-463` centralizes resize,
  paint, status, mouse/copy selection, and focused command routing.
- `tui/src/main.rs:574-670` currently registers commits/diff and restores the
  requested initial focus after registration. A stash registration belongs in
  this construction path only when `Started.repo` exists, followed by the same
  focus restoration so an ancillary pane never steals startup focus.
- `tui/src/main.rs:1196-1289` already dispatches all ten pane/focus names before
  a focused view; the `stashes.focus` arm needs no edit. Once `stashes` registers,
  `5` works through the same `Host::keys` and `gitten.toml` path that already
  works for commits and diff.

### Refusals and conflicts already have an honest status-line rail

- `app/src/verbs.rs:365-377` says apply keeps the entry and pop drops it only
  after a clean restore; pop's sequencing failure is surfaced through the job
  error. The `Write` wrapper passes the repository's `Result` through unchanged
  (`app/src/verbs.rs:477-489`).
- `tui/src/main.rs:1479-1515` turns a started job into `running <job name>` and a
  failed finish into the write's exact error text. Every finish—including a
  refusal—advances generation and refreshes all stale repository panes because
  a failed git operation may already have changed the index or working tree.
- `tui/src/main.rs:1518-1549` attempts every registered repository tenant,
  focused or hidden, remembers the first refresh error, and appends it after a
  write error. Therefore the stash pane must not synthesize a vague `conflict`
  state. On apply/pop conflict, the repository's refusal text owns the status
  line; the refresh shows the actual stack (including a pop entry git kept) and
  the actual files/diff state.

## Approach

1. **Add one app-level stash read.** In `app/src/acquire.rs`, add a dedicated
   `LoadedStashes { label, stashes }` result and `stashes(&dyn Repo)` helper. Run
   `Repo::describe` beside `Repo::stashes`, as the existing repository views run
   their independent title/data reads beside each other. Return an empty vector
   as success and preserve repository errors verbatim. Do not add a CLI view or
   touch `Data`.
2. **Build the flat terminal tenant.** Add `tui/src/stashes.rs`. Flatten the
   read once into rows containing index, commit, preformatted `stash@{n}`, and
   message; draw `title + one cell + message` through `Screen::span`. Hold one
   `Viewport`, one scrollbar state, and one optional armed index. Keep frame
   drawing allocation-free; no `format!`, row cloning, or width table rebuild
   belongs in `paint`.
3. **Define selection narrowly.** The cursor row is the selected stash for all
   three verbs, and its `index` is the only argument sent to `Write`. Keyboard
   movement and a mouse click move that cursor. `copy.selection` returns
   `stash@{n} <message>` for the cursor. Match the desktop and keep drag range,
   `select.all`, and copy-on-select inert; a drag may continue cursor/scrollbar
   movement but must not create a multi-stash selection.
4. **Register on every real repository launch.** In `App::new`, acquire and
   register `stashes` for both commits-shaped and diff-shaped `Source::Repo`
   launches, even when the successful answer is empty. Register it with
   `Placement::sidebar("stashes")`, then restore focus to the startup view.
   Fixture and patch launches have no repository and register no stash tenant;
   `5` continues to answer `no stashes pane` there. A stash ancillary-read error
   must not abort an otherwise valid launch: register an explicit unavailable
   empty state, put the exact error on the first status line, and allow the next
   refresh to recover. Do not render a read failure as `nothing stashed`.
5. **Let the foundation lay it out.** Wide mode gives commits and stashes equal
   vertical slices in canonical order, with stashes at the foot; a direct repo
   diff gets the stash sidebar beside its main diff. Narrow mode shows only the
   focused pane at full body size, so `5` both focuses and reveals stashes. Empty
   success draws `nothing stashed`; unavailable draws `stash list unavailable`.
   The common header supplies the live configured focus key and pane name; the
   view label is `<describe> · N parked` (or `unavailable` after a failed read).
6. **Dispatch four existing names.** Add app-owned handlers before
   `Screens::run`: `stash_selected` for apply/pop/drop and
   `stash_working_tree` for `files.stash`. The former requires the focused
   `Stashes` tenant and a current row; the latter requires only the repository.
   Every accepted action submits the corresponding `Write` to the existing
   queue. No handler calls `Repo` write methods directly.
7. **Confirm drop exactly once, and pop not at all.** First drop on a row stores
   its index and sets exactly `drop stash@{n}? press again to confirm`; second
   drop on that same row clears the arm and queues. A keyboard cursor move,
   moving wheel/scrollbar, mouse move to another row, or refresh disarms; a focus
   round-trip alone does not. Pop queues on its first press. The confirmation
   must not say the core identifier `stashes.drop`; help/config expose command
   names, while the prompt speaks the git address. Once queued, the runner's
   existing job label (`stash drop stash@N`, etc.) owns the running band.
8. **Refresh by commit identity.** Add `Screens::Stashes` to the existing
   generation rail. Re-acquire through the new app helper, clear any drop arm,
   anchor a surviving cursor by full stash commit, and otherwise clamp the old
   position after indices renumber. On refresh error keep the last good rows and
   generation, return the error, and continue attempting every later tenant.
   On clean apply the row remains; on clean pop/drop it disappears and the list
   renumbers; on conflict the refusal text remains the status and refreshed
   repository state decides what is visible.

## Changes by layer

### Core

No production or test change.

- Keep `core/src/refs.rs` as pure data and `core/src/command.rs` as the sole
  command/default registry. Do not add stash view logic, confirmation state, a
  terminal alias, or a key binding.
- Keep `core/Cargo.toml` dependency-free.

### App

**`app/src/acquire.rs`**

- Import `gitten_core::refs::Stash` and add a small public load result dedicated
  to the ancillary stack, with repository description and `Vec<Stash>` kept as
  separate fields. Name it unambiguously (`LoadedStashes` is preferred).
- Add `pub fn stashes(repo: &dyn Repo) -> Result<LoadedStashes, String>`. Fetch
  description and stack concurrently, join the infallible description, preserve
  the stash read's exact error, and accept an empty vector.
- Add fake-backed tests for order/data preservation, empty success, and exact
  error propagation. Do not modify `Loaded`, `Data`, `View`, `Source`, startup
  parsing, or any verb.

No change to `app/src/verbs.rs`: the four constructors already are the shared
write seam.

### TUI library

**`tui/src/stashes.rs` (new)**

- Add a private flattened `Row` with `index`, `commit`, `title: String`, and
  `message: String`, plus a public `Stashes` view holding rows, `Viewport`, pane
  width, scrollbar glyph/state, `armed: Option<usize>`, and the ancillary-read
  availability state.
- Provide `new`, `unavailable`, `replace`, `set_scrolloff`, `set_bar`, `resize`,
  all shared vertical viewport operations, `current`, `confirm_or_arm_drop`,
  pane-local `press`/`drag`/`release`, `copy_text`, inert selection methods,
  `paint`, and `status`.
- `replace` flattens once, clears unavailable/armed/mouse state, anchors by
  commit, and clamps when the identity vanished. Store titles once; do not
  allocate them in paint or status.
- Paint every body row through `Screen::span(row, x, cols)`. Draw the cursor
  background only while focused, preserve an armed row's error foreground when
  unfocused, wash blank rows to pane background, and paint the scrollbar over
  the pane's last column. At nonzero origin, no title or long message may cross
  the pane boundary.
- Empty success paints `nothing stashed` in faint ink. An unavailable read
  paints `stash list unavailable` and exposes no current row. Normal status is
  `position/total · stash@{n}`; empty and unavailable status must distinguish
  `0 parked` from `unavailable`.

**`tui/src/lib.rs`**

- Export `pub mod stashes;` and add it to the module table. No other library
  module changes are expected.

### TUI assembly

**`tui/src/main.rs`**

- Import `gitten_tui::stashes::Stashes` and add `Screens::Stashes { view,
  label, generation }`. Extend every exhaustive `Screens` adapter—mode, label,
  generation, refresh, resize, paint, status, mouse, copy/selection, and
  `run`—without adding stash-specific branches to geometry or key resolution.
- In `Screens::refresh`, call `acquire::stashes(repo)`, replace the view, update
  `<describe> · N parked`, and advance generation only on success. Preserve the
  last good tenant on error and return it through the existing first-error rail.
- In `App::new`, perform the ancillary read only when `started.repo` exists.
  Register the resulting normal/empty/unavailable tenant with
  `Placement::sidebar("stashes")`, apply the selected scrollbar glyphs, then
  restore `commits` or `diff` focus. Preserve the initial read error in
  `App::message`; do not abort startup or print it behind the alternate screen.
- Add `stash_selected(command)` using the focused stash row. Refuse outside the
  pane with `<command> is not supported here`, refuse an empty/unavailable stack
  with `nothing selected on the stash stack`, and submit the exact existing
  `Write` for the selected index. Implement the drop arm before building its
  job; apply and pop bypass it.
- Add `stash_working_tree()` using `Write::stash_push(handle, None)`. It does not
  inspect the files or stash pane. With no repository, say
  `a fixture has no working tree to park`; on a stopped queue use the existing
  `the job queue is shutting down` wording.
- Dispatch `files.stash` and the three `stashes.*` names before the focused
  tenant's generic `run`. Keep `stashes.focus` in the existing registry arm.
- Update repository-backed test fakes to implement stash reads/writes explicitly
  so the new ancillary read is observed rather than falling through a trait
  default. Adjust plan-016 pane-count assertions from commits+diff to
  commits+stashes+diff where the launch is repository-backed; fixture/patch
  assertions remain unchanged.

No changes to `tui/src/panes.rs`, `tui/src/commits.rs`, `tui/src/diff.rs`,
`tui/src/screen.rs`, `tui/src/scrollbar.rs`, `tui/src/term.rs`, or
`tui/Cargo.toml` are expected. Reuse their public seams. If the new view cannot
do that, stop rather than widening this pass.

## Test list

All tests are headless. Use deterministic fake repositories for command/refresh
tests and fixed in-memory screens for drawing. Every named test below must state
its fixture and assert the listed behavior.

1. **`stash_acquisition_preserves_stack_data_and_accepts_absence`**
   (`app/src/acquire.rs`) — fake `Repo` fixture returning two stashes, then an
   empty vector. Assert description, newest-first order, indices, messages, and
   full commits are unchanged; empty is `Ok`.
2. **`stash_acquisition_preserves_the_repository_refusal`**
   (`app/src/acquire.rs`) — fake whose stash read returns a sentinel error.
   Assert the helper returns that exact text and does not translate it into an
   empty list.
3. **`stash_rows_are_address_then_message_and_empty_is_quiet`**
   (`tui/src/stashes.rs`) — two-row `Stash` fixture plus empty and unavailable
   fixtures, painted at nonzero x. Assert `stash@{0} On main: …`, dim address,
   normal message ink, focused cursor background, `nothing stashed` only for
   successful emptiness, `stash list unavailable` only for read failure, and no
   cell outside the span changes.
4. **`stash_refresh_follows_commit_identity_and_renumbers_titles`**
   (`tui/src/stashes.rs`) — cursor on old `stash@{1}`, then remove the row above
   it. Assert the same commit becomes `stash@{0}` and stays selected; removing
   the selected commit clamps; replacing with empty yields cursor/top zero.
5. **`stash_drop_arm_survives_only_the_same_row`**
   (`tui/src/stashes.rs`) — assert first arm false/second same-index true, another
   row re-arms, cursor move/moving scroll/mouse row change/refresh disarm, a
   focus-only round trip does not mutate the view, and the question formatter is
   exactly `drop stash@{0}? press again to confirm`.
6. **`repository_launch_registers_stashes_in_the_existing_ring`**
   (`tui/src/main.rs`) — repository-backed commits and repository-backed diff
   `Started` fixtures. Assert names/list order include `commits, stashes` where
   applicable, `stashes` has canonical placement, requested startup focus is
   restored, `5` resolves via `Host::keys` and focuses it, mode becomes
   `stashes`, and no new key table/name exists. Fixture and patch launches assert
   no stash tenant and exact `no stashes pane` on focus.
7. **`wide_and_narrow_frames_place_the_stash_tenant_without_layout_edits`**
   (`tui/src/main.rs`) — 120-column and 80-column repository fixtures. Assert
   wide commits/stashes slices tile the sidebar in canonical order beside diff;
   headers show live configured focus keys and `<describe> · N parked`; narrow
   mode shows only focus at full width, and `5` reveals stashes. Assert the
   existing geometry generation/cache behavior and divider clipping survive.
8. **`stash_apply_job_uses_the_selected_index_and_keeps_the_entry`**
   (`tui/src/main.rs`) — recording mutable repo with two stashes. Move to index
   1, dispatch `stashes.apply`, drain the real runner, and assert the call/job
   label targets index 1, generation advances, all repository panes refresh,
   and the selected stash remains anchored. Repeat with a sentinel apply
   refusal: exact text owns the status and no vague conflict copy is invented.
9. **`stash_pop_is_one_press_and_only_clean_success_removes`**
   (`tui/src/main.rs`) — clean-pop fake removes the selected entry; conflict fake
   returns a sentinel error and keeps it. Assert the first `g` queues immediately
   with no confirmation, clean refresh removes/renumbers, conflict refresh keeps
   the row, and the exact refusal text is the status message.
10. **`stash_drop_requires_two_presses_and_refusals_never_spend_an_arm_twice`**
    (`tui/src/main.rs`) — first `d` queues nothing and shows the exact question;
    second same-row `d` queues `Write::stash_drop` and clears it. Assert a moved
    cursor asks anew, an empty pane refuses before the queue, and a backend stale-
    index refusal is surfaced exactly after the confirmed job and refresh.
11. **`files_stash_queues_a_message_less_push_and_surfaces_clean_tree_refusal`**
    (`tui/src/main.rs`) — dispatch the command directly against a repository
    fixture without registering a files pane. Assert one `stash_push(None)` job,
    refreshed stack contains the new top entry, and no prompt/input mode opens.
    Repeat on a clean-tree refusal and assert exact job error plus a refreshed
    unchanged stack; no-repo fixture says it has no working tree to park.
12. **`stash_finishes_refresh_every_registered_repository_pane`**
    (`tui/src/main.rs`) — commits+stashes+loaded diff fake with per-read counters,
    stashes focused in narrow mode. Finish one successful apply and one refused
    pop. Assert each generation refreshes commits, stashes, and diff exactly
    once regardless of focus/visibility; one refresh failure does not skip later
    tenants and appends after the write refusal in the existing `write · refresh`
    order.
13. **`wave_one_and_plan_016_features_survive_stash_registration`**
    (`tui/src/main.rs`) — extend existing focus, mouse capture, live-key header,
    search, hunk-job, Markdown pane-width, config reload, copy, and refreshed-
    frame tests to run with the third registered tenant. Assert search still
    targets named commits, hunk verbs still target diff, mouse drag stays in its
    captured pane, headers/status follow focus, and narrow hidden tenants refresh.
14. Run the required gates exactly as written:

    ```sh
    cargo test -p gitten-tui -p gitten-app
    cargo test -p gitten-core
    cargo fmt --check
    cargo clippy -p gitten-tui -p gitten-app --all-targets -- -D warnings
    ./check.sh
    ```

Done means all of the following are machine-checkable:

- [ ] A real repository launch always has a named `stashes` tenant, including a
      successful empty stack; fixture/patch launches do not.
- [ ] `5`, space, `g`, `d`, and files-mode `s` resolve only through the existing
      shared keymap and commands; no core or terminal key registry changed.
- [ ] Apply/pop/drop address the cursor row's index; push passes `None`; all four
      writes use `Write` plus `Submitter`, never direct repository writes.
- [ ] Drop alone asks twice with the exact git address; pop acts once and relies
      on git's clean-only drop semantics.
- [ ] Every successful/refused job refreshes all registered repository panes;
      conflict/refusal text is preserved and the refreshed stack is authoritative.
- [ ] Wide/narrow placement, pane-local clipping, focus routing, search, staging,
      Markdown, mouse capture, copy, reload, and plan-016 tests remain green.
- [ ] `git diff --name-only` lists only `app/src/acquire.rs`, `tui/src/lib.rs`,
      `tui/src/main.rs`, and the new `tui/src/stashes.rs`.

## Stop conditions

Stop and escalate if any occurs:

- `Placement::sidebar("stashes")`, canonical rank 4, named registration,
  focused lookup, iteration, or narrow layout no longer works as cited. Do not
  edit `tui/src/panes.rs` to accommodate this tenant.
- Any stash/focus command or default binding differs from
  `core/src/command.rs`, or implementation appears to require a terminal alias,
  hardcoded keypress, or new `[keys]` table.
- The read requires a `View::Stashes`/`Data::Stashes` startup variant, direct git
  I/O in `tui`, or changes to shell/web. Add only the ancillary app helper.
- Apply/pop/drop/push cannot be expressed through the exact existing `Write`
  signatures, or a UI preflight starts duplicating repository refusal logic.
- Correct pop behavior appears to require a second confirmation. Bring evidence
  that the shared verb can drop after a failed apply; do not diverge from the
  cited clean-only semantics and window behavior without review.
- A stash conflict is swallowed, replaced with generic `conflict`, prevents the
  registration-wide refresh, or removes the stash from the view without the
  refreshed read saying it is gone.
- A failed ancillary stash read aborts a valid commits/diff launch, or is drawn
  as the successful empty state. Preserve a recoverable unavailable tenant.
- Refresh anchors by index rather than commit, an armed index survives refresh,
  or a drop/pop renumber leaves the cursor addressing a different entry than the
  one drawn.
- The implementation introduces multi-selection, a message prompt, stash diff
  preview, synchronous I/O on cursor movement, or any plan-017/018 dependency.
- Painting allocates/formats per row, uses `Screen::row` instead of a pane span,
  crosses the divider, or changes any foundation painter/layout module.
- Any test needs a live external repository, network, tty, raw mode, or launched
  client, or any required verification fails twice after one reasonable fix.

## Risks

- **Ancillary startup latency (MED):** the first stack read adds one git process
  after shared startup has loaded the requested view. Keep description and stack
  reads parallel inside app acquisition; do not add a background terminal
  protocol in this thin pass. Record timing only if the added read is visibly
  outside existing startup noise.
- **Index renumbering (HIGH):** a drop/pop changes every later `stash@{n}`. An
  arm or cursor anchored only by index can target yesterday's row after refresh.
  Commit identity anchoring plus unconditional refresh disarm are release gates.
- **Conflict partial effects (HIGH):** apply/pop may return an error after git
  changed files or the index. Treat every finished refusal as stale, preserve the
  error text, and trust refreshed reads rather than trying to infer rollback.
- **Foundation regression (MED):** repository tests and wide frames that assumed
  exactly commits+diff will now have a third tenant and two sidebar slices.
  Update expectations deliberately; do not weaken the pass-016 assertions or
  bypass the registry to preserve old counts.
- **Unavailable versus empty (MED):** an ancillary read failure must not prevent
  the app from opening, but `nothing stashed` would falsely assert a successful
  empty read. Keep explicit unavailable copy until a later successful refresh.
- **Mode reachability without plan 017 (LOW):** the default `files.stash` key is
  active only in files mode, and this plan intentionally does not register that
  pane. Dispatch and custom global bindings work now; a future files tenant gets
  the shipped `s` binding without changing this code. That is independence, not
  a missing local key.
- **Scope collision with parallel plans (MED):** plans 017/018 may also extend
  `Screens` and ancillary acquisition. Keep the stash helper and variant narrow,
  avoid a speculative omnibus enum, and let the integrator reconcile adjacent
  exhaustive matches without introducing dependencies among the plans.
