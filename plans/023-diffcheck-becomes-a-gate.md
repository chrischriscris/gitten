# Plan 023: Make the differ-vs-git check a gate instead of a printout

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat d53a0c7..HEAD -- check.sh git/examples/diffcheck.rs core/src/differ.rs .github/workflows/check.yml`
> If any in-scope file changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition.

> **REBASED ONTO `full/full` — 2026-08-30.** This plan was first written
> against `main` (`87229df`) and executed there; that branch is discarded.
> The new base is **`full/full` (`d53a0c7`)**, 127 commits ahead. The cited
> code is unchanged in substance — only line numbers moved. Locate by symbol
> and content, never by the line numbers in the body below:
>
> | anchor | body says | on `full/full` |
> |---|---|---|
> | `check.sh` differs-vs-git section | 75-88 | **unchanged file**, same lines |
> | `diffcheck.rs` `mismatches += 1` (hunks) | 157 | **160** |
> | `diffcheck.rs` `mismatches += 1` (drift) | 174 | **177** |
> | `diffcheck.rs` final `println!` / end of `main` | 184-191 | **~187-193** |
> | `diffcheck.rs` pre-existing `exit(1)` | 51 | 51 |
> | `differ.rs` `score = score.min(...)` | 623, 631 | **644, 652** |
> | `differ.rs` threshold guard `chain.count > best.map_or` | 608 | **629** |
> | `differ.rs` `fn anchor` doc | 551-561 | **572-593** |
> | `differ.rs` `#[cfg(test)] mod tests` | 1722 | **1918** |
> | `check.yml` jobs | test/lint/test-shell/audit | same four, one small edit |
>
> Two things changed around it that do NOT affect this plan but will surprise
> you: acquisition now goes through `gitten_git::open(&repo).pairs(...)` (a
> repo-access trait landed), and `differs.file_using(...)` takes a trailing
> `Option` for blob OIDs (a diff cache landed). `diffcheck` passes `None`.
> Do not "fix" either.
>
> Everything else in this plan — the gate, `diffgate`, the CI job, the
> mutation-validated test — is unchanged and still needed: `full/full`'s
> `diffcheck` still has exactly one `exit(1)` (the pre-existing one at :51),
> so a differ disagreeing with git still exits 0 there.

> **BLOCKER FOUND ON `full/full`, AND WHAT TO DO ABOUT IT (added 2026-08-30
> after a first execution attempt STOPPED here).** Turning the gate on at
> `d53a0c7` makes diffcheck exit 1 immediately, before any other change:
>
> ```
> patience  +893 -592  78h │ git --patience +895 -594 78h
>           -4 of 1489 — within tolerance · 1/78 hunks IN THE WRONG PLACE
> ```
>
> Reproduced on `HEAD~2..HEAD`, `HEAD~4..HEAD`, `HEAD~10..HEAD` and
> `HEAD~25..HEAD` — **always exactly `-4` lines and exactly 1 misplaced hunk**,
> localised by `WORST=1` to **`tui/src/diff.rs` (ours 75 changed lines, git
> 79)**. Our patience finds a *smaller* script than git's patience, identical
> in size to `--minimal`. Histogram — the shipped default — agrees with git
> exactly in every range.
>
> **This is a pre-existing contradiction in the checker, not a bug this plan
> introduces, and not (necessarily) a bug in the differ.** `diffcheck` says
> both of these, twelve lines apart:
>
> - *"The anchored rows have no such freedom and are held to exact positions"*
> - *"Myers has one correct length; **the anchored ones have a range of
>   defensible ones**, so only a drift past a fraction of a percent means
>   anything."*
>
> Those cannot both hold. A script that is licensed to be a different *size*
> cannot be required to have the same hunk *positions* — different-length
> scripts are different objects. Patience drifts `-4`, is explicitly accepted
> as "within tolerance", and is then failed for the positions that accepted
> drift necessarily implies.
>
> So this plan gains **Step 1b**: make the position comparison conditional on
> the scripts being the same size. That is removing a self-contradiction, not
> loosening a check to get green — and it costs nothing where it matters,
> because histogram runs at drift 0 and stays fully position-checked.
>
> The patience-vs-git divergence itself is recorded as a **separate finding**
> in `plans/README.md` for investigation on its own merits. Do not try to fix
> `core/src/differ.rs` in this plan.

## Status

- **Priority**: P1
- **Effort**: M
- **Risk**: LOW
- **Depends on**: none
- **Category**: tests
- **Planned at**: originally `87229df` (main); **rebased onto `d53a0c7` (`full/full`), 2026-08-30**

## Why this matters

The product's central correctness claim is that its differs agree with git.
The tool that checks that claim — `git/examples/diffcheck.rs` — counts
mismatches, prints them, and **exits 0 regardless**. `check.sh` runs it purely
informationally (piped through `sed`, stderr discarded), and CI never runs it
at all. The two most load-bearing rules in the histogram differ (score a run
by its *rarest* line; *tighten* the threshold as the search runs) are
documented in `core/src/differ.rs` as costing hundreds of spurious changed
lines when inverted — and today, inverting either one merges green. This plan
turns diffcheck into a gate locally and in CI, and pins the rarest-line rule
with a unit test that needs no external repository.

## Current state

- `git/examples/diffcheck.rs` — compares each differ's changed-line counts and
  hunk positions against six `git diff` invocations. `mismatches` is
  incremented at two sites and then only *printed*:

  ```rust
  // git/examples/diffcheck.rs:151-160 (inside the per-algorithm loop)
  let hunk_note = match (misplaced, name) {
      (0, _) => String::new(),
      (n, "myers") => format!(" · {n}/{hunks} placed differently (both minimal)"),
      (n, _) => {
          mismatches += 1;
          format!(" · {n}/{hunks} hunks IN THE WRONG PLACE")
      }
  };
  ```

  ```rust
  // git/examples/diffcheck.rs:168-176
  let verdict = if drift == 0 {
      "=".to_string()
  } else if drift.abs() <= tolerance {
      format!("{drift:+} of {} — within tolerance", g_adds + g_dels)
  } else {
      mismatches += 1;
      format!("{drift:+} of {} — TOO FAR", g_adds + g_dels)
  };
  ```

  ```rust
  // git/examples/diffcheck.rs:184-191 (end of main — note: no exit code)
  println!(
      "\n{}",
      if mismatches == 0 {
          "every algorithm agrees with git on how many lines changed"
      } else {
          "a count or a position outside tolerance means a changed answer"
      }
  );
  ```

  The only `std::process::exit(1)` is at `git/examples/diffcheck.rs:51`, for a
  failed `gitten_git::pairs` call.

- `check.sh:75-88` — the "differs vs git" section. Unlike the correctness
  section above it (which wraps every command in `report`, the helper at
  `check.sh:24-36` that accumulates `FAILED`), this loop contributes nothing
  to the exit status and hides errors:

  ```sh
  for spec in HEAD~4..HEAD "$(git rev-list --max-parents=0 HEAD | tail -1)..HEAD"; do
    cargo run -q -p gitten-git --example diffcheck --release . "$spec" 2>/dev/null | sed 's/^/  /'
  done
  for repo in "$HOME/Projects/cmux" "$HOME/Projects/git"; do
    [ -d "$repo/.git" ] || continue
    cargo run -q -p gitten-git --example diffcheck --release "$repo" HEAD~5..HEAD 2>/dev/null \
      | sed 's/^/  /'
  done
  ```

- `.github/workflows/check.yml` — four jobs (`test:33`, `lint:62`,
  `test-shell:94`, `audit:128`). None runs diffcheck. The `test` job checks out
  with `fetch-depth: 3` (its comment explains the acquisition tests need three
  commits), which is too shallow for diffcheck's `HEAD~4..HEAD` and
  root-commit revspecs.

- `core/src/differ.rs:551-561` — the doc comment on `fn anchor` records what a
  wrong scoring rule costs: *"score by the most common line and a
  four-hundred-line run of unique code loses to a one-line run somewhere else
  the moment a single `}` falls inside it … the wrong way round cost 582
  spurious changed-line pairs on a 690-line file — `git/examples/diffcheck.rs`
  is what catches it."* The scoring itself is `score = score.min(...)` at
  `differ.rs:623` and `:631`; the tightening threshold is the
  `chain.count > best.map_or(max, |(score, ..)| score.max(1))` guard at
  `differ.rs:608`. The existing histogram tests (in `#[cfg(test)] mod tests`
  starting at `differ.rs:1722`) assert edit-script shape and apply-back
  correctness, not anchor quality — an inverted scoring rule still produces a
  *valid* diff, just a much worse one.

- Repo conventions: comments explain *why*, at the density you see in the
  excerpts above. `check.sh`'s own header comment records that this exact
  class of bug (output kept, status discarded) happened before and was fixed
  for the `cargo test` lines — extend that fix, match that voice.

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Core tests | `cargo test -q -p gitten-core` | exit 0 |
| Git-crate build | `cargo build -q -p gitten-git --example diffcheck` | exit 0 |
| Diffcheck (self) | `cargo run -q -p gitten-git --example diffcheck --release . HEAD~4..HEAD` | exit 0, table printed |
| Full check | `./check.sh` | exit 0, `✓` summary |
| Lint | `cargo clippy -q --workspace --all-targets -- -D warnings` | exit 0 |
| Format | `cargo fmt --check` | exit 0 |

## Scope

**In scope** (the only files you should modify):
- `git/examples/diffcheck.rs`
- `check.sh`
- `.github/workflows/check.yml`
- `core/src/differ.rs` (test module only — no production code changes)
- (Step 1b touches the verdict logic in `git/examples/diffcheck.rs`, already in scope)
- `plans/README.md` (status row)

**Out of scope** (do NOT touch, even though they look related):
- Any production code in `core/src/differ.rs` — this plan gates the current
  behaviour; it must not change it.
- `git/src/lib.rs` — acquisition is not under test here.
- The external-repo diffcheck runs (`$HOME/Projects/...`) stay local-only;
  do not try to fetch fixtures in CI.

## Git workflow

- Branch: `advisor/013-diffcheck-becomes-a-gate`
- Commit style: imperative sentence, no prefix, explaining the why —
  e.g. `Make diffcheck's disagreement an exit status, not a sentence`.
  Match `git log --oneline -10` for tone.
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: diffcheck exits non-zero when a differ disagrees with git

In `git/examples/diffcheck.rs`, after the final `println!` (currently
`:184-191`), add:

```rust
if mismatches > 0 {
    std::process::exit(1);
}
```

Also update the final message's mismatch arm to include the count, e.g.
`"{mismatches} count(s) or position(s) outside tolerance — a changed answer"`,
so a red run says how red.

**Verify**: `cargo run -q -p gitten-git --example diffcheck --release . HEAD~4..HEAD; echo "exit=$?"`
→ table printed, `exit=0`.

### Step 1b: compare hunk positions only when the scripts are the same size

This is required for the gate to be honest; without it the gate is red on an
unmodified tree for a reason the checker itself calls acceptable.

In `git/examples/diffcheck.rs`, the position verdict currently reads:

```rust
let hunk_note = match (misplaced, name) {
    (0, _) => String::new(),
    (n, "myers") => format!(" · {n}/{hunks} placed differently (both minimal)"),
    (n, _) => {
        mismatches += 1;
        format!(" · {n}/{hunks} hunks IN THE WRONG PLACE")
    }
};
```

Note it runs *before* `drift` is computed. Move the `drift` computation above
it (it depends only on `adds`/`dels`/`g_adds`/`g_dels`, so it can move freely)
and make the failing arm conditional on `drift == 0`:

- `myers` keeps its existing exemption, for its existing reason: equal-length
  minimal scripts can still differ in shape.
- Any other algorithm: if `drift == 0`, the scripts are the same size and the
  positions must match exactly — `mismatches += 1` as today. If `drift != 0`
  (and within tolerance, or it fails on the count anyway), the positions are
  not comparable; print them as informational, worded so a reader can see it,
  e.g. `" · {n}/{hunks} placed differently (script is {drift:+} lines)"`.

Write the comment that replaces the current one in the file's voice, stating
the rule plainly: *positions are only comparable between scripts of the same
size*, and that histogram — the default — runs at drift 0, so it stays held to
exact positions.

**Verify**, and report each number:
1. `cargo run -q -p gitten-git --example diffcheck --release . HEAD~4..HEAD; echo $?` → `0`, with histogram still showing `=` and patience showing the informational note.
2. Same for `HEAD~2..HEAD`, `HEAD~10..HEAD`, `HEAD~25..HEAD` → all `0`.
3. **The gate still catches a genuine position regression.** Prove it: temporarily make `ranges()` shift one hunk's start (e.g. add 1 to the first coordinate it emits) so histogram's positions differ while its counts do not, run diffcheck, confirm `exit=1` and that the message names histogram as IN THE WRONG PLACE. Revert and confirm `exit=0`. If you cannot construct this, STOP and report — a position check that can no longer fail is worse than none.

### Step 2: check.sh treats a diffcheck disagreement as a failure

In `check.sh`, rework the "differs vs git" section so each invocation's exit
status lands in `FAILED`, while keeping the table visible (the existing
`report` helper swallows all non-matching output, and the diffcheck table IS
the output — so don't reuse `report` blindly). Use this shape:

```sh
diffgate() {
  local label=$1; shift
  local out status
  out=$("$@" 2>&1); status=$?
  printf '%s\n' "$out" | sed 's/^/  /'
  if [ "$status" -ne 0 ]; then
    FAILED="$FAILED $label"
    printf '  ✗ %s disagreed with git\n' "$label"
  fi
}
```

and call it for each of the four invocations, e.g.
`diffgate "diffcheck(., $spec)" cargo run -q -p gitten-git --example diffcheck --release . "$spec"`.
Drop the `2>/dev/null` (a build error in this section is currently invisible;
the header comment of check.sh exists because of exactly that pattern). Keep
the `[ -d "$repo/.git" ] || continue` guards for the optional local repos.

**Verify**: `./check.sh; echo "exit=$?"` → all sections run, `exit=0`.
Then verify the gate actually gates: temporarily change `tolerance` in
`git/examples/diffcheck.rs` to `-1` for all algorithms, run
`cargo run -q -p gitten-git --example diffcheck --release . HEAD~4..HEAD; echo $?`
→ `exit=1` (any nonzero drift now trips). **Revert the temporary change** and
confirm exit 0 again.

### Step 1c: myers' exact-count check cannot survive the differ's own budget

**Evidence, gathered by the advisor — do not re-derive it, but do not skip the
step either.** Running the gate over `check.sh`'s *second* invocation (the
whole history in one diff: `$(git rev-list --max-parents=0 HEAD | tail -1)..HEAD`,
152 files / 93,209 new lines) fails on myers:

```
myers  +83270 -579  198h │ git --minimal +83265 -574 198h  +10 of 83839 — TOO FAR
```

`WORST=1` localises all of it to one file, `shell/src/main.rs` (ours 9,383
changed lines, git 9,373). Three facts establish the cause:

1. It is **not** a `full/full` regression: main's differ, given the same input
   (`diffcheck . <root>..full/full` from a main worktree), produces the
   identical `+83270/-579`. Main's own history never reaches it only because
   main is smaller (53k lines), where myers matches git exactly.
2. It is **not** the new cache or the `RefCell`→`Mutex` change on `full/full`.
   Same numbers with and without them.
3. It **is** the step budget. Raising `core/src/differ.rs`'s
   `pub const MAX_STEPS: usize = 40_000_000` by 100× makes myers agree with
   git *exactly* — `+83265/-574` on both sides, whole run reports "every
   algorithm agrees". Restored afterwards; `differ.rs` is untouched.

That is the documented design, not a bug — AGENTS.md: *"Both algorithms are
quadratic in the number of differing lines in the worst case. Bound them and
degrade to 'this region was replaced'."* Myers is O(N·D); ~9.4k differing
lines needs ~88M steps against a 40M budget. So on a large enough file a
*bounded* myers **cannot** be minimal, and diffcheck's comment — *"A minimal
script has exactly one length, so Myers must match exactly; if it does not,
one of the two is not minimal and that is a bug"* — asserts a property the
implementation explicitly declines to provide. Same shape as Step 1b: the
checker demanding something the code never promised.

**What to do.** Keep myers' count out of the *gate*, and say exactly why in
the code:

- In the `verdict` match, treat a myers count drift as informational rather
  than `mismatches += 1` — print it prominently (it is still worth seeing),
  e.g. `"{drift:+} of {} — myers is bounded, see MAX_STEPS"`.
- Replace the surrounding comment with the real rule, in the file's voice:
  myers is minimal *only within its step budget*; past it the differ degrades
  by design, so an exact-length assertion is unsatisfiable on large inputs.
  Reference `MAX_STEPS` and note that raising it makes the drift vanish,
  which is the evidence that this is the bound and not a bug.
- Do **not** touch `core/src/differ.rs`. Do **not** change `MAX_STEPS`.
- Do **not** weaken histogram, patience, or the whitespace variants: they all
  agree exactly on this same 93k-line input, and histogram is the shipped
  default. It stays fully gated on both counts and positions.

This is recorded as a deliberate, temporary loss of coverage. Plan 030
restores it properly by surfacing budget exhaustion from the differ so the
checker can hold myers exact whenever the differ actually ran to completion.

**Verify:**
1. `cargo run -q -p gitten-git --example diffcheck --release . "$(git rev-list --max-parents=0 HEAD | tail -1)..HEAD"; echo $?` → `0`, with the myers line still *showing* its drift.
2. All four short ranges still `0`.
3. `./check.sh; echo $?` → `0`.
4. Histogram is still gated: re-run the ours-only position fault from Step 1b's item 3 and confirm `exit=1`. Then revert.

### Step 3: CI runs diffcheck on this repository

In `.github/workflows/check.yml`, add a job `diffcheck` (after `test`,
following the same style — `Swatinem/rust-cache@v2`, `--locked`):

```yaml
  diffcheck:
    runs-on: ubuntu-latest
    steps:
      # Diffcheck diffs real history: HEAD~4..HEAD and the whole history from
      # the root commit, so it needs the full clone the test job deliberately
      # avoids.
      - uses: actions/checkout@v5
        with:
          fetch-depth: 0
      - uses: Swatinem/rust-cache@v2
      - run: cargo run -q --locked -p gitten-git --example diffcheck --release . HEAD~4..HEAD
      - run: cargo run -q --locked -p gitten-git --example diffcheck --release . "$(git rev-list --max-parents=0 HEAD | tail -1)..HEAD"
```

Do NOT change the `test` job's `fetch-depth: 3` — its comment explains why it
is 3. Also update the workflow's header comment (lines 1-13), which currently
says "Two jobs, deliberately small" while the file defines four (about to be
five) — plan 028 rewrites that comment fully; here just make the job list
accurate if you touch it, or leave the header to plan 028 if it is already
fixed.

**Verify**: `cargo run -q --locked -p gitten-git --example diffcheck --release . HEAD~4..HEAD; echo $?`
→ exit 0 locally. YAML sanity: `python3 -c "import yaml,sys; yaml.safe_load(open('.github/workflows/check.yml'))"`
→ no error (if PyYAML is unavailable, `ruby -ryaml -e "YAML.load_file('.github/workflows/check.yml')"` or careful visual diff).

### Step 4: pin the rarest-line scoring rule with a unit test

In `core/src/differ.rs`'s existing `#[cfg(test)] mod tests` (starts at
`:1722` — follow the style of the histogram tests already there, which build
inputs with the local `lines(...)` helper and check edit scripts), add a test
`histogram_scores_a_run_by_its_rarest_line`.

Construct the discriminating input: an old and new file where a **long run of
unique lines contains one very common line** (a `}` repeated many times
elsewhere in the file), and a **short run of unique lines** exists as an
alternative anchor. Scored by rarest line, the long run's score is 1 (its
unique lines) and it wins; scored by its most common line, the long run scores
high (the `}`) and loses to the short run, fragmenting the diff. Assert on the
resulting edit script: with correct scoring, the changed region is one
contiguous edit (or the exact expected small set); count the edits and assert
the changed-line total.

The test MUST be validated by mutation: temporarily replace `score.min(...)`
with `score.max(...)` at `core/src/differ.rs:623` and `:631`, run the test,
confirm it FAILS; revert, confirm it passes. If you cannot construct an input
where the mutation changes the edit script, this is a STOP condition — report
what you tried rather than committing a test that pins nothing.

**Verify**:
1. `cargo test -q -p gitten-core histogram_scores_a_run_by_its_rarest_line` → 1 passed.
2. Apply the mutation above → same command → FAILED. Revert → passes.

### Step 5: run the whole gate

**Verify**: `./check.sh; echo "exit=$?"` → `exit=0`, and
`cargo fmt --check && cargo clippy -q --workspace --all-targets -- -D warnings` → exit 0.

## Test plan

- New test: `histogram_scores_a_run_by_its_rarest_line` in
  `core/src/differ.rs` tests module (Step 4), mutation-validated.
- Changed behaviour of `diffcheck` is covered by the Step 2 fault-injection
  check (temporary `tolerance = -1`).
- Existing suites must stay green: `cargo test -q -p gitten-core -p gitten-git`.

## Done criteria

Machine-checkable. ALL must hold:

- [ ] `cargo run -q -p gitten-git --example diffcheck --release . HEAD~4..HEAD; echo $?` prints `0`
- [ ] `grep -n 'exit(1)' git/examples/diffcheck.rs` shows a second site (mismatch gate), beyond line ~51
- [ ] `grep -c 'diffgate' check.sh` ≥ 5 (definition + 4 call sites)
- [ ] `grep -n 'diffcheck' .github/workflows/check.yml` shows the new job
- [ ] `cargo test -q -p gitten-core` exits 0 and includes the new histogram test
- [ ] `./check.sh` exits 0
- [ ] `cargo fmt --check` and `cargo clippy -q --workspace --all-targets -- -D warnings` exit 0
- [ ] No files outside the in-scope list are modified (`git status`)
- [ ] `plans/README.md` status row updated

## STOP conditions

Stop and report back (do not improvise) if:

- Step 1's gate makes `./check.sh` or the CI-shaped self-run **red on current
  main** — that means a differ already disagrees with git and the finding is
  bigger than this plan; report the diffcheck output verbatim.
- You cannot construct a mutation-detecting input in Step 4 after a few
  attempts.
- The code at the "Current state" locations doesn't match the excerpts.
- The fix appears to require changing production code in `core/src/differ.rs`.

## Maintenance notes

- Any future tuning of the histogram weights/threshold (`differ.rs:608-631`)
  now fails loudly in CI if it changes the answer on this repo's history. That
  is the point — but this repo's history is a *narrow* corpus. The follow-up
  recorded in `plans/README.md` (golden corpus distilled from
  `fixtures/real/`) widens it and should reuse the exit-code gate added here.
- The backlogged BUG-10 (`compact_with` clamps the slide for deletions only,
  `differ.rs:1283`) was deliberately left unfixed until this gate exists;
  whoever picks it up runs this gate before and after.
- Reviewers: check that Step 2 didn't reintroduce `2>/dev/null` and that the
  CI job uses `fetch-depth: 0`.
