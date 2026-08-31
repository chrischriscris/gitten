# Plan 054: Branches in recency order, HEAD pinned first

> Adopted from draft `hp-04` (authored by session full-44 against
> `635aba8`); renumbered into this pass. Note: the staged design pass touches
> `shell/src/views/branches.rs`, so the drift check will report drift there —
> the in-scope change is in `git/src/lib.rs`, which the pass does not touch.
> Interaction with plan 047: the branch-dot colour is keyed by *list index*
> today, so this plan's re-ordering makes 047 (colour keyed by name, not
> index) more urgent; land in either order, but both should land.

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving on. On
> any STOP condition, stop and report — do not improvise. When done, update
> this plan's row in `plans/high-priority/README.md`.
>
> **Drift check (run first)**:
> `git diff --stat 635aba8..HEAD -- git/src/lib.rs shell/src/views/branches.rs core/`
> On any in-scope drift, compare the "Current state" excerpts against the
> live code; on a mismatch, STOP.
>
> **Base**: the commit the operator names (the design pass, once committed).
> Branch: `git switch -c advisor/ui-054-branch-recency <base>`. Line numbers
> are against `635aba8`; match on quoted content.
>
> **Shared ground rules**: see `plans/high-priority/README.md`.

## Status

- **Priority**: P1
- **Effort**: S

## Why this matters

`branches()` lists `refs/heads` with **no `--sort`**, so git hands them back
in refname order — alphabetical. Observed live: eleven `worktree-agent-*`
branches and four `advisor/*` branches stacked *above* `main` and the
checked-out branch, in a pane whose whole job is "which branch do I want
next". Alphabetical order rewards whoever names branches earliest in the
alphabet; every git UI that gets this right (lazygit included) sorts by
recency and pins HEAD's branch first, because "the branch I was just on" and
"the branch I'm on" are the two most likely answers.

This is a data-level fix in the acquisition layer — one flag plus one pin —
which is exactly where it belongs: every client (window, terminal, browser)
gets the order for free, and no client re-sorts.

## Current state

- The invocation, `git/src/lib.rs:1206-1211`:

  ```rust
  let raw = run(
      &self.root,
      &[
          "for-each-ref",
          &format!("--format={BRANCH_FORMAT}"),
          "refs/heads",
      ],
  )?;
  ```

  followed by `parse_branches(&raw)` into `Vec<Branch>` in encounter order.
- The trait doc (`git/src/lib.rs:358-360`): "The local branches, with HEAD's
  position and each branch's upstream" — HEAD's position is already part of
  the read model, so the pin needs no new acquisition.
- The branches view consumes the `Vec` in order (`shell/src/views/branches.rs`).

## Scope

**In scope**: `git/src/lib.rs` (the `for-each-ref` invocation, the pin, the
trait doc line), the fake `Repo`'s branches fixture if order-sensitive
tests exist, tests.

**Out of scope**: grouping/indenting by prefix (`advisor/`,
`worktree-agent/`) — a view-side idea, separately planned if wanted; remote
branches; any view change at all (the fix is upstream of the views);
`--sort` on any other listing.

## Git workflow

Branch `advisor/ui-054-branch-recency` from the operator-named base.

## Steps

### Step 1: Sort by committer date

Add `"--sort=-committerdate"` to the `for-each-ref` argument list. Git owns
the sort — no comparator lands in our code, the same reason writes shell
out ("don't reimplement; you will get it subtly wrong"). Note in the doc
comment that ties (same commit on two branches) fall back to git's
secondary refname order, which is deterministic.

**Verify**: `cargo test -p gitten-git` green.

### Step 2: Pin HEAD's branch first

After `parse_branches`, stable-partition the checked-out branch (the one
whose head matches HEAD's position, already in the read model) to index 0.
Stable, so the recency order of everything else is untouched. Detached HEAD
pins nothing. Update the trait doc on `branches()` to state the contract:
"most recently committed first; the checked-out branch first of all."

**Verify**: a test against the fake with three branches where the
checked-out one is neither newest nor first alphabetically — asserts pin +
recency of the rest. If any existing test pinned alphabetical order, update
it to the new contract and say so in the report (that test was pinning the
bug).

### Step 3: The seam check and the gate

`grep -rn "sort" shell/src/views/branches.rs cli/ web/ 2>/dev/null` — no
client may now re-sort; if one does, that is a bug under the one-implementation
rule: remove it and note it. Then `./dev check`.

## Test plan

- gitten-git: order contract test (Step 2), detached-HEAD case pins nothing.
- Whole-workspace `./dev check` green.

## Done criteria

- `branches()` documents and delivers: checked-out first, then most recent
  commit first.
- No client sorts branch lists.
- The commit message states the observed failure (agent-branch flood above
  `main`) so the log carries the why.

## STOP conditions

- Something downstream keys on branch *index* rather than name (a cursor
  restored by position across refresh would now jump) — report where before
  changing it.
- A consumer (the commits pane's branch decoration, a picker) turns out to
  require refname order — report it; do not maintain two orders.
- Drift check fails.
