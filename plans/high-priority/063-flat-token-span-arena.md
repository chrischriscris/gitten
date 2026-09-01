# Plan 063: Hold a hunk's tokens and spans in one buffer, not a box per line

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. **Step 1 is a feasibility gate with a mandatory
> report before any wide edit — do not skip past it.** When done, update the
> status row for this plan in `plans/high-priority/README.md`.
>
> **Drift check (run first)**:
> `git diff --stat da9f8a7..HEAD -- core/src/prepared.rs core/src/markdown.rs core/src/runs.rs core/src/rows.rs shell/src/views/ tui/src/rows.rs web/src/api.rs`
> If any in-scope file changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> structural mismatch, treat it as a STOP condition.

## Status

- **Priority**: P3
- **Effort**: L
- **Risk**: MED–HIGH — it changes the shape of `prepared::Line`, which all three
  clients and the Markdown layout read
- **Depends on**: plan 062 (soft — if 062 landed, the arena's natural owner is the
  hunk, which is also by then the unit of work)
- **Category**: perf
- **Planned at**: commit `da9f8a7`, 2026-08-31

## Why this matters

Every prepared line owns two independently heap-allocated, exact-size slices:

```rust
    pub spans: Box<[Span]>,
    pub tokens: Box<[Token]>,
```

On the 714k-line fixture that is **~1.06 million boxes and +65 MB**, and it is
the largest remaining item in the pipeline's memory profile.
`docs/measurements.md` attributes the peak stage by stage and then says, after
the line-text arena was built, measured and **reverted**:

> The arena did what it claimed … and it still lost. … the −5.7 % on the patch
> path shows line text was never the fragmentation — **the ~1.06M token/span
> boxes in `prepare` are, and the arena does not touch them. That is the target a
> future memory pass should measure against, not the line text.**

It costs time as well as memory. Each of those boxes is a `Vec` grown by doubling
and then `into_boxed_slice()`d, which reallocates and memcpies to shrink to fit —
so it is two allocator round trips per non-empty line, across ten worker threads,
which `docs/measurements.md` already identifies as "contention and allocator
pressure". And it costs the render path: a row that reads a contiguous slice of a
shared buffer touches fewer cache lines than one chasing a pointer per line.

**Read the reverted line-text arena's post-mortem before starting**:
`docs/decisions/0026-line-text-is-not-the-memory-to-save.md`. That attempt lost
because an arena of *line text* pinned its whole backing buffer alive — one
surviving context-line slice kept a whole file resident, defeating the streaming
that `each_pair` had just won. **Tokens and spans do not have that problem**:
they are dropped with their hunk and nothing outside a hunk holds a reference to
one. Confirming that remains true is Step 1's job.

## Current state

`core/src/prepared.rs:24-43` — the type this plan reshapes:

```rust
pub struct Line {
    pub kind: LineKind,
    pub moved: bool,
    pub old_no: Option<u32>,
    pub new_no: Option<u32>,
    /// The same allocation the parsed [`crate::DiffLine`] holds whenever the
    /// line fit the clip budget: `clip`'s fast path is a refcount bump.
    pub text: Arc<str>,
    /// Never mutated after `prepare` — hence exact-size boxed slices rather
    /// than `Vec`s with spare capacity. Markdown layout rebuilds these rather
    /// than editing them in place.
    pub spans: Box<[Span]>,
    pub tokens: Box<[Token]>,
}
```

`core/src/prepared.rs:214`, `254-255` — where they are built, one pair per line:

```rust
        let mut spans: Vec<Vec<Span>> = vec![Vec::new(); h.lines.len()];
        // ...
                spans: std::mem::take(&mut spans[i]).into_boxed_slice(),
                tokens: std::mem::take(&mut tokens[i]).into_boxed_slice(),
```

**Every reader, counted at `da9f8a7`** (`grep -rn "\.tokens\b\|\.spans\b"`):

| File | Sites | What it does |
|---|---|---|
| `core/src/markdown.rs` | 44 | the layout pass — **the only code that mutates them** |
| `shell/src/views/markdown.rs` | 9 | reads |
| `shell/src/views/diff.rs` | 7 | reads; `2435-2436` moves them into its own row type |
| `shell/src/views/split.rs` | 4 | reads |
| `core/src/rows.rs`, `tui/src/rows.rs` | 3 each | reads |
| `core/examples/paint.rs` | 3 | reads |
| `tui/src/markdown.rs`, `shell/src/config.rs` | 2 each | reads |
| `core/src/runs.rs:451`, `web/src/api.rs`, `core/examples/bench.rs` | 1 each | reads |

**The one seam every presentation goes through** is `core/src/runs.rs:451`:

```rust
            runs(at.clone(), &l.tokens, &l.spans, l.kind, l.moved, &mut out);
```

`runs` takes `&[Token]` and `&[Span]`. **If `Line` can still hand out two slices,
that signature never changes and most of the table above needs no edit at all.**
That is the design this plan aims at.

**The three mutation sites, and the fact that makes an arena possible.** All of
them are in `core/src/markdown.rs`, and **none of them grows a line's token or
span count**:

- `core/src/markdown.rs:488-493` `rule_row` — sets both to `Box::default()` (empty).
- `core/src/markdown.rs:541-562` — `iter().map(...).filter(|(s, e, _)| s < e).collect()`.
- `core/src/markdown.rs:1735-1756` — the same map-and-filter shape.

Map-then-filter over the existing entries can only produce **the same count or
fewer**. So a rebuild can be written in place into the region the line already
owns, shortening its length — the arena never has to move or grow. **Step 1
verifies this claim before anything depends on it.**

`FlowedRow` (`core/src/markdown.rs:983-989`) also holds `Box<[Token]>`/`Box<[Span]>`
but is a *reflow* output built per visible table row, not a per-line cost.
**It is out of scope.**

**Repo conventions**: `core` has **zero dependencies** and must keep them. Doc
comments explain *why*, in prose, at length; a claim that stops being true gets
rewritten in the same commit that falsifies it.

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Core tests | `cargo test -q -p gitten-core` | exit 0 |
| Whole workspace | `cargo test -q --workspace` | exit 0 |
| Lint | `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 |
| Format | `cargo fmt --all --check` | exit 0 |
| Everything headless | `./dev check` | prints `✓ all green`, exit 0 |
| Benchmark | `cargo run -q -p gitten-core --example bench --release` | prints `prepare` and token count |
| Peak memory | `/usr/bin/time -l cargo run -q -p gitten-core --example bench --release` | reports peak RSS |
| Differs vs git | `cargo run -q -p gitten-git --example diffcheck --release . HEAD~4..HEAD` | exit 0 |

## Scope

**In scope**:
- `core/src/prepared.rs` — the type and its construction
- `core/src/markdown.rs` — the three mutation sites, only as far as the new shape
  requires
- Any reader in the table above that does not compile after the change
- `docs/measurements.md` — one new subsection with the before/after (this is the
  measurement the doc explicitly asks a future pass to take)

**Out of scope** (do NOT touch):
- `core/src/runs.rs`'s `runs()` signature. If your design forces a change here,
  the design is wrong — see STOP conditions.
- `FlowedRow` and the reflow path in `core/src/markdown.rs:983+`.
- `Line::text`'s `Arc<str>`. The line-text arena was **built, measured and
  reverted** (`docs/decisions/0026`); do not revisit it inside this plan.
- Any behaviour change. Every rendered row must be byte-identical before and
  after; this plan moves bytes around in memory and nothing else.
- Plans 060/061/062's files, beyond what shared compilation forces.

## Git workflow

- Branch: `advisor/perf-063-flat-token-span-arena`
- Commit per step; the steps are ordered so the tree compiles between them.
- Commit message style, from `git log`: lowercase, `scope: sentence`, e.g.
  `core: a hunk's tokens live in one buffer, not a box per line`.
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1 (GATE — report before proceeding): prove the two premises

Do **no** refactoring in this step. Establish two facts and report them:

**Premise A — nothing outside a hunk outlives it holding a token or span.**
Search every reader in the table above for a site that stores a `Token`, `Span`,
`&[Token]` or `&[Span]` (or a `Box<[..]>` moved out of a `Line`) into something
with a longer lifetime than the `Prepared` it came from. `shell/src/views/diff.rs:2435-2436`
moves them into the view's own row type — establish whether that row type outlives
the `Prepared`, or is rebuilt from it.

**Premise B — no mutation site grows a line's token or span count.** Confirm the
three sites listed above are the only writers (`grep -n "\.tokens = \|\.spans = " core/src/markdown.rs core/src/prepared.rs`)
and that each is a map-then-filter that cannot grow.

Also capture the baseline:

```sh
cargo run -q -p gitten-core --example bench --release            # tokens, prepare
/usr/bin/time -l cargo run -q -p gitten-core --example bench --release 2>&1 | grep -i "maximum resident"
./dev check                                                       # must print ✓ all green
```

**Verify / report**: post both premises with the `file:line` evidence and the
baseline numbers.

- If **Premise A is false** — something outlives the hunk — **STOP and report.**
  An arena whose regions are handed out as slices cannot survive that, and the
  design needs the owner's decision.
- If **Premise B is false** — some site grows a count — **STOP and report.** A
  growing rebuild needs an append-and-repoint arena, which is a different and
  larger design than this plan describes.
- If `./dev check` is not green before you start, STOP.

### Step 2: Give `Line` accessors, keeping the fields as they are

Make every reader go through a method before changing any representation:

```rust
impl Line {
    /// The line's syntax tokens. A slice rather than the field, so how they are
    /// stored is this module's business and not every presentation's.
    pub fn tokens(&self) -> &[Token] { &self.tokens }
    /// The words an intraline pass marked as changed inside this line.
    pub fn spans(&self) -> &[Span] { &self.spans }
}
```

Then make the fields **private** and fix every reader in the table above to call
`l.tokens()` / `l.spans()`. `core/src/runs.rs:451` becomes
`runs(at.clone(), l.tokens(), l.spans(), l.kind, l.moved, &mut out)` — `runs`
itself is untouched, which is the point.

The three mutation sites in `core/src/markdown.rs` need a writer too. Give them
one that expresses the shrink-only contract:

```rust
    /// Replaces this line's tokens with the result of `f` applied to each,
    /// dropping the ones it rejects. **Shrink-only by construction**: markdown
    /// layout removes markers and remaps ranges, and never invents a token — a
    /// contract the storage below relies on.
    pub fn retain_map_tokens(&mut self, f: impl FnMut(Token) -> Option<Token>) { ... }
    pub fn retain_map_spans(&mut self, f: impl FnMut(Span) -> Option<Span>) { ... }
    /// Drops both, for a row whose text no revision produced — see `rule_row`.
    pub fn clear_marks(&mut self) { ... }
```

**No representation has changed yet.** This step is only about routing every
access through a seam.

**Verify**: `cargo test -q --workspace` → exit 0, and `./dev check` → `✓ all green`.
Commit here — this is the safe rollback point.

### Step 3: Move the storage into a per-hunk buffer

Now change the representation behind the accessors:

```rust
pub struct Hunk {
    pub header: String,
    pub lines: Vec<Line>,
    /// Every line's tokens, end to end, in line order. One buffer per hunk
    /// instead of a box per line: a 714k-line diff held ~1.06M of those and
    /// 65 MB of the peak — see docs/measurements.md. A line addresses its own
    /// with a range, and a renderer gets the same `&[Token]` it always did.
    tokens: Vec<Token>,
    spans: Vec<Span>,
}

pub struct Line {
    // ...
    tokens: Range<u32>,
    spans: Range<u32>,
}
```

**The accessors now need the hunk.** Two workable shapes — pick one and say which
in your report:

- **(a) The hunk hands out the slice**: `Hunk::tokens_of(&self, i: usize) -> &[Token]`,
  and every reader that walks `h.lines` uses the index it already has. Most
  readers do iterate a hunk's lines, so this is usually a small edit.
- **(b) The line borrows the buffer**: iterate with a method that yields
  `(&Line, &[Token], &[Span])` triples.

Do **not** store a raw pointer or an `Arc` to the buffer inside `Line` to
preserve the no-argument accessor. That reintroduces exactly the pinning that
killed the line-text arena.

`prepare` builds it by appending each line's tokens to the hunk buffer and
recording the range — **one `Vec` per hunk that grows by doubling and is never
shrunk to fit**, which is where the ~2M allocator round trips go away.

Preserve the sanitize step's semantics: `sanitize` currently rewrites
`spans[i]`/`tokens[i]` in place and counts rejections. It must run **before** the
ranges are recorded, or it has to be able to shorten a region.

**Verify**: `cargo test -q --workspace` → exit 0; `./dev check` → `✓ all green`;
`cargo run -q -p gitten-git --example diffcheck --release . HEAD~4..HEAD` → exit 0.

### Step 4: Make the markdown rebuild write in place

The three sites now shorten a region rather than allocating a new box. Implement
`retain_map_tokens`/`retain_map_spans` as a compacting walk over the line's own
region in the hunk buffer, shortening `Range::end`. The vacated tail is dead
space in the buffer — that is fine and expected; it is bounded by what markdown
removes, and the buffer dies with the hunk.

**Verify**: `cargo test -q -p gitten-core` → exit 0, and specifically the Markdown
layout tests. Then a rendered frame, which is what actually exercises the layout:

```sh
COLS=120 ROWS=40 cargo run -q -p gitten-tui --example dump --release -- diff --fixtures
```

→ exits 0, prints a frame, no panic.

### Step 5: Measure, and write it down

```sh
cargo run -q -p gitten-core --example bench --release
/usr/bin/time -l cargo run -q -p gitten-core --example bench --release 2>&1 | grep -i "maximum resident"
```

Run on `da9f8a7` and on the branch, on the **same** `fixtures/big.diff`.
`docs/measurements.md`'s own methodology section is strict about this and you
should follow it: interleave the runs (ABBA), flip which side goes first, take
medians of at least four rounds a side, and leave settle gaps —
`docs/measurements.md:376-383` records naive back-to-back A/B swinging **+25–95%**
on this machine purely from allocator state.

Add a subsection to `docs/measurements.md` in the register of the existing ones:
what changed, the peak-RSS row, the `prepare` row, and the statement that
structural output is identical either side (`bench` prints token and
replace-pair counts — quote them and show they match).

**Verify**: `./dev check` → `✓ all green`; the token count and replace-pair count
printed by `bench` are **identical** before and after.

## Test plan

New tests in `core/src/prepared.rs`'s `mod tests`:

1. `tokens_and_spans_round_trip_through_the_arena` — prepare a multi-hunk,
   multi-line diff and assert, for every line, that the accessor returns exactly
   what the old per-line boxes would have: compare against a serial reconstruction
   built directly from `highlight_hunk` and `intraline`.
2. `a_line_with_no_tokens_has_an_empty_range` — a blank or unhighlightable line
   returns an empty slice and does not panic.
3. `ranges_do_not_overlap_within_a_hunk` — walk a hunk's lines and assert each
   line's range starts at the previous one's end, and the last ends at the
   buffer's length. Catches an off-by-one in the append loop, which is the most
   likely bug in Step 3 and the one that silently shows a line its neighbour's
   highlighting.
4. `markdown_rebuild_only_shrinks` — after the Markdown layout pass, assert every
   line's range is within the region it had before. Pins Premise B as a test
   rather than an assumption.

In `core/src/markdown.rs`'s tests, keep every existing layout assertion
unmodified — they are the regression net for Step 4.

**Verification**: `cargo test -q --workspace` → all pass, including 4 new tests.

## Done criteria

ALL must hold:

- [ ] Step 1's two premises reported with evidence, before any refactor
- [ ] `cargo fmt --all --check` exits 0
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` exits 0
- [ ] `cargo test -q --workspace` exits 0
- [ ] `./dev check` prints `✓ all green` and exits 0
- [ ] `cargo run -q -p gitten-git --example diffcheck --release . HEAD~4..HEAD` exits 0
- [ ] `grep -n "pub spans: Box<\[Span\]>\|pub tokens: Box<\[Token\]>" core/src/prepared.rs` → no match
- [ ] `git diff da9f8a7..HEAD -- core/src/runs.rs` shows **no change to `runs()`'s signature**
- [ ] `bench`'s token count and replace-pair count identical before and after
- [ ] Peak RSS reported for both sides, medians of ≥4 interleaved rounds
- [ ] `docs/measurements.md` has the new subsection
- [ ] `core/Cargo.toml` has no new dependency

## STOP conditions

Stop and report back — do not improvise — if:

- **Step 1's Premise A or B is false.** Both are design preconditions, not
  details.
- Making this work requires changing `runs()`'s signature in `core/src/runs.rs`.
  Every presentation goes through it; if the arena cannot hand out a plain
  `&[Token]`, the representation is wrong.
- You find yourself wanting to put an `Arc`, a raw pointer, or a lifetime
  parameter inside `Line` so the accessor can stay argument-free. That is the
  pinning failure `docs/decisions/0026` records; report and let the owner choose.
- Peak RSS does not fall, or `prepare` regresses. This plan is a memory pass with
  a documented target; a change that does not hit it is not worth its risk.
  Report the numbers rather than searching for a different justification.
- Any rendered output differs — a changed token count, a changed replace-pair
  count, a different frame from `dump`.
- The reader table above turns out to be incomplete and the ripple reaches files
  outside the Scope list.

## Maintenance notes

- **What a reviewer should scrutinize**: the append loop's range arithmetic
  (test 3 exists for it), the sanitize-before-record ordering, and that the
  markdown rebuild genuinely shrinks in place rather than quietly appending.
- **What will interact with this**: plan 062 makes the hunk the unit of parallel
  work; if both land, each worker naturally owns one hunk's buffers and there is
  no sharing to synchronise. If 062 lands *after* this, its reassembly must move
  the buffers with their hunk.
- **Deliberately deferred**: `FlowedRow`'s boxes on the reflow path, and the
  per-hunk `Vec<Arc<str>>`/`Vec<&str>`/`Vec<LineKind>` scratch vectors in
  `one_hunk`. Both are smaller and neither is what `docs/measurements.md` names.
- **The dead space** left by a markdown shrink is deliberate and bounded. If a
  future pass wants it back, compacting once at the end of the layout pass is
  cheaper than moving ranges during it.
