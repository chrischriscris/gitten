# Plan 055: The status pane earns its keycap

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving on. On
> any STOP condition, stop and report — do not improvise. When done, update
> this plan's row in the high-priority index.
>
> **Drift check (run first)**:
> `git diff --stat 635aba8..HEAD -- shell/src/views/status.rs shell/src/main.rs core/src/command.rs`
> On any in-scope drift, compare the "Current state" excerpts against the
> live code; on a mismatch, STOP. Pass 8's plans 037/039 touch
> `core/src/command.rs` and `main.rs` — expect drift there and match on
> content.
>
> **Base**: `git switch -c advisor/ui-055-status-verbs origin/full/full`
> (`635aba8`). Line numbers are against that commit; match on quoted content.
>
> **Shared ground rules**: see the README in this directory.

## Status

- **Priority**: P1
- **Effort**: M

## Why this matters

Pane 1 is a focus stop with nothing to focus. `1` is bound globally to
`status.focus` (`core/src/command.rs:382`; the command table names it at
`:1017`), the header lights up with the accent bar like every focused pane
— and then the keyboard is nowhere: the view holds no cursor by design
(`shell/src/views/status.rs:107`: "The pane holds no cursor, so it has no
view model to test") and no verb is reachable from it. A keycap reads as
*press me* (`chrome.rs:170`, the keycap doc's own words); pressing this one
teaches that the affordance lies.

Meanwhile the verbs that belong to exactly this pane's facts — the branch
and its drift from upstream — exist as commands with hints (`push`, `pull`,
`fetch` hint text at `core/src/command.rs:1088-1098`) but no home a user
can *see*. lazygit's `[1] Status` pane (which this pane cites as its model,
`status.rs:3-11`) is where sync verbs live.

So: the pane gets a cursor over a short list of action rows — pull, push,
fetch — each row a command *name* projected from the same table the keymap
and the help overlay read. Selecting and pressing `enter` dispatches the
name through the same path a keybinding takes. Rule 1 falls out: an
extension adding a verb to this pane is adding a row, which is data.

## Current state

- `status.rs` renders two static lines: the branch (`⎇ full/full`) and its
  drift `↑n ↓m` or `✓` (`status.rs:80-93`), acquiring nothing itself.
- The sidebar section builder gives status the `_ => 0` rows arm, so it is
  sized as header + one row.
- `chrome::list_row` (`chrome.rs:70`) is the shared cursor row device the
  other four panes use.
- The command table (`core/src/command.rs`) holds `("<name>", "<help>",
  Some("<hint>"))` rows — push/pull/fetch are present near `:1088-1098`;
  find their exact command names and current mode bindings before Step 1.

## Scope

**In scope**: `shell/src/views/status.rs` (cursor + rows + tests),
`core/src/command.rs` (a "status" mode's j/k/enter bindings if pane modes
are how the other panes do it — mirror them exactly), `shell/src/main.rs`
(dispatch arm for the pane's cursor commands, the section's `rows` count so
the pane is sized for its rows), docs touch-ups where the pane is described
as verb-less.

**Out of scope**: new git verbs, remotes management, any change to what
push/pull/fetch *do*, the pane's branch/drift line content.

## Git workflow

Branch `advisor/ui-055-status-verbs` from `origin/full/full`.

## Steps

### Step 1: Read how a pane owns its keys

Before writing anything: read how the files pane binds j/k/enter and
dispatches (its mode in `core/src/command.rs`, its `run` arm in the view,
the shell's dispatch). The status pane must be the fifth verse of the same
song, not a new melody. Write down (in the commit message of Step 2) the
three places a pane's verb passes through.

### Step 2: Rows as data

Give `Status` a row model: the existing branch/drift line stays row 0
(inert, not selectable — it is a fact, not a verb), followed by action rows
`(command_name, label)`: `pull`, `push`, `fetch` (their exact registered
names from Step 1). The rows live in a slice the view exposes — a
projection, so the section builder's `rows` count and the render read the
same source. The view gains the same `select`/cursor shape its four
siblings have (if 045 has landed, reuse its `select(ix)` convention so a
click works here too for free).

**Verify**: view unit tests — cursor clamps to action rows, skips the fact
row; `rows()` reports the real count so the pane is sized to show them.

### Step 3: Enter dispatches the name

`enter` on an action row hands the command *name* to the same dispatch a
keypress resolves to — nothing in the chain is a function pointer. The
existing global key for each verb keeps working unchanged; this pane is a
second door to the same commands, which is the proof they were commands.

**Verify**: a `TestAppContext` test — focus pane 1, move to `pull`, enter;
the fake `Repo` records the pull. The same test asserts the global key
still reaches it.

### Step 4: The rows read as verbs

Draw action rows with `chrome::list_row` plus the row's hint-style label —
the label in `c.fg`, the bound global key (if any) right-aligned in `c.dim`
the way the help overlay spells pairs, so the pane quietly teaches the
faster path. While a job runs, the running verb's row shows the same
feedback the status band shows (plan 032 landed elapsed text — read it from
the same cell, do not duplicate state).

**Verify**: build; row content asserted in a render-adjacent test if the
harness allows, else state it.

### Step 5: Full gate

`./dev check`. Hand the owner `./dev desktop`.

## Test plan

- Cursor model tests (clamp, skip fact row).
- Dispatch test through the fake repo (enter on each of the three rows).
- Existing status pane facts (branch, drift) untouched — its current tests
  stay green.

## Done criteria

- `1` focuses a pane where j/k move a visible cursor and `enter` runs the
  selected verb through the standard dispatch.
- The action rows are data (name + label), and an extension command added
  to the same table would render and dispatch with zero shell edits.
- The keycap's promise ("press me") is kept everywhere it is drawn.

## STOP conditions

- A docs/decisions entry (search `docs/decisions/` for the status pane)
  documents the verb-less pane as a *decision* rather than a state — report
  it and stop; this plan then needs the owner's call, not an executor's.
- The pane-mode pattern from Step 1 turns out to require `core` changes
  beyond adding a mode's bindings — report the shape.
- Drift check fails on `command.rs`/`main.rs` beyond pass 8's stated scope.
