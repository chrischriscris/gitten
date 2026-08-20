# 0003 — A table-driven scanner, not tree-sitter

**Status** accepted
**Date** 2026-08

## Context

The diff view needed syntax highlighting. `gpui-component` — already a dependency
— ships a complete tree-sitter highlighter behind per-language features, and
`syntect` is already in the lock file via `gpui-base`. Both were free to adopt.

## Decision

A single-pass byte scanner in `core`, with all language-specific facts as data in
a `Syntax` table. Highlighting is a `trait Highlighter`, so this is one
implementation rather than the mechanism.

## Why not tree-sitter

Three reasons, in order of how much they mattered:

1. **A diff hands you fragments, which is the input a parser is worst at.** On
   hunk-shaped input tree-sitter drops from 7.1 to 2.6 MB/s *and* loses a fifth of
   its spans to error recovery. The scanner does not move.
2. **Throughput.** 7.1 MB/s against 104–262. On the 714k-line fixture that is the
   difference between a third of a second and several.
3. **Weight.** 1.55 MB of shared engine plus 0.2–3.3 MB per grammar, a C toolchain
   in the build, and query compilation of up to 21 ms per language.

## Why not syntect

0.4 MB/s with fancy-regex, ~0.8 with Oniguruma, +2.0 MB of syntax dumps and
+15 MB RSS. Being already in the dependency tree does not make it fast.

## Evidence

Full tables in [../measurements.md](../measurements.md). The number that decided
it: **2.6 MB/s and 114 spans/KB on fragments, against 7.1 MB/s and 143 on whole
files.**

## Consequences

The scanner colours 40–67% of bytes where tree-sitter manages 66–89%, and has no
semantic classes at all: a call is a name before `(`, a type is a capitalised
word. There is no scope, no resolution, no injection.

Markup is where the model genuinely breaks — html, php, markdown — and those get
no table rather than a wrong one.

**What would make us revisit:** anything needing real structure. Symbol-aware
navigation, folding, semantic move detection. The trait is the door, and a
tree-sitter implementation can take exactly the languages the scanner cannot do,
in its own crate, without `core` gaining a dependency. See
[0004](0004-markdown-second-highlighter.md) for that shape already in use.
