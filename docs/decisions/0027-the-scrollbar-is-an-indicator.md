# 0027 — The scrollbar is an indicator

**Status** accepted
**Date** 2026-08

## Context

The terminal's scrollbar was draggable, the way every window scrollbar is: a
press on the thumb grabbed it, a press on the track jumped to the pointer, and
a drag moved the list. On screen it worked. In the hand it did not, and the
reason is arithmetic the drag cannot escape.

A thumb's travel is one viewport's worth of cells; the list's is everything
else. `Viewport::thumb` places the thumb against `max_top` and the free travel,
so the list moves `max_top / (track − thumb_len)` rows for every cell the
pointer moves. A seven-thousand-commit log in a forty-row pane: travel 39,
`max_top` 6,960 — **a hundred and seventy-eight rows for every cell of drag**.
A 714k-line diff: **eighteen thousand**. The pointer in a terminal reports
whole cells and nothing finer, so that number is not a lag or a tuning
constant — it is the quantum of the control. The window carries the same ratio
and wins on pointer resolution alone: twenty-five-odd positions to the cell
means its thumb can be aimed. A terminal's cannot.

## Decision

**The bar says where you are and takes nothing.** `hit`, `grab` and `drag` are
gone from `scrollbar.rs`, with the `grabbed` field they served in every pane.
A press on the bar column lands on no pane at all. The division of labour that
replaces it: precision is the keyboard's (`j`, `ctrl-d`, `/`), scrolling is
the wheel's, and the bar is the one thing on screen that says where in a
714k-row diff you are — at half-cell resolution, never smaller than a cell of
ink.

**Where it sits**: the thumb is `▐` — the right half of the pane's last content
cell. At an internal boundary it sits immediately inside the existing rule
without repainting or distorting that rule; at the screen edge its right edge
is flush with the terminal boundary. (An earlier design straddled `▐▌` across
the pane and divider cells, but painting into the divider column overwrote the
centred rule `│` / `╎`; keeping the thumb inside the pane preserves both the
rule and row backgrounds cleanly.) At fractional rows, `▝` and `▗` provide
half-cell vertical resolution. The overlay costs no reflow for the reason 0022
stated: with wrapping on nothing reaches that column, and with wrapping off the
line is being scrolled sideways underneath it. The pane paints itself; the edge
geometry is the paint loop's.

The window keeps its draggable bar. That is a difference between the doors,
not a gap in this one — the same kind `split.rs` already documents for
horizontal scrolling, and for the same reason: the pointer.

## Why not

**Keep the drag and accept the coarseness.** The bar already drew a one-cell
thumb on a huge diff, so the hand had nowhere to hold it and nowhere to put
it. A control that cannot be aimed is not a control; it is a jump with a
random walk attached.

**Slow the drag rate** — map pointer motion to a fraction of the list, the way
a touch screen scrolls content faster than the finger. That breaks
thumb-under-finger, which every scrollbar the user has ever dragged honours;
and the terminal cell is the *pointer's* resolution, so a slowed thumb stops
following the hand and the drag reads as broken rather than as gentle.

**Half-cell drag resolution.** The `▀`/`▄` halves the bar now draws are
paint, not protocol: the emulator reports a mouse position as a cell and
nothing between cells exists to report. The halves sharpen the eye; they
cannot sharpen the hand.

**Keep click-to-jump on the track.** A click lands where aimed — one shot, no
accumulated overshoot — so it survives the coarseness argument that killed the
drag. But a bar that takes clicks and not drags is a stranger affordance than
one that takes nothing, and the keyboard already owns every jump this app
needs: `gg`/`G`, `ctrl-d`/`ctrl-u`, `/`. Quiet wins.

**Remove the scrollbar entirely.** It is the only place a 714k-row diff says
where you are. The cost is one `Screen::over` per visible row, and
[0022](0022-the-mouse-in-a-terminal.md) already measured it unmeasurable.

## Evidence

The drag arithmetic is `Viewport::thumb`'s own, run at the numbers above —
no simulation, the same formula the bar draws with. The removal took
`hit`, `grab`, `drag` and the free `thumb` out of `scrollbar.rs`, a
`grabbed: Option<usize>` out of five panes, and left every press test
asserting the new contract: the bar column is the row under it, and the list
does not move.

## Consequences

A press on the divider is still a press in no pane. The bar overlays the pane's
last content cell but takes no mouse input; a press there remains a press on
the row underneath it.

`Viewport::top_at` stays in `core` untouched. It is the seam, not the
implementation — `core`'s tests still pin it as `thumb`'s exact inverse, and a
door with pointer pixels is entitled to drag with it. Nothing in this
repository does, today.

What would make us revisit it: a terminal that reports pointer positions
finer than a cell. None does; if one ever does, the seam is waiting.
