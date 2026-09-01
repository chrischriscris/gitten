# Plan 050: A context menu projected from the keymap

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update this plan's row in
> `plans/high-priority/README.md`.
>
> **Drift check (run first)**: `grep -n "fn row_bar" shell/src/views/diff.rs`
> must hit (the design pass is on your base). Line refs were taken at
> `00842dc` + the staged design pass; match on quoted content where a ref
> drifted; STOP on a structural mismatch.
>
> **Build cost**: `export CARGO_TARGET_DIR=/tmp/gitten-target`. Never launch
> `./dev desktop` or `./dev tui`.

## Status

- **Priority**: P2
- **Effort**: M
- **Risk**: MED (a new floating surface; modal-guard interactions)
- **Depends on**: plan 045 (a right-click must select the row it lands on
  before offering verbs for it)
- **Category**: feature (mouse parity) — every verb is keyboard-only today

## Why this matters

There is no context menu anywhere, and outside the title-bar pickers no
mouse path to any verb: stage, commit, checkout, apply, discard are all
keys. But the app already owns the whole mechanism a context menu needs:
`Keymap::help` projects the active modes against the command registry into
`(keys, doc)` rows — that projection *is* the status bar's hints and the
help overlay. A context menu is the same projection over the clicked pane's
mode, drawn at the pointer, dispatching by command *name* through the one
path every key already uses.

That shape is why this is cheap and why it satisfies rule 1: an extension
that registers a command in a pane's mode appears in that pane's context
menu without being told — the same test `SplitRows` and the pickers already
pass. No hardcoded menu entries, ever.

## Current state

- The projection: `host.keys.help(&host.commands, modes)` → `HelpRow::Mode /
  Command { name, keys, doc } / Blank` (used by `chrome::hints`,
  `shell/src/chrome.rs:310-360`, and the help overlay,
  `shell/src/help.rs:37-119`). `Commands::hint` gives the short label;
  `doc` the long one.
- Dispatch by name: the wheel path already dispatches resolved command
  names outside a physical keypress (`main.rs:3841-3860`) — the context
  menu uses the same entry point.
- The floating-surface kit, all proven in `controls.rs`: `deferred` with
  priority (the picker list at `controls.rs:231`), `.occlude()`, dismiss on
  `on_mouse_down_out` (`controls.rs:199-202`), the `picker_backdrop`
  (`controls.rs:241-245`), row height `ROW_H = 24` ("a menu row is a
  target"), width from `font.advance` (`controls.rs:174-180`).
- Who may hold state: `controls.rs`'s header documents the pattern — the
  control is a pure function; the open flag lives on `DevShell` (one field,
  `self.open: Option<Open>` for pickers). The context menu gets the same
  treatment: one `Option<ContextMenu>` (pane name, row, position) on
  `DevShell`.
- Modal guards: pickers/help swallow wheels via occluding surfaces and
  `on_wheel` stands aside for them (`main.rs:3803`); `sync_modes` closes
  pickers on focus change. Plans 037/039/042 shaped these rules — read
  their landed forms first.

## Commands you will need

| Purpose | Command | Expected |
|---|---|---|
| Core tests | `cargo test -q -p gitten-core` | exit 0 |
| Shell tests | `cargo test -q -p gitten-shell` | exit 0 |
| Everything | `./dev check` | exit 0 |

## Scope

**In scope**: `shell/src/controls.rs` or a sibling `shell/src/menu.rs` (the
menu element, pure function of rows + position); `shell/src/main.rs` (the
open-state field, the right-click wiring, wheel/keypress guards, dispatch);
the sidebar views only for the right-click handler beside plan 045's
left-click one; `core/src/command.rs` only if the projection needs a
narrower helper than `help` (prefer reusing `help` filtered to the pane's
own mode — no globals section in a context menu).

**Out of scope**: submenus, separators beyond the projection's own `Blank`
rows, icons, disabled-item rendering (a command the registry projects is
runnable by definition — that is the honest-hints rule the status bar
already enforces); the diff pane's text-selection right-click semantics
(diff rows keep text selection; the menu triggers only where selection does
not claim the button — see STOP conditions).

## Git workflow

- Branch: `advisor/ui-050-context-menu`
- Commits per step, e.g. `shell: a right-click asks the keymap what it may
  do here`
- No push, no PR, unless the operator instructed it.

## Steps

### Step 1: The element

A `pub fn context_menu(...)` beside the picker: takes the projected rows
(only `HelpRow::Command`s of the pane's own mode, each `(keys, hint-or-doc)`),
the theme/font, a screen-space position, and two callbacks (`on_pick(name)`,
`on_dismiss`). Presentation mirrors the picker list: `deferred` priority 1
over a priority-0 backdrop, `occlude`, `on_mouse_down_out` dismisses,
`ROW_H = 24`, key drawn bright and label dim (the status bar's ink rule).
Position: below-right of the pointer, clamped so the menu never paints past
the window edge (the one placement decision the picker never needed —
clamp, don't flip, and say so in a comment).

**Verify**: `cargo test -q -p gitten-shell` → exit 0 (a width/row-count
pure-function test like `help::panel_width`'s).

### Step 2: Open, dispatch, close

- `DevShell` gains `context: Option<...>` (pane name, row index, position).
- Right-click (`MouseButton::Right` mouse-down) on a sidebar row: run plan
  045's select-row verb first, then set `context`. Right-click on empty
  pane space: open with the pane's mode rows, no row selection.
- `on_pick` dispatches the command name through the same path the wheel
  uses, then clears `context`.
- Guards, matching the pickers exactly: `on_wheel` stands aside while open
  (extend the `self.help || self.open.is_some()` check at `main.rs:3803`);
  any keypress dismisses first (a keyboard-first app must never make a key
  wait for a mouse surface); focus change dismisses (`sync_modes`).

**Verify**: `cargo test -q -p gitten-shell` → exit 0.

### Step 3: Tests

- `a_context_menus_rows_are_the_keymaps_own` — build the projection for the
  files mode, assert `files.stage`'s hint is in it and nothing hardcoded is
  (the seam test: register a new command in the mode, assert it appears
  without an edit to the menu).
- `a_pick_dispatches_by_name_and_closes` and
  `a_keypress_dismisses_the_menu_before_resolving`.

**Verify**: `./dev check` → exit 0.

## Done criteria

- [ ] `./dev check` exits 0
- [ ] No literal command name appears in the menu code
      (`grep -n '"files\.\|"branches\.\|"commits\.\|"stashes\.' <menu file>`
      → no hits)
- [ ] A registered-at-test-time command appears in the projection (test)
- [ ] Wheel and keys are guarded while open, matching picker behaviour
- [ ] No files outside the in-scope list modified (`git status`)
- [ ] `plans/high-priority/README.md` row updated

## STOP conditions

- Plan 045 has not landed on your base (`grep` its select-row verb) — this
  plan's right-click depends on it; report rather than inlining a second
  selection path.
- GPUI's right-button events conflict with the diff's selection `press`
  handling in a way that costs diff rows their text selection — leave the
  diff out entirely and report; sidebar-only is still a shippable step.
- The projection cannot exclude the globals without a change to
  `Keymap::help`'s row shape — report; changing `core`'s projection API
  affects the help panel, the status bar and the TUI at once.
