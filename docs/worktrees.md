# Worktrees

One rule for where they live, one checklist for when they die. Written after
PR #63 left a live worktree behind a merged branch and two abandoned
`.wt` roots (`gitten.wt/`, `plait-worktrees/`) had to be deleted by hand.

## Layout

A worktree lives at `<repo>/.worktrees/<slug>`, gitignored, never as a
sibling in `~/Projects` and never under a per-repo `.wt` root.

```sh
git worktree add .worktrees/projects-mru -b feat/projects-mru
```

`<slug>` is the branch name without the type prefix (`feat/projects-mru` →
`projects-mru`). Why inside the repo rather than beside it:

- `git worktree list` shows all of them; nothing to discover by listing `~/Projects`.
- No root dir survives the last worktree — the `.wt`-root pattern always did,
  because the root outlives its contents and nothing owns deleting it.
- Precedent: `hyros/.worktrees/` works the same way one level up.

Cost control: a second checkout duplicates `target/` (~9 GB measured on
`gitten-projects`). Point cargo at the main checkout's cache from inside a
worktree:

```sh
export CARGO_TARGET_DIR=/Users/chus/Projects/gitten/target
```

## Prefer worktrees for features

Feature work goes in worktrees; the main checkout defaults to `main`. This is
a coordination convention, not a correctness rule — git already refuses to
delete a branch that is checked out anywhere, so nothing technically breaks if
you work a feature in the main dir. The one obligation when you do: say so
before anyone deletes branches, so a branch everyone believes is merged and
deletable doesn't silently gain new commits.

## Removal checklist

This repo squash-merges (every `(#NN)` commit on `main` is a squash), so
"merged" does **not** mean ancestry — `merge-base --is-ancestor` fails on a
squash-merged branch by design. Check all four, in order:

```sh
WT=.worktrees/<slug>; BR=<branch>
git -C $WT status --porcelain=v1 --untracked-files=all  # 1. empty: clean
gh pr list --head $BR --state all                        # 2. MERGED on GitHub
git fetch origin
git diff $BR origin/main --stat                          # 3. empty: main holds every byte
```

Then:

```sh
git worktree remove $WT
git branch -D $BR              # -D, not -d: squash means "not merged" to git
git push origin --delete $BR
git worktree prune
```

Step 3 is the load-bearing one. A non-empty diff is a veto no matter what the
PR page says; an empty diff after a squash-merge is the proof the branch's
content survived under a new hash (PR #63: `569fc78` → `e9072c7`, zero-byte diff).

## Finding dead ones

```sh
git worktree list --porcelain          # every registered worktree; each path must exist
git worktree prune --dry-run -v        # non-empty output means registered-but-gone
```

The reverse — a checkout whose `.git` file points at a vanished admin dir —
shows up as `fatal: not a git repository` on any git command inside it. An
empty `.worktrees/` dir with nothing registered is not a worktree, delete it.
