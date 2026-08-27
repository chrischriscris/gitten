# Plan 017: Register a complete files pane in the terminal

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving on. Do not
> update `plans/README.md`; the integrator owns the index. Do not launch a client
> or take over a tty. If anything in **Stop conditions** occurs, stop and report
> what you verified instead of improvising.
>
> **Drift check (run first)**:
> `git diff --stat 7b7ec51..HEAD -- core/src/command.rs core/src/host.rs core/src/status.rs app/src/acquire.rs app/src/verbs.rs shell/src/main.rs shell/src/input.rs shell/src/views/files.rs tui/src/lib.rs tui/src/main.rs tui/src/panes.rs`
> If any cited behavior below changed, compare it with the live code before
> editing. A mismatch in `Panes<T>`, files command names, verb signatures,
> prompt isolation, or generation-wide refresh is a STOP condition.

## Status

- **Priority**: P1
- **Effort**: L (multi-day: a new virtualized list, seven command paths,
  destructive confirmation, generalized terminal input, acquisition/refresh,
  mouse routing, and fixed-cell integration coverage)
- **Risk**: HIGH (the feature writes repositories and shares the prompt/job
  state machines already serving search and hunk staging)
- **Depends on**: plan 016, landed in this tree; plans 013–015 must survive
- **Category**: terminal feature / client parity
- **Planned at**: commit `7b7ec51`, 2026-08-27
- **Confidence**: HIGH on the pane, command, verb, confirmation, and prompt
  shapes; MED on the best wording of the initial status-read failure until the
  fixed-cell frames are reviewed

## Why this matters

The terminal now has the pane foundation but still registers only `commits` and
`diff`. Its existing `files.focus` dispatch therefore says `no files pane`, and
the second-sidebar cycle promised by plan 016 has no second list. Meanwhile the
shared status model, command names, write jobs, and generation refresh rails are
already complete; the missing work is a terminal tenant and terminal input.

This pass makes `files` a first-class sidebar pane for every repository-backed
launch. It does not add a keymap, write path, layout branch, CLI root view, or
dependency. A file operation uses the same `core::command` name and the same
`gitten_app::verbs::Write` constructor as the window, then the existing runner
refreshes every registered repository pane.

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Required headless gate | `cargo test -p gitten-tui -p gitten-app` | all existing and new tests pass; no tty or window opens |
| Core contract regression | `cargo test -p gitten-core` | all shared status/command/viewport tests pass |
| Format | `cargo fmt --check` | exit 0 |
| Lint | `cargo clippy -p gitten-tui -p gitten-app --all-targets -- -D warnings` | no warnings |
| Full repository gate | `./check.sh` | exit 0; no interactive client launches |
| Scope | `git status --short` | only `tui/src/files.rs`, `tui/src/lib.rs`, and `tui/src/main.rs` are modified |

Do not run `./dev tui` or `./dev desktop`. Use fake `Repo` handles and
in-memory `Screen` frames. The user explicitly reserves `plans/README.md` for
the integrator.

## Scope

In scope:

- A terminal `Files` view over `core::status::Status`, using one `Viewport`,
  flattened rows, pane-local paint, section/path cursor identity, and the
  existing scrollbar.
- Eager registration under stable name `files` and
  `Placement::sidebar("files")` whenever startup supplied a repository handle.
- Exact dispatch of `files.stage`, `files.stage-all`, `files.discard`,
  `files.ignore`, `files.stash`, `files.commit`, and `files.amend` through the
  existing shared job runner and `Write` constructors.
- A generalized terminal prompt state that preserves search and adds one-line
  commit/amend messages on the bottom row, resolved only through `input` mode.
- Status refresh through the registration-wide generation path, including an
  unfocused or narrow-hidden files tenant.
- Headless unit/integration tests and fixed-cell frames for every behavior named
  in the Test list.

Explicitly out of scope:

- Any edit to `tui/src/panes.rs`. Its generic registry, canonical ranks,
  `BuiltinLayout`, 96-column fallback, `Rect`, and geometry cache contract are
  law for this pass.
- Any edit to `plans/README.md`, docs, shell, web, `gitten.toml`, Cargo manifests,
  lockfiles, or dependency versions.
- A `View::Files` CLI root view, a new `app::acquire::Data` variant, background
  refresh protocol, automatic diff preview, file diff opening, partial-file
  staging, multi-select, or a custom stash message.
- New command names, default bindings, modes, terminal-local key tables, or a
  confirmation command such as `files.discard-confirm`.
- Moving display flattening into core. The porcelain-v2 facts already live
  there; cell strings, colors, clipping, and terminal hit testing remain client
  drawing/input.

## Baseline facts (provenance)

### The pane foundation is already the required seam

- `tui/src/panes.rs:1-33` says adding a files tenant is a `register` call, not a
  layout or dispatch branch. `canonical_rank` assigns `files` rank 1 between
  status and branches (`tui/src/panes.rs:63-75`), and
  `Placement::sidebar(name)` is the built-in registration path
  (`tui/src/panes.rs:78-98`).
- `Panes<T>::register` replaces by stable name and focuses the tenant
  (`tui/src/panes.rs:300-354`); `list_order`, `reading_order`, `walk`, and
  `cycle_sidebar` derive all navigation from registrations
  (`tui/src/panes.rs:435-503`). Registering `files` therefore makes `2` work and
  gives `pane.next`/`pane.prev` the second list without a change in panes code.
- `BuiltinLayout` gives registered sidebars equal vertical slices in canonical
  order beside the main pane at 96 columns and wider, and below 96 gives the
  entire body only to the focused pane (`tui/src/panes.rs:203-297`). No files-
  specific geometry is needed or permitted.
- `App` already owns `Panes<Screens>`, a cached replaceable layout, a retained
  repository handle, stable-name mouse capture, the shared runner, and the
  current generation (`tui/src/main.rs:479-571`). It builds commits+empty diff
  for a commit launch and diff alone for a diff launch
  (`tui/src/main.rs:575-670`).
- All ten pane/focus names dispatch before view commands; missing names produce
  exact `no <name> pane` notices (`tui/src/main.rs:1196-1218,1291-1311`). The
  focused tenant supplies mode, paint, status, view commands, copy, and mouse
  behavior through `Screens` (`tui/src/main.rs:197-464`).

### The status model already says exactly what the pane must show

- `core::status::Status` is pure porcelain-v2 data. It separates index-versus-
  HEAD `staged`, worktree-versus-index `unstaged`, `untracked`, and unresolved
  `conflicts`; ignored entries are optional and deliberately not acquired by
  default (`core/src/status.rs:1-14,248-273`).
- Paths are raw `PathBytes`; addressing must use `as_bytes`, while display may
  use `to_string_lossy` (`core/src/status.rs:21-48`). The same path may appear
  in both staged and unstaged, so a cursor/refresh anchor must be
  `(Section, PathBytes)`, never path alone (`core/src/status.rs:199-203` and
  `shell/src/views/files.rs:435-468`).
- The window flattens only non-empty sections in this order: staged, unstaged,
  untracked, conflicts. Each section gets one heading with its count, then file
  rows; status letters are `A/M/D/R/T`, `?`, or the porcelain conflict pair
  (`shell/src/views/files.rs:67-108,174-252`). It performs that allocation once
  per refresh, never per frame (`shell/src/views/files.rs:28-33`).
- The window cursor never rests on a heading, shows `working tree clean` for an
  empty status, anchors refresh by section+path, and copies the current row as
  `letters path` (`shell/src/views/files.rs:304-317,409-479,618-687,706-724`).
  The TUI should preserve those semantics in cells, not copy GPUI machinery.

### The seven shared commands already have names, docs, and keys

- The built-in keymap binds files mode `space/c/A/D/a/i/s` to
  `files.stage`, `files.commit`, `files.amend`, `files.discard`,
  `files.stage-all`, `files.ignore`, and `files.stash`
  (`core/src/command.rs:398-416`). Input Enter/Esc already resolve to
  `input.accept`/`input.cancel`, and the panes mode already owns Ctrl-J/Ctrl-K
  (`core/src/command.rs:521-528`).
- The command registry descriptions are authoritative: stage toggles the
  selected file, stage-all follows the cursor's index side, discard is asked
  twice, ignore applies to the selected untracked file, commit records staged
  changes, amend rewrites HEAD with staged changes and a new message, and stash
  parks the working tree (`core/src/command.rs:994-1021,1044-1050`).
- `Host` holds the shared keymap and command registry so every client and
  `gitten.toml` address names rather than physical keys
  (`core/src/host.rs:1-12,59-70,98-114`). This pass adds no names or defaults.

### Read acquisition and refresh are already separated from drawing

- Shared startup returns `Host`, `Source`, `Loaded`, and the retained repository
  handle; a client owns what it draws (`docs/clients.md:24-31,55-71`).
  `app::acquire` deliberately exposes CLI-root commits and diff data only
  (`app/src/acquire.rs:27-53,104-125`); files is a repository sidebar, not a new
  command-line view.
- The window eagerly reads status and registers `files` during startup whenever
  a repository exists; fixtures get no pane. A failed initial status read does
  not abort the window (`shell/src/main.rs:4734-4823`). Its files refresh is one
  `repo.status()` plus preparation, guarded by generation
  (`shell/src/main.rs:744-785`).
- The terminal already advances generation after every finished job, including
  a refused write, and calls `refresh_stale` over every registered tenant while
  retaining the first refresh error and attempting the rest
  (`tui/src/main.rs:1479-1549`). `Screens::refresh` preserves old data when
  re-acquisition fails and marks a tenant current only after a successful apply
  (`tui/src/main.rs:218-300`). Files must join this path; it must not introduce
  a second refresh loop.

### Every write constructor and refusal boundary is already defined

- `Write::stage(&Handle, Vec<u8>)` and `Write::unstage` operate on one raw path;
  `stage_many`/`unstage_many` submit all paths as one job so one keypress causes
  one generation bump (`app/src/verbs.rs:66-97`).
- `Write::discard` destroys unstaged tracked work, while
  `remove_untracked` deletes a file absent from the object database; the caller
  must confirm before constructing either destructive job
  (`app/src/verbs.rs:125-153`). `Write::ignore` appends the raw path to
  `.gitignore` (`app/src/verbs.rs:155-161`).
- `Write::commit(&Handle, String)` and `Write::amend` submit messages and rely on
  the finished generation refresh to reveal the new/replaced commit
  (`app/src/verbs.rs:163-180`). `Write::stash_push(&Handle, Option<String>)`
  parks tracked work; the window supplies `None` for git's WIP message
  (`app/src/verbs.rs:355-363` and `shell/src/main.rs:1756-1771`).
- The window's row semantics are explicit: staged rows unstage; unstaged,
  untracked, and conflict rows stage (`shell/src/main.rs:1376-1413`). Stage-all
  on staged unstages all staged paths; elsewhere it stages unstaged+untracked,
  excluding conflicts (`shell/src/main.rs:1555-1602`). Ignore refuses every
  section except untracked (`shell/src/main.rs:1604-1633`).
- Discard is the only destructive files-pane confirmation. Staged and conflict
  rows refuse; unstaged asks `discard …? press again to confirm`; untracked asks
  `delete …? press again to confirm`. The second press on the same section+path
  submits, while movement, wheel, or refresh disarms
  (`shell/src/main.rs:1497-1553` and `shell/src/views/files.rs:319-355,653-669`).
  No confirm command is needed: the same `files.discard` command plus pane state
  is the window's exact pattern. Amend opens its field directly and does not use
  the double-ask flow (`shell/src/main.rs:1455-1495`).
- The reset reference proves the wider confirmation convention: opening the
  reset question stores the selected SHA and pushes the reset mode; a strength
  command acts only while that same target remains armed, otherwise it refuses
  (`shell/src/main.rs:1776-1872`). Files discard needs no new mode because its
  second press is the same command, while reset needs a mode only to give
  `s/m/h` their temporary meanings.

### The terminal and window both have one-line input

- The terminal search prompt owns only the bottom status row, resolves keys
  against exactly `input`, sanitizes pasted line breaks/tabs into spaces, makes
  the mouse inert, and uses Enter/Esc to accept/cancel
  (`tui/src/main.rs:891-968,970-1057,1632-1651`). This isolation—not search-
  specific filtering—is the precedent commit input must reuse.
- The window's `Input` is also a single-line field: Enter is not inserted because
  it is the named accept command, and paste replaces CR/LF with spaces
  (`shell/src/input.rs:1-7,39-66,93-119,347-350`). Commit and amend both open that
  field empty; trim-empty accept closes it and refuses without a job
  (`shell/src/main.rs:1415-1495`). Therefore a multi-line TUI editor would exceed
  parity and scope. A logical one-line `String` may be longer than the terminal;
  drawing must show its tail and caret without truncating the stored value.

## Approach

1. **Add one terminal-native files list.** Create `tui/src/files.rs` with
   `Section`, flattened heading/file rows, a `Viewport`, scrollbar state, and an
   optional armed discard. Flatten once at construction/refresh. Preserve raw
   paths for verbs and precompute lossy display strings, rename origins, letters,
   and counts so paint allocates nothing per frame.
2. **Match the window's section and cursor semantics.** Draw only non-empty
   sections in `STAGED`, `UNSTAGED`, `UNTRACKED`, `CONFLICTS` order, each heading
   followed by its count and rows. A heading is never selectable. Refresh anchors
   to `(section,path)`; disappearance clamps and settles onto a file. The selected
   file means unstage in Staged and stage in all other sections. Stage-all means
   all staged paths when the cursor is Staged, otherwise all unstaged+untracked;
   conflicts are deliberately not bulk-staged.
3. **Register eagerly when a repository exists.** During `App::new`, read
   `repo.status()` and `repo.describe()` (overlap them if the code remains clear),
   build `Screens::Files`, register it as `Placement::sidebar("files")`, then
   restore the launch focus (`commits` or `diff`) because registration focuses
   its addition. Do this for commits and repository-diff launches. Do not
   register for fixtures/patches. This mirrors the window's startup rather than
   delaying the first useful `2` press behind another acquisition.
4. **Say all three empty states honestly.** No repository means no registration
   and exact `no files pane` on focus. A successful empty `Status` draws
   `working tree clean` and header label `<describe> · 0 changed`. An initial
   status-read failure still registers a retryable files tenant, logs the error
   before raw mode, and draws `status unavailable` with that header label—do not
   call a failed read clean. A later refresh failure preserves the last good rows
   and reaches the existing status-line error path.
5. **Extend `Screens`, not the registry.** Add a `Files` variant carrying the
   view, label, and generation. Adapt the existing mode/label/generation,
   refresh, resize, paint, status, mouse, copy/select, and `run(view.*)` methods.
   Use pane-local coordinates and `Screen::span`; no branch goes into
   `panes.rs`. File-list copy is the current `letters path`; drag selection and
   `select.all` remain inert, while a click moves the cursor and scrollbar drag
   uses the existing local last-column convention.
6. **Dispatch all files verbs at the app/job boundary.** Add app helpers modeled
   on the window. Read section/path(s) from the focused `Files`, construct only
   the matching `Write`, and submit it through `self.submitter`. Say every refusal
   before queueing. `files.stash` is repository-scoped and selection-free, using
   `stash_push(None)` as the window does; all other files commands require the
   files pane to be focused. Queue shutdown uses the existing sentence.
7. **Keep discard confirmation as view state.** The first valid press stores the
   exact `(Section, PathBytes)` and puts the question in `App::message`; the
   second identical press clears the arm and queues discard/delete. Any cursor
   move, wheel, mouse press on another row, status replacement, or different
   discard target clears/moves the arm. Focus changes alone may preserve it, as
   the window does. Do not add a mode, yes/no key, or command name.
8. **Generalize the existing prompt state.** Replace `search: Option<String>`
   with a typed `Prompt` enum: `Search { query }`, `CommitMessage { text }`, and
   `AmendMessage { text }`. Keep the input-only resolver and edit sanitizer
   generic; route edits to live filtering only for Search. While any prompt
   stands, mouse/pane/view/global commands are isolated exactly as search is now.
9. **Use a single-line bottom-row message UX.** `files.commit` opens an empty
   ` commit: <text>█` prompt; amend opens ` amend: <text>█`. Keep the caret and
   newest tail visible when text exceeds the row. Enter closes, trim-checks, and
   submits `Write::commit` or `Write::amend`; Esc closes and discards text with no
   write. Amend differs only in label and constructor: it starts empty, does not
   prefill HEAD's message, and adds no extra confirmation because the window does
   not. Preserve Search's exact `/query█ · hits/total`, accept, and cancel frames.
10. **Join the existing refresh wave.** `Screens::Files::refresh` calls
    `repo.status()`, replaces rows/label on success, clears any armed discard,
    and sets its generation only after apply. `refresh_stale` remains a single
    registry-wide loop. A write finishing while files is unfocused or hidden in
    narrow mode must still refresh it.

## Changes by layer (every file)

### Core

No changes.

- `core/src/status.rs` already owns all repository facts and raw path identity.
- `core/src/command.rs` already owns every files/input/pane command and default.
- `core/src/host.rs` already exposes those registries to `gitten.toml` and both
  clients.
- Keep `core/Cargo.toml` dependency-free. If this feature requires a new core
  command, confirmation variant, terminal row type, or dependency, STOP.

### App and git acquisition

No changes.

- `app/src/verbs.rs` already exposes every required `Write` signature and
  refusal boundary; use it unchanged.
- `app/src/acquire.rs` remains the commits/diff CLI acquisition seam. Do not add
  `View::Files` or `Data::Status` for a sidebar the startup repository handle can
  read, matching the window's existing status acquisition.
- The runner, submitter, generation, `gitten.toml` parser, and `Repo` trait remain
  unchanged. All writes go through `Write` behind the retained `Handle`; no
  direct `Repo` write call is allowed.

### Shell, web, docs, plans, manifests

No changes. They are references and regression surfaces only. In particular,
do not modify `plans/README.md`.

### TUI

**`tui/src/files.rs` (new)**

- Define terminal-owned `Section::{Staged,Unstaged,Untracked,Conflicts}` and
  private flattened `Entry::{Heading,File}`/file display data. Preserve
  `PathBytes` separately from lossy/pre-split display text. Map changes,
  conflicts, rename origins, section labels, and colors once at prepare time
  except theme lookup, which remains live at paint.
- Define `Files` with rows, `Viewport`, pane width, `Bar`, initial availability,
  and `armed: Option<(Section, PathBytes)>`. Constructors must distinguish
  successful status (including clean) from unavailable initial status.
- Implement `resize`, `replace`, `paint`, `status`, `current_file`,
  `cursor_section`, `paths_in`, `confirm_or_arm_discard`, all shared `view.*`
  movements, wheel/page movement, pane-local mouse press/drag/release,
  scrollbar hit/drag, `copy_text`, and inert selection methods in the shapes
  `Screens` already expects.
- Paint every row with `Screen::span(y, x, cols)`. Draw headings as quiet caps
  plus right-side count; file rows as a two-cell status column, path, optional
  dim rename origin, full-width cursor background only when focused, armed row
  in `chrome.error`, and overlay scrollbar on the pane's final local column.
  The clean/unavailable message is quiet at the top-left. Do not allocate or
  recompute flattened rows per frame.
- Add unit tests beside the view for flatten order/counts/letters/rename/raw-byte
  identity, heading skipping, movement and scrollbar, refresh anchoring, clean
  versus unavailable states, arm/disarm, copy text, live theme ink, nonzero-x
  clipping, and degenerate dimensions.

**`tui/src/lib.rs`**

- Export `pub mod files;` and add it to the module table comment. No other public
  seam or dependency changes.

**`tui/src/main.rs`**

- Import `gitten_tui::files::Files`; add `Screens::Files { view, label,
  generation }` and exhaustively adapt every existing adapter method. Files mode
  is exactly `"files"`.
- In `App::new`, for `Source::Repo` with a handle, eagerly acquire status and a
  repository description, register `files` with
  `Placement::sidebar("files")`, and restore the original focused pane. Keep
  direct fixture/patch launches without a files registration. Rebuild header-key
  cache and modes after all registrations, as today.
- Add `files_stage`, `files_stage_all`, `files_discard`, `files_ignore`,
  `files_stash`, `begin_commit_message`, and `begin_amend_message` helpers. Match
  the window's exact target rules and refusal text closely enough for shared
  behavior to be recognizable; never build or submit a refused job.
- Dispatch all seven existing `files.*` names before generic focused-pane
  `run`. Do not match a physical key. `files.commit`/`files.amend` validate
  focused pane and repository before opening input; trim-empty accepts close and
  say `a commit needs a message` without submission.
- Replace the search-only state/helpers with typed `Prompt`, generic input edit
  and finish routing, while retaining search-specific live filter behavior and
  its exact tests. `Input::Paste` is accepted whenever any prompt stands and is
  sanitized once. Help/mouse/pane actions remain inert under all prompt kinds.
- Draw commit/amend prompts on the status row with tail visibility and the
  search prompt with its existing shape. Do not create a second input row or
  reduce pane geometry.
- Add Files to `Screens::refresh`: one status read, prepare off the render path,
  replace/label/generation on success, old data on error. Leave
  `App::refresh_stale`'s attempt-all/first-error loop structurally unchanged.
- Extend existing fake repository recording so tests can observe status
  generations and exact stage/unstage/stage-many/unstage-many/discard/delete/
  ignore/stash/commit/amend calls. Keep tests headless and deterministic.

**`tui/src/panes.rs`**

No change. Registration must be sufficient. If it is not, stop and escalate.

## Test list

All tests are headless. Use exact command/job assertions and fixed `Screen`
cells/text/ink rather than opaque snapshots.

1. **`repository_startup_registers_files_into_the_sidebar_ring`**
   (`tui/src/main.rs`) — build commits and direct working-tree-diff starts with a
   fake handle. Assert stable registrations include files, commits/diff keep
   launch focus, `2` focuses files, sidebar order is files then commits, Ctrl-J/K
   cycle both directions, headers derive live keys, and fixture/patch starts
   still answer exact `no files pane`. Assert `tui/src/panes.rs` is untouched.
2. **`files_sections_render_in_porcelain_order_with_counts`**
   (`tui/src/files.rs`) — fixture with every `Change`, all seven conflicts, a
   staged rename, duplicate staged+unstaged path, untracked path, and invalid
   UTF-8 bytes. Assert headings/order/counts, letters, lossy display only,
   distinct `(section,path)` rows, raw verb bytes, colors, and clipping inside a
   nonzero-x span.
3. **`files_view_skips_headings_and_refreshes_by_section_path`**
   (`tui/src/files.rs`) — assert key/page/wheel/mouse movement never rests on a
   heading; scrollbar is local; refresh preserves the staged twin rather than
   jumping to the unstaged twin; vanished anchors clamp; clean refresh clears
   viewport and any discard arm.
4. **`files_stage_submits_stage_or_unstage_and_says_refusals`**
   (`tui/src/main.rs`) — for each section assert staged submits exactly
   `Write::unstage` and unstaged/untracked/conflict submit exactly `Write::stage`
   with original bytes. Wrong focus, no row, no repo, and closed queue each say
   the expected refusal and submit zero jobs.
5. **`files_stage_all_uses_the_cursor_side_as_one_job`**
   (`tui/src/main.rs`) — staged cursor submits one `unstage_many` containing all
   staged paths; every other section submits one `stage_many` containing
   unstaged+untracked only. Assert conflicts are excluded, raw bytes survive,
   empty target and no repo refuse, and no per-path job/generation wave appears.
6. **`files_discard_arms_then_submits_the_exact_destructive_job`**
   (`tui/src/files.rs`, `tui/src/main.rs`) — unstaged first press asks and submits
   nothing, identical second press submits `Write::discard`; untracked uses
   `delete` wording and `Write::remove_untracked`. Assert staged/conflict/no row/
   no repo refuse, and move, wheel, refresh, mouse-to-another-row, or another
   target requires a fresh two presses. Focus away/back alone preserves the arm.
7. **`files_ignore_only_submits_for_untracked_rows`**
   (`tui/src/main.rs`) — untracked submits exactly `Write::ignore` with raw path;
   staged/unstaged/conflict/no row/wrong focus/no repo/closed queue refuse and
   submit nothing.
8. **`files_stash_pushes_without_a_selection_or_message`**
   (`tui/src/main.rs`) — dispatch in files mode submits exactly
   `Write::stash_push(handle, None)` even on a clean tree; no repository and
   closed queue refuse. Assert it reads no selected path and adds no prompt.
9. **`commit_message_prompt_accepts_cancels_and_isolates_input`**
   (`tui/src/main.rs`) — from focused files, `c` opens an empty one-line bottom
   prompt; printable command-looking keys and multiline paste become text, not
   commands; mouse/pane focus/view/global commands are inert; a long message
   keeps tail+caret visible; Esc closes with no job; Enter submits the full
   unsliced `Write::commit`; whitespace-only, wrong focus, and no repo refuse.
10. **`amend_message_uses_the_same_prompt_but_the_amend_job`**
    (`tui/src/main.rs`) — `A` opens empty `amend:` input, not HEAD's old subject;
    Esc cancels, whitespace refuses, and Enter submits exactly `Write::amend`
    with the full sanitized text. Assert there is no double-ask or new command.
11. **`a_files_write_refreshes_every_generation_tenant`**
    (`tui/src/main.rs`) — finish a successful and a refused recorded write;
    assert generation advances, files status is read again, section/path anchor
    and label update, commits and repository diff also refresh while unfocused or
    narrow-hidden, fixtures remain untouched, every pane is attempted after the
    first refresh error, and old files data survives its own refresh error.
12. **`files_empty_states_and_narrow_frames_are_honest`**
    (`tui/src/main.rs`) — assert no-repo absence, clean `working tree clean`, and
    initial read-failure `status unavailable` are distinct. At 120 columns files
    and commits split the existing sidebar into canonical equal slices beside
    diff; at 95/80 only focus is drawn full-body. Assert label/header is the live
    focus key plus `files` plus `<describe> · N changed`, title/status name files,
    no row crosses a divider, and zero/one-row bodies do not panic.
13. **`wave_1_and_plan_016_features_survive_the_files_tenant`**
    (`tui/src/main.rs`) — retain exact search prompt/filter accept/cancel and
    input-only isolation; hunk stage/unstage submission and registration-wide
    refresh; Markdown pane-width reflow; persistent diff/back; focused-only
    `view.*`/wheel/copy; mouse Down capture through Up; live config keys/theme/
    scrolloff; help modes; and `no status/branches/stashes pane` notices. Update
    old assertions that expected only one sidebar, but weaken none of their
    behavioral claims.
14. Run the required gates exactly as written:

    ```sh
    cargo test -p gitten-tui -p gitten-app
    cargo test -p gitten-core
    cargo fmt --check
    cargo clippy -p gitten-tui -p gitten-app --all-targets -- -D warnings
    ./check.sh
    git status --short
    ```

Done criteria:

- [ ] Repository starts register exactly one `files` tenant; no-repository starts
      register none; `2` and the sidebar cycle work only through existing names.
- [ ] Sections, counts, raw paths, cursor anchors, clean/unavailable states,
      headers, wide slices, and narrow focus are proven in fixed-cell tests.
- [ ] Every files verb submits the exact shared `Write` or gives a tested refusal;
      discard never constructs a job before its second valid press.
- [ ] Commit/amend input is one-line, configurable through existing input
      commands, isolated, cancellable, tail-visible, and submits unsliced text.
- [ ] Every job finish refreshes all stale repository tenants and preserves old
      data on a failed refresh; Wave 1/plan 016 tests remain behaviorally intact.
- [ ] `tui/src/panes.rs`, core, app, shell, web, docs, manifests, lockfile,
      `gitten.toml`, and `plans/README.md` are unchanged.
- [ ] `git status --short` lists only `tui/src/files.rs`, `tui/src/lib.rs`, and
      `tui/src/main.rs`; every gate exits 0 without launching a client.

## Stop conditions

Stop and escalate if any occurs:

- Registering `files` requires any edit to `tui/src/panes.rs`, a new placement,
  a changed canonical rank, a layout branch, or different 96-column behavior.
- Any files/input command or verb signature cited above is absent or semantically
  different, or parity appears to require a new terminal alias, physical-key
  match, command name, default binding, or confirmation mode.
- Status acquisition cannot be expressed as a read through the retained
  repository handle without adding a Files CLI root view or moving I/O into
  `tui/src/files.rs`. Bring back the concrete type/ownership conflict.
- A writer calls `Repo` directly, runs on the terminal loop, bypasses
  `Write`/`Submitter`, queues one job per path for stage-all, or fails to preserve
  raw path bytes.
- Discard can submit on its first press, a stale arm survives cursor/wheel/mouse/
  refresh movement, staged/conflicted discard becomes destructive, or untracked
  discard uses tracked checkout mechanics.
- Prompt input resolves against the full mode stack, lets globals/pane movement
  consume message characters, needs a second terminal row, inserts line breaks,
  slices the logical message to the visible tail, or changes search's established
  accept/cancel/filter behavior.
- A job finish refreshes only focused/visible panes, stops after the first error,
  marks a failed files refresh current, or replaces last-good status with a false
  clean tree.
- A files painter uses whole-screen `Screen::row`, writes outside its span,
  allocates flattened strings per frame, parks the cursor on a heading, or treats
  lossy display text as the path passed to a verb.
- Any test requires a live external repository, raw mode, tty, network, launched
  client, wall-clock sleep, or modification outside the three authorized TUI
  files; or a verification command fails twice after one reasonable correction.

## Risks

- **Destructive target drift (HIGH):** the same path can exist in staged and
  unstaged sections. Bare-path selection or a confirmation stored outside the
  view could execute against a different row after refresh. Section+raw-path
  identity and disarm tests are release gates.
- **Prompt regression (HIGH):** search currently uses a simple `Option<String>`.
  Generalizing it can accidentally restore globals while input stands, break
  live filtering, or route accept to the wrong consumer. A typed prompt enum and
  the Wave-1 survival test keep destinations explicit.
- **Generation/borrow complexity (HIGH):** `Screens` centralizes all tenant
  behavior, and adding one variant touches refresh, paint, input, and copy. A
  missed arm can compile only if hidden behind a wildcard; keep exhaustive
  matches and test files both focused and hidden.
- **Startup latency (MED):** eager status adds one git read before first frame.
  The window makes the same product choice and overlaps its startup reads. Keep
  preparation linear and flatten once; do not make registration lazy or move a
  status read onto the render path.
- **Short sidebar slices (MED):** files and commits share terminal height equally
  in wide mode because that is plan 016's replaceable policy. Do not reopen the
  foundation here. Fixed-height frames should expose whether headers, clean/error
  states, and heading settling remain useful in tiny slices.
- **Initial error wording (MED):** the window currently degrades a failed initial
  status read to an empty pane, which can look clean. This plan deliberately says
  `status unavailable` in the terminal so the required no-data state is distinct;
  reviewers should scrutinize wording, while preserving the stronger honesty
  invariant and retryable registered tenant.
- **Parity by duplication (MED):** section-to-verb dispatch necessarily exists
  once per client because clients map command names to methods. If implementation
  starts duplicating porcelain parsing, repository writes, job semantics, or
  viewport arithmetic, stop—the shared model/verbs/runner/Viewport seams were
  missed.
