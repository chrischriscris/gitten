# 0001 — Histogram diff, not Myers

**Status** accepted; implementation superseded by
[0013](0013-differs-in-core-not-a-dependency.md)
**Date** 2026-08

## Context

Git defaults to Myers. A diff viewer's whole job is making a change legible, and
the algorithm decides what "the change" even looks like.

## Decision

Histogram. Diffing is a `trait Differ`; the view never calls a differ directly.

This originally said *`imara-diff`, Histogram* and named Histogram and Myers as
its first two implementations, describing something that had not been built —
`plait-git` ran `git diff` and the unified output was parsed back. The trait and
the algorithms now exist, written out in `core` rather than pulled in, and there
are three of them. [0013](0013-differs-in-core-not-a-dependency.md) is why, and
is also the record of a doc describing an intention being read as a description
for two weeks.

## Why not Myers

Histogram anchors on lines that appear exactly once in both sides. In source code
those are function signatures and declarations, so a moved block reads as a move.
Myers dissolves the same block into line-soup — alternating adds and deletes that
happen to be minimal and are unreadable.

## Evidence

Qualitative on the thing that decided it: the failure is legibility, not
throughput, and any real repository with a moved function shows it. There is a
test — `histogram_reads_a_moved_block_as_a_move` — that pins the shape.

Quantitatively, both are now checked against git on real history; see
[0013](0013-differs-in-core-not-a-dependency.md) and
[../measurements.md](../measurements.md).

## Consequences

Diverging from git's default means a diff here can differ from `git diff` on the
same commit. That is intended; both are correct, one is easier to read.

Semantic and language-aware differs arrive as extensions behind the same trait.
