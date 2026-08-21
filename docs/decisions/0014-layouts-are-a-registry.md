# 0014 — Side-by-side is a registered layout, not a second view

**Status** accepted
**Date** 2026-08

## Context

Unified and side-by-side are the two ways everybody expects to read a diff. The
obvious implementation is a `Diff` view with a `mode` field and two branches
through `render`, and it is wrong for the same reason a `match` on file type
would have been wrong in [0006](0006-row-seam-without-boxing.md): the second
presentation is not the last one, and the third cannot be added without editing
the first two.

The `Rows` seam already existed and already chooses a presentation per file. What
it could not express is a presentation of the *whole diff*.

## Decision

**A `Layout` is a name and a closure that builds a set of `Rows`
implementations.** `Layouts` is the registry, `unified` and `split` are its two
entries, and `s` cycles them. `unified` builds `[TextRows, MarkdownRows]`;
`split` builds `[SplitRows]`, which claims every path.

**Which line sits opposite which is `core`'s decision, not the renderer's.**
`align::align` returns one `Slot` per row, and `replace_pairs` — which the
intraline pass uses — is a filter over the same function.

**The startup layout is `host.layout`, a `String`.** The registry cannot be on
`Host`, because a `Rows` implementation returns an `AnyElement` and `core` never
knows a UI exists. The *choice* out of it is data, so it can be, and `[diff]
layout` in `plait.toml` sets it.

## Why the pairing has to be shared

`replace_pairs` matches a run of N removals to the M additions after it, by
position, and the intraline pass computes changed words from those pairs. A
side-by-side view that paired differently would draw a removal beside an addition
whose highlighted fragments were computed against a *different* line — words lit
up that correspond to nothing on the row. There is one function and both callers
use it, which is why `align` is in `core` beside `replace_pairs` rather than in
the renderer that wanted it.

## Why not two views, or a pane each

A view owns a scroll position, a focus handle and a session key. Two of them
means two of each, and switching means throwing away the one you were reading —
including its position, which `session.rs` exists to preserve. A layout is one
view with different rows.

It also would not have proved anything. `SplitRows` is 300 lines and needed no
new trait, no new argument and no edit to `TextRows`; that is the only test of a
seam that counts, and a second view would have skipped it.

## Why one column width for the whole diff, and not per viewport

Both columns are as wide as the widest line anywhere in the diff, so the divider
is one straight vertical line from the first row to the last. Per-file widths
move the divider as you scroll, and a boundary that drifts is worse than one that
is too far right.

Half the viewport would be the familiar answer and `uniform_list` cannot give it:
horizontal scrolling needs `flex_none` rows with an intrinsic width, so a row
cannot be told how wide the window is. Clipping to a fixed column instead would
lose text that unified mode shows, which is worse than scrolling.

## Why `s` is hardcoded

There is no keymap. [0012](0012-config-is-data-behaviour-is-not.md) says a
binding names a command and the command lives in code, and command dispatch is
listed in [../architecture.md](../architecture.md) as not built. This is the
first real action in the application and it is shaped like the last one will be:
the view owns a focus handle, the binding is global, the handler is a method. When
dispatch lands, this becomes a named command it can reach.

`s` is worth revisiting when it does: lazygit uses it for *stash*, and this
project is explicitly heading for lazygit's keyboard model. It is one line to
change and the point of hardcoding it now is that it is one line.

It is also no longer the only route. There is a picker in the title bar, which is
discoverable in a way a hidden key is not — see
[0015](0015-title-bar-controls-are-hand-rolled.md).

## Evidence

`./check.sh`, the `diffs` section. The alignment pass costs **4–10 ns a row** and
3.1 ms on the 714k-line fixture — nothing, next to a `prepare` that is 247 ms on
the same input.

Rows saved is the number that says whether the presentation is worth having, and
it depends entirely on the diff: **81%** of the unified row count on the two
edit-heavy fixtures, **100%** on the two that are near-pure addition or deletion.
A diff with nothing replaced has nothing to pair, and side-by-side gives it two
half-empty columns. Full table in [../measurements.md](../measurements.md).

## Consequences

**Switching rebuilds the rows**, which re-runs `prepare`: 8 ms on a typical diff,
247 ms on the pathological fixture, once, on a keystroke. The parsed diff is kept
alive to make that possible, which on the 714k-line fixture is a second copy of
every line held for the life of the window. Cloning the *prepared* diff instead
would pay the same memory plus the clone at load whether or not anybody ever
presses the key.

Making it instant means the row implementations sharing their text behind a
refcount instead of owning it, which is a change to `prepared::Line` and not to
the view. That is the lever if it ever matters.

**The reading position is preserved proportionally, not exactly.** The two
layouts do not have the same number of rows — that is the point — so a row index
means something different in each and there is nothing exact to preserve.

**`split` has no Markdown specialist.** A rendered document in a 44-character
column is worse than its source, and two columns are already the answer to "show
me both versions". Registering one is two lines if that turns out to be wrong.

**Row count is presentation-dependent now.** `docs/extending.md` says row count is
not a presentation's to change, and that still holds *within* a column — the
gutter shows both line numbers and they have to keep adding up. A second column
is a different claim.
