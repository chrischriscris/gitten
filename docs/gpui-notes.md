# GPUI notes

What GPUI does that you would not guess, each one paid for once already. Read
it before changing anything in `gui/`: most of these fail silently, and the
last paragraph's five are invisible in a screenshot, so `./dev verify` will not
catch them for you. They lived in `AGENTS.md` until it grew past what every
task should have to read.

Interactivity requires identity. `.id()` before `.overflow_y_scroll()`, clicks,
hover, drag — those methods live on `StatefulInteractiveElement` and there is no
way in without an id.

`uniform_list` for anything long. It builds only visible rows and brings its own
scrolling, so don't wrap it in another scroll container. To scroll a *whole row*
sideways, use its own `with_horizontal_sizing_behavior(Unconstrained)`, make rows
`flex_none` (anything `overflow_hidden` clips instead of scrolling), **and** point
it at the widest row with `with_width_from_item(Some(i))`. That last one is the
trap: the list measures exactly ONE row to decide its scrollable width and
defaults to row 0, so a short first row means nothing scrolls no matter how long
the rest are. Compute the widest index at load.

**A gutter that stays put is not that, though — it is the view's own offset.** The
list scrolls a row, and a row is the line numbers *and* the text; nothing outside
it can hold one of them still. So a diff row is exactly as wide as the viewport,
the furniture is drawn first, and the text goes in a clipping window inside the
row with a negative margin on it — which is `Pen::scroll` written in pixels, and
the same reason: the tokens and the intraline spans address the *line*, so
slicing the string instead pairs styling with the wrong bytes. What that costs is
the wheel, the bound and the scrollbar; `gpui_component`'s `ScrollbarHandle` is a
public trait, so the bar is four methods over your own `Cell` and it drags the
thumb for you. What it buys, beyond the gutter: no `with_width_from_item`, so the
list stops shaping a 2000-character line every frame to size a scroll nothing
uses.

**A list that scrolls one axis will happily scroll it with the other axis's
delta.** `uniform_list` leaves `overflow.x` visible, and `div`'s scroll handler
reads that as permission: `Overflow::Scroll if !restrict_scroll_to_axis &&
overflow.x != Scroll => delta.x`. So a sideways flick scrolls the rows *down* —
and with the text panning from the same event, diagonally. Whoever owns the other
axis has to take the event away, and `on_scroll_wheel` cannot: it is bubble-phase
and the list is a child, so it has already run. Register on the window in the
**capture** phase, `stop_propagation` when the gesture is horizontal, and lock the
axis for the whole gesture with `gpui::OngoingScroll` — the same lock `div` uses.
Deciding per event is a coin flip fifty times a second, which is the drift you
feel rather than see.

**A row's background is the row's width, so give every row `min_w_full`.** A bg
that stops after the last character is a ragged margin down a wall of additions;
Bitbucket, GitHub and git's own pager all run the colour to the edge. `min_w_full`
is what does it, because `uniform_list` lays each visible row out as its own root
against an available width of *the viewport plus however far it is scrolled
horizontally* — so 100% means "to the right edge of the window, wherever the
window is scrolled to" and the fill follows the scroll for nothing. A *minimum*,
so a long line keeps the width there is to scroll to; and it does not disturb
`with_width_from_item`, whose one measurement runs against `MaxContent`, where a
percentage minimum has no parent width to resolve against and drops out. A row
that draws columns rather than one run of text (a side-by-side pair) gives them
`flex_1` and `min_w(0)` instead: two halves and a rule come out at exactly the
row's width however the window is dragged, and without the `min_w` a flex item is
never narrower than its content, so one long line pushes the second column off
the screen.

Closing the last window does not end the process; macOS keeps appless processes
alive. `cx.on_window_closed` + `cx.quit()`. Cmd-Q is separate and equally manual:
no application menu exists, so register the action, bind the key, and set a menu
(`on_action` + `bind_keys` + `set_menus`).

**The window has no titlebar of its own.** `WindowOptions::default()` leaves
`appears_transparent: false`, which is an opaque system titlebar — titled with the
executable's name, because `title` is `None` — stacked on whatever strip the app
draws itself. Two title bars. So: transparent, set the title anyway (Mission
Control and the Window menu read it), and `traffic_light_position` at `(10, 10)`,
because macOS uses that inset above *and* below the 12px button to size the band
and 32 is what comes out. Leave `app_owns_titlebar_drag` false and the platform
still moves the window from the empty part of the strip.

**Another library's theme is not ours.** `gpui_component::init` sets its theme to
*Light*, and the two scrollbars are the only widgets from it this app uses — so
they painted a light track and an accent thumb over a near-black diff until
something pushed our palette at them. Two writes per colour, because it reads the
track off `ThemeColor` and the thumb off `ThemeTokens`, and `Theme::sync_base` is
what actually reaches the layer that paints: setting the fields alone does
nothing. `config::sync_widgets`, on startup and on every reload.

**A column of numbers is right-aligned.** Line numbers were `w(52).child(n)`,
which puts the units digit of `9` and of `1234` four characters apart — the one
thing the eye does with that column is run down it. `justify_end` and a pad.

A bare binary is not an `.app` bundle, so the window opens behind everything,
the menu bar is titled with the executable's name and there is no icon.
`cx.activate(true)` is the dev fix for the first of those; `./dev bundle` is the
answer to all three. The mark is `gui/assets/icon.svg` — three lanes of a
commit graph weaving past each other, which is what the word means — and the
iconset is re-rendered from the vector at every size rather than downscaled from
one PNG, because at 16 and 32 pixels that is the whole legibility of it.

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

**Five traps, none of them visible in a screenshot.** A `flex_none()` after
`flex_1()` zeroes the growth — split halves are `flex_1` **and** `min_w(0)`, and
it is the pair that keeps them from shearing, not either one. A `subscribe`
callback runs **inside an update of the subscriber**, so a callback that updates
its own subscriber re-enters it and aborts the process (`entity_map.rs`, "cannot
update X while it is already being updated"); reach for `cx.defer` past the
effect cycle instead. The same shape bites through focus: a floating field that
focuses itself from its own `render` never tells the frame loop, so on the frame
it disappears nothing takes the keyboard back and the window answers no key at
all for the rest of its life — `DevShell::reclaim_focus`, called from every path
that moves the keyboard by name. A test module that does `use super::*` where
`gpui` is also in scope shadows the builtin `#[test]` with gpui's `test` macro
and recurses to the limit; import the names the test uses. And a graph curve's
two halves each anchor on their **own** dot — `core::graph::Curve` says which one
leaves and which reaches — so a fixture that mirrors one half to make the other
tears at the row boundary.
