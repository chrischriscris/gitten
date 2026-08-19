# plait

A desktop git client with lazygit's keyboard model. Rust, GPUI.

## Three rules, in priority order

1. **Extensible.** Anything a built-in does, an extension must be able to do too.
   Catch yourself hardcoding something a user might want to swap — make it a trait first.
2. **Beautiful and usable.** Dense, quiet, keyboard-first. Not done until it looks like it belongs.
3. **Blazingly fast.** Nothing on the render path allocates per frame or recomputes what a cache could hold.

They conflict sometimes. That order breaks the tie.

## The one architectural rule

`core/` never knows a UI exists. No GPUI, no rendering, no terminal. It has zero
dependencies today and that is deliberate: it compiles in a second and its tests
need no window.

Everything except drawing and input belongs there — git, lane assignment, diffs,
the extension host, command dispatch, the mode stack. If a keystroke can trigger
a command, `cli/` and an extension must reach it through the same path. One
implementation, three doors.

## Git

**Reads through `gix`** — status, log, diff, graph traversal. These run on every
keystroke; no process spawn on the hot path.

**Writes through the `git` binary** — push, pull, merge, rebase, commit. Rare and
latency-insensitive, and shelling out means hooks, credential helpers, SSH agents
and `.gitconfig` behave exactly as they do in the user's terminal. Don't
reimplement any of that; you will get it subtly wrong.

Both behind one trait. Frontends never learn which path ran.

## Diffs

`imara-diff`, Histogram — not Myers. Histogram anchors on lines appearing exactly
once, so function signatures become anchors and a moved block reads as a move
instead of dissolving into line-soup. Git defaults to Myers. We deliberately don't.

Diffing is a `trait Differ`. Histogram and Myers are only the first two
implementations; semantic and language-aware differs arrive as extensions. The
view never calls a differ directly.

Intraline highlighting is a **second pass**: line diff first, then re-diff only the
changed pairs at word level. Words, not characters — char diffs on code are
confetti. Never diff untokenized text; it degrades badly.

Cache diffs by blob OID. They never change.

## Building

    ./check.sh                          # everything headless: tests + every fixture
    cargo test -p plait-core            # just correctness, sub-second
    cargo build --release -p plait-shell
    ./target/release/plait-shell [commits|diff]
    PLAIT_STATS=1 ./target/release/plait-shell diff     # frame/heap overlay

The overlay forces a redraw every frame so the fps number means something —
GPUI is reactive and draws nothing at rest, so an honest idle reading would be
zero. It measures how fast we *can* redraw, not what the app costs sitting still.
Never read those numbers off a debug build.

**Never launch the app unless asked.** Build it, test it, bench it — but a window
appearing unannounced interrupts whoever is at the keyboard. Say it's ready and
hand over the command.

`rust-toolchain.toml` tracks the channel Zed pins. When GPUI fails with an
unstable-feature error, that pin has drifted — go read Zed's `rust-toolchain.toml`.

Never judge performance on a debug build. GPUI debug is not representative.

Fixtures: `./fixtures/dump.sh <repo> [count]` for real, `./fixtures/gen.sh <n> <m>`
for synthetic at any scale. Use both — synthetic tests scale, real tests *shape*,
and shape is where the crashes live. Kept under `fixtures/real/`:

- `~/Projects/git` (blobless, 82k commits) — the tree stress case. 26% merges,
  280 lanes, 37 octopus merges up to 10 parents, 7 roots. Big popular repos are
  *not* a substitute: bun has 17k commits and 97% of rows sit at one lane,
  because squash-merge workflows produce straight lines.
- `pr30683.diff` (714k lines, near-pure deletion, a 65k-token line),
  `pr30698.diff` (the zig→rust migration, heaviest intraline), `pr33933.diff`
  (near-pure addition). Each pathological in a different direction.

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

## Don't

Don't add dependencies to `core/`.

Don't put logic in `shell/` that `cli/` would have to duplicate.

Don't build what the framework already has. `uniform_list` was there the whole
time and a hand-rolled list cost a day.

Don't let an AI feature bypass the extension API. The built-ins are extensions;
that's the proof the API is worth using.
