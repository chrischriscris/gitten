# The diff pipeline

From `git diff` bytes to pixels, in six stages. Five of them are in `core` and
have no idea a window exists.

```
  1  acquire     plait_git::diff                     bytes
  2  parse       parse_unified_diff                  Vec<FileDiff>
  3  prepare     prepared::prepare                   Vec<prepared::File>
       3a clip        every line, to a column budget
       3b intraline   changed words, per replace-pair
       3c syntax      tokens, per hunk side
  4  build       Rows::build, per file               renderer-owned rows
       4a layout      markdown::lay_out, if the renderer asks
  5  order       Vec<RowRef>                         8 bytes per row
  6  draw        Rows::render + Theme                one StyledText per row
```

Stages 1–3 run once, at load. Stage 4 runs once, at load. Stage 6 runs per
visible row per frame and is the only one on the render path.

Stage 4a is indented because it is not part of the pipeline everything goes
through: it runs only for the renderer that asks for it, and today only
`MarkdownRows` does.

## 2. Parse

`parse_unified_diff` walks unified output into `FileDiff → Hunk → DiffLine`,
tracking both line numbers as it goes. Binary files, renames and mode changes are
skipped rather than modelled — a gap, not a design.

Never `read_to_string` anything from git. Commit metadata is not guaranteed UTF-8
and real history carries Latin-1 author names; `git/git` panics that outright.
Read bytes and `String::from_utf8_lossy`.

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

Cost: **70–90 ns a row**, at load. On the two markdown fixtures that is 5.1 ms of
a 90.7 ms `prepare` and 6.5 ms of a 16.6 ms one — the share says nothing about
this pass and everything about how much intraline work the diff has.

## 4–5. Rows, and the order table

Each file goes to the `Rows` implementation that claims its path; the last
registered claimant wins, and `TextRows` claims everything, which is what makes it
the fallback. `MarkdownRows` is the second built-in and takes `.md`, `.markdown`
and `.mdx`; it is registered in `Diff::new` through the same call an extension
would use, for the same reason `Highlighters::builtin` routes Markdown away from
the scanner — a built-in that skips the seam leaves the seam untested.

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

Both built-ins draw the same anatomy, share the same gutter, sign column and
`runs` merge, and differ only in what they do with the text area. `MarkdownRows`
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

| fixture | diff lines | parse | prepare |
|---|---|---|---|
| `pr33933.diff` | 20,831 | 1.6 ms | 8.0 ms |
| `pr30698.diff` | 50,604 | 6.2 ms | 98.9 ms |
| `pr30683.diff` | 713,996 | 57.1 ms | 288.7 ms |

`prepare` dominates load on the pathological fixture, and syntax dominates
`prepare`. It is load, not the render path. Making it lazy per file is the next
lever if a third of a second on a 714k-line diff ever matters.
