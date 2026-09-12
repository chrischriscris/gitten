# 002 — W9: worktrees, submodules and bisect

One packet, three slices, one worker. This is packet **W9** of
[001 — TUI lazygit parity swarm](001-tui-lazygit-parity-swarm.md), written out at
implementation depth so it can be dispatched without the coordinator in the loop.
Read 001 §"Architectural requirements", §"Swarm execution contract" and
§"Verification gates" first — they bind this document, and where the two disagree
001 wins.

Its dependencies (W4 sync/branch movement, W5 operation lifecycle) are landed and
gate-verified. It does **not** depend on W7, which is in flight.

## Objective

Three repository surfaces lazygit has and gitten has nothing of: linked
worktrees, submodules, bisect. Each is a list plus a small verb family plus one
piece of state that outlives a keypress. They are grouped because they are the
three "specialized workflow" panes, not because they share code — the slices are
independently landable and independently droppable.

## Ledger rows

| ID | Row | Importance | Slice |
|---|---|---|---|
| LG-084 | Worktrees panel: list/new/switch/open-in-editor/remove | daily | 1 |
| LG-085 | New worktree from any ref row (`w`) | minor | 1 |
| LG-086 | Submodules panel: enter/remove/update/new/URL/init/bulk | advanced | 2 |
| LG-087 | Bisect options (`b`) | minor | 3 |
| LG-088 | Git-flow options (`i` in branches) | minor | 3 (decision only) |

Statuses in `tui-parity-ledger.md` are the coordinator's to change. Report what you
implemented and the test that proves it; do not edit the ledger.

## Base, worktree, ownership

```sh
# from /Users/chus/Projects/gitten, once:
git worktree add .worktrees/tui-parity-w9 -b feat/tui-parity-w9 529ec80
```

`529ec80` is the W6 tip — the integrated chain head at dispatch time. W7 is being
written concurrently on `feat/tui-parity-w7` and lands **before** this packet
integrates; the coordinator rebases this branch onto the W7 tip at integration.
Nothing you write may assume W7's stash/tag/reflog work exists.

- **Your only working directory** is `.worktrees/tui-parity-w9`. Never write to
  `/Users/chus/Projects/gitten`, `/Users/chus/Projects/gitten-ux-polish`, or any
  other `.worktrees/*`. Never push, never write to a remote, never open a PR.
- Commit locally on `feat/tui-parity-w9`, early and often — one commit per working
  increment, so an interrupted lane leaves landed work behind.
- **Files you own outright:** `core/src/worktree.rs` (new), `core/src/submodule.rs`
  (new), `core/src/bisect.rs` (new), `tui/src/worktrees.rs` (new),
  `tui/src/submodules.rs` (new), and the tests you add.
- **Files you share** — additive edits only, each inside a block delimited by
  `// --- w9 begin` / `// --- w9 end` so the coordinator's rebase sees one hunk per
  file: `core/src/lib.rs` (module declarations), `core/src/command.rs`
  (registrations + bindings), `git/src/lib.rs` (trait methods + `Binary` impls +
  parsers), `app/src/{acquire,act,verbs}.rs`, `tui/src/main.rs` (dispatch arms and
  pane registration), `tui/src/panes.rs` (`canonical_rank` entries only).
- Do not reformat, reorder or "tidy" anything outside your blocks. A diff that
  touches a shared file in ten places is a diff the coordinator cannot land beside
  three other lanes.

## What already exists — do not rebuild it

Verified at `529ec80`; check before you write, and if a claim here is wrong, trust
the code and say so in your report.

- `Repo` (`git/src/lib.rs`) is one object-safe trait behind `Handle = Arc<dyn Repo>`,
  and **every method has a default that returns `Err(unserved("…"))`**. New reads and
  verbs go on that trait with defaults; the test fakes then override only what a
  test needs, and no fake breaks when you add a method.
- Process helpers, all in `git/src/lib.rs`: `run`, `run_bytes` (argv from `&[u8]`
  through `OsStrExt::from_bytes` — this is how a raw path stays raw), `run_stdin`
  (arbitrary payload on stdin, never argv), `run_env`, `display_args`, `top_level`,
  `join_raw`, `chunk_end` (argv chunking for long path lists).
- `nameable` and `refuse_dashes` — name validation that stops a value being read as
  a git option. Every path, URL, ref and branch name you accept goes through them or
  after a `--`; a submodule URL beginning `-` is the exact shape of that bug.
- `git_state_path(name)` / `git_state_exists(name)` — cached `.git/<name>` lookups,
  already carrying `rebase_in_progress` and `cherry_pick_in_progress`. Bisect's state
  files are the third tenant, not a fourth mechanism.
- `worktree_branches()` already runs `git worktree list --porcelain` and
  `parse_worktree_branches` reads it, to mark branches checked out elsewhere. Slice 1
  **extends that parse into a full record and re-uses the same single invocation** —
  two listings of the same thing that can disagree is a bug waiting to be filed.
- `submodule_state` in the status parser already reads porcelain-v2's gitlink flags,
  and `a_submodule_bump_is_a_one_line_synthetic_file` shows how a gitlink change
  renders. Slice 2 adds identity (name, path, url, recorded OID), not flags.
- `app/src/projects.rs` holds the MRU and W4 landed `project.open`/`project.switch`
  end to end. "Enter a submodule" and "switch to a worktree" are that path with a
  different source of the target — not a second repository-opening mechanism.
- `tui/src/panes.rs` is a registry: a pane is a stable name plus a value, with
  `canonical_rank` for sidebar order and a `Layout` trait for geometry. Adding a pane
  is a `register` call and a rank, never a new branch in layout code. An absent pane
  answers its focus command with `no <name> pane`.
- W5's operation lifecycle (`operation.abort` / `operation.continue` /
  `operation.skip`, `merge.*`) already models a standing operation and drops it on a
  repository switch. Read that code before slice 3.

## Slice 1 — worktrees

**Read.** One `git worktree list --porcelain` per acquisition, parsed into
`core::worktree::Worktree { path: Vec<u8>, head: Option<String>, branch:
Option<Vec<u8>>, bare: bool, detached: bool, locked: Option<Vec<u8>>, prunable:
Option<Vec<u8>>, current: bool }`. Absence is data, not an empty string: a locked
worktree with no reason and an unlocked one must not read alike. Prefer the
NUL-terminated form (`--porcelain -z`, git ≥ 2.36) and fall back to the line form —
a worktree path may contain a newline, and the line form cannot say so. Record which
form ran in the module doc.

`parse_worktree_branches` keeps its signature and becomes a thin projection of the
new parse; its existing test must still pass untouched.

**Verbs**, on `Repo`, all through the binary:

| Verb | Invocation | Refusals that are ours, not git's |
|---|---|---|
| `worktree_add(path, start, new_branch)` | `worktree add [-b <new>] -- <path> [<commitish>]` | a branch already checked out elsewhere (`worktree_branches` knows); a path inside the repository's own `.git`; a name/URL-shaped path caught by `refuse_dashes` |
| `worktree_remove(path, force)` | `worktree remove [--force] -- <path>` | removing the **current** worktree — refuse by identity, never by string compare of a display path |
| `worktree_lock(path, reason)` / `worktree_unlock(path)` | `worktree lock/unlock` | — |
| `worktree_prune()` | `worktree prune` | — |

Git already refuses a dirty removal without `--force`; surface its words rather than
pre-empting them, and make the forced variant a twice-pressed key (the convention
from W0: `D D`, not a modal).

**Pane.** `tui/src/worktrees.rs`, registered as `"worktrees"`, ranked after
`stashes` in `canonical_rank`. Rows: path (shortened against the main worktree's
top level), branch or `(detached)`, a lock/prunable marker, and the current one
marked. Commands: `worktrees.focus`, `worktrees.new`, `worktrees.switch` (`space`),
`worktrees.remove` (`D` twice), `worktrees.open` (deferred — see below).

**Switching** is `project.open` against the linked worktree's path. It is a
repository change: the standing operation drops, the diff cache keys are per-OID and
stay valid, and a pending write result must not leak across the switch — W4's
`tui_parity_a_pending_result_cannot_leak_across_a_repository_switch` is the pattern
and your switch needs its own version of it.

**`open-in-editor` is not yours.** LG-084's editor handoff belongs to the tool
service in [003](003-w10a-external-tools-and-custom-commands.md). Register
`worktrees.open` only if 003 has landed in your base; otherwise leave it out
entirely and say so. A registered command that refuses is W0's contract and is
acceptable; a stub that pretends is not.

**LG-085 (`w` from a ref row).** One shared action taking a `core::refs::Target`,
bound in every list mode that has a ref-shaped row. Bind it in the modes that exist
in *your* base (`commits`, `branches`, `remotes`) and list the ones you could not
(`stashes`, `tags`) in your report as one-line binding adds for the coordinator.
Do not touch `tui/src/stashes.rs` — W7 owns that file this week.

**Acceptance (slice 1).** Hermetic scratch repositories only, per the `Scratch`
helper in `git/src/lib.rs`'s tests. A linked worktree is created, listed with its
branch, switched into (HEAD reads the other branch), refused for a branch checked
out elsewhere, refused for the current worktree's own removal, removed when clean,
refused when dirty, forced when asked twice. A path with a space and a path with a
non-UTF-8 byte both survive the round trip. Named `tui_parity_*` tests drive the
keys, not just the `Repo` methods.

## Slice 2 — submodules

**Read.** `core::submodule::Submodule { name: Vec<u8>, path: Vec<u8>, url:
Option<Vec<u8>>, recorded: Option<String>, checked_out: Option<String>, state }`
where `state` distinguishes uninitialized, initialized-and-current, initialized-with
-a-different-checkout, and conflicted — porcelain's `-`, ` `, `+`, `U`. Compose it
from `git config -f .gitmodules -z --get-regexp` (identity and URL, which is where a
non-UTF-8 path or a `-`-leading URL comes from) plus `git submodule status`
(checkout state). `.gitmodules` absent means no submodules, which is data and not an
error; a submodule in `.git/config` but not `.gitmodules` is a real state and must
render rather than vanish.

**Verbs.** `submodule_init(paths)`, `submodule_update(paths, init, recursive)`,
`submodule_sync(paths)`, `submodule_add(url, path)`, `submodule_remove(path)`,
`submodule_set_url(path, url)`, plus the bulk forms (all paths). Removal is the one
with a real sequence — `submodule deinit --force -- <path>`, `git rm --force --
<path>`, then the module directory under `.git/modules` — and it is destructive in
a way git will not undo: twice-pressed, and the pane says what will be deleted
before the second press.

**Enter and return.** Entering is `project.open` on the submodule's absolute path
(`join_raw` against the top level). Returning is the parent, which means the parent
is remembered as the MRU's previous entry — reuse it rather than inventing a stack.
A submodule with a detached HEAD is the normal case, not an error state.

**Pane.** `tui/src/submodules.rs`, registered `"submodules"`, ranked last. Rows:
path, state marker, short recorded OID, and the URL when the pane is wide (`WIDE_AT`
already decides that question).

**Acceptance (slice 2).** A nested scratch repository added as a submodule of
another: listed uninitialized, initialized, updated, entered (the opened repository
is the submodule's own), returned, URL changed and re-synced, removed with the
working tree and `.git/modules` entry both gone. A submodule path with a space, and
one whose URL begins with `-`, are both handled. No test may reference a path
outside its temporary directory — this repository has real `.worktrees/*` checkouts
and a real `.gitmodules`-free tree, and a test that reaches them is a defect
regardless of whether it passes.

## Slice 3 — bisect, and the git-flow decision

**State.** `core::bisect::Bisect` reads: in progress or not, the configured terms
(`BISECT_TERMS` — a repository may be bisecting old/new, not good/bad, and hard-
coding "good" prints a lie), the start ref (`BISECT_START`), the log
(`.git/BISECT_LOG`), and how many revisions remain (git says so on stdout; parse its
line rather than recomputing `log2`). Detection rides `git_state_exists`, the same
mechanism `rebase_in_progress` uses.

**Verbs.** `bisect_start(bad, goods)`, `bisect_mark(term, rev)`, `bisect_skip(rev)`,
`bisect_reset(to)`, `bisect_log()`. Each is a checkout with a side effect, so each
finishes through the write queue and bumps the invalidation generation: the commits
pane, the files pane and the diff all re-acquire, because HEAD moved under all three.

**Lifecycle.** A standing bisect is a standing operation. Register it with W5's
lifecycle so `m` offers reset, so the status line says a bisect is running with its
terms, and so a repository switch drops it. Do not add a parallel notion of
"current operation" — if the lifecycle cannot hold a bisect, report that as a
contract gap and stop the slice rather than forking the model.

**Pane or menu.** Bisect needs no list of its own: `b` in commits opens the menu
(start here / mark this / skip / reset), and the standing state renders in the
existing status line. Prefer that to a pane nobody would focus.

**Acceptance (slice 3).** A scratch repository with a known bad commit planted at a
known depth: `b` starts, marking walks, the reached commit is the planted one, the
log lists the answers, reset restores the original HEAD and leaves no
`refs/bisect/*`. A bisect started with old/new terms prints old/new. A bisect
interrupted by a repository switch does not survive it.

**LG-088 (git-flow) is a decision, not an implementation.** `git flow` is a
third-party binary; running it is what [003](003-w10a-external-tools-and-custom-commands.md)'s
custom-command seam is for. Write the case in your report — what upstream's `i` menu
offers, what shipping it would require, and the recommendation (ship as a
configured custom command; do not hardcode a vendor workflow) — and leave the row
for the coordinator to mark EXCLUDED-with-reason or scheduled. 001 forbids silent
omission, not omission.

## Hazards

- **This repository is being worked on by four agents in eight worktrees.** Slice 1
  is a feature about worktrees, which makes an unhermetic test uniquely dangerous
  here: `git worktree remove` or `worktree prune` against the wrong root would
  delete a lane's work. Every test creates its own temporary root, and no test
  argument is ever a path from the developer's tree.
- **`worktree prune` is not a cleanup you run helpfully.** It is a verb the user
  asks for.
- Names and paths are bytes throughout. `String::from_utf8_lossy` at the drawing
  edge only; a lossy value must never travel back into an argv.
- `--topo-order` stays on every history traversal; nothing in this packet changes
  log acquisition, and if it appears to, you are in the wrong file.
- Adding a `Repo` method without a default breaks every fake in the tree. Default it.
- Keep the render path allocation-free: pane rows are built at load, cached, and
  drawn from cache, like every existing pane.

## Gates

Run in your worktree, and paste the tail of each with its exit status:

```sh
cargo fmt --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked -p gitten-core -p gitten-app -p gitten-git -p gitten-tui
cargo test --locked -p gitten-core -p gitten-app -p gitten-git -p gitten-tui tui_parity_
COLS=120 ROWS=40 ./dev dump commits .
COLS=80  ROWS=24 ./dev dump diff --fixtures
```

The filtered run passes with zero tests, so name yours. `./dev check` rewrites
generated fixtures and must not run concurrently with another lane — the coordinator
schedules it. Never launch `./dev tui` or `./dev desktop`: hand the command over
instead.

## Proof protocol

Two lanes in this campaign fabricated completion reports. Consequently:

1. Every report ends with verbatim `git log --oneline 529ec80..HEAD` and
   `git diff --stat 529ec80..HEAD`.
2. Every claimed commit must resolve: `git cat-file -e <sha>` for each.
3. Every claimed test must appear by name in captured test-binary output. Counts
   without names are not evidence.
4. Gate output is pasted as tails with exit statuses, not summarized.
5. A failing gate is reported as failing. **A smaller true report beats a larger
   false one**, and a lane that landed slice 1 honestly is worth more than one that
   claims three.

## Out of scope

W7's stash/tag/reflog family. W8's patch builder. W10's tools, custom commands and
conveniences. The desktop window — 004 owns it, and per AGENTS.md a feature asked
for without a client named means the window, but this packet names the terminal.
Put the *seam* in `core`/`app` so the window can pick it up; put the implementation
in the terminal only.

## Dispatch text

> You are the W9 implementation lane for gitten. Read
> `plans/002-w9-worktrees-submodules-bisect.md` in full, then
> `plans/001-tui-lazygit-parity-swarm.md` and `AGENTS.md`. Work only in
> `.worktrees/tui-parity-w9` on `feat/tui-parity-w9`, based on 529ec80. Deliver
> slice 1 completely before starting slice 2; commit after each working increment.
> Deliver working code and named `tui_parity_` tests, not stubs and not a new plan.
> Never push, never touch another worktree, never launch a client. Finish with the
> proof protocol from the plan: verbatim git log, diff stat, gate tails with exit
> statuses, executed test names.
