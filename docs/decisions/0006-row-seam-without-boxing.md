# 0006 — Row presentation behind a trait, 8 bytes a row

**Status** accepted
**Date** 2026-08

## Context

The diff view flattened every file into a closed `enum Row` and drew it with a
`match`. A different presentation for one file type — a rendered Markdown diff, an
image diff — meant editing that enum and that match.

Rule 1 says an extension must be able to do what a built-in does. Rule 3 says
nothing on the render path allocates per frame. The obvious fix breaks the second:
`Vec<Box<dyn Row>>` is one allocation per row, and the deletion fixture has
714,000 rows.

## Decision

`trait Rows` claims paths, owns its rows, and draws them by index. The list keeps
an order table of

```rust
struct RowRef { owner: u16, index: u32 }   // 8 bytes
```

`TextRows` is the built-in and claims every path, which is what makes it the
fallback. `Diff::with_renderers` takes the list; the last claimant wins.

## Why not box each row

714k allocations to build and to chase on every scroll, against 5.7 MB of flat
index table. The indirection also lands exactly where it hurts most — the render
path — for no expressive gain.

## Why row height stays fixed

`uniform_list` builds only visible rows and is the only reason a 714k-row diff
scrolls at all. Variable height means giving that up. So an implementation draws
anything it likes within `ROW_H` and cannot ask for more.

## Consequences

The presentations this seam *cannot* express are the ones needing their own
layout: a rendered Markdown preview, side-by-side images, a graph inline in a
diff. Those want a pane, and panes do not exist yet — the shell puts one view in
the window. When they arrive, that is a second, coarser seam, not a change to this
one.

Two tests keep this honest: a specialist claiming `.md` and collapsing a file to
one row, and everything falling back when nobody claims.
