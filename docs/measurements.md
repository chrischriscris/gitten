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
PLAIT_STATS=1 ./target/release/plait-shell diff         # frame/heap overlay
```

`bench` and `shape` read `fixtures/big.diff` and `fixtures/log.txt`; `check.sh`
swaps each real fixture in and restores what was there.

### Load, per fixture

`prepare` is clip + intraline + syntax — everything between parsing and rows.

| fixture | diff lines | files | parse | prepare | of which syntax | tokens | scanned |
|---|---|---|---|---|---|---|---|
| `pr33933.diff` | 20,831 | 35 | 1.6 ms | 8.0 ms | 6.5 ms | 32,236 | 115 MB/s |
| `pr30698.diff` | 50,604 | 1,398 | 6.2 ms | 98.9 ms | 35.6 ms | 122,237 | 67 MB/s |
| `pr30683.diff` | 713,996 | 1,375 | 57.1 ms | 288.7 ms | 237.3 ms | 1,330,580 | 114 MB/s |

`pr30698` is the outlier because intraline dominates it (57.5 ms): it is the
zig→rust migration, so nearly every line is a near-identical rewrite of another —
the worst case for a quadratic word diff and exactly why it is a fixture.

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
