# Plan 062: Make `prepare`'s unit of work a hunk, so one big file uses every core

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/high-priority/README.md`.
>
> **Drift check (run first)**:
> `git diff --stat da9f8a7..HEAD -- core/src/prepared.rs`
> If `core/src/prepared.rs` changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> structural mismatch, treat it as a STOP condition.

## Status

- **Priority**: P2
- **Effort**: M
- **Risk**: MED — it reshapes a parallel pass whose output order is load-bearing
- **Depends on**: plan 060 (soft — adopt `intraline_with` in Step 5 if 060 landed;
  skip that step cleanly if it has not)
- **Category**: perf
- **Planned at**: commit `da9f8a7`, 2026-08-31

## Why this matters

`prepare` — clip, intraline, syntax highlight — is the pass between a parsed diff
and drawable rows, and it is parallel. But its **unit of work is a whole file**,
and parallelism is additionally gated on there being more than one file:

```rust
    let workers = match lines > PARALLEL_ABOVE && files.len() > 1 {
```

So a diff of one large file runs on **one core**, however large it is, and
however many cores the machine has. That is not an exotic case: a lockfile, a
generated file, a single-file refactor, or opening the diff of one file from the
files pane all land there.

The code says so itself, at `core/src/prepared.rs:306-309`:

> **A single-file diff gets nothing from this.** The unit of work is a file, so
> one 700k-line file is still one core. Stealing hunks instead would fix that
> and is the next thing here; it needs the per-file timing accumulation to move,
> which is why it is not in this pass.

For scale: `docs/measurements.md` records file-level stealing taking `pr30683.diff`
(714k lines, 1,375 files) from 314 ms to 77.5 ms — 4.1×. A single-file diff of
comparable size gets none of that today.

This plan moves the work unit from file to hunk, and moves the timing
accumulation the old comment names as the blocker.

## Current state

**File**: `core/src/prepared.rs`. `core` has **zero dependencies** and must keep
them — the threading is `std::thread::scope` and `AtomicUsize`, nothing else.

`core/src/prepared.rs:191-268` — `one_file`, the current unit of work. Read the
whole function in the repo before starting; the parts that matter for this plan:

```rust
fn one_file(
    f: &FileDiff,
    hl: &dyn Highlighter,
    max_line_chars: usize,
) -> (File, Duration, Duration) {
    let mut intraline_time = Duration::ZERO;
    let mut syntax_time = Duration::ZERO;
    let mut rejected = 0usize;

    let all = || f.hunks.iter().flat_map(|h| &h.lines);
    let adds = all().filter(|l| l.kind == LineKind::Added).count();
    let dels = all().filter(|l| l.kind == LineKind::Removed).count();
    let mut hunks = Vec::with_capacity(f.hunks.len());

    for h in &f.hunks {
        let mut texts: Vec<Arc<str>> = h
            .lines
            .iter()
            .map(|l| clip(&l.text, max_line_chars))
            .collect();

        // Second pass: only the removed/added pairs a line diff already
        // matched get word-level spans.
        let mut spans: Vec<Vec<Span>> = vec![Vec::new(); h.lines.len()];
        let t = Instant::now();
        for (d, a) in replace_pairs(h) {
            let (o, n) = intraline(&texts[d], &texts[a]);
            spans[d] = o;
            spans[a] = n;
        }
        intraline_time += t.elapsed();

        let t = Instant::now();
        let refs: Vec<&str> = texts.iter().map(|t| &**t).collect();
        let kinds: Vec<LineKind> = h.lines.iter().map(|l| l.kind).collect();
        let mut tokens = highlight_hunk(hl, &f.path, &refs, &kinds);
        syntax_time += t.elapsed();

        for i in 0..h.lines.len() {
            sanitize(&texts[i], &mut spans[i], |s| (s.start, s.end), &mut rejected);
            sanitize(&texts[i], &mut tokens[i], |t| (t.start, t.end), &mut rejected);
        }

        let lines = h.lines.iter().enumerate().map(|(i, l)| Line { /* ... */ }).collect();
        hunks.push(Hunk { header: h.header.clone(), lines });
    }
    let file = File { path: f.path.clone(), adds, dels, hunks, rejected };
    (file, intraline_time, syntax_time)
}
```

**The whole loop body is already per-hunk and already independent.** Everything it
reads from outside the hunk is `f.path` (for `highlight_hunk`'s language routing)
and `max_line_chars`. Nothing carries across iterations except the three
accumulators — `intraline_time`, `syntax_time`, `rejected` — which is exactly
what the old comment meant by "the per-file timing accumulation".

`core/src/prepared.rs:310-380` — `prepare`, the fan-out:

```rust
pub fn prepare(files: &[FileDiff], hl: &dyn Highlighter, max_line_chars: usize) -> Prepared {
    let lines: usize = files.iter().flat_map(|f| &f.hunks).map(|h| h.lines.len()).sum();
    let workers = match lines > PARALLEL_ABOVE && files.len() > 1 {
        true => std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).min(files.len()),
        false => 1,
    };

    if workers <= 1 {
        // ...the serial path...
    }

    let next = AtomicUsize::new(0);
    let batches: Vec<Vec<(usize, File, Duration, Duration)>> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                s.spawn(|| {
                    let mut mine = Vec::new();
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(f) = files.get(i) else { break };
                        let (file, intra, syn) = one_file(f, hl, max_line_chars);
                        mine.push((i, file, intra, syn));
                    }
                    mine
                })
            })
            .collect();
        handles.into_iter()
            .map(|h| h.join().unwrap_or_else(|p| std::panic::resume_unwind(p)))
            .collect()
    });

    let mut done: Vec<(usize, File, Duration, Duration)> = batches.into_iter().flatten().collect();
    done.sort_unstable_by_key(|(i, ..)| *i);
    // ...sum the durations, push files in order...
}
```

**Three invariants the current code documents and this plan must preserve:**

1. **Output order is order-for-order identical to serial.** `core/src/prepared.rs:297-304`:
   *"Rows address files by index and a client caches by it, so a reordered `files`
   is not a cosmetic difference — it is every row pointing at the wrong file."*
   Hunks within a file are addressed by index too, so hunk order matters equally.
   The test is `parallel_and_serial_agree_exactly`.
2. **A panic in a worker is resumed, not swallowed** (`core/src/prepared.rs:361-364`).
3. **`intraline` and `syntax` are CPU time summed across workers, not wall
   clock**, and `Prepared::threads` is what stops that reading as a broken
   measurement. `core/examples/bench.rs:123-131` prints `×{threads} cpu` for
   exactly this reason.

`PARALLEL_ABOVE = 2_000` lines (`core/src/prepared.rs:185`) — the floor below which
threading is pure loss. It stays.

**Repo conventions**: doc comments explain *why*, at length, in prose. When you
change a documented claim, change the doc in the same commit — the comment at
306-309 saying a single-file diff gets nothing becomes false in this plan and must
be rewritten, not left.

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Core tests | `cargo test -q -p gitten-core` | exit 0, all pass |
| Whole workspace | `cargo test -q --workspace` | exit 0, all pass |
| Lint | `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 |
| Format | `cargo fmt --all --check` | exit 0 |
| Benchmark | `cargo run -q -p gitten-core --example bench --release` | prints the `prepare` line |
| Differs vs git | `cargo run -q -p gitten-git --example diffcheck --release . HEAD~4..HEAD` | exit 0 |

`bench` reads `fixtures/big.diff`. `./fixtures/fetch.sh` downloads the real
fixtures (network-bound, minutes); `./fixtures/gen.sh <n> <m>` makes a synthetic
one offline. **For this plan you need a single-file input to show the win** —
see Step 6 for how to make one without the network.

## Scope

**In scope**:
- `core/src/prepared.rs`

**Out of scope** (do NOT touch):
- `core/src/lib.rs` — `intraline`, `clip`, `replace_pairs`, `sanitize`. Step 5
  *calls* a function plan 060 adds there; it does not edit the file.
- `core/src/syntax.rs` — `highlight_hunk` and the `Highlighter` trait. Language
  routing takes the file path and must keep taking the same path for every hunk
  of that file.
- The public shape of `Prepared`, `File`, `Hunk`, `Line`. Reshaping how spans and
  tokens are stored is plan 063; doing both at once makes a bisect useless.
- `PARALLEL_ABOVE`'s value.

## Git workflow

- Branch: `advisor/perf-062-prepare-steals-hunks`
- Commit message style, from `git log`: lowercase, `scope: sentence`, e.g.
  `core: prepare's unit of work is a hunk, so one file is not one core`.
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: Record the baseline

```sh
cargo test -q --workspace
cargo run -q -p gitten-core --example bench --release   # save the whole output
```

**Verify**: tests exit 0. Save `bench`'s output — you will compare the `prepare`,
`intraline`, `syntax` and `×N cpu` figures against it at the end.

### Step 2: Extract `one_hunk`

Pull the body of `one_file`'s `for h in &f.hunks` loop into a free function:

```rust
/// One hunk: clip, intraline, highlight, sanitize. The unit of work a worker
/// pulls, and independent of every other hunk — nothing in here reads outside
/// the hunk except the file's path, which only picks a lexer.
fn one_hunk(
    h: &crate::Hunk,
    path: &str,
    hl: &dyn Highlighter,
    max_line_chars: usize,
) -> (Hunk, Duration, Duration, usize) {
```

returning the built `Hunk`, its intraline time, its syntax time, and its
`rejected` count. Rewrite `one_file` to loop over `one_hunk` and sum the four.

**This step must not change behaviour at all.** It is a pure extraction so that
Step 4 has something to hand a worker.

**Verify**: `cargo test -q --workspace` → exit 0, all pass.

### Step 3: Widen the parallel gate

Change the worker count so a single large file qualifies:

```rust
    // Hunks, not files: a 700k-line diff of one file is the case the file-shaped
    // gate excluded, and it is exactly the case with the most to gain. The line
    // floor still decides whether to spawn at all — below it, threading is pure
    // loss whatever the shape.
    let hunks: usize = files.iter().map(|f| f.hunks.len()).sum();
    let workers = match lines > PARALLEL_ABOVE && hunks > 1 {
        true => std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
            .min(hunks),
        false => 1,
    };
```

Note `.min(hunks)`, not `.min(files.len())` — spawning more workers than there are
work items is waste.

**Verify**: `cargo test -q -p gitten-core` → exit 0. The serial path is still
taken for small diffs; the parallel path is still correct because Step 4 has not
run yet and workers still pull whole files. **If `parallel_and_serial_agree_exactly`
fails here, STOP** — it means the gate change alone moved an answer, which it
must not.

### Step 4: Steal hunks

Replace the file-indexed work loop with a hunk-indexed one.

Build a flat index of work items **before** spawning, so a worker's `fetch_add`
maps to a `(file, hunk)` pair with no searching:

```rust
    // One flat list of (file, hunk), so a worker's counter is an index and not a
    // search. Built once, outside the threads.
    let work: Vec<(u32, u32)> = files
        .iter()
        .enumerate()
        .flat_map(|(fi, f)| (0..f.hunks.len()).map(move |hi| (fi as u32, hi as u32)))
        .collect();
```

Each worker pulls an index, does `one_hunk`, and pushes
`(file_index, hunk_index, Hunk, Duration, Duration, usize)`.

Then reassemble on the main thread:

- **Place hunks by index, never by arrival.** Allocate
  `Vec<Option<Hunk>>` of the right length per file and write each result into its
  slot, or collect and `sort_unstable_by_key(|(fi, hi, ..)| (*fi, *hi))`. Do not
  push in completion order.
- **`adds`, `dels` and `path` come from the input `FileDiff`, serially.** They are
  counted over `f.hunks`' *input* lines (`one_file`'s `all()` closure) and cost
  nothing; computing them outside the workers keeps them off the hot path and
  removes a reason for a worker to know about its file.
- **`rejected` is the sum of its hunks' counts.**
- **`intraline` and `syntax` are the sums over all hunks**, unchanged in meaning:
  CPU time summed across workers.
- **A file with zero hunks still produces a `File`** with an empty `hunks` vec.
  This is the easiest thing to lose when the work list is hunk-shaped — a file
  with no hunks contributes no work items, so it must be created during
  reassembly from `files`, not from the results.
- Keep the panic resume (`std::panic::resume_unwind`) exactly as it is.

**Verify**: `cargo test -q --workspace` → exit 0. `parallel_and_serial_agree_exactly`
passing is the whole point of this step.

### Step 5 (only if plan 060 has landed): reuse one LCS scratch per worker

If `core/src/lib.rs` exports `intraline_with(old, new, &mut Vec<u32>)`, give each
worker one `Vec<u32>` for its whole lifetime and thread it into `one_hunk`. If
that function does not exist, **skip this step entirely and say so in your
report** — do not add it here, `core/src/lib.rs` is out of scope.

**Verify**: `cargo test -q --workspace` → exit 0.

### Step 6: Measure the case this plan exists for

Make a single-file input and compare. Without the network:

```sh
# A large single-file diff from this repo's own history.
git log --format=%H -- core/src/differ.rs | tail -1 > /dev/null   # sanity: file has history
git diff $(git rev-list --max-parents=0 HEAD | tail -1)..HEAD -- core/src/differ.rs > fixtures/big.diff
wc -l fixtures/big.diff        # note the size; bigger is a better demonstration
cargo run -q -p gitten-core --example bench --release
```

If that diff is under ~2,000 lines it will not cross `PARALLEL_ABOVE` and shows
nothing — pick a larger single file (`shell/src/main.rs` and `tui/src/main.rs` are
the largest in the tree), or concatenate history for one path.

Run the same input on `da9f8a7` and on the branch. **Restore the fixture
afterwards** — `fixtures/big.diff` is gitignored but other measurements read it.

**Verify** and report:
- Single-file input: `prepare` wall clock before vs after, and `×N cpu` showing
  N > 1 after where it was 1 before.
- Multi-file input (whatever `fixtures/big.diff` held at Step 1): `prepare` wall
  clock **must not regress**. Finer-grained stealing costs a little more
  coordination; a small regression here would be a real trade to report, not to
  hide.

Then the full gate set:
- `cargo fmt --all --check` → exit 0
- `cargo clippy --workspace --all-targets -- -D warnings` → exit 0
- `cargo test -q --workspace` → exit 0
- `cargo run -q -p gitten-git --example diffcheck --release . HEAD~4..HEAD` → exit 0

### Step 7: Rewrite the doc comment that this plan makes false

`core/src/prepared.rs:306-309` currently says a single-file diff gets nothing and
that hunk stealing "is the next thing here". Replace it with what is now true:
the unit of work is a hunk, why the line floor still gates spawning at all, and
what the remaining serial floor is (a diff of one hunk).

Also update the `# Work stealing, not chunks` section above it if it says
"files" where it now means "hunks".

**Verify**: `grep -n "single-file diff gets nothing" core/src/prepared.rs` returns
no match.

## Test plan

New tests in `core/src/prepared.rs`'s `mod tests`:

1. `one_file_of_many_hunks_agrees_with_serial` — a `FileDiff` with **one** path
   and enough hunks and lines to cross `PARALLEL_ABOVE`, prepared through
   `prepare` and compared whole against the serial result. This is the case the
   plan exists for and the one no existing test covers.
2. `a_file_with_no_hunks_survives` — a `files` slice containing a `FileDiff` with
   an empty `hunks` vec alongside two normal ones; assert the output has three
   `File`s, in order, with the empty one still present and its `path` intact.
   Catches the reassembly bug named in Step 4.
3. `hunks_keep_their_order_within_a_file` — a file whose hunks are deliberately
   uneven in size (one hunk forty times the others, so they genuinely complete out
   of order), asserting the output hunk headers come back in input order.
4. `rejected_sums_across_hunks` — a file where two different hunks each produce a
   rejected span/token, asserting `File::rejected == 2`. Pins the accumulator
   that moved.

Use `parallel_and_serial_agree_exactly` as the structural pattern — it compares
`Vec<File>`s whole rather than sampling, and new tests should too.

**Verification**: `cargo test -q -p gitten-core` → all pass, including 4 new tests.

## Done criteria

ALL must hold:

- [ ] `cargo fmt --all --check` exits 0
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` exits 0
- [ ] `cargo test -q --workspace` exits 0
- [ ] `cargo run -q -p gitten-git --example diffcheck --release . HEAD~4..HEAD` exits 0
- [ ] `parallel_and_serial_agree_exactly` passes, unmodified
- [ ] The four new tests exist and pass
- [ ] `grep -n "single-file diff gets nothing" core/src/prepared.rs` → no match
- [ ] `git status --porcelain` lists no modified file other than
      `core/src/prepared.rs` and this plan's status row
- [ ] `core/Cargo.toml` has no new dependency
- [ ] Single-file before/after reported, showing `×N cpu` with N > 1
- [ ] Multi-file `prepare` wall clock reported and not materially regressed

## STOP conditions

Stop and report back — do not improvise — if:

- The excerpts above do not match the live code (drift).
- `parallel_and_serial_agree_exactly` fails at any step. Output order is the one
  thing this pass may not change; a client caches rows by file and hunk index, so
  a reordering is every row pointing at the wrong content. Do not "fix" it by
  relaxing the test.
- The multi-file `prepare` wall clock regresses by more than ~10%. Finer stealing
  has a coordination cost and there is a real trade to discuss; report the numbers
  rather than deciding it alone.
- You need to change `Prepared`, `File`, `Hunk` or `Line`'s public shape. You do
  not — that is plan 063.
- `highlight_hunk` turns out to need something from a neighbouring hunk. It does
  not today (it takes the path and the hunk's own lines), but if a language router
  has become stateful across hunks, hunk-level stealing is unsound and this plan
  needs rethinking, not patching.

## Maintenance notes

- **What a reviewer should scrutinize**: the reassembly. Specifically that hunks
  are placed by `(file, hunk)` index rather than arrival order, that a file with
  zero hunks still appears, and that `adds`/`dels` are still counted over the
  *input* lines rather than the prepared ones.
- **What will interact with this**: plan 063 changes how spans and tokens are
  stored per line; if it lands after this, its arena should be per **hunk**, which
  is now also the unit of work — that is a happy accident worth keeping.
- **The remaining serial floor**: a diff of one hunk. Splitting *within* a hunk
  would mean splitting a syntax-highlighting run, which is stateful across lines
  (a fence, a block comment), so it is not a free next step and should not be
  attempted without a design.
- **`Prepared::threads`** now means "workers over hunks". `core/examples/bench.rs`
  prints it as `×N cpu`; check that reads correctly after the change.
