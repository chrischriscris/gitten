# 0019 — The window has no titlebar of its own

**Status** accepted
**Date** 2026-08

## Context

`WindowOptions::default()` leaves `appears_transparent: false` and `title: None`.
That is an opaque macOS titlebar — system grey, titled with the *executable's*
name because nothing set a title — sitting directly on top of the app's own
32-pixel strip, which is where the real title lived. Two title bars, one of them
written by nobody, in a palette that is not ours.

The same window had a second borrowed-chrome problem. `gpui_component::init` sets
its theme to **Light** and nothing here ever changed it, so the two scrollbars —
the only widgets from that library the app uses — were painting a light track and
an accent-coloured thumb over a `#0e0d0c` diff.

## Decision

The strip *is* the titlebar. `appears_transparent: true`, the title set so the
window still has a name in Mission Control and the Window menu, and
`traffic_light_position` at `(10, 10)`: macOS uses that inset above and below the
12-pixel button to size the band, so it comes out at exactly `TITLE_H` and the
lights sit centred in our own strip rather than floating above it. The title then
starts after them, which is what `LIGHTS_W` is.

Dragging stays the platform's: `app_owns_titlebar_drag` is left false, so the
empty part of the strip moves the window and nothing here implements that.

`config::sync_widgets` pushes our palette into the other library's theme — dark
mode first so anything we have *not* named lands somewhere defensible, then the
scrollbar track, thumb and radius, then `Theme::sync_base`, which is what actually
reaches the layer that paints. It runs again on every config reload, so a
scrollbar follows a saved `plait.toml` like every other colour in the window.

## Why not draw our own scrollbar

Rule 2 wants every colour to come from `plait_core::theme`, and this is the
cheaper way to get it: eight assignments against a thumb, a track, hover states
and drag handling. [0015](0015-title-bar-controls-are-hand-rolled.md) hand-rolled a
picker and gave three reasons for it; none of them apply here — there is no
placement problem, no trait to satisfy, and the colours *can* be handed over. The
day a scrollbar needs to look like something the library cannot express, that
record is the shape of the argument to make.

## Why a minimum window size

There is no useful window narrower than its own gutters. The diff view's wrap
budget already bottoms out at eight characters and says why; `window_min_size` is
the other end of the same sentence.

## Consequences

Both are invisible until the window opens, and neither is covered by a test — the
strip's geometry is a number in `window_options` and the scrollbar's colour lives
in another crate's global. `./dev desktop` is the check.

If a titlebar ever needs to be taller than the traffic lights want, `TITLE_H` and
the `(TITLE_H - 12) / 2` in `window_options` are the two numbers that have to move
together.
