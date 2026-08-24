# Syntax highlighting

A scanner that knows no languages, plus a table per language, plus a router that
picks an implementation per path.

```
   path + lines
        │
        ▼
   Highlighters ──── route "md"  ──► Markdown        line-oriented, its own code
   (router)     ──── route ...   ──► your impl       tree-sitter, LSP, anything
                └─── fallback    ──► Lexer
                                      │
                                      └─ Languages: path → Syntax table
                                                            │
                                                            ▼
                                                    lex() / lex_lines()
                                                            │
                                                            ▼
                                              Vec<Vec<Token>>  one list per line
```

## The scanner

`lex(src, syn, out)` makes one pass over bytes and appends non-overlapping
`Token`s in order. It understands exactly five shapes: line comments, block
comments, strings, numbers, words. Everything language-specific is data.

Per position it tries, in order: line-comment openers, block-comment openers,
string openers, digits, word characters. First-byte tables built with the
`Syntax` skip work no rule could do. The union table (`opens: [bool; 256]`)
skips the whole rule walk for any byte that cannot open anything — worth about
2× on every language measured — and per-category tables (`line_opens`,
`block_opens`, `string_opens`) let each attempt decline on its first byte
instead of walking its patterns; keywords get the same gate (`kw_first`) plus a
minimum-length check, so a word too short to be any keyword never reaches the
binary search.

The one non-obvious invariant: **a byte whose opener matched but whose rule then
declined must still advance the cursor.** An apostrophe in HTML prose, a `#`
inside `${x#y}`, the `/` that closed a comment. Three tests hung on this before
the fallback existed.

## The tables

```rust
Syntax::new()
    .line(&["//"])                                    // to end of line
    .block(&[("/*", "*/")]).nested_block()            // Rust nests, C does not
    .strings(&[("r#\"", "\"#", false, true),          // longest opener first
               ("\"", "\"", true, true)])             // (open, close, escape, multiline)
    .keywords(&["fn", "let", "match"])                // sorted here, binary searched later
    .capitalized_types()                              // Foo is a type, FOO_BAR a constant
    .call_heuristic()                                 // name before `(` is a call, after `.` a field
    .line_needs_boundary()                            // `#` only after whitespace
    .quote_after_eq()                                 // markup: a quote only after `=`
```

Registered by extension or by whole filename:

```rust
languages.register(&["rs"], syntax);
languages.register(&["toml", "Cargo.lock"], toml_syntax);
```

25 extensions ship. `for_path` tries the whole filename before the extension,
which is how `Cargo.lock` reaches a table at all. No table means no highlighting —
the honest answer for a language nobody has described.

### Two rules the measurements forced

Both are one-liners; both fix the worst failure their language had.

**`line_needs_boundary`** — `#` only opens a comment at line start or after
whitespace. Without it, `$#` and `${x#y}` paint the rest of every shell line.
Stray colouring in a shell corpus: 0.99% → 0.82%.

**`quote_after_eq`** — in markup a quote only opens a string directly after `=`.
Prose is full of apostrophes and none of them are strings. HTML: 6.2% → 2.5%.

### The heuristics, and what they cost

`capitalized_types` is off for C and Lua, where nearly every capitalised word is a
macro and the file would light up. `call_heuristic` lifted Rust coverage from 29%
to 45% of bytes for 12 MB/s. A word with no lowercase letter and more than two
characters is a constant, not a type — `MaybeUninit` against
`LIBUS_RECV_BUFFER_LENGTH`, while `T`, `E` and `IO` stay types.

## Why not a parser

Full numbers in [measurements.md](measurements.md); decision in
[decisions/0003](decisions/0003-scanner-over-tree-sitter.md). The short version:

| | scanner | tree-sitter | syntect |
|---|---|---|---|
| whole files | 104–262 MB/s | 7.1 MB/s | 0.4 MB/s |
| hunk-shaped fragments | unchanged | **2.6 MB/s, a fifth of spans lost** | — |
| binary cost | 0 | 1.5 MB + 0.2–3.3 MB per grammar | 2.0 MB |
| bytes coloured | 40–67% | 66–89% | — |

A diff hands you fragments, which is the input a parser is worst at: error
recovery is its slow path, and the recovery loses spans. The scanner has no parse
context to lose, so fragmentation costs it nothing measurable (49.8 → 50.4
spans/KB).

It pays for that in coverage and in semantic classes. A call is a name before `(`.
There is no scope, no resolution, no injection.

## Where the model breaks

Measured per language against tree-sitter as the oracle, comment and string
agreement in bytes, ~900 KB of real source each:

- **Sound** — 100% comment precision, ≤0.6% stray colouring: rust, go, java,
  kotlin, python, ts, js, json, toml, zig, c, c++, lua, css, swift, shell.
- **Under-colours, never lies** — yaml and css unquoted scalars, C char literals
  and `#include <...>`: recall gaps with no bleed.
- **Breaks** — html with inline `<script>`, php's `<?php` islands, markdown.
  These need injections, which need a parser.

The last group gets **no table rather than a wrong one**. Guessing produced the
worst mis-colouring of anything measured.

## Markdown, the second implementation

Markdown ships as a `Highlighter` of its own, not a table — the proof the trait is
worth having. Prose has no keywords, an apostrophe is not a string, and what is
worth colouring is structure. The table-driven attempt mis-coloured a fifth of
every file.

100 lines that walk lines instead of bytes: ATX and setext headings, fenced code
(the one piece of state that crosses lines), blockquotes, thematic breaks, list
markers, then inline code spans, emphasis, strong and links. Unclosed delimiters
are left as text — in a diff a line often *is* half a construct, and an unmatched
`*` is far more likely to be a bullet than the start of emphasis.

It emits `Heading`, `Strong`, `Emphasis` and `Link`, which is why those `Kind`s
exist in `core` rather than inside it: a theme has to be able to style them
without knowing which highlighter ran.

## Kinds

Twelve, indexed rather than matched so a theme lookup is one load:

```
Comment  Str  Number  Keyword  Type  Constant  Func  Property     ← code
Heading  Strong  Emphasis  Link                                   ← prose
```

Small on purpose. These are the classes a scanner can identify without a parse,
and a dense diff should not wear more colours than this.

## Multi-line constructs

`lex_lines` joins the lines, scans once, then splits the tokens back per line,
clipping any that cross a boundary. A doc comment or a multi-line string is
several rows in a diff and lexing each row alone would lose it.

An unterminated single-line string stops at its newline. A language whose strings
legitimately span lines (Rust) can run on, but only to the end of the text it was
given — one side of one hunk. The next hunk starts clean.

## Looking at it

```
cargo run -q -p gitten-core --example paint --release 40 .md
```

Paints `fixtures/big.diff` in 24-bit ANSI using the same highlighters, the same
`prepare` and the same theme the window uses, and prints the palette as a legend.
A bad table shows up in a second.
