# Plan 043: The error band's summary survives an argv that contains `": "`

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving on. If a
> STOP condition occurs, stop and report. When done, update this plan's row in
> `plans/README.md`.
>
> **Drift check (run first)**: `git diff --stat 635aba8..HEAD -- git/src/lib.rs shell/src/main.rs`
> Written against `635aba8`. On a mismatch with the excerpts below, STOP.

## Status

- **Priority**: P2
- **Effort**: S–M
- **Risk**: LOW
- **Depends on**: 042 (both extend `GitError`'s construction in `shell/src/main.rs`; land 042 first)
- **Category**: bug
- **Planned at**: commit `635aba8`, 2026-08-31

## Why this matters

`GitError::new` derives the band's one-line summary by stripping `"git "` and
taking everything after the **first** `": "`. The acquisition layer's shape is
`git {args}: {stderr}` — and argv can contain `": "`: a hook declining
`git commit -m "wip: x"` yields full = `git commit -m wip: x: hook declined`
and summary = `x: hook declined` — a garbled non-sentence in the one line
most readers see.

## Current state

- `shell/src/main.rs:1087-1108`:

  ```rust
  impl GitError {
      fn new(full: impl Into<SharedString>) -> Self {
          let full = full.into();
          // The acquisition layer's shape is `git {args}: {stderr}` — strip that
          // prefix and the summary is git's first line, not the argv's. An
          // error that arrived by another road is already its own summary.
          let body = match full.strip_prefix("git ") {
              Some(rest) => match rest.find(": ") {
                  Some(at) => &rest[at + ": ".len()..],
                  None => rest,
              },
              None => full.as_ref(),
          };
          let summary = body
              .lines()
              .find(|line| !line.trim().is_empty())
              .unwrap_or(body);
          Self { summary: summary.into(), full }
      }
  }
  ```

- The producer: `git/src/lib.rs` formats errors as `format!("git {args}: {stderr}")`
  at the failure sites (~lines 91, 148, 151 — grep `format!("git ` to enumerate).

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Git crate tests | `cargo test -p gitten-git` | all pass |
| Window tests | `cargo test -p gitten-shell` | all pass |
| Lint/fmt | `cargo clippy --workspace -- -D warnings` / `cargo fmt --all -- --check` | exit 0 |

## Scope

**In scope**: `git/src/lib.rs` (carry argv and stderr separately at the
failure sites), `shell/src/main.rs` (`GitError::new` + its tests).

**Out of scope**: `core/` (no git knowledge there), the tui's error rendering
(it shares `GitError` — if it constructs one, adopt the new constructor; if it
formats its own string, leave it).

## Git workflow

- Branch: `advisor/ui-043-git-error-summary-split`, from `635aba8` (or 042's tip).
- Commit style: `shell,git: the error's summary is git's words, not the argv's`.

## Steps

### Step 1: Carry the parts at the producer

Add `GitError::from_parts(args: &str, stderr: impl Into<SharedString>) -> Self`
in `shell/src/main.rs` (next to `new`), summary = stderr's first non-empty
line — the same rule `new` applies to `body` today, applied to the part that
is actually stderr. Keep `new(full)` for errors that arrive by another road
(its doc says so), with its first-`": "` heuristic downgraded to `rfind` so a
`full` that *does* carry the acquisition shape splits at the last `": "`.

At `git/src/lib.rs`, change the failure sites from the single formatted string
to emitting the parts (a small struct or tuple through the existing error
channel — read how the error reaches the shell first; if the channel is a
`String`, a `Git parts` variant is the shape that does not widen the pipe).

**Verify**: `cargo test -p gitten-git -p gitten-shell` → all pass.

### Step 2: Tests

- In `shell/src/main.rs`'s `GitError` tests: `from_parts("commit -m \"wip: x\"",
  "hook declined")` → summary `hook declined`; the `rfind` fallback on
  `new("git commit -m wip: x: hook declined")` → same summary; a stderr with
  no lines → whole-stderr fallback (existing behaviour preserved).
- In `git/src/lib.rs`: one test per failure-site shape, asserting the parts
  survive intact.

**Verify**: `cargo test -p gitten-git -p gitten-shell` → all pass including the new tests.

## Test plan

As named in Step 2. Existing `GitError` tests extend naturally; none may be
deleted.

## Done criteria

- [ ] `cargo test -p gitten-git -p gitten-shell` exits 0 with the new tests
- [ ] `grep -n 'rest.find(": ")' shell/src/main.rs` finds nothing (rfind or gone)
- [ ] clippy `-D warnings` + fmt clean
- [ ] No files outside the in-scope list modified
- [ ] `plans/README.md` status row updated

## STOP conditions

- The error channel between `git/` and `shell/` is not a plain string (report
  its actual shape; the parts-carrying fix may already exist upstream).
- More than ~5 `format!("git ` sites exist (the shape differs per site — report).

## Maintenance notes

- Any new git failure site must use `from_parts`, not format-then-parse; the
  reviewer greps `format!("git ` in `git/src/lib.rs`.
