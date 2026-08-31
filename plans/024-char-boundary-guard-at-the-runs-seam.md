# Plan 024: Guard the runs seam against mid-character offsets

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat d53a0c7..HEAD -- core/src/runs.rs core/src/select.rs core/src/prepared.rs`
> If any in-scope file changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition.

> **REBASED ONTO `full/full` — 2026-08-30.** This plan was first written
> against `main` (`87229df`) and executed there; that branch is discarded.
> The new base is **`full/full` (`d53a0c7`)**, 127 commits ahead.
>
> **The two files this plan actually changes are byte-identical on
> `full/full`**: `core/src/prepared.rs` and `core/src/select.rs` are
> unchanged between `main` and `full/full`, so every excerpt and line number
> in the "Current state" section below is still exact. `Selected::range` is
> still `range(self, len: usize)` there, and `prepared.rs` still has
> `is_char_boundary` only inside `#[cfg(test)]`. The finding is fully live.
>
> What moved is the **call sites** you must update in Step 3, all of them
> mechanical. `shell/src/views/diff.rs` (+1805/-370), `markdown.rs`,
> `split.rs` and `tui/src/rows.rs` were all rewritten, so the list of
> `.range(...)` / `selected(...)` callers will differ from any earlier run.
> Do not work from a remembered list — let `cargo build --workspace` name
> them, and report the list you actually changed.
>
> Step 2's readout: `arrange()` in `shell/src/views/diff.rs` still builds a
> `reports` vector; append there, only when nonzero. If the tui's status line
> still cannot reach `Prepared::rejected()` without editing
> `core/src/rows.rs`, skip the tui and say so — same ruling as before.

## Status

- **Priority**: P1
- **Effort**: S
- **Risk**: LOW
- **Depends on**: none
- **Category**: bug (latent panic at an extension seam)
- **Planned at**: originally `87229df` (main); **rebased onto `d53a0c7` (`full/full`), 2026-08-30**

## Why this matters

Every presentation in every client slices line text through one funnel:
`core::runs` computes byte ranges from syntax tokens, intraline spans and the
selection, and the client does `&text[run.at]`. A range end that is inside a
multi-byte UTF-8 character panics that slice — on the render path, taking the
window down. Today three inputs can deliver such an offset and none is
checked: a `Differ`/`Highlighter` implementation (a documented extension
seam), and `Selected::range`, whose length-clamp can land mid-character. The
codebase already treats the analogous seam correctly: `Wrap` output is
validated break-by-break and bad ones are counted and dropped
(`core/src/wrap.rs:431-448`). This plan gives runs and selection the same
contract. Three previously-filed findings (BUG-05, BUG-11, BUG-14 in
`plans/README.md`) are closed by this one change because the funnel is shared.

## Current state

- `core/src/runs.rs` — the funnel. `runs_selected` clamps the selection by
  *value* only:

  ```rust
  // core/src/runs.rs:127-129
  // Clamped into `at` like tokens and spans — a drag across several rows
  // reaches each one as the part of itself it covers.
  let sel = sel.start.clamp(at.start, at.end)..sel.end.clamp(at.start, at.end);
  ```

  and folds token/span edges in with `.min` only (range-clamped, never
  boundary-checked):

  ```rust
  // core/src/runs.rs:170-186
  let mut edge = at.end;
  match tok {
      Some(t) => edge = edge.min(t.end as usize),
      None => {
          if let Some(t) = tokens.get(ti) {
              edge = edge.min(t.start as usize);
          }
      }
  }
  // ... same for spans, then sel.start / sel.end fold in below
  ```

  **Crucially, `runs`/`runs_selected` never see the text** — their signature
  is `(at: Range<usize>, tokens, spans, kind, moved, sel, out)`. The boundary
  check cannot live here without a signature change (see Steps for where it
  goes instead).

- `core/src/select.rs:166-169` — the one public exit for selection ranges,
  length-clamp only:

  ```rust
  /// The bytes of a text `len` long that are selected, clamped into it.
  pub fn range(self, len: usize) -> Range<usize> {
      self.from.min(len)..self.to.min(len)
  }
  ```

  `Caret::off` (`select.rs:113-118`) *documents* "Always on a character
  boundary" as an invariant the frontend upholds — an unenforced contract on a
  public field. The fix pattern already exists in this file: `word_at` walks
  back to a boundary at `select.rs:390-393`
  (`while off > 0 && !text.is_char_boundary(off) { off -= 1; }`), with a
  two-byte `é` test near `select.rs:637`.

- `core/src/prepared.rs:164-178` — where extension-supplied spans and tokens
  enter a `Line` unvalidated:

  ```rust
  let mut spans: Vec<Vec<Span>> = vec![Vec::new(); h.lines.len()];
  // ...
  for (d, a) in replace_pairs(h) {
      let (o, n) = intraline(&texts[d], &texts[a]);
      spans[d] = o;
      spans[a] = n;
  }
  // ...
  let mut tokens = highlight_hunk(hl, &f.path, &refs, &kinds);
  ```

  (`intraline` is built-in and safe today; `highlight_hunk` calls whatever
  `Highlighter` the host routes to — the extension seam.)

- `core/src/wrap.rs:425-448` — the exemplar to copy. `Wrapped::take` validates
  each `Break` (in range, ascending, `is_char_boundary` both ends), keeps the
  good ones, and counts the bad in `rejected`, which `Flat::report` surfaces
  on the stats overlay. Its doc comment states the policy: *"a range that
  points past its line is a slice panic on the render path, and `Wrap` is a
  seam an extension reaches."*

- Consumers that would panic: `shell/src/views/diff.rs:1782, 1903, 1913`
  (`&text[at]` on run ranges), `tui/src/rows.rs:328-330` (slices around a
  selection range). Do not modify them — the fix is upstream.

- Note: `core/` has **zero dependencies** and that is a hard rule. Everything
  below is std-only.

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Core tests | `cargo test -q -p gitten-core` | exit 0, sub-second |
| All clients still build/test | `cargo test -q -p gitten-shell -p gitten-tui -p gitten-web` | exit 0 |
| Lint | `cargo clippy -q --workspace --all-targets -- -D warnings` | exit 0 |
| Format | `cargo fmt --check` | exit 0 |

## Scope

**In scope** (the only files you should modify):
- `core/src/prepared.rs`
- `core/src/select.rs`
- `plans/README.md` (status row)

**Out of scope** (do NOT touch, even though they look related):
- `core/src/runs.rs` — it has no access to the text and changing its public
  signature ripples through every client; the guard belongs where text and
  ranges meet (below).
- `shell/`, `tui/`, `web/` — consumers; they are what this plan protects.
- `core/src/wrap.rs` — already correct; it is the pattern, not a target.
- `core/src/differ.rs` — `Edit` ranges are *line* indices, not byte offsets;
  they cannot cause a slice panic and are already clamped downstream.

## Git workflow

- Branch: `advisor/014-char-boundary-guard-at-the-runs-seam`
- Commit style: imperative sentence, no prefix, why-first — e.g.
  `Sanitize spans and tokens where they enter a Line, like Wrap breaks`.
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: sanitize spans and tokens as they enter `Line`

In `core/src/prepared.rs`, add a private helper (near the top of the file,
doc-commented in the file's voice — explain *why*, citing the Wrap precedent):

```rust
/// Drops any span/token whose ends are not on character boundaries of `text`
/// or run past it, counting what it dropped. The same contract `Wrapped::take`
/// enforces for breaks, for the same reason: these come through a seam an
/// extension reaches, and a bad range is a slice panic on the render path.
fn sane<T: Copy>(text: &str, items: &mut Vec<T>, at: impl Fn(&T) -> (usize, usize), rejected: &mut usize)
```

(Exact shape is yours; a plain function per type is fine too. `Span` and
`Token` both expose `start`/`end` as `u32` — check their definitions in
`core/src/prepared.rs`/`core/src/syntax.rs` and adapt.) The predicate per
item, matching `Wrapped::take`:
`end >= start && end <= text.len() && text.is_char_boundary(start) && text.is_char_boundary(end)`.
Also enforce ascending order (each `start >=` previous `end`) — `runs` relies
on ordered inputs to terminate correctly.

Apply it in the hunk loop (current `prepared.rs:164-183`) to each line's
`spans[i]` and its tokens before they are moved into `Line { spans, tokens }`.
Accumulate a rejected count.

**Verify**: `cargo test -q -p gitten-core` → exit 0 (built-ins produce only
valid ranges, so nothing changes).

### Step 2: surface the rejected count

Mirror `Wrapped::rejected` (`core/src/wrap.rs`, used by `Flat::report` in
`core/src/rows.rs:241-260`): expose the count on `Prepared` (or per `File`,
whichever is less invasive — `Prepared` already carries load-report fields
like `intraline`/`threads`; follow that pattern in `core/src/prepared.rs`'s
struct) and append `"{n} spans/tokens rejected"` to whatever report string the
stats overlay reads, exactly the way wrap rejections are reported: only when
nonzero. Find the report plumbing with `grep -n "rejected" core/src/rows.rs
core/src/prepared.rs` and match it.

**Verify**: `cargo test -q -p gitten-core` → exit 0.

### Step 3: clamp `Selected::range` to char boundaries

`Selected::range` (`core/src/select.rs:167-169`) currently takes only `len`.
Change the clamp to walk each end down to a boundary. Since it does not have
the text, change its signature to take `&str` instead of `usize`:

```rust
/// The bytes of `text` that are selected, clamped into it and onto character
/// boundaries — a frontend derives offsets from columns so a mid-character
/// end should be impossible, but this is the one exit for a range that will
/// be sliced, and "should" is not a contract (see `word_at`, which already
/// walks back for the same reason).
pub fn range(self, text: &str) -> Range<usize> {
    let snap = |mut off: usize| {
        off = off.min(text.len());
        while off > 0 && !text.is_char_boundary(off) {
            off -= 1;
        }
        off
    };
    let (from, to) = (snap(self.from), snap(self.to));
    from..to.max(from)
}
```

Then fix the callers: `grep -rn "\.range(" tui/src shell/src web/src | grep -v test`
plus test callers. Expected call sites include `tui/src/rows.rs` (~line 328
area) and shell/web equivalents — each currently passes `text.len()`; pass
`text` instead. This is a compile-guided rename: `cargo build --workspace`
lists every site.

**Verify**: `cargo test -q -p gitten-core -p gitten-tui -p gitten-shell -p gitten-web` → exit 0.

### Step 4: tests

In `core/src/prepared.rs`'s test module: build a `File` whose hunk lines
contain multi-byte text (use `é`/`日` — the repo's own tests use `é`, see
`core/src/select.rs:637` area), inject a span and a token whose `end` lands
mid-character and one that overruns `len`, run the prepare path, and assert
(a) they are gone from the resulting `Line`, (b) the rejected count is exact,
(c) a valid span on the same line survived. Model the fixture-building on the
existing tests in that module (there are asserts on `is_char_boundary` at
`prepared.rs:444-445` to crib from).

In `core/src/select.rs`'s test module: a selection whose `to` lands inside a
two-byte `é` comes back snapped to the boundary; slicing the text with the
result does not panic. Model on the `word_at` boundary test near `:637`.

**Verify**: `cargo test -q -p gitten-core` → exit 0 including the new tests.

### Step 5: full gate

**Verify**: `./check.sh; echo $?` → 0. `cargo fmt --check && cargo clippy -q --workspace --all-targets -- -D warnings` → exit 0.

## Test plan

Covered in Step 4. Cases: mid-character span end (dropped, counted),
overrunning token (dropped, counted), valid neighbour (kept), selection end
mid-`é` (snapped), empty selection unaffected, `range` on empty text → `0..0`.

## Done criteria

Machine-checkable. ALL must hold:

- [ ] `grep -n 'is_char_boundary' core/src/prepared.rs` shows the guard in
      production code (not only in `#[cfg(test)]`)
- [ ] `grep -n 'is_char_boundary' core/src/select.rs` shows it inside `range`
- [ ] `cargo test -q -p gitten-core -p gitten-shell -p gitten-tui -p gitten-web -p gitten-app -p gitten-git` exits 0
- [ ] New tests from Step 4 exist and pass
- [ ] `./check.sh` exits 0
- [ ] `cargo fmt --check` and `cargo clippy -q --workspace --all-targets -- -D warnings` exit 0
- [ ] No files outside the in-scope list are modified except mechanical
      `range()` call-site updates in `tui/`, `shell/`, `web/` (`git status` —
      list them in your report)
- [ ] `plans/README.md` status row updated

## STOP conditions

Stop and report back (do not improvise) if:

- Step 1's guard makes an existing test fail — that means a *built-in*
  produces an invalid range today, which is a live bug worth its own report,
  not a silent drop.
- `Selected::range`'s signature change ripples into more than ~6 call sites
  or into `core/src/runs.rs` itself.
- The code at the "Current state" locations doesn't match the excerpts.

## Maintenance notes

- Anyone adding a third input to `runs` (beyond tokens/spans/selection) must
  route it through the same sanitize step; the guard is at the *entry* to
  `Line`, not inside `runs`, and that only works while `Line` is the sole
  source of run inputs.
- `differ::verify` (test-only, `core/src/differ.rs:1737`) remains the
  edit-script analogue; promoting it to a `debug_assert` on `Differ::diff`
  output was considered and deferred — edit ranges are line indices, not byte
  offsets, so they cannot panic a slice. Revisit if an extension differ ships.
- Reviewers: confirm the drop-and-count semantics (losing colour, never
  content) and that nothing on the happy path allocates — the guard must be a
  filter pass, not a rebuild.
