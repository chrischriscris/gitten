# 0001 — Histogram diff, not Myers

**Status** accepted
**Date** 2026-08

## Context

Git defaults to Myers. A diff viewer's whole job is making a change legible, and
the algorithm decides what "the change" even looks like.

## Decision

`imara-diff`, Histogram. Diffing is a `trait Differ`; Histogram and Myers are the
first two implementations and the view never calls a differ directly.

## Why not Myers

Histogram anchors on lines that appear exactly once in both sides. In source code
those are function signatures and declarations, so a moved block reads as a move.
Myers dissolves the same block into line-soup — alternating adds and deletes that
happen to be minimal and are unreadable.

## Evidence

Qualitative, and deliberately so: the failure is legibility, not throughput. Any
real repository with a moved function shows it.

## Consequences

Diverging from git's default means a diff here can differ from `git diff` on the
same commit. That is intended; both are correct, one is easier to read.

Semantic and language-aware differs arrive as extensions behind the same trait.
