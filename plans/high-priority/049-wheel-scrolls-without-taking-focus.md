# Plan 049: The wheel scrolls without taking the keyboard

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update this plan's row in
> `plans/high-priority/README.md`.
>
> **Drift check (run first)**: `grep -n "fn row_bar" shell/src/views/diff.rs`
> must hit (the design pass is on your base). Line refs were taken at
> `00842dc` + the staged design pass; match on quoted content where a ref
> drifted; STOP on a structural mismatch.
>
> **Build cost**: `export CARGO_TARGET_DIR=/tmp/gitten-target`. Never launch
> `./dev desktop` or `./dev tui`.

## Status

- **Priority**: P2
- **Effort**: M
- **Risk**: MED — this *changes a deliberate decision*; read "Why the
  current behaviour exists" before touching anything
- **Category**: UX — a glance should not move the keyboard

## Why this matters

Scrolling over an unfocused pane focuses it before the wheel resolves
(`DevShell::on_wheel`, `shell/src/main.rs:3794`; the focusing arms at
`:3820-3828` — `self.focus_pane(at, cx)` / `self.set_spot(Spot::Main, cx)`).
So peeking at the commit list with the trackpad while working in the diff
moves the keyboard: the accent bar jumps panes, the status-bar badge and
hints change, and the next keypress lands somewhere the user did not send
it. A wheel is a glance; a click is a commitment. Every terminal multiplexer
and editor that gets this right scrolls the pane under the pointer and
leaves focus alone.

## Why the current behaviour exists (do not skip)

The comment at `main.rs:3806-3810` says: focus the region first, "otherwise
an unfocused pane's native list scroller would become a second,
unconfigured input path when this capture handler stood aside." The wheel is
deliberately routed through the keymap (`wheelup`/`wheeldown` resolve to
command *names*, `main.rs:3841-3860`), and commands dispatch against the
*focused* pane. Focusing was the cheap way to make the hovered pane the
dispatch target. The goal of this plan is to keep everything that comment
protects — one configured input path, keymap-resolved wheel — while removing
only the focus side effect: **resolve against the hovered pane's mode and
dispatch to the hovered pane, without focusing it.**

## Current state

- `on_wheel` (`main.rs:3794-3860`): clears pending chords; stands aside
  under help/pickers; hit-tests the stack (`list_bounds` per screen) and the
  main region; **focuses** the hit region; axis-locks via
  `views::diff::locked`; pans X on the screen; resolves Y through the keymap
  (`Code::WheelUp/WheelDown`) and dispatches by name.
- Command dispatch resolves mode and target from the *focused* pane (the
  `Modes` stack and `panes.focused()`); find the exact resolution path from
  `on_wheel`'s dispatch call before designing (grep the function it calls
  with the resolved name).
- Views' wheel motion goes through their shared viewport/reconcile model —
  scrolling moves the viewport and reconciles the cursor to stay visible;
  cursor moves disarm questions ("any cursor move, wheel or refresh
  disarms", `main.rs:2093`). That stays true for the *hovered* pane.
- Plan 042 (modal guard bundle) touches wheel-under-prompt behaviour; check
  whether it landed and rebase your reading if so.

## Commands you will need

| Purpose | Command | Expected |
|---|---|---|
| Shell tests | `cargoest -q -p gitten-shell`* | exit 0 |
| Everything | `./dev check` | exit 0 |

*typo guard: the command is `cargo test -q -p gitten-shell`.

## Scope

**In scope**: `shell/src/main.rs` (`on_wheel` and whatever narrow hook the
dispatch path needs to aim a resolved command at a named pane instead of the
focused one), tests.

**Out of scope**: the axis lock, the pixel-smooth path, the keymap
resolution of wheel codes (all correct); focus semantics of *clicks*
(clicking still focuses — plan 045); scrollbar drags (they already act on
their own pane).

## Git workflow

- Branch: `advisor/ui-049-wheel-keeps-focus`
- Commit style: `shell: the wheel scrolls the pane under it and leaves the
  keyboard alone`
- No push, no PR, unless the operator instructed it.

## Steps

### Step 1: Map the dispatch path

From `on_wheel`'s dispatch call, trace how a command name reaches a pane
method: where the mode comes from, where the target screen comes from.
Write the two-sentence summary in the Step 2 commit message. Decide the
narrowest seam: most likely a `dispatch_to(name, pane_name, …)` variant —
or a parameter threaded through the existing dispatch — that resolves the
*mode* from the target pane rather than the focused one, used **only** by
the wheel path. Do not fork the dispatch table.

### Step 2: Dispatch to the hovered pane without focusing

Replace `self.focus_pane(at, cx)` / `self.set_spot(Spot::Main, cx)` in
`on_wheel` with resolution + dispatch against the hit pane. Keep the
capture-phase interception exactly as is — the comment's "second,
unconfigured input path" concern is about GPUI's native scroller running
un-keymapped, and this plan keeps the interceptor as the only wheel
consumer. Update the `main.rs:3806-3810` comment to argue the new rule in
the house voice (a wheel is a glance; the keymap still owns both halves of
the motion; the hovered pane is the target *for this event only*).

The hovered pane's viewport/cursor reconcile and disarm rules run as they
do today — on that pane. The focused pane's state is untouched; the status
bar badge and hints must not change during the gesture.

**Verify**: `cargo test -q -p gitten-shell` → exit 0. `./dev check` → 0.

### Step 3: Tests

- `a_wheel_over_an_unfocused_pane_scrolls_it_and_focus_stays_put` —
  simulate the wheel path against a `DevShell` (there are existing
  `TestAppContext` shell tests dispatching commands; model on them), assert
  the hovered pane's viewport moved and `panes.focused_name()` did not
  change.
- `a_wheel_over_the_hovered_pane_disarms_only_that_panes_question` — arm in
  the hovered pane, wheel, assert disarmed; arm in the *focused* pane,
  wheel elsewhere, assert it survives (the arm belongs to the pane whose
  state moved, not to the pane that holds the keyboard — if the current
  disarm rule is global, keep it global and adjust this test; state which
  in the report).

**Verify**: `./dev check` → exit 0.

## Done criteria

- [ ] `./dev check` exits 0
- [ ] `grep -n "focus_pane\|set_spot" shell/src/main.rs` shows no call
      inside `on_wheel`
- [ ] The two tests above pass
- [ ] The `on_wheel` comment argues the new rule (no stale "focus that
      region before resolving" text)
- [ ] No files outside `shell/src/main.rs` modified (`git status`)
- [ ] `plans/high-priority/README.md` row updated

## STOP conditions

- Dispatch cannot address a non-focused pane without changing the `Modes`
  stack semantics that prompts and help depend on — report the coupling; a
  wrong fix here breaks every modal guard plans 037–042 built.
- Some pane verbs reachable via `wheelup`/`wheeldown` rebinding (the keymap
  allows `wheeldown = "view.page-down"` or any command) *require* focus to
  be coherent — enumerate which, and report rather than special-casing more
  than one.
- Plan 042's landed changes conflict with the `on_wheel` region beyond a
  trivial rebase.
