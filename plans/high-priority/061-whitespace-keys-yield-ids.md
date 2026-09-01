# Plan 061: Stop interning every line twice under a whitespace relation

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/high-priority/README.md`.
>
> **Drift check (run first)**:
> `git diff --stat da9f8a7..HEAD -- core/src/differ.rs`
> If `core/src/differ.rs` changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> structural mismatch, treat it as a STOP condition.

## Status

- **Priority**: P2
- **Effort**: M
- **Risk**: MED — it adds a method to the public `Differ` trait (an extension seam)
- **Depends on**: none
- **Category**: perf
- **Planned at**: commit `da9f8a7`, 2026-08-31

## Why this matters

When `[diff] whitespace` is anything but `exact`, every line of both files is
**interned twice, hashed twice, and allocated once more than it needs to be**:

1. `Whitespace::keys` normalises each line into a scratch `String`, then
   `KeyArena::intern` FxHashes those bytes, walks the bucket, and returns an
   `Arc<str>` handle — an `Arc::clone` (atomic refcount bump) per line, into a
   fresh `Vec<Arc<str>>` per side.
2. The differ then calls `Scratch::diffed`, which builds a `LineMap` and
   **FxHashes the very same bytes again** to assign the dense `u32` ids its inner
   loops actually compare.

The arena already knows the id — `KeyArena::intern` computes
`let id = self.keys.len() as u32;` and then throws it away in favour of a handle.

`docs/measurements.md` names this fix by name and calls it next:

> those rows are still 2–3× `Exact`, because a whitespace key is interned twice
> over: once by `KeyArena` into an `Arc<str>` and again by the line map. Having
> `Whitespace::keys` yield ids directly removes a whole pass and is the next
> thing here.

The measured gap it is talking about, on a 94-file / 100k-line revspec:
`histogram` 6.4 ms against `ws-eol` 13.3 ms, `ws-change` 20.7 ms, `ws-all`
18.9 ms. This plan removes one full hash-and-compare pass over every line from
those three modes.

**Answers must not move.** `diffcheck` compares changed-line counts *and every
hunk position* against six `git` invocations, three of which are the whitespace
modes; it is the gate for this plan.

## Current state

**File**: `core/src/differ.rs` — the diff engine. `core` has **zero
dependencies** and must keep them.

`core/src/differ.rs:110-114` — the public extension seam. An extension registers
a differ through this trait, so it may not break:

```rust
pub trait Differ: Send + Sync {
    fn name(&self) -> &'static str;

    fn diff(&self, path: &str, old: &[Arc<str>], new: &[Arc<str>]) -> Vec<Edit>;
}
```

`core/src/differ.rs:992-1007` — the first interning pass:

```rust
    fn keys(self, lines: &[Arc<str>], arena: &mut KeyArena, out: &mut Vec<Arc<str>>) {
        match self {
            Whitespace::Exact => {}
            _ => {
                out.clear();
                out.reserve(lines.len());
                // Taken out so `intern` can touch the arena while the scratch
                // is borrowed; handed back afterwards.
                let mut norm = std::mem::take(&mut arena.norm);
                for line in lines {
                    norm.clear();
                    self.normalize(line, &mut norm);
                    out.push(arena.intern(&norm));
                }
                arena.norm = norm;
            }
        }
    }
```

`core/src/differ.rs:1036-1053` — the arena, which computes an id and discards it:

```rust
impl KeyArena {
    fn intern(&mut self, key: &str) -> Arc<str> {
        let mut hasher = crate::FxHasher::default();
        hasher.write(key.as_bytes());
        let hash = hasher.finish();
        if let Some(ids) = self.buckets.get(&hash) {
            for &id in ids {
                if &*self.keys[id as usize] == key {
                    return Arc::clone(&self.keys[id as usize]);
                }
            }
        }
        let id = self.keys.len() as u32;
        self.keys.push(Arc::from(key));
        self.buckets.entry(hash).or_default().push(id);
        Arc::clone(&self.keys[id as usize])
    }
}
```

`core/src/differ.rs:371-396` — the second interning pass, inside every built-in:

```rust
    fn diffed(&mut self, old: &[Arc<str>], new: &[Arc<str>], max: Option<u32>) -> Vec<Edit> {
        self.begin_file();
        let mut map: LineMap =
            LineMap::with_capacity_and_hasher(old.len() + new.len(), <_>::default());
        number(&mut map, old, &mut self.ids_old);
        let a = std::mem::take(&mut self.ids_old);
        number(&mut map, new, &mut self.ids_new);
        let b = std::mem::take(&mut self.ids_new);

        let mut out = Vec::new();
        match max {
            Some(max) => self.anchored(&a, &b, max, &mut out),
            None => self.myers(&a, &b, Region::whole(&a, &b), &mut out),
        }
        // Handed back rather than dropped: the next file starts at full size.
        self.ids_old = a;
        self.ids_new = b;
        out
    }
```

`core/src/differ.rs:1844-1867` — where the two paths part:

```rust
        match ws {
            // Byte-for-byte: the lines themselves are the keys, and their
            // handles are already shared.
            Whitespace::Exact => self.assemble(path, differ, old, new, old, new),
            _ => {
                let mut arena = KeyArena::default();
                let (mut ko, mut kn) =
                    (Vec::with_capacity(old.len()), Vec::with_capacity(new.len()));
                ws.keys(old, &mut arena, &mut ko);
                ws.keys(new, &mut arena, &mut kn);
                self.assemble(path, differ, old, new, &ko, &kn)
            }
        }
```

`core/src/differ.rs:1886-1911` — `assemble`. Note the handles are used by three
things, not one: `differ.diff`, `compact_with` and `moves`. **The
`Vec<Arc<str>>` therefore stays**; only the differ's own re-hash goes away.

```rust
        let mut edits = differ.diff(path, keys_old, keys_new);
        if self.indent_heuristic {
            compact_with(old, new, keys_old, keys_new, &mut edits);
        }
        let mut hunks = hunks(old, new, &edits, self.context);
        let m = moves(keys_old, keys_new, &edits, self.min_moved);
```

**The three built-in differs** (`core/src/differ.rs:217-253`) are each a
four-line wrapper over `Scratch::diffed` with a different `max`:
`Histogram` passes `Some(MAX_ANCHOR_OCCURRENCES)`, `Patience` `Some(1)`,
`Myers` `None`.

**Two other `Differ` implementations exist and are tests, not production code** —
`app/src/config.rs:1294` (`Semantic`, a rule-1 registration test) and
`git/src/lib.rs:3638` (`Counting`). Both must keep compiling **untouched**, which
is why the new trait method gets a default body.

**Repo conventions**: doc comments explain *why*. Tests live in the
`#[cfg(test)] mod tests` at the bottom of the same file; `verify(old, new, edits)`
(`core/src/differ.rs:1933`) asserts every structural property an edit script must
have and is what new differ tests call.

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Core tests | `cargo test -q -p gitten-core` | exit 0, all pass |
| Whole workspace | `cargo test -q --workspace` | exit 0, all pass |
| Lint | `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 |
| Format | `cargo fmt --all --check` | exit 0 |
| **The gate** | `cargo run -q -p gitten-git --example diffcheck --release . HEAD~4..HEAD` | exit 0, all six modes agree with git |
| Whole-history gate | `cargo run -q -p gitten-git --example diffcheck --release . $(git rev-list --max-parents=0 HEAD \| tail -1)..HEAD` | exit 0 |

`diffcheck` runs all six modes including the three whitespace ones. It is the
correctness gate for this plan and it must pass **before and after**.

## Scope

**In scope**:
- `core/src/differ.rs`

**Out of scope** (do NOT touch):
- `app/src/config.rs` and `git/src/lib.rs` — they contain `Differ` impls that
  must keep compiling with no edit. If either needs changing, the default body on
  the new trait method is wrong. That is a STOP condition.
- `compact_with` and `moves` — they compare `Arc<str>` handles and could take ids
  too. That is a **deliberate follow-up**, not this plan: it widens the diff and
  muddies attribution of the measurement.
- The `Whitespace` normalisation rules themselves. This plan changes how a key
  is *carried*, never what it *is*.
- `MAX_ANCHOR_OCCURRENCES`, `MAX_STEPS`, `MIN_INTRALINE_SIMILARITY`.

## Git workflow

- Branch: `advisor/perf-061-whitespace-keys-yield-ids`
- Commit message style, from `git log`: lowercase, `scope: sentence`, e.g.
  `core: a whitespace key is interned once, not twice`.
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: Record the baseline

Before any edit, capture the numbers and prove the gate is green today:

```sh
cargo run -q -p gitten-git --example diffcheck --release . HEAD~4..HEAD
```

Save the full output to a file — it prints a per-mode timing and the
count/position verdicts. You will diff your final run against it.

**Verify**: exits 0. If it does not, STOP — the gate must be green before you
change anything, or you cannot attribute a later failure.

### Step 2: Have the arena hand back its id

Change `KeyArena::intern` to return both the id and the handle, since it computes
the id already:

```rust
    /// The id and handle for `key`'s content, inserting it when new. Ids are
    /// dense from zero and equal exactly when the content is, which is what lets
    /// a differ compare `u32`s without hashing the text a second time.
    fn intern(&mut self, key: &str) -> (u32, Arc<str>) {
```

Return `(id, Arc::clone(&self.keys[id as usize]))` on both the hit and the miss
path.

Then widen `Whitespace::keys` to fill an id vector alongside the handles:

```rust
    fn keys(
        self,
        lines: &[Arc<str>],
        arena: &mut KeyArena,
        out: &mut Vec<Arc<str>>,
        ids: &mut Vec<u32>,
    ) {
```

`Whitespace::Exact` leaves **both** output vectors alone, exactly as it leaves
`out` alone today — the `Exact` path never calls this.

Update the caller at `core/src/differ.rs:1856-1865` to allocate and pass the two
id vectors.

**Verify**: `cargo test -q -p gitten-core` → exit 0. Nothing about the answers has
changed yet; the ids are computed and unused.

### Step 3: Add the interned entry point to the `Differ` trait

Add a second method **with a default body**, so no existing implementation needs
an edit:

```rust
    /// The same answer as [`Differ::diff`], for a caller that has already
    /// interned the keys. `ids` are dense from zero and equal exactly when the
    /// keys are, so an implementation whose inner loops compare lines can skip a
    /// hash pass the caller has already paid for.
    ///
    /// The default ignores them, which is the right answer for an implementation
    /// that needs the text itself — a semantic differ reads words, not numbers.
    /// Overriding this is an optimisation and never a change of answer: both
    /// methods must return the same edit script for the same input, which is
    /// what `histogram_interned_and_text_paths_agree` pins.
    fn diff_interned(
        &self,
        path: &str,
        old: &[Arc<str>],
        new: &[Arc<str>],
        ids: (&[u32], &[u32]),
    ) -> Vec<Edit> {
        let _ = ids;
        self.diff(path, old, new)
    }
```

**Verify**: `cargo test -q --workspace` → exit 0. `app/src/config.rs` and
`git/src/lib.rs` must compile with **no edit**. If either fails to compile, STOP.

### Step 4: Split `Scratch::diffed` so the ids can come from outside

Extract the part after interning into its own method:

```rust
    /// The search itself, over ids the caller supplies. `diffed` is this with an
    /// interning pass in front of it.
    fn diffed_ids(&mut self, a: &[u32], b: &[u32], max: Option<u32>) -> Vec<Edit> {
        self.begin_file();
        let mut out = Vec::new();
        match max {
            Some(max) => self.anchored(a, b, max, &mut out),
            None => self.myers(a, b, Region::whole(a, b), &mut out),
        }
        out
    }
```

Then rewrite `diffed` to call it, preserving the scratch-buffer handback that
`core/src/differ.rs:392-394` documents ("Handed back rather than dropped: the next
file starts at full size").

**Watch the ordering**: `begin_file()` is called at the top of `diffed` today.
It must be called exactly once per file in the new arrangement too — not twice,
and not zero times.

**Verify**: `cargo test -q -p gitten-core` → exit 0, and
`cargo run -q -p gitten-git --example diffcheck --release . HEAD~4..HEAD` → exit 0
with output identical in verdicts to Step 1's saved baseline.

### Step 5: Override `diff_interned` in the three built-ins

For `Histogram`, `Patience` and `Myers` (`core/src/differ.rs:217-253`), add:

```rust
    fn diff_interned(
        &self,
        _path: &str,
        _old: &[Arc<str>],
        _new: &[Arc<str>],
        ids: (&[u32], &[u32]),
    ) -> Vec<Edit> {
        self.scratch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .diffed_ids(ids.0, ids.1, Some(MAX_ANCHOR_OCCURRENCES))
    }
```

with each differ's own `max` (`Some(MAX_ANCHOR_OCCURRENCES)` / `Some(1)` / `None`)
— the same value its `diff` passes. **A mismatch here silently turns one
algorithm into another and `diffcheck` will catch it; do not guess, copy each
one's existing argument.**

**Verify**: `cargo test -q -p gitten-core` → exit 0.

### Step 6: Route the whitespace path through it

In `assemble`, take the ids as an `Option` and pick the entry point:

```rust
        // Ids when the caller interned them — the whitespace relations do,
        // because normalising a line is already a pass over it. `Exact` has
        // nothing interned yet and the differ does its own.
        let mut edits = match ids {
            Some(ids) => differ.diff_interned(path, keys_old, keys_new, ids),
            None => differ.diff(path, keys_old, keys_new),
        };
```

`compute`'s `Whitespace::Exact` arm passes `None`; the whitespace arm passes
`Some((&ido, &idn))`.

**Verify**, all of these:
- `cargo test -q --workspace` → exit 0
- `cargo fmt --all --check` → exit 0
- `cargo clippy --workspace --all-targets -- -D warnings` → exit 0
- `cargo run -q -p gitten-git --example diffcheck --release . HEAD~4..HEAD` → exit 0,
  **verdicts identical to Step 1's baseline**
- `cargo run -q -p gitten-git --example diffcheck --release . $(git rev-list --max-parents=0 HEAD | tail -1)..HEAD` → exit 0

Report the before/after per-mode timings `diffcheck` prints for `ws-eol`,
`ws-change` and `ws-all`.

## Test plan

New tests in `core/src/differ.rs`'s `mod tests`:

1. `histogram_interned_and_text_paths_agree` — build a non-trivial old/new pair,
   intern it with `intern(&old, &new)`, call `Histogram::diff` and
   `Histogram::diff_interned` on the same input, and `assert_eq!` the two edit
   scripts. Repeat for `Patience` and `Myers` (one test each, or a loop over
   `Box<dyn Differ>`). This is the test that makes the override an optimisation
   rather than a second algorithm.
2. `a_default_differ_ignores_ids` — define a local `struct Text;` implementing
   only `name` and `diff` (returning a fixed script), call `diff_interned` on it
   with deliberately wrong ids (e.g. all zeros of the right length), and assert it
   returns the same script `diff` does. Pins the default body, which is what
   protects the two out-of-scope extension impls.
3. `whitespace_keys_and_ids_agree` — for each non-`Exact` `Whitespace` variant,
   run `keys` over a line set containing duplicates-under-the-relation
   (e.g. `"a  b"`, `"a b"`, `"a\tb"`) and assert: `ids.len() == lines.len()`,
   and `ids[i] == ids[j]` **iff** `out[i] == out[j]`. That biconditional is the
   whole correctness claim of the change.
4. `whitespace_ids_are_shared_across_both_sides` — intern old **and** new through
   one arena, and assert a line present in both sides gets the same id. If ids
   were per-side the differ would find nothing in common; this is the highest
   consequence bug available here.

Existing tests that must pass unmodified — everything using `verify()`, and the
whole of `cargo test -q --workspace`.

**Verification**: `cargo test -q -p gitten-core` → all pass, including 4+ new tests.

## Done criteria

ALL must hold:

- [ ] `cargo fmt --all --check` exits 0
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` exits 0
- [ ] `cargo test -q --workspace` exits 0
- [ ] `diffcheck` on both revspecs exits 0, with verdicts identical to Step 1's baseline
- [ ] The four new tests exist and pass
- [ ] `git diff da9f8a7..HEAD --name-only` lists `core/src/differ.rs` and this
      plan's status row, and nothing else
- [ ] `app/src/config.rs` and `git/src/lib.rs` are unmodified
- [ ] `core/Cargo.toml` has no new dependency
- [ ] Before/after `ws-eol` / `ws-change` / `ws-all` timings reported

## STOP conditions

Stop and report back — do not improvise — if:

- The excerpts above do not match the live code (drift).
- `diffcheck` exits non-zero, or any verdict differs from Step 1's baseline, at
  any step. **This is the one failure you must never work around.** A whitespace
  mode disagreeing with `git` means the ids are not equivalent to the handles;
  report which mode and which file.
- Compiling requires an edit to `app/src/config.rs` or `git/src/lib.rs`. It means
  the default body on `diff_interned` is missing or wrong.
- You find that `compact_with` or `moves` also need ids to make this work. They
  do not — they take the `Arc<str>` handles, which this plan keeps. If you believe
  otherwise, report rather than widening the change.
- `begin_file()` ends up called a different number of times per file than before.

## Maintenance notes

- **What a reviewer should scrutinize**: (a) each built-in's `diff_interned`
  passing the *same* `max` as its `diff`; (b) `begin_file()` called exactly once
  per file; (c) the scratch-buffer handback in `diffed` surviving the split;
  (d) `diffcheck`'s position column, not just its counts — `docs/measurements.md`
  records two bugs found only there.
- **What will interact with this**: an extension overriding `diff_interned` takes
  on the obligation that both methods agree. The doc comment says so; keep it.
- **Deliberately deferred**: `compact_with` and `moves` still compare `Arc<str>`
  handles under a whitespace relation and would be cheaper on ids. Doing them in
  this plan would blur which change bought which milliseconds. Measure this one
  first, then decide.
