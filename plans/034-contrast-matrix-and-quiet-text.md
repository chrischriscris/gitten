# Plan 034: The contrast report measures everything drawn, and quiet text gets a floor

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat 038d0ad..HEAD -- core/src/theme.rs core/examples/contrast.rs shell/src/chrome.rs shell/src/views/markdown.rs shell/src/views/files.rs docs/theming.md docs/decisions/0020-furniture-has-its-own-floor.md`
> On any in-scope drift, compare the "Current state" excerpts against the
> live code; on a mismatch, STOP.

> **Base — read before your first command**: create your branch from
> `origin/full/full` (`038d0ad`), NOT from whatever HEAD your worktree starts
> on:
>
> ```sh
> git switch -c <the branch named under "Git workflow"> origin/full/full
> ```
>
> Line numbers in this plan were refreshed against `038d0ad`. Where one is off
> by a few lines, **match on the quoted content** — every excerpt is verbatim
> from that commit.
>
> **Build cost**: this workspace builds GPUI. Export a shared target dir first
> so you are not doing a cold build of the whole tree:
> `export CARGO_TARGET_DIR=/tmp/gitten-pass6-target`. Cargo locks it, so if
> another executor is mid-build your first command may wait — that is expected,
> not a hang.
>
> **Palette note**: `chrome.raised` and `chrome.keycap` do **not** exist on this
> base — they live in an uncommitted design pass in the author's working tree.
> Do not add them and do not reference them. Any step that would need them is
> marked SKIP-AND-REPORT.

## Status

- **Priority**: P2
- **Effort**: M
- **Risk**: LOW–MED
- **Depends on**: soft on plan 031 (adds `Surface::Cursor`; this plan's
  matrix covers whatever `Surface::ALL` contains, so land 031 first when
  possible)
- **Category**: tech-debt (design system) — the guard rail that would have
  caught most of this pass's colour findings
- **Planned at**: commit `038d0ad` (`origin/full/full`), 2026-08-31

## Why this matters

gitten's theming discipline is real — two documented contrast floors (3.5:1
text, 3.0:1 furniture), a `readable` resolver, a `Surface` enum — but the
enforcement covers exactly two ink groups: syntax × surface and gutter ×
surface (`Theme::rebuild`, `core/src/theme.rs:536-552`). Everything else is
drawn raw and measured by nothing:

- The contrast example's whole chrome section passes floor `0.0`
  (`core/examples/contrast.rs:122-141`), so its below-floor marker can never
  fire there; the markdown palette, the graph lanes and `diff.rule` are not
  printed at all; chrome inks are only ever compared against `chrome.bg`,
  never against `title_bg`/`status_bg`/`selection_bg`, which is where the
  shell actually draws them.
- `chrome.faint` is documented as a border colour with no legibility floor
  (`docs/decisions/0020-furniture-has-its-own-floor.md`, the "Why not
  resolve every chrome colour this way" section) — but it is drawn as *text*
  in at least four places: section labels (`shell/src/chrome.rs:86-97` (unchanged on this base)), the
  pane-header count (`chrome.rs:229`), the files
  pane's empty state (`shell/src/views/files.rs:720`), and a rename's old path
  (the rename origin row, `shell/src/views/files.rs` — find it via `renamed_from` at :60 and its draw site). Computed with the repo's own `contrast()`: ~2.05:1 on
  `chrome.bg` — roughly half the furniture floor. `controls.rs:110-115` already concedes this in a comment ("that
  measures 1.95:1 on the title bar, so 'dim and inert rather than removed'
  was in practice removed") and works around it locally.
- `markdown.marker` is drawn as glyphs (bullets at
  `shell/src/views/markdown.rs:610`, fence language labels at 648) on
  every diff surface but is resolved against none — on the dark theme's
  `added_word_bg` it computes to ~1.4:1, effectively invisible, and the
  in-flight word-bg strengthening makes that worse, not better. In all three
  shipped themes `marker` is byte-identical to `chrome.dim`.
- `chrome.dim` clears the 3.5 text floor by 0.02 in all three themes — a
  hand-tuned coincidence nothing recomputes or tests.

One piece of work fixes the class: resolve `marker` per surface like the
gutter, give quiet chrome text a resolved path, extend the example into a
real ink × surface matrix with honest floors, and promote the matrix into a
test so a fourth theme (or a newly added palette field) cannot ship past it.

## Current state

`Theme::rebuild` (`core/src/theme.rs:536-552`) — the pattern to extend:

```rust
    pub fn rebuild(&mut self) {
        self.resolved = vec![Style::default(); Kind::COUNT * Surface::COUNT];
        for kind in Kind::ALL {
            for surface in Surface::ALL {
                let base = self.syntax[kind.index()];
                let bg = self.background(surface);
                self.resolved[kind.index() * Surface::COUNT + surface.index()] = Style {
                    fg: readable(base.fg, bg, self.min_contrast),
                    ..base
                };
            }
        }
        for surface in Surface::ALL {
            let bg = self.background(surface);
            self.gutter[surface.index()] = readable(self.diff.gutter_fg, bg, self.min_furniture);
        }
    }
```

The example's zero-floor chrome section (`core/examples/contrast.rs:122-141`):

```rust
    println!("  -- chrome --");
    for (name, fg) in [
        ("fg", c.fg), ("dim", c.dim), ("faint", c.faint),
        ("accent", c.accent), ("error", c.error),
    ] {
        row(&format!("{name} on bg"), contrast(fg, c.bg), 0.0);
    }
    for (name, bg) in [
        ("title_bg", c.title_bg), ("status_bg", c.status_bg), ("border", c.border),
        ("selection_bg", c.selection_bg),
    ] {
        row(&format!("{name} vs bg"), contrast(bg, c.bg), 0.0);
    }
```

A `faint`-as-text site (`shell/src/chrome.rs:86-97` (unchanged on this base), `section_label`):

```rust
pub fn section_label(host: &Host, text: SharedString, count: Option<SharedString>, h: f32) -> Div {
    let c = host.theme.chrome;
    div()
        ...
        .text_color(rgb(c.faint))
```

A `marker` draw site (`shell/src/views/markdown.rs:610`):

```rust
            Block::Bullet(d) => row.child(
                div()
                    .flex_none()
                    .w(px(m.indent))
                    .text_color(rgb(md.marker))
                    .child(if blank { " " } else { m.bullet(d) }),
            ),
```

(and the fence label at `markdown.rs:648`: `body.text_color(rgb(md.marker))`.
The row already resolves a `surface` for its tokens — the gutter calls at
`markdown.rs:477-478` show it in scope as `surface`; reuse that value.)

Repo conventions: `core` has zero dependencies; the example runs read-only;
theme fields are documented with arguments; `docs/theming.md` has a field
table listing every `gitten.toml` colour key; decision records are amended,
not silently invalidated. `rgb_fields!` in `app/src/config.rs` maps toml keys
to palette fields — new *fields* need an entry there, but this plan adds
resolved *tables*, not user-facing fields, so config should not change.

## Commands you will need

| Purpose | Command | Expected |
|---|---|---|
| Core tests | `cargo test -q -p gitten-core` | exit 0 |
| Shell tests | `cargo test -q -p gitten-shell` | exit 0 |
| The report | `cargo run -q -p gitten-core --example contrast` | tables, `*` only where argued |
| Everything | `./dev check` | exit 0 |

## Scope

**In scope**:
- `core/src/theme.rs` (marker table, quiet-text helper, the new test)
- `core/examples/contrast.rs`
- `shell/src/chrome.rs` (the two faint-text sites)
- `shell/src/views/files.rs` (the two faint-text sites)
- `shell/src/views/markdown.rs` (the two marker sites)
- `docs/theming.md`, `docs/decisions/0020-furniture-has-its-own-floor.md`
  (amend the stale exemption paragraph)

**Out of scope**:
- Retuning any shipped hex value. If the new test fails on a shipped theme,
  that is a *finding* — STOP and report which pair fails by how much; the
  fix is a taste call.
- `chrome.error` semantics / a `warning` field (deferred — needs three
  themes' worth of taste; noted in plans/README.md).
- `tui/` — it reads the same resolved tables and benefits for free.

## Git workflow

- Branch: `advisor/ui-034-contrast-matrix`
- Commits: `core: the marker is furniture, so it resolves like furniture`, etc.
- No push/PR unless instructed.

## Steps

### Step 1: Resolve `markdown.marker` per surface

In `core/src/theme.rs`, mirror the gutter exactly:
- Add `marker: [Rgb; Surface::COUNT]` beside `gutter` (same non-serialized,
  rebuilt-not-chosen treatment — check how `gutter` is initialized in the
  theme constructors: `[0; Surface::COUNT]` then `rebuilt()`).
- Fill it in `rebuild()` with
  `readable(self.markdown.marker, bg, self.min_furniture)`.
- Add `pub fn marker_on(&self, surface: Surface) -> Rgb` beside `gutter_on`.
- In `shell/src/views/markdown.rs`, replace both `rgb(md.marker)` draw sites
  with `rgb(theme.marker_on(<the row's surface>))` — the row already knows
  its surface for tokens; reuse the same value the token merge uses (find
  the `kind`/`moved` → surface mapping already in that render arm).

**Verify**: `cargo test -q -p gitten-core && cargo test -q -p gitten-shell`
→ exit 0.

### Step 2: Give quiet chrome text a resolved path

- In `core/src/theme.rs`, add
  `pub fn quiet_on(&self, bg: Rgb) -> Rgb { readable(self.chrome.faint, bg, self.min_furniture) }`
  with a doc comment stating the split this creates: `faint` *as a border*
  has no floor (0020's rule stands); `faint` *as text* goes through this and
  gets the furniture floor.
- Swap the four faint-text sites to it:
  - `shell/src/chrome.rs` `section_label` → `quiet_on(c.bg)` (labels sit on
    the pane background).
  - `shell/src/chrome.rs` pane-header count (~229) → `quiet_on(c.title_bg)`
    (the header strip's background on this base).
  - `shell/src/views/files.rs` empty state (720, inside the `is_clean` block at
    712-724) and the rename origin row → `quiet_on(c.bg)`.
  Grep for other `c.faint`/`chrome.faint` used inside a `.text_color(` and
  apply the same judgment: text → `quiet_on`, borders → unchanged. List the
  sites you changed in the commit message.
- Amend `docs/decisions/0020-furniture-has-its-own-floor.md`: the "faint is
  a border" exemption now reads "faint as a border has no floor; faint as
  text resolves through `quiet_on`" — append to the decision, don't rewrite
  its history.

**Verify**: `cargo test -q -p gitten-shell` → exit 0. Visually:
`THEME=dark ./dev dump files . 2>/dev/null | head -20` still renders (the
dump path exercises the tui, which shares the theme — a smoke check only).

### Step 3: The matrix in the example

Rewrite the chrome section of `core/examples/contrast.rs` (keep the diff and
syntax sections as they are):

- Text inks × the backgrounds they are drawn on, at `t.min_contrast`:
  `fg`, `dim`, `accent`, `error` each against `bg`, `title_bg`, `status_bg`,
  `selection_bg` — these are where `chrome.rs`, `controls.rs`, `main.rs` and
  the views draw them.
- Quiet text: `quiet_on(bg)` / `quiet_on(title_bg)` / `quiet_on(status_bg)` at
  `t.min_furniture` (these are resolved, so they document rather than fail —
  the `*` will show when the raw value was lifted, same convention as the
  gutter section).
- Furniture with no floor, printed for the hierarchy (floor 0.0 is correct
  here, keep it): `border`, `selection_bg` vs `bg` —
  but add `selection_bg` worst-vs-diff-rows at floor 1.05, exactly the
  computation `selected_bg` already gets (`contrast.rs:142-155`).
- New markdown section: `marker` unlifted vs context (floor
  `min_furniture`, will mark `*` — that is `readable` doing its job, same as
  the gutter's unlifted row), then `marker_on` per `Surface::ALL`; `code_bar`,
  `quote_bar`, `rule` vs `context_bg` at floor 0.0 (hierarchy rows).
- New graph section: each `lanes[i]` and `lane_overflow` vs `chrome.bg` at
  `t.min_furniture` for the lanes (they are 2px strokes carrying meaning) —
  if a shipped lane fails, print it; do not lift lanes in code (out of
  scope), just measure.

**Verify**: `cargo run -q -p gitten-core --example contrast` → the new
sections print for all three themes. Record in your report which rows carry
`*` — expected: none among the text inks; if a text ink fails, STOP (see
STOP conditions).

### Step 4: Promote the text-ink matrix to a test

In `core/src/theme.rs` tests, beside `every_token_is_legible_on_every_surface`
(find it by name), add:

```text
chrome_text_inks_clear_the_text_floor_where_they_are_drawn
```

For every registered theme (iterate the registry the way the existing
all-themes tests do): assert `contrast(ink, bg) >= min_contrast` for the
text-ink × background pairs from Step 3, and
`contrast(quiet_on(bg), bg) >= min_furniture` for the quiet set, and
`contrast(marker_on(s), background(s)) >= min_furniture` for all surfaces.
Use a small table of (name, ink, bg, floor) so the failure message names the
pair.

**Verify**: `cargo test -q -p gitten-core` → exit 0. If a shipped theme
fails a raw-ink assertion by a hair (e.g. `dim` at 3.49), STOP and report
the exact pairs and deltas — retuning hexes is out of scope.

### Step 5: Docs and full gate

- `docs/theming.md`: note in the floors section that chrome text inks are
  now *asserted* (test-pinned) while syntax/gutter/marker/quiet are
  *resolved* (lifted at rebuild) — one sentence each, matching the file's
  register; update the field table's `markdown` row to mention the marker is
  resolved per surface.

**Verify**: `./dev check` → exit 0, no `✗`.

## Test plan

Covered by Steps 4 (the new all-themes assertion) and the existing
`every_token_is_legible_on_every_surface`. No shell-side tests needed beyond
compilation — the shell changes swap colour lookups only.

## Done criteria

- [ ] `./dev check` exits 0
- [ ] `grep -n "0.0" core/examples/contrast.rs` — remaining zero floors are
      only the documented hierarchy rows (diff row backgrounds, chrome
      surface-vs-bg rows)
- [ ] `grep -n "marker_on\|quiet_on" core/src/theme.rs shell/src` shows the
      table, the helper, and their call sites
- [ ] `grep -rn "text_color(rgb(c.faint))\|text_color(rgb(md.marker))" shell/src`
      returns no hits
- [ ] The new core test exists and passes for all registered themes
- [ ] 0020 amended; `docs/theming.md` updated
- [ ] No files outside the in-scope list modified (`git status`)
- [ ] `plans/README.md` status row updated

## STOP conditions

- Any shipped theme fails a Step-4 assertion (report the pairs and deltas —
  the fix is palette taste, not code).
- `Metrics`/`markdown.rs` turns out to not have the surface in scope at the
  marker draw sites (report what the render arm actually has).
- The `faint` sweep finds a text site whose background is not statically
  known (e.g. drawn over variable content) — report it rather than guessing
  a background.

## Maintenance notes

- Every new palette field from now on should gain a matrix row in the same
  commit — an earlier palette pass landing without measurement is what
  motivated this plan. When `chrome.raised`/`keycap` land from the author's
  design pass, add their rows here in that same commit.
- Deferred: a `warning` chrome field (armed/conflict/failure currently share
  `error` — three meanings, one colour, and its own doc comment says a
  palette where one colour means two things is a palette a theme cannot
  retune); lifting graph lanes if Step 3's measurement shows shipped
  failures.
- Reviewers: check the quiet_on swap did not brighten section labels enough
  to compete with row text — the intended outcome is "reads at a glance",
  not "louder than the rows it labels" (the hierarchy paragraph in the
  example's doc comment is the standard).
