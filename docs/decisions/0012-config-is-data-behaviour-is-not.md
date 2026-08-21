# 0012 — Config is data, behaviour is not

**Status** accepted
**Date** 2026-08

## Context

`plait.toml` exists and reloads live, which raised the question of whether it was
the right format. The answer turns on two things that were not decided yet:

1. **There will be a settings panel** — preferences you click, not only a file you
   edit. And the file has to keep working alongside it.
2. **Extensibility is the point.** Rule 1 says anything a built-in does, an
   extension must be able to do too, so eventually something has to express
   *behaviour*, not just values.

Those pull in opposite directions, and the temptation is to answer both with one
language — Lua, the way Neovim and WezTerm do.

## Decision

Two layers, and the line between them is not the format, it is **data versus
behaviour**.

```
plait.toml        data.  Hand-edited OR written by the settings panel.
                  Round-tripped with toml_edit, so comments and order survive.
plugins/*.lua     behaviour.  Separate files. Referenced from the config by
                  name, never inlined into it.
```

Zed's split, and it is well-trodden: JSON settings plus a settings UI plus
separate extensions. Helix does the same with TOML and Scheme.

**Behaviour never appears in the settings file.** That is the load-bearing rule.
The moment a function lives in `plait.toml`, the panel cannot round-trip the file,
and the panel is a stated requirement.

```toml
[[bind]]
keys = "t"
lua = "my.toggle_source"    # by name — the behaviour lives elsewhere
```

## Why TOML, and why it does not change

A settings panel has to write the file back **without destroying it**: comments,
key order, and untouched sections all have to survive. Tick one checkbox and lose
every comment in a hand-tuned theme, and nobody trusts the panel again.

That single requirement decides the format, because it eliminates every option
that cannot be edited in place:

| | round-trips with comments | keymaps | expressions | cost |
|---|---|---|---|---|
| **TOML** | **yes, `toml_edit`** | ok | no | **already in the tree** |
| KDL | yes, `kdl` crate | best | no | new dependency |
| JSON5 | few mature editors | ok | no | new dependency |
| JSON | no comments to keep | ok | no | free |
| Lua | **impossible** | yes | yes | new dependency + a C compiler |
| YAML | partial | poor¹ | no | new dependency |

¹ The Norway problem lands squarely on single-letter keybindings: bare `n`, `y`
and `on` parse as booleans.

`toml_edit 0.22` is already in the dependency tree — `toml 0.8` is built on it —
and it is what `cargo add` uses to edit a `Cargo.toml` while keeping your
comments. So the panel's hardest requirement is met by something already
compiled, with no migration and no new dependency.

KDL is the better *data* format on the merits, and would have been the choice if
the config were only ever hand-written: it nests keymaps without TOML's
single-line inline-table restriction. It lost on being a new dependency with a
less proven round-trip, against an incumbent that is free.

**A note against an earlier objection.** TOML's array-of-tables (`[[bind]]`) was
called out as verbose — four lines per binding, 240 for sixty. Under this
decision that is a *strength*: a uniform shape is trivial for a panel to append
to, remove from and reorder, where an inline table is not.

## Why not Lua as the config format

It is the tempting answer, and one argument for it is genuinely strong: a computed
theme. `added_word_bg = lighten(added_bg, 0.12)` means changing one base colour
moves fifteen derived ones, which is exactly the loop the live reload exists to
serve.

It loses anyway, on three counts:

**The panel cannot round-trip code.** There is no way to represent
`lighten(base, 0.12)` in a checkbox and write it back. This alone is decisive
given requirement 1.

**Arbitrary execution on load.** A shared theme becomes a script you have to
trust. Sandboxing is possible and is real work nobody has asked for.

**It could never be the whole extension story anyway** — see below.

## What can never be scripted, whatever the language

Worth pinning now, because it is the thing that will be re-argued when plugins
arrive. The seams split by how hot they are:

| seam | called | scriptable |
|---|---|---|
| commands, keybindings | per keystroke | yes, comfortably |
| `Differ` | per file, at load | probably |
| `Highlighter` | per hunk, at load | maybe |
| `Rows::render`, `runs()` | **per visible row per frame** | no |

`Rows::render` runs about fifty times a frame; `runs()` rebuilds a style list per
visible row per redraw deliberately, because caching it for 714k rows costs 40×
the memory of the rows. Neither can cross a script boundary.

`Highlighter` is the interesting one: it is called per hunk at load, so the *rate*
is survivable, but its contract is not. Its ranges must be sorted,
non-overlapping and on char boundaries —
[syntax-highlighting.md](../syntax-highlighting.md) notes that breaking the last
one panics inside GPUI's text layout. Rust's type system and the tests enforce
that today; a scripted highlighter turns it into a runtime contract to be
validated on every hunk of a 714k-row diff.

So Lua's territory is commands, keybindings and workflow. The per-frame seams stay
native, and a scripted `Highlighter` needs a validation pass in front of it before
it is allowed near the renderer.

## Consequences

**No computed colours in the config, and that is the real cost.** The mitigation
is to notice that derivation usually belongs in `core` rather than in the file:
`Theme::rebuild` already resolves every syntax colour against every surface with
`readable()`, so the config states *intent* and `core` derives the rest. When a
field is only ever "the same as that one but lighter", the answer is to derive it
in `Theme` and stop asking the config for it — `added_word_bg` against `added_bg`
is the obvious candidate.

**A second format is still possible later** and this decision does not block it.
Adding Lua is additive; it removes nothing, because the config layer never held
behaviour in the first place.

**Reversal is cheap until it ships.** `config::apply` is a pure function of a
string and `config::dump` its inverse; swapping formats is those two functions and
their tests, and nothing else in the codebase knows what TOML is. What gets
expensive is not the code but the compatibility promise, and there are no users
yet.

**The panel does not exist**, and this decision is most of what it needs: a
format it can rewrite in place, and a rule keeping anything unrewritable out of
the file it owns.
