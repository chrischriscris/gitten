# 0021 — A theme is a registered palette, and the config file writes one

**Status** accepted
**Date** 2026-08

## Context

Colour has been data in a dependency-free crate since
[0005](0005-theme-in-core.md), and `plait.toml` has been able to set every field
of it since [0012](0012-config-is-data-behaviour-is-not.md). What did not exist
was a *second* palette: `Host::theme` was one `Theme`, built by
`Theme::default_dark()`, and "switch to a light theme" meant retyping sixty hex
values into a config file — with no way to switch back, because the file is the
only place the old ones would have been.

Every other seam in the app had already been through this. Differs, wraps,
highlighters and layouts are registries; the title-bar picker is a pure function
of one, and [0015](0015-title-bar-controls-are-hand-rolled.md) is the note that
says any seam with a registry gets a control for free. The theme was the seam
without one.

## Decision

**`Themes` is a registry of `Theme`, and `dark`, `light` and `slate` are its
three entries.** `host.select_theme(name)` copies one into `host.theme`,
`host.cycle_theme()` steps through them, `theme.cycle` is the command name and
`T` is bound to it globally.

**The registry holds no selection.** This is the one place it differs from
`Differs` and `Wraps`, and the reason is that a theme is the only seam whose
implementation is data the config file *edits*: `plait.toml` sets colours on top
of whatever it selected. A registry that also owned the selection would have to
answer whether the entry or the edit is the truth. So `themes` is the catalogue,
`theme` is what is drawn, and `theme.name` is which one it came from.

**A `[theme]` table is a theme definition, and is registered.** `name` is read
before everything under it and selects the base; the rest is applied on top; the
result is registered under that name when the file is done. Naming a built-in
corrects that entry rather than adding a second one called the same thing, which
is how `register` already behaves everywhere else.

**A second palette is a port of the first.** Every structural ratio in `light`
and `slate` is `dark`'s — `added_bg` 1.20:1 from its context row, `file_bg`
1.18:1, a changed word 1.29:1 from the line it is inside, the gutter 2.05:1
before it is lifted. `examples/contrast.rs` prints all of them for every
registered theme, and is what the two new palettes were solved against.

## Why `name` selects rather than labels

It was a free-text label, and there was nothing for it to name. Making it select
is what lets a file say `name = "light"` in one line instead of sixty, and it
costs the case where somebody wanted a label: that case still works, because an
unregistered name defines a theme rather than failing.

The one shape worth a warning is a `[theme]` table holding an unknown `name` and
nothing else — that file changed nothing at all, and the cause is a typo. A table
that also sets colours is a definition. The alternative, warning on every
unregistered name, fires once per save for anybody with a hand-written palette,
and [theming.md](../theming.md) is explicit that a warning which fires on a
value you meant teaches you to ignore the ones that matter.

## Why the file's theme goes into the registry

Because otherwise the picker lies. `plait config` dumps *every* colour of the
theme you are on, so a file produced by it and then edited is a full palette; if
that palette were not registered, picking "light" from the menu would apply the
file's sixty overrides on top of light and change nothing on screen. Registering
the file's theme under its own name makes a pick a choice between whole palettes,
and leaves the other two exactly as they shipped.

## Why a pick goes through the reload

`Host` is behind an `Rc` every view holds, replaced wholesale so nobody can ever
see half a theme — there is no host to mutate. So a pick sets a global naming the
choice and then runs the same rebuild a saved file runs: defaults, then the file,
then the pick on top. One path, because two would be two orders in which a theme
and a colour can disagree, and it is what makes a pick survive the next save the
way the diff view's own layout and wrap indices do.

## Consequences

**Selecting copies a `Theme`** — a `String`, three `Vec`s and a 96-entry resolved
table. Sharing the entry behind a refcount instead would make the config file's
edits visible in the catalogue, which is the thing this design is avoiding, and
the copy is not what a pick costs anyway: the pick is a whole `Host::new` plus a
re-read of the file, **145 µs** release, on a click. Three extra themes are 21 µs
of that (`Theme::dark()` is 7 µs, and `rebuild` is all of it); the rest was
already there.

**`Theme::default_dark` is now `Theme::dark`.** One rename across 18 call sites,
all of them tests; the constructor was named for being the default and it is now
one of three.

**The contrast tests run over the registry**, so a fourth palette cannot ship
illegible without failing them — and two new invariants came out of writing the
other two: a sign colour has to clear the floor on *both* rows it can land on
(`added_fg` is drawn on moved additions too), and `absent_bg` is read against the
row opposite it rather than against context.

**The terminal client implements `theme.cycle` too.** Not scope creep — the
keymap is shared and its `?` panel lists every binding, so a shipped key that did
nothing there would be a help screen that lies. It is six lines and the same
"survives a reload" rule as the window's.
