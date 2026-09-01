# Plan 027: The overflow lane (and every graph colour) clears the furniture contrast floor

> **SUPERSEDED — DO NOT EXECUTE. Blocked 2026-08-30 after review.**
>
> This plan's central premise is wrong and contradicts a binding decision
> record. `docs/decisions/0020-furniture-has-its-own-floor.md`, under "Why not
> resolve every chrome colour this way", names `lane_overflow` explicitly:
> *"`faint` is a border, `rule` is a hairline, `lane_overflow` is a stroke —
> none of them has a legibility floor, and a 1px line held to a text floor is a
> bright seam."* `docs/theming.md:223` agrees, grouping `lanes`/`lane_overflow`
> under `graph`, separate from the `furniture` group that holds only
> `gutter_fg`. Per README's rule, the record wins until numbers beat it.
>
> The 1.87:1 measurement in this plan is accurate; the conclusion drawn from it
> is not. The advisor verified the arithmetic and failed to verify the
> normative claim that `lane_overflow` belongs in the furniture bucket — that
> claim came from an audit subagent and should have been checked against 0020
> during vetting.
>
> What survives: **author initials are text** (`shell/src/views/commits.rs:209`
> draws them with `.text_color(...)`), they sit in their own `commits` group,
> and 0020 does not cover them — so the "a hand-written `gitten.toml` palette
> can ship illegible initials" half of the motivation is still live and is
> re-planned, narrowed, as **plan 029**.
>
> Executing this plan as written would need a new decision record superseding
> 0020's classification of `lane_overflow` first. That is the maintainer's
> call, not an executor's.


> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat 87229df..HEAD -- core/src/theme.rs shell/src/graph.rs tui/src/commits.rs`
> If any in-scope file changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition.

## Status

- **Priority**: P2
- **Effort**: S
- **Risk**: LOW
- **Depends on**: none
- **Category**: bug / UI
- **Planned at**: commit `87229df`, 2026-08-30

## Why this matters

The graph gutter is capped at 12 lanes and everything past the cap collapses
onto the last column drawn in `lane_overflow` — the project's own docs call
this "the overflow drawn honestly": the one place the graph admits it is not
showing everything. On **all three shipped themes** `lane_overflow` measures
**1.87:1** against `chrome.bg` (WCAG, computed from the committed hex values
0x453f39/0x0e0d0c, 0xbbb7b0/0xfaf7f1, 0x3a444d/0x0f1319) — well below the
3.0 furniture floor the theme enforces *by test* for gutters and hunk
markers. The honesty mark is effectively invisible. Separately, `lanes` and
`authors` are user-editable in `gitten.toml` and reach the screen unresolved,
so a hand-written palette can be illegible with no warning — unlike every
syntax colour, which is floored through `readable`. This plan routes all
three through the same resolution `rebuild()` already applies to syntax and
gutter colours, and pins them with the same style of test.

## Current state

- `core/src/theme.rs:230-239` — the raw palette fields:

  ```rust
  /// Cycled per graph lane, and per author for the initials column. Any
  /// length; the drawing code takes them modulo.
  pub lanes: Vec<Rgb>,
  pub lane_overflow: Rgb,
  pub authors: Vec<Rgb>,
  ```

  followed by the private resolved-storage pattern this plan extends:
  `resolved: Vec<Style>` (syntax × surface) and the gutter equivalent, both
  filled by `Theme::rebuild`.

- `core/src/theme.rs:509-525` — `rebuild()` resolves syntax (against
  `min_contrast`) and `diff.gutter_fg` (against `min_furniture`) per surface,
  and touches nothing else:

  ```rust
  pub fn rebuild(&mut self) {
      self.resolved = vec![Style::default(); Kind::COUNT * Surface::COUNT];
      for kind in Kind::ALL { for surface in Surface::ALL {
          let base = self.syntax[kind.index()];
          let bg = self.background(surface);
          self.resolved[...] = Style { fg: readable(base.fg, bg, self.min_contrast), ..base };
      }}
      for surface in Surface::ALL {
          let bg = self.background(surface);
          self.gutter[surface.index()] = readable(self.diff.gutter_fg, bg, self.min_furniture);
      }
  }
  ```

- `core/src/theme.rs:573-591` — the accessors return raw palette entries:

  ```rust
  pub fn lane(&self, i: usize) -> Rgb {
      if self.lanes.is_empty() { return self.chrome.fg; }
      self.lanes[i % self.lanes.len()]
  }
  pub fn author(&self, author: &str) -> Rgb {
      if self.authors.is_empty() { return self.chrome.dim; }
      let hash = /* byte fold */;
      self.authors[hash as usize % self.authors.len()]
  }
  ```

  `lane_overflow` has no accessor — consumers read the field:
  `shell/src/graph.rs:61` (`return rgb(theme.lane_overflow);`) and
  `tui/src/commits.rs:575`; tui tests assert against it at
  `tui/src/commits.rs:932` and `:954`.

- `core/src/theme.rs:707-724` — `readable(fg, bg, target) -> Rgb`: returns
  `fg` untouched if it already clears `target`, otherwise mixes toward
  white/black until it does. **A colour that already clears the floor is
  never touched** — that property is what makes this change invisible for the
  shipped `lanes` (all ≥ 4.3:1) and `authors` (≥ 4.36:1) and a real change
  only for `lane_overflow`.

- `readable` costs a handful of `powf` per call, which is why syntax resolves
  once in `rebuild` rather than per frame (the comment on `resolved` says
  exactly this). `Theme::author` is called **per visible row per frame** in
  the shell (`shell/src/views/commits.rs:207`, with a comment accepting that
  cost as a byte-fold + modulo) — so the resolution must happen in
  `rebuild()`, never inside `lane()`/`author()` per call.

- Rebuild is already called everywhere it needs to be:
  `app/src/config.rs:583` (`host.theme.rebuild()` after applying config
  colours), `core/src/theme.rs:528` (`set_syntax`), `:570` (the setter just
  above `lane()`), and each shipped theme is constructed through `rebuilt()`
  (`theme.rs:527-530` area). Verify with `grep -rn 'rebuild()' core app` that
  no path sets `lanes`/`authors`/`lane_overflow` without a following rebuild
  — as of this writing there is none.

- The test conventions to follow: `core/src/theme.rs:731+` has
  `every_token_is_legible_on_every_surface` and a furniture-floor test near
  `:896`; `raising_the_floor_is_one_field_and_a_rebuild` at `:833` shows the
  set-then-rebuild test shape. A helper `each_rgb` exists in
  `app/src/config.rs:89` but theme tests live in core and iterate themselves.

- The graph draws on the commit list's background, which is `chrome.bg` —
  resolve against that, not against diff surfaces.

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Core tests | `cargo test -q -p gitten-core` | exit 0 |
| Consumers | `cargo test -q -p gitten-shell -p gitten-tui` | exit 0 |
| App round-trip | `cargo test -q -p gitten-app` | exit 0 |
| Full gate | `./check.sh` | exit 0 |
| Lint / format | `cargo clippy -q --workspace --all-targets -- -D warnings && cargo fmt --check` | exit 0 |

## Scope

**In scope** (the only files you should modify):
- `core/src/theme.rs`
- `shell/src/graph.rs` (field read → accessor, one site)
- `tui/src/commits.rs` (field read → accessor, one prod site + two test sites)
- `plans/README.md` (status row)

**Out of scope** (do NOT touch, even though they look related):
- The shipped palette hex values — do not "fix" `lane_overflow` by picking a
  new constant per theme; the floor mechanism is the fix, so a user palette
  gets the same guarantee.
- `min_furniture` / `min_contrast` values.
- `app/src/config.rs` — the pub fields stay pub and the config keeps writing
  them; resolution happens after, in `rebuild()`.
- `web/` — check `grep -rn 'lane_overflow\|\.lane(\|\.author(' web/` and
  leave it alone unless it reads the raw field, in which case treat it like
  the tui site.

## Git workflow

- Branch: `advisor/017-overflow-lane-meets-the-furniture-floor`
- Commit style: imperative sentence, why-first — e.g.
  `Floor the graph colours like the gutter: the overflow mark measured 1.87:1`.
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: resolve the three graph colours in `rebuild()`

In `core/src/theme.rs`:

1. Add private resolved storage beside `resolved`/`gutter`, following their
   doc-comment style (say *why*: resolved once because `author` runs per
   visible row per frame; the raw fields stay as the user's input):

   ```rust
   lanes_on_bg: Vec<Rgb>,
   overflow_on_bg: Rgb,
   authors_on_bg: Vec<Rgb>,
   ```

2. At the end of `rebuild()`, fill them against `self.chrome.bg` at
   `self.min_furniture` (all three are glanced marks — strokes and initials —
   not body text; the shipped values already clear 3.0 except the overflow,
   so only it visibly changes):

   ```rust
   let bg = self.chrome.bg;
   self.lanes_on_bg = self.lanes.iter().map(|&c| readable(c, bg, self.min_furniture)).collect();
   self.overflow_on_bg = readable(self.lane_overflow, bg, self.min_furniture);
   self.authors_on_bg = self.authors.iter().map(|&c| readable(c, bg, self.min_furniture)).collect();
   ```

3. Switch `lane()` and `author()` to index the resolved vecs (keep the
   empty-fallbacks exactly as they are), and add:

   ```rust
   /// The colour of the 13th-and-beyond lane, resolved like every other piece
   /// of furniture — the overflow mark is the graph admitting it is not
   /// showing everything, which is worthless when it measures 1.87:1.
   pub fn overflow(&self) -> Rgb { self.overflow_on_bg }
   ```

4. Initialize the three new fields at every construction site the compiler
   points at (the three shipped themes go through `rebuilt()`, so empty/zero
   defaults are fine — `rebuild()` fills them).

**Verify**: `cargo build -q -p gitten-core` → exit 0.
`cargo test -q -p gitten-core` → exit 0.

### Step 2: switch the consumers

- `shell/src/graph.rs:61`: `rgb(theme.lane_overflow)` → `rgb(theme.overflow())`.
- `tui/src/commits.rs:575`: `theme.lane_overflow` → `theme.overflow()`.
- `tui/src/commits.rs:932` and `:954` (tests): compare against
  `host.theme.overflow()` instead of the raw field.
- Run the web grep from Scope; update the same way if it hits.

**Verify**: `cargo test -q -p gitten-shell -p gitten-tui -p gitten-web` → exit 0.

### Step 3: pin it with tests

In `core/src/theme.rs`'s test module, beside the existing furniture-floor
test (near `:896`), add:

1. `graph_colours_clear_the_furniture_floor`: for **each** shipped theme
   (iterate the same way the existing every-theme tests do), assert
   `contrast(t.overflow(), t.chrome.bg) >= t.min_furniture`, and the same for
   every entry of the resolved lanes and authors. This test FAILS before
   Step 1 (the overflow sits at 1.87) — run it against the unfixed accessor
   first if you want proof, but the committed order must be fix-then-test in
   the same change.
2. `a_hostile_palette_is_floored`: set `t.lanes = vec![t.chrome.bg]` (worst
   case: lane colour == background), `t.lane_overflow = t.chrome.bg`,
   `t.authors = vec![t.chrome.bg]`, call `t.rebuild()`, assert all resolved
   outputs clear the floor. Model the set-then-rebuild shape on
   `raising_the_floor_is_one_field_and_a_rebuild` (`theme.rs:833`).

**Verify**: `cargo test -q -p gitten-core graph_colours` and
`cargo test -q -p gitten-core a_hostile_palette_is_floored` → pass.

### Step 4: full gate

**Verify**: `./check.sh; echo $?` → 0;
`cargo fmt --check && cargo clippy -q --workspace --all-targets -- -D warnings` → exit 0.
Also `cargo test -q -p gitten-app` → exit 0 (the config round-trip test at
`app/src/config.rs:1424` reads/writes the raw fields, which this plan does
not change — confirm it still passes untouched).

## Test plan

Step 3's two tests, plus: existing tui tests updated to the accessor (they
now assert the resolved colour reaches the cell, which is strictly stronger),
and all existing theme tests unchanged and green. Optional sanity you can
eyeball without opening a window: `./dev dump commits . 40` renders a frame
to stdout (the repo rule allows dump — it interrupts nobody); not a gate.

## Done criteria

Machine-checkable. ALL must hold:

- [ ] `grep -n 'overflow_on_bg\|lanes_on_bg\|authors_on_bg' core/src/theme.rs` shows fields + rebuild fills
- [ ] `grep -rn 'lane_overflow' shell/src tui/src web/src` shows **no raw reads outside core** (config writes in `app/` are fine and expected)
- [ ] `cargo test -q -p gitten-core -p gitten-shell -p gitten-tui -p gitten-web -p gitten-app` exits 0, incl. the two new tests
- [ ] `./check.sh` exits 0; fmt and clippy clean
- [ ] No files outside the in-scope list modified (`git status`)
- [ ] `plans/README.md` status row updated

## STOP conditions

Stop and report back (do not improvise) if:

- Any existing test asserts the overflow's *raw* hex reaches the screen with a
  comment saying the sub-floor dimness is deliberate (the CLAUDE.md line "in a
  dim grey so the overflow is visible" suggests the intent was visible-but-
  quiet; `readable` preserves that by stopping exactly at 3.0 — but if a
  decision record says otherwise, stop).
- You find a mutation path that writes `lanes`/`authors`/`lane_overflow`
  without a following `rebuild()` — fixing that path is in scope only if it
  is a one-line `rebuild()` call; anything larger, report.
- `web/` turns out to serialize the raw field into its payload in a way that
  changes its API shape.

## Maintenance notes

- Anyone adding a new palette field that reaches the screen must decide:
  resolved through `rebuild()` (like syntax, gutter, and now the graph
  colours) or raw (only acceptable for backgrounds and hairlines, which have
  no floor by design — see the CLAUDE.md note on `chrome.border`/`diff.rule`).
  The reviewer question for every future theme PR is "which list is this
  field on?"
- If a selected-row background ever lands in the commit list (a known gap),
  the graph colours will draw on a second surface — at that point the
  resolution should become per-surface like the gutter's, and
  `lane()`/`author()`/`overflow()` grow a `Surface` parameter. Do not build
  that now.
- Reviewers: confirm no `readable` call moved onto a per-frame path (the
  resolution must be in `rebuild` only), and that the shipped lanes/authors
  hexes are byte-identical after resolve (they already clear the floor).
