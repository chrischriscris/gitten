# Plan 041: The status bar never advertises a dead key

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving on. If a
> STOP condition occurs, stop and report. When done, update this plan's row in
> `plans/README.md`.
>
> **Drift check (run first)**: `git diff --stat 635aba8..HEAD -- shell/src/main.rs shell/src/chrome.rs`
> Written against `635aba8`. On a mismatch with the excerpts below, STOP.

## Status

- **Priority**: P1
- **Effort**: S
- **Risk**: LOW
- **Depends on**: none
- **Category**: bug
- **Planned at**: commit `635aba8`, 2026-08-31

## Why this matters

While the help panel stands, its mode deliberately swallows every pane verb
(plan 033: keys resolve against `help::MODE` alone). The status bar's hints,
though, are still computed from the focused pane's mode rows — so the bar
advertises `space stage`, `D discard`, keys that cannot fire, and hides
help's own live `j/k/g/G` scroll keys. The codebase states the rule in the
same file: "a hint naming a dead key is the one lie a panel of keys must
never tell" (`shell/src/main.rs:4750-4752`, written for the error exits).
A user presses an advertised key over the panel and nothing happens.

## Current state

- `shell/src/main.rs:4729-4740` — the hints arm:

  ```rust
  let (hints, truncated) = match (&message, self.input.is_some()) {
      // ...
      (None, false) => {
          // ...
          chrome::hints(
              &host,
              &self.modes,     // <-- includes help::MODE, but `which` is the pane's
              which,
              chrome::hints_budget(&host, width, &badge),
          )
      }
  };
  ```

  `self.modes` is the live stack — `sync_modes` pushes `help::MODE` when
  `self.help` (`main.rs:1389-1391`) — but `chrome::hints` projects rows for
  the modes it is given, keyed by `which` (the focused view's name), and
  `chrome.rs:433-451` shows only `[active, "global"]`-style mode rows.
- `core/src/command.rs:539-548` — help's own bindings (the keys that ARE
  live while help stands): `?`/`esc`/`j`/`down`/`k`/`up`/`g`/`home`/`G`/`end`.
- `shell/src/chrome.rs:387-393` — `hints` is "a projection and no
  decision": it walks the rows the given modes carry and prefers the
  focused pane's own mode before the globals.
- The help overlay itself is fine: it is a grouped reference of all modes'
  rows and its close hint is live (`shell/src/help.rs:117-125`).

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Window tests | `cargo test -p gitten-shell` | all pass |
| Lint/fmt | `cargo clippy --workspace -- -D warnings` / `cargo fmt --all -- --check` | exit 0 |

## Scope

**In scope**: `shell/src/main.rs` (the hints arm + test).

**Out of scope**: `shell/src/chrome.rs` (`hints`/`status_bar` are projections;
the fix is the caller's mode slice), `shell/src/help.rs`, `core/`.

## Git workflow

- Branch: `advisor/ui-041-live-hints-while-help-stands`, from `635aba8`.
- Commit style: `shell: the band says what is live while help stands`.

## Steps

### Step 1: Project the live modes when help stands

In the `(None, false)` hints arm (`main.rs:4731-4739`), pass the modes that
actually own the keyboard:

```rust
// While help stands it owns the keyboard: the bar projects help's rows,
// not the focused pane's — a hint naming a key the panel swallowed is the
// one lie a panel of keys must never tell.
let modes: &[&str] = if self.help {
    &[help::MODE, "global"]
} else {
    &self.modes
};
```

and pass `modes` to `chrome::hints`. Check how `help::MODE` and the global
mode name are spelled where they are already used as slices (grep
`resolve_mode_any(help::MODE` and the `"global"` literal in
`chrome.rs:433-451`) and use those exact spellings; define nothing new.

`which` stays as-is: with help's mode first, the projection prefers help's
rows, which is the point.

**Verify**: `cargo test -p gitten-shell` → all pass.

### Step 2: Test

In `shell/src/main.rs`'s test module (model on the existing `hints` tests —
grep `hints_budget` or `status_bar` in `#[gpui::test]`s): open help
(`run_command("help")`), assert the band's hints contain help's live chords
(`esc`, `j`, `k`) and none of the focused pane's swallowed ones (`space`,
`D` for a files-pane focus).

**Verify**: `cargo test -p gitten-shell` → all pass including the new test.

## Test plan

One new test as named in Step 2. Existing `hints` tests pin the shape and
must pass unchanged.

## Done criteria

- [ ] `cargo test -p gitten-shell` exits 0 with the new test
- [ ] clippy `-D warnings` + fmt clean
- [ ] No files outside `shell/src/main.rs` modified
- [ ] `plans/README.md` status row updated

## STOP conditions

- `chrome::hints` turns out to filter by live-key liveness internally
  already (then the bar cannot lie and this plan is void) — report.
- The global mode's name is not a plain `"global"` string literal.

## Maintenance notes

- The message overlay (plan 037) adds another mode; its band behaviour is
  that plan's scope. If both stand, help wins the keys (its resolution arm
  is first) — the hints slice should follow the same precedence.
