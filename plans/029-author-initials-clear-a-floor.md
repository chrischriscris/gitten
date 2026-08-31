# Plan 029: Author initials clear a contrast floor, because they are text

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat 87229df..HEAD -- core/src/theme.rs shell/src/views/commits.rs tui/src/commits.rs web/src/api.rs`
> If any in-scope file changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition.

## Status

- **Priority**: P3
- **Effort**: S
- **Risk**: LOW
- **Depends on**: none. **Replaces**: plan 027 (superseded — see below)
- **Category**: UI / robustness
- **Planned at**: commit `87229df`, 2026-08-30

## Why this matters

This is the surviving half of plan 027, which was blocked in review.

Plan 027 proposed flooring three graph colours. That was wrong for two of
them: `docs/decisions/0020-furniture-has-its-own-floor.md` names
`lane_overflow` explicitly — *"`faint` is a border, `rule` is a hairline,
`lane_overflow` is a stroke — none of them has a legibility floor, and a 1px
line held to a text floor is a bright seam"* — and `docs/theming.md:223`
groups `lanes`/`lane_overflow` under `graph`, apart from the `furniture` group
that holds only `gutter_fg`. Those two stay unfloored. **Do not touch them.**

Author initials are a different thing and 0020 does not cover them. They are
**text**: `shell/src/views/commits.rs:209` draws them with
`.text_color(rgb(host.theme.author(&c.author)))`, two letters in their own
column, on a background they do not choose — which is 0020's own stated test
for what needs a floor (*"The gutter is the one piece of furniture that is
text, on backgrounds it does not choose"*). `docs/theming.md:224` even gives
them their own `commits` group. Yet `Theme::author` returns a raw palette
entry, so unlike every syntax colour it is never resolved.

Nothing is broken today: the shipped `authors` palettes measure ≥ 4.36:1
against `chrome.bg`, well clear. This is a **guard, not a fix** — `[theme]
authors` is user-editable in `gitten.toml` (`app/src/config.rs:569-575`) and a
hand-written palette can currently ship initials nobody can read, with no
warning, in the one place the theme system otherwise guarantees legibility.
Judge it on that modest basis.

## Current state

- `core/src/theme.rs:239` — the raw field, user-editable via config:

  ```rust
  pub authors: Vec<Rgb>,
  ```

- `core/src/theme.rs:~583-591` — the accessor, returning the raw entry:

  ```rust
  /// Stable per author name, so one person's commits clump visibly in a long
  /// list without anyone assigning colours by hand.
  pub fn author(&self, author: &str) -> Rgb {
      if self.authors.is_empty() {
          return self.chrome.dim;
      }
      let hash = author
          .bytes()
          .fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32));
      self.authors[hash as usize % self.authors.len()]
  }
  ```

- `core/src/theme.rs:509-525` — `rebuild()`, the pattern to extend. It already
  resolves syntax against `min_contrast` and `diff.gutter_fg` against
  `min_furniture`, each into a private table, once:

  ```rust
  for surface in Surface::ALL {
      let bg = self.background(surface);
      self.gutter[surface.index()] = readable(self.diff.gutter_fg, bg, self.min_furniture);
  }
  ```

- `core/src/theme.rs:707-724` — `readable(fg, bg, target)` returns `fg`
  untouched when it already clears `target`. This is what makes the change
  invisible on all three shipped themes.

- **Performance constraint**: `Theme::author` is called once per visible row
  per frame (`shell/src/views/commits.rs:207-209`, whose comment accepts a
  byte-fold + modulo as the cost). `readable` is ~6 `powf`. So the resolution
  MUST happen in `rebuild()` into a stored table — never inside `author()`.

- Consumers: `shell/src/views/commits.rs:209`, `tui/src/commits.rs` (grep
  `theme.author(`), and `web/src/api.rs:133-134` which serializes the raw list
  to the browser.

- `rebuild()` is already called after every config apply
  (`app/src/config.rs:583`) and by `set_syntax`; shipped themes construct
  through `rebuilt()`.

- Test conventions: `core/src/theme.rs:731+`, e.g.
  `every_token_is_legible_on_every_surface`, the furniture-floor test near
  `:896`, and `raising_the_floor_is_one_field_and_a_rebuild` at `:833` for the
  set-then-rebuild shape.

## Which floor

Use **`min_furniture`** (3.0), not `min_contrast`. An initial is glanced at,
like a line number — 0020's whole argument for two floors. Say so in the
comment.

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Core tests | `cargo test -q -p gitten-core` | exit 0 |
| Consumers | `cargo test -q -p gitten-shell -p gitten-tui -p gitten-web -p gitten-app` | exit 0 |
| Lint / format | `cargo clippy -q --workspace --all-targets -- -D warnings && cargo fmt --check` | exit 0 |

## Scope

**In scope**:
- `core/src/theme.rs`
- `web/src/api.rs` (serialize the resolved list — one line)

**Out of scope** (do NOT touch):
- `lanes`, `lane_overflow`, and every consumer of them — `shell/src/graph.rs`,
  `tui/src/commits.rs`'s overflow sites. Decision 0020 governs; leaving them
  raw is correct. **Touching them fails review.**
- The shipped `authors` hex values.
- `min_furniture` / `min_contrast` values.
- `app/src/config.rs` — the raw field stays `pub` and config keeps writing it.

## Git workflow

- Branch: `advisor/019-author-initials-clear-a-floor`
- Commit style: imperative, why-first — e.g.
  `Floor the author initials: they are text, unlike the lanes beside them`.
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: resolve the author palette in `rebuild()`

In `core/src/theme.rs`, add a private `authors_on_bg: Vec<Rgb>` beside
`resolved`/`gutter`, doc-commented in the file's voice: why it is resolved
once (per-row-per-frame accessor), why `min_furniture` and not `min_contrast`
(glanced, like a line number), and why the lanes beside it are deliberately
*not* here (0020 — they are strokes). Fill it at the end of `rebuild()`:

```rust
self.authors_on_bg = self
    .authors
    .iter()
    .map(|&c| readable(c, self.chrome.bg, self.min_furniture))
    .collect();
```

Switch `author()` to index `authors_on_bg`, keeping the `is_empty()` →
`chrome.dim` fallback exactly as it is. Initialize the new field at every
construction site the compiler names.

**Verify**: `cargo test -q -p gitten-core` → exit 0.

### Step 2: the web sends what the other clients draw

`web/src/api.rs:133-134` serializes `&t.authors`. Add a public accessor
returning the resolved slice and use it, so all three clients draw the same
initials. Name it so clippy's `misnamed_getters` does not fire — it returns a
different value than the same-named field, so `resolved_authors()` (not
`authors()`).

Leave the `lanes` serialization alone.

**Verify**: `cargo test -q -p gitten-web -p gitten-core` → exit 0;
`cargo clippy -q --workspace --all-targets -- -D warnings` → exit 0.

### Step 3: tests

In `core/src/theme.rs`'s test module, beside the furniture-floor test:

1. `author_initials_clear_the_furniture_floor`: for every shipped theme, for a
   handful of names, assert `contrast(t.author(name), t.chrome.bg) >=
   t.min_furniture`. Passes before this change too (the shipped palettes are
   already clear) — it is a guard against a future palette, and its comment
   should say exactly that so nobody mistakes it for a regression test.
2. `a_hostile_author_palette_is_floored`: set `t.authors = vec![t.chrome.bg]`,
   call `t.rebuild()`, assert the result clears the floor. **This one fails
   before the change** and is the real test.
3. `the_lanes_are_deliberately_not_floored`: set `t.lanes = vec![t.chrome.bg]`
   and `t.lane_overflow = t.chrome.bg`, `rebuild()`, and assert `t.lane(0)`
   and `t.lane_overflow` come back **unchanged** — pinning 0020 so a future
   pass does not "fix" them by reflex, the way plan 027 tried to. Cite the
   decision record in the comment.

**Verify**: `cargo test -q -p gitten-core` → exit 0, three new tests pass.
Then confirm test 2 is real: temporarily revert `author()` to index
`self.authors`, run it, confirm it FAILS, restore.

### Step 4: gate

**Verify**: `cargo test -q -p gitten-core -p gitten-shell -p gitten-tui -p gitten-web -p gitten-app` → exit 0;
`cargo fmt --check && cargo clippy -q --workspace --all-targets -- -D warnings` → exit 0.

## Test plan

The three tests in Step 3, with test 2 mutation-checked. Existing theme tests
stay green. Note: any *existing* test that writes `t.authors` directly and then
calls `t.author(...)` will now need a `t.rebuild()` between them — that is a
true consequence of the change, not a workaround; update such tests and list
them in your report.

## Done criteria

- [ ] `grep -n 'authors_on_bg' core/src/theme.rs` shows the field, the fill in `rebuild()`, and the use in `author()`
- [ ] `grep -n 'lane_overflow' core/src/theme.rs` shows it still returned raw, with no `readable` applied
- [ ] `cargo test -q -p gitten-core -p gitten-shell -p gitten-tui -p gitten-web -p gitten-app` exits 0 with the three new tests
- [ ] `cargo fmt --check` and clippy clean
- [ ] `git diff --stat` shows only `core/src/theme.rs` and `web/src/api.rs`
- [ ] `plans/README.md` status row updated

## STOP conditions

- Any change you are about to make touches `lanes` or `lane_overflow`
  resolution — stop; that is 017's mistake and 0020 forbids it.
- `a_hostile_author_palette_is_floored` passes *before* the change (would mean
  the resolution already happens somewhere and this plan is redundant).
- A decision record or doc you find says author colours are also deliberately
  unfloored — stop and report it, the way 017's executor correctly did.

## Maintenance notes

- The rule this establishes, for the next palette field: **is it drawn as
  glyphs?** Text gets a floor (`min_contrast` for body, `min_furniture` for
  glanced furniture); strokes, hairlines and surfaces do not. 0020 is the
  record; `docs/theming.md`'s group table is the quick reference.
- Reviewers: confirm no `readable` call landed on a per-frame path, and that
  the lanes are untouched.
- Deferred: whether `lane_overflow` at 1.87:1 is *too* quiet is a real design
  question, but it is a question for a decision record with measurements
  behind it, not a drive-by change. If it is ever revisited, the honest
  framing is "is a 2px stroke at 1.87:1 visible enough to carry the meaning
  the cap depends on", and the answer may still be yes.
