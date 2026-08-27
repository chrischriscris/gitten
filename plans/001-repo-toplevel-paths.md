# Plan 001: Working-tree diffs are correct from any subdirectory of a repo

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan in
> `plans/README.md`.
>
> **Drift check (run first)**: `git diff --stat 3a8b347..HEAD -- git/src/lib.rs`
> If `git/src/lib.rs` changed since this plan was written, compare the "Current
> state" excerpts against the live code before proceeding; on a mismatch, treat
> it as a STOP condition.

## Status

- **Priority**: P1
- **Effort**: S
- **Risk**: LOW
- **Depends on**: none
- **Category**: bug
- **Planned at**: commit `3a8b347`, 2026-08-24

## Why this matters

`git diff --raw` and `git status --porcelain` both emit paths **relative to the
repository root**. The acquisition layer joins those paths onto `repo`, which is
whatever the user passed — and the default is `PathBuf::from(".")`, i.e. the
current working directory. So when the tool is run from any subdirectory of a
repo, the join produces a wrong absolute path.

Concrete cost: run `gitten diff` from a subdirectory and every unstaged
working-tree modification reads as a **full deletion** (the working-tree read
fails, so the new side is empty), and **every untracked file silently
disappears** (its read fails and the file is skipped). Both failures look like
plausible output, not errors. This is the single most common way the tool is
invoked, so the most common invocation is silently wrong.

## Current state

- `git/src/lib.rs` — the only crate that talks to a repository. All reads shell
  out to the `git` binary via `fn run(repo, args)` (line 55).
- `git/src/lib.rs:689-694` — `new_side` reads the working tree for the unstaged
  (null-OID) side of a modified file:

  ```rust
  fn new_side(oid: &str, repo: &Path, path: &str) -> Option<Vec<u8>> {
      if !is_null_oid(oid) {
          return None;
      }
      std::fs::read(repo.join(path)).ok()   // <-- path is root-relative; repo may be a subdir
  }
  ```

- `git/src/lib.rs:355-357` — `untracked` reads each untracked file the same way:

  ```rust
  let Ok(content) = std::fs::read(repo.join(path)) else {
      continue;
  };
  ```

- `app/src/cli.rs:165-167` — the default source path is the cwd:

  ```rust
  (None, None) => Source::Repo {
      path: PathBuf::from("."),
      arg: tail.get(1).cloned().unwrap_or_default(),
  },
  ```

- Empirically verified at audit time: from `core/src/`, `git show --raw -z
  --abbrev=64 HEAD` prints `.github/workflows/check.yml` — a root-relative path,
  not `../../.github/...`.

Repo conventions to match:
- Reads go through `fn run(repo, args)` (`git/src/lib.rs:55`), which does
  `Command::new("git").arg("-C").arg(repo)...` and returns `Result<Vec<u8>>`.
  Use it; do not spawn `git` a second way.
- Never `read_to_string` git output — it is not guaranteed UTF-8. Use
  `String::from_utf8_lossy` (see `git/src/lib.rs:341`).
- Error type is `anyhow::Result` (aliased `Result` in this crate). Trimming
  trailing whitespace off a one-line git answer is done with
  `String::from_utf8_lossy(&bytes).trim().to_string()` (see line 428).

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Test this crate | `cargo test -q -p gitten-git` | `test result: ok`, exit 0 |
| Whole check | `cargo test -q -p gitten-core -p gitten-app -p gitten-git` | all ok |
| Lint | `cargo clippy -p gitten-git --all-targets` | no warnings |
| Format | `cargo fmt -p gitten-git` | no diff after |

## Scope

**In scope**:
- `git/src/lib.rs` (add a top-level resolver; use it in `new_side` and `untracked`)

**Out of scope** (do NOT touch):
- `app/src/cli.rs` — the cwd default is fine; the fix is to resolve the root
  inside the git layer, not to change the CLI default.
- `git/src/lib.rs`'s `pairs`/`each_pair`/`diff` public signatures — do not change
  the parameters callers pass; resolve the root internally.
- Anything about how `--raw` paths are parsed — they are already correct.

## Git workflow

- Branch: `advisor/001-repo-toplevel-paths`
- Commit message style matches this repo's log (imperative, lowercase-ish, one
  line, e.g. "Join working-tree paths onto the repo top level"). End the commit
  body with the repo's usual trailer if one is present in recent history; if not,
  omit.
- Do NOT push or open a PR unless instructed.

## Steps

### Step 1: Add a top-level resolver

Add a private helper in `git/src/lib.rs`:

```rust
/// The repository's top level. `--raw` and `--porcelain` paths are relative to
/// it, while the `repo` a caller passes may be any subdirectory (the CLI default
/// is the cwd), so working-tree reads must join onto this, not onto `repo`.
fn top_level(repo: &Path) -> PathBuf {
    match run(repo, &["rev-parse", "--show-toplevel"]) {
        Ok(bytes) => {
            let s = String::from_utf8_lossy(&bytes);
            let trimmed = s.trim();
            if trimmed.is_empty() {
                repo.to_path_buf()
            } else {
                PathBuf::from(trimmed)
            }
        }
        Err(_) => repo.to_path_buf(),
    }
}
```

Confirm `PathBuf` is imported (`use std::path::{Path, PathBuf};` — check the top
of the file; add `PathBuf` if only `Path` is imported).

**Verify**: `cargo build -p gitten-git` → exit 0.

### Step 2: Use the resolved root in `untracked`

In `untracked` (`git/src/lib.rs:327`), resolve the root once before the loop and
join onto it:

```rust
fn untracked(repo: &Path) -> Result<Vec<Pair>> {
    let root = top_level(repo);
    // ...existing run(repo, &["status", ...]) stays as-is — git -C handles cwd...
    // in the loop, replace repo.join(path) with root.join(path):
    let Ok(content) = std::fs::read(root.join(path)) else {
        continue;
    };
```

Note: the `run(repo, ...)` call itself stays keyed on `repo` — `git -C <subdir>`
correctly finds the repo from a subdirectory. Only the filesystem `join` needs
the root.

**Verify**: `cargo build -p gitten-git` → exit 0.

### Step 3: Use the resolved root in `new_side`

`new_side` (`git/src/lib.rs:689`) currently takes `repo`. Change it to take the
already-resolved root, and resolve the root once in its caller rather than per
file. Find the caller of `new_side` (grep `new_side` in `git/src/lib.rs`) — it is
inside `each_pair`'s blob-streaming loop. Resolve `let root = top_level(repo);`
once near the top of `each_pair` (before the per-file loop) and pass `&root` to
`new_side`. Rename the parameter to `root` for clarity:

```rust
fn new_side(oid: &str, root: &Path, path: &str) -> Option<Vec<u8>> {
    if !is_null_oid(oid) {
        return None;
    }
    std::fs::read(root.join(path)).ok()
}
```

If `new_side` is called in more than one place, pass the resolved root to each.

**Verify**: `cargo build -p gitten-git` → exit 0; `grep -n "repo.join" git/src/lib.rs`
returns nothing (all joins now use `root`).

### Step 4: Add a regression test

The crate's tests build scratch repositories. Find the existing test helper
(grep `fn .*-> .*Repo\|tempdir\|create_dir_all\|Command::new("git")` inside the
`#[cfg(test)] mod tests` block near the bottom of `git/src/lib.rs`, around line
750+ — there is a helper that inits a repo and writes files). Model a new test on
the existing `pairs(&repo, "")` working-tree test (grep
`a working tree diff` — it is around line 809).

Write a test that:
1. Inits a scratch repo, commits a file `sub/a.txt` with content "one\n".
2. Modifies `sub/a.txt` to "two\n" in the working tree (no commit).
3. Adds an untracked file `sub/new.txt`.
4. Calls the working-tree diff entry point (`pairs(&repo.join("sub"), "")` — i.e.
   passes the **subdirectory** as `repo`).
5. Asserts the modified file has a non-empty new side (not a full deletion) and
   that the untracked file appears in the result.

**Verify**: `cargo test -q -p gitten-git` → new test passes; `test result: ok`.

## Test plan

- New test in `git/src/lib.rs`'s `#[cfg(test)] mod tests`, following the
  structure of the existing working-tree diff test.
- Cases: (a) a working-tree modification from a subdirectory keeps its new side;
  (b) an untracked file in a subdirectory is present in the result. The
  pre-fix code fails both.
- Verification: `cargo test -q -p gitten-git` → all pass including the new test.

## Done criteria

ALL must hold:

- [ ] `cargo build -p gitten-git` exits 0
- [ ] `cargo test -q -p gitten-git` exits 0; the new subdirectory test exists and passes
- [ ] `grep -n "repo.join" git/src/lib.rs` returns no matches
- [ ] `cargo clippy -p gitten-git --all-targets` produces no new warnings
- [ ] `cargo fmt --check -p gitten-git` clean
- [ ] No files outside `git/src/lib.rs` modified (`git status`)
- [ ] `plans/README.md` status row updated

## STOP conditions

Stop and report (do not improvise) if:

- `git rev-parse --show-toplevel` is not available or `run` has a different
  signature than the excerpt (the crate drifted).
- `new_side` turns out to be called from outside `each_pair` in a place where the
  root cannot be resolved once — report the call sites you found.
- The existing working-tree test does not exist or `pairs` has a different
  signature — report what you found instead of guessing an entry point.

## Maintenance notes

- If a future change lets the user pass a path that is *not* inside a repo
  (e.g. a bare patch file — that path already exists via stdin and is handled
  elsewhere), `top_level` falls back to `repo`, which is correct for that case.
- A reviewer should check that `run(repo, ...)` is still keyed on `repo` (so
  `git -C` finds the repo from a subdir) and only the filesystem joins moved to
  `root`. Moving the `run` key to `root` would be wrong — it would re-read the
  whole repo from the top instead of scoping to what the user asked for. Actually
  `git diff`/`status` are whole-repo regardless of cwd, so this is cosmetic, but
  keep the change minimal.
