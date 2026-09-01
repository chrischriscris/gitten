# Plan 036: The views keep their own rules — truncation, empty states, the split's edges

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat 038d0ad..HEAD -- shell/src/views/ shell/src/chrome.rs`
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
- **Effort**: M (six independent items, each S)
- **Risk**: LOW (one MED item, called out)
- **Depends on**: none. Plan 031 rewrites the same render arms in
  `diff.rs`/`split.rs` — if both are queued, land 031 first and re-locate
  this plan's excerpts in its wake.
- **Category**: tech-debt (consistency) / bug (two docstrings are false)
- **Planned at**: commit `038d0ad` (`origin/full/full`), 2026-08-31

## Why this matters

Six places where the views break rules the codebase itself states:

1. **Four truncation behaviours across four sidebar panes, and two
   docstrings are false.** Branches and commits ellipsize
   (`.min_w_0().truncate()`); stashes hard-clips (`whitespace_nowrap`, no
   `truncate` — while its own doc at `stashes.rs:207` says "messages
   truncate rather than pan"); files hard-clips paths at the *right* edge —
   throwing away the filename, the only part being scanned — while
   `files.rs:488` says "paths truncate rather than pan". No ellipsis means
   `src/views/diff` and `src/views/diffs.rs` render identically.
2. **A conflicted file renders as `UUsrc/main.rs`.** The files status column
   is exactly `STATUS_CHARS = 2.0` characters wide (:702, draw site :797) with no trailing air
   (`files.rs:795-816`); all seven conflict kinds emit two letters, filling
   the box. Single letters get air by accident, so the column looks fine
   until the one state that "ends work" (the file's own words) — now drawn
   in error ink by the design pass — welds itself to its path. The branches
   pane states the intended pattern: "One character wide plus one of air, so
   every name aligns" (`branches.rs:~821`).
3. **The commits pane and the main region have no empty state, and the block
   is now triplicated.** Files ("working tree clean"), stashes ("nothing
   stashed") and branches ("no branches yet") each draw a quiet faint line —
   three near-identical twelve-line copies. Commits (a `/` filter matching
   nothing → blank list) and the diff (`order.is_empty()` → bare rectangle)
   draw nothing. One blank diff pane currently means any of: nothing selected, a
   clean tree, or an empty projection — and three copies of one twelve-line
   block is the drift vector `chrome.rs` was written to prevent.
4. **The split layout breaks the edge rule the unified layout documents.**
   The unified row runs the line's kind background through the page padding
   to both edges; `row_frame`'s doc calls this "what every diff viewer worth
   reading does". The split row paints its pads in neutral chrome
   (`split.rs:458-465`), so toggling `s` changes whether additions read as
   a block or a striped column. The same lines hand-roll the cursor ternary
   instead of calling `row_background`, so the split frame and its cells
   resolve the cursor through two code paths.
5. **The page pad has two spellings that agree by coincidence.**
   `PAD = 16.0` is a hit-test offset (`header_hit` subtracts it), but the
   file header (`diff.rs:2643`) and the markdown row (`markdown.rs:475`)
   draw with `.px_4()` — GPUI rems that resolve to 16 only via the library's
   default rem size. A rem-size change desynchronises clicks from glyphs.
6. **The pane header never clips.** `pane_header_with` has no
   `overflow_hidden` and the diff header's right-edge items (`+adds`,
   `-dels`, `hunk n/m`, `loading`) are `flex_none` after an unshrinkable
   path — a deep path pushes `hunk 2/7`, the answer to "which hunk will
   space stage", silently out of the window.

## Current state

Truncation, the four spellings:

- `shell/src/views/branches.rs:833` — `.min_w_0().truncate()` ✓
- `shell/src/views/commits.rs:799` — `.min_w_0().flex_shrink(1.0).truncate()` ✓
- `shell/src/views/stashes.rs:452-460` — `.flex_none().min_w_0().whitespace_nowrap()` ✗
- `shell/src/views/files.rs:815` — `.flex_none().min_w_0()` wrapping
  `chrome::path_spans` (`chrome.rs:101-113`: `whitespace_nowrap`, two
  `flex_none` children — dir dim, filename bright) ✗
- The repo already uses the right tool once: `main.rs:~4301` (unchanged on this base)
  `.text_ellipsis_start()`.

The files status column (`shell/src/views/files.rs:795-816`):

```rust
        Entry::File(f) => list_row(host, current, focused, ROW_H)
            .child(
                div()
                    .flex_none()
                    .w(px(STATUS_CHARS * ch))
                    ...
                    .child(SharedString::from(f.letters)),
            )
            .child(
                div().flex_none().min_w_0().child(...path...)
```

The files empty state to extract (`shell/src/views/files.rs:712-724`):

```rust
        if let Some(empty) = self.is_clean().then(|| {
            div()
                .size_full()
                .pl(px(ROW_PAD))
                .pt_2()
                .flex()
                .items_start()
                .text_color(rgb(c.faint))
                .child("working tree clean")
                .into_any_element()
        }) {
            return empty;
        }
```

(`stashes.rs:376-388` is the near-identical twin, and `branches.rs:742-752` is a third. **Note**: if plan 034
landed, the ink is `quiet_on(c.bg)` rather than raw `faint` — extract
whatever is live.)

The split row frame (`shell/src/views/split.rs:458-465`):

```rust
                let cell = px(self.cell_px(self.width));
                row_frame()
                    .items_center()
                    .px(px(PAD))
                    .bg(rgb(match current {
                        true => theme.chrome.selection_bg,
                        false => theme.chrome.bg,
                    }))
```

…while two nearby cell sites already call
`super::diff::row_background(current, ..., theme)` (`split.rs:567,576`).
`cell_px` and the hit test divide clicks using `PAD` as the left offset —
the padding's *width* must not change.

Pane header (`shell/src/chrome.rs:196-231` (unchanged on this base)): `.w_full()`, no
`overflow_hidden`; name child is `flex_none`; right-edge furniture follows a
`flex_grow` spacer. The diff header's `right` block is built at
`main.rs:~4198-4229` (main.rs is unchanged on this base): the HEAD subject is `flex_shrink(1.0)` +
`text_ellipsis_start`, the counters are `flex_none`.

Commits' no-match path: `commits.rs:404-443` (`apply_query` swaps `visible`
and re-anchors); `Commits::render` (617-618) has no empty branch; the header
shows a `filter_note` like `0/4173` — the only signal today.

Diff render: `diff.rs:2013` `uniform_list("diff", order.len(), ...)`; no
zero-row branch in `Diff::render` (1980-2100 or so). A one-file no-hunk diff keeps
its header row (pinned by test
`a_one_file_diff_with_no_hunks_keeps_its_only_row`, `diff.rs`, search the name) — that
case stays as is; only *zero rows total* gets the sentence.

Conventions: shared furniture lives in `chrome.rs` precisely so panes cannot
drift; docstrings must describe the code; sentence-named tests; commit style
`crate: lowercase sentence`.

## Commands you will need

| Purpose | Command | Expected |
|---|---|---|
| Shell tests | `cargo test -q -p gitten-shell` | exit 0 |
| Everything | `./dev check` | exit 0, no `✗` |
| A frame | `COLS=80 ./dev dump files . 2>/dev/null \| head` | rows print |

## Scope

**In scope**: `shell/src/views/files.rs`, `stashes.rs`, `commits.rs`,
`branches.rs`, `diff.rs`, `split.rs`, `markdown.rs` (one pad line),
`shell/src/chrome.rs` (the `empty_line` helper, `path_spans` shrink
behaviour, header `overflow_hidden`), `shell/src/main.rs` (only the diff
header right-block flex properties, ~4198-4229).

**Out of scope**:
- Modelling binary/rename/mode-change rows in the diff parser (an M–L
  change to `core` — deferred; the empty sentence is the honest interim).
- `Rows::render` signature, cursor, armed state — plan 031.
- Colour values, hover, mouse — plans 034/035 and the deferred mouse
  decision.
- `tui/` — its drawing is its own and tested.

## Git workflow

- Branch: `advisor/ui-036-view-polish`
- Commit per item.
- No push/PR unless instructed.

## Steps

### Step 1: One truncation rule

- `stashes.rs` message: add `.truncate()` (drop `whitespace_nowrap` if the
  two conflict — `truncate` implies nowrap in GPUI's styled ext; check how
  `branches.rs:~831` spells it and match).
- Files paths: in `chrome::path_spans`, make the *directory* child
  shrinkable — `.min_w_0().flex_shrink(1.0).text_ellipsis_start()` — and
  keep the filename `flex_none`. The filename is what must never be lost;
  a squeezed row gives up the directory's head (`…views/diff.rs`).
  `path_spans` is shared by the files rows, the diff pane header and the
  title strip — this change is *wanted* in all three (the header's "path
  must not truncate" comment at `main.rs:~4195` protects the filename, which
  this preserves; update that comment to say so). Then remove the dead
  outer `flex_none` on the files row's path wrapper so the shrink can act.
- Fix the two false docstrings (`files.rs:488`, `stashes.rs:~206`) to
  describe the new true behaviour.

**Verify**: `cargo test -q -p gitten-shell` → exit 0.

### Step 2: Air after the status letters

`files.rs`: widen the column — `.w(px((STATUS_CHARS + 1.0) * ch))` — so the
gap is part of the column (the convention `commits.rs`'s `WHO_CHARS` uses).
Letters stay left-aligned in the box; `UU` gains its missing character of
air; single letters keep theirs.

**Verify**: `cargo test -q -p gitten-shell` → exit 0.
`COLS=80 ./dev dump files . 2>/dev/null | head` → letters and paths separate.

### Step 3: `chrome::empty_line`, four callers

- Extract the files/stashes/branches empty block into
  `pub fn empty_line(host: &Host, text: SharedString) -> AnyElement` in
  `chrome.rs` (a literal extraction — same inks, insets; keep the "quiet
  line, not an empty box; top-left where a reader scans" comment with it).
- Call it from files, stashes and branches (behaviour unchanged), and add:
  - `branches.rs`: it **already has** this empty state on this base
    (`branches.rs:742-752`, "no branches yet") — convert it to the helper,
    do not add a second one.
  - `commits.rs`: zero visible rows → with a standing query,
    `no commits match "<query>"` (echoing the query is what makes the state
    legible — the view holds it; see `apply_query`); without one,
    `"no commits"`.
  - `diff.rs` `Diff::render`: `order.is_empty()` →
    `empty_line(host, "no changes to show".into())` (the pane header above
    it already names what was selected).

**Verify**: `cargo test -q -p gitten-shell` → exit 0. Add one test per new
empty state where the view's test module already constructs the view headless
(commits and branches have such modules — model on their existing
construction; assert the render branch is taken via the view's state, e.g.
`visible.is_empty()`, not by rendering).

### Step 4: The split row resolves the cursor once and fills its edges

(The MED-risk item.) In `split.rs:~458-465`:

- Replace the hand-rolled ternary with
  `super::diff::row_background(current, theme.chrome.bg, theme)` — one code
  path for the cursor everywhere.
- Fill the pads with the halves' own backgrounds so the colour reaches both
  edges like the unified layout: drop `.px(px(PAD))` from the frame and
  instead flank the two cells with two `flex_none` divs of width `PAD`
  whose backgrounds are the old cell's and the new cell's respective row
  backgrounds (the cells compute these already — lift the two values to
  where the frame is built). The geometry must not move: the hit test
  (`cell_px`, and the click math near it) assumes text starts at `PAD` —
  it still does; only the fill under the pads changes.

**Verify**: `cargo test -q -p gitten-shell` → exit 0 — specifically the
split divider/hit tests (`split.rs`, the divider tests and neighbours) must pass
untouched. If any hit test fails, STOP (the geometry moved).

### Step 5: One spelling for the pad; the header clips

- `diff.rs:2643` and `markdown.rs:475`: `.px_4()` → `.px(px(PAD))`
  (imported from the diff module where needed). Grep
  `px_4` across `shell/src/views/` — any other hit inside a row that
  `header_hit`/selection math measures gets the same swap; hits in
  non-measured chrome stay.
- `chrome.rs` `pane_header_with`: add `.overflow_hidden()` to the strip so
  nothing ever paints outside it. Confirm the absolute `ROW_BAR` child and
  still draws (it is inside the strip's bounds; it should).
- `main.rs:~4198-4229` (main.rs is unchanged on this base) (diff header right block): the counters and
  `hunk n/m` stay `flex_none`; the *path* is now shrinkable from Step 1's
  `path_spans` change, which is what gives way instead. Nothing else needed
  — verify by reading the flex chain, and note in the commit message which
  element yields.

**Verify**: `cargo test -q -p gitten-shell` → exit 0, and the diff-view
selection/`header_hit` tests specifically (grep `header_hit` in
`diff.rs`'s test module) still pass.

### Step 6: Full gate

**Verify**: `./dev check` → exit 0, no `✗`.

## Test plan

- Step 3's empty-state assertions (state-level, headless).
- Step 4 leans on the existing split divider/hit tests as its regression
  net — they must pass unmodified.
- Step 5 leans on the existing `header_hit`/selection tests — unmodified.
- Everything else is styling pinned by docstring; fix the docstrings
  (Step 1) so they are true again.

## Done criteria

- [ ] `./dev check` exits 0
- [ ] `grep -n "whitespace_nowrap" shell/src/views/stashes.rs` → no
      untruncated message row
- [ ] `grep -n "text_ellipsis_start" shell/src/chrome.rs` → the directory
      span
- [ ] `grep -n "empty_line" shell/src` → one definition, five call sites
      (files, stashes, branches, commits, diff)
- [ ] `grep -n "px_4" shell/src/views/diff.rs shell/src/views/markdown.rs`
      → no hits in measured rows
- [ ] `grep -cn "selection_bg" shell/src/views/split.rs` → the frame no
      longer hand-rolls the ternary
- [ ] The two docstrings describe the code
- [ ] No files outside the in-scope list modified (`git status`)
- [ ] `plans/README.md` status row updated

## STOP conditions

- Plan 031 landed and rewrote the split/unified row frames — re-locate
  before editing; if `row_background` was replaced by a different mechanism,
  reconcile with its diff or report.
- `text_ellipsis_start` on the directory span misbehaves inside
  `whitespace_nowrap` `path_spans` (symptom: no ellipsis, or the filename
  clips) — report with what GPUI actually did; do not stack workarounds.
- Step 4's pad-fill restructure fails a hit test twice — report; the
  fallback design (keep neutral pads in *both* layouts instead) is a
  maintainer taste call.
- Any `.px_4()` hit is load-bearing for a rem-relative design elsewhere.

## Maintenance notes

- The diff's honest interim empty state should be replaced when
  binary/rename/mode rows are modelled in `core` (deferred; a `+0 -0` PNG
  header still actively misleads — that parser work is the real fix).
- Future presentations get `empty_line` for free — mention it in
  `docs/extending.md` if that file gains an empty-state note.
- Reviewers: eyeball a wide and a narrow `./dev dump files` frame — Step 1
  and 2 both move the files row's x positions by design.
