# Plan 002: A revspec cannot be smuggled to git as an option

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving on. If a
> "STOP conditions" item occurs, stop and report. When done, update this plan's
> row in `plans/README.md`.
>
> **Drift check (run first)**: `git diff --stat 3a8b347..HEAD -- git/src/lib.rs`
> If `git/src/lib.rs` changed, compare the "Current state" excerpts against the
> live code before proceeding; on a mismatch, STOP.

## Status

- **Priority**: P1
- **Effort**: S
- **Risk**: LOW
- **Depends on**: none
- **Category**: security
- **Planned at**: commit `3a8b347`, 2026-08-24

## Why this matters

The user-supplied revspec is appended to the `git diff` / `git show` argument
vector as a bare positional, with no `--` or `--end-of-options` separator. A
revspec that begins with `-` is therefore parsed by git as an **option**, not a
revision:

- `--output=<path>` makes `git diff` write its output to an arbitrary file
  (arbitrary file write / clobber).
- `--ext-diff` re-enables external diff drivers — which is exactly what the
  adjacent `--no-ext-diff` (added on purpose) exists to suppress. That reopens
  `diff.<driver>.command` from the repository's own config as a code-execution
  surface, widening the deliberate "shell out to git" design rather than honoring
  it.

Today the revspec comes from argv, so the realistic vector is a wrapper (an
editor plugin, a script, a shell alias) forwarding an untrusted string. The
defense is one token, and the `-C <repo>` argument already gets this right by
construction. This is cheap insurance on the one place the git boundary can be
widened by input.

## Current state

`git/src/lib.rs`, inside `each_pair` (the argument construction, around lines
183-193):

```rust
const RAW: [&str; 5] = ["--raw", "-z", "-M", "--abbrev=64", "--no-ext-diff"];
let raw = if revspec.is_empty() {
    run(repo, &[&["diff"], &RAW[..], &["HEAD"]].concat())?
} else if revspec.contains("..") {
    run(repo, &[&["diff"], &RAW[..], &[revspec]].concat())?     // <-- bare positional
} else {
    // A bare revision means "what did this commit change".
    run(
        repo,
        &[&["show"], &RAW[..], &["--format=", revspec]].concat(),  // <-- bare positional
    )?
};
```

`--end-of-options` (git ≥ 2.24) tells git that everything after it is a
positional, even if it starts with `-`. It is the correct fix here; a trailing
`--` would **not** work, because git parses options before the `--` path
separator.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Test this crate | `cargo test -q -p gitten-git` | `test result: ok` |
| Lint | `cargo clippy -p gitten-git --all-targets` | no warnings |
| Format check | `cargo fmt --check` | clean |

## Scope

**In scope**:
- `git/src/lib.rs` — the two argument vectors in `each_pair`, and a test.

**Out of scope**:
- `app/src/cli.rs` — do not add revspec validation there; the fix belongs at the
  git boundary so every caller (CLI, future clients, tests) is covered.
- The `--format=` / `-M` / `--abbrev=64` flags — leave them exactly as they are.

## Git workflow

- Branch: `advisor/002-revspec-option-termination`
- One commit, imperative message (e.g. "Stop a revspec from being read as a git
  option"). Match the repo's log style.

## Steps

### Step 1: Insert `--end-of-options` before the revspec in both arms

In the `revspec.contains("..")` arm:

```rust
run(repo, &[&["diff"], &RAW[..], &["--end-of-options", revspec]].concat())?
```

In the bare-revision arm — note git's option parsing means the separator must
come before the positional revspec, and `--format=` is fine before it:

```rust
run(
    repo,
    &[&["show"], &RAW[..], &["--format=", "--end-of-options", revspec]].concat(),
)?
```

Leave the empty-revspec arm (`&["HEAD"]`) as-is — `HEAD` is a constant, not user
input.

**Verify**: `cargo build -p gitten-git` → exit 0.

### Step 2: Confirm normal revspecs still work

**Verify**: `cargo test -q -p gitten-git` → all existing tests still pass (they
exercise real revspecs like `HEAD~1` and `A..B` — `--end-of-options` must not
change their behavior).

### Step 3: Add a regression test

In the `#[cfg(test)] mod tests` block, add a test that builds a scratch repo and
calls the diff entry point with a hostile revspec, asserting it does **not**
write the target file. Model it on the existing scratch-repo tests. Concretely:

- Pick a path under the test's temp dir that must not exist afterward, e.g.
  `repo.join("PWNED")`.
- Call the diff entry point (`diff(&repo, "--output=<that path>", ...)` or
  `pairs(&repo, "--output=<that path>")` — use whichever the other tests use)
  with the crafted revspec.
- Assert the target file does **not** exist after the call (the call may return
  an error or an empty diff — either is acceptable; the file being written is the
  failure).

**Verify**: `cargo test -q -p gitten-git` → new test passes. If you run the test
against the **unpatched** code first (optional, to confirm it catches the bug),
`--output=` would create the file and the assertion would fail.

## Test plan

- New test in `git/src/lib.rs` tests module: a `--output=<temp>`-prefixed revspec
  does not create the target file.
- Optional second assertion: a well-formed `HEAD~1..HEAD` still yields the
  expected pairs (proves `--end-of-options` didn't break normal use). An existing
  test may already cover this — if so, don't duplicate.
- Verification: `cargo test -q -p gitten-git` → all pass.

## Done criteria

ALL must hold:

- [ ] `cargo test -q -p gitten-git` exits 0; the `--output=` regression test passes
- [ ] `grep -n "end-of-options" git/src/lib.rs` shows the separator in both diff arms
- [ ] Existing revspec tests unchanged and passing
- [ ] `cargo clippy -p gitten-git --all-targets` clean
- [ ] No files outside `git/src/lib.rs` modified
- [ ] `plans/README.md` status row updated

## STOP conditions

- The local `git --version` is below 2.24 (no `--end-of-options`) — report this;
  the fallback (reject a `-`-leading revspec in the git layer with a clear error)
  is the alternative but changes the approach.
- The argument vectors in `each_pair` don't match the excerpt — report the drift.

## Maintenance notes

- If a future client accepts a revspec from a network or file source (not argv),
  this guard becomes load-bearing rather than defense-in-depth — keep it.
- A reviewer should confirm `--end-of-options` is present in **both** the
  `diff` (`..`) and `show` (bare-rev) arms; the two are easy to fix asymmetrically.
