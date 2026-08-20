# 0004 — Markdown as a second `Highlighter`

**Status** accepted
**Date** 2026-08

## Context

Markdown was in the first batch of language tables. Measured against tree-sitter
it mis-coloured 21.8% of every file — the worst result of any language, by a
factor of two.

## Decision

Markdown is a separate `Highlighter` implementation, routed by
`Highlighters::route(&["md", "markdown", "mdx"], Markdown)`. It walks lines, not
bytes: headings, fences, blockquotes, list markers, then inline code, emphasis and
links.

## Why not a better table

The model is wrong, not the table. Prose has no keywords, an apostrophe is not a
string, and what is worth colouring is structure — none of which a
comment/string/keyword scanner can express. Every table that got closer on
headings got worse on prose.

## Why not tree-sitter for it

Would have worked, and remains the option if inline HTML or code-block injection
matters later. It was not worth 1.55 MB of engine plus a grammar for a file type
that appears a handful of times in most diffs, when 100 lines cover it.

## Evidence

21.81% bleed with a table, worst contiguous run 25,701 bytes. See
[../measurements.md](../measurements.md).

## Consequences

The `Heading`, `Strong`, `Emphasis` and `Link` kinds exist in `core` rather than
inside this implementation, so a theme can style them without knowing which
highlighter ran.

This is also the built-in proof that the routing seam is real: something ships
through the same call an extension would use. If that stops being true, the seam
is untested.
