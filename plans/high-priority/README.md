# High-priority UI plans (pass 9 — the 2026-08-31 GUI design review)

Fifteen plans from a full review of the GPUI window: the screenshot walk,
the chrome, the five sidebar panes, the diff view, dispatch and input.
Written by two advisor sessions working in parallel (045–053 by one, 054–059
by the other, `full-44`); duplicates were reconciled before this index was
final. Everything already owned by plans 031–044 (truncation, armed-state
visibility, modal overlays, picker keyboard access, contrast sweeps,
dead-key hints) was deliberately **excluded** — nothing here double-plans
that work.

## Base — read before dispatching anything

Most plans were written against the working tree at `00842dc` on
`full/full` **with the design pass staged but not yet committed**; 052, 054
and 055–059 verified their excerpts against `635aba8` and carry a note
saying so. Before dispatching an executor:

1. Commit the staged design pass on `full/full`.
2. Tell each executor the resulting commit as its base. Each plan's drift
   check confirms the base has `fn row_bar` in `shell/src/views/diff.rs`
   (the marker that the design pass is present), or names its own check.

Line refs drift; every plan says to match on quoted content and to STOP on
a structural mismatch.

## The plans

| Plan | Title | Priority | Effort | Depends on | Hot files |
|---|---|---|---|---|---|
| 045 | Clicking a row selects it | P1 | M | none | sidebar views, main.rs |
| 046 | The sidebar spends its pixels where the user is | P1 | L | none | main.rs, chrome.rs, config |
| 047 | Branch marks tell the truth | P1 | M | none (054 reinforces) | branches.rs, git/src/lib.rs |
| 048 | `/` searches every list pane | P2 | M | none | main.rs, three views, command.rs |
| 049 | The wheel scrolls without taking the keyboard | P2 | M | none | main.rs |
| 050 | A context menu projected from the keymap | P2 | M | 045 | controls/menu, main.rs |
| 051 | The current hunk shows its extent; armed tint covers it | P2 | M | 031/038 landed | diff.rs, split.rs, markdown.rs |
| 052 | The title bar survives a narrow window | P1 | M | none | main.rs, controls.rs |
| 053 | Small polish batch (eight items) | P3 | S×8 | fence vs 059 | scattered |
| 054 | Branches in recency order, HEAD pinned first | P1 | S | none | git/src/lib.rs |
| 055 | The status pane earns its keycap | P1 | M | none | status.rs, main.rs |
| 056 | Commit columns align | P2 | S | none | commits.rs, graph.rs |
| 057 | Diff scrollbar hunk ticks | P2 | M | cross-ref 051 | diff.rs |
| 058 | Chrome metric unification | P2 | L | run last | chrome, views |
| 059 | Nits bundle (six items) | P1 | S×6 | fence vs 053 | scattered |

Dedupe fences: 053 and 059 each name the items the other owns — an executor
must not fix anything twice. 053 item 5 and 059's zero-count item edit the
same pane-header call sites; whichever lands second rebases.

## Dispatch order

`shell/src/main.rs` is the collision zone: 045, 046, 048, 049, 050, 052 and
055 all touch it. `diff.rs` is shared by 051 and 057. To keep merges cheap:

- **Wave 1 (parallel)**: 054 (git layer), 047 (branches.rs), 056
  (commits.rs), 059, 053 — small, mostly disjoint diffs; 053/059 respect
  their fences.
- **Wave 2 (serial through main.rs)**: 049 → 045 → 046 → 048 → 052 → 055,
  each rebased on the previous merge.
- **Wave 3**: 050 (needs 045), 051 then 057 (both in diff.rs, in that
  order).
- **Last**: 058 (metric unification sweeps whatever the others left; its
  header says run last).

Branch names: `advisor/ui-0NN-<slug>` as each plan states. Executors do not
push or open PRs unless the operator says so, and **never** launch
`./dev desktop` or `./dev tui` — `./dev dump` and the test suites are the
eyes (repo rule: never open a window unasked).

Status tracking: update the table below, not the top-level
`plans/README.md` (it is mid-edit by the owner; the owner will fold this
pass into it).

The fourteen executed plans moved to `plans/done/` on 2026-08-31 (the PR
numbers below are the record); 056 stays here — it is the DROPPED entry and
its file holds the STOP rationale.

| Plan | Status |
|---|---|
| 045 | MERGED (#40, test fix #41) |
| 046 | MERGED (#42) |
| 047 | MERGED (#38) |
| 048 | MERGED (#43) |
| 049 | MERGED (#37) |
| 050 | MERGED (#48) |
| 051 | MERGED (#47) |
| 052 | MERGED (#44) |
| 053 | MERGED (#39 items 2–8, #46 item 1; items 3 and 7's branches.rs bullet no-ops on the merged base) |
| 054 | MERGED (#35) |
| 055 | MERGED (#45) |
| 056 | DROPPED — executor STOP condition: the uniform-gutter fix reverses the trunk-readability decision at commits.rs:755-758 (the subject follows its own row's graph so a trunk commit reads from the left). Needs the plan owner to either accept trunk subjects behind the widest-merge gutter or withdraw the plan. |
| 057 | MERGED (#49) |
| 058 | MERGED (#50) |
| 059 | MERGED (#36) |
