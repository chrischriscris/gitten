# Plan 007: The desktop crate's tests run in CI

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result. If a "STOP conditions"
> item occurs, stop and report. When done, update this plan's row in
> `plans/README.md`.
>
> **Drift check (run first)**: `git diff --stat 3a8b347..HEAD -- .github/workflows/check.yml`
> If it changed, re-read the current job definitions before editing.

## Status

- **Priority**: P2
- **Effort**: S–M
- **Risk**: MED (GPUI on a headless Linux runner is the unknown)
- **Depends on**: none
- **Category**: tests / dx
- **Planned at**: commit `3a8b347`, 2026-08-24

## Why this matters

The CI test job runs `-p gitten-core -p gitten-app -p gitten-git -p gitten-web
-p gitten-tui` — but **not** `gitten-shell`. The desktop crate holds the largest
single tested surface in the repo: ~112 tests across `shell/src/views/diff.rs`
(the 3.7k-line view), `markdown.rs`, `split.rs`, `session.rs`, and `controls.rs`,
covering reflow, order tables, and colours. Almost all are ordinary `#[test]`
functions (only two are `#[gpui::test]`). `./check.sh` runs them, so the two
"one command to know it works" gates disagree — a PR from a contributor without a
Mac gets a green check for code whose tests never executed.

The lint job **already** installs the fontconfig/xcb/wayland/vulkan set and runs
`cargo clippy --workspace --all-targets` on ubuntu, so every one of these tests
already *compiles* on the runner. Running them is one `-p` flag or one small job
away.

## Current state

`.github/workflows/check.yml`:

- `test` job (lines ~50-60), the run step:

  ```yaml
  - run: >-
      cargo test -q --locked
      -p gitten-core -p gitten-app -p gitten-git -p gitten-web -p gitten-tui
  ```

  It checks out with `fetch-depth: 3` (the git acquisition tests use this repo as
  their fixture and need HEAD~1) and uses `Swatinem/rust-cache@v2`. It does **not**
  install the Linux GPUI system libraries.

- `lint` job (lines 62-83) — installs the GPUI Linux deps (only on a cache miss)
  and runs `cargo clippy --workspace --all-targets --locked -- -D warnings`. This
  proves `gitten-shell`'s tests compile on ubuntu.

- `audit` job (lines 91-103) — advisory, `continue-on-error: true`.

The two `#[gpui::test]` functions are both in `shell/src/views/split.rs` (per the
audit); the rest are plain `#[test]`. GPUI on a headless ubuntu runner may need a
software-GL fallback (`LIBGL_ALWAYS_SOFTWARE=1`) or an `xvfb-run` wrapper for the
two `#[gpui::test]` cases — this is the unknown.

## Commands you will need

| Purpose | Command | Expected |
|---------|---------|----------|
| Run shell tests locally (Mac) | `cargo test -q -p gitten-shell` | `test result: ok` |
| Validate workflow YAML | `python3 -c "import yaml,sys; yaml.safe_load(open('.github/workflows/check.yml'))"` | no error, exit 0 |

## Scope

**In scope**:
- `.github/workflows/check.yml` — add a job (or extend one) that runs
  `gitten-shell`'s tests on ubuntu.

**Out of scope**:
- The tests themselves — do not modify `shell/src` test code except, if strictly
  necessary, to guard the two `#[gpui::test]` cases (see Step 3).
- `check.sh` — it already runs the shell tests; leave it.
- Promoting the `audit` job to a gate — unrelated.

## Git workflow

- Branch: `advisor/007-shell-tests-in-ci`
- One commit, e.g. "Run the desktop crate's tests in CI".

## Steps

### Step 1: Add a `test-shell` job that reuses the lint job's Linux setup

Add a third job modeled on the `lint` job (it is the one that already installs
the GPUI deps). It must: check out, restore the rust-cache, install the Linux
GPUI dependencies (the same `apt-get` block as `lint`, gated the same way on a
cache miss), then run the shell tests:

```yaml
  test-shell:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v5
        with:
          fetch-depth: 3
      - uses: Swatinem/rust-cache@v2
        id: cache
      - if: steps.cache.outputs.cache-hit != 'true'
        name: Install Linux build dependencies
        run: |
          sudo apt-get update
          sudo apt-get install -y \
            fontconfig libfontconfig1-dev \
            libxcb1-dev libxcb-util-dev \
            libxkbcommon-dev libxkbcommon-x11-0 \
            libwayland-dev libudev-dev libvulkan-dev
      - run: cargo test -q --locked -p gitten-shell
```

Match the existing indentation and the `env:` block at the top of the file
(`CARGO_TERM_COLOR`, the `*_DEBUG=0` vars) — those apply workflow-wide, so the
new job inherits them.

**Verify**: `python3 -c "import yaml; yaml.safe_load(open('.github/workflows/check.yml'))"`
exits 0.

### Step 2: Land it non-blocking first if GPUI on headless Linux is unproven

Because a headless GPUI test run on ubuntu is the unknown, add
`continue-on-error: true` to the `test-shell` job **initially**, with a comment
saying to remove it once the job is proven green (mirror the `audit` job's
comment style). This keeps `main` from going red on a first-time flake while the
signal is still established. Removing `continue-on-error` later is a one-line
follow-up.

If the executor cannot run GitHub Actions to observe the result, leave
`continue-on-error: true` in and note in the commit body that it should be
removed once observed green.

**Verify**: YAML still parses.

### Step 3: If the two `#[gpui::test]` cases fail headless, gate them

Only if a CI run (or a local `xvfb-run cargo test -p gitten-shell` on Linux, if
available) shows the two `#[gpui::test]` functions in `shell/src/views/split.rs`
failing for want of a display/GPU: add
`#[cfg_attr(not(target_os = "macos"), ignore)]` to those two functions and a
one-line comment explaining why (no headless GL on the runner). Do **not** ignore
the plain `#[test]` functions — they must run on Linux; that is the whole point.

**Verify**: `cargo test -q -p gitten-shell` on Mac still runs everything (the
`cfg_attr` only ignores on non-macOS).

## Test plan

- No new product tests. The deliverable is that `gitten-shell`'s existing tests
  execute in CI on ubuntu.
- Verification: the workflow parses; on a pushed branch, the `test-shell` job
  appears and runs `cargo test -p gitten-shell`.

## Done criteria

ALL must hold:

- [ ] `.github/workflows/check.yml` has a job that runs `cargo test ... -p gitten-shell` on ubuntu
- [ ] The workflow YAML parses (`yaml.safe_load` exits 0)
- [ ] The GPUI Linux dependencies are installed in that job (same set as `lint`)
- [ ] Plain `#[test]` functions in `shell/src` are NOT ignored on Linux
- [ ] If landed non-blocking, a comment says to remove `continue-on-error` once green
- [ ] No files outside `.github/workflows/check.yml` modified (unless Step 3's
      `cfg_attr` was needed, in which case only `shell/src/views/split.rs`)
- [ ] `plans/README.md` status row updated

## STOP conditions

- The shell tests fail on Linux for a reason other than a missing display/GPU
  (e.g. a genuine platform-specific assertion) — report the failure; do not
  paper over a real Linux bug with `ignore`.
- The lint job's dependency set does not actually let `gitten-shell` tests link
  (a missing lib at test time that clippy did not need) — report the linker error
  so the apt list can be extended deliberately.

## Maintenance notes

- Once the job is observed green, remove `continue-on-error` in a follow-up so it
  becomes a real gate.
- This unblocks DEBT-01 (moving `shell`'s duplicated `expand`/`assemble` into
  `core`) and Plan 006 (the prepare cache) — both rely on the shell tests
  actually running on every change, not just on a maintainer's Mac.
- Decision record `docs/decisions/0024-ci-is-two-jobs.md` says CI is two jobs; it
  is already three (the `audit` job). If you touch CI, appending a one-line
  "amended: now N jobs" note to that record keeps it honest (optional, out of the
  strict scope above).
