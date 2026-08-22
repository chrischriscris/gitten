# 0023 — The gutter does not scroll, so the offset is not the list's

**Status** accepted
**Date** 2026-08

## Context

With wrapping off, a line wider than the window has to be reachable. The window
did that the way `uniform_list` offers: `ListHorizontalSizingBehavior::Unconstrained`,
rows as wide as their text, and the list scrolls them. It works, and it scrolls
the **whole row** — so eight characters into a minified line the line numbers, the
`+`/`-` and the file's own name have left the screen, and what is on the left edge
is the middle of a string. The terminal had never done that: `Pen::scroll` swallows
columns of everything written *after* the gutter, so the numbers stay and the text
moves under them.

Two clients, one diff, two answers to "what is at the left edge" — and the
window's was the worse one. Nothing outside a row can fix it, because the list
scrolls a row and a row is the numbers and the text together.

## Decision

**The horizontal offset belongs to the view, and a row is always exactly as wide
as the viewport.**

`Pan` is one `Cell<f32>` on `Diff`: how far the text is scrolled, how far it may
go, and the box the scrollbar overlays. A presentation is handed the offset in
`render` and puts its text in `scrolled(shift, …)` — a `flex_grow` window with
`overflow_x_hidden`, `min_w(0)` and a negative left margin on the text. Whatever
it drew before that stays where it is. `Rows::overflow` is the other half: how far
the widest row reaches past the right edge, which is what bounds the offset, and
`Rows::hit` takes `x` from the window's left edge plus the offset separately, so
the caret arithmetic is the terminal's — `(x - chrome).max(0) + shift`.

Side-by-side follows: the columns are half the window each whatever the wrap is
doing, and each column's text scrolls inside its own window. That is what the
terminal already did, for the reason a terminal could not avoid.

A **negative margin, not a slice of the string.** The syntax tokens and the
intraline spans address the line, so cutting the text before `runs` merges pairs
styling with the wrong bytes — the same trap `Pen::scroll` exists to avoid.

## Why not counter-translate the gutter instead

Keep the list scrolling and draw the furniture at `+offset` inside each row, so it
lands back at the window's edge. It is a smaller change and it is two coordinate
systems in one row: the gutter has to be absolutely positioned and painted *after*
the text it now covers, with an opaque background of its own, and the hit test has
to subtract an offset the row's own geometry still contains. Worse, it cannot do
side-by-side — the second column's gutter is in the middle of the row, and holding
it still while the text either side of it moves is not a translation of anything.

## Why not leave the second client different

Because the seam is shared and the divergence was not a design: `shift` was in the
terminal's `Frame` and in its `hit`, and the window's row trait had no word for
it. One of the two had to grow the concept, and the one with the sticky gutter was
the one that already read correctly.

## Evidence

No frame-time change to measure — the offset is one `f32` read per frame and one
extra element per visible row, against fifty rows. What it *removes* is
measurable: `with_width_from_item(Some(widest))` had `uniform_list` lay out and
shape the widest row on every prepaint to decide a scrollable width, which on
`pr30683.diff` is a 65k-token line clipped to 2000 characters. That measurement is
gone; the list now measures row 0 for its height, which is `ROW_H` by construction.

## Consequences

The window owns three things the list used to: the wheel, the bound, and the
scrollbar. The bar is `gpui_component`'s over a `ScrollbarHandle` impl on `Pan`,
four methods, and it drags the thumb for us.

The wheel is the one that bit. Two components on one event is not a division of
labour: `uniform_list` leaves `overflow.x` visible, and `div`'s handler treats
that as permission to scroll *vertically* by a horizontal delta, so a sideways
flick moved the rows down while the text moved right — diagonal, and it read as
the view drifting rather than as a bug. So the axis is decided **once**, in the
capture phase, and a horizontal gesture never reaches the list at all. And it is
decided per *gesture* and not per event, with `gpui::OngoingScroll`: a trackpad
swipe is never exactly straight, and choosing again fifty times a second is the
same drift by another route. A gesture that unlocks mid-flick keeps both axes,
because eating it would stop the diff scrolling down until the fingers lift.

Whether there is anywhere left to scroll does not enter into it: a page with
nothing to the right does nothing when you swipe right, it does not start
scrolling down.

A presentation that ignores `shift` and `overflow` compiles and does not scroll
sideways, which is correct for one that wraps.

Two things follow that are not obvious. A presentation is now told the width even
when the wrap breaks nothing — with wrapping off the width still decides a
column's half, a Markdown rule's length and how far there is to scroll — so the
cheap path for a non-breaking wrap moved from the view into each implementation.
And the offset is bounded by the overflow rather than by the widest row's whole
width, which the terminal uses: you cannot scroll the last character off the left
edge here, because a window has a scrollbar whose thumb would then be lying.

What would make us revisit it: a keyboard binding. `view.left` and `view.right`
are registered commands with `h`/`l` bound in `core::command`, and the terminal
dispatches them; the window has no key for them yet, because it still binds keys
by hand in `main.rs` rather than reading `[keys]`.
