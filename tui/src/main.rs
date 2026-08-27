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
//!                                                     Screen::run ────┘
//! ```
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
use gitten_core::command::{chord_string, Key, Modes, Resolve};
use gitten_core::differ::Overrides;
use gitten_core::host::Host;
use gitten_core::runs::Run;
use gitten_core::Hunk;
use gitten_tui::commits::{Commits, Glyphs};
use gitten_tui::diff::Diff;
use gitten_tui::help;
use gitten_tui::screen::{Ink, Pen, Screen};
use gitten_tui::scrollbar::Bar;
use gitten_tui::term::{Input, Mouse, MouseKind, Term};
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

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

/// What is on screen. A stack, so `esc` goes back to where you came from.
///
/// Each entry carries three things beside its view: the source it was acquired
/// from, the label it was acquired under, and the invalidation generation it
/// was acquired at. The first two are what a refresh re-reads and renames; the
/// third is what tells the two apart — a screen whose generation is behind the
/// job queue's is stale, and a fixture's never is, because no write anywhere
/// can stale it.
enum Screens {
    Commits {
        view: Commits,
        source: Source,
        label: String,
        generation: Generation,
    },
    Diff {
        view: Diff,
        source: Source,
        label: String,
        generation: Generation,
    },
}

impl Screens {
    /// Which mode's bindings are live. The name the keymap and `gitten.toml` use.
    fn mode(&self) -> &'static str {
        match self {
            Screens::Commits { .. } => "commits",
            Screens::Diff { .. } => "diff",
        }
    }

    fn source(&self) -> &Source {
        match self {
            Screens::Commits { source, .. } | Screens::Diff { source, .. } => source,
        }
    }

    fn label(&self) -> &str {
        match self {
            Screens::Commits { label, .. } | Screens::Diff { label, .. } => label,
        }
    }

    fn generation(&self) -> Generation {
        match self {
            Screens::Commits { generation, .. } | Screens::Diff { generation, .. } => *generation,
        }
    }

    /// Re-acquires this screen from the repository when a finished job has
    /// staled it, applying the result in place. `None` for a screen nothing
    /// can stale — one already at `target`, or one with no repository behind
    /// it, whose data no write anywhere can move. `Some(result)` otherwise,
    /// because a failed re-acquisition is a failed refresh and the caller
    /// has an error to keep.
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
        // The generation travels with the refresh: a screen that re-acquired
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
                Source::Repo { .. } => {
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
                Source::Fixtures | Source::Patch { .. } => None,
            },
        }
    }

    /// A new size — and, on the same call, the margin the config file asks for.
    ///
    /// Both per frame, because both are a comparison when nothing changed and
    /// because this is the one path that has the size *and* the live host. It is
    /// what makes `[view] scrolloff` land on the next frame rather than the next
    /// launch, like every other number in that file.
    fn resize(&mut self, cols: usize, rows: usize, host: &Host) {
        match self {
            Screens::Commits { view: c, .. } => {
                c.set_scrolloff(host.view.scrolloff);
                c.resize(cols, rows);
            }
            Screens::Diff { view: d, .. } => {
                d.set_scrolloff(host.view.scrolloff);
                d.resize(cols, rows, host);
            }
        }
    }

    fn paint(&self, screen: &mut Screen, top: usize, host: &Host, out: &mut Vec<Run>) {
        match self {
            Screens::Commits { view: c, .. } => c.paint(screen, top, host),
            Screens::Diff { view: d, .. } => d.paint(screen, top, host, out),
        }
    }

    fn status(&self, host: &Host) -> String {
        match self {
            Screens::Commits { view: c, .. } => c.status(),
            Screens::Diff { view: d, .. } => d.status(host),
        }
    }

    /// A press in the body, at `row` rows down it.
    ///
    /// The count and the modifier arrive as scalars rather than as an event
    /// type, which is what keeps the views free of `term` — a view takes
    /// already-hit-tested numbers exactly as it takes already-loaded data.
    fn press(&mut self, col: usize, row: usize, clicks: u8, extend: bool, host: &Host) {
        match self {
            Screens::Commits { view: c, .. } => c.press(col, row, extend, host),
            Screens::Diff { view: d, .. } => d.press(col, row, clicks, extend, host),
        }
    }

    /// The pointer moved with the button down. `row` is signed: a row above the
    /// body is negative and scrolls it.
    fn drag(&mut self, col: usize, row: isize, host: &Host) {
        match self {
            Screens::Commits { view: c, .. } => c.drag(row, host),
            Screens::Diff { view: d, .. } => d.drag(col, row, host),
        }
    }

    fn release(&mut self) {
        match self {
            Screens::Commits { view: c, .. } => c.release(),
            Screens::Diff { view: d, .. } => d.release(),
        }
    }

    /// What `copy.selection` copies here: the selection, or the row the cursor is
    /// on when there is none.
    fn copy_text(&self) -> String {
        match self {
            Screens::Commits { view: c, .. } => c.copy_text(),
            Screens::Diff { view: d, .. } => d.copy_text(),
        }
    }

    /// What the *mouse* has selected, and nothing else. Empty after a click, so
    /// copy-on-select can tell a gesture that selected something from one that
    /// only moved the cursor.
    fn selection(&self) -> String {
        match self {
            Screens::Commits { view: c, .. } => c.selection(),
            Screens::Diff { view: d, .. } => d.selection(),
        }
    }

    fn select_all(&mut self) {
        match self {
            Screens::Commits { view: c, .. } => c.select_all(),
            Screens::Diff { view: d, .. } => d.select_all(),
        }
    }

    fn select_none(&mut self) -> bool {
        match self {
            Screens::Commits { view: c, .. } => c.select_none(),
            Screens::Diff { view: d, .. } => d.select_none(),
        }
    }

    /// Runs a command, or says it does not know it.
    ///
    /// The `view.*` half is the same list for both screens and is what makes
    /// them bindable in [`gitten_core::command::GLOBAL`]: a key that scrolls one
    /// list scrolls every list, and nothing had to say so twice.
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
                // The terminal shows one screen at a time — there is no
                // second pane to walk to. The commands are still answered:
                // a key that resolves must not read as one that failed.
                "pane.left" | "pane.right" => {}
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
        }
        true
    }
}

struct App {
    host: Host,
    /// Where to acquire more from, for opening a commit's diff: the path the
    /// view is named after, and the one handle the startup opened, so every
    /// diff this process shows came through the same repository. `None` for a
    /// fixture, which has no repository behind it — and the key then does
    /// nothing, which is what an unbound key does too.
    repo: Option<(std::path::PathBuf, gitten_git::Handle)>,
    stack: Vec<Screens>,
    screen: Screen,
    modes: Modes,
    /// Keys typed so far that have not resolved to a command. Empty almost
    /// always; a chord is what puts something in it.
    pending: Vec<Key>,
    /// Something to say once, on the status line: an error, or what a key just
    /// did. Cleared by the next keypress, so it cannot go stale.
    message: String,
    /// The shared write queue. One FIFO worker, owned here, whose finishes
    /// every client treats the same way: a generation advances — a refusal as
    /// much as a success — and every repository-backed screen re-acquires.
    jobs: Runner,
    /// The cloneable end of [`App::jobs`], handed out to whatever submits.
    submitter: Submitter,
    /// The generation the queue has advanced to, and so the one every screen
    /// in the stack was last refreshed against.
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
    /// The last press, for counting a double click: when, and in which cell.
    clicked: Option<(Instant, usize, usize)>,
    clicks: u8,
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
        let screen = match started.loaded.data {
            Data::Commits(commits) => {
                let mut list = Commits::with_glyphs(commits, glyphs);
                list.set_bar(bar);
                Screens::Commits {
                    view: list,
                    source,
                    label,
                    generation: Generation::default(),
                }
            }
            Data::Diff(files) => {
                let mut diff = Diff::new(files, &host);
                diff.set_bar(bar);
                Screens::Diff {
                    view: diff,
                    source,
                    label,
                    generation: Generation::default(),
                }
            }
        };
        let jobs = Runner::new();
        let submitter = jobs.submitter();
        let mut app = Self {
            host,
            repo,
            stack: vec![screen],
            screen: Screen::new(0, 0),
            modes: Modes::new(),
            pending: Vec::new(),
            message: String::new(),
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
            clicked: None,
            clicks: 0,
        };
        app.sync_modes();
        app
    }

    /// The mode stack follows what is on screen. Rebuilt rather than pushed and
    /// popped in step with `stack`, because two things kept in step drift.
    fn sync_modes(&mut self) {
        self.modes = Modes::new();
        if let Some(screen) = self.stack.last() {
            self.modes.push(screen.mode());
        }
        if self.help {
            self.modes.push("help");
        }
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
                Some(Input::Key(key)) => self.press(key),
                Some(Input::Mouse(m)) => self.mouse(m),
                Some(Input::Resize(w, h)) => {
                    size = (w, h);
                    self.screen.resize(w, h);
                }
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
        // The wrap and the layout are the view's own indices into registries
        // that may have changed shape; a resize re-applies them safely.
        let (w, h) = self.screen.size();
        for screen in &mut self.stack {
            screen.resize(w, h.saturating_sub(2), &self.host);
        }
    }

    /// One keypress.
    fn press(&mut self, key: Key) {
        self.message.clear();
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

    /// One mouse event.
    ///
    /// The whole of the routing, and it is short for the same reason [`press`]
    /// is: the rows this file owns are the title bar and the status line, so
    /// what is left is "which row of the body" and the view does the rest. The
    /// day there are panes, this is the function that grows a hit test and
    /// nothing else does.
    fn mouse(&mut self, m: Mouse) {
        let (_, h) = self.screen.size();
        // The help panel is drawn over the body, so a click that reached a view
        // through it would act on a row it is hiding.
        if h < 3 || self.help {
            return;
        }
        let body = 1..h - 1;
        // Signed, and not clamped: a drag above the body is a negative row and
        // scrolls it, which is what dragging past the top of a page does.
        let row = m.row as isize - body.start as isize;
        match m.kind {
            MouseKind::Down => {
                if !body.contains(&m.row) {
                    return;
                }
                self.message.clear();
                let clicks = self.count(m.col, m.row);
                // Disjoint field borrows, as everywhere else in this file: the
                // host is read while a screen is written, and moving it out and
                // back would rebuild every theme and every registry per click.
                let Self { stack, host, .. } = self;
                let Some(screen) = stack.last_mut() else {
                    return;
                };
                screen.press(m.col, row as usize, clicks, m.shift, host);
                // Two clicks on a commit open it, which is the one gesture a
                // terminal has for "go in" besides the key that already does.
                if clicks == 2 && matches!(self.stack.last(), Some(Screens::Commits { .. })) {
                    self.open_diff();
                }
            }
            MouseKind::Drag => {
                let Self { stack, host, .. } = self;
                if let Some(screen) = stack.last_mut() {
                    screen.drag(m.col, row, host);
                }
            }
            MouseKind::Up => {
                if let Some(screen) = self.stack.last_mut() {
                    screen.release();
                }
                // Copy-on-select, and this is the only place it can be: a
                // selection is finished when the button comes up, and writing
                // one to the terminal per motion event would be an escape
                // sequence per cell the pointer crossed.
                if self.host.mouse.copy_on_select {
                    let text = self
                        .stack
                        .last()
                        .map(Screens::selection)
                        .unwrap_or_default();
                    if !text.is_empty() {
                        self.copy = Some(text);
                    }
                }
            }
        }
    }

    /// How many times this cell has been clicked in quick succession.
    ///
    /// Ours to count because the protocol does not carry it — see [`DOUBLE`].
    /// Capped at three: nothing means more than a row, and an uncapped counter
    /// would make a fourth click mean something a third did not.
    fn count(&mut self, col: usize, row: usize) -> u8 {
        let now = Instant::now();
        let again = self
            .clicked
            .is_some_and(|(at, c, r)| (c, r) == (col, row) && now.duration_since(at) < DOUBLE);
        self.clicks = match again {
            true => (self.clicks + 1).min(3),
            false => 1,
        };
        self.clicked = Some((now, col, row));
        self.clicks
    }

    /// A command name into an effect.
    ///
    /// The client's own commands first, then the screen's. That order is what
    /// lets a screen override `back` one day without this file having to know.
    fn dispatch(&mut self, command: &str) {
        match command {
            "quit" => self.quit = true,
            "help" => {
                self.help = !self.help;
                self.sync_modes();
            }
            "back" => self.back(),
            // The whole window's, so it is here and not on a screen — and the
            // name is said, because a palette that changed without saying which
            // one it is now leaves you cycling to find out.
            "theme.cycle" => {
                self.host.cycle_theme();
                self.picked_theme = Some(self.host.theme.name.clone());
                self.message = format!("theme: {}", self.host.theme.name);
            }
            "commits.open-diff" => self.open_diff(),
            // The hunk verbs act on the *repository*, not the screen: they
            // need the source the diff was acquired from and the handle it
            // was acquired through, and a view is drawing and input only.
            // Routed here, ahead of the screen, for the same reason the
            // window routes them in its `run_command`.
            "diff.stage-hunk" | "diff.unstage-hunk" => self.hunk_verb(command),
            // The clipboard is the terminal's, not this process's — see
            // `Term::copy`. Held until the loop, which is the one place that has
            // a terminal to write to.
            "copy.selection" => {
                let text = self
                    .stack
                    .last()
                    .map(Screens::copy_text)
                    .unwrap_or_default();
                match text.is_empty() {
                    true => self.message = "nothing to copy".into(),
                    false => self.copy = Some(text),
                }
            }
            "select.all" => {
                if let Some(screen) = self.stack.last_mut() {
                    screen.select_all();
                }
            }
            "select.none" => {
                if let Some(screen) = self.stack.last_mut() {
                    screen.select_none();
                }
            }
            // Disjoint field borrows rather than moving the host out and
            // back: `Host::new()` rebuilds every theme, every registry and the
            // whole resolved contrast table, and doing that per keypress is a
            // thing that would never have shown up in a timing.
            _ => {
                let known = match self.stack.last_mut() {
                    Some(screen) => screen.run(command, &self.host),
                    None => false,
                };
                if !known {
                    self.message = format!("{command} does nothing here");
                }
            }
        }
    }

    /// Closes the help, or leaves the innermost screen.
    ///
    /// One key for both, because both are "get me out of this" and a reader does
    /// not distinguish them. The last screen is never popped: `esc` on the thing
    /// you started with is not a quit, and a client that vanished on it would be
    /// a client you could not trust the key in.
    fn back(&mut self) {
        // Innermost first, and a selection is inside a screen: `esc` drops what
        // the mouse is holding before it leaves the diff the mouse was holding
        // it in. One key, one direction, no order to remember.
        if self.help {
            self.help = false;
        } else if self.stack.last_mut().is_some_and(Screens::select_none) {
            // There was a selection and it is gone; that is the whole of this
            // `esc`, and the screen underneath stays where it is.
        } else if self.stack.len() > 1 {
            self.stack.pop();
            let (w, h) = self.screen.size();
            if let Some(screen) = self.stack.last_mut() {
                screen.resize(w, h.saturating_sub(2), &self.host);
            }
        }
        self.sync_modes();
    }

    /// Opens the diff of the commit under the cursor, on top of the list.
    ///
    /// The I/O is here and not in the view, which is the same rule the GPUI
    /// client follows: a view takes already-loaded data and never learns what a
    /// repository is. A bare revision is "what did this commit change" to
    /// [`gitten_git::Repo::pairs`], merges included.
    fn open_diff(&mut self) {
        let Some(Screens::Commits { view: list, .. }) = self.stack.last() else {
            self.message = "no commit selected".into();
            return;
        };
        let Some(commit) = list.current() else { return };
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
                let mut diff = Diff::new(
                    match loaded.data {
                        Data::Diff(files) => files,
                        Data::Commits(_) => return,
                    },
                    &self.host,
                );
                diff.set_bar(self.bar);
                let (w, h) = self.screen.size();
                diff.resize(w, h.saturating_sub(2), &self.host);
                self.stack.push(Screens::Diff {
                    view: diff,
                    source,
                    label: format!("{} {subject}", &sha[..sha.len().min(8)]),
                    // Acquired this instant, so it is as current as the
                    // queue's last finish — not a generation older.
                    generation: self.generation,
                });
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
    fn hunk_verb(&mut self, command: &str) {
        let source = match self.stack.last() {
            Some(screen) => screen.source().clone(),
            None => return,
        };
        let hunk = match self.stack.last() {
            Some(Screens::Diff { view, .. }) => view.current_hunk(),
            _ => None,
        };
        // Everything decided ahead of anything queued: a refusal is said
        // here, and the queue only ever sees a job that means it.
        let handle = self.repo.as_ref().map(|(_, handle)| handle);
        match hunk_action(command, &source, handle, hunk) {
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
    /// and re-acquires **every** stale repository-backed screen in the stack,
    /// the hidden ones included: a commit list under the diff being staged
    /// into is as stale as the diff itself. The write's own error is the
    /// message, with at most one refresh failure appended; every screen is
    /// still attempted even after one of them fails.
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

    /// Re-acquires every screen in the stack a finished job has staled.
    ///
    /// Synchronous, on the terminal loop — the accepted tradeoff: `git apply`
    /// itself ran on the shared worker above, and a second terminal background
    /// protocol is not this plan's scope. The screen stays drawn while it
    /// blocks; a measured window refresh of the same work runs 48–370 ms.
    fn refresh_stale(&mut self, target: Generation) -> Result<(), String> {
        let Some((_, repo)) = self.repo.clone() else {
            return Ok(());
        };
        // Every screen, not only the one on top — and every screen *tried*,
        // even after one of them fails: the first failure is remembered, the
        // rest are not skipped, because a stale hidden screen is still stale.
        let mut first = None;
        for screen in &mut self.stack {
            if let Some(result) = screen.refresh(target, &self.host, repo.as_ref()) {
                if result.is_err() {
                    first = result.err().or(first);
                }
            }
        }
        first.map_or(Ok(()), Err)
    }

    /// A title row, the screen, a status row.
    ///
    /// Two rows of chrome and everything else given to the view, because the
    /// view is the reason the window is open. The title says what you are
    /// looking at; the status says where you are in it.
    fn draw(&mut self) {
        let (w, h) = self.screen.size();
        if w == 0 || h < 3 {
            return;
        }
        let c = self.host.theme.chrome;
        self.screen.clear(Ink::new(c.dim, c.bg));
        let body = h - 2;

        title(
            &mut self.screen.row(0),
            &self.host,
            self.stack.last().map(Screens::label).unwrap_or(""),
            self.stack.last().map(Screens::mode),
        );

        if let Some(screen) = self.stack.last_mut() {
            screen.resize(w, body, &self.host);
        }
        if let Some(screen) = self.stack.last() {
            screen.paint(&mut self.screen, 1, &self.host, &mut self.runs);
        }

        let status = match self.message.is_empty() {
            true => self
                .stack
                .last()
                .map(|s| s.status(&self.host))
                .unwrap_or_default(),
            false => self.message.clone(),
        };
        let ink = Ink::new(c.dim, c.status_bg);
        let loud = Ink::new(c.accent, c.status_bg);
        // The previous frame's cost, not this one's — this one has not been
        // drawn yet, and a number measured after the fact would be describing a
        // frame nobody saw.
        let cost = match self.stats {
            Some((took, cells)) => format!(" · {took:.0?} · {cells} cells"),
            None => String::new(),
        };
        {
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

/// Whether to report what a frame cost. `GITTEN_STATS=0` turns it off, so
/// `./dev` can set it and a caller can still say no — the same rule the window's
/// overlay follows.
fn stats_on() -> bool {
    std::env::var("GITTEN_STATS").is_ok_and(|v| v != "0")
}

/// The title row: what you are looking at, and what would change it.
fn title(pen: &mut Pen, host: &Host, label: &str, mode: Option<&str>) {
    let c = &host.theme.chrome;
    let ink = Ink::new(c.fg, c.title_bg);
    let dim = Ink::new(c.dim, c.title_bg);
    pen.put(" ", ink);
    pen.put("gitten", Ink::new(c.accent, c.title_bg).bold());
    pen.put("  ", dim);
    if let Some(mode) = mode {
        pen.put(mode, ink);
        pen.put("  ", dim);
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use gitten_core::command::Code;
    use gitten_core::parse_unified_diff;
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
            Ok(three_commits())
        }

        fn pairs(&self, _revspec: &str) -> gitten_git::Result<Vec<Pair>> {
            let mut s = self.0.lock().unwrap();
            s.pairs_reads += 1;
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

    /// The fake's working-tree world: one file, both edits, untracked list
    /// as given. OIDs are `None` — a worktree pair never caches, so no test
    /// ever reads a neighbour's answer.
    fn fake(untracked: &[&str]) -> (Handle, Arc<Mutex<FakeState>>) {
        let state = Arc::new(Mutex::new(FakeState {
            before: vec![pair("f.txt", side(0), side(3))],
            after: vec![pair("f.txt", side(0), side(2))],
            refuses: vec![b"refuse".to_vec()],
            untracked: untracked.iter().map(|u| u.as_bytes().to_vec()).collect(),
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

        let (handle, _state) = fake(&[]);
        let (message, _) = said(
            Source::Repo {
                path: std::path::PathBuf::from("/fake"),
                arg: String::new(),
            },
            Some(handle),
            0,
        );
        assert_eq!(message, "the keyboard is not on a hunk");

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
    fn every_finished_generation_refreshes_both_stacked_screens() {
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
        // Open the diff: the stack is a commit list with a diff on top of
        // it, and the commit list is the hidden one the refresh must not
        // forget.
        app.dispatch("commits.open-diff");
        assert_eq!(app.stack.len(), 2);
        assert!(matches!(app.stack[0], Screens::Commits { .. }));
        assert!(matches!(app.stack[1], Screens::Diff { .. }));
        let open_reads = state.lock().unwrap().pairs_reads;

        // One job that lands and one that is refused — both finish, and
        // both finishes must stale the whole stack.
        let first = Write::stage_patch(&handle, b"first".to_vec()).expect("a non-empty patch");
        assert!(app.submitter.submit(Box::new(first)).is_ok(), "queued");
        let second = Write::stage_patch(&handle, b"refuse-me".to_vec()).expect("a non-empty patch");
        assert!(app.submitter.submit(Box::new(second)).is_ok(), "queued");
        assert!(
            until(Duration::from_secs(2), || {
                app.drain_jobs();
                let s = state.lock().unwrap();
                s.log_reads >= 2 && s.pairs_reads >= open_reads + 2
            }),
            "the queue never finished both jobs"
        );

        let s = state.lock().unwrap();
        // One re-acquire per screen per finish: the commit list is the
        // hidden screen, and it was refreshed exactly as often as the diff.
        assert_eq!(s.log_reads, 2, "{}", s.writes.len());
        assert_eq!(s.pairs_reads, open_reads + 2, "{}", s.log_reads);
        assert_eq!(s.writes.len(), 2, "{}", s.log_reads);
        // The refusal is the message; the success's evidence is the screen.
        assert_eq!(app.message, "the fake refused");
        // And the generation the queue advanced to is the one every screen
        // was refreshed against — a refusal's as much as a success's.
        assert!(app.generation > Generation::default());
        for screen in &app.stack {
            assert_eq!(screen.generation(), app.generation);
        }
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
        let (path, hunk) = {
            let Some(Screens::Diff { view, .. }) = app.stack.last() else {
                panic!("a diff is on top");
            };
            view.current_hunk().expect("the keyboard is on a hunk")
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
}
