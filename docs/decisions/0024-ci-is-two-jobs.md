# 0024 — CI is two jobs, and most of check.sh stays home

**Status** accepted
**Date** 2026-08

## Context

The repository had no CI. The checks that matter are already scripted
(`./check.sh` — "everything headless"), but two promises were kept by memory
alone: that nothing is written in a way that makes Linux impossible, and that
the headless test crates stay green before a push lands.

## Decision

One workflow, `.github/workflows/check.yml`, two jobs, both ubuntu:

- **test** — the portable part of `check.sh`'s correctness section: `cargo test`
  over `gitten-core`, `gitten-app`, `gitten-web` and `gitten-tui`. Each is headless
  by design, so they run unmodified.
- **linux** — `cargo check --workspace --all-targets` with the packages a Linux
  GPUI build needs. This is the enforcement of the Linux rule; nothing else
  checks it between writing a macOS-ism and pushing it. The same job runs
  `gitten-shell`'s headless GPUI tests after the check: they open no real window,
  but need the native packages available when their dependencies are built.

Both jobs use `--locked`, because `Cargo.lock` is committed and a push without
its lockfile entry should fail loudly.

The native packages are installed only when the Rust cache is not an exact hit.
On an exact hit every native dependency and build script output is already
compiled; clippy checks workspace crates but does not link an executable. Exact
hits also run Cargo offline, avoiding an index update for sources whose lockfile
and cache key already agree. Cache misses still install the full package set and
allow the network before compiling anything.

The toolchain is not pinned in the workflow: `rust-toolchain.toml` says
1.97.1 and rustup honours it on the runner exactly as it does locally, so there
is one pin and it lives where Zed's drift is already tracked.

## Why not run check.sh

Its trees/differs sections read `$HOME/Projects/git` and `$HOME/Projects/cmux`,
which do not exist on a runner, and cloning git.git per push is exactly the
network-bound case the script itself warns about. Its bench sections measure
timing on a shared VM — a wrong number by construction, the same reason frame
times are meaningless off a debug build. Measurements stay local (`./dev dump`,
[../measurements.md](../measurements.md)).

## Why not macOS

No release process exists to gate, so there is nothing a macOS job would catch
that `linux` does not, at several times the runner cost. `./dev bundle` belongs
in a release workflow when one exists.

## Consequences

A macOS-only regression still lands before CI sees anything — CI runs after
the push, not instead of running `./check.sh` first. diffcheck against real
history stays on laptops; if it ever needs to be runnable elsewhere, it gets a
manual `workflow_dispatch` job, not a place in this one.
