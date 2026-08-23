# Roadmap — from viewer to product

A plan, not a description. [architecture.md](architecture.md) records what *is*;
this records the order to build what isn't, one self-contained block at a time.
Written 2026-08, when the read side was done and the write side was empty. A
plan that outlives its first few items is fiction — prune it as they land, and
distrust anything here that has drifted from the code.

Sizes are gut estimates of session scale (S ≈ an afternoon, M ≈ days,
L ≈ a week or more), not measurements. Nothing here carries a number because
nothing here has been built yet.

## Where this stands

The viewer is assembled: commit graph with lanes, the diff pipeline (three
differs, two layouts, wrapping, intraline, rendered Markdown), selection and
copy, theming, config hot reload, command dispatch as data, three clients
proving the boundary. The actor does not exist: **zero write verbs** in
`plait-git`, no branch/stash/remote/tag/reflog reads, no staged/unstaged split
in what `pairs()` returns, no text input anywhere in `shell/`, one view filling
the window. Lazygit is mostly actor — staging, committing, rebasing is its
centre of gravity, not reading logs. What follows closes that gap without
disturbing the foundations, because the expensive bets (commands as names, one
acquisition layer, the client boundary) are already made correctly.

## Phase A — seams

Cheap while nothing sits on them; expensive once ten features do. Strictly
before everything else.

| # | Block | Lands | Why now | Unblocks | Size |
|---|---|---|---|---|---|
| 1 | **Repo access trait** | `plait-git` | Five free functions today (`log`, `pairs`, …). One surface so reads (someday gix) and writes (binary) hide behind it; frontends never learn which ran | every later item plugs in here | S |
| 2 | **GPUI adopts `core::command` dispatch** | `shell` | Still GPUI's action system; every verb added before the migration costs three re-bindings after. Do it once, then each new command gets `[keys]`, help panel and extension reach for free | all of D | M |
| 3 | **Job runner + invalidation generation** | `app`/`shell` (never `core` — no I/O there) | Writes are processes taking seconds; they cannot block render. Queue → run → completion event → bump a generation → affected views re-acquire → `session.rs` restores selection | every write verb | M |
| 4 | **Text input block** | `shell`, mode on the existing mode stack | No text field exists anywhere in the shell. Consumer #1 is the commit message; #2 the search prompt (#17) | #10, #17 | M |
| 5 | **Pane layout + focus model** | `shell` | One view fills the window; lazygit *is* a focus-switching pane grid. Two stacked panes and a focus ring is enough to start. Already listed under "Panes" in [architecture.md](architecture.md); the diff view measures its own box, so tenants exist ([decisions/0017](decisions/0017-wrapping-is-more-rows-not-taller-ones.md)) | Files/branches/stash panels | L |

## Phase B — read models

Panels need data before verbs. Independent of A except #6 shapes #1's surface.

| # | Block | Lands | Notes | Size |
|---|---|---|---|---|
| 6 | **True status model** | types in `core`, parsing in `plait-git` | porcelain v1 today folds untracked into one pair set; no XY codes, no renames (`git/src/lib.rs:247`). Parse `--porcelain=v2` into staged / unstaged / untracked entries. **Do in the same pass as #1** — the model defines the trait's surface, and a seam shaped against real data beats a revised one | S/M |
| 7 | **Branch + ref reads** | `plait-git` | Local and remote branches, upstream, ahead/behind, HEAD. Refs are gix's home turf, so this is also the honest start of the gix port — no hot path exists yet to break | M |
| 8 | **Stash, remotes, tags, reflog reads** | `plait-git` | Each small through the trait; each feeds a panel later | S each |
| 9 | **Diff cache keyed by blob OID** | `plait-git`/acquisition edge | Prescribed in AGENTS.md, never built; acquisition already yields both OIDs. Pays twice: repeat views free, post-commit reloads re-diff only what changed | S |

## Phase C — tracer bullet

**10. Stage/unstage a file + commit with message.** Not a feature — a test of
the frame. One verb through every layer at once: command name → keymap → job
runner → binary write → generation bump → status re-acquire → message typed via
the input block → diff view shows the result. Items 1–9 exist precisely so this
is assembly rather than invention; wherever it fights back, the flaw is
upstream, and fixing it now costs one verb instead of fifteen. *M*

Everything in D hangs off C independently, so the product is usable after any
subset — stop anywhere and keep what you have.

## Phase D — verb breadth

| # | Block | Notes | Size |
|---|---|---|---|
| 11 | **Discard changes · stage/unstage all · ignore file** | file-level verbs over the C rails | S |
| 12 | **Checkout · create · delete · rename branch** | fills the branches panel's action column | S/M |
| 13 | **Push · pull · fetch** | progress/status chip in the existing titlebar strip | M |
| 14 | **Stash · pop · apply · drop** | | S |
| 15 | **Reset soft/mixed/hard · revert · amend HEAD** | amend rides #10's commit path | M |
| 16 | **Hunk staging** | selection in diff view → synthesized patch → `git apply --cached`. The edit script already yields hunk boundaries, so this is selection plumbing plus patch emission; line-level staging follows on the same rails | M/L |
| 17 | **Search/filter over commits** | `/` prompt; consumer #2 for the input block | S |

## Phase E — genuinely hard

The long tail. Each is its own vertical slice; none blocks another, and none is
a verb over existing rails.

| # | Block | Why it is hard |
|---|---|---|
| 18 | **Interactive-rebase todo editor** | An editable ordered list (pick/reword/squash/fixup/drop), driven by pointing `GIT_SEQUENCE_EDITOR` back into the app. New UI paradigm, not a new verb |
| 19 | **Conflict merge editor** | Inline three-way resolution. Lazygit's actual differentiator and the hardest thing on this page |
| 20 | **Cherry-pick register · bisect · worktrees · submodules · tag UI** | Long tail; each a slice |

## Deliberately not on this list

Tracked elsewhere, so they are not re-planned here: `cli/`, extension loading,
code hot reload, the gix port beyond its start in #7, semantic diffs,
`\ No newline at end of file`, cross-file move detection, GPUI render tests,
Linux builds — see [architecture.md](architecture.md) § "Not built yet", which
is the canonical record of gaps; update both together.

Two standing notes. First, the `cli/` door stays optional for a desktop v1 —
build it whenever a command name first feels ambiguous, which is exactly when
it earns its keep. Second, rule 1 applies to every verb above: if a write op
exists, an extension reaches it through the same command name, or the seam is
wrong. The tracer bullet (#10) should prove that too — wire one command from
the registry, not around it.
