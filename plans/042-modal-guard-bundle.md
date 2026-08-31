# Plan 042: Three small modal-guard fixes (wheel under prompt, live esc hint, stale overlay flag)

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving on. If a
> STOP condition occurs, stop and report. When done, update this plan's row in
> `plans/README.md`.
>
> **Drift check (run first)**: `git diff --stat 635aba8..HEAD -- shell/src/main.rs git/src/lib.rs`
> Written against `635aba8`. On a mismatch with the excerpts below, STOP.

## Status

- **Priority**: P1
- **Effort**: S (three independent one-site fixes)
- **Risk**: LOW
- **Depends on**: 037 softly (both touch `message_overlay`'s surroundings in
  `shell/src/main.rs`; land 037 first)
- **Category**: bug
- **Planned at**: commit `635aba8`, 2026-08-31

## Why this matters

Three defects, each one site, all in the window's modal plumbing:

1. **A wheel notch under a standing prompt re-aims the keyboard's pane.**
   `on_wheel` guards help and pickers but not the text field
   (`main.rs:3803`): while a commit message is being typed, wheeling a list
   calls `focus_pane`/`set_spot(Spot::Main)` (`main.rs:3821,3825`), moving
   the focus ring and armed-state target while the prompt keeps the keys —
   when the prompt closes, the keyboard has walked somewhere the finger, not
   the user, aimed.
2. **The error band's exits hint hardcodes `esc` and re-walks the keymap
   every frame.** `main.rs:4745-4754` builds `· esc dismiss · {key} full
   text` inside render: the dismiss half is a literal `"esc"` while the
   full-text half correctly resolves `live_keys_for("message.show", …)`.
   Errors persist until dismissed — indefinitely — so this is (a) a dead-key
   lie for a config that rebinds `back`, and (b) a per-frame allocation +
   registry walk on the render path (repo rule 3). `open_input`
   (`main.rs:1435-1449`) already resolves its exits once at open and says
   so: "the field does not re-walk the keymap per frame for a keyboard it
   holds."
3. **A job started while the message overlay is open leaves `show_message`
   stale.** `JobEvent::Started` clears `self.error` but not
   `self.show_message` (`main.rs:2845-2847`). The overlay vanishes (the
   render gate needs `error.is_some()`), the flag stays true, and the
   user's next `esc` is swallowed closing an overlay that is not there; if
   they never press it, the next failure auto-reopens the overlay
   uninvited.

## Current state

```rust
// main.rs:3799-3805
if self.help || self.open.is_some() {
    return;
}
```

```rust
// main.rs:4741-4754 (inside the render path)
// An error says how to leave, where it stands: `esc` dismisses,
// the message key opens the full text. Live keys only — a hint
// naming a dead key is the one lie a panel of keys must never
// tell.
let exits = self
    .error
    .as_ref()
    .and_then(|_| {
        host.keys
            .live_keys_for("message.show", &self.modes)
            .into_iter()
            .next()
    })
    .map(|key| SharedString::from(format!("· esc dismiss · {key} full text")));
```

```rust
// main.rs:2845-2847
JobEvent::Started { name } => {
    self.running = Some((format!("running {name}"), Instant::now()));
    self.error = None;
```

The overlay flag: `self.show_message`, pushed into the mode stack by
`sync_modes` only when `self.show_message && self.error.is_some()`
(`main.rs:1392-1394`).

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Window tests | `cargo test -p gitten-shell` | all pass |
| Lint/fmt | `cargo clippy --workspace -- -D warnings` / `cargo fmt --all -- --check` | exit 0 |

## Scope

**In scope**: `shell/src/main.rs` only.

**Out of scope**: `shell/src/input.rs` (the prompt field is correct),
`core/`, the GitError summary parsing (plan 043).

## Git workflow

- Branch: `advisor/ui-042-modal-guard-bundle`, from `635aba8` (or 037's tip).
- Commit style: three commits, e.g. `shell: a wheel notch under a prompt
  moves nothing`, `shell: the error's exits resolve once, where the error
  is set`, `shell: a started job closes the message overlay with the error`.

## Steps

### Step 1: The prompt joins the wheel guard

`main.rs:3803` → `if self.help || self.input.is_some() || self.open.is_some() { return; }`,
with the comment's list updated to name all three.

**Verify**: `cargo test -p gitten-shell` → all pass.

### Step 2: The error's exits resolve once, where the error is set

Wherever `self.error` is assigned (grep `self.error = Some` — the job-finish
path), also compute both live keys and store the finished sentence:
`live_keys_for("back", &self.modes)` and `live_keys_for("message.show",
&self.modes)`, formatted as `· {back} dismiss · {show} full text` into a
field on the standing error (extend the `GitError` holder in `main.rs` or a
tuple beside it — your call, but the render arm must only read). The render
arm at 4745-4754 shrinks to reading the stored `SharedString`. An error that
arrived without the acquisition prefix still resolves both keys the same
way. Drop the `esc` literal.

**Verify**: `cargo test -p gitten-shell` → all pass.

### Step 3: A started job closes the overlay with the error

At `main.rs:2847`, beside `self.error = None;`, add
`self.show_message = false;` and call `self.sync_modes(cx);` if the
surrounding code does for that path (match what the `Finished` arm does
below it — mirror it exactly).

**Verify**: `cargo test -p gitten-shell` → all pass.

### Step 4: Tests

Model on the existing esc-ladder and job tests (grep
`esc_peels_the_overlay_then_the_error_then_the_ladder` and
`drain_jobs` in the test module):

1. wheel over a pane while `input.is_some()` moves neither `spot` nor focus;
2. with a config that rebinds `back` (the test `Host`'s keymap supports a
   custom binding — see how existing tests rebind; if none does, bind via
   the test host's `keys`), the band's dismiss half names that key, not
   `esc`, and the string is computed once (assert on the stored field, not
   a re-derivation);
3. overlay open → a job starts → `esc` reaches the app (does not get
   swallowed), and no overlay reopens on the next failure without `message.show`.

**Verify**: `cargo test -p gitten-shell` → all pass including the 3 new tests.

## Test plan

Three tests as named in Step 4, in `shell/src/main.rs`'s test module.

## Done criteria

- [ ] `grep -n '"esc dismiss"' shell/src/main.rs` finds nothing
- [ ] `cargo test -p gitten-shell` exits 0 with the new tests
- [ ] clippy `-D warnings` + fmt clean
- [ ] No files outside `shell/src/main.rs` modified
- [ ] `plans/README.md` status row updated

## STOP conditions

- The error is assigned in more than one place with different mode stacks
  (report the sites).
- Step 2's stored-sentence approach conflicts with how `GitError` is
  constructed for non-job errors and no single assignment site exists.

## Maintenance notes

- Any new modal must join the `on_wheel` guard in the same commit that adds
  it — the reviewer checks the guard first.
- If the command log pane (direction option) lands, job events move there;
  Step 3's clear moves with them.
