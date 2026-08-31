# Plan 047: Branch marks tell the truth

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update this plan's row in
> `plans/high-priority/README.md`.
>
> **Drift check (run first)**: `grep -n "fn row_bar" shell/src/views/diff.rs`
> must hit (the design pass is on your base). Line refs were taken at
> `00842dc` + the staged design pass; match on quoted content where a ref
> drifted; STOP on a structural mismatch.
>
> **Build cost**: `export CARGO_TARGET_DIR=/tmp/gitten-target`. Never launch
> `./dev desktop` or `./dev tui`.

## Status

- **Priority**: P1
- **Effort**: M
- **Risk**: LOW
- **Depends on**: none (reinforced by plan 054 — see below)
- **Category**: bug (honesty) — a colour that reads as semantic and is not

## Why this matters

Every local branch's dot is coloured `theme.lane(i)` where `i` is **the
branch's index in the local list** (`shell/src/views/branches.rs:226`,
flatten-time). Six lane colours cycle, so with 16 locals the rainbow repeats
every six rows, and creating or deleting one branch **reshuffles every colour
below it** — the test at `branches.rs:~1337` pins exactly that behaviour.
The doc comment above it claims the opposite: "…inks so each branch keeps
one colour across the app" (`branches.rs:60-63`). To a user, sixteen coloured
dots read as state — stale? worktree? diverged? — and mean nothing except
"not HEAD".

Plan 054 (recency sort) makes this acute: once the list re-orders on every
commit, index-keyed colours change *every time you commit*.

Separately: in a workflow with a dozen `worktree-agent-*` branches, the one
genuinely useful branch fact the pane does not show is **"checked out in
another worktree"** — a branch you cannot check out here without an error.
lazygit marks it; we should.

After this plan: a branch's dot colour is a stable function of its **name**
(the same trick the commits pane already uses for author initials), the doc
comment says what is true, and a branch checked out in another worktree
carries a quiet right-edge mark.

## Current state

- Dot assignment at flatten (`branches.rs:219-259`): HEAD's branch gets
  `●` in `chrome.accent`; other locals `●` in `theme.lane(i)`; remotes `○`
  in a quiet ink; detached HEAD its own dim row.
- The stable-hash precedent: the commits pane colours author initials by a
  hash of the author name through `theme.author(...)` (see
  `shell/src/views/commits.rs:~785-792` and `core/src/theme.rs` — `authors`
  is "cycled … per author for the initials column"). `theme.lanes` and
  `theme.authors` are both "any length; the drawing code takes them modulo".
- Worktree facts are not acquired anywhere: `gitten-git` has no worktree
  read (`grep -rn "worktree" git/src/` → nothing relevant). CLAUDE.md:
  `gitten-git` is the only crate that talks to a repository; reads spawn the
  `git` binary; never `read_to_string` git output (bytes +
  `from_utf8_lossy`).
- The flatten tests live at `branches.rs:~940-1060` (glyph/colour
  expectations quoted there: `"●feature"`, `t.lane(0)`, …).

## Commands you will need

| Purpose | Command | Expected |
|---|---|---|
| Git-layer tests | `cargo test -q -p gitten-git` | exit 0 |
| Shell tests | `cargo test -q -p gitten-shell` | exit 0 |
| Everything | `./dev check` | exit 0 |

## Scope

**In scope**: `shell/src/views/branches.rs` (colour keying, the doc comment,
the worktree mark, tests); `git/src/lib.rs` (one new read: worktree
checkouts); `core/src/theme.rs` **only if** a shared name-hash helper does
not already exist for authors (if `theme.author(name)` hashes internally,
add the sibling or generalize — do not duplicate the hash).

**Out of scope**: list *order* (plan 054); the graph gutter's lane colours
(they are topology-keyed and correct); any new palette values; checkout
verbs refusing worktree-taken branches (a later verb-level guard — this plan
only *shows* the fact).

## Git workflow

- Branch: `advisor/ui-047-branch-marks`
- Commits per step, e.g. `shell: a branch's colour follows its name, not its
  row`
- No push, no PR, unless the operator instructed it.

## Steps

### Step 1: Colour by name

Replace `theme.lane(i)` at `branches.rs:226` with a stable hash of the
branch name into the same palette, using the exact mechanism the author
initials use (find `theme.author`'s hashing and mirror it — same function
shape, same modulo rule; if the hash lives view-side in commits.rs, move it
somewhere both can call rather than copying). HEAD's accent dot and the
remote `○` rule are untouched.

Rewrite the `branches.rs:60-63` doc comment to say what is now true: the
colour follows the *name*, so it survives refreshes, re-orders and other
panes' opinions.

Update the test at `branches.rs:~1337` — it currently pins index-keying (it
was pinning the bug); replace with
`a_branchs_colour_survives_the_list_reordering` (insert a branch above,
assert the colour did not change).

**Verify**: `cargo test -q -p gitten-shell` → exit 0.

### Step 2: Acquire worktree checkouts

In `git/src/lib.rs`, add a read that lists branches checked out in
worktrees other than this one: spawn
`git worktree list --porcelain` (bytes, lossy UTF-8), parse the
`branch refs/heads/<name>` lines, drop the entry whose `worktree` path is
the repo's own toplevel. Return the branch names. House rules: no
`read_to_string`, tolerate a missing/old git by returning empty rather than
erroring (a display garnish must never take the pane down — same posture as
`(gone)` upstreams).

**Verify**: `cargo test -q -p gitten-git` → exit 0, with a parser unit test
over a captured porcelain fixture (two worktrees + the main one; a detached
worktree entry, which has no `branch` line, is skipped).

### Step 3: Mark it in the row

Thread the set into the branches flatten (the same refresh wave that reads
`head()`), and give a taken branch a quiet right-edge mark *before* the
drift cell: the word `worktree` in `chrome.faint`-resolved ink (through the
same quiet-text resolver the pane already uses — no raw `faint`, plan 040's
rule). Not a glyph: the app ships no icons, and a word costs nothing at the
right edge where `(gone)` already lives.

**Verify**: `cargo test -q -p gitten-shell` → exit 0; a flatten test
asserting the mark appears for a taken branch and never for HEAD's own.

### Step 4: Gate

**Verify**: `./dev check` → exit 0.

## Done criteria

- [ ] `./dev check` exits 0
- [ ] `grep -n "theme.lane" shell/src/views/branches.rs` → no hits in the
      dot-assignment path
- [ ] A test proves colour stability under re-ordering
- [ ] The `branches.rs:60-63` comment no longer claims index-keyed colours
      keep identity
- [ ] `git worktree list --porcelain` parse has a fixture test incl. a
      detached worktree
- [ ] No files outside the in-scope list modified (`git status`)
- [ ] `plans/high-priority/README.md` row updated

## STOP conditions

- `theme.author` turns out to be positional too (index-keyed rather than
  hashed) — then the "stable hash" precedent does not exist; report before
  inventing one, because the hash choice belongs in `core` and deserves the
  owner's eye.
- The TUI or web client draws branch dots from the same flatten and asserts
  the old colours in its own tests — update those too only if they are
  pinning the index bug; report if they pin something else.
- `git worktree list --porcelain` output on the machine's git version does
  not match the documented format (check `git --version` ≥ 2.7 first).
