# 0013 — The diff algorithms are written in `core`, not pulled in

**Status** accepted
**Date** 2026-08

Supersedes the *implementation* half of
[0001](0001-histogram-not-myers.md); its conclusion — Histogram, not Myers —
stands unchanged.

## Context

[0001](0001-histogram-not-myers.md) said "`imara-diff`, Histogram. Diffing is a
`trait Differ`." Neither half was true. There was no trait, `imara-diff` was
never in the lock file, and nothing in the codebase diffed anything: `plait-git`
ran `git diff` and `parse_unified_diff` read the unified output back.

That is a working diff viewer and a dead end. *Git* was choosing the algorithm,
so the choice was not ours to expose, and a semantic or language-aware differ
could not have existed — which makes rule 1 false for the single most important
thing this application does.

Fixing it forces two questions at once. Where does the algorithm live, given that
`core` may have no dependencies? And what does acquisition hand it, given that a
finished unified diff has already been diffed by somebody else?

## Decision

**`trait Differ` in `core`, with the algorithms written out.** Three of them:
`Histogram` (selected), `Patience`, `Myers`. An implementation returns only the
edit script; line numbering, context and hunk headers are `differ::hunks`, shared
by all of them.

**`plait-git` acquires blob pairs, not diffs.** One `git diff --raw -z -M
--abbrev=64` names every changed path and both object ids; one `git cat-file
--batch` streams every blob. Two processes, whatever the file count. `core`
decides which lines correspond, afterwards.

`parse_unified_diff` stays, for reading `.diff` fixtures off disk. That is a
different job and it is now the only caller.

## Why not a crate

`imara-diff` is the obvious answer and `core/Cargo.toml` is the reason it is not
available: an empty `[dependencies]` is the architectural rule, not a
housekeeping preference. A wrapper crate outside `core` would work and would put
the trait and its only real implementations on opposite sides of a boundary, so
that the shipped configuration reaches around the seam rather than through it —
the failure [0004](0004-markdown-second-highlighter.md) exists to avoid.

Written out, the whole module including tests is smaller than the wrapper would
have been, and `cargo test -p plait-core` still runs in 0.6 s.

## Why not keep git as the differ and expose its flags

`--histogram`, `--patience`, `--minimal` are one line of plumbing and would have
given the user the same three choices. They would also have made the choice a
match arm on git's command line: an extension could never add a fourth, which is
rule 1 failing on exactly the axis this feature is about.

It is also slower. Ours is 8–20× faster than the process it replaced — not a
reason on its own, and not nothing on a 310k-line diff.

## Why blob pairs rather than a hybrid

A `Differ` needs two texts, and `git diff` will not give them. Keeping both
paths — git's output when the algorithm is git's, blobs when it is ours — means
two acquisition layers with different bugs, and a rename or a submodule handled
correctly in one and not the other. Both of those cost a real bug during this
work (see Evidence); having them to find twice would have been worse.

The object ids are the reason to want it this way round anyway: a blob's content
never changes, so a diff keyed on the pair of them is cacheable forever. That
cache is not built yet and this is what makes it possible.

## Evidence

`./check.sh`, the `differs vs git` section, which runs
`git/examples/diffcheck.rs` against four repositories. A minimal edit script has
exactly one length, so **Myers must match `git diff --minimal` exactly** — and
does, on every input tried, including this repository's entire history in one
diff. Full table in [../measurements.md](../measurements.md).

Two bugs it caught that reading did not:

- **An anchor scored by its most common line instead of its rarest.** Backwards,
  and plausible either way round in prose. It cost 582 spurious changed-line
  pairs on one 690-line file: a long run of unique code lost to a one-line run
  the moment a single `}` fell inside it.
- **A null object id read the same on both sides.** On the new side it means "in
  the working tree"; on the old side it means "the file did not exist". Conflated,
  every added file diffed against itself and showed no change at all.

Both were invisible in the totals until the check reported per-file deltas, which
is the argument for the check existing.

## Consequences

**Hunk boundaries can differ from git's by one.** Git runs `--indent-heuristic`
by default, which slides a hunk to a more readable equivalent position. Ours does
not, so the hunk *count* occasionally differs while the changed-line count does
not. The check deliberately does not compare offsets: that would measure a
preference and report it as a bug. The heuristic is a good future addition.

**A missing trailing newline is invisible.** Content is split into lines, and
`a\nb\n` and `a\nb` produce the same list, so git's `\ No newline at end of file`
has nowhere to live. The old parser had the same gap; it is now in a place where
it could be fixed, which needs a per-side flag on `Pair` and somewhere in
`DiffLine` to put a note.

**Both algorithms degrade rather than stall.** Myers is O((N+M)D) and Histogram
falls back to it on any region with nothing rare to anchor on, so a fully
rewritten generated file is bounded by `MAX_STEPS` (40 million) and past it the
region is reported as replaced. The same trade as `MAX_INTRALINE_TOKENS`, for the
same reason.

**Recursion is an explicit stack.** A file whose every anchor peels off one line
recurses as deep as the file is long, which is a stack overflow rather than a slow
load, and generated code has exactly that shape.

**A blobless clone fetches on demand.** `cat-file --batch` triggers the promisor
fetch, so the first diff of a range in `~/Projects/git` took 15 s of network and
42 ms once local. `git diff` in the same repository does the same thing.

What would make us revisit: a differ that wants a whole file's syntax tree rather
than its lines. The trait takes `path` for exactly that reason, but an
implementation needing the *blob* rather than the split lines would want
acquisition to hand it one.
