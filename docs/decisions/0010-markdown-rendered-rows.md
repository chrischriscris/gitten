# 0010 — Markdown renders as rows, and the markers come off in `core`

**Status** accepted
**Date** 2026-08

## Context

A `.md` file's diff was shown the way git shows it: the characters, with prose
colours on them. `## `, `**`, `` ` `` and every URL sat in the text at full
weight. That is the source, and reading the source is what the tool is for — but
it is not what a `.md` file is, and a rendered presentation was the first thing
[0006](0006-row-seam-without-boxing.md) named as the reason the `Rows` seam
exists.

Three things had to be decided: what "rendered" can mean inside a fixed row
height, where the marker removal lives, and how it finds the markers.

## Decision

### Rendered *rows*, not a rendered document

`uniform_list` requires one height for every row and is the only reason a 714k-row
diff scrolls, so a heading gets bigger and not much bigger, a blank line still
costs a whole row, and a fenced block cannot have a block background. Three
devices do the work within that:

| device | for | why not the alternative |
|---|---|---|
| row font size | headings | `HighlightStyle` has no font size — see below |
| a left bar | fenced blocks, quotes | a background means added/removed, and a diff may not give that up |
| furniture: glyphs, a rule | the markers that were removed | a bullet as text would become a run in the merge and take the text's colour |
| space padding | table columns | see *Tables* below — an element per cell costs the render path |

**The size constraint is not taste, it is GPUI.** `HighlightStyle`
(`gpui/src/style.rs:580`) carries colour, weight, slant, background, underline,
strikethrough and fade — and is documented as *"a single font, uniformly sized and
spaced text."* There is no font size on a run. So size is settable per row and
never within one, which is the whole reason headings scale at the element level
and inline markup varies only in the other properties. The scale tops out at 18px
because a glyph needs roughly 1.2× its point size of line box and `ROW_H` is 22px;
past that a heading clips into its neighbour. Levels 4–6 land on the body size and
separate by weight, which is what most typographic scales do at that depth anyway.

A *preview* — reflowed, variable height, tables as tables — still wants a pane,
and panes still do not exist. 0006 stands; this is the row-shaped half of it.

### Marker removal lives in `core`

`core::markdown::lay_out(&mut [prepared::Line]) -> Vec<Block>`. It returns what
each line structurally is and rewrites each line so the markers are gone and every
token and span still indexes it. The shell turns a `Block` into pixels;
`examples/paint.rs` turns the same `Block` into escape codes and contains no
markdown logic. That is the boundary check from
[0007](0007-assembly-in-core.md), run again.

`theme::MarkdownPalette` for the same reason `Kind::Heading` is in `core` rather
than inside the highlighter that emits it — a theme has to be able to style the
bar down a blockquote without knowing which frontend draws it.

### The markers are found from the tokens, not by parsing

This is the part worth reading twice. By the time a row reaches `lay_out`, the
`Markdown` highlighter has already emitted a `Strong` token over `**word**`, a
`Link` over `[text](url)`, a `Str` over `` `code` `` — *delimiters included*,
because a token is a range of the source. So the set of bytes to hide is derivable
from ranges that already exist, by looking at the bytes at each end of each token.
No second scan of the line, and no parser.

Deriving it from the *bytes* rather than from the token's provenance is what keeps
it correct if `.md` is routed elsewhere: a tree-sitter highlighter puts `Strong`
over different ranges, and the check is "does this token begin with two
asterisks", so the answer stays right. A `Strong` token wearing no delimiters
keeps every byte it has. There is a test for exactly that.

Removal is deletion only — never a replacement, never an insertion — which is why
it is `String::drain` back to front over the buffer the line already owns. The pass
allocates nothing per row. A bullet's `•` is furniture the renderer draws, not a
byte spliced into the text, and that is why: an insertion would have made the
remap two-directional for one glyph.

Ranges are moved *before* the text is cut, because both have to be in the same
coordinates while the mapping is computed. Get that backwards, or cut before
remapping, and a token indexes bytes that are gone — a panic in a debug build,
mojibake in release. Same failure mode as the clip ordering in 0007, same kind of
test pinning it.

### Tables align by padding, not by layout

A table is the one construct whose cells must line up with the rows *around* them,
so it is the one place a line cannot be laid out on its own. Two ways to do it:

| | space padding | an element per cell |
|---|---|---|
| render path | one `StyledText`, unchanged | N `StyledText`s, tokens sliced per cell |
| fonts | monospaced only | any |
| the remap | needs insertion | unchanged |

Padding wins because the render path is the thing rule 3 protects and this
presentation's whole claim is that it costs nothing per frame. The shell sets
`font_family("Menlo")`, so a space is a column; `Layout::monospaced()` is the
frontend stating that, and `Layout::proportional()` leaves tables verbatim rather
than misaligning them by a fraction of a glyph per cell. `core` cannot see a font,
so it must be told.

The price is that the remap needs *insertion*, which `apply` — deletion-only,
in place, allocation-free — cannot express. So tables get `remap`, the general
piecewise-linear form, and a new `String` per table row. Both exist on purpose:
tables are 1–2.5% of changed lines, and paying for generality on every row of a
71k-row diff to serve 2% of them would be the wrong trade. Measured cost of the
whole pass with alignment: **70–100 ns a row**, against 70–90 without.

Column widths are measured per *run* — a maximal stretch of table rows — and per
hunk side, so two tables either side of a paragraph get two grids and a removed
row is never widened by an added row's long cell. A hunk that shows the middle of
a table has no header and no separator; it aligns to what is on screen, which
beats refusing to align.

The runs and their measurements leave this pass in a `Tables`, and the reason is
the one thing about a table that is not knowable at load: whether it fits the
window. When it does not, the grid is laid out again at reflow — squeezed columns,
cells wrapped inside them — off the same runs and the same measurements, because
re-deriving "which rows are one table" at the other end is a second answer to it
and two answers drift. See `docs/decisions/0017` and stage 4c of the pipeline.

**The sharp edge, and it drew blood.** `for_each_side` hands a *context* row to its
caller twice, because a context row belongs to both sides. The token pass does not
care — it assigns `out[row]`, and assigning twice is assigning once. This pass
mutates, and padding an already-padded row is not idempotent: the first pass wrote
a `│`, the second measured the grid it had just drawn and then panicked splitting
that three-byte glyph at byte 1. The fix is structural rather than a guard —
measure every grid first, rewrite each row exactly once, added side winning for
context rows. `for_each_side` now documents the requirement, and two tests pin it.

## Why not a CommonMark parser

`markdown 1.0` is already in the tree (`gpui-component` depends on it), so this
was a real option at zero build cost, and it was the wrong shape twice.

**`core` takes no dependencies.** That is rule-shaped, not a preference.

**A hunk is not a document.** It hands you the middle of a list and half of a
fenced block, constantly, and it interleaves two documents that must be parsed
separately — the removed side and the added side. `fixtures/real/md.diff` is 229
files and 71,705 markdown rows and there is no point in it where a CommonMark parse of the
rows as given would be correct. Everything stateful here already has to run per
hunk side, which is why `syntax::for_each_side` is now one shared implementation
that both the token pass and the block pass call: if they split a hunk
differently, a fence would open on one and not the other and the two would
disagree about the same line.

And there would be nothing to spend the parse on. The tokens locate the markup
already; a parser would re-derive it and then need mapping back onto per-line byte
ranges, which is where the cost and the bugs both are.

## Evidence

`./check.sh`, release, M1 Pro. Two real markdown shapes, because they are not
alike — see [../measurements.md](../measurements.md):

| fixture | rows | files | prepare | `lay_out` | per row |
|---|---|---|---|---|---|
| `md.diff` (rust-lang/book, 80% paragraph) | 71,705 | 228 | 90.7 ms | 5.1 ms | 71 ns |
| a technical-docs tree (34% paragraph, 12% heading) | 75,684 | 1,019 | 16.6 ms | 6.5 ms | 86 ns |

**70–100 ns a row, at load, once.** The share of `prepare` is 6% on one and 38% on
the other, which says nothing about this pass and everything about `prepare`:
prose is edited sentence by sentence, so the book fixture is 72 ms of intraline
against the technical-docs one's 1 ms. Quote the per-row figure.

Nothing on the render path changed. A row is still one `StyledText` and one run
list through the same merge; every markdown-specific decision is a `Copy` field
read out of a `Vec`.

## Consequences

**Inline markup inside a heading keeps its markers.** The highlighter marks a
heading as one whole-line `Heading` token and never scans inside it, so those
delimiters are not located and there is nothing to cut. Locating them means
splitting that token around them, and tokens must stay sorted and
non-overlapping. Measured on both fixtures: 21.3% of the book's headings carry
inline markup and 3.4% of the technical-docs tree's — inverse heading counts
landing in the same place, **under half a percent of changed rows either way**.
That is the figure that decided it. A test pins the behaviour so it is a known
quantity rather than a surprise.

**Emphasis that spans a line break keeps its markers**, for the same reason and a
better one. The highlighter matches a delimiter run within one line and leaves an
unclosed one as text — deliberately, because in a diff a line often *is* half a
construct and an unmatched `*` is far more likely to be a bullet than the start of
emphasis. So an unpaired run is never located and never cut. Measured at 0.05% of
changed lines in the book fixture and 0.17% in the technical-docs one. Guessing
would cost more than it buys: the failure mode is a whole paragraph drawn as
emphasis because one asterisk was a footnote.

**A fenced block is not syntax-highlighted as its language.** The fence knows it
said `rust` and the row draws it as one `Str`. Doing better means injection — the
`Markdown` highlighter routing its fence bodies back through `Highlighters` — which
[0004](0004-markdown-second-highlighter.md) already named as the thing
tree-sitter would be for. It is the obvious next lever and it is a change to the
highlighter, not to this.

**Blank lines cost a full row.** 9% of the book fixture and 30% of the
technical-docs one. Fixed row height, and not fixable here.

**An indented code block is not recognised as code.** `classify` knows fences and
nothing else, so a four-space block is prose — its contents get emphasis, links
and list markers interpreted, and its `# comments` were read as headings until
`heading_level` started counting the indent. Detecting it properly is not a
one-liner: within a hunk a four-space line is equally likely to be a list
continuation, and `Block::Ordered` already claims `indent >= 2` for exactly that
case, so the two rules would have to be arbitrated with a blank line that may not
be in the hunk at all.

Fences are unambiguous, so the answer for now is to use them — this repository's
own docs had exactly one indented block left and it is the one that surfaced the
heading bug. There is a scan for them in the git history of this file if a future
one creeps back in.

**A new fixture.** Every other diff fixture is code, and prose is a different
distribution: `fixtures/fetch.sh` now builds `md.diff` from rust-lang/book. It
turned out to be the heaviest intraline case in the set as well — 72 ms of a
91 ms `prepare`, against 58 ms for `pr30698.diff`, which had been the fixture
chosen for that.
