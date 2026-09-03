# Perf gate

How to tell whether a change made anything slower, without fooling yourself.
The full methodology — ABBA interleaving, why the mean keeps VM-reclaim noise,
why debug numbers are meaningless — lives in `docs/measurements.md`. This is
the operator's card for the harness that encodes it.

## The one command

```sh
./fixtures/bench.sh --json --rounds 3 > before.json   # on main
./fixtures/bench.sh --json --rounds 3 > after.json    # on the branch
```

Compare the `median` figures by hand. There is no threshold and no gate that
compares two vintages for you — that is deliberate (see below).

Human mode drops `--json` and prints grep-friendly medians:

```sh
./fixtures/bench.sh --rounds 3
```

Options: `--rounds N` (1 is fine for a smoke run), `--frames N` (repaints each
frame average covers, default 50), `--settle S` (seconds between rounds,
default 1), `--fixtures DIR` (default `$GITTEN_FIXTURES`, else
`fixtures/small`, else `fixtures/`).

## What it measures

On the committed `fixtures/small` (3,000 commits, 5,577 diff lines, 36 files —
see `fixtures/manifest.toml`): core `bench` stages (log parse, lane assignment,
diff parse, `prepare` with intraline/syntax CPU split, `align`, `wrap`) plus
three terminal frames (diff unified, diff split, commits) at `COLS=120 ROWS=40`.
Rounds alternate which stage runs first and the reported figure is the median.
Always `--release`.

`small` is hermetic: deterministic output of `fixtures/gen.sh 3000 6000`
(seed 7), committed, no network, no `$HOME`. A cold clone with `HOME` unset
gets numbers:

```sh
env -u HOME ./fixtures/bench.sh --json --rounds 1 --settle 0
```

`bench` and `dump` read `fixtures/log.txt` / `fixtures/big.diff` by relative
path, so `bench.sh` swaps the chosen fixtures in under a stash-trap and
restores whatever was there on exit. Point `GITTEN_FIXTURES` at a scratch dir
and even the swap touches nothing committed:

```sh
GITTEN_FIXTURES=/tmp/scratch ./fixtures/gen.sh 1000 1000
GITTEN_FIXTURES=/tmp/scratch ./fixtures/bench.sh --rounds 1
```

Same override works for `fixtures/dump.sh`. Both scripts write temp files and
rename into place, so an interrupted run never leaves a half-written fixture.

## The `check.sh` section

`./check.sh` ends with `perf gate (advisory)`: one `--rounds 1` JSON run on
`fixtures/small` plus a schema/parse validation. It fails only when the
*harness* is broken (fixtures missing, `bench.sh` errors, JSON does not
parse) — never on a timing. `GITTEN_PERF=0` skips it,
`GITTEN_PERF_ROUNDS=N` sets the rounds. Existing sections are untouched: same
order, same behaviour, same exit codes.

## CI note

If this ever runs in CI, keep it advisory: record the JSON as an artifact,
fail only on harness errors. Never gate on a cross-vintage comparison —
different machines, different neighbours, different numbers. A regression is a
human reading `before.json` against `after.json` from the same machine in one
sitting, the way every table in `measurements.md` was produced.
