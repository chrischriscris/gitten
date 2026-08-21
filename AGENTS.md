# plait

A desktop git client with lazygit's keyboard model. Rust, GPUI.

## Three rules, in priority order

1. **Extensible.** Anything a built-in does, an extension must be able to do too.
   Catch yourself hardcoding something a user might want to swap — make it a trait first.
2. **Beautiful and usable.** Dense, quiet, keyboard-first. Not done until it looks like it belongs.
3. **Blazingly fast.** Nothing on the render path allocates per frame or recomputes what a cache could hold.

They conflict sometimes. That order breaks the tie.

This file is philosophy only. Systems, decisions and the numbers behind them live
in `docs/` — start at `docs/README.md`. Nothing about a particular feature belongs
here; if it needs writing down, it goes there or into a doc comment.

## The one architectural rule

`core/` never knows a UI exists. No GPUI, no rendering, no terminal. It has zero
dependencies today and that is deliberate: it compiles in a second and its tests
need no window.

Everything except drawing and input belongs there — git, lane assignment, diffs,
the extension host, command dispatch, the mode stack. If a keystroke can trigger
a command, `cli/` and an extension must reach it through the same path. One
implementation, three doors.

## Git

`plait-git` is the acquisition layer — the only crate that talks to a repository.
`core` stays pure and does no I/O; `shell` does no I/O either. Both views take
already-loaded data, which is also why they are trivial to test and to drop into
a pane.

**Reads through `gix`** — status, log, diff, graph traversal. These run on every
keystroke; no process spawn on the hot path.

**Writes through the `git` binary** — push, pull, merge, rebase, commit. Rare and
latency-insensitive, and shelling out means hooks, credential helpers, SSH agents
and `.gitconfig` behave exactly as they do in the user's terminal. Don't
reimplement any of that; you will get it subtly wrong.

Both behind one trait. Frontends never learn which path ran.

**It acquires content, not diffs.** Two lists of lines per changed file, and
`core` decides which lines correspond. Let git produce the diff and git owns the
algorithm, which makes `trait Differ` decoration and rule 1 false for the most
important thing this app does. Two processes for a whole diff however many files:
one `git diff --raw`, one `git cat-file --batch`.

## Diffs

Histogram — not Myers. Histogram anchors on lines appearing exactly once, so
function signatures become anchors and a moved block reads as a move instead of
dissolving into line-soup. Git defaults to Myers. We deliberately don't.

Diffing is a `trait Differ` and the algorithms are written out in `core`, not
pulled in — `core` has no dependencies and that is the rule, not housekeeping.
Histogram, patience and Myers are the first three; semantic and language-aware
differs arrive as extensions. The view never calls a differ directly, and an
implementation returns only the edit script: line numbers, context and hunk
headers are shared.

**Check a differ against git, don't argue about it.** `diffcheck` compares
changed-line counts *and every hunk position* against six git invocations over
four repositories. Three bugs so far were invisible in the totals: two showed up
in the per-file deltas, and the third only in the positions — identical counts,
hunks in places git does not put them. Compare the positions.

Two things in histogram are wrong in the plausible direction, so read them before
touching them: a run is scored by its **rarest** line, not its most common one,
and the threshold **tightens** as the search runs. Getting either backwards costs
hundreds of spurious changed lines and still looks like a working diff.

Both algorithms are quadratic in the number of differing lines in the worst case.
Bound them and degrade to "this region was replaced"; a line-for-line pairing of a
generated file is not worth a visible pause and nobody was reading it. Recurse on
an explicit stack — a file whose every anchor peels off one line is as deep as it
is long, and generated code has that shape.

**Everything except the edit script is shared.** Whitespace normalisation, the
indent-heuristic slide, hunk assembly and move detection all live beside the trait
and run for every implementation, including one an extension compiles in. A
`Differ` decides which lines correspond and nothing else.

Ignoring whitespace is an **equivalence relation, not an algorithm** — normalise
per line, length-preserving, and the script still addresses the original lines
while the hunks show the real text. That is what stops `histogram-ignore-ws` from
having to exist.

A **moved** block is deleted here and added there, and it is the one thing in a
diff a reader may skip. Flag it beside `kind`, never as a fourth `LineKind`: it is
still an addition or a removal, and `align`, `replace_pairs` and the adds/dels
counts must not learn about it. Require three lines — two matching lines are a
coincidence and `}` is everywhere.

**Port git's heuristics; don't approximate them.** The indent heuristic's weights
are xdiff's because an approximation produces boundaries no test can call right.
Its one trap: a position's indentation is compared by *sign*, not magnitude.

**Fence a code block; never indent one.** The renderer knows fences and nothing
else, so a four-space block is prose — emphasis, links and list markers all get
interpreted inside it. `#` was the worst of it: a `# comment` trailing an
indented command read as an `<h1>` until `heading_level` started counting the
indent, which is CommonMark's rule and now shared by the block pass and the token
pass. Applies to this file too.

Intraline highlighting is a **second pass**: line diff first, then re-diff only the
changed pairs at word level. Words, not characters — char diffs on code are
confetti. Never diff untokenized text; it degrades badly.

**Which removal pairs with which addition is decided once, in `core::align`.** The
intraline pass and the side-by-side layout both read it. Pair differently in a
renderer and you draw a removal beside an addition whose changed words were
computed against another line — highlighting that corresponds to nothing on
screen.

## Layouts

Unified and side-by-side are two entries in a registry, not two branches of a
`render`. A `Layout` is a name and a closure that builds a set of `Rows`; `s`
cycles them and `[diff] layout` picks the one to open in. The registry is
shell-side because a `Rows` implementation returns a UI element; the *name* is on
`Host`, because a name is data.

Adding a presentation must need no edit to the existing ones. `SplitRows` needed
no new trait, no new argument and no change to `TextRows` — that is the only test
of a seam that counts. A registered layout appears in the title-bar dropdown
without being told to; if a new seam needs a control written for it by hand, the
seam is shaped wrong.

Cache diffs by blob OID. They never change. Acquisition yields both OIDs for
exactly this reason; the cache itself is not built.

## Wrapping

**A wrapped line is more rows, never a taller one.** `uniform_list` is the only
reason 714k rows scroll, and it needs every row the same height. So a wrap returns
byte ranges into a line and the line stays one line — because the edit script, the
hunk numbers, `replace_pairs`, `align`, the spans and the tokens all address
lines, and splitting one in two before those ran pairs a removal with the wrong
addition.

Two things are wrong in the plausible direction. **Word wrap searches backwards
from the column**, so the row is never wider than the budget; forwards overflows
by however long the next word is and looks right on prose. And **the budget is per
line, not per diff** — a bullet, an indent and an 18px heading each cost
characters, and one number for the whole diff is what makes a presentation write
its own wrap.

`off` is an entry in the registry, not a flag beside one — the pickers are a pure
function of a registry, so that is what puts it in the menu for free. Unlike the
layouts this registry *is* on `Host`: a break point is a property of text, so
`core` can hold the implementations, and what the frontend supplies is the column
count. Everything else is shared — the range partition, the validation, the flat
table — so a `Wrap` decides where a line breaks and nothing else.

## Building

```sh
./check.sh                          # everything headless: tests, every fixture,
                                    # and the differs against git's own answer
cargo run -q -p plait-git --example diffcheck --release [REPO] [REVSPEC]
                                    # just that comparison; WORST=1 for the
                                    # files it did worst on
./dev.sh diff . HEAD~2..HEAD        # rebuild + relaunch on every save,
                                    # landing back on the same row.
                                    # Debug + stats overlay by default,
                                    # so its timings mean nothing —
                                    # ./dev.sh --release for real ones
cargo test -p plait-core            # just correctness, sub-second
cargo build --release -p plait-shell
./target/release/plait-shell commits [REPO] [LIMIT]
./target/release/plait-shell diff    [REPO] [REVSPEC]
./target/release/plait-shell diff --fixtures        # read fixtures/ instead
PLAIT_STATS=1 ./target/release/plait-shell diff     # frame/heap overlay
```

The overlay forces a redraw every frame so the fps number means something —
GPUI is reactive and draws nothing at rest, so an honest idle reading would be
zero. It measures how fast we *can* redraw, not what the app costs sitting still.
Never read those numbers off a debug build.

Colour and font live in `plait.toml` and reload on the next frame — no rebuild.
`[diff] algorithm`, `context`, `layout` and `wrap` live there too. `context`
applies on the next launch; the others have controls in the title bar and change
live, and the file sets what they *open* on. A control there is the temporary answer
until keybindings and a settings panel are config — the picker is a pure function
of a list and an index, so any seam with a registry gets one for free.
`plait config > plait.toml` writes a complete one. Code still costs a rebuild;
`./dev.sh` is what removes the quitting and retyping around it.

**Never launch the app unless asked.** Build it, test it, bench it — but a window
appearing unannounced interrupts whoever is at the keyboard. Say it's ready and
hand over the command.

`rust-toolchain.toml` tracks the channel Zed pins. When GPUI fails with an
unstable-feature error, that pin has drifted — go read Zed's `rust-toolchain.toml`.

Never judge performance on a debug build — `cargo run` without `--release` is a
different, much slower binary, and the title bar says so. `[profile.dev.package."*"]
opt-level = 3` optimizes dependencies in dev builds so `cargo run` is at least
usable; our own crates stay unoptimized and debuggable.

Fixtures: `./fixtures/dump.sh <repo> [count]` for real, `./fixtures/gen.sh <n> <m>`
for synthetic at any scale. Use both — synthetic tests scale, real tests *shape*,
and shape is where the crashes live. Kept under `fixtures/real/`:

- `~/Projects/git` (blobless, 82k commits) — the tree stress case. 26% merges,
  280 lanes, 37 octopus merges up to 10 parents, 7 roots. Big popular repos are
  *not* a substitute: bun has 17k commits and 97% of rows sit at one lane,
  because squash-merge workflows produce straight lines.
- `pr30683.diff` (714k lines, near-pure deletion, a 65k-token line),
  `pr30698.diff` (the zig→rust migration), `pr33933.diff` (near-pure addition).
  Each pathological in a different direction.
- `md.diff` (rust-lang/book, 72k lines) — the only *prose* fixture, and the one
  the rendered Markdown presentation is measured on. Prose is edited a sentence at
  a time, which makes it the heaviest intraline case in the set: 72 ms of a 91 ms
  `prepare`, ahead of `pr30698`. Code diffs replace lines; prose diffs replace
  words inside them, and no code fixture showed that. A technical-docs tree is a
  third distribution again — a third of the paragraphs, six times the headings,
  92 replace-pairs total — and `docs/measurements.md` has both.

**Always log with `--topo-order`.** It is what git itself uses for `--graph`, and
lane assignment assumes it: without it, branches interleave and the drawing is
simply wrong. It is *not* a width optimization — it narrows git/git (417 -> 280
lanes) and widens cmux (19 -> 73). Correctness, not compactness.

**Never `read_to_string` anything from git.** Commit metadata is not guaranteed
UTF-8 and real history carries Latin-1 author names; `git/git` panics it outright.
Read bytes and `String::from_utf8_lossy`. Never fail to show a repo over one bad byte.

## The graph gutter is capped

12 lanes, hard. git/git runs 280 concurrent lanes, which is a 3,920px gutter that
pushes the commit text clean off the screen. Lanes past the cap collapse onto the
last column in a dim grey so the overflow is visible rather than silently misdrawn.
Nobody reads past a dozen lanes; git's own `--graph` is unreadable well before that.

## GPUI notes

Interactivity requires identity. `.id()` before `.overflow_y_scroll()`, clicks,
hover, drag — those methods live on `StatefulInteractiveElement` and there is no
way in without an id.

`uniform_list` for anything long. It builds only visible rows and brings its own
scrolling, so don't wrap it in another scroll container. For horizontal scrolling
use its own `with_horizontal_sizing_behavior(Unconstrained)`, make rows `flex_none`
(anything `overflow_hidden` clips instead of scrolling), **and** point it at the
widest row with `with_width_from_item(Some(i))`. That last one is the trap: the
list measures exactly ONE row to decide its scrollable width and defaults to row
0, so a short first row means nothing scrolls no matter how long the rest are.
Compute the widest index at load.

Closing the last window does not end the process; macOS keeps appless processes
alive. `cx.on_window_closed` + `cx.quit()`. Cmd-Q is separate and equally manual:
no application menu exists, so register the action, bind the key, and set a menu
(`on_action` + `bind_keys` + `set_menus`).

A bare binary is not an `.app` bundle, so the window opens behind everything.
`cx.activate(true)` is the dev fix; a real bundle is the shipping one.

Custom drawing is `canvas()` + `PathBuilder` + `window.paint_path`. Keep it
per-row where the geometry allows and it virtualizes with the list for free.

**A view cannot know its own size during `render`.** It is handed a box by
whatever assembled it, and that happens after. A zero-height `canvas` reports the
box during paint, so anything sized by it lands on the frame *after* — which is
correct and one frame late, and is what wrapping runs on. Reaching for
`window.viewport_size()` instead is the shortcut, and it is a view assuming it
owns the window: right until there are panes.

**Anything that floats needs `deferred`.** Siblings paint in order, so a dropdown
overflowing the *first* child of a column is painted under the second — visible
nowhere, and it looks like the element was never built. `gpui::deferred(child)`
keeps the layout where it is and moves the paint after every ancestor.
`.occlude()` on top of that, or the rows underneath take the clicks: hit-testing
is paint order too, and an absolutely positioned child does not claim the space it
covers.

**An element's identity is its path, and unnamed ancestors are not in it.** Two
controls whose inner elements are both `.id("list")` are the *same* element, so
one drives the other's hover and click state. Give the wrapper an id, or make the
inner ones unique.

**Read the host on the render path, never a captured clone.** `DevShell` held an
`Rc<Host>` from startup, so the window chrome and the font for the whole window
silently did not hot-reload while every view inside them did. `config::host(cx)`
per frame is a refcount bump.

## Don't

Don't add dependencies to `core/`.

Don't put logic in `shell/` that `cli/` would have to duplicate.

Don't build what the framework already has. `uniform_list` was there the whole
time and a hand-rolled list cost a day.

Don't let an AI feature bypass the extension API. The built-ins are extensions;
that's the proof the API is worth using.
