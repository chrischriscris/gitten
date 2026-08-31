# Plan 035: One chip language in the chrome — radii, raised surfaces, the prompt's exits, honest hints

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat 038d0ad..HEAD -- shell/src/chrome.rs shell/src/controls.rs shell/src/help.rs shell/src/input.rs shell/src/main.rs`
> On any in-scope drift, compare the "Current state" excerpts against the
> live code; on a mismatch, STOP.
>
> **Note**: the working tree at planning time carried the uncommitted
> raised/keycap design pass. This plan *completes* that pass; if it has been
> committed since, the excerpts still apply.

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
- **Effort**: S–M (five independent items, each S)
- **Risk**: LOW
- **Depends on**: none (033 touches `help.rs` — if both run, land 033 first
  and re-check item 2's line numbers)
- **Category**: tech-debt (visual consistency)
- **Planned at**: commit `038d0ad` (`origin/full/full`), 2026-08-31

## Why this matters

The chrome has no single chip language. On this base, one 32px title strip
contains **three different corner radii**: the status badge at `rounded(2px)`,
the keycap and the branch chip and the picker trigger at `rounded(3px)`, and
the floating panels (help, picker menu) at `rounded(4px)`. The picker triggers
— the only actually *clickable* furniture up there — hover to `status_bg`,
which is ~1.04:1 from the `title_bg` they sit on, i.e. an invisible hover; the
floating panels also sit on `title_bg`, which the palette's own docs call
within 1.05:1 of the content behind them, so a panel is held off a live diff by
a hairline alone. (An uncommitted design pass in the author's tree introduces
`chrome.raised`/`keycap` to fix the *fills*; those fields are absent here, so
Step 2 is gated and this plan's durable contribution is the single radius, the
prompt, and the budget.) Meanwhile the commit-message prompt —
the highest-stakes text entry in the app — shows no way to accept or cancel
(the status hints are deliberately blanked while it is open, on the comment
"its field owns the keyboard and speaks for itself"; the field says nothing),
uses lowercase labels where every other chrome label is uppercase, and sits
at a third left-inset. And the status bar's width arithmetic exists twice:
`chrome::hints_budget` is documented as the single home and is
`#[allow(dead_code)]` while `main.rs` recomputes it inline with a different
formula, and overflowing hints are dropped with no cue.

Five small fixes, one visual language.

## Current state

Radii and fills in the strip:

- `shell/src/main.rs:4410-4414` — branch chip: outlined, `.border_1()
  .border_color(rgb(c.border))`, `.rounded(px(3.0))`, height `CHIP_H`
  (22, `main.rs:101`). No fill on this base.
- `shell/src/chrome.rs:132-137` — keycap: outlined, `.border_1()
  .border_color(rgb(ink))`, `.rounded(px(3.0))`, size `ch * 1.6`.
- `shell/src/chrome.rs:~255-265` — status badge: `.bg(rgb(c.accent))`,
  `.rounded(px(2.0))` (line 261), height `ch * 1.7`.
- `shell/src/controls.rs:~120-155` — picker trigger:

```rust
        .h(px(H))          // H = 22 (controls.rs:52)
        .px_2()
        .rounded(px(3.))
        .border_1()
        .border_color(rgb(if open { c.faint } else { c.border }))
        .bg(rgb(if open { c.status_bg } else { c.title_bg }))
        ...
    if p.enabled {
        trigger = trigger
            .cursor_pointer()
            .hover(|s| s.bg(rgb(c.status_bg)))
```

- `shell/src/controls.rs:~188-192` — the open menu: `bg(title_bg)`, border
  `faint`, `rounded(px(4.))`. Menu item hover at ~213: `bg(status_bg)`.
- `shell/src/help.rs:~64-67` — the help panel: `bg(c.title_bg)`, border
  `faint`, `rounded(px(4.))`.

The palette's intent (`core/src/theme.rs`, `raised` docs, working tree): a
chip fill and the focused header band take "one visible step above the strip";
`keycap` is one step above `raised`.

The prompt (`shell/src/input.rs:~694-749`): a label + text field,
`h(px(34.0))`, `px_4` (16px inset); labels arrive lowercase ("commit",
"search" — set at the `open_input` call sites in `main.rs`, e.g. ~1429,
~2629). The status bar beneath uses `px_2` (8px) and uppercase (`PROMPT`
badge); list rows use `ROW_PAD` (12px). The hints blank
(`main.rs:~4477-4478`):

```rust
                let hints = match (&message, self.input.is_some()) {
                    (Some(_), _) | (None, true) => Vec::new(),
```

The duplicated budget: `shell/src/chrome.rs:372-376`:

```rust
#[allow(dead_code)]
pub fn hints_budget(host: &Host, bar_px: f32) -> f32 {
    let ch = host.font.char_width();
    (bar_px - ch * (8.0 + version().len() as f32 + 4.0)).max(0.0)
}
```

vs the inline copy (`main.rs:~4480-4486`), which uses the *actual* badge
length (`badge.chars().count() + 6`) — the inline one is more correct; the
helper hardcodes 8. `chrome::hints` (`chrome.rs:310-360`) silently stops
adding pairs at the budget with no overflow cue.

Input-mode keys: `core/src/command.rs` has an input mode (see
`shell/src/input.rs` — `input::MODE`) with accept/cancel commands; find their
names with `grep -n 'input\.' core/src/command.rs`. `Keymap::live_keys_for`
answers "which key runs this right now" (used by `help.rs:83`).

## Commands you will need

| Purpose | Command | Expected |
|---|---|---|
| Shell tests | `cargo test -q -p gitten-shell` | exit 0 |
| Core tests | `cargo test -q -p gitten-core` | exit 0 |
| Everything | `./dev check` | exit 0, no `✗` |

## Scope

**In scope**: `shell/src/chrome.rs`, `shell/src/controls.rs`,
`shell/src/help.rs` (background only), `shell/src/input.rs`,
`shell/src/main.rs` (chip radius, label case, hints budget call).

**Out of scope**:
- Palette values in `core/src/theme.rs` — use the fields that exist.
- Row heights / font-derived sizing (a separate, riskier change — recorded
  as deferred in plans/README.md).
- `views/*` (plan 036's territory).
- Mouse/click additions beyond what exists (a product decision — deferred).

## Git workflow

- Branch: `advisor/ui-035-chrome-polish`
- One commit per item is fine: `shell: one radius, one chip height`, etc.
- No push/PR unless instructed.

## Steps

### Step 1: One radius, one chip height

In `shell/src/chrome.rs`, add beside `ROW_BAR`/`HEADER_H`:

```rust
/// Corner radius for every chip, pill, keycap and floating panel. One value:
/// three radii in one 32px strip read as three design languages.
pub const RADIUS: f32 = 4.0;
```

Replace the literal radii: status badge (2.0, `chrome.rs:261`), keycap (3.0,
`chrome.rs:136`), branch chip (3.0, `main.rs:4414`), picker trigger (3.0,
`controls.rs:135`), picker menu (4.0, `controls.rs:192`), help panel (4.0,
`help.rs:67`) — all become `chrome::RADIUS`. Chip heights: the branch chip (`CHIP_H` 22)
and picker trigger (`H` 22) stay 22 (they share the strip); the badge keeps
its `ch * 1.7` (it lives in the status bar, not the strip) — do not unify
heights across different strips, only radii.

**Verify**: `cargo test -q -p gitten-shell` → exit 0.
`grep -rn "rounded(px(" shell/src | grep -v RADIUS` → no chip/panel hits
remain (row-internal rounding, if any exists elsewhere, is out of scope —
list what's left in your report).

### Step 2: Raised surfaces for the clickable and the floating — SKIP-AND-REPORT

**Gate**: run `grep -n "pub raised\|pub keycap" core/src/theme.rs` first.
On this base it returns **nothing**, because those palette fields live in an
uncommitted design pass in the author's tree. If it returns nothing: **skip
this entire step**, note it in your report as "Step 2 skipped: chrome.raised /
chrome.keycap absent on base", and continue with Step 3. Do **not** add the
fields yourself — that would duplicate the author's in-flight work. Only carry
out the step below if the grep finds both fields.


- Picker trigger (`controls.rs`): closed fill `c.raised` (was `title_bg`),
  hover and open fill `c.keycap` (one step further — the palette's own
  ladder), border unchanged. The disabled trigger keeps `title_bg` (flat =
  inert is honest).
- Picker menu (`controls.rs:~192`): `bg(c.raised)`.
- Menu item hover (`controls.rs:~213`): `bg(c.keycap)` (was `status_bg`,
  ~1.04:1 from the menu's own fill).
- Help panel (`help.rs:~64`): `bg(c.raised)`.

Update the nearby comments — several argue for the old colours (e.g. the
trigger's "the fill cannot do this job" border rationale predates `raised`;
keep the border, fix the argument).

**Verify**: `cargo test -q -p gitten-shell` → exit 0.
`grep -n "title_bg" shell/src/controls.rs shell/src/help.rs` → remaining
hits are the disabled trigger only.

### Step 3: The prompt says its exits and aligns with its neighbours

In `shell/src/input.rs` (~694-749):

- Uppercase the *rendered* label (`.to_uppercase()` at render, so call sites
  stay lowercase data) to match the badge below it.
- Left inset: `px_2` (the status strip's inset — the `PROMPT` badge sits
  directly below this field; today the two step by 8px).
- At the field's right edge, render live exit hints the way the help panel's
  close hint does (`help.rs:80-88` is the exemplar): resolve the accept and
  cancel commands' keys via `host.keys.live_keys_for(<name>, modes)` — find
  the command names with `grep -n 'input\.' core/src/command.rs` first —
  and draw `enter accept · esc cancel` (key in `c.fg`, label in `c.dim`,
  same pairing as `chrome::status_bar`'s hint style at `chrome.rs:274-287`).
  No live key → no hint (the panel-of-keys rule).
- The field needs `modes` (or the resolved key strings) at render — thread
  whatever is cheapest from `main.rs`'s render, where both are in hand;
  precomputing the two strings at `open_input` time is acceptable and
  allocation-free per frame (the repo's render-path rule).

**Verify**: `cargo test -q -p gitten-shell` → exit 0.

### Step 4: One budget, an honest ellipsis

- `chrome::hints_budget`: take the badge as a parameter —
  `pub fn hints_budget(host: &Host, bar_px: f32, badge: &str) -> f32` using
  the inline formula's badge arithmetic (`badge.chars().count() as f32 + 6.0
  + version().len() as f32 + 4.0`); drop `#[allow(dead_code)]`.
- `main.rs:~4480-4486`: replace the inline computation with the call.
- Overflow cue: make `chrome::hints` return whether it stopped early —
  change its return to `(Vec<(SharedString, SharedString)>, bool)` (or a
  small struct) — and when truncated, `status_bar` appends a single faint
  `"…"` after the last hint. Update `chrome.rs`'s existing hints tests
  (~380+) for the new return shape; add one asserting the truncated flag
  flips when `max_px` is small.

**Verify**: `cargo test -q -p gitten-shell` → exit 0.
`grep -n "allow(dead_code)" shell/src/chrome.rs` → gone.

### Step 5: Full gate

**Verify**: `./dev check` → exit 0, no `✗`.

## Test plan

- Step 4's updated/added hints tests (the only logic here; the rest is
  styling, which this repo pins by argument-in-comment rather than by test).
- All existing shell tests pass unmodified except the hints return shape.

## Done criteria

- [ ] `./dev check` exits 0
- [ ] One `RADIUS` const; no stray chip radii literals
- [ ] Picker trigger/menu/help panel on `raised`; hovers on `keycap` — **or**
      Step 2 recorded as skipped because those fields are absent on the base
- [ ] Prompt renders uppercase label, `px_2` inset, and live accept/cancel
      hints (or none when unbound)
- [ ] `hints_budget` parameterized, called from `main.rs`, dead-code allow
      removed; truncation shows `…`
- [ ] No files outside the in-scope list modified (`git status`)
- [ ] `plans/README.md` status row updated

## STOP conditions

- The input field cannot reach `modes`/keys without a structural change
  beyond passing two precomputed strings at `open_input` — report the
  shape.
- Plan 033 landed and moved the help panel's construction so Step 2's
  one-liner doesn't apply cleanly — reconcile by reading its diff; on
  conflict, report.
- Any hover/fill change makes a control *less* distinct than before on the
  light theme (eyeball the hexes: `raised` is darker than `title_bg` on
  light — that is intended elevation-as-shadow; but if keycap-on-raised is
  under ~1.1:1 in any theme, report instead of shipping an invisible hover).

## Maintenance notes

- Plan 034's matrix should cover `raised`/`keycap` as hover fills once both
  land — if 034 already landed, add the two hover rows to its example
  section in this plan's branch.
- Deferred: click-to-focus on the keycap/header (the keycap now looks even
  more pressable; the mouse story is a product decision recorded in
  plans/README.md), and deriving `CHIP_H`/`H`/heights from `font.size`.
