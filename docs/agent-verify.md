# Seeing the window: `./dev verify`

The desktop window, launched, driven and photographed with nothing on screen.
It runs the real `open_main_window` on GPUI's test platform with the headless
Metal renderer behind it. There is no `NSWindow`, no Dock icon and no
activation, so it cannot take focus from whoever is at the keyboard, and it
needs no Screen Recording permission because a frame is rendered into a texture
rather than captured off the display.

This is how an agent checks its own work on the window. "Never launch a client
unless asked" still holds for `./dev gui`; it does not apply here, because
nothing is launched that a person could see.

```sh
./dev verify -s 'shot launch' -s state -- diff . HEAD~1
./dev verify --json -f steps.txt -- commits ~/src/repo
```

The arguments after `--` are the ordinary command line (`commits .` when there
are none). Each run is one launch: the steps run in order, each followed by a
settle, and the process exits. Session and recent-project files go to a scratch
directory, so a run restores nothing a person left and records nothing they will
see. It builds into `target/verify`, never `target/debug`, so it does not swap the
binary under a running `./dev gui` or wait on its cargo lock. The first run
compiles GPUI once more, about 2 GB.

## Steps

```text
-s, --step STEP     one step; repeat it. Several steps may share one -s, `;`-separated
-f, --file FILE     steps from a file, one per line, `#` comments
-o, --out DIR       where shots go (target/verify-shots)
    --json          one JSON object on stdout; failures as {error, code, hint} on stderr

cmd NAME            a named command, through the one dispatch path a key reaches:
                    cmd workspace.history, cmd files.stage, cmd commands.palette
key KEYS            space-separated keystrokes, GPUI spelling: key down down enter,
                    key cmd-k, key shift-tab, key escape
type TEXT           text, a character at a time, into whatever holds the keyboard
click X Y           a primary click, logical pixels from the window's top-left
scroll X Y DX DY    a wheel gesture at a point, by a pixel delta
wait MS             simulated time passing, then a settle
shot NAME           the frame as NAME.png
state               the shell as JSON: destination, keyboard, focused pane, every
                    pane's label, the notice and error band, running job, open
                    overlays, the write generation, viewport size and scale
```

**Coordinates are logical pixels.** A shot is at the window's scale factor
(`scale` in `state`, 2 on a Retina display), so a point measured on a 2560×1600
PNG is clicked at half its value. The default window is 1280×800; set
`GITTEN_QA_GEOM` to change it. The other three QA doors (`GITTEN_QA_VIEW`,
`GITTEN_QA_DIALOG`, `GITTEN_QA_THEME`) work too, through the same entry points
they use in a real launch.

**The desktop answers few keys.** Arrows, Tab, Enter and Esc, plus the `cmd-`
chords on the menu. Everything else is a click or a named command, which is the
native-keys decision and not a gap in this tool. A key nothing answered reports
`"handled": false`. Use `cmd` for a verb, and `key` to test what a keyboard
user can actually do.

**A click reports nothing about itself.** GPUI's click handlers do not stop
propagation, so the dispatch result cannot say whether one ran. Follow a click
with `state` or `shot`.

**Settling waits for the work a person would wait for.** After each step the run
ticks simulated time and waits until no write is running, no refresh wave is in
flight and no main-view load is pending. It also waits until that has held for
150 ms of real time, because writes run on the job worker's own thread and the
watcher on notify's. A step that is still busy after 10 s is marked
`"unsettled": true` rather than hidden.

**Every settling step reports `busyMs`:** how long, in real milliseconds, the
window stayed busy before it went quiet. It counts what runs off GPUI's
executor, the job worker's thread and the watcher; a refresh's reads the test
dispatcher finishes inside the settle's first tick read as 0. A write whose own
refresh lands reads tens of milliseconds; a number near 2000 means something
waited on the repository watcher, whose rate floor is two seconds of *real*
time. That is
also why `wait MS` cannot stand in for it: `wait` moves the simulated clock,
and 3000 simulated milliseconds pass in a fraction of a real second.

A change the watcher has stamped but not yet turned into a wave is not waited
for. A write leaves one behind: its index rename is heard a few milliseconds
after it finishes, and a run pumps the finish faster than that, so the stamp
lands after the write's own wave began and buys a second, identical one about
two seconds later. If that wave starts during a later step, the step's settle waits
for it, so `generation` in `state` can read one higher than the writes alone
explain.

The config is the user's `gitten.toml`, so shots match what they see. Set
`GITTEN_CONFIG` to a file of your own when a run has to be independent of it. The
frame/row/heap readout is off unless `GITTEN_STATS` is set, because it would be
drawn into every shot.

## Not covered

- **Timing.** The executor is GPUI's deterministic test dispatcher and the clock
  is simulated, so frame times from a verify run mean nothing. Use
  [agent-perf.md](agent-perf.md) and the `tti` example for timing.
- **The platform's input stack.** Input enters at `Window::dispatch_keystroke` /
  `dispatch_event`, which is where the platform hands it over. Bindings,
  hitboxes and handlers are the real ones; IME composition, the system's own key
  equivalents, the traffic lights and the application menu are not.
- **Clicking by element.** GPUI keeps the debug-selector bounds `pub(crate)`
  outside its own `VisualTestContext`, so a click is a coordinate. Read it off a
  shot.
- **Other platforms.** The headless renderer is GPUI's Metal one. Elsewhere,
  `shot` says there is no renderer and every other step still runs.

## Exit status

0 when every step ran; 1 when a step could not be carried out at all (a
keystroke that does not parse, a shot with no renderer); 2 when the run could not
start (a bad step, an unwritable output directory, a launch that opened no
window). A key the window ignored is not a failure: that is the window's answer.

The implementation is `gui/src/verify.rs`, and `./check.sh` runs a two-launch
smoke of it under "window frames (headless)".
