mod config;
mod controls;
mod dispatch;
mod graph;
mod help;
mod session;
mod stats;
mod views;

use gpui::*;
use gpui_component::*;
use plait_app::acquire::Data;
use plait_app::cli::{Source, View};
use plait_app::jobs::{Event as JobEvent, Generation, Job, Runner, Submitter};
use plait_app::{Started, Startup};
use plait_core::command::{chord_string, Code, Key, Modes, Resolve};
use plait_core::differ::{Overrides, Whitespace};
use plait_core::host::Host;
use plait_core::theme::Rgb;
use plait_core::FileDiff;
use stats::Stats;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Startup-stage timestamps on stderr, behind `PLAIT_START_LOG=1`.
///
/// Time-to-first-frame hides between stages nobody measures: acquisition and
/// the config were timed, the GPUI window path never was, so a slow launch
/// could not be attributed to anything. Every mark prints cumulative
/// milliseconds since the top of [`main`] and the step since the previous
/// mark, which is what makes a jittery macOS launch readable — three runs of
/// one number mean nothing; three runs of a table do.
///
/// Off by default, and the off path is one relaxed load per mark across about
/// a dozen marks in a launch. The first mark also pins `T0`, so nothing before
/// it is miscounted.
mod start {
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
    use std::sync::OnceLock;
    use std::time::Instant;

    static T0: OnceLock<Instant> = OnceLock::new();
    static LAST_US: AtomicU64 = AtomicU64::new(0);

    pub fn on() -> bool {
        static ON: OnceLock<bool> = OnceLock::new();
        *ON.get_or_init(|| std::env::var_os("PLAIT_START_LOG").is_some_and(|v| v != "0"))
    }

    /// Pins the epoch. Later than process start by exec and dyld, which no
    /// user-space code here can move.
    pub fn begin(now: Instant) {
        _ = T0.set(now);
    }

    pub fn mark(stage: &str) {
        if !on() {
            return;
        }
        let now = Instant::now();
        let t0 = *T0.get_or_init(|| now);
        let us = now.duration_since(t0).as_micros() as u64;
        let prev = LAST_US.swap(us, Relaxed);
        eprintln!(
            "[start] {:>8.3}ms  (+{:>8.3}ms)  {stage}",
            us as f64 / 1e3,
            (us - prev) as f64 / 1e3,
        );
    }
}

#[global_allocator]
static ALLOC: stats::Counting = stats::Counting;

// The three keys that stay GPUI actions, and why: they are the platform's.
// Cmd-Q quits whatever Mac app you are in, Cmd-C and Cmd-A are what the Edit
// menu exists for, and a Mac user's fingers already know all three. The menu
// items below carry them; their handlers call [`DevShell::run_command`] with
// the *named* commands every other door uses — `quit`, `copy.selection`,
// `select.all` — so a menu item is an adapter and not a second path.
actions!(plait, [Quit, CopySelection, SelectAll]);

/// The title strip, which is also the window's titlebar — see the note on
/// [`window_options`]. Tall enough to hold the traffic lights with the same air
/// above and below them, which is what makes it read as one band rather than as
/// a toolbar bolted under a titlebar.
const TITLE_H: f32 = 32.0;
/// Where the traffic lights start, and therefore how much room they need. macOS
/// draws three 12px buttons with ~8px between them, so they end around 62; the
/// title begins after them.
const LIGHTS_X: f32 = 10.0;
const LIGHTS_W: f32 = 72.0;
/// The error band under the title bar. Shorter than a title bar because it is
/// one sentence, and it is only there when something has to be said.
const BAND_H: f32 = 22.0;

/// What only this client has. The two views, the arguments and `plait.toml` are
/// documented once, in `plait_app::cli::usage`, because they are the same in
/// every client — see that function for why that is a promise and not a
/// convenience.
const EXTRA: &str =
    "  The title bar carries five pickers: the presentation (unified, side-by-side),
  where a line too wide for the window breaks (off, word, char), the diff
  algorithm (histogram, patience, myers), how much whitespace has to match
  (exact, trailing, change, all — git's default, --ignore-space-at-eol, -b and
  -w) and the theme (dark, light, slate, and whatever plait.toml adds). `s`
  cycles the presentation, `w` the wrap and `T` the theme — all three through
  `[keys]` in plait.toml, where `?` lists everything.

  The file is re-read every time it is saved, and colours and font apply on the
  next frame — no rebuild, no relaunch.

  ./dev.sh <args>  rebuild and relaunch on every source change, landing back
                   on the row you were reading. Debug build and the overlay by
                   default; pass --release before trusting a timing.

  PLAIT_STATS=1   frame, row and heap overlay
";

/// How to acquire the diff again under different overrides.
///
/// A closure and not a repository, because the shell does no I/O and must not
/// learn what one is beyond the single operation it names: the repository path
/// is captured here in `main`, the revision comes *in* — the startup one, or a
/// commit whose diff was opened — and the live [`Host`] is passed in rather
/// than captured, so a config reload cannot leave a stale registry behind it.
///
/// `None` means nothing on screen can be re-diffed — a `.diff` fixture was
/// diffed by somebody else — and the control is drawn inert.
type Rediff = Rc<dyn Fn(&Host, &Overrides, &str) -> Result<Vec<FileDiff>, String>>;

/// Which picker is open. At most one, because two open menus over a diff is two
/// things to dismiss.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Open {
    Theme,
    Layout,
    Wrap,
    Algorithm,
    Whitespace,
}

/// What is on screen. A stack, so `esc` goes back to where you came from.
///
/// **This is the typed adapter between dispatch and drawing.** [`Modes`] name
/// what is live, [`Keymap::resolve`] names what a key meant, and the match on
/// that name lives in exactly one place — [`DevShell::run_command`]. What lands
/// here is everything a *screen* can be asked to do, as methods, so no command
/// decision ever has to reach into drawing code to find out what is showing.
#[derive(Clone)]
enum Screen {
    Commits {
        view: Entity<views::commits::Commits>,
        source: Source,
        generation: Generation,
        label: String,
    },
    Diff {
        view: Entity<views::diff::Diff>,
        source: Source,
        generation: Generation,
        label: String,
    },
}

impl Screen {
    fn any(&self) -> AnyView {
        match self {
            Screen::Commits { view, .. } => view.clone().into(),
            Screen::Diff { view, .. } => view.clone().into(),
        }
    }

    /// Which mode's bindings are live. The name the keymap and `plait.toml` use.
    fn mode(&self) -> &'static str {
        match self {
            Screen::Commits { .. } => "commits",
            Screen::Diff { .. } => "diff",
        }
    }

    fn label(&self) -> String {
        match self {
            Screen::Commits { label, .. } => label.clone(),
            Screen::Diff { label, .. } => label.clone(),
        }
    }

    fn source(&self) -> &Source {
        match self {
            Screen::Commits { source, .. } | Screen::Diff { source, .. } => source,
        }
    }

    fn generation(&self) -> Generation {
        match self {
            Screen::Commits { generation, .. } | Screen::Diff { generation, .. } => *generation,
        }
    }

    fn view_kind(&self) -> View {
        match self {
            Screen::Commits { .. } => View::Commits,
            Screen::Diff { .. } => View::Diff,
        }
    }

    fn replace(
        &mut self,
        loaded: plait_app::acquire::Loaded,
        generation: Generation,
        host: &Host,
        cx: &mut App,
    ) -> Result<(), String> {
        match (self, loaded.data) {
            (
                Screen::Commits {
                    view,
                    label,
                    generation: loaded_generation,
                    ..
                },
                Data::Commits(commits),
            ) => {
                view.update(cx, |view, cx| {
                    view.replace(commits, host);
                    cx.notify();
                });
                *label = loaded.label;
                *loaded_generation = generation;
                Ok(())
            }
            (
                Screen::Diff {
                    view,
                    label,
                    generation: loaded_generation,
                    ..
                },
                Data::Diff(files),
            ) => {
                view.update(cx, |view, cx| view.replace(files, host, cx));
                *label = loaded.label;
                *loaded_generation = generation;
                Ok(())
            }
            _ => Err("re-acquisition returned the wrong view".into()),
        }
    }

    /// The box this screen's row list occupies, for hit-testing a wheel event.
    fn list_bounds(&self, cx: &App) -> Bounds<Pixels> {
        match self {
            Screen::Commits { view, .. } => view.read(cx).list_bounds(),
            Screen::Diff { view, .. } => view.read(cx).list_bounds(),
        }
    }

    /// Moves this screen's text sideways, where it has any — a commit graph has
    /// nothing off the left edge to reach, and says so by not moving. Whether
    /// anything moved decides a redraw.
    fn pan_pixels(&self, dx: f32, cx: &App) -> bool {
        match self {
            Screen::Commits { view, .. } => view.read(cx).pan_pixels(dx),
            Screen::Diff { view, .. } => view.read(cx).pan_pixels(dx),
        }
    }

    /// Runs one of the commands a screen owns: the `view.*` family both share
    /// and each screen's own additions. False is "not one of mine", and the
    /// caller says so — an unknown command that resolved is worth naming rather
    /// than swallowing.
    fn run(&self, command: &str, host: &Host, cx: &mut App) -> bool {
        match self {
            Screen::Commits { view, .. } => view.update(cx, |v, c| {
                let known = v.run_view(command, host);
                if known {
                    c.notify();
                }
                known
            }),
            Screen::Diff { view, .. } => view.update(cx, |d, c| {
                let known = d.run_view(command, host);
                if known {
                    c.notify();
                }
                known
            }),
        }
    }

    /// The wheel's smooth path: pixels into the list, in the direction the
    /// resolved command says and at whatever `[view] scroll` multiplies them
    /// by. The host rides along because the viewport's margin is live.
    fn scroll_pixels(&self, dy: f32, host: &Host, cx: &mut App) -> bool {
        match self {
            Screen::Commits { view, .. } => view.update(cx, |v, _| v.scroll_pixels(dy, host)),
            Screen::Diff { view, .. } => view.update(cx, |v, _| v.scroll_pixels(dy, host)),
        }
    }

    /// `select.all` / `select.none`, answered by whichever screen is up. A
    /// commit graph has no selection yet and answers no; a command nothing
    /// handles there is inert — the same answer an unbound key gives.
    fn select(&self, all: bool, cx: &mut App) -> bool {
        match self {
            Screen::Commits { view, .. } => view.update(cx, |v, _| match all {
                true => v.select_all(),
                false => v.select_none(),
            }),
            Screen::Diff { view, .. } => view.update(cx, |d, cx| match all {
                true => {
                    d.select_all(cx);
                    true
                }
                false => d.select_none(cx),
            }),
        }
    }
}

struct DevShell {
    /// The app half of the title, drawn bright: which program this is. Which
    /// *view* it is showing is the active screen's to say — see
    /// [`DevShell::active_view`] — because a commit list that opened a diff
    /// under the cursor is a diff on screen, whatever launched the window.
    which: &'static str,
    stack: Vec<Screen>,
    stats: Option<Stats>,
    /// How to fetch the diff again with a different algorithm. `None` for a
    /// `.diff` fixture, where there is no repository behind the rows at all.
    rediff: Option<Rediff>,
    /// The repository path used in labels and the persistent handle used for
    /// every acquisition. `None` for a fixture, which has no repository behind
    /// it — and the key then says so, which is what an unbound key does too.
    repo: Option<(std::path::PathBuf, plait_git::Handle)>,
    jobs: Runner,
    submitter: Submitter,
    generation: Generation,
    running: Option<String>,
    /// The live picks. Every field `None` means "whatever the config selected",
    /// which is what the controls show until somebody changes one — so the strip
    /// agrees with `plait.toml` rather than with a copy of it taken at startup.
    over: Overrides,
    open: Option<Open>,
    /// A failed re-diff. Shown, not swallowed: the usual cause is a repository
    /// that moved under the window, and silently keeping the old rows would be a
    /// diff labelled with an algorithm that did not produce it.
    error: Option<SharedString>,
    /// One sentence about what a key just did — an unbound chord, or a command
    /// that resolved to nothing this screen can do. Cleared by the next key,
    /// so it cannot go stale. Same band as [`DevShell::error`], which wins.
    notice: Option<String>,
    /// Where `plait.toml` is. Held because picking a theme goes through the same
    /// reload a save does — see [`config::reload`] for why there is only one
    /// path.
    config: std::path::PathBuf,
    /// Startup logging, and nothing else: whether [`start::mark`] has already
    /// stamped the first render. One bool read per frame afterwards.
    first_render: Cell<bool>,
    /// Which modes' bindings are live, innermost last: the screen's, then help
    /// over it. Rebuilt by [`DevShell::sync_modes`] whenever either changes.
    modes: Modes,
    /// Keys typed so far that have not resolved. Empty almost always; a chord
    /// is what puts something in it. One entry per press, and every entry
    /// carries **every spelling** that press could mean
    /// ([`dispatch::translate`]) — which of them runs is the keymap's
    /// `resolve_any` decision, made against the whole chord at once, so a
    /// half-typed `ß`/alt-s stays alive as both. Reset on every change of
    /// host, mode, focus, picker, help or screen — a pending chord is a
    /// promise about what is on screen, and none of those promises survive a
    /// change of any of it.
    pending: Vec<Vec<Key>>,
    help: bool,
    /// The window's one focusable element: this shell itself. Key events reach a
    /// listener through the focus path, so something has to hold focus, and one
    /// handle owned here means the views never have to know input exists.
    focus: FocusHandle,
    focused: Option<FocusHandle>,
    /// The host the last key was resolved against. A saved `plait.toml` swaps
    /// the map mid-session; a chord half-typed against the old one means nothing
    /// under the new.
    seen_host: Option<Rc<Host>>,
    /// Which axis the wheel gesture in flight belongs to. `gpui`'s own lock,
    /// held here — the one place that sees every wheel event first.
    ongoing: Cell<OngoingScroll>,
}

impl DevShell {
    /// The screen commands act on.
    fn active(&self) -> Option<&Screen> {
        self.stack.last()
    }

    /// What is drawn: the active screen's own view. Derived from the stack
    /// every time, never stored beside it — a second copy of an `AnyView` was
    /// exactly how the strip once kept saying "commits" over a diff, and two
    /// sources of truth disagree precisely when it matters.
    fn active_view(&self) -> AnyView {
        self.active()
            .expect("the bottom screen is never popped")
            .any()
    }

    /// The view name the title strip shows: the active screen's mode — which
    /// is also the name `[keys]` groups its bindings under. Falls back to what
    /// launched the window only if there were no screens at all, which does
    /// not happen; the fallback keeps the type honest rather than the UI.
    fn active_view_name(&self) -> &'static str {
        self.active().map_or(self.which, Screen::mode)
    }

    /// Rebuilds the mode stack from what is on screen, and drops whatever was
    /// pending against the previous arrangement: any half-typed chord, and any
    /// open picker — a menu belongs to the screen it was opened over, and one
    /// left standing after the screen changes is invisible but still in
    /// `self.open`, where [`DevShell::on_wheel`] swallows for it forever.
    /// Called on every change of screen or help state — the places
    /// [`Modes`] can change.
    fn sync_modes(&mut self) {
        self.modes = Modes::new();
        if let Some(screen) = self.active() {
            self.modes.push(screen.mode());
        }
        if self.help {
            self.modes.push("help");
        }
        self.pending.clear();
        self.open = None;
    }

    /// The live host, and the chord reset that goes with it when the file has
    /// been reloaded since the last key.
    fn fresh_host(&mut self, cx: &mut Context<Self>) -> Rc<Host> {
        let host = config::host(cx);
        let changed = match &self.seen_host {
            Some(seen) => !Rc::ptr_eq(seen, &host),
            None => true,
        };
        if changed {
            self.pending.clear();
            self.seen_host = Some(host.clone());
        }
        host
    }

    fn set_notice(&mut self, message: impl Into<String>) {
        self.notice = Some(message.into());
    }

    /// The one queue future built-ins and compiled-in extensions share.
    #[allow(dead_code)]
    fn submit(&self, job: Box<dyn Job>) -> Result<(), Box<dyn Job>> {
        self.submitter.submit(job)
    }

    fn drain_jobs(&mut self, cx: &mut Context<Self>) {
        let mut changed = false;
        while let Some(event) = self.jobs.try_next() {
            changed = true;
            match event {
                JobEvent::Started { name } => {
                    self.running = Some(format!("running {name}"));
                    self.error = None;
                }
                JobEvent::Finished {
                    outcome: Err(error),
                    ..
                } => {
                    self.running = None;
                    self.error = Some(error.into());
                }
                JobEvent::Finished {
                    outcome: Ok(generation),
                    ..
                } => {
                    self.running = None;
                    if generation > self.generation {
                        self.generation = generation;
                        self.refresh_active(cx);
                    }
                }
            }
        }
        if changed {
            cx.notify();
        }
    }

    fn refresh_active(&mut self, cx: &mut Context<Self>) {
        let Some(screen) = self.active() else { return };
        if screen.generation() >= self.generation || matches!(screen.source(), Source::Fixtures) {
            return;
        }
        let (view, source) = (screen.view_kind(), screen.source().clone());
        let Some((_, repo)) = self.repo.as_ref() else {
            return;
        };
        let host = config::host(cx);
        match plait_app::acquire::reacquire(view, &source, &host, Some(repo.as_ref()), &self.over) {
            Ok(loaded) => {
                // Re-check at the write itself. This remains correct if
                // acquisition becomes asynchronous later.
                let generation = self.generation;
                let Some(screen) = self.stack.last_mut() else {
                    return;
                };
                if screen.generation() >= generation {
                    return;
                }
                match screen.replace(loaded, generation, &host, cx) {
                    Ok(()) => self.error = None,
                    Err(error) => self.error = Some(error.into()),
                }
            }
            Err(error) => self.error = Some(error.into()),
        }
    }

    /// One of the platform's menu actions: named dispatch through
    /// [`DevShell::run_command`], with the pending chord cleared first — a menu
    /// item is an intervening event, and a chord is a promise about what is on
    /// screen that survives none of them.
    fn native(&mut self, command: &str, cx: &mut Context<Self>) {
        self.pending.clear();
        self.run_command(command, cx);
    }

    /// Re-acquires the diff under `next` and swaps it in.
    ///
    /// The whole cost is one acquisition plus one `prepare` — 40–120 ms and
    /// 8–250 ms respectively, on a click. Cheap enough not to need a spinner and
    /// not cheap enough to do on a keystroke repeat, which is why these are menus
    /// and only the layout is bound to a key.
    fn set_overrides(&mut self, next: Overrides, cx: &mut Context<Self>) {
        let Some(rediff) = self.rediff.clone() else {
            return;
        };
        let (view, revision) = match self.active() {
            Some(Screen::Diff {
                view,
                source: Source::Repo { arg, .. },
                ..
            }) => (view.clone(), arg.clone()),
            _ => return,
        };
        if next == self.over {
            return;
        }
        let host = config::host(cx);
        match rediff(&host, &next, &revision) {
            Ok(files) => {
                self.over = next;
                self.error = None;
                view.update(cx, |d, cx| d.replace(files, &host, cx));
                let load = view.read(cx).load.clone();
                if let Some(stats) = &mut self.stats {
                    stats.reloaded(load);
                }
            }
            // The old rows stay on screen, which is the right failure: they are
            // still a true diff, just not the one that was asked for.
            Err(e) => self.error = Some(e.into()),
        }
        cx.notify();
    }

    /// Costs no re-diff and no `prepare` — only where the lines break moves —
    /// which is why this one needs none of `set_overrides`' machinery.
    fn set_wrap(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(Screen::Diff { view, .. }) = self.active() else {
            return;
        };
        let view = view.clone();
        let host = config::host(cx);
        view.update(cx, |d, cx| d.set_wrap(index, &host, cx));
        cx.notify();
    }

    fn set_layout(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(Screen::Diff { view, .. }) = self.active() else {
            return;
        };
        let view = view.clone();
        // A layout change is a fresh look at the same diff, so a message about
        // an algorithm that failed to load is no longer describing the screen.
        self.error = None;
        let host = config::host(cx);
        view.update(cx, |d, cx| d.set_layout(index, &host, cx));
        let load = view.read(cx).load.clone();
        if let Some(stats) = &mut self.stats {
            stats.reloaded(load);
        }
        cx.notify();
    }

    /// Swaps the whole palette, by name.
    ///
    /// Through [`config::reload`] and not by editing the host in place, because
    /// there is no host to edit: it is an `Rc` every view is holding, replaced
    /// wholesale precisely so nobody can see half a theme. So a pick is a
    /// rebuild from the file with the pick applied on top — which is also what
    /// makes it survive the next save, and what makes a colour in `plait.toml`
    /// still count after one.
    fn set_theme(&mut self, name: String, cx: &mut Context<Self>) {
        cx.set_global(config::Chosen(Some(name)));
        for w in config::reload(&self.config, cx) {
            eprintln!("plait: {w}");
        }
        cx.notify();
    }

    /// `theme.cycle`. On the shell and not on a screen, because a palette is the
    /// window's — the commit graph is drawn out of the same one.
    fn cycle_theme(&mut self, cx: &mut Context<Self>) {
        let host = config::host(cx);
        let Some(next) = host.themes.after(&host.theme.name).map(|t| t.name.clone()) else {
            return;
        };
        drop(host);
        self.set_theme(next, cx);
    }

    /// One named command, run. **This is the one dispatch path**: the keymap
    /// resolves to a name, and whatever resolved it — a key, a chord, a wheel
    /// notch rebinding, a menu item — arrives here and nowhere else.
    ///
    /// The client's own commands first, then the screen's. That order is what
    /// lets a screen override `back` one day without this file having to know.
    fn run_command(&mut self, command: &str, cx: &mut Context<Self>) {
        match command {
            "quit" => cx.quit(),
            "help" => {
                self.help = !self.help;
                self.sync_modes();
            }
            "back" => self.back(cx),
            "theme.cycle" => self.cycle_theme(cx),
            "commits.open-diff" => self.open_diff(cx),
            "copy.selection" => self.copy_selection(cx),
            // Both are answered by whichever screen is up; a commit graph has no
            // selection yet, and a command nothing handles there is inert — the
            // same answer an unbound key gives.
            "select.all" | "select.none" => {
                if let Some(screen) = self.active() {
                    screen.select(command == "select.all", cx);
                }
            }
            _ => {
                let known = match self.active() {
                    Some(screen) => {
                        let host = config::host(cx);
                        screen.run(command, &host, cx)
                    }
                    None => false,
                };
                if !known {
                    // Resolvable, registered — and not implemented by anything
                    // this client ships. Said, not swallowed: an extension's
                    // command reaches this exact point without one line changing
                    // here, and the honest answer to it is a sentence.
                    self.set_notice(format!("{command} is not supported here"));
                }
            }
        }
        cx.notify();
    }

    /// Closes the help, or the picker over it, or leaves the innermost screen.
    ///
    /// One key for all of it, because all of it is "get me out of this" — and
    /// **innermost first**, or a picker left open after its screen is popped
    /// keeps occluding nothing: invisible, but still in `self.open`, where
    /// [`DevShell::on_wheel`] swallows every event for it forever. So an open
    /// menu is the whole of this `esc`: closed, pending dropped with it, no
    /// selection cleared and no screen popped. A selection is inside a screen,
    /// so it goes next; the first screen is never popped at all — `esc` on the
    /// thing you started with is not a quit.
    fn back(&mut self, cx: &mut Context<Self>) {
        if self.help {
            self.help = false;
            self.sync_modes();
            return;
        }
        if self.open.take().is_some() {
            // The theme's as much a picker as the rest: same key, same exit.
            self.pending.clear();
            cx.notify();
            return;
        }
        if let Some(screen) = self.active() {
            if screen.select(false, cx) {
                // There was a selection and it is gone; that is the whole of
                // this `esc`, and the screen underneath stays where it is.
                cx.notify();
                return;
            }
        }
        if self.stack.len() > 1 {
            // The stack is the source of truth; dropping the top *is* the
            // change. Everything derived from it — the view on screen, the
            // strip's name, the live modes — comes back out of it below.
            self.stack.pop();
            self.sync_modes();
            self.refresh_active(cx);
        }
    }

    /// Opens the diff of the commit under the cursor, on top of the list.
    ///
    /// The I/O is here and not in the view — the same rule the terminal follows:
    /// a view takes already-loaded data and never learns what a repository is.
    fn open_diff(&mut self, cx: &mut Context<Self>) {
        let Some(Screen::Commits { view, .. }) = self.active() else {
            self.set_notice("no commit selected");
            return;
        };
        let view = view.clone();
        let host = config::host(cx);
        // Meet the list where it actually is: a scrollbar drag moved the offset
        // without moving the cursor, and "open this commit" means the one being
        // *looked at*.
        view.update(cx, |v, _| v.reconcile(&host));
        let Some(commit) = view.read(cx).current().cloned() else {
            return;
        };
        let Some((path, repo)) = self.repo.clone() else {
            self.set_notice("a fixture has no repository to diff against");
            return;
        };
        let source = Source::Repo {
            path,
            arg: commit.sha.clone(),
        };
        match plait_app::acquire::acquire(View::Diff, &source, &host, Some(repo.as_ref())) {
            Ok(loaded) => {
                let Data::Diff(files) = loaded.data else {
                    return;
                };
                let view = cx.new(|cx| views::diff::Diff::new(files, host.clone(), cx));
                // The new diff was acquired with the file's own settings, so the
                // picks start from there too — a stale override would be a strip
                // describing an algorithm that did not produce this screen.
                self.over = Overrides::default();
                // A success says so: an error left up here would be describing
                // an open that already worked.
                self.error = None;
                let screen = Screen::Diff {
                    view,
                    source,
                    generation: self.generation,
                    label: format!(
                        "{} {}",
                        &commit.sha[..commit.sha.len().min(8)],
                        commit.subject
                    ),
                };
                // Pushed, and that is the whole of it — the view, the title's
                // middle word and the modes are all read back off the stack.
                self.stack.push(screen);
                self.sync_modes();
            }
            Err(e) => self.error = Some(e.into()),
        }
    }

    /// `copy.selection`: the mouse's selection, or the keyboard's row. The
    /// clipboard is the window system's here — [`Context::write_to_clipboard`]
    /// — which is why this lives beside dispatch and not in a view.
    fn copy_selection(&mut self, cx: &mut Context<Self>) {
        let host = config::host(cx);
        match self.active() {
            Some(Screen::Diff { view, .. }) => {
                let view = view.clone();
                // The row the copy falls back to is the cursor's, so the drag
                // has to be met where it left the list first.
                view.update(cx, |d, _| d.reconcile(&host));
                view.update(cx, |d, cx| d.copy(cx));
            }
            Some(Screen::Commits { view, .. }) => {
                let view = view.clone();
                view.update(cx, |v, _| v.reconcile(&host));
                let text = view.read(cx).cursor_text();
                if !text.is_empty() {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                }
            }
            None => {}
        }
    }

    /// One keypress, wherever it landed.
    ///
    /// Translation, resolution, dispatch — the whole of the input pipeline, and
    /// deliberately short: what a key *means* is the live keymap's answer, not
    /// this method's opinion. Anything consumed stops propagation, because the
    /// alternative is a second, hardcoded meaning firing behind it — which is
    /// precisely what this file used to have and must not again.
    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        // A pending chord is a promise about where the keyboard is; a focus
        // change breaks it. Cheap to check, never wrong.
        let now_focused = window.focused(cx);
        if self.focused != now_focused {
            self.focused = now_focused;
            self.pending.clear();
        }
        let candidates = dispatch::translate(&ev.keystroke);
        if candidates.is_empty() {
            return;
        }
        let host = self.fresh_host(cx);
        self.pending.push(candidates);
        // One candidate list per press, handed over whole: which spelling runs
        // is the map's decision, made against the chord at once.
        let typed: Vec<&[Key]> = self.pending.iter().map(Vec::as_slice).collect();
        match host.keys.resolve_any(&self.modes, &typed) {
            Resolve::Pending => {}
            Resolve::Run(name) => {
                let name = name.to_string();
                self.pending.clear();
                self.notice = None;
                cx.stop_propagation();
                cx.notify();
                self.run_command(&name, cx);
                return;
            }
            Resolve::None => {
                // Named by the spellings as they were typed — the insert when
                // there was one, the key underneath when there was not.
                let shown: Vec<Key> = self.pending.iter().map(|c| c[0]).collect();
                let unknown = chord_string(&shown);
                self.pending.clear();
                // Said, not swallowed: a key that does nothing and a key that is
                // not bound look identical, and only one of them is worth
                // opening `?` about.
                self.set_notice(format!("{unknown} is not bound"));
            }
        }
        cx.stop_propagation();
        cx.notify();
    }

    /// The pixels the smooth path feeds the list for a resolved wheel command.
    ///
    /// The **command** signs them, not the finger: a finger-flick away from you
    /// is positive, but `wheelup = "view.scroll-down"` means that flick scrolls
    /// *down* — so the resolved name flips it. `[view] scroll` multiplies. Any
    /// other command — a page, an extension's — is `None` here and goes through
    /// named dispatch instead; unbinding does nothing at all, exactly like a key.
    fn smooth_pixels(command: &str, dy: f32, rows: usize) -> Option<f32> {
        let px = dy.abs() * rows as f32;
        match command {
            "view.scroll-up" => Some(px),
            "view.scroll-down" => Some(-px),
            _ => None,
        }
    }

    /// One wheel event, wherever it rolled.
    ///
    /// Two rules, both inherited from the probe this replaced: the gesture has
    /// **one axis for its life** ([`views::diff::locked`], `gpui`'s own lock),
    /// and what is locked is decided *here*, in the capture phase, before the
    /// list's own scroll handler can turn a sideways flick into vertical
    /// movement.
    ///
    /// What changed with command dispatch is who owns the vertical half. The
    /// delta becomes a [`Code::WheelUp`] / [`Code::WheelDown`] and resolves
    /// through the same map as every other press — so `wheeldown = ""` really
    /// stops the wheel, `wheeldown = "view.page-down"` really pages, and what
    /// ships (`view.scroll-down`) moves the list by the event's own pixels,
    /// which is what keeps a trackpad smooth.
    fn on_wheel(&mut self, ev: &ScrollWheelEvent, window: &mut Window, cx: &mut Context<Self>) {
        // A wheel notch is an intervening event wherever it lands: a chord
        // half-typed when the fingers touch the wheel is not half-typed any
        // more. The same rule the focus and host checks apply, one line each.
        self.pending.clear();
        // Help is up, or a picker menu: their full-window occluding surfaces
        // keep the rows out of the hit path, while this capture interceptor
        // stands aside so a handler on the visible panel can still see the
        // event. Stopping propagation here would prevent that bubble handler.
        if self.help || self.open.is_some() {
            return;
        }
        // Over the rows, and not over the title bar or a dropdown above them. A
        // capture-phase handler is registered on the window, so it is outside
        // the hit test a bubble-phase one gets for free.
        let screen = match self.active() {
            Some(screen) => screen.clone(),
            None => return,
        };
        if !screen.list_bounds(cx).contains(&ev.position) {
            return;
        }
        let mut ongoing = self.ongoing.get();
        let delta = views::diff::locked(
            ev.delta.pixel_delta(window.line_height()),
            ev.modifiers.shift,
            &mut ongoing,
            ev.touch_phase,
        );
        self.ongoing.set(ongoing);

        // The horizontal axis is the text's, where the screen has text to move.
        let mut moved = false;
        if !delta.x.is_zero() {
            moved |= screen.pan_pixels(-f32::from(delta.x), cx);
        }
        // The vertical axis belongs to the keymap. What resolved decides *both*
        // halves of the motion: which way the list moves comes from the command
        // — `wheelup = "view.scroll-down"` really does scroll down — and how
        // far is the event's own pixels at `[view] scroll`'s multiplier, which
        // is what keeps a trackpad smooth. Any other command still dispatches
        // by name through the one path; an unbound or half-typed chord does
        // nothing, exactly like a key.
        if !delta.y.is_zero() {
            let host = self.fresh_host(cx);
            let key = Key::new(
                match f32::from(delta.y) > 0.0 {
                    true => Code::WheelUp,
                    false => Code::WheelDown,
                },
                ev.modifiers.control,
                ev.modifiers.alt,
                false,
            );
            // Unbound or half-typed: the wheel does nothing, which is what an
            // unbound key does too.
            if let Resolve::Run(name) = host.keys.resolve(&self.modes, &[key]) {
                let name = name.to_string();
                match Self::smooth_pixels(&name, f32::from(delta.y), host.view.rows) {
                    // The smooth path: the command came from `[keys]`, the
                    // distance from the finger.
                    Some(px) => moved |= screen.scroll_pixels(px, &host, cx),
                    _ => {
                        self.notice = None;
                        self.run_command(&name, cx);
                    }
                }
            }
        }
        // Ours either way. A gesture that unlocked mid-flick carries both axes
        // for the rest of its life, and letting one through would scroll the
        // list twice — once here, once natively.
        cx.stop_propagation();
        if moved {
            cx.notify();
        }
    }

    /// The pickers, right-aligned in the title bar. The theme is always one of
    /// them, because a palette is the window's; the other four drive the diff
    /// view and are drawn only when that is what is on screen — the commit graph
    /// has none of them to choose.
    ///
    /// Each one is the same shape: a list of names from a registry or an enum,
    /// and an index into it. That is why adding a presentation or an algorithm
    /// needs no work here.
    fn strip(&self, host: &Host, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let me = cx.entity().downgrade();

        // `Fn(bool)` per picker rather than one shared handler: which menu is
        // open is one field, and the closure is what knows which one it is. Both
        // halves reset pending chords: an open picker is a context, and keys
        // pressed into it start from nothing.
        let toggle = |which: Open| {
            let me = me.clone();
            move |next: bool, _: &mut Window, cx: &mut App| {
                _ = me.update(cx, |this, cx| {
                    this.open = next.then_some(which);
                    this.pending.clear();
                    cx.notify();
                });
            }
        };
        // Every override-driven pick is the same two lines: close, then re-diff.
        let pick_over = |build: fn(&Overrides, usize) -> Overrides| {
            let me = me.clone();
            move |i: usize, _: &mut Window, cx: &mut App| {
                _ = me.update(cx, |this, cx| {
                    this.open = None;
                    this.pending.clear();
                    let next = build(&this.over, i);
                    this.set_overrides(next, cx);
                });
            }
        };

        // Straight off the registry in `core`, so a theme an extension registers
        // — or one written in `plait.toml` — is in this menu without a line here
        // changing. Outside the `diff` check below on purpose: a palette is the
        // whole window's, and the commit graph is drawn out of the same one.
        let theme_names = host.themes.names();
        let themes = controls::Picker::new(
            "theme",
            &theme_names,
            host.themes.index_of(&host.theme.name).unwrap_or(0),
        );
        let theme_picker = controls::picker(
            "theme-picker",
            &themes,
            self.open == Some(Open::Theme),
            &host.theme,
            &host.font,
            toggle(Open::Theme),
            {
                let names: Vec<String> = theme_names.iter().map(|s| s.to_string()).collect();
                let me = me.clone();
                move |i, _, cx| {
                    let Some(name) = names.get(i).cloned() else {
                        return;
                    };
                    _ = me.update(cx, |this, cx| {
                        this.open = None;
                        this.pending.clear();
                        this.set_theme(name, cx);
                    });
                }
            },
        );

        // Everything below drives the diff view, so there is nothing to draw
        // when that is not what is on screen: the commit graph gets no strip of
        // dead controls.
        let Some(Screen::Diff { view, source, .. }) = self.active() else {
            return vec![theme_picker];
        };

        let names = view.read(cx).layout_names();
        let layouts = controls::Picker::new("layout", &names, view.read(cx).layout_index());

        // Straight off the registry in `core`, so a wrap an extension registers
        // is in this menu the day it exists.
        let wrap_names = view.read(cx).wrap_names(host);
        let wrap = controls::Picker::new("wrap", &wrap_names, view.read(cx).wrap_index());

        let algorithms = host.differ.names();
        let selected = self
            .over
            .algorithm
            .as_deref()
            .unwrap_or(host.differ.selected());
        let algorithm = controls::Picker::new(
            "algorithm",
            &algorithms,
            algorithms.iter().position(|n| *n == selected).unwrap_or(0),
        )
        .enabled(self.rediff.is_some() && matches!(source, Source::Repo { .. }));

        let ws_names: Vec<&str> = Whitespace::ALL.iter().map(|w| w.name()).collect();
        let ws = self.over.whitespace.unwrap_or(host.differ.whitespace);
        let whitespace = controls::Picker::new(
            "whitespace",
            &ws_names,
            Whitespace::ALL.iter().position(|w| *w == ws).unwrap_or(0),
        )
        .enabled(self.rediff.is_some() && matches!(source, Source::Repo { .. }));

        vec![
            controls::picker(
                "layout-picker",
                &layouts,
                self.open == Some(Open::Layout),
                &host.theme,
                &host.font,
                toggle(Open::Layout),
                {
                    let me = me.clone();
                    move |i, _, cx| {
                        _ = me.update(cx, |this, cx| {
                            this.open = None;
                            this.pending.clear();
                            this.set_layout(i, cx);
                        });
                    }
                },
            ),
            controls::picker(
                "wrap-picker",
                &wrap,
                self.open == Some(Open::Wrap),
                &host.theme,
                &host.font,
                toggle(Open::Wrap),
                {
                    let me = me.clone();
                    move |i, _, cx| {
                        _ = me.update(cx, |this, cx| {
                            this.open = None;
                            this.pending.clear();
                            this.set_wrap(i, cx);
                        });
                    }
                },
            ),
            controls::picker(
                "algorithm-picker",
                &algorithm,
                self.open == Some(Open::Algorithm),
                &host.theme,
                &host.font,
                toggle(Open::Algorithm),
                {
                    // The registry's own order, so an extension's differ is
                    // reachable here the day it is registered.
                    let names: Vec<String> = algorithms.iter().map(|s| s.to_string()).collect();
                    let me = me.clone();
                    move |i, _, cx| {
                        let Some(name) = names.get(i).cloned() else {
                            return;
                        };
                        _ = me.update(cx, |this, cx| {
                            this.open = None;
                            this.pending.clear();
                            let next = Overrides {
                                algorithm: Some(name),
                                ..this.over.clone()
                            };
                            this.set_overrides(next, cx);
                        });
                    }
                },
            ),
            controls::picker(
                "whitespace-picker",
                &whitespace,
                self.open == Some(Open::Whitespace),
                &host.theme,
                &host.font,
                toggle(Open::Whitespace),
                pick_over(|over, i| Overrides {
                    whitespace: Whitespace::ALL.get(i).copied(),
                    ..over.clone()
                }),
            ),
            theme_picker,
        ]
    }
}

// The `Screen` adapter above takes plain arguments so it reads as one thing;
// what follows is the shell itself.
impl Render for DevShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // One-shot: the first render of the shell is the first frame being
        // assembled, which is the end of what a startup measurement covers.
        if !self.first_render.replace(true) {
            start::mark("first render");
        }
        let overlay = self.stats.as_mut().map(|s| {
            s.tick();
            (s.frames(), s.rows(), s.heap(), s.load.clone())
        });
        if overlay.is_some() {
            window.request_animation_frame();
        }

        // The live host, read per frame, not the one captured when this was
        // built. It was the captured one, which meant the window chrome and the
        // font for the whole window silently did not hot-reload while every view
        // inside it did — the exact trap `docs/extending.md` warns about, in the
        // one place nobody looked.
        let host = config::host(cx);
        let c = host.theme.chrome;
        let f = &host.font;
        // Derived, not stored: what is on screen and what it is called are both
        // read back off the stack here, so opening a diff over the list and
        // coming back move the strip's middle word without a single line of
        // bookkeeping — and cannot fail to.
        let view = self.active_view();
        let which = self.active_view_name();
        let strip = self.strip(&host, cx);
        let error = self.error.clone();
        let notice = self.notice.clone();
        let running = self.running.clone();

        // The title is three things, so it is drawn as three: the app bright, the
        // view dim, the repository dimmer and shrinkable. One grey run of text
        // said none of that, and the separators are punctuation rather than
        // content — `faint` is where punctuation belongs.
        let dot = || div().flex_none().text_color(rgb(c.faint)).child("·");

        // The one focusable element in the window, and where key dispatch enters
        // it: a capture-phase listener on the root, so a keystroke is translated
        // and resolved *before* anything nested can give it a private meaning.
        // Taking focus on the first frame is what puts this element on the
        // dispatch path at all.
        if self.focused.is_none() {
            window.focus(&self.focus, cx);
            self.focused = Some(self.focus.clone());
        }
        let mut root = div()
            .id("shell")
            .size_full()
            .v_flex()
            .bg(rgb(c.bg))
            .text_color(rgb(c.fg))
            // From the host, not a constant: `text_sm` was `rems(0.875)` — 14px —
            // and the family was hardcoded here while three other things
            // depended on which font it was.
            .text_size(px(f.size))
            .font_family(f.family.clone())
            .track_focus(&self.focus)
            .capture_key_down(cx.listener(Self::on_key))
            // The menu's three adapters. Each calls the same named dispatch a
            // key resolves to — see the note on the `actions!` above.
            .on_action(cx.listener(|this, _: &Quit, _, cx| this.native("quit", cx)))
            .on_action(
                cx.listener(|this, _: &CopySelection, _, cx| this.native("copy.selection", cx)),
            )
            .on_action(cx.listener(|this, _: &SelectAll, _, cx| this.native("select.all", cx)));

        // The wheel, heard first: capture phase on a paint-time probe, the same
        // trick the diff view's old one used, moved up to where the mode stack
        // and the keymap live.
        {
            let me = cx.entity().downgrade();
            root = root.child(
                canvas(
                    |_, _, _| {},
                    move |_, _, window, _cx| {
                        let me = me.clone();
                        window.on_mouse_event(move |ev: &ScrollWheelEvent, phase, window, cx| {
                            if phase == DispatchPhase::Capture {
                                _ = me.update(cx, |this, cx| this.on_wheel(ev, window, cx));
                            }
                        });
                    },
                )
                .absolute()
                .top_0()
                .left_0()
                .h(px(0.)),
            );
        }

        root = root
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_2()
                    .h(px(TITLE_H))
                    // The window has no titlebar of its own any more, so the
                    // traffic lights are drawn *into* this strip and the title
                    // has to start after them.
                    .pl(px(LIGHTS_W))
                    .pr_3()
                    .bg(rgb(c.title_bg))
                    .border_b_1()
                    .border_color(rgb(c.border))
                    .text_color(rgb(c.dim))
                    .child(div().flex_none().text_color(rgb(c.fg)).child("plait"))
                    .child(dot())
                    .child(div().flex_none().child(which))
                    .child(dot())
                    // The one thing in the strip that is allowed to shrink, and
                    // everything else is `flex_none`. A repository is the part of
                    // a title a reader can reconstruct; a picker pushed off the
                    // right edge — which is what a strip of `flex_none` children
                    // and no `min_w_0` did — is a control that no longer exists.
                    .child(
                        div()
                            .flex_shrink(1.0)
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            // From the *start*: the label is a path and a
                            // revspec, and `…/git HEAD~2..HEAD` is the half worth
                            // keeping.
                            .text_ellipsis_start()
                            .child(self.active_label()),
                    )
                    .children(cfg!(debug_assertions).then(|| {
                        // One word and not the sentence this used to be — "DEBUG
                        // BUILD — timings meaningless, use --release" was fifty
                        // characters of a strip that has four controls in it, and
                        // the sentence belongs beside the numbers it is about,
                        // which is the stats overlay. No chip behind it either:
                        // every surface in this palette is within 1.05:1 of every
                        // other, so a filled chip is a rectangle nobody sees, and
                        // the accent alone is unmistakable.
                        div().flex_none().text_color(rgb(c.accent)).child("debug")
                    }))
                    // Pushes the controls to the right edge and takes the clicks
                    // that land between them, so a stray click on the title bar
                    // does not fall through to whatever is under it.
                    .child(div().flex_grow(1.0))
                    .children(strip),
            )
            // Its own band and not a word in the title bar, where it was: an
            // error is a whole sentence, the strip is already four controls and a
            // path, and `flex_none` on both meant a long one pushed the pickers
            // off the window rather than wrapping or truncating. An error wins
            // over a notice: it describes what failed, the notice describes what
            // was tried since.
            .children(
                error
                    .map(|e| band(&c, e, c.error))
                    .or_else(|| running.map(|n| band(&c, SharedString::from(n), c.dim)))
                    .or_else(|| notice.map(|n| band(&c, SharedString::from(n), c.dim))),
            )
            .child(div().flex_grow(1.0).overflow_hidden().child(view))
            // The menu itself is deferred at priority 1. Its transparent
            // priority-0 backdrop blocks the rest of the window without
            // covering the menu, so capture can leave overlay wheel ownership
            // alone without exposing the native list scroller underneath.
            .children(self.open.is_some().then(controls::picker_backdrop))
            .children(overlay.map(|(frames, rows, heap, load)| {
                div()
                    .flex_none()
                    .v_flex()
                    .px_4()
                    .py_2()
                    .gap_1()
                    .bg(rgb(c.status_bg))
                    .border_t_1()
                    .border_color(rgb(c.border))
                    .text_color(rgb(c.dim))
                    .child(
                        div()
                            .flex()
                            .gap_6()
                            .child(div().text_color(rgb(c.accent)).child(frames))
                            .child(rows)
                            .child(heap),
                    )
                    .child(div().text_color(rgb(c.faint)).child(load))
            }))
            // The help overlay, last so it paints over everything: deferred, so
            // it escapes the column's paint order; occluding, so the rows under
            // it get neither the clicks nor the wheel. Its rows come from the
            // same projection the terminal draws, which is why neither client
            // can drift from the other.
            .children(
                self.help
                    .then(|| help::overlay(&config::host(cx), &self.modes)),
            );
        root
    }
}

/// One sentence on its own band under the title bar.
fn band(c: &plait_core::theme::ChromePalette, text: SharedString, ink: Rgb) -> impl IntoElement {
    div()
        .flex_none()
        .flex()
        .items_center()
        .h(px(BAND_H))
        .px_4()
        .bg(rgb(c.status_bg))
        .border_b_1()
        .border_color(rgb(c.border))
        .text_color(rgb(ink))
        .child(div().min_w_0().truncate().child(text))
}

fn main() {
    start::begin(std::time::Instant::now());
    start::mark("main enter");
    // Arguments, `plait.toml`, `--help`, `plait config` and acquisition, all of
    // it shared with every other client — see `plait_app`. What is left in this
    // file is a window.
    let started = match Startup::new("plait", View::Commits)
        .blurb("a git client")
        .extra(EXTRA)
        .go()
    {
        Ok(started) => started,
        Err(exit) => exit.finish(),
    };
    start::mark("startup done (args + plait.toml + acquire)");
    let Started {
        view: which,
        source,
        host,
        loaded,
        config: config_path,
        repo,
    } = started;
    let host = Rc::new(host);

    // Names this exact view, so a saved scroll position is only ever restored
    // into the diff it was taken in — see `session.rs`.
    let session_key = source.key(which);
    let session_path = session::path();

    // How to fetch the diff again with a different algorithm, and where a
    // commit's diff would come from. Built here, where the source is known, so
    // nothing downstream has to learn what a repository is. `None` for a `.diff`
    // fixture. The handle is the one Startup opened, so every re-acquisition
    // keeps any backend state alive rather than opening the path again.
    let (rediff, repo) = match (&source, repo) {
        (Source::Repo { path, .. }, Some(repo)) => {
            let for_diff = repo.clone();
            let rediff: Rediff = Rc::new(move |host: &Host, over: &Overrides, revision: &str| {
                plait_git::diff(for_diff.as_ref(), revision, &host.differ, over)
            });
            (Some(rediff), Some((path.clone(), repo)))
        }
        _ => (None, None),
    };

    let which_name = which.name();
    let label = loaded.label.clone();
    let data = loaded.data;

    // One for the action handler, one for the strip: both reload from the file,
    // and the async task below takes the original.
    let shell_config_path = config_path.clone();

    let app = gpui_platform::application().with_assets(gpui_component_assets::Assets);
    start::mark("gpui application up; entering run");
    app.run(move |cx| {
        start::mark("app.run enter");
        gpui_component::init(cx);
        cx.set_global(config::Active(host.clone()));
        // Nothing picked yet: the file's theme is the one on screen.
        cx.set_global(config::Chosen(None));
        // After `gpui_component::init`, which sets its own theme to Light — see
        // `config::sync_widgets`, which is the only thing standing between that
        // and a pair of light scrollbars over a near-black diff.
        config::sync_widgets(&host, cx);
        start::mark("run setup through widget theme sync");

        // Re-read the file whenever it is written, and hand the result to every
        // window. The watcher's callback runs on its own thread, so it only sets
        // a flag; the task below is what touches the app.
        //
        // Polling a flag rather than plumbing an async channel through: a save is
        // a human action, 120 ms of latency is imperceptible, and this is five
        // lines with nothing to get wrong about wakeups.
        let dirty = Arc::new(AtomicBool::new(false));
        let watcher = {
            let dirty = dirty.clone();
            config::watch(&config_path, move || dirty.store(true, Ordering::Relaxed)).ok()
        };
        if watcher.is_none() {
            eprintln!(
                "plait: could not watch {}; config reload is off",
                config_path.display()
            );
        }
        cx.spawn(async move |cx: &mut AsyncApp| {
            // Held for as long as the task lives: dropping a `notify` watcher
            // stops it watching, silently.
            let _watcher = watcher;
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(120))
                    .await;
                if !dirty.swap(false, Ordering::Relaxed) {
                    continue;
                }
                // The same call a theme pick makes — see `config::reload`.
                let warnings = cx.update(|cx| config::reload(&config_path, cx));
                for w in warnings {
                    eprintln!("plait: {w}");
                }
            }
        })
        .detach();

        // The platform's keys, not this app's: these three exist for the menu —
        // accelerators macOS shows and performs — and their handlers are the
        // element-level adapters in `render`, which call the same named dispatch
        // every keypress uses. Nothing else is a `KeyBinding` anywhere in this
        // crate: `s`, `w`, `T`, `escape` and the rest resolve through the live
        // keymap, where `[keys]` can move them.
        cx.bind_keys([
            KeyBinding::new("cmd-q", Quit, None),
            KeyBinding::new("cmd-c", CopySelection, None),
            KeyBinding::new("cmd-a", SelectAll, None),
        ]);

        // Open the window here and now, not from a spawned task: the task only
        // ran at the executor's next pump, which put a scheduling hop between
        // this body and the first frame for no benefit — and every registration
        // above is in place before any event can be delivered, because none are
        // delivered until this closure yields.
        start::mark("opening window");
        cx.open_window(
            window_options(started_title(which, &label).into()),
            move |window, cx| {
                start::mark("window callback enter");
                // Where the last run of this exact command left off. Restored
                // before the first frame so you never see row 0 flash past.
                let resume = session::restore(&session_key, &session_path);
                start::mark("session restored");
                #[allow(clippy::type_complexity)]
                let (screen, rendered, top, total, note, load): (
                    Screen,
                    Rc<Cell<usize>>,
                    Rc<Cell<usize>>,
                    Rc<Cell<usize>>,
                    Rc<std::cell::RefCell<SharedString>>,
                    String,
                ) = match data {
                    Data::Commits(commits) => {
                        let e = cx.new(|_| views::commits::Commits::new(commits, host.clone()));
                        let v = e.read(cx);
                        if let Some(r) = &resume {
                            // The viewport model is filled in before either
                            // call, so a saved row clamps against a list that
                            // exists and a margin from the live file — see
                            // `Commits::scroll_to`.
                            v.scroll_to(r.top, &host);
                            v.go_to(r.top, &host);
                        }
                        (
                            Screen::Commits {
                                view: e,
                                source: source.clone(),
                                generation: Generation::default(),
                                label: label.clone(),
                            },
                            v.rendered.clone(),
                            // The commit graph has a fixed row count: one per
                            // commit, and nothing reflows it.
                            v.top.clone(),
                            Rc::new(Cell::new(v.total())),
                            Rc::new(std::cell::RefCell::new(SharedString::default())),
                            v.load.clone(),
                        )
                    }
                    Data::Diff(files) => {
                        let e = cx.new(|cx| views::diff::Diff::new(files, host.clone(), cx));
                        let v = e.read(cx);
                        if let Some(r) = &resume {
                            v.scroll_to(r.top, &host);
                            v.go_to(r.top, &host);
                        }
                        (
                            Screen::Diff {
                                view: e.clone(),
                                source: source.clone(),
                                generation: Generation::default(),
                                label: label.clone(),
                            },
                            v.rendered.clone(),
                            v.top.clone(),
                            v.total.clone(),
                            v.note.clone(),
                            v.load.clone(),
                        )
                    }
                };
                let initial_screens = vec![screen];
                start::mark("view built");

                // First-paint evidence, and only when logging: the views count
                // rows as the list builds them (see `rendered`), so the counter
                // going non-zero is the first frame actually carrying content.
                // Polled at 5 ms rather than hooked into a render — a paint has
                // no callback, and this task dies as soon as it fires.
                if start::on() {
                    let drawn = rendered.clone();
                    cx.spawn(async move |cx| loop {
                        cx.background_executor()
                            .timer(Duration::from_millis(5))
                            .await;
                        let n = drawn.get();
                        if n > 0 {
                            start::mark(&format!("first rows drawn ({n})"));
                            break;
                        }
                    })
                    .detach();
                }

                // Persist as you scroll, so any kind of death keeps the position:
                // `dev.sh` kills the process, and nothing runs on the way out.
                // Only on change, so an idle window writes nothing at all.
                {
                    let (key, path) = (session_key.clone(), session_path.clone());
                    let start = resume.map(|r| r.top).unwrap_or(0);
                    cx.spawn(async move |cx: &mut AsyncApp| {
                        let mut last = start;
                        loop {
                            cx.background_executor()
                                .timer(Duration::from_millis(400))
                                .await;
                            let now = top.get();
                            if now != last {
                                last = now;
                                session::save(
                                    &session::Session {
                                        key: key.clone(),
                                        top: now,
                                    },
                                    &path,
                                );
                            }
                        }
                    })
                    .detach();
                }
                let stats = stats::enabled().then(|| Stats::new(rendered, total, note, load));
                let focus = cx.focus_handle();
                let jobs = Runner::new();
                let submitter = jobs.submitter();
                let shell = cx.new(|_| DevShell {
                    which: which_name,
                    stack: initial_screens,
                    stats,
                    rediff,
                    repo,
                    jobs,
                    submitter,
                    generation: Generation::default(),
                    running: None,
                    over: Overrides::default(),
                    open: None,
                    error: None,
                    notice: None,
                    config: shell_config_path,
                    first_render: Cell::new(false),
                    modes: Modes::new(),
                    pending: Vec::new(),
                    help: false,
                    focus,
                    focused: None,
                    seen_host: None,
                    ongoing: Cell::default(),
                });
                {
                    let shell = shell.clone();
                    shell.update(cx, |shell, _| {
                        shell.sync_modes();
                    });
                }
                {
                    let shell = shell.downgrade();
                    cx.spawn(async move |cx: &mut AsyncApp| loop {
                        cx.background_executor()
                            .timer(Duration::from_millis(50))
                            .await;
                        if shell.update(cx, |shell, cx| shell.drain_jobs(cx)).is_err() {
                            break;
                        }
                    })
                    .detach();
                }
                cx.new(|cx| Root::new(shell, window, cx))
            },
        )
        .expect("failed to open window");
        start::mark("open_window returned");
        cx.activate(true);

        // Closing the last window must end the process — macOS keeps an
        // appless process alive otherwise.
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        // Last, not first: these two are the platform round trips of this
        // closure — ~20 ms measured between them, the single largest thing
        // here — and nothing on screen needs either. The cost is a first
        // touch, not a property of one call: whichever platform API runs
        // first after setup absorbs it (it moved with `set_menus`, then with
        // `on_window_closed`, as they were reordered), so both go *after* the
        // window rather than paying it on the way to frame zero. No event is
        // delivered to keys, menus or actions until this closure returns and
        // the event loop starts, so registering after the window exists races
        // nothing; the bar just fills in while frame zero paints.
        //
        // Three items, each an adapter onto a named command — see the note on
        // the `actions!`.
        cx.set_menus(vec![
            Menu {
                name: "plait".into(),
                items: vec![MenuItem::action("Quit", Quit)],
                disabled: false,
            },
            // Not decoration: without an Edit menu macOS gives the window no
            // Copy item, and the OS is entitled to be asked. The keys work
            // either way — this is what makes them *discoverable*.
            Menu {
                name: "Edit".into(),
                items: vec![
                    MenuItem::action("Copy", CopySelection),
                    MenuItem::action("Select All", SelectAll),
                ],
                disabled: false,
            },
        ]);
        start::mark("menus + close handler registered");
    });
}

impl DevShell {
    /// What the title's dimmest third says: the repository and revision, or the
    /// commit whose diff is on top.
    fn active_label(&self) -> SharedString {
        SharedString::from(
            self.active()
                .map(Screen::label)
                .unwrap_or_else(|| self.which.to_string()),
        )
    }
}

/// The window's own title — what macOS shows in Mission Control, the Window menu
/// and the tab bar. Not what is drawn in the strip: that is three separate
/// colours in [`DevShell::render`], because "plait", the view and the repository
/// are three different kinds of thing and one grey run of text says so about
/// none of them.
///
/// `Started::title` is the shared one; this exists because the window is opened
/// after the `Started` has been taken apart, and reassembling it to ask would be
/// sillier than the two lines.
fn started_title(view: View, label: &str) -> String {
    format!("plait · {} · {label}", view.name())
}

/// The window, and the one decision in it worth writing down: **there is no
/// system titlebar.**
///
/// `WindowOptions::default()` leaves `appears_transparent: false`, which is an
/// opaque macOS titlebar — in system grey, titled with the executable's name
/// because `title` was never set — stacked directly on top of this app's own
/// 32-pixel strip. Two title bars, one of them nobody wrote.
///
/// So the strip *is* the titlebar. `traffic_light_position` is the inset of the
/// close button, and macOS uses that same inset above and below it to size the
/// band, so `(10, 10)` on a 12px button is a 32px titlebar — exactly
/// [`TITLE_H`], which is why the lights sit centred in the strip rather than
/// floating in it. Dragging still belongs to the platform: `app_owns_titlebar_drag`
/// stays false, so the empty part of the strip moves the window for free.
///
/// A minimum size, because there is no useful window narrower than its own
/// gutters — the diff view's wrap budget bottoms out at eight characters and
/// says so, and this is the other end of the same argument.
fn window_options(title: SharedString) -> WindowOptions {
    WindowOptions {
        titlebar: Some(TitlebarOptions {
            title: Some(title),
            appears_transparent: true,
            traffic_light_position: Some(point(px(LIGHTS_X), px((TITLE_H - 12.0) / 2.0))),
        }),
        window_min_size: Some(size(px(560.), px(320.))),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::{DevShell, Open, Screen};
    use crate::views::commits::Commits;
    use gpui::{AppContext as _, TestAppContext};
    use plait_app::cli::Source;
    use plait_app::jobs::{Generation, Runner};
    use plait_core::command::{Key, Modes};
    use plait_core::host::Host;
    use std::cell::Cell;
    use std::rc::Rc;

    /// A shell on one commits screen, with whatever picker is named open and
    /// one key of a chord half-typed — the state `esc` meets most often.
    fn shell(which: Option<Open>, cx: &mut TestAppContext) -> gpui::Entity<DevShell> {
        cx.new(|cx| {
            let commits = cx.new(|_| Commits::new(Vec::new(), Rc::new(Host::new())));
            let jobs = Runner::new();
            DevShell {
                which: "commits",
                stack: vec![Screen::Commits {
                    view: commits,
                    source: Source::Fixtures,
                    generation: Generation::default(),
                    label: "repo".into(),
                }],
                stats: None,
                rediff: None,
                repo: None,
                submitter: jobs.submitter(),
                jobs,
                generation: Generation::default(),
                running: None,
                over: Default::default(),
                open: which,
                error: None,
                notice: None,
                config: std::path::PathBuf::new(),
                first_render: Cell::new(false),
                modes: Modes::new(),
                pending: vec![vec![Key::char('g')]],
                help: false,
                focus: cx.focus_handle(),
                focused: None,
                seen_host: None,
                ongoing: Cell::default(),
            }
        })
    }

    #[gpui::test]
    fn esc_closes_any_open_picker_and_touches_nothing_else(cx: &mut TestAppContext) {
        for which in [
            Open::Theme,
            Open::Layout,
            Open::Wrap,
            Open::Algorithm,
            Open::Whitespace,
        ] {
            let shell = shell(Some(which), cx);
            shell.update(cx, |s, cx| s.back(cx));
            shell.read_with(cx, |s, _| {
                assert!(s.open.is_none(), "{which:?} stayed open");
                assert!(
                    s.pending.is_empty(),
                    "{which:?}: the half-typed chord survived"
                );
                assert_eq!(s.stack.len(), 1, "{which:?}: esc popped the screen too");
                assert!(!s.help, "{which:?}: esc reached past the menu");
            });
        }
    }

    #[gpui::test]
    fn esc_without_a_picker_still_leaves_the_screen_the_innermost_exit(cx: &mut TestAppContext) {
        // The control: nothing open, two screens stacked — back pops to the
        // first and no further.
        let shell = shell(None, cx);
        shell.update(cx, |s, cx| {
            let commits = cx.new(|_| Commits::new(Vec::new(), Rc::new(Host::new())));
            s.stack.push(Screen::Commits {
                view: commits,
                source: Source::Fixtures,
                generation: Generation::default(),
                label: "second".into(),
            });
        });
        assert_eq!(shell.read_with(cx, |s, _| s.stack.len()), 2);
        shell.update(cx, |s, cx| s.back(cx));
        shell.read_with(cx, |s, _| {
            assert_eq!(s.stack.len(), 1, "the top screen was not popped");
            assert!(s.open.is_none());
        });
        // And on the last screen esc stops: it was never a quit.
        shell.update(cx, |s, cx| s.back(cx));
        shell.read_with(cx, |s, _| assert_eq!(s.stack.len(), 1));
    }

    #[test]
    fn the_wheel_follows_the_resolved_command_not_the_finger() {
        // A flick away from the user is positive; what it *does* is whatever
        // `[keys]` resolved to. The shipped binding signs it one way…
        assert_eq!(
            DevShell::smooth_pixels("view.scroll-up", 40.0, 1),
            Some(40.0)
        );
        assert_eq!(
            DevShell::smooth_pixels("view.scroll-down", 40.0, 1),
            Some(-40.0)
        );
        // …and a rebound `wheelup = "view.scroll-down"` sends the very same
        // flick the other way — the finger's own sign never leaks through.
        assert_eq!(
            DevShell::smooth_pixels("view.scroll-down", -40.0, 1),
            Some(-40.0)
        );
    }

    #[test]
    fn the_scroll_setting_multiplies_and_other_commands_dispatch_by_name() {
        // `[view] scroll` scales the finger's pixels…
        assert_eq!(
            DevShell::smooth_pixels("view.scroll-up", -12.5, 4),
            Some(50.0)
        );
        // …and everything else — a page, an extension's command — keeps the
        // event's pixels out of it and goes through named dispatch instead.
        assert_eq!(DevShell::smooth_pixels("view.page-down", 30.0, 1), None);
        assert_eq!(DevShell::smooth_pixels("blame.toggle", 30.0, 1), None);
    }
}
