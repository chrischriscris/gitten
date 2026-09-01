# Plan 065: Break the 3,200-line `impl DevShell` into modules by responsibility

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. **This is a pure code-motion plan. You must not
> change a single line of logic.** When done, update the status row for this
> plan in `plans/high-priority/README.md`.
>
> **Drift check (run first)**: `git diff --stat da9f8a7..HEAD -- shell/src/main.rs`
> `shell/src/main.rs` is the most-edited file in the tree. If it has changed,
> re-derive the line ranges below by method **name** rather than trusting the
> numbers; on a missing or renamed method, treat it as a STOP condition.

## Status

- **Priority**: P1
- **Effort**: M
- **Risk**: LOW — the compiler verifies a pure move; no behaviour is touched
- **Depends on**: none. **Do this before any other plan that edits
  `shell/src/main.rs`**, or land it after them — never concurrently.
- **Category**: tech-debt
- **Planned at**: commit `da9f8a7`, 2026-08-31

## Why this matters

`impl DevShell` runs from `shell/src/main.rs:1460` to `:4697` — **3,237 lines and
87 methods in one block** — and holds at least six unrelated responsibilities:
pane and focus routing, every prompt, roughly twenty-five write verbs, the job
runner and refresh cycle, live settings, raw input events, and the chrome
builders. The file has two separate section markers both labelled
`// ---- the branch verbs` (`:2113` and `:2501`), which is what a file looks like
when nobody can hold its shape in their head.

**The cost is already being paid, in writing.** The pass-9 plan index scheduled
its own work around this file:

> `main.rs` is the collision zone: 045, 046, 048, 049, 050 and 052 all touch it.
> To keep merges cheap, integrate in waves: **Wave 2 (serial through main.rs)**:
> 049 → 045 → 046 → 048 → 052, each rebased on the previous merge.

Six plans serialized — not because they depend on each other, but because they
all edit one file. That is the concrete cost: parallel work made sequential, and
a rebase per plan. `plans/README.md` also records a real bug from exactly this —
a conflict resolution during pass 7 "briefly took the keycap's border ink for its
numeral", caught two PRs later.

After this plan, a verb change and a focus change touch different files, and the
waves can run in parallel.

## Current state

**File**: `shell/src/main.rs`, 11,009 lines total — production code ends at
`:6148` where `#[cfg(test)] mod tests` begins. There is a second test module,
`mod title_tests`, at `:10857`.

**The crate already has this convention.** `shell/src/main.rs:1-12`:

```rust
mod chrome;
mod config;
mod controls;
mod dispatch;
mod graph;
mod help;
mod input;
mod menu;
mod panes;
mod session;
mod stats;
mod views;
```

Each is a sibling file in `shell/src/`. This plan adds six more of the same kind.

**Why this works in Rust, so you do not doubt it midway**: `DevShell` is declared
in the crate root (`main.rs`). A type's private fields are visible in the module
that defines it *and every descendant module*, so a `impl DevShell` block living
in `shell/src/verbs.rs` can read and write every private field with no `pub(crate)`
anywhere. Inherent impls may be split across modules of the same crate freely.

**The six groups, by method name.** Line numbers are from `da9f8a7`; **match on
the method name, not the number**:

**1 → `shell/src/verbs.rs`** — the write verbs (files, history, branches):
`stage_or_unstage` (1725), `discard_selected` (1846), `stage_all` (1905),
`ignore_selected` (1951), `hunk_verb` (1991), `stash_working_tree` (2100),
`reset_menu` (2135), `reset_question` (2160), `reset_selected` (2167),
`revert_selected` (2221), `cherry_pick_selected` (2253), `rewrite_selected` (2304),
`rebase_abort_command` (2385), `rebase_continue_command` (2397),
`cherry_pick_abort_command` (2417), `cherry_pick_continue_command` (2429),
`rebase_branch_selected` (2449), `branches_target` (2505), `checkout_branch` (2520),
`sync_remote` (2561), `status_verb` (2587), `stash_selected` (2606),
`checkout_commit` (2723), `delete_branch_selected` (2919).

**2 → `shell/src/prompts.rs`** — everything that stands a text field up or takes
its answer: `open_input` (1633), `close_input` (1668), `begin_commit_message` (1759),
`commit_message` (1777), `begin_amend_message` (1799), `amend_message` (1819),
`begin_branch_new` (2644), `begin_commit_branch_prompt` (2652),
`begin_named_branch_prompt` (2677), `begin_branch_tag_prompt` (2700),
`begin_branch_rename` (2748), `begin_branch_prompt` (2777), `branch_named` (2814),
`begin_tag_prompt` (2853), `tag_named` (2884), `begin_search` (2979),
`search_edited` (3008), `finish_search` (3041), `search_pane` (3057).

**3 → `shell/src/jobs.rs`** — submitting work and folding results back:
`writes` (1707), `submit` (3083), `drain_jobs` (3087), `refresh_stale` (3163),
`finish_refresh` (3212), `invalidate_refresh` (3239).

**4 → `shell/src/focus.rs`** — which pane holds the keyboard and what the main
view shows: `active` (1464), `column_commits` (1478), `list_order` (1496),
`set_spot` (1508), `sync_focus` (1525), `active_view_name` (1550),
`sync_modes` (1563), `stack_for` (1577), `register_pane` (3070), `back` (3572),
`focus_pane` (3625), `cycle_pane` (3646), `pane_walk` (3669), `focus_named` (3709),
`focus_main` (3722), `sync_main_diff` (3743), `schedule_main_diff` (3776).

**5 → `shell/src/events.rs`** — raw input and pointer gestures: `on_key` (3967),
`smooth_pixels` (4042), `open_context_menu` (4061), `context_pick` (4092),
`on_wheel` (4120), `copy_selection` (3893).

**6 → `shell/src/settings.rs`** — the live knobs: `native` (3249),
`set_overrides` (3265), `set_wrap` (3306), `set_layout` (3316), `set_theme` (3341),
`cycle_theme` (3351).

**Deliberately staying in `main.rs`**, because they are the shell's own shape
rather than a responsibility beside it:
`fresh_host` (1610), `set_notice` (1623), `set_question` (1629) — three tiny
helpers every group calls; `run_command` (3366) and `run_command_from` (3377) —
the dispatch table, which is the map of everything above and belongs next to the
struct; `strip` (4231), `probed` (4510), `section_content` (4527),
`commits_section` (4585), and `impl Render for DevShell` (4698) — the render path;
`fn main` (5540); and every `struct`/`enum` declaration.

**Repo conventions**: doc comments explain *why*, in prose. Each existing sibling
module opens with a `//!` module doc saying what it holds and why it is its own
file — see `shell/src/chrome.rs` and `shell/src/panes.rs` for the register to
match.

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Shell tests | `cargo test -q -p gitten-shell` | exit 0, all pass |
| Whole workspace | `cargo test -q --workspace` | exit 0, all pass |
| Lint | `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 |
| Format | `cargo fmt --all --check` | exit 0 |
| Everything headless | `./dev check` | prints `✓ all green`, exit 0 |

**Never launch the app.** `AGENTS.md`: *"Never launch a client unless asked … a
window appearing unannounced, or a terminal taken over, interrupts whoever is at
the keyboard."* The headless tests are the verification here.

## Scope

**In scope**:
- `shell/src/main.rs` — removing method bodies and adding six `mod` lines
- `shell/src/verbs.rs`, `prompts.rs`, `jobs.rs`, `focus.rs`, `events.rs`,
  `settings.rs` (all new)

**Out of scope** (do NOT touch):
- **Any change to logic, control flow, names, strings, or signatures.** Not a
  renamed variable, not a tidied `match`, not a clippy suggestion beyond what the
  move mechanically requires. A move you can review by eye is the entire value of
  this plan; a move with edits hidden in it is worse than no move.
- `shell/src/main.rs`'s two test modules. They stay where they are. Moving tests
  is a second, separate decision.
- The `Screen`, `Spot`, `Prompt`, `Notice`, `GitError`, `Writes`, `Refresh` types
  and every other declaration.
- `shell/src/dispatch.rs` — an existing module that translates keystrokes. Do not
  add to it and do not name a new module anything like it.
- `tui/src/main.rs` — the same shape exists there and is plan 066's subject.

## Git workflow

- Branch: `advisor/cx-065-split-the-devshell-impl`
- **One commit per group** (six commits). A reviewer reads a move commit by
  checking that nothing appears in the diff that was not in the file before.
- Commit message style, from `git log`: lowercase, `scope: sentence`, e.g.
  `shell: the write verbs get their own file`.
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: Baseline

```sh
cargo test -q --workspace 2>&1 | tee /tmp/065-before.txt
./dev check
```

Record the exact test counts per crate. `./dev check` must print `✓ all green`.

**Verify**: both exit 0. If not, STOP — you cannot attribute a later failure.

### Step 2: Move group 1 (`verbs.rs`) and prove the pattern works

Create `shell/src/verbs.rs`:

```rust
//! The write verbs: what a keypress does to the repository.
//!
//! Split out of `main.rs`, where they shared one 3,200-line `impl` with focus
//! routing, prompts, jobs and the render path. Nothing here is new — every
//! method is the one that was in that block, moved. They sit together because
//! they are one shape: check the pane, find the target, refuse in words if
//! there is nothing to act on, arm or spend a destructive question, then hand a
//! `Write` job to the runner.

use crate::{DevShell, Screen, /* ...whatever the moved bodies reference... */};

impl DevShell {
    // ...the twenty-four methods, moved verbatim...
}
```

Delete those methods from `main.rs` and add `mod verbs;` to the module list at
the top, keeping it alphabetical with the existing ones.

**Resolving imports is the only real work.** The moved bodies reference types and
helpers that `main.rs` had in scope. Add `use crate::…` lines to the new file
until it compiles. Do **not** make anything `pub` to achieve this — private items
of the crate root are already reachable from a descendant module. If you find
yourself adding `pub`, you have mis-derived what needs importing; STOP and report.

**Verify**:
- `cargo test -q -p gitten-shell` → exit 0, same test count as Step 1
- `git diff --stat` on this commit shows `main.rs` losing roughly what
  `verbs.rs` gains (a modest delta for `use` lines and the module doc is
  expected; a large one means something was rewritten)

### Step 3: Move groups 2–6

Repeat Step 2 for `prompts.rs`, `jobs.rs`, `focus.rs`, `events.rs` and
`settings.rs`, in that order, **one commit each**. Give each a `//!` doc in the
same register as `verbs.rs`'s.

After each group:

**Verify**: `cargo test -q -p gitten-shell` → exit 0, unchanged test count.

If a method turns out to belong to two groups by its name, put it where its
*callers* are and say so in your report. Do not split a method.

### Step 4: Confirm nothing changed but addresses

This is the review the plan exists to make possible. Run it yourself:

```sh
# Every moved method still exists, exactly once, somewhere in the crate.
for m in stage_or_unstage discard_selected stage_all ignore_selected hunk_verb \
         stash_working_tree reset_menu reset_selected revert_selected \
         cherry_pick_selected rewrite_selected rebase_branch_selected \
         branches_target checkout_branch sync_remote status_verb stash_selected \
         checkout_commit delete_branch_selected open_input close_input \
         begin_commit_message commit_message begin_amend_message amend_message \
         begin_branch_new begin_branch_rename branch_named tag_named \
         begin_search search_edited finish_search writes submit drain_jobs \
         refresh_stale finish_refresh invalidate_refresh active list_order \
         set_spot sync_focus sync_modes register_pane back focus_pane \
         cycle_pane pane_walk focus_named focus_main sync_main_diff \
         schedule_main_diff on_key on_wheel copy_selection open_context_menu \
         context_pick native set_overrides set_wrap set_layout set_theme \
         cycle_theme; do
  n=$(grep -rc "fn $m" shell/src/*.rs | awk -F: '{s+=$2} END {print s}')
  [ "$n" = "1" ] || echo "MISSING OR DUPLICATED: $m ($n)"
done
```

**Verify**: the loop prints nothing.

Then the whole gate set:
- `cargo fmt --all --check` → exit 0
- `cargo clippy --workspace --all-targets -- -D warnings` → exit 0
- `cargo test -q --workspace` → exit 0, **test counts identical to Step 1's
  `/tmp/065-before.txt`**
- `./dev check` → `✓ all green`

### Step 5: Shrink the marker, and say what happened

Delete the two now-meaningless `// ---- the branch verbs` markers in the
**production** part of `main.rs` (`:2113` and `:2501`) — they were section
dividers inside the block this plan dissolved.

**There is a third one at `:10215`, inside the test module. Leave it.** The test
modules are out of scope, and a marker inside one is that module's business.

Add a short paragraph to `shell/src/main.rs`'s module documentation naming the
six modules and what each holds, so the next reader finds a verb without grepping.

**Verify** — note the `awk`, which stops at the first `#[cfg(test)]` so the
count covers production code only:
- `awk '/^#\[cfg\(test\)\]/{exit} {print}' shell/src/main.rs | grep -c "the branch verbs"` → `0`
- `awk '/^#\[cfg\(test\)\]/{exit} {print}' shell/src/main.rs | wc -l` reported
  before and after; it is 6,147 at `da9f8a7`, and should lose roughly 2,500 lines

## Test plan

**No new tests.** This plan adds no behaviour, and a test written against moved
code tests the move rather than the code.

The test plan is that **the existing suite passes with an identical count**:

- `cargo test -q -p gitten-shell` — the desktop tests, which use GPUI's headless
  context and lay out the real `uniform_list`
- `cargo test -q --workspace`
- `./dev check` — which additionally draws real terminal frames and would catch a
  panic no unit test reaches

A changed test count in either direction is a STOP condition: fewer means a test
stopped being compiled, more means something was added that should not have been.

## Done criteria

ALL must hold:

- [ ] `cargo fmt --all --check` exits 0
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` exits 0
- [ ] `cargo test -q --workspace` exits 0 with **the same test counts** as Step 1
- [ ] `./dev check` prints `✓ all green`
- [ ] Step 4's loop prints nothing
- [ ] The six new files exist, each with a `//!` module doc
- [ ] `awk '/^#\[cfg\(test\)\]/{exit} {print}' shell/src/main.rs | grep -c "the branch verbs"` → 0
      (production only; the marker inside the test module stays)
- [ ] `git diff da9f8a7..HEAD --stat` touches only `shell/src/main.rs`, the six
      new files, and this plan's status row
- [ ] `git diff da9f8a7..HEAD -- shell/src/main.rs | grep '^+' | grep -v '^+++'`
      shows **only** `mod` lines and module documentation — no moved logic came
      back in modified form
- [ ] No item gained a `pub` it did not have

## STOP conditions

Stop and report back — do not improvise — if:

- A method named in the six groups does not exist, or exists under a different
  name (drift — `shell/src/main.rs` is the most-edited file in the tree).
- Making a module compile requires adding `pub` or `pub(crate)` to anything.
  Private items of the crate root are already visible to descendant modules; if
  that is not working, your understanding of the layout is wrong and guessing
  will produce a wider diff than the plan intends.
- The test count changes in either direction.
- You are tempted to fix something you notice while moving it — a clippy lint, a
  duplicated guard, an awkward name. **Write it down in your report and leave the
  code alone.** The reviewability of a pure move is this plan's entire product.
- Two methods turn out to be mutually recursive across group boundaries in a way
  that will not compile. (It should compile — they are inherent methods on one
  type — but report rather than restructuring if it does not.)
- `./dev check` fails on the terminal-frames section. That section catches panics
  no unit test reaches, and a move should not be able to cause one.

## Maintenance notes

- **What a reviewer should scrutinize**: the `+` lines in `main.rs`'s diff.
  There should be nothing there but `mod` declarations and prose. Everything else
  is `-` lines and new files.
- **What will interact with this**: every future plan that touches the window.
  That is the point — after this, a verb change and a focus change are different
  files, and the pass-9 index's serial wave through `main.rs` can be parallel.
- **Deliberately deferred**: `run_command`/`run_command_from` (205 lines of
  dispatch) stays in `main.rs` on purpose — it is the index of everything moved
  out, and reading it next to the struct is how someone finds their way. Move it
  only if it grows a second responsibility.
- **Not done here**: the same 87-method shape exists in `tui/src/main.rs`. Plan
  066 addresses the deeper version of that problem — the two clients duplicating
  the verbs outright — and doing this split first makes the shared seam easier to
  see.
