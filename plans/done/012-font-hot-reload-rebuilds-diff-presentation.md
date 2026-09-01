# Plan 012: A font edit in gitten.toml rebuilds the diff presentation

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**:
> `git diff --stat 2dfcb82..HEAD -- shell/src/views/diff.rs shell/src/views/markdown.rs`
> Both files churn often (`views/diff.rs` took 165 changed lines when caching
> landed). Compare every excerpt below against live code before proceeding;
> any mismatch beyond cosmetic line drift is a STOP condition.

## Status

- **Priority**: P2
- **Effort**: M
- **Risk**: MED — the early-exit key touched here also guards resize-drag cost,
  and a change must keep the "resize crossing no boundary costs nothing"
  invariant (there is a test).
- **Depends on**: none strictly; land after plans/010 and plans/011 (same view
  family, unrelated files)
- **Category**: bug
- **Planned at**: commit `2dfcb82`, 2026-08-26

## Why this matters

Saving `[font] size 14→18`, or swapping family/monospaced, reshapes every glyph
on the next frame (the window root applies `.text_size(px(f.size))` /
`.font_family(…)` from the per-frame host, `shell/src/main.rs:462-467`) — but
the diff view's row tables keep their old shape. Its `reflow` early-exits on a
key of `(width, wrap-name)` only, so neither value moves on a pure font edit
and the stale budgets/table layouts survive until the user happens to toggle
layout or drag across a width boundary. What stays stale includes heading pixel
sizes baked into the Markdown presentation at build time
(`shell/src/views/markdown.rs`, `Metrics::for_font`, :97–142: heading scale +
`layout.monospaced`, consumed at :953–955 for body text sizes) — markdown
headings draw at the old scale next to new-size body text, and a mono↔proportional
flip leaves tables laid out under the previous decision.

This is the documented hot-reload contract being violated one layer deeper than
the last time it was fixed: `shell/src/config.rs:73-80` describes exactly this
class of bug for the window chrome ("views call this on the render path rather
than holding a clone").

The fix: key the presentation off the whole `Font` (it is pure data and derives
`PartialEq`), and on change rebuild through the layout application path that
already exists — cheap, because it re-uses the prepared diff (`Rc` clone) and
re-runs no intraline/syntax pass.

## Current state

All in `shell/src/views/diff.rs` unless noted.

- The applied-state field on `Diff`, :581 and its initialiser/reset points:
    ```rust
    applied: (f32, &'static str),
    ```
    initialised `(0.0, "")` at :964, reset to the same by `apply_layout` at
    :1036.
- `reflow`, :697–708 — the early exit to extend:
    ```rust
    fn reflow(&mut self, width: f32, host: &Host) {
        let wrap = host.wrap.at(self.wrap);
        if (width, wrap.name()) == self.applied || width <= 0.0 {
            return;
        }
        self.applied = (width, wrap.name());
        let changed = { /* r.reflow(width, host, wrap) over renderers */ };
        ...
    ```
- `apply_layout(&mut self, index: usize, host: &Host)` at :1016–1036:
    ```rust
    self.current = index;   // (:1021)
    ...
    self.applied = (0.0, "");   // (:1036)
    ```
    It rebuilds via `arrange(...)` (free fn at :1076, which constructs fresh
    renderers — `MarkdownRows::new(Metrics::for_font(&host.font), …)` among
    them) from the cached prepared diff.
  - Precedent for an *internal* rebuild on config change already exists two
    lines of intent away: :876 calls `self.apply_layout(self.current, host)`
    from within the view (the settings-changed path), and the guard just above
    it (:852) shows the bounds/equal-index skip convention:
    `if index >= self.layouts.len() || index == self.current { return; }`.
    Your code should follow both conventions.
- Related state: `current: usize` field at :571; `renderers:
  Rc<RefCell<Vec<Box<dyn Rows>>>>` at :590.
- `core/src/font.rs`: `pub struct Font { family: String, size: f32,
  monospaced: bool, advance: f32 }` deriving `Debug, Clone, PartialEq`. A value
  comparison IS the fingerprint — no bit tricks needed.
- The perf invariant under test, `a_resize_that_crosses_no_character_boundary_costs_nothing`
  (:3115), exercises `reflow` headlessly; module-local test helpers construct a
  `Diff` without a window (see `with_renderers` constructor near :908 and test
  bodies around :2836/:3549 that call `diff.apply_layout(1, &host)`).
- Conventions: comments state costs and reasons in prose; state like this lives
  as plain fields next to `applied`; `Rc<Host>` flows in per frame.

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Crate tests | `cargo test -p gitten-shell` | all pass |
| The invariant test | `cargo test -p gitten-shell resize_that_crosses_no_character_boundary` | pass |
| Full gate | `./check.sh` | exit 0 |

## Scope

**In scope** (only these files):
- `shell/src/views/diff.rs`
- `shell/src/views/diff.rs`'s inline test module (new test inside it)

**Out of scope** (do NOT touch):
- `core/` — `Font` already carries everything needed.
- `markdown.rs` / `split.rs` internals — they rebuild correctly once `arrange`
  runs again; adding per-reflow metric rebuilding there would duplicate state.
- The blob-OID prepared-diff cache or `prepare()` itself — this fix must NOT
  trigger re-prepare (241 ms on the 714k fixture); it rides `arrange`.
- Roadmap A#2 (GPUI adopting `core::command` dispatch): tempting but separate.

## Git workflow

- Branch: `advisor/012-font-rebuild-diff-presentation`
- Commit style: sentence-case imperative like `Cache the prepared diff so a
  layout toggle stops re-preparing`. Do NOT push/open PR unles instructed.

## Steps

### Step 1: Give the view a font fingerprint

Add beside `applied` (:581):

```rust
/// The font the row tables were built against. `Font` is plain data deriving
/// PartialEq, so a value comparison is the fingerprint; a mismatch means the
/// metrics the renderers were built with no longer describe what will be drawn.
font_applied: Option<Font>,
```

Import `gitten_core::font::Font` if the file does not already name it. Initialise
to `None` wherever `applied` is initialised (:964 area).

**Verify**: `cargo build -p gitten-shell` → exit 0.

### Step 2: Rebuild on mismatch, before the width check

At the top of `reflow` (:697), before the existing early-exit:

```rust
if self.font_applied.as_ref() != Some(&host.font) {
    self.font_applied = Some(host.font.clone());
    // Reset first: arrange() has already been given today's host, and the
    // width half of `applied` must re-fire on the rebuilt renderers.
    self.applied = (0.0, "");
    self.apply_layout(self.current, host);
}
```

Notes for correctness:

- `apply_layout` resets `applied` itself (:1036) and rebuilds renderers via
  `arrange`; with the reset, execution falls straight into the normal
  single-pass flow below (wrap lookup, renderer reflow over the NEW renderers,
  order-table rebuild, anchor kept by logical row). No second frame is needed,
  which keeps a font save looking instant rather than flickering.
- The existing `reflow` docs above :697 explain why it must stay O(width-compare)
  on the common path; your comparison adds one `Option<Font>` equality — 2 float
  compares, 1 string eq, 1 bool — still O(1), and the resize test pins that.

Do NOT widen the `(f32, &'static str)` tuple itself; a dedicated field keeps
both independent concerns independent.

**Verify**: `cargo build -p gitten-shell` → exit 0. Then manual behavior probe
on release: `./dev --release desktop commits`, open a diff (`d`... whatever maps
in your run), edit `~/.config/gitten/gitten.toml` `[font] size = 18`, save —
expected: wrapped columns and markdown headings adopt the new size within one
frame, scroll position roughly preserved.

### Step 3: Pin both halves with tests

Inside the file's existing `#[cfg(test)] mod tests`, following the helpers used
by :2836/:3549 (construct via `Diff::with_renderers` where convenient):

1. Mirror of `a_resize_that_crosses_no_character_boundary_costs_nothing`
   (:3115): assert that two consecutive `reflow(width, &host_other_font)` calls
   with different `Font`s each do work while repeated same-font calls do not —
   whichever observable the existing resize test uses (e.g. renderers rebuilt /
   order table version). Model its setup lines exactly; stop reading at its
   body, do not invent a harness.
2. A pure unit test on the fingerprint contract:
   `Some(Font::jetbrains_mono()) == Some(Font::jetbrains_mono())`,
   and `Font { advance: 0.602, ..font_menlo() } != Font::menlo()` — guards the
   derive staying intact.

Name test(s) descriptively: `a_font_change_rebuilds_the_presentation`.

**Verify**: `cargo test -p gitten-shell font_change_rebuilds` → pass;
`cargo test -p gitten-shell resize_that_crosses_no_character_boundary` → still
passes (THE gate for the MED risk).

### Step 4: Full gate

**Verify**: `./check.sh` → exit 0.

## Test plan

- New test(s) named in Step 3, in `shell/src/views/diff.rs`'s test module,
  modeled structurally on the resize-cost test at :3115 and the internal
  `apply_layout` usages at :2836/:3549.
- Existing invariant test must remain green untouched (its file-level meaning:
  resizes stay cheap).
- No changes to `prepared.rs`/cache: `grep -n 'prepare(' shell/src/views/diff.rs`
  before/after shows identical occurrences.

## Done criteria

All must hold:

- [ ] `grep -n 'font_applied' shell/src/views/diff.rs` finds field + init + comparison site
- [ ] `cargo test -p gitten-shell` exits 0 including the new font-change test
- [ ] The resize-invariant test passes unmodified
- [ ] `./check.sh` exits 0
- [ ] No files outside the in-scope list modified (`git status --short`)
- [ ] `plans/README.md` status row updated

## STOP conditions

Stop and report back (do not improvise) if:

- Excerpts diverge materially — especially: `reflow` no longer owns the early
  exit; `apply_layout` gained a signature/return that makes the self-call path
  unsuitable; the :3115 test was refactored away (find where the invariant now
  lives before touching anything).
- Implementing reveals `arrange()` re-runs anything expensive (intraline,
  syntax, tokenisation) rather than riding the prepared cache — report measured
  timings instead of shipping a slowdown; the fallback design (invalidating only
  MarkdownRows/split budgets, leaving TextRows) exists but needs review first.
- The fix forces touching files outside the scope list.

## Maintenance notes

- When roadmap A#2 lands ([keys] → core::command), a settings panel will write
  `Host` wholesale; every such writer must mutate `host.font` (so this
  fingerprint catches panel-driven edits too) — do not add a side channel.
- Future knobs landing on `Font` come for free through `PartialEq`; future knobs
  landing ELSEWHERE on metrics (a theme-visible monospace hint?) must join this
  fingerprint deliberately.
- Reviewer focus: placement of the fingerprint check ABOVE the width early-exit
  (below it, font edits would never reach the compare), and that nothing in the
  patch touches `prepare`/acquisition.
