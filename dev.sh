#!/usr/bin/env bash
# Kept so `./dev.sh diff .` still works. `./dev` is the entry point now, and it
# covers every client rather than only the window:
#
#   ./dev gui     diff .      this, spelled out
#   ./dev tui     diff .      the terminal
#   ./dev web     diff .      the loopback agent API
#   ./dev                     the rest
exec "$(dirname "$0")/dev" gui "$@"
