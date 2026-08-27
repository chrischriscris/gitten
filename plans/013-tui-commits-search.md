# Plan 013: Add incremental commit search to the terminal client

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. The integrator owns `plans/README.md`; do not edit
> it in this task.
>
> **Drift check (run first)**:
> `git diff --stat 67fee3d..HEAD -- tui/src/main.rs tui/src/commits.rs tui/src/term.rs`
> If an in-scope file changed since this plan was written, compare the
> "Current state" and "Baseline facts" below against the live code before
> proceeding; on a semantic mismatch, treat it as a STOP condition.

## Status

- **Priority**: P1
- **Effort**: M — several coupled input, viewport, rendering, and headless-test
  changes; more than one focused session is plausible
- **Risk**: MED
- **Depends on**: none
- **Category**: direction
- **Planned at**: commit `67fee3d`, 2026-08-27

## Why this matters

The window already filters a loaded commit history incrementally when `/` is
pressed, but the terminal resolves the same configured `commits.search` command
and then reports that it does nothing. That breaks the client contract: the
command name, modes, configuration, and search result are shared data, while the
terminal should own only collecting text and drawing the answer.

This plan makes `/` behave like the window without introducing another search
algorithm or another keymap. It also closes the subtle modal-input hole: a
printable global binding such as shipped `? = help` must become query text while
the prompt owns input, yet configured `[keys.input]` accept/cancel bindings must
still resolve through `core::command`.

## Current state

- `core/src/search.rs:25-72` already owns the entire search algorithm.
  `search::Index::new(&[Commit])` folds each commit's full `sha`, `author`, and
  `subject` once. `search::Index::indices(query)` trims and lowercases the
  needle, matches the three fields separately with substring semantics, and
  returns a new ascending `Vec<usize>` into the original commit slice. Empty or
  whitespace-only input returns every source index. It takes a plain commit
  slice; it does **not** know the shell's `Rc<Data>`, `visible` order table,
  GPUI, or any fold/order structure outside its own private rows.
- `core/src/command.rs:315-529` defines the shipped keymap. `Keymap::builtin`
  binds `/` to `commits.search` in mode `commits` (:458-459), `enter` to
  `input.accept` and `esc` to `input.cancel` in mode `input` (:521-525), and
  `?` to `help` in `global` (:322-324). `Commands::builtin` registers those
  exact three input/search names at :908 and :1083-1084. There is no
  `search.next`, `search.previous`, `commits.next-match`, or equivalent command
  or shipped binding in this file.
- `core/src/command.rs:620-675` exposes `Keymap::resolve_mode_any`, which resolves
  against exactly one mode without falling through to `global`.
  `command::tests::exact_mode_resolution_does_not_turn_text_into_global_commands`
  (:1223-1234) proves the intended input rule: plain `j` is not a command in
  `input`, while `enter` resolves to `input.accept`.
- `shell/src/main.rs:2609-2691` is the window precedent. The exact path is
  `DevShell::begin_search` -> the `input::Input` edited subscription ->
  `DevShell::search_edited` -> `Commits::apply_query`, followed by
  `DevShell::finish_search` on close. Enter keeps the last query, Esc clears it,
  an empty accepted query removes the filter, and reopening pre-fills the
  standing query. `DevShell::run_command` dispatches `commits.search`,
  `input.accept`, and `input.cancel` at `shell/src/main.rs:2976-3004`.
- `shell/src/views/commits.rs:46-71,389-452,500-520,625-665` shows how hits are
  represented: the full commits stay resident, `search::Index` is built once,
  and `visible: Rc<Vec<usize>>` maps filtered viewport rows back into the full
  data. `Commits::apply_query` anchors the cursor by commit SHA across each
  rebuild and `Commits::replace_prepared` reapplies a standing query after a
  refresh. The window does **not** paint query substrings with a special colour;
  membership in `visible` is the hit marker.
- `tui/src/commits.rs:156-252` currently stores only `commits: Vec<Commit>` and
  makes `Viewport` indices address that vector directly. `Commits::selected`,
  `Commits::lines`, `Commits::paint`, and `Commits::row`
  (`tui/src/commits.rs:310-490`) all share that assumption, so changing only
  `current()` would make drawing, copying, mouse selection, and opening a diff
  disagree.
- `tui/src/commits.rs:433-455` already gives the cursor row
  `theme.chrome.selection_bg`, gives a dragged non-cursor row
  `theme.chrome.selected_bg`, and washes the whole row through `Commits::row`.
  Keep those meanings. After filtering, the current visible hit continues to
  use `selection_bg`; do not add a search-only palette token or tint every
  surviving row (every surviving row is already a hit).
- `tui/src/main.rs:248-290` is `Screens::run`, the command-name-to-method edge.
  Its commits arm currently handles list movement only. `App::press`
  (`tui/src/main.rs:494-520`) resolves the full mode stack, then
  `App::dispatch` (`tui/src/main.rs:604-665`) handles client commands before
  delegating. `App::sync_modes` (:389-399) currently pushes the screen mode and
  optional `help`, but has no input state.
- `tui/src/main.rs:737-789` paints title, body, then the single status row.
  `App::run` (:404-465) draws before blocking in `Term::poll`; after a key edits
  state, the loop immediately starts again and draws the next frame. No timer,
  explicit refresh call, or per-frame search belongs in the implementation.
- `tui/src/help.rs:45-124` (`help::paint`) is the nearest modal drawing
  precedent: it draws headlessly over a `Screen`, uses live `Host` registries,
  clips safely, and is guarded from mouse input in `App::mouse`. Search is only
  one line, so use the existing status row rather than adding a floating panel
  or second viewport.
- `tui/src/term.rs:359-410` translates `KeyCode::Char(c)` directly to
  `Code::Char(c)`; `/` therefore already arrives as `Key::char('/')` without a
  terminal-layer special case. `translate_event` (:305-321) currently drops
  `Event::Paste` because the app has no text input. Once search exists, paste
  must become data for the active prompt while remaining unable to type `q` or
  any other command when no prompt is active.
- Baseline verification at plan time:
  `cargo test -p gitten-tui -p gitten-core` passed with 372 core tests and 127
  TUI-library tests; the TUI binary currently has zero tests.

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Focused core search | `cargo test -p gitten-core search::` | the five search tests pass |
| Focused input-mode keymap | `cargo test -p gitten-core exact_mode_resolution` | the exact-mode regression test passes |
| TUI library and binary | `cargo test -p gitten-tui` | all library, binary, and doc tests pass |
| Required headless gate | `cargo test -p gitten-tui -p gitten-core` | all pass; no window opens and no tty is entered |
| Scope check | `git status --short` | only the three in-scope source files are modified (plus this plan if present in the executor worktree) |

Do **not** run `./dev tui`, `cargo run -p gitten-tui`, or any command that enters
raw mode or the alternate screen. The tests and `Screen` frame assertions are
the verification surface for this plan.

## Scope

**In scope** (the only implementation files to modify):

- `tui/src/commits.rs`
- `tui/src/main.rs`
- `tui/src/term.rs`

**Out of scope**:

- `core/src/search.rs` — its input and output already fit this client.
- `core/src/command.rs` — all required names and bindings already ship.
- Every file under `app/` — `Startup` already hands this client the configured
  `Host`; no second parser or TUI-only setting is needed.
- `shell/` and every shipped window binding.
- `core/src/theme.rs` and `app/src/config.rs`; reuse existing chrome tokens.
- Next/previous-match commands or `n`/`N` bindings. The shared feature is a
  filter, and no such window command exists to reuse.
- A general-purpose terminal text editor, command palette, panes, or web input.

## Git workflow

- Branch: `full/tui-search` (assigned by the integrator; the executor works on
  the branch its worktree was created with and never renames it)
- Commit style: sentence-case imperative with a crate prefix (`tui:`, `tui,core:`),
  matching `The search prompt, end to end at the shell` and `Merge search over
  commits into the verb rails`.
- Do not push, open a PR, launch a client, or edit `plans/README.md` unless the
  integrator explicitly asks.

## Baseline facts (with provenance)

| Fact | Provenance | Consequence |
|---|---|---|
| Search indexes full SHA, author, and subject once, then returns ascending source indices from a plain `Vec`-compatible commit slice. | `search::Index::new` and `search::Index::indices`, `core/src/search.rs:31-72` | Construct one `Index` in `Commits::with_glyphs`; never lowercase commits or search during `paint`. |
| Window search is live filtering, not next-match navigation. | `DevShell::begin_search`, `DevShell::search_edited`, and `DevShell::finish_search`, `shell/src/main.rs:2609-2677`; `Commits::apply_query`, `shell/src/views/commits.rs:389-452` | Reuse filter semantics exactly; add no new command name. |
| The reusable names are `commits.search`, `input.accept`, and `input.cancel`. | `Keymap::builtin` and `Commands::builtin`, `core/src/command.rs:458-525,908,1083-1084` | Dispatch these verbatim. `/` is scoped to `commits`; Enter/Esc are scoped to `input`. |
| Shipped `?` is global, while printable text must not fall through from `input`. | `Keymap::builtin`, `core/src/command.rs:322-325`; `Keymap::resolve_mode_any`, :647-675; `exact_mode_resolution_does_not_turn_text_into_global_commands`, :1223-1234 | While the prompt is active, resolve only the `input` mode before treating an unclaimed plain character as text. Do not change `?` for the window or globally. |
| The terminal loop redraws after every handled event and is idle otherwise. | `App::run`, `tui/src/main.rs:404-465` | Apply the query in the key/paste handler; the next loop paints it. No redraw timer or search in `draw`. |
| `/` already arrives as a normal character key. | `translate` in `tui/src/term.rs:359-410` | Add no slash special case to `term.rs`; pin this with a translation test. |
| TUI viewport, mouse range, copy, and painting currently address the source vector directly. | `Commits::current`, `Commits::selected`, `Commits::lines`, `Commits::paint`, and `Commits::row`, `tui/src/commits.rs:249-252,310-490` | Introduce one visible-to-source indirection and route every reader through it in the same change. |
| The existing cursor/hit bar is `chrome.selection_bg`; `chrome.selected_bg` means a mouse drag. | `Commits::paint`, `tui/src/commits.rs:433-455`; palette semantics in `core/src/theme.rs:184-202` | Keep the current visible match on `selection_bg`; do not overload drag selection or create a palette field. |

## Approach

### Step 1: Put the shared index and one visible-order table in `Commits`

In `tui/src/commits.rs`, add `gitten_core::search::Index`, a
`visible: Vec<usize>`, and `query: Option<String>` to `Commits`.
`Commits::with_glyphs` must build the index once from `&commits`, initialize
`visible` to `0..commits.len()`, and keep `commits`/`draws` untouched as the
source-order arrays.

Add methods matching the established window vocabulary:

- `query(&self) -> Option<&str>` for reopening with the standing value.
- `apply_query(&mut self, query: &str)` for the only per-edit rebuild.
- `filter_note(&self) -> Option<String>` (or an allocation-free equivalent used
  only by status drawing) to expose `visible/total` when a filter stands.

`apply_query` must trim empty input to `None`, no-op when the normalized query
is unchanged, anchor the current commit by full SHA, rebuild `visible` from
`Index::indices`, set `Viewport::len` to the visible count, and put the anchor
back where it survives. If it does not survive, clamp to the nearest valid
visible row; if there are no hits, leave `current()` as `None`, top/cursor
internally valid, and painting blank. Clear `sel`, `dragging`, and `grabbed` when
the result set changes so a source-row selection cannot be reinterpreted as a
visible-row selection.

Route every row consumer through `visible`: `current`, `lines`/copy, mouse
selection, `paint`, and `row`/`draws`. The `Viewport`, `sel`, `selected`, and
scrollbar continue to speak visible row numbers; only the final lookup maps to
the source index. Update `len`/`is_empty` deliberately: retain a total accessor
for loaded commits and use visible length for viewport/status behavior, rather
than letting one ambiguous `len()` mean both.

In `Commits::paint`, preserve the existing ink precedence. The current visible
hit uses `theme.chrome.selection_bg`; dragged rows use
`theme.chrome.selected_bg`; ordinary surviving hits use `theme.chrome.bg`.
Filtering is the hit indication, as in the window, so do not compute query
substring ranges in the renderer.

**Verify**: `cargo test -p gitten-tui commits::tests::` -> all existing commit
tests and the new filter tests pass.

### Step 2: Add a status-line search prompt and exact input-mode dispatch

In `tui/src/main.rs`, add one explicit prompt state to `App` (for example,
`search: Option<String>`). Do not put it in `Screens`: collecting terminal text
is client input, while `Commits::apply_query` remains the view operation over
already-loaded data.

Add small `App` methods with single responsibilities:

- `begin_search`: only over `Screens::Commits`; seed the prompt from
  `Commits::query`, clear `pending`, sync modes, and leave the full commit data
  in place.
- `edit_search`: append a plain unmodified `Code::Char`, remove the last Unicode
  scalar on Backspace/Delete, or insert sanitized pasted text; call
  `Commits::apply_query` immediately after each edit.
- `finish_search(accept)`: on accept, close with the live query standing; on
  cancel, call `apply_query("")` before closing. Both clear `pending` and sync
  modes. An accepted empty query therefore removes the filter.

Extend `App::sync_modes` to push `input` above the active screen whenever the
prompt exists. In `App::press`, branch to prompt handling before normal full-
stack resolution:

1. Push the key into the existing `pending` buffer.
2. Resolve the pending chord with `Keymap::resolve_mode_any("input", ...)`, not
   `Keymap::resolve(&self.modes, ...)`, so the shipped global `?`, `q`, `j`, and
   other printable bindings cannot steal query characters.
3. On `Resolve::Run`, clear pending and dispatch the returned configured command
   name (`input.accept`/`input.cancel` by default).
4. On `Resolve::Pending`, keep the buffer and wait for the configured chord.
5. On `Resolve::None`, clear the buffer and edit only when the press is a plain
   character, Backspace, or Delete. Modified/control keys and invalid chord
   continuations do nothing; they must not fall through to global commands.

This preserves `[keys.input]` customization and the chord buffer while making
`?` literal text under the shipped map. A user may deliberately bind `?` in
`[keys.input]`; that inner binding wins, as mode scoping promises. Do not alter
the global help binding, the help painter, or the window.

Handle `commits.search`, `input.accept`, and `input.cancel` in `App::dispatch`.
Ignore mouse gestures while the prompt is active, matching the existing modal
guard for the help overlay in `App::mouse`.

In `App::draw`, let the active prompt own the existing status row: draw `/` and
a one-cell caret in `theme.chrome.accent`, query text in `theme.chrome.fg` on
`theme.chrome.status_bg`, and the live match count/status in dim/faint ink if
room remains. Clip through `Pen`; never allocate or search from the row painter.
When no prompt is active, retain the current status/message/cost behavior.

**Verify**: `cargo test -p gitten-tui --bin gitten-tui` -> the new headless App
input/frame tests pass; no terminal is entered.

### Step 3: Preserve bracketed-paste safety while admitting prompt text

In `tui/src/term.rs`, change `Input` only as much as required to carry
`Paste(String)`. `translate_event(Event::Paste(text))` should return that data;
`App::run` should forward it to `edit_search` only while the prompt is active
and ignore it otherwise. Because `String` is not `Copy`, remove `Copy` from
`Input` without weakening `Key`, `Mouse`, or `MouseKind`.

Extract a small `App` event-routing method (for example, `App::input`) if needed
so `App::run` and binary tests exercise the same `Input::Key`/`Input::Paste`
decision without either test calling `Term::poll` or `Term::enter`.

Sanitize a paste as one status-line query: normalize line breaks/tabs to spaces
and discard other control characters before one call to `apply_query`. Do not
feed pasted characters through `App::press` or `Keymap`; pasted `q`, `?`, and
`enter` text must never execute commands. Update the module comments that
currently say the app has no text input.

Add a translation test proving `/` remains `Key::char('/')`, replace
`a_paste_is_not_an_input` with a test that paste is one `Input::Paste`, and add
an App-level test proving pasted `q?` edits an open query but does nothing when
the prompt is closed.

**Verify**: `cargo test -p gitten-tui term::tests::` and
`cargo test -p gitten-tui --bin gitten-tui` -> all pass.

### Step 4: Run the complete headless gate and inspect scope

Run the required joint test command, then inspect the worktree. Do not launch
the client for manual verification; the new binary tests and `Screen` assertions
must cover the interaction and frame.

**Verify**: `cargo test -p gitten-tui -p gitten-core` -> exit 0, including the
new tests; `git status --short` -> no implementation file outside the three-file
scope is modified.

## Changes by layer (core / app / tui)

### Core

- **No files touched.** Reuse `search::Index::new`, `search::Index::indices`,
  `Keymap::resolve_mode_any`, `Modes`, `Resolve`, and the existing registered
  command names from `core/src/search.rs` and `core/src/command.rs`.
- Add no dependency to `core`, no UI state, no new command, and no next-match
  binding.

### App

- **No files touched.** `gitten_app::Started.host` already supplies the same
  configured `Host` and keymap used by the window. The TUI must read that host;
  it must not parse `gitten.toml` or create a parallel setting.

### TUI

- `tui/src/commits.rs`: build and retain the shared search index; add the
  visible-to-source order table, standing query, SHA anchoring, filtered status,
  and indirection across cursor, copy, mouse, scrollbar, and row painting.
- `tui/src/main.rs`: own the one-line prompt, exact `input`-mode resolution,
  accept/cancel lifecycle, status-line rendering, paste routing, and headless
  end-to-end tests around `App::press`/`App::draw`.
- `tui/src/term.rs`: surface bracketed paste as inert text data and pin `/`
  translation; keep all crossterm imports confined here.

## Test list

Name tests exactly or comparably; each must state its fixture in the test body.

- `commits::tests::a_query_filters_all_three_fields_in_source_order` — fixture:
  a small `parse_log` history with distinct SHA, author, and subject matches;
  assert `current`, painted rows, visible count, and order use the indices from
  `core::search`.
- `commits::tests::filtering_anchors_the_cursor_by_sha_and_a_miss_clamps` —
  fixture: 30 alternating `engine`/`compiler` commits like the window search
  tests; assert a surviving SHA stays current, a removed anchor clamps, and zero
  hits produce `None` without a panic.
- `commits::tests::clearing_a_query_restores_every_row_and_copy_uses_visible_rows`
  — fixture: the same alternating history; assert empty/whitespace restores the
  full list and selection/copy never leaks filtered-out source rows.
- `commits::tests::a_filtered_cursor_keeps_the_existing_selection_bar` — fixture:
  `LOG` in `tui/src/commits.rs`; paint to `Screen`, assert the current hit's
  first and last cells use `host.theme.chrome.selection_bg`, ordinary hits use
  `chrome.bg`, and no `chrome.selected_bg` appears without a mouse drag.
- `main::tests::slash_types_live_on_the_status_line_and_enter_keeps_the_filter`
  — fixture: an `App` built from public `gitten_app::Started` fields and 30
  alternating commits; press `/`, type `engine`, call `draw`, assert the bottom
  `Screen::row_text` contains `/engine`, only 15 commits remain, then Enter
  closes the prompt without restoring rows.
- `main::tests::escape_cancels_and_a_second_slash_prefills_the_standing_query` —
  same fixture; assert cancel restores 30, accept/reopen starts from the accepted
  query, and accepted empty input removes it.
- `main::tests::question_mark_is_text_in_input_mode_and_help_outside_it` — same
  fixture; assert `?` toggles `App::help` before search, but while search is open
  it appends to the query and leaves help closed. This is the collision
  regression test.
- `main::tests::configured_input_bindings_and_chords_own_the_pending_buffer` —
  fixture: mutate the test `Host` keymap exactly as `[keys.input]` would (unbind
  shipped Enter, bind another key and one two-key chord); assert only the new
  binding accepts/cancels, `Resolve::Pending` leaves the prompt unchanged, and
  the buffer clears on finish.
- `main::tests::pasted_commands_are_query_text_only_while_input_is_open` —
  fixture: the same `App`; assert `Input::Paste("q?")` changes the query without
  quitting/help, and the same paste with no prompt changes no app state.
- `term::tests::slash_arrives_as_cores_plain_character_key` — fixture:
  `KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE)`; assert
  `Key::char('/')`.
- `term::tests::a_paste_is_one_text_input_and_never_a_key_sequence` — fixture:
  `Event::Paste("q?\nengine")`; assert one `Input::Paste` carrying the original
  string.

Done criteria:

- [ ] `cargo test -p gitten-tui -p gitten-core` exits 0.
- [ ] `/` resolves through the live `Host` keymap to `commits.search`; no
  keypress match hardcodes slash behavior.
- [ ] Enter/Esc (or their configured replacements) resolve through exact
  `input` mode to the existing names, and shipped `?` is query text while the
  prompt is active.
- [ ] Every filtered viewport/cursor/mouse/copy/paint lookup passes through one
  visible-to-source table backed by `search::Index::indices`.
- [ ] Search work runs on edits only; `Commits::paint` and `App::draw` perform no
  lowercasing or index rebuild.
- [ ] Accept keeps, cancel clears, empty accept removes, second `/` pre-fills,
  and zero results are headlessly tested.
- [ ] No window opens and no test calls `Term::enter` or grabs a tty.
- [ ] No implementation files outside `tui/src/main.rs`, `tui/src/commits.rs`,
  and `tui/src/term.rs` are modified; `plans/README.md` remains untouched.

## Stop conditions

Stop and report back instead of hacking around any of these:

- `search::Index::indices` no longer returns ascending indices into the same
  commit slice passed to `Index::new`, or it starts requiring shell/GPUI order
  structures. That invalidates the central reuse assumption.
- The live registry no longer contains all three names `commits.search`,
  `input.accept`, and `input.cancel`, or `/`, Enter, and Esc are no longer scoped
  to `commits`/`input` as documented. Do not add replacement names locally.
- Correct input isolation cannot be expressed with the existing public
  `Keymap::resolve_mode_any` without changing `core`. Report the exact missing
  operation and proposed core API; do not bypass the configured keymap.
- A complete visible-to-source conversion appears to require changing
  `Viewport`, `core::search`, or any shell file. This likely means a direct-index
  reader was missed or the code drifted; enumerate the callers before expanding
  scope.
- Preserving pasted-text safety would require feeding paste through the keymap,
  synthesizing key events, or disabling bracketed paste. None is acceptable.
- A headless App fixture cannot be constructed from public data without I/O or
  entering a terminal. Report which field blocks it; do not weaken production
  visibility or launch the client for the test.
- The required test command fails twice after one reasonable correction, any
  test tries to enter raw mode, or an implementation file outside scope must be
  touched.

## Risks

- **Index-space drift (highest)**: the TUI currently assumes viewport rows are
  source rows everywhere. Missing one lookup can open, copy, select, or paint a
  different commit than the cursor names. Review every use of `view.cursor()`,
  `view.row_at`, `selected()`, and direct `commits[index]`/`draws[index]` access.
- **Modal fallthrough**: using `Keymap::resolve(&modes, ...)` while typing would
  make `?` open help, `q` quit, and `j` move the list. Exact input-mode
  resolution is load-bearing; the collision test must fail if global fallback
  returns.
- **Chord/text ambiguity**: a configured input chord may reserve a printable
  prefix. Honor `Resolve::Pending`; after an invalid continuation, clear the
  buffer rather than replaying keys as text, which could execute or duplicate
  input. Document this small, intentional trade-off in code.
- **Paste regression**: changing `Input` from `Copy` and surfacing
  `Event::Paste` broadens the event enum. Exhaustive matches must keep paste
  inert outside prompts and must never translate its contents into commands.
- **Selection semantics under filtering**: a drag range is in visible order.
  Clear it when the query changes and map each row at copy time; slicing the
  source vector by visible endpoints includes hidden commits.
- **Visual overstatement**: tinting every surviving row as a match would turn a
  filtered screen into a wall of selection colour and collide with mouse
  selection. Reuse `chrome.selection_bg` only for the cursor/current hit and
  retain `chrome.selected_bg` exclusively for drag selection.
- **Long query clipping**: the status prompt is one row and `Pen` clips safely.
  Keep editing the full string even when its left edge is no longer visible;
  horizontal prompt scrolling is a follow-up only if real use proves it needed.
