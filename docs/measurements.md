# Measurements

Every number quoted anywhere in these docs, with what produced it. A figure that
cannot be reproduced is folklore; where a measurement is *not* reproducible from
this repository today, it says so and gives the recipe.

**Machine for everything below:** Apple M1 Pro, 10 cores, macOS 26.5.2,
rustc 1.97.1, `--release`. Never read any of these off a debug build — `cargo run`
without `--release` is a different, much slower binary, and the title bar says so.

## Reproducible from this repo

```
./check.sh                                              # all of the below, plus tests
cargo test -p plait-core                                # correctness, sub-second
cargo run -q -p plait-core --example bench   --release   # load timings, per fixture
cargo run -q -p plait-core --example shape   --release   # topology statistics
cargo run -q -p plait-core --example verify  --release   # lane invariants
cargo run -q -p plait-core --example paint   --release   # the diff view, in ANSI
cargo run -q -p plait-git  --example diffcheck --release [REPO] [REVSPEC]
                                                        # differs, against git's own answer
PLAIT_STATS=1 ./target/release/plait-shell diff         # frame/heap overlay
```

`bench` and `shape` read `fixtures/big.diff` and `fixtures/log.txt`; `check.sh`
swaps each real fixture in and restores what was there.

### The differs, against git

`git/examples/diffcheck.rs`, run for four inputs by `./check.sh`'s `differs vs
git` section. **A minimal edit script has exactly one length, so Myers must match
`git diff --minimal` exactly** — that is the correctness check, not a benchmark.
Hunk *offsets* are deliberately not compared: git runs `--indent-heuristic` by
default, which slides a hunk to an equivalent, more readable position, so the
counts agree while the hunk count occasionally differs by one.

Changed lines, ours against git's, on this machine:

| input | files | old+new lines | our histogram | git `--histogram` | our myers | git `--minimal` |
|---|---|---|---|---|---|---|
| this repo, `HEAD~4..HEAD` | 19 | 25,587 | +1693 −46 | +1693 −46 | +1693 −46 | +1693 −46 |
| this repo, whole history | 49 | 30,216 | +9633 −315 | +9633 −315 | +9625 −307 | +9625 −307 |
| `cmux`, `HEAD~5..HEAD` | 36 | 622,288 | +2264 −224 | +2264 −224 | +2264 −224 | +2264 −224 |
| `git/git`, `HEAD~5..HEAD` | 18 | 40,753 | +527 −36 | +527 −36 | +527 −36 | +527 −36 |

Every algorithm agrees with git on every input, except our `patience`, which is
**+4 changed lines in 9,938** on the whole-history diff. Not a bug and not
tolerated silently: ours is patience's *idea* — anchor only on lines appearing
once — through the histogram machinery, where git's `--patience` takes the longest
increasing subsequence of all unique-line matches at once. `diffcheck` flags a
drift past 1%.

Time, same inputs, ours against the `git` process it replaced. The comparison is
unfair in git's favour on nothing and in ours on process startup, so read it as an
order of magnitude and not as a factor:

| input | our histogram | our myers | `git diff --histogram` |
|---|---|---|---|
| this repo, `HEAD~4..HEAD` | 1.3 ms | 1.1 ms | 15.7 ms |
| this repo, whole history | 2.1 ms | 3.1 ms | 19.8 ms |
| `cmux`, `HEAD~5..HEAD` | 28.6 ms | 23.5 ms | 53.0 ms |
| `git/git`, `HEAD~5..HEAD` | 2.4 ms | 1.6 ms | 22.4 ms |

Acquisition is separate and is two processes regardless of file count: 25–32 ms on
this repository, 96 ms for cmux's 622k lines. One exception worth knowing about —
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

### The cost of a pick

Both title-bar controls rebuild rather than re-render, so what they cost is made
of numbers already in this file:

| control | what it re-runs | typical | pathological fixture |
|---|---|---|---|
| layout | `prepare` + row build | 8 ms | 247 ms |
| algorithm | acquisition + diff + `prepare` + row build | 35–140 ms | — |

Layout re-runs the pipeline from stage 3 against the parsed diff the view is
holding, so it is the `prepare` column of the table below. Algorithm has to
acquire again — 25–110 ms of `git diff --raw` and `cat-file` from the table above,
plus 1–29 ms of diffing — because it changes what the diff *is*.

The pathological column is blank for algorithm on purpose: the 714k-line fixtures
are `.diff` files, which have no algorithm to change. That is also why the control
is inert for them.

On a click both are fine. Neither would be on a key held down, which is the other
reason the algorithm is a menu and only the layout is bound to `s`.

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

### Topology

`shape`, on the two real repositories:

| repo | commits | merges | p50 lanes | p99 | max | rows at 1 lane |
|---|---|---|---|---|---|---|
| git/git | 82k | 25.8% | 126 | 226 | 280 | 0.9% |
| cmux | — | 16.2% | 9 | 70 | 73 | 7.2% |

### Binary and dependencies

| | |
|---|---|
| `plait-shell`, release, before syntax highlighting | 12,916,304 bytes |
| after: scanner, theme, host, prepared, seams | 13,056,496 bytes (+123 KB) |
| new dependencies | none |
| `core` dependencies | none, and `[dependencies]` is empty |

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
