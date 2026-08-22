//! `plait-tui` — plait in the terminal you started it from.
//!
//! The assembly, and deliberately thin: arguments, `plait.toml` and acquisition
//! are `plait_app`; the views are `plait_tui`; which command a key runs is
//! `plait_core::command`. What is left here is a loop.
//!
//! # Nothing in this file decides what a key does
//!
//! It reads one, asks the keymap what it means, and calls a method named by the
//! answer. The keymap is on `Host`, so `plait.toml` and an extension reach it
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
//! `plait.toml` is noticed. Nothing redraws at rest — the same property the GPUI
//! client has for free, arrived at here on purpose, and the reason the frame
//! timing in `docs/measurements.md` is measured rather than observed.

use plait_app::acquire::{self, Data};
use plait_app::cli::{self, Source, View};
use plait_app::Startup;
use plait_core::command::{chord_string, Key, Modes, Resolve};
use plait_core::host::Host;
use plait_core::runs::Run;
use plait_tui::commits::{Commits, Glyphs};
use plait_tui::diff::Diff;
use plait_tui::help;
use plait_tui::screen::{Ink, Pen, Screen};
use plait_tui::term::{Input, Term};
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const EXTRA: &str = "  --ascii        draw the graph without box-drawing characters
  --no-mouse     leave the wheel to the terminal, and drag-select with it

  `?` lists every key, from the same keymap `plait.toml` writes. Colours and the
  keymap are re-read every time the file is saved.
";


/// How often the loop wakes to notice a saved config file.
///
/// A save is a human action and 150 ms of latency is imperceptible; polling a
/// flag rather than plumbing a channel is what the GPUI client does too, for the
/// same reason. It costs one `poll` syscall per interval and no redraw.
const TICK: Duration = Duration::from_millis(150);

fn main() {
    let mut start = Startup::new("plait-tui", View::Commits)
        .blurb("plait in the terminal you started it from")
        .extra(EXTRA);
    let glyphs = match cli::take_switch(start.take(), "--ascii") {
        true => Glyphs::ascii(),
        false => Glyphs::default(),
    };
    let mouse = !cli::take_switch(start.take(), "--no-mouse");

    let started = match start.go() {
        Ok(started) => started,
        Err(exit) => exit.finish(),
    };
    let config_path = started.config.clone();
    let mut app = App::new(started, glyphs);

    // The panic hook before the terminal is touched: a panic between the two
    // would leave raw mode on with nothing to restore it.
    Term::guard();
    let mut term = match Term::enter(mouse) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("plait-tui: could not take the terminal: {e}");
            std::process::exit(1);
        }
    };

    // The watcher's callback runs on its own thread, so it only sets a flag.
    let dirty = Arc::new(AtomicBool::new(false));
    let watcher = {
        let dirty = dirty.clone();
        plait_app::config::watch(&config_path, move || dirty.store(true, Ordering::Relaxed)).ok()
    };
    // Held for as long as the loop runs: dropping a watcher stops it watching,
    // silently, which is a good way to lose an afternoon.
    let _watcher = watcher;

    if let Err(e) = app.run(&mut term, &dirty, &config_path) {
        term.leave();
        eprintln!("plait-tui: {e}");
        std::process::exit(1);
    }
    // Explicitly, before anything is printed: `Drop` would do it, but not until
    // after a `println!` had gone to the alternate screen.
    term.leave();
}

/// What is on screen. A stack, so `esc` goes back to where you came from.
enum Screens {
    Commits(Commits),
    Diff(Diff),
}

impl Screens {
    /// Which mode's bindings are live. The name the keymap and `plait.toml` use.
    fn mode(&self) -> &'static str {
        match self {
            Screens::Commits(_) => "commits",
            Screens::Diff(_) => "diff",
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
            Screens::Commits(c) => {
                c.set_scrolloff(host.view.scrolloff);
                c.resize(cols, rows);
            }
            Screens::Diff(d) => {
                d.set_scrolloff(host.view.scrolloff);
                d.resize(cols, rows, host);
            }
        }
    }

    fn paint(&self, screen: &mut Screen, top: usize, host: &Host, out: &mut Vec<Run>) {
        match self {
            Screens::Commits(c) => c.paint(screen, top, host),
            Screens::Diff(d) => d.paint(screen, top, host, out),
        }
    }

    fn status(&self, host: &Host) -> String {
        match self {
            Screens::Commits(c) => c.status(),
            Screens::Diff(d) => d.status(host),
        }
    }

    /// Runs a command, or says it does not know it.
    ///
    /// The `view.*` half is the same list for both screens and is what makes
    /// them bindable in [`plait_core::command::GLOBAL`]: a key that scrolls one
    /// list scrolls every list, and nothing had to say so twice.
    fn run(&mut self, command: &str, host: &Host) -> bool {
        match self {
            Screens::Commits(c) => match command {
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
            Screens::Diff(d) => match command {
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
    /// Where to acquire more from, for opening a commit's diff. `None` for a
    /// fixture, which has no repository behind it — and the key then does
    /// nothing, which is what an unbound key does too.
    repo: Option<std::path::PathBuf>,
    stack: Vec<Screens>,
    label: String,
    screen: Screen,
    modes: Modes,
    /// Keys typed so far that have not resolved to a command. Empty almost
    /// always; a chord is what puts something in it.
    pending: Vec<Key>,
    /// Something to say once, on the status line: an error, or what a key just
    /// did. Cleared by the next keypress, so it cannot go stale.
    message: String,
    help: bool,
    quit: bool,
    /// The theme `theme.cycle` picked, if anything has. `None` means the file's.
    picked_theme: Option<String>,
    /// What the last frame cost, when `PLAIT_STATS` is set.
    ///
    /// Two numbers and no overlay: how long the draw took, and how many cells
    /// reached the terminal. The second is the one worth watching — a scroll is
    /// a screenful and a cursor move should be a handful, and a number that is
    /// always the whole grid means something is repainting ink it did not need
    /// to. `PLAIT_STATS=1` and the same "0 is off" rule as the window.
    stats: Option<(Duration, usize)>,
    /// The run-list buffer, owned across frames so drawing allocates nothing.
    runs: Vec<Run>,
}

impl App {
    fn new(started: plait_app::Started, glyphs: Glyphs) -> Self {
        let repo = match &started.source {
            Source::Repo { path, .. } => Some(path.clone()),
            Source::Fixtures => None,
        };
        let label = started.loaded.label.clone();
        let host = started.host;
        let screen = match started.loaded.data {
            Data::Commits(commits) => Screens::Commits(Commits::with_glyphs(commits, glyphs)),
            Data::Diff(files) => Screens::Diff(Diff::new(files, &host)),
        };
        let mut app = Self {
            host,
            repo,
            stack: vec![screen],
            label,
            screen: Screen::new(0, 0),
            modes: Modes::new(),
            pending: Vec::new(),
            message: String::new(),
            help: false,
            quit: false,
            picked_theme: None,
            stats: None,
            runs: Vec::new(),
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
    ) -> io::Result<()> {
        let mut size = (0, 0);
        while !self.quit {
            let now = Term::size();
            if now != size {
                size = now;
                self.screen.resize(size.0, size.1);
            }
            let t = Instant::now();
            self.draw();
            let cells = self.screen.flush(term.out())?;
            if stats_on() {
                self.stats = Some((t.elapsed(), cells));
            }

            match Term::poll(TICK)? {
                Some(Input::Key(key)) => self.press(key),
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
        let mut warnings = plait_app::config::load(&mut next, path);
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
            true => "plait.toml reloaded".into(),
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
                let unknown = plait_core::command::chord_string(&self.pending);
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
        if self.help {
            self.help = false;
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
    /// `plait_git::pairs`, merges included.
    fn open_diff(&mut self) {
        let Some(Screens::Commits(list)) = self.stack.last() else {
            self.message = "no commit selected".into();
            return;
        };
        let Some(commit) = list.current() else { return };
        let (sha, subject) = (commit.sha.clone(), commit.subject.clone());
        let Some(repo) = self.repo.clone() else {
            self.message = "a fixture has no repository to diff against".into();
            return;
        };
        let source = Source::Repo { path: repo, arg: sha.clone() };
        match acquire::acquire(View::Diff, &source, &self.host) {
            Ok(loaded) => {
                let mut diff = Diff::new(match loaded.data {
                    Data::Diff(files) => files,
                    Data::Commits(_) => return,
                }, &self.host);
                let (w, h) = self.screen.size();
                diff.resize(w, h.saturating_sub(2), &self.host);
                self.stack.push(Screens::Diff(diff));
                self.label = format!("{} {subject}", &sha[..sha.len().min(8)]);
                self.sync_modes();
            }
            Err(e) => self.message = e,
        }
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
            &self.label,
            self.stack.last().map(Screens::mode),
        );

        if let Some(screen) = self.stack.last_mut() {
            screen.resize(w, body, &self.host);
        }
        if let Some(screen) = self.stack.last() {
            screen.paint(&mut self.screen, 1, &self.host, &mut self.runs);
        }

        let status = match self.message.is_empty() {
            true => self.stack.last().map(|s| s.status(&self.host)).unwrap_or_default(),
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
            let at = w.saturating_sub(plait_tui::screen::width(&pending) + 1);
            let mut pen = self.screen.span(h - 1, at, w - at);
            pen.put(&pending, loud);
            pen.wash(ink);
        }

        if self.help {
            help::paint(&mut self.screen, 1, body, &self.host, &self.modes);
        }
    }
}

/// Whether to report what a frame cost. `PLAIT_STATS=0` turns it off, so
/// `./dev` can set it and a caller can still say no — the same rule the window's
/// overlay follows.
fn stats_on() -> bool {
    std::env::var("PLAIT_STATS").is_ok_and(|v| v != "0")
}

/// The title row: what you are looking at, and what would change it.
fn title(pen: &mut Pen, host: &Host, label: &str, mode: Option<&str>) {
    let c = &host.theme.chrome;
    let ink = Ink::new(c.fg, c.title_bg);
    let dim = Ink::new(c.dim, c.title_bg);
    pen.put(" ", ink);
    pen.put("plait", Ink::new(c.accent, c.title_bg).bold());
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
    let pad = pen.room().saturating_sub(plait_tui::screen::width(&hint));
    pen.fill(pad, ' ', dim);
    pen.put(&hint, dim);
    pen.wash(dim);
}
