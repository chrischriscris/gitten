# Plan 018: Add the terminal branches pane and its shared branch verbs

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving on. If a
> STOP condition occurs, stop and report; do not rewrite the pane registry, do
> not invent terminal-only command names, and do not fold rebase into this pass.
> Do not update `plans/README.md`: the integrator owns the index.
>
> **Drift check (run first)**:
> `git diff --stat 7b7ec51..HEAD -- core/src/command.rs core/src/refs.rs app/src/acquire.rs app/src/verbs.rs git/src/lib.rs shell/src/views/branches.rs shell/src/main.rs tui/src/lib.rs tui/src/main.rs tui/src/panes.rs tui/src/commits.rs tui/src/screen.rs tui/src/scrollbar.rs`
> If an in-scope path changed, compare the cited symbols and contracts below
> with live code before editing. Changes from a concurrently executed files-pane
> plan are not a dependency and must be preserved. A mismatch in
> `Panes::register`, canonical ranks, `Screens::refresh`, input-mode resolution,
> job-generation handling, or a branch verb signature is a STOP condition.

## Status

- **Priority**: P1
- **Effort**: L (multi-day: a new headless list view, three-read acquisition and
  refresh, five write paths, and turning the search-only field into a reusable
  one-line prompt without regressing plan 016)
- **Risk**: HIGH
- **Depends on**: plan 016, already landed at this baseline; explicitly does
  **not** depend on plan 017 or any files-pane implementation
- **Category**: terminal feature / client parity
- **Planned at**: commit `7b7ec51`, 2026-08-27
- **Confidence**: HIGH on shared commands, verbs, pane registration, and branch
  model; MED on the final cell-level branch-row density until fixed-width frame
  tests prove the marks and tracking counts at 40 columns

## Why this matters

The terminal now has the extensible pane foundation, but its canonical
`branches` slot is still empty: the shared keymap resolves `3` to
`branches.focus` and the branch mode already contains checkout, create, rename,
delete, tag, and rebase names, while the TUI can only answer `no branches pane`.
This pass registers a real repository-backed tenant without changing the ring,
and sends every mutation through the same `Write` jobs and runner the window and
extensions use.

The result is deliberately narrower than copying the whole desktop surface.
Local and remote branches plus detached HEAD are the window's actual branches
view; tags have a creation verb but are not a section there. Rebase is a
lifecycle, not one more row action: the window couples rebase-onto to destructive
confirmation, git's conflict state, and repository-level abort/continue. That
whole lifecycle is deferred rather than leaving a rewrite half-wired.

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Required feature gate | `cargo test -p gitten-tui -p gitten-app` | all existing and new tests pass; no tty, raw mode, alternate screen, or window |
| Core regression | `cargo test -p gitten-core` | all shared command/ref tests pass |
| Format | `cargo fmt --check` | exit 0 |
| Lint | `cargo clippy -p gitten-tui -p gitten-app --all-targets -- -D warnings` | no warnings |
| Repository gate | `./check.sh` | exit 0; no client is launched |
| Scope | `git status --short` | only `tui/src/branches.rs`, `tui/src/lib.rs`, and `tui/src/main.rs` are modified |

Do not run `./dev tui`, `./dev desktop`, or any command that takes a tty or
opens a window. Render into in-memory `Screen` values and drive the existing
fake `Repo` plus `Runner` in tests.

## Scope

In scope:

- A terminal-owned `Branches` view registered as
  `Placement::sidebar("branches")` on every repository-backed launch.
- Local and remote sections, detached-HEAD visibility, current-local marking,
  upstream distance, empty states, pane-local viewport/mouse/scrollbar drawing,
  and byte-preserving verb targets.
- Existing command names `branches.checkout`, `branches.new`,
  `branches.rename`, `branches.delete`, and `branches.new-tag`.
- A reusable one-line terminal prompt state built from the existing search
  prompt path; new, rename, and tag each use one field.
- Delete's exact two-press, same-target confirmation pattern.
- Repository reads (`branches`, `remote_branches`, `head`) at registration and
  again through registration-wide generation refresh.
- Preservation of all plan-016 focus, geometry, mouse, narrow-mode, search,
  staging, Markdown, configuration, and copy behavior.

Explicitly out of scope:

- **Rebase-onto, abort, and continue.** Name the follow-up **“TUI rebase
  lifecycle: rebase-onto, conflict state, abort, and continue”**. This pass must
  leave `commits.rebase-onto`, `rebase.abort`, and `rebase.continue` unhandled in
  the TUI; their existing keys continue to report that they do nothing here.
- A tags section, tag reads, annotated-tag messages, tag deletion, or a tags
  pane. This pass creates a lightweight tag on a selected local branch only.
- Remote-branch deletion, force deletion, creating-and-checking-out in one
  action, branch sorting/filtering/search, or automatic fetch.
- A direct `branches` CLI view. The branches pane is a tenant beside either of
  the existing `commits`/`diff` startup views, not a third `app::cli::View`.
- Any change to `core`, `app`, `git`, `shell`, `tui/src/panes.rs`, dependencies,
  `gitten.toml`, or a key table. Those files are reference contracts only.
- Any dependency on plan 017. If a files pane happens to exist by execution
  time, preserve it as another independently registered sidebar tenant.

## Baseline facts (provenance)

### Plan 016 is the law, and already has the branch slot

- `tui/src/panes.rs:63-75` assigns `branches` canonical sidebar rank 2 between
  files and commits. `Placement::sidebar` fills that rank from the stable name
  (`tui/src/panes.rs:78-98`). Do not edit either table.
- `Panes<T>::register` replaces by name and focuses the resident, while
  `get`/`get_mut`, `iter_mut`, focus, reading order, and sidebar cycling are
  generic (`tui/src/panes.rs:300-517`). Registering the tenant is sufficient to
  put it in the ring; a branches-specific layout branch would violate the seam.
- Wide geometry divides the sidebar equally among every registered list and
  keeps one main region; below 96 columns only the focused pane is placed
  (`tui/src/panes.rs:203-297`). A third list must use that behavior unchanged.
- `App::dispatch` already answers all ten pane/focus names before view dispatch,
  including `branches.focus` (`tui/src/main.rs:1196-1218`). Once registration
  lands, the existing global `3` works without another dispatch arm.
- `App::sync_modes` adds `panes` when at least two sidebar lists exist, then the
  focused screen's mode (`tui/src/main.rs:673-696`). A repository launch with
  branches plus commits therefore activates the already-configured Ctrl-J/K
  sidebar ring; a diff launch with only branches as its sidebar does not pretend
  there is a second list.
- Pane headers, labels, focus keys, clipping, resize, and narrow visibility are
  derived from registry entries and cached geometry in `App::draw`
  (`tui/src/main.rs:1551-1696`). The new view supplies only its label, status,
  and pane-local paint.

### The desktop branches view defines the row grammar

- `shell/src/views/branches.rs:1-15` states the structure: local branches first,
  then remote-tracking branches; detached HEAD is a visible top row. The view
  does not contain a tags section.
- Its flattened rows are `Detached`, non-selectable `Heading`, `Local`, and
  `Remote` (`shell/src/views/branches.rs:31-150`). Verb targets remain raw bytes:
  local name, separate remote/name halves, or detached state.
- `flatten` emits detached HEAD, `LOCAL <count>`, local rows, then
  `REMOTE <count>` and remote rows (`shell/src/views/branches.rs:187-264`). The
  checked-out local uses the accent; other locals use lane inks; remotes are
  hollow and faint. Tracking renders distance only as `↑n`/`↓n`, suppresses
  zeros, and says `(gone)` when counts are unknowable
  (`shell/src/views/branches.rs:152-185`).
- Headings are skipped by cursor settling, so every resting selection is an
  actionable row (`shell/src/views/branches.rs:383-404,585-642`). An empty model
  renders the top-left sentence `no branches yet`
  (`shell/src/views/branches.rs:734-752`).
- Refresh anchors the cursor by exact `Target` bytes, clears destructive arming,
  and clamps/settles when the target disappeared
  (`shell/src/views/branches.rs:471-512`). The terminal must preserve the same
  identity semantics with its own `Viewport`.
- The desktop row uses a mark, name, and right-aligned tracking distance; the
  name truncates before it can displace the distance
  (`shell/src/views/branches.rs:806-870`). The terminal equivalent must make the
  same priority explicit in cells and paint only through `Screen::span`.

### What row selection means for each included verb

- `branches.checkout` accepts a local branch or a remote-tracking row. A remote
  target is joined as `remote/branch` for git and checks out detached; the
  detached-HEAD row refuses with `HEAD is already detached here`
  (`shell/src/main.rs:2173-2214`).
- `branches.new` reads no row and creates at HEAD without checking it out
  (`shell/src/main.rs:2285-2290`; `app/src/verbs.rs:295-302`). It remains
  available in an empty/unborn repository; the backend, not the view, decides
  whether HEAD is a valid start.
- `branches.rename` accepts local rows only. The prompt retains the original
  raw bytes and pre-fills the visible name only when those bytes are valid UTF-8;
  otherwise it starts empty so accepting cannot rename to lossy mojibake
  (`shell/src/main.rs:2388-2415`).
- `branches.delete` accepts local rows only. Detached and remote rows receive
  explicit refusals; the write uses `force = false`, so unmerged and checked-out
  branches are refused in git's own words (`shell/src/main.rs:2553-2606`).
- `branches.new-tag` accepts local rows only, opens one tag-name field, and
  creates a lightweight tag (`message = None`) at that branch revspec
  (`shell/src/main.rs:2340-2361,2522-2550`). The TUI must improve only the
  transport detail: retain the selected branch's raw bytes as the target rather
  than round-tripping its lossy display text.

### Names and reads are already shared data

- `core/src/refs.rs:25-35` aliases `RefName` to byte-preserving `PathBytes`;
  display is lossy but addressing is not. `Branch` includes raw name, commit,
  optional upstream, and `head`; `RemoteBranch` keeps remote and branch separate
  (`core/src/refs.rs:94-163`).
- `HeadState` distinguishes an attached (possibly unborn) branch from detached
  HEAD (`core/src/refs.rs:39-59`). A bare/unborn repository can therefore have
  an attached name but zero branch rows; the pane's honest body remains
  `no branches yet`.
- Tags and stashes do exist as separate read models (`core/src/refs.rs:165-213`),
  and `Repo` exposes `stashes()` and `tags()` (`git/src/lib.rs:377-390`). They are
  deliberately not branch-pane acquisition in this plan.
- The repository read seam already exposes `branches()`, `remote_branches()`,
  and `head()` (`git/src/lib.rs:338-375`). The binary backend implements those
  reads without decoding refnames (`git/src/lib.rs:1185-1279`).
- `app/src/acquire.rs:34-38,127-224` models only startup `Commits` and `Diff` CLI
  data. Adding branches to that enum would imply a new CLI view and would not
  match the already-landed repository-tenant pattern. The TUI should call the
  injected `Repo` reads from its assembly layer, as the window does, and pass
  already-loaded ref data into the view.

### The same command names and writes already exist

- The shared keymap binds branch mode exactly once: Space checkout, `n` new,
  lowercase `r` rebase-onto, uppercase `R` rename, `d` delete, and `T` new tag
  (`core/src/command.rs:428-439`). `3` globally focuses branches
  (`core/src/command.rs:373-384`). Do not add or shadow any of these.
- `Commands::builtin` registers the same public names and descriptions,
  including delete's “asked twice” disclosure
  (`core/src/command.rs:1022-1043`). Help and `gitten.toml` therefore update
  automatically when the focused screen reports mode `branches`.
- Exact job constructors are:
  `Write::checkout(&Handle, Vec<u8>)`,
  `Write::create_branch(&Handle, Vec<u8>, Option<Vec<u8>>)`,
  `Write::rename_branch(&Handle, Vec<u8>, Vec<u8>)`,
  `Write::delete_branch(&Handle, Vec<u8>, bool)`, and
  `Write::create_tag(&Handle, Vec<u8>, Vec<u8>, Option<String>)`
  (`app/src/verbs.rs:286-342`). The TUI submits those jobs; it never invokes a
  `Repo` write directly.
- The backend refuses blank branch/tag names, option-shaped names beginning in
  `-`, and otherwise returns git's own error text; non-force deletion uses
  `git branch -d` (`git/src/lib.rs:1482-1523,1708-1725,1960-1989`). The prompt
  gives the earlier actionable `a branch needs a name` / `a tag needs a name`
  response, but duplicate/invalid/unmerged/current-branch failures must still
  travel through the job unchanged.
- `Write` itself is a `Job`; `run` calls the captured trait operation and the
  runner emits its name/outcome (`app/src/verbs.rs:477-489`). Existing app tests
  prove raw non-UTF-8 branch bytes survive checkout/delete/rename and that a
  backend refusal is returned verbatim (`app/src/verbs.rs:1020-1043,1046-1169`).

### Delete confirmation is pane state, not a dialog or core state

- The desktop stores one armed `Target`. First press stores the exact target and
  returns false; a second press on that same target clears the arm and returns
  true; another target merely moves the question. Cursor move, wheel, or refresh
  clears it (`shell/src/views/branches.rs:352-380,669-689`).
- The first delete press says exactly
  `delete branch <lossy display>? press again to confirm`; the second clears the
  question, queues `Write::delete_branch(..., false)`, and lets the running band
  speak (`shell/src/main.rs:2553-2606`). The terminal answer is the same status
  sentence and an error-coloured armed row, with the arm keyed by raw bytes.
- Nothing new belongs in `core`: `branches.delete` and its destructive
  description are already public data. The exact selected refname is pane-local
  state, and only its lossy presentation is disclosed to the status line.

### The existing terminal prompt and runner are the extension points

- The TUI's current `search: Option<String>` is app-owned input state; while it
  exists, keys resolve against exactly `input`, plain text edits the value,
  paste is sanitized, Enter accepts, Esc cancels, mouse waits, and the prompt
  occupies the status row (`tui/src/main.rs:513-526,891-1057,1632-1651`). Branch
  names need the same input ownership, not a second event/key loop.
- The prompt is presently search-specific (`edit_search`, `apply_query`, and
  `finish_search`). Generalizing its state must preserve live search filtering
  and cancel restoration exactly; name prompts do not edit repository state
  until accept.
- `App::hunk_verb` is the submission precedent: establish all view/source
  refusals first, construct a `Write`, submit through `Submitter`, and report a
  shutting-down queue (`tui/src/main.rs:1435-1477`).
- Every finished job advances `generation` and calls `refresh_stale`; that method
  iterates **all registered panes**, remembers the first refresh error, and still
  attempts the rest (`tui/src/main.rs:1479-1549`). A branch write needs no
  special completion path: a successful or refused job makes every repository
  tenant re-read.

### Rebase is deliberately a complete follow-up

- The desktop's `commits.rebase-onto` is aimed from the selected local or remote
  branch row, refuses detached HEAD, asks twice, joins a remote target without
  losing its halves, then submits `Write::rebase_onto`
  (`shell/src/main.rs:2102-2160`; `app/src/verbs.rs:201-212`). It moves the
  **currently checked-out branch** onto the selected target; it does not rebase
  the selected row itself.
- A dirty tree is returned as git's refusal. A conflict returns nonzero while
  leaving `.git/rebase-merge`/`.git/rebase-apply` standing; the backend exposes
  that state through `Repo::rebase_in_progress` and implements
  `rebase --abort`/`--continue` (`git/src/lib.rs:693-765,1644-1671`).
- The window has no dedicated rebase-state pane or proactive indicator: its
  shell never calls `rebase_in_progress`. It surfaces the failed job's text in
  the normal notice/running band and keeps `rebase.abort` / `rebase.continue`
  available as repository-level commands (`shell/src/main.rs:2038-2068,
  3053-3057`). Core binds those exits in commits mode on `A`/`C`
  (`core/src/command.rs:483-490`).
- Therefore adding only lowercase `r` here would strand a conflict. The named
  follow-up must plan rebase-onto, same-target double confirmation, runner error
  surfacing, abort/continue from any repository pane, and head/branch/status
  refresh as one unit. This plan's tests must assert all three names remain
  unhandled so scope cannot drift accidentally.

## Approach

1. **Build a terminal-native, pure ref list.** Add `tui/src/branches.rs` with a
   `Prepared`/`prepare` boundary and a `Branches` view. Flatten once per load into
   detached, heading, local, and remote rows. Store display strings, tracking
   text, current state, and stable local hue indices beside byte-preserving
   targets; resolve those indices through the live theme during paint, with no
   per-frame formatting or allocation.
2. **Use the desktop's section grammar in cells.** Detached comes first, then
   `LOCAL <count>` and `REMOTE <count>` only when non-empty. Default marks are
   `●` for the current local, `•` for another local, and `○` for remote; current
   uses accent and every other local a stable lane ink. Supply an ASCII mark set
   (`*` current, `o` local, `o` remote with remote remaining faint) through a
   small constructor-owned `Marks` value, analogous to `commits::Glyphs` and
   `scrollbar::Bar`, so `--ascii` never grows a branch in paint. Current is
   therefore distinguishable without relying on colour in either set.
3. **Make row selection an exact verb target.** Headings are never a resting
   cursor row. Local, remote, and detached rows produce `Target`; movement,
   paging, top/bottom, wheel, and mouse settle off headings. Copy yields the
   exact row spelling (`name` or `remote/name`) lossily for the clipboard, but
   every job receives original bytes.
4. **Register on every repository launch.** In `App::new`, when `started.repo`
   exists, read description plus local branches, remote branches, and HEAD;
   execute the three fallible ref reads in one `std::thread::scope`. Prepare and
   register `branches` with `Placement::sidebar("branches")` at generation zero,
   then restore the startup pane's focus. Fixture and patch launches register no
   branches tenant and continue to answer `no branches pane`.
5. **Treat read failure as an honest empty tenant, not a lost app.** At startup,
   a local/remote read failure registers `no branches yet`, seeds the status
   message with `branch reads failed: <error>`, and does not prevent the already
   acquired commit/diff view from opening. A HEAD-only failure keeps successfully
   read branches but marks none current and reports the failure. On generation
   refresh, retain the old view and generation on failure so the error surfaces
   and a later wave can retry.
6. **Extend `Screens`, never `Panes`.** Add a repository-shaped `Branches`
   variant with view, label, and generation but no CLI `Source`. Adapt
   `mode`, label/generation, resize, paint, status, mouse/copy/select, `run`, and
   `refresh`. The `run` arm handles only shared `view.*`; branch write commands
   remain app-level because they need the repository and submitter.
7. **Generalize the one-line prompt once.** Replace search-only state with an
   app-owned `Prompt` enum and shared text/editor state. `Search` retains live
   apply/cancel behavior. `BranchNew`, `BranchRename { from }`, and
   `TagNew { at }` capture their stable pane name and raw target bytes at open.
   A single label plus one text value is enough for each; annotated tags and
   multi-field sequencing are out of scope.
8. **Give rename terminal-appropriate prefill semantics.** A valid UTF-8 local
   name is prefilled and initially selected: the first character or paste
   replaces it, Backspace/Delete clears it, and accepting without editing keeps
   it. A non-UTF-8 name opens blank while retaining `from` as raw bytes. New and
   tag start blank. Paste uses the current sanitizer; all controls are removed
   and newline/tab become spaces.
9. **Dispatch through existing names and jobs.** Add app-level arms for the five
   included branch commands. Apply the exact target gates above, build the exact
   `Write` constructor, and submit through `self.submitter`. Empty/whitespace
   prompt accept gives the actionable branch/tag sentence without queueing;
   otherwise do not pre-validate git ref syntax or duplicates.
10. **Confirm deletion in the pane.** Store one armed raw `Target` in
    `Branches`. The first `branches.delete` press paints that row with error ink
    and writes the exact question. Only the second press on the same target
    queues non-force deletion. Any cursor move, wheel, mouse move to a different
    row, focus loss, prompt opening, config reload, or refresh disarms it.
11. **Refresh by generation and identity.** `Screens::refresh` calls the same
    three-read helper for a stale branches tenant, replaces prepared data,
    anchors by raw `Target`, updates label and generation only on success, and
    clears any arm. `refresh_stale` remains unchanged and registration-wide.
12. **Preserve Wave-1/016 behavior as acceptance, not cleanup.** Existing
    commits/diff construction, geometry caching, narrow focus, search, hunk
    staging, copy, mouse capture, config reload, and Markdown tests remain green.
    Do not special-case a two- or three-pane layout anywhere.

## Changes by layer

### Core

No production or test changes.

- Keep `core/src/refs.rs` as the model. Reuse `Branch`, `RemoteBranch`,
  `HeadState`, `Upstream`, and `RefName`; do not introduce terminal ref types.
- Keep `core/src/command.rs` as the only command/default registry. No new name,
  key, mode, destructive metadata, or TUI alias is needed.
- Keep `core/Cargo.toml` dependency-free.

If a core edit appears necessary, STOP: the read model and all command names are
already present.

### App and git backend

No production or test changes.

- Use the existing `Write` signatures exactly. Do not call `Repo::checkout`,
  `create_branch`, `rename_branch`, `delete_branch`, or `create_tag` from TUI
  dispatch.
- Do not extend `app::acquire::Data`/`View`; branches are a repository tenant,
  not a startup view.
- Let the backend keep the authoritative name, option, merge, current-branch,
  dirty-tree, and duplicate refusals. The terminal contributes only context it
  uniquely knows: row kind, prompt emptiness, and confirmation state.
- Run `cargo test -p gitten-app` because this pass relies on those job contracts,
  even though no app file changes.

### TUI: `tui/src/branches.rs` (new)

- Define public `Marks { local, remote, current }` with `Default` and `ascii()`;
  `Branches::with_marks` is the extension/`--ascii` constructor and
  `Branches::new` uses default marks.
- Define `Target::{Local(RefName), Remote { remote, branch }, Detached}` as the
  exact selected identity. Add a private flattened `Row` carrying all paint
  data, and `Prepared { rows, label }`.
- Implement `prepare(local, remotes, head, describe)` with the exact
  section/mark/tracking decisions above. Keep stable hue indices/current flags,
  not resolved colours, so a config reload recolours the next frame like
  `Commits`; the view receives data and performs no repository read.
- Hold `Viewport`, pane width, `Bar`, drag/grab state, and `armed: Option<Target>`.
  Implement `resize`, `set_scrolloff`, `set_bar`, view movement, pane-local mouse
  press/drag/release, `current`, `copy_text`, inert select-all/select-none,
  `confirm_or_arm_delete`, `disarm`, `replace`, `status`, and `paint`.
- `replace` anchors on the selected raw target, clears arm/gesture state,
  preserves dimensions/bar/scrolloff, and settles onto the next selectable row
  when an anchor disappears.
- Paint each row with `Screen::span(y, x, cols)`. Reserve the rightmost tracking
  text before clipping the name; paint the scrollbar over the last pane column
  using `scrollbar` helpers. Empty data draws `no branches yet` at the first
  content row. Cursor ink appears only when focused; an armed row uses
  `theme.chrome.error` even if unfocused.
- Keep frame work proportional to visible rows. All labels, lossy names,
  remote/name joins, counts, and target lookup data are built at prepare time.

### TUI: `tui/src/lib.rs`

- Export `pub mod branches;` and update the module table comment. No other
  public API change.

### TUI: `tui/src/main.rs`

- Import `Branches`, `Marks`, and the shared ref types. Add a pure
  `load_branches(&dyn Repo, &Host) -> Result<Prepared, BranchReadError>` helper
  (an equivalent small result type is acceptable) that performs local, remote,
  and HEAD reads concurrently and distinguishes a HEAD-only failure from losing
  the branch lists.
- Add `Screens::Branches { view, label, generation }` and cover it in every
  exhaustive adapter. Its refresh has no `Source`: it always reads this app's
  retained repository. Its view mode is exactly `branches`.
- During `App::new`, register branches whenever `repo` exists, apply `Marks::ascii`
  and `Bar::ascii` under `--ascii`, and restore the original startup focus after
  registration. Do not assume commits exists: a direct repository diff launch
  receives branches plus diff; fixtures/patches do not.
- Replace `search: Option<String>` with a reusable prompt state containing
  `kind`, `label`, `text`, and initial-selection state. Rename the routing
  helpers away from search-specific names where necessary, but keep search's
  live filtering and count. `press_input`, paste handling, `sync_modes`, help/
  mouse guards, status-row drawing, accept, and cancel read the generic prompt.
- Opening any name prompt requires focused `branches`, a repository handle, and
  the target constraints above. Capture stable pane name plus raw target at
  opening; accepting refuses if that pane disappeared. While input is open no
  pane command, mouse event, or branch action reaches a view.
- Add dispatch arms for checkout/new/rename/delete/new-tag before
  `Screens::run`. Factor a small `submit_write` helper only if it eliminates the
  repeated queue-shutdown sentence without hiding which preconditions each
  command checks.
- On checkout, join remote/name bytes with one `/`. On tagging, pass the local
  branch's raw bytes directly as the tag target. On delete, require the same raw
  target twice and call `Write::delete_branch(..., false)`.
- Disarm destructive state whenever branch attention moves or the pane loses
  focus. Do this through the branch view's movement/focus adapters; do not put a
  branch-specific condition in `Panes`.
- Keep `refresh_stale`'s loop and first-error policy unchanged. Extend only
  `Screens::refresh` so the generic loop naturally includes branches.
- Extend the existing `FakeState`/`FakeRepo` with local/remotes/head snapshots,
  read counters, write recordings/refusals, and `create_tag`; do not add a second
  runner harness. Existing app constructors that use a repository must seed
  honest branch data or explicitly assert the empty/error case.

No changes are expected in `tui/src/panes.rs`, `tui/src/commits.rs`,
`tui/src/diff.rs`, `tui/src/screen.rs`, `tui/src/scrollbar.rs`,
`tui/src/help.rs`, `tui/src/term.rs`, or `tui/Cargo.toml`. If production changes
there appear necessary, apply the STOP conditions before broadening scope.

## Test list

All tests are headless. View tests use fixed in-memory screens; app tests use
the existing mutex-backed `FakeRepo`, `Runner`, and bounded `until` helper.

1. **`prepare_sections_detached_current_remote_and_tracking_once`**
   (`tui/src/branches.rs`) — fixture: detached HEAD plus two locals (one tracked
   ahead/behind, one gone) and two remotes. Assert order, heading counts, lossy
   display versus raw target bytes, `↑n ↓n`, `(gone)`, zero suppression, and no
   tags row/read.
2. **`current_branch_has_a_textual_mark_in_default_and_ascii_frames`**
   (`tui/src/branches.rs`) — fixture: local `main` with `head=true`, another
   local, and a remote at 40 columns. Assert current mark/accent, distinct remote
   mark/faint ink, name truncation before right-hand counts, no paint beyond the
   pane span, and ASCII contains no non-ASCII mark.
3. **`headings_never_hold_selection_and_empty_repositories_say_so`**
   (`tui/src/branches.rs`) — fixtures: local+remote, remote-only, detached-only,
   empty/bare. Assert initial/down/up/page/top/bottom/mouse settle on targets,
   heading clicks choose the next honest row, and empty paint says
   `no branches yet` without panicking at zero width/height.
4. **`refresh_anchors_raw_target_and_clears_delete_arm`**
   (`tui/src/branches.rs`) — select a non-UTF-8 local, arm delete, replace with
   reordered rows, then remove it. Assert byte-identity anchor survives reorder,
   disappearance clamps/settles, arm and drag/grab state clear, and viewport
   dimensions/Bar survive.
5. **`repository_launch_registers_branches_in_the_existing_ring`**
   (`tui/src/main.rs`) — fixture: commits launch plus fake local/remotes/head.
   Assert names/order are branches→commits→diff by canonical rank, initial focus
   remains commits, global `3` resolves through builtin keymap and focuses
   branches, mode is `branches`, Ctrl-J/K uses the now-real sidebar ring, and
   `tui/src/panes.rs` required no edit.
6. **`direct_diff_gets_branches_while_fixtures_do_not`**
   (`tui/src/main.rs`) — fixtures: repository diff, diff fixture, patch. Assert
   repo diff registers branches+diff and preserves diff focus; non-repository
   launches retain their previous tenant counts and exact `no branches pane`
   response. At 95 columns, `3` swaps full-width visibility to branches; at 96+
   branches and diff occupy the foundation's disjoint rectangles.
7. **`branch_reads_fail_softly_at_start_and_retry_on_generation`**
   (`tui/src/main.rs`) — fake local/remote failure and separate HEAD-only
   failure. Assert startup retains commits/diff, registers honest empty or
   unmarked data with the error message, no generation is falsely advanced,
   and the next successful refresh fills/marks the pane.
8. **`checkout_jobs_local_and_remote_bytes_and_refuses_detached`**
   (`tui/src/main.rs`) — fixtures: non-UTF-8 local, separate remote/name halves,
   detached row, no selection, backend refusal. Assert Space submits
   `Write::checkout` once with exact bytes (remote joined once), detached/empty
   queue nothing with exact notices, and git's refusal reaches `message`
   unchanged after drain.
9. **`new_branch_prompt_accepts_one_name_and_never_checks_it_out`**
   (`tui/src/main.rs`) — fixture: focused branches including an empty/bare view.
   Assert `n` opens one blank `input` prompt, typed and sanitized pasted text is
   inert until accept, Enter submits `Write::create_branch(name, None)`, no
   checkout write occurs, whitespace accept says `a branch needs a name`, Esc
   cancels with no job, and configured input accept/cancel bindings still work.
10. **`rename_prompt_captures_raw_from_and_prefills_only_utf8`**
    (`tui/src/main.rs`) — fixtures: UTF-8 local, non-UTF-8 local, remote, detached.
    Assert uppercase `R` opens one field, valid prefill has replace-on-first-edit
    semantics, unchanged accept is allowed through to git, invalid UTF-8 opens
    blank while `from` remains exact bytes, remote/detached queue nothing with
    `only a local branch can be renamed`, and empty accept queues nothing.
11. **`delete_arms_same_raw_target_then_submits_non_force_once`**
    (`tui/src/main.rs` + `tui/src/branches.rs`) — fixture: two local rows,
    remote, detached. Assert first `d` shows exact question and error-tints only
    that row; second on the same raw target submits
    `Write::delete_branch(name, false)` once; changing row/focus, wheel, mouse,
    prompt, reload, or refresh disarms; remote and detached use exact refusals;
    backend “not fully merged” remains verbatim.
12. **`new_tag_captures_local_raw_target_and_creates_lightweight_tag`**
    (`tui/src/main.rs`) — fixtures: non-UTF-8 local, remote, detached. Assert `T`
    opens one blank tag field, accept submits `Write::create_tag(name,
    raw_branch, None)`, whitespace says `a tag needs a name`, duplicate refusal
    surfaces unchanged, and non-local rows queue nothing with
    `only a local branch can be tagged here`.
13. **`every_branch_write_refreshes_every_registered_repository_pane`**
    (`tui/src/main.rs`) — parameterize checkout/create/rename/delete/tag fake
    successes. For each, drain the shared runner and assert generation advances,
    branch local/remote/head reads repeat, changed current/rows land, commits and
    an acquired diff also refresh while unfocused/hidden, and first refresh error
    does not skip later tenants. Repeat one refused write: it also triggers the
    registration-wide refresh contract.
14. **`generic_prompt_preserves_search_lifecycle_and_mouse_inertness`**
    (`tui/src/main.rs`) — run the existing search prompt assertions after the
    prompt refactor: live filtering/count, text/paste safety, Enter keep, Esc
    restore, exact input-only resolution, configured chords, mouse inertness,
    and search targeted by stable `commits` name. Then open/cancel a name prompt
    and prove it never mutates commit search state.
15. **`branch_header_status_copy_and_help_use_live_registry_data`**
    (`tui/src/main.rs`) — fixed wide frame. Assert header says `branches`, label
    reports local/remote counts, configured focus key (default `3`) is derived
    from `Host`, title/status follow focus, `y` copies only the selected refname,
    help lists the five included branch actions plus the existing deferred rebase
    name from core, and no local key table exists.
16. **`rebase_commands_remain_explicitly_deferred`** (`tui/src/main.rs`) — focus
    branches and dispatch `commits.rebase-onto`; focus commits and dispatch
    `rebase.abort`/`rebase.continue`. Assert no job is submitted and each reports
    `<command> does nothing here`. This is a scope fence for the named lifecycle
    follow-up, not the desired final product behavior.
17. **`wave_1_and_plan_016_features_survive_a_third_tenant`**
    (`tui/src/main.rs`) — reuse existing fixed-frame tests with branches
    registered: commits Enter still replaces/focuses persistent diff, Back
    returns to the last list, wide/narrow geometry and mouse capture remain
    correct, hunk jobs refresh hidden panes, Markdown reflows to diff width,
    config reload preserves focus/modes, and copy/search/help behavior stays
    headless. Do not weaken previous assertions to accommodate branches.
18. Run the required gate exactly as written:

    ```sh
    cargo test -p gitten-tui -p gitten-app
    cargo test -p gitten-core
    cargo fmt --check
    cargo clippy -p gitten-tui -p gitten-app --all-targets -- -D warnings
    ./check.sh
    ```

Done criteria:

- [ ] Every repository-backed launch registers one `branches` sidebar tenant;
      fixture and patch launches register none; startup focus is preserved.
- [ ] Existing `3`/`branches.focus`, pane traversal, help, and `gitten.toml`
      bindings work by registration alone; no pane-registry or key-table edit.
- [ ] Local/remote/detached/empty rows render headlessly with a non-colour-only
      current mark, correct tracking distance, raw-byte targets, span clipping,
      and no per-frame row formatting/allocation.
- [ ] Checkout, create, rename, non-force delete, and lightweight tag creation
      use the exact shared command names, `Write` jobs, `Submitter`, and runner;
      all target and backend refusal tests pass.
- [ ] Delete requires the same raw target twice and disarms on every attention or
      data change named above.
- [ ] One generic input path serves search/new/rename/tag; search semantics and
      configured `input` bindings are unchanged.
- [ ] Every job completion refreshes branches plus every other registered
      repository pane by generation, including hidden and unfocused tenants.
- [ ] Rebase-onto/abort/continue remain explicitly unimplemented and tested as
      such, with the named lifecycle follow-up recorded in this plan.
- [ ] `core/`, `app/`, `git/`, `shell/`, `tui/src/panes.rs`, dependencies,
      `gitten.toml`, and `plans/README.md` are unchanged.
- [ ] All verification commands exit 0 without launching a client or taking a
      tty.

## Stop conditions

Stop and escalate if any occurs:

- `Panes::register`, `Placement::sidebar("branches")`, canonical rank 2, or the
  existing `branches.focus` path cannot register/focus the tenant without a
  `tui/src/panes.rs` change. The 016 API is the foundation; do not rewrite it.
- A files-pane change from parallel plan 017 conflicts in `tui/src/main.rs`.
  Preserve both independent tenants and ask the integrator to resolve the
  assembly; do not depend on, revert, or imitate the files implementation.
- Any included command name, default key, `Write` signature, or raw-byte model
  differs from the cited baseline. Reconcile against shared core/app/git rather
  than creating a TUI alias or adapter verb.
- Implementing branches appears to require extending `app::cli::View`,
  `app::acquire::Data`, adding a dependency, or putting UI/input state in core.
  This is a repository tenant plus terminal drawing, not a new startup view.
- A selected ref is converted through lossy UTF-8 before checkout, rename,
  delete, or tag targeting. Display may be lossy; addressing may not.
- A prompt needs a second field, cursor-editing widget, or annotated-tag message
  to complete the included verbs. Keep this pass to the existing one-line input
  model and lightweight tags.
- Delete can queue on its first press, confirm after the cursor/focus/data moved,
  target a remote/detached row, or silently force-delete after git refuses
  unmerged work. All are scope/safety failures.
- Rebase-onto, abort, or continue starts being implemented. Stop and move it to
  **“TUI rebase lifecycle: rebase-onto, conflict state, abort, and continue.”**
  A branch-only `r`, or abort/continue without the initiating action and state
  tests, is explicitly worse than the deferred lifecycle.
- A startup branch-read failure prevents the already-acquired commits/diff from
  opening, or a refresh failure replaces good old rows with fabricated emptiness
  and advances their generation.
- A write bypasses `Write`/`Submitter`, a finish refreshes only the focused or
  visible pane, or one refresh failure skips later registered tenants.
- A painter uses `Screen::row` for pane content, slices raw text before deciding
  styles, allocates/joins/formats every row per frame, or paints outside its
  span/scrollbar column.
- Existing search, staging, Markdown, copy, mouse capture, wide/narrow geometry,
  config reload, or direct-diff tests must be weakened instead of adapted to the
  additional honest tenant.
- Any test requires a live external repository, network, raw terminal, window,
  or tty, or a verification command fails twice after one reasonable correction.

## Risks

- **Prompt-state refactor (HIGH):** search currently assumes every input edit is
  a live query. A generic enum can compile while cancel applies the wrong side
  effect or a name key falls through to globals. Keep one input router and pin
  each prompt kind's accept/cancel behavior separately.
- **Exhaustive adapter spread (HIGH):** `Screens` centralizes every per-pane
  operation. Missing branches in refresh, mouse, copy, status, or mode routing
  can be hidden by a wildcard. Prefer exhaustive matches and the named
  integration tests.
- **Startup latency (MED):** branch acquisition is three git reads, and upstream
  divergence may add count reads. Run local/remote/head concurrently as the
  window does; never repeat them per frame or per focus.
- **Raw names versus terminal input (HIGH):** existing branch names can be
  arbitrary bytes while typed replacements are UTF-8. Carry existing targets as
  bytes and treat prompt text only as the new UTF-8 byte sequence; never prefill
  invalid bytes lossily.
- **Destructive-state drift (HIGH):** an arm surviving movement, focus, refresh,
  or a prompt can delete a row different from the one the question named. Key it
  by `Target`, clear it broadly, and test each invalidation source.
- **Sidebar height pressure (MED):** on a repository commits launch, branches and
  commits split the existing sidebar height. This is the 016 layout contract,
  not a reason to add custom stacking or a breakpoint. Empty states and short
  panes must remain useful and safe at degenerate heights.
- **Current marking after writes (MED):** checkout changes both HEAD and every
  branch's `head` bit. A partial refresh can show two currents or none. Treat
  local/remotes/head as one prepared snapshot and swap it only when the required
  branch reads succeeded.
- **Rebase discoverability (MED, accepted):** help will list core's lowercase
  `r` even though this pass leaves it unhandled, just as command resolution
  already does for absent client capabilities. The explicit test and named
  follow-up make that gap visible; half-wiring a destructive rewrite is not the
  remedy.
