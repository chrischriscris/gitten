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

`./check.sh` has two advisory sections at the end. The first, `perf gate (advisory)`,
is one `--rounds 1` JSON run on `fixtures/small` plus a schema/parse validation.
The second, `tti (terminal, advisory)`, is the suite below on this repo, 3
rounds. Both fail only when the *harness* is broken (fixtures missing,
`bench.sh` errors, JSON does not parse, the example cannot run) — never on a
timing. `GITTEN_PERF=0` skips the first, `GITTEN_PERF_ROUNDS=N` sets its
rounds; `GITTEN_TTI=0` skips the second, `GITTEN_TTI_ROUNDS=N` sets its
rounds. Existing sections are untouched: same order, same behaviour, same exit
codes.

## Time to interactive

`bench.sh` measures the load pipeline; `tti` measures the road to the first
thing a person can *use*, in both real clients. On this repository:

```sh
cargo build -q --release -p gitten-tui                 # the binary it times
cargo run -q -p gitten-tui --example tti --release .   # single side, terminal only
```

- The **terminal** number is spawn → `first frame flushed` on a private pty —
  the stage the list the launch asked for has been interactive since
  (`GITTEN_START_LOG=1` puts the stage marks on stderr, which rides the pty).
  `q` on the pty master ends each run. `spawn → startup frame flushed` adds
  the deferred sidebars and preview; binaries older than the deferral never
  print it and its absence is reported, not an error.
- The **desktop** number is the wall clock around `target/release/gitten-shell`
  under `GITTEN_START_QUIT=1` — the client quits itself at the first rows. A
  window does appear, briefly; that is the measurement. The side is skipped
  with a note when the binary is missing, and `GITTEN_TTI_SHELL=0` turns it
  off (check.sh does, to stay windowless).

Against another vintage — the only comparison that means anything:

```sh
GITTEN_BASELINE=$OLD/target/release/gitten-tui \
GITTEN_BASELINE_SHELL=$OLD/target/release/gitten-shell \
cargo run -q -p gitten-tui --example tti --release .
```

Rounds are ABBA-interleaved with settle gaps, the starting side flips every
round, one warmup per side is discarded, and the reported figure is the
median — the discipline of `docs/measurements.md` in full (`ROUNDS`, default
7; `SETTLE` seconds, default 1). Without a baseline, single-side medians and
no comparison. `--json` (or `GITTEN_FORMAT=json`) prints schema
`gitten.tti/1`: a median and its samples per figure per side, `deltaPct` for
the current-vs-baseline comparison. `deltaPct` is the entire extent of what
the suite concludes. The recorded baseline — today's figures, machine and
commands — lives in
[measurements.md](measurements.md#time-to-interactive-as-recorded); a future
pass measures against it by pointing `GITTEN_BASELINE` at a build of this
vintage, not by re-reading these paragraphs.

The suite advises and never gates. The one enforcement is opt-in:
`GITTEN_TTI_MAX_FIRST_FRAME_MS`, `GITTEN_TTI_MAX_FILLED_MS` and
`GITTEN_TTI_MAX_SHELL_MS` — set one and a median past it exits non-zero.
Unset, every run exits 0. Two structural tests in `tui/src/main.rs` pin the
ordering the number depends on — the skeleton defers the startup loads, a
fixture launch defers nothing — because a timing that drifts is read here, but
a load that moved back into `App::new` is a regression that no round would
explain.

## CI note

If this ever runs in CI, keep it advisory: record the JSON as an artifact,
fail only on harness errors. Never gate on a cross-vintage comparison —
different machines, different neighbours, different numbers. A regression is a
human reading `before.json` against `after.json` from the same machine in one
sitting, the way every table in `measurements.md` was produced.
