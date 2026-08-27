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
use gitten_app::{StartClock, Startup};
use gitten_core::command::{chord_string, Code, Key, Modes, Resolve};
use gitten_core::host::Host;
use gitten_core::runs::Run;
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
enum Screens {
    Commits(Commits),
    Diff(Diff),
}

/// The mode a text field owns the keyboard in — the name the keymap and
/// `gitten.toml` use, and the same name the window's input module holds. While
/// the search prompt stands, bindings are resolved against exactly this mode
/// and nothing else.
const INPUT: &str = "input";

impl Screens {
    /// Which mode's bindings are live. The name the keymap and `gitten.toml` use.
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

    /// A press in the body, at `row` rows down it.
    ///
    /// The count and the modifier arrive as scalars rather than as an event
    /// type, which is what keeps the views free of `term` — a view takes
    /// already-hit-tested numbers exactly as it takes already-loaded data.
    fn press(&mut self, col: usize, row: usize, clicks: u8, extend: bool, host: &Host) {
        match self {
            Screens::Commits(c) => c.press(col, row, extend, host),
            Screens::Diff(d) => d.press(col, row, clicks, extend, host),
        }
    }

    /// The pointer moved with the button down. `row` is signed: a row above the
    /// body is negative and scrolls it.
    fn drag(&mut self, col: usize, row: isize, host: &Host) {
        match self {
            Screens::Commits(c) => c.drag(row, host),
            Screens::Diff(d) => d.drag(col, row, host),
        }
    }

    fn release(&mut self) {
        match self {
            Screens::Commits(c) => c.release(),
            Screens::Diff(d) => d.release(),
        }
    }

    /// What `copy.selection` copies here: the selection, or the row the cursor is
    /// on when there is none.
    fn copy_text(&self) -> String {
        match self {
            Screens::Commits(c) => c.copy_text(),
            Screens::Diff(d) => d.copy_text(),
        }
    }

    /// What the *mouse* has selected, and nothing else. Empty after a click, so
    /// copy-on-select can tell a gesture that selected something from one that
    /// only moved the cursor.
    fn selection(&self) -> String {
        match self {
            Screens::Commits(c) => c.selection(),
            Screens::Diff(d) => d.selection(),
        }
    }

    fn select_all(&mut self) {
        match self {
            Screens::Commits(c) => c.select_all(),
            Screens::Diff(d) => d.select_all(),
        }
    }

    fn select_none(&mut self) -> bool {
        match self {
            Screens::Commits(c) => c.select_none(),
            Screens::Diff(d) => d.select_none(),
        }
    }

    /// The live filter count, while a search prompt stands over a list. A diff
    /// has nothing to count, and a note is only drawn when there is one.
    fn filter_note(&self) -> Option<String> {
        match self {
            Screens::Commits(c) => c.filter_note(),
            Screens::Diff(_) => None,
        }
    }

    /// Runs a command, or says it does not know it.
    ///
    /// The `view.*` half is the same list for both screens and is what makes
    /// them bindable in [`gitten_core::command::GLOBAL`]: a key that scrolls one
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
                // The terminal shows one screen at a time — there is no
                // second pane to walk to. The commands are still answered:
                // a key that resolves must not read as one that failed.
                "pane.left" | "pane.right" => {}
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

/// One edit to the open query, already reduced to text by whoever routed the
/// event: a character typed, or a character removed. The prompt never sees an
/// event, so a key is either claimed by a binding or it is text.
enum Edit {
    Char(char),
    /// Backspace or Delete — both remove backwards, because a status-line
    /// prompt has no cursor position to delete from.
    Backspace,
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
    label: String,
    screen: Screen,
    modes: Modes,
    /// Keys typed so far that have not resolved to a command. Empty almost
    /// always; a chord is what puts something in it.
    pending: Vec<Key>,
    /// Something to say once, on the status line: an error, or what a key just
    /// did. Cleared by the next keypress, so it cannot go stale.
    message: String,
    /// The open search prompt, holding the query typed so far. `None` while the
    /// keyboard belongs to the screens.
    ///
    /// Here and not on a screen because collecting terminal text is input — the
    /// client's to gather, by the same rule that makes it the client's to
    /// translate a platform event — while [`Commits::apply_query`] stays the
    /// whole of what a view knows about it. It stands only over
    /// [`Screens::Commits`]; every reader below can rely on that.
    search: Option<String>,
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
                Screens::Commits(list)
            }
            Data::Diff(files) => {
                let mut diff = Diff::new(files, &host);
                diff.set_bar(bar);
                Screens::Diff(diff)
            }
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
            search: None,
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
        // Above whatever screen it stands over: the prompt is the innermost
        // thing on screen while it is open, and it is what the help panel
        // should be listing bindings for.
        if self.search.is_some() {
            self.modes.push(INPUT);
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
    /// it live. Only over the list — a diff has no query, and says so.
    fn begin_search(&mut self) {
        let Some(Screens::Commits(list)) = self.stack.last() else {
            self.message = "commits.search is not supported here".into();
            return;
        };
        let standing = list.query().unwrap_or_default().to_string();
        self.search = Some(standing);
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
        let Self { stack, .. } = self;
        if let Some(Screens::Commits(list)) = stack.last_mut() {
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
    /// The whole of the routing, and it is short for the same reason [`press`]
    /// is: the rows this file owns are the title bar and the status line, so
    /// what is left is "which row of the body" and the view does the rest. The
    /// day there are panes, this is the function that grows a hit test and
    /// nothing else does.
    fn mouse(&mut self, m: Mouse) {
        let (_, h) = self.screen.size();
        // The help panel and the search prompt are drawn over the body, so a
        // click that reached a view through either would act on a row it is
        // hiding. The keyboard gathers the query; the mouse waits.
        if h < 3 || self.help || self.search.is_some() {
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
                if clicks == 2 && matches!(self.stack.last(), Some(Screens::Commits(_))) {
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
            // The prompt's three names, and the whole of what a search is:
            // open it, accept it, cancel it. Each resolves through the live
            // keymap — `commits.search` in the commits mode, the other two in
            // `input` while the prompt stands — so `gitten.toml` moves them
            // the way it moves everything else.
            "commits.search" => self.begin_search(),
            "input.accept" => self.finish_search(true),
            "input.cancel" => self.finish_search(false),
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
        let Some(Screens::Commits(list)) = self.stack.last() else {
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
                if let Some(note) = self.stack.last().and_then(Screens::filter_note) {
                    pen.put(" · ", ink);
                    pen.put(&note, Ink::new(c.faint, c.status_bg));
                }
            }
            pen.wash(ink);
        } else {
            let status = match self.message.is_empty() {
                true => self
                    .stack
                    .last()
                    .map(|s| s.status(&self.host))
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

    /// The commits list, which every search test is about.
    fn list(app: &App) -> &Commits {
        match app.stack.last() {
            Some(Screens::Commits(list)) => list,
            _ => panic!("the commits list is not on screen"),
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
}
