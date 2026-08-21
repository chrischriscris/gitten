# The diff pipeline

From two blobs to pixels, in six stages. Five of them are in `core` and have no
idea a window exists.

```
  1  acquire     plait_git::pairs                    Vec<Pair>   two texts per file
  2  diff        Differs::file                       Vec<FileDiff>
       2a script     Differ::diff, per file       ── the seam: histogram, myers, …
       2b hunks      differ::hunks                   context, line numbers, headers
  3  prepare     prepared::prepare                   Vec<prepared::File>
       3a clip        every line, to a column budget
       3b intraline   changed words, per replace-pair
       3c syntax      tokens, per hunk side
  4  build       Rows::build, per file               renderer-owned rows
       4a layout      markdown::lay_out, if the renderer asks
       4b align       align::align, if the renderer asks
  5  order       Vec<RowRef>                         8 bytes per row
  6  draw        Rows::render + Theme                one StyledText per row
```

Stages 1–5 run once, at load, and again from stage 3 when the layout changes.
Stage 6 runs per visible row per frame and is the only one on the render path.

Stages 4a and 4b are indented because they are not part of the pipeline
everything goes through: each runs only for the renderer that asks for it —
`MarkdownRows` wants the block structure, `SplitRows` wants the alignment.

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

An implementation produces **only the edit script**. Line numbers, context lines,
hunk headers and the `@@ … @@ fn name` suffix are `differ::hunks`, shared by every
differ — that bookkeeping is identical for all of them, and a hunk header that
disagrees with the lines under it is a bug that survives review.

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

Correctness is checked against git rather than argued. A minimal edit script has
exactly one length, so `myers` must match `git diff --minimal` exactly —
`git/examples/diffcheck.rs`, run by `./check.sh`. Numbers in
[measurements.md](measurements.md).

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
every row of that run is rewritten once to the grid. Padding is an insertion, so
tables use the general piecewise remap rather than the deletion-only in-place one
— worth it for 1–2.5% of rows, not worth it for all of them. It only lines up in a
monospaced face, which is what `Layout::monospaced()` is the frontend asserting.

Cost: **70–100 ns a row**, at load. On the two markdown fixtures that is 5.6 ms of
an 89.3 ms `prepare` and 7.2 ms of a 17 ms one — the share says nothing about this
pass and everything about how much intraline work the diff has.

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
struct RowRef { owner: u16, index: u32 }   // 8 bytes
```

At 714k rows a boxed row per row would be 714k allocations to chase on every
scroll. See [decisions/0006](decisions/0006-row-seam-without-boxing.md).

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
differ only in what they do with the text area. `SplitRows` draws it twice at a
narrower gutter, either side of a one-pixel rule, and fills the half with no line
in it with `diff.absent_bg` — its own colour, because "unchanged" and "there is
no line here" are opposite things and `context_bg` already means the first. `MarkdownRows`
sets a font size on the row for a heading — `HighlightStyle` has no font size, so
that cannot be a run — and puts glyphs, bars and rules beside the text as separate
elements rather than as runs, so they can carry their own colour.

One `StyledText` with a run list, not an element per span:

```rust
StyledText::new(text.clone()).with_highlights(runs(text, tokens, spans, theme, kind))
```

`runs` sweeps the token edges and the span edges together — both inputs are
already sorted and internally non-overlapping — and emits one run per segment:

```
  text     fn draw(&self) { ... }
  tokens   ██          ████            keyword, func
  spans        ████████████            changed words
  runs     ├──┼─┼──────┼───┤           fg from token+surface, bg from span
```

Syntax sets the foreground, intraline sets the background, and the foreground is
resolved against whichever background it lands on — see [theming.md](theming.md).

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
