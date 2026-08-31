# Plan 033: The help overlay is modal, scrollable, and aligned

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat 038d0ad..HEAD -- shell/src/help.rs shell/src/main.rs core/src/command.rs`
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

- **Priority**: P1
- **Effort**: M
- **Risk**: LOW–MED
- **Depends on**: none
- **Category**: bug (UX) — the discoverability surface is unsafe and clips
- **Planned at**: commit `038d0ad` (`origin/full/full`), 2026-08-31

## Why this matters

The `?` overlay is the app's only in-window discoverability surface, and it
has three defects:

1. **It is not modal for the keyboard.** Opening help pushes a `"help"` mode
   (`shell/src/main.rs:1288-1290`) but nothing is bound in that mode, and
   `on_key` resolves against the *full* mode stack
   (`main.rs:3518-3521`), so every pane binding still fires underneath the
   panel: `D` over the Files pane arms a file discard behind the help screen.
   A panel of keys that executes repository verbs while you read it is a trap.
2. **It clips silently.** The panel is `.max_h_full().overflow_hidden()` with
   no scroll (`shell/src/help.rs:62-63`). Rows are 24px each plus mode
   headings and blanks; on a laptop window the tail bindings simply do not
   exist, with no cue that anything was cut.
3. **The key column is computed but never applied.** `panel_width`
   (`help.rs:126-137`) measures the widest key and reserves that width in the
   panel, but each row's key cell is a content-sized `flex_none` div with a
   no-op `.justify_end()` (`help.rs:104-110`), so descriptions start at a
   different x per row — a ragged list where two columns were designed — and
   the reserved width shows up as dead space at the right edge.

## Current state

`shell/src/help.rs` is 193 lines; read it whole before editing. The key
excerpts:

The panel (`help.rs:58-68`):

```rust
                div()
                    .occlude()
                    .v_flex()
                    .w(px(w))
                    .max_h_full()
                    .overflow_hidden()
                    .bg(rgb(c.title_bg))
                    ...
```

A command row (`help.rs:99-114`):

```rust
                            HelpRow::Command { keys, doc, .. } => div()
                                .h(px(ROW_H))
                                .flex()
                                .items_center()
                                .gap_3()
                                .child(
                                    div()
                                        .flex_none()
                                        .justify_end()
                                        .text_color(rgb(c.fg))
                                        .child(SharedString::from(keys)),
                                )
                                .child(
                                    div().min_w_0().truncate().text_color(rgb(c.dim)).child(doc),
                                ),
```

`panel_width` (`help.rs:126-137`) already computes `key_w` internally:

```rust
pub(crate) fn panel_width(rows: &[HelpRow], host: &Host) -> f32 {
    let mut key_w = 0.0_f32;
    let mut doc_w = 0.0_f32;
    for row in rows {
        if let HelpRow::Command { keys, doc, .. } = row {
            key_w = key_w.max(str_px(keys, host));
            doc_w = doc_w.max(str_px(doc, host));
        }
    }
    (key_w + str_px(" · ", host).max(12.) + doc_w + PAD).clamp(MIN_W - 2.0 * PAD, MAX_W - 2.0 * PAD)
        + 2.0 * PAD
}
```

Mode plumbing in `shell/src/main.rs`:

- `sync_modes` (~1285-1292): pushes `input::MODE` when a prompt is open,
  pushes `"help"` when `self.help`.
- `on_key` (~3518-3521): resolves via `resolve_mode_any(input::MODE, ...)`
  when an input is open, else `resolve_any(&self.modes, ...)` — the input
  case is the exemplar for "one mode owns the keyboard".
- `run_command` `"help"` arm (~2971): toggles `self.help`.
- `back()` (~3126): closes help.

`core/src/command.rs`:

- `Keymap::builtin()` — bindings by mode; `bind` errors on collision within
  a mode; rebinding a key that exists globally in a *different* mode is fine
  (there is a test: `k.bind("diff", "?", "help")` "rebinds the only `?`").
- `Keymap::help(&commands, &modes)` (~745) — projects only active modes.
- `live_keys_for(name, modes)` — which keys reach a command right now; the
  overlay uses it for its close hint and there are tests (~2110+) showing a
  mode with bindings shadows global keys.
- `Modes` — the stack; innermost wins.

There are three existing tests in `help.rs` (panel width) — keep them passing.

Repo conventions: keys are data in `core`; the shell never matches on
keypresses. Sentence-named tests. Commit style `crate: lowercase sentence`.

## Commands you will need

| Purpose | Command | Expected |
|---|---|---|
| Core tests | `cargo test -q -p gitten-core` | exit 0 |
| Shell tests | `cargo test -q -p gitten-shell` | exit 0 |
| Everything | `./dev check` | exit 0, no `✗` |

## Scope

**In scope**:
- `shell/src/help.rs`
- `shell/src/main.rs` (key resolution while help is up; scroll state)
- `core/src/command.rs` (bindings for the `help` mode)

**Out of scope**:
- A search/filter line inside the overlay (deferred; see Maintenance).
- An "all modes" projection (`Keymap::help_all`) — deferred.
- `tui/` — the terminal's `?` panel is its own drawing over the same
  projection; do not touch it.
- Plan 035 restyles overlay backgrounds (`raised`); don't restyle here
  beyond what the steps say.

## Git workflow

- Branch: `advisor/ui-033-help-overlay`
- Commits: `shell,core: the help panel owns the keyboard it explains`, etc.
- No push/PR unless instructed.

## Steps

### Step 1: Make help modal for the keyboard

- In `core/src/command.rs` `Keymap::builtin()`, bind in mode `"help"`:
  `"?"` → `"help"` (toggles closed; the test noted above proves rebinding
  `?` in a non-global mode is legal), `"escape"` → `"help"` if `escape` is
  how `back` is spelled elsewhere — first grep the builtin map for how
  escape/back are bound (`grep -n '"escape"\|"esc"' core/src/command.rs`)
  and mirror the existing spelling and command (`back` may already close
  help via `back()`; if so bind escape to that same command in help mode
  only if it is not already global). Add scroll bindings:
  `"j"`/`"down"` → `"view.scroll-down"`, `"k"`/`"up"` → `"view.scroll-up"`,
  `"g"`/`"home"` → `"view.top"`, `"G"`/`"end"` → `"view.bottom"` — these
  command names already exist globally; binding them in `"help"` changes
  nothing about what they mean, only guarantees they stay reachable.
- In `shell/src/main.rs` `on_key` (~3518): when `self.help` is true, resolve
  with `resolve_mode_any("help", &typed)` — exactly the input-prompt
  pattern on the adjacent line — so an unbound key while help is up runs
  nothing underneath. (The `Resolve::None` arm already reports
  `"{chord} is not bound"`, which is the honest answer here too.)
- In `run_command`, route the movement commands to the help panel's scroll
  when `self.help` (Step 2 adds the scroll state) instead of the focused
  pane.

**Verify**: `cargo test -q -p gitten-core` → exit 0 (no bind collisions).
New core test: with `help` pushed innermost over `files`, `keys("D")`
resolves to `Resolve::None` (model on the mode-shadowing tests ~2110+).

### Step 2: Make the panel scroll

- Give the row stack a scroll container: `.id("help-rows")` +
  `.overflow_y_scroll()` and track a `ScrollHandle` (GPUI's
  `StatefulInteractiveElement` scrolling — `.id()` first, that is the way in
  per the repo's GPUI notes; look at how `controls.rs`'s open picker list
  handles a long list for the nearest in-repo pattern, and at
  `views/mod.rs`'s `vertical_scrollbar` wrapper if a visible bar is cheap to
  add — a bar is nice-to-have, wheel + keys are the requirement).
- Keep the heading (`"keys · ? closes"` line) fixed above the scroll region.
- Route `view.scroll-down`/`-up`/`top`/`bottom` (Step 1) to the handle by
  `ROW_H` steps.
- When content overflows, append a fixed faint footer line inside the panel:
  `"…"` or `"{n} more below"` — content-cut honesty; skip it when everything
  fits.

**Verify**: `cargo test -q -p gitten-shell` → exit 0. The three `panel_width`
tests still pass.

### Step 3: Align the key column

- Change `panel_width` to also expose the measured key width — e.g. return a
  small struct `PanelMetrics { w: f32, key_w: f32 }` (update its three tests
  mechanically).
- In the row builder, set the key cell to
  `.w(px(key_w)).flex().justify_end()` — a real right-aligned fixed column
  (right-aligned so the key sits against its description, which is what the
  `" · "` gap in the width formula assumed). The doc cell keeps
  `.min_w_0().truncate()`.

**Verify**: `cargo test -q -p gitten-shell` → exit 0. Add one test: with two
rows whose keys are `"c"` and `"ctrl+shift+d"`, the metrics' `key_w` equals
the longer key's `str_px` (pin the alignment input; the draw itself is
untestable without a window, which is the repo's accepted line).

### Step 4: Full gate

**Verify**: `./dev check` → exit 0, no `✗`.

## Test plan

- Core: help-mode swallow test (Step 1) and a `live_keys_for("help", modes)`
  sanity check that the close hint still resolves with the new bindings.
- Shell: metrics test (Step 3); existing `panel_width` tests updated, not
  deleted.
- Manual-by-dump is not applicable (the overlay is desktop-only); rely on
  the unit layer, which is this repo's convention for chrome.

## Done criteria

- [ ] `./dev check` exits 0
- [ ] `grep -n '"help"' core/src/command.rs` shows help-mode bindings
- [ ] Core test proves an unbound pane verb does not resolve while help is up
- [ ] `grep -n "overflow_hidden" shell/src/help.rs` no longer governs the row
      stack (heading may keep it)
- [ ] Key cells have a fixed width from the measured metrics
- [ ] No files outside the in-scope list modified (`git status`)
- [ ] `plans/README.md` status row updated

## STOP conditions

- `resolve_mode_any("help", ...)` turns out not to fall back to the help
  mode's own bindings the way the input path does (read its implementation
  in `core/src/command.rs` first; if input relies on something help can't,
  report the difference rather than approximating).
- Binding `escape` in help mode collides or double-fires with the global
  back path — report with the resolve-order evidence.
- GPUI's `overflow_y_scroll` on the panel fights the `.occlude()` wheel
  ownership (symptom: the wheel scrolls the diff underneath) — report; the
  capture-phase note in the repo's GPUI docs is the relevant context.

## Maintenance notes

- Deferred: a live filter line over the rows (the rows are a projection of
  `Keymap::help`, so a filter is a `retain` — cheap once scrolling exists),
  and an all-modes toggle (`Keymap::help_all`) so other panes' verbs are
  visible from anywhere. Both are natural follow-ups on this structure.
- Plan 035 will move the panel's background to `chrome.raised` — a one-line
  change that should not conflict.
- Reviewers: check that the help mode's scroll bindings do not appear as
  duplicate rows in the panel itself (the projection walks active modes —
  the `"help"` mode's own rows will now appear; if that reads as noise,
  `HelpRow` filtering by mode name is the lever, but call it out rather than
  silently hiding rows).
