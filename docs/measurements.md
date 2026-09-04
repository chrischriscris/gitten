# Measurements

Every number quoted anywhere in these docs, with what produced it. A figure that
cannot be reproduced is folklore; where a measurement is *not* reproducible from
this repository today, it says so and gives the recipe.

**Machine for everything below:** Apple M1 Pro, 10 cores, macOS 26.5.2,
rustc 1.97.1, `--release`. Never read any of these off a debug build — `cargo run`
without `--release` is a different, much slower binary, and the title bar says so.

## Reproducible from this repo

```
./dev check                                             # all of the below, plus tests
cargo test -p gitten-core                                # correctness, sub-second
cargo test -p gitten-app                                 # the config file and the command line
cargo run -q -p gitten-core --example bench   --release   # load timings, per fixture
cargo run -q -p gitten-core --example shape   --release   # topology statistics
cargo run -q -p gitten-core --example verify  --release   # lane invariants
cargo run -q -p gitten-core --example paint   --release   # the diff view, in ANSI
cargo run -q -p gitten-git  --example diffcheck --release [REPO] [REVSPEC]
                                                        # differs, against git's own answer
./dev dump diff --fixtures                              # a terminal frame, and what it cost
GITTEN_STATS=1 ./target/release/gitten-shell diff         # frame/heap overlay
```

`bench` and `shape` read `fixtures/big.diff` and `fixtures/log.txt`; `check.sh`
swaps each real fixture in and restores what was there.

### The differs, against git

`git/examples/diffcheck.rs`, six git invocations per input, four inputs, run by
`./check.sh`'s `differs vs git` section. It compares **changed-line counts and
every hunk position**. A minimal edit script has exactly one length, so `myers`
must match `git diff --minimal` line for line.

Changed lines and hunk positions, this machine, against `git 2.51`:

| mode | git flags | counts match | positions match |
|---|---|---|---|
| `histogram` | `--histogram` | all four inputs | all four inputs |
| `patience` | `--patience` | +4 in 9,938 on one input | all four inputs |
| `myers` | `--minimal` | all four inputs | 1–4 hunks per input placed differently |
| `trailing` | `--histogram --ignore-space-at-eol` | all four | all four |
| `change` | `--histogram -b` | all four | all four |
| `all` | `--histogram -w` | all four | all four |

Five of the six rows match git's hunk positions exactly on every input, which is
what makes them a test of the indent heuristic rather than a description of it.

Two rows do not match exactly, and neither is a defect:

**`patience`** is patience's *idea* — anchor only on lines appearing once —
through the histogram machinery, where git's `--patience` takes the longest
increasing subsequence of all unique-line matches at once. `diffcheck` flags a
drift past 1%.

**`myers`** matches git's line counts everywhere and places 1–4 hunks per input
elsewhere. A minimal script has one length but not one *shape*: several scripts of
that length exist, ours picks a different one from git's, and the slide then places
it differently. The counts agreeing is what proves both are still minimal. The
anchored rows have no such freedom, which is what makes their exact positions the
real test of the indent heuristic.

**The positions column is the whole reason this check was extended**, and it
earned itself twice in one sitting. Both bugs it found left every changed-line
count identical:

1. The indent heuristic scored a position's indentation by *magnitude* where git
   compares it by *sign*. Plausible either way in prose; it slid hunks to places
   git does not put them.
2. The slide tested line equality against the real text rather than through the
   whitespace relation, so under `-b` and `-w` it could not cross a reindented
   line where git can. Two hunks in cmux's history.

Nothing in the counts column moved for either. Comparing positions found the
first in one run and the second in the next.

Time, ours against the `git` process it replaced. Unfair to git on process
startup and to nobody else, so read it as an order of magnitude:

| input | ours, histogram | ours, `-w` | `git diff --histogram` |
|---|---|---|---|
| this repo, `HEAD~4..HEAD` | 3.2 ms | 7.4 ms | 31 ms |
| `cmux`, `HEAD~4..HEAD` (307k lines) | 28 ms | 108 ms | 79 ms |
| `git/git`, `HEAD~4..HEAD` | 2.0 ms | 8.4 ms | 22 ms |

Ignoring whitespace costs 2–4× the exact relation, and all of it is the
per-line normalisation. This table's runs paid one `String` per line of both
files; since [the August 2026 memory pass](#the-august-2026-memory-pass) the
normalized form materialises once per *distinct* line, interned in a per-file
arena and byte-compared on collision. Still paid on a click, not on a frame.

Acquisition is separate and is two processes regardless of file count: 45 ms on
this repository, 125 ms for cmux's 307k lines. One exception worth knowing about —
**a blobless partial clone fetches over the network on demand.** The first
`HEAD~5..HEAD` in `~/Projects/git` cost **15.0 s** of `cat-file` waiting on the
promisor remote and **42 ms** once the blobs were local. `git diff` in the same
repository does exactly the same thing.

### Alignment, for the two-column layout

`align::align` — one `Slot` per side-by-side row, and the pairing the intraline
pass shares. Reported by `bench` on its own line.

| fixture | diff lines | split rows | of unified | paired both sides | `align` | per row |
|---|---|---|---|---|---|---|
| `pr33933.diff` | 20,831 | 20,829 | 100% | 48 | 86 µs | 4 ns |
| `pr30698.diff` | 50,604 | 41,129 | 81% | 41,129 | 366 µs | 9 ns |
| `pr30683.diff` | 713,996 | 713,595 | 100% | 1,811 | 3.1 ms | 4 ns |
| `md.diff` | 71,756 | 58,077 | 81% | 36,537 | 561 µs | 10 ns |

The cost is nothing — 3.1 ms against a 247 ms `prepare` on the same input. The
interesting column is *of unified*, because it says when the presentation earns
its place: **81% on the two edit-heavy fixtures, 100% on the two that are
near-pure addition or deletion.** A diff with nothing replaced has nothing to
pair, and side-by-side gives it two half-empty columns and the same row count.

### Wrapping, and what a resize costs

`wrap::Wrapped::build` — the break points for every line, at one column budget.
Reported by `bench` on its own line; `WRAP_COLS=n` sets the budget, and the
default 150 is roughly a 1440px window of text in the shipped 14px face.

| fixture | diff lines | `wrap` | per line | rows at 150 | rows at 80 |
|---|---|---|---|---|---|
| `pr33933.diff` | 20,831 | 0.9 ms | 43 ns | 20,831 (1.00×) | 21,736 (1.04×) |
| `pr30698.diff` | 50,604 | 2.3 ms | 45 ns | 51,465 (1.02×) | 60,526 (1.20×) |
| `pr30683.diff` | 713,996 | 26.2 ms | 37 ns | 717,534 (1.00×) | 787,218 (1.10×) |
| `md.diff` | 71,756 | 3.0 ms | 42 ns | 72,257 (1.01×) | 74,959 (1.04×) |
| synthetic 1M | 928,577 | 44.3 ms | 48 ns | 928,577 (1.00×) | — |

Two numbers matter and neither is the total.

**Per line is what decides whether reflowing on resize is viable**, because a
drag pays this once per column crossed. 37–48 ns against a `prepare` that is
5,600 ns a line on the same fixture — so the frame where a resize crosses a
character boundary costs 26 ms on the worst input in the set and 0.9–3.0 ms on
the realistic ones, and every other frame of the drag is a float comparison. The
reason it can be that cheap is that a reflow re-runs stages 4c and 5 and nothing
above them: no clip, no intraline, no syntax.

**Rows added is far smaller than it looks, and depends on the budget rather than
the fixture.** At a real window width three of the four fixtures grow by 1% or
less, because code lines are short — `pr30683` is a 714k-line deletion of source,
and 150 columns holds nearly all of it. The two that move are the ones with long
lines: `pr30698`, the zig→rust migration, at 1.20× on an 80-column window, and
`md.diff`, which is prose. Wrapping is not a 2× row count; it is a few percent,
paid where the text actually needs it.

`0 rejected` on every fixture is the validation in `Wrapped::build` reporting that
the shipped wraps produce no invalid break — the column exists because it is a
seam an extension reaches, and a wrap whose breaks were all thrown away would
otherwise look exactly like one that found nothing to do.

### The cost of a pick

Both title-bar controls rebuild rather than re-render, so what they cost is made
of numbers already in this file:

| control | what it re-runs | typical | pathological fixture |
|---|---|---|---|
| wrap | break points + order table | 1–3 ms | 26 ms |
| layout | `prepare` + row build | 8 ms | 247 ms |
| algorithm | acquisition + diff + `prepare` + row build | 35–140 ms | — |

Layout re-runs the pipeline from stage 3 against the parsed diff the view is
holding, so it is the `prepare` column of the table below. Algorithm has to
acquire again — 25–110 ms of `git diff --raw` and `cat-file` from the table above,
plus 1–29 ms of diffing — because it changes what the diff *is*.

The pathological column is blank for algorithm on purpose: the 714k-line fixtures
are `.diff` files, which have no algorithm to change. That is also why the control
is inert for them.

Wrap re-runs neither: the lines, their tokens and their spans are the same
objects, and only where they break moves. That is why it is bound to `w` and also
why it can happen on a resize drag at all — see the table above.

On a click all three are fine. Only wrap would be fine on a key held down, which
is the other reason the algorithm is a menu.

### Load, per fixture

`prepare` is clip + intraline + syntax — everything between parsing and rows.

| fixture | diff lines | files | parse | prepare | of which syntax | tokens | scanned |
|---|---|---|---|---|---|---|---|
| `pr33933.diff` | 20,831 | 35 | 1.6 ms | 8.0 ms | 6.5 ms | 32,236 | 115 MB/s |
| `pr30698.diff` | 50,604 | 1,398 | 6.2 ms | 98.9 ms | 35.6 ms | 122,237 | 67 MB/s |
| `pr30683.diff` | 713,996 | 1,375 | 57.1 ms | 288.7 ms | 237.3 ms | 1,330,580 | 114 MB/s |
| `md.diff` | 71,756 | 229 | 6.6 ms | 90.7 ms | 12.9 ms | 45,250 | 258 MB/s |

`pr30698` was the fixture chosen because intraline dominates it (57.5 ms): it is
the zig→rust migration, so nearly every line is a near-identical rewrite of
another — the worst case for a quadratic word diff.

`md.diff` turns out to be worse, and unintentionally so. It was added for the
rendered Markdown presentation and is **71.7 ms of intraline out of a 90.7 ms
`prepare`, 79% of the pass** — because prose is edited a sentence at a time, so
almost every changed line is a near-identical rewrite of the one it replaced. It
is now the heaviest intraline case in the set. Code diffs replace whole lines;
prose diffs replace words inside them, and nothing in the code fixtures showed
that.

### The Markdown layout pass

`markdown::lay_out` — block classification, marker removal, range remapping. Runs
at load, for markdown files only, and adds nothing to the render path. Reported by
`bench` on its own line whenever the diff contains a `.md` file.

| fixture | rows | files | `prepare` | `lay_out` | per row | of `prepare` |
|---|---|---|---|---|---|---|
| `md.diff` (rust-lang/book) | 71,705 | 228 | 89.3 ms | 5.6 ms | 78 ns | 6.2% |
| a technical-docs tree | 75,684 | 1,019 | 17.0 ms | 7.2 ms | 95 ns | 42.4% |

Table alignment is in those figures and is close to free on average: 70–100 ns a
row with it, 70–90 without. Tables are 2.4% and 1.0% of changed lines, and a hunk
with no table skips the pass on one `any(is_table)` scan of the blocks.

**Quote the per-row figure, not the share.** The two shares differ by 7× and the
per-row costs by 1.2×; the share is a statement about how much intraline work the
*rest* of the diff had, not about this pass.

The per-row figure is only meaningful at scale. `pr30683.diff` has five markdown
files and 44 markdown rows in 714k, and reports 1,474 ns/row — nearly all of it
the walk over 1,375 file paths to find those five, and their cold cache lines.
That number is about finding the work, not doing it.

`bench` measures this in place on the rows `prepare` just produced, which is also
what the view does. It used to clone the prepared diff first to leave `p`
untouched, which put the first touch of every markdown file inside the timer and
reported **610 µs** for those same 44 rows — a tenfold overstatement, entirely
from page faults on a freshly duplicated 714k-line structure. Worth remembering
before defensively cloning anything inside a timer.

Breakdown, single runs on the technical-docs fixture, by disabling the later
stages in turn — this machine is noisy enough (`prepare` itself swings 2× run to
run) that these are proportions rather than figures:

| stage | cost | share |
|---|---|---|
| classify blocks | 2.8 ms | ~37% |
| derive the cuts from the tokens | +2.1 ms | ~28% |
| drain the text, remap the ranges | +2.6 ms | ~35% |

### Fitting a table to the window

`markdown::flow_table` — squeezed columns, cells wrapped inside them — is the
other half of that pass, and the half that runs at *reflow* rather than at load,
because the width is the only part of a table's layout the load pass cannot know.
`md.diff` through `MarkdownRows`, release, 73,557 rows of which 908 are table rows
in 214 grids:

| budget | reflow | table rows re-laid out |
|---|---|---|
| 400 cols | 3.3 ms | 0 |
| 150 cols | 4.3 ms | 364 |
| 120 cols | 4.5 ms | 457 |
| 100 cols | 4.7 ms | 540 |
| 80 cols | 4.6 ms | 601 |

So the flow is **1.0–1.4 ms** on top of a 3.3 ms wrap of the whole diff, at
**2–3 µs a re-laid-out row** — a third more expensive than a reflow without it,
and a third of a frame at 73k rows. The 400-column row is the floor and the thing
to keep: a table that fits costs one comparison, and a diff with no table in it
does no work here at any width, because this runs off the sparse list of which
rows are in a grid rather than over the rows.

**40% of the table rows in a real prose corpus do not fit 150 columns**, which is
the number that says this is not an edge case. Before it existed, one of them made
the whole 73k-row view scroll sideways.

### The two Markdown shapes

Every other diff fixture here is code. Prose is a different distribution, and the
two real markdown corpora measured are not alike either — which is why both are
recorded. Counted over the `+`/`-` lines:

| | rust-lang/book | a technical-docs tree |
|---|---|---|
| diff lines | 71,756 | 75,684 |
| files | 229 | 1,019 |
| changed (`+`/`-`) lines | 48,878 | 74,601 |
| paragraph | 79.7% | 34.2% |
| blank | 9.2% | 29.6% |
| heading | 2.2% | 12.1% |
| bullet / ordered | 2.6% | 19.9% |
| fence | 1.8% | 3.2% |
| table | 2.4% | 1.0% |
| quote | 2.1% | 0.1% |
| replace-pairs | 13,679 | 92 |

The book is prose: long paragraphs, few headings, and intraline work on 13,679
line pairs. The technical-docs tree is the opposite — a third of the size in
paragraphs, six times the headings, eight times the lists, nearly a third of it
blank, and **92** replace-pairs in the whole diff. A renderer that looked good on
one and untested on the other would be half tested; so would a claim about what
this costs.

The percentages are over the changed lines only, which is what
`grep '^[+-]' | grep -v '^+++\|^---'` gives you; the diff-line counts include
context and come from `bench`.

### What the marker removal cannot reach

It removes markers the token pass located, so it inherits that pass's blind spots
exactly. Both were counted over the changed lines of both fixtures:

| | rust-lang/book | a technical-docs tree |
|---|---|---|
| headings carrying inline markup | 230 / 1,081 = 21.3% | 306 / 9,008 = 3.4% |
| — as a share of all changed lines | 0.47% | 0.41% |
| lines with an unpaired `**` run | 0.05% | 0.17% |

The heading share differs by 6× and lands in the same place either way, because
the two fixtures have inverse heading counts: a programming book puts `` `code` ``
in a fifth of its headings and has few of them; a technical-docs tree has eight
times as many headings and marks up almost none. Under half a percent of rows on
both, which is the figure that decided it.

```
grep '^[+-]' F | grep -v '^+++\|^---' | sed 's/^[+-]//' | grep -cE '^ *#{1,6} .*(\*\*|`|\[[^]]+\]\()'
grep '^[+-]' F | grep -v '^+++\|^---' | sed 's/^[+-]//' \
  | awk '{s=$0; if (gsub(/\*\*/,"",s)%2==1) o++} END {print o, NR}'
```

Both are left alone on purpose rather than guessed at; see
[decisions/0010](decisions/0010-markdown-rendered-rows.md).

Reproducing them:

```
./fixtures/fetch.sh                                     # builds md.diff from rust-lang/book
git -C <a docs repo> diff HEAD~2000..HEAD -- '*.md' > fixtures/real/md.diff
```

The second is the technical-docs shape; the tree measured above is private, so
that row is not reproducible from this repository — the recipe is. Any repository
with a large `docs/` tree and a few thousand commits produces the same shape.

### Synthetic scale

`./fixtures/gen.sh 1000000 1000000` — a million commits, ~929k diff lines:

| stage | time |
|---|---|
| parse log | 466 ms |
| assign lanes | 301 ms |
| parse diff | 76 ms |
| prepare | 683 ms |

These absolutes predate [the August 2026 allocation and startup
pass](#the-august-2026-allocation-and-startup-pass) and [the memory pass
after it](#the-august-2026-memory-pass), which moved most of them;
read them next to the *before* column there, never against the *after* — the two
columns were measured in one sitting, this table was not, and cross-vintage
comparisons are how an improvement reads as a regression.

### The August 2026 allocation and startup pass

Two commits, reviewed together because they share a measurement session:
borrowed graph lane IDs, a flattened LCS table and token buffer,
allocation-free syntax routing behind lexer category gates, and a byte-length
fast path in `clip` (`core`); labels, untracked status and the tui's watcher run
beside acquisition instead of behind it, and `GITTEN_START_LOG=1` prints per-stage
startup timings (`app`, `git`, `shell`, `tui`). Outputs are byte-identical either
side: 1,000,000
commits / widest 21 lanes; 928,577 lines / 5,953 files / 142,858 replace-pairs /
2,071,441 tokens / 0 wrap rejections.

```
cargo run -q -p gitten-core --example bench --release   # per-stage, fixtures/big.diff + log.txt
GITTEN_START_LOG=1 ./target/release/gitten-shell diff .   # startup stages, opt-in
```

Before/after, `main` vs the pass, median of six rounds a side. The design
matters more than usual: naive back-to-back A/B runs here swung **+25–95 %**
depending purely on which binary ran second — the first process's multi-GB
allocation churn leaves the VM reclaiming under the second. Rounds are therefore
ABBA-interleaved with settle gaps and a flipping starting side; position bias
verified absent (ran-first ≈ ran-second within each side), CV < 3 % on every CPU
stage.

| stage (fixture) | before | after | Δ |
|---|---|---|---|
| parse `log.txt`, 1M commits | 328 ms | 270 ms | −17.8 % |
| assign lanes | 212 ms | 190 ms | −10.2 % |
| intraline, 142,858 replace-pairs | 441 ms | 371 ms | −15.9 % |
| syntax highlighting | 560 ms | 286 ms | **−48.9 %** |
| `prepare`, whole assembly¹ | ~1,100 ms | 709 ms | ≈ −36 % |
| wrap @150 cols | 29.2 ms | 29.1 ms | −0.3 % |
| whole process wall | 2.47 s | 1.97 s | **−20.1 %** |

¹ The example quantizes this line at 0.1 s. `align` is omitted: untouched by the
pass, and at 13–27 % CV across rounds its ≤ 2 ms drift is noise.

Terminal startup, spawn → first fully-presented frame (first frame-close
sentinel on a private pty; seven alternating runs a side, ranges non-overlapping):

| | before | after | Δ |
|---|---|---|---|
| spawn → first frame | 759 ms | 666 ms | −12.2 % |

On the branch itself, `GITTEN_START_LOG` puts nearly all of that in two stages —
acquired 347 ms, views built 308 ms — everything else under a millisecond. The
desktop client was **not** measured: it needs a window, and nothing headless
exercises it; its win (window-before-acquisition) is structural rather than a
number here. The stage clock also only exists after the pass, so no per-stage
baseline against `main` is possible by construction.

Since then the tui's startup has split into two frames: the list frame
(`first frame flushed`) draws with the sidebars and the preview diff in their
loading shape, and one wave of deferred reads fills them before a second frame
(`startup frame flushed`) is flushed. "First fully-presented frame" now means
the second. The desktop's time-to-interactive is now reproducible headlessly —
`GITTEN_START_QUIT=1 GITTEN_START_LOG=1 ./target/release/gitten-shell commits .`
quits the moment its first rows are drawn, so a wall clock around the process
*is* the number (a window still appears) — recorded in
[the desktop section below](#the-desktop-opens-its-window-before-it-acquires).

### Time to interactive, as recorded

```sh
cargo run -q -p gitten-tui --example tti --release .            # one side, medians
GITTEN_BASELINE=<old>/gitten-tui \
  cargo run -q -p gitten-tui --example tti --release . --json   # ABBA + deltaPct
```

Both clients, this repository, release, the same M1 Pro, 7 rounds a side
ABBA-interleaved (warmup discarded, medians), September 2026. **This is the
baseline the next optimization pass measures against** — and the harness is the
tool to measure it with: point `GITTEN_BASELINE` at today's binary before
touching anything, and the `deltaPct` it prints is the whole conclusion. Noise
between *identical* binaries measures ±14.5 % with this discipline, so a delta
smaller than that is not a result.

| | previous vintage¹ | current | Δ |
|---|---|---|---|
| tui spawn → first frame (the list, interactive) | 110.5 ms | **39.2 ms** | −64.5 % |
| tui spawn → filled frame (sidebars + preview) | — | 67.2 ms | deferred work lands 28 ms after the list is usable |

¹ The binary before the startup split into two frames: its single frame was
the filled frame, so it has no separate list-frame figure.

The desktop's number lives in its own table below (~282 ms; the previous
vintage measured ~357 ms the same way), because it needs a window and the
harness gets it from `GITTEN_START_QUIT` rather than the pty. Two structural
tests in `tui/src/main.rs` pin the deferral itself — the startup loads run in
`load_startup` after the first frame, not in `App::new` before it, and a
fixture launch defers nothing — so moving work back onto the road to frame one
breaks `cargo test`, not just the timing.

### The August 2026 memory pass

One commit on top of [the allocation and startup pass above](#the-august-2026-allocation-and-startup-pass):
acquired line text is one `Arc<str>` shared from git through the prepared rows
instead of a `String` per copy; each differ reuses its scratch buffers across
files; normalized whitespace keys are interned per file; token and span offsets
are `u32`. A second commit touches only dev profiles — no runtime number here.

Measured against **`main` as of the merge of the pass above**, not against that
pass's baseline — the passes share a twin commit, so comparing across vintages
would credit it twice. Same discipline as the table above: six rounds a side,
starting side flipped every round, settle gaps, medians. Structural output
identical either side: 1,000,000 commits / widest 21 lanes; 928,577 lines /
5,953 files / 142,858 replace-pairs / 2,071,441 tokens / 0 wrap rejections.
Peak RSS read off `/usr/bin/time -l target/release/examples/bench`.

| stage | main | branch | Δ |
|---|---|---|---|
| parse `log.txt`, 1M commits | 263 ms | 286 ms | **+8.7 %** |
| assign lanes | 195 ms | 189 ms | −2.7 % |
| intraline, 142,858 replace-pairs | 372 ms | 374 ms | noise |
| syntax highlighting | 292 ms | 286 ms | noise |
| `prepare` | 728 ms | 723 ms | −0.7 % |
| `align` | 7.9 ms | 5.3 ms | **−33 %** |
| wrap @150 cols | 30.9 ms | 27.4 ms | −11 % |
| **peak RSS, whole bench run** | **1,161 MB** | **972 MB** | **−16.3 % (−189 MB)** |

The RSS line is the point of the pass and the steadiest number in it: twelve
runs spread under half a megabyte. Most of the win is line text — a loaded diff
held each line as several independent heap strings, and now holds one buffer per
distinct text with counts beside it.

Re-checked in a second sitting before merge — same machine, same fixtures,
independent A/B against the same base vintage. Peak RSS reproduced to the
megabyte; `align` −32 % and wrap −9 %. Parse came in higher than the table:
+10–12 % against main, so read that row as the low edge of what reproduces and
the trade as ~+11 %.

The parse regression is the pass's one deliberate trade, and it took three
measurements to attribute correctly. The first cut read +11 % and looked like
the intern map's hashing; a cheaper hasher (`FxHasher`, `core::parse_log`) was
tried against that and is kept — hashing short names with SipHash was the
obviously wasteful half — but an A/B either side of it measures no separable
end-to-end effect: the two sit within run noise. What survived attribution:
deleting the map entirely changes nothing — **a mapless variant parses at the
same speed** — which moves the cost to where it actually lives: `Arc<str>`
construction itself. In among
`parse_log`'s other allocations an author-sized `Arc` runs ~25 ns a commit
against ~14 for `String::to_string`, and that price is the representation this
pass exists to buy: one shared buffer per distinct name instead of one heap
string per commit, plus sixteen fewer bytes of `Commit` per row — tens of MB on
a million-commit history, nothing on seven authors. The escape hatch, if the
time ever matters more than the memory, is reverting `Commit.author` to
`String`; measured here so whoever pulls it knows what each side costs.

| parse variant (1M-commit fixture) | ms |
|---|---|
| `String::to_string` per commit (`main`) | 239 |
| `Arc<str>`, interned through a hash map | 268 |
| `Arc<str>`, no map | 272 |

Measured outside the workspace with the real `parse_log` on the real fixture,
three interleaved rounds a side — the micro-benchmark that first "explained"
the gap (a loop doing nothing but the author construction) undershot it 3×,
which is exactly why attribution ran on the real function.

`align` and wrap got faster for free. Both walk spans and line text, and both
moved from owned `String`s to compact `u32` ranges beside one buffer — better
locality, nothing else changed. Their ranges do not overlap across rounds, so
unlike `align`'s usual 13–27 % CV these two rows are real effects.

Diffing answers did not move: `./check.sh`'s `differs vs git` section ran on the
merged tree and matches the tolerance profile in the section above exactly.

### One hasher, everywhere

`FxHasher` was written for `parse_log`'s author map and stayed there. Every other
intern map in `core` was still on `HashMap`'s default SipHash — including the
**line** map, which is the hottest map in the application, and which additionally
started at zero capacity and rehashed its way up once per file.

```sh
cargo run -q -p gitten-git --example diffcheck --release . 6401fcd~4..6401fcd
```

94 files, 50,103 old lines, 50,434 new. Same binary configuration, same revspec,
`main` against the change, back to back:

| mode | before | after | Δ |
|---|---|---|---|
| `histogram` | 14.4 ms | 6.4 ms | **−56 %** |
| `patience` | 14.2 ms | 5.9 ms | **−58 %** |
| `myers` | 6.9 ms | 3.8 ms | −45 % |
| `ws-eol` | 32.1 ms | 13.3 ms | **−59 %** |
| `ws-change` | 44.5 ms | 20.7 ms | −53 % |
| `ws-all` | 34.0 ms | 18.9 ms | −44 % |

Per fixture, whole-file histogram diffs over both reconstructed sides:

| fixture | lines | before | after |
|---|---|---|---|
| `pr33933.diff` | 20,877 | 1.5 ms | 0.7 ms |
| `pr30698.diff` | 82,258 | 34.1 ms | 17.3 ms |
| `md.diff` | 94,614 | 52.1 ms | 24.3 ms |
| `pr30683.diff` | 715,406 | 45.7 ms | 20.9 ms |

**Answers are unchanged**, which is the only reason the number counts: `diffcheck`
reports the same changed-line counts *and the same hunk positions* against all six
git invocations, matching the tolerance profile in
[the differs section](#the-differs-against-git) exactly.

Half the differ, for a type alias. Two things are worth taking from it rather
than the number. The whitespace rows moved most because `KeyArena` was hashing
**twice** — SipHash over the key to get a `u64`, then SipHash over that `u64` to
place it — so the mode that normalises every line paid for it twice per line. And
those rows are still 2–3× `Exact`, because a whitespace key is interned twice over:
once by `KeyArena` into an `Arc<str>` and again by the line map. Having
`Whitespace::keys` yield ids directly removes a whole pass and is the next thing
here.

### `prepare`, across cores

A file is independent of every other file, so `prepare` was one core doing what
ten can. Workers pull the next file off an atomic counter rather than taking a
contiguous chunk each — files are wildly uneven and static chunking measured
**2.1×** on `md.diff` where stealing measures 6.3×, because one of that fixture's
229 files is most of its work.

```sh
cp fixtures/real/md.diff fixtures/big.diff
cargo run -q -p gitten-core --example bench --release   # the `prepare` line
```

Wall clock for the whole call, 10 workers, `main` against the change:

| fixture | files | lines | before | after | Δ |
|---|---|---|---|---|---|
| `md.diff` | 229 | 71,756 | 73.0 ms | **11.6 ms** | 6.3× |
| `pr30698.diff` | 1,398 | 50,604 | 68.6 ms | **12.9 ms** | 5.3× |
| `pr30683.diff` | 1,375 | 713,996 | 314.0 ms | **77.5 ms** | 4.1× |
| `pr33933.diff` | 35 | 20,831 | 5.6 ms | **2.1 ms** | 2.7× |

Two things the table does not say. **CPU time goes up**: `md.diff`'s intraline
sum moved 55.4 → 75.9 ms across ten workers, which is contention and allocator
pressure paid to get the wall clock down, and is the right trade for a load. And
**the numbers stop adding up** — `prepare` is wall clock, `intraline` and
`syntax` are summed across workers, so the example prints `×N cpu` next to them;
without it the pass reads as a broken measurement rather than a parallel one.

`parallel_and_serial_agree_exactly` compares the whole `Vec<File>` against the
serial path on a 40-file fixture whose largest file is forty times its smallest,
so results genuinely complete out of order. Rows address files by index, which is
why the guarantee is order-for-order identity and not just set equality.

A **single-file** diff gets nothing: the unit of work is a file. Stealing hunks
instead would fix that and needs the per-file timing accumulation to move.

### Acquisition peak, streamed vs collected

`pairs` built a `Vec<Pair>` holding both sides of every changed file at once,
which put back exactly the peak `BlobStream` was written to avoid. `each_pair`
hands each pair to `diff`, which diffs it and drops it, so the peak is one file's
content plus the whole edit script rather than every file's content plus the edit
script.

```sh
COLS=120 ROWS=40 /usr/bin/time -l \
  ./target/release/examples/dump diff ~/Projects/cmux HEAD~40..HEAD   # peak footprint
```

Peak memory footprint, `main` against the change:

| input | changed files | before | after |
|---|---|---|---|
| `cmux HEAD~40..HEAD` | 482 | 113 MB | **81 MB** |
| this repo, whole history | 94 | 24 MB | **21 MB** |

The win tracks how much of each file is context rather than change: a `FileDiff`
keeps only changed lines and their surroundings, so the 990 untouched lines of a
1,000-line file are read, compared, and freed — which only happens if the pair
they came in is freed too. `pairs` is kept for the test and `diffcheck`, which
want the list and are small enough that the pile is free.

### Why the line-text arena was reverted

The obvious next memory move — replace the 714k per-line `Arc<str>` with slices
into one arena per file — was built, measured, and reverted. See
[decision 0026](decisions/0026-line-text-is-not-the-memory-to-save.md); the
numbers are here.

A counting global allocator over the pipeline stages (`pr30683.diff --patch`,
714k lines) attributes the peak:

| stage | live | note |
|---|---|---|
| raw read | 27.9 MB | the patch text |
| parsed → `Vec<FileDiff>` | 108 MB | +80 MB: `Arc<str>` text, `DiffLine`s, vecs |
| `prepare` | 147 MB | +65 MB: **~1.06M `Box<[Token]>`/`Box<[Span]>`**, tokens, clipped text |

The arena did what it claimed — parse allocations **727,987 → 13,992 (−98 %)**,
byte-identical diffs vs git — and it still lost:

| path | metric | before | after |
|---|---|---|---|
| patch (`pr30683 --patch`) | RSS | 330 MB | 312 MB (−5.7 %) |
| **repo (`cmux HEAD~120..HEAD`)** | **RSS** | **149 MB** | **192 MB (+29 %)** |

The repository regression is the arena pinning its whole backing buffer: one
surviving context-line slice holds the entire file resident, un-freeing exactly
what the streaming change above releases. And the −5.7 % on the patch path shows
line text was never the fragmentation — the ~1.06M token/span boxes in `prepare`
are, and the arena does not touch them. That is the target a future memory pass
should measure against, not the line text.

### Topology

`shape`, on the two real repositories:

| repo | commits | merges | p50 lanes | p99 | max | rows at 1 lane |
|---|---|---|---|---|---|---|
| git/git | 82k | 25.8% | 126 | 226 | 280 | 0.9% |
| cmux | — | 16.2% | 9 | 70 | 73 | 7.2% |

### Binary and dependencies

| | |
|---|---|
| `gitten-shell`, release, before syntax highlighting | 12,916,304 bytes |
| after: scanner, theme, host, prepared, seams | 13,056,496 bytes (+123 KB) |
| new dependencies | none |
| `core` dependencies | none, and `[dependencies]` is empty |

### The terminal frontend

```sh
COLS=120 ROWS=40 ./dev dump diff --fixtures
COLS=120 ROWS=40 ./dev dump commits ~/Projects/git 82000
```

Load is `core`'s and is the same work in every frontend — the numbers above for
`prepare` are inside it. What is new is the frame, and the claim being measured is
that it does not depend on the size of the diff: it is 50 rows either way.
`FRAMES` sets how many repaints to average; the default is 50.

| view | rows | load | frame |
|---|---|---|---|
| `pr30683.diff`, unified | 740,383 | 473 ms | 15 µs |
| `pr30683.diff`, side-by-side | 973,394 | 532 ms | 16 µs |
| `md.diff`, unified | 74,467 | 110 ms | 12 µs |
| `md.diff`, side-by-side | 90,963 | 117 ms | 14 µs |
| `git/git`, 82k commits | — | 156 ms | 28 µs |

Re-measured at `FRAMES=200` when the mouse landed
([decisions/0022](decisions/0022-the-mouse-in-a-terminal.md)); the first run of
the table read 2–3 µs lower on the same fixtures and the same sizes, and none of
that is the scrollbar or the selection. Building with `Scrolling::scrollbar`
defaulted to `false` — so no `Screen::over` runs at all — measures 15 µs, 12 µs
and 29 µs on the three rows above, which is the same number back inside the
noise. A bar is 40 cells and a selection is `Selection::at` per visible row, two
integer comparisons against a cached range; neither is a function of the diff.

Ten times the rows costs nothing per frame, which is the point. The 12 µs is 40
rows × the run merge × a memcpy into the cell buffer, and nothing in it
allocates: `runs` walks the tokens and the spans together rather than collecting
their edges, the run-list buffer belongs to the caller, and a row's text is
sliced out of the line rather than copied. Collecting edges into a `Vec` per row
was the first version and cost 2 µs a frame — 14%, for an answer the sweep
already had.

The side-by-side row count is *higher* than unified's here, which looks wrong and
is not: `pr30683` is near-pure deletion, so almost every row has one side, and the
column is half the width so more of them wrap.

### The desktop opens its window before it acquires

Startup used to acquire, then open the window; an explicit repository launch
(`gitten commits .`, `gitten diff .`) now opens it first. `shell/src/main.rs`
takes `Startup::configure` — everything `go()` does but the acquisition, which
lives in `app/src/lib.rs` — and registers empty screens one generation below the
shell's, the sidebar panes in a loading shape (header label `STARTUP_LOADING`,
drawn as "loading"), the saved session row riding `pending_restore`. One
background wave — the same `refresh_stale` a repository switch rides, through the
existing per-pane `Refresh { load, apply }` machinery — acquires and fills
everything, and `finish_refresh` applies the restored scroll and schedules the
preview diff exactly as it does after a switch. A skeleton's sidebar panes draw
empty rows under that header for one frame (~100 ms); the honest-emptiness
convention — a read that has not landed must not draw as a clean tree — is
carried by the label, the same way the TUI's loading shapes work. Fixtures and
patches keep the synchronous `go()` road: in-process reads with no spawn floor to
defer against, and failures still print to stderr and exit.

```sh
GITTEN_START_QUIT=1 GITTEN_START_LOG=1 ./target/release/gitten-shell commits .
```

Quits the moment its first rows are drawn — a window appears and closes — so a
wall clock around the process is the GUI time-to-interactive. This repository,
release, the same M1 Pro, medians of 12 ABBA-interleaved runs a side (the two
coldest new-binary runs, ~440 ms, were post-rebuild dyld outliers):

| | previous binary | window first | Δ |
|---|---|---|---|
| GUI time-to-interactive | ~357 ms | ~282 ms | ≈ −21 % |

The `GITTEN_START_LOG` marks, same runs:

| mark | was | is |
|---|---|---|
| startup done | 107 ms | 80 ms |
| window callback | ~65 ms | 0.9 ms |
| first render | 213 ms | 147 ms |
| first rows drawn | 308–357 ms across runs | 278 ms |

The trade, because it changed behaviour: a repository that will not open — a bad
revspec, a path that is not a repository — used to `exit(1)` with a message on
stderr; now the window opens and the wave's failure lands in the window's
existing error band (`error_is_load`), and the process stays up. Scripting
against the GUI binary was never the supported door — the headless `cli` harness
and the TUI are. The bare launch (Finder, `open`, no arguments) keeps its old
shape: one cheap `git status` probe on the cwd decides, a repository opens —
now skeleton-first — and with none, recents or the picker open with the same
stderr as before. See
[decisions/0030](decisions/0030-window-before-acquisition.md).

## Not reproducible from this repo

These decided things, so they are recorded in full. Each was run in a throwaway
crate outside the workspace, because reproducing them means depending on ~20
tree-sitter grammar crates and `syntect`, and `core` has no dependencies on
purpose. The recipe is enough to rebuild the harness.

**Harness:** a binary crate with `tree-sitter 0.26`, `tree-sitter-highlight`,
grammar crates for rust/go/typescript/javascript/python/c/cpp/zig/lua/php/bash/
java/kotlin/swift/css/html/json/yaml/toml/md, and
`syntect 5.3 { default-features = false, features = ["parsing", "default-syntaxes", "regex-fancy"] }`
— the same syntect feature set `gpui-component` already pulls in. Corpus: the
`.rs` files of Zed's `gpui` crate for the shootout, and ~900 KB per language
collected from `~/Projects` and `~/.cargo` for the accuracy table.

### Engine shootout

85 files, 2.41 MB, 69,084 lines of real Rust:

| engine | throughput | lines/s | p50 per 28 KB file | p99 | init | RSS |
|---|---|---|---|---|---|---|
| table scanner | **212–227 MB/s** | 6.2 M | **77 µs** | 1.3 ms | 0 | +0 |
| tree-sitter, parse only | 12.5 MB/s | 358 k | 1.4 ms | 22 ms | 0.1 ms | +7.7 MB |
| tree-sitter, parse + query | 7.1–7.6 MB/s | 217 k | 2.2 ms | 34 ms | 2–21 ms per grammar | +8 MB |
| `tree-sitter-highlight` | 7.1 MB/s | 204 k | 2.4 ms | 38 ms | 17 ms | +8 MB |
| syntect, fancy-regex | 0.4 MB/s | 12.5 k | 39 ms | 674 ms | 0.9 ms | +15 MB |

syntect's own docs put Oniguruma at ~2× fancy-regex, so ~0.8 MB/s with the C
engine — still an order of magnitude behind tree-sitter.

Query compilation is per grammar and not trivial: rust 20.7 ms, cpp 20.6 ms,
zig 20.4 ms, python 6.1 ms, typescript 6.0 ms, c 3.4 ms, go 1.8 ms. Anything using
tree-sitter must compile queries lazily on first use, never at startup.

### The fragment penalty

The measurement that decided it. Same 85 files, then the same files with 12 of
every 40 lines kept, which is roughly the shape of a hunk:

| input | tree-sitter | spans/KB | ERROR nodes | scanner | spans/KB |
|---|---|---|---|---|---|
| whole files | 7.1 MB/s | 142.9 | 8 | 211.8 MB/s | 49.8 |
| 12 of every 40 lines | **2.8 MB/s** | **124.1** | 316 | 201.1 MB/s | 50.3 |
| 6 of every 40 lines | **2.6 MB/s** | **114.4** | 333 | 195.7 MB/s | 50.4 |

Error recovery costs 2.7× the time *and* loses a fifth of the highlighting. The
scanner does not move, because it never had parse context to lose.

### Binary cost of tree-sitter

Stripped binaries, each linking exactly one grammar, over a 0.32 MB baseline; the
shared engine derived from a five-grammar build:

| | |
|---|---|
| tree-sitter engine + query machinery, shared | ~1.55 MB |
| per grammar | go 0.21, js 0.40, python 0.45, c 0.60, zig 0.65, rust 1.08, typescript 1.36, **cpp 3.30** |
| five grammars together, measured | +5.26 MB |
| syntect + default syntaxes | +2.03 MB |

Enabling 10 tree-sitter language features on `gpui-component` cost 49 s of
incremental build and grew the binary by only 68 KB — because nothing called the
registry and the linker dropped the tables. The megabytes arrive when it is used.

### Per-language accuracy

tree-sitter as the oracle, comment and string agreement in bytes, ~900 KB per
language. "bleed" is bytes the scanner colours as comment or string that
tree-sitter says are neither — the failure that makes a diff unreadable.

| lang | MB/s | coloured | ts | cmt prec | recall | str prec | recall | bleed | worst run |
|---|---|---|---|---|---|---|---|---|---|
| json | 262 | 67% | 67% | — | — | 100% | 100% | 0.00% | 0 |
| yml | 239 | 50% | 89% | 87% | 100% | 100% | 51% | 0.00% | 0 |
| toml | 207 | 53% | 97% | 100% | 100% | 100% | 100% | 0.00% | 1 |
| h | 198 | 54% | 81% | 100% | 100% | 57% | 39% | 0.28% | 408 |
| sh | 163 | 59% | 81% | 96% | 98% | 97% | 80% | 0.82% | 412 |
| c | 161 | 40% | 69% | 100% | 100% | 75% | 97% | 0.62% | 449 |
| py | 156 | 62% | 72% | 100% | 100% | 100% | 100% | 0.00% | 0 |
| css | 155 | 26% | 77% | 100% | 100% | 95% | 47% | 0.08% | 682 |
| rs | 151 | 55% | 66% | 100% | 99% | 100% | 100% | 0.00% | 0 |
| java | 150 | 66% | 78% | 100% | 100% | 100% | 100% | 0.00% | 0 |
| js | 144 | 66% | 89% | 100% | 100% | 100% | 97% | 0.03% | 48 |
| zig | 143 | 40% | 73% | 100% | 100% | 97% | 100% | 0.04% | 21 |
| swift | 139 | 51% | 76% | 100% | 100% | 88% | 100% | 1.64% | 62 |
| cpp | 137 | 52% | 69% | 100% | 100% | 89% | 87% | 0.42% | 260 |
| ts | 128 | 50% | 76% | 100% | 100% | 100% | 99% | 0.00% | 0 |
| go | 106 | 50% | 74% | 100% | 100% | 100% | 100% | 0.00% | 0 |
| kt | 111 | 43% | 77% | 100% | 100% | 100% | 100% | 0.00% | 3 |
| lua | 87 | 47% | 79% | 100% | 79% | 100% | 100% | 0.00% | 0 |
| **html** | 86 | 18% | 30% | 100% | 100% | 82% | 98% | **2.46%** | 1011 |
| **php** | 141 | 50% | 47% | 91% | 100% | 61% | 100% | **9.18%** | 13711 |
| **md** | 190 | 23% | 20% | — | — | — | — | **21.81%** | 25701 |

Reading the failures: `h` and `c` string precision is char literals and
`#include <...>`, a misclassification with no bleed. `yml`, `css` and `sh` string
recall is unquoted scalars the scanner leaves plain. The three bold rows are the
model breaking — html needs `<script>` injections, php is an HTML host with
`<?php` islands, and md's numbers are partly an artifact of tree-sitter's own
markdown block query capturing almost nothing. Those three get no table; markdown
gets its own `Highlighter` instead.

Two rules came out of this table: `#` needing a word boundary took shell bleed
from 0.99% to 0.82%, and quotes needing a preceding `=` took html from 6.21% to
2.46%.

Ground-truth caveat worth knowing if you rebuild this: `tree-sitter-cpp` and
`tree-sitter-typescript` ship highlight queries that only *extend* their parent
language, so on their own they capture no comments and no strings at all. Both
must be concatenated with the c and javascript queries or the oracle reports
garbage — it did, at first, and made both languages look catastrophic.

### The ratios a theme is built to

```sh
cargo run -q -p gitten-core --example contrast --release          # all seven
cargo run -q -p gitten-core --example contrast --release light
```

A floor keeps a palette legible; *hierarchy* is what a reader learns, and the
hierarchy is a set of ratios rather than a set of colours. So the second through
seventh palettes were ported from the first by number — pick the hue, solve for the tint
that lands on dark's figure. Every row below is the same contrast function used on
the render path, against that theme's own context row unless it says otherwise:

| | dark | light | slate | gruvbox | catppuccin | tokyo-night | rose-pine |
|---|---|---|---|---|---|---|---|
| `file_bg` | 1.18 | 1.18 | 1.19 | 1.27 | 1.30 | 1.18 | 1.23 |
| `hunk_bg` | 1.09 | 1.05 | 1.05 | 1.01 | 1.06 | 1.06 | 1.04 |
| `added_bg` | 1.20 | 1.21 | 1.21 | 1.25 | 1.19 | 1.23 | 1.20 |
| `removed_bg` | 1.16 | 1.16 | 1.16 | 1.09 | 1.10 | 1.10 | 1.12 |
| `added_word_bg`, against its line | 2.09 | 1.29 | 1.29 | 1.85 | 1.80 | 1.61 | 1.55 |
| `removed_word_bg`, against its line | 1.62 | 1.18 | 1.17 | 1.46 | 1.46 | 1.30 | 1.40 |
| `absent_bg`, against the row opposite | 1.25 | 1.25 | 1.25 | 1.22 | 1.26 | 1.16 | 1.18 |
| `context_fg` | 7.15 | 7.20 | 7.15 | 5.30 | 7.37 | 8.10 | 5.48 |
| `added_fg` on an addition | 8.51 | 8.49 | 8.51 | 5.72 | 9.24 | 7.58 | 8.65 |
| `added_fg` on a *moved* addition | 8.01 | 7.96 | 8.09 | 5.60 | 8.73 | 7.50 | 8.10 |
| `gutter_fg`, before it is lifted | 2.05 | 2.04 | 2.06 | 2.26 | 2.46 | 1.74 | 3.42 |
| `chrome.dim` | 3.53 | 3.55 | 3.52 | 4.02 | 4.44 | 2.76 | 5.48 |
| `chrome.accent` | 9.11 | **5.20** | 9.14 | 8.69 | 9.27 | 6.79 | 10.77 |

Building them is not free and is not on any path that matters: `Theme::dark()` is
**16 µs** release, almost all of it `rebuild` resolving 12 classes across 8
surfaces, and `Host::new()` — which now builds eight themes, seven catalogued and
one active — is **270 µs**. That is what a theme pick costs, because a pick is a
rebuild of the host from the file. Measured with a 200-iteration loop over each
constructor.

Two numbers could not be carried across, and both are the same point about a light
background. **The accent is 5.2:1 rather than 9.1:1** — contrast against paper is
darkness, and an amber taken to 9:1 is a brown. **`absent_bg` is 1.51:1 from its
context row** where dark's is 1.04:1, because on paper there is no room left above
the background; the comparison that actually decides it — 1.25:1 against the row
opposite — is identical in all three.

### Contrast, before the fix

`contrast()` from `theme.rs` over the palette as it shipped, every token class
against every diff surface:

| kind | context | added | removed | added_word | removed_word |
|---|---|---|---|---|---|
| Comment | 2.86 | 2.38 | 2.47 | **1.15** | 1.50 |
| Keyword | 6.71 | 5.58 | 5.81 | 2.70 | 3.52 |
| Str | 7.85 | 6.53 | 6.80 | 3.16 | 4.11 |
| Type | 7.58 | 6.30 | 6.56 | 3.05 | 3.97 |
| Func | 11.74 | 9.76 | 10.16 | 4.73 | 6.15 |
| Heading | 15.21 | 12.64 | 13.16 | 6.13 | 7.97 |

1.15:1 is a grey smear on green, and it is what a screenshot showed. Lifting the
comment alone to clear 3.5 gave `#b7b3b0` — louder than the code around it — so
the changed-word backgrounds were darkened too, and then `#8f8a84` clears it.

The floor is now asserted for every class on every surface by
`every_token_is_legible_on_every_surface` in `theme.rs`, so this table cannot
regress silently.

### Chrome and furniture, before the fix

Same function, one layer out: `contrast()` over the palette as it shipped, for the
things that are *not* token text. Reproduce any row with
`gitten_core::theme::contrast(a, b)`.

The furniture — one hex literal, drawn on five row backgrounds:

| | context | added | removed | moved_added | moved_removed |
|---|---|---|---|---|---|
| `gutter_fg` `#4a4540` | **2.05** | 1.70 | 1.77 | **1.60** | 1.78 |
| resolved at `min_furniture` 3.0 | 3.31 | 3.10 | 3.23 | 3.27 | 3.23 |
| what it resolves to | `#686460` | `#706c68` | `#706c68` | `#777470` | `#706c68` |

The blend is 24 steps, so a resolved value overshoots the floor by up to a fifth of
a step — 3.31 where 3.0 was asked for. That is [0009](decisions/0009-contrast-resolution.md)'s
algorithm unchanged, and the reason the numbers are not all 3.00.

The surfaces, against a context row — every one of them a boundary somebody has to
see:

| | before | after |
|---|---|---|
| `diff.file_bg` | **1.048** — and the same value as `chrome.title_bg` | 1.176, plus a `diff.rule` hairline |
| `diff.hunk_bg` | 1.089 — *more* prominent than the file header above it | 1.051 |
| `chrome.title_bg` | 1.048 | 1.048, plus a `chrome.border` hairline |
| `chrome.status_bg` | 1.038 | 1.038, plus a hairline |

That is the whole argument for [0019](decisions/0019-the-strip-is-the-titlebar.md)
and the hairlines: nothing in a near-black palette is more than about 1.2:1 from
anything else, so a tint cannot carry an edge and a pixel can. `absent_bg` is the
case that proves it — pure black is 1.082:1 against a context row, so no value at
all would have made a "hole" read against the body of the file. What it actually
reads against is the *changed row opposite*, which is the only place it appears:
1.25:1 against an addition, 1.20:1 against a removal.

Two floors are asserted rather than tabulated —
`every_token_is_legible_on_every_surface` and
`a_line_number_clears_the_furniture_floor_on_every_surface` in `theme.rs`, plus
`a_file_header_is_a_step_and_a_hunk_header_is_not` for the hierarchy — so none of
this can regress silently.

### Intraline pair similarity

Dice coefficient over tokens, `2·LCS / (len_a + len_b)`, for every pair
`replace_pairs` produces in a fixture:

| fixture | pairs | below 0.4 | lowest legitimate pair |
|---|---|---|---|
| `pr30698.diff` | 9,447 | **0%** | 0.60 — `#define ZIG_DECL` → `#define RUST_DECL` |
| `pr30683.diff` | 396 | **15.6%** | junk: `/**` → `// Historical note: …` at 0.0 |

91.9% of `pr30698`'s pairs sit above 0.9. The floor is 0.4: below every legitimate
pair measured, above the junk. A test pins the 0.60 case so tightening the floor
cannot silently stop highlighting real renames.

### The exec→`main` gap, attributed

Every TTI number above has an unattributed slice at the bottom: the wall the
process spends before `main`, which no stage clock can see. Measured with a
throwaway harness outside the workspace (`/tmp/tti-attribution`): a hello-world
probe that prints at its first statement and exits — and, for a second mode,
calls `libc::_exit` to skip runtime teardown — plus a Python driver that spawns
each target over `os.posix_spawn` with stderr to a pipe and stamps every event
from one clock, the parent's monotonic. No cross-domain assumption; as a check,
the probe also prints realtime at its first statement, which agrees with the
monotonic reading within 0.5 ms. `exec→first line` below is the reading taken
just after the spawn syscall returns to the arrival of the target's first
stderr byte, so it is loader work plus one pipe write and nothing of the
driver. Five kinds of run, ABBA-interleaved with 1.0 s settles, medians of
eleven rounds a side.

```
python3 /tmp/tti-attribution/harness2.py 11 min hexit help tui shell
python3 /tmp/tti-attribution/analyze2.py                 # per-run attribution
```

The `tui` row is `GITTEN_START_LOG=1 ./target/release/gitten-tui commits .` on a
non-tty stdin — it exits at `could not take the terminal` after printing its
`gitten-start:` stages, so wall minus the printed stage sum is everything its
clock cannot see. The `shell` row is
`GITTEN_START_LOG=1 GITTEN_START_QUIT=1 ./target/release/gitten-shell commits .`
— a window appears and closes, as in [the desktop section](#the-desktop-opens-its-window-before-it-acquires).
Release binaries as of `27e15a0`, toolchain 1.97.1: `gitten-tui` 2.4 MB,
`gitten-shell` 16.1 MB, probe 0.43 MB. The machine was **not idle** — another
session was building in this tree during part of the sitting — so SDs are
quoted and the minima are the honest floors.

| target | exec→first stderr line | sd | best | realtime check |
|---|---|---|---|---|
| probe, 0.43 MB, full runtime | 4.7 | 7.7 | 2.0 | 4.3 |
| probe, `libc::_exit` | 3.6 | 1.6 | 2.0 | 2.9 |
| `gitten-tui`, 2.4 MB | 6.1 | 9.9 | 4.2 | — |
| `gitten-shell`, 16.1 MB | 7.1 | 4.8 | 6.0 | — |

Paired within the same round: tui − probe **+2.3 ms**, shell − probe **+5.0 ms**,
`_exit` − probe −0.3 ms (noise), so the probe is a fair floor. The tui row is
the first `gitten-start:` mark; the shell row is the `[start] main enter` mark,
which is the literal first statement of its `main`, so it *is* exec→`main`.

The TUI run, same eleven rounds, attributed (medians):

| slice | ms |
|---|---|
| spawn→exit wall | 47.9 |
| — exec→first mark | 6.1 |
| — marked stages, `args parsed`→`views built` | 41.9 |
| — tail: failure path, `exit(1)`, teardown | 0.6 |

So exec→`main` is **~5–6 ms** — the 6.1 less the unmarked head between `main`
and the first mark (`Startup::new`, two flag switches, the config-watcher
thread, sub-millisecond) — of which ~2.3 ms is dyld of the 2.4 MB binary over
the 0.43 MB floor, and ~2.5–3.5 ms is the fork/exec + libc start any process
pays. Teardown is **0.6 ms** on this path (`process::exit(1)` runs no
destructors; the probe's own full-runtime-minus-`_exit` difference, 1.2 ms,
bounds what runtime teardown could add). The wall the stage clock cannot see
here is 5.8 ms — **not** the 35–38 ms a TTI-vs-stage-sum comparison suggests.
That comparison's residue is not loader work: the non-tty run exits before the
frame, and on a real run the road between `views built` and the first frame is
itself marked (`first frame flushed`), i.e. app work the clock does see. The
non-tty wall also exceeds the 39.2 ms first-frame TTI above because this path
still acquires — 35–50 ms of `git` subprocess inside the stage sum under load.

The desktop run, same eleven rounds, attributed (medians; single-run minima in
brackets):

| slice | ms |
|---|---|
| spawn→exit wall (`GITTEN_START_QUIT`) | 325.5 [283.8] |
| — exec→`main enter` | 7.1 [6.0] |
| — `main enter`→`gpui application up` | 71.0 [58.1] |
| — gpui up→`first rows drawn` | 225.7 [191.3] |
| — quit + teardown after the last mark | 26.2 [21.3] |

exec→`main` is **7.1 ms** — 2.4 ms of it dyld over the probe floor, the rest
the common fork/exec cost. The 71 ms after `main enter` is almost entirely
GPUI's `application()` init: the shared startup before it is sub-millisecond
(`gitten-start: config loaded in 0.1–0.3 ms` on the same runs), and the mark's
low end, 64.6 ms, is where the ~64 ms quoted in the desktop section sits. The
26 ms after `first rows drawn` — the quit poll reaching process exit — is
measured as a gap, not decomposed.

**Is the shell's binary worth attacking for TTI? No.** Halving 16.1 MB saves at
most ~1.2 ms of the 2.4 ms dyld premium — under half a per cent of a 282 ms
TTI, and far under the ±14.5 % identical-binary noise this file already
records. The numbers point elsewhere: ~71 ms of GPUI init before the window
can exist and ~226 ms to first rows are 98 % of the road. The TUI's exec→first
is ~6 ms against a 39.2 ms first frame, and only ~2.3 ms of it is
size-dependent — a slimming pass buys less there too. What remains
unattributed in both clients is inside the marked stages (acquisition, the
window road) and the shell's 26 ms exit gap, which a future pass should
decompose before anyone optimizes it.

## Fixtures



`fixtures/dump.sh <repo> [count]` for real history, `fixtures/gen.sh <n> <m>` for
synthetic at any scale. Use both — synthetic tests scale, real tests *shape*, and
shape is where the crashes live.

| fixture | what makes it worth keeping |
|---|---|
| `~/Projects/git` (82k commits) | the tree stress case: 26% merges, 280 lanes, 37 octopus merges up to 10 parents, 7 roots |
| `pr30683.diff` | 714k lines, near-pure deletion, one 65k-token line |
| `pr30698.diff` | the zig→rust migration: heaviest intraline, 1,398 files |
| `pr33933.diff` | near-pure addition |
