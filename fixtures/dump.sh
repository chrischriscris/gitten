#!/usr/bin/env bash
# Dump a real repo into flat fixtures so the spike can render dense, realistic
# data without gitoxide wired in yet. Testing the framework, not integration.
#
#   ./fixtures/dump.sh ~/src/linux            # or any big repo
#   ./fixtures/dump.sh ~/src/linux 20000      # commit count (default 5000)
#
# Fields are \x1f-separated and records \x1e-separated — control characters git
# will never put in a subject, so there is nothing to escape. Parsed by
# gitten_core::parse_log.
set -euo pipefail

REPO="${1:?usage: dump.sh <repo-path> [commit-count]}"
COUNT="${2:-5000}"
# Hermetic override: point at a scratch dir and the committed fixtures are
# never touched. Defaults to the fixtures dir beside this script.
OUT="${GITTEN_FIXTURES:-$(cd "$(dirname "$0")" && pwd)}"
mkdir -p "$OUT"

# Written to temp files and renamed into place, so an interrupted run never
# leaves a half-written fixture behind.
TMPLOG="$(mktemp "$OUT/.log.txt.XXXXXX")"
TMPDIFF="$(mktemp "$OUT/.big.diff.XXXXXX")"
trap 'rm -f "$TMPLOG" "$TMPDIFF"' EXIT

git -C "$REPO" log --topo-order -n "$COUNT" \
  --format='%H%x1f%h%x1f%P%x1f%an%x1f%at%x1f%s%x1e' > "$TMPLOG"

# A deliberately large diff — the diff view needs to be tested against
# something that hurts, not a three-line change.
BIG=$(git -C "$REPO" log -n 300 --format='%H' --merges | tail -1)
git -C "$REPO" diff "$BIG^" "$BIG" > "$TMPDIFF" 2>/dev/null \
  || git -C "$REPO" diff 'HEAD~50' HEAD > "$TMPDIFF"

/bin/mv -f "$TMPLOG" "$OUT/log.txt"
/bin/mv -f "$TMPDIFF" "$OUT/big.diff"
trap - EXIT

printf 'log.txt  %s commits\nbig.diff %s lines\n' \
  "$(tr -cd '\036' < "$OUT/log.txt" | wc -c | tr -d ' ')" \
  "$(wc -l < "$OUT/big.diff" | tr -d ' ')"
