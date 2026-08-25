# Roadmap — from viewer to product

A plan, not a description. [architecture.md](architecture.md) records what *is*;
this records the order to build what isn't, one self-contained block at a time.
Written 2026-08, when the read side was done and the write side was empty. A
plan that outlives its first few items is fiction — prune it as they land, and
distrust anything here that has drifted from the code.

## Where this stands

The viewer *and* the actor are one program now. Every read goes through the
object-safe `Repo` trait behind a retained `Handle`; every write is a verb on
that same trait, through the `git` binary, run serially by app's job runner,
where each finish — a refusal as much as a success — bumps an invalidation
generation every repository pane re-acquires on. Status separates staged/
unstaged/untracked/conflicted; branches, stashes, remotes, tags and the reflog
have read models in `core::refs`; and the window is lazygit-shaped: Files,
Branches and Stashes panes in a focus ring, `/` search over commits, hunk
staging from the diff view, messages typed into a native input slot,
destructive verbs asked twice.

What remains below is the long tail. Nothing left lands by assembly.

## Phase A — seams (landed)

Blocks #1–#6 landed together: `Repo`, GPUI command dispatch, the job runner
and generation refresh, the text input block, pane layout/focus, and true
porcelain-v2 status. They stay numbered because later entries refer to them;
they no longer compete with product work.

## Phase B — read models (#7–#8 landed)

Branch + ref reads (#7) and stash/remotes/tags/reflog (#8) landed together with
the panes that consume them: the models are `core::refs` — names as bytes,
absence as data — the reads are trait methods, and Files, Branches and Stashes
are tenants of the pane registry. One block escaped and stays open below: #9.

## Phase C — tracer bullet (#10 landed)

Stage/unstage a file, then commit with a typed message, went through every
layer at once: command name → keymap → job runner → binary write → generation
bump → status re-acquire → input block → diff view shows the result. Amend
rode the same rails. It fought back exactly where predicted — upstream, at the
seams — and those fixes are in the code, not worth retelling here.

## Phase D — verb breadth (#11–#17 landed)

File verbs (#11 discard · stage-all · ignore), branch verbs (#12 checkout/
create/delete/rename), sync (#13 push/pull/fetch), stash (#14 push/apply/pop/
drop), reset soft/mixed/hard · revert · amend (#15), hunk staging (#16:
selection plumbing plus `core::patch` emission over `git apply --cached`,
space/u/D in `[diff]`; line-level staging follows on the same rails when
asked for) and search over commits (#17: `/` over a folded index,
`core::search`). Each hung off C independently, so any subset could have been
the last.

## Still open

The long tail. Each is its own vertical slice; none blocks another, and none
is a verb over existing rails.

| # | Block | Why it is hard |
|---|---|---|
| 9 | **Diff cache keyed by blob OID** | Prescribed in AGENTS.md, never built; acquisition already yields both OIDs, and a blob never changes. Pays twice: repeat views free, post-write reloads re-diff only what changed |
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
wrong. #10 proved that too — an extension command stages through the same
`Pane::run` slot and the same `app::verbs::Write` jobs `files.stage` uses.

Third, [competition.md](competition.md) holds the field notes on hunk: patch
input has landed, watch mode lands as consumer two of #3, agent annotations
wait for the extension host, and everything else there is sorted — smaller
gaps, and what not to take. Read it when planning a phase, not before.
