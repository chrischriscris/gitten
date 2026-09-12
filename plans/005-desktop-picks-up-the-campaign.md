# 005 — The window picks up what the campaign landed

A lane for the **desktop app**, running beside the terminal packets of
[001](001-tui-lazygit-parity-swarm.md) and touching none of their files. One
worker, four slices. Read 001 §"Architectural requirements" and
§"Swarm execution contract", `AGENTS.md`, and `docs/clients.md` before starting.

## Why this exists

AGENTS.md: **the desktop app is the product**, the other doors exist to keep the
shared layers honest, and a shared seam is never worth a worse window.
`docs/clients.md` says the same from the other side: the implementation belongs
only where it was asked for.

A week of campaign asked for the terminal. That was right, and it worked — W0 to W7
put availability, diff sources, search, partial staging, sync, the operation
lifecycle, conflict answers and history editing into `core`, `gitten-git` and
`gitten-app`, where every client can reach them. The consequence is that **the window
now has less of gitten in it than the terminal does**, and the gap is not features
that need building: it is features that are built, tested, shared, and unreachable
from the product.

So this lane is not "port the TUI". It is: walk the chain tip's command registry,
find every name the window cannot answer, and answer the ones that need no new
design. And because the window may only reach through the seams, it is also the
campaign's cheapest audit of whether those seams are real — **every place you would
have to add something to `core`, `app` or `gitten-git` to finish a row is a finding,
not a patch you write.**

## Base, worktree, ownership

```sh
# from /Users/chus/Projects/gitten, once:
git worktree add .worktrees/desktop-pickup -b feat/desktop-pickup 529ec80
```

`529ec80` is the W6 tip: the shared seams you need are in it. W7 (stash, tags,
reflog) is in flight and lands later; nothing you write may assume it.

- **Your only working directory** is `.worktrees/desktop-pickup`. Never write to
  `/Users/chus/Projects/gitten`, `/Users/chus/Projects/gitten-ux-polish`, or another
  `.worktrees/*`. Never push, never write to a remote, never open a PR. Commit
  locally, one commit per working increment.
- **You own `shell/` and nothing else.** Not `core/`, not `app/`, not `git/`, not
  `tui/`, not `web/`, not `cli/`. If a row cannot be finished without a change
  outside `shell/`, write the contract proposal into your report — the exact
  signature you need and why the window cannot get there — and move on to the next
  row. That rule is the point of the lane, not an inconvenience in it.
- New panes and views go in `shell/src/views/*.rs` beside the existing
  `files.rs` / `branches.rs` / `commits.rs` / `diff.rs` / `stashes.rs` / `split.rs`.
  `shell/src/main.rs` is 528 KB and the largest file in the tree: add to it only the
  registration and the dispatch arm. **Do not refactor it** — 001 explicitly forbids
  a wholesale dispatcher refactor as a prerequisite, and a lane that spends its
  budget moving code lands nothing.

## Slice 1 — the inventory, then the operation lifecycle

**The inventory is mechanical and comes first**, because it is the map for the rest
and it is the artifact the coordinator wants even if the lane stops early. The
command registry is data: `Commands::builtin()` at your base lists every name, and
the window's dispatch answers a subset. Produce a table — name, whether the window
dispatches it, whether it is refused, and which slice would land it — as
`docs/desktop-gaps.md`, and keep it accurate as you work. Generate it from the
registry rather than by reading `main.rs` with your eyes; a name you miss is a row
nobody knows is missing.

Then land the first family: **the standing operation**. W5 put it in `app`
(`operation.abort` / `operation.continue` / `operation.skip`, and the lifecycle that
knows a rebase, a cherry-pick, a revert or a merge is in progress). The window ships
`rebase.abort` / `rebase.continue` names and no lifecycle behind them.

- The chrome says what is standing, with the words the shared layer supplies — a
  half-finished rebase is the single most important thing a git client can tell you,
  and the window currently does not.
- The three verbs dispatch, refuse honestly when nothing is standing (W0's contract:
  a command that is available but reaches no handler says so; one the client does not
  support says *that* instead), and refresh on finish either way.
- A repository switch drops the standing operation — the shared layer already does
  this and W4 has the test; the window must not cache its own copy that outlives it.

## Slice 2 — conflicts, at the level the window can honestly draw

W5 landed whole-file and region conflict answers: `files.resolve-ours`,
`files.resolve-theirs`, `files.resolve-both`, `files.resolve-keep`, plus the
merge-side commands. The window already models `Section::Conflicts` in its files view
and already knows that staging a conflicted file records a resolution. What it lacks
is the answer commands, the reset menus (`files.reset-menu`,
`files.reset-upstream-*`), and `files.nuke`.

Two things stay out, deliberately:

- **The inline three-way merge editor is not in this lane.** `docs/roadmap.md` #19
  calls it lazygit's actual differentiator and says it deserves a supervised design
  session rather than an overnight. Believe it. Whole-file and region answers are
  reachable through the shared seam and are what this slice ships.
- **The rebase-todo screen's window presentation** is design work too (the terminal's
  is a screen; the window's is not a port). Land the verbs W6 exposed that need no
  new surface — reword, mark-base, move up/down, squash/fixup/drop from the commits
  pane — and park the plan editor with a note.

**The confirmation idiom is a decision to record, not to make twice.** The terminal
asks twice with a repeated key (`D D`, `d d`) because a modal in a terminal costs a
screen. The window has modal plumbing (`shell/src/modal.rs`, and
`settings_window.rs` landed a real window). Recommendation to put to the
coordinator: keep the twice-pressed key as the *keyboard* path so muscle memory
transfers, and use the modal only where the terminal shows a menu — but write the
case, and do not redesign the destructive-verb idiom on your own authority.

## Slice 3 — the ref panes

- **A remotes pane.** `Repo::remotes` and the `remotes.*` command family exist and W4
  proved them in the terminal; the window's sidebar is a registry
  (`shell/src/panes.rs`: register a name and a value, `canonical_rank` orders it), so
  a tenant is a registration and a view file, not a layout change. Rows read names
  and URLs; `remotes.new` / `remotes.edit` / `remotes.remove` / `remotes.fetch`
  behind them, with removal asked twice and never retargeting after the selection
  moves (W4's test names the hazard: `a_remote_removal_asks_twice_and_never_retargets`).
- **Branch verb breadth**: `branches.fast-forward`, `branches.force-checkout`,
  `branches.merge`, `branches.merge-squash`, `branches.set-upstream`,
  `branches.unset-upstream`, `branches.checkout-name`, `branches.checkout-previous`,
  `branches.open-log`. All shared, all tested in the terminal, none reachable in the
  window.
- **Commits verb breadth**: `commits.checkout`, `commits.revert`, `commits.copy` /
  `commits.paste` / `commits.clear-copies` (the cherry-pick register),
  `commits.reset-*`, `commits.reword`, `commits.mark-base`, `commits.move-up` /
  `move-down`, `commits.reset-author`.
- Every one of these is a write, so every one goes through the existing queue and the
  generation refresh the window already has. A verb that draws its own optimistic
  state is a bug: the generation bump is what makes three panes agree.

## Slice 4 — navigation and input parity

The small things that make the window feel a generation behind the terminal, all of
them shared already: `search.next` / `search.prev` / `search.clear` (the window has
`/` and no `n`), `select.mark` and the range it enables, `input.newline` (a multi-line
commit message), `files.toggle-side`, `files.open-diff`, `diff.next-hunk` /
`diff.prev-hunk`, `diff.toggle-line-selection`, `diff.search`, and the per-pane
previews W1 landed (a branch, a stash or a remote row driving the main view instead
of the commits pane always driving it).

Prefer the shared model in every case: `core::search` holds the matcher, `core::select`
holds selection, W1's source model holds which pair the main view shows. The window
writing its own is how the row flattening and the branch colours came to be written
twice before they were written once.

## GPUI rules that will bite

All of these are in AGENTS.md; they are repeated because each one has already cost a
day in this repository.

- **Interactivity requires identity**: `.id()` before scroll, click, hover, drag —
  and an element's identity is its *path*, so two unnamed wrappers around two
  `.id("list")` children are the same element and drive each other's state.
- **`uniform_list` for anything long**, never wrapped in another scroll container;
  `min_w_full` on every row so a background runs to the edge; `flex_1` + `min_w(0)`
  for a row that draws columns.
- **Anything that floats needs `deferred`** plus `.occlude()`, or it paints under its
  sibling and the rows underneath take its clicks. Every menu in slices 2 and 3 is
  this case.
- **Read the host on the render path** (`config::host(cx)`), never a captured clone,
  or your new pane silently stops hot-reloading its font and colours.
- `gpui_component`'s theme must be pushed at ours (`config::sync_widgets`) or a new
  scrollbar paints a light track over a near-black diff.
- A column of numbers is right-aligned; a view cannot know its own size during
  `render`; custom drawing is `canvas()` + `PathBuilder` + `paint_path`, kept per row
  so it virtualizes with the list.
- Never run a bare `cargo update`, never bump GPUI, never touch
  `rust-toolchain.toml`. The four GPUI git dependencies are pinned by `Cargo.lock`
  and nothing else.

## Verification

The desktop has real headless tests: `check.sh` runs `cargo test -q -p gitten-shell`
with GPUI's test context and no window appears. Two patterns to copy, both already in
the tree:

- `#[gpui::test]` with `TestAppContext` — `shell/src/input.rs`.
- Plain `#[test]` over the pure geometry/metric functions — `shell/src/help.rs`,
  which also documents the trap: **import by name, never `use gpui::*` in a test
  module**, or GPUI's attribute macro shadows `#[test]` and nothing expands.

Every dispatch arm you add gets a test that presses the key against a fake `Repo` and
asserts the resulting write or the refusal wording — the shell's existing fakes
(including the one whose `AtomicBool` makes the next revert conflict) are the pattern.

```sh
cargo fmt --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked -p gitten-shell
cargo test --locked --workspace
```

**You cannot see the window, and you may not open it.** AGENTS.md: never launch a
client unless asked; a window appearing unannounced interrupts whoever is at the
keyboard. So the visual half of this lane is a handover, not a screenshot: finish
with the exact command the user runs (`./dev desktop`, plus the pane and key
sequence) and a short checklist of what to look at per slice. Do not substitute a
`./dev dump` frame for it — that is the terminal's renderer, not the window's, and
saying otherwise would be a false claim about verification.

## Proof protocol

Two lanes in this campaign fabricated completion reports. So:

1. End the report with verbatim `git log --oneline 529ec80..HEAD` and
   `git diff --stat 529ec80..HEAD`.
2. Every claimed commit resolves under `git cat-file -e`.
3. Every claimed test is named from captured test-binary output.
4. Gate tails pasted with exit statuses, not summarized.
5. Say plainly which rows are landed, which are parked for design, and which are
   blocked on a seam that does not exist — with the signature you would need. **A
   smaller true report beats a larger false one.**

## Out of scope

Any change to `core/`, `app/`, `git/`, `tui/`, `web/` or `cli/`. The inline three-way
merge editor. A window presentation for the rebase-todo plan. W7's stash/tag/reflog
family. New visual language: this lane closes a capability gap in the existing
design, and an aesthetics pass is its own branch (`feat/ux-aesthetics-pass` in the
overnight notes).

## Dispatch text

> You are the desktop pickup lane for gitten. Read
> `plans/005-desktop-picks-up-the-campaign.md` in full, then `AGENTS.md`,
> `docs/clients.md` and `plans/001-tui-lazygit-parity-swarm.md`. Work only in
> `.worktrees/desktop-pickup` on `feat/desktop-pickup`, based on 529ec80. You may
> change `shell/` only: if a row needs anything outside it, record the contract you
> would need and move on. Slice 1's inventory (`docs/desktop-gaps.md`, generated from
> the command registry) comes first and is committed before any feature work. Add
> headless `gitten-shell` tests for every dispatch arm you add. Never push, never
> touch another worktree, and never open the window — hand over the command and a
> checklist instead. Finish with the plan's proof protocol.
