# gitten

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

The doors are real now — a GPUI window, a terminal and a loopback agent API —
and they are the cheap check on this. Anything two of them need is a bug until
it is in `core`: the row flattening, the order table, the token-versus-span merge
and the graph's branch colours were each written twice before they were written
once.

**A client is drawing and input, and nothing else.** Everything before it is
shared: `gitten-git` acquires, `gitten-app` holds `gitten.toml` and the command line,
`core` holds the rest. A client that has to write its own argument parsing is a
client nobody else will write — which is why the config parser living behind GPUI
was a bug and not a layout choice.

**The desktop app is the product.** A terminal client comes after it; the loopback
agent API is a door and not a plan. **A feature asked for without a client
named means the GPUI window** — build it there, and stop.

That is not in tension with the paragraph above; it is the reason for it. The
other doors exist to keep this honest, because they are the cheapest way to find
out that something UI-shaped leaked into `core`, and because somebody else's
client should be possible without a patch to this repo. They are not equal claims
on the roadmap. So: put the *seam* in `core` in the same pass as the feature, and
put the *implementation* only where it was asked for. A second client's
implementation, written unasked, is scope nobody wanted; a hardcoded constant
that makes the second client impossible is a bug either way.

And the tie-break, when the two pull against each other: **a shared seam is never
worth a worse window.** If the abstraction that would serve three clients makes
the desktop slower or uglier, the desktop wins and the seam waits.

**A key is data and a command is a name.** `core::command` resolves a keypress to
a command *name*; a client turns that name into a method call. Nothing in the
chain is a function pointer, which is what lets a config file hold it, a help
screen list it and an extension add one. A `match` on keypresses inside a client
is a keymap that client owns alone.

## macOS first, Linux never ruled out

**The Mac is the machine this is written on; Linux is the check that nothing was
written badly.** Develop there, measure there, and fix a Mac-only bug first — but
nothing may be *written* in a way that makes Linux impossible, and today nothing
is. There is not one `cfg(target_os)` in the tree, no `objc`, `cocoa` or
`core-foundation` in any `Cargo.toml`, and the terminal's clipboard is OSC 52
rather than `pbcopy` — reached for because a terminal is often not on the machine
the clipboard is on, and portable for free. GPUI, `gix` and the `git` binary all
run on both. That is not luck yet, but it is cheap to keep: when the portable
mechanism costs nothing, take it.

When it costs something, the same tie-break as everywhere else — **a portable
mechanism is never worth a worse window.** The Mac wins and the other platform
waits. What it does *not* get to do is win by accident, in a constant nobody
noticed, the way a hardcoded lane count would.

The exceptions are honest and few, and they are exceptions rather than a
pattern. `./dev bundle` is `sips` and `iconutil` and will never be anything else;
an `.app` is a macOS artifact and the Linux equivalent is a different script
nobody has asked for. The traffic lights, the application menu and the
appless-process quit in [docs/gpui-notes.md](docs/gpui-notes.md) are macOS
platform facts, not layout choices — a Linux window has its own and GPUI hands
them over. Neither is a licence for a third.

Windows is not a target and not a promise.

## Git

`gitten-git` is the acquisition layer — the only crate that talks to a repository.
`core` stays pure and does no I/O; `shell` does no I/O either. Both views take
already-loaded data, which is also why they are trivial to test and to drop into
a pane.

**Reads spawn the `git` binary today** — status, log, diff, graph traversal all
go through `Command::new("git")` (`git/src/lib.rs`). `gix` is the intended
destination, not the current state: the goal is no process spawn on the hot path,
and moving reads onto `gix` is roadmap work (`docs/roadmap.md`). Until then, do
not write a doc, a comment or a plan that assumes a `gix` handle exists — there is
no `gix` dependency in the tree. The blob-OID diff cache was the cheaper half of
the same goal and has landed — `core::differ`, keyed on the pair's OIDs plus the
settings that reach the answer.

**Writes go through the `git` binary** — stage, commit, branch, stash, reset,
push and pull are verbs on the same trait as the reads. Shelling out means
hooks, credential helpers, SSH agents and `.gitconfig` behave exactly as they
do in the user's terminal — don't reimplement any of that; you will get it
subtly wrong.

One object-safe trait behind every path — `Repo`, held behind a `Handle` — so
a frontend never learns which path ran, and a fake stands in for the binary in
the tests.

**Untracked files are `git status`, never `git diff`.** `git diff` compares the
index and the working tree against a commit and an untracked file is in none of
the three, so it has nothing to diff and never will. Ask `status` separately for
an empty revspec and synthesise a pair with an empty old side. Getting this wrong
means "show me my uncommitted work" silently omits every file you just created.

**It acquires content, not diffs.** Two lists of lines per changed file, and
`core` decides which lines correspond. Let git produce the diff and git owns the
algorithm, which makes `trait Differ` decoration and rule 1 false for the most
important thing this app does. Two processes for a whole diff however many files:
one `git diff --raw`, one `git cat-file --batch`.

## Read before you touch it

The rules below this file's philosophy live beside the code they govern, and
each has already cost a day once. Read the row before the change, not after.

| touching | read first |
|---|---|
| anything in `gui/` | [docs/gpui-notes.md](docs/gpui-notes.md), then check it with `./dev verify` ([docs/agent-verify.md](docs/agent-verify.md)) |
| a differ, `align`, wrapping, a layout, a diff view | [docs/diff-rules.md](docs/diff-rules.md) |
| the commit graph | [docs/commit-graph.md](docs/commit-graph.md) — the gutter is capped at 12 lanes, hard |
| a command or a key | [docs/features.md](docs/features.md): every command, its keys, and the file in each client that answers it |
| colour, a surface, contrast | [docs/theming.md](docs/theming.md) |
| a new seam | [docs/extending.md](docs/extending.md) |

`./dev lint` holds the rules here that a grep can check, and runs before every
commit.

## Building

**`./dev` is the way in.** One script, every client, development defaults on.

```sh
./dev                               what it does, and the rest of the flags
./dev tui     diff . HEAD~2..HEAD   the terminal
./dev gui     commits               the window: rebuild + relaunch on save,
                                    landing back on the same row
./dev web     diff --fixtures       the agent API; prints a URL, opens nothing
./dev dump    commits ~/src 600     one frame on stdout, timing on stderr.
                                    COLS, ROWS, LAYOUT, WRAP, THEME, AT, FRAMES
./dev check                         everything headless
./dev verify -s 'shot x' -- diff .  the window, headless: driven, photographed,
                                    and no focus taken — docs/agent-verify.md
./dev lint                          the rules here that a grep can hold
./dev features                      regenerate docs/features.md, the command map
./dev config > gitten.toml           a complete, correct starting file
./dev bundle                        target/gitten.app — icon, name, a real app

./dev --release tui diff .          when you need honest numbers
GITTEN_STATS=0 ./dev tui             without the readout
```

Debug and the stats readout are the defaults, because that is the loop you
iterate in. **The frame timings are meaningless in a debug build** — a different,
much slower binary, and both clients say so. What is still worth watching is the
row counts, the cell counts and the load breakdown.

`gui` and `web` relaunch on every save; `tui` cannot, because it owns the
terminal's stdin and there is nothing to put in front of it. Quit with `q` and
press up-enter. `./dev dump` is the watchable one.

Under it, and worth knowing when something is wrong:

```sh
cargo test -p gitten-core            # just correctness, sub-second
cargo test -p gitten-app             # the config file and the command line
cargo run -q -p gitten-git --example diffcheck --release [REPO] [REVSPEC]
                                    # the differs against git's own answer;
                                    # WORST=1 for the files it did worst on
```

The overlay forces a redraw every frame so the fps number means something —
GPUI is reactive and draws nothing at rest, so an honest idle reading would be
zero. It measures how fast we *can* redraw, not what the app costs sitting still.
Never read those numbers off a debug build.

Colour and font live in `gitten.toml` and reload on the next frame — no rebuild.
`[diff] algorithm`, `context`, `layout` and `wrap` live there too. `context`
applies on the next launch; the others have controls in the title bar and change
live, and the file sets what they *open* on. A control there is the temporary
answer until a settings panel exists — the picker is a pure function of a list
and an index, so any seam with a registry gets one for free, and the terminal's
`?` panel is the same trick over the keymap.
`./dev config > gitten.toml` writes a complete one, and `[keys]` is in it — every
client reads the same file. Code still costs a rebuild; `./dev` is what removes
the quitting and retyping around it.

**Never launch a client unless asked.** Build it, test it, bench it — but a
window appearing unannounced, or a terminal taken over, interrupts whoever is at
the keyboard. Say it's ready and
hand over the command. The terminal frontend's `dump` example is the exception
and exists for it — a frame on stdout interrupts nobody, so look at a colour or
a glyph there rather than asking someone to open something.

**`./dev verify` is the window's `dump`, and it is how a change to the window
is checked before anyone else looks at it.** The real launch, on GPUI's test
platform with the offscreen renderer: steps in, PNGs and state out, and no
window anywhere for focus to go to. A desktop change is not done when it
compiles; it is done when a shot of it says so. What the run cannot tell you —
timing, IME, the platform's own chrome — is in `docs/agent-verify.md`; say so
rather than claiming it.

**A window's state is reproducible from the environment, because a hand is not.**
`GITTEN_QA_GEOM=1280x800`, `GITTEN_QA_VIEW=history`, `GITTEN_QA_DIALOG=commands`
and `GITTEN_QA_THEME=github-dark` open the window in the state a capture needs,
and those four are chosen because they are what no argument can say: the
repository, the revspec and the patch are already the command line, and `diff -`
takes one on stdin. Each goes through the same entry point a keystroke reaches,
nothing is persisted, and a value nothing recognises is said on stderr and then
ignored — a typo that silently photographed the default state is the one failure
a capture cannot see for itself (`gui/src/qa.rs`). The doors make a launch
*reproducible*; they do not make it polite, so the rule above still stands.

`rust-toolchain.toml` tracks the channel Zed pins. When GPUI fails with an
unstable-feature error, that pin has drifted — go read Zed's `rust-toolchain.toml`.

**GPUI is pinned only by `Cargo.lock`, so never run a bare `cargo update`.** The
four GPUI git dependencies in `gui/Cargo.toml` (`gpui`, `gpui_platform`,
`gpui-component`, `gpui-component-assets`) are declared without a `rev`; the exact
commits live in `Cargo.lock` and nothing else holds them. A bare `cargo update`
floats them to Zed's default-branch tip — a wall of compile errors and probably a
`rust-toolchain.toml` bump, at a moment nobody chose. A *scoped* update
(`cargo update -p <crate>` for some unrelated dependency) does **not** touch the
git deps and is safe. Bumping GPUI is deliberate: `cargo update -p gpui -p
gpui-component`, then match `rust-toolchain.toml` to whatever channel the new Zed
commit pins — one reviewed commit. (Pinning the deps with `rev = "…"` in the
manifest was tried and rejected: `gpui-component` depends on `gpui` *bare*, so a
`rev` on our side splits the whole Zed tree into two source variants and compiles
GPUI twice. The lockfile is the pin; this note is the guard.)

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

## Don't

Don't add dependencies to `core/`.

Don't put logic in `gui/` that `cli/` would have to duplicate.

Don't build what the framework already has. `uniform_list` was there the whole
time and a hand-rolled list cost a day.

Don't let an AI feature bypass the extension API. The built-ins are extensions;
that's the proof the API is worth using.
