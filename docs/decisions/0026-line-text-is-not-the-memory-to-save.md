# 0026 — Line text is not the memory to save

**Status** accepted
**Date** 2026-08

## Context

A diff of a large repository peaks at several times its own size in memory: the
`pr30683.diff` fixture (27 MB, 714k lines) peaked at **326 MB RSS** on the patch
path, and `cmux HEAD~120..HEAD` at 149 MB on the repository path. A counting
allocator put only 147 MB of the 326 *live* — the gap is fragmentation, and the
obvious suspect was the **714k per-line `Arc<str>` allocations**: one heap block
per line of text. The proposed fix was the classic one, an arena: hold a file's
content in a single `Arc<str>` and make each line a `(start, end)` slice into it,
turning 714k allocations into ~1,400.

## Decision

**Do not arena the line text.** The `Arc<str>`-per-line stays. The memory it
costs is not where the memory goes, and moving it makes the product's own path
worse.

## Why not the line-text arena

It was built and measured — cleanly, byte-identically (`diffcheck` agreed with
git on every count and hunk position), all tests green. Two findings killed it:

1. **It pins the file arena on the streaming path.** A `Text` slice keeps its
   whole backing buffer alive, so on the repository path — where `each_pair`
   diffs a file and drops it — a single surviving context line now holds the
   file's *entire* content resident. That is the exact memory
   [0009's streaming change](../measurements.md#acquisition-peak-streamed-vs-collected)
   exists to release: "the other 990 lines of a 1,000-line one-line fix are
   garbage the moment the differ has run." The arena un-frees them. `cmux
   HEAD~120..HEAD` went **149 MB → 192 MB, +29 %** — a regression on the primary
   path, which is the desktop window opening a repository.

2. **Line text was never the fragmentation.** Removing 714k line-text
   allocations (parse: 728k → 14k) recovered only ~19 MB of RSS on the patch
   path. The dominant cost is the **~1.06M per-line `Box<[Token]>` and
   `Box<[Span]>` allocations in `prepare`** — one pair of boxes per line, live
   for the whole diff. That is where the 180 MB live→RSS gap comes from, and the
   text arena does not touch it.

So the change traded a 6 % win on the secondary path (patch) for a 29 % loss on
the primary one (repository), to attack an allocation source that was not the
problem. A clean implementation of the wrong idea.

## Evidence

`pr30683.diff --patch`, and `cmux HEAD~120..HEAD` for the repository path, via
`/usr/bin/time -l` and a counting global allocator. Full numbers in
[measurements.md](../measurements.md#why-the-line-text-arena-was-reverted).

## Consequences

Line text keeps one `Arc<str>` per line, shared unchanged from acquisition to
screen — the property the pipeline already rests on, and the reason a visible row
is a refcount bump and not a copy.

**What would make us revisit — and what to aim at instead.** The real target is
the token and span storage: one arena of `Token` per file with a per-line range,
replacing ~1M `Box<[Token]>`/`Box<[Span]>` with ~1,400 `Vec`s. That has no
streaming-pin problem, because tokens are built in `prepare` and never held
against an undropped acquisition buffer — and it attacks the allocation source
this record identified as dominant. It is a larger change than the text arena
(every renderer reads tokens and spans), so it wants its own decision record and
its own before/after, measured the way this one was rather than assumed. The
lesson this record exists to carry: **measure which allocation is the cost before
migrating it, and check the streaming path, not only the patch path.**
