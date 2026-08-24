# gitten

A desktop git client with lazygit's keyboard model. History and diffs, written
in Rust on [GPUI](https://github.com/zed-industries/zed), driven almost
entirely from the keyboard. The name is what the graph gutter draws: strands
weaving past each other.

<!--
  Screenshots wanted here: the commit list on a repository with a real
  history, and a side-by-side diff. Nothing staged before then — a screenshot
  of an empty repo sells nothing.
-->

## How this was built

**gitten is fully AI-written — every line of Rust, every decision record, this
README.** A human sets direction, reviews, and deletes what does not survive;
coding agents do the writing. That is stated up front rather than left to be
discovered, because the project makes two claims about it:

- **It does not show in the usual places.** Every design choice has a record
  in [docs/decisions/](docs/decisions/) — what was tried, what the numbers
  said, what it would take to revisit — and every number quoted anywhere in
  the docs carries the command that reproduces it
  ([docs/measurements.md](docs/measurements.md)).
- **It is checked, not trusted.** The diff algorithms are compared hunk by
  hunk against git itself; the themes are asserted legible by tests; the
  pipeline is benchmarked at synthetic scale and against pathological real
  fixtures.

Judge it accordingly.

## Status

Pre-release, and honest about it:

- **Reads only.** Browsing history and reading diffs work. Staging,
  committing, pushing and rebasing do not exist yet.
- **macOS binaries only.** The source is kept portable on purpose — no
  `cfg(target_os)` anywhere, no macOS-only crate — but nobody has compiled it
  on Linux, so portability is a property of the source and not of a binary.
- **No releases.** Build from source (below), and expect config-format churn
  before 0.1.
- The rest of what is missing — extension loading, panes, a diff cache,
  `gix` reads — is kept current in
  [docs/architecture.md](docs/architecture.md#not-built-yet).

## What works

| | |
|---|---|
| Commit list | topo-order graph gutter, one colour per branch, capped at 12 lanes with the overflow drawn honestly |
| Diff algorithms | histogram, patience and Myers written from scratch in `core` (a crate with zero dependencies), verified against git's own changed lines and hunk positions |
| Layouts | unified and side-by-side, cycled live with `s`; wrapping with `w` adds rows, never taller ones |
| Intraline | word-level second pass over changed pairs; moved blocks flagged beside their kind, three-line minimum |
| Syntax highlighting | a hand-written scanner over twenty-odd languages, chosen over tree-sitter on measurement |
| Markdown diffs | rendered as blocks; tables wider than the window are squeezed per column rather than broken across rows |
| Themes | three shipped palettes, contrast floors asserted by test, editable live in `gitten.toml` |
| Config | `gitten.toml` hot-reloads on the next frame — colours, font, algorithm, layouts, keybindings |
| Scale | git/git's 82k-commit history and a 714k-line pull request are fixtures, not stress tests |

Three frontends share one pipeline: the GPUI window (the product), a terminal
client (`gitten-tui`), and a browser proof (`gitten-web`) whose whole job is to
keep the boundary honest. Anything two of them need lives in `core`.

## Numbers

Release build, Apple M1 Pro. Each row is reproduced by a command in
[docs/measurements.md](docs/measurements.md):

| | |
|---|---|
| frame cost independent of diff size | 15 µs/frame at 740k rows (terminal client) |
| git/git, 82k commits, loaded | 156 ms |
| our histogram vs spawning `git diff --histogram` | 3.2 ms vs 31 ms, same input |

## Building

You need macOS, [rustup](https://rustup.rs) (the toolchain is pinned in
`rust-toolchain.toml`, tracking whatever Zed pins), and a `git` binary on
your `PATH`. The first build compiles GPUI out of Zed's tree — several
minutes, once.

```sh
git clone https://github.com/chrischriscris/gitten && cd gitten
./dev desktop commits               # the window, on this repository's history
./dev desktop diff . HEAD~2..HEAD   # or a diff on any revspec
```

`desktop` and `web` rebuild and relaunch on save. Everything else:

```sh
./dev tui    diff . HEAD~2..HEAD    # the terminal client
./dev web    diff --fixtures        # the browser proof; prints a URL
./dev dump   commits ~/src/somerepo # one frame on stdout, timing on stderr
./dev check                         # everything headless: tests + benchmarks
./dev config > gitten.toml           # a complete, correct starting config
./dev bundle                        # target/gitten.app — icon and all
```

Debug builds and the stats overlay are the defaults, because that is the loop
you iterate in. Frame *timings* are meaningless outside `--release`; the row
and cell counts are still worth watching.

Headless checks, if you would rather not run the script:

```sh
cargo test -p gitten-core     # correctness, sub-second
cargo run -q -p gitten-git --example diffcheck --release . HEAD~50
                             # our differs against git's own answer
```

## Keys

lazygit's model: one key, one verb. The defaults:

| | |
|---|---|
| `j` / `k` or arrows | move · `ctrl-d` / `ctrl-u` page · `g` / `G` jump to top and bottom |
| `s` / `w` | cycle layout / cycle wrapping, live |
| `T` | cycle theme |
| drag, `y`, `ctrl-a` | select, copy, select all (in the terminal, a drag copies on release) |
| `q` / `?` | quit · key help — the terminal derives its help panel from the same `[keys]` table |

A binding is data, not a `match`: every default lives in `core::command`,
every client reads the same `[keys]` table in `gitten.toml`, and the help
panel is derived from it rather than written by hand.

## How it fits together

One rule holds the shape: **`core/` never knows a UI exists.** No GPUI, no
I/O, an empty `[dependencies]` — which is why it compiles in a second and its
tests need no window. `gitten-git` is the only crate that talks to a
repository (writes through the `git` binary today, reads meant for `gix`);
`gitten-app` holds `gitten.toml` and the command line; each client is drawing
and input, and nothing else.

```
gitten-core                                   zero deps — differs, graph, rows, keys, themes
gitten-git · gitten-app                        the only git boundary · gitten.toml and the cli
gitten-shell │ gitten-tui │ gitten-web │ yours  the window │ the tty │ a browser proof │ next
```

Start reading at [docs/README.md](docs/README.md). `AGENTS.md` holds the
philosophy — three rules and the tie-break between them — and is short on
purpose.

## Contributing

Issues are open and read. Before opening a PR, know how this repository runs:

- `AGENTS.md` is the constitution. It is a few hundred words; read it.
- If a [decision record](docs/decisions/) covers what you are changing, the
  record wins until you have numbers that beat it.
- `core/` stays dependency-free and UI-free. A change that needs either
  belongs in another crate.
- Performance claims arrive with the command that measured them, and
  `./check.sh` passes.

AI-assisted contributions are welcome and expected — nearly all of this tree
was written that way. What is not welcome is unreviewed output: a PR is read
like any other, and the bar is identical whether the typing was done by a
person or a model.

## Licence

Dual-licensed under either of

- MIT ([LICENSE-MIT](LICENSE-MIT))
- Apache License 2.0 ([LICENSE-APACHE](LICENSE-APACHE))

at your option. Unless a contribution states otherwise, it is licensed under
both, per Apache-2.0 §5.
