# 005 — The window picks up what the campaign landed

A lane for the **desktop app**: one worker, five slices, `gui/` only. Rewritten
2026-11-02 against `main` after #84; the 2026-09-07 version assumed `shell/`,
a W6 base and a worker that could not see the window, and all three are gone.

Read before starting: `AGENTS.md` (and its "Read before you touch it" table),
`docs/clients.md`, `docs/gpui-notes.md`, `docs/agent-verify.md`.

## Why this exists

AGENTS.md: **the desktop app is the product**, and a feature asked for without
a client named means the window. The terminal campaign (001, W0–W9, #73) put
availability, diff sources, search, partial staging, sync, the operation
lifecycle, conflict answers, history editing, stash, tags, reflog and
worktrees into `core`, `gitten-git` and `gitten-app`, where every client can
reach them. The consequence is that **the window has less of gitten in it than
the terminal does**, and the gap is mostly not features that need building. It
is features that are built, tested, shared, and unreachable from the product.

`docs/features.md` is the inventory, generated from the registry: today 124 of
218 commands are not named anywhere in `gui/`. That is the list this lane
works down. It is not "port the TUI": the window is mouse-first with a
palette and `cmd-` chords (desktop-v2, `plans/desktop-v2/PLAN.md`), so a row
lands when a person can reach it the way the window is used, not when a
letter key is bound.

And because the window may only reach through the shared seams, the lane is
also the cheapest audit of whether those seams are real. **Every place you
would have to change `core`, `app` or `git` to finish a row is a finding, not
a patch you write.**

## Base, ownership

- Branch `feat/desktop-pickup` in its own worktree, from `main` once #84 and
  the verify-findings fix (the first-write refresh bug in `drain_jobs`) have
  merged. Before that, stack it on `fix/verify-findings`; every write in
  slices 2–4 depends on that refresh firing.
- **You own `gui/` and nothing else.** If a row needs a change outside it,
  write the contract into your report (the exact signature, and why the
  window cannot get there) and move on to the next row.
- New panes and views go in `gui/src/views/*.rs`. `gui/src/main.rs` is 690 KB:
  add only registrations and dispatch arms to it, and **do not refactor it**.
  A lane that spends its budget moving code lands nothing.
- Commit locally, one commit per slice, and push only when asked. After each
  slice, run `./dev features` and commit the regenerated map with it, so the
  gap visibly closes.

## Slice 0 — the palette stops offering what the window cannot do

The palette lists the whole registry, so 124 of its rows answer "not
supported here" when chosen. The terminal already solved this: it builds a
strict `core::command::Availability` (`tui/src/main.rs`, `fn tui_availability`)
and both its help and its dispatch read it. Do the same in the window:

- a strict `Availability` naming every command the window dispatches, with
  `Disabled(reason)` where the launch turns one away (a fixture view with no
  repository);
- the palette lists `Available` and `Disabled` rows only, and a disabled row
  says its reason where the doc line was;
- the dispatch's fallback refuses through it instead of through ad-hoc
  `"… is not supported here"` notices;
- a test that every name the window's dispatch matches is `Available`, so
  the list and the `match` cannot drift.

This comes first because every later slice then has a visible effect, which
is a command appearing in the palette.

## Slice 1 — the standing operation

W5 put the lifecycle in `app`: it knows a rebase, cherry-pick, revert or merge
is in progress, and `operation.abort` / `operation.continue` /
`operation.skip` act on it. The window ships `rebase.abort` and
`rebase.continue` and no lifecycle behind them.

- The chrome says what is standing, in the words the shared layer supplies. A
  half-finished rebase is the most important thing a git client can tell you,
  and the window currently does not say it.
- The three verbs dispatch, refuse honestly when nothing is standing, and
  refresh on finish either way. Abort and continue get buttons beside the
  statement. Skip goes in the palette only.
- A repository switch drops the standing operation. The shared layer does this
  already; the window must not cache a copy that outlives it.

## Slice 2 — conflicts, at the level the window can honestly draw

The files view already models `Section::Conflicts`. Add the answers:
`files.resolve-ours` / `-theirs` / `-both` / `-keep` on a conflicted row,
plus the merge-side `merge.take-*`, `merge.undo` and
`merge.next-conflict` / `prev-conflict`. Also the reset menu
(`files.reset-menu`, `files.reset-upstream-*`) and `files.nuke`.

Out, deliberately:

- **The inline three-way merge editor.** `docs/roadmap.md` #19 calls it the
  differentiator, and it needs a supervised design session.
- **A window presentation of the rebase-todo plan** (`todo.*`,
  `commits.interactive-rebase`). The terminal's is a screen; the window's is
  not a port. Park it with a note.

**Destructive confirmation is a decision to record, not to make.** The
terminal asks twice with a repeated key. The window has `modal.rs` and a
question band with clickable answers (`Notice::Question`). Recommend one
idiom for the window in the report, use it, and don't invent a third.

## Slice 3 — the ref sections

The sidebar is tabbed sections (decision 0031) over a pane registry
(`gui/src/panes.rs`), so a new tenant is a registration and a view file.

- **Remotes**: names and URLs, plus `remotes.fetch` / `new` / `edit` /
  `remove`. Removal asks once more and never retargets after the selection
  moves; the terminal's
  `tui_parity_a_remote_removal_asks_twice_and_never_retargets` names the
  hazard.
- **Tags**: `tags.checkout` / `new` / `delete` / `push`.
- **Worktrees**: `worktrees.switch` / `new` / `remove`. Removal follows
  `docs/worktrees.md`'s checklist.
- **Reflog** is the stretch row: `reflog.recover` and the focus. Park it if
  it does not fit.

Every one of these is a write, so every one goes through the existing job
queue and the generation refresh. A verb that draws its own optimistic state
is a bug: the refresh is what makes three panes agree.

## Slice 4 — verb breadth on existing panes

All shared, all tested in the terminal, none reachable in the window:

- **branches**: `merge`, `merge-squash`, `fast-forward`, `force-checkout`,
  `checkout-name`, `checkout-previous`, `set-upstream`, `unset-upstream`,
  `delete-remote`, `open-log`.
- **commits**: `reword`, `move-up` / `move-down`, `mark-base`,
  `reset-author`, the cherry-pick register (`copy` / `paste` /
  `clear-copies`), and the fixup trio (`create-fixup`, `find-fixup-base`,
  `apply-fixups`).
- **stashes**: `rename`, `new-branch`, `open-diff`; **files**: the
  `stash-*` variants, `toggle-side`, `open-diff`.

Reach them through the row's context menu, where the row already has one,
and the palette, rather than new chrome.

## Out of this lane, and why

- **The patch builder** (`patch.*`, 16 commands). It needs a selection model
  in the diff view the window does not have yet, and that is design.
- **Navigation keys** (`search.next`/`prev`/`clear`, `select.mark`,
  `diff.next-hunk`/`prev-hunk`, `diff.toggle-line-selection`, `tab.*`,
  `input.newline`). In a mouse-first window most of these are a `cmd-`
  chord, a shift-click or a field behaviour. Which one is a keyboard-model
  decision for the owner, not a dispatch arm. List them in the report with a
  recommendation for each.
- **Bisect** (`commits.bisect-*`): its own surface. **Undo/redo**
  (`history.*`): needs the reflog section first.
- Any change outside `gui/`. New visual language: this lane closes a
  capability gap in the existing design.

## Verification

Two halves, and both are required for a row to count.

**Behaviour**: every dispatch arm you add gets a `#[gpui::test]` that runs the
command against a fake `Repo` and asserts the write or the refusal wording.
The existing fakes in `gui/src/main.rs`'s tests are the pattern, including the
one whose `AtomicBool` makes the next revert conflict.

**Sight**: `./dev verify`, headless, never `./dev gui`. For each slice, take a
shot of each new surface in the state it is about. Use a scratch repository
for anything that writes. A conflict, a half-finished rebase and a remote are
all a few lines of `git` in a `mktemp -d`; put those scripts in the report so
the shots can be retaken. **Read every PNG yourself** before calling a row
done. A step that passed only means the window answered, not that the result
looks right. The `verify-gitten` skill has the loop.

```sh
./dev lint
cargo fmt --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked -p gitten-gui
./check.sh                      # once, at the end
```

## Proof protocol

Two lanes in the last campaign fabricated completion reports. So:

1. End with verbatim `git log --oneline <base>..HEAD` and
   `git diff --stat <base>..HEAD`.
2. Name every claimed test from captured test output, and paste the gate
   tails with exit statuses.
3. Attach every shot path, the scratch-repo script that produced it, and one
   sentence on what each shot shows.
4. Paste `docs/features.md`'s gui-column dash count before and after.
5. Say plainly which rows landed, which are parked for design, and which are
   blocked on a seam that does not exist, with the signature you would need.
   **A smaller true report beats a larger false one.**
6. List where the agent tooling (`./dev verify`, the skills, the feature map)
   slowed you down or was wrong.

## Dispatch text

> You are the desktop pickup lane for gitten. Read
> `plans/005-desktop-picks-up-the-campaign.md` in full, then what it tells you
> to read. Work in your own worktree on `feat/desktop-pickup`, based where
> the plan says. Change `gui/` only. If a row needs anything outside it,
> record the contract you would need and move on. Slice 0 lands and is
> committed before any other slice. Every dispatch arm gets a headless test,
> and every surface gets a `./dev verify` shot you have looked at. Never open
> the window, and never push unless asked. Finish with the plan's proof
> protocol.
