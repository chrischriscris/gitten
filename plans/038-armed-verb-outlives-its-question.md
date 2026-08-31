# Plan 038: An armed verb's question lives exactly as long as the arm

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving on. If a
> STOP condition occurs, stop and report. When done, update this plan's row in
> `plans/README.md`.
>
> **Drift check (run first)**: `git diff --stat 635aba8..HEAD -- shell/src/main.rs shell/src/views/`
> Written against `635aba8`. On a mismatch with the excerpts below, STOP.

## Status

- **Priority**: P1
- **Effort**: S
- **Risk**: LOW
- **Depends on**: none (do not run concurrently with plan 037 — both edit the
  `Resolve::Run` block in `shell/src/main.rs`; land 037 first)
- **Category**: bug
- **Planned at**: commit `635aba8`, 2026-08-31

## Why this matters

The double-press contract is documented in the window's own handlers: "first
press arms the row **and says so in the band**; second press on the same row
builds the job; any cursor move, wheel or refresh disarms before it can lie"
(`shell/src/main.rs:1630-1632`). One path breaks the saying half: **every**
resolved keypress clears `self.notice` (`main.rs:3735`), and `self.notice` is
where the armed question's sentence lives (`set_question`, `main.rs:1418-1422`)
— but the arm itself is view state (`shell/src/views/files.rs:671-682`,
`confirm_or_arm_discard`), cleared only by cursor move, wheel, refresh or
spend. So: `D` (question stands) → `c` (commit prompt opens; the band question
vanishes) → `esc` → `D` spends the arm and discards instantly, with no
question standing. The keyboard-only variant `g` → `l` → `l` → `h` lands an
instant hard reset. A press the user believes re-asks executes.

## Current state

- `shell/src/main.rs:3730-3739` — every resolved command clears the notice:

  ```rust
  match resolved {
      Resolve::Pending => {}
      Resolve::Run(name) => {
          let name = name.to_string();
          self.pending.clear();
          self.notice = None;          // <-- kills the question, not the arm
          cx.stop_propagation();
          cx.notify();
          self.run_command(&name, cx);
          return;
      }
  ```

- `shell/src/main.rs:1418-1422` — `set_question` stores the question text as
  `self.notice`. `Notice` has two variants (lines 1056-1057): `Info` and
  `Question`.
- `shell/src/main.rs:3344-3349` — the existing disarm sweep, on the
  cursor-move path:

  ```rust
  if view.read(cx).armed() {
      view.update(cx, |v, _| v.disarm());
      cx.notify();
      return;
  }
  ```

  Each view implements `disarm()` (e.g. `shell/src/views/commits.rs:380-383`)
  and exposes `armed()`.
- The spend sites are the verb handlers themselves (e.g. the `files.discard`
  arm in `main.rs`), where `confirm_or_arm_*` returning `true` means the job
  is built — the arm is gone and the question must go with it.

**Key invariant** (why the fix is shaped the way it is): the second press of
the armed verb runs through the *same* `Resolve::Run` block. Killing the arm
or the question there unconditionally would break the double-press itself.
The contract is: the question lives as long as the arm; the arm's own verb
(spend) clears both; anything else that runs clears neither.

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Window tests | `cargo test -p gitten-shell` | all pass |
| Lint | `cargo clippy --workspace -- -D warnings` | exit 0 |
| Format | `cargo fmt --all -- --check` | exit 0 |

## Scope

**In scope**:
- `shell/src/main.rs` (the notice-clearing rules + tests)

**Out of scope**:
- `shell/src/views/*` — the views' arm/disarm state is correct as is.
- The reset-menu question (`core/src/command.rs` "question" mode) — it owns
  its keyboard and is unaffected.

## Git workflow

- Branch: `advisor/ui-038-question-lives-as-long-as-the-arm`, from `635aba8`.
- Commit style: `shell: the question stands as long as the arm does`.

## Steps

### Step 1: An intervening command clears only an info notice

At `main.rs:3735`, replace `self.notice = None;` with a variant-aware clear:

```rust
// An intervening command says nothing about an armed row: the question in
// the band describes state the view still holds, and clearing the sentence
// while the arm survives is how a press the user believes re-asks
// executes. Only an info notice — a toast about a finished thing — dies
// with the next key.
if self
    .notice
    .as_ref()
    .is_some_and(|n| !n.is_question())
{
    self.notice = None;
}
```

Add `fn is_question(&self) -> bool` to `Notice` in `main.rs` (~line 1056) if
no such predicate exists.

**Verify**: `cargo test -p gitten-shell` → all pass.

### Step 2: The spend sites still clear the question

For each verb handler that *spends* an arm (`confirm_or_arm_discard`,
`confirm_or_arm_reset`, `confirm_or_arm_drop`, `discard_hunk`, the branch
delete) — the handler body knows the `true` return: confirm each sets
`self.notice = None` when the job is built. Where one already routes through
a shared helper that clears it, leave it. Grep: `grep -n "set_question\|notice = None" shell/src/main.rs`.

**Verify**: `cargo test -p gitten-shell` → all pass.

### Step 3: The disarm sweep clears the question too

At the disarm sweep (`main.rs:3344-3349`), after `v.disarm()`, also clear a
standing question: `self.notice = None;` beside `cx.notify()`. The cursor
move disarmed the arm; the sentence must not outlive it.

**Verify**: `cargo test -p gitten-shell` → all pass.

### Step 4: Tests

Model on `a_cursor_move_disarms_a_hard_reset_before_any_yes_can_land`
(`main.rs:8495`) and `moving_the_keyboard_disarms_and_a_staged_row_refuses_aloud`
(`main.rs:8097`):

1. `an_intervening_command_keeps_the_question_standing_with_its_arm` — arm a
   files discard (`D`), run `commits.new` (any unrelated command), assert the
   band still shows the question (`shell.notice` is a `Question`) and the arm
   is alive (`view.read(cx).armed()`); then a second `D` still asks (does not
   spend) — the contract intact.
2. `a_cursor_move_after_an_intervening_command_clears_both` — continue the
   first test with a cursor move; assert no question in the band and no arm.
3. `the_spending_press_clears_the_question` — arm, press `D` again, assert
   the job started and the band is clear.

**Verify**: `cargo test -p gitten-shell` → all pass including the 3 new tests.

## Test plan

See Step 4 — three tests, named, in `shell/src/main.rs`'s test module,
modelled on the two cited `#[gpui::test]`s.

## Done criteria

- [ ] `cargo test -p gitten-shell` exits 0 with the 3 new tests
- [ ] `cargo clippy --workspace -- -D warnings` exits 0; `cargo fmt --all -- --check` clean
- [ ] A grep of `Resolve::Run`'s block shows the conditional clear, not an unconditional one
- [ ] No files outside `shell/src/main.rs` modified
- [ ] `plans/README.md` status row updated

## STOP conditions

- `Notice` turns out to have more than two variants or the question text does
  not live in `self.notice` — report what you found.
- The spend sites are not reachable from `shell/src/main.rs` (an arm spent
  inside a view without the shell knowing) — report; the fix shape changes.
- Any existing test asserts the old behaviour (notice cleared by any press) —
  report the test name; it is pinning the bug.

## Maintenance notes

- Reviewer: check the second-press path specifically — it is the one flow
  where a naive "clear everything" or "clear nothing" both fail.
- If a command palette lands (direction option), its filter input goes
  through `input::MODE`'s arm, not this one; nothing to change.
