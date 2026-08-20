#!/usr/bin/env bash
# Everything that can be checked without opening a window.
#
#   ./check.sh          correctness, then the pipeline across every real fixture
#
# Fixtures are swapped in and out; whatever was in fixtures/ is restored at the end.
set -uo pipefail
cd "$(dirname "$0")"
STASH=$(mktemp -d)
trap '[ -f "$STASH/log.txt" ] && /bin/cp -f "$STASH/log.txt" fixtures/log.txt
      [ -f "$STASH/big.diff" ] && /bin/cp -f "$STASH/big.diff" fixtures/big.diff
      rm -rf "$STASH"' EXIT
[ -f fixtures/log.txt ]  && /bin/cp -f fixtures/log.txt  "$STASH/"
[ -f fixtures/big.diff ] && /bin/cp -f fixtures/big.diff "$STASH/"

echo "── correctness ─────────────────────────────────────────"
cargo test -q -p plait-core 2>&1 | grep -E "^test result|^error" || true

echo
echo "── trees ───────────────────────────────────────────────"
for repo in "$HOME/Projects/git" "$HOME/Projects/cmux"; do
  [ -d "$repo/.git" ] || continue
  printf '%s\n' "  $(basename "$repo")"
  git -C "$repo" log --topo-order --format='%H%x1f%h%x1f%P%x1f%an%x1f%at%x1f%s%x1e' > fixtures/log.txt 2>/dev/null
  cargo run -q -p plait-core --example shape --release 2>/dev/null | sed 's/^/  /'
done

echo
echo "── diffs ───────────────────────────────────────────────"
for d in fixtures/real/*.diff; do
  [ -f "$d" ] || continue
  printf '%s\n' "  $(basename "$d")"
  /bin/cp -f "$d" fixtures/big.diff
  cargo run -q -p plait-core --example bench --release 2>/dev/null \
    | grep -A2 '^DIFF' | tail -2 | sed 's/^/  /'
done

echo
echo "── synthetic scale ─────────────────────────────────────"
./fixtures/gen.sh 1000000 1000000 >/dev/null 2>&1
cargo run -q -p plait-core --example bench --release 2>/dev/null | sed 's/^/  /'
