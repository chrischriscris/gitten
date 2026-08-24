#!/usr/bin/env bash
# Everything that can be checked without opening a window.
#
#   ./check.sh          correctness, then the pipeline across every real fixture
#
# The `differs` section needs a repository rather than a fixture: a `.diff` file
# has already been diffed by somebody, so it cannot test the thing that diffs.
#
# Fixtures are swapped in and out; whatever was in fixtures/ is restored at the end.
#
# **It exits non-zero when something failed.** It did not, for a while: every
# `cargo test` line ended in `| grep ... || true`, which discards the status along
# with the noise, so `./dev check` printed `test result: FAILED` and exited 0.
# Anything reading the exit code — a hook, a CI step, a person typing `&&` —
# was told everything was fine. `report` keeps the quiet output and keeps the
# status.
set -uo pipefail
cd "$(dirname "$0")"

FAILED=""

# Runs a command, prints only the lines worth reading, and remembers a failure.
# The status comes from the command and not from `grep`, which is the whole point.
report() {
  local what=$1; shift
  local out status
  out=$("$@" 2>&1); status=$?
  printf '%s\n' "$out" | grep -E "^test result|^error" || true
  if [ "$status" -ne 0 ]; then
    FAILED="$FAILED $what"
    printf '  ✗ %s failed\n' "$what"
    # The reason, not just the verdict: a panic message beats going and
    # re-running it by hand.
    printf '%s\n' "$out" | grep -E "panicked|^---- |assertion" | head -8 | sed 's/^/    /'
  fi
}
STASH=$(mktemp -d)
trap '[ -f "$STASH/log.txt" ] && /bin/cp -f "$STASH/log.txt" fixtures/log.txt
      [ -f "$STASH/big.diff" ] && /bin/cp -f "$STASH/big.diff" fixtures/big.diff
      rm -rf "$STASH"' EXIT
[ -f fixtures/log.txt ]  && /bin/cp -f fixtures/log.txt  "$STASH/"
[ -f fixtures/big.diff ] && /bin/cp -f fixtures/big.diff "$STASH/"

echo "── correctness ─────────────────────────────────────────"
report core cargo test -q -p gitten-core
# The shared startup: the config file, the command line, acquisition. Every
# client depends on it, so a break here breaks all of them at once.
report app cargo test -q -p gitten-app
# The acquisition layer — the only crate that talks to a repository. It was
# missing from this list *and* from CI, so `parse_raw`, the `cat-file` batch
# protocol and untracked status were tested by nothing that anybody ran. Its
# tests build their own scratch repositories, so they are as headless as the rest.
report git cargo test -q -p gitten-git
# The browser door. Headless too — every test in it is a payload or a row
# index, and neither needs a socket.
report web cargo test -q -p gitten-web
# The terminal door, and the only frontend whose *drawing* is tested: its screen
# is a cell buffer, so "this row is a removal, red on dark red, with the changed
# word lit" is an assertion and not something to go and look at.
report tui cargo test -q -p gitten-tui
# The desktop drawing tests use GPUI's headless test context: no window appears,
# but the real uniform list is laid out and its visible rows are measured.
report shell cargo test -q -p gitten-shell

echo
echo "── trees ───────────────────────────────────────────────"
for repo in "$HOME/Projects/git" "$HOME/Projects/cmux"; do
  [ -d "$repo/.git" ] || continue
  printf '%s\n' "  $(basename "$repo")"
  git -C "$repo" log --topo-order --format='%H%x1f%h%x1f%P%x1f%an%x1f%at%x1f%s%x1e' > fixtures/log.txt 2>/dev/null
  cargo run -q -p gitten-core --example shape --release 2>/dev/null | sed 's/^/  /'
done

echo
echo "── differs vs git ──────────────────────────────────────"
# Against git's own answer, on real history. A blobless clone lazily fetches
# every blob it is asked for, so the first run there is network-bound; that is
# also true of `git diff` in the same repository.
# The second is the whole history in one diff: every file this repo has ever
# had, which is the widest single input the differs get here.
for spec in HEAD~4..HEAD "$(git rev-list --max-parents=0 HEAD | tail -1)..HEAD"; do
  cargo run -q -p gitten-git --example diffcheck --release . "$spec" 2>/dev/null | sed 's/^/  /'
done
for repo in "$HOME/Projects/cmux" "$HOME/Projects/git"; do
  [ -d "$repo/.git" ] || continue
  cargo run -q -p gitten-git --example diffcheck --release "$repo" HEAD~5..HEAD 2>/dev/null \
    | sed 's/^/  /'
done

echo
echo "── diffs ───────────────────────────────────────────────"
for d in fixtures/real/*.diff; do
  [ -f "$d" ] || continue
  printf '%s\n' "  $(basename "$d")"
  /bin/cp -f "$d" fixtures/big.diff
  cargo run -q -p gitten-core --example bench --release 2>/dev/null \
    | grep -A4 '^DIFF' | tail -n +2 | sed 's/^/  /'
done

echo
echo "── synthetic scale ─────────────────────────────────────"
./fixtures/gen.sh 1000000 1000000 >/dev/null 2>&1
cargo run -q -p gitten-core --example bench --release 2>/dev/null | sed 's/^/  /'

echo
echo "── terminal frames ─────────────────────────────────────"
# One frame of each view, drawn and thrown away: it exercises the whole path
# from acquisition to cells without a terminal, and a panic in a presentation is
# invisible to the unit tests only if no fixture reaches it.
#
# Which is why these count towards the exit status. Catching a panic here is the
# entire reason the section exists, and a panic that only tints one line of a
# report is a panic nobody notices — the frame still prints, because it is
# printed as it is built.
frame() {
  local what=$1; shift
  printf '%s' "  $what "
  if ! "$@" 2>/dev/null | sed 's/\x1b\[[0-9;]*m//g' | tail -1; then
    FAILED="$FAILED frame:$what"
    printf '✗ panicked or exited non-zero\n'
  fi
}
# `env` and not a `VAR=x frame …` prefix: an assignment in front of a *function*
# call outlives the call, which is a footgun nobody needs in a loop.
for view in commits diff; do
  frame "$view" env COLS=120 ROWS=40 \
    cargo run -q -p gitten-tui --example dump --release -- "$view" .
done
for layout in unified split; do
  frame "$layout" env COLS=120 ROWS=40 "LAYOUT=$layout" \
    cargo run -q -p gitten-tui --example dump --release -- diff --fixtures
done

echo
if [ -n "$FAILED" ]; then
  echo "✗ failed:$FAILED"
  exit 1
fi
echo "✓ all green"
