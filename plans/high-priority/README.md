# Pass 10 — performance and complexity

Seven plans from a full-repo audit scoped to **performance and complexity** at
the owner's request, planned against `da9f8a7` on `full/full`.

## How this wave was produced, and what that means for you

The audit ran with three parallel read-only scouts; **all three died on a model
rate limit before reporting**, so the whole survey was redone by the advisor
directly. Every excerpt in every plan below was opened and read by the advisor
at `da9f8a7` — none came from a scout's report. The upside is unusual
confidence in the line numbers; the downside is narrower coverage than a
four-scout wave, recorded honestly in "What was not audited" below.

`shell/src/main.rs` and `git/src/lib.rs` both moved during the audit session
(`00842dc` → `da9f8a7`, 20 commits including the whole pass-9 UI wave). Every
finding was re-verified at `da9f8a7` afterwards. **`core/src/lib.rs`,
`core/src/differ.rs` and `core/src/prepared.rs` did not change in that window**,
which is why plans 060–063 carry the most precise line numbers here.

## A note on this directory

`plans/high-priority/` was **deleted at `da9f8a7`** ("plans: fold pass 9's status
table into the index, drop plans/high-priority") and recreated for this wave
because the owner asked for these plans here by name. If the fold was the
intended long-term shape, moving these seven into `plans/` and folding this table
into `plans/README.md` is one `git mv` and costs nothing.

## The plans

| Plan | Title | Priority | Effort | Risk | Depends on | Hot files | Status |
|---|---|---|---|---|---|---|---|
| 060 | Trim the common head and tail before the intraline LCS table | P1 | S–M | LOW | — | `core/src/lib.rs` | DONE |
| 061 | Stop interning every line twice under a whitespace relation | P2 | M | MED | — | `core/src/differ.rs` | DONE |
| 062 | Make `prepare`'s unit of work a hunk, so one big file uses every core | P2 | M | MED | 060 (soft) | `core/src/prepared.rs` | DONE |
| 063 | Hold a hunk's tokens and spans in one buffer, not a box per line | P3 | L | MED–HIGH | 062 (soft) | `core/src/prepared.rs`, `core/src/markdown.rs`, all clients | REJECTED — median peak RSS rose 975.8 MB → 977.0 MB; rolled back |
| 064 | Stop spawning `rev-parse --git-path` on every in-progress check | P3 | S | LOW | — | `git/src/lib.rs` | DONE |
| 065 | Break the 3,200-line `impl DevShell` into modules by responsibility | P1 | M | LOW | — | `shell/src/main.rs` | REJECTED — Rust requires moved private methods to gain visibility |
| 066 | One verb, written once — the tracer bullet for a shared verb seam | P2 | M | MED | 065 (soft) | `core/src/refs.rs`, `app/`, both `main.rs` | DONE |

Status values: TODO | IN PROGRESS | DONE | BLOCKED (one-line reason) |
REJECTED (one-line rationale).

## Dispatch order

The four `core` plans and the two client plans touch disjoint files, so the two
tracks run in parallel.

**Track A — the load path (`core`), serial:**

1. **060 first.** It is the largest measured win for the smallest change, and it
   adds `intraline_with`, which 062's Step 5 adopts.
2. **061** is independent of 060 and may run beside it — different file. Listed
   second only because 060 has the better ratio.
3. **062** after 060 (soft: its Step 5 is a clean skip if 060 has not landed).
4. **063 last**, and only after its own Step 1 gate reports. It is the one plan
   here with a real chance of being abandoned on its findings, which is why that
   gate exists.

**Track B — the clients:**

5. **065 before anything else that edits `shell/src/main.rs`**, including any
   revived pass-9 work. It is a pure move and every day it waits it costs another
   rebase.
6. **066** after 065, so its window half edits a small file rather than a
   3,200-line block.

**064** is independent of everything and can land at any point.

## What a dispatcher should know

- **060, 061, 062 all report a measurement.** `docs/measurements.md` is strict
  about methodology — ABBA-interleaved rounds, flipped starting side, medians,
  settle gaps — because naive back-to-back A/B on this machine swung **+25–95%**
  from allocator state alone (`docs/measurements.md:376-383`). The plans say so;
  hold executors to it.
- **`diffcheck` is the gate for 060 and 061**, and it compares hunk *positions*,
  not just counts. `docs/measurements.md` records two bugs that were invisible in
  the counts and showed up only in the positions.
- **065's whole product is reviewability.** Its done criteria require that the
  `+` lines in `main.rs`'s diff contain nothing but `mod` declarations and prose.
  A reviewer who checks only "tests pass" has not reviewed it.
- **066 deliberately stops after one verb** and its report is the deliverable.
  An executor that ports five has failed the plan, not exceeded it.
- **Fixtures are not committed.** `./fixtures/fetch.sh` downloads them
  (network-bound, minutes); `./fixtures/gen.sh <n> <m>` makes synthetic ones
  offline. 062 needs a *single-file* input and shows how to build one from this
  repo's own history without the network.

## Verified as already fixed — do not re-report

Checked at `da9f8a7` against findings carried in `plans/README.md`:

- **Per-row clones in the commit list** — `Commits` now holds `Rc<Data>` and
  `Rc<Vec<usize>>`, and relative times, author initials, the search index and the
  sha column width are all computed once at load
  (`shell/src/views/commits.rs:61-100`, `560-612`).
- **The tui's per-cell `String` allocations** — `screen::Cell` is `Copy` and 12
  bytes, the flush is damage-tracked with run coalescing and SGR dedup, and
  characters go out through `encode_utf8` into a stack buffer over a `BufWriter`
  (`tui/src/screen.rs:141-152`, `247-300`).
- **The widest-row measurement** — deleted along with the sideways scroll.
- **The blob-OID diff cache** — landed (`docs/roadmap.md` #9).
- **Unresolved merge-conflict markers in `plans/README.md`** — resolved by the
  owner in the pass-9 restructure. `grep -rn "^<<<<<<<"` over the tree is clean.

## Findings considered and rejected

Measured and left out for leverage. Recorded so nobody re-audits them.

- **The title strip rebuilds its five pickers per frame** and clones every theme
  name into a `Vec<String>` (`shell/src/main.rs:3894-3990`, now `strip` at
  `:4231`). Real, but O(1) in registry size — tens of small allocations against a
  render that only runs when something changed. Not worth the code.
- **The commit list's armed-row lookup is an O(n) sha scan per frame**
  (`shell/src/views/commits.rs:645-648`). Only runs while a reset is armed, which
  is a transient state ended by any cursor move. Not worth the code.
- **"Reads spawn the `git` binary"** — not a finding. It is a documented, chosen
  design (`AGENTS.md`, `docs/roadmap.md`), and the gix port is tracked
  separately. 064 removes only *repeated* spawns for an answer that cannot have
  changed; it does not start a port.
- **`git/src/lib.rs` is 7,676 lines** — by design of the acquisition layer, and
  3,051 of those are production code with the rest tests. No clean seam worth the
  churn was found.

## What was not audited

Say so plainly, since this wave lost its scouts:

- **Correctness, security and test coverage** were out of scope by request and
  were not swept. The open BUG-05/08/09/10/11/12 items in `plans/README.md` were
  not re-verified.
- **`web/`** was skipped entirely — it is a proof, not a plan.
- **`core/src/markdown.rs` (2,925 lines) and `core/src/syntax.rs` (2,131)** were
  read only where 063's ripple reaches them. Neither was audited for its own
  complexity, and both are large enough to deserve it.
- **`core/src/command.rs`, `core/src/graph.rs`, `core/src/wrap.rs`,
  `core/src/theme.rs`** were not opened. `graph.rs` and `wrap.rs` in particular
  are on measured hot paths and had no attention this wave.
- **The tui's pane/verb layer** (`tui/src/panes.rs`, `files.rs`, `branches.rs`,
  `commits.rs`, `stashes.rs`) was read only for the cross-client duplication
  count behind 066.
- **Dependencies, CI and DX** were not looked at.

## Direction options surfaced, not planned

The maintainer's call; each has evidence and none was turned into a plan.

- **The intraline similarity gate builds a whole table before rejecting a pair.**
  `docs/measurements.md` records 15.6% of pairs in the deletion-heavy fixture
  falling below `MIN_INTRALINE_SIMILARITY`, and each of those pays a full
  quadratic table to discover it. An O(a+b) token-overlap upper bound would skip
  them — but it must never reject a pair the exact ratio would have kept, which
  is a correctness argument worth its own design. Named as deferred in 060.
- **Splitting work below the hunk.** 062 takes `prepare` from file-level to
  hunk-level stealing; the floor after it is a diff of one hunk. Going finer means
  splitting a syntax-highlighting run, which is stateful across lines (a fence, a
  block comment), so it is not a free next step.
- **The remaining ~24 duplicated verbs.** 066 ports one and reports. Whether to
  port the rest, in what batches, and whether `Acts` should be two traits are
  decisions that want that report first.
