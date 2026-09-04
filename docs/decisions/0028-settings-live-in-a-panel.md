# 0028 — The settings live in a panel, not in the title strip

**Status** accepted; implementation superseded by
[0029](0029-settings-live-in-a-window.md)
**Date** 2026-09

## Context

The title strip carried five pickers — layout, wrap, algorithm, whitespace,
theme — and five was already too many for 44 pixels. The tier logic in
`shell/src/controls.rs` collapsed them into one composed menu on narrow
windows, and every sixth knob would have needed the same budget arithmetic
again. [0015](0015-title-bar-controls-are-hand-rolled.md) called the strip
*the interim answer* and named the real one: a settings panel.

[0015](0015-title-bar-controls-are-hand-rolled.md) also set the terms for the
handover: the pick does not persist while it is interim — the file is the
source of truth and a control that quietly rewrites it would be a settings
panel with no confirmation — and that is wrong the moment the panel exists.

## Decision

`shell/src/settings.rs` — one panel, fifteen rows in six sections, opened by
`,`, the title strip's gear button, or the menu bar's `Settings…` (`Cmd-,`).
Every row applies live through the route its picker took, and every change is
also written back to `gitten.toml` as the new default. The panel *is* the
confirmation the interim controls were missing: choosing is doing, and the
file opens where the panel left off.

Three things the panel holds that the strip never did: `context`, `moves`
and `indent_heuristic` (a forced re-acquire under the patched host, where
`set_overrides` would early-return on an unchanged `over`), `font.size` /
`font.family`, and the `[view]` / `[mouse]` knobs (a wholesale host patch on
the next frame). What stays in the file only: `font.monospaced`,
`font.advance` and the colours — nothing in the panel is next-launch, so no
row needs an asterisk beside it.

## Why not a native settings window

- **Every colour in this app comes from `gitten_core::theme`.** A native
  window draws from the platform palette, so matching means a second theme to
  keep in sync — the same reason [0015](0015-title-bar-controls-are-hand-rolled.md)
  refused `Popover`, now with a whole window behind it.
- **A registry lists itself.** The rows are built from the same names the
  pickers read, so an extension's algorithm or theme is a row the day it is
  registered. A native form hardcodes its fields.
- **The other doors stay possible.** The panel is an overlay over the same
  mode stack the `?` panel uses, and keyboard-first for the same reason. A
  native window is a Mac-only answer to a shared question.

## Why not the menu bar itself

It is there, but only as an adapter: `Settings…` dispatches the *named*
`settings` command every other door uses, the way `Quit` dispatches `quit`.
A menu item is how a Mac user discovers the panel; it is not where the panel
lives, because a menu cannot show fifteen current values.

## Evidence

The cost of a pick is unchanged from [0015](0015-title-bar-controls-are-hand-rolled.md):
layout is one `prepare`, algorithm/whitespace/context one acquisition plus
one `prepare`. The panel adds one synchronous file write per change — a few
hundred bytes through `app::config::save_setting`, which preserves comments
and key order — and the watcher's reload converges on the same values.

## Consequences

**`shell/src/controls.rs` is deleted.** The tier arithmetic, the composed
menu and the five triggers went with the strip; the backdrop moved to
`menu.rs`, where the menus that still need it live. A picker registered
tomorrow is a settings row, not a sixth trigger.

**The pick now persists**, which reverses [0015](0015-title-bar-controls-are-hand-rolled.md)'s
interim rule deliberately rather than by drift: that record's "wrong the
moment the panel exists" is this record's existence.

**`,` is bound in `core`.** The terminal resolves it to `settings` and
answers "not supported here" — the same honest sentence any client gives a
command it cannot run, and the terminal keeps `s` / `w` / `T` for the knobs
themselves.
