# Machine-readable tools (`--json`)

Every headless tool speaks a human dialect by default and one JSON object with
`--json` (or `GITTEN_FORMAT=json` in the environment). The flag may sit
anywhere in the argument list; the environment form exists for runners that
cannot add flags. Stdout is the machine contract: exactly one JSON object and
nothing else. Timings, warnings and the `WORST` human lines stay on stderr or
disappear; failures become `{error, code, hint}` on stderr with a nonzero
exit.

```sh
cargo run -q -p gitten-tui --example dump -- diff . HEAD~4..HEAD --json
GITTEN_FORMAT=json cargo run -q -p gitten-core --example bench
cargo run -q -p gitten-git --example diffcheck --release . HEAD~4..HEAD --json
```

All objects are hand-written, no serde — `core` has no dependencies and that
is the rule, so each example carries the same ~15-line escaper `web/src/json.rs`
documents. Numbers that measure time are milliseconds with three decimals
(`loadMs`); ratios carry four (`ratio`); a machine does its own rounding.

## The environment, typed

`app::env` parses every knob once, so the tools do not each re-decide what
`COLS=abc` means. Unparseable numbers fall back to the default rather than
failing; unknown names (`LAYOUT`, `WRAP`, `THEME`) fall back with a warning on
stderr in both modes.

| variable | default | meaning |
|---|---|---|
| `COLS` | 100 | `dump` screen width in columns |
| `ROWS` | 40 | `dump` screen height in rows, status bar included |
| `LAYOUT` | `unified` | `dump` presentation to paint |
| `WRAP` | host default | `dump` wrap registry entry |
| `THEME` | `dark` | `dump`, `paint` palette |
| `AT` | 0 | `dump` rows scrolled down before painting |
| `FRAMES` | 50 | `dump` repaints averaged into `frameMs` |
| `WRAP_COLS` | 150 (`bench`), 100 (`paint`) | wrap budget in columns |
| `WORST` | unset | `diffcheck`: also report the worst files per algorithm |
| `GITTEN_STATS` | off (`0` off, anything else on) | frame/row/heap readout |
| `GITTEN_START_LOG` | off (same rule) | startup-stage timings on stderr |
| `GITTEN_FORMAT` | unset | `json` selects machine output, like `--json` |
| `GITTEN_CONFIG` | see `config::path` | which `gitten.toml` to read |

## `gitten.dump/1` — one frame, as data

`cargo run -q -p gitten-tui --example dump -- [diff|commits] ... [--json]`

| field | meaning |
|---|---|
| `schema` | `"gitten.dump/1"` |
| `view` | `"diff"` or `"commits"` |
| `source` | `"REPO REVSPEC"`, or the `--fixtures` / `--patch FILE` / `stdin` spelling that chose the data |
| `cols`, `rows`, `at`, `frames` | the resolved geometry and repaint count |
| `layout`, `wrap`, `theme` | the resolved registry names actually painted with |
| `loadMs` | building the view: `Diff::new` + resize + scroll |
| `frameMs` | one repaint averaged over `frames` — build `--release` before believing it |
| `rowsTotal` | `view.rows()` for a diff, `view.len()` for commits |
| `status` | the same string the app's own bar would show |

Failures: `code` is `usage` (no such view, `--patch` without a file, a patch
handed to `commits`) or `acquire` (the repository or revspec did not resolve)
or `io` (a fixture/patch file did not read); `hint` says what to try.

## Core bench tools

All carry `{schema, tool, version: 1}` first.

- `gitten.bench/1` (`core --example bench`): fixture-scale pipeline timings
  over `fixtures/log.txt` and `fixtures/big.diff`. `commitReadMs`,
  `commitParseMs`, `lanesMs`, `widestLanes`; `diffLines`, `diffFiles`,
  `replacePairs`, `diffReadMs`, `diffParseMs`, `intralineMs`; `prepareMs` with
  the CPU-side `prepareIntralineCpuMs` / `prepareSyntaxCpuMs` (summed across
  `prepareThreads` workers — wall clock and CPU deliberately do not add up),
  `tokens`, `bytes`, `mbPerSecPerCore`; `alignMs`, `alignRows`,
  `alignPctOfLines`, `alignPaired`, `alignNsPerRow`; `wrapMs`, `wrapRows` at
  `wrapCols`, `wrapXLines`, `wrapNsPerLine`, `wrapRejected`; `markdownMs` and
  friends, or `"markdownMs": null` when the fixture holds no Markdown.
- `gitten.shape/1` (`core --example shape`): topology over
  `fixtures/log.txt`. `commits`, `parents` (counts keyed by parent count:
  `{"1": 60512, ...}`), `lanesP50`, `lanesP99`, `lanesMax`,
  `rowsAtOneLanePct`.
- `gitten.contrast/1` (`core --example contrast [THEME]`): every ratio the
  human tables print, as `themes: [{name, minContrast, minFurniture, checks:
  [{label, ratio, floor, pass}]}]`. Syntax rows are flattened to one check per
  kind per surface (`"Keyword on Context (lifted)"` when `readable` moved it)
  plus a `"Kind raw"` record with floor 0. An unknown theme is a failure here,
  where human mode only warns.
- `gitten.paint/1` (`core --example paint [ROWS] [PATH-FILTER]`): the same
  selection the ANSI frame would draw, counted. `theme`, `wrapCols`,
  `budget`, `filter`, `files: [{path, adds, dels, lines}]` (every file in the
  fixture, filtered or not — `lines` is the file's total), `rowsPrinted` under
  the budget and filter.

## `gitten.diffcheck/1` — the differs against git

`cargo run -q -p gitten-git --example diffcheck --release [REPO] [REVSPEC] [--json]`

Top level: `repo`, `revspec`, `acquireMs`, `files` (acquired), `oldLines`,
`newLines`, `binary`, then `modes` — one entry per algorithm with `name`,
`flags` (git's side), `oursAdds/oursDels/oursHunks/oursMs`,
`theirsAdds/theirsDels/theirsHunks/theirsMs`, `drift` (signed changed-line
delta, ours minus git), `verdict` (the human sentence), `hunkNote`,
`mismatches`, and `files: [{path, ours, theirs, hunkPositionsMatch}]` where
`ours`/`theirs` are changed-line counts and the match compares every
`@@ -a,b +c,d @@` position. `summary: {files, mismatches}` closes the object;
`files` there counts the non-binary files actually compared. With `WORST=1`
each mode also carries `worst: [{path, delta, ours, theirs}]`, worst first, at
most six — the same rows the human mode prints — and the array is present but
empty when every file agreed, so "no drift" reads differently from "not asked". Exit status is 1 when
`mismatches` is nonzero, in both modes, and the JSON still prints: a failing
gate is still a readable answer.

## Reproducing the numbers

- Run from the repository root: the examples read `fixtures/` by relative path.
- Timings mean nothing in a debug build — `--release` for `frameMs`,
  `prepareMs` and the diffcheck columns, exactly as the window's overlay says.
  Counts, ratios and positions are build-independent.
- `diffcheck` shells out to `git` six times per mode (plus acquisition); the
  `theirsMs` column is process-spawn dominated and not a benchmark.
- `bench`'s wrap section answers a resize: `WRAP_COLS=80` for a narrow window.
- `dump`'s `frameMs` is repaint cost with the overlay's forced redraw; an idle
  app draws nothing, so this is ceiling throughput, not sitting-still cost.
