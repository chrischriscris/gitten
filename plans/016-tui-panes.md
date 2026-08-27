# Plan 016: Give the terminal a persistent pane focus ring

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving on. If a
> STOP condition occurs, stop and report; do not preserve the screen stack by
> hiding it behind the pane registry, and do not invent terminal-only command
> names. Do not update `plans/README.md`: the integrator owns the index.
>
> **Drift check (run first)**:
> `git diff --stat 15bff4a..HEAD -- shell/src/panes.rs shell/src/main.rs shell/src/chrome.rs core/src/command.rs core/src/view.rs tui/src/main.rs tui/src/screen.rs tui/src/diff.rs tui/src/commits.rs tui/src/split.rs tui/src/markdown.rs tui/src/term.rs`
> If any listed path changed, compare every cited function and command below
> with live code before editing. A mismatch in the pane command names/defaults,
> `App`'s search/job ownership, `Diff::reflow`'s width contract, mouse gesture
> routing, or `Screen::span` clipping is a STOP condition.

## Status

- **Priority**: P1
- **Effort**: L (multi-day: this replaces the terminal's navigation model,
  introduces cached two-region geometry and focus routing, changes both
  viewport painters, and must re-prove all three pass-3 features headlessly)
- **Risk**: HIGH
- **Depends on**: plans 013, 014, and 015 are already integrated at this
  baseline; preserve them rather than reimplementing them
- **Category**: terminal feature / architecture foundation
- **Planned at**: commit `15bff4a`, 2026-08-27
- **Confidence**: HIGH on command reuse, ownership, and wide/narrow behavior;
  MED on the 96-column breakpoint until fixed-cell frame tests confirm the
  chosen floors with the largest commit gutter

## Why this matters

The desktop is already lazygit-shaped: a left stack of named list panes remains
visible beside a persistent main diff, focus decides whose commands are live,
and `commits.open-diff` moves focus instead of replacing the list. The terminal
still has `Vec<Screens>` navigation: Enter synchronously acquires a diff and
pushes it over the commits, while Esc pops it. `docs/terminal.md` names panes as
the remaining structural work and points directly at the unused
`Screen::span` primitive.

This pass makes the terminal use the same stable pane names and command names as
the window while keeping terminal geometry where it belongs: in `tui`. It ships
only the two views the terminal actually has—`commits` and `diff`—but introduces
a registry and placement vocabulary into which later status, files, branches,
stashes, or compiled-in extension panes can register without changing key
dispatch or replacing a two-field layout. Search, hunk writes/refresh, Markdown
reflow, selection, and OSC 52 copying are release gates, not follow-up cleanup.

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Required headless gate | `cargo test -p gitten-tui -p gitten-core` | all existing and new tests pass; no raw mode, alternate screen, tty, or window |
| Format | `cargo fmt --check` | exit 0 |
| Lint | `cargo clippy -p gitten-tui -p gitten-core --all-targets -- -D warnings` | no warnings |
| Broader regression | `cargo test -p gitten-app` | pass-3 acquisition/job/config tests still pass |
| Full repository gate | `./check.sh` | exit 0; no client is launched |
| Scope | `git status --short` | only files listed under Changes by layer are modified |

Do not run `./dev tui`, `./dev desktop`, or any command that enters a tty or
opens a window. Exercise frames with in-memory `Screen` values. `./dev dump`
would be non-interactive, but is unnecessary for this pass's acceptance gate.

## Scope

In scope:

- A terminal-owned, generic registry of stable pane names, sidebar/main
  placement, focus, traversal order, and cached geometry.
- A persistent `commits` pane and persistent `diff` main pane on commit-list
  launches; a diff-only launch still has one full-width diff pane.
- Wide side-by-side composition and a deterministic one-pane narrow fallback.
- Focus dispatch through the existing `pane.*` and `*.focus` command names.
- Per-pane resize, reflow, viewport, painting, hit testing, scrollbar handling,
  mouse capture, selection, status, refresh, and config reload.
- Removing `App::stack` and its push/pop behavior after equivalent pane-backed
  state is in place.
- Preserving Ctrl-J as Ctrl-J in the terminal translator so core's existing
  `pane.next` default is reachable when more than one sidebar list exists.

Explicitly out of scope:

- Building terminal status, files, branches, or stashes views. Their stable
  slots and focus commands are reserved; absent panes answer with `no <name>
  pane`, as the window does.
- Automatic diff preview on every commit cursor move. This pass retains the
  terminal's existing explicit Enter/double-click acquisition and makes the
  resulting diff persistent. A debounced background preview like
  `DevShell::schedule_main_diff` is separate work; synchronously acquiring on
  every `j`/`k` would be a regression.
- Vertical stacking of placeholder panes, resizable dividers, persisted pane
  proportions, or a new `gitten.toml` table. The same existing file continues
  to supply keys, view settings, layout, wrap, theme, font, and mouse behavior.
- A new core layout/focus type. Pane rectangles and terminal hit testing are
  client drawing/input; `core::view::Viewport` already supplies the shared
  per-list scrolling seam.
- Any shell, app, git-backend, web, Markdown-model, diff-pipeline, job-runner,
  clipboard-protocol, or dependency change.
- A new command name or a terminal-local key table.

## Baseline facts (provenance)

### The desktop reference is a registry plus one main region

- `shell/src/panes.rs` defines generic `Panes<T>` entries by stable string name
  and registration order. `register` replaces a resident in place and focuses
  it; `position`, `focused_name`, `focus`, `cycle`, and `names` are the
  non-GPUI focus seam. The module comment explicitly says a files or branches
  tenant must not add a layout/dispatch branch.
- `shell/src/main.rs` `DevShell` holds `panes: Panes<Screen>` for the left lists,
  a separate persistent `main: Screen` diff, and `spot: Spot` for which region
  owns the keyboard. `active`, `list_order`, `set_spot`, `sync_focus`, and
  `sync_modes` derive dispatch and modes from that focus rather than from what
  was opened most recently.
- `shell/src/main.rs` `DevShell::render` lays the named sidebar residents out
  beside `main_region`; the sidebar uses `shell/src/chrome.rs::SIDEBAR_SHARE`,
  currently `0.32`, while the main diff flexes into the remainder. Each region
  clips its children. `chrome::pane_header`/`pane_header_with` name and mark the
  focused pane, and `chrome::status_bar` reports the active mode and registry-
  derived hints.
- `shell/src/main.rs` `DevShell::cycle_pane` cycles the left-list order only;
  `DevShell::pane_walk` walks that reading order and then the main diff without
  wrapping at the ends. `focus_named` reports an absent registered name instead
  of silently inventing a pane. `focus_main` is the implementation of
  `commits.open-diff`.
- `shell/src/main.rs` `DevShell::back` first dismisses overlays/input. From
  `Spot::Main` it returns to `Spot::List`; it does not destroy the diff. List
  selection is cleared only when already in the list region. This is the
  behavior the terminal must converge on.
- The desktop's header currently passes the literal `"6"` to
  `chrome::pane_header_with` for the main diff, while the shared keymap binds
  `0` to `diff.focus`. The terminal must derive displayed focus keys from
  `Host::keys`, not copy either literal. This plan does not repair desktop
  chrome.

### Shared command names and defaults already cover panes

- `core/src/command.rs` `Commands::builtin` registers exactly
  `pane.next`, `pane.prev`, `pane.left`, `pane.right`, `status.focus`,
  `files.focus`, `branches.focus`, `commits.focus`, `stashes.focus`, and
  `diff.focus`. It also registers the already-used `commits.open-diff`,
  `commits.search`, `input.accept`, `input.cancel`, `back`, the `view.*`
  commands, and the diff hunk/layout/wrap commands.
- `core/src/command.rs` `Keymap::builtin` binds global `h`/Left to
  `pane.left`, global `l`/Right to `pane.right`, `1` to `status.focus`, `2` to
  `files.focus`, `3` to `branches.focus`, `4` to `commits.focus`, `5` to
  `stashes.focus`, and `0` to `diff.focus`. In mode `panes`, Ctrl-J and Ctrl-K
  bind to `pane.next` and `pane.prev`. Enter in mode `commits` remains
  `commits.open-diff`; Esc globally remains `back`.
- Therefore this pass adds **no command name and no default binding**. The TUI
  must answer the names above in `App::dispatch`, route `view.*` to the focused
  pane, and let `Host::keys` plus the existing `[keys]`/`[keys.<mode>]` config
  decide which physical keys invoke them.
- `tui/src/term.rs` `translate` currently special-cases Ctrl-J as `Code::Enter`
  to tolerate an LF-shaped Return. That predates the `panes` default and makes
  core's Ctrl-J binding unreachable in this client. Preserve Ctrl-J as
  `Key::new(Code::Char('j'), true, false, false)`; ordinary crossterm
  `KeyCode::Enter` remains Enter. Do not choose a replacement terminal binding.

### The terminal currently has stack ownership everywhere

- `tui/src/main.rs` `Screens` is an enum whose variants each own a view,
  `Source`, label, and `Generation`. Its `resize`, `paint`, `press`, `drag`,
  `release`, copy/select methods, `filter_note`, and `run` adapt common app
  operations to `Commits` or `Diff`.
- `tui/src/main.rs` `App` holds `stack: Vec<Screens>`. `sync_modes`, config
  `reload`, `dispatch`, `mouse`, `draw`, `hunk_verb`, `refresh_stale`, and copy
  routing all read `stack.last()` or iterate the stack. `open_diff` acquires
  synchronously, resizes a new `Diff` to the whole body, and pushes it.
  `back` clears selection and then pops when `stack.len() > 1`.
- `tui/src/main.rs` `draw` owns exactly row 0 (title), row `h - 1` (status or
  search prompt), and hands all `h - 2` body rows plus the full terminal width
  to the top screen. `title` already accepts a mode and label; the status row
  uses the active screen's status when no message is standing.
- `Screens::run` currently swallows `pane.left` and `pane.right` as no-ops on a
  commit list because only one screen is visible. That compatibility arm must
  be deleted: pane commands become app/registry commands and an unhandled pane
  name must never be mistaken for a view action.
- The existing job invariant is stronger than “refresh what is visible.”
  `App::drain_jobs` advances `generation` on every finished job and
  `App::refresh_stale` refreshes every repository-backed stack entry. The pane
  conversion must iterate every registered repository pane, including an
  unfocused diff or commits list.

### Each view already owns an independent shared Viewport

- `core/src/view.rs` `Viewport` stores `len`, `height`, `top`, `cursor`, and
  `scrolloff`; `set_height`, movement, scrolling, page, `row_at`, scrollbar
  `thumb`, and clamping maintain their invariants. `Commits` and `Diff` already
  contain separate `Viewport` values, so per-pane scrolling requires no core
  change and no duplicated terminal arithmetic.
- `tui/src/commits.rs` `Commits::resize` assigns its own column count and
  viewport height. `paint` currently draws every visible row through
  `Screen::row` and paints its scrollbar at `self.cols - 1`; `press`/`drag` use
  coordinates local to that view.
- `tui/src/diff.rs` `Diff::resize(cols, height, host)` assigns `self.cols`,
  sets its own viewport height, and calls `Diff::reflow`. `Diff::reflow` passes
  **that exact `self.cols`** to every `Rows::reflow`, then expands the shared
  order table. This is the load-bearing Markdown fact: supplying the diff pane
  width to `resize`, rather than the terminal width, makes
  `MarkdownRows::reflow` budget against the narrower pane.
- `tui/src/markdown.rs` `MarkdownRows::reflow` subtracts its dynamic diff chrome
  and block furniture from the `cols` supplied by `Diff`. `tui/src/split.rs`
  `SplitRows::reflow` similarly derives two half-width budgets. Neither needs a
  pane branch; both must receive the right width through the existing seam.

### Screen clipping and mouse events already expose the needed primitives

- `tui/src/screen.rs` `Screen::span(y, x, cols)` returns a `Pen` over only that
  row slice, clamps its bounds to the screen, and is covered by
  `a_span_pen_cannot_reach_outside_its_columns`. `Pen::take` provides the same
  clipping within a presentation. Pane painters must use `span` for every row;
  painting with `Screen::row` after computing pane geometry would still let a
  long commit or diff overwrite its neighbor.
- `tui/src/diff.rs` `Diff::paint` and `tui/src/commits.rs` `Commits::paint`
  currently call `Screen::row`, then paint the scrollbar with an absolute x.
  They are the two painters that need an x origin plus local width. Rows,
  Markdown, and split already draw only through the `Pen` they receive.
- `tui/src/term.rs` `Mouse` carries terminal-cell `col` and `row` for left-button
  down/drag/up. Wheel events deliberately become coordinate-free
  `Code::WheelUp`/`WheelDown` keys. With panes, that remains correct because the
  keymap resolves the wheel to `view.scroll-*` and app dispatch sends it to the
  **focused** pane.
- `tui/src/main.rs` `App::mouse` owns click counting, body-row subtraction,
  double-click-to-open, release-time `copy_on_select`, and the “input/help is
  inert to mouse” guard. The pane hit test belongs there. A drag/up must remain
  captured by the pane where Down began even after the pointer crosses the
  divider; otherwise one gesture would splice two panes' selection state.
- `Term::copy` remains OSC 52. `App::copy` is deferred until the loop owns the
  terminal, and `copied` formats its feedback. Pane routing changes which view
  supplies text, not how it is copied.

## Approach

1. **Replace stack identity with stable pane identity.** Add a generic terminal
   `Panes<T>` registry modeled on `shell::panes::Panes`, but with terminal-only
   placement metadata. Register only `commits` and `diff` in this pass. Reserve
   canonical sidebar ranks for `status`, `files`, `branches`, `commits`, and
   `stashes`; reserve `diff` as the main slot. Do not create empty fake views for
   the four unbuilt lists.
2. **Cache geometry at resize/registry change.** A built-in lazygit layout policy
   computes pane rectangles from the body `Rect`; store the result until size or
   registrations change so rendering allocates nothing per frame. At 96 columns
   and wider, allocate the sidebar `max(floor(32%), 40)` columns, one divider
   column, and at least 55 columns to the diff. At 95 columns and below, draw
   only the focused pane at full body width. Do not stack the two vertically:
   terminal height is the scarcer axis, and two headers plus two short
   viewports make both lists less useful than one honest viewport.
3. **Make both panes persistent.** On a commits launch, register the loaded
   commits pane and an empty diff main pane, focus commits, and show the empty
   diff's header/empty body in wide mode. `commits.open-diff` performs the same
   explicit acquisition as today, replaces the `diff` tenant in place, and
   focuses it. On a direct diff/fixture/patch launch, register only the loaded
   diff and let it fill the body. Do not reload the diff merely to move focus.
4. **Match the window's focus semantics.** `pane.left`/`pane.right` walk the
   canonical registered sidebar order followed by the main diff and stop at
   edges. `pane.next`/`pane.prev` cycle registered sidebar lists only and are
   useful once a later pass registers a second list. Direct `*.focus` commands
   use stable names; missing panes report `no <name> pane`. Enter and a commits
   double click replace/focus diff. `back` dismisses help first; from diff it
   focuses commits without clearing/destroying diff; in commits it clears that
   pane's selection and otherwise stays. A diff-only launch has nowhere to go
   back to.
5. **Route modes and commands from focus.** Build `Modes` as `panes` when at
   least two sidebar lists are registered, then the focused pane mode, then
   help/input as today. Search input still resolves against exactly `input`.
   Route every `view.*`, copy/select, diff-specific command, hunk verb, status,
   and label through the focused tenant. Add no key match; preserve Ctrl-J in
   `term::translate` so core's existing mode binding can reach dispatch.
6. **Draw through clipped pane-local coordinates.** Give `Commits::paint` and
   `Diff::paint` an x origin and focused flag. Replace every whole-row pen with
   `Screen::span(row, x, self.cols)`, and place the scrollbar at
   `x + self.cols - 1`. Resize each tenant to its content rectangle (pane width,
   pane height minus its one-row header). The focused pane alone draws its
   cursor bar; unfocused panes retain Viewport/selection state but are inert.
7. **Make mouse Down choose and capture a pane.** Hit-test the cached rectangles;
   a Down in a pane focuses it, translates to pane-local col/row, and calls that
   view's `press`. Store that stable pane name for Drag and Up; clamp/overshoot
   vertically relative to the captured pane and clamp x into its local width so
   selection cannot cross the divider. Release and optional copy-on-select read
   the captured pane. Wheel remains a key and scrolls the focused pane.
8. **Re-prove pass 3 through the pane path.** Search targets the named commits
   tenant while its prompt owns input; staging reads the focused diff and every
   completed job refreshes all registered repository panes; Markdown reflows at
   diff-rectangle width; copy/select still produces the same text and hands it
   to the unchanged OSC 52 queue.

## Changes by layer

### Core

No production or test change expected.

- Keep `core/src/command.rs` as the sole command/default registry. Do not add a
  `tui.*` alias, a second digit binding, or a new mode. Tests in TUI should
  assert against `Keymap::builtin` and `Commands::builtin` directly.
- Keep `core/src/view.rs` unchanged. Each terminal tenant already owns the
  `Viewport` it needs; pane rectangles only provide different heights.
- Keep `core/Cargo.toml` unchanged with its empty `[dependencies]`.

If correct focus or geometry requires a core edit, STOP. The only possible
shared facts in this pass—command names/defaults and list viewport arithmetic—
already exist there.

### App

No change.

- `gitten_app::Started`, `acquire`/`reacquire`, `Runner`, `Submitter`,
  `Generation`, `Write`, and config-loaded `Host` already expose everything the
  pane container needs.
- The same `gitten.toml` continues to drive both clients. Do not add a pane
  config table in this pass and do not move geometry into `app`.

### TUI

**`tui/src/panes.rs` (new)**

- Add a generic `Panes<T>` registry whose entries contain a stable name,
  `Placement` (`Sidebar { rank }` or `Main`), and value. Registration replaces
  in place without duplicating a name. Expose checked `get`/`get_mut`,
  `position`, `names`, `focused_name`, `focused`/`focused_mut`, `focus_named`,
  registered sidebar order, full reading order, and sidebar cycling.
- Use canonical rank constants/table for `status`, `files`, `branches`,
  `commits`, and `stashes`, matching `DevShell::list_order`; unknown compiled-in
  sidebar tenants follow those in registration order. `diff` occupies Main.
  The registry is generic and contains no `Screens`, git, crossterm, or drawing.
- Add `Rect { x, y, width, height }`, pane-header/content subdivision, and a
  cached `Geometry`/layout result. Wide layout starts at `WIDE_AT = 96`, uses a
  one-cell divider, a 32% sidebar clamped to `SIDEBAR_MIN = 40`, and guarantees
  `DIFF_MIN = 55`. Narrow layout gives the full body to the focused pane and no
  rectangle to the others. Use checked/saturating arithmetic for zero width and
  terminals shorter than header+one content row.
- Keep layout replaceable at construction (a small `Layout` trait or injected
  function is acceptable) so a compiled-in client extension can replace the
  built-in geometry without changing registry/focus dispatch. Recompute/cache
  only on screen-size, focus (narrow mode), or registration changes; no Vec or
  String allocation belongs in `App::draw`.
- Unit-test registration/replacement, canonical and extension order, focus by
  name, non-wrapping left/right walk, wrapping sidebar next/prev, wide geometry,
  narrow visibility, exact divider ownership, and degenerate dimensions.

**`tui/src/main.rs`**

- Declare the new local `panes` module and replace `App::stack` with
  `Panes<Screens>` plus cached geometry and `gesture: Option<stable pane name>`.
  Keep `Screens` as the per-view adapter, but permit the initially empty diff to
  carry no acquired `Source`/generation until Enter replaces it. Repository-
  backed residents still retain their source, label, and generation.
- In `App::new`, a commits launch registers `commits` in the sidebar and an
  empty `Diff::new(Vec::new(), &host)` as `diff` Main, then restores focus to
  commits after registration. A diff launch registers only the loaded `diff`.
  Apply the selected scrollbar glyphs to both.
- Rewrite `sync_modes`, `reload`, `draw`, `dispatch`, `back`, `open_diff`,
  `hunk_verb`, `refresh_stale`, search helpers, and copy/select helpers to use
  stable panes rather than `last()`/stack iteration. Delete stack push/pop and
  all comments/tests that describe one-screen-at-a-time behavior.
- Add app-level `focus_named`, `cycle_pane`, and `pane_walk` adapters matching
  the desktop functions and exact missing-pane notice. Dispatch all ten
  existing pane/focus names before `Screens::run`. Delete the old
  `Screens::run` pane no-op arms.
- `open_diff` must read `Commits::current` through the named commits tenant,
  acquire exactly once through the retained `Handle`, replace the named diff
  tenant on success, resize it to current diff content geometry, and focus it.
  A failure preserves the old diff and current focus and reports the error.
- `back` order is: close help; if focused diff and commits exists, focus commits;
  otherwise clear the focused pane's selection; otherwise no-op. Search Esc
  remains `input.cancel` through `press_input`, so it restores the unfiltered
  commits and never reaches `back`.
- Draw global title row 0, pane headers/body in rows `1..h-1`, and the status or
  search row at `h-1`. Each pane header shows its first currently configured key
  for `<name>.focus` (or blank when unbound), stable pane name, and view label;
  focus uses the theme accent. The global title and normal status prefix both
  name the focused pane; messages remain louder and the search prompt keeps its
  existing `/query█ · hits/total` shape. Help still overlays the body and uses
  the focused mode stack.
- Resize each visible or registered pane from cached content geometry, not the
  full screen. Hidden narrow-mode panes retain their last Viewport and receive
  the new width/height when focused before painting. Config reload must
  re-apply geometry to all tenants and therefore reflow a hidden diff before it
  is next shown.
- Replace `App::mouse`'s whole-body routing with pane-rectangle hit testing.
  Down focuses then presses the hit tenant; Drag/Up route to the captured name.
  Include pane name in `clicked` identity so the same global cell after a
  narrow-mode focus switch cannot become another pane's second click. Clear
  gesture capture after Up, resize, help/input opening, or registry replacement.
  Copy on Up reads only the captured pane's finished selection.
- Iterate all registry entries in `refresh_stale`; an empty placeholder diff is
  skipped because it has no source. Keep first-error precedence and attempt all
  other panes. The old “both stacked screens” test becomes a visible/unfocused
  pane refresh test rather than losing its assertion.

**`tui/src/commits.rs`**

- Change `Commits::paint` to accept pane x and `focused`. For every visible or
  blank row, obtain the pen with `Screen::span(row, x, self.cols)`. Paint the
  scrollbar at `x + self.cols.saturating_sub(1)` only when `self.cols > 0`.
- Draw the cursor background only when focused; keep dragged selection ink and
  the stored `Viewport` unchanged while unfocused. Mouse methods remain local-
  coordinate APIs.
- Convert existing painter tests to an origin-aware helper and add a sentinel
  test proving a long subject cannot overwrite the divider or diff span.

**`tui/src/diff.rs`**

- Change `Diff::paint` to accept pane x and `focused`. Use
  `Screen::span(row, x, self.cols)` for rendered and blank rows, and offset the
  scrollbar by x. Set `Frame::current` only when this diff is focused; do not
  alter text selection, row ownership, hunk lookup, or `Pen::scroll`.
- Keep `Diff::resize` and `Diff::reflow` signatures. The caller now supplies the
  diff content rectangle's width and height, which automatically reflows
  `TextRows`, `MarkdownRows`, and `SplitRows` at pane width.
- Add a nonzero-origin clipping test and a Markdown-in-pane reflow integration
  test. Existing layout, wrapping, selection, hunk, replacement, and scrollbar
  tests remain passing.

**`tui/src/term.rs`**

- Remove the Ctrl-J-to-Enter `feed` fold from `translate`. Preserve
  `KeyCode::Char('j')` plus Control as Ctrl-J; keep `KeyCode::Enter` as Enter and
  keep release filtering unchanged.
- Replace `a_line_feed_is_the_return_key` with assertions that normal Enter
  resolves `commits.open-diff`, Ctrl-J resolves `pane.next` in `panes` mode, and
  Ctrl-K resolves `pane.prev`. This is a deliberate compatibility trade: a pty
  layer that reports Return only as LF loses the old fallback because the shared
  keymap now gives Ctrl-J a real meaning. Do not hide that conflict by inspecting
  active modes in the platform translator.

No changes are expected in `tui/src/screen.rs`, `tui/src/rows.rs`,
`tui/src/markdown.rs`, `tui/src/split.rs`, `tui/src/scrollbar.rs`,
`tui/src/help.rs`, `tui/src/lib.rs`, or `tui/Cargo.toml`. They are cited because
their existing generic `Pen`, reflow, hit, scrollbar, and help seams must be
used unchanged. If production changes there appear necessary, reconcile the
reason against the STOP conditions before broadening scope.

## Test list

All tests are headless and use fixed in-memory `Screen` dimensions. Prefer
named cell/text/ink assertions over opaque snapshots.

1. **`registration_replaces_by_name_and_preserves_canonical_order`**
   (`tui/src/panes.rs`) — register commits, diff, two fake extension sidebars,
   then replace one. Assert no duplicate, stable focus, canonical built-ins
   before extensions, and Main last.
2. **`pane_walk_stops_and_sidebar_cycle_wraps`** (`tui/src/panes.rs`) — with
   fake status/files/commits/stashes/diff tenants, assert left/right use full
   reading order and stop at edges while next/prev wrap only through sidebar
   tenants. Repeat with only commits+diff: left/right move; sidebar cycle says
   there is no second list.
3. **`wide_geometry_has_one_owned_divider_and_no_overlap`**
   (`tui/src/panes.rs`) — at 96, 120, and 160 columns assert sidebar is at least
   40, diff at least 55, divider exactly one cell, rectangles are disjoint and
   cover the body, and headers leave nonnegative content rectangles.
4. **`narrow_geometry_shows_only_the_focused_pane`**
   (`tui/src/panes.rs`) — at 95, 80, and zero columns assert only focus has a
   full-body rectangle, focus switching swaps visibility without changing
   tenant Viewport state, and diff-only launch remains full width.
5. **`each_pane_clips_to_its_span_and_owns_its_scrollbar`**
   (`tui/src/commits.rs` / `tui/src/diff.rs`) — paint long rows at nonzero x
   with sentinel divider/neighbor cells; assert neither painter crosses its
   width and each scrollbar is on its own last column with underlying row
   background preserved.
6. **`shared_defaults_focus_the_registered_terminal_panes`**
   (`tui/src/main.rs` / `tui/src/term.rs`) — assert builtin h/l and arrows run
   `pane.left/right`, 4 runs `commits.focus`, 0 runs `diff.focus`, Ctrl-J/K in
   `panes` mode run next/prev, and digits for absent status/files/branches/
   stashes produce exact `no <name> pane` notices. Assert no terminal command
   or key table was introduced.
7. **`enter_replaces_and_focuses_a_persistent_diff_and_back_returns`**
   (`tui/src/main.rs`) — fake three-commit repository; Enter acquires selected
   SHA once, replaces—not appends—the diff tenant, focuses it, and leaves commits
   state resident. Esc focuses commits and preserves diff cursor/layout/wrap;
   a second Enter replaces the same tenant. Direct diff launch has one tenant
   and Esc is a no-op.
8. **`view_commands_and_wheel_reach_only_the_focused_viewport`**
   (`tui/src/main.rs`) — give both tenants long lists, dispatch `view.down` and
   translated wheel keys under each focus, and assert only that tenant's
   cursor/top changes. Unfocused cursor bars are absent from the frame.
9. **`mouse_down_focuses_the_hit_pane_and_drag_stays_captured`**
   (`tui/src/main.rs`) — Down in each rectangle focuses it and translates local
   coordinates; drag across divider/up in the other rectangle modifies/releases
   only the origin tenant. Assert scrollbar clicks use local last column and the
   double/triple clock includes pane identity.
10. **`copy_on_select_finishes_once_in_the_captured_pane`**
    (`tui/src/main.rs`) — drag in commits and in diff with
    `copy_on_select = true`; each Up queues exactly that pane's selection once,
    `copied` still reports line count, `copy.selection` uses focused fallback,
    and no `Term`/OSC bytes are emitted by the headless test. Existing
    `Term::copy` OSC 52 tests remain unchanged.
11. **`search_prompt_isolated_over_the_commits_pane`** — focus commits, open
    `/`, type and paste command-looking characters, and assert live
    `Commits::apply_query`, hit count, input-only modes, Enter accept, and Esc
    restore all match the existing pass-3 tests. While open, mouse and pane
    focus commands do not reach either view. In narrow mode the commits pane
    remains the visible pane for the prompt.
12. **`staging_refreshes_focused_and_unfocused_panes`** — open/focus a
    working-tree diff, dispatch space/u through builtin diff mode, drain the
    fake shared runner, and assert the existing hunk gates plus generation
    behavior and re-acquisition of both diff and commits. Repeat after focusing
    commits before completion to prove refresh is registration-wide, not focus-
    local. Existing real-repository stage/unstage round trip remains passing.
13. **`markdown_reflows_to_the_diff_pane_not_the_screen`** — load the committed
    Markdown fixture into a 120-column wide frame. Assert `Diff::resize` receives
    the diff content width (not 120), `MarkdownRows` produces the expected extra
    visual segments/table flow at that width, every row stays inside the diff
    span, and switching to 95-column narrow mode reflows to the full body width
    without rebuilding the Markdown model or changing logical line identity.
14. **`title_headers_and_status_name_the_focus_from_live_keys`** — assert wide
    frame has commits and diff headers, the focused one uses accent, title/status
    name the same focus, defaults show 4 and 0, and a config override/unbind
    changes the displayed key without changing pane code. Narrow frame shows
    only the focused header.
15. **`help_and_config_reload_follow_the_focused_pane`** — help rows use the
    focused mode plus global bindings; reload preserves focus, rebuilds modes,
    resizes/reflows both tenants from cached geometry, and keeps live search,
    layout/wrap choices, glyph selection, scrolloff, scrollbar, mouse config,
    and theme behavior.
16. Run the required gate exactly as written:

    ```sh
    cargo test -p gitten-tui -p gitten-core
    cargo test -p gitten-app
    cargo fmt --check
    cargo clippy -p gitten-tui -p gitten-core --all-targets -- -D warnings
    ./check.sh
    ```

Done criteria:

- [ ] `App` has no `stack: Vec<Screens>`, `stack.last()`, screen push, or pop;
      Enter replaces/focuses the stable diff tenant and `back` changes focus.
- [ ] Only commits and diff content panes ship; no placeholder status/files/
      branches/stashes view or acquisition was added.
- [ ] All ten existing pane/focus command names are answered from
      `core::command`; no new name/default/local key match exists.
- [ ] Wide geometry is disjoint at and above 96 columns; narrow geometry shows
      exactly the focused pane below it; both are verified at degenerate sizes.
- [ ] Every view resize, paint, hit, drag, scrollbar, status, and command uses
      its pane-local rectangle/Viewport; `Screen::span` fences every painted row.
- [ ] Search, staging/job refresh, Markdown pane-width reflow, selection,
      copy-on-select, and OSC 52 behavior have explicit passing survival tests.
- [ ] `core/Cargo.toml`, all `app/` files, `gitten.toml` format, shell, web,
      Markdown core/model files, and dependencies are unchanged.
- [ ] Every verification command exits 0 without launching a client or grabbing
      a tty.

## Stop conditions

Stop and escalate if any occurs:

- Any pane/focus name or stated default no longer exists in
  `core/src/command.rs`, or making it work appears to require a terminal alias
  or hardcoded keypress match. Reconcile with the shared registry instead.
- The only way to preserve Return in the supported terminal path is to keep
  translating Ctrl-J to Enter. Bring back a minimized crossterm event trace and
  ask whether `pane.next`'s shared default or LF-only Return compatibility wins;
  do not silently bind another terminal key.
- Pane composition requires a dependency, a UI rectangle/focus type in core, or
  changes to `app`/shell. Geometry is client drawing/input; viewport arithmetic
  and commands are already shared.
- A painter still calls `Screen::row` for pane content, or clips by slicing text
  before tokens/spans. Both can paint/style outside the pane. Use `Screen::span`
  and the existing `Pen::scroll` path.
- Markdown receives whole-screen width, reflows in `draw` every frame, performs
  a second prepare/layout pass, or loses table/logical-row identity. The only
  intended change is the scalar width supplied to `Diff::resize`.
- Adding the empty persistent diff requires pretending it was acquired from the
  working tree, assigning a fake generation, or allowing hunk writes against
  it. Model “not loaded yet” honestly and refuse actions until Enter replaces it.
- A job finish refreshes only focused/visible panes, or a refresh failure stops
  later registered panes being attempted. Pass 3 requires generation-wide
  invalidation.
- Search input shares the normal mode stack, pane navigation can consume query
  characters, Esc both cancels search and moves focus, or the prompt targets a
  pane by current index rather than stable name.
- Drag routing changes tenant after Down, copy-on-select reads the pane under Up
  instead of the captured pane, or a scrollbar is hit using global rather than
  pane-local columns.
- Meeting the minimum widths appears to require vertically stacking commits and
  diff below 96 columns. Stop with fixed-size frame evidence; do not create two
  unusably short viewports as an unreviewed fallback.
- Automatic per-cursor diff preview is introduced without a debounced background
  acquisition/cancellation design. Synchronous git I/O on `j`/`k` is outside
  scope and violates the responsiveness goal.
- Any test needs a live repository outside its temporary fixture, network, raw
  mode, a tty, or a launched client, or any verification command fails twice
  after one reasonable correction.

## Risks

- **Navigation-model migration (HIGH):** stack assumptions currently reach
  search, copy, mouse, refresh, status, and back behavior. A partial conversion
  can compile while silently addressing the wrong pane. The named-tenant tests
  and final grep for stack operations are mandatory.
- **Width/reflow churn (HIGH):** the diff's width is part of its expanded row
  table. Recomputing geometry per frame or alternately resizing hidden/visible
  panes would repeatedly reflow large Markdown fixtures. Cache geometry on
  resize/focus/registration and call `Diff::resize` only with stable rectangles.
- **Narrow threshold (MED):** 96 is a design choice grounded in 40 columns for
  abbreviated SHA + initials + useful graph/subject, one divider, and 55 for
  diff gutters plus readable text. It is not a universal terminal truth. Keep
  the constants together in the replaceable TUI layout policy and adjust only
  with fixed-cell evidence; do not add premature config.
- **Ctrl-J ambiguity (MED):** terminal LF and Ctrl-J are the same byte in some
  pty paths. Core now assigns Ctrl-J a pane meaning, so the old Return fallback
  cannot coexist in a stateless event translator. Normal crossterm Enter remains
  supported; the changed test must document the compatibility decision.
- **Persistent stale diff (MED):** without automatic preview, moving the commits
  cursor leaves the last explicitly opened diff visible. Its header must retain
  the opened SHA/subject so it never implies it follows the current row. Enter
  is the deliberate refresh/focus action.
- **Mouse capture (HIGH):** a global coordinate routed independently on each
  event can cross the divider, release the wrong scrollbar, or copy mixed text.
  Stable-name capture from Down through Up and pane-local coordinate tests are
  release gates.
- **Focus versus selection paint (MED):** hiding an unfocused cursor must not
  erase an actual dragged text/row selection or its copy content. Focus controls
  the caret bar and keyboard only; selection state remains pane-owned until its
  normal clear/refresh path.
- **Extensibility drift (MED):** hardcoding `commits` and `diff` as struct fields
  would make later panes reopen this foundation. The registry, canonical ranks,
  placement metadata, and replaceable layout policy are the pass's L-sized
  value even though only two tenants ship today.
