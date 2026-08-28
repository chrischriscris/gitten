//! `gitten-tui` — gitten in the terminal you started it from.
//!
//! The assembly, and deliberately thin: arguments, `gitten.toml` and acquisition
//! are `gitten_app`; the views are `gitten_tui`; which command a key runs is
//! `gitten_core::command`. What is left here is a loop.
//!
//! # Nothing in this file decides what a key does
//!
//! It reads one, asks the keymap what it means, and calls a method named by the
//! answer. The keymap is on `Host`, so `gitten.toml` and an extension reach it
//! the same way — and the same file drives the GPUI client. A `match` on
//! keypresses here would be a keymap this client owned alone, which is the thing
//! `docs/architecture.md` spent two versions asking for and not getting.
//!
//! ```text
//!   crossterm event → term::translate → Key → Keymap::resolve → "diff.next-file"
//!                                                                     │
//!                                                     App::dispatch ──┘
//! ```
//!
//! # One line of text
//!
//! The one modal input this client has is the commit-list search: `/` opens a
//! prompt on the status row, each edit filters the list live, and Enter keeps
//! what was typed while Esc restores the whole list. While it stands, keys are
//! resolved against exactly the `input` mode — never the full stack — so the
//! shipped globals cannot read the query, and everything else the prompt takes
//! arrives as text.
//!
//! # The loop is idle until something happens
//!
//! It blocks on input with a timeout, and the timeout exists only so a saved
//! `gitten.toml` is noticed. Nothing redraws at rest — the same property the GPUI
//! client has for free, arrived at here on purpose, and the reason the frame
//! timing in `docs/measurements.md` is measured rather than observed.

use gitten_app::acquire::{self, Data};
use gitten_app::cli::{self, Source, View};
use gitten_app::jobs::{Event as JobEvent, Generation, Job, Runner, Submitter};
use gitten_app::verbs::Write;
use gitten_app::{StartClock, Startup};
use gitten_core::command::{chord_string, Code, Key, Modes, Resolve};
use gitten_core::differ::Overrides;
use gitten_core::host::Host;
use gitten_core::runs::Run;
use gitten_core::Hunk;
use gitten_tui::commits::{Commits, Glyphs};
use gitten_tui::diff::Diff;
use gitten_tui::help;
use gitten_tui::screen::{Ink, Pen, Screen};
use gitten_tui::scrollbar::Bar;
use gitten_tui::stashes::Stashes;
use gitten_tui::term::{Input, Mouse, MouseKind, Term};
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

mod panes;

const EXTRA: &str = "  --ascii        draw the graph and the scrollbar without box-drawing
  --no-mouse     leave the mouse to the terminal: no wheel, no click, and the
                 emulator's own drag-to-select instead of gitten's

  `?` lists every key, from the same keymap `gitten.toml` writes. Colours and the
  keymap are re-read every time the file is saved.
";

/// How often the loop wakes to notice a saved config file.
///
/// A save is a human action and 150 ms of latency is imperceptible; polling a
/// flag rather than plumbing a channel is what the GPUI client does too, for the
/// same reason. It costs one `poll` syscall per interval and no redraw.
const TICK: Duration = Duration::from_millis(150);

/// Longest gap between two presses that still counts as a double click.
///
/// A terminal reports a press and nothing else — there is no `click_count` in
/// the protocol the way there is in every window system — so the count is ours
/// to keep. 400 ms is what macOS, GTK and Windows all default to within 100 ms of,
/// and the same cell has to be hit twice: a double click that moved is two
/// clicks, which is what makes a fast drag-then-click not select a word.
const DOUBLE: Duration = Duration::from_millis(400);

fn main() {
    // `Startup::go` reports the stages before this point — arguments, host,
    // config, acquisition. The clock below is armed where that hands over and
    // marks the ones only a terminal client has, so every number is the stage
    // itself and not the road so far.
    let mut start = Startup::new("gitten-tui", View::Commits)
        .blurb("gitten in the terminal you started it from")
        .extra(EXTRA);
    let glyphs = match cli::take_switch(start.take(), "--ascii") {
        true => Glyphs::ascii(),
        false => Glyphs::default(),
    };
    let mouse = !cli::take_switch(start.take(), "--no-mouse");

    // The watcher is armed while acquisition runs, not after it. It needs only
    // the config file's *name* — the same `config::path()` the shared startup
    // will read, and the environment cannot change between the two — and its
    // setup costs a couple of milliseconds of thread and kernel registration
    // that would otherwise sit on the road to the first frame behind a git
    // subprocess that takes far longer than that. Nothing here prints, so the
    // early start is invisible on every path that ends in `Exit`.
    let dirty = Arc::new(AtomicBool::new(false));
    let watcher = {
        let (tx, rx) = std::sync::mpsc::channel();
        let path = gitten_app::config::path();
        let dirty = dirty.clone();
        std::thread::spawn(move || {
            let _ = tx.send(
                gitten_app::config::watch(&path, move || dirty.store(true, Ordering::Relaxed)).ok(),
            );
        });
        rx
    };

    let started = match start.go() {
        Ok(started) => started,
        Err(exit) => exit.finish(),
    };
    let mut clock = StartClock::new();
    let config_path = started.config.clone();
    let mut app = App::new(started, glyphs);
    clock.stage("views built");

    // The panic hook before the terminal is touched: a panic between the two
    // would leave raw mode on with nothing to restore it.
    Term::guard();
    let mut term = match Term::enter(mouse) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("gitten-tui: could not take the terminal: {e}");
            std::process::exit(1);
        }
    };
    clock.stage("terminal taken");

    // Held for as long as the loop runs: dropping a watcher stops it watching,
    // silently, which is a good way to lose an afternoon. The recv collects
    // what the thread above armed — it has had all of acquisition to finish,
    // so this is the tail wait and not the setup; if that ever exceeded the
    // time git took, this number would be the part still on the critical path.
    let _watcher = watcher.recv().ok().flatten();
    clock.stage("watcher joined");

    if let Err(e) = app.run(&mut term, &dirty, &config_path, &mut clock) {
        term.leave();
        eprintln!("gitten-tui: {e}");
        std::process::exit(1);
    }
    // Explicitly, before anything is printed: `Drop` would do it, but not until
    // after a `println!` had gone to the alternate screen.
    term.leave();
}

/// One pane's tenant: a view and what it was acquired from.
///
/// Each tenant carries three things beside its view: the source it was
/// acquired from, the label it was acquired under, and the invalidation
/// generation it was acquired at. The first two are what a refresh re-reads
/// and renames; the third is what tells the two apart — a tenant whose
/// generation is behind the job queue's is stale, and a fixture's never is,
/// because no write anywhere can stale it.
///
/// The diff tenant carries an [`Option`] where the commit list carries a
/// source, and that is load-bearing: on a commits launch the diff pane is
/// registered **empty** — nothing acquired, nothing to refresh, nothing a
/// hunk verb can act on — and Enter replaces it with a real acquisition.
/// Pretending it was acquired from the working tree would let staging and
/// refreshing reach a pane that holds nothing; "not loaded yet" is a state
/// the type can say.
enum Screens {
    Commits {
        view: Commits,
        source: Source,
        label: String,
        generation: Generation,
    },
    Diff {
        view: Diff,
        source: Option<Source>,
        label: String,
        generation: Generation,
    },
    /// The stash stack. No source: the pane is only ever registered behind a
    /// repository — a fixture and a patch are not shaped like a stack — so a
    /// refresh is a plain re-read through the handle the app holds, and the
    /// generation rail is the whole of its staleness story.
    Stashes {
        view: Stashes,
        label: String,
        generation: Generation,
    },
}

/// What an empty diff pane's header says instead of a sha it does not have.
const EMPTY_DIFF_LABEL: &str = "press enter on a commit";

/// What a stash pane's header says when its side read failed — never
/// `nothing stashed`, which would assert a successful read that did not
/// happen. The exact error goes to the status line; the next refresh
/// re-reads the stack, and a read that succeeds replaces this pane outright.
const STASH_UNAVAILABLE: &str = "unavailable";

/// The mode a text field owns the keyboard in — the name the keymap and
/// `gitten.toml` use, and the same name the window's input module holds. While
/// the search prompt stands, bindings are resolved against exactly this mode
/// and nothing else.
const INPUT: &str = "input";

impl Screens {
    /// Which mode's bindings are live. The name the keymap and `gitten.toml` use.
    fn mode(&self) -> &'static str {
        match self {
            Screens::Commits { .. } => "commits",
            Screens::Diff { .. } => "diff",
            Screens::Stashes { .. } => "stashes",
        }
    }

    fn label(&self) -> &str {
        match self {
            Screens::Commits { label, .. }
            | Screens::Diff { label, .. }
            | Screens::Stashes { label, .. } => label,
        }
    }

    fn generation(&self) -> Generation {
        match self {
            Screens::Commits { generation, .. }
            | Screens::Diff { generation, .. }
            | Screens::Stashes { generation, .. } => *generation,
        }
    }

    /// Re-acquires this tenant from the repository when a finished job has
    /// staled it, applying the result in place. `None` for a tenant nothing
    /// can stale — one already at `target`, one with no repository behind it,
    /// or the empty diff, whose data no write anywhere can move because it
    /// has none. `Some(result)` otherwise, because a failed re-acquisition is
    /// a failed refresh and the caller has an error to keep.
    ///
    /// Synchronous on the terminal loop, deliberately: the window refreshes
    /// panes off-thread because it can, and a second terminal background
    /// protocol is not M-sized work. Measured window costs for the same
    /// operation run 48–370 ms — one git read plus one prepare pass — so a
    /// refresh here pauses input for that long and leaves the last frame
    /// drawn while it does.
    fn refresh(
        &mut self,
        target: Generation,
        host: &Host,
        repo: &dyn gitten_git::Repo,
    ) -> Option<Result<(), String>> {
        if self.generation() >= target {
            return None;
        }
        // The generation travels with the refresh: a pane that re-acquired
        // at `target` is exactly as current as `target` says, however many
        // finishes followed it down the queue.
        match self {
            Screens::Commits {
                view,
                source,
                label,
                generation,
            } => match source {
                Source::Repo { .. } => {
                    let loaded = match acquire::reacquire(
                        View::Commits,
                        source,
                        host,
                        Some(repo),
                        &Overrides::default(),
                    ) {
                        Ok(loaded) => loaded,
                        Err(e) => return Some(Err(e)),
                    };
                    let Data::Commits(commits) = loaded.data else {
                        return Some(Err("re-acquisition returned the wrong view".into()));
                    };
                    view.replace(commits);
                    *label = loaded.label;
                    *generation = target;
                    Some(Ok(()))
                }
                Source::Fixtures | Source::Patch { .. } => None,
            },
            Screens::Diff {
                view,
                source,
                label,
                generation,
            } => match source {
                Some(source @ Source::Repo { .. }) => {
                    let loaded = match acquire::reacquire(
                        View::Diff,
                        source,
                        host,
                        Some(repo),
                        &Overrides::default(),
                    ) {
                        Ok(loaded) => loaded,
                        Err(e) => return Some(Err(e)),
                    };
                    let Data::Diff(files) = loaded.data else {
                        return Some(Err("re-acquisition returned the wrong view".into()));
                    };
                    view.replace(files, host);
                    *label = loaded.label;
                    *generation = target;
                    Some(Ok(()))
                }
                // A fixture, a patch — or the empty pane, which was never
                // acquired from anywhere and has nothing to re-read.
                Some(Source::Fixtures) | Some(Source::Patch { .. }) | None => None,
            },
            // The stack has no source to consult: it is only ever
            // registered behind a repository, so every refresh is a plain
            // re-read through the handle the caller holds. A failed read
            // keeps the last good rows, their label and their generation —
            // the tenant stays exactly where it was, the caller hears the
            // error, and the next finish tries this pane again.
            Screens::Stashes {
                view,
                label,
                generation,
            } => {
                let loaded = match acquire::stashes(repo) {
                    Ok(loaded) => loaded,
                    Err(e) => return Some(Err(e)),
                };
                let parked = loaded.stashes.len();
                view.replace(loaded.stashes);
                *label = stash_label(&loaded.label, parked);
                *generation = target;
                Some(Ok(()))
            }
        }
    }

    /// A new size — this pane's own content rectangle, never the whole screen,
    /// and on the same call the margin the config file asks for.
    ///
    /// Both per call, because both are a comparison when nothing changed and
    /// because this is the one path that has the size *and* the live host. It
    /// is what makes `[view] scrolloff` land on the next frame rather than the
    /// next launch, like every other number in that file — and what makes a
    /// Markdown reflow budget against the pane, because [`Diff::resize`] is
    /// handed the pane's width and passes it down to every presentation.
    fn resize_to(&mut self, rect: crate::panes::Rect, host: &Host) {
        match self {
            Screens::Commits { view: c, .. } => {
                c.set_scrolloff(host.view.scrolloff);
                c.resize(rect.width, rect.height);
            }
            Screens::Diff { view: d, .. } => {
                d.set_scrolloff(host.view.scrolloff);
                d.resize(rect.width, rect.height, host);
            }
            Screens::Stashes { view: s, .. } => {
                s.set_scrolloff(host.view.scrolloff);
                s.resize(rect.width, rect.height);
            }
        }
    }

    /// Paints into this pane's rectangle: `x` is the pane's first column, `y`
    /// its first content row.
    fn paint(
        &self,
        screen: &mut Screen,
        x: usize,
        y: usize,
        focused: bool,
        host: &Host,
        out: &mut Vec<Run>,
    ) {
        match self {
            Screens::Commits { view: c, .. } => c.paint(screen, x, y, focused, host),
            Screens::Diff { view: d, .. } => d.paint(screen, x, y, focused, host, out),
            Screens::Stashes { view: s, .. } => s.paint(screen, x, y, focused, host),
        }
    }

    fn status(&self, host: &Host) -> String {
        match self {
            Screens::Commits { view: c, .. } => c.status(),
            Screens::Diff { view: d, .. } => d.status(host),
            Screens::Stashes { view: s, .. } => s.status(),
        }
    }

    /// A press in this pane's content, at `row` rows down it and `col`
    /// columns across it — both pane-local, already hit-tested.
    ///
    /// The count and the modifier arrive as scalars rather than as an event
    /// type, which is what keeps the views free of `term` — a view takes
    /// already-hit-tested numbers exactly as it takes already-loaded data.
    fn press(&mut self, col: usize, row: usize, clicks: u8, extend: bool, host: &Host) {
        match self {
            Screens::Commits { view: c, .. } => c.press(col, row, extend, host),
            Screens::Diff { view: d, .. } => d.press(col, row, clicks, extend, host),
            Screens::Stashes { view: s, .. } => s.press(col, row, extend, host),
        }
    }

    /// The pointer moved with the button down, in this pane's coordinates.
    /// `row` is signed: a row above the pane is negative and scrolls it.
    fn drag(&mut self, col: usize, row: isize, host: &Host) {
        match self {
            Screens::Commits { view: c, .. } => c.drag(row, host),
            Screens::Diff { view: d, .. } => d.drag(col, row, host),
            Screens::Stashes { view: s, .. } => s.drag(row, host),
        }
    }

    fn release(&mut self) {
        match self {
            Screens::Commits { view: c, .. } => c.release(),
            Screens::Diff { view: d, .. } => d.release(),
            Screens::Stashes { view: s, .. } => s.release(),
        }
    }

    /// What `copy.selection` copies here: the selection, or the row the cursor is
    /// on when there is none.
    fn copy_text(&self) -> String {
        match self {
            Screens::Commits { view: c, .. } => c.copy_text(),
            Screens::Diff { view: d, .. } => d.copy_text(),
            Screens::Stashes { view: s, .. } => s.copy_text(),
        }
    }

    /// What the *mouse* has selected, and nothing else. Empty after a click, so
    /// copy-on-select can tell a gesture that selected something from one that
    /// only moved the cursor.
    fn selection(&self) -> String {
        match self {
            Screens::Commits { view: c, .. } => c.selection(),
            Screens::Diff { view: d, .. } => d.selection(),
            Screens::Stashes { view: s, .. } => s.selection(),
        }
    }

    fn select_all(&mut self) {
        match self {
            Screens::Commits { view: c, .. } => c.select_all(),
            Screens::Diff { view: d, .. } => d.select_all(),
            Screens::Stashes { view: s, .. } => s.select_all(),
        }
    }

    fn select_none(&mut self) -> bool {
        match self {
            Screens::Commits { view: c, .. } => c.select_none(),
            Screens::Diff { view: d, .. } => d.select_none(),
            Screens::Stashes { view: s, .. } => s.select_none(),
        }
    }

    /// The live filter count, while a search prompt stands over a list. A diff
    /// has nothing to count, and a note is only drawn when there is one.
    fn filter_note(&self) -> Option<String> {
        match self {
            Screens::Commits { view: c, .. } => c.filter_note(),
            Screens::Diff { .. } | Screens::Stashes { .. } => None,
        }
    }

    /// Runs a command, or says it does not know it.
    ///
    /// The `view.*` half is the same list for both panes and is what makes
    /// them bindable in [`gitten_core::command::GLOBAL`]: a key that scrolls one
    /// list scrolls every list, and nothing had to say so twice. The pane
    /// moves are *not* here — they are the app's, answered from the registry
    /// before this is ever asked, because a pane command aimed at a view
    /// would be a pane command that stops working the day a second list
    /// registers.
    fn run(&mut self, command: &str, host: &Host) -> bool {
        match self {
            Screens::Commits { view: c, .. } => match command {
                "view.down" => c.down(),
                "view.up" => c.up(),
                "view.page-down" => c.page(1),
                "view.page-up" => c.page(-1),
                "view.scroll-down" => c.scroll_y(host.view.rows as isize),
                "view.scroll-up" => c.scroll_y(-(host.view.rows as isize)),
                "view.top" => c.to_top(),
                "view.bottom" => c.to_bottom(),
                // A commit list has nothing off the left edge to reach.
                "view.left" | "view.right" => {}
                _ => return false,
            },
            Screens::Diff { view: d, .. } => match command {
                "view.down" => d.down(),
                "view.up" => d.up(),
                "view.page-down" => d.page(1),
                "view.page-up" => d.page(-1),
                "view.scroll-down" => d.scroll_y(host.view.rows as isize),
                "view.scroll-up" => d.scroll_y(-(host.view.rows as isize)),
                "view.top" => d.to_top(),
                "view.bottom" => d.to_bottom(),
                "view.left" => d.scroll_x(-8),
                "view.right" => d.scroll_x(8),
                "diff.next-file" => d.jump_file(1),
                "diff.prev-file" => d.jump_file(-1),
                "diff.cycle-layout" => d.cycle_layout(host),
                "diff.cycle-wrap" => d.cycle_wrap(host),
                _ => return false,
            },
            Screens::Stashes { view: s, .. } => match command {
                "view.down" => s.down(),
                "view.up" => s.up(),
                "view.page-down" => s.page(1),
                "view.page-up" => s.page(-1),
                "view.scroll-down" => s.scroll_y(host.view.rows as isize),
                "view.scroll-up" => s.scroll_y(-(host.view.rows as isize)),
                "view.top" => s.to_top(),
                "view.bottom" => s.to_bottom(),
                // A stack has nothing off the left edge to reach.
                "view.left" | "view.right" => {}
                _ => return false,
            },
        }
        true
    }
}

/// One edit to the open query, already reduced to text by whoever routed the
/// event: a character typed, a character removed, or a paste's worth. The
/// prompt never sees an event, so a pasted `q` is the character `q` and the
/// keymap never learns a paste happened.
enum Edit {
    Char(char),
    /// Backspace or Delete — both remove backwards, because a status-line
    /// prompt has no cursor position to delete from.
    Backspace,
    /// A bracketed paste, sanitized on its way in.
    Paste(String),
}

struct App {
    host: Host,
    /// Where to acquire more from, for opening a commit's diff: the path the
    /// view is named after, and the one handle the startup opened, so every
    /// diff this process shows came through the same repository. `None` for a
    /// fixture, which has no repository behind it — and the key then does
    /// nothing, which is what an unbound key does too.
    repo: Option<(std::path::PathBuf, gitten_git::Handle)>,
    /// The panes, by stable name. `commits` lives in the sidebar, `diff` in
    /// the main slot; both persist for as long as the process does, and a
    /// focus change moves the keyboard between them without destroying
    /// either. Which pane the keyboard is in decides the modes, the command
    /// routing, the title and the status line — not what was opened last.
    panes: panes::Panes<Screens>,
    /// The layout policy that turns the body into pane rectangles. A box so a
    /// compiled-in client extension can replace the built-in geometry at
    /// construction without touching the registry or the dispatch.
    layout: Box<dyn panes::Layout>,
    /// The pane rectangles, computed by [`App::layout`] and cached on the four
    /// things that can move them: the screen size, the body height, the
    /// registry's generation, and which pane is focused (the narrow layout
    /// shows only the focused one). `draw` reads this and allocates nothing.
    geometry: Option<((usize, usize, usize, usize), panes::Geometry)>,
    /// The sidebar list that held the keyboard last, so `back` from the main
    /// region returns *there* and not to whatever happens to be first.
    last_list: Option<String>,
    screen: Screen,
    modes: Modes,
    /// Keys typed so far that have not resolved to a command. Empty almost
    /// always; a chord is what puts something in it.
    pending: Vec<Key>,
    /// Something to say once, on the status line: an error, or what a key just
    /// did. Cleared by the next keypress, so it cannot go stale.
    message: String,
    /// The open search prompt, holding the query typed so far. `None` while the
    /// keyboard belongs to the panes.
    ///
    /// Here and not on a pane because collecting terminal text is input — the
    /// client's to gather, by the same rule that makes it the client's to
    /// translate a platform event — while [`Commits::apply_query`] stays the
    /// whole of what a view knows about it. It stands only over the pane
    /// named `commits`, by that name and not by focus, so every reader below
    /// can rely on it and a focus change cannot strand the query.
    search: Option<String>,
    /// The shared write queue. One FIFO worker, owned here, whose finishes
    /// every client treats the same way: a generation advances — a refusal as
    /// much as a success — and every repository-backed pane re-acquires.
    jobs: Runner,
    /// The cloneable end of [`App::jobs`], handed out to whatever submits.
    submitter: Submitter,
    /// The generation the queue has advanced to, and so the one every pane
    /// was last refreshed against.
    generation: Generation,
    help: bool,
    quit: bool,
    /// The theme `theme.cycle` picked, if anything has. `None` means the file's.
    picked_theme: Option<String>,
    /// What the last frame cost, when `GITTEN_STATS` is set.
    ///
    /// Two numbers and no overlay: how long the draw took, and how many cells
    /// reached the terminal. The second is the one worth watching — a scroll is
    /// a screenful and a cursor move should be a handful, and a number that is
    /// always the whole grid means something is repainting ink it did not need
    /// to. `GITTEN_STATS=1` and the same "0 is off" rule as the window.
    stats: Option<(Duration, usize)>,
    /// The run-list buffer, owned across frames so drawing allocates nothing.
    runs: Vec<Run>,
    /// The glyphs the scrollbar is drawn with, so a diff opened from the commit
    /// list is drawn with the same ones. `--ascii`.
    bar: Bar,
    /// Text a command asked to be put on the clipboard, handed to the terminal
    /// at the top of the next loop.
    ///
    /// Deferred rather than copied where it is produced, because writing it is a
    /// [`Term`] call and dispatch has views and a host and deliberately no
    /// terminal — the same reason acquisition is in `main` and not in a view.
    copy: Option<String>,
    /// The pane a mouse gesture began in, from Down to Up: drags, releases and
    /// copy-on-select all belong to the pane where the button went down, even
    /// after the pointer crosses the divider — one gesture, one pane's
    /// selection state, never a splice of two.
    gesture: Option<String>,
    /// The last press, for counting a double click: when, in which cell, and
    /// in which pane. The pane is part of the identity, so the same cell
    /// cannot become another pane's double click after a focus switch moves
    /// the panes under the pointer.
    clicked: Option<(Instant, usize, usize, String)>,
    clicks: u8,
    /// Each pane's advertised focus key, resolved once per host or registry
    /// change — the first key bound to `<name>.focus`, or empty when the name
    /// is unbound. Drawing reads this cache, so a frame allocates nothing for
    /// a header.
    focus_keys: Vec<(String, String)>,
}

impl App {
    fn new(started: gitten_app::Started, glyphs: Glyphs) -> Self {
        let repo = match &started.source {
            Source::Repo { path, .. } => started.repo.clone().map(|h| (path.clone(), h)),
            Source::Fixtures | Source::Patch { .. } => None,
        };
        let source = started.source;
        let label = started.loaded.label.clone();
        let host = started.host;
        let bar = match glyphs == Glyphs::ascii() {
            true => Bar::ascii(),
            false => Bar::default(),
        };
        // The ancillary stack, read beside the view that was asked for: one
        // more `git stash list` after startup already holds the requested
        // view, through the same handle. A read that fails says so and the
        // launch goes on — the pane opens as unavailable, the exact error is
        // kept for the status line, and the next refresh re-reads the stack.
        let mut stash_error = None;
        let stash_tenant =
            repo.as_ref()
                .map(|(_, handle)| match acquire::stashes(handle.as_ref()) {
                    Ok(loaded) => {
                        let parked = loaded.stashes.len();
                        let mut view = Stashes::new(loaded.stashes);
                        view.set_bar(bar);
                        (view, stash_label(&loaded.label, parked))
                    }
                    Err(e) => {
                        let mut view = Stashes::unavailable();
                        view.set_bar(bar);
                        stash_error = Some(e);
                        (view, STASH_UNAVAILABLE.to_string())
                    }
                });
        let stash_tenant = stash_tenant.map(|(view, label)| Screens::Stashes {
            view,
            label,
            generation: Generation::default(),
        });
        let mut panes = panes::Panes::new();
        let mut last_list = None;
        match started.loaded.data {
            Data::Commits(commits) => {
                let mut list = Commits::with_glyphs(commits, glyphs);
                list.set_bar(bar);
                panes.register(
                    "commits",
                    panes::Placement::sidebar("commits"),
                    Screens::Commits {
                        view: list,
                        source,
                        label,
                        generation: Generation::default(),
                    },
                );
                last_list = Some("commits".to_string());
                // The stack, between the list and the main pane in
                // registration order — the order `names` reports and the
                // order the refresh rail walks. Only a repository launch
                // has one; a fixture and a patch have no stack to read.
                if let Some(pane) = stash_tenant {
                    panes.register("stashes", panes::Placement::sidebar("stashes"), pane);
                }
                // The persistent main pane, registered **empty**: no source, no
                // acquisition, nothing for a hunk verb or a refresh to act on
                // until Enter replaces it. Its header is drawn like any other
                // pane's, saying what it is rather than pretending to hold a
                // diff it does not have.
                panes.register(
                    "diff",
                    panes::Placement::Main,
                    Screens::Diff {
                        view: Diff::new(Vec::new(), &host),
                        source: None,
                        label: EMPTY_DIFF_LABEL.to_string(),
                        generation: Generation::default(),
                    },
                );
                // `register` focuses what it registers, so the empty diff has
                // the keyboard for the length of that call. A commits launch
                // opens on the list.
                panes.focus_named("commits");
            }
            Data::Diff(files) => {
                // The stack beside the diff a repository launch asked for,
                // ahead of it in registration order; a fixture or a patch
                // has no repository and so no tenant.
                if let Some(pane) = stash_tenant {
                    panes.register("stashes", panes::Placement::sidebar("stashes"), pane);
                }
                let mut diff = Diff::new(files, &host);
                diff.set_bar(bar);
                panes.register(
                    "diff",
                    panes::Placement::Main,
                    Screens::Diff {
                        view: diff,
                        source: Some(source),
                        label,
                        generation: Generation::default(),
                    },
                );
                // `register` focuses what it registers, and the diff was
                // registered last — but the restoration is written out, the
                // same as the commits branch: a launch keeps the view it
                // asked for, and an ancillary pane never steals startup
                // focus.
                panes.focus_named("diff");
            }
        }
        let jobs = Runner::new();
        let submitter = jobs.submitter();
        let mut app = Self {
            host,
            repo,
            panes,
            layout: Box::new(panes::BuiltinLayout),
            geometry: None,
            last_list,
            screen: Screen::new(0, 0),
            modes: Modes::new(),
            pending: Vec::new(),
            message: String::new(),
            search: None,
            jobs,
            submitter,
            generation: Generation::default(),
            help: false,
            quit: false,
            picked_theme: None,
            stats: None,
            runs: Vec::new(),
            bar,
            copy: None,
            gesture: None,
            clicked: None,
            clicks: 0,
            focus_keys: Vec::new(),
        };
        // A failed side read is kept, not fatal: the pane opened as
        // unavailable and its header says so; the exact error goes to the
        // status line, where the person launching can read it, and not to
        // stderr, which sits behind the alternate screen.
        if let Some(e) = stash_error {
            app.message = e;
        }
        app.sync_header_keys();
        app.sync_modes();
        app
    }

    /// The mode stack follows the keyboard. Rebuilt rather than pushed and
    /// popped in step with focus, because two things kept in step drift.
    ///
    /// `panes` comes first, when a second sidebar list exists to cycle
    /// between — its Ctrl-J/Ctrl-K bindings would be a lie with one list —
    /// then the focused pane's own mode, then help and the search prompt.
    fn sync_modes(&mut self) {
        self.modes = Modes::new();
        if self.panes.list_order().len() > 1 {
            self.modes.push(panes::MODE);
        }
        if let Some(screen) = self.panes.focused() {
            self.modes.push(screen.mode());
        }
        if self.help {
            self.modes.push("help");
        }
        // Above whatever pane it stands over: the prompt is the innermost
        // thing on screen while it is open, and it is what the help panel
        // should be listing bindings for.
        if self.search.is_some() {
            self.modes.push(INPUT);
        }
    }

    /// Recomputes the cached pane geometry when anything that can move it
    /// changed: the screen size, the registrations, or the focus — the narrow
    /// layout shows only the focused pane, so a focus switch moves every
    /// rectangle below [`panes::WIDE_AT`]. Two comparisons when nothing did,
    /// which is the whole reason the result is cached.
    fn ensure_geometry(&mut self) {
        let (w, h) = self.screen.size();
        if w == 0 || h < 3 {
            self.geometry = None;
            return;
        }
        let key = (w, h, self.panes.generation(), self.panes.focused_index());
        if self.geometry.as_ref().is_some_and(|(k, _)| *k == key) {
            return;
        }
        let body = crate::panes::Rect {
            x: 0,
            y: 1,
            width: w,
            height: h - 2,
        };
        let geometry = self.layout.arrange(&self.panes.spots(), body);
        self.geometry = Some((key, geometry));
    }

    /// A pane's content rectangle — under its one header row — or `None` when
    /// the layout gave it nothing, which is the narrow layout's answer for
    /// the pane that does not have the keyboard.
    fn pane_content(&self, name: &str) -> Option<crate::panes::Rect> {
        self.geometry.as_ref()?.1.rect(name).map(|r| r.content())
    }

    /// The focus key each pane's header advertises: the first key bound to
    /// `<name>.focus`, from the live keymap — or empty when the name is
    /// unbound, in which case the header shows no key at all. Resolved once
    /// per host or registry change into [`App::focus_keys`], because a frame
    /// has no business formatting strings.
    fn sync_header_keys(&mut self) {
        let host = &self.host;
        self.focus_keys = self
            .panes
            .names()
            .map(|name| {
                let key = host
                    .keys
                    .keys_for(&format!("{name}.focus"))
                    .first()
                    .cloned()
                    .unwrap_or_default();
                (name.to_string(), key)
            })
            .collect();
    }

    fn run(
        &mut self,
        term: &mut Term,
        dirty: &AtomicBool,
        config_path: &std::path::Path,
        clock: &mut StartClock,
    ) -> io::Result<()> {
        let mut size = (0, 0);
        let mut first = true;
        while !self.quit {
            let now = Term::size();
            if now != size {
                size = now;
                self.screen.resize(size.0, size.1);
                // The rectangles the gesture was captured under are gone.
                self.gesture = None;
            }
            // Before the frame, so the message a copy leaves is on the status
            // line of the frame that follows it — OSC 52 has no reply to read,
            // so saying what happened is the only feedback there is.
            if let Some(text) = self.copy.take() {
                self.message = match term.copy(&text) {
                    Ok(()) => copied(&text),
                    Err(e) => format!("could not copy: {e}"),
                };
            }
            // Before the frame, for the same reason: a finish re-acquires
            // synchronously and the frame that follows draws what it found.
            // With no input the loop wakes on the tick, so a completed write
            // is noticed within one TICK — the tick bounds notice latency,
            // never the refresh itself, which is the `Screens::refresh`
            // call below and is as long as the re-acquisition takes.
            self.drain_jobs();
            let t = Instant::now();
            self.draw();
            let cells = self.screen.flush(term.out())?;
            // The startup's last stage covers the resize above it, so the
            // buffer allocation and the full repaint are inside the number.
            // Later frames cost one false comparison.
            if first {
                first = false;
                clock.stage("first frame flushed");
            }
            if stats_on() {
                self.stats = Some((t.elapsed(), cells));
            }

            match Term::poll(TICK)? {
                // A resize stays in the loop, which keeps the size it compares
                // against; every other event routes through [`App::input`].
                Some(Input::Resize(w, h)) => {
                    size = (w, h);
                    self.screen.resize(w, h);
                    self.gesture = None;
                }
                Some(input) => self.input(input),
                // A tick. The only thing it is for.
                None => {
                    if dirty.swap(false, Ordering::Relaxed) {
                        self.reload(config_path);
                    }
                }
            }
        }
        Ok(())
    }

    /// One event, routed: a key, a mouse gesture, or a paste.
    ///
    /// [`App::run`] and the headless tests meet here, so both exercise the same
    /// Key/Paste decision and neither has to own a terminal to do it. A resize
    /// is the loop's, which is the one event that carries no decision.
    fn input(&mut self, input: Input) {
        match input {
            Input::Key(key) => self.press(key),
            Input::Mouse(m) => self.mouse(m),
            // A paste is text, and only while a prompt stands is there anywhere
            // for it to go. It is never a key: pasted `q`, `?` and Enter are
            // characters, and the keymap never sees them.
            Input::Paste(text) if self.search.is_some() => self.edit_search(Edit::Paste(text)),
            // No prompt, no text input anywhere — the paste is dropped whole,
            // the same nothing `translate_event` returned before there was a
            // prompt to take one.
            Input::Paste(_) => {}
            Input::Resize(..) => {}
        }
    }

    /// Re-reads the config file.
    ///
    /// From defaults every time and not from the live host: otherwise deleting a
    /// line from the file would leave the old value in place, and the file would
    /// stop describing what you see. The views survive it because they read the
    /// theme on the frame that draws them — a colour, a font and now a *keymap*
    /// all land on the next frame.
    fn reload(&mut self, path: &std::path::Path) {
        let mut next = Host::new();
        let mut warnings = gitten_app::config::load(&mut next, path);
        // A theme cycled with a key outlives a save of the file, the way the
        // view's own wrap and layout indices do: the file says what this opened
        // on, and the key says what is on screen now. It loses only when the
        // file stopped defining it.
        if let Some(name) = self.picked_theme.clone() {
            if !next.select_theme(&name) {
                warnings.push(format!("the theme {name:?} is no longer registered"));
                self.picked_theme = None;
            }
        }
        self.host = next;
        self.message = match warnings.is_empty() {
            true => "gitten.toml reloaded".into(),
            // On the status line rather than stderr: stderr is behind the
            // alternate screen and would be seen only after quitting.
            false => warnings.join(" · "),
        };
        self.sync_header_keys();
        // Re-apply geometry to every pane the layout gave a rectangle to, so a
        // changed `[view] scrolloff` and a reflowed presentation reach the
        // panes that do not have the keyboard too. A pane hidden by the narrow
        // layout keeps its last viewport and is resized when it is next shown;
        // one that merely lost the keyboard — the diff beside a focused list —
        // reflows here, from its cached rectangle and not the screen's.
        self.ensure_geometry();
        let rects: Vec<(String, crate::panes::Rect)> = self
            .geometry
            .as_ref()
            .map(|(_, g)| {
                g.placed()
                    .map(|(n, r)| (n.to_string(), r.content()))
                    .collect()
            })
            .unwrap_or_default();
        for (name, rect) in &rects {
            if let Some(pane) = self.panes.get_mut(name) {
                pane.resize_to(*rect, &self.host);
            }
        }
    }

    /// One keypress.
    fn press(&mut self, key: Key) {
        self.message.clear();
        // While the prompt stands it owns the keyboard, and the full stack
        // must not see the key: a query is text, and the globals would read
        // it. See [`App::press_input`].
        if self.search.is_some() {
            self.press_input(key);
            return;
        }
        self.pending.push(key);
        // Borrowed out of the host before the match, because running a command
        // needs the host and `Resolve` holds a reference into its keymap.
        let resolved = match self.host.keys.resolve(&self.modes, &self.pending) {
            Resolve::Run(command) => Some(command.to_string()),
            Resolve::Pending => return,
            Resolve::None => {
                let unknown = gitten_core::command::chord_string(&self.pending);
                self.pending.clear();
                // Said, not swallowed: a key that does nothing and a key that
                // is not bound look identical, and only one of them is worth
                // opening `?` about.
                self.message = format!("{unknown} is not bound — ? for the keys");
                return;
            }
        };
        self.pending.clear();
        if let Some(command) = resolved {
            self.dispatch(&command);
        }
    }

    /// One keypress while the search prompt owns the keyboard.
    ///
    /// Resolved against exactly the `input` mode —
    /// [`Keymap::resolve_mode_any`], and never the full stack — because the
    /// shipped global bindings must not read the query: `?` would open help,
    /// `q` would quit, `j` would move the list. A binding written in
    /// `[keys.input]` still wins first, which is what makes Enter and Esc (or
    /// their configured replacements) close the prompt; a user who deliberately
    /// binds `?` there gets a `?` that means what they said, as mode scoping
    /// has always promised. After that, a plain unmodified character is text
    /// and nothing else is.
    ///
    /// A chord that has begun but not resolved keeps the buffer and waits. One
    /// that resolves to nothing drops the buffer rather than replaying it as
    /// text: the keys of a failed chord are a chord, and retyping them as
    /// characters could execute or duplicate input. That is the trade — a
    /// configured chord may reserve a printable first key, and this honours it.
    fn press_input(&mut self, key: Key) {
        self.pending.push(key);
        // One spelling per press, one position per key — the same shape
        // [`Keymap::resolve`] builds, resolved against exactly one mode.
        let typed: Vec<&[Key]> = self.pending.iter().map(std::slice::from_ref).collect();
        match self.host.keys.resolve_mode_any(INPUT, &typed) {
            Resolve::Run(command) => {
                self.pending.clear();
                let command = command.to_string();
                self.dispatch(&command);
            }
            // A configured chord may still be forming. Wait for it.
            Resolve::Pending => {}
            Resolve::None => {
                self.pending.clear();
                match key.code {
                    Code::Char(c) if !key.ctrl && !key.alt => self.edit_search(Edit::Char(c)),
                    Code::Backspace | Code::Delete if !key.ctrl && !key.alt => {
                        self.edit_search(Edit::Backspace)
                    }
                    // Modified keys and everything else no binding claimed do
                    // nothing, and say nothing: a key that does nothing while
                    // a field owns the keyboard is the field doing its job,
                    // and none of them may fall through to the globals.
                    _ => {}
                }
            }
        }
    }

    /// `commits.search`: gather a query over the commit list.
    ///
    /// Seeded from the standing query, so a second `/` finds the list as the
    /// first one left it; the full data stays in place, and each edit filters
    /// it live. Only over the pane *named* `commits` — the prompt holds the
    /// name, not an index, so however focus moves while it stands the query
    /// still lands on the list it was opened over — and a diff has no query,
    /// and says so.
    fn begin_search(&mut self) {
        let standing = match self.panes.get("commits") {
            Some(Screens::Commits { view, .. }) => view.query().unwrap_or_default().to_string(),
            _ => {
                self.message = "commits.search is not supported here".into();
                return;
            }
        };
        self.search = Some(standing);
        self.gesture = None;
        self.pending.clear();
        self.sync_modes();
    }

    /// One edit to the open query, and the filter it rebuilds.
    ///
    /// Per keystroke and never per frame — the next frame only draws what this
    /// already decided, which is why nothing in [`App::draw`] searches.
    fn edit_search(&mut self, edit: Edit) {
        let Some(query) = self.search.as_mut() else {
            return;
        };
        match edit {
            Edit::Char(c) => query.push(c),
            Edit::Backspace => {
                // A status line has no cursor position: both delete keys
                // remove the last Unicode scalar, whatever it is. A long query
                // keeps being edited whether its tail is on screen or not.
                query.pop();
            }
            Edit::Paste(text) => {
                // One paste, one edit, and never a transcript of keypresses:
                // line breaks and tabs become spaces and other control
                // characters are dropped, because that is what fits the one
                // line the prompt owns. Nothing here can execute, whatever the
                // paste held — see [`App::input`] for the door this came in.
                query.extend(text.chars().filter_map(|c| match c {
                    '\n' | '\r' | '\t' => Some(' '),
                    c if c.is_control() => None,
                    c => Some(c),
                }));
            }
        }
        // The edit is in; the borrow `query` holds ends with it. The query is
        // copied out rather than borrowed across the call, because the list
        // filter needs `&mut self` — a line of text, once per keystroke and
        // never per frame, against a rebuild the filter does anyway.
        let query = self.search.clone().unwrap_or_default();
        self.apply_query(&query);
    }

    /// The filter `query` describes, onto the list under the prompt — an empty
    /// one is no filter. The one place an edit, an accept or a cancel reaches
    /// the view, so the prompt and the list stay two things: the prompt is
    /// input, the list is data, and this is the line between them.
    fn apply_query(&mut self, query: &str) {
        // Disjoint field borrows, as everywhere else in this file: the query is
        // read while the list is written.
        let Self { panes, .. } = self;
        if let Some(Screens::Commits { view: list, .. }) = panes.get_mut("commits") {
            list.apply_query(query);
        }
    }

    /// `input.accept` / `input.cancel` of the open prompt.
    ///
    /// Accept keeps the last edit standing — an *empty* accept is how a filter
    /// comes off — and cancel restores the unfiltered list. Both close the
    /// prompt and give the keyboard back.
    fn finish_search(&mut self, accept: bool) {
        if self.search.is_none() {
            return;
        }
        if !accept {
            self.apply_query("");
        }
        self.search = None;
        self.pending.clear();
        self.sync_modes();
    }

    /// One mouse event.
    ///
    /// The routing, and it is short: a hit test against the cached pane
    /// rectangles, then pane-local coordinates into the pane the pointer is
    /// over. Everything below that — which text, which byte — is the view's,
    /// because only a presentation knows where its own text starts.
    ///
    /// **A gesture is captured by the pane where Down landed**, by stable
    /// name, until Up: a drag that crosses the divider keeps selecting in the
    /// pane it started in, and the release reads that pane's finished
    /// selection — never the pane the pointer happens to be over when the
    /// button comes up. One gesture, one pane's selection state.
    fn mouse(&mut self, m: Mouse) {
        let (_, h) = self.screen.size();
        // The help panel and the search prompt are drawn over the body, so a
        // click that reached a view through either would act on a row it is
        // hiding. The keyboard gathers the query; the mouse waits.
        if h < 3 || self.help || self.search.is_some() {
            return;
        }
        match m.kind {
            MouseKind::Down => {
                let Some((name, rect)) = self.hit(m.col, m.row) else {
                    return;
                };
                self.message.clear();
                let clicks = self.count(m.col, m.row, &name);
                // A click in a pane means that pane: the keyboard moves with
                // the pointer, and the modes and geometry follow.
                if self.panes.focused_name() != name {
                    self.focus_named(&name);
                }
                // A press on the header focuses and nothing else — the same
                // answer the window's pane headers give — so a gesture that
                // started there has a pane to be released against and no more.
                if m.row == rect.y {
                    self.gesture = Some(name);
                    return;
                }
                let local_col = m.col - rect.x;
                let local_row = m.row - rect.y - 1;
                {
                    let Self { panes, host, .. } = self;
                    if let Some(pane) = panes.get_mut(&name) {
                        pane.press(local_col, local_row, clicks, m.shift, host);
                    }
                }
                // Two clicks on a commit open it, which is the one gesture a
                // terminal has for "go in" besides the key that already does.
                if clicks == 2 && name == "commits" {
                    self.open_diff();
                }
                self.gesture = Some(name);
            }
            MouseKind::Drag => {
                let Some(name) = self.gesture.clone() else {
                    return;
                };
                // The captured pane, wherever it now is: coordinates are
                // clamped into its own width — a drag cannot select across
                // the divider by ending in the neighbour — and its overshoot
                // is relative to its own viewport. A pane the narrow layout
                // hid holds the gesture still; the button is still down and
                // its release still belongs to it.
                let Some(rect) = self.pane_rect(&name) else {
                    return;
                };
                let local_col = m
                    .col
                    .saturating_sub(rect.x)
                    .min(rect.width.saturating_sub(1));
                let local_row = m.row as isize - rect.y as isize - 1;
                let Self { panes, host, .. } = self;
                if let Some(pane) = panes.get_mut(&name) {
                    pane.drag(local_col, local_row, host);
                }
            }
            MouseKind::Up => {
                let Some(name) = self.gesture.take() else {
                    return;
                };
                // Copy-on-select, and this is the only place it can be: a
                // selection is finished when the button comes up, and writing
                // one to the terminal per motion event would be an escape
                // sequence per cell the pointer crossed. The text is the
                // captured pane's, not the focused one's and not the pane
                // under the pointer's.
                let text = {
                    let Self { panes, .. } = self;
                    panes
                        .get_mut(&name)
                        .map(|pane| {
                            pane.release();
                            pane.selection()
                        })
                        .unwrap_or_default()
                };
                if self.host.mouse.copy_on_select && !text.is_empty() {
                    self.copy = Some(text);
                }
            }
        }
    }

    /// The pane under a cell of the screen, by name and rectangle.
    fn hit(&self, col: usize, row: usize) -> Option<(String, crate::panes::Rect)> {
        let (_, geometry) = self.geometry.as_ref()?;
        let name = geometry.hit(col, row)?;
        Some((name.to_string(), geometry.rect(name)?))
    }

    /// A pane's full rectangle, header included, from the cached geometry.
    fn pane_rect(&self, name: &str) -> Option<crate::panes::Rect> {
        self.geometry.as_ref()?.1.rect(name)
    }

    /// How many times this cell of this pane has been clicked in quick
    /// succession.
    ///
    /// Ours to count because the protocol does not carry it — see [`DOUBLE`].
    /// Capped at three: nothing means more than a row, and an uncapped counter
    /// would make a fourth click mean something a third did not. The pane is
    /// part of the identity: the same global cell means a different pane after
    /// a focus switch, and a double click that moved panes is two clicks.
    fn count(&mut self, col: usize, row: usize, pane: &str) -> u8 {
        let now = Instant::now();
        let again = self.clicked.as_ref().is_some_and(|(at, c, r, p)| {
            (*c, *r, p.as_str()) == (col, row, pane) && now.duration_since(*at) < DOUBLE
        });
        self.clicks = match again {
            true => (self.clicks + 1).min(3),
            false => 1,
        };
        self.clicked = Some((now, col, row, pane.to_string()));
        self.clicks
    }

    /// A command name into an effect.
    ///
    /// The pane commands come first — they are the registry's, and answering
    /// them from a view would make them stop working the day a view stops
    /// being focused. Then the client's own commands, then the focused pane's.
    fn dispatch(&mut self, command: &str) {
        match command {
            // The ten names the shared registry ships, answered from the pane
            // registry and not from any view: h/l and the arrows walk the
            // reading order — sidebar lists, then the main diff — and stop at
            // the edges; Ctrl-J/Ctrl-K cycle the sidebar lists; the digits and
            // `diff.focus` name panes, and a name with no pane is said, not
            // swallowed. No new command name, no local key table: every one of
            // these resolved through the same keymap `gitten.toml` writes.
            "pane.left" => self.pane_walk(-1),
            "pane.right" => self.pane_walk(1),
            "pane.next" => self.cycle_pane(1),
            "pane.prev" => self.cycle_pane(-1),
            "status.focus" | "files.focus" | "branches.focus" | "commits.focus"
            | "stashes.focus" | "diff.focus" => {
                let name = command.strip_suffix(".focus").unwrap_or(command);
                self.focus_named(name);
            }
            "quit" => self.quit = true,
            "help" => {
                self.help = !self.help;
                // The help panel covers the panes; a gesture captured under
                // it has nowhere honest to be released into.
                self.gesture = None;
                self.sync_modes();
            }
            "back" => self.back(),
            // The whole window's, so it is here and not on a pane — and the
            // name is said, because a palette that changed without saying which
            // one it is now leaves you cycling to find out.
            "theme.cycle" => {
                self.host.cycle_theme();
                self.picked_theme = Some(self.host.theme.name.clone());
                self.message = format!("theme: {}", self.host.theme.name);
            }
            "commits.open-diff" => self.open_diff(),
            // The prompt's three names, and the whole of what a search is:
            // open it, accept it, cancel it. Each resolves through the live
            // keymap — `commits.search` in the commits mode, the other two in
            // `input` while the prompt stands — so `gitten.toml` moves them
            // the way it moves everything else.
            "commits.search" => self.begin_search(),
            "input.accept" => self.finish_search(true),
            "input.cancel" => self.finish_search(false),
            // The hunk verbs act on the *repository*, not the pane: they
            // need the source the diff was acquired from and the handle it
            // was acquired through, and a view is drawing and input only.
            // Routed here, ahead of the pane, for the same reason the
            // window routes them in its `run_command`.
            "diff.stage-hunk" | "diff.unstage-hunk" => self.hunk_verb(command),
            // The clipboard is the terminal's, not this process's — see
            // `Term::copy`. Held until the loop, which is the one place that has
            // a terminal to write to.
            "copy.selection" => {
                let text = self
                    .panes
                    .focused()
                    .map(Screens::copy_text)
                    .unwrap_or_default();
                match text.is_empty() {
                    true => self.message = "nothing to copy".into(),
                    false => self.copy = Some(text),
                }
            }
            "select.all" => {
                if let Some(pane) = self.panes.focused_mut() {
                    pane.select_all();
                }
            }
            "select.none" => {
                if let Some(pane) = self.panes.focused_mut() {
                    pane.select_none();
                }
            }
            // Disjoint field borrows rather than moving the host out and
            // back: `Host::new()` rebuilds every theme, every registry and the
            // whole resolved contrast table, and doing that per keypress is a
            // thing that would never have shown up in a timing.
            _ => {
                let known = match self.panes.focused_mut() {
                    Some(pane) => pane.run(command, &self.host),
                    None => false,
                };
                if !known {
                    self.message = format!("{command} does nothing here");
                }
            }
        }
    }

    /// Focuses the pane registered under `name` — what `commits.focus` and
    /// friends run, and what the walk and the cycle land on. Said, not
    /// swallowed, when nothing is registered under the name: an absent pane
    /// is the honest answer to an honest question, and the same sentence the
    /// window gives.
    fn focus_named(&mut self, name: &str) {
        match self.panes.position(name) {
            Some(_) => {
                self.panes.focus_named(name);
                // The keyboard is on a list again: `back` from the diff comes
                // here, to the list that held it last.
                if matches!(
                    self.panes.focused_placement(),
                    Some(panes::Placement::Sidebar { .. })
                ) {
                    self.last_list = Some(name.to_string());
                }
                self.sync_modes();
            }
            None => self.message = format!("no {name} pane"),
        }
    }

    /// Walks the keyboard one pane over — what h/l and the arrows run. The
    /// order is the reading order: the sidebar's lists top to bottom, then the
    /// main diff as the last stop. Left of the diff is the sidebar's foot;
    /// right of the last list is the diff; an edge answers and stays, which is
    /// what a walk that refuses to wrap must do to keep h/l a line and not a
    /// ring — the number keys already cover the jumping.
    fn pane_walk(&mut self, by: isize) {
        let Some(name) = self.panes.walk(by).map(str::to_string) else {
            return;
        };
        self.focus_named(&name);
    }

    /// Cycles the lists — what ctrl-j/ctrl-k do once a second list registers.
    /// The command names say *pane*: they were named for the panes that used
    /// to stack, and a rename would break every `[keys]` file in flight.
    fn cycle_pane(&mut self, by: isize) {
        if self.panes.list_order().len() < 2 {
            self.message = "no second list to cycle to".into();
            return;
        }
        if self.panes.cycle_sidebar(by) {
            self.sync_modes();
        }
    }

    /// Closes the help, or leaves the main pane for the lists.
    ///
    /// One key for both, because both are "get me out of this" and a reader does
    /// not distinguish them. From the diff it goes back to the list that held
    /// the keyboard — **without clearing or destroying the diff**, which stays
    /// exactly as it was, its cursor and its selection included: it is the
    /// window's persistent main pane, not a screen that a key dismissed. In a
    /// list it drops the mouse's selection first, and otherwise does nothing:
    /// `esc` on the thing you started with is not a quit, and a client that
    /// vanished on it would be a client you could not trust the key in.
    fn back(&mut self) {
        if self.help {
            self.help = false;
        } else if matches!(self.panes.focused_placement(), Some(panes::Placement::Main))
            && !self.panes.list_order().is_empty()
        {
            let name = self
                .last_list
                .clone()
                .unwrap_or_else(|| self.panes.list_order()[0].to_string());
            self.focus_named(&name);
        } else if self.panes.focused_mut().is_some_and(Screens::select_none) {
            // There was a selection and it is gone; that is the whole of this
            // `esc`, and the pane underneath stays where it is.
        }
        self.sync_modes();
    }

    /// Opens the diff of the commit under the cursor, into the main pane.
    ///
    /// The I/O is here and not in the view, which is the same rule the GPUI
    /// client follows: a view takes already-loaded data and never learns what a
    /// repository is. A bare revision is "what did this commit change" to
    /// [`gitten_git::Repo::pairs`], merges included.
    ///
    /// The pane named `commits` is read by that name and not by focus, the way
    /// the window reads its commit column: opening a diff is about the list's
    /// cursor, not about whoever has the keyboard. On success the diff tenant
    /// is **replaced in place** — the registry never grows, and the empty pane
    /// a commits launch registered becomes a real one — resized to the
    /// geometry the diff pane already has, and focused. On a failure the old
    /// diff survives untouched and so does the focus; the error is the
    /// message.
    fn open_diff(&mut self) {
        let commit = match self.panes.get("commits") {
            Some(Screens::Commits { view, .. }) => view.current().cloned(),
            _ => {
                self.message = "no commit selected".into();
                return;
            }
        };
        let Some(commit) = commit else { return };
        let (sha, subject) = (commit.sha.clone(), commit.subject.clone());
        let Some((path, repo)) = self.repo.clone() else {
            self.message = "a fixture has no repository to diff against".into();
            return;
        };
        let source = Source::Repo {
            path,
            arg: sha.clone(),
        };
        match acquire::acquire(View::Diff, &source, &self.host, Some(repo.as_ref())) {
            Ok(loaded) => {
                let files = match loaded.data {
                    Data::Diff(files) => files,
                    Data::Commits(_) => return,
                };
                let mut diff = Diff::new(files, &self.host);
                diff.set_bar(self.bar);
                self.ensure_geometry();
                if let Some(rect) = self.pane_content("diff") {
                    diff.set_scrolloff(self.host.view.scrolloff);
                    diff.resize(rect.width, rect.height, &self.host);
                }
                self.panes.register(
                    "diff",
                    panes::Placement::Main,
                    Screens::Diff {
                        view: diff,
                        source: Some(source),
                        label: format!("{} {subject}", &sha[..sha.len().min(8)]),
                        // Acquired this instant, so it is as current as the
                        // queue's last finish — not a generation older.
                        generation: self.generation,
                    },
                );
                // A registry replacement: whatever the mouse was holding was
                // holding a pane that no longer exists.
                self.gesture = None;
                self.sync_modes();
            }
            Err(e) => self.message = e,
        }
    }

    /// `diff.stage-hunk` / `diff.unstage-hunk`: send the hunk the keyboard is
    /// on to the index, or take it back out. The terminal's share of the
    /// window's `hunk_verb`: the gates, the patch, the verb — and not one
    /// line more, because every one of those is shared with an extension
    /// calling the same command through the same name.
    ///
    /// The verbs reach only a diff that was actually acquired: the empty pane
    /// a commits launch registers has no source, and "not loaded yet" refuses
    /// rather than pretending — no fake source, no fake generation, no patch
    /// against nothing.
    fn hunk_verb(&mut self, command: &str) {
        let (source, hunk) = match self.panes.focused() {
            Some(Screens::Diff {
                view,
                source: Some(source),
                ..
            }) => (Some(source.clone()), view.current_hunk()),
            Some(Screens::Diff { source: None, .. }) => {
                self.message = "no diff is open".into();
                return;
            }
            _ => {
                self.message = "the keyboard is not on a diff".into();
                return;
            }
        };
        // Everything decided ahead of anything queued: a refusal is said
        // here, and the queue only ever sees a job that means it.
        let handle = self.repo.as_ref().map(|(_, handle)| handle);
        match hunk_action(
            command,
            source.as_ref().expect("a diff with a source"),
            handle,
            hunk,
        ) {
            Ok(job) => {
                if self.submitter.submit(job).is_err() {
                    self.message = "the job queue is shutting down".into();
                }
            }
            Err(e) => self.message = e,
        }
    }

    /// Drains the job queue. Called before each frame, so the frame this
    /// iteration draws is the one the finished jobs produced.
    ///
    /// Every `Finished` — a refusal as much as a success, because git can
    /// answer nonzero with work already left behind — advances the generation
    /// and re-acquires **every** stale repository-backed pane on the
    /// registry, the hidden ones included: a commit list beside the diff
    /// being staged into is as stale as the diff itself. The write's own
    /// error is the message, with at most one refresh failure appended;
    /// every pane is still attempted even after one of them fails.
    fn drain_jobs(&mut self) {
        while let Some(event) = self.jobs.try_next() {
            match event {
                JobEvent::Started { name } => self.message = format!("running {name}"),
                JobEvent::Finished {
                    outcome,
                    generation,
                    done,
                    ..
                } => {
                    let write = outcome.err();
                    let mut refresh = None;
                    if generation > self.generation {
                        self.generation = generation;
                        refresh = self.refresh_stale(generation).err();
                    }
                    self.message = match (write, refresh) {
                        (Some(write), Some(refresh)) => format!("{write} · {refresh}"),
                        (Some(write), None) => write,
                        (None, Some(refresh)) => refresh,
                        // A clean write's evidence is the refreshed screen
                        // itself; a job that named its finish gets its word.
                        (None, None) => done.unwrap_or_default(),
                    };
                }
            }
        }
    }

    /// Re-acquires every registered pane a finished job has staled.
    ///
    /// Synchronous, on the terminal loop — the accepted tradeoff: `git apply`
    /// itself ran on the shared worker above, and a second terminal background
    /// protocol is not this plan's scope. The screen stays drawn while it
    /// blocks; a measured window refresh of the same work runs 48–370 ms.
    ///
    /// Every registered pane, not only the focused or visible one — and every
    /// pane *tried*, even after one of them fails: the first failure is
    /// remembered, the rest are not skipped, because a stale pane the narrow
    /// layout has hidden is still stale.
    fn refresh_stale(&mut self, target: Generation) -> Result<(), String> {
        let Some((_, repo)) = self.repo.clone() else {
            return Ok(());
        };
        let mut first = None;
        {
            let Self { panes, host, .. } = self;
            for pane in panes.iter_mut() {
                if let Some(result) = pane.refresh(target, host, repo.as_ref()) {
                    if result.is_err() {
                        // The *first* failure stands, as the contract above
                        // says: a later pane's error never overwrites an
                        // earlier one — registration order decides, and the
                        // reader met that pane first.
                        first = first.or(result.err());
                    }
                }
            }
        }
        first.map_or(Ok(()), Err)
    }

    /// A title row, the panes, a status row.
    ///
    /// Row 0 is the title and row `h - 1` the status or search prompt; the
    /// rows between belong to the panes, each inside the rectangle the cached
    /// geometry gave it — its own one-row header, then its view. Nothing in
    /// this function computes geometry: that happened once, when the size or
    /// the focus or the registrations changed, and everything here is a read.
    fn draw(&mut self) {
        let (w, h) = self.screen.size();
        if w == 0 || h < 3 {
            return;
        }
        let c = self.host.theme.chrome;
        self.screen.clear(Ink::new(c.dim, c.bg));
        let body = h - 2;
        self.ensure_geometry();

        // The title says what you are looking at — the pane that holds the
        // keyboard, and what that pane is showing.
        {
            let Self {
                screen,
                panes,
                host,
                ..
            } = self;
            let name = panes.focused_name();
            let label = panes.focused().map(Screens::label).unwrap_or("");
            title(&mut screen.row(0), host, name, label);
        }

        // Each placed pane: resize to its own content rectangle (two
        // comparisons when nothing moved — the views cache their applied
        // width), draw its header, then its view, clipped to its columns.
        {
            let Self {
                panes,
                geometry,
                screen,
                host,
                runs,
                focus_keys,
                ..
            } = self;
            let Some((_, geometry)) = geometry.as_ref() else {
                return;
            };
            for (name, rect) in geometry.placed() {
                if let Some(pane) = panes.get_mut(name) {
                    pane.resize_to(rect.content(), host);
                }
            }
            for (name, rect) in geometry.placed() {
                let Some(pane) = panes.get(name) else {
                    continue;
                };
                let focused = panes.focused_name() == name;
                let key = focus_keys
                    .iter()
                    .find(|(n, _)| n == name)
                    .map(|(_, k)| k.as_str())
                    .unwrap_or("");
                // The header pen is the rectangle's own header row, and the
                // content pen its content — the same subdivision the resize
                // above used, read back rather than recomputed.
                let head = rect.header();
                header(
                    &mut screen.span(head.y, head.x, head.width),
                    host,
                    key,
                    name,
                    pane.label(),
                    focused,
                );
                let content = rect.content();
                if content.width > 0 && content.height > 0 {
                    pane.paint(&mut *screen, content.x, content.y, focused, host, runs);
                }
            }
        }

        let ink = Ink::new(c.dim, c.status_bg);
        let loud = Ink::new(c.accent, c.status_bg);
        // While the prompt stands it owns the status row: `/`, the query, and
        // a caret — one line, no second viewport. The live count follows in
        // faint ink when there is room left to say it. A query longer than the
        // row clips through the pen, and keeps being edited whether its tail
        // is visible or not.
        if let Some(query) = self.search.as_deref() {
            let mut pen = self.screen.row(h - 1);
            pen.put(" ", ink);
            pen.put("/", loud);
            pen.put(query, Ink::new(c.fg, c.status_bg));
            pen.put("█", loud);
            if pen.room() > 2 {
                if let Some(note) = self.panes.get("commits").and_then(Screens::filter_note) {
                    pen.put(" · ", ink);
                    pen.put(&note, Ink::new(c.faint, c.status_bg));
                }
            }
            pen.wash(ink);
        } else {
            // The normal status names the focused pane first, then lets the
            // pane say where it is — one line answering "where am I" with the
            // same word the title used.
            let status = match self.message.is_empty() {
                true => self
                    .panes
                    .focused()
                    .map(|pane| {
                        format!(
                            "{} · {}",
                            self.panes.focused_name(),
                            pane.status(&self.host)
                        )
                    })
                    .unwrap_or_default(),
                false => self.message.clone(),
            };
            // The previous frame's cost, not this one's — this one has not been
            // drawn yet, and a number measured after the fact would be
            // describing a frame nobody saw.
            let cost = match self.stats {
                Some((took, cells)) => format!(" · {took:.0?} · {cells} cells"),
                None => String::new(),
            };
            let mut pen = self.screen.row(h - 1);
            pen.put(" ", ink);
            pen.put(&status, if self.message.is_empty() { ink } else { loud });
            pen.put(&cost, Ink::new(c.faint, c.status_bg));
            pen.wash(ink);
        }
        // The keys typed so far, at the right-hand end, where a modal editor
        // puts them. Only ever non-empty mid-chord.
        let pending = chord_string(&self.pending);
        if !pending.is_empty() {
            let at = w.saturating_sub(gitten_tui::screen::width(&pending) + 1);
            let mut pen = self.screen.span(h - 1, at, w - at);
            pen.put(&pending, loud);
            pen.wash(ink);
        }

        if self.help {
            help::paint(&mut self.screen, 1, body, &self.host, &self.modes);
        }
    }
}

/// The gates `diff.stage-hunk` / `diff.unstage-hunk` run, headless, in the
/// window's own words.
///
/// Everything decided before anything is queued: only a working-tree diff has
/// an index to aim at; a commit's diff is between two snapshots and has
/// neither index nor worktree in reach; a fixture or a patch has no repository
/// behind it; the keyboard may not be on a hunk at all. And a hunk whose every
/// line is an addition *looks* like a creation but only [`Repo::status`] knows
/// whether it is one — at `[diff] context = 0` a mid-file addition to a tracked
/// file carries no old numbers either, so absence of them is not evidence.
/// The status read is the same one the files pane draws from; a status that
/// cannot be read is not proof of a creation, so the patch is still emitted
/// and git's own refusal is what surfaces.
///
/// The patch is [`gitten_core::patch::emit`]'s and nothing else's; the verb is
/// a [`Write`] job against the caller's retained handle; the caller owns the
/// queue. Nothing here runs git and nothing here blocks — a constructor
/// refusal (an empty patch) comes back as an error and is said, not queued.
fn hunk_action(
    command: &str,
    source: &Source,
    repo: Option<&gitten_git::Handle>,
    hunk: Option<(String, Hunk)>,
) -> Result<Box<dyn Job>, String> {
    match source {
        Source::Repo { arg, .. } if arg.is_empty() => {}
        Source::Repo { .. } => {
            return Err(
                "only the working-tree diff can act on hunks — this one is between commits".into(),
            )
        }
        Source::Fixtures => return Err("a fixture has no repository behind it".into()),
        Source::Patch { .. } => return Err("a patch file has no repository behind it".into()),
    }
    let repo = repo.ok_or_else(|| "no repository is open".to_string())?;
    let Some((path, hunk)) = hunk else {
        return Err("the keyboard is not on a hunk".into());
    };
    // A status read on the path, and only for hunks that could be creations —
    // every other shape pays nothing and cannot be misread this way.
    let creation = !hunk.lines.iter().any(|l| l.old_no.is_some())
        && repo
            .status()
            .map(|s| {
                s.untracked
                    .iter()
                    .any(|e| e.path.as_bytes() == path.as_bytes())
            })
            .unwrap_or(false);
    if creation {
        return Err(
            "that hunk adds a new file — stage or unstage it whole from the files pane".into(),
        );
    }
    let patch = gitten_core::patch::emit(&path, &[&hunk]);
    let built = match command {
        "diff.stage-hunk" => Write::stage_patch(repo, patch),
        _ => Write::unstage_patch(repo, patch),
    };
    built.map(|job| Box::new(job) as Box<dyn Job>)
}

/// What to say on the status line after a copy.
///
/// Lines and not bytes, because a selection is measured in what you can see.
fn copied(text: &str) -> String {
    match text.lines().count() {
        1 => "copied 1 line".into(),
        n => format!("copied {n} lines"),
    }
}

/// A stash pane's label: whose repository, and how much is parked — the
/// window's title-strip line, one cell row tall here.
fn stash_label(describe: &str, parked: usize) -> String {
    format!("{describe} · {parked} parked")
}

/// Whether to report what a frame cost. `GITTEN_STATS=0` turns it off, so
/// `./dev` can set it and a caller can still say no — the same rule the window's
/// overlay follows.
fn stats_on() -> bool {
    std::env::var("GITTEN_STATS").is_ok_and(|v| v != "0")
}

/// The title row: what you are looking at, and what would change it.
///
/// The focused pane is named here in the same word its header and the status
/// line use — one name, three places, so "where is the keyboard" has one
/// answer.
fn title(pen: &mut Pen, host: &Host, pane: &str, label: &str) {
    let c = &host.theme.chrome;
    let ink = Ink::new(c.fg, c.title_bg);
    let dim = Ink::new(c.dim, c.title_bg);
    pen.put(" ", ink);
    pen.put("gitten", Ink::new(c.accent, c.title_bg).bold());
    pen.put("  ", dim);
    pen.put(pane, ink);
    pen.put("  ", dim);
    pen.put(label, dim);
    // The one key worth advertising, right-aligned. *Which* key comes from the
    // keymap, so rebinding `?` moves this too — the same reason the help panel
    // has no list of keys in it.
    let hint = match host.keys.keys_for("help").first() {
        Some(key) => format!("{key} keys "),
        None => String::new(),
    };
    let pad = pen.room().saturating_sub(gitten_tui::screen::width(&hint));
    pen.fill(pad, ' ', dim);
    pen.put(&hint, dim);
    pen.wash(dim);
}

/// A pane's header row: its focus key, its stable name, and what it is showing.
///
/// The key is the first one bound to `<name>.focus` in the live keymap — a
/// config file that moves or unbinds it moves the header without a line of
/// code, and an unbound pane advertises no key at all rather than a stale one.
/// The focused pane's key and name draw in the theme accent, which is the one
/// "which pane" mark a cell grid gets; the label is what the pane was acquired
/// under, faint, because it is the least of the three.
fn header(pen: &mut Pen, host: &Host, key: &str, name: &str, label: &str, focused: bool) {
    let c = &host.theme.chrome;
    let bg = c.title_bg;
    let key_ink = match focused {
        true => Ink::new(c.accent, bg),
        false => Ink::new(c.faint, bg),
    };
    let name_ink = match focused {
        true => Ink::new(c.accent, bg).bold(),
        false => Ink::new(c.dim, bg),
    };
    let label_ink = Ink::new(c.faint, bg);
    let dim = Ink::new(c.faint, bg);
    pen.put("  ", dim);
    if !key.is_empty() {
        pen.put(key, key_ink);
        pen.put("  ", dim);
    }
    pen.put(name, name_ink);
    if !label.is_empty() {
        pen.put("  ", dim);
        pen.put(label, label_ink);
    }
    pen.wash(dim);
}

#[cfg(test)]
mod tests {
    use super::*;
    use gitten_app::acquire::{Data, Loaded};
    use gitten_app::Started;
    use gitten_core::parse_log;

    /// Thirty commits alternating half by author and half by subject — even
    /// rows are Ada/engine, odd rows Grace/compiler — so one query hits exactly
    /// half of either. The same fixture the window's search tests use, spelled
    /// for [`parse_log`], which is the door the real data arrives through.
    fn mixed(n: usize) -> Vec<gitten_core::Commit> {
        let log: String = (0..n)
            .map(|i| {
                let even = i % 2 == 0;
                let sha = format!("{i:08}");
                let parent = match i + 1 < n {
                    true => format!("{:08}", i + 1),
                    false => String::new(),
                };
                format!(
                    "{sha}\x1f{sha}\x1f{parent}\x1f{}\x1f1\x1f{}\x1e",
                    if even { "Ada Lovelace" } else { "Grace Hopper" },
                    if even {
                        format!("engine note {i}")
                    } else {
                        format!("compiler pass {i}")
                    },
                )
            })
            .collect();
        parse_log(&log)
    }

    /// An app headlessly: every field public, no I/O, no terminal. The screen
    /// is sized by hand, exactly as the loop would have resized it.
    fn app(n: usize) -> App {
        let started = Started {
            view: View::Commits,
            source: Source::Fixtures,
            host: Host::new(),
            loaded: Loaded {
                label: "test history".into(),
                data: Data::Commits(mixed(n)),
            },
            config: std::path::PathBuf::new(),
            repo: None,
        };
        let mut app = App::new(started, Glyphs::default());
        app.screen.resize(60, 12);
        app
    }

    /// The commits list, which every search test is about — read through the
    /// registry by name, as the app itself reads it.
    fn list(app: &App) -> &Commits {
        match app.panes.get("commits") {
            Some(Screens::Commits { view: list, .. }) => list,
            _ => panic!("the commits pane is not registered"),
        }
    }

    /// The diff view, for tests that look at what the main pane holds.
    fn diff_view(app: &App) -> &Diff {
        match app.panes.get("diff") {
            Some(Screens::Diff { view, .. }) => view,
            _ => panic!("the diff pane is not registered"),
        }
    }

    /// The bottom row of the last drawn frame — the status row the prompt owns.
    fn status(app: &App) -> String {
        app.screen.row_text(app.screen.size().1 - 1)
    }

    /// The body of the last drawn frame, one string per row.
    fn body(app: &App) -> Vec<String> {
        let h = app.screen.size().1;
        (1..h - 1).map(|y| app.screen.row_text(y)).collect()
    }

    fn type_(app: &mut App, text: &str) {
        for c in text.chars() {
            app.press(Key::char(c));
        }
    }

    /// A mouse event at a cell of the screen, button unmodified.
    fn click(kind: MouseKind, col: usize, row: usize) -> Mouse {
        Mouse {
            kind,
            col,
            row,
            ctrl: false,
            alt: false,
            shift: false,
        }
    }

    #[test]
    fn shared_defaults_focus_the_registered_terminal_panes() {
        // The shipped map resolves the pane moves and the digits — through
        // `Host::new().keys`, the same map every client reads, and not
        // through anything this client owns. There is no terminal key table
        // and no terminal command name anywhere in the chain: `h` is a key
        // with a shared meaning, `pane.left` is a name this file answers.
        let keys = Host::new().keys;
        let mut commits = Modes::new();
        commits.push("commits");
        assert_eq!(
            keys.resolve(&commits, &[Key::char('h')]),
            Resolve::Run("pane.left")
        );
        assert_eq!(
            keys.resolve(&commits, &[Key::char('l')]),
            Resolve::Run("pane.right")
        );
        assert_eq!(
            keys.resolve(&commits, &[Key::plain(Code::Char('4'))]),
            Resolve::Run("commits.focus")
        );
        assert_eq!(
            keys.resolve(&commits, &[Key::plain(Code::Char('0'))]),
            Resolve::Run("diff.focus")
        );
        // Ctrl-J/Ctrl-K in `panes` mode — the mode `sync_modes` builds only
        // when a second sidebar list exists; `term.rs` proves the translation.
        let mut panes = Modes::new();
        panes.push("panes");
        panes.push("commits");
        assert_eq!(
            keys.resolve(&panes, &[Key::ctrl(Code::Char('j'))]),
            Resolve::Run("pane.next")
        );
        assert_eq!(
            keys.resolve(&panes, &[Key::ctrl(Code::Char('k'))]),
            Resolve::Run("pane.prev")
        );

        // And the dispatch answers them: the walk runs and stops at the
        // edges, the digits name panes, and an absent pane is said, exactly.
        let mut app = app(30);
        app.press(Key::char('h'));
        assert_eq!(
            app.panes.focused_name(),
            "commits",
            "left from the first pane wrapped to the diff"
        );
        app.press(Key::char('l'));
        assert_eq!(app.panes.focused_name(), "diff");
        app.press(Key::char('l'));
        assert_eq!(
            app.panes.focused_name(),
            "diff",
            "right past the main pane wrapped to the lists"
        );
        app.press(Key::plain(Code::Left));
        assert_eq!(
            app.panes.focused_name(),
            "commits",
            "the arrows stopped walking"
        );

        app.press(Key::plain(Code::Char('0')));
        assert_eq!(app.panes.focused_name(), "diff");
        app.press(Key::plain(Code::Char('4')));
        assert_eq!(app.panes.focused_name(), "commits");
        for (digit, name) in [
            ('1', "status"),
            ('2', "files"),
            ('3', "branches"),
            ('5', "stashes"),
        ] {
            app.press(Key::plain(Code::Char(digit)));
            assert_eq!(
                app.panes.focused_name(),
                "commits",
                "{name} stole the focus"
            );
            assert_eq!(app.message, format!("no {name} pane"), "{name}");
        }
        // The cycle is a registry answer too, and with one sidebar list it
        // says so rather than pretending.
        app.dispatch("pane.next");
        assert_eq!(app.message, "no second list to cycle to");
        assert_eq!(app.panes.focused_name(), "commits");
    }

    #[test]
    fn search_prompt_isolated_over_the_commits_pane() {
        // The prompt stands over the pane *named* commits — by name, not by
        // index — and while it stands, neither the mouse nor a pane focus key
        // reaches either view: the keys are query text, resolved against
        // exactly the `input` mode, and the mouse waits.
        let mut app = app(30);
        app.press(Key::char('/'));
        assert!(app.search.is_some(), "the prompt did not open");

        // The shipped focus keys are text while the prompt owns the keyboard.
        app.press(Key::plain(Code::Char('4')));
        assert_eq!(app.search.as_deref(), Some("4"), "4 was not text");
        assert_eq!(app.panes.focused_name(), "commits", "4 moved the focus");
        app.press(Key::char('h'));
        assert_eq!(app.search.as_deref(), Some("4h"), "h was not text");
        assert_eq!(app.panes.focused_name(), "commits", "h moved the focus");
        // The mouse is inert under the prompt.
        app.draw();
        app.mouse(click(MouseKind::Down, 10, 4));
        assert_eq!(
            list(&app).cursor(),
            0,
            "the mouse moved the list under the prompt"
        );
        app.mouse(click(MouseKind::Up, 10, 4));

        // Esc cancels and restores the unfiltered list.
        app.press(Key::plain(Code::Esc));
        assert!(app.search.is_none(), "esc did not cancel the prompt");
        assert_eq!(
            list(&app).filter_note(),
            None,
            "esc did not restore the list"
        );

        // And the prompt is drawn over the pane the narrow layout shows: at
        // 60 columns the commits pane is the visible one, live-filtered.
        app.press(Key::char('/'));
        type_(&mut app, "engine");
        app.draw();
        let rows = body(&app);
        assert!(rows.iter().any(|r| r.contains("engine note 0")), "{rows:?}");
        assert!(
            rows.iter().all(|r| !r.contains("compiler")),
            "a filtered-out row is drawn under the prompt: {rows:?}"
        );
        assert!(status(&app).contains("/engine"), "{:?}", status(&app));
        assert_eq!(list(&app).filter_note().as_deref(), Some("15/30"));
    }

    #[test]
    fn slash_types_live_on_the_status_line_and_enter_keeps_the_filter() {
        let mut app = app(30);
        app.press(Key::char('/'));
        assert!(app.search.is_some(), "the prompt did not open");
        type_(&mut app, "engine");
        app.draw();
        assert!(status(&app).contains("/engine"), "{:?}", status(&app));
        // The filter is live while the prompt stands: fifteen of the thirty,
        // and only those drawn.
        assert_eq!(list(&app).filter_note().as_deref(), Some("15/30"));
        let rows = body(&app);
        assert!(rows.iter().any(|r| r.contains("engine note 0")), "{rows:?}");
        assert!(
            rows.iter().all(|r| !r.contains("compiler")),
            "a filtered-out row is drawn: {rows:?}"
        );
        // Enter closes the prompt and keeps the last edit standing.
        app.press(Key::plain(Code::Enter));
        assert!(app.search.is_none());
        assert_eq!(list(&app).filter_note().as_deref(), Some("15/30"));
        app.draw();
        assert!(
            !status(&app).contains("/engine"),
            "the prompt is gone; only the filter stands"
        );
    }

    #[test]
    fn escape_cancels_and_a_second_slash_prefills_the_standing_query() {
        let mut app = app(30);
        app.press(Key::char('/'));
        type_(&mut app, "engine");
        app.press(Key::plain(Code::Enter));
        assert_eq!(list(&app).filter_note().as_deref(), Some("15/30"));

        // A second `/` finds the query as the first one left it.
        app.press(Key::char('/'));
        assert_eq!(app.search.as_deref(), Some("engine"));
        app.draw();
        assert!(status(&app).contains("/engine"), "{:?}", status(&app));
        // Cancel — the edit never stood, and the whole list comes back.
        app.press(Key::plain(Code::Esc));
        assert!(app.search.is_none());
        assert_eq!(list(&app).filter_note(), None);
        app.draw();
        assert!(
            body(&app).iter().any(|r| r.contains("compiler")),
            "cancel did not restore the list"
        );

        // An accepted empty query removes the filter too: it is the same door
        // out, reached by keeping an empty prompt.
        app.press(Key::char('/'));
        assert_eq!(app.search.as_deref(), Some(""));
        app.press(Key::plain(Code::Enter));
        assert!(app.search.is_none());
        app.press(Key::char('/'));
        type_(&mut app, "compiler");
        app.press(Key::plain(Code::Enter));
        assert_eq!(list(&app).filter_note().as_deref(), Some("15/30"));
    }

    #[test]
    fn question_mark_is_text_in_input_mode_and_help_outside_it() {
        // The collision the exact-mode rule exists for: the shipped `?` is a
        // global binding, and a prompt that resolved the full stack would open
        // help with every character you typed.
        let mut app = app(30);
        app.press(Key::char('?'));
        assert!(app.help, "help did not open while no prompt stood");
        app.press(Key::char('?'));
        assert!(!app.help);

        app.press(Key::char('/'));
        app.press(Key::char('?'));
        assert!(!app.help, "help opened over the prompt");
        assert_eq!(app.search.as_deref(), Some("?"), "the ? was not text");
        // The same for the other printable global: `q` quits nothing here.
        app.press(Key::char('q'));
        assert!(!app.quit);
        assert_eq!(app.search.as_deref(), Some("?q"));
        app.draw();
        assert!(status(&app).contains("/?q"), "{:?}", status(&app));
    }

    #[test]
    fn pasted_commands_are_query_text_only_while_input_is_open() {
        // One paste, one edit, through the same [`App::input`] the loop uses —
        // and never a key: the pasted `q` and `?` are characters in a string,
        // the keymap is never consulted, and nothing executes between lines.
        let mut open = app(30);
        open.press(Key::char('/'));
        open.input(Input::Paste("q?\nengine\tb".into()));
        assert!(!open.quit, "a pasted q quit");
        assert!(!open.help, "a pasted ? opened help");
        assert_eq!(
            open.search.as_deref(),
            Some("q? engine b"),
            "the paste did not arrive as one sanitized edit"
        );
        // It never became commands, but it did become a query: one that
        // matches nothing, which is the filter doing what the text says.
        assert_eq!(list(&open).filter_note().as_deref(), Some("0/30"));
        open.draw();
        assert!(
            status(&open).contains("/q? engine b"),
            "{:?}",
            status(&open)
        );

        // With no prompt there is nowhere for a paste to go: it is dropped
        // whole, and nothing in the app moves.
        let mut quiet = app(30);
        quiet.input(Input::Paste("q?".into()));
        assert!(!quiet.quit);
        assert!(!quiet.help);
        assert_eq!(quiet.search.as_deref(), None);
        assert!(quiet.pending.is_empty());
        assert_eq!(list(&quiet).filter_note(), None);
    }

    #[test]
    fn configured_input_bindings_and_chords_own_the_pending_buffer() {
        // Exactly what `[keys.input]` would write: the shipped Enter unbound,
        // accept on another key, and a two-key chord for cancel — the chord is
        // what exercises `Resolve::Pending`, which a single key never reaches.
        let mut app = app(30);
        app.host.keys.unbind(INPUT, "enter");
        app.host
            .keys
            .bind(INPUT, "ctrl-s", "input.accept")
            .expect("test binding");
        app.host
            .keys
            .bind(INPUT, "alt-x alt-z", "input.cancel")
            .expect("test binding");
        app.press(Key::char('/'));
        type_(&mut app, "engine");

        // The unbound Enter is now an unclaimed key in the input mode: it
        // edits nothing, and it must not fall through to the globals either.
        app.press(Key::plain(Code::Enter));
        assert!(app.search.is_some(), "the unbound enter closed the prompt");
        assert_eq!(app.search.as_deref(), Some("engine"));

        // The configured accept key closes it, filter standing.
        app.press(Key::ctrl(Code::Char('s')));
        assert!(app.search.is_none());
        assert_eq!(list(&app).filter_note().as_deref(), Some("15/30"));
        assert!(app.pending.is_empty(), "the buffer did not clear on finish");

        // The chord's first key waits for its continuation and touches nothing.
        app.press(Key::char('/'));
        let alt_x = Key::new(Code::Char('x'), false, true, false);
        let alt_z = Key::new(Code::Char('z'), false, true, false);
        app.press(alt_x);
        assert_eq!(
            app.search.as_deref(),
            Some("engine"),
            "a pending chord edited"
        );
        assert_eq!(
            chord_string(&app.pending),
            "alt-x",
            "the chord did not wait"
        );
        // An invalid continuation drops the buffer rather than replaying it as
        // text; the character typed is still text.
        app.press(Key::char('q'));
        assert_eq!(app.search.as_deref(), Some("engineq"));
        assert!(app.pending.is_empty());
        // Completed, the chord cancels: the list is whole again.
        app.press(alt_x);
        app.press(alt_z);
        assert!(app.search.is_none());
        assert_eq!(list(&app).filter_note(), None);
        assert!(app.pending.is_empty());
    }

    #[test]
    fn markdown_reflows_to_the_diff_pane_not_the_screen() {
        // The committed Markdown fixture, in the diff pane of a 120-column
        // frame. The sidebar takes its share and the divider one column, so
        // the diff pane's content is 79 wide — and *that* is the budget the
        // Markdown presentation wraps at, because [`Diff::resize`] is handed
        // the pane's width and passes it down. The screen's 120 never
        // reaches the presentation.
        const MD: &str = include_str!("../tests/fixtures/md.diff");
        let mut app = app(30);
        app.screen = Screen::new(120, 24);
        // The fixture is a patch: a diff tenant over a patch source, exactly
        // as a `--patch` launch would register one. Nothing here is
        // repository-backed, so no refresh can touch it.
        let files = gitten_core::parse_unified_diff(MD);
        let mut diff = Diff::new(files.clone(), &app.host);
        diff.set_bar(app.bar);
        app.panes.register(
            "diff",
            panes::Placement::Main,
            Screens::Diff {
                view: diff,
                source: Some(Source::Patch { file: None }),
                label: "md.diff".into(),
                generation: app.generation,
            },
        );
        app.sync_modes();
        app.draw();

        // The geometry: sidebar 40, one divider, diff 79 — and the diff
        // pane's rows are the 79-column answer, not the 120-column one.
        let content = app.pane_content("diff").expect("the diff pane is visible");
        assert_eq!(
            content,
            crate::panes::Rect {
                x: 41,
                y: 2,
                width: 79,
                height: 21
            }
        );
        let host = Host::new();
        let mut at_pane = Diff::new(files.clone(), &host);
        at_pane.resize(79, 21, &host);
        let mut at_screen = Diff::new(files, &host);
        at_screen.resize(120, 21, &host);
        assert_eq!(
            diff_view(&app).rows(),
            at_pane.rows(),
            "the pane drew at another width"
        );
        assert!(
            at_pane.rows() > at_screen.rows(),
            "the fixture does not wrap differently at 79 and 120: {} vs {}",
            at_pane.rows(),
            at_screen.rows()
        );

        // Every painted row stays inside the diff span: the divider column
        // the layout owns is blank, and the text starts at the pane's edge.
        let (w, h) = app.screen.size();
        for y in 2..h - 1 {
            assert_eq!(
                app.screen.char_at(40, y),
                Some(' '),
                "row {y} drew into the divider"
            );
        }
        assert!(
            (2..h - 1).any(|y| (41..w).any(|x| app.screen.char_at(x, y).is_some_and(|c| c != ' '))),
            "nothing was drawn in the diff pane"
        );

        // Down at 95 columns the layout is narrow: the diff takes the whole
        // body, reflows to *that* width — and the Markdown model is not
        // rebuilt: a selection made before the switch still names the same
        // bytes, through the same logical lines.
        // Wherever the presentation put a word: a double click takes one.
        let mut selected = String::new();
        for row in 3..20 {
            app.mouse(click(MouseKind::Down, 60, row));
            app.mouse(click(MouseKind::Up, 60, row));
            app.mouse(click(MouseKind::Down, 60, row));
            app.mouse(click(MouseKind::Up, 60, row));
            selected = diff_view(&app).selection();
            if !selected.is_empty() {
                break;
            }
        }
        assert!(!selected.is_empty(), "the double click took no word");
        app.screen.resize(95, 24);
        app.draw();
        assert_eq!(
            app.pane_content("diff"),
            Some(crate::panes::Rect {
                x: 0,
                y: 2,
                width: 95,
                height: 21
            })
        );
        // The narrow reflow used the *new* width: the same row count the
        // presentation produces when a pane of exactly that width asks for
        // it. (This fixture happens to wrap identically at 79 and 95; it is
        // 79 against 120 that differs, asserted above.)
        let mut at_narrow = Diff::new(gitten_core::parse_unified_diff(MD), &host);
        at_narrow.resize(95, 21, &host);
        assert_eq!(diff_view(&app).rows(), at_narrow.rows());
        assert_eq!(
            diff_view(&app).selection(),
            selected,
            "the reflow lost the line the selection was on"
        );
    }

    #[test]
    fn title_headers_and_status_name_the_focus_from_live_keys() {
        let mut app = app(30);
        app.screen = Screen::new(120, 24);
        app.draw();
        let (w, h) = app.screen.size();
        let _ = w;
        let c = app.host.theme.chrome;

        // Both headers, each naming its pane and its first configured focus
        // key — 4 for commits, 0 for diff, straight out of the shipped map.
        let commits_header = app.screen.row_text(1)[..40.min(w)].to_string();
        assert!(commits_header.contains('4'), "{commits_header:?}");
        assert!(commits_header.contains("commits"), "{commits_header:?}");
        let diff_header = app.screen.row_text(1)[41..].to_string();
        assert!(diff_header.contains('0'), "{diff_header:?}");
        assert!(diff_header.contains("diff"), "{diff_header:?}");
        assert!(
            diff_header.contains(EMPTY_DIFF_LABEL),
            "the empty pane did not say so: {diff_header:?}"
        );

        // The focused header wears the accent; the other does not. The name
        // starts five columns into each header — two spaces, the key, two
        // more.
        assert_eq!(
            app.screen.ink(5, 1).unwrap().fg,
            c.accent,
            "commits is focused"
        );
        assert_eq!(
            app.screen.ink(46, 1).unwrap().fg,
            c.dim,
            "the diff header drew as if it had the keyboard"
        );
        assert!(
            app.screen.row_text(0).contains("commits"),
            "the title did not name the focus"
        );
        assert!(
            app.screen.row_text(h - 1).contains("commits ·"),
            "the status did not name the focus: {:?}",
            app.screen.row_text(h - 1)
        );

        // Focus moves — the accent, the title and the status all follow.
        app.press(Key::plain(Code::Char('0')));
        app.draw();
        assert_eq!(
            app.screen.ink(46, 1).unwrap().fg,
            c.accent,
            "diff took the accent"
        );
        assert_eq!(
            app.screen.ink(5, 1).unwrap().fg,
            c.dim,
            "commits kept the accent"
        );
        assert!(
            app.screen.row_text(0).contains("diff"),
            "the title did not follow"
        );
        assert!(
            app.screen.row_text(h - 1).contains("diff ·"),
            "{:?}",
            app.screen.row_text(h - 1)
        );

        // A config override changes the displayed key without changing pane
        // code: the header reads the live keymap through the cache, and an
        // unbound pane advertises no key at all.
        app.press(Key::plain(Code::Char('4')));
        assert!(app.host.keys.unbind("global", "4"));
        app.sync_header_keys();
        app.draw();
        let header = app.screen.row_text(1);
        assert!(
            !header.contains('4'),
            "the unbound key is still advertised: {header:?}"
        );
        assert!(header.contains("commits"), "{header:?}");

        // Narrow: only the focused pane draws a header at all.
        app.press(Key::plain(Code::Char('0')));
        app.screen.resize(60, 12);
        app.draw();
        assert!(
            app.screen.row_text(1).contains("diff"),
            "{:?}",
            app.screen.row_text(1)
        );
        assert!(
            !app.screen.row_text(1).contains("commits"),
            "the hidden pane drew a header: {:?}",
            app.screen.row_text(1)
        );
    }

    #[test]
    fn help_and_config_reload_follow_the_focused_pane() {
        let mut app = app(30);
        // Tall enough that the help panel shows past the global section into
        // the focused mode's own bindings — the property under test.
        app.screen = Screen::new(60, 50);

        // Help is a function of the active modes, and the focused pane is
        // what decides those: over the commits pane it lists the commits
        // bindings and not the diff's, and over the diff pane the other way.
        app.dispatch("help");
        app.draw();
        let rows = body(&app);
        // The panel shows keys and what they do, not command names — the
        // commits binding's own description is the marker.
        assert!(
            rows.iter().any(|r| r.contains("show the diff pane")),
            "help did not follow the commits mode: {rows:?}"
        );
        assert!(
            rows.iter().all(|r| !r.contains("the next presentation")),
            "help listed a diff binding over the commits pane"
        );
        app.dispatch("help");
        app.press(Key::plain(Code::Char('0')));
        app.dispatch("help");
        app.draw();
        let rows = body(&app);
        assert!(
            rows.iter().any(|r| r.contains("the next presentation")),
            "help did not follow the diff mode: {rows:?}"
        );
        assert!(
            rows.iter()
                .all(|r| !r.contains("show the diff pane, loaded")),
            "help listed the commits bindings over the diff"
        );
        app.dispatch("help");

        // A reload rebuilds the host from the file and re-applies geometry to
        // every pane — and keeps the focus, the live search and the
        // rectangles it cached.
        let path =
            std::env::temp_dir().join(format!("gitten-tui-test-{}.toml", std::process::id()));
        std::fs::write(&path, "[view]\nscrolloff = 3\n").expect("a config file");
        // Back on the commits pane, where `/` lives, then the prompt.
        app.press(Key::plain(Code::Char('4')));
        app.press(Key::char('/'));
        type_(&mut app, "engine");
        app.reload(&path);
        assert!(app.search.is_some(), "the reload closed the live prompt");
        assert_eq!(list(&app).filter_note().as_deref(), Some("15/30"));
        assert_eq!(
            app.panes.focused_name(),
            "commits",
            "the reload moved the focus"
        );
        assert_eq!(app.message, "gitten.toml reloaded", "{:?}", app.message);
        // The focused pane keeps its cached rectangle; the diff the narrow
        // layout hides keeps its viewport and is resized when next shown.
        assert!(app.pane_content("commits").is_some());
        assert!(
            app.pane_content("diff").is_none(),
            "a hidden pane kept a rectangle"
        );
        app.press(Key::plain(Code::Enter));
        assert!(app.search.is_none());
        assert_eq!(list(&app).filter_note().as_deref(), Some("15/30"));
        app.draw();
        assert!(
            body(&app).iter().any(|r| r.contains("engine")),
            "the reload left the frame unusable"
        );
        // And "when next shown" is now: the focus switch resizes the pane
        // before painting it, at the body's own width.
        app.press(Key::plain(Code::Char('0')));
        app.draw();
        assert!(
            app.pane_content("diff").is_some(),
            "a hidden pane was not resized on show"
        );
        std::fs::remove_file(&path).ok();
    }
}

#[cfg(test)]
mod staging {
    use super::*;
    use gitten_core::command::Code;
    use gitten_core::parse_unified_diff;
    use gitten_core::refs::Stash;
    use gitten_core::status::Status;
    use gitten_core::Commit;
    use gitten_git::{Handle, Pair, Repo};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    /// The unedited side, and the edited sides: `which` bit-flags the two
    /// edits into existence — 0 neither, 1 the first, 2 the second, 3 both.
    fn side(which: usize) -> Vec<Arc<str>> {
        (0..40usize)
            .map(|i| match (which & 1 != 0, i) {
                (true, 4) => Arc::<str>::from("EDIT ONE"),
                _ => match (which & 2 != 0, i) {
                    (true, 34) => Arc::<str>::from("EDIT TWO"),
                    _ => Arc::<str>::from(format!("line {i}").as_str()),
                },
            })
            .collect()
    }

    fn pair(path: &str, old: Vec<Arc<str>>, new: Vec<Arc<str>>) -> Pair {
        Pair {
            path: path.to_string(),
            old_path: None,
            status: 'M',
            old,
            new,
            old_oid: None,
            new_oid: None,
            binary: false,
        }
    }

    /// The working-tree diff the fake answers with before anything lands:
    /// two hunks, one per edit, under one file.
    const HUNK_DIFF: &str = "\
diff --git a/new.txt b/new.txt
--- /dev/null
+++ b/new.txt
@@ -0,0 +1,2 @@
+created
+lines
diff --git a/tracked.txt b/tracked.txt
--- a/tracked.txt
+++ b/tracked.txt
@@ -5,0 +6,1 @@
+inserted
";

    #[derive(Default)]
    struct FakeState {
        /// What `pairs` answers before and after the first write lands —
        /// the world the refresh is supposed to re-read.
        before: Vec<Pair>,
        after: Vec<Pair>,
        applied: usize,
        /// Patches beginning with one of these are refused: a job that
        /// fails, without failing the queue.
        refuses: Vec<Vec<u8>>,
        /// Every write that reached the repository, recorded.
        writes: Vec<String>,
        pairs_reads: usize,
        log_reads: usize,
        untracked: Vec<Vec<u8>>,
        /// When set, the next log (or pairs) read fails with exactly this
        /// message — so two panes can fail *simultaneously*, each in its own
        /// words, and a test can name which of the two errors stood.
        fail_log: Option<String>,
        fail_pairs: Option<String>,
        /// The stack the ancillary read answers, newest first, and how many
        /// times it was read. Writes record the address they aimed at and,
        /// when they land, change what the next read answers — which is what
        /// lets a test observe a refresh reading the stack after a drop.
        stashes: Vec<Stash>,
        stash_writes: Vec<String>,
        stash_reads: usize,
        /// When set, the next stash write fails with exactly this message and
        /// changes nothing: git's refusal, with the stack left as it was.
        refuse_stash: Option<String>,
    }

    /// A repository that exists only as this struct. Reads answer what the
    /// test handed in; writes are recorded and — when they land — change
    /// what the next read answers, which is what lets a test observe a
    /// refresh reading the world after the write. No process, no tty, no
    /// window, and nothing recorded is a real repository.
    struct FakeRepo(Arc<Mutex<FakeState>>);

    fn three_commits() -> Vec<Commit> {
        ["one", "two", "three"]
            .map(|sha| Commit {
                sha: sha.into(),
                short: sha.into(),
                parents: Box::from(&[][..]),
                author: "Ada Lovelace".into(),
                timestamp: 1,
                subject: format!("commit {sha}"),
            })
            .to_vec()
    }

    impl Repo for FakeRepo {
        fn log(&self, _limit: usize) -> gitten_git::Result<Vec<Commit>> {
            let mut s = self.0.lock().unwrap();
            s.log_reads += 1;
            if let Some(message) = s.fail_log.clone() {
                return Err(message);
            }
            Ok(three_commits())
        }

        fn pairs(&self, _revspec: &str) -> gitten_git::Result<Vec<Pair>> {
            let mut s = self.0.lock().unwrap();
            s.pairs_reads += 1;
            if let Some(message) = s.fail_pairs.clone() {
                return Err(message);
            }
            Ok(match s.applied {
                0 => s.before.clone(),
                _ => s.after.clone(),
            })
        }

        fn status(&self) -> gitten_git::Result<Status> {
            let s = self.0.lock().unwrap();
            Ok(Status {
                untracked: s
                    .untracked
                    .iter()
                    .map(|p| gitten_core::status::UntrackedEntry {
                        path: gitten_core::status::PathBytes::from_bytes(p),
                    })
                    .collect(),
                ..Status::default()
            })
        }

        fn describe(&self) -> String {
            "fake (main)".into()
        }

        fn stashes(&self) -> gitten_git::Result<Vec<Stash>> {
            let mut s = self.0.lock().unwrap();
            s.stash_reads += 1;
            Ok(s.stashes.clone())
        }

        fn stash_push(&self, message: Option<&str>) -> gitten_git::Result<usize> {
            let mut s = self.0.lock().unwrap();
            if let Some(e) = s.refuse_stash.clone() {
                return Err(e);
            }
            // git's own semantics: a new entry at the top, every later index
            // shifted by one. The commit is the fake's stand-in for the stash
            // object's identity — what a refresh anchors by.
            let landed = format!("pushed{}", s.stash_writes.len());
            for (i, entry) in s.stashes.iter_mut().enumerate() {
                entry.index = i + 1;
            }
            s.stashes.insert(
                0,
                Stash {
                    index: 0,
                    message: "WIP on fake (main)".into(),
                    commit: landed,
                },
            );
            s.stash_writes
                .push(format!("push {}", message.unwrap_or("")));
            Ok(0)
        }

        fn stash_apply(&self, index: usize) -> gitten_git::Result<()> {
            let mut s = self.0.lock().unwrap();
            if let Some(e) = s.refuse_stash.clone() {
                return Err(e);
            }
            // An apply keeps the entry; only the write is recorded.
            s.stash_writes.push(format!("apply stash@{{{index}}}"));
            Ok(())
        }

        fn stash_pop(&self, index: usize) -> gitten_git::Result<()> {
            let mut s = self.0.lock().unwrap();
            if let Some(e) = s.refuse_stash.clone() {
                return Err(e);
            }
            // A clean pop drops the entry and renumbers everything above it.
            s.stash_writes.push(format!("pop stash@{{{index}}}"));
            let at = index.min(s.stashes.len().saturating_sub(1));
            s.stashes.remove(at);
            for (i, entry) in s.stashes.iter_mut().enumerate() {
                entry.index = i;
            }
            Ok(())
        }

        fn stash_drop(&self, index: usize) -> gitten_git::Result<()> {
            let mut s = self.0.lock().unwrap();
            if let Some(e) = s.refuse_stash.clone() {
                return Err(e);
            }
            s.stash_writes.push(format!("drop stash@{{{index}}}"));
            let at = index.min(s.stashes.len().saturating_sub(1));
            s.stashes.remove(at);
            for (i, entry) in s.stashes.iter_mut().enumerate() {
                entry.index = i;
            }
            Ok(())
        }

        fn stage_patch(&self, patch: &[u8]) -> gitten_git::Result<()> {
            let mut s = self.0.lock().unwrap();
            s.writes
                .push(format!("stage {}", String::from_utf8_lossy(patch)));
            if s.refuses.iter().any(|r| patch.starts_with(r)) {
                return Err("the fake refused".into());
            }
            s.applied += 1;
            Ok(())
        }

        fn unstage_patch(&self, patch: &[u8]) -> gitten_git::Result<()> {
            let mut s = self.0.lock().unwrap();
            s.writes
                .push(format!("unstage {}", String::from_utf8_lossy(patch)));
            s.applied += 1;
            Ok(())
        }
    }

    /// The fake's stack: two entries, newest first, whose commits are the
    /// identity a refresh anchors by.
    fn two_stashes() -> Vec<Stash> {
        vec![
            Stash {
                index: 0,
                message: "On main: wip things".into(),
                commit: "aaa".into(),
            },
            Stash {
                index: 1,
                message: "On dev: other work".into(),
                commit: "bbb".into(),
            },
        ]
    }

    /// The fake's working-tree world: one file, both edits, untracked list
    /// as given. OIDs are `None` — a worktree pair never caches, so no test
    /// ever reads a neighbour's answer.
    fn fake(untracked: &[&str]) -> (Handle, Arc<Mutex<FakeState>>) {
        let state = Arc::new(Mutex::new(FakeState {
            before: vec![pair("f.txt", side(0), side(3))],
            after: vec![pair("f.txt", side(0), side(2))],
            refuses: vec![b"refuse".to_vec()],
            untracked: untracked.iter().map(|u| u.as_bytes().to_vec()).collect(),
            stashes: two_stashes(),
            ..Default::default()
        }));
        (Arc::new(FakeRepo(Arc::clone(&state))), state)
    }

    /// The fake's working-tree world, tall in *hunks* and not just in lines:
    /// an edit every ten lines, so the diff the pane shows has twenty hunks
    /// and hundreds of rows. A diff shorter than its pane never moves `top`,
    /// however hard the wheel is spun — the pane has nothing to scroll — and
    /// two edits in a four-hundred-line file are still one screen of hunks.
    /// Same shape as [`fake`]: one file, `applied` flipping `before` to
    /// `after` on the first write.
    fn fake_tall(untracked: &[&str]) -> (Handle, Arc<Mutex<FakeState>>) {
        let side = |edited: bool| -> Vec<Arc<str>> {
            (0..200usize)
                .map(|i| match edited && i % 10 == 4 {
                    true => Arc::<str>::from(format!("EDIT {i}").as_str()),
                    false => Arc::<str>::from(format!("line {i}").as_str()),
                })
                .collect()
        };
        let state = Arc::new(Mutex::new(FakeState {
            before: vec![pair("f.txt", side(false), side(true))],
            after: vec![pair("f.txt", side(false), side(false))],
            refuses: vec![b"refuse".to_vec()],
            untracked: untracked.iter().map(|u| u.as_bytes().to_vec()).collect(),
            stashes: two_stashes(),
            ..Default::default()
        }));
        (Arc::new(FakeRepo(Arc::clone(&state))), state)
    }

    /// An application on one diff screen, from a hand-built `Started`: no
    /// arguments, no config file, and no terminal — the frame is an
    /// in-memory `Screen`, which is the whole of what the draw path needs.
    fn app_on_diff(source: Source, repo: Option<Handle>) -> App {
        let started = gitten_app::Started {
            view: View::Diff,
            source,
            host: Host::new(),
            loaded: acquire::Loaded {
                label: "fake".into(),
                data: Data::Diff(parse_unified_diff(HUNK_DIFF)),
            },
            config: std::path::PathBuf::from("/nonexistent/gitten.toml"),
            repo,
        };
        let mut app = App::new(started, Glyphs::default());
        app.screen = Screen::new(60, 24);
        app
    }

    /// The same, on a repository: the diff the app opens on is what *this*
    /// handle answers, acquired through the front door, so a refresh
    /// re-reading the same handle lands on comparable data.
    fn app_on_fake(source: &Source, handle: &Handle) -> App {
        let host = Host::new();
        let loaded =
            acquire::acquire(View::Diff, source, &host, Some(handle.as_ref())).expect("changes");
        let started = gitten_app::Started {
            view: View::Diff,
            source: source.clone(),
            host,
            loaded,
            config: std::path::PathBuf::from("/nonexistent/gitten.toml"),
            repo: Some(handle.clone()),
        };
        let mut app = App::new(started, Glyphs::default());
        app.screen = Screen::new(60, 24);
        app
    }

    /// `row` keypresses down, one at a time — the same `view.down` the key
    /// sends, so the cursor lands where the keyboard would have put it.
    fn move_to(app: &mut App, row: usize) {
        app.dispatch("view.top");
        for _ in 0..row {
            app.dispatch("view.down");
        }
    }

    /// Types a query into the open prompt, one character per key.
    fn type_(app: &mut App, text: &str) {
        for c in text.chars() {
            app.press(Key::char(c));
        }
    }

    /// Waits for the queue to finish what was submitted, draining as the
    /// loop would. Bounded, because a broken queue must fail the test and
    /// not hang it.
    fn until(deadline: Duration, mut done: impl FnMut() -> bool) -> bool {
        let end = std::time::Instant::now() + deadline;
        while std::time::Instant::now() < end {
            if done() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        false
    }

    /// A hundred commits, so a pane's viewport has something to scroll.
    fn hundred_commits() -> Vec<Commit> {
        (0..100)
            .map(|i| {
                let sha = format!("{i:08}");
                let parent = match i + 1 < 100 {
                    true => format!("{:08}", i + 1),
                    false => String::new(),
                };
                Commit {
                    sha: sha.clone(),
                    short: sha,
                    parents: match parent.is_empty() {
                        true => Vec::new().into_boxed_slice(),
                        false => vec![parent].into_boxed_slice(),
                    },
                    author: "Ada Lovelace".into(),
                    timestamp: 1,
                    subject: format!("commit {i}"),
                }
            })
            .collect()
    }

    /// A wide application on a repository: the commits pane focused, the
    /// empty diff beside it, both visible at 120 columns.
    fn commits_app(handle: &Handle) -> App {
        let started = gitten_app::Started {
            view: View::Commits,
            source: Source::Repo {
                path: std::path::PathBuf::from("/fake"),
                arg: String::new(),
            },
            host: Host::new(),
            loaded: acquire::Loaded {
                label: "fake".into(),
                data: Data::Commits(hundred_commits()),
            },
            config: std::path::PathBuf::from("/nonexistent/gitten.toml"),
            repo: Some(handle.clone()),
        };
        let mut app = App::new(started, Glyphs::default());
        app.screen = Screen::new(120, 24);
        app
    }

    fn commits_of(app: &App) -> &Commits {
        match app.panes.get("commits") {
            Some(Screens::Commits { view, .. }) => view,
            _ => panic!("the commits pane is not registered"),
        }
    }

    fn diff_of(app: &App) -> &Diff {
        match app.panes.get("diff") {
            Some(Screens::Diff { view, .. }) => view,
            _ => panic!("the diff pane is not registered"),
        }
    }

    /// A mouse event at a cell of the screen, button unmodified.
    fn click(kind: MouseKind, col: usize, row: usize) -> Mouse {
        Mouse {
            kind,
            col,
            row,
            ctrl: false,
            alt: false,
            shift: false,
        }
    }

    #[test]
    fn enter_replaces_and_focuses_a_persistent_diff_and_back_returns() {
        let (handle, state) = fake(&[]);
        let mut app = commits_app(&handle);
        app.draw();
        assert_eq!(app.panes.focused_name(), "commits");
        let open_reads = state.lock().unwrap().pairs_reads;

        // Enter acquires the selected commit's diff exactly once, replaces
        // the empty tenant — never appends — and focuses it. (The third
        // tenant is the stack a repository launch always carries.)
        app.press(Key::plain(Code::Enter));
        assert_eq!(app.panes.focused_name(), "diff");
        assert_eq!(app.panes.names().count(), 3, "enter appended a pane");
        assert_eq!(state.lock().unwrap().pairs_reads, open_reads + 1);
        assert!(
            matches!(
                app.panes.get("diff"),
                Some(Screens::Diff {
                    source: Some(_),
                    ..
                })
            ),
            "the diff pane was not really acquired"
        );
        // The commits pane stays resident, its cursor where it was.
        assert_eq!(commits_of(&app).cursor(), 0);

        // The diff's state is its own: move it, leave, and it is unchanged —
        // `esc` moved the keyboard, not the pane.
        app.dispatch("view.down");
        app.dispatch("view.down");
        assert_eq!(diff_of(&app).cursor(), 2);
        app.press(Key::plain(Code::Esc));
        assert_eq!(app.panes.focused_name(), "commits");
        assert!(
            matches!(
                app.panes.get("diff"),
                Some(Screens::Diff {
                    source: Some(_),
                    ..
                })
            ),
            "back destroyed the diff"
        );
        assert_eq!(
            diff_of(&app).cursor(),
            2,
            "back disturbed the diff's cursor"
        );
        assert_eq!(diff_of(&app).layout_name(), "unified");

        // A second Enter replaces the same tenant, again exactly once.
        let reads = state.lock().unwrap().pairs_reads;
        app.press(Key::plain(Code::Enter));
        assert_eq!(
            app.panes.names().count(),
            3,
            "a second enter appended a pane"
        );
        assert_eq!(app.panes.focused_name(), "diff");
        assert_eq!(state.lock().unwrap().pairs_reads, reads + 1);

        // A direct diff launch has one tenant and `esc` goes nowhere.
        let mut app = app_on_diff(Source::Fixtures, None);
        assert_eq!(app.panes.names().count(), 1);
        app.press(Key::plain(Code::Esc));
        assert_eq!(app.panes.focused_name(), "diff");
        assert_eq!(app.panes.names().count(), 1, "esc invented a pane");
    }

    #[test]
    fn the_empty_diff_pane_refuses_hunk_verbs_until_enter_replaces_it() {
        // The empty pane a commits launch registers was never acquired: no
        // source, nothing to act on. The verbs refuse it by name — no write,
        // no submit, no pretending it holds a working tree — and Enter, the
        // same key that replaces the tenant, is what makes the path live.
        let (handle, state) = fake(&[]);
        let mut app = commits_app(&handle);
        app.dispatch("diff.focus");
        assert_eq!(app.panes.focused_name(), "diff");

        app.dispatch("diff.stage-hunk");
        assert_eq!(app.message, "no diff is open");
        app.dispatch("diff.unstage-hunk");
        assert_eq!(app.message, "no diff is open");
        assert!(
            state.lock().unwrap().writes.is_empty(),
            "the empty pane queued a write"
        );

        // Enter replaces the tenant with a real acquisition — a *commit*
        // diff, which is what this key opens from a list — and the verb path
        // is live: the refusal now comes from the verb's own gate reading
        // the new source, not from the empty pane. (The write itself landing
        // is `a_refreshed_frame_is_drawable_headlessly`'s, on a launch whose
        // diff *is* the working tree.)
        app.dispatch("commits.focus");
        app.press(Key::plain(Code::Enter));
        assert_eq!(app.panes.focused_name(), "diff");
        assert!(matches!(
            app.panes.get("diff"),
            Some(Screens::Diff {
                source: Some(_),
                ..
            })
        ));
        app.dispatch("diff.stage-hunk");
        assert_eq!(
            app.message,
            "only the working-tree diff can act on hunks — this one is between commits"
        );
        app.dispatch("diff.unstage-hunk");
        assert_eq!(
            app.message,
            "only the working-tree diff can act on hunks — this one is between commits"
        );
        assert!(
            state.lock().unwrap().writes.is_empty(),
            "the refusals queued a write"
        );
    }

    #[test]
    fn view_commands_and_wheel_reach_only_the_focused_viewport() {
        let (handle, _state) = fake_tall(&[]);
        let mut app = commits_app(&handle);
        app.draw();
        // Both tenants long: the diff loaded, the list a hundred deep.
        app.dispatch("commits.open-diff");
        app.dispatch("commits.focus");
        app.draw();

        // Under the commits focus, `view.down` and the wheel move the commits
        // viewport and touch nothing else.
        let (dc, dt) = (diff_of(&app).cursor(), diff_of(&app).top());
        app.dispatch("view.down");
        assert_eq!(commits_of(&app).cursor(), 1);
        assert_eq!((diff_of(&app).cursor(), diff_of(&app).top()), (dc, dt));
        app.input(Input::Key(Key::plain(Code::WheelDown)));
        assert!(
            commits_of(&app).top() > 0,
            "the wheel did not scroll the list"
        );
        assert_eq!(diff_of(&app).top(), dt);

        // Under the diff focus, the same commands move the diff alone.
        app.dispatch("diff.focus");
        let (cc, ct) = (commits_of(&app).cursor(), commits_of(&app).top());
        app.dispatch("view.down");
        assert_eq!(diff_of(&app).cursor(), dc + 1);
        assert_eq!(
            (commits_of(&app).cursor(), commits_of(&app).top()),
            (cc, ct)
        );
        app.input(Input::Key(Key::plain(Code::WheelDown)));
        assert!(
            diff_of(&app).top() > dt,
            "the wheel did not scroll the diff"
        );
        assert_eq!(commits_of(&app).top(), ct);

        // And the frame agrees: the unfocused pane draws no cursor bar, the
        // focused one exactly one.
        app.draw();
        let (w, h) = app.screen.size();
        let bar = app.host.theme.chrome.selection_bg;
        let lit = |x0: usize, x1: usize| {
            (1..h - 1)
                .filter(|y| (x0..x1).any(|x| app.screen.ink(x, *y).is_some_and(|i| i.bg == bar)))
                .count()
        };
        assert_eq!(lit(0, 40), 0, "the unfocused list drew a cursor bar");
        assert_eq!(
            lit(41, w),
            1,
            "the focused diff did not draw exactly one bar"
        );
    }

    #[test]
    fn mouse_down_focuses_the_hit_pane_and_drag_stays_captured() {
        let (handle, state) = fake_tall(&[]);
        let mut app = commits_app(&handle);
        app.draw();
        app.dispatch("commits.open-diff");
        app.dispatch("commits.focus");
        app.draw();

        // Down in the commits rectangle presses it, in its own coordinates.
        app.mouse(click(MouseKind::Down, 5, 4));
        assert_eq!(app.panes.focused_name(), "commits");
        assert_eq!(
            commits_of(&app).cursor(),
            2,
            "the press did not translate to pane-local rows"
        );

        // Down in the diff rectangle focuses the diff and presses *it*.
        app.mouse(click(MouseKind::Down, 60, 6));
        assert_eq!(app.panes.focused_name(), "diff");

        // A drag that crosses the divider back into the commits region is
        // still the diff's gesture: it selects in the diff, splices nothing
        // into the list, and the release reads the pane the button went
        // down in — not the one under the pointer when it came up.
        app.mouse(click(MouseKind::Drag, 10, 8));
        app.mouse(click(MouseKind::Up, 10, 8));
        assert_eq!(app.panes.focused_name(), "diff");
        assert!(
            !diff_of(&app).selection().is_empty(),
            "the drag never selected"
        );
        assert_eq!(
            commits_of(&app).selection(),
            "",
            "the gesture spliced two panes"
        );
        assert_eq!(
            app.copy.as_deref(),
            Some(diff_of(&app).selection().as_str()),
            "copy-on-select did not read the captured pane"
        );

        // A press on the diff's scrollbar: its own last column, not the
        // screen's, and the drag follows the thumb.
        let top = diff_of(&app).top();
        app.mouse(click(MouseKind::Down, 119, 5));
        app.mouse(click(MouseKind::Drag, 119, 12));
        assert!(diff_of(&app).top() > top, "the bar did not take the drag");
        app.mouse(click(MouseKind::Up, 119, 12));

        // Two quick clicks in the commits pane open the diff — the clock
        // counts, and the pane it counted in is part of what it counted.
        app.dispatch("commits.focus");
        let reads = state.lock().unwrap().pairs_reads;
        app.mouse(click(MouseKind::Down, 10, 4));
        app.mouse(click(MouseKind::Up, 10, 4));
        app.mouse(click(MouseKind::Down, 10, 4));
        assert_eq!(
            app.panes.focused_name(),
            "diff",
            "the double click did not open the diff"
        );
        assert_eq!(
            state.lock().unwrap().pairs_reads,
            reads + 1,
            "opened twice or never"
        );

        // The same cell under a different pane is not a double click: the
        // narrow layout puts the diff where the commits was, and the clock
        // counts per pane.
        app.screen.resize(60, 24);
        app.draw();
        app.mouse(click(MouseKind::Down, 10, 4));
        assert_eq!(app.clicks, 1, "the clock counted across panes");
        app.mouse(click(MouseKind::Up, 10, 4));
        assert_eq!(diff_of(&app).selection(), "", "a single click selected");
        // ...and a second quick click in the *same* pane is a double.
        app.mouse(click(MouseKind::Down, 10, 4));
        assert_eq!(app.clicks, 2);
        app.mouse(click(MouseKind::Up, 10, 4));
        assert!(
            !diff_of(&app).selection().is_empty(),
            "two clicks in one pane did not take a word"
        );
    }

    #[test]
    fn copy_on_select_finishes_once_in_the_captured_pane() {
        let (handle, _state) = fake(&[]);
        let mut app = commits_app(&handle);
        app.draw();
        app.dispatch("commits.open-diff");
        app.dispatch("commits.focus");
        app.draw();

        // A drag in the commits pane: the Up queues exactly its selection,
        // once, and the feedback counts lines.
        app.mouse(click(MouseKind::Down, 5, 4));
        app.mouse(click(MouseKind::Drag, 5, 7));
        app.mouse(click(MouseKind::Up, 5, 7));
        let commits_text = commits_of(&app).selection();
        assert!(
            !commits_text.is_empty(),
            "the drag in the list selected nothing"
        );
        assert_eq!(app.copy.as_deref(), Some(commits_text.as_str()));
        assert_eq!(
            copied(&commits_text),
            format!("copied {} lines", commits_text.lines().count())
        );

        // The same gesture in the diff: its Up queues the diff's text, not
        // the list's and not a splice of both.
        app.dispatch("diff.focus");
        app.mouse(click(MouseKind::Down, 60, 4));
        app.mouse(click(MouseKind::Drag, 60, 6));
        app.mouse(click(MouseKind::Up, 60, 6));
        let diff_text = diff_of(&app).selection();
        assert!(
            !diff_text.is_empty(),
            "the drag in the diff selected nothing"
        );
        assert_ne!(diff_text, commits_text);
        assert_eq!(app.copy.as_deref(), Some(diff_text.as_str()));

        // `copy.selection` reads the focused pane, with its keyboard
        // fallback — and what is queued is text only: the loop owns the
        // terminal, so nothing here emits an OSC byte.
        app.dispatch("copy.selection");
        assert_eq!(
            app.copy.as_deref(),
            Some(diff_of(&app).copy_text().as_str())
        );
    }

    #[test]
    fn shared_defaults_reach_terminal_dispatch() {
        // The two names exist under the shipped bindings, and the terminal
        // resolves them through the same builtin keymap every client reads.
        let mut modes = Modes::new();
        modes.push("diff");
        let keys = Host::new().keys;
        assert_eq!(
            keys.resolve(&modes, &[Key::plain(Code::Char(' '))]),
            Resolve::Run("diff.stage-hunk")
        );
        assert_eq!(
            keys.resolve(&modes, &[Key::plain(Code::Char('u'))]),
            Resolve::Run("diff.unstage-hunk")
        );

        // And the dispatch itself answers both, with no local key table
        // anywhere in this client: the refusal is the repository's, which is
        // what "the name reached the verb" looks like.
        let mut app = app_on_diff(
            Source::Repo {
                path: std::path::PathBuf::from("/fake"),
                arg: String::new(),
            },
            None,
        );
        app.press(Key::plain(Code::Char(' ')));
        assert_eq!(app.message, "no repository is open");
        app.press(Key::plain(Code::Char('u')));
        assert_eq!(app.message, "no repository is open");
        // A binding under `[keys.diff]` in `gitten.toml` rides the same path.
        app.host.keys.bind("diff", "p", "diff.stage-hunk").unwrap();
        app.press(Key::plain(Code::Char('p')));
        assert_eq!(app.message, "no repository is open");
    }

    #[test]
    fn non_working_tree_and_untracked_hunks_are_refused_before_submission() {
        // Every refusal below names itself in the window's words, and not
        // one of them reaches the queue.
        let said = |source: Source, repo: Option<Handle>, row: usize| {
            let mut app = app_on_diff(source, repo);
            move_to(&mut app, row);
            app.dispatch("diff.stage-hunk");
            (app.message.clone(), app)
        };

        let (message, _) = said(
            Source::Repo {
                path: std::path::PathBuf::from("/fake"),
                arg: "HEAD~1..HEAD".into(),
            },
            None,
            1,
        );
        assert_eq!(
            message,
            "only the working-tree diff can act on hunks — this one is between commits"
        );

        let (message, _) = said(Source::Fixtures, None, 1);
        assert_eq!(message, "a fixture has no repository behind it");

        let (message, _) = said(Source::Patch { file: None }, None, 1);
        assert_eq!(message, "a patch file has no repository behind it");

        let (message, _) = said(
            Source::Repo {
                path: std::path::PathBuf::from("/fake"),
                arg: String::new(),
            },
            None,
            1,
        );
        assert_eq!(message, "no repository is open");

        let (handle, state) = fake(&[]);
        let (message, _) = said(
            Source::Repo {
                path: std::path::PathBuf::from("/fake"),
                arg: String::new(),
            },
            Some(handle),
            0,
        );
        assert_eq!(message, "the keyboard is not on a hunk");
        assert!(
            state.lock().unwrap().writes.is_empty(),
            "a refusal queued a write"
        );

        // An untracked creation is refused by name — and the refusal names
        // the pane that serves whole-file verbs, because a patch cannot
        // carry the mode `git apply --cached` would need.
        let (handle, state) = fake(&["new.txt"]);
        let (message, _) = said(
            Source::Repo {
                path: std::path::PathBuf::from("/fake"),
                arg: String::new(),
            },
            Some(handle),
            1,
        );
        assert_eq!(
            message,
            "that hunk adds a new file — stage or unstage it whole from the files pane"
        );
        assert!(
            state.lock().unwrap().writes.is_empty(),
            "a refusal queued a write"
        );

        // The plausible wrong refusal: this hunk is *also* every line an
        // addition, but the file is tracked — `[diff] context = 0` makes a
        // mid-file insertion look exactly like a creation, and geometry
        // alone does not decide which it is.
        let (handle, state) = fake(&["new.txt"]);
        let (message, app) = said(
            Source::Repo {
                path: std::path::PathBuf::from("/fake"),
                arg: String::new(),
            },
            Some(handle),
            6,
        );
        assert!(message.is_empty(), "{message}");
        assert!(
            until(Duration::from_secs(2), || {
                !state.lock().unwrap().writes.is_empty()
            }),
            "the tracked insertion never reached the repository"
        );
        let writes = state.lock().unwrap().writes.clone();
        assert_eq!(writes.len(), 1, "{writes:?}");
        assert!(writes[0].starts_with("stage "), "{writes:?}");
        // The one that landed is the only write the whole table produced:
        // every refusal above left the queue untouched.
        assert!(
            app.submitter.submit(Box::new(Dead)).is_ok(),
            "the queue still runs"
        );
    }

    /// A job that does nothing, for probing the queue's liveness.
    struct Dead;
    impl Job for Dead {
        fn name(&self) -> &str {
            "dead"
        }
        fn run(self: Box<Self>) -> Result<(), String> {
            Ok(())
        }
    }

    #[test]
    fn staging_refreshes_focused_and_unfocused_panes() {
        let (handle, state) = fake(&[]);
        let started = gitten_app::Started {
            view: View::Commits,
            source: Source::Repo {
                path: std::path::PathBuf::from("/fake"),
                arg: String::new(),
            },
            host: Host::new(),
            loaded: acquire::Loaded {
                label: "fake".into(),
                data: Data::Commits(three_commits()),
            },
            config: std::path::PathBuf::from("/nonexistent/gitten.toml"),
            repo: Some(Arc::new(FakeRepo(Arc::clone(&state)))),
        };
        let mut app = App::new(started, Glyphs::default());
        app.screen = Screen::new(60, 24);
        // Open the diff, then put the keyboard back on the list: the commit
        // list is the focused pane and the diff is the registered one the
        // refresh must not forget — hidden by the narrow layout or not.
        app.dispatch("commits.open-diff");
        assert_eq!(app.panes.names().count(), 3, "open-diff appended a pane");
        assert!(matches!(app.panes.get("diff"), Some(Screens::Diff { .. })));
        app.dispatch("commits.focus");
        assert_eq!(app.panes.focused_name(), "commits");
        let open_reads = state.lock().unwrap().pairs_reads;

        // One job that lands and one that is refused — both finish, and
        // both finishes must stale all three panes, the stack included.
        let first = Write::stage_patch(&handle, b"first".to_vec()).expect("a non-empty patch");
        assert!(app.submitter.submit(Box::new(first)).is_ok(), "queued");
        let second = Write::stage_patch(&handle, b"refuse-me".to_vec()).expect("a non-empty patch");
        assert!(app.submitter.submit(Box::new(second)).is_ok(), "queued");
        let open_stash_reads = state.lock().unwrap().stash_reads;
        assert!(
            until(Duration::from_secs(2), || {
                app.drain_jobs();
                let s = state.lock().unwrap();
                s.log_reads >= 2
                    && s.pairs_reads >= open_reads + 2
                    && s.stash_reads >= open_stash_reads + 2
            }),
            "the queue never finished both jobs"
        );

        let s = state.lock().unwrap();
        // One re-acquire per pane per finish: the commit list is the focused
        // one, the diff and the stack the unfocused ones, and each was
        // refreshed exactly as often as the others.
        assert_eq!(s.log_reads, 2, "{}", s.writes.len());
        assert_eq!(s.pairs_reads, open_reads + 2, "{}", s.log_reads);
        assert_eq!(s.stash_reads, open_stash_reads + 2, "{}", s.log_reads);
        assert_eq!(s.writes.len(), 2, "{}", s.log_reads);
        // The refusal is the message; the success's evidence is the pane.
        assert_eq!(app.message, "the fake refused");
        // And the generation the queue advanced to is the one every pane
        // was refreshed against — a refusal's as much as a success's, the
        // focused pane's as much as the hidden one's.
        assert!(app.generation > Generation::default());
        for name in ["commits", "diff"] {
            let pane = app
                .panes
                .get(name)
                .unwrap_or_else(|| panic!("{name} is registered"));
            assert_eq!(pane.generation(), app.generation, "{name}");
        }
    }

    #[test]
    fn two_failing_panes_surface_the_first_one_s_error() {
        // Both panes stale, both re-acquisitions failing, each in its own
        // words: the message that stands is the *first* pane's — commits,
        // by registration order — and never whichever pane happened to fail
        // last. The registry made simultaneous failures ordinary, so the
        // contract "the first failure is remembered" is a test now.
        let (handle, state) = fake(&[]);
        let mut app = commits_app(&handle);
        // A second repository pane, so two refreshes run on one finish.
        app.dispatch("commits.open-diff");
        app.dispatch("commits.focus");
        state.lock().unwrap().fail_log = Some("the log read failed".into());
        state.lock().unwrap().fail_pairs = Some("the pairs read failed".into());

        assert!(app.submitter.submit(Box::new(Dead)).is_ok(), "queued");
        assert!(
            until(Duration::from_secs(2), || {
                app.drain_jobs();
                app.generation > Generation::default()
            }),
            "the finish was never drained"
        );
        assert_eq!(app.message, "the log read failed", "the last error stood");
        assert_ne!(app.message, "the pairs read failed");
    }

    #[test]
    fn a_refreshed_frame_is_drawable_headlessly() {
        let (handle, state) = fake(&[]);
        let source = Source::Repo {
            path: std::path::PathBuf::from("/fake"),
            arg: String::new(),
        };
        let mut app = app_on_fake(&source, &handle);
        // The keyboard is on the first hunk — the one about to be staged.
        move_to(&mut app, 2);
        let (path, hunk) = match app.panes.get("diff") {
            Some(Screens::Diff { view, .. }) => {
                view.current_hunk().expect("the keyboard is on a hunk")
            }
            _ => panic!("a diff is registered"),
        };
        assert_eq!(path, "f.txt");
        let patch = gitten_core::patch::emit(&path, &[&hunk]);
        // What the fake will be asked to apply is exactly the chosen hunk's
        // edit and not its distant neighbour's.
        let applied = parse_unified_diff(&String::from_utf8_lossy(&patch));
        let changed: Vec<&str> = applied[0]
            .hunks
            .iter()
            .flat_map(|h| &h.lines)
            .filter(|l| l.kind != gitten_core::LineKind::Context)
            .map(|l| l.text.as_ref())
            .collect();
        assert_eq!(changed, ["line 4", "EDIT ONE"]);

        app.dispatch("diff.stage-hunk");
        assert!(
            until(Duration::from_secs(2), || {
                app.drain_jobs();
                !state.lock().unwrap().writes.is_empty()
            }),
            "the staged hunk never reached the repository"
        );
        assert!(
            until(Duration::from_secs(2), || {
                app.drain_jobs();
                app.generation > Generation::default()
            }),
            "the finish was never drained"
        );

        // The frame the refreshed state produces: the staged hunk is gone
        // from the diff the fake now answers with, the surviving hunk is
        // still drawn, and the cursor is a row of the body and not of the
        // chrome.
        app.draw();
        let (w, h) = app.screen.size();
        let frame: Vec<String> = (0..h).map(|y| app.screen.row_text(y)).collect();
        assert!(
            frame.iter().any(|row| row.contains("EDIT TWO")),
            "the surviving hunk vanished: {frame:?}"
        );
        assert!(
            frame.iter().all(|row| !row.contains("EDIT ONE")),
            "a staged hunk is still on screen: {frame:?}"
        );
        let bar = app.host.theme.chrome.selection_bg;
        let lit: Vec<usize> = (0..h)
            .filter(|y| app.screen.ink(0, *y).is_some_and(|ink| ink.bg == bar))
            .collect();
        assert_eq!(lit.len(), 1, "{lit:?}");
        assert!(
            lit[0] >= 1 && lit[0] < h - 1,
            "the cursor lit the chrome, not the body: {lit:?}"
        );
        let _ = w;
    }

    #[test]
    fn repository_launch_registers_stashes_in_the_existing_ring() {
        // The shipped map already binds the digit and the three verbs: the
        // terminal adds no name, no alias and no key table anywhere in the
        // chain, and the focus key resolves through `Host::new().keys` — the
        // same map `gitten.toml` writes — exactly as it did before this pane
        // had a tenant.
        let keys = Host::new().keys;
        let mut commits = Modes::new();
        commits.push("commits");
        assert_eq!(
            keys.resolve(&commits, &[Key::plain(Code::Char('5'))]),
            Resolve::Run("stashes.focus")
        );
        let mut stashes_mode = Modes::new();
        stashes_mode.push("panes");
        stashes_mode.push("stashes");
        assert_eq!(
            keys.resolve(&stashes_mode, &[Key::char('g')]),
            Resolve::Run("stashes.pop"),
            "the mode override is the shared map's, not a terminal table"
        );

        // A repository-backed commits launch: three tenants, the stack in
        // its canonical sidebar slot between the list and the main pane —
        // both in the registration order `names` reports and the refresh
        // rail walks — and the requested startup focus restored over
        // whatever the registrations focused last.
        let (handle, _state) = fake(&[]);
        let mut app = commits_app(&handle);
        let names: Vec<&str> = app.panes.names().collect();
        assert_eq!(names, ["commits", "stashes", "diff"], "{names:?}");
        assert_eq!(app.panes.focused_name(), "commits");
        assert_eq!(app.panes.list_order(), ["commits", "stashes"]);
        assert_eq!(app.panes.reading_order(), ["commits", "stashes", "diff"]);

        // `5` reaches it — through the keymap, and the mode follows the
        // keyboard.
        app.press(Key::plain(Code::Char('5')));
        assert_eq!(app.panes.focused_name(), "stashes");
        assert_eq!(
            app.panes.focused().map(|pane| pane.mode()),
            Some("stashes"),
            "the keyboard is in stashes mode"
        );
        assert_eq!(
            app.panes.focused_placement(),
            Some(panes::Placement::Sidebar { rank: 4 }),
            "canonical rank 4, from the registry and not a layout edit"
        );

        // A repository-backed diff launch registers it the same way, and
        // the diff keeps the focus the launch asked for.
        let source = Source::Repo {
            path: std::path::PathBuf::from("/fake"),
            arg: String::new(),
        };
        let diff_app = app_on_fake(&source, &handle);
        assert_eq!(
            diff_app.panes.names().collect::<Vec<_>>(),
            ["stashes", "diff"]
        );
        assert_eq!(diff_app.panes.focused_name(), "diff");

        // No repository, no tenant: a fixture and a patch launch answer the
        // focus command with the same sentence an absent pane always got.
        for mut app in [
            app_on_diff(Source::Fixtures, None),
            app_on_diff(Source::Patch { file: None }, None),
        ] {
            assert!(
                app.panes.get("stashes").is_none(),
                "a repository-free launch invented a stash pane"
            );
            app.dispatch("stashes.focus");
            assert_eq!(app.message, "no stashes pane");
        }
    }

    #[test]
    fn wide_and_narrow_frames_place_the_stash_tenant_without_layout_edits() {
        let (handle, _state) = fake(&[]);
        let mut app = commits_app(&handle);
        app.draw();

        // Wide: the sidebar splits into two canonical slices — commits on
        // top, the stack at the foot — and the diff takes the rest, one
        // divider column between. No geometry module changed to make room:
        // this is the registry's equal-slice answer to a third tenant.
        assert_eq!(
            app.pane_rect("commits"),
            Some(crate::panes::Rect {
                x: 0,
                y: 1,
                width: 40,
                height: 11
            })
        );
        assert_eq!(
            app.pane_rect("stashes"),
            Some(crate::panes::Rect {
                x: 0,
                y: 12,
                width: 40,
                height: 11
            })
        );
        assert_eq!(
            app.pane_rect("diff"),
            Some(crate::panes::Rect {
                x: 41,
                y: 1,
                width: 79,
                height: 22
            })
        );

        // Headers name the live configured focus keys — 4 and 5, straight
        // out of the shipped map — and the stack says whose repository it
        // is and how much is parked.
        let commits_header = app.screen.row_text(1)[..40].to_string();
        assert!(
            commits_header.contains('4') && commits_header.contains("commits"),
            "{commits_header:?}"
        );
        let stashes_header = app.screen.row_text(12);
        assert!(stashes_header.contains('5'), "{stashes_header:?}");
        assert!(stashes_header.contains("stashes"), "{stashes_header:?}");
        assert!(
            stashes_header.contains("fake (main) · 2 parked"),
            "{stashes_header:?}"
        );

        // The divider is nobody's: blank from the top of the body to the
        // bottom, including across both sidebar headers.
        for y in 2..23 {
            assert_eq!(
                app.screen.char_at(40, y),
                Some(' '),
                "row {y} drew into the divider"
            );
        }

        // And the stack itself drew: both rows, address first.
        let rows: Vec<String> = (13..23).map(|y| app.screen.row_text(y)).collect();
        assert!(rows.iter().any(|r| r.contains("stash@{0}")), "{rows:?}");
        assert!(rows.iter().any(|r| r.contains("stash@{1}")), "{rows:?}");

        // Narrow: only the focused pane, at the full body. `5` is what both
        // focuses the stack and reveals it.
        app.screen.resize(80, 24);
        app.draw();
        assert_eq!(app.panes.focused_name(), "commits");
        assert!(
            app.pane_content("stashes").is_none(),
            "a hidden pane kept a rectangle"
        );
        app.press(Key::plain(Code::Char('5')));
        app.draw();
        assert_eq!(
            app.pane_content("stashes"),
            Some(crate::panes::Rect {
                x: 0,
                y: 2,
                width: 80,
                height: 21
            })
        );

        // The geometry cache still answers for its key: an unchanged frame
        // reuses the rectangles it cached, and only a size, registry or
        // focus change invalidates them.
        let cached = app.geometry.as_ref().map(|(k, _)| *k);
        app.draw();
        assert_eq!(app.geometry.as_ref().map(|(k, _)| *k), cached);
    }

    #[test]
    fn wave_one_and_plan_016_features_survive_stash_registration() {
        let (handle, state) = fake(&[]);
        let mut app = commits_app(&handle);
        app.draw();
        app.dispatch("commits.open-diff");
        app.draw();

        // Focus: the digits and the cycle still walk the ring, and the
        // title, header and status all follow the keyboard.
        app.press(Key::plain(Code::Char('4')));
        assert_eq!(app.panes.focused_name(), "commits");
        app.dispatch("pane.next");
        assert_eq!(
            app.panes.focused_name(),
            "stashes",
            "the cycle did not reach the second list"
        );
        app.dispatch("pane.next");
        assert_eq!(
            app.panes.focused_name(),
            "commits",
            "the cycle did not wrap"
        );
        app.press(Key::plain(Code::Char('5')));
        assert_eq!(app.panes.focused_name(), "stashes");
        app.draw();
        assert!(
            app.screen.row_text(0).contains("stashes"),
            "the title did not follow: {:?}",
            app.screen.row_text(0)
        );
        assert!(
            app.screen.row_text(12).contains('5') && app.screen.row_text(12).contains("stashes"),
            "the header did not advertise the stack: {:?}",
            app.screen.row_text(12)
        );
        assert!(
            app.screen
                .row_text(23)
                .contains("stashes · 1/2 · stash@{0}"),
            "the status did not follow: {:?}",
            app.screen.row_text(23)
        );

        // Search still targets the commit list — by the pane's own name,
        // not by whoever holds the keyboard. The expected count is a direct
        // filter's answer, so the assertion is about routing and not about
        // re-deriving the search index here.
        app.press(Key::plain(Code::Char('4')));
        app.press(Key::char('/'));
        assert!(app.search.is_some(), "the prompt did not open");
        type_(&mut app, "commit 9");
        app.press(Key::plain(Code::Enter));
        let mut direct = Commits::new(hundred_commits());
        direct.apply_query("commit 9");
        let filtered = match app.panes.get("commits") {
            Some(Screens::Commits { view, .. }) => view.filter_note(),
            _ => panic!("the commits pane is registered"),
        };
        assert_eq!(
            filtered,
            direct.filter_note(),
            "search did not reach the list"
        );
        assert!(filtered.is_some(), "the query filtered nothing");

        // Hunk verbs still route to the diff pane and its own gate — not to
        // the pane that holds the keyboard, and not to the stack.
        app.press(Key::plain(Code::Char('0')));
        app.dispatch("diff.stage-hunk");
        assert_eq!(
            app.message,
            "only the working-tree diff can act on hunks — this one is between commits"
        );
        assert!(
            state.lock().unwrap().stash_writes.is_empty(),
            "the hunk verb reached the stack"
        );

        // Mouse capture: a drag in the commit list selects its rows and its
        // release reads that pane; a press in the stack's slice moves the
        // keyboard there, and a drag inside the stack builds no selection —
        // a stack is acted on one entry at a time.
        app.dispatch("commits.focus");
        app.mouse(click(MouseKind::Down, 5, 4));
        app.mouse(click(MouseKind::Drag, 5, 7));
        app.mouse(click(MouseKind::Up, 5, 7));
        assert!(
            !commits_of(&app).selection().is_empty(),
            "the drag in the list selected nothing"
        );
        app.mouse(click(MouseKind::Down, 5, 15));
        assert_eq!(
            app.panes.focused_name(),
            "stashes",
            "the press did not move the keyboard to the stack"
        );
        app.mouse(click(MouseKind::Up, 5, 15));
        app.mouse(click(MouseKind::Down, 5, 16));
        app.mouse(click(MouseKind::Drag, 5, 17));
        app.mouse(click(MouseKind::Up, 5, 17));
        assert_eq!(
            app.panes.get("stashes").map(|pane| pane.selection()),
            Some(String::new()),
            "a drag built a stash selection"
        );

        // A reload rebuilds the host and re-applies geometry to every placed
        // pane — the stack included — and keeps the focus where it was.
        let path =
            std::env::temp_dir().join(format!("gitten-tui-stash-{}.toml", std::process::id()));
        std::fs::write(&path, "[view]\nscrolloff = 1\n").expect("a config file");
        app.reload(&path);
        assert_eq!(
            app.panes.focused_name(),
            "stashes",
            "the reload moved the focus"
        );
        assert!(app.pane_content("stashes").is_some());
        std::fs::remove_file(&path).ok();

        // Narrow mode, stack focused: the hidden list and diff are stale
        // all the same when a job finishes.
        app.screen.resize(60, 24);
        app.draw();
        assert!(
            app.pane_content("commits").is_none(),
            "narrow mode showed the hidden list"
        );
        let reads = state.lock().unwrap().log_reads;
        let job = Write::stash_apply(&handle, 0);
        assert!(app.submitter.submit(Box::new(job)).is_ok(), "queued");
        assert!(
            until(Duration::from_secs(2), || {
                app.drain_jobs();
                state.lock().unwrap().log_reads > reads
            }),
            "the hidden tenants were not refreshed"
        );
        for name in ["commits", "stashes", "diff"] {
            assert_eq!(
                app.panes.get(name).unwrap().generation(),
                app.generation,
                "{name} was not refreshed to the generation"
            );
        }
    }
}
