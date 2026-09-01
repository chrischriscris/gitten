# Plan 064: Stop spawning `rev-parse --git-path` on every in-progress check

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/high-priority/README.md`.
>
> **Drift check (run first)**: `git diff --stat da9f8a7..HEAD -- git/src/lib.rs`
> If `git/src/lib.rs` changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> structural mismatch, treat it as a STOP condition.

## Status

- **Priority**: P3
- **Effort**: S
- **Risk**: LOW
- **Depends on**: none
- **Category**: perf
- **Planned at**: commit `da9f8a7`, 2026-08-31

## Why this matters

`git_state_exists` answers "does git's sequencing state directory `<name>` exist"
by **spawning a `git` process every time it is asked**:

```rust
    fn git_state_exists(&self, name: &str) -> bool {
        let Ok(raw) = run(&self.root, &["rev-parse", "--git-path", name]) else {
```

Its two callers each ask twice:

```rust
    fn rebase_in_progress(&self) -> bool {
        ["rebase-merge", "rebase-apply"].iter().any(|state| self.git_state_exists(state))
    }
    fn cherry_pick_in_progress(&self) -> bool {
        ["CHERRY_PICK_HEAD", "sequencer"].iter().any(|state| self.git_state_exists(state))
    }
```

So "is a rebase in progress?" costs up to two process spawns, and every
`cherry_pick` and `rebase_todo` asks before it starts. A spawn is milliseconds on
macOS; a `stat` is microseconds.

**The path is invariant and the answer is not.** `rev-parse --git-path <name>`
resolves *where* the state file would be — a property of the repository layout,
fixed for the life of a `Handle`. Whether the file is *there* changes constantly,
which is the whole question. Today both are recomputed together, so the
invariant half is paid over and over.

This is small today because both callers are pre-verb guards that run on a
keypress. It stops being small the moment the in-progress state is drawn — an
open roadmap item (`plans/README.md` records "In-progress rebase/cherry-pick
invisible … in-progress state still renders nowhere"). At that point this is four
spawns on **every refresh**. Fixing it now is a few lines; fixing it later is a
few lines plus a performance bug report.

## Current state

**File**: `git/src/lib.rs` — the acquisition layer, the only crate that talks to a
repository. Reads spawn the `git` binary today; that is deliberate and this plan
does **not** change it. It removes *repeated* spawns for an answer that cannot
have changed.

`git/src/lib.rs:935-941` — the type behind the handle:

```rust
/// The shipped implementation: the `git` binary.
///
/// Private on purpose. It is *an* answer to [`Repo`], not the surface; the day
/// gix takes over the reads, this type dies or shrinks and nothing outside the
/// crate notices.
struct Binary {
    root: PathBuf,
}
```

`git/src/lib.rs:921` — `pub type Handle = Arc<dyn Repo>;`. **`Binary` lives behind
an `Arc` and every method takes `&self`**, so a cache needs interior mutability
and must be `Send + Sync`.

`git/src/lib.rs:1886-1911` — the function to fix, in full:

```rust
    /// Whether one of git's sequencing state directories exists —
    /// `rebase-merge` for the modern interactive rebase, `rebase-apply` for
    /// `am`-shaped ones and older git's fallback. Resolved through
    /// `--git-path`, which is what makes a linked worktree answer about its
    /// own state directory instead of the main `.git`.
    fn git_state_exists(&self, name: &str) -> bool {
        let Ok(raw) = run(&self.root, &["rev-parse", "--git-path", name]) else {
            return false;
        };
        let shown = trimmed(&raw);
        if shown.is_empty() {
            return false;
        }
        use std::os::unix::ffi::OsStrExt;
        let at = Path::new(std::ffi::OsStr::from_bytes(shown));
        match at.is_absolute() {
            true => at.exists(),
            // Relative answers are relative to where git was pointed.
            false => self.root.join(at).exists(),
        }
    }
```

`git/src/lib.rs:1706-1711`, `1741-1746` — the two callers, quoted above.

**The four names ever passed** are `"rebase-merge"`, `"rebase-apply"`,
`"CHERRY_PICK_HEAD"` and `"sequencer"` — all `&'static str` literals in the two
callers. Confirm with
`grep -n "git_state_exists(" git/src/lib.rs` before you rely on it.

**Repo conventions**: `git/` may use dependencies, but **prefer `std`** — the
crate deliberately has no platform crates (`AGENTS.md`: "There is not one
`cfg(target_os)` in the tree, no `objc`, `cocoa` or `core-foundation`"). The
existing `use std::os::unix::ffi::OsStrExt` above is the closest thing to a
platform assumption in this function and it stays as it is. Tests build **real
scratch repositories** — see `git/src/lib.rs:7166`, `7281`, `7358` for the
pattern, all of which already call `rebase_in_progress()`.

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Git crate tests | `cargo test -q -p gitten-git` | exit 0, all pass |
| Whole workspace | `cargo test -q --workspace` | exit 0 |
| Lint | `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 |
| Format | `cargo fmt --all --check` | exit 0 |

## Scope

**In scope**:
- `git/src/lib.rs` — `Binary`, `git_state_exists`, and new tests

**Out of scope** (do NOT touch):
- `run()` and the other spawn helpers. Reads going through the `git` binary is a
  deliberate, documented design (`AGENTS.md`, `docs/roadmap.md`); this plan does
  not start a gix port.
- `rebase_in_progress` / `cherry_pick_in_progress`'s **logic** — the names they
  check and the `any()` short-circuit stay exactly as they are.
- Caching the *existence* answer. Only the resolved **path** is memoized. Getting
  this backwards would make the app blind to a rebase that started a second ago.
- Any other `rev-parse` call site. There are seven in the file; the other five
  answer questions whose results genuinely change (`HEAD`, `--abbrev-ref HEAD`,
  `stash@{0}`).

## Git workflow

- Branch: `advisor/perf-064-memoize-the-git-path-lookup`
- Commit message style, from `git log`: lowercase, `scope: sentence`, e.g.
  `git: where the state file would be is asked once, not every time`.
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: Add the memo to `Binary`

Give `Binary` a cache from state name to resolved path. It must be `Send + Sync`
because `Handle` is `Arc<dyn Repo>` and the job runner uses it across threads.
Use `std` only:

```rust
struct Binary {
    root: PathBuf,
    /// Where each of git's sequencing state files *would* be, resolved once.
    ///
    /// `rev-parse --git-path` answers a question about the repository's layout,
    /// which does not move for the life of a handle — a linked worktree's own
    /// state directory is still its own an hour later. Whether the file is
    /// *there* is the question the callers actually ask, and that is a `stat`
    /// on every call, below. Resolving both together meant a process spawn to
    /// re-learn something that could not have changed.
    ///
    /// A `Mutex<Vec<..>>` and not a map: the whole domain is four static names,
    /// so a linear scan of at most four entries is cheaper than hashing one.
    state_paths: std::sync::Mutex<Vec<(String, Option<PathBuf>)>>,
}
```

`None` in the value position memoizes a *failed* resolution — a repository where
`rev-parse` errors or answers empty. Without that, the failing path re-spawns
forever, which is the case that most deserves the cache.

Update `open()` (`git/src/lib.rs:929-933`) to initialise it with
`Default::default()`.

**Verify**: `cargo test -q -p gitten-git` → exit 0 (nothing uses the field yet).

### Step 2: Split resolution from existence

Rewrite `git_state_exists` as two parts. **The split is the whole plan — keep the
resolution logic byte-for-byte identical and only change when it runs.**

```rust
    /// Where git would keep the state file called `name`, resolved once per
    /// handle and remembered. `None` when this repository cannot answer.
    fn git_state_path(&self, name: &str) -> Option<PathBuf> {
        let mut memo = self
            .state_paths
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((_, at)) = memo.iter().find(|(k, _)| k == name) {
            return at.clone();
        }
        // ...exactly today's body, up to producing an absolute PathBuf...
        let at = /* Some(absolute path) or None */;
        memo.push((name.to_string(), at.clone()));
        at
    }

    fn git_state_exists(&self, name: &str) -> bool {
        // The path cannot move; whether anything is at it is the question, and
        // that is asked afresh every time — a rebase that started a moment ago
        // must be visible to the very next call.
        self.git_state_path(name).is_some_and(|at| at.exists())
    }
```

Two details that are easy to get wrong:

1. **Resolve the relative case *before* memoizing.** Today the
   `at.is_absolute()` branch joins `self.root` at the point of the `exists()`
   call. Do that join inside `git_state_path` so the memo always holds an
   absolute path, and `git_state_exists` is a single `.exists()`.
2. **`unwrap_or_else(PoisonError::into_inner)`** on the lock, matching how the
   rest of the tree handles a poisoned mutex — see
   `core/src/differ.rs:223-225`. A panic elsewhere must not make the repository
   permanently unreadable.

**Verify**: `cargo test -q -p gitten-git` → exit 0, and every existing
`rebase_in_progress()` assertion (`git/src/lib.rs:7166`, `7185`, `7281`, `7328`,
`7358`) passes unmodified.

### Step 3: Gates

**Verify**, all of these:
- `cargo fmt --all --check` → exit 0
- `cargo clippy --workspace --all-targets -- -D warnings` → exit 0
- `cargo test -q --workspace` → exit 0

## Test plan

New tests in `git/src/lib.rs`'s `#[cfg(test)] mod tests`, following the scratch-
repository pattern the existing rebase tests use:

1. `a_started_rebase_is_seen_after_the_path_was_already_resolved` — the
   regression test for the one thing this plan could break. In a scratch repo:
   call `rebase_in_progress()` (false — this populates the memo), then actually
   stop a rebase mid-flight the way `git/src/lib.rs:7358`'s existing test does,
   then call `rebase_in_progress()` again and assert it is now **true**. If the
   existence answer were cached, this fails.
2. `a_finished_rebase_stops_being_seen` — the same in reverse: in progress, then
   abort, then assert false. Covers the other direction of a stale cache.
3. `the_state_path_is_resolved_once` — assert memoization actually happens.
   Simplest honest form: call `git_state_exists("rebase-merge")` twice and assert
   the memo vector has exactly one entry for that name afterwards (the test is in
   the same module, so it can reach the private field).
4. `an_unresolvable_state_name_is_remembered_as_unresolvable` — call
   `git_state_exists` with a name in a directory that is **not** a repository,
   assert `false` both times, and assert the memo recorded a `None` entry rather
   than staying empty.

**A linked worktree is the case the original doc comment calls out.** If you can
create one cheaply in a scratch repo (`git worktree add`), add:

5. `a_linked_worktree_answers_about_its_own_state` — resolve from the linked
   worktree and assert the memoized path is under the worktree's own git dir, not
   the main `.git`. If `git worktree add` is impractical in the test harness, say
   so in your report rather than skipping it silently.

**Verification**: `cargo test -q -p gitten-git` → all pass, including the new tests.

## Done criteria

ALL must hold:

- [ ] `cargo fmt --all --check` exits 0
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` exits 0
- [ ] `cargo test -q --workspace` exits 0
- [ ] Tests 1–4 exist and pass; test 5 exists or its absence is explained
- [ ] `grep -n "rev-parse\", \"--git-path" git/src/lib.rs` shows exactly one
      occurrence, inside `git_state_path`
- [ ] `rebase_in_progress` and `cherry_pick_in_progress` bodies are unchanged
      (`git diff da9f8a7..HEAD -- git/src/lib.rs` shows no edit inside them)
- [ ] `git status --porcelain` lists no modified file other than `git/src/lib.rs`
      and this plan's status row

## STOP conditions

Stop and report back — do not improvise — if:

- The excerpts above do not match the live code (drift).
- Test 1 or 2 fails. That means existence is being cached along with the path,
  which makes the app blind to state changes — the one bug this plan must not
  introduce. Do not weaken the test.
- You conclude the memo needs to be invalidated (by a generation counter, a
  refresh hook, anything). It does not: the *path* is invariant for a handle's
  life. If you believe otherwise, report the case rather than building an
  invalidation mechanism.
- Making `Binary` hold the cache breaks `Send`/`Sync` for `Handle`. It should not
  with a `Mutex`; if it does, report the error rather than reaching for `unsafe`.
- You are tempted to replace `--git-path` with `--git-dir` plus a `join`. It looks
  equivalent and is not, for paths git special-cases; this plan deliberately
  memoizes git's own answer rather than reimplementing its resolution.

## Maintenance notes

- **What a reviewer should scrutinize**: that only the path is memoized, and that
  `.exists()` is still called on every request. Test 1 is the guard, and it is
  the review's whole burden.
- **What will interact with this**: the open "in-progress rebase/cherry-pick is
  invisible" item. When the UI starts drawing that state, these calls move onto
  the refresh path — this plan is what makes that affordable. Whoever builds it
  should also check whether the `.exists()` stat wants batching at that point.
- **Deliberately deferred**: the other five `rev-parse` spawns. They answer
  questions whose results change (`HEAD` moves), so memoizing them would be a
  correctness bug, not an optimisation.
