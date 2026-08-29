# Plan 021: Implement diff.discard-hunk in the terminal and pin client command parity

> **Executor instructions**: Follow the plan step by step. Run every verification
> command and confirm the expected result before moving on. If a STOP condition
> fires, stop and report — do not improvise. Do not edit `plans/` in any way:
> the orchestrator owns the index. Do not push.
>
> **Drift check (run first)**: your worktree was bootstrapped for you. Verify
> `git -C "$WT" log --oneline` shows exactly two commits — `eb888e1` plus one
> `carry:` commit touching exactly the **eight** tui files listed in the anchor
> refresh below — and `git -C "$WT" status --short` is empty. The excerpts
> below are from that carried state; where the scrollbar refactor moved an
> anchor, the **Anchor refresh** block wins. If anything differs, STOP.

## Anchor refresh — 2026-08-28, after the scrollbar-indicator refactor landed

The maintainer's working tree moved while this plan was in flight: the
"scrollbar is an indicator" refactor (decision 0027, carried in the tree)
removed `Diff`'s scrollbar grab — the `grabbed` field and every
`scrollbar::hit/grab/drag` call site — and shifted anchors. Where the excerpts
and steps below disagree with the carried files, **this block wins**. Anchors
verified by the reviewer against the carried tree:

- `tui/src/diff.rs`: `struct Diff` :51, `rebuild` :174, `reflow` :212, `down`
  :303, `up` :307, `page` :311, `scroll_y` :316, `to_top` :325, `to_bottom`
  :329, `jump_file` :339, `scroll_x` :367, `press` :437 (last param is now
  `_host`, and the scrollbar-grab branch is gone), `drag` :491 (likewise; the
  `grabbed` field no longer exists), `set_layout` :593, `cycle_layout` :602,
  `set_wrap` :617, `cycle_wrap` :631, `replace` :672.
- `tui/src/main.rs`: routing arm :2148, `hunk_verb` :2377, `hunk_action`
  :2927, creation-refusal :2960, the `_ => Write::unstage_patch` catch-all
  :2966, `the_empty_diff_pane_refuses_hunk_verbs_until_enter_replaces_it`
  :5681, `non_working_tree_and_untracked_hunks_are_refused_before_submission`
  :6195, `a_refreshed_frame_is_drawable_headlessly` :6410,
  `rebase_commands_remain_explicitly_deferred` :8605.
- The files-pane arm exemplar moved with the same refactor: locate
  `a_discard_arms_on_its_row_and_a_move_asks_again` by grep in
  `tui/src/files.rs`; its semantics are unchanged.
- Step 1's disarm rule for `press`/`drag` keeps its meaning with the scrollbar
  gone: disarm when the cursor's logical row or the viewport top changed —
  there is no thumb to jump, and the click still moves the cursor.
- The `shell/src/main.rs` window references kept their line numbers (the shell
  was not refactored).

Everything else — verbs, patch emission, the parity-guard command lists, the
command table, done criteria — is unchanged and was re-verified against the
carried tree.

## Status

- **Priority**: P1
- **Effort**: M
- **Risk**: MED (destructive verb; the `_` arm it lands in is currently wrong)
- **Depends on**: none
- **Category**: bug / parity
- **Planned at**: commit `eb888e1f3f3733b6f2020e2877c9d1fa68094f07`, 2026-08-28

## Why this matters

The shared keymap binds `D` in `[diff]` to `diff.discard-hunk`, the window
implements it (arm, confirm, `Write::discard_patch`), and the TUI's help panel —
a pure function of the same registry — advertises it. In the terminal the key
resolves, falls into `App::dispatch`'s catch-all, and says
`diff.discard-hunk does nothing here`. Worse: `hunk_action`'s match ends in
`_ => Write::unstage_patch`, so the day someone adds the dispatch arm naively,
`D` **unstages** instead of discarding. This plan implements the verb and adds a
parity guard so the next command core registers cannot silently become a
no-op (or a mis-aimed one) in this client.

## Current state

### The trap and the missing arm — `tui/src/main.rs`

Routing, `:2130-2132` (carried tree; only two of three verbs are routed):

```rust
"diff.stage-hunk" | "diff.unstage-hunk" => self.hunk_verb(command),
```

The gates and patch emission, `hunk_action`, `:2901-2943` — note the catch-all:

```rust
let patch = gitten_core::patch::emit(&path, &[&hunk]);
let built = match command {
    "diff.stage-hunk" => Write::stage_patch(repo, patch),
    _ => Write::unstage_patch(repo, patch),
};
built.map(|job| Box::new(job) as Box<dyn Job>)
```

and the creation refusal, `:2921-2935`, which currently has one sentence for
all verbs:

```rust
if creation {
    return Err(
        "that hunk adds a new file — stage or unstage it whole from the files pane".into(),
    );
}
```

`App::hunk_verb` (`:2361-2392`) reads the focused diff's `current_hunk()`,
checks the source gates, and submits; constructor errors surface via
`Err(e) => self.message = e`.

### The window's contract, to mirror exactly

- `shell/src/main.rs:3037-3039` routes all three names to `hunk_verb`.
- `shell/src/main.rs:1720-1725` — the creation refusal is **per verb**:
  stage/unstage → `"that hunk adds a new file — stage or unstage it whole from
  the files pane"`; discard → `"that hunk creates the file — discard it whole
  from the files pane"`.
- `shell/src/main.rs:1729-1738` — discard asks twice:
  `view.confirm_or_arm_discard_hunk(row)`; on the first press the band says
  `format!("discard this hunk of {path}? press again to confirm")`; the row it
  arms on is `cursor_row_id()` read *before* the verb runs (`:1692-1696`).
- `shell/src/main.rs:1740-1745` — `Write::discard_patch(&writes.repo, patch)`
  for the discard arm. The constructor (read `app/src/verbs.rs`, `discard_patch`,
  around `:126-136`) refuses an empty patch with its own sentence; surface that
  sentence verbatim like every other constructor refusal.

### What the terminal diff view has today — `tui/src/diff.rs`

- No armed-hunk state at all. Fields `:51-106` (selection, drag, scrollbar grab
  — no `armed_hunk`).
- Every cursor mutator is a separate public method, NOT funnelled: `down` `:306`,
  `up` `:310`, `page` `:314`, `scroll_y` `:319`, `to_top` `:328`, `to_bottom`
  `:332`, `jump_file` `:342`, `scroll_x` `:370`, `press` `:440` (calls
  `self.view.go_to(visual)`), `drag` `:499`, `replace` `:685`, `set_layout`
  `:606` → `rebuild` `:177`, `cycle_layout` `:615`, `set_wrap` `:630` →
  `reflow` `:215`, `cycle_wrap` `:644`.
- The logical-row address exists: `RowId` is already imported (`:46`), and
  `:394-411` shows `r.logical()` producing one from an order-table entry.
- The arm pattern to copy is the files pane's
  (`tui/src/files.rs:552-579`, `confirm_or_arm_discard` / `armed_row` /
  `armed_index`), and its test shape is
  `tui/src/files.rs:1334` `a_discard_arms_on_its_row_and_a_move_asks_again`.

### The registry — `core/src/command.rs`

`Commands::builtin()` (`:856-1100`) is the full name list; the existing test
`every_shipped_binding_names_a_registered_command` (`:1722-1730`) is the
shape the parity guard extends. The TUI test
`rebase_commands_remain_explicitly_deferred` (`tui/src/main.rs:8528`) already
pins three deliberately-unimplemented names — the guard generalizes that.

## Commands you will need

| Purpose | Command | Expected |
|---|---|---|
| Tests | `cargo test -q -p gitten-tui` (with `CARGO_TARGET_DIR=/Users/chus/Projects/gitten.wt/tui/target`) | all pass |
| Lint | `cargo clippy -q -p gitten-tui --all-targets --locked -- -D warnings` | no warnings |
| Format | `cargo fmt --check` | clean |
| Scope | `git -C "$WT" status --short` | only `tui/src/diff.rs` and `tui/src/main.rs` |

## Scope

**In scope**: `tui/src/diff.rs`, `tui/src/main.rs`.

**Out of scope** (do NOT touch):
- `core/`, `app/`, `git/`, `shell/` — the verb, patch emission and registry all
  exist; if any change there looks necessary, STOP.
- The armed hunk's **render tint**: the window tints the armed row, but the
  TUI's tint would need a new `Frame` field rippling through the `Rows` trait
  and every presentation. Deferred deliberately (recorded in
  `plans/README.md`); the status-line question carries the ask this pass.
- Implementing any other unimplemented command (reset/revert/cherry-pick/sync/
  rebase/status pane). Those are direction items, not this plan.

## Git workflow

- Branch: `advisor/021-tui-discard-hunk-parity-guard` (created, checked out,
  carry commit on top of `eb888e1`).
- Commit per step; messages like `tui: the hunk discard arms like the window's`.
- Do NOT push. Do NOT touch `plans/`.

## Steps

### Step 1: `tui/src/diff.rs` — the arm

1. Add the field to `Diff` (`:51-106`): `armed_hunk: Option<RowId>` (init
   `None` in both constructors).
2. Add methods:

```rust
/// The cursor's logical row — the address the window arms a discard on.
pub fn cursor_row_id(&self) -> Option<RowId> {
    self.order.get(self.view.cursor()).map(|r| r.logical())
}

/// Arms — or confirms — a discard of the hunk under this exact logical row.
/// First call asks (false); second call on the same row spends the arm (true);
/// anything else re-arms and asks again. Same contract as every list pane's
/// destructive verb.
pub fn confirm_or_arm_discard_hunk(&mut self, at: RowId) -> bool {
    let already = self.armed_hunk == Some(at);
    self.armed_hunk = match already {
        true => None,
        false => Some(at),
    };
    already
}

/// Drops the question. What a cursor move, a wheel, a refresh or a
/// presentation change calls.
pub fn disarm_hunk(&mut self) {
    self.armed_hunk = None;
}
```

3. Disarm on **every change of what the question was about**, calling
   `self.disarm_hunk()` at the top of: `down`, `up`, `page`, `scroll_y`,
   `to_top`, `to_bottom`, `jump_file`, `replace`, `set_wrap`, `cycle_wrap`,
   `set_layout`, `cycle_layout` (or once inside `rebuild` for the layout pair —
   your call, but both names must be covered), and in `press`/`drag` **only
   when** the cursor's logical row or the viewport top actually changed (a
   click on the armed row keeps the question, matching the files pane;
   `scroll_x` deliberately does **not** disarm — the window's horizontal pan
   doesn't either).
4. Unit tests in `diff.rs`'s `mod tests`, modelled on
   `a_discard_arms_on_its_row_and_a_move_asks_again` (files pane) and using the
   existing `view(...)`/`two_hunks()` helpers: arm/spend on the same row; a
   different row re-arms; each of `down`, `page`, `scroll_y`, `jump_file`
   disarms; `scroll_x` does not; a click on the armed row keeps it and a click
   on another row clears it; `replace` disarms; `cycle_layout` and
   `cycle_wrap` disarm.

**Verify**: `cargo test -q -p gitten-tui diff` → all pass, including the new ones.

### Step 2: `tui/src/main.rs` — route and arm

1. Routing (`:2132`): add the third name —
   `"diff.stage-hunk" | "diff.unstage-hunk" | "diff.discard-hunk" => self.hunk_verb(command),`.
2. In `hunk_verb`, for `diff.discard-hunk` only, after the existing
   source/repository gates and after reading `(path, hunk)` via
   `view.current_hunk()`: read `row = view.cursor_row_id()` **before** arming
   (the window's order), then

```rust
if command == "diff.discard-hunk" {
    let Some(row) = row else { /* unreachable when current_hunk answered, refuse defensively */ };
    if !view.confirm_or_arm_discard_hunk(row) {
        self.message = format!("discard this hunk of {path}? press again to confirm");
        return;
    }
    self.message.clear(); // the question is spent; the running band speaks next
}
```

   (`view` here is the focused `Screens::Diff`'s `Diff`, borrowed the way the
   existing body does. Stage/unstage keep acting on the first press — window
   parity.)

**Verify**: `cargo build -q -p gitten-tui` → exit 0.

### Step 3: `hunk_action` — three explicit arms

Replace the catch-all so a mis-route can never unstage:

```rust
let built = match command {
    "diff.stage-hunk" => Write::stage_patch(repo, patch),
    "diff.unstage-hunk" => Write::unstage_patch(repo, patch),
    "diff.discard-hunk" => Write::discard_patch(repo, patch),
    _ => return Err(format!("{command} is not a hunk verb")),
};
```

and make the creation refusal per verb (window parity):

```rust
if creation {
    return Err(match command {
        "diff.discard-hunk" => {
            "that hunk creates the file — discard it whole from the files pane".into()
        }
        _ => "that hunk adds a new file — stage or unstage it whole from the files pane".into(),
    });
}
```

Surface `Write::discard_patch`'s empty-patch refusal verbatim (the existing
`Err(e) => self.message = e` in `hunk_verb` already does).

**Verify**: `cargo build -q -p gitten-tui` → exit 0.

### Step 4: Integration tests

In `tui/src/main.rs`'s `staging` test module:

1. Generalize `non_working_tree_and_untracked_hunks_are_refused_before_submission`
   (`:6118`): its `said` helper takes the command name; run the same refusal
   table for `diff.discard-hunk` (between-commits, fixture, patch, no
   repository, not-on-a-hunk, untracked-creation — the last now expects the
   *discard* sentence).
2. New test `discard_hunk_asks_twice_then_submits_the_exact_patch`:
   fake repository (`fake(&[])` + `app_on_fake`), cursor onto a hunk; first
   `diff.discard-hunk` press → message is exactly
   `discard this hunk of f.txt? press again to confirm` and
   `state.writes` is empty; second press → exactly one write whose recorded
   patch, parsed with `gitten_core::parse_unified_diff`, contains the chosen
   hunk's edit and not the neighbour's (model the assertion on
   `a_refreshed_frame_is_drawable_headlessly`, `:6333`); a cursor move between
   presses forces a fresh question; a `Write::stash_apply` finish (refresh)
   disarms.
3. Extend `the_empty_diff_pane_refuses_hunk_verbs_until_enter_replaces_it`
   (`:5604`) to dispatch all three verbs on the empty pane.

**Verify**: `cargo test -q -p gitten-tui` → all pass.

### Step 5: The parity guard

In `tui/src/main.rs`'s `mod tests` add two constants and one test:

- `HANDLED_COMMANDS: &[&str]` — derive it from the **actual** dispatch arms
  (`App::dispatch` plus `Screens::run`) rather than trusting any list blindly;
  it must include every name the terminal answers: `quit`, `help`, `back`,
  `theme.cycle`, all twelve `view.*`, `diff.next-file`, `diff.prev-file`,
  `diff.cycle-layout`, `diff.cycle-wrap`, `diff.stage-hunk`,
  `diff.unstage-hunk`, `diff.discard-hunk`, `commits.open-diff`,
  `commits.search`, `status.focus`, `files.focus`, `branches.focus`,
  `commits.focus`, `stashes.focus`, `diff.focus`, `input.accept`,
  `input.cancel`, `pane.next`, `pane.prev`, `pane.left`, `pane.right`,
  `select.all`, `select.none`, `copy.selection`, `files.stage`,
  `files.commit`, `files.amend`, `files.discard`, `files.stage-all`,
  `files.ignore`, `files.stash`, `branches.checkout`, `branches.new`,
  `branches.rename`, `branches.delete`, `branches.new-tag`, `stashes.apply`,
  `stashes.pop`, `stashes.drop`.
- `DEFERRED_COMMANDS: &[&str]` — the deliberate gaps, exactly:
  `repo.push`, `repo.pull`, `repo.fetch`, `repo.refresh`,
  `commits.reset-menu`, `commits.reset-soft`, `commits.reset-mixed`,
  `commits.reset-hard`, `commits.revert`, `commits.cherry-pick`,
  `commits.cherry-pick-abort`, `commits.cherry-pick-continue`,
  `commits.squash-up`, `commits.fixup-up`, `commits.drop-commit`,
  `commits.new-tag`, `commits.new-branch`, `commits.checkout`,
  `commits.rebase-onto`, `rebase.abort`, `rebase.continue`.
- `parity_guard_covers_every_registered_command`:
  1. every name in `Commands::builtin().all()` is in `HANDLED ∪ DEFERRED`;
  2. the two lists are disjoint;
  3. for each **HANDLED** name: a fresh headless `app(30)` (fixture app, no
     repository) dispatches it, and the resulting `app.message` is never
     `format!("{name} does nothing here")` — this fails the day an arm is
     deleted. Use a fresh app per command; prompt-opening commands
     (`files.commit`, `files.amend`, `branches.new`, `branches.rename`,
     `branches.new-tag`, `commits.search`) open a prompt on the throwaway app,
     which is fine.
  4. for each **DEFERRED** name: the message IS exactly that fallback — so
     implementing one later forces reclassification instead of silence.
  Keep `rebase_commands_remain_explicitly_deferred` (`:8528`) green: its three
  names are in `DEFERRED_COMMANDS`.

**Verify**: `cargo test -q -p gitten-tui parity` → passes; then deliberately
comment out one dispatch arm in a scratch build to confirm the guard catches
it, and revert.

### Step 6: Full gates

**Verify**:
- `cargo test -q -p gitten-tui` → all pass
- `cargo clippy -q -p gitten-tui --all-targets --locked -- -D warnings` → clean
- `cargo fmt --check` → clean
- `git -C "$WT" status --short` → only the two in-scope files

## Test plan

- `tui/src/diff.rs`: the arm unit tests from step 1 (arm/spend, per-mutator
  disarm, pan-keeps, click semantics, refresh/layout/wrap disarm).
- `tui/src/main.rs`: the refusal table extended to discard; the two-press
  submit test with patch-bytes assertion; the empty-pane test widened; the
  parity guard.
- Patterns to copy: `a_discard_arms_on_its_row_and_a_move_asks_again`
  (`tui/src/files.rs:1334`) for arm semantics;
  `a_refreshed_frame_is_drawable_headlessly` (`tui/src/main.rs:6333`) for
  asserting the submitted patch's contents;
  `every_shipped_binding_names_a_registered_command`
  (`core/src/command.rs:1722`) for registry walks.

## Done criteria

- [ ] `cargo test -q -p gitten-tui` exits 0, including the parity guard and the
      new discard tests
- [ ] clippy/fmt gates exit 0
- [ ] `grep -n "_ => Write::unstage_patch" tui/src/main.rs` → no matches
- [ ] `grep -n "diff.discard-hunk" tui/src/main.rs` → the routing arm, the
      hunk_verb arm, `hunk_action`'s arm, and the parity list
- [ ] `git -C "$WT" diff <carry>..HEAD --stat` → only `tui/src/diff.rs` and
      `tui/src/main.rs`

## STOP conditions

Stop and report if:
- `Write::discard_patch`'s signature or refusal behaviour differs from what
  `app/src/verbs.rs` shows (read it before step 3).
- Arming appears to require a `core` or `shell` change.
- The parity guard's behavioral sweep cannot run on the headless fixture app
  for some HANDLED command (say which; do not weaken check 3 for it without
  reporting).
- The window's discard contract in `shell/src/main.rs:1720-1745` has drifted
  from the excerpts.

## Maintenance notes

- `DEFERRED_COMMANDS` is the living roadmap of this client's gaps (sync, reset,
  revert, cherry-pick, folds, rebase lifecycle, status pane). When one lands,
  move it to `HANDLED_COMMANDS` in the same commit — the guard makes that the
  path of least resistance.
- The armed hunk has no render tint this pass (deliberate; a `Frame` field
  would ripple through every presentation). If the tint lands later, it reads
  `armed_hunk` the way `files.rs` reads `armed_index`.
- Reviewer focus: the disarm surface. Every mutator list in step 1.3 must be
  exhaustive; a missed one lets a stale yes fire on a moved keyboard.
