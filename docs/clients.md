# Writing a client

gitten is a core and a set of clients. Three exist, and they are **not** equal:

| | |
|---|---|
| `gitten-shell` | **the product.** GPUI. A feature asked for without a client named means this one. |
| `gitten-tui` | planned, and built; the one that comes after the window. |
| `gitten-web` | a *proof*, not a plan. It exists because a client written in JavaScript could not exist at all if `core` had leaked anything UI-shaped. |

So read this page as two separate obligations. **The seam belongs in `core`, in
the same pass as the feature** — that is what makes a fourth client possible and
what keeps the window honest. **The implementation belongs only where it was
asked for.** Writing the terminal's version of something nobody asked for is
scope; hardcoding a constant that makes the terminal's version impossible is a
bug. Both are avoidable and only one of them is work.

When the two pull against each other, the window wins and the seam waits: a
shared abstraction is never worth a worse desktop app.

This page is what a client is responsible for, and what it must not
reimplement.

## The line

```
   gitten-core   the pipeline. no dependencies, no I/O, no idea a UI exists
   gitten-git    acquisition. the only crate that talks to a repository
   gitten-app    gitten.toml, the command line, loading, background jobs
   ──────────────────────────────────────────────────────────────────────
   a client     drawing, and input
```

Everything above the line has **one** implementation. If a second client needs
something that is below it, that is a bug in the layering, not a thing to write
twice — and it has been three times already:

| what | was written | now |
|---|---|---|
| the row flattening and the order table | twice | `core::rows` |
| tokens × spans → styled runs | three times | `core::runs` |
| which branch is which colour, and the shape of a row | once, in the window | `core::graph` |
| arguments, `--fixtures`, error strings | twice | `app::cli`, `app::acquire` |
| `gitten.toml` | once, behind GPUI | `app::config` |
| which command a key runs | nowhere shared | `core::command` |
| what the mouse has selected, and what copying it yields | nowhere — the window had none | `core::select` |
| where a scrollbar's thumb goes | nowhere — no client drew one | `core::view` |

The last two are the ones that mattered most. Before `gitten-app`, the parser for
`gitten.toml` lived in `shell/src/config.rs` — so the *window* was the only client
that could be configured, and `gitten-web` shipped with a comment apologising for
it. Before `core::command`, a keybinding was three `match` statements that could
not be made to agree.

## What a client's `main` looks like

```rust
let started = match Startup::new("gitten-tui", View::Commits)
    .blurb("gitten in the terminal you started it from")
    .extra(EXTRA)              // this client's own flags, for the usage text
    .go()
{
    Ok(started) => started,
    Err(exit) => exit.finish(),  // --help, `config`, or a failure. Prints and leaves.
};
```

That call has already: parsed the arguments, read `gitten.toml` and printed its
warnings, answered `--help` and `gitten config`, chosen the differ the file asked
for, and acquired the data. What comes back is a `Host`, a `View`, a `Source` and
a `Loaded`.

A client with something to draw before its data exists — the desktop — takes
`Startup::configure` instead: everything `go` does except the acquisition, which
the client schedules itself through the same `acquire::acquire`.

```rust
let configured = Startup::new("gitten-shell", View::Commits)
    .configure()?;   // args parsed, gitten.toml read and warned about; no repository read yet
```

The window opens on empty screens and one background wave fills them. A client
without anything to draw first takes `go()` and never sees the `Configured` that
returns. What is shared is the chain, not the ordering.

A client with its own flags takes them out first, so they may appear anywhere on
the line rather than only where a positional parser looks:

```rust
let mut start = Startup::new("gitten-web", View::Diff);
let port = cli::take_value(start.take(), "--port")?;
```

## The arguments are a promise

```sh
gitten-shell diff . HEAD~2..HEAD
gitten-web   diff . HEAD~2..HEAD
gitten-tui   diff . HEAD~2..HEAD

./dev desktop diff . HEAD~2..HEAD    # …and one script that reaches all three
./dev web     diff . HEAD~2..HEAD
./dev tui     diff . HEAD~2..HEAD
```

Same words, same order, same errors, same `gitten.toml`. A client is a way of
*looking* at a repository, not a different tool, so what you type to reach one
reaches any of them. `app::cli::usage` emits the shared half and folds in the
client's own lines, which is what stops the three drifting — they did drift, in
their error messages, within a week of each other.

One deliberate strictness: **a word that is not a view is `--help`**, not a
repository. Letting the view word be optional so `gitten .` means `gitten diff .`
costs more than it gives, because `gitten dfif .` then shows the default view of a
repository called `dfif` and looks like it worked.

## Input: a key is data, a command is a name

`core::command` resolves a keypress to a command **name**. A client turns that
name into a method call on a view it owns. Nothing in the chain is a function
pointer, which is what lets `gitten.toml` hold it:

```text
  a platform event → Key → Keymap::resolve(&modes, pending) → "diff.next-file"
      per client     core            core                        per client
```

So a client writes exactly two input-shaped things:

1. **A translation** from its platform's event to `command::Key`. In `gitten-tui`
   that is `term.rs`, forty lines, and it is the only file in the crate that
   imports `crossterm`; in the window it is `dispatch.rs`, whose whole job is
   GPUI's keystroke spelling.
2. **A `match` on command names.** `"view.down" => self.down()`. A name it does
   not handle is a key that does nothing, which is what an unbound key does too —
   so a browser tab ignoring `quit` is not a hole.

Everything between is shared: the modes, the chords, the config file, the
validation, and the help screen.

**The wheel goes through the same pipe.** `Code::WheelUp` and `Code::WheelDown`
are variants of the enum `j` is, so a notch resolves to `view.scroll-down` the
way a keypress resolves to `view.down` — rebindable, on the help screen, and the
same name in all three clients. Kept out of `Code`, it would have been a `match`
in each client deciding that the wheel scrolls: three keymaps nobody could
configure, which is the exact thing this module exists to prevent. A wheel's
platform position can travel beside that key so a client with panes chooses the
recipient after resolving the binding. A mouse *position* is not itself a key
and is not here; it belongs to whatever was pointed at, and
`gitten-tui/src/main.rs` is what a client routing one looks like.

### What `Keymap` will not do

**Time.** `g` followed by `g` is a chord and `g` alone is a binding, and telling
them apart needs a clock that `core` does not have. So `Keymap::bind` *rejects* a
chord that is a prefix of another, in either direction, and the ambiguity cannot
arise. Nothing in the shipped map is longer than one key.

**Behaviour.** `Commands` is names and one-line descriptions. It exists for the
two things a name-based system otherwise loses: a help screen that lists what is
actually there, and a config file that can say *no such command* instead of
silently binding a key to nothing.

### `[keys]` in `gitten.toml`

```toml
[keys]
"ctrl-d" = "view.page-down"
"j" = ""                      # unbind: a shipped key must be removable, not only movable

[keys.diff]
"s" = "diff.cycle-layout"     # only where a diff is on screen
```

A bare key is global; a sub-table is a mode, and a mode overrides the global
binding for the same key and inherits everything else. Commands are validated
against `host.commands`, so one an extension registered is bindable the day it
exists and a typo is named.

## What a client still owns

**Drawing**, and the type `Rows::render` returns:

| client | `render` produces |
|---|---|
| `gitten-shell` | `AnyElement` |
| `gitten-tui` | cells, through a `Pen` |
| `gitten-web` | text pieces on the wire |

**The registry of presentations.** `Layouts` is client-side because a `Rows`
implementation returns a UI element. What is on `Host` is `layout` — the *name*
of the one to open in, which is data and therefore configurable.

**How a reload reaches the views.** `config::watch` is shared; what to do when it
fires is not. GPUI swaps a global and refreshes its windows; the terminal drops a
flag into its event loop and redraws. Both rebuild the `Host` from defaults
rather than mutating the live one, so deleting a line from the file makes the
default come back instead of leaving the old value in place.

**A column.** How wide one is, and what one is measured in: `Font::advance` for a
proportional face, `unicode-width` for a terminal cell, whatever CSS says for a
browser. `Rows::reflow` therefore takes pixels in one client and columns in
another, and the implementation owns the conversion because it owns the furniture
it draws around the text.

## The checks, and how to run them

Three, all cheap:

1. **`core/Cargo.toml` has an empty `[dependencies]`.** If something needs
   adding, the thing wanting it belongs in another crate.
2. **A client contains no pipeline code.** `gitten-tui`'s two presentations are
   `TextRows` and `SplitRows`, and neither clips, diffs, highlights or wraps
   anything: they hold a `core::rows::Flat` and draw it.
3. **The same `gitten.toml` drives all three.** `./dev config > gitten.toml` then
   start any of them; a colour, a differ, a wrap, a keybinding and `[view]` all
   apply.

`./dev check` runs the first two by proxy — every crate's tests, then a real frame
of each terminal view over real history, which is the cheapest thing that would
notice a panic in a presentation.

## Not there yet

- **`gitten-web` has no selection of its own**, and does not need one: a browser
  selects text for free. The window and the terminal both drive `core::select`,
  and the terminal's half of it turned out to be exactly what this entry
  predicted — `hit` and `selectable` on its own `Rows`, and nothing else. See
  [decisions/0022](decisions/0022-the-mouse-in-a-terminal.md).
- **`gitten-web` has no input at all**, so the keymap reaches it only once the
  browser sends keypresses to an endpoint. It has `j`/`k`/`g`/`G` in its own
  script, which is exactly the duplication `core::command` exists to end.
- **`gitten-web` still holds its own row flattening**, and its own copy of
  `runs`. `core::rows` is canonical everywhere else — the terminal uses it,
  and the window migrated onto it (`Ordered` plus `RowRef`, so GPUI keeps its
  refcount-bumped strings and the order table is shared).
- **Extension loading.** Every seam takes an implementation, and `Host` is
  reachable from a client's `main`, but nothing loads one from outside the
  binary. Today "an extension" means code compiled in — see
  [extending.md](extending.md).
