#!/usr/bin/env bash
# Perf regression harness: core bench + terminal frames, human text or JSON.
#
#   ./fixtures/bench.sh                 human-readable, grep-friendly
#   ./fixtures/bench.sh --json          {"schema":"gitten.bench/1",...} on stdout
#
#   --rounds N       bench rounds per stage (default 3; 1 is fine for a smoke run)
#   --fixtures DIR   dir holding log.txt + big.diff (default below)
#   --frames N       repaints each dump averages over (default 50)
#   --settle S       seconds between rounds (default 1)
#
# Fixture default: $GITTEN_FIXTURES, else fixtures/small when it exists, else
# fixtures/. `small` is committed and hermetic — no network, no $HOME — so a
# cold clone with HOME unset still gets numbers.
#
# bench and dump read fixtures/log.txt and fixtures/big.diff by relative path,
# so the chosen fixtures are swapped in under the same STASH-trap pattern
# check.sh uses and whatever was there is restored on exit. Nothing here
# mutates a fixture in place.
#
# Rounds alternate which stage runs first (ABBA, starting side flipped every
# round) with a settle gap between them, and the reported figure is the median:
# back-to-back runs share VM-reclaim noise, and the mean would keep it. See
# docs/measurements.md for why. Always --release: debug timings are meaningless.
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

JSON=0
ROUNDS=3
FRAMES=50
SETTLE=1
FIXDIR="${GITTEN_FIXTURES:-}"
while [ $# -gt 0 ]; do
  case "$1" in
    --json) JSON=1; shift ;;
    --rounds) ROUNDS="${2:?}"; shift 2 ;;
    --fixtures) FIXDIR="${2:?}"; shift 2 ;;
    --frames) FRAMES="${2:?}"; shift 2 ;;
    --settle) SETTLE="${2:?}"; shift 2 ;;
    -h|--help) sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "bench.sh: unknown flag $1" >&2; exit 2 ;;
  esac
done
[ -z "$FIXDIR" ] && { [ -f fixtures/small/log.txt ] && FIXDIR=fixtures/small || FIXDIR=fixtures; }
case "$ROUNDS" in ''|*[!0-9]*|0) echo "bench.sh: --rounds must be >= 1" >&2; exit 2 ;; esac
case "$FRAMES" in ''|*[!0-9]*|0) echo "bench.sh: --frames must be >= 1" >&2; exit 2 ;; esac
case "$SETTLE" in ''|*[!0-9]* ) echo "bench.sh: --settle must be >= 0" >&2; exit 2 ;; esac
[ -f "$FIXDIR/log.txt" ] || { echo "bench.sh: no log.txt in $FIXDIR" >&2; exit 2; }
[ -f "$FIXDIR/big.diff" ] || { echo "bench.sh: no big.diff in $FIXDIR" >&2; exit 2; }

log() { printf '%s\n' "$*" >&2; }

STASH=$(mktemp -d)
trap '[ -f "$STASH/log.txt" ] && /bin/cp -f "$STASH/log.txt" fixtures/log.txt || rm -f fixtures/log.txt
      [ -f "$STASH/big.diff" ] && /bin/cp -f "$STASH/big.diff" fixtures/big.diff || rm -f fixtures/big.diff
      rm -rf "$STASH"' EXIT
[ -f fixtures/log.txt ]  && /bin/cp -f fixtures/log.txt  "$STASH/"
[ -f fixtures/big.diff ] && /bin/cp -f fixtures/big.diff "$STASH/"
/bin/cp -f "$FIXDIR/log.txt" fixtures/log.txt
/bin/cp -f "$FIXDIR/big.diff" fixtures/big.diff

log "── building (release) ──"
cargo build -q --release --example bench -p gitten-core >&2 \
  || { log "bench.sh: core bench build failed"; exit 1; }
cargo build -q --release --example dump -p gitten-tui >&2 \
  || { log "bench.sh: tui dump build failed"; exit 1; }

TMP=$(mktemp -d)
trap '[ -f "$STASH/log.txt" ] && /bin/cp -f "$STASH/log.txt" fixtures/log.txt || rm -f fixtures/log.txt
      [ -f "$STASH/big.diff" ] && /bin/cp -f "$STASH/big.diff" fixtures/big.diff || rm -f fixtures/big.diff
      rm -rf "$STASH" "$TMP"' EXIT

run_core() { # run_core <round>
  ./target/release/examples/bench >"$TMP/core.$1.out" 2>"$TMP/core.$1.err" \
    || { log "bench.sh: core bench round $1 failed"; cat "$TMP/core.$1.err" >&2; exit 1; }
}
run_frames() { # run_frames <round>
  COLS=120 ROWS=40 FRAMES="$FRAMES" ./target/release/examples/dump diff --fixtures \
    >"$TMP/fu.$1.out" 2>"$TMP/fu.$1.err" \
    || { log "bench.sh: dump diff round $1 failed"; exit 1; }
  COLS=120 ROWS=40 FRAMES="$FRAMES" LAYOUT=split ./target/release/examples/dump diff --fixtures \
    >"$TMP/fs.$1.out" 2>"$TMP/fs.$1.err" \
    || { log "bench.sh: dump split round $1 failed"; exit 1; }
  COLS=120 ROWS=40 FRAMES="$FRAMES" ./target/release/examples/dump commits --fixtures \
    >"$TMP/fc.$1.out" 2>"$TMP/fc.$1.err" \
    || { log "bench.sh: dump commits round $1 failed"; exit 1; }
}

r=1
while [ "$r" -le "$ROUNDS" ]; do
  # ABBA: the starting side flips every round, so VM-reclaim noise from one
  # stage does not always land on the same other stage.
  if [ $((r % 2)) -eq 1 ]; then run_core "$r"; run_frames "$r"; else run_frames "$r"; run_core "$r"; fi
  [ "$r" -lt "$ROUNDS" ] && sleep "$SETTLE"
  r=$((r + 1))
done

REV=$(git rev-parse --short HEAD 2>/dev/null || echo unknown)
DATE=$(date -u +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || echo unknown)

JSON="$JSON" ROUNDS="$ROUNDS" TMP="$TMP" REV="$REV" DATE="$DATE" \
FIXDIR="$FIXDIR" FRAMES="$FRAMES" python3 - <<'PY'
import json, math, os, re, statistics

tmp, rounds = os.environ["TMP"], int(os.environ["ROUNDS"])
as_json = os.environ["JSON"] == "1"

DUR = re.compile(r"([\d.]+)\s*(ns|µs|us|ms|s)")
SCALE = {"ns": 1e-6, "us": 1e-3, "µs": 1e-3, "ms": 1.0, "s": 1e3}

def ms(tok):
    m = DUR.fullmatch(tok)
    return float(m.group(1)) * SCALE[m.group(2)] if m else None

def after(parts, key):
    try:
        return ms(parts[parts.index(key) + 1])
    except (ValueError, IndexError, TypeError):
        return None

def num_before(parts, key):
    try:
        return float(parts[parts.index(key) - 1].replace(",", "").strip("()"))
    except (ValueError, IndexError):
        return None

core, frames = {}, {"diff_unified": {}, "diff_split": {}, "commits": {}}
for r in range(1, rounds + 1):
    with open(f"{tmp}/core.{r}.out", encoding="utf-8", errors="replace") as f:
        lines = f.read().splitlines()
    c = {}
    for ln in lines:
        p = ln.split()
        if not p:
            continue
        if p[0] == "COMMITS":
            c["commits"] = num_before(p, "widest") or float(p[1])
            try:
                c["widest_lanes"] = float(p[3])
            except (IndexError, ValueError):
                pass
        elif p[0] == "read" and "lanes" in p:
            c["log_read_ms"], c["log_parse_ms"], c["lanes_ms"] = (
                after(p, "read"), after(p, "parse"), after(p, "lanes"))
        elif p[0] == "DIFF":
            c["diff_lines"], c["diff_files"], c["replace_pairs"] = (
                num_before(p, "lines"), num_before(p, "files"), num_before(p, "replace-pairs"))
        elif p[0] == "read" and "intraline" in p:
            c["diff_read_ms"], c["diff_parse_ms"], c["intra_serial_ms"] = (
                after(p, "read"), after(p, "parse"), after(p, "intraline"))
        elif p[0] == "prepare":
            c["prepare_ms"], c["intra_cpu_ms"], c["syntax_cpu_ms"] = (
                after(p, "prepare"), after(p, "intraline"), after(p, "syntax"))
            for t in p:
                if t.startswith("×"):
                    try:
                        c["threads"] = float(t[1:])
                    except ValueError:
                        pass
            c["tokens"], c["mb_scanned"], c["mb_per_s_core"] = (
                num_before(p, "tokens"), num_before(p, "MB"), num_before(p, "MB/s/core)"))
        elif p[0] == "align":
            c["align_ms"], c["split_rows"], c["paired"] = (
                after(p, "align"), num_before(p, "rows"), num_before(p, "paired"))
        elif p[0] == "wrap":
            c["wrap_ms"], c["wrapped_rows"], c["wrap_rejected"] = (
                after(p, "wrap"), num_before(p, "rows"), num_before(p, "rejected"))
        elif p[0] == "markdown":
            c["markdown_ms"] = after(p, "markdown")
    for k, v in c.items():
        core.setdefault(k, []).append(v)
    for key, path in (("diff_unified", f"{tmp}/fu.{r}.err"),
                      ("diff_split", f"{tmp}/fs.{r}.err"),
                      ("commits", f"{tmp}/fc.{r}.err")):
        with open(path, encoding="utf-8", errors="replace") as f:
            tail = f.read().splitlines()
        line = next((ln for ln in reversed(tail) if ln.startswith("load ")), "")
        p = line.split()
        fms, fus = after(p, "load"), None
        try:
            fus = ms(p[p.index("frame") + 1])
        except (ValueError, IndexError, TypeError):
            pass
        n = num_before(p, "rows") or num_before(p, "commits")
        fr = frames[key]
        fr.setdefault("load_ms", []).append(fms)
        fr.setdefault("frame_us", []).append(fus * 1000.0 if fus is not None else None)
        fr.setdefault("rows", []).append(n)

def med(xs):
    xs = [x for x in xs if x is not None and (not isinstance(x, float) or not math.isnan(x))]
    return statistics.median(xs) if xs else None

def pack(d):
    return {k: {"median": med(v), "samples": v} for k, v in sorted(d.items())}

core_m, frames_m = pack(core), {k: pack(v) for k, v in frames.items()}
counts = {k: v["median"] for k, v in core_m.items()
          if k in ("commits", "widest_lanes", "diff_lines", "diff_files",
                   "replace_pairs", "tokens", "threads", "split_rows", "paired",
                   "wrapped_rows", "wrap_rejected", "rows") or k.endswith("_rows")}

if as_json:
    import platform
    doc = {
        "schema": "gitten.bench/1",
        "rev": os.environ["REV"],
        "date": os.environ["DATE"],
        "profile": "release",
        "fixtures": {"dir": os.environ["FIXDIR"], **counts},
        "rounds": rounds,
        "frames_env": {"cols": 120, "rows": 40, "frames": int(os.environ["FRAMES"])},
        "core": core_m,
        "frames": frames_m,
        "machine": {"os": platform.system(), "cores": os.cpu_count()},
    }
    print(json.dumps(doc, indent=2))
else:
    fix, rnd = os.environ["FIXDIR"], rounds
    print(f"bench {fix} ({rnd} rounds, median)")
    for k in ("log_parse_ms", "lanes_ms", "diff_parse_ms", "prepare_ms",
              "intra_cpu_ms", "syntax_cpu_ms", "align_ms", "wrap_ms"):
        if k in core_m:
            print(f"  core {k} {core_m[k]['median']:.3f}ms")
    if "mb_per_s_core" in core_m:
        print(f"  core mb_per_s_core {core_m['mb_per_s_core']['median']:.0f}MB/s/core")
    for k, v in frames_m.items():
        if "frame_us" in v:
            print(f"  frames {k} load {v['load_ms']['median']:.1f}ms"
                  f" frame {v['frame_us']['median']:.1f}us"
                  f" rows {v['rows']['median']:.0f}")
PY
