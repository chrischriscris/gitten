#!/usr/bin/env bash
# Rebuild and relaunch on every source change, landing back where you were.
#
#   ./dev.sh diff . HEAD~2..HEAD
#   ./dev.sh commits
#   ./dev.sh --debug diff .            # ~2.5s rebuild, much slower app
#
# Not hot reload. Changing code still costs a rebuild — 3-5 s release — and this
# removes everything either side of it: no quitting, no retyping the command, and
# the window comes back on the row you were reading. The position is kept by
# shell/src/session.rs, which writes it *while you scroll* because this script
# kills the process and nothing runs on the way out.
#
# Real hot reload was investigated and rejected; see docs/architecture.md. GPUI
# keeps a thread-local element arena and a process-wide entity id counter, so a
# reloadable dylib forks both unless gpui itself is dynamically linked.
#
# Colour and font changes need none of this: plait.toml reloads live, on the next
# frame. Use this for code.
set -uo pipefail
cd "$(dirname "$0")"

PROFILE=release
FLAGS=(--release)
if [ "${1:-}" = "--debug" ]; then
  PROFILE=debug
  FLAGS=()
  shift
fi

BIN="target/$PROFILE/plait-shell"
WATCH=(core/src core/examples git/src shell/src)
LOG=$(mktemp)
MARKER=$(mktemp)
running=""

dim() { printf '\033[2m%s\033[0m\n' "$1"; }
red() { printf '\033[31m%s\033[0m\n' "$1"; }

cleanup() {
  [ -n "$running" ] && kill "$running" 2>/dev/null
  rm -f "$LOG" "$MARKER"
  exit 0
}
trap cleanup INT TERM

cycle() {
  # Kill first: two windows fighting over one session file is confusing, and the
  # binary underneath the running one is about to be replaced anyway.
  if [ -n "$running" ]; then
    kill "$running" 2>/dev/null
    wait "$running" 2>/dev/null
    running=""
  fi

  dim "── building ──────────────────────────────"
  local start=$SECONDS
  # To a file, not a pipe: a pipe loses cargo's exit status under pipefail, and
  # launching a stale binary after a failed build is the one thing this must not
  # do. Output is only worth seeing when it went wrong.
  if ! cargo build "${FLAGS[@]}" -p plait-shell >"$LOG" 2>&1; then
    grep -vE '^\s*(Compiling|Finished|Blocking|Downloaded|Updating)' "$LOG"
    red "── build failed — fix it and save again ──"
    return
  fi
  dim "── $((SECONDS - start))s · launching ─────────────────"

  "$BIN" "$@" &
  running=$!
}

cycle "$@"

# `find -newer` against a marker we touch each cycle: POSIX, no dependencies, and
# -quit stops at the first hit instead of walking the whole tree. 400 ms of
# latency on a 3-second rebuild is nothing.
while true; do
  sleep 0.4
  if [ -n "$(find "${WATCH[@]}" \( -name '*.rs' -o -name '*.toml' \) \
              -newer "$MARKER" -print -quit 2>/dev/null)" ]; then
    touch "$MARKER"
    cycle "$@"
  fi
  # Closed by hand: stop, rather than leaving a watcher running in a terminal
  # you have stopped looking at.
  if [ -n "$running" ] && ! kill -0 "$running" 2>/dev/null; then
    dim "── window closed ─────────────────────────"
    cleanup
  fi
done
