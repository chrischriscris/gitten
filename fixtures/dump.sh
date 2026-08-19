#!/usr/bin/env bash
# Dump a real repo into flat fixtures so the spike can render dense, realistic
# data without gitoxide wired in yet. Testing the framework, not integration.
#
#   ./fixtures/dump.sh ~/src/linux            # or any big repo
#   ./fixtures/dump.sh ~/src/linux 20000      # commit count (default 5000)
#
# Fields are \x1f-separated and records \x1e-separated — control characters git
# will never put in a subject, so there is nothing to escape. Parsed by
# plait_core::parse_log.
set -euo pipefail

REPO="${1:?usage: dump.sh <repo-path> [commit-count]}"
COUNT="${2:-5000}"
OUT="$(cd "$(dirname "$0")" && pwd)"

git -C "$REPO" log --topo-order -n "$COUNT" \
  --format='%H%x1f%h%x1f%P%x1f%an%x1f%at%x1f%s%x1e' > "$OUT/log.txt"

# A deliberately large diff — the diff view needs to be tested against
# something that hurts, not a three-line change.
BIG=$(git -C "$REPO" log -n 300 --format='%H' --merges | tail -1)
git -C "$REPO" diff "$BIG^" "$BIG" > "$OUT/big.diff" 2>/dev/null \
  || git -C "$REPO" diff 'HEAD~50' HEAD > "$OUT/big.diff"

printf 'log.txt  %s commits\nbig.diff %s lines\n' \
  "$(tr -cd '\036' < "$OUT/log.txt" | wc -c | tr -d ' ')" \
  "$(wc -l < "$OUT/big.diff" | tr -d ' ')"
