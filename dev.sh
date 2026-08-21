#!/usr/bin/env bash
# Kept so `./dev.sh diff .` still works. `./dev` is the entry point now, and it
# covers every client rather than only the window:
#
#   ./dev desktop diff .      this, spelled out
#   ./dev tui     diff .      the terminal
#   ./dev web     diff .      a browser tab
#   ./dev                     the rest
exec "$(dirname "$0")/dev" desktop "$@"
