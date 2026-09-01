# Plan 060: Trim the common head and tail before the intraline LCS table

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/high-priority/README.md`.
>
> **Drift check (run first)**:
> `git diff --stat da9f8a7..HEAD -- core/src/lib.rs`
> If `core/src/lib.rs` changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> structural mismatch, treat it as a STOP condition.

## Status

- **Priority**: P1
- **Effort**: S–M
- **Risk**: LOW
- **Depends on**: none
- **Category**: perf
- **Planned at**: commit `da9f8a7`, 2026-08-31

## Why this matters

`intraline` is the word-level second pass that runs on every removed/added line
pair a line diff already matched. It is **the single largest item in `prepare`**:
`docs/measurements.md` records **71.7 ms of a 90.7 ms `prepare` (79%)** on
`md.diff` and 57.5 ms on `pr30698.diff`. It builds a full `a × b` LCS table over
the whole token sequence of both lines, with **no common prefix/suffix trim** —
the standard, answer-preserving first move for any pairwise diff.

Prose is edited a sentence at a time and code lines are rewritten a token at a
time, so the typical pair shares almost all of its head and tail. A 40-token pair
with a 3-token change is a 41×41 = 1,681-cell table today; after trimming it is
about 4×4 = 16 cells. The table is also `vec![0u32; cells]` — freshly allocated
and zeroed **per line pair** — so `md.diff`'s 13,679 replace-pairs alone pay
~13,679 allocations and tens of MB of zeroing per diff.

After this plan: the same spans come out, computed over the part of the pair that
actually differs, on a scratch buffer reused across pairs.

## Current state

**File**: `core/src/lib.rs` — the shared diff helpers. `intraline` is public and
called from `core/src/prepared.rs:217` inside the parallel `prepare` pass.

`core/src/lib.rs:530-604`, the whole function as it exists today:

```rust
pub fn intraline(old: &str, new: &str) -> (Vec<Span>, Vec<Span>) {
    // Offsets are u32, so a line beyond 4 GB has no representation. prepare
    // clips at 2000 characters; the guard keeps the assumption honest for a
    // direct caller that does not.
    if old.len() > u32::MAX as usize || new.len() > u32::MAX as usize {
        return (Vec::new(), Vec::new());
    }
    let mut tokens: Vec<(u32, u32)> = Vec::with_capacity(old.len() / 4 + new.len() / 4 + 8);
    push_tokens(&mut tokens, old);
    let na = tokens.len();
    push_tokens(&mut tokens, new);
    let (a, b) = tokens.split_at(na);

    if a.len() > MAX_INTRALINE_TOKENS || b.len() > MAX_INTRALINE_TOKENS {
        return (Vec::new(), Vec::new());
    }

    // Classic LCS table over tokens — one flat allocation of u32 rather than a
    // Vec per row of usize: [...]
    let w = b.len() + 1;
    let mut lcs = match (a.len() + 1).checked_mul(w) {
        Some(cells) => vec![0u32; cells],
        None => return (Vec::new(), Vec::new()),
    };
    for i in (0..a.len()).rev() {
        let (upper, lower) = lcs.split_at_mut((i + 1) * w);
        let cur = &mut upper[i * w..];
        let ta = token_text(old, a[i]);
        for j in (0..b.len()).rev() {
            cur[j] = if ta == token_text(new, b[j]) {
                lower[j + 1] + 1
            } else {
                lower[j].max(cur[j + 1])
            };
        }
    }

    // The table's corner is the length of the longest common subsequence, so
    // the similarity of the pair is already paid for.
    let common = lcs[0];
    let similarity = 2.0 * common as f32 / (a.len() + b.len()) as f32;
    if similarity < MIN_INTRALINE_SIMILARITY {
        return (Vec::new(), Vec::new());
    }

    let (mut old_spans, mut new_spans) = (Vec::new(), Vec::new());
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if token_text(old, a[i]) == token_text(new, b[j]) {
            i += 1;
            j += 1;
        } else if lcs[(i + 1) * w + j] >= lcs[i * w + j + 1] {
            push_span(&mut old_spans, a[i].0, a[i].0 + a[i].1);
            i += 1;
        } else {
            push_span(&mut new_spans, b[j].0, b[j].0 + b[j].1);
            j += 1;
        }
    }
    while i < a.len() {
        push_span(&mut old_spans, a[i].0, a[i].0 + a[i].1);
        i += 1;
    }
    while j < b.len() {
        push_span(&mut new_spans, b[j].0, b[j].0 + b[j].1);
        j += 1;
    }
    coalesce(&mut old_spans, old);
    coalesce(&mut new_spans, new);
    (old_spans, new_spans)
}
```

Supporting facts you need:

- `core/src/lib.rs:471-490` `push_tokens(out, line)` appends `(offset, length)`
  `u32` pairs. **Token offsets are absolute byte offsets into their own line**,
  so a span built from a token needs no remapping when you skip earlier tokens.
- `core/src/lib.rs:494-496` `token_text(side, t) -> &str` slices a token back out.
- `core/src/lib.rs:506` `MAX_INTRALINE_TOKENS = 1000`.
- `core/src/lib.rs:522` `MIN_INTRALINE_SIMILARITY = 0.4`.
- `push_span` merges into the previous span when adjacent; `coalesce` runs at the
  end against the **full** line text. Neither changes in this plan.

**Repo conventions to match**: `core` has **zero dependencies** and must keep
them — do not add a crate. Doc comments explain *why*, not *what*, and are
written in prose (see the comment above `MAX_INTRALINE_TOKENS` for the register).
Tests live in the `#[cfg(test)] mod tests` at the bottom of the same file; model
new ones on `intraline_marks_only_the_changed_tokens`
(`core/src/lib.rs:901-913`) and `intraline_handles_a_substitution_on_both_sides`
(`core/src/lib.rs:916-928`).

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Core tests | `cargo test -q -p gitten-core` | exit 0, all pass |
| Whole workspace | `cargo test -q --workspace` | exit 0, all pass |
| Lint | `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 |
| Format | `cargo fmt --all --check` | exit 0, no diff |
| Differs vs git | `cargo run -q -p gitten-git --example diffcheck --release . HEAD~4..HEAD` | exit 0 |
| Benchmark | `cargo run -q -p gitten-core --example bench --release` | prints an `intraline` figure |

`bench` reads `fixtures/big.diff`. If you need a specific fixture, run
`./fixtures/fetch.sh` first (network-bound, several minutes) and then
`cp fixtures/real/md.diff fixtures/big.diff`. If the network is unavailable,
measure on whatever `fixtures/big.diff` already holds and say so in your report —
**do not skip the correctness gates over it.**

## Scope

**In scope** (the only files you may modify):
- `core/src/lib.rs`

**Out of scope** (do NOT touch):
- `core/src/prepared.rs` — the caller. `intraline`'s signature does not change,
  so it needs no edit. Parallelising or re-shaping `prepare` is plans 062/063.
- `core/src/differ.rs` — the *line*-level diff. Unrelated to this pass.
- `MAX_INTRALINE_TOKENS` and `MIN_INTRALINE_SIMILARITY` values — the constants
  stay exactly 1000 and 0.4. This plan changes how the answer is computed, never
  which pairs qualify for one.

## Git workflow

- Branch: `advisor/perf-060-intraline-affix-trim`
- One commit per step is fine; squash is not required.
- Commit message style, from `git log`: lowercase, `scope: sentence`, e.g.
  `core: the intraline table starts where the two lines start differing`.
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: Pin today's behaviour with characterization tests

Before changing anything, add tests that will fail loudly if the trim changes an
answer. Add to the `#[cfg(test)] mod tests` block in `core/src/lib.rs`:

1. `intraline_trim_agrees_with_the_untrimmed_table` — a table-driven test over at
   least 12 pairs covering: identical lines; a change only at the head; only at
   the tail; only in the middle; changes at both ends; one side empty; both sides
   empty; a pure insertion; a pure deletion; a pair below the similarity floor
   (e.g. `"/**"` against `"// Historical note: nothing here matches"`); a pair
   with multi-byte characters (e.g. `"let café = 1;"` vs `"let café = 2;"`); and a
   pair whose entire content is one repeated token (`"x x x x"` vs `"x x x x"`).
   For now, assert the *current* outputs — record what `intraline` returns today
   by running the test and pasting in the observed spans.

**Verify**: `cargo test -q -p gitten-core` → exit 0, all pass including the new test.

### Step 2: Extract the LCS core so the trim has something to call

Refactor `intraline`'s body so the table build + backtrack over a token slice pair
is its own private function, with **no behaviour change yet**:

```rust
/// The LCS table over two token slices, and the spans its backtrack produces.
/// Returns the LCS length alongside them, because the caller needs it for the
/// similarity ratio and the table's corner is where it is already paid for.
fn lcs_spans(
    old: &str,
    new: &str,
    a: &[(u32, u32)],
    b: &[(u32, u32)],
    lcs: &mut Vec<u32>,
) -> Option<(u32, Vec<Span>, Vec<Span>)>
```

`None` is the `checked_mul` overflow path that today returns two empty `Vec`s.
`lcs` is a caller-owned scratch buffer: inside, `lcs.clear()` then
`lcs.resize(cells, 0)`.

Call it from `intraline` with the full `a` and `b` and the existing behaviour
must be bit-identical.

**Verify**: `cargo test -q -p gitten-core` → exit 0, all pass (including Step 1's
new test, unchanged).

### Step 3: Trim the common prefix and suffix

In `intraline`, after the `MAX_INTRALINE_TOKENS` guard and before calling
`lcs_spans`, compute the shared affixes and pass only the middle.

**Five things here are load-bearing. Getting any of them wrong is a silent
behaviour change, which is why Step 1 exists:**

1. **The `MAX_INTRALINE_TOKENS` guard stays on the full, pre-trim token counts.**
   It bounds a pathological line, and moving it after the trim would start
   highlighting minified bundles that are deliberately skipped today.
2. **The suffix must not overlap the prefix.** After counting `pre` matching
   tokens from the front, cap the suffix at
   `(a.len() - pre).min(b.len() - pre)`. Without this an identical pair
   double-counts and the arithmetic below goes wrong.
3. **The similarity ratio is computed on the full counts.** The trimmed tokens
   are all common by construction, so:
   `common = pre as u32 + suf as u32 + trimmed_lcs_len`, and the denominator
   stays `(a.len() + b.len())` — the **full** lengths, not the trimmed ones.
   Then the existing `< MIN_INTRALINE_SIMILARITY` check is unchanged.
4. **Compare tokens by text, not by `(offset, length)`.** Two equal words at
   different offsets are equal tokens; comparing the tuples would find almost no
   common prefix. Use `token_text(old, a[i]) == token_text(new, b[j])`.
5. **Spans need no remapping.** Token offsets are already absolute byte offsets
   into their own line, so a span pushed from a middle token is correct as-is.
   Prefix and suffix tokens are common and therefore produce no spans at all.

Shape:

```rust
    // The head and tail two lines share are in every common subsequence, so the
    // table only has to cover what is left. Prose is edited a sentence at a time
    // and code a token at a time, which makes this most of the work on the
    // fixtures that cost the most: see docs/measurements.md on `md.diff`.
    let mut pre = 0;
    while pre < a.len() && pre < b.len() && token_text(old, a[pre]) == token_text(new, b[pre]) {
        pre += 1;
    }
    // Never past the prefix: an identical pair would otherwise count its tokens
    // twice and the similarity ratio would exceed 1.
    let room = (a.len() - pre).min(b.len() - pre);
    let mut suf = 0;
    while suf < room
        && token_text(old, a[a.len() - 1 - suf]) == token_text(new, b[b.len() - 1 - suf])
    {
        suf += 1;
    }
    let (mid_a, mid_b) = (&a[pre..a.len() - suf], &b[pre..b.len() - suf]);
```

Then call `lcs_spans(old, new, mid_a, mid_b, &mut lcs)`, and:

```rust
    // The trimmed tokens are common by construction, so they count towards the
    // pair's similarity — which is measured against the whole line either side,
    // never against the middle the table happened to cover.
    let common = pre as u32 + suf as u32 + mid_common;
    let similarity = 2.0 * common as f32 / (a.len() + b.len()) as f32;
```

**Verify**: `cargo test -q -p gitten-core` → exit 0, all pass. Step 1's
characterization test passing unmodified is the point of this step.

### Step 4: Reuse the LCS scratch across pairs

`intraline` allocates and zeroes a fresh table per call. Add a variant that takes
the scratch buffer from its caller, and keep `intraline` as a thin wrapper so
every existing caller and test compiles untouched:

```rust
/// [`intraline`], with the LCS table handed in. `prepare` diffs thousands of
/// pairs per file and the table is the only allocation in here that survives a
/// call, so a per-worker buffer removes one malloc and one zeroing pass per pair.
pub fn intraline_with(old: &str, new: &str, lcs: &mut Vec<u32>) -> (Vec<Span>, Vec<Span>)
```

```rust
pub fn intraline(old: &str, new: &str) -> (Vec<Span>, Vec<Span>) {
    intraline_with(old, new, &mut Vec::new())
}
```

**Do not** change `core/src/prepared.rs` to use it in this plan — that file is out
of scope and belongs to plan 062, which reworks the same loop. Adding the seam
here is what makes 062 cheap.

**Verify**: `cargo test -q -p gitten-core` → exit 0.

### Step 5: Gates and a measurement

Run the full gate set, then record a before/after.

**Verify**, all of these:
- `cargo fmt --all --check` → exit 0
- `cargo clippy --workspace --all-targets -- -D warnings` → exit 0
- `cargo test -q --workspace` → exit 0
- `cargo run -q -p gitten-git --example diffcheck --release . HEAD~4..HEAD` → exit 0

Then measure. `intraline` spans are not what `diffcheck` compares, so the numbers
are informational, not a gate:

```sh
cargo run -q -p gitten-core --example bench --release   # note the `intraline` figure
```

Run it on the branch and on `da9f8a7` (`git stash`-free: use a second checkout or
`git worktree add`). Report both figures. **Expect a large drop on prose-shaped
input and a small one on near-pure additions or deletions** — a diff with few
replace-pairs has little for this pass to do either way.

## Test plan

New tests in `core/src/lib.rs`'s `mod tests`:

1. `intraline_trim_agrees_with_the_untrimmed_table` (Step 1) — the 12+ pair table
   described above.
2. `intraline_similarity_is_measured_against_the_whole_line` — a pair that is
   long, identical except for one token in the middle, and would sit *above* the
   floor on full counts but *below* it if the ratio were computed on the trimmed
   middle only. Assert spans come back non-empty. This is the test that catches
   load-bearing point 3.
3. `intraline_identical_lines_have_no_middle_left` — `intraline("abc def", "abc def")`
   returns two empty span vectors and does not panic. Catches point 2.
4. `intraline_trims_nothing_when_the_first_token_differs` — `("a b c", "z b c")`
   still highlights exactly `a` and `z`.
5. `intraline_with_reuses_a_dirty_buffer` — call `intraline_with` twice on
   different pairs through the *same* `Vec<u32>`, pre-filled with garbage
   (`vec![9u32; 64]`), and assert both answers equal what `intraline` gives. This
   is what catches a `resize`-without-`clear` bug in Step 2.

Existing tests that must still pass unmodified —
`intraline_marks_only_the_changed_tokens`,
`intraline_handles_a_substitution_on_both_sides`,
`intraline_bails_out_on_machine_generated_lines` (`core/src/lib.rs:1011`).

**Verification**: `cargo test -q -p gitten-core` → all pass, including 5 new tests.

## Done criteria

ALL must hold:

- [ ] `cargo fmt --all --check` exits 0
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` exits 0
- [ ] `cargo test -q --workspace` exits 0
- [ ] `cargo run -q -p gitten-git --example diffcheck --release . HEAD~4..HEAD` exits 0
- [ ] The five new tests exist and pass
- [ ] `grep -n "MAX_INTRALINE_TOKENS" core/src/lib.rs` still shows the guard
      applied to the full token counts, before any trim
- [ ] `git status --porcelain` lists no modified file other than `core/src/lib.rs`
      and this plan's status row
- [ ] `bench`'s `intraline` figure reported for both `da9f8a7` and the branch
- [ ] `core/Cargo.toml` has no new dependency (`git diff da9f8a7..HEAD -- core/Cargo.toml` is empty)

## STOP conditions

Stop and report back — do not improvise — if:

- `core/src/lib.rs`'s `intraline` does not match the excerpt above (drift).
- Step 1's characterization test fails after Step 3 and you cannot make it pass
  without editing the *expected* values. Changing an expectation to match new
  behaviour defeats the entire test; report the differing pair instead.
- `diffcheck` exits non-zero. It checks the *line* diff, which this plan does not
  touch, so a failure here means something unexpected is coupled — report it.
- You find yourself needing to edit `core/src/prepared.rs` or any file outside
  `core/src/lib.rs`.
- The similarity gate visibly changes which pairs get spans on the fixtures
  (i.e. `bench`'s `replace-pairs` count moves). That count must be identical
  before and after; this plan changes cost, never answers.

## Maintenance notes

- **What a reviewer should scrutinize**: the three arithmetic points — the
  prefix/suffix overlap cap, the similarity numerator (`pre + suf + mid`), and
  the similarity denominator (full lengths). Everything else is mechanical.
- **What will interact with this**: plan 062 reworks `prepare`'s inner loop and
  should adopt `intraline_with` with one scratch buffer per worker. Plan 063
  reshapes how spans are stored; it consumes `intraline`'s output but does not
  change how it is computed.
- **Deliberately deferred**: a cheap pre-filter that rejects a
  below-similarity-floor pair *before* building any table (a token multiset
  overlap bound is O(a+b)). `docs/measurements.md` records 15.6% of pairs in the
  deletion-heavy fixture falling below the floor, and today each of those pays a
  full table to find out. It is a separate change with its own correctness
  argument — the bound must never reject a pair the exact ratio would have kept.
