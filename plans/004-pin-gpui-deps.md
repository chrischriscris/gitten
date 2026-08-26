# Plan 004: The GPUI dependencies are pinned to a known-good revision

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving on. If a
> "STOP conditions" item occurs, stop and report. When done, update this plan's
> row in `plans/README.md`.
>
> **Drift check (run first)**: `git diff --stat 3a8b347..HEAD -- shell/Cargo.toml Cargo.lock`
> If either changed, re-read the current pinned revs from `Cargo.lock` (see Step
> 1) and use those instead of the ones quoted here.

## Status

- **Priority**: P1
- **Effort**: S
- **Risk**: LOW
- **Depends on**: none
- **Category**: migration (dependency hygiene)
- **Planned at**: commit `3a8b347`, 2026-08-24
- **OUTCOME (2026-08-24): REJECTED — the `rev`-pin approach is counterproductive
  for this graph.** Two executor runs proved it: (1) `gpui-component` (third
  party) depends on `gpui` **bare** (no rev); pinning our `gpui` with `?rev=`
  creates a second source variant of the entire zed tree (870 → 893 lock
  entries), so the same commit can compile twice — no lockfile command avoids
  this. (2) The `Cargo.lock` already pins the exact commits, so reproducibility
  holds today; the only real exposure is a careless bare `cargo update`. The
  proportionate mitigation is a process note (see below), not a manifest change.
  **Recommended replacement**: add one line to `AGENTS.md` near the
  `rust-toolchain.toml` guidance — "GPUI is pinned only by `Cargo.lock`; never
  run a bare `cargo update` (it floats the git deps to Zed's tip). Bump GPUI
  deliberately: `cargo update -p gpui -p gpui-component` + a matching
  `rust-toolchain.toml` bump, one commit." A scoped `cargo update -p <crate>` for
  an unrelated dep does NOT float the git deps, so day-to-day updates are safe.
  If a hard manifest-level pin is still wanted, the only mechanism that unifies
  the graph is a `[patch."https://github.com/zed-industries/zed"]` section, whose
  dedup behavior needs verifying — that is a new investigation plan, not this one.

## Why this matters

`shell/Cargo.toml` declares the four GPUI dependencies (plus a dev-dependency) as
bare `git = "…"` with no `rev`, `tag`, or `branch`. `Cargo.lock` currently pins
them to specific commits, so builds are reproducible **today** — but only until
the next `cargo update`. Any `cargo update` (even a well-meant `cargo update -p
anyhow`, which rewrites the whole lockfile's git sources) floats both `gpui` and
`gpui-component` to their default-branch tips, with no version constraint to stop
it. GPUI has no stability guarantee and its API churns; that lands as a wall of
compile errors, plus a possible `rust-toolchain.toml` bump, at a moment nobody
chose. `--locked` protects CI, not a developer's working tree.

Pinning to the currently-locked revs cannot break a build that works today, and
makes bumping GPUI a deliberate, reviewable one-line change.

## Current state

`shell/Cargo.toml`:

```toml
[dependencies]
gitten-app = { path = "../app" }
gitten-core = { path = "../core" }
gitten-git = { path = "../git" }
gpui = { git = "https://github.com/zed-industries/zed" }
gpui_platform = { git = "https://github.com/zed-industries/zed", features = ["font-kit"] }
gpui-component = { git = "https://github.com/longbridge/gpui-component" }
gpui-component-assets = { git = "https://github.com/longbridge/gpui-component" }
anyhow = "1.0"

[dev-dependencies]
gpui = { git = "https://github.com/zed-industries/zed", features = ["test-support"] }
```

Currently locked revisions (from `Cargo.lock` at `3a8b347`):
- `zed-industries/zed` → `00c0e96e769062e373203c62830f510fa121db76`
- `longbridge/gpui-component` → `9e3a29dcbdebc318632bf68203f26c33e9f0e902`

`rust-toolchain.toml:6` pins `1.97.1` with the comment "Must track the channel
Zed pins… Bump this when Zed bumps theirs" — that is a *second* coupling to the
same SHA. It is already pinned; this plan does not change it, but the maintenance
note records that the two move together.

## Commands you will need

| Purpose | Command | Expected |
|---------|---------|----------|
| Confirm locked revs | `grep 'zed-industries/zed#\|longbridge/gpui-component#' Cargo.lock \| sort -u` | prints the two SHAs above |
| Regenerate lock to match manifest (offline; git repos are already cached) | `cargo generate-lockfile --offline` | exit 0 |
| Resolve against the lock WITHOUT a full compile | `cargo metadata --locked --format-version 1 >/dev/null` | exit 0 (no "cannot update the lock file" error) |

**Important — a correction to a naive first attempt.** Adding `rev = "…"` to a
git dependency does **not** leave `Cargo.lock` byte-identical. Cargo rewrites the
source string for each affected crate from the bare `git+<url>#<sha>` form to
`git+<url>?rev=<sha>#<sha>`. The **resolved commit SHA is unchanged** — so the
build that works today still works — but the lockfile's source lines change.
That is expected and correct. Do **not** try to keep `Cargo.lock` unchanged;
commit the regenerated lock alongside the manifest. Do **not** run a full
`cargo build` to verify — that is a multi-minute cold GPUI compile; use
`cargo metadata --locked` to prove the graph resolves against the lock.

## Scope

**In scope**:
- `shell/Cargo.toml` — add `rev = "…"` to all five GPUI git declarations.
- `Cargo.lock` — will change (source-string rewrites only, same SHAs); commit
  the regenerated lock. This is expected, not a violation.

**Out of scope**:
- `rust-toolchain.toml` — leave it. It is already pinned.
- Any actual version bump of GPUI — that is a separate, deliberate change. The
  resolved SHAs must be identical before and after (verified in Step 3).

## Git workflow

- Branch: `advisor/004-pin-gpui-deps`
- One commit, e.g. "Pin the GPUI dependencies to their locked revs".

## Steps

### Step 1: Read the current locked revs (do not trust this file blindly)

```
grep 'zed-industries/zed#\|longbridge/gpui-component#' Cargo.lock | sort -u
```

Use the SHAs this prints. They should match the two in "Current state"; if the
tree drifted, use what the lockfile actually says.

### Step 2: Add `rev` to every GPUI git declaration

Edit `shell/Cargo.toml` so each GPUI dependency carries the matching `rev`:

```toml
gpui = { git = "https://github.com/zed-industries/zed", rev = "00c0e96e769062e373203c62830f510fa121db76" }
gpui_platform = { git = "https://github.com/zed-industries/zed", rev = "00c0e96e769062e373203c62830f510fa121db76", features = ["font-kit"] }
gpui-component = { git = "https://github.com/longbridge/gpui-component", rev = "9e3a29dcbdebc318632bf68203f26c33e9f0e902" }
gpui-component-assets = { git = "https://github.com/longbridge/gpui-component", rev = "9e3a29dcbdebc318632bf68203f26c33e9f0e902" }

[dev-dependencies]
gpui = { git = "https://github.com/zed-industries/zed", rev = "00c0e96e769062e373203c62830f510fa121db76", features = ["test-support"] }
```

(Use whatever SHAs Step 1 printed, if different.)

**Verify**: `cargo build -p gitten-shell` (no `--locked`) is NOT how you verify —
skip building. Just confirm the manifest edit is syntactically valid:
`cargo metadata --format-version 1 >/dev/null` exits 0 (this also regenerates
`Cargo.lock` to match the manifest — that is Step 3's regeneration, done here as
a side effect, which is fine).

### Step 3: Regenerate the lock and confirm the SHAs are unchanged

Regenerate `Cargo.lock` so it matches the manifest, then prove the resolved
commits did not move:

```
cargo generate-lockfile --offline
grep 'zed-industries/zed#\|longbridge/gpui-component#' Cargo.lock | grep -oE '#[a-f0-9]{40}' | sort -u
```

The `grep` must print exactly the two SHAs from Step 1 (`#00c0e96e…` and
`#9e3a29dc…`) and nothing else — the source strings now carry `?rev=…` but the
trailing `#<sha>` is unchanged. Then confirm the graph resolves against the lock
without wanting to change it:

```
cargo metadata --locked --format-version 1 >/dev/null
```

**Verify**: `cargo generate-lockfile --offline` exits 0; the SHA grep prints only
the two original SHAs; `cargo metadata --locked ...` exits 0 (no "cannot update
the lock file" error). If `--offline` fails because the git repos are not cached
in this environment, drop `--offline` (it will fetch); if there is no network
either, STOP and report — the lock cannot be regenerated here.

## Test plan

No new tests — this is a manifest + lockfile change. Verification is that the
graph resolves with `--locked` after regeneration and the resolved SHAs are
unchanged.

## Done criteria

ALL must hold:

- [ ] All five GPUI declarations in `shell/Cargo.toml` carry a `rev`
- [ ] The four GPUI SHAs in `Cargo.lock` are unchanged from Step 1 (SHA grep
      prints only `#00c0e96e…` and `#9e3a29dc…`)
- [ ] `cargo metadata --locked --format-version 1 >/dev/null` exits 0
- [ ] Only `shell/Cargo.toml` and `Cargo.lock` are modified (`git status`)
- [ ] Both files are committed together in one commit
- [ ] `plans/README.md` status row updated

## STOP conditions

- The SHA grep in Step 3 shows a **different** commit than Step 1 — the resolved
  version moved; STOP and report (something other than a source-string rewrite
  happened).
- `Cargo.lock` changes include crates unrelated to gpui/zed (a transitive bump) —
  STOP and report; regeneration should only rewrite the gpui source strings.
- The lock cannot be regenerated at all (no cached git repos and no network) —
  STOP and report; this environment cannot verify the plan.

## Maintenance notes

- Bumping GPUI is now: change the `rev` in `shell/Cargo.toml`, run
  `cargo update -p gpui -p gpui-component`, and bump `rust-toolchain.toml` to
  whatever channel the new Zed commit pins — **one deliberate commit**. Note that
  coupling in `AGENTS.md` near the `rust-toolchain.toml` guidance if it is not
  already stated.
- A reviewer should confirm the dev-dependency `gpui` got the same rev; it
  resolves separately and is easy to miss.
