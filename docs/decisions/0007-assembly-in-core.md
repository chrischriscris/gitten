# 0007 — Row assembly belongs in `core`, not the view

**Status** accepted
**Date** 2026-08

## Context

Clipping, the intraline pass and the syntax pass were written inside
`views/diff.rs`. Within the same sitting the loop had been copy-pasted into
`examples/bench.rs` and `examples/paint.rs` — and only the shell's copy clipped or
intraline-diffed, so the two examples were quietly measuring and painting
something the app does not do.

`AGENTS.md` already said: *don't put logic in `shell/` that `cli/` would have to
duplicate.* Three copies existed before anyone wrote a `cli/`.

## Decision

`core::prepared::prepare(files, highlighter, max_line_chars) -> Prepared`. One
pass, in one place: clip, then intraline, then syntax. A frontend's share is
drawing.

The clip budget stays a frontend constant (`MAX_LINE_CHARS = 2000` in the diff
view) because how wide a row may get is a rendering question. `core` applies it.

## Why the ordering is part of the decision

Clipping must happen *before* both passes so that no span or token can point past
what will be drawn. Reversed, the renderer indexes a line by a range that no
longer exists in it: a panic in a debug build, mojibake in release. A test pins the
ordering.

## Evidence

Three call sites collapsed to one. `bench` now reports 288.7 ms of `prepare` on the
714k-line fixture — the honest total including clipping — where it had been
reporting 237 ms for the syntax pass alone and calling that the cost.

## Consequences

`prepare` returns owned `String`s and two `Vec`s per line, which the shell then
moves into rows: the same allocation count as before, no copies added.

The boundary test is now cheap to run: `paint.rs` draws a real diff — clipped,
intraline-marked, syntax-coloured, themed — and contains no pipeline code. When
that stops being true, logic has leaked back into the shell.
