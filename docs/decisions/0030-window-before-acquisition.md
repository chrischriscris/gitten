# 0030 — The desktop opens its window before it acquires

**Status** accepted
**Date** 2026-09

## Context

Startup was acquire-then-window: `Startup::go` did the whole chain — args,
`gitten.toml`, the repository read — and the window opened on a `Started` that
already held its rows. On this repo that put ~65 ms of `git` processes between
double-click and first render, all of it blank. The terminal cannot draw first —
it owns stdin and has nothing to put in front of itself — but the desktop can,
and blank-before-data is the one thing a GUI is never forgiven for.

The trap was doing it client-side: a `main` that re-parsed args and re-read the
config before opening its window would fork the startup chain, and the second
copy would drift the way the second copies in [clients.md](../clients.md) always
have.

## Decision

The seam moves into `gitten-app` and the ordering stays the client's.
[`Startup::configure`](../../app/src/lib.rs) is everything `Startup::go` does
except the acquisition, which the client schedules itself through the same
`acquire::acquire` `go` would have run. What is shared is the chain, not the
ordering. A client with nothing to draw first takes `go()` and never sees
`Configured` — the TUI is unchanged.

`shell/src/main.rs` holds the `Launch` enum: `Ready(Started)` and
`Skeleton(Configured)`. An explicit repository launch takes Skeleton — the
window opens on empty screens registered one generation below the shell's,
sidebar panes in a loading shape (header label `STARTUP_LOADING`), the saved
session row riding `pending_restore` — and one background wave acquires and
fills everything. Nothing about the wave is new: it is the same `refresh_stale`
a repository switch rides, through the existing per-pane
`Refresh { load, apply }`, and `finish_refresh` applies the restored scroll and
schedules the preview diff exactly as it does after a switch. The skeleton
frame's empty sidebar panes carry the honest-emptiness convention on their
"loading" header, the same way the TUI's loading shapes work.

Two launches keep the synchronous road. A fixture or patch is an in-process read
with no spawn floor to defer against, and its failure still prints to stderr and
exits. A bare launch (Finder, `open`, no arguments) keeps its old behaviour
outright: one cheap `git status` probe on the cwd decides, a repository opens —
now skeleton-first — and with none, recents or the picker open as before, stderr
included.

## Why not a client-side copy of the chain

It works once and drifts after: the desktop's version would be the only one
that parsed arguments twice, and the next seam (`--fixtures`, an opener) would
have to be wired through both. `configure` costs one method and one type and
makes the ordering a client choice, which is what an extension-authored client
with a splash screen needs anyway.

Deferring *within* `go()` — acquire on a thread, window first anyway — was
rejected because the failure path then has two answers instead of one clean one,
and because a `Started` that arrives mid-frame is exactly the race the wave's
generation guard already exists to end.

## Evidence

`GITTEN_START_QUIT=1 GITTEN_START_LOG=1 ./target/release/gitten-shell commits .`
— the harness this repo's measurement page said existed and was unrecorded.
This repo, release, M1 Pro, medians of 12 ABBA-interleaved runs a side: GUI
time-to-interactive ~357 ms → ~282 ms, and the startup-done mark 107 → 80 ms,
the window callback ~65 → 0.9 ms. Full tables, marks and the dyld-outlier
caveat in [measurements.md](../measurements.md#the-desktop-opens-its-window-before-it-acquires).

## Consequences

**A repository that will not open no longer exits 1.** A bad revspec or a path
that is not a repository now opens the window and lands the wave's failure in
the window's existing error band (`error_is_load`), and the process stays up.
Scripting against the GUI binary was never the supported door — the headless
`cli` harness and the TUI are — and the bare-launch fallback keeps its stderr
answer for the one case (`open` on a non-repository) where the process was the
only thing talking.

**A one-frame loading shape is now visible** (~100 ms) in the sidebar panes of a
skeleton launch. It is the honest state, not a polish item to hide.

**The seam is general and the uses are not.** No other client wants it today;
`configure` exists because the desktop asked. A client that would draw first
takes it; one that would not takes `go()` and loses nothing.
