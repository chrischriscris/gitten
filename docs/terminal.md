# The terminal frontend

`plait-tui`. The third door, after the GPUI window and the browser.

It exists to be the cheap check on the boundary. `docs/architecture.md` names the
test — *a second frontend needs no logic of its own* — and a terminal is the
hardest place to cheat: it has no layout engine to hide work in, no container
that scrolls itself, and no element tree. If something a view needs is not in
`core`, it is immediately obvious, because there is nothing else there.

Writing it moved six things out of the clients, and one of them —
`plait.toml` — could not be reached from anywhere but the window before. That is
the interesting part of this page; [clients.md](clients.md) is the general
version.

## What is shared, and what each door still owns

```
                        core                          per frontend
  ─────────────────────────────────────────────  ────────────────────────
  prepare        clip, intraline, syntax
  rows::Flat     File/Hunk/Line rows, wrap index
  rows::Present  claims, len, build, rows, width  + render → a UI type
  rows::expand   visual ↔ logical order table
  rows::assemble the whole load path
  runs::runs     tokens × spans → styled runs     → HighlightStyle / SGR / JSON
  graph::Hues    which branch is which colour     → curves / box drawing
  graph::MAX_LANES, lane_count
  command::*     a key → a command name            → a platform event → Key,
                                                     and a match on the name
  select::*      carets, rows, words, the copy,     → where a click landed, and
                 what finishing a drag means         a background on a run
  view::Viewport top, cursor, and the thumb         → glyphs for the scrollbar
  app::config    plait.toml, watched                how a reload reaches a view
  app::cli       the arguments, the usage           its own flags
  app::acquire   a view of a source → data
  theme, font, wrap, differ, syntax
```

Everything on the left had at least two implementations before, or — for
`command` — none that was shared at all. `web/src/rows.rs` and
`shell/src/views/diff.rs` have not been migrated onto `core::rows` yet, so the row
flattening has one canonical implementation and two copies that predate it;
`runs` and `graph` are canonical everywhere except `plait-web`; `command` is used
by this client and not yet by the window. See [Still to do](#still-to-do).

## The one thing a `Rows` implementation owns

`render`, and its return type is the reason the trait cannot live in `core`:

| frontend | `render` produces |
|---|---|
| `plait-shell` | `AnyElement` |
| `plait-web` | text pieces on the wire |
| `plait-tui` | cells, through a `Pen` |

Everything above it is `core::rows::Present`, which the frontend trait extends.
So a presentation that exists in one door is a `render` away from existing in
another, and `SplitRows` is the proof: `tui/src/split.rs` is
`shell/src/views/split.rs` with the GPUI taken out and *no* pipeline code, no
second alignment rule, no second wrap table.

## Two dependencies, and where the line is

`crossterm`, for what `gpui_platform` does for the shell: raw mode, the alternate
screen, and **parsing a keypress out of a byte stream**. That last one is a
decade of terminal archaeology — a terminal reports `Shift-F5` differently
depending on the emulator, the terminfo entry and whether the kitty protocol is
on — and it is exactly the "don't build what the framework already has" case. It
is confined to `term.rs`; nothing else in the crate imports it, which is what
makes the views testable.

`unicode-width`, for how many columns a character occupies. `Font::advance` is
the window's answer to the same question and has none here. Already in the lock
file as a gpui dependency, so it costs no download and no compile.

Drawing is *not* on that list. Cell diffing and 24-bit colour are forty lines,
and owning them is what gives the views a headless render target — see below.

## The screen is ours, and that is the point

`screen::Screen` is a `Vec<Cell>`. Presentations draw into it; `flush` compares
it against what is on the terminal and writes escape codes only for the runs that
differ.

**This is the only frontend whose drawing is tested.**
`docs/architecture.md`'s *Not built yet* lists "a rendering test" as the shell's
one untested stage: a panic in `render`, a colliding element id or a floating
element painted under its sibling are all found by launching. Here,
"the second row is a removal, red on dark red, with the changed word lit" is an
assertion:

```rust
let x = (0..40).find(|x| screen.char_at(*x, 3) == Some('1')).unwrap();
assert_eq!(screen.ink(x, y).unwrap().bg, theme.diff.removed_word_bg);
assert_eq!(screen.ink(x - 1, y).unwrap().bg, theme.diff.removed_bg);
```

`Screen::print` is the other half: the whole grid as lines, with no cursor
positioning, so `examples/dump.rs` prints a real frame of the real views to
stdout. No window appears — which `AGENTS.md` asks for explicitly, and which is
also how a colour gets checked in a code review.

## What is genuinely different in a terminal

Four things, and only these. Everything else is the same code.

**A column is a cell, not a fraction of an em.** So `Rows::reflow` takes columns
where the shell's takes pixels, and `screen::cols` replaces `Font::advance`.

**A view knows its own size before it draws.** GPUI hands a view its box during
paint, which is why the shell's wrapping lands one frame late
([decisions/0017](decisions/0017-wrapping-is-more-rows-not-taller-ones.md)); a
terminal is queried, so a resize is a method call.

**Nothing scrolls itself.** No `uniform_list`, no scroll container. Vertical
scrolling is a `for` over `top..top + height`, which virtualizes for free.
Horizontal scrolling is `Pen::scroll(n)`, which swallows the first *n* columns of
everything written after it — so the line numbers and the `+`/`-` stay put while
the text moves under them. Slicing the text instead is the obvious alternative
and it is wrong: the tokens and the intraline spans address the *line*, so a
slice taken before `runs` pairs styling with the wrong bytes.

Side-by-side is the same thing twice, for the same reason. A terminal has no
container to be wider than itself: a column wider than half the screen puts the
right-hand gutter off the edge. So the columns stay half the screen and their
contents scroll — and the window does that now too, because a column sized to the
widest line in the diff was the right file off the right of the screen on every
row of every file. See
[decisions/0023](decisions/0023-the-gutter-does-not-scroll.md).

**A wide character claims two cells.** Getting that wrong does not misalign one
row, it shears every row below it, because the cursor ends up somewhere the grid
does not agree with. A lead cell plus a continuation cell, and `flush` extends a
changed run backwards to the lead cell before positioning the cursor.

## The graph, in box drawing

Topology is `assign_lanes` and colour is `graph::Hues`, both untouched. What this
decides is the alphabet: two columns per lane — `git log --graph`'s spacing, and
the only way a `─` between two lanes is expressible at all.

`git log` is newest-first, so a row below is *older*: a lane converging on a
commit came from **above** and its corner points up (`╯`), and a lane forked out
of it continues **below** and points down (`╮`). Backwards, this draws a history
where branches merge into their own children.

The honest cost of one row per commit: the window paints a branch changing lanes
as an S spanning a whole row, in halves that meet on the boundary. A cell grid
has no halves, so a merge and a fork are drawn on the commit's own row as a
horizontal run with a corner at the far end — which is what `git --graph` and
lazygit both do. A merge and a fork in the same lane on the same row collapse
into one glyph.

`Glyphs` is a struct and not a set of literals, so `Glyphs::ascii()` is the
whole of what a terminal without box drawing needs, and a Nerd Font set is one
more constructor.

**The gutter is one width for the whole list**, unlike the window's per-row one.
The window can scroll a container wider than itself; a terminal cannot, and a
subject starting in a different column on every row is a list the eye cannot
scan.

## The loop

`main.rs`, and it is thin on purpose: a key, a command name, a method.

```text
  crossterm event → term::translate → Key → Keymap::resolve → "diff.next-file"
                                                                    │
                                                    Screens::run ───┘
```

Nothing in that file decides what a key *does*. The keymap is on `Host`, so
`plait.toml` and an extension reach it the same way, and `?` lists whatever is
actually bound because the help panel is a pure function of the registry. See
[clients.md](clients.md) for the seam.

Four things about it are decisions:

**A wheel notch is a key.** `Code::WheelUp` and `Code::WheelDown` are variants of
the same enum `j` is, so the wheel resolves through the keymap, appears on the
`?` panel, and is rebindable in `plait.toml` — where the alternative was a
`match` in this client deciding that the wheel scrolls, which is the keymap
`core::command` exists to stop three clients each owning. What it runs is
`view.scroll-down` / `view.scroll-up`, also on `ctrl-e` / `ctrl-y`, because
moving the *view* is a different verb from moving the cursor and deserved a
command rather than a flag on `view.down`. Where the pointer was is dropped:
there is one scrollable thing on screen, and a coordinate nothing can route is
one that gets routed wrong the day there are panes.

**A button is not a key**, for the same reason and pointing the other way: a
position cannot be a line in `plait.toml`, because a config file cannot hold a
hit test. So it arrives as an `Input::Mouse` and this file routes it — see
[the mouse](#the-mouse) above, and
[decisions/0022](decisions/0022-the-mouse-in-a-terminal.md).

**One row per event, and that is not a taste.** A terminal reports the wheel once
per *line* of the platform's scroll delta, not once per notch — Ghostty on macOS
sends three for one detent of a mouse — so a client that multiplies is
multiplying a number the user already tuned in System Settings. Three rows an
event measured as nine rows a notch here, which reads as a page. At one, plait
scrolls at exactly the speed the terminal's own scrollback does. `[view] scroll`
is the multiplier for anyone whose emulator sends one event per notch, where 3 is
the usual answer.

**It is idle at rest.** The loop blocks on input with a 150 ms timeout, and the
timeout exists only so a saved `plait.toml` is noticed. Nothing redraws unless
something happened — the property GPUI gives the window for free, arrived at here
on purpose.

## The mouse

A notch is a key and a button is a place, and that split is the whole design —
see [decisions/0022](decisions/0022-the-mouse-in-a-terminal.md).

```text
  wheel   ──► Code::WheelUp/Down ──► Keymap ──► "view.scroll-up"   rebindable
  button  ──► Input::Mouse ──► main: which row of the body ──► a view's method
```

`main.rs` owns two rows — the title bar and the status line — so all it does is
subtract them and hand the rest down. Everything below that is the view's,
because only a presentation knows where its own text starts:

| what | where |
|---|---|
| which visual row a cell is over | `Diff::locate`, from the viewport |
| which text and which byte | `Rows::hit`, per presentation, in columns |
| which rows lie between two carets, and what they copy | `core::select` |
| how many times you clicked | `main.rs`, against a 400 ms clock |
| whether finishing one copies it | `[mouse] copy_on_select`, read per gesture |

**A click moves the cursor**, so the keyboard carries on from where the pointer
left off — `enter` on the commit you just clicked, `y` on the line you just
pointed at. **A drag selects**, and past the top or the bottom of the body it
scrolls by the overshoot and keeps going. **Two clicks take a word and three take
the row**, with `core::select::word_at` deciding what a word is, because a
terminal and a window must not disagree about `foo(bar,`. Two clicks in the
commit list open the diff.

Three things about it are worth knowing:

**A cell has no right half.** A window rounds a click to the nearest character
boundary, which is what makes a drag include the character it started on. There
is nothing to round here, so the *head* of a drag is taken one column further on
than a click would be — `Diff::head`, and the direction is asked first so a
backwards drag is not off by one the other way.

**The commit list selects rows, not bytes.** A commit row is a sha, initials, a
graph and a subject in fixed columns, and a graph is not a thing anybody pastes.
So a drag there takes whole commits and `y` yields `sha subject` per line. The
diff selects bytes, because a diff is text.

**Finishing a selection copies it.** `[mouse] copy_on_select`, on by default,
because taking the drag took select-then-`cmd-c` with it and the app owes that
back — X11 terminals have worked this way since 1987. A **drag** copies and a
**click** does not: a click is a cursor move, and a clipboard that changed
whenever you pointed at a line is one you cannot keep anything in. A double or a
triple click is a selection and does copy. `y` copies either way, and copies the
row the cursor is on when nothing is selected.

Once per gesture, on the button coming up — a write per motion event would be an
escape sequence per cell the pointer crossed. The status line says `copied 3
lines`, which is the only feedback there is.

**Copying is OSC 52**, not `pbcopy` and not a clipboard crate: a terminal is
frequently not on the machine the clipboard is on, and OSC 52 is the one
mechanism that follows the session rather than the process. `cmd-c` and
`ctrl-shift-c` cannot be used for this — the emulator eats them before the pty,
and what they copy is the *emulator's* selection, which is empty while plait
holds the mouse. The cost is that an emulator without OSC 52 copies nothing and
cannot say so — there is no reply. tmux needs `set-clipboard on`, which is its
3.x default.

## The scrollbar

One column at the right-hand edge, drawn **over** the rows rather than beside
them, so a removal's red still reaches the edge underneath it. Reserving a column
instead costs a reflow: one column fewer is a different wrap, a different row
count and therefore a different scrollbar.

Where the thumb goes is `Viewport::thumb` and is `core`'s, because it is
arithmetic about a list — and it has the property the obvious version misses: the
thumb touches the end of the track **exactly** when the last row is on screen.
Proportional-to-`len` leaves it short of the bottom on a list scrolled all the way
down, which reads as "there is more" when there is not. It is never shorter than
one cell, so 714k rows still have something to grab, and there is no bar at all
on a list that fits. `[view] scrollbar = false` turns it off; `--ascii` draws it
as `|` and `#`.

**`enter` opens a diff and `esc` comes back**, as a *stack of screens* rather
than a pane. The acquisition is in `main`, not in the view: a view takes
already-loaded data and never learns what a repository is, which is the same rule
the GPUI client follows. A bare revision is "what did this commit change" to
`plait_git::pairs`, merges included.

## Cost

Loading is `core`'s and is the same number in every frontend. Drawing is
per-visible-row and independent of the diff, which is the whole claim:

| fixture | rows | load | frame |
|---|---|---|---|
| `pr30683.diff` | 740,383 | 473 ms | 15 µs |
| `pr30683.diff`, side-by-side | 973,394 | 532 ms | 16 µs |
| `md.diff` | 74,467 | 110 ms | 12 µs |
| `md.diff`, side-by-side | 90,963 | 117 ms | 14 µs |
| `git/git`, 82k commits | — | 156 ms | 28 µs |

The scrollbar and the selection are inside those and are not measurable in them:
turning the bar off entirely moves the first row by less than the noise between
two runs. See [measurements.md](measurements.md).

```sh
./dev dump diff --fixtures
```
prints the frame and the timings; `FRAMES=n` sets how many repaints to average.
Release only — a debug build measures a different program, exactly as the
window's overlay says.

Nothing in a frame allocates. The run-list buffer is the caller's and is reused
across frames, a row's text is sliced out of the line rather than copied, and
`core::runs` walks the tokens and the spans together instead of collecting their
combined edges — that last one was worth 2 µs of the 14 the first version cost.

## Known wrong, and deliberately

**Wrap budgets are counted in characters, not columns.** `core::wrap` has no
dependencies and cannot ask how wide a glyph is, so a line of CJK wraps a little
wide and the pen clips it — a visible truncation rather than a broken grid.
Fixing it properly means a way for a frontend to tell `core` how it measures a
column, which is a change to the `Wrap` seam and not a new implementation behind
it. The window has the same approximation through `Font::advance`, where it is
also wrong and much less visible.

**A combining mark at the start of a row is dropped**, having nothing to attach
to. A terminal does the same.

## Still to do

- **Assembly.** The views are components: `Diff` and `Commits` hold state and
  expose commands (`down`, `page`, `jump_file`, `cycle_layout`), and neither
  knows what a keypress is. There is no `main`, no event loop and no keymap —
  deliberately, because command dispatch and the mode stack belong on `Host` and
  are not built, and a keymap written in `tui/` is one `cli/` would have to
  duplicate.
- **Migrating `shell` and `web` onto `core::rows`.** Both still hold their own
  row flattening and order table. `shell`'s is the harder one: `TextRows` stores
  `SharedString` so GPUI is handed a refcount bump rather than a copy per frame,
  so it wants `Flat` plus a parallel table rather than `Flat` alone.
- **`web` onto `core::runs`.** `web/src/rows.rs::pieces` is `runs` with the gap
  handling that `runs` now has. This is a deletion.
- **`MarkdownRows`.** `core/examples/paint.rs` already draws the furniture in
  ANSI, so the terminal version is that function and a `Rows` impl — and the
  furniture itself is then a fourth thing to lift into `core`.
- **`selection_bg` in the shell.** It was added to `ChromePalette` for this
  client because a hardcoded selection colour is not a seam, and it means the row
  the keyboard is on. No GPUI view draws *that* yet. The mouse selection is
  `chrome.selected_bg` and both clients now draw *that* —
  [decisions/0018](decisions/0018-selection-is-a-model-not-a-text-element.md).
- **A drag does not autoscroll on a clock.** Holding the pointer outside the body
  scrolls by the overshoot once, per event, and stops when the pointer stops. A
  timer would fix it and a timer is a thing to own; the selection extends past
  the last visible row without one either way.
- **Panes.** One screen at a time: `enter` on a commit opens its diff *over* the
  list and `esc` comes back. lazygit puts them side by side, and
  `Screen::span` is already the shape that would do it — nothing uses it for
  that yet.
