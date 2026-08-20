# 0005 — Colour is data, and it lives in `core`

**Status** accepted
**Date** 2026-08

## Context

35 hex literals were spread across four shell files, and the syntax palette was a
private `fn colour(kind) -> u32` inside the diff view. Changing the theme meant
editing the renderer.

`core/` is not allowed to know a UI exists, which makes "put the theme in core"
look wrong at first glance.

## Decision

`core::theme::Theme` holds every colour the app draws as `0xRRGGBB`, plus bold and
italic flags. No GPUI types, no `Hsla`, no rendering. The shell converts to
`Hsla`; the ANSI painter converts the same numbers to escape codes.

## Why this is not a boundary violation

The rule bans knowing about a UI, not describing appearance. A `u32` is not a UI
type, and the test that it stayed honest is concrete: `examples/paint.rs` reads the
same theme and needs no code of its own. A palette only one frontend can read is
a palette in the wrong crate.

## Why not a theme file format

Later, and it changes nothing here: parsing TOML into this struct is additive.
Shipping a format first would have meant designing one before knowing which fields
exist.

## Consequences

`Theme` is 15 diff colours, 7 chrome colours, two cycling lists, 12 styles and a
contrast floor. Every one of them is a public field, so an extension mutates what
it likes — at the cost of `rebuild()` being required after direct edits, which the
type system cannot enforce without closing the struct.

Lane and author colours cycle, so a theme may ship any number of them; an empty
list falls back to chrome colours rather than panicking.
