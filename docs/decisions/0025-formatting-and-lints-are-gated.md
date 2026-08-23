# 0025 — Formatting and lints are gated

**Status** accepted
**Date** 2026-08

## Context

The code had never been rustfmt-formatted — the pinned toolchain's minimal
profile ships without it — and clippy had never run: 24 warnings, all
mechanical, none architectural. Nothing enforced either before a push, so CI
(0024) caught style only after it was history.

## Decision

One definition of clean, enforced twice so local and CI cannot disagree:

    cargo fmt --check
    cargo clippy --workspace --all-targets -- -D warnings

- **`.githooks/pre-commit`**, wired with `git config core.hooksPath .githooks`
  (once per clone). Warm cache this costs about a second; a commit that would
  fail CI fails before it exists. `--no-verify` is the deliberate way through.
- **CI's `lint` job**, which replaces the plain `cargo check` job from 0024:
  clippy type-checks everything check did, so the Linux canary loses nothing
  and the workspace still compiles on Linux exactly as often. Still two jobs.

`rust-toolchain.toml` gains `components = ["rustfmt", "clippy"]` — the profile
stays minimal otherwise, and the pin itself does not move.

## The adoption cost, paid once

A 52-file reformat — blame noise with no information in it; point
`.git-blame-ignore-revs` at that commit — and the 24 clippy fixes. All were
mechanical except three renames forced by clippy's rule that a method named
`new` returns `Self`, where the old/new vocabulary survived by moving into the
name's suffix: `Edit::old()/new()` → `old_range()/new_range()`;
`Slot::old()/new()` → `left()/right()` (their own doc comments already said
"left column" / "right column"); `Moves::old(line)/new(line)` →
`in_old(line)/in_new(line)` (its doc asks "is this line part of a move").

`web`'s state went from `Arc` to `Rc`: http.rs runs every request on the
serving thread precisely because the host is not Send, so the Arc was the same
type with a promise nothing keeps.

## Consequences

New lints arrive when the toolchain pin moves, and they break commits until
addressed — deliberately, since the pin means it happens on our schedule, in
one sitting, rather than as a slow red drift. Clippy's `too_many_arguments`
stays allowed on view-helper functions, per the precedent that already existed;
the split view's `cell()` joined that list rather than being restructured under
a commit days old.
