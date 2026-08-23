#!/usr/bin/env bash
# Everything that can be checked without opening a window.
#
#   ./check.sh          correctness, then the pipeline across every real fixture
#
# The `differs` section needs a repository rather than a fixture: a `.diff` file
# has already been diffed by somebody, so it cannot test the thing that diffs.
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
# The shared startup: the config file, the command line, acquisition. Every
# client depends on it, so a break here breaks all of them at once.
cargo test -q -p plait-app 2>&1 | grep -E "^test result|^error" || true
# The browser door. Headless too — every test in it is a payload or a row
# index, and neither needs a socket.
cargo test -q -p plait-web 2>&1 | grep -E "^test result|^error" || true
# The terminal door, and the only frontend whose *drawing* is tested: its screen
# is a cell buffer, so "this row is a removal, red on dark red, with the changed
# word lit" is an assertion and not something to go and look at.
cargo test -q -p plait-tui 2>&1 | grep -E "^test result|^error" || true
# The desktop drawing tests use GPUI's headless test context: no window appears,
# but the real uniform list is laid out and its visible rows are measured.
cargo test -q -p plait-shell 2>&1 | grep -E "^test result|^error" || true

echo
echo "── trees ───────────────────────────────────────────────"
for repo in "$HOME/Projects/git" "$HOME/Projects/cmux"; do
  [ -d "$repo/.git" ] || continue
  printf '%s\n' "  $(basename "$repo")"
  git -C "$repo" log --topo-order --format='%H%x1f%h%x1f%P%x1f%an%x1f%at%x1f%s%x1e' > fixtures/log.txt 2>/dev/null
  cargo run -q -p plait-core --example shape --release 2>/dev/null | sed 's/^/  /'
done

echo
echo "── differs vs git ──────────────────────────────────────"
# Against git's own answer, on real history. A blobless clone lazily fetches
# every blob it is asked for, so the first run there is network-bound; that is
# also true of `git diff` in the same repository.
# The second is the whole history in one diff: every file this repo has ever
# had, which is the widest single input the differs get here.
for spec in HEAD~4..HEAD "$(git rev-list --max-parents=0 HEAD | tail -1)..HEAD"; do
  cargo run -q -p plait-git --example diffcheck --release . "$spec" 2>/dev/null | sed 's/^/  /'
done
for repo in "$HOME/Projects/cmux" "$HOME/Projects/git"; do
  [ -d "$repo/.git" ] || continue
  cargo run -q -p plait-git --example diffcheck --release "$repo" HEAD~5..HEAD 2>/dev/null \
    | sed 's/^/  /'
done

echo
echo "── diffs ───────────────────────────────────────────────"
for d in fixtures/real/*.diff; do
  [ -f "$d" ] || continue
  printf '%s\n' "  $(basename "$d")"
  /bin/cp -f "$d" fixtures/big.diff
  cargo run -q -p plait-core --example bench --release 2>/dev/null \
    | grep -A4 '^DIFF' | tail -n +2 | sed 's/^/  /'
done

echo
echo "── synthetic scale ─────────────────────────────────────"
./fixtures/gen.sh 1000000 1000000 >/dev/null 2>&1
cargo run -q -p plait-core --example bench --release 2>/dev/null | sed 's/^/  /'

echo
echo "── terminal frames ─────────────────────────────────────"
# One frame of each view, drawn and thrown away: it exercises the whole path
# from acquisition to cells without a terminal, and a panic in a presentation is
# invisible to the unit tests only if no fixture reaches it.
for view in commits diff; do
  printf '%s' "  $view "
  COLS=120 ROWS=40 cargo run -q -p plait-tui --example dump --release -- "$view" . \
    2>/dev/null | sed 's/\x1b\[[0-9;]*m//g' | tail -1
done
for layout in unified split; do
  printf '%s' "  $layout "
  COLS=120 ROWS=40 LAYOUT=$layout cargo run -q -p plait-tui --example dump --release -- \
    diff --fixtures 2>/dev/null | sed 's/\x1b\[[0-9;]*m//g' | tail -1
done
