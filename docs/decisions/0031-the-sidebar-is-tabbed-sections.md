# 0031 — The terminal sidebar is tabbed sections, not eight slices

**Status** accepted
**Date** 2026-09

## Context

The terminal registered eight sidebar lists — files, worktrees, branches,
remotes, tags, commits, reflog, stashes — and `BuiltinLayout` gave each an
equal slice of the column: `body.height / n`, the remainder to the earlier
panes. That was the right answer for one list and defensible for three. At
eight it is arithmetic nobody can read.

A 24-row terminal has 22 rows of body. Eight slices is **2.75 rows each** — a
header row and one or two rows of content. The commit list showed two commits;
the branches pane showed a section heading and one branch; the reflog showed a
header and nothing. Every viewport in the column was too short to answer the
question it existed for, and there were eight of them.

Widening the terminal does not help: the sidebar's height is the scarce axis
and `SIDEBAR_SHARE` only buys columns. Nor does dropping panes: each of the
eight earns its place — the objection is to showing all eight at once.

## Decision

**The lists are grouped into sections and take turns in them.**
`panes::SECTIONS` is the table, in lazygit's reading order:

| | tabs |
|---|---|
| status | `status` *(reserved; nothing registers, so it collapses out)* |
| files | `files`, `worktrees` |
| branches | `branches`, `remotes`, `tags` |
| commits | `commits`, `reflog` |
| stash | `stashes` |

Every section draws **one header row of tabs**. The section with the keyboard
draws **every row the column has left** — 19 of 22 with four sections drawn.
Only the tab a section is showing gets a rectangle at all; the tabs behind it
are hidden exactly as the narrow layout hides an unfocused pane, and are
resized when they are next shown.

`canonical_rank` is that table flattened, so the walk order (`h`/`l`,
`ctrl-j`/`ctrl-k`) is the order the column draws rather than registration order
behind the built-ins. `Placement::Sidebar` carries the section beside the rank.

**Keys.** `[`/`]` walk the tabs of the focused section, bound in a `tabs` mode
of its own — pushed only when the focused list has a second *registered* tab to
reach, so the pair is never advertised where it would answer with a refusal.
Not a corner of `panes`, because the two are different facts: `panes` is "there
is more than one list to cycle", which every client with a sidebar has, and
`tabs` is "the list with the keyboard shares its slot", which only a client
whose sidebar groups its lists has. The window pushes `panes` and would
otherwise have gained a `[` that says *not supported here* for a feature it
does not have.

The numbers are untouched: `2` is still `files.focus`. Reaching a section by
naming its first tab is what makes a section jump out of a binding that already
shipped, and it keeps every `<name>.focus` command meaning exactly one pane.

**Mouse.** A press on a tab's word focuses that tab; a press anywhere else on a
section header focuses the tab that section is showing. Both are the whole of
"expand", because the open section is only ever the one the keyboard is in.

**State.** Which tab a section shows and which section is open while the *diff*
has the keyboard are one fact at two scopes — the one the keyboard sat on last
— so `Panes` keeps one recency list of names and `Panes::spots` resolves both
from it, once per layout, into `Spot::shown`/`Spot::open`. A `Layout` therefore
stays a pure function of the spots and the body, and nothing about tabs is
re-derived per row.

**An absent pane collapses its tab out of the header**, and a section with no
registered tab collapses out of the column: a fixture launch has one list and
draws one header, not five. Nothing is advertised that a keypress could not
land on.

## Why not stack them and scroll the column

Two scroll axes in one column, and the pane you want is off screen with no
indication of where. lazygit's own answer is tabs, and the number keys were
already spelling out its sections.

## Why not shrink the unfocused panes to two rows each

That is the same eight viewports, six of them now useless *and* still taking
sixteen rows. One header row per section costs four and admits what it is.

## Why not keep the label on a section header

`  3  branches - remotes - tags` is 30 columns of a 40-column sidebar, so
`fake (main) · 2 local` arrives as `fake (ma`. Drawing it only where it happens
to fit makes whether the sidebar names your branch depend on how long a tab's
name is. The title bar says the focused pane's label in full, at the width of
the whole window, and that is the label anybody is reading.

## Why the reserved `status` slot stays in the table

Nothing registers a `status` pane in the terminal and this record does not
invent one. The row stays so that the sections below it are numbered `2`–`5`,
which is what the shipped `global` bindings already say, and so that
`status.focus` names a rank rather than falling to the tail with the
extensions. An empty section draws nothing, which is the same answer an absent
`worktrees` gets.

## Consequences

- Three sidebar viewports out of four are one row tall, so anything read off an
  unfocused list is read off the title bar or after a keypress. That is the
  trade and it is the point.
- The frame geometry moved: the open section's rows depend on which section is
  open, so a test that hard-codes a screen row has to say which section has the
  keyboard. Every one of them now does.
- What would make us revisit: a *taller* terminal. At 60 rows, four sections of
  fourteen would be readable and the tabs would be costing something. The
  layout is a `Layout` implementation, so that is a policy change and not a
  rewrite.

## Superseded 2026-09: equal shares, like lazygit

The expand-the-focused-section policy above was replaced: every section now
keeps an equal share of the column (remainder to the earlier sections),
focused or not, and focus is header highlight alone. The trigger was the
reference itself — lazygit shows every section with real rows at all times,
and the one-row collapsed sections meant anything read off an unfocused list
needed the title bar or a keypress. Four sections at 6, 6, 5, 5 rows over a
22-row body is glanceable in a way eight slices at three rows never was, and
it is what the reference does, so the trade the old Consequences section
defended is gone rather than rebalanced.

Two companion changes landed with it. The number keys focus *sections*: a
section head's `<name>.focus` lands on the tab that section is showing
(`Panes::shown_tab`), so `2` with worktrees showing focuses worktrees. The
`<name>.focus` names themselves are unchanged — no new command, no help
churn — and `App::focus_named`, the walk, the cycle, the clicks and the tab
keys all still move to exactly the pane they name. `Spot::open` and
`Header::open` are removed: nothing outside the layout's own tests read
them, and height no longer has an expanded state to record.

## Supersession (2026-09-08): h/l cycle panes, [ and ] cycle a section's tabs

Two keys moved onto the lazygit model the sections were built for:

- **`h`/`l` cycle the panes** — every sidebar section, standing for the tab
  it is showing, then the main region, wrapping. A pane is a section or the
  main region; the old walk's edge-stop is gone. `pane.left`/`pane.right`
  keep their names; the walk they replaced is not coming back.
- **`[`/`]` cycle the tabs of the selected section**, wrapping inside it,
  and are only advertised where the focused section has a second registered
  tab — the honest reading of "the tabs of the selected pane". A fixture
  with one list and a stash on its own are the two shapes without them.

The equal-shares policy above stands; nothing in this record's arithmetic
changes with either key move.
