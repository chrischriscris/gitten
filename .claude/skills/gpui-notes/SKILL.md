---
name: gpui-notes
description: The GPUI traps this repo has already paid for (uniform_list sizing, scroll axes, row backgrounds, deferred/occlude, element identity, subscribe re-entry, focus reclaim, the test-macro shadowing, titlebar and theme sync). Load before writing or changing anything in gui/src, and when a desktop bug looks like layout, scrolling, focus, clicks landing in the wrong place or a crash in entity_map.rs.
---

# GPUI notes

Read [docs/gpui-notes.md](../../../docs/gpui-notes.md) in full before the
change. It is the single copy; this skill only exists so it loads when you're
in `gui/` without every other task paying for it.

Why read it before the change rather than after: most of these fail silently,
and the five traps in its last paragraph never show up in a screenshot, so
`./dev verify` won't catch them for you. `./dev lint` catches exactly one of
them (`use super::*` beside `use gpui::*`).

Then check the change with the `verify-gitten` skill.
