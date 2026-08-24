# Roadmap — from viewer to product

A plan, not a description. [architecture.md](architecture.md) records what *is*;
this records the order to build what isn't, one self-contained block at a time.
Written 2026-08, when the read side was done and the write side was empty. A
plan that outlives its first few items is fiction — prune it as they land, and
distrust anything here that has drifted from the code.

Sizes are gut estimates of session scale (S ≈ an afternoon, M ≈ days,
L ≈ a week or more), not measurements.

## Where this stands

The viewer and its actor seams are assembled: repository access is one trait,
status separates staged/unstaged/untracked/conflicted paths without decoding
their bytes, GPUI dispatches named commands from the shared keymap, writes have
a serial background runner and invalidation generations, native text input can
join the mode stack, and the window has registered stacked panes with logical
focus. The commit and diff panes are the first two tenants.

The actor itself does not exist: **zero write verbs** in `gitten-git`, no
branch/stash/remote/tag/reflog reads and no files panel consuming the status
model. Lazygit is mostly actor — staging, committing, rebasing is its centre of
gravity, not reading logs. What follows closes that gap on seams that now exist.

## Phase A — seams (landed)

Blocks #1–#6 landed together: `Repo`, true porcelain-v2 status, GPUI command
dispatch, the job runner and generation refresh, native text input, and pane
layout/focus. They stay numbered because later entries refer to them; they no
longer compete with product work.

## Phase B — read models

Panels need data before verbs. Status (#6) already shaped the repository surface;
the remaining read models add tenants to it.

| # | Block | Lands | Notes | Size |
|---|---|---|---|---|
| 7 | **Branch + ref reads** | `gitten-git` | Local and remote branches, upstream, ahead/behind, HEAD. Refs are gix's home turf, so this is also the honest start of the gix port — no hot path exists yet to break | M |
| 8 | **Stash, remotes, tags, reflog reads** | `gitten-git` | Each small through the trait; each feeds a panel later | S each |
| 9 | **Diff cache keyed by blob OID** | `gitten-git`/acquisition edge | Prescribed in AGENTS.md, never built; acquisition already yields both OIDs. Pays twice: repeat views free, post-commit reloads re-diff only what changed | S |

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

Three standing notes. First, the `cli/` door stays optional for a desktop v1 —
build it whenever a command name first feels ambiguous, which is exactly when
it earns its keep. Second, rule 1 applies to every verb above: if a write op
exists, an extension reaches it through the same command name, or the seam is
wrong. The tracer bullet (#10) should prove that too — wire one command from
the registry, not around it.

Third, [competition.md](competition.md) holds the field notes on hunk: patch
input has landed, watch mode lands as consumer two of #3, agent annotations
wait for the extension host, and everything else there is sorted — smaller
gaps, and what not to take. Read it when planning a phase, not before.
