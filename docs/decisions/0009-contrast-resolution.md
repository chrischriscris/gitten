# 0009 — Resolve foregrounds against their background

**Status** accepted
**Date** 2026-08

## Context

The palette had one colour per token class, chosen against the near-black context
background. A diff draws on five different backgrounds, and the two for changed
words are much lighter. Measured, the comment grey against the added-word
background was **1.15:1** — a grey smear on green, which is how a screenshot found
it. WCAG AA for body text is 4.5.

## Decision

`Theme` carries a contrast floor and resolves every token class against every
`Surface` once, in `rebuild()`, into a `12 × 5` table. `readable(fg, bg, target)`
returns `fg` untouched if it already clears the floor, otherwise blends it toward
white — or black on a light background — in 24 steps until it does.

The changed-word backgrounds were darkened at the same time: `#2c5c33` → `#1e3a23`
and `#6b2f2a` → `#43201a`.

## Why blend rather than pick a second colour

Blending keeps the hue, so a lifted comment is still the same grey-brown, just far
enough off the background to read. Themes then only have to be tasteful; they never
enumerate a colour per surface, and a theme written for one palette cannot be
illegible on another.

## Why 3.5 and not 4.5

A diff wants its comments to recede. Lifting them to 4.5 on the changed-word
background gives `#cac8c5`, which is louder than the code around it. 3.5 is
legible and still quiet. It is one public field.

## Why darken the backgrounds too

Because lifting alone was not enough: at the old background the comment had to go
to `#b7b3b0` to clear 3.5, which made it the brightest thing on the line. Darker
background, gentler lift, `#8f8a84`.

## Why precomputed

`readable` costs six `powf` per call and `render` asks for a style per run per
visible row per frame — roughly 500 calls a frame. Resolution happens once; render
is one array index.

## Consequences

`syntax`, `diff` and `min_contrast` are public fields, so **`rebuild()` is
required after editing them directly**. `set_syntax` does it. The type system
cannot enforce it without closing the struct, and open fields are worth more.

`every_token_is_legible_on_every_surface` asserts the floor for all 60
combinations, so a new palette cannot ship illegible.
