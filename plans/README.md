# Implementation Plans

Passes so far, and pass 5 open (020–022, the 2026-08-28 audit wave):

- **Pass 1** (2026-08-24, audited at `3a8b347`): plans 001–007. All landed on
  main and merged — verified 2026-08-26 against `2dfcb82`
  (`aa936f7`, `e25056b`, `db2b4fb`, `c857964`, `a020a3d`, `3d547ae`, `fe6cd74`).
- **Pass 2** (2026-08-26, audited at `2dfcb82`): plans 008–012. Executed by
  subagents in isolated worktrees, adversarially reviewed, and **all merged
  into main** on 2026-08-26 — see the status table and the follow-ups section.
- **Pass 3** (2026-08-27, planned at `67fee3d`): plans 013–015 — the terminal
  client catches up with the window. Plans drafted by Codex from live source,
  baseline facts independently verified by the integrator, executed by OpenCode
  subagents in isolated worktrees off `full/tui`, adversarially reviewed
  (GREEN / CONCERNS-dispositioned / CONCERNS-dispositioned), folds applied,
  merged into `full/tui` in this tree. Integrated gate: fmt, workspace clippy
  `-D warnings`, and `cargo test --workspace` all green (1,086 tests).
  **`full/tui` is complete and awaiting its owner's merge decision**; this tree
  is the integration line and nothing has touched `full/full`.
- **Pass 4** (2026-08-27/28, planned at `15bff4a`): interface parity — COMPLETE.
  016 (the pane focus ring), 017 (files), 019 (stashes) and 018 (branches) are
  implemented, adversarially reviewed, folded where the findings earned it, and
  merged into `full/tui` in this tree. The terminal is lazygit-shaped: five
  tenants (files, branches, commits, stashes + the diff main), every verb the
  window ships reachable through the same command names, the same
  `gitten.toml`, the same keymap. Rebase-onto/abort/continue are deliberately
  deferred as one named follow-up ("TUI rebase lifecycle") with a scope-fence
  test pinning the gap. **`full/tui` awaits its owner's merge decision**;
  nothing has touched `full/full`.

- **Pass 5** (2026-08-28, audited at `eb888e1`): plans 020–022 from the
  terminal-vs-window audit. **The maintainer's working tree moved during
  execution** — the scrollbar-indicator refactor (decision 0027) grew the
  uncommitted state from three to eight tui files — so each advisor branch's
  `carry:` commit is a snapshot of all eight files taken 2026-08-28 late
  (`target/gitten-wip3.patch`), rebuilt after two bootstrap defects the
  executors correctly STOPPED on (a patch-file carry, then a partial carry).
  docs/decisions 0027 + its README edit remain live-tree only (docs, not
  build-relevant). 020 (transactional terminal entry) verdict **APPROVE**;
  021 (`diff.discard-hunk` + command-parity guard) re-dispatched on the
  repaired carry; 022 (async loads/refreshes) branches from 021. Direction
  items surfaced but not planned this wave: TUI status pane, live algorithm /
  whitespace pickers, cursor-follow diff preview, a grapheme-aware prompt
  editor, and the armed-hunk render tint (deferred in 021 — a `Frame` field
  ripples through every presentation). **Merging is the owner's; the advisor
  never merges.** Merge order: commit or stash the working-tree WIP first,
  then merge `advisor/020` and `advisor/022` (`advisor/022` contains 021);
  the branches' carries and your local WIP are the same content at the same
  snapshot, so the merge is a fast-forward-shaped no-op for the carried files.

- **Pass 6** (2026-08-30/31, audited at `87229df` on `main`): plans 023–030.
  A full-repo audit — correctness, perf, complexity, UI and docs — run by an
  advisor with four parallel read-only subagents, every finding vetted against
  the code before planning. **Two base-branch mistakes shaped this pass and
  are worth knowing about**: it was first audited and executed against `main`
  (127 commits stale), then re-targeted onto a *local* `full/full` that was
  itself 60 commits behind `origin/full/full`. Consequences: plan 026 lost half
  its scope (the widest-row measurement it fixed had already been deleted with
  the sideways scroll), plan 024's call sites had to be re-derived rather than
  replayed against the rewritten markdown/tui code, and these plans were
  renumbered from 013–020 to **023–030** on discovering the collision with
  passes 3–5. Anything below still marked TODO was planned against `main` and
  **must be drift-checked against `origin/full/full` before execution**.
  023, 024 and 026 are **merged into `full/full`** (PRs #23/#24/#25); each was
  independently re-verified by the reviewer on the final base, including
  re-running every new test's mutation check.

## Pass 4 follow-ups — surfaced by reviewers, recorded not folded

- A Down-Down-without-Up (mouse protocol violation by the terminal) leaves a
  stale one-row selection on the abandoned pane; self-heals on the next press,
  no cross-pane splice. Only worth fixing with evidence a real emulator does it.
- Pass-3's recorded follow-ups (gpgsign in `Scratch`, `coords` doc at context 0,
  CJK-in-squeezed-cells, SGR 58 eyeball) remain open.

Execute pass 3 in the order below unless dependencies say otherwise. Each
executor: read the plan fully before starting, honor its STOP conditions; the
integrator owns this table and updates it as waves land.

## Execution order & status

| Plan | Title | Priority | Effort | Depends on | Status |
|------|-------|----------|--------|------------|--------|
| 001 | Resolve the repo top level before joining working-tree paths | P1 | S | — | DONE (merged as `aa936f7`) |
| 002 | Terminate git's option parsing before the user revspec | P1 | S | — | DONE (merged as `e25056b`) |
| 003 | Harden the unified-patch parser (hunk coords, `--`/`++` lines, spaced paths) | P1 | S–M | — | DONE (merged as `db2b4fb`) |
| 004 | Pin the GPUI git dependencies to a rev | P1 | S | — | REJECTED (rev-pin duplicates the zed tree) → replaced by an `AGENTS.md` "never bare `cargo update`" note |
| 005 | Isolate the web request handler from panics; bound the head read | P2 | S | — | DONE (merged as `c857964` + `a020a3d`) |
| 006 | Stop re-running `prepare` on a layout toggle | P2 | M | — | DONE (merged as `3d547ae`) |
| 007 | Run the desktop crate's tests in CI | P2 | S–M | — | DONE (merged as `fe6cd74`; promoted to a required gate) |
| 008 | Render a real diff for merge commits | P1 | S | — | MERGED (`7344f82` + followup `28707d6`, via `7ca04b8`) |
| 009 | Negotiate bracketed paste so pasting is safe in the terminal | P1 | S | — | MERGED (`bc130fb`, fast-forward) |
| 010 | Pick the commit list's widest row in characters, not bytes | P2 | S | — | MERGED (`d485bcb`, via `54f9272`) |
| 011 | Author colours read the live theme | P2 | S | 010 softly | MERGED (`b7b0d75`, same merge commit) |
| 012 | A font edit in gitten.toml rebuilds the diff presentation | P2 | M | — | MERGED (`dc14543` + followup `9a860a0`, via `d0b1ca5`) |
| 013 | Incremental commit search in the terminal (`/`, shared `core::search`) | P1 | M | — | DONE (pass 3, `369503a`+2) — adversarially reviewed **GREEN**; 5 non-blocking notes accepted |
| 014 | Stage/unstage the selected hunk from the terminal diff | P1 | M | — | DONE (pass 3, `b234f62`+5, fold `196704f`) — reviewed CONCERNS (all non-blocking); 3 folds in |
| 015 | Render the shared Markdown presentation in the terminal | P2 | L | — | DONE (pass 3, `efe00bb`+3, folds `23e7672`) — reviewed CONCERNS (all non-blocking); core markdown.rs is a pure addition; 2 folds in |
| 016 | The terminal pane focus ring (foundation for lazygit-shaped TUI) | P1 | L | 013–015 | DONE (pass 4, `0cfbb41`+4, fold `a1c5b2d`) — reviewed CONCERNS (all non-blocking); 2 folds in; executor's "2-file scope" claim was false (7 files, all inside plan authorization — test/harness fallout), recorded |
| 019 | The terminal stashes pane (apply/pop/drop/push) | P1 | S–M | 016 | DONE (pass 4, `ba49f9c`+3, fold `754c032`) — reviewed CONCERNS (all non-blocking); 2 folds in (unavailable-state regression test + unfocused armed-ink assert); drop-arm renumbering probed and unbroken |
| 017 | The terminal files pane (sections, all file verbs, commit/amend prompts) | P1 | L | 016 | DONE (pass 4, `c99cc1f`+3, fold `20b1415`) — reviewed CONCERNS (all non-blocking); 3 folds in; merged with the stashes tenant at `e58cd1d` |
| 018 | The terminal branches pane (checkout/new/rename/delete/tag; rebase deferred as a named lifecycle follow-up) | P1 | L | 016, 017 | DONE (pass 4, `91e3876`+5, fold `4b65866`) — reviewed CONCERNS (all non-blocking); integrator ruling: the destructive arm outlives focus round-trips on every pane, matching the window (the ctrl-j bypass finding became the consistency fix); zero-alloc armed matching |
| 020 | Transactional terminal entry — a failed start cannot leave raw mode on | P1 | S | — | DONE (advisor/020-transactional-terminal-entry, 3 commits on the 8-file carry) — verdict APPROVE: all gates re-run by reviewer (tests 175 lib, clippy, fmt clean), scope clean, diff read; one report inaccuracy (test count "174→178"; actual 171→175), work unaffected; branch carry repaired by orchestrator |
| 021 | `diff.discard-hunk` in the terminal + the client command-parity guard | P1 | M | — | DONE (advisor/021-tui-discard-hunk-parity-guard, 6 commits on the 8-file carry) — verdict APPROVE: gates re-run by reviewer (182 lib + 60 bin tests, clippy, fmt), scope clean (diff.rs + main.rs), diff read, sabotage re-verified by reviewer (guard fails "copy.selection lost its arm" under a renamed arm, recovers on revert). Judgment calls accepted, documented: `view.*` is ten names not twelve (derived from real arms); four diff-pane names sweep after `diff.focus`; fake `Repo` gained a test-only `discard_patch`; job built before the arm is spent (a tightening of the window's order) |
| 022 | Repository loads and refreshes off the terminal loop (loader thread, supersede guards) | P1 | L | 021 | DONE (advisor/022-tui-async-loads, 6 commits on 021's tip) — verdict APPROVE: gates re-run by reviewer on a cold isolated target dir (182 lib + 63 bin, clippy, fmt), scope read hunk-by-hunk (apply-time guards: batch id, target generation, request id; first-error stands; FIFO composition order documented), migrated assertions verified verbatim, both new guard tests assert real state. Accepted deviations, documented: `Screens` not `Send` → `Tenant` extraction + associated `acquire_snapshot` on the loader thread (the window's own model); snapshot variants carry labels (behavior-preserving); `catch_unwind` in the loader mirroring the write Runner; one scope extension — `Panes::iter_mut` deleted (4 lines, dead after step 3, caught by the plan's own clippy gate) |
| 023 | Make the differ-vs-git check a gate instead of a printout | P1 | M | — | **MERGED** (pass 6, PR #23, `128f061`, merge `356342a`) — all 5 CI jobs green incl. the new `diffcheck` job. Fixed two contradictions in the checker on the way: hunk positions are now compared only between same-size scripts, and myers' exact-count check stands down past its step budget (see 030) |
| 024 | Guard the runs seam against mid-character offsets | P1 | S | — | **MERGED** (pass 6, PR #24, `69217a4`, merge `038d0ad`) — 388 core tests, 13 workspace binaries; guard mutation-validated by the reviewer on the final base. Closes three latent render-path panics (BUG-05/BUG-11/BUG-14) |
| 025 | A startup failure opens the window and says so | P1 | M | — | TODO — **planned against `main`; drift-check before executing** |
| 026 | A font edit keeps your selection and your place in the diff | P2 | S | — | **MERGED** (pass 6, PR #25, `5748465`, merge `cb6ceda`) — 330 shell tests, mutation-validated. Its commit-list half was dropped: `full/full` had already deleted the widest-row measurement |
| 027 | The overflow lane clears the furniture contrast floor | P2 | S | — | **REJECTED** — premise contradicts decision 0020, which exempts `lane_overflow` by name as a stroke with no legibility floor. The 1.87:1 measurement is real; the conclusion was not. Caught by the executor at its STOP condition before committing. Replaced by 029 |
| 028 | Make the docs say what the code does | P2 | M | — | TODO — **partly obsolete**: 023 already fixed `check.yml`'s stale job list, and the Linux/keyboard claims may have been overtaken on `full/full`. Re-audit rather than execute |
| 029 | Author initials clear a contrast floor, because they are text | P3 | S | replaces 027 | TODO — the surviving half of 027: initials are glyphs on a background they do not choose, and 0020 does not cover them. A guard for hand-written palettes, not a fix |
| 030 | Surface budget exhaustion, so the checker can hold myers exact again | P3 | M | 023 | TODO — restores the exactness 023 had to drop. Referenced by name in `git/examples/diffcheck.rs` |

## Pass 3 follow-ups — surfaced by reviewers, pre-existing, not pass regressions

- `app/src/acquire.rs` `Scratch::git` pins identity via `-c` but not `commit.gpgsign`; a machine with global
  signing fails fixture setup. Named by plan 014's own stop conditions; one-line fix whenever next touched.
- `core/src/patch.rs` `coords` doc claims only whole-file selections can empty a side — false at
  `[diff] context = 0`, where an all-addition hunk in a tracked file emits `--- /dev/null` + `@@ -0,0`
  and real `git apply --cached` refuses. Shared window+terminal behavior by design (git's own refusal
  surfaces); the *doc* is the bug. Verified against the real backend by the 014 reviewer.
- CJK text inside a *squeezed* table cell shears the grid in display columns — `flow_row` pads by
  `chars().count()` (core markdown flow, unchanged, both clients affected), and the terminal module
  doc records the inherited caveat. Same root as the known characters-versus-columns wrap seam.
- The SGR 58 table hairline: the byte sequence and the SGR-4 fallback are pinned by tests, but its
  rendering in a terminal without underline-colour support deserves one live eyeball (`./dev dump`)
  before pass 3's merge is forgotten.

## Adversarial-review follow-ups — RESOLVED

After execution, four read-only adversarial reviewers probed each branch with
real git scratch repos and dependency-source reads. Verdicts: GREEN (008),
GREEN (009), CONCERNS-all-nonblocking (010/011), GREEN (012). Disposition:

- **Folded into branches before merging:** 008's git ≥ 2.31 floor note,
  corrected `[diff] diffMerges` rationale (measured NOT reproducible on git
  2.55 — combined raw records belong to `diff-tree -c`, which this crate never
  runs; the parser guard stays as belt-and-braces), and diffcheck's bare-
  revision oracle harmonized to first-parent (`28707d6`). 012's font fingerprint
  seeded at construction, deleting the one-time double-arrange (`9a860a0`,
  test now asserts `builds == 1`).
- **Post-merge hardening on `advisor/adversarial-followups`** (ff-merged:
  `3df7d76`, `f00ecf1`, `777d959`): crossterm pinned with
  `features = ["bracketed-paste"]` (load-bearing comment — a future
  default-feature slim would silently resurrect pasted-key typing); pinning
  test `translate_event(Event::Paste("q")) == None`; Term's struct-doc mode
  list refreshed; `estimated_row_width`'s doc softened to what chars-ranking
  actually delivers.
- **Filed as issues:** East-Asian display-width table in `core`
  (chrischriscris/gitten#20); anchor-semantics decision for font rebuilds
  (chrischriscris/gitten#21).

## Dependency notes

- 022 branches from 021's advisor branch (both rework `tui/src/main.rs`'s
  job/refresh paths, and 022's tests must cover the discard verb's refresh
  path). 020 is independent and may land in either order.
- No hard ordering among 008–012; they touch disjoint files except 010→011
  (`shell/src/views/commits.rs`), which is why 010 is listed first.
- Do 008 before any future golden-corpus work: the merge-record case it fixes
  belongs in that corpus's first batch.
- 006 landed large changes in `shell/src/views/diff.rs`; if executing 012 much
  later, re-run its drift check first.

## Findings surfaced but NOT turned into plans

Both passes vetted these; left out for leverage. Still open unless noted.

- **BUG-05**: `Differ`/`Highlighter` output not range-validated like `Wrap`
  output (`core/src/differ.rs` ~1700, `core/src/prepared.rs` ~177); `verify`
  helper exists only under `#[cfg(test)]`. Effort M.
- **BUG-08**: wrap budget counts chars, terminal draws cells → CJK overwrap
  (`core/src/wrap.rs` ~196, `tui/src/rows.rs` ~498). Needs a width fn threaded
  through the `Wrap` seam without adding a dep to `core`. Effort M.
- **BUG-09**: `FileDiff.path` = `Pair::label()` ("old → new") misroutes
  highlighting/specialized views when the NEW name has no extension
  (`git/src/lib.rs` ~403). Real fix threads a separate label through `Present`.
  Effort M.
- **BUG-10**: `compact_with` clamps slide vs neighbours for deletions only,
  not insertions (`core/src/differ.rs` ~1283). Effort S, MED confidence.
- **BUG-11**: `Selected::range` clamps length but not to char boundaries
  (`core/src/select.rs` ~167). Effort S.
- **BUG-12**: intraline/syntax timings summed across worker threads, reported
  as wall time (`core/src/prepared.rs` ~303). Metrics-only. Effort S.
- **Unmerged/conflicted paths render as pure additions** with out-of-alphabet
  status `U` (`git/src/lib.rs` synthetic path ~495–503, working-tree fallback
  ~720). Proper fix stages triplets via `ls-files -u`. Effort S–M. Worth a doc
  comment at minimum even if deferred.
- **PERF** items from pass 1 remain open: per-row clones in
  `shell/src/views/commits.rs:167-190`; tui per-cell String allocations;
  title-bar strip allocation per frame (`shell/src/main.rs` ~447); blob-OID
  diff cache still unbuilt (roadmap B#9).
- **Command-registry ↔ client-dispatch cross-check test**: nothing fails when
  core renames a command a client dispatches (`_ => {}` swallow). Start with
  tui (fully implements the registry); shell documents the gap until roadmap
  A#2. Effort S–M. Grew more valuable with every planned Phase C/D verb.
- **diffcheck golden corpus in CI**: distill ~20 small cases (inputs +
  git-captured expected hunk positions) into ordinary tests; measurements.md
  credits this harness with bugs invisible to unit totals. Live-repo diffcheck
  stays local (network-bound). Effort M.
- **DOC drift** (verified both sides 2026-08-26): README claims every client
  reads `[keys]` (false for desktop, `docs/clients.md` contradicts) and "nobody
  has compiled it on Linux" (CI compiles+runs it); `docs/architecture.md`
  §Not-built-yet contradicts shipped code five ways incl. its own tui table and
  the Linux bullet; `docs/extending.md` omits three real `Host` fields
  (`view`, `mouse`, `themes`) while using one; decisions 0024/0025 describe a
  two-job CI (four now); orphaned `docs/interactive/index.html` freezes stale
  numbers. `.claude/`, `plans/`, `registry-linux/`, `target-linux/` are
  untracked but unignored (git-status noise / accidental `add -A` risk).
  Effort S each, M bundled.

## Direction options (pass 2)

Maintainer's call, evidence-backed: repo-access trait + porcelain-v2 status
model (roadmap A#1+B#6, same pass); GPUI adopting `core::command` dispatch
(A#2 — would also make README's `[keys]` claim true); text input block + commit
search (A#4+#17 — most felt gap on the 82k-commit fixture). See
`docs/roadmap.md` for orderings and trade-offs.

## Findings considered and rejected

Pass 1:
- Web attribute-quote XSS, Host/DNS-rebinding, CSP/`nosniff`, oversized-head
  desync: fixed before planning (see 005).
- TUI escape-sequence injection defended; json escaping, align invariant,
  scrollbar guards, wide-char continuation, BlobStream ordering verified
  correct.
- Config-from-hostile-cwd subversion (SEC-07): format is data-only today;
  revisit before the config grows a plugin-pointer field.

Pass 2:
- Promoting cargo-audit to a gate: declined upstream for sound reasons (GPUI
  pin churn breaks builds; report-as-summary is right). Not re-litigated.
- Fuzzing the TOML config parser: industry parser + 34 table-driven tests;
  pre-release adversarial fuzz buys little.
- Porting MarkdownRows to the tui unasked: implementation-only scope per
  AGENTS.md client rule.
- Full diffcheck against blobless fixtures in CI: network-bound; use a golden
  corpus instead (above).
- Web: memoizing `(cols, wrap)` reflow across tabs — proof client, single user,
  localhost; the clamp on request params already bounds worst case. Not worth
  the code today.
