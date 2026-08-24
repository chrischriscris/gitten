# 0020 — Furniture has a lower contrast floor than text

**Status** accepted
**Date** 2026-08

## Context

[0009](0009-contrast-resolution.md) resolves every token class against every
surface and asserts a 3.5 floor for all of them. It ran for syntax tokens and
nothing else, so the *furniture* — line numbers, and the coordinates in a hunk
header — was a single hex literal drawn on five different row backgrounds.
Measured: **2.05:1** on a context row and **1.60:1** on a moved one. Below the
WCAG floor for non-body text, and below the point where a number is worth the
column it occupies.

Raising it to `min_contrast` is the wrong fix. Clearing 3.5 on every row
background needs roughly `#807974`, which is nearly as bright as the code the
numbers are labelling.

## Decision

A second floor. `min_furniture`, shipped at **3.0** — the WCAG figure for
everything that is not body copy — resolved through the same `readable` into the
same kind of per-`Surface` table:

```rust
theme.gutter_on(surface)
```

Two floors because they are two jobs: body text is read continuously, and a line
number is looked up once and should recede for the rest of the time. A test
asserts the second is lower than the first, so the day they converge one of them
is wrong.

The hunk header split follows from it. `gitten_core::hunk_parts` cuts
`@@ -41,9 +41,11 @@ fn dispatch() {` at the second `@@`, the coordinates take the
furniture colour and the declaration keeps `hunk_fg` — because the coordinates
*are* furniture, a line number with a range around it, and drawn as one run they
claimed to be as interesting as the code.

## Why not resolve every chrome colour this way

Because most of them are not text on a diff surface. `faint` is a border, `rule`
is a hairline, `lane_overflow` is a stroke — none of them has a legibility floor,
and a 1px line held to a text floor is a bright seam. The gutter is the one piece
of furniture that is *text*, on backgrounds it does not choose. See
[../theming.md](../theming.md#what-a-hairline-is-for).

## Evidence

`contrast()` over the shipped palette, before and after, in
[../measurements.md](../measurements.md#chrome-and-furniture-before-the-fix).

## Consequences

`min_furniture` is a public field like `min_contrast`, with the same
`rebuild()`-after-editing rule, and `gitten config` writes it. One more number in
the file, and one more table in the theme: one `Rgb` per `Surface`, computed once.

`a_line_number_clears_the_furniture_floor_on_every_surface` pins it, so a new
palette cannot ship a gutter nobody can read.
