# 0018 — A selection is a model, not a text element

**Status** accepted
**Date** 2026-08

## Context

You could not select text in the window. A browser gives it away and so does a
terminal, so the two other doors had it for free and the product did not — which
is the wrong way round for [the client that is the product](../../AGENTS.md).

GPUI has nothing to turn on. `StyledText` shapes a line and paints glyphs; it
carries a `TextLayout` that can map a point to a byte, but nothing that holds a
selection, paints one or copies it. Zed's editor implements all of it itself.

Three constraints, all of them from work already done:

- **`uniform_list` only builds the visible rows.** A selection that ran off the
  bottom of the window has no element at the far end to anchor to, so it cannot
  be element state. It has to be state the *view* holds and the rows read.
- **A wrapped line is several rows and one line**
  ([0017](0017-wrapping-is-more-rows-not-taller-ones.md)). A selection addressed
  in visual rows evaporates on every resize, and one that copied per visual row
  would paste the width of somebody's window into their file.
- **Where the text starts is the presentation's business.** Two gutters and a
  sign column in the built-in, a divider and two of everything in
  `SplitRows`, and in `MarkdownRows` a quote bar, three levels of indent, a
  bullet and an 18px heading — per row.

## Decision

**The selection is a model in `core::select`, and the frontend owns two things:
pixels in, and a background colour out.**

`Caret` is a *logical* row — the `(owner, index)` pair `RowRef::logical` returns —
plus a byte offset into that row's text. Byte offsets because that is what the
edit script, the tokens, the spans and `Wrapped` already address, so nothing has
to be converted before it can be painted. Logical rows because a reflow moves
every visual one, exactly as it moves the reading position.

**The frontend's half is two trait methods and no more.** `Rows::hit` turns an
x in pixels into `(part, byte)`, and `Rows::selectable` hands back the text of a
part. Both default — `hit` to `None` — so an extension's presentation compiles
unchanged and is simply not selectable until it says where its text is.

**A caret caches the visual rows its logical row occupies.** The render path asks
*is this row selected, and which of its bytes* once per visible row per frame, and
it may not answer that by searching a 714k-entry order table. With the cache it is
two integer comparisons and a `RowId` compare. `Selection::resolve` rebuilds the
cache from the order table after a reflow — once per resize that crossed a
character boundary, against a re-expansion that is already O(n).

**A selection is fixed to the `part` its anchor landed in.** A row may draw more
than one text; `SplitRows` draws two. Dragging down the left column selects the
left column, and a drag that crosses the divider runs to the *edge* of the column
it started in rather than following the mouse. Parts are laid out left to right,
which is what makes that rule expressible without either side knowing what a
column is.

**A part with no text is skipped, not pasted as a blank line.** Dragging down the
new side of a side-by-side diff past a lone removal yields the new file with the
holes closed up — code that compiles rather than code with gaps in it.

**The highlight is a third layer in the run merge, not an overlay.** Tokens style
the foreground, intraline spans light the changed words, and the selection lights
whatever is selected — one sweep over three sets of edges, into the flat
non-overlapping run list `StyledText` already wanted. A selection outranks a
changed word: both are backgrounds, only one can be drawn, and the reader already
knows which words changed.

**`Surface::Selected` is an eighth surface.** So every syntax class is resolved
against the selection background by the machinery that already exists
([0009](0009-contrast-resolution.md)). Without it, `comment` on the selection is
the one run in the diff nobody can read.

**The keys are the platform's, and the commands are named anyway.** `cmd-c`,
`cmd-a` and `escape` are bound with `KeyBinding::new` beside `s` and `w`, because
a Mac user's fingers already know them and `core::command::Key` has no `cmd`.
`copy.selection`, `select.all` and `select.none` are registered commands all the
same, so the day the window reads `[keys]` they are rebindable with nothing
changing in `main.rs`.

## Why `font.advance` and not `TextLayout::index_for_position`

GPUI can answer this exactly, and exactly is not free: it needs the laid-out
`StyledText` of the row that was clicked, which is built inside the
`uniform_list` closure and dropped at the end of the frame. Keeping them means a
side table of `TextLayout`s per visible row per frame, written on the render path
and read by the mouse — and it only ever answers for rows that are *on screen*,
so `hit` would still need a second implementation for a drag past the bottom.

`columns()`, `width()` and `with_width_from_item` are all already
`font.advance` arithmetic, and `Font::monospaced` exists to be asked. So the
caret is exact in a monospaced face and drifts along a long line in a
proportional one — the same approximation, in the same place, as the wrap budget
it has to agree with.

## Why not paint an overlay

A rectangle per selected row, absolutely positioned over the list, is the obvious
alternative and needs three things the run merge does not: the exact pixel scroll
offset every frame, a per-presentation answer for where a byte *is* in x rather
than the reverse, and a translucent colour over text instead of behind it. It also
cannot style the selected text — which is what `Surface::Selected` is for.

## Why the drag listener is on the window

An element's `on_mouse_move` fires only while the pointer is inside its box, so a
selection dragged up into the title bar would silently stop extending halfway
through. While a drag is live the mouse belongs to it wherever the pointer is, so
the move listener is registered on the *window* — from a zero-height `canvas`,
because `Window::on_mouse_event` asserts it is called during paint and `render`
is not paint. Only while dragging, so no listener is walked on a mouse move that
is not one.

`on_mouse_up_out` catches the button released somewhere else entirely, which is
otherwise a drag that never ends.

## What is deliberately not there

**Autoscroll has no clock.** Dragging past an edge pulls the diff along a row per
row of overshoot, per mouse-move event. Holding the mouse *still* outside the
window does not keep scrolling — that needs a timer running for as long as a
button is held. It costs less than it sounds: `locate` is not clamped to the
viewport, so the selection already extends onto rows that are not drawn, and what
the autoscroll buys is being able to see where it got to.

**The commits view is not selectable.** It has no `Rows` seam — no
presentations, no order table — so this bought it nothing. The right answer there
is `copy.sha` and friends: a command per field, not a drag.

## Consequences

**`shell` uses `core`'s `RowRef` and `Ordered` now.** It had field-identical
copies of both; `core::select` speaks the real ones. Its `expand` is still its
own, because a `Rows` returns an `AnyElement` and so cannot be a `Present` —
which remains the reason for the last row-flattening duplicate in the tree.

**`Rows::render` takes a fifth argument.** Every implementation's signature
changed, including an extension's. The alternative was a presentation reaching
back into the view for state it is being drawn from, which is worse.

**A click clears.** Which is why a fresh `Selection` is *empty* until something
extends it, rather than being a one-byte selection at the caret: pressing the
mouse has to be able to mean "no longer selected".

**A layout change drops the selection.** A replace pair is one row in the
two-column presentation and two in the unified one, so there is nothing to carry
across and `resolve` says so rather than guessing. A wrap change and a resize keep
it.

## Evidence

`cargo test -p gitten-core select` and `cargo test -p gitten-shell`. The tests worth
knowing about, because each is a bug that was available:

- a wrapped line copies **once**, without the break
- the same drag backwards copies the same bytes
- a click on a continuation row addresses the *line*, not the row
- the divider decides which column a click is in, on both sides of one pixel
- a bullet's indent and a heading's type size both move the caret with them
- a selection survives a reflow and dies with a layout change
