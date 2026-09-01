# Plan 066: One verb, written once — the tracer bullet for a shared verb seam

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise.
>
> **This plan deliberately ports exactly ONE verb.** When Step 5 passes, you are
> done. Do not port a second verb, however mechanical it looks — the point of a
> tracer bullet is to find out what the seam costs before twenty-four of them
> depend on it. Porting more is a STOP condition, not initiative.
>
> **Drift check (run first)**:
> `git diff --stat da9f8a7..HEAD -- shell/src/main.rs tui/src/main.rs shell/src/views/branches.rs tui/src/branches.rs core/src/refs.rs app/src/verbs.rs`
> On a structural mismatch with the excerpts below, treat it as a STOP condition.

## Status

- **Priority**: P2
- **Effort**: M (as scoped — one verb. The full port it enables is L.)
- **Risk**: MED — it introduces a new cross-client seam
- **Depends on**: plan 065 (soft — landing 065 first makes `shell/src/main.rs`'s
  half of this a small file rather than a 3,200-line block. Not required.)
- **Category**: tech-debt
- **Planned at**: commit `da9f8a7`, 2026-08-31

## Why this matters

`CLAUDE.md` states the rule this plan serves:

> **A client is drawing and input, and nothing else.** … Anything two of them
> need is a bug until it is in `core`: the row flattening, the order table, the
> token-versus-span merge and the graph's branch colours were each written twice
> before they were written once.

Today **the window and the terminal each implement the same ~25 write verbs**, in
full. At `da9f8a7`, 26 method names appear in both `shell/src/main.rs` and
`tui/src/main.rs` — `delete_branch_selected`, `checkout_branch`, `hunk_verb`,
`stash_selected`, `stash_working_tree`, `begin_commit_message`,
`begin_amend_message`, `begin_branch_new`, `begin_branch_rename`, `begin_search`,
`drain_jobs`, `refresh_stale`, `sync_main_diff`, `submit`, `back`, `pane_walk`,
`cycle_pane`, `focus_named`, `sync_modes` and more — plus five renamed pairs
(`stage_all`/`files_stage_all`, `discard_selected`/`files_discard`,
`ignore_selected`/`files_ignore`, `stage_or_unstage`/`files_stage`,
`begin_branch_tag_prompt`/`begin_branch_tag`).

**This is not two implementations of one idea. It is one implementation, typed
twice.** Compare `shell/src/main.rs:2919-2961` against `tui/src/main.rs:1792-1832`:
the same guards in the same order, the same arm-then-spend protocol, the same
`unreachable!` with the same comment, and **character-for-character the same
sentences shown to the user**:

- `"nothing selected to delete"`
- `"a detached HEAD is not a branch"`
- `"a remote branch is its remote's to delete — fetch prunes it here"`
- `"a fixture has no repository to delete branches from"`

The drift is already documented. `plans/README.md` records a pass-4 integrator
ruling that had to reconcile, by hand, whether "the destructive arm outlives focus
round-trips on every pane, matching the window". That is one client's policy being
manually copied to the other, after the fact, because nothing made them share it.

**What this plan does not do**: port all 25. It builds the seam, moves one verb
onto it, and stops — so the cost of the seam is known from a real port rather than
estimated from a sketch. The remaining verbs are a follow-up the owner decides on
with this plan's report in hand.

## Current state

### The two copies

`shell/src/main.rs:2919-2961`:

```rust
    fn delete_branch_selected(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.active(), Some(Screen::Branches { .. })) {
            self.set_notice("branches.delete is not supported here");
            return;
        }
        let Some(target) = self.branches_target(cx) else {
            self.set_notice("nothing selected to delete");
            return;
        };
        let shown = match &target {
            views::branches::Target::Local(name) => String::from_utf8_lossy(name.as_bytes()),
            views::branches::Target::Remote { remote, branch } => {
                format!("{}/{}", remote.to_string_lossy(), branch.to_string_lossy()).into()
            }
            views::branches::Target::Detached => {
                self.set_notice("a detached HEAD is not a branch");
                return;
            }
        };
        if matches!(target, views::branches::Target::Remote { .. }) {
            self.set_notice("a remote branch is its remote's to delete — fetch prunes it here");
            return;
        }
        let Some(Screen::Branches { view, .. }) = self.active() else {
            unreachable!("checked above");
        };
        let Some(writes) = self.writes() else {
            self.set_notice("a fixture has no repository to delete branches from");
            return;
        };
        // Arm, or spend the arm. False means the question was just asked.
        if !view.update(cx, |b, _| b.confirm_or_arm_delete(&target)) {
            self.set_question(format!("delete branch {shown}? press again to confirm"));
            return;
        }
        self.notice = None; // the question is spent; the running band speaks next
        let name = match target {
            views::branches::Target::Local(name) => name.as_bytes().to_vec(),
            _ => unreachable!("remotes and detached refuse above"),
        };
        let job = gitten_app::verbs::Write::delete_branch(&writes.repo, name, false);
        if !writes.send(Box::new(job)) {
            self.set_notice("the job queue is shutting down");
        }
    }
```

`tui/src/main.rs:1792-1832` is the same function with four substitutions:
`self.set_notice(x)` → `self.message = x.into()`, `self.branches_target(cx)` →
`self.branch_target()`, `view.update(cx, |b, _| b.confirm_or_arm_delete(&t))` →
a `match self.panes.focused_mut()`, and `writes.send(..)` → `self.submit(..)`.
Its question text is already factored into `tui/src/branches.rs:276`:

```rust
pub fn delete_question(shown: &str) -> String {
    format!("delete branch {shown}? press again to confirm")
}
```

— the window inlines the same string.

### `Target` is also written twice

`shell/src/views/branches.rs:143-155` and `tui/src/branches.rs:81` declare the
**same three-variant enum** with the same meaning:

```rust
pub(crate) enum Target {
    /// A local branch, named relative to `refs/heads`.
    Local(PathBytes),
    /// A remote-tracking branch. Checkout may aim here — git detaches onto
    /// the fetched commit — but rename and delete refuse tonight, on purpose.
    Remote { remote: PathBytes, branch: PathBytes },
    /// The detached-HEAD row: a place, not a branch, and every branch verb
    /// says so rather than guessing which branch was meant.
    Detached,
}
```

It is pure data about refs and it belongs in `core::refs`, whose own module doc
already claims exactly this territory (`core/src/refs.rs:1-8`):

> The names git keeps: branches, stashes, remotes, tags and the reflog. …
> they are pure data: acquisition lives in `gitten-git`, drawing lives in a
> client, and neither gets to teach these types about the other.

`core::refs` already defines `pub type RefName = crate::status::PathBytes;`
(`core/src/refs.rs:35`), which is what both copies' `PathBytes` resolves to.

### Where the shared orchestration belongs

**`app/`, not `core/`.** The policy needs `app::verbs::Write` and the job runner,
and `Write` depends on `gitten-git` — so `core`, which has zero dependencies,
cannot hold it. `app/src/verbs.rs`'s module doc already frames the crate as the
place where a verb is composed (`app/src/verbs.rs:1-10`):

> This module is the wrapper and nothing else: it captures a [`Handle`] clone
> plus the verb's arguments, names itself for the running band, and calls the
> trait. No client learns whether the implementation shelled out, and an
> extension composes these exact words — or its own, over the same handle and
> the same queue — without a line changing here.

The three things a client genuinely owns, and which must stay behind a trait:
**what is selected**, **how a sentence is shown**, and **how a job is queued**.

`app/src/jobs.rs:14` — `pub trait Job: Send + 'static`, the queue's currency.

**Repo conventions**: `core` takes no new dependencies, ever. Doc comments explain
*why*, in prose. Tests live in a `#[cfg(test)] mod tests` at the bottom of the
same file. `app` has its own suite: `cargo test -p gitten-app`.

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Core | `cargo test -q -p gitten-core` | exit 0 |
| App | `cargo test -q -p gitten-app` | exit 0 |
| Terminal | `cargo test -q -p gitten-tui` | exit 0 |
| Desktop | `cargo test -q -p gitten-shell` | exit 0 |
| Whole workspace | `cargo test -q --workspace` | exit 0 |
| Lint | `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 |
| Format | `cargo fmt --all --check` | exit 0 |
| Everything headless | `./dev check` | prints `✓ all green`, exit 0 |

**Never launch a client.** Use `./dev check` and the `dump` example; a window
appearing unannounced interrupts whoever is at the keyboard.

## Scope

**In scope**:
- `core/src/refs.rs` — receives `Target`
- `shell/src/views/branches.rs`, `tui/src/branches.rs` — drop their copies, re-export
- `app/src/verbs.rs` or a new `app/src/act.rs` — the orchestration seam
- `shell/src/main.rs`, `tui/src/main.rs` — `delete_branch_selected` only
- `app/src/lib.rs` — one `mod` line if you add a module

**Out of scope** (do NOT touch):
- **Any verb other than `branches.delete`.** Not `checkout_branch`, not
  `stash_selected`, not one more however similar. See the executor note at the top.
- Any user-facing string. Every sentence above must appear, byte for byte, in the
  ported path. If the shared version reads better, **leave it** and say so in your
  report; a wording change here would hide a behaviour change inside a refactor.
- The arm/confirm state itself. `confirm_or_arm_delete` stays where it is, on each
  client's branches view — *when* to ask is shared policy, *what is currently
  armed* is view state.
- `core`'s dependency list.
- The keymap and command names.

## Git workflow

- Branch: `advisor/cx-066-one-verb-one-orchestration`
- Commit per step.
- Commit message style, from `git log`: lowercase, `scope: sentence`, e.g.
  `core,shell,tui: a branch verb's target is one type, not two`.
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: Baseline

```sh
cargo test -q --workspace 2>&1 | tee /tmp/066-before.txt
./dev check
```

**Verify**: both exit 0; `./dev check` prints `✓ all green`. Record per-crate test
counts. If not green, STOP.

### Step 2: Hoist `Target` into `core::refs`

Move the enum to `core/src/refs.rs`, keeping its doc comments verbatim — they are
good and they explain the refusals the verb depends on. Use `RefName` for the
name fields.

In both `shell/src/views/branches.rs` and `tui/src/branches.rs`, delete the local
declaration and re-export: `pub use gitten_core::refs::Target;` (matching each
file's existing visibility — the shell's is `pub(crate)`). Every existing use site
then compiles unchanged.

**Verify**: `cargo test -q --workspace` → exit 0, **same counts as Step 1**, and
`grep -rn "enum Target" shell/src tui/src` → no match.

### Step 3: Build the seam

Add the orchestration home in `app/` — a new `app/src/act.rs` is cleanest, since
`verbs.rs`'s doc says it is "the wrapper and nothing else". Define the three
capabilities a client owns:

```rust
//! What a verb *decides*, once, for every client.
//!
//! `verbs.rs` holds what a verb *does* — a `Write` over the repository handle.
//! This holds the part above it that was being written twice: which guards run
//! in which order, the words a refusal uses, and when a destructive question is
//! asked rather than answered. A client supplies three things it genuinely owns
//! — what is selected, how a sentence reaches the reader, and how a job is
//! queued — and nothing else about it is visible here.

/// The client's side of a verb. Drawing and input stay in the client; this is
/// the narrow window a shared verb reaches them through.
pub trait Acts {
    /// The branch row the keyboard is on, or `None` when the focused pane is
    /// not a branch list at all. The distinction between "wrong pane" and
    /// "nothing selected" is the client's, because only it knows what has focus.
    fn branch_target(&self) -> Option<Target>;
    /// A refusal or a result, in the client's own furniture.
    fn say(&mut self, message: String);
    /// A destructive question standing until it is answered or dropped.
    fn ask(&mut self, question: String);
    /// Arms this target, or spends an arm already standing on it. `false` means
    /// the question was just asked and nothing has happened yet.
    fn confirm_or_arm(&mut self, target: &Target) -> bool;
    /// The repository, absent when the client is showing a fixture.
    fn repo(&self) -> Option<Handle>;
    /// Queues the job. `false` means the queue is shutting down.
    fn submit(&mut self, job: Box<dyn Job>) -> bool;
}
```

Then the verb, once:

```rust
/// `branches.delete`, for every client.
///
/// The guard order is load-bearing and is why this is shared rather than
/// described: a detached HEAD is refused before a remote is, because "not a
/// branch" is a truer thing to say than "its remote's to delete"; and the arm
/// is spent only after the repository is known to exist, so a fixture cannot
/// consume a question it can never answer.
pub fn delete_branch(c: &mut impl Acts) {
    // ...the guards, in exactly the order the two copies share...
}
```

**Copy the sentences byte for byte** from the excerpt in "Current state",
including the em dash in `"a remote branch is its remote's to delete — fetch
prunes it here"`.

One difference between the two copies you must resolve and **report**: the window
refuses with `"branches.delete is not supported here"` when the focused pane is
not a branch list; the terminal routes that through
`self.branches_focused("branches.delete")`. Keep the window's wording — it is the
one that is literally a string in both trees — and note in your report whether the
terminal's message came out identical.

**Verify**: `cargo test -q -p gitten-app` → exit 0. Nothing calls the new function
yet.

### Step 4: Port the terminal, then the window

Implement `Acts` for the terminal's app struct and replace
`tui/src/main.rs:1792-1832`'s body with a call to `act::delete_branch(self)`.
Then the same for the window at `shell/src/main.rs:2919-2961`.

The window's implementation has one wrinkle worth expecting: its
`confirm_or_arm` and `branch_target` both need `&mut Context<Self>`, which the
trait's `&mut self` does not carry. Resolve it the way the codebase already does
elsewhere — read what is needed before the call and stash it, or thread the
context through a wrapper struct that borrows both. **If you cannot resolve it
without changing the trait's shape into something GPUI-specific, STOP and report**
— that is precisely the cost this tracer bullet exists to discover, and reporting
it is a successful outcome, not a failure.

Do the terminal first: it is the simpler client, and if the seam is wrong you find
out cheaply.

**Verify**: `cargo test -q --workspace` → exit 0, same counts as Step 1, plus
whatever new tests Step 5 adds.

### Step 5: Prove both clients still say the same things

**Verify**, all of these:
- `cargo fmt --all --check` → exit 0
- `cargo clippy --workspace --all-targets -- -D warnings` → exit 0
- `cargo test -q --workspace` → exit 0
- `./dev check` → `✓ all green`
- Each of the four sentences appears exactly **once** in production code.
  The `prod` helper stops at each file's first `#[cfg(test)]`, so test
  assertions — which legitimately quote these sentences, e.g.
  `tui/src/main.rs:8506` and `:8582` — are excluded. Counting the whole file
  instead gives 3 for two of these sentences and the check is meaningless:

  ```sh
  prod() { awk '/^#\[cfg\(test\)\]/{exit} {print}' "$1"; }
  for s in "nothing selected to delete" \
           "a detached HEAD is not a branch" \
           "fetch prunes it here" \
           "has no repository to delete branches from"; do
    n=0
    for f in shell/src/main.rs tui/src/main.rs app/src/act.rs app/src/verbs.rs core/src/refs.rs; do
      [ -f "$f" ] && n=$((n + $(prod "$f" | grep -c "$s")))
    done
    printf '%s: %s\n' "$s" "$n"
  done
  ```

  **Each reports `2` at `da9f8a7` — that is the duplication. Each must report
  `1` when you are done, and the surviving one must be in `app/`.** That is this
  plan's whole thesis, made checkable.

  The test assertions in `tui/src/main.rs` keep quoting the sentences and should
  keep passing; if one now needs its expected string edited, the port changed a
  message and that is a STOP condition.

### Step 6: Report, and stop

Write up, for the owner's decision on the remaining 24 verbs:

- What the seam cost: lines added in `app/`, lines removed from each client, and
  the net.
- The `&mut Context<Self>` wrinkle from Step 4 and how it resolved.
- Which of the remaining verbs look like they fit this trait unchanged, which
  would need a fourth or fifth capability on `Acts`, and which do not fit at all
  (the prompt-opening verbs are the ones to look hardest at — they suspend and
  resume rather than running to completion).
- Whether `Acts` should have been two traits.

**Then stop.** Do not port a second verb.

## Test plan

New tests in `app/src/act.rs`'s `mod tests`, over a **fake** `Acts`
implementation that records what it was told (a `Vec<String>` of `say`/`ask`
calls and a `Vec<String>` of submitted job names). Model it on the fake `Repo` in
`app/src/verbs.rs`'s tests (`app/src/verbs.rs:611` shows the pattern).

1. `deleting_with_nothing_selected_refuses_in_words` — `branch_target` returns
   `None`; assert the exact sentence and that no job was submitted.
2. `a_detached_head_is_refused_before_a_remote_is` — target `Detached`; assert the
   detached sentence, not the remote one. Pins the guard order the doc claims.
3. `a_remote_branch_is_refused` — target `Remote`; assert the remote sentence and
   no job.
4. `the_first_press_asks_and_the_second_deletes` — `confirm_or_arm` returns
   `false` then `true`; assert the first call produced a question and no job, and
   the second produced a job named for the branch and no further question.
5. `a_fixture_refuses_before_it_spends_the_arm` — `repo()` returns `None`; assert
   the fixture sentence **and** that `confirm_or_arm` was never called. This pins
   the ordering the shared doc comment claims and that both copies happen to
   share; it is the subtlest thing being unified.

These five tests are the real product of this plan: today this policy is tested
only through two clients' UI harnesses, if at all.

**Verification**: `cargo test -q -p gitten-app` → all pass, including 5 new tests.

## Done criteria

ALL must hold:

- [ ] `cargo fmt --all --check` exits 0
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` exits 0
- [ ] `cargo test -q --workspace` exits 0
- [ ] `./dev check` prints `✓ all green`
- [ ] `grep -rn "enum Target" shell/src tui/src` → no match
- [ ] Step 5's sentence loop reports `1` for all four sentences (it reports `2`
      at `da9f8a7`), and the surviving occurrence of each is in `app/`
- [ ] No test's expected message string was edited
- [ ] The five new `app` tests exist and pass
- [ ] **Exactly one verb was ported.** `git diff da9f8a7..HEAD -- shell/src/main.rs tui/src/main.rs`
      touches `delete_branch_selected` and nothing else
- [ ] `core/Cargo.toml` and `app/Cargo.toml` have no new dependency
- [ ] Step 6's report written

## STOP conditions

Stop and report back — do not improvise — if:

- The excerpts above do not match the live code (drift).
- **You have ported one verb and are considering a second.** Stop; that is the
  end of this plan.
- The `Acts` trait needs a GPUI type, a crossterm type, or any client-specific
  type to compile. The seam must be expressible without one; if it cannot be, that
  is the finding and the owner needs it before 24 more verbs depend on it.
- Any user-facing sentence would have to change. Report the wording problem
  instead — a string change inside a refactor is invisible in review.
- The window's `&mut Context<Self>` cannot be reconciled with `&mut self` without
  distorting the trait.
- `./dev check`'s terminal-frames section fails. It draws real frames and catches
  panics the unit tests do not reach.
- Test 5 (`a_fixture_refuses_before_it_spends_the_arm`) fails and you find the two
  clients actually disagree about that ordering today. That is a real behaviour
  difference the shared version must pick a side on, and the owner picks it.

## Maintenance notes

- **What a reviewer should scrutinize**: the guard order in
  `act::delete_branch`, against both original copies, line by line; and the
  sentence-count loop from Step 5.
- **What will interact with this**: every remaining duplicated verb. This plan's
  report is the input to deciding whether to port them, and in what batches.
- **Why one verb and not five**: five ports would settle the trait's shape by
  majority vote among verbs that happen to be similar. One port plus an honest
  report about the other 24 lets the owner choose. The verbs that will strain
  `Acts` are the prompt-opening ones — they do not run to completion, they suspend
  — and none of them is in this plan.
- **`Target` in `core::refs` is the durable half of this change.** Even if the
  `Acts` seam is rejected, the type belonging in `core` stands on its own: it is
  pure ref data, both clients need it, and `core/src/refs.rs`'s module doc already
  claims that territory.
