# Competition — field notes

External snapshots, kept so they are findable at the moment they matter (planning
a phase, shaping a seam). Facts about other products rot fast — re-check the
source before acting on any of this; the *slotting* below is the durable part.

## hunk — adjacent, not head-on

[modem-dev/hunk](https://github.com/modem-dev/hunk), snapshot 2026-08: a
review-first **terminal** diff viewer aimed at agent-authored changesets.
TypeScript/Bun on OpenTUI + `@pierre/diffs`; MIT; ~8.7k stars. It opens diffs
(`hunk diff`, `hunk show`) across git/jj/sapling, runs as pager/difftool, has
watch mode, inline AI annotations, and an experimental TS extension API
(themes, VCS backends, changeset transforms, sidebar replacement).

The overlap is real but narrow: diff presentation, keyboard-first density,
and — almost verbatim — our own instincts (keybindings as named commands in a
TOML table; extensions reaching everything built-ins can). The non-overlap is
the product: hunk is a read-only viewer with no staging, no commits, no graph,
no writes anywhere. It competes with delta and lazygit's diff pane; we compete
with lazygit whole.

| | gitten | hunk |
|---|---|---|
| Surface | desktop GPUI window | terminal (OpenTUI) |
| Scope | full client — lazygit's verb set | read-only viewer |
| Differ | own histogram/patience/Myers, move detection | delegates to `@pierre/diffs`; no structural diffing by its own comparison table |
| Wedge | the whole local-git workflow | reviewing what an agent just wrote |

## What to steal, and when

**Patch input — adopted** (2026-08-23): `git diff | gitten diff -`, or
`gitten diff --patch pr30683.diff` — a mailed patch or a CI artifact with no
checkout, in every client at once because the CLI is shared. A patch takes no
revspec (`Source::Patch`), `-` past the repository slot is help and not a
revision called `-`, and `commits --patch …` refuses with the reason rather
than opening on nothing.

**Watch mode — adopt as consumer #2 of [#3](roadmap.md).** Auto-reload as the
working tree changes needs exactly #3's rails: watcher event takes the same
completion-event → generation-bump → views-re-acquire path a finished write
takes, with selection restored. Building it before #3 means hand-rolling a
private invalidation path and deleting it weeks later. Wiring it through #3
afterwards is nearly free *and* proves the seam has more than one consumer.

**Agent annotations — wait for the extension host.** Notes attached to hunks
or lines are a contributed feature, not a built-in: AGENTS.md already rules
that an AI feature must not bypass the extension API. Nothing to decide until
extension loading exists (tracked in architecture.md, deliberately off the
roadmap).

**Pager/difftool modes — not ours to build.** That is the terminal door, and
hunk owns it well. Our terminal client stays a proof of the boundary.

## Smaller gaps

Known, none urgent, recorded so they are not rediscovered:

- **`tab_width`.** There is no display tab stop anywhere client-side (checked
  2026-08-23: the only tab arithmetic in `core` is xdiff's 8 in
  `differ.rs::indent_of` and CommonMark's 4 in `syntax.rs`, both pinned to the
  algorithms they serve and both must stay). A tab-indented Makefile diff
  renders at whatever the font decides. Config plus a renderer concern; small.
- **`line_numbers = false`, sidebar auto.** Both registry-with-an-index
  shaped, so the existing picker trick gives a title-bar control for free when
  somebody wants them.
- **Distribution.** hunk ships an install script, Homebrew, mise and Nix;
  we have `./dev bundle`. A gap to know about, not code to take yet.
- **Positioning.** Their comparison matrix names the neighbourhood — delta,
  difftastic, diff-so-fancy — useful when a README needs a positioning
  paragraph.

## Not taken

OpenTUI/React (wrong perf model), Pierre diffs (`core` has zero dependencies
by rule and the differs are written out on purpose —
[decisions/0013](decisions/0013-differs-in-core-not-a-dependency.md)), rich
note markup, and above all the review-only scope: that is their product, not
a piece of ours.

## Standing response

Do not race hunk at their game. Their existence validates the diff-viewer
investment and sharpens ours: the differentiator is closing the zero-write-
verbs gap ([roadmap.md](roadmap.md), Phases B→C→D). Chasing watch mode or
annotations ahead of Phase A buys a worse copy of hunk plus nothing.
