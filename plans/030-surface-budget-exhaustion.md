# Plan 030: Surface budget exhaustion, so the checker can hold myers exact again

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving on. If a
> STOP condition occurs, stop and report. Do not update `plans/README.md` —
> the reviewer maintains it.
>
> **Base: `full/full` (`d53a0c7`).**
> **Drift check**: `git diff --stat d53a0c7..HEAD -- core/src/differ.rs git/examples/diffcheck.rs`

## Status

- **Priority**: P3
- **Effort**: M
- **Risk**: MED (public type change in `core`)
- **Depends on**: plan 023 (which creates the gate this restores coverage to)
- **Category**: tests / tech-debt
- **Planned at**: `d53a0c7` (`full/full`), 2026-08-31

## Why this matters

Plan 023 turns `diffcheck` into a gate, and had to drop one assertion to do
it: myers' exact changed-line count. The reason is real and measured — myers
is minimal only *within* its step budget, and past that the differ degrades
by design (AGENTS.md: *"Bound them and degrade to 'this region was replaced'"*).

Measured on `full/full` at `d53a0c7`, whole history in one diff:

```
myers  +83270 -579  198h │ git --minimal +83265 -574 198h  +10 of 83839
```

All of it is one file, `shell/src/main.rs` (9,383 changed lines vs git's
9,373). Raising `MAX_STEPS` (`core/src/differ.rs:161`, currently `40_000_000`)
by 100× makes myers agree with git **exactly** — `+83265/-574` both sides.
Myers is O(N·D); ~9.4k differing lines needs ~88M steps. Confirmed not to be a
`full/full` regression: main's differ produces the identical numbers on the
same input.

So the assertion is unsatisfiable *when the budget binds* — and perfectly
satisfiable when it does not, which is the overwhelmingly common case. Today
the checker cannot tell the two apart, so plan 023 had to give up the check
entirely. If the differ simply *said* when it gave up, the checker could hold
myers exact everywhere the differ actually finished, and skip only the cases
where exactness is impossible. That is strictly better coverage than either
extreme.

## Current state

- `core/src/differ.rs:161` — `pub const MAX_STEPS: usize = 40_000_000;`
- `core/src/differ.rs:~398-402` — `fn spend(&mut self, n: usize) -> bool`
  charges the budget and reports whether it survived. Its `false` return is
  the exact moment the answer stops being guaranteed minimal.
- `core/src/differ.rs:363` — `fn begin_file(&mut self)` resets per-file state,
  including the budget. So "did the budget bind" is a per-file question.
- Callers of `spend` treat `false` as "degrade": see the comments around
  `differ.rs:444` ("Out of budget, or a split that would have recursed on the
  ...") and `:550` and `:591`.
- `core/src/differ.rs:2170` — `an_exhausted_budget_degrades_to_a_replace_instead_of_stalling`
  already drives `Ctx` with a deliberately tiny budget; it is the natural
  place to assert the new flag, and the pattern to copy.
- `git/examples/diffcheck.rs` — after plan 023, the myers arm of `verdict` is
  informational. This plan makes it conditional instead.

## Scope

**In scope**:
- `core/src/differ.rs` — record and expose per-file budget exhaustion
- whichever public type carries a per-file diff result out of `core` (likely
  `FileDiff` in `core/src/lib.rs` — find it with `grep -rn 'pub struct FileDiff' core/src`)
- `git/examples/diffcheck.rs` — consume the flag
- any call site the compiler names as a result of the struct gaining a field

**Out of scope**:
- Changing `MAX_STEPS` itself. The bound is deliberate; this plan makes it
  *legible*, not larger.
- The histogram/patience/whitespace verdicts — untouched, still exact.
- Any behaviour change to the diff output itself. This plan adds a report
  channel and nothing else; every existing test must pass unchanged.

## Steps

### Step 1: record exhaustion in `Ctx`

Add a `budget_spent: bool` (name it in the file's voice) to `Ctx`, set it
where `spend` returns `false`, and clear it in `begin_file`. Nothing else
reads it yet.

**Verify**: `cargo test -q -p gitten-core` → exit 0, unchanged pass count.

### Step 2: carry it out of `core` on the per-file result

Add a public, documented field — e.g. `degraded: bool` — to the per-file diff
type, set from `Ctx` when the file's diff is assembled. Doc comment states
plainly: *this file's script is not guaranteed minimal; the differ hit its
step budget and degraded, which is by design.*

The compiler will name every construction site. This is a public type in a
zero-dependency crate that three clients build on, so expect a handful.

**Verify**: `cargo test -q --workspace` → exit 0. `cargo clippy -q --workspace --all-targets -- -D warnings` → exit 0.

### Step 3: assert it where the budget is already exercised

Extend `an_exhausted_budget_degrades_to_a_replace_instead_of_stalling`
(`core/src/differ.rs:2170`) — or add a sibling — to assert the flag is set
when the tiny budget is exhausted, and **not** set on an ordinary small diff.
Both directions, or the flag proves nothing.

**Verify**: `cargo test -q -p gitten-core` → exit 0 including both assertions.

### Step 4: hold myers exact again, except where it cannot be

In `git/examples/diffcheck.rs`, restore myers' zero-tolerance count check —
but skip it (informational, as plan 023 left it) for any run in which some
file came back `degraded`. Report which file degraded, so a reader sees why
the strict check stood down rather than silently losing it.

**Verify**, reporting each:
1. `cargo run -q -p gitten-git --example diffcheck --release . HEAD~4..HEAD; echo $?` → `0`, myers held exact (nothing degraded at this size).
2. `cargo run -q -p gitten-git --example diffcheck --release . "$(git rev-list --max-parents=0 HEAD | tail -1)..HEAD"; echo $?` → `0`, myers reported as skipped-because-degraded, naming `shell/src/main.rs`.
3. **The restored check really bites**: inject a fault that changes myers' count on a *small* range where nothing degrades (e.g. drop one edit from the myers path in the checker's own accounting), confirm `exit=1`, revert, confirm `0`.
4. `./check.sh; echo $?` → `0`.

## Done criteria

- [ ] `grep -n 'degraded' core/src/differ.rs git/examples/diffcheck.rs` shows the flag set, carried and consumed
- [ ] `cargo test -q --workspace` exits 0
- [ ] Both directions of the Step 3 assertion exist
- [ ] Step 4's items 1–4 all report as specified, including the fault injection
- [ ] `cargo fmt --check` and clippy clean

## STOP conditions

- Adding the field to the public per-file type ripples into more than ~10
  construction sites, or forces a change to a client's rendering logic.
- Any existing differ test changes its expected output — this plan must not
  alter a single diff.
- The flag turns out to be set on ordinary small diffs (would mean `spend`
  returns `false` on paths that are not degradation).

## Maintenance notes

- Once this lands, plan 023's Step 1c comment should be updated to point here
  rather than describing a permanent exemption.
- A natural follow-on: surface `degraded` in the clients' stats readout the
  way wrap rejections and (after plan 024) span rejections are — a file whose
  diff silently gave up currently looks identical to one that did not.
