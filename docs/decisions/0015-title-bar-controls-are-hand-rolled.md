# 0015 — The title-bar pickers are hand-rolled

**Status** accepted
**Date** 2026-08

## Context

Layout and algorithm are both registries of named things, and both were only
reachable by editing `gitten.toml`. Layout also had `s`. Neither is discoverable:
nothing on screen said which algorithm produced the diff, or that there was a
choice.

Keybindings and a settings panel are the real answer and are both listed in
[../architecture.md](../architecture.md) as not built. A control in the title bar
is the interim one.

AGENTS.md says **don't build what the framework already has**, and
gpui-component has both a `Popover` and a `Select`. So this record exists to make
the exception arguable rather than silent.

## Decision

`shell/src/controls.rs` — one `Picker`: a label, the value it holds, the
registered alternatives, and an enabled flag. `DevShell` draws two of them and
owns the one field that says which is open.

The floating list is `gpui::deferred` and dismisses through
`on_mouse_down_out`. Those are GPUI's, not ours.

## Why not `Popover` or `Select`

- **Every colour in this app comes from `gitten_core::theme`**, which the ANSI
  painter and a future terminal frontend read too — see
  [0005](0005-theme-in-core.md). `Popover` draws its surface from
  gpui-component's theme, so matching means keeping a second theme in sync with
  ours, and not matching is what rule 2 forbids. `appearance(false)` drops the
  surface and, by its own documentation, also drops dismiss-on-outside-click,
  which is the part worth having.
- **`Popover::trigger` requires `Selectable`**, a gpui-component trait, so a
  trigger drawn in our palette needs a wrapper type whose only job is to satisfy
  it.
- **The hard part of a dropdown is placement, and here there is none.** This sits
  in a fixed 32-pixel strip at the top of the window. It always opens downward and
  never needs to flip, so the general positioner earns nothing.
- **`Select` is a searchable, multi-selectable list behind a delegate trait and
  its own state entity**, for a list of three.

The `uniform_list` lesson AGENTS.md is quoting stands: a virtualised scrolling
list is genuinely hard and a hand-rolled one cost a day. A three-item menu in a
fixed slot is genuinely not. The moment a picker wants search, grouping or
keyboard navigation is the moment to delete this and take `Select`, and the module
comment says so.

## Why the algorithm needs a closure and the layout does not

Layout is presentation: the rows are rebuilt from the diff already in hand.
Algorithm changes what the diff *is*, so it has to be acquired again — and the
view does no I/O and must not learn what a repository is.

So `main` captures the source and hands `DevShell` one closure,
`Fn(&Host, Option<&str>) -> Result<Vec<FileDiff>, String>`. `None` means the
source cannot be re-diffed — a `.diff` fixture was diffed by somebody else — and
the control is drawn dim and inert rather than hidden, because a control that
appears and disappears as you change view is harder to find than one that greys.

The live `Host` is passed *in* rather than captured, so a config reload cannot
leave the closure holding a stale registry.

## Why the pick is a name and not a `Differs`

`Differs::file_using(Some("myers"), …)` overrides both the routes and the
configured fallback for one call. The alternative — building a second `Differs`
with a different selection — would lose whatever an extension registered into the
host's, and rule 1 says an extension's algorithm must be pickable exactly as a
built-in's is. A name is also what `gitten.toml` already uses, per
[0012](0012-config-is-data-behaviour-is-not.md).

The override beats the *routes* too, deliberately: someone who asks for myers
asked for the whole diff in myers, and quietly leaving `.json` on whatever it was
routed to would make the control lie about what is on screen.

## Evidence

The cost of a pick, from [../measurements.md](../measurements.md): layout is one
`prepare`, 8 ms typical and 247 ms on the pathological fixture. Algorithm is one
acquisition plus one `prepare` — 25–110 ms plus that. On a click, both are fine;
neither would be on a key held down, which is the other reason this is a menu and
`s` is not bound to the algorithm.

## Consequences

**Two GPUI traps were found writing this and are now in AGENTS.md.** A floating
element overflowing the first child of a column paints *under* its sibling unless
deferred — invisible, and indistinguishable from never having been built. And an
element's identity is its path with unnamed ancestors omitted, so two pickers whose
inner elements were both `"list"` were one element driving both hover states.

**A pre-existing hot-reload bug surfaced.** `DevShell` held an `Rc<Host>` captured
at startup, so the window chrome and the font for the whole window did not
hot-reload while every view inside them did — the exact trap
[extending.md](../extending.md) warns about, in the one place nobody had looked.
Fixed here because the strip needs the live host anyway.

**The pick does not persist.** Relaunching returns to `gitten.toml`. That is right
while this is interim — the file is the source of truth and a control that quietly
rewrote it would be a settings panel with no confirmation — and wrong the moment
the panel exists.

**`theme.chrome.error` is a new colour**, for a re-acquisition that failed. Its own
field and not `diff.dels_fg`: that red means "this line was removed", and a
palette where one colour means two things is a palette a theme cannot retune.
