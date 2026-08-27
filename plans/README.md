# Implementation Plans

Three passes so far:

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
