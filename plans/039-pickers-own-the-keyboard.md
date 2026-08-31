# Plan 039: The title-bar pickers own the keyboard

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving on. If a
> STOP condition occurs, stop and report. When done, update this plan's row in
> `plans/README.md`.
>
> **Drift check (run first)**: `git diff --stat 635aba8..HEAD -- shell/src/main.rs shell/src/controls.rs core/src/command.rs`
> Written against `635aba8`. On a mismatch with the excerpts below, STOP.

## Status

- **Priority**: P1
- **Effort**: S–M
- **Risk**: LOW
- **Depends on**: none
- **Category**: bug (interaction model)
- **Planned at**: commit `635aba8`, 2026-08-31

## Why this matters

Five window settings — theme, layout, wrap, algorithm, whitespace — open a
`controls::Picker` dropdown from the title bar. In a keyboard-first app
(`AGENTS.md` rule 2) they are reachable **only by mouse**: the module doc
concedes it (`shell/src/controls.rs:33-37`, "if a picker ever needs search,
grouping or keyboard navigation, that is the moment to throw this away and take
the framework's"). Worse than the missing navigation: while a picker stands, the
keyboard is **not owned by anything** — `Shell::on_key` resolves input fields
against `input::MODE` and help against `help::MODE`, but an open picker gets
neither, so keys resolve through the full stack and act on the rows the menu
occludes; and `sync_modes` clears `self.open = None` on every resolved command
(`main.rs:1396`), so the same press both moves the hidden list and dismisses
the menu — one press, two unstated effects. `q` quits with the menu up. Help
and the text field prove the ownership pattern exists; the picker is the one
modal left outside it.

## Current state

- `shell/src/main.rs:1198` — `open: Option<Open>`; the `Open` value names the
  open picker (grep `struct Open` / `enum Open` nearby for its exact shape).
- `shell/src/main.rs:1390-1396` — `sync_modes`:

  ```rust
  if self.help {
      self.modes.push(help::MODE);
  }
  if self.show_message && self.error.is_some() {
      self.modes.push(MESSAGE_MODE);
  }
  self.pending.clear();
  self.open = None;      // <-- every resolved command closes the picker
  ```

- `shell/src/main.rs:3720-3728` — the resolution match: input →
  `resolve_mode_any(input::MODE, …)`; help → `resolve_mode_any(help::MODE, …)`;
  fall-through → `resolve_any(&self.modes, …)`. No picker arm.
- `shell/src/main.rs:3799-3805` — the wheel already stands aside for an open
  picker (`if self.help || self.open.is_some() { return; }`).
- `shell/src/controls.rs:100-108` — `picker(id, p, open, theme, font,
  on_toggle, on_pick)`: a trigger plus a deferred, `on_mouse_down_out`-dismissed
  list. `Picker { label, options, current, enabled }` (lines 59-69) has no
  highlighted-row state; rows carry `on_click` only (lines 232-243).
- `core/src/command.rs:539-548` — the help mode, the template for a mode that
  re-spells its own verbs; `bind(&mut self, mode, chord, command)` at line 561
  rejects prefix conflicts, so single-chord bindings only.

Repo conventions: `core/` takes no dependencies; commands are names, resolved
by each client (`AGENTS.md` "a key is data and a command is a name"); commit
messages lowercase imperative (`shell: the help panel owns the keyboard it explains`).

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Core tests | `cargo test -p gitten-core` | all pass |
| Window tests | `cargo test -p gitten-shell` | all pass |
| Lint | `cargo clippy --workspace -- -D warnings` | exit 0 |
| Format | `cargo fmt --all -- --check` | exit 0 |

## Scope

**In scope**:
- `core/src/command.rs` (a "picker" mode's bindings + tests)
- `shell/src/main.rs` (mode push, resolution arm, highlighted-index state, dispatch)
- `shell/src/controls.rs` (draw the highlighted row; accept keyboard-driven
  highlight changes)

**Out of scope**:
- `controls::Select` (the framework's searchable widget) — the module doc's
  alternative. Only if Step 1's STOP fires.
- The web and terminal clients — pickers are window chrome.
- The command palette direction option — unrelated.

## Git workflow

- Branch: `advisor/ui-039-pickers-own-the-keyboard`, from `635aba8`.
- Commit style: `shell: the picker owns the keyboard it occludes`.

## Steps

### Step 1: A "picker" mode in `core`

In `core/src/command.rs::builtin()`, beside the help bindings:

```rust
// The title-bar pickers own the keyboard while they stand: resolved against
// their mode alone, so half the keymap cannot act on rows the menu occludes.
// j/k move the highlight, enter picks, esc dismisses.
bind("picker", "j", "picker.down");
bind("picker", "down", "picker.down");
bind("picker", "k", "picker.up");
bind("picker", "up", "picker.up");
bind("picker", "enter", "picker.pick");
bind("picker", "esc", "back");
bind("picker", "?", "help");   // help stays reachable, as from any mode
```

Register the four commands with one-line docs (the same table the help panel
projects). **Verify**: `cargo test -p gitten-core` → all pass.

### Step 2: The shell resolves against the mode while a picker stands

- In `sync_modes` (`main.rs:1390-1396`): when `self.open.is_some()`, push a
  `PICKER_MODE` ("picker"; define `const PICKER_MODE: &str = "picker";` beside
  `MESSAGE_MODE` at line 1041) **before** the `self.open = None` line — that
  line closes pickers on every command and must become conditional: only
  close on commands that are not picker verbs. Simplest honest rule: leave
  `self.open` untouched here and let the picker verbs and `back` close it
  (grep every `self.open = Some` writer first — the toggle handler owns
  closing via `on_toggle` already).
- In the resolution match (`main.rs:3720-3728`), add before the fall-through:

  ```rust
  false if self.open.is_some() => host.keys.resolve_mode_any(PICKER_MODE, &typed),
  ```

**Verify**: `cargo test -p gitten-shell` → all pass.

### Step 3: The picker verbs

- Highlighted index: add `highlighted: usize` to the shell's open state beside
  `Open` (not to `controls::Picker` — that stays a pure function of its
  fields; pass it in as a field of `Picker` and let `picker()` draw it).
- Dispatch in `Shell::run_command`'s match: `picker.down` / `picker.up` move
  the highlight (clamp to `options.len()`); `picker.pick` calls the same
  closure `on_pick` uses with the highlighted index, then closes;
  `back` closes via the toggle path (same as an outside click).
- In `controls.rs`, draw the highlighted row's ink the way the hovered/clicked
  row reads today (`selection_bg` wash — copy the existing row-draw at
  lines 232-243).

**Verify**: `cargo test -p gitten-shell` → all pass.

### Step 4: Tests

In `core/src/command.rs`, model on the help-mode tests: with "picker" pushed,
`files.discard` resolves to nothing; `j`/`enter`/`esc` resolve to the picker
verbs. In `shell/src/main.rs` (`#[gpui::test]`, model on a picker-opening
test — grep `open = Some` in the test module):

1. opening a picker then pressing `j` moves the highlight and the picker stays open;
2. `enter` applies the highlighted option (assert the picked value changed) and closes;
3. `q` with a picker open does **not** quit — it is `picker.down`'s chord neighbour... 
   (use an unbound-in-picker chord, e.g. `D`, and assert nothing underneath fired
   and the picker remains open).

**Verify**: `cargo test -p gitten-core -p gitten-shell` → all pass.

## Test plan

- New core tests: picker mode swallows the stack; its six chords resolve.
- New shell tests: highlight movement, pick-and-close, no underneath action.
- Structural patterns: the help-mode tests (core) and any existing picker test
  (shell) named in Step 4.

## Done criteria

- [ ] `cargo test -p gitten-core -p gitten-shell` exits 0 with the new tests
- [ ] `cargo clippy --workspace -- -D warnings` exits 0; fmt clean
- [ ] Every keyboard-openable setting is pickable without the mouse (the five
      pickers share the one mode — open each in a test or by hand via `./dev`)
- [ ] No files outside the in-scope list modified
- [ ] `plans/README.md` status row updated

## STOP conditions

- `Open`'s shape does not identify *which* picker is open (needed to route
  `picker.pick` to the right `on_pick`) — report what you found.
- `bind` rejects a chord as a prefix conflict (read line 557-560 first) — do
  not swap chords on a guess; report the conflict.
- The `on_toggle`/`on_pick` closure signatures make shell-side dispatch
  impossible without widening `controls.rs`'s public surface beyond drawing
  the highlight — report.

## Maintenance notes

- Reviewer: verify the `sync_modes` change did not break the toggle's own
  close path (opening a *second* picker must close the first — check the
  toggle handler).
- When the command palette lands it should reuse this mode or follow 037's
  pattern; note the overlap in its plan.
- Deferred: search inside pickers (that is `controls::Select`'s job, per the
  module doc).
