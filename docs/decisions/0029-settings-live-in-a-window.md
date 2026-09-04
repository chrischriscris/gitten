# 0029 — The settings live in a window, not in a panel

**Status** accepted
**Date** 2026-09

## Context

[0028](0028-settings-live-in-a-panel.md) moved the knobs off the title strip
into a centered overlay: fifteen rows in six sections, every row live, all
doors dispatching the named `settings` command. The overlay worked, and then
the surface earned more than an overlay gives: search across sections, a
section sidebar that stays put, and one line of teaching per row do not fit
560 pixels without becoming a scroll maze.

The honest version of "more room" is a second window — Zed's settings are a
`cx.open_window` for the same reason (`crates/settings_ui`, "Zed — Settings",
own traffic lights). The question was whether a window costs the keyboard:
the overlay owned every press through the mode stack, and a surface outside
the keymap is a mouse surface.

## Decision

`,` opens a **"gitten — Settings" GPUI window** (`shell/src/settings_window.rs`);
a second open activates instead of duplicating, tracked in an app-global
handle. The window carries its own fixed key context — the `settings` mode
alone, plus `input` while the search field holds focus — so the shipped
bindings and any extension's mean what they mean in the main window. `esc`
and `,` close; closing returns focus to the main window, which never stopped
living underneath.

[0028](0028-settings-live-in-a-panel.md)'s three refusals, answered:

- **Theme.** This is a GPUI window, not a native one: it reads the same
  `gitten_core::theme` global as the main window. No second palette.
- **Registry rows.** The rows are the same `settings::build` — an
  extension's algorithm or theme is still a row the day it is registered,
  now with the one-line `desc` the teaching needs.
- **The mode stack.** The window resolves `settings::MODE` through the same
  `Keymap`, with the same pending-chord accumulation. All doors — `,`, the
  gear, the menu, `cmd-,` — still dispatch the named `settings` command;
  the TUI still answers "not supported here".

One implementation per knob, still: the window holds the filter and the
selection, and every turn goes through `DevShell::settings_apply` — the live
route and the `gitten.toml` write are the overlay's old ones, not a copy.
Opening draws synchronously, and the first draw reads the main shell, so the
open defers past the dispatching update — the reentrancy Zed defers past the
workspace, found here the way it is always found: as a borrow panic in a test.

## Evidence

`cargo test -p gitten-shell`: 391 pass, including `the_settings_command_opens_one_window`
(one window after `,`, still one after a second `,`) and the `settings` /
`settings_window` unit tests (row teaching, filter words, flat address
space ending on the file fallback). `cargo clippy --workspace --all-targets
-- -D warnings` and `cargo fmt --check` green. (`gitten-app`'s
`config_prints_a_file_that_reads_back` fails on clean HEAD too — pre-existing,
untouched by this.)

## Consequences

**The overlay is deleted.** `settings::overlay`, the shell's `settings` /
`settings_sel` / `settings_scroll` state, its mode-stack arm, its key and
wheel routing and its render block go with it. The help and message overlays
keep their shape through the new `shell/src/modal.rs` — one scrim, one box,
whoever's content — instead of each drawing it.

**New surface, same contract.** Search narrows rows across sections;
sections with no match dim rather than vanish; reopening (and every
keystroke) resets to the top. Rows the repository decides (`from_repo`
false) stay dimmed, and the file-only `keys` row opens `gitten.toml` in
`$EDITOR` — `$EDITOR` unset is a footer sentence, not a guess.

**Closing the main window first leaves the app on settings.** An edge, not
a crash: the main entity outlives its window and the knobs keep turning.
