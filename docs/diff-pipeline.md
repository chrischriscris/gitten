# The diff pipeline

From two blobs to pixels, in six stages. Five of them are in `core` and have no
idea a window exists.

```
  1  acquire     plait_git::pairs                    Vec<Pair>   two texts per file
  2  diff        Differs::file                       Vec<FileDiff>
       2a normalise  Whitespace::keys                how much whitespace must match
       2b script     Differ::diff, per file       ── the seam: histogram, myers, …
       2c compact    differ::compact                 slide each change to a boundary
       2d hunks      differ::hunks                   context, line numbers, headers
       2e moves      differ::moves + mark_moved      deleted here, added there
  3  prepare     prepared::prepare                   Vec<prepared::File>
       3a clip        every line, to a column budget
       3b intraline   changed words, per replace-pair
       3c syntax      tokens, per hunk side
  4  build       Rows::build, per file               renderer-owned rows
       4a layout      markdown::lay_out, if the renderer asks
       4b align       align::align, if the renderer asks
       4c wrap        wrap::Wrapped, per width     ── the seam: word, char, off
  5  order       Vec<RowRef>                         8 bytes per row
  6  draw        Rows::render + Theme                one StyledText per row
```

Stages 1–5 run once, at load, and again from stage 3 when the layout changes.
Stage 6 runs per visible row per frame and is the only one on the render path.

**Stages 4c and 5 also run again on every resize** that crosses a character
boundary, and nothing above them does — see [4c](#4c-wrap-for-a-window-that-is-a-certain-width-today).
That split is the whole reason wrapping is viable: stage 3 is 8–247 ms and
depends on nothing about the window; 4c is 1–26 ms and depends on nothing else.

Stages 4a, 4b and 4c are indented because they are not part of the pipeline
everything goes through: each runs only for the renderer that asks for it —
`MarkdownRows` wants the block structure, `SplitRows` wants the alignment, and
all three built-ins want the wrap.

## 1. Acquire

Two lists of lines per changed file, and nothing more:

```rust
pub struct Pair {
    pub path: String,
    pub old_path: Option<String>,   // Some when renamed
    pub status: char,               // git's --raw letter: A M D R C T
    pub old: Vec<String>,
    pub new: Vec<String>,
    pub binary: bool,
}
```

Two processes, whatever the file count. `git diff --raw -z -M --abbrev=64` names
every changed path and both object ids; `git cat-file --batch` streams every blob
in one go. A `git show` per file would be a `fork` per file, which on a
thousand-file diff is a second before any work happens.

Four things in that command line are load-bearing:

- **`-z`**, because a path may contain anything a filesystem allows, and without
  it git quotes and escapes them. It also means records run together with no
  newline between the previous path's NUL and the next record's `:`.
- **`-M`**, so a rename arrives as one file with two names instead of a delete
  and an add of an identical blob.
- **`--abbrev=64`**, which looks like a no-op and is not. `--raw` abbreviates
  object ids by default, and `cat-file --batch` echoes back the *full* id in its
  response header — so an abbreviated request cannot be matched to its answer. 64
  is clamped to whatever the repository's hash length actually is, which makes it
  right for SHA-256 repositories too.
- **`--no-ext-diff`**, because a user's `diff.external` would replace the output
  format entirely.

Writing the request list to `cat-file` happens on another thread. It answers as
it reads, so a large enough request fills the pipe git is writing into while this
process is still filling the pipe git is reading from, and both block forever. A
thousand-file diff is two thousand object ids, so it is not a rare shape.

Three cases that are not a blob to be fetched:

- **A null object id** — all zeros, at whatever width. On the *new* side it means
  "not in the object database", which for a working-tree diff is the ordinary
  case: read the file from disk. On the *old* side it means the file did not
  exist. Conflating them makes every added file diff against itself and show no
  change at all, which is a bug that looks like a working diff.
- **A gitlink** (mode `160000`) is a commit in *another* repository. There is
  nothing here to fetch, so the content is synthesised as git synthesises it:
  `Subproject commit <oid>`.
- **A partial clone.** `cat-file` triggers the promisor fetch, so the first diff
  of a range in a blobless clone is network-bound — 15 s against 42 ms once local.
  `git diff` there does the same thing.

Never `read_to_string`. Git guarantees no encoding, real history carries Latin-1
author names, and `git/git` has commits that are not valid UTF-8 at all. Read
bytes and `String::from_utf8_lossy`; never fail to show a repository over one bad
byte.

A trailing newline terminates the last line rather than starting an empty one,
which means `a\nb\n` and `a\nb` produce the same list and git's `\ No newline at
end of file` has nowhere to live. A gap, not a design.

`parse_unified_diff` still exists and is now only for reading `.diff` fixtures off
disk — a file that has already been diffed by somebody else, which is why it
cannot test the thing that diffs.

## 2. Diff

Which lines correspond is a judgement, so it is a seam:

```rust
pub trait Differ {
    fn name(&self) -> &'static str;
    fn diff(&self, path: &str, old: &[&str], new: &[&str]) -> Vec<Edit>;
}
```

An implementation produces **only the edit script**. Everything else in stage 2 is
shared, and deliberately: it is identical for every algorithm, it composes with an
extension's, and a second copy of any of it is a hunk header that quietly
disagrees with the lines under it.

Three built-ins, all in `core` because `core` may have no dependencies and that
is the rule rather than a preference — see
[decisions/0013](decisions/0013-differs-in-core-not-a-dependency.md):

| name | what it does |
|---|---|
| `histogram` | anchors each region on its rarest common line, recurses either side, falls back to `myers` where nothing is rare. **Selected.** |
| `patience` | the same, with the rarity threshold at one |
| `myers` | the minimal edit script, middle-snake, linear space |

Two things about the anchored pair are worth knowing before touching them, both
because they are wrong in the plausible direction:

- **A run is scored by its *rarest* line, not its most common one.** Backwards,
  and it reads fine either way round: score by the most common and a
  four-hundred-line run of unique code loses to a one-line run the moment a
  single `}` falls inside it. Measured on this repository, the wrong way round
  cost 582 spurious changed-line pairs on one 690-line file.
- **The threshold tightens as the search runs.** Once a run scoring 2 is in hand,
  a line appearing forty times cannot be part of anything better. Not only an
  optimisation: without it a *longer* run wins on length alone however common its
  lines are, and this repository's whole history came out 10 changed lines worse
  than `git diff --histogram` over 9,958.

Both algorithms are worst-case quadratic in the number of *differing* lines, so
both are bounded by `MAX_STEPS` (40 million, tens of milliseconds) and degrade to
"this region was replaced" past it. The same trade as `MAX_INTRALINE_TOKENS`, for
the same reason. Recursion is an explicit work stack, not the call stack: a file
whose every anchor peels off one line recurses as deep as the file is long, which
is a stack overflow rather than a slow load, and generated code has that shape.

Correctness is checked against git rather than argued.
`git/examples/diffcheck.rs`, run by `./check.sh`, compares **changed-line counts
and every hunk position** against six git invocations over four repositories.
A minimal edit script has exactly one length, so `myers` must match `git diff
--minimal` line for line. Numbers in [measurements.md](measurements.md).

### 2a. Normalise, for a whitespace relation

`Whitespace` is not a fourth algorithm, it is a different *equivalence relation*
on lines — which is why it is a knob on `Differs` rather than three more
implementations, and why `histogram-ignore-ws` does not exist.

| | git | rule |
|---|---|---|
| `exact` | *(default)* | byte for byte |
| `trailing` | `--ignore-space-at-eol` | trailing whitespace only |
| `change` | `-b` | any run of whitespace equals any other; trailing gone |
| `all` | `-w` | all whitespace, anywhere |

Normalising is **per line and length-preserving**, so the edit script computed
over the keys still addresses the original lines, and `hunks` is handed the real
text. That is the whole trick, and it means every algorithm — including one
compiled in by an extension that has never heard of this — gets it for free.

One consequence, and it is git's too: a line whose only change was whitespace
becomes *context*, and a context line is printed from the **old** file. So its
`new_no` points at bytes that differ from what is on screen. That is `-w`
working.

`change` is the one to read twice: a run collapses to a single space rather than
vanishing, so `foo` and ` foo` still differ. Only the *amount* is ignored.

### 2c. Compact, the indent heuristic

A run of changed lines can often sit in several places that say exactly the same
thing: when the line leaving one end of the group equals the line entering the
other, the whole group shifts by one and means the same. Which position a reader
wants is not arbitrary — a hunk starting at a function's signature reads, and the
same hunk starting at the previous function's closing brace does not.

This is git's `--indent-heuristic`, on by default there and here, and **ported
rather than reinvented** for one reason: it is the only version whose output can
be checked. The weights are xdiff's, names and values.

Two things about it are easy to get wrong and were:

- **A position's indentation is compared by *sign*, not by magnitude.** git keeps
  `effective_indent` and `penalty` as two numbers and compares the first with a
  three-way compare weighted by `INDENT_WEIGHT`. Adding `INDENT_WEIGHT * indent`
  into one score instead — which reads like the same thing — slid hunks to
  plausible places git does not put them, with identical line counts throughout.
  Comparing hunk *positions* in `diffcheck` is what caught it, and is why that
  check exists.
- **Ties go to the later position.** git's comparison is `<=`. Ties are common and
  the two answers are visibly different.

- **Whether a group *can* slide is the relation's question; how readable the
  result is, is the text's.** A slide is possible when the line leaving one end
  equals the line entering the other — under `-w`, "equals" means the *keys*, so a
  group may cross a line that differs from it only in indentation. Scoring, on the
  other hand, has to read the real text, because indentation is exactly what the
  keys erased. Comparing the text for both loses slides git makes: two hunks in
  cmux's history, with identical line counts, again found only by comparing
  positions.

Only a pure insertion or deletion slides. A replace has both sides moving, which
git handles per file through machinery this does not have; its boundaries are
pinned on both sides anyway.

### 2e. Moves, after the script is known

A block deleted here and added there is not a change, it is a relocation — and
the one thing in a diff a reader is allowed to *skip*. `moves` finds them and
`mark_moved` sets `DiffLine::moved`.

A post-pass and not a differ, because a move is only visible once the whole script
exists and every algorithm produces the same one. Three rules keep it honest:

- **Only lines the script touched.** Indexing over the whole file makes every
  repeated line in an unchanged region a move.
- **`MIN_MOVED_LINES`, three by default.** Two matching lines are a coincidence —
  `}` and a blank line are everywhere — and reporting them costs the feature its
  entire value. Git's `--color-moved=zebra` uses 3 for the same reason.
- **A landing is claimed once.** A block deleted once and added twice marks one of
  them, or the moved-line count exceeds the number of lines that exist.

`moved` is a flag beside `kind` and not a fourth `LineKind`, deliberately: a moved
line is still an addition or a removal, and `align`, `replace_pairs` and the
adds/dels counts all have to keep working untouched. Only the drawing cares — it
swaps the background for `diff.moved_added_bg` or `diff.moved_removed_bg` and
leaves the `+`/`-` alone so the sign column still scans, which is how git's
`--color-moved` does it too.

## 3. Prepare

One pass, in this order, because each step depends on the last.

### 3a. Clip first

```rust
pub fn clip(s: &str, max_chars: usize) -> String
```

The frontend owns the budget (`MAX_LINE_CHARS = 2000` in the diff view) because
how wide a row may get is a rendering question. Core applies it.

Clipping comes first so that **no span or token can ever point past what will be
drawn**. Get this backwards and the renderer indexes a line by a range that no
longer exists in it — a panic in a debug build, mojibake in a release one. There
is a test for exactly this ordering.

Why at all: text layout is linear in length and real repositories contain
minified bundles. A single 9.6-million-character line was measured in the wild.

### 3b. Intraline, second pass only

Line diff first, then re-diff only the pairs a line diff already matched as a
replace. `replace_pairs` matches a run of removals to the additions that follow
it, by position.

Two guards, both measured:

- **`MAX_INTRALINE_TOKENS = 1000`.** The LCS table is `a × b` cells; a 14k-token
  base64 line would allocate over a gigabyte for one pair.
- **`MIN_INTRALINE_SIMILARITY = 0.4`.** Position-matched pairing lies when the
  counts merely line up, and a pair that is not a rewrite of itself gets no word
  highlighting at all — see
  [decisions/0008](decisions/0008-intraline-similarity-floor.md).

Spans then close over whitespace-only gaps, or a rewritten sentence draws as a row
of separate blocks with the background showing through between the words.

Words, not characters. Char diffs on code are confetti.

### 3c. Syntax, per hunk side

```rust
highlight_hunk(hl, path, texts, kinds) -> Vec<Vec<Token>>
```

The old and new lines of a hunk are two different texts that happen to be printed
interleaved. Scanning them as one splices a removed line into an added one and
produces text that was never valid in any language, so each side is scanned
separately and context lines — which belong to both — are scanned twice.

Details in [syntax-highlighting.md](syntax-highlighting.md).

## 4b. Align, for a renderer that wants two columns

```rust
align::align(&[LineKind]) -> Vec<Slot>
```

One `Slot` per row of a two-column layout: `Context`, `Replace(old, new)`,
`Removed` or `Added`, as indices into the hunk's line list. A run of N removals
followed by M additions pairs index-wise; `min(N, M)` rows carry both sides and
the leftovers stand alone.

**It is the same function `replace_pairs` is built on**, and that is the reason it
is in `core` rather than in the renderer that wanted it. `replace_pairs` feeds the
intraline pass; if the two disagreed about which removal goes with which addition,
a row would show a removal beside an addition whose changed words were computed
against a *different* line — fragments highlighted that correspond to nothing on
screen. `align` is the primitive and `pairs` is a filter over it.

Cost: **4–10 ns a row**, 3.1 ms on the 714k-line fixture. The number that matters
is not the cost but the row count: 81% of unified's on the edit-heavy fixtures,
100% on the near-pure add-or-delete ones. A diff with nothing replaced has nothing
to pair.

## 4a. Layout, for a renderer that wants the document

Optional, and owned by the renderer rather than by `prepare`, because whether a
`.md` file is drawn as prose or as source is a presentation question.

```rust
markdown::lay_out(&mut hunk.lines) -> Vec<Block>
```

Per hunk side, through the same `syntax::for_each_side` the token pass uses — a
fence opens on one side and not the other constantly, and one shared splitter is
what stops the block pass and the token pass disagreeing about the same line.

It returns what each line structurally is and rewrites each line so its markers
are gone and every token and span still indexes it. **The markers are located from
the tokens, not by parsing**: a `Strong` token covers `**word**` including its
delimiters, so hiding them is a matter of checking the bytes at each end of a
range that already exists. Nothing is re-scanned and no parser is involved — see
[decisions/0010](decisions/0010-markdown-rendered-rows.md), which also says why a
CommonMark parse is the wrong shape for a hunk.

Removal is deletion only, so it is `String::drain` back to front over the buffer
the line already owns: nothing allocates per row. Ranges move *before* the text is
cut, because both have to be in the same coordinates while the mapping is
computed. Reversed, a token indexes bytes that are gone — the same failure the
clip ordering in 3a exists to prevent, with the same kind of test pinning it.

Tables are the exception to "per line": a cell has to line up with the rows around
it, so column widths are measured per *run* of table rows and per hunk side, then
every row of that run is rewritten once to the grid. The widths each run was
measured to come back out of `lay_out_tables` in a `Tables`, because one thing
about a table is not knowable here — whether it fits the window. See *the grid and
the window* in stage 4c. Padding is an insertion, so
tables use the general piecewise remap rather than the deletion-only in-place one
— worth it for 1–2.5% of rows, not worth it for all of them. It only lines up in a
monospaced face, which is what `Layout::monospaced()` is the frontend asserting.

Cost: **70–100 ns a row**, at load. On the two markdown fixtures that is 5.6 ms of
an 89.3 ms `prepare` and 7.2 ms of a 17 ms one — the share says nothing about this
pass and everything about how much intraline work the diff has.

## 4c. Wrap, for a window that is a certain width today

```rust
pub trait Wrap {
    fn name(&self) -> &'static str;
    fn breaks_lines(&self) -> bool { true }
    fn breaks(&self, text: &str, cols: usize, out: &mut Vec<Break>);
}
```

A long line is the one thing in a diff you cannot read by scrolling — the eye
loses the row on the way back, and in the two-column layout it loses both. So it
wraps, and **a wrapped line is *n* rows of `ROW_H`, never one taller row**:
`uniform_list` needs every row the same height and that is the only reason 714k
rows scroll at all.

Which means the wrap cannot make new lines. It returns **byte ranges into the
line**, and the line — its numbers, its tokens, its spans — is one object shared
by all of its rows:

```
  line 41  ────────────────────────────────────────────────────────────►
    41 │ + let result = compute(alpha, beta,          seg 0   [0 .. 34)
       │       gamma, delta, epsilon);                seg 1   [35 .. 61)
       ▲                     ▲
   no number on a            the space it broke on is dropped, not drawn
   continuation
```

Everything except the break points is shared, and deliberately: `Wrapped` turns
them into the range partition, **validates them**, holds them flat and answers by
index. So an implementation cannot produce a range that points past its line —
the same guarantee as clipping before the intraline pass in [3a](#3a-clip-first),
for the same reason, and it matters more here because this is a seam an extension
reaches. Invalid breaks are counted and reported on the overlay rather than
asserted: an assertion makes the validation untestable and turns somebody else's
bug into a crash.

`Vec<Vec<Break>>` is the obvious storage and the wrong one — 714k allocations for
a table that is mostly empty, because most lines fit. One contiguous buffer,
indexed, exactly like the order table below.

### Why it is not part of `prepare`

Because the budget is the window and the window moves. `prepare` produces text,
spans and tokens, none of which depend on the width; the wrap depends on nothing
else. One pass would make every frame of a resize drag pay for a syntax scan it
cannot use.

So a reflow re-runs 4c and 5 and stops. It exits on a float comparison when the
width crossed no character boundary, which is most frames of a drag, and it exits
before touching anything when the selected wrap is `off`.

### `off` is in the registry

Not a flag beside it. The title-bar pickers are a pure function of a registry, so
a registered wrap appears in the menu with nothing written by hand — the property
[decisions/0015](decisions/0015-title-bar-controls-are-hand-rolled.md) exists to
preserve. `Off` answers `breaks_lines() == false`, which is what lets a resize
skip the reflow rather than rescan 714k lines to be told nothing moved.

`word` is selected. It breaks at the last run of whitespace that fits, searching
*backwards* from the column so the row is never wider than the budget — forwards
overflows by however long the next word is, and it looks right on prose. The
whitespace it broke on is dropped, so the partition has holes and only whitespace
may fall in one. A word longer than the budget is broken mid-word, because the
alternative is a row wider than the window, which is the thing wrapping is for.

### The budget is per line

Not per diff, because `MarkdownRows` draws a bar, up to three levels of indent
and a bullet in front of its text, and draws a heading at 18px where the body is
14. Two rows of the same width hold different numbers of characters, and passing
the budget in per line is what stops that presentation needing a wrap of its own.
A table passes the width its *grid* has to fit, and is never broken at it: see
below.

Headers do not wrap. A file header is a path plus `+N -N`, which is not one
string to slice.

### The grid and the window

A table row is the one row whose text is a *layout*, so it is the one row that
cannot be broken at a column: the second half of row three would land under the
first half of row four, in a column that means something else. What happens
instead is that the grid is laid out again — `markdown::flow_table`, at reflow,
because the width is the only part of a table's layout `lay_out` cannot know:

- **The columns are squeezed by water-filling, not in proportion.** A column that
  already fits its share keeps the width it asked for; what is left is split
  between the ones that do not, repeatedly, because settling one raises the share
  for the rest. Proportional shrinking takes the `Yes` out of a three-character
  column to save a paragraph four characters it will not notice.
- **A cell wraps through the selected `Wrap`.** A cell is prose and an extension's
  policy is what breaks prose everywhere else in the diff. So `off` squeezes
  nothing — the reader asked to scroll — and `char` breaks a cell mid-word.
- **A row becomes as many rows as its tallest cell needs**, which is the same
  answer prose gets and keeps `uniform_list`'s fixed row height. Its sub-rows come
  back as one string joined by newlines, and `wrap::Budget::At` puts them in the
  same flat table every other row's rows are in — one answer to "how many rows is
  this", not two.
- **There is a floor.** Three columns cost ten characters of pipes and padding
  before a letter is drawn; below one character a column there is nothing honest
  to draw, so the table is left whole and the view scrolls to it.

Tokens and spans are carried onto the new text *clipped to each piece*, so a
`**phrase**` broken across two sub-rows is two ranges. One range spanning them
would paint everything in between — the pipes, the padding and the whole of the
next column.

Cost: **~1.1 µs a table row per reflow**, so nothing on any real diff and 2.3 ms
on a synthetic 2,000-row all-table one. It runs off the sparse table of which rows
are in a grid, so a diff with no table in it does no work here at any width.

Cost: **36–52 ns a line**, so 0.9–3.0 ms on the real fixtures and 26 ms on the
714k-line one. Rows added depends on the budget and not on the fixture: 1.00–1.02×
at 150 columns, 1.04–1.20× at 80. Code lines are short.

## 4–5. Rows, and the order table

Each file goes to the `Rows` implementation that claims its path; the last
registered claimant wins, and `TextRows` claims everything, which is what makes it
the fallback. `MarkdownRows` is the second built-in and takes `.md`, `.markdown`
and `.mdx`; it is registered through the same call an extension would use, for the
same reason `Highlighters::builtin` routes Markdown away from the scanner — a
built-in that skips the seam leaves the seam untested.

**Which set of implementations is loaded is the layout.** `Layouts` is a registry
of named builders, `unified` builds `[TextRows, MarkdownRows]`, `split` builds
`[SplitRows]` claiming everything, and `s` cycles them. Switching re-runs this
pipeline from stage 3, which is why the parsed diff is kept alive; see
[decisions/0014](decisions/0014-layouts-are-a-registry.md) for what that costs and
what the alternative would have cost.

Implementations keep their own row storage. The list keeps only:

```rust
struct RowRef { owner: u16, seg: u16, index: u32 }   // 8 bytes
```

At 714k rows a boxed row per row would be 714k allocations to chase on every
scroll. See [decisions/0006](decisions/0006-row-seam-without-boxing.md).

`seg` is which row of a wrapped line this is, and it fits in the two bytes the
other two fields left over — so wrapping cost this table nothing. The cap is
65,535 rows per line, which `MAX_LINE_CHARS` over `MIN_WRAP_COLS` puts out of
reach by a factor of 260.

The table is also its own record of the *unwrapped* shape: consecutive entries
with the same owner and index are one logical row, and an index is unique within
an owner. So a reflow expands the previous table in place of a second one kept
alongside it — 8 bytes a row, once, however many times the window is dragged.

## 6. Draw

Anatomy of a line row, all rows the same height so `uniform_list` can virtualize
the whole thing:

```
 ROW_H = 22px
├──────────────┼──────────────┼────┼───────────────────────────────────────────►
│  GUTTER_W 52 │  GUTTER_W 52 │ 16 │  one StyledText, N style runs
│   old line # │   new line # │ +- │  fn draw(&self) { self.paint(1); } // later
└──────────────┴──────────────┴────┴───────────────────────────────────────────►
                                       ▲       ▲                       ▲
                                       │       │                       │
                                   keyword  changed-word bg        comment
                                            (from intraline)    (lifted for
                                                                 its surface)
```

All three presentations draw that anatomy, share the same `runs` merge, and
differ only in what they do with the text area. All three also take `moved`
through the same `line_colors` and `runs`, so none of them can disagree about what
a relocated line looks like — and a moved line's intraline spans are dropped in
`runs` rather than drawn in the row's own colour, because they would be describing
a change the detection just said was not one. `SplitRows` draws it twice at a
narrower gutter, either side of a one-pixel rule, and fills the half with no line
in it with `diff.absent_bg` — its own colour, because "unchanged" and "there is
no line here" are opposite things and `context_bg` already means the first. `MarkdownRows`
sets a font size on the row for a heading — `HighlightStyle` has no font size, so
that cannot be a run — and puts glyphs, bars and rules beside the text as separate
elements rather than as runs, so they can carry their own colour.

One `StyledText` with a run list, not an element per span:

```rust
StyledText::new(text.clone()).with_highlights(runs(at, tokens, spans, theme, kind, moved, sel))
```

`runs` sweeps the token edges, the span edges and the selection's together — all
three inputs are already sorted and internally non-overlapping — and emits one run
per segment:

```
  text     fn draw(&self) { ... }
  tokens   ██          ████            keyword, func
  spans        ████████████            changed words
  sel            ██████████████        what the mouse is holding
  runs     ├──┼─┼┼─────┼───┼─┤         fg from token+surface, bg from span or sel
```

Syntax sets the foreground, intraline sets the background, and the foreground is
resolved against whichever background it lands on — see [theming.md](theming.md).

A selection outranks a changed word: both are backgrounds, only one can be drawn,
and the reader already knows which words changed. It is a `Surface` of its own, so
the token colours that land on it are resolved against it like any other —
`Surface::Selected`, and
[decisions/0018](decisions/0018-selection-is-a-model-not-a-text-element.md) for why
it is a run and not an overlay.

Rows hold `SharedString`, not `String`: `render` runs per visible row per redraw,
so handing GPUI a `String` copies the line every frame.

The run list *is* rebuilt per visible row per frame. Caching it for 714k rows
costs roughly 40× the memory of the rows themselves, and 50 rows × ~15 runs is
nothing — a deliberate exception to "recomputes what a cache could hold", written
down rather than left silent.

## Load cost

`./check.sh`, release, this machine — see [measurements.md](measurements.md) for
the full table:

| fixture | diff lines | parse | prepare | align |
|---|---|---|---|---|
| `pr33933.diff` | 20,831 | 1.6 ms | 8.0 ms | 86 µs |
| `pr30698.diff` | 50,604 | 6.2 ms | 98.9 ms | 366 µs |
| `pr30683.diff` | 713,996 | 57.1 ms | 288.7 ms | 3.1 ms |

`prepare` dominates load on the pathological fixture, and syntax dominates
`prepare`. It is load, not the render path. Making it lazy per file is the next
lever if a third of a second on a 714k-line diff ever matters.

Stages 1 and 2 are not in that table because the fixtures are pre-made `.diff`
files and skip both. Measured against a real repository they are 25–96 ms of
acquisition and 1–29 ms of diffing, so the pipeline's cost is `prepare` wherever
the diff came from.
