# 0022 — A notch is a key, a button is a place

**Status** accepted
**Date** 2026-08

## Context

The terminal client asked for mode 1000 so the wheel would scroll, decoded the
clicks it got as a side effect, and threw them away. That was defensible for
exactly as long as nothing could use one — but it had already taken something
away: **an emulator forwarding clicks is no longer drag-selecting text with
them**, so gitten was the one program in the terminal you could not copy a line
out of. `shift` (`option` on iTerm) is the escape hatch and it is not a feature.

Meanwhile the wheel was the only way to move a long list with the pointer, and
nothing on screen said where in a 714k-row diff you were.

## Decision

**Two different things, routed two different ways.**

A **wheel notch is a key**: `Code::WheelUp` / `Code::WheelDown` resolve through
the keymap to `view.scroll-up` / `view.scroll-down`, appear on the `?` panel and
are rebindable in `gitten.toml`. Unchanged from 0012's rule — it is data, so it is
config.

A **button is a position**, and a position cannot be a key, because a config file
cannot hold a hit test. It leaves `term.rs` as `Input::Mouse { kind, col, row }`,
`main.rs` subtracts the two rows of chrome it owns, and the view underneath does
the rest. The seam is `Rows::hit(index, seg, col, shift) -> Option<Hit>`, which is
the shell's `hit` with pixels replaced by cells; `Hit` moved into `core::select`
so the two doors spell it the same way.

**The model is `core::select`, unchanged.** Carets, which rows lie between them,
what a wrapped line copies, where a word ends and what a two-column diff does
when you drag down one side were all decided in
[0018](0018-selection-is-a-model-not-a-text-element.md) for the window, and the
terminal needed none of it rewritten — it needed `hit`, `selectable`, and a
background on a run.

**The scrollbar's geometry is `Viewport::thumb`** and only its glyphs are the
client's. It is drawn *over* the last column of the rows, not beside them.

**Copying is OSC 52**, and **finishing a selection copies it** — a drag, a double
click or a triple click, once, when the button comes up. `[mouse]
copy_on_select` turns it off.

## Why not

**Leave the mouse alone and keep the emulator's selection.** That is `--no-mouse`
and it is still there. As the default it means no wheel, which is the one mouse
gesture everybody uses, and it means the app cannot ever act on a click — no
click-to-focus-a-pane, no drag on a scrollbar, no double-click to open a commit.

**Mode 1003 instead of 1002.** 1003 reports every cell the pointer crosses over
an idle screen: a packet per cell, decoded and discarded, for a feature nothing
has. 1002 reports motion only while a button is held, which is exactly a drag.

**A `Pointer` type in `core`, beside `Key`.** Tempting, and wrong: the coordinate
is in cells here and pixels in the window, and the one genuinely shared part —
which row, which byte — is `Rows::hit`'s answer and already crosses the boundary
as a `Hit`. A shared event type would have carried a unit that means two things.

**Reserve a column for the scrollbar.** One column fewer is a different wrap,
which is a different row count, which is a different scrollbar. Overlaying costs
the last column of text on a list long enough to scroll, and with wrapping on
nothing reaches it anyway.

**A thumb sized and placed proportionally to `len`.** The obvious version, and it
leaves the thumb short of the bottom of the track when the list is scrolled all
the way down — which reads as "there is more below" when there is not. Placing it
against `max_top` and the free travel makes touching the end mean exactly one
thing.

**`cmd-c` / `ctrl-shift-c`, the way you would in any other terminal program.**
They never arrive: the emulator intercepts them before the pty, and `Key` has no
super modifier because no terminal delivers one reliably. What they copy is the
emulator's own selection, which gitten cannot write to and which is empty anyway
while mode 1002 is on. Hence copy-on-select and `y`, which is also why
copy-on-select is *on* by default: an app that takes the drag and gives back no
copy is worse than one that never took it.

**Copy on select into the primary selection (OSC 52 `p`) instead of the
clipboard (`c`).** That is what X11 actually does, and it is the more polite
answer — a middle-click buffer that does not touch what you last copied
deliberately. It is also useless on macOS, where there is no primary selection
and an emulator either ignores the write or aliases it to the clipboard. One
buffer, named once, beats a knob that means two different things per platform.

**`pbcopy`, or a clipboard crate.** A terminal is frequently not on the machine
the clipboard is on — ssh, a container, tmux — and only OSC 52 follows the
session rather than the process. It also costs no dependency, in a crate whose
two dependencies are a stated constraint. The price is real and is stated in
`docs/terminal.md`: an emulator that does not implement it copies nothing and has
no way to say so.

**Row-range selection in the diff, or byte selection in the commit list.**
Neither. A diff is text and copies as text; a commit row is a sha, initials, a
graph and a subject in fixed columns, and a graph is not something anybody pastes.
So the diff selects bytes and the list selects commits, and `y` means the same
thing — "copy what is selected" — in both.

## Evidence

A selection costs `Selection::at` per visible row per frame — two integer
comparisons against the caret's cached visual range
([0018](0018-selection-is-a-model-not-a-text-element.md)) — and the scrollbar
costs one `Screen::over` per visible row. Neither is a function of diff size, and
neither is measurable: the terminal frame table in
[../measurements.md](../measurements.md) was re-run at `FRAMES=200`, and building
with `Scrolling::scrollbar` defaulted to `false` gives back 15 µs, 12 µs and
29 µs against 15, 12 and 28 with it on.

## Consequences

Mode 1002 is on, so the emulator's own drag-to-select needs `shift` (`option` on
iTerm) — the same override it always needed for the wheel, now buying something.
`--no-mouse` remains for a terminal that has neither.

The double-click interval is ours to keep: the protocol carries a press and no
count, so `main.rs` holds a 400 ms clock and a cell. That is the only clock in
the client, and it is why `core::command` still has none.

What would make us revisit it: **panes**. `main.rs` currently routes to the top
of the screen stack because there is exactly one thing under the pointer. When
there are two, that function grows a hit test over the layout — and nothing else
has to change, which is the point of the coordinate stopping there.
