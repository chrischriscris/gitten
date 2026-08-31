# Plan 031: The diff pane shows where the keyboard is and what is armed

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat 038d0ad..HEAD -- core/src/theme.rs shell/src/views/ shell/src/main.rs docs/extending.md`
> If any in-scope file changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition.

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

- **Priority**: P1
- **Effort**: L
- **Risk**: MED
- **Depends on**: none
- **Category**: bug (UX) — the flagship pane cannot answer "where is the keyboard and what will the next destructive key hit"
- **Planned at**: commit `038d0ad` (`origin/full/full`), 2026-08-31

## Why this matters

Three defects compound in the diff pane — the largest pane in the app and the
one every git verb acts through:

1. **The keyboard cursor is invisible on most rows.** The cursor is drawn by
   substituting `chrome.selection_bg` for the row's own background. In the
   shipped dark theme `selection_bg` is `0x241f1a` and `file_bg` is `0x231e1a`
   — a contrast of ~1.01:1. The theme's own documentation
   (`core/src/theme.rs`, the `border` field docs) states that surfaces within
   1.05:1 are "invisible as a boundary". The light and slate themes have the
   same problem. So on file headers, added lines and removed lines — most of a
   diff — the cursor cannot be seen. `diff.stage-hunk` / `diff.discard-hunk`
   act on *the hunk the cursor is in*, so an invisible cursor means staging
   the wrong hunk.
2. **Tokens and line numbers on the cursor row bypass both contrast floors.**
   The theme resolves syntax ink and gutter ink per `Surface`, but the cursor
   row's background (`selection_bg`) is not a `Surface`, so inks resolved for
   `Context` (etc.) are painted onto `selection_bg` unresolved. Computed with
   the repo's own `contrast()`: the resolved dark-theme context gutter lands
   at ~2.78:1 on `selection_bg`, below the 3.0 furniture floor; several syntax
   kinds land below the 3.5 text floor. This is the exact bug class
   `min_furniture` was introduced to kill (see
   `docs/decisions/0020-furniture-has-its-own-floor.md`).
3. **The armed destructive state is invisible, and the diff never learns
   focus.** `armed_hunk` is written and cleared but read by no render path,
   so after `D` arms a hunk discard, nothing in the pane marks the hunk that
   a second `D` will destroy. The four sidebar panes all tint the armed row
   toward `chrome.error` and their comments say why ("named by its own colour
   and not only by the band above it"). The diff also never receives
   `set_focused` (only Files/Branches/Stashes/Commits do, `shell/src/main.rs:1241-1244`),
   so its cursor looks identical whether or not the pane holds the keyboard.
   Finally, the "press again to confirm" question is rendered in `c.dim` —
   the same ink as "running push" — so the one sentence the user must read is
   the quietest text on screen.

After this plan: the cursor row carries a left accent bar (the same device
`chrome::list_row` uses in the sidebar, accent when focused / faint when not),
every ink on the cursor row is resolved against a new `Surface::Cursor`, the
armed hunk's rows are tinted toward `chrome.error`, and armed questions render
in error ink.

## Current state

Relevant files:

- `core/src/theme.rs` — the `Surface` enum (8 variants), `rebuild()` (resolves
  syntax × surface and gutter × surface), `background(surface)`.
- `shell/src/views/diff.rs` — the `Rows` trait (`render` at ~line 254),
  `TextRows` (the unified presentation), `row_background` (2838),
  `armed_hunk` on `Diff` (698), `diff.cycle-layout` handling (1055; its own disarm at 1071, the fall-through at 1078).
- `shell/src/views/split.rs` — `SplitRows`; calls `super::diff::row_background`
  at 567/576 but hand-rolls the cursor ternary for the row frame at 463.
- `shell/src/views/markdown.rs` — `MarkdownRows`; same trait.
- `shell/src/main.rs` — focus dispatch (~1241), the armed-question notices
  (e.g. ~1732 `"discard this hunk of {path}? press again to confirm"`), the
  message band render (~4469-4502), `set_notice` (~1310).
- `shell/src/chrome.rs` — `list_row` (~57), the sidebar's cursor-bar exemplar.
- `docs/extending.md` — documents the `Rows` trait as an extension seam
  (trait shown at ~line 249 of that doc). Must be updated with the signature change.

`Surface` today (`core/src/theme.rs:43-58`):

```rust
impl Surface {
    pub const ALL: [Surface; 8] = [
        Surface::Context,
        Surface::Added,
        Surface::Removed,
        Surface::AddedWord,
        Surface::RemovedWord,
        Surface::MovedRemoved,
        Surface::MovedAdded,
        Surface::Selected,
    ];
    pub const COUNT: usize = Self::ALL.len();
    ...
}
```

`background()` (`core/src/theme.rs:559-569`) maps each variant to a palette
field; `Selected` maps to `chrome.selected_bg`.

`rebuild()` (`core/src/theme.rs:536-552`) fills `resolved` (syntax × surface,
lifted to `min_contrast`) and `gutter` (gutter_fg × surface, lifted to
`min_furniture`).

`Rows::render` today (`shell/src/views/diff.rs:251-262`, and the same signature again on `TextRows` at 2371):

```rust
    fn render(
        &self,
        index: usize,
        seg: usize,
        host: &Host,
        sel: Option<Selected>,
        current: bool,
        shift: f32,
    ) -> AnyElement;
```

`row_background` (`shell/src/views/diff.rs:2838-2843`):

```rust
pub(crate) fn row_background(current: bool, base: Rgb, theme: &Theme) -> Rgb {
    match current {
        true => theme.chrome.selection_bg,
        false => base,
    }
}
```

The unified row resolves its gutter against the line kind's surface even when
the row is the cursor (`shell/src/views/diff.rs:2375-2405`):

```rust
                let (bg, fg, sign) = line_colors(*kind, *moved, p);
                let bg = row_background(current, bg, theme);
                let gutter = theme.gutter_on(surfaces(*kind, *moved).0);
```

The sidebar's exemplar for the cursor bar (`shell/src/chrome.rs:57-77`):

```rust
pub fn list_row(host: &Host, current: bool, focused: bool, h: f32) -> Div {
    let c = host.theme.chrome;
    let bg = match current { true => c.selection_bg, false => c.bg };
    let bar = match (current, focused) {
        (true, true) => c.accent,
        (true, false) => c.faint,
        (false, _) => bg,
    };
    div()...
        .bg(rgb(bg))
        .border_l(px(ROW_BAR))
        .border_color(rgb(bar))
        .pl(px(ROW_PAD - ROW_BAR))
}
```

Focus dispatch reaches four views and not the diff (`shell/src/main.rs:1241-1244`):

```rust
                Screen::Files { view, .. } => view.update(cx, |v, _| v.set_focused(focused)),
                Screen::Branches { view, .. } => view.update(cx, |v, _| v.set_focused(focused)),
                Screen::Stashes { view, .. } => view.update(cx, |v, _| v.set_focused(focused)),
                Screen::Commits { view, .. } => view.update(cx, |v, _| v.set_focused(focused)),
```

`diff.cycle-layout` returns before the fall-through disarm; only `cycle-wrap`
clears explicitly (`shell/src/views/diff.rs:1055-1078`). The trailing
`self.armed_hunk = None` after the match is skipped by the early
`return true;` in the cycle-layout arm.

The message band draws notices in `c.dim` (`shell/src/main.rs:4469-4472`):

```rust
                let message = error
                    .map(|e| (e, c.error))
                    .or_else(|| notice.clone().map(|n| (n.into(), c.dim)))
                    .or_else(|| running.map(|n| (n.into(), c.dim)));
```

Repo conventions that apply:

- `core/` has zero dependencies and never imports GPUI. `Surface` lives in
  core; the bar-drawing lives in shell. Keep that boundary.
- Doc comments argue for pixel/colour choices rather than asserting them —
  match that register (see the `list_row` doc comment as the exemplar).
- Test names are sentences: `the_cursor_bar_beats_every_row_background_in_every_presentation`
  (`shell/src/views/diff.rs:3176`). Follow the style.
- Commit messages: `crate: lowercase sentence` (see `git log --oneline`).

## Commands you will need

| Purpose | Command | Expected on success |
|---|---|---|
| Core tests | `cargo test -q -p gitten-core` | exit 0 |
| Shell tests | `cargo test -q -p gitten-shell` | exit 0 |
| Everything headless | `./dev check` | exit 0, no `✗` lines |
| Contrast report | `cargo run -q -p gitten-core --example contrast` | prints per-theme tables |
| One frame, no window | `./dev dump diff . 2>/dev/null \| head -50` | rows on stdout |

Do NOT launch `./dev desktop` — never open a window unasked (repo rule).

## Scope

**In scope** (the only files you should modify):
- `core/src/theme.rs`
- `core/examples/contrast.rs` (extend the gutter/syntax loops to the new surface — they iterate `Surface::ALL`, so this may be automatic)
- `shell/src/views/diff.rs`
- `shell/src/views/split.rs`
- `shell/src/views/markdown.rs`
- `shell/src/main.rs` (focus dispatch, notice kinds, message band)
- `docs/extending.md` (the `Rows` trait signature it documents)
- `docs/theming.md` (mention the new surface in the field table's vicinity)

**Out of scope** (do NOT touch, even though they look related):
- `tui/` — the terminal client has its own render path and already draws a
  cursor its own way; nothing here changes core row *flattening*.
- `core/src/rows.rs`, `core/src/select.rs` — the shared flattening/selection
  seams; this plan changes colour resolution and the shell trait only.
- The theme palette *values* — do not retune `selection_bg` hexes; the bar is
  the visibility mechanism, and value tuning is plan 034's territory.
- `shell/src/views/files.rs`, `branches.rs`, `stashes.rs`, `commits.rs` —
  the sidebar panes already do all of this correctly.

## Git workflow

- Branch: `advisor/ui-031-diff-cursor-focus-armed`
- Commit per step; message style matches the log, e.g.
  `core,shell: the cursor row is a surface like any other`
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: Add `Surface::Cursor` to core

In `core/src/theme.rs`:
- Add a `Cursor` variant to `Surface` with a doc comment in the file's
  register (the row the keyboard is on; its background is
  `chrome.selection_bg`; it exists because a token is resolved against
  whatever it actually lands on — the same sentence the enum already uses).
- Add it to `Surface::ALL` (making `COUNT` 9 — it derives from `ALL.len()`).
- Map it in `background()` to `self.chrome.selection_bg`.
- `rebuild()` iterates `Surface::ALL`, so the resolved and gutter tables grow
  automatically. Confirm no other code indexes surfaces with a hardcoded `8`:
  `grep -rn "COUNT\|; 8\]" core/src shell/src tui/src` and read each hit.

**Verify**: `cargo test -q -p gitten-core` → exit 0. Then
`cargo run -q -p gitten-core --example contrast | grep -i cursor` → gutter and
syntax rows for the new surface appear for every theme, none marked below
floor (the resolver lifts them by construction).

### Step 2: Resolve cursor-row inks in all three presentations

In each presentation, when `current` is true, resolve against
`Surface::Cursor` instead of the line kind's surface:

- `shell/src/views/diff.rs` (unified): where the row computes
  `theme.gutter_on(surfaces(*kind, *moved).0)` and where tokens are merged
  against a surface, pick `Surface::Cursor` when `current`. Find every
  `surfaces(` / `gutter_on(` / `syntax_on(` call in the render path and make
  the same substitution. The word-span surfaces (`AddedWord`/`RemovedWord`)
  keep their own backgrounds even on the cursor row — leave them alone.
- `shell/src/views/split.rs`: same at its `gutter_on`/token sites (~577 and
  the cell body).
- `shell/src/views/markdown.rs`: same at 477-478 (gutter) and 610/648 (marker).

**Verify**: `cargo test -q -p gitten-shell` → exit 0.
`./dev dump diff . 2>/dev/null | head -5` still renders.

### Step 3: Bundle row state and thread `focused` + `armed` through `Rows::render`

Replace the bare `current: bool` parameter with a small struct so the seam
changes once (this trait is a documented extension point —
`docs/extending.md` — and must not change signature twice):

```rust
/// Everything a presentation needs to know about one row's relationship to
/// the keyboard, beyond what the row itself holds.
#[derive(Clone, Copy, Default)]
pub struct RowState {
    /// The keyboard's row.
    pub current: bool,
    /// Whether this pane holds the keyboard at all.
    pub focused: bool,
    /// An armed destructive question stands over this row's hunk.
    pub armed: bool,
}
```

- Update the trait, `TextRows`, `SplitRows`, `MarkdownRows`, and every caller
  (the compiler is the worklist). Note there are also **three test-local
  implementors** already on this base that must be updated: `shell/src/views/diff.rs:4212`
  and `:4568`, and `shell/src/views/split.rs:1020` — they take `_current: bool`
  today. They are expected; they are not a scope violation.
- `Diff` gains a `focused: bool` field and a `set_focused(&mut self, bool)`
  like the sidebar views have; add the `Screen::Diff` arm to the dispatch at
  `shell/src/main.rs:1241` (and to the same dispatch for any other diff-holding
  screen variant — search for how the four existing arms are enumerated).
- `Diff::render` computes `armed` per row from `armed_hunk` and
  `Rows::hunk_at(index)` (both already exist; `armed_hunk` stores
  `(u16, u32)` — read how `confirm_or_arm_discard_hunk` keys it and match).
- Update the trait excerpt in `docs/extending.md` to the new signature.

**Verify**: `cargo test -q -p gitten-shell` → exit 0. `./dev check` → exit 0.

### Step 4: Draw the cursor bar and the armed tint

- In the unified row frame, split row frame, and markdown row frame: when
  `state.current`, draw a left bar `ROW_BAR` (2px) wide in `chrome.accent`
  when `state.focused`, `chrome.faint` when not — exactly `list_row`'s rule
  (`shell/src/chrome.rs:57-77`), and keep the `selection_bg` wash as the
  supporting tint. Like `list_row`, draw the bar on *every* row (in the row's
  own background when not current) so text never shifts when the cursor
  moves: `border_l(px(ROW_BAR))` + adjust the existing left padding by
  `ROW_BAR` so `PAD` stays the same visual distance. Note the unified `PAD`
  is also a hit-test offset (`header_hit` subtracts it) — if you change
  effective text origin, update the hit test the same way and run the
  selection tests.
- When `state.armed`, tint the row's sign column and gutter toward
  `chrome.error` (the sidebar panes' armed rule — see
  `shell/src/views/files.rs:789-810` for the exemplar and its comment).
- While here: replace the hand-rolled cursor ternary at
  `shell/src/views/split.rs:~462` with `row_background(...)` so the split
  frame and its cells cannot resolve the cursor through two code paths.

**Verify**: `cargo test -q -p gitten-shell` → exit 0. Then
`COLS=100 ./dev dump diff . 2>/dev/null | head -30` — output still shaped like
a diff. Add/extend a shell test asserting the bar colour logic (model it on
`the_cursor_bar_beats_every_row_background_in_every_presentation` at
`shell/src/views/diff.rs:3176`, which you must also update for `RowState`).

### Step 5: Disarm on layout cycle; armed questions in error ink

- In `shell/src/views/diff.rs`, the `"diff.cycle-layout"` arm returns before
  the fall-through disarm. Add `self.armed_hunk = None;` before its
  `return true;` with a one-line comment matching cycle-wrap's ("the rows are
  about to be re-arranged; whatever the question was armed against may land
  somewhere else").
- In `shell/src/main.rs`: give notices a kind. Change
  `notice: Option<String>` to carry it — e.g.
  `enum Notice { Info(String), Question(String) }` — keep `set_notice` for
  info and add `set_question` for the armed prompts. Route every
  "press again to confirm" caller through `set_question` (grep
  `press again to confirm` — there are several: discard hunk, discard file,
  squash/fixup/drop, rebase, delete branch, stash drop). In the message band
  (~4471), render `Question` in `c.error` instead of `c.dim`.

**Verify**: `cargo test -q -p gitten-shell && cargo test -q -p gitten-app` →
exit 0. `grep -n "press again to confirm" shell/src/main.rs` → every hit is a
`set_question` call.

### Step 6: Full gate

**Verify**: `./dev check` → exit 0, no `✗` lines. `cargo run -q -p gitten-core
--example contrast` → no `*` marks in the gutter/syntax cursor rows.

## Test plan

- Extend `every_token_is_legible_on_every_surface` (in `core/src/theme.rs`
  tests) — it iterates `Surface::ALL`, so confirm it now covers `Cursor` and
  still passes.
- Update `the_cursor_bar_beats_every_row_background_in_every_presentation`
  (`shell/src/views/diff.rs`) for `RowState`; add an assertion that the bar
  ink is `accent` when focused and `faint` when not.
- New shell test: an armed hunk's rows report the armed state (exercise
  `Diff`'s per-row armed computation directly — arm via
  `confirm_or_arm_discard_hunk`, then assert the row range that `hunk_at`
  maps to the armed key is flagged).
- New shell test: `cycle-layout` clears `armed_hunk` (arm, cycle, assert
  `None`) — model on whatever existing test arms a discard.

## Done criteria

Machine-checkable. ALL must hold:

- [ ] `./dev check` exits 0
- [ ] `cargo run -q -p gitten-core --example contrast` shows `gutter on Cursor`
      and syntax-on-Cursor columns for all three themes with no `*`
- [ ] `grep -n "set_focused" shell/src/main.rs` includes a `Screen::Diff` arm
- [ ] `grep -rn "current: bool" shell/src/views/diff.rs` no longer appears in
      the `Rows` trait definition (replaced by `RowState`)
- [ ] `grep -n "press again to confirm" shell/src/main.rs` — all hits routed
      through `set_question`
- [ ] `docs/extending.md` shows the new `Rows::render` signature
- [ ] No files outside the in-scope list are modified (`git status`)
- [ ] `plans/README.md` status row updated

## STOP conditions

Stop and report back (do not improvise) if:

- `Surface` is indexed anywhere by a hardcoded `8` or by serialized indices
  that persist across the change (search first; report what you find).
- `grep -rn "impl Rows for" --include=*.rs .` returns an implementor **outside**
  `shell/src/views/` (the three test-local ones inside it are known and listed
  in Step 3) — the seam is wider than planned.
- Adding the left bar shifts text or breaks the mouse-selection hit tests
  (`cargo test -q -p gitten-shell` failures in selection/hit tests after
  Step 4 that a padding adjustment does not fix on the first try).
- The armed-hunk key shape `(u16, u32)` does not line up with what
  `hunk_at` returns per row in a way a simple mapping resolves.

## Maintenance notes

- Any future presentation (`Rows` implementor) now receives `RowState` — the
  extension docs must keep the struct's field meanings current.
- Plan 034 (contrast matrix) will add report coverage for
  `selection_bg`-vs-diff-rows; if it lands first, expect its numbers to move
  when this plan adds the ninth surface.
- Deferred out of this plan: retuning `selection_bg` values (the bar makes
  the cursor visible regardless), and promoting `file_bg`/`hunk_bg` to
  surfaces (the hunk header's inline `readable` call at
  `shell/src/views/diff.rs:~2645` remains a known ad-hoc site — noted in
  plans/README.md as a follow-up).
