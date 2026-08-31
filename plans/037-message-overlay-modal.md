# Plan 037: The message overlay owns the keyboard, the way help does

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving on. If a
> STOP condition occurs, stop and report — do not improvise. When done, update
> this plan's row in `plans/README.md`.
>
> **Drift check (run first)**: `git diff --stat 635aba8..HEAD -- shell/src/main.rs core/src/command.rs`
> This plan was written against `635aba8` (= `origin/full/full`). If any
> in-scope file changed since, compare the "Current state" excerpts against the
> live code before proceeding; on a mismatch, STOP.

## Status

- **Priority**: P1
- **Effort**: M
- **Risk**: LOW-MED
- **Depends on**: none
- **Category**: bug
- **Planned at**: commit `635aba8`, 2026-08-31

## Why this matters

`message.show` (`` ` ``) opens a full-window overlay that displays a failed git
command's complete answer. The help panel used to have the same flaw and plan
033 fixed it: while help stands, keys resolve against help's mode *alone*, so a
chord the panel does not bind runs nothing. The message overlay never got that
treatment — the keyboard still resolves through the full pane stack while the
overlay covers the screen. `D` arms a files discard behind a panel the user
cannot see under; `j`/`k` scroll rows the user cannot see; the wheel
hit-tests and focuses lists under it. A panel of keys that arms a file
discard behind itself is a trap, and this is one.

## Current state

- `shell/src/main.rs` — the window. Key facts:
  - `const MESSAGE_MODE: &str = "message";` (line 1041). `sync_modes` pushes it
    only when `self.show_message && self.error.is_some()` (lines 1392-1394).
  - Key resolution, `Shell::on_key` (lines 3720-3728):

    ```rust
    let resolved = match self.input.is_some() {
        true => host.keys.resolve_mode_any(input::MODE, &typed),
        // While the help panel stands it owns the keyboard the same way a
        // native field does: resolved against its mode *alone*, so a chord
        // the map does not give it runs nothing underneath — a pane's `D`
        // reads as "not bound" instead of arming a discard behind a screen
        // that is only describing it. `Resolve::None` below says so.
        false if self.help => host.keys.resolve_mode_any(help::MODE, &typed),
        false => host.keys.resolve_any(&self.modes, &typed),
    };
    ```

    There is no arm for the message overlay: `self.modes` already contains
    `MESSAGE_MODE` (sync_modes pushed it), but `resolve_any` falls through
    inner modes, so a pane's `D` still resolves and still fires.
  - The wheel interceptor stands aside for help and pickers but not the
    overlay (lines 3799-3805):

    ```rust
    if self.help || self.open.is_some() {
        return;
    }
    ```

  - The overlay itself, `message_overlay` (lines 4833-4862): a full-window
    `div().occlude().absolute().inset_0()` panel on `c.title_bg`. It blocks
    the mouse but not key resolution.
  - The esc ladder that closes it: `Shell::back` (lines ~3310-3314) spends
    its first `esc` clearing `show_message` before anything else.
- `core/src/command.rs` — the keymap. `message.show` is bound **GLOBAL** on
  `` ` `` (line 332), `esc` → `back` GLOBAL (line 333). The help mode is the
  ownership template (lines 532-548): it re-spells its exits in its own mode
  so they stay reachable when inheriting the globals no longer happens:

  ```rust
  bind("help", "?", "help");
  bind("help", "esc", "back");
  bind("help", "j", "view.scroll-down");
  // ... k / g / G / home / end
  ```

  `MESSAGE_MODE` ("message") binds nothing today. Core's own test
  `the_help_mode_swallows_the_pane_verbs_it_is_only_describing` (same file)
  documents the semantics a mode-alone resolution must have.

Repo conventions: no dependency may be added to `core/`; GPUI element work
follows the notes in `AGENTS.md` (`.id()` before interactivity, `deferred` +
`.occlude()` for floats). Commit messages are lowercase imperative prose, e.g.
`shell: the help panel owns the keyboard it explains`.

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Core tests | `cargo test -p gitten-core` | all pass |
| Window tests | `cargo test -p gitten-shell` | all pass |
| Lint | `cargo clippy --workspace -- -D warnings` | exit 0 |
| Format | `cargo fmt --all -- --check` | exit 0 |

## Scope

**In scope** (the only files you should modify):
- `core/src/command.rs` (add the "message" mode's bindings + tests)
- `shell/src/main.rs` (the resolution arm, the wheel guard, tests)

**Out of scope** (do NOT touch):
- `shell/src/help.rs` — the help panel is correct; do not refactor it.
- The overlay's visual design (colours, radius, padding) — separately planned.
- `self.show_message` lifecycle beyond what Step 3 needs (stale-flag cases are
  another plan's scope).

## Git workflow

- Branch: `advisor/ui-037-message-overlay-modal`, branched from `635aba8`.
- Commit per step; message style: `shell: the message overlay owns the
  keyboard it explains` (lowercase imperative; see `git log --oneline -10`).
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: Give the "message" mode its own bindings in `core`

In `core/src/command.rs`, inside `Commands::builtin()`, beside the help mode's
bindings (lines 539-548), add the overlay's exits re-spelled in its own mode
(the overlay is read once and dismissed; its only live verbs are leaving it):

```rust
// The message overlay owns the keyboard the way help does: resolved against
// its mode alone, so a pane verb cannot arm behind a panel that is only
// describing git's answer. The same command names the lists use, re-spelled
// here so they stay reachable when inheriting the globals no longer happens.
bind("message", "`", "message.show");
bind("message", "esc", "back");
```

**Verify**: `cargo test -p gitten-core` → all pass (new mode compiles; the
bind must not be rejected as a prefix conflict — `` ` `` and `esc` are single
chords).

### Step 2: Resolve against the mode alone while the overlay stands

In `shell/src/main.rs`, extend the resolution `match` (lines 3720-3728) with
an arm before the fall-through, mirroring the help arm:

```rust
false if self.show_message && self.error.is_some() => {
    host.keys.resolve_mode_any(MESSAGE_MODE, &typed)
}
```

Add a comment in the help arm's voice: a pane's `D` reads as "not bound"
instead of arming a discard behind a panel that is only describing git's
answer. `Resolve::None`'s existing handler already says so; confirm its
message covers this path (it prints "is not bound" for an unbound key when
no inner mode owns the press — check the branch at lines ~3741-3758 and
extend its comment only if needed).

**Verify**: `cargo test -p gitten-shell` → all pass.

### Step 3: The wheel stands aside too

In `shell/src/main.rs` line 3803, extend the guard:

```rust
if self.help || (self.show_message && self.error.is_some()) || self.open.is_some() {
    return;
}
```

matching the comment's list of full-window occluding surfaces.

**Verify**: `cargo test -p gitten-shell` → all pass.

### Step 4: Tests

In `core/src/command.rs`, model on the help-mode tests (grep
`the_help_mode_swallows_the_pane_verbs`): with the "message" mode pushed, a
pane verb (`files.discard`) resolves to nothing while `` ` `` and `esc`
resolve to `message.show` / `back`.

In `shell/src/main.rs` (`#[gpui::test]`, model on the existing ladder test
`esc_peels_the_overlay_then_the_error_then_the_ladder`): with an error set and
the overlay shown, a resolved pane verb (arm `D` on a files row first) does
not reach the pane — the arm does not exist after the press; `esc` then
closes the overlay and the ladder continues as before.

**Verify**: `cargo test -p gitten-core -p gitten-shell` → all pass, including
the new tests.

## Test plan

- New: message mode swallows pane verbs; its two exits stay live (core).
- New: no pane verb fires while the overlay stands; the esc ladder is
  unchanged (shell).
- Structural pattern: the help-mode tests named above.

## Done criteria

- [ ] `cargo test -p gitten-core -p gitten-shell` exits 0 with the new tests
- [ ] `cargo clippy --workspace -- -D warnings` exits 0
- [ ] `cargo fmt --all -- --check` exits 0
- [ ] `grep -n 'resolve_mode_any(MESSAGE_MODE' shell/src/main.rs` finds exactly one call
- [ ] No files outside the in-scope list modified (`git status`)
- [ ] `plans/README.md` status row updated

## STOP conditions

- The resolution `match` at `main.rs:3720-3728` does not match the excerpt
  (033's shape changed since).
- `resolve_mode_any` turns out to inherit globals (read its doc) — then
  binding only the two exits is insufficient; report back with what its doc
  actually says instead of binding more chords on a guess.
- A step's verification fails twice after a reasonable fix attempt.

## Maintenance notes

- Any future overlay-like panel must follow the same three moves (mode in
  core, resolve-alone arm, wheel guard) — the reviewer should check all
  three, not just the first.
- If the command palette (a recorded direction option) lands, it is another
  modal and inherits this pattern.
- Deferred out of scope: scrollable overlay body for very long answers, and
  the stale-`show_message` lifecycle (plan 042).
