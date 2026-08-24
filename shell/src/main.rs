mod config;
mod controls;
mod dispatch;
mod graph;
mod help;
mod input;
mod panes;
mod session;
mod stats;
mod views;

use gitten_app::acquire::{Data, Loaded};
use gitten_app::cli::{Source, View};
use gitten_app::jobs::{Event as JobEvent, Generation, Job, Runner, Submitter};
use gitten_app::{Started, Startup};
use gitten_core::command::{chord_string, Code, Key, Modes, Resolve};
use gitten_core::differ::{Overrides, Whitespace};
use gitten_core::host::Host;
use gitten_core::theme::Rgb;
use gitten_core::FileDiff;
use gpui::*;
use gpui_component::*;
use stats::Stats;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Startup-stage timestamps on stderr, behind `GITTEN_START_LOG=1`.
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
        *ON.get_or_init(|| std::env::var_os("GITTEN_START_LOG").is_some_and(|v| v != "0"))
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
actions!(gitten, [Quit, CopySelection, SelectAll]);

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
/// The slice of the pane column the working tree claims before the rest of the
/// panes divide what is left — see [`Pane::height_share`]. A code constant on
/// purpose tonight; a drag handle between panes would own this properly.
const FILES_SHARE: f32 = 0.3;

/// What only this client has. The two views, the arguments and `gitten.toml` are
/// documented once, in `gitten_app::cli::usage`, because they are the same in
/// every client — see that function for why that is a promise and not a
/// convenience.
const EXTRA: &str =
    "  The title bar carries five pickers: the presentation (unified, side-by-side),
  where a line too wide for the window breaks (off, word, char), the diff
  algorithm (histogram, patience, myers), how much whitespace has to match
  (exact, trailing, change, all — git's default, --ignore-space-at-eol, -b and
  -w) and the theme (dark, light, slate, and whatever gitten.toml adds). `s`
  cycles the presentation, `w` the wrap and `T` the theme — all three through
  `[keys]` in gitten.toml, where `?` lists everything.

  The file is re-read every time it is saved, and colours and font apply on the
  next frame — no rebuild, no relaunch.

  ./dev.sh <args>  rebuild and relaunch on every source change, landing back
                   on the row you were reading. Debug build and the overlay by
                   default; pass --release before trusting a timing.

  GITTEN_STATS=1   frame, row and heap overlay
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

type RefreshValue = Box<dyn std::any::Any + Send>;
type ApplyRefresh = dyn FnOnce(RefreshValue, &Host, &mut App) -> Result<(), String>;

/// What the open input's accept means, and the only things the shell's prompt
/// slot can hold tonight. A third consumer becomes a third variant and nothing
/// else changes: the field routes by what it was opened *for*, never by who is
/// listening.
#[derive(Debug)]
enum Prompt {
    /// The message a `files.commit` accept turns into a commit job.
    CommitMessage,
    /// A `/` query over one pane, named by its registration name — a name and
    /// not a type, so the slot stays open to whatever pane learns to answer a
    /// search next. Every edit filters that pane live (see
    /// [`DevShell::search_edited`]); accepting keeps the last edit standing,
    /// cancelling clears it.
    Search { target: String },
}

/// The write rails, handed to every pane command: the repository this window
/// opened on, and the one queue every job rides.
///
/// Owned clones rather than borrows — a refcount and a channel sender each —
/// because a command speaks *between* acquiring the rails and aiming the
/// write (the discard that clears its own question from the band), and an
/// extension may want to keep a half past the call. Both copies are cheap by
/// design; nothing here was meant to be held.
///
/// Passed by reference so a command can aim a write without owning either
/// half — which is what makes rule 1 true for verbs rather than merely said:
/// a compiled-in extension pane stages through exactly these two things that
/// `files.stage` does, and a fixture window hands `None`, whose honest answer
/// is the notice a built-in gives. `None` is also all a drawing-only command
/// ever sees of them.
#[derive(Clone)]
struct Writes {
    repo: gitten_git::Handle,
    submit: Submitter,
}

impl Writes {
    /// Queues one job. False is the queue rejecting work — the window is
    /// going away — and saying so is the caller's, who knows what was tried.
    fn send(&self, job: Box<dyn Job>) -> bool {
        self.submit.submit(job).is_ok()
    }
}

/// A pane-owned refresh split at the thread boundary: pure blocking load, then
/// GPUI apply. The shell schedules both halves without knowing the tenant's data
/// type, which is what lets a files or extension pane refresh without joining a
/// central acquisition enum.
struct Refresh {
    generation: Generation,
    load: Box<dyn FnOnce() -> Result<RefreshValue, String> + Send>,
    apply: Box<ApplyRefresh>,
}

impl Refresh {
    fn new<T: Send + 'static>(
        generation: Generation,
        load: impl FnOnce() -> Result<T, String> + Send + 'static,
        apply: impl FnOnce(T, &Host, &mut App) -> Result<(), String> + 'static,
    ) -> Self {
        Self {
            generation,
            load: Box::new(move || load().map(|value| Box::new(value) as RefreshValue)),
            apply: Box::new(move |value, host, cx| {
                let value = value
                    .downcast::<T>()
                    .map_err(|_| "pane refresh returned the wrong data type".to_string())?;
                apply(*value, host, cx)
            }),
        }
    }
}

/// One pane tenant, independent of how the shell lays panes out.
///
/// Built-ins and compiled-in extensions enter through the same object-safe
/// seam. Only drawing, local command behavior and optional repository refresh
/// live here; stable naming, placement and focus belong to [`panes::Panes`].
trait Pane {
    fn any(&self) -> AnyView;
    fn mode(&self) -> &'static str;

    /// What this pane says in the title strip. Takes the app so a pane whose
    /// label depends on live view state — a filtered commit list counts its
    /// rows — can read itself before answering.
    fn label(&self, cx: &App) -> String;

    /// A tenant-owned repository refresh. The load half must contain every
    /// blocking operation; the apply half is the only half allowed to touch a
    /// GPUI entity. A pane not backed by the current repository returns `None`.
    fn refresh(
        &self,
        _generation: Generation,
        _host: &Host,
        _overrides: &Overrides,
        _repo: gitten_git::Handle,
    ) -> Option<Refresh> {
        None
    }

    fn list_bounds(&self, _cx: &App) -> Bounds<Pixels> {
        Bounds::default()
    }

    /// The fraction of the pane column this tenant claims before the rest
    /// divide what is left. `None` — every pane built before this existed — is
    /// an equal share, unchanged. A viewer that is context rather than content
    /// (the working tree) claims a fixed slice; whatever the window opens *for*
    /// keeps `None` and stays the star.
    fn height_share(&self, _cx: &App) -> Option<f32> {
        None
    }

    fn pan_pixels(&self, _dx: f32, _cx: &App) -> bool {
        false
    }

    /// Runs one of this pane's commands. `writes` is [`Writes`] when the
    /// window sits on a repository — the same handle and queue a built-in
    /// verb uses — and the pane answers for itself whether it can act on
    /// them. False is "not one of mine", and the caller says so.
    fn run(&self, _command: &str, _host: &Host, _writes: Option<&Writes>, _cx: &mut App) -> bool {
        false
    }

    fn scroll_pixels(&self, _dy: f32, _host: &Host, _cx: &mut App) -> bool {
        false
    }

    fn select(&self, _all: bool, _cx: &mut App) -> bool {
        false
    }
}

/// One pane tenant. Built-ins keep repository metadata beside their typed GPUI
/// view; extensions enter through [`Screen::Custom`].
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
        generation: Rc<Cell<Generation>>,
        label: Rc<RefCell<String>>,
    },
    Diff {
        view: Entity<views::diff::Diff>,
        source: Source,
        generation: Rc<Cell<Generation>>,
        label: Rc<RefCell<String>>,
    },
    /// The working tree. No `Source`: it is always about the repository the
    /// window opened on, and a fixture — which has no working tree at all —
    /// simply never gets one registered.
    Files {
        view: Entity<views::files::Files>,
        generation: Rc<Cell<Generation>>,
        label: Rc<RefCell<String>>,
    },
    Custom(Rc<dyn Pane>),
}

impl Screen {
    fn commits(
        view: Entity<views::commits::Commits>,
        source: Source,
        generation: Generation,
        label: impl Into<String>,
    ) -> Self {
        Self::Commits {
            view,
            source,
            generation: Rc::new(Cell::new(generation)),
            label: Rc::new(RefCell::new(label.into())),
        }
    }

    fn diff(
        view: Entity<views::diff::Diff>,
        source: Source,
        generation: Generation,
        label: impl Into<String>,
    ) -> Self {
        Self::Diff {
            view,
            source,
            generation: Rc::new(Cell::new(generation)),
            label: Rc::new(RefCell::new(label.into())),
        }
    }

    fn files(
        view: Entity<views::files::Files>,
        generation: Generation,
        label: impl Into<String>,
    ) -> Self {
        Self::Files {
            view,
            generation: Rc::new(Cell::new(generation)),
            label: Rc::new(RefCell::new(label.into())),
        }
    }

    fn any(&self) -> AnyView {
        match self {
            Screen::Commits { view, .. } => view.clone().into(),
            Screen::Diff { view, .. } => view.clone().into(),
            Screen::Files { view, .. } => view.clone().into(),
            Screen::Custom(pane) => pane.any(),
        }
    }

    /// Which mode's bindings are live. The name the keymap and `gitten.toml` use.
    fn mode(&self) -> &'static str {
        match self {
            Screen::Commits { .. } => "commits",
            Screen::Diff { .. } => "diff",
            Screen::Files { .. } => "files",
            Screen::Custom(pane) => pane.mode(),
        }
    }

    fn label(&self, cx: &App) -> String {
        match self {
            Screen::Commits { view, label, .. } => {
                let base = label.borrow().clone();
                // The filter's count rides on the acquisition label rather
                // than replacing it — the same shape the working tree uses for
                // "0 changed" — and the pane cell keeps holding only what the
                // repository named, so a refresh has nothing to recompose.
                match view.read(cx).filter_note() {
                    Some(note) => format!("{base} · {note}"),
                    None => base,
                }
            }
            Screen::Diff { label, .. } | Screen::Files { label, .. } => label.borrow().clone(),
            Screen::Custom(pane) => pane.label(cx),
        }
    }

    fn refresh(
        &self,
        target: Generation,
        host: &Host,
        overrides: &Overrides,
        repo: gitten_git::Handle,
    ) -> Option<Refresh> {
        match self {
            Screen::Commits {
                view,
                source,
                generation,
                label,
            } => {
                if generation.get() >= target || matches!(source, Source::Fixtures) {
                    return None;
                }
                let source = source.clone();
                let load_host = host.clone();
                let view = view.clone();
                let generation = generation.clone();
                let label = label.clone();
                Some(Refresh::new(
                    target,
                    move || {
                        let loaded = gitten_app::acquire::reacquire(
                            View::Commits,
                            &source,
                            &load_host,
                            Some(repo.as_ref()),
                            &Overrides::default(),
                        )?;
                        let Data::Commits(commits) = loaded.data else {
                            return Err("re-acquisition returned the wrong view".into());
                        };
                        Ok((loaded.label, views::commits::prepare(commits, &load_host)))
                    },
                    move |(next_label, prepared): (String, views::commits::Prepared), host, cx| {
                        if generation.get() >= target {
                            return Ok(());
                        }
                        view.update(cx, |view, cx| {
                            view.replace_prepared(prepared, host);
                            cx.notify();
                        });
                        label.replace(next_label);
                        generation.set(target);
                        Ok(())
                    },
                ))
            }
            Screen::Diff {
                view,
                source,
                generation,
                label,
            } => {
                if generation.get() >= target || matches!(source, Source::Fixtures) {
                    return None;
                }
                let source = source.clone();
                let load_host = host.clone();
                let overrides = overrides.clone();
                let view = view.clone();
                let generation = generation.clone();
                let label = label.clone();
                Some(Refresh::new(
                    target,
                    move || {
                        let loaded = gitten_app::acquire::reacquire(
                            View::Diff,
                            &source,
                            &load_host,
                            Some(repo.as_ref()),
                            &overrides,
                        )?;
                        let Data::Diff(files) = &loaded.data else {
                            return Err("re-acquisition returned the wrong view".into());
                        };
                        let prepared = views::diff::prepare_files(files, &load_host);
                        Ok((loaded, prepared))
                    },
                    move |(loaded, prepared): (Loaded, gitten_core::prepared::Prepared),
                          host,
                          cx| {
                        if generation.get() >= target {
                            return Ok(());
                        }
                        let Data::Diff(files) = loaded.data else {
                            return Err("re-acquisition returned the wrong view".into());
                        };
                        view.update(cx, |view, cx| {
                            view.replace_prepared(files, prepared, host, cx)
                        });
                        label.replace(loaded.label);
                        generation.set(target);
                        Ok(())
                    },
                ))
            }
            Screen::Files {
                view,
                generation,
                label,
            } => {
                if generation.get() >= target {
                    return None;
                }
                let view = view.clone();
                let generation = generation.clone();
                let label = label.clone();
                Some(Refresh::new(
                    target,
                    move || {
                        // The whole of the blocking half: one `git status`.
                        // The describe rides along beside it so the label
                        // keeps naming the repository, the way acquisition
                        // overlaps its own pieces.
                        let described = std::thread::scope(|s| {
                            let title = s.spawn(|| repo.describe());
                            let status = repo.status()?;
                            Ok::<_, String>(views::files::prepare(
                                status,
                                &title.join().unwrap_or_default(),
                            ))
                        })?;
                        Ok(described)
                    },
                    move |prepared: views::files::Prepared, host, cx| {
                        if generation.get() >= target {
                            return Ok(());
                        }
                        let label_text = prepared.label.clone();
                        view.update(cx, |v, cx| {
                            v.replace_prepared(prepared, host);
                            cx.notify();
                        });
                        label.replace(label_text);
                        generation.set(target);
                        Ok(())
                    },
                ))
            }
            Screen::Custom(pane) => pane.refresh(target, host, overrides, repo),
        }
    }

    /// The box this screen's row list occupies, for hit-testing a wheel event.
    fn list_bounds(&self, cx: &App) -> Bounds<Pixels> {
        match self {
            Screen::Commits { view, .. } => view.read(cx).list_bounds(),
            Screen::Diff { view, .. } => view.read(cx).list_bounds(),
            Screen::Files { view, .. } => view.read(cx).list_bounds(),
            Screen::Custom(pane) => pane.list_bounds(cx),
        }
    }

    /// The slice of the pane column this screen claims — see [`Pane::height_share`].
    fn height_share(&self, cx: &App) -> Option<f32> {
        match self {
            // The working tree is context; whatever the window opened for gets
            // an equal share and stays the star.
            Screen::Files { .. } => Some(FILES_SHARE),
            Screen::Commits { .. } | Screen::Diff { .. } => None,
            Screen::Custom(pane) => pane.height_share(cx),
        }
    }

    /// Moves this screen's text sideways, where it has any — a commit graph has
    /// nothing off the left edge to reach, and says so by not moving. Whether
    /// anything moved decides a redraw.
    fn pan_pixels(&self, dx: f32, cx: &App) -> bool {
        match self {
            Screen::Commits { view, .. } => view.read(cx).pan_pixels(dx),
            Screen::Diff { view, .. } => view.read(cx).pan_pixels(dx),
            Screen::Files { view, .. } => view.read(cx).pan_pixels(dx),
            Screen::Custom(pane) => pane.pan_pixels(dx, cx),
        }
    }

    /// Runs one of the commands a screen owns: the `view.*` family both share
    /// and each screen's own additions. False is "not one of mine", and the
    /// caller says so — an unknown command that resolved is worth naming rather
    /// than swallowing.
    fn run(&self, command: &str, host: &Host, writes: Option<&Writes>, cx: &mut App) -> bool {
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
            Screen::Files { view, .. } => view.update(cx, |f, c| {
                let known = f.run_view(command, host);
                if known {
                    c.notify();
                }
                known
            }),
            Screen::Custom(pane) => pane.run(command, host, writes, cx),
        }
    }

    /// The wheel's smooth path: pixels into the list, in the direction the
    /// resolved command says and at whatever `[view] scroll` multiplies them
    /// by. The host rides along because the viewport's margin is live.
    fn scroll_pixels(&self, dy: f32, host: &Host, cx: &mut App) -> bool {
        match self {
            Screen::Commits { view, .. } => view.update(cx, |v, _| v.scroll_pixels(dy, host)),
            Screen::Diff { view, .. } => view.update(cx, |v, _| v.scroll_pixels(dy, host)),
            Screen::Files { view, .. } => view.update(cx, |v, _| v.scroll_pixels(dy, host)),
            Screen::Custom(pane) => pane.scroll_pixels(dy, host, cx),
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
            Screen::Files { view, .. } => view.update(cx, |f, _| match all {
                true => f.select_all(),
                false => f.select_none(),
            }),
            Screen::Custom(pane) => pane.select(all, cx),
        }
    }

    fn custom(pane: impl Pane + 'static) -> Self {
        Self::Custom(Rc::new(pane))
    }
}

struct DevShell {
    /// The app half of the title, drawn bright: which program this is. Which
    /// *view* it is showing is the focused pane's to say, because a commit list
    /// that opened a diff is still a diff while that pane owns the keyboard.
    which: &'static str,
    /// Stable names make this a registry: reopening a diff replaces the `diff`
    /// tenant instead of appending a duplicate pane.
    panes: panes::Panes<Screen>,
    stats: Option<Stats>,
    /// How to fetch the diff again with a different algorithm. `None` for a
    /// `.diff` fixture, where there is no repository behind the rows at all.
    rediff: Option<Rediff>,
    /// The repository path used in labels and the persistent handle used for
    /// every acquisition. `None` for a fixture, which has no repository behind
    /// it — and the key then says so, which is what an unbound key does too.
    repo: Option<(std::path::PathBuf, gitten_git::Handle)>,
    jobs: Runner,
    submitter: Submitter,
    generation: Generation,
    /// The newest refresh batch and the work still outstanding in it. Older
    /// batches may finish later; their generation keeps them from changing this
    /// batch's status or replacing newer pane data.
    refresh_id: u64,
    refresh_pending: usize,
    refresh_error: Option<String>,
    running: Option<String>,
    /// The one native text field over the active screen, if a command is
    /// gathering input. Consumers subscribe to its accepted/cancelled event.
    input: Option<Entity<input::Input>>,
    /// What accepting that input is for — set by whoever opened it, consumed
    /// by [`DevShell::close_input`]. One at a time, because there is one
    /// field; a second prompt replaces the first and says what it means.
    prompt: Option<Prompt>,
    /// The live half of a [`Prompt::Search`]: the subscription that carries
    /// each edit to the pane being filtered. Held so it dies with the prompt —
    /// replaced when another opens, dropped the moment one closes.
    search_live: Option<Subscription>,
    /// The live picks. Every field `None` means "whatever the config selected",
    /// which is what the controls show until somebody changes one — so the strip
    /// agrees with `gitten.toml` rather than with a copy of it taken at startup.
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
    /// Where `gitten.toml` is. Held because picking a theme goes through the same
    /// reload a save does — see [`config::reload`] for why there is only one
    /// path.
    config: std::path::PathBuf,
    /// Startup logging, and nothing else: whether [`start::mark`] has already
    /// stamped the first render. One bool read per frame afterwards.
    first_render: Cell<bool>,
    /// Which modes' bindings are live, innermost last: the pane container, the
    /// focused tenant, then input or help over it. Rebuilt by
    /// [`DevShell::sync_modes`] whenever any of those changes.
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
    /// The host the last key was resolved against. A saved `gitten.toml` swaps
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
        Some(self.panes.focused())
    }

    /// The view name the title strip shows: the active screen's mode — which
    /// is also the name `[keys]` groups its bindings under. Falls back to what
    /// launched the window only if there were no screens at all, which does
    /// not happen; the fallback keeps the type honest rather than the UI.
    fn active_view_name(&self) -> &'static str {
        self.active().map_or(self.which, Screen::mode)
    }

    /// Rebuilds the mode stack from what is focused, and drops whatever was
    /// pending against the previous arrangement: any half-typed chord, and any
    /// open picker — a menu belongs to the pane it was opened over, and one
    /// left standing after focus changes is invisible but still in
    /// `self.open`, where [`DevShell::on_wheel`] swallows for it forever.
    /// Called on every change of pane focus or help state — the places
    /// [`Modes`] can change.
    fn sync_modes(&mut self) {
        self.modes = Modes::new();
        if self.panes.len() > 1 {
            self.modes.push(panes::MODE);
        }
        if let Some(screen) = self.active() {
            self.modes.push(screen.mode());
        }
        if self.input.is_some() {
            self.modes.push(input::MODE);
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

    fn open_input(&mut self, input: Entity<input::Input>, cx: &mut Context<Self>) {
        // Whatever the previous prompt was filtering live stops now; what its
        // last edit did to its pane is that prompt's close to decide.
        self.search_live = None;
        if let Some(previous) = self.input.replace(input) {
            previous.update(cx, |input, cx| input.cancel(cx));
        }
        self.sync_modes();
        cx.notify();
    }

    /// Closes the field, accepting or cancelling it — and hands the accepted
    /// text to whatever opened it. The consumer is a slot rather than a
    /// subscription because the answer has exactly one destination: the
    /// prompt that is closing as it fires.
    fn close_input(&mut self, accept: bool, cx: &mut Context<Self>) {
        let Some(input) = self.input.take() else {
            return;
        };
        // The live feed dies with the prompt; the routing below settles what
        // the last edit left on the pane.
        self.search_live = None;
        // Read before the entity confirms its own event: the value is what the
        // consumer asked for; accept only closes.
        let text = input.read(cx).value().to_string();
        input.update(cx, |input, cx| match accept {
            true => input.accept(cx),
            false => input.cancel(cx),
        });
        self.sync_modes();
        match (accept, self.prompt.take()) {
            (true, Some(Prompt::CommitMessage)) => self.commit_message(text),
            // A search keeps what was typed on accept and clears on cancel —
            // `esc` means "forget it", not "keep half of it".
            (_, Some(Prompt::Search { target })) => {
                self.finish_search(&target, accept.then_some(text), cx)
            }
            _ => {}
        }
        cx.notify();
    }

    /// The write rails as every pane command receives them: this window's
    /// repository and its queue, or `None` over a fixture. The shell's own
    /// verbs go through the same value — there is no second path for a
    /// built-in.
    fn writes(&self) -> Option<Writes> {
        let (_, repo) = self.repo.as_ref()?;
        Some(Writes {
            repo: gitten_git::Handle::clone(repo),
            submit: self.submitter.clone(),
        })
    }

    /// `files.stage`: act on the row the keyboard is on, by the side of the
    /// index it sits on. Staged means unstage; everything else — unstaged,
    /// untracked, a conflict whose resolution is being recorded — means stage.
    /// That is lazygit's rule and git's own asymmetry: `add` is the one word
    /// for "the index should hold this".
    ///
    /// Like every verb's I/O, this reads its context from the focused view and
    /// then leaves the screen alone: the write runs on the job thread, and a
    /// successful finish bumps the generation so all repository panes
    /// re-acquire at once.
    fn stage_or_unstage(&mut self, cx: &mut Context<Self>) {
        let Some(Screen::Files { view, .. }) = self.active() else {
            self.set_notice("files.stage is not supported here");
            return;
        };
        let under = view
            .read(cx)
            .current_file()
            .map(|f| (f.section, f.path.clone()));
        let Some((section, path)) = under else {
            self.set_notice("nothing selected to stage");
            return;
        };
        // No rails means no repository behind this window — the same answer
        // the pane would get if it asked for them itself.
        let Some(writes) = self.writes() else {
            self.set_notice("a fixture has no working tree to stage in");
            return;
        };
        let bytes = path.as_bytes().to_vec();
        let job = match section {
            views::files::Section::Staged => gitten_app::verbs::Write::unstage(&writes.repo, bytes),
            _ => gitten_app::verbs::Write::stage(&writes.repo, bytes),
        };
        if !writes.send(Box::new(job)) {
            self.set_notice("the job queue is shutting down");
        }
    }

    /// `files.commit`: gather a message over the pane, then commit on accept.
    ///
    /// The input owns the keyboard while it is open — [`input::MODE`] sits on
    /// top of the pane stack — and [`DevShell::close_input`] routes the text
    /// back here through the prompt slot.
    fn begin_commit_message(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.active(), Some(Screen::Files { .. })) {
            self.set_notice("files.commit is not supported here");
            return;
        }
        if self.repo.is_none() {
            self.set_notice("a fixture has no repository to commit in");
            return;
        }
        let input = cx.new(|cx| input::Input::new("commit", "commit message", "", cx));
        self.open_input(input, cx);
        // After `open_input`, which may have cancelled a previous prompt.
        self.prompt = Some(Prompt::CommitMessage);
    }

    /// The accepted commit text, as a job. Empty refused again here — the
    /// trait refuses it too, but saying so beside the field that just closed
    /// beats making the reader find out twice.
    fn commit_message(&mut self, message: String) {
        if message.trim().is_empty() {
            self.set_notice("a commit needs a message");
            return;
        }
        let Some(writes) = self.writes() else {
            self.set_notice("a fixture has no repository to commit in");
            return;
        };
        if !writes.send(Box::new(gitten_app::verbs::Write::commit(
            &writes.repo,
            message,
        ))) {
            self.set_notice("the job queue is shutting down");
        }
    }

    /// `files.discard`: the one destructive verb, and it confirms on the
    /// keyboard because no dialog exists to confirm anywhere else. First
    /// press arms the row the keyboard is on and asks once, here in the
    /// band; second press on the same row builds the job; any cursor move,
    /// wheel or refresh disarms before it can lie (`Files` owns that state;
    /// this side only asks whether the press was the second one).
    ///
    /// Two refusals said up front rather than answered badly: a staged row,
    /// whose unstaged side may be empty and whose undo is unstage; and a
    /// conflict, whose working-tree side is the merge's open question.
    fn discard_selected(&mut self, cx: &mut Context<Self>) {
        let Some(Screen::Files { view, .. }) = self.active() else {
            self.set_notice("files.discard is not supported here");
            return;
        };
        let under = view
            .read(cx)
            .current_file()
            .map(|f| (f.section, f.path.clone(), f.path_text.to_string()));
        let Some((section, path, shown)) = under else {
            self.set_notice("nothing selected to discard");
            return;
        };
        match section {
            views::files::Section::Staged => {
                self.set_notice("that change is staged — unstage it before discarding");
                return;
            }
            views::files::Section::Conflicts => {
                self.set_notice("a conflicted file needs its merge resolved, not discarded");
                return;
            }
            views::files::Section::Untracked | views::files::Section::Unstaged => {}
        }
        // The rails, taken once like every sibling takes them: a fixture has
        // no working tree to discard from, and saying so outranks arming one.
        let Some(writes) = self.writes() else {
            self.set_notice("a fixture has no working tree to discard from");
            return;
        };
        // Arm, or spend the arm. False means the question was just asked.
        if !view.update(cx, |f, _| f.confirm_or_arm_discard(section, &path)) {
            self.set_notice(views::files::discard_question(section, &shown));
            return;
        }
        self.notice = None; // the question is spent; the running band speaks next
        let bytes = path.as_bytes().to_vec();
        let job = match section {
            views::files::Section::Untracked => {
                gitten_app::verbs::Write::remove_untracked(&writes.repo, bytes)
            }
            _ => gitten_app::verbs::Write::discard(&writes.repo, bytes),
        };
        if !writes.send(Box::new(job)) {
            self.set_notice("the job queue is shutting down");
        }
    }

    /// `files.stage-all`: every row, on the side of the index the keyboard
    /// sits in — the one rule `space` keeps for a single row, at scale.
    /// Staged row or staged heading: unstage everything staged. Anything
    /// else — unstaged, untracked, their headings, an empty tree: stage
    /// everything unstaged and untracked. Deterministic and visible (you
    /// can see where the cursor is), which is why it wins over a toggle:
    /// pressed twice in one place, it answers the same both times.
    ///
    /// Conflicts belong to neither direction — staging one records a
    /// resolution, which is its own decision. One job either way, so one
    /// generation bump and one re-acquire wave per keypress.
    fn stage_all(&mut self, cx: &mut Context<Self>) {
        let Some(Screen::Files { view, .. }) = self.active() else {
            self.set_notice("files.stage-all is not supported here");
            return;
        };
        let staging = view.read(cx).cursor_section() != Some(views::files::Section::Staged);
        let (first, second) = match staging {
            true => (
                views::files::Section::Unstaged,
                Some(views::files::Section::Untracked),
            ),
            false => (views::files::Section::Staged, None),
        };
        let mut targets = view.read(cx).paths_in(first);
        if let Some(second) = second {
            targets.extend(view.read(cx).paths_in(second));
        }
        if targets.is_empty() {
            self.set_notice(match staging {
                true => "nothing unstaged or untracked to stage",
                false => "nothing staged to unstage",
            });
            return;
        }
        let Some(writes) = self.writes() else {
            self.set_notice("a fixture has no working tree to act on");
            return;
        };
        let bytes = targets.iter().map(|p| p.as_bytes().to_vec()).collect();
        let job = match staging {
            true => gitten_app::verbs::Write::stage_many(&writes.repo, bytes),
            false => gitten_app::verbs::Write::unstage_many(&writes.repo, bytes),
        };
        if !writes.send(Box::new(job)) {
            self.set_notice("the job queue is shutting down");
        }
    }

    /// `files.ignore`: append the untracked file to the root `.gitignore`,
    /// creating that file when it is absent, and let the refresh do the
    /// rest — git stops listing ignored files on its own, so the entry
    /// leaves the pane without anything being deleted or moved.
    ///
    /// Only an untracked row answers. `.gitignore` governs files git does
    /// not yet track, so answering over a tracked change would be a no-op
    /// wearing a success badge.
    fn ignore_selected(&mut self, cx: &mut Context<Self>) {
        let Some(Screen::Files { view, .. }) = self.active() else {
            self.set_notice("files.ignore is not supported here");
            return;
        };
        let under = view
            .read(cx)
            .current_file()
            .map(|f| (f.section, f.path.clone()));
        let Some((views::files::Section::Untracked, path)) = under else {
            self.set_notice("only an untracked file can be ignored");
            return;
        };
        let Some(writes) = self.writes() else {
            self.set_notice("a fixture has no repository to ignore in");
            return;
        };
        let job = gitten_app::verbs::Write::ignore(&writes.repo, path.as_bytes().to_vec());
        if !writes.send(Box::new(job)) {
            self.set_notice("the job queue is shutting down");
        }
    }

    /// `commits.search`: gather a query over the focused commits pane.
    ///
    /// While the field is open every edit filters that pane's list live — the
    /// subscription installed here forwards each [`input::Event::Edited`] to
    /// [`DevShell::search_edited`] — so accept and cancel differ only in
    /// whether the last edit stands. A second `/` finds the current query
    /// already in the field, because the pane still holds it; an empty accept
    /// is how a filter comes off.
    ///
    /// The target is the pane's registration name taken at open, not "the
    /// focused screen" read again at close: a click can move focus while the
    /// field holds the keyboard's *mode*, and the query belongs to the pane it
    /// was typed over.
    fn begin_search(&mut self, cx: &mut Context<Self>) {
        let Some(Screen::Commits { view, .. }) = self.active() else {
            self.set_notice("commits.search is not supported here");
            return;
        };
        let target = self.panes.focused_name().to_string();
        let initial = view.read(cx).query().unwrap_or_default().to_string();
        let input = cx.new(|cx| input::Input::new("search", "search", initial, cx));
        self.open_input(input.clone(), cx);
        // After `open_input`, which may have cancelled a previous prompt.
        self.prompt = Some(Prompt::Search { target });
        self.search_live = Some(cx.subscribe(&input, Self::search_edited));
    }

    /// One edit in an open search: into the pane, before the next frame. Runs
    /// per keystroke and only while a search prompt lives — never per render.
    fn search_edited(
        &mut self,
        _: Entity<input::Input>,
        event: &input::Event,
        cx: &mut Context<Self>,
    ) {
        let input::Event::Edited(text) = event else {
            return;
        };
        let Some(Prompt::Search { target }) = &self.prompt else {
            return;
        };
        if let Some(view) = self.commits_pane(target).map(|(_, view)| view.clone()) {
            // Meet the list where its last drag left it — the order
            // `open_diff` and `copy` read in — before anchoring. Otherwise
            // typing right after a scrollbar drag anchors to the cursor as it
            // froze, not to the commit now being looked at.
            let host = config::host(cx);
            view.update(cx, |v, _| {
                v.reconcile(&host);
                v.apply_query(text);
            });
            cx.notify();
        }
    }

    /// Accept or cancel of a search prompt: what the last edit left standing,
    /// or its absence. Same routing as the live half, one last time.
    fn finish_search(&mut self, target: &str, query: Option<String>, cx: &mut Context<Self>) {
        let Some((_, view)) = self.commits_pane(target) else {
            return;
        };
        let query = query.unwrap_or_default();
        view.update(cx, |v, _| v.apply_query(&query));
    }

    /// The named pane's commits screen, when that is what the name registers:
    /// the one place search routing learns which screens answer. A closed pane
    /// or a kind with no search answers nothing, quietly — the prompt is
    /// closing anyway.
    fn commits_pane(&self, target: &str) -> Option<(usize, &Entity<views::commits::Commits>)> {
        let at = self.panes.position(target)?;
        match self.panes.iter().nth(at)? {
            Screen::Commits { view, .. } => Some((at, view)),
            _ => None,
        }
    }

    /// The one registration path built-ins and compiled-in extensions share.
    #[allow(dead_code)]
    fn register_pane(
        &mut self,
        name: impl Into<String>,
        pane: impl Pane + 'static,
        cx: &mut Context<Self>,
    ) {
        self.panes.register(name, Screen::custom(pane));
        self.sync_modes();
        cx.notify();
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
                    self.invalidate_refresh();
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
                        self.refresh_stale(cx);
                    }
                }
            }
        }
        if changed {
            cx.notify();
        }
    }

    fn refresh_stale(&mut self, cx: &mut Context<Self>) {
        let Some((_, repo)) = self.repo.as_ref().cloned() else {
            return;
        };
        self.invalidate_refresh();
        let refresh_id = self.refresh_id;
        let host = config::host(cx);
        let target = self.generation;
        let refreshes: Vec<Refresh> = self
            .panes
            .iter()
            .filter_map(|screen| screen.refresh(target, &host, &self.over, repo.clone()))
            .collect();
        if refreshes.is_empty() {
            return;
        }

        self.refresh_pending = refreshes.len();
        self.refresh_error = None;
        for refresh in refreshes {
            let Refresh {
                generation,
                load,
                apply,
            } = refresh;
            let loaded = cx.background_spawn(async move { load() });
            cx.spawn(async move |shell, cx: &mut AsyncApp| {
                let result = loaded.await;
                _ = shell.update(cx, move |shell, cx| {
                    let result = if refresh_id != shell.refresh_id || generation < shell.generation
                    {
                        Ok(())
                    } else {
                        result.and_then(|value| {
                            let host = config::host(cx);
                            apply(value, &host, cx)
                        })
                    };
                    shell.finish_refresh(refresh_id, result, cx);
                });
            })
            .detach();
        }
    }

    fn finish_refresh(
        &mut self,
        refresh_id: u64,
        result: Result<(), String>,
        cx: &mut Context<Self>,
    ) {
        if refresh_id != self.refresh_id {
            return;
        }
        if let Err(error) = result {
            self.refresh_error.get_or_insert(error);
        }
        self.refresh_pending = self.refresh_pending.saturating_sub(1);
        if self.refresh_pending == 0 {
            if self.error.is_none() {
                self.error = self.refresh_error.take().map(Into::into);
            } else {
                self.refresh_error = None;
            }
        }
        cx.notify();
    }

    fn invalidate_refresh(&mut self) {
        self.refresh_id = self.refresh_id.saturating_add(1);
        self.refresh_pending = 0;
        self.refresh_error = None;
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
                self.invalidate_refresh();
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
    /// makes it survive the next save, and what makes a colour in `gitten.toml`
    /// still count after one.
    fn set_theme(&mut self, name: String, cx: &mut Context<Self>) {
        cx.set_global(config::Chosen(Some(name)));
        for w in config::reload(&self.config, cx) {
            eprintln!("gitten: {w}");
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
            "input.accept" => self.close_input(true, cx),
            "input.cancel" => self.close_input(false, cx),
            "pane.next" => self.cycle_pane(1, cx),
            "pane.prev" => self.cycle_pane(-1, cx),
            "files.focus" => self.focus_named("files", cx),
            "commits.open-diff" => self.open_diff(cx),
            "commits.search" => self.begin_search(cx),
            // The working tree's verbs. Context comes from the focused pane,
            // the write from the job queue — and where either is missing, the
            // same honest sentence an unknown command gets.
            "files.stage" => self.stage_or_unstage(cx),
            "files.commit" => self.begin_commit_message(cx),
            "files.discard" => self.discard_selected(cx),
            "files.stage-all" => self.stage_all(cx),
            "files.ignore" => self.ignore_selected(cx),
            "copy.selection" => self.copy_selection(cx),
            // Both are answered by whichever screen is up; a commit graph has no
            // selection yet, and a command nothing handles there is inert — the
            // same answer an unbound key gives.
            "select.all" | "select.none" => {
                if let Some(input) = self.input.clone() {
                    input.update(cx, |input, cx| {
                        input.select_all_text(command == "select.all", cx)
                    });
                } else if let Some(screen) = self.active() {
                    screen.select(command == "select.all", cx);
                }
            }
            _ => {
                let known = match self.active() {
                    Some(screen) => {
                        let host = config::host(cx);
                        let writes = self.writes();
                        screen.run(command, &host, writes.as_ref(), cx)
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

    /// Closes the help, the picker over it, or the focused secondary pane.
    ///
    /// One key for all of it, because all of it is "get me out of this" — and
    /// **innermost first**, or a picker left open after its screen is popped
    /// keeps occluding nothing: invisible, but still in `self.open`, where
    /// [`DevShell::on_wheel`] swallows every event for it forever. So an open
    /// menu is the whole of this `esc`: closed, pending dropped with it, no
    /// selection cleared and no pane closed. A selection is inside a pane, so
    /// it goes next; the root pane is never closed at all — `esc` on the thing
    /// you started with is not a quit.
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
        if self.input.is_some() {
            self.close_input(false, cx);
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
        if self.panes.close_focused().is_some() {
            self.sync_modes();
            cx.notify();
        }
    }

    fn focus_pane(&mut self, at: usize, cx: &mut Context<Self>) {
        if self.panes.focus(at) {
            self.sync_modes();
            cx.notify();
        }
    }

    fn cycle_pane(&mut self, by: isize, cx: &mut Context<Self>) {
        if self.panes.cycle(by) {
            self.sync_modes();
            cx.notify();
        }
    }

    /// Focuses a tenant by its stable registration name — what `files.focus`
    /// runs. Said, not swallowed, when nothing is registered under the name: a
    /// fixture has no working tree to show, and the honest answer to the key
    /// is the same sentence an unbound one gets.
    fn focus_named(&mut self, name: &str, cx: &mut Context<Self>) {
        match self.panes.position(name) {
            Some(at) => self.focus_pane(at, cx),
            None => self.set_notice(format!("no {name} pane")),
        }
    }

    /// Opens the diff of the commit under the cursor in its registered pane.
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
        match gitten_app::acquire::acquire(View::Diff, &source, &host, Some(repo.as_ref())) {
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
                let screen = Screen::diff(
                    view,
                    source,
                    self.generation,
                    format!(
                        "{} {}",
                        &commit.sha[..commit.sha.len().min(8)],
                        commit.subject
                    ),
                );
                // Stable registration replaces an older diff tenant rather
                // than growing a second copy of the same panel.
                self.panes.register("diff", screen);
                self.sync_modes();
            }
            Err(e) => self.error = Some(e.into()),
        }
    }

    /// `copy.selection`: the mouse's selection, or the keyboard's row. The
    /// clipboard is the window system's here — [`Context::write_to_clipboard`]
    /// — which is why this lives beside dispatch and not in a view.
    fn copy_selection(&mut self, cx: &mut Context<Self>) {
        if let Some(input) = self.input.as_ref() {
            if let Some(text) = input.read(cx).selected_text() {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }
            return;
        }
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
            Some(Screen::Files { view, .. }) => {
                let view = view.clone();
                view.update(cx, |f, _| f.reconcile(&host));
                let text = view.read(cx).cursor_text();
                if !text.is_empty() {
                    // Letters first, then the path — the spelling git itself
                    // prints, so it pastes into a shell usefully.
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                }
            }
            Some(Screen::Custom(pane)) => {
                let writes = self.writes();
                pane.run("copy.selection", &host, writes.as_ref(), cx);
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
        let resolved = match self.input.is_some() {
            true => host.keys.resolve_mode_any(input::MODE, &typed),
            false => host.keys.resolve_any(&self.modes, &typed),
        };
        match resolved {
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
                if self.input.is_some() {
                    // Not an app command in this mode, so it is text-field
                    // mechanics or text for the platform input handler. Let it
                    // continue down the focus path untouched.
                    self.pending.clear();
                    return;
                }
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
        // Over one pane's rows, and not over the title bar or a dropdown above
        // them. Focus it before resolving the wheel: otherwise an unfocused
        // pane's native list scroller would become a second, unconfigured input
        // path when this capture handler stood aside.
        let Some(at) = self
            .panes
            .iter()
            .position(|screen| screen.list_bounds(cx).contains(&ev.position))
        else {
            return;
        };
        self.focus_pane(at, cx);
        let screen = self.panes.focused().clone();
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
        // — or one written in `gitten.toml` — is in this menu without a line here
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
        // Every registered tenant gets an equal vertical share. The wrapper is
        // the focus ring and the click target; the view inside still measures
        // its own box, so neither diff wrapping nor list virtualization learns
        // that panes exist.
        let focused_pane = self.panes.focused_index();
        let pane_views = self
            .panes
            .iter()
            .enumerate()
            .map(|(at, screen)| {
                let focused = at == focused_pane;
                // Equal shares by default. A tenant that claims a fixed slice
                // of the column gets it as a basis; the equal-share panes grow
                // into whatever is left.
                let sized = match screen.height_share(cx) {
                    Some(share) => div().flex_basis(relative(share)),
                    None => div().flex_1(),
                };
                sized
                    .id(("pane", at))
                    .relative()
                    .min_h_0()
                    .overflow_hidden()
                    .border_1()
                    .border_color(rgb(match focused {
                        true => c.accent,
                        false => c.border,
                    }))
                    .debug_selector(move || format!("pane-{at}"))
                    .capture_any_mouse_down(
                        cx.listener(move |this, _, _, cx| this.focus_pane(at, cx)),
                    )
                    .child(screen.any())
            })
            .collect::<Vec<_>>();
        let which = self.active_view_name();
        let strip = self.strip(&host, cx);
        let error = self.error.clone();
        let notice = self.notice.clone();
        let running = self
            .running
            .clone()
            .or_else(|| (self.refresh_pending > 0).then(|| "refreshing repository".to_string()));
        let input = self.input.clone();

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
        let desired_focus = input
            .as_ref()
            .map(|input| input.read(cx).focus_handle())
            .unwrap_or_else(|| self.focus.clone());
        if self.focused.as_ref() != Some(&desired_focus) {
            window.focus(&desired_focus, cx);
            self.focused = Some(desired_focus);
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
                    .child(div().flex_none().text_color(rgb(c.fg)).child("gitten"))
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
                            .child(self.active_label(cx)),
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
            .child(div().min_h_0().flex_grow(1.0).v_flex().children(pane_views))
            .children(input)
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
fn band(c: &gitten_core::theme::ChromePalette, text: SharedString, ink: Rgb) -> impl IntoElement {
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
    // Arguments, `gitten.toml`, `--help`, `gitten config` and acquisition, all of
    // it shared with every other client — see `gitten_app`. What is left in this
    // file is a window.
    let started = match Startup::new("gitten", View::Commits)
        .blurb("a git client")
        .extra(EXTRA)
        .go()
    {
        Ok(started) => started,
        Err(exit) => exit.finish(),
    };
    start::mark("startup done (args + gitten.toml + acquire)");
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
                gitten_git::diff(for_diff.as_ref(), revision, &host.differ, over)
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
        input::bind_keys(cx);
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
                "gitten: could not watch {}; config reload is off",
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
                    eprintln!("gitten: {w}");
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
                            Screen::commits(
                                e,
                                source.clone(),
                                Generation::default(),
                                label.clone(),
                            ),
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
                            Screen::diff(
                                e.clone(),
                                source.clone(),
                                Generation::default(),
                                label.clone(),
                            ),
                            v.rendered.clone(),
                            v.top.clone(),
                            v.total.clone(),
                            v.note.clone(),
                            v.load.clone(),
                        )
                    }
                };
                let mut initial_panes = panes::Panes::new(which_name, screen);

                // The working tree gets its compact pane, above wherever a
                // diff later opens. One blocking `git status` here, beside the
                // rest of startup acquisition; from the next write on, the
                // generation-guarded refresh path keeps it current. A fixture
                // has no repository and so no pane at all.
                if let Some((_, handle)) = &repo {
                    start::mark("files status begin");
                    let described = std::thread::scope(|s| {
                        // Beside, not behind — describe spawns and runs while
                        // status blocks, and is joined only once status is
                        // back. Joining before would put two git processes in
                        // sequence on the launch path.
                        let title = s.spawn(|| handle.describe());
                        let status = handle.status();
                        let title = title.join().unwrap_or_default();
                        match status {
                            Ok(status) => views::files::prepare(status, &title),
                            // Shown as a clean tree rather than failing the
                            // window: one bad status must not take the launch.
                            Err(e) => {
                                eprintln!("gitten: status failed, showing an empty pane: {e}");
                                views::files::prepare(Default::default(), &title)
                            }
                        }
                    });
                    start::mark("files status done");
                    let label = described.label.clone();
                    initial_panes.register(
                        "files",
                        Screen::files(
                            cx.new(|_| views::files::Files::from_prepared(described)),
                            Generation::default(),
                            label,
                        ),
                    );
                    // Registration focuses what it adds; startup keeps the
                    // keyboard where it launched.
                    initial_panes.focus(0);
                    start::mark("files pane built");
                }
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
                    panes: initial_panes,
                    stats,
                    rediff,
                    repo,
                    jobs,
                    submitter,
                    generation: Generation::default(),
                    refresh_id: 0,
                    refresh_pending: 0,
                    refresh_error: None,
                    running: None,
                    input: None,
                    prompt: None,
                    search_live: None,
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
                name: "gitten".into(),
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
    fn active_label(&self, cx: &App) -> SharedString {
        SharedString::from(
            self.active()
                .map(|screen| screen.label(cx))
                .unwrap_or_else(|| self.which.to_string()),
        )
    }
}

/// The window's own title — what macOS shows in Mission Control, the Window menu
/// and the tab bar. Not what is drawn in the strip: that is three separate
/// colours in [`DevShell::render`], because "gitten", the view and the repository
/// are three different kinds of thing and one grey run of text says so about
/// none of them.
///
/// `Started::title` is the shared one; this exists because the window is opened
/// after the `Started` has been taken apart, and reassembling it to ask would be
/// sillier than the two lines.
fn started_title(view: View, label: &str) -> String {
    format!("gitten · {} · {label}", view.name())
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
    use super::{config, input, panes, DevShell, Open, Pane, Refresh, Screen, Writes};
    use crate::views::commits::Commits;
    use gitten_app::cli::Source;
    use gitten_app::jobs::{Event as JobEvent, Generation, Job, Runner, Submitter};
    use gitten_core::command::{Code, Key, Keymap, Modes, Resolve};
    use gitten_core::host::Host;
    use gitten_core::status::Status;
    use gitten_core::Commit;
    use gitten_git::{Pair, Repo};
    use gpui::{AppContext as _, TestAppContext};
    use std::cell::Cell;
    use std::path::PathBuf;
    use std::rc::Rc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    struct ExtensionPane {
        view: gpui::Entity<Commits>,
        ran: Rc<Cell<bool>>,
        generation: Rc<Cell<Generation>>,
    }

    impl ExtensionPane {
        /// What a verb looks like from a pane that did not ship with the app:
        /// aim a [`gitten_app::verbs::Write`] at the handed repository and
        /// hand it to the handed queue. No field of the shell is reached, no
        /// built-in is special-cased — these are the same two rails
        /// `files.stage` rides.
        fn stage_like_a_builtin(&self, writes: &Writes) -> bool {
            let job = gitten_app::verbs::Write::stage(&writes.repo, b"notes.md".to_vec());
            let queued = writes.send(Box::new(job));
            self.ran.set(queued);
            queued
        }
    }

    impl Pane for ExtensionPane {
        fn any(&self) -> gpui::AnyView {
            self.view.clone().into()
        }

        fn mode(&self) -> &'static str {
            "extension"
        }

        fn label(&self, _: &gpui::App) -> String {
            "extension pane".into()
        }

        fn refresh(
            &self,
            target: Generation,
            _: &Host,
            _: &gitten_core::differ::Overrides,
            repo: gitten_git::Handle,
        ) -> Option<Refresh> {
            if self.generation.get() >= target {
                return None;
            }
            let generation = self.generation.clone();
            Some(Refresh::new(
                target,
                move || {
                    repo.status()?;
                    Ok("extension-owned data".to_string())
                },
                move |value: String, _, _| {
                    assert_eq!(value, "extension-owned data");
                    generation.set(target);
                    Ok(())
                },
            ))
        }

        fn run(&self, command: &str, _: &Host, writes: Option<&Writes>, _: &mut gpui::App) -> bool {
            match (command, writes) {
                ("extension.toggle", _) => {
                    self.ran.set(true);
                    true
                }
                // A fixture has no repository to write through; `None` here is
                // the same honest nothing a built-in verb answers.
                ("extension.stage", Some(writes)) => self.stage_like_a_builtin(writes),
                _ => false,
            }
        }
    }

    struct Succeed;

    impl Job for Succeed {
        fn name(&self) -> &str {
            "succeed"
        }

        fn run(self: Box<Self>) -> Result<(), String> {
            Ok(())
        }
    }

    fn successful_generation() -> Generation {
        let runner = Runner::new();
        runner
            .submitter()
            .submit(Box::new(Succeed))
            .unwrap_or_else(|_| panic!("runner rejected a job"));
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            while let Some(event) = runner.try_next() {
                if let JobEvent::Finished {
                    outcome: Ok(generation),
                    ..
                } = event
                {
                    return generation;
                }
            }
            assert!(Instant::now() < deadline, "job did not finish");
            std::thread::yield_now();
        }
    }

    struct RefreshRepo {
        calls: Arc<AtomicUsize>,
    }

    impl Repo for RefreshRepo {
        fn log(&self, _: usize) -> gitten_git::Result<Vec<Commit>> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(vec![Commit {
                sha: "1".into(),
                short: "1".into(),
                parents: Box::from(&[][..]),
                author: "test".into(),
                timestamp: 0,
                subject: "refreshed".into(),
            }])
        }

        fn pairs(&self, _: &str) -> gitten_git::Result<Vec<Pair>> {
            Ok(Vec::new())
        }

        fn status(&self) -> gitten_git::Result<Status> {
            Ok(Status::default())
        }

        fn describe(&self) -> String {
            "refreshed".into()
        }
    }

    /// A shell on one commits screen, with whatever picker is named open and
    /// one key of a chord half-typed — the state `esc` meets most often.
    fn shell(which: Option<Open>, cx: &mut TestAppContext) -> gpui::Entity<DevShell> {
        cx.new(|cx| {
            let commits = cx.new(|_| Commits::new(Vec::new(), Rc::new(Host::new())));
            let jobs = Runner::new();
            DevShell {
                which: "commits",
                panes: panes::Panes::new(
                    "commits",
                    Screen::commits(commits, Source::Fixtures, Generation::default(), "repo"),
                ),
                stats: None,
                rediff: None,
                repo: None,
                submitter: jobs.submitter(),
                jobs,
                generation: Generation::default(),
                refresh_id: 0,
                refresh_pending: 0,
                refresh_error: None,
                running: None,
                input: None,
                prompt: None,
                search_live: None,
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
                assert_eq!(s.panes.len(), 1, "{which:?}: esc closed the pane too");
                assert!(!s.help, "{which:?}: esc reached past the menu");
            });
        }
    }

    #[gpui::test]
    fn esc_without_a_picker_closes_only_the_focused_secondary_pane(cx: &mut TestAppContext) {
        // The control: nothing open, two panes — back closes the focused
        // secondary and never closes the root.
        let shell = shell(None, cx);
        shell.update(cx, |s, cx| {
            let commits = cx.new(|_| Commits::new(Vec::new(), Rc::new(Host::new())));
            s.panes.register(
                "second",
                Screen::commits(commits, Source::Fixtures, Generation::default(), "second"),
            );
            s.sync_modes();
        });
        assert_eq!(shell.read_with(cx, |s, _| s.panes.len()), 2);
        shell.update(cx, |s, cx| s.back(cx));
        shell.read_with(cx, |s, _| {
            assert_eq!(s.panes.len(), 1, "the secondary pane was not closed");
            assert!(s.open.is_none());
        });
        // And on the last screen esc stops: it was never a quit.
        shell.update(cx, |s, cx| s.back(cx));
        shell.read_with(cx, |s, _| assert_eq!(s.panes.len(), 1));
    }

    #[gpui::test]
    fn pane_commands_move_focus_and_rebuild_the_effective_modes(cx: &mut TestAppContext) {
        let shell = shell(None, cx);
        shell.update(cx, |shell, cx| {
            let commits = cx.new(|_| Commits::new(Vec::new(), Rc::new(Host::new())));
            shell.panes.register(
                "second",
                Screen::commits(commits, Source::Fixtures, Generation::default(), "second"),
            );
            shell.sync_modes();
        });
        shell.read_with(cx, |shell, app| {
            assert_eq!(shell.active_label(app).as_ref(), "second");
            assert_eq!(shell.modes.top(), "commits");
        });

        shell.update(cx, |shell, cx| shell.run_command("pane.prev", cx));
        shell.read_with(cx, |shell, app| {
            assert_eq!(shell.active_label(app).as_ref(), "repo");
            assert_eq!(shell.modes.as_slice(), &["global", panes::MODE, "commits"]);
            let mut keys = Keymap::builtin();
            keys.bind("commits", "ctrl-j", "view.down").unwrap();
            assert_eq!(
                keys.resolve(
                    &shell.modes,
                    &[Key::new(Code::Char('j'), true, false, false)]
                ),
                Resolve::Run("view.down"),
                "the focused tenant did not override the pane container"
            );
        });

        shell.update(cx, |shell, cx| shell.run_command("pane.next", cx));
        shell.read_with(cx, |shell, app| {
            assert_eq!(shell.active_label(app).as_ref(), "second");
            assert_eq!(shell.panes.focused_index(), 1);
        });
    }

    #[gpui::test]
    fn a_compiled_in_extension_registers_a_pane_without_a_screen_variant(cx: &mut TestAppContext) {
        let shell = shell(None, cx);
        let view = cx.new(|_| Commits::new(Vec::new(), Rc::new(Host::new())));
        let ran = Rc::new(Cell::new(false));
        let refreshed = Rc::new(Cell::new(Generation::default()));
        let target = successful_generation();
        shell.update(cx, |shell, cx| {
            cx.set_global(config::Active(Rc::new(Host::new())));
            shell.register_pane(
                "extension",
                ExtensionPane {
                    view,
                    ran: ran.clone(),
                    generation: refreshed.clone(),
                },
                cx,
            );
            shell.run_command("extension.toggle", cx);
            shell.repo = Some((
                PathBuf::from("/fake"),
                Arc::new(RefreshRepo {
                    calls: Arc::new(AtomicUsize::new(0)),
                }),
            ));
            shell.generation = target;
            shell.refresh_stale(cx);
        });
        cx.run_until_parked();
        shell.read_with(cx, |shell, app| {
            assert_eq!(shell.active_label(app).as_ref(), "extension pane");
            assert_eq!(shell.active_view_name(), "extension");
            assert_eq!(shell.panes.len(), 2);
        });
        assert!(ran.get(), "the extension's command did not reach its pane");
        assert_eq!(refreshed.get(), target);
    }

    #[gpui::test]
    fn one_generation_refreshes_every_visible_repository_pane(cx: &mut TestAppContext) {
        let shell = shell(None, cx);
        let generation = successful_generation();
        let calls = Arc::new(AtomicUsize::new(0));
        let path = PathBuf::from("/fake");
        let source = Source::Repo {
            path: path.clone(),
            arg: "1".into(),
        };
        shell.update(cx, |shell, cx| {
            let root = cx.new(|_| Commits::new(Vec::new(), Rc::new(Host::new())));
            shell.panes = panes::Panes::new(
                "commits",
                Screen::commits(root, source.clone(), Generation::default(), "stale root"),
            );
            let second = cx.new(|_| Commits::new(Vec::new(), Rc::new(Host::new())));
            shell.panes.register(
                "second",
                Screen::commits(second, source.clone(), Generation::default(), "stale"),
            );
            shell.repo = Some((
                path,
                Arc::new(RefreshRepo {
                    calls: calls.clone(),
                }),
            ));
            shell.generation = generation;
            shell.running = Some("running next write".into());
            cx.set_global(config::Active(Rc::new(Host::new())));
            shell.refresh_stale(cx);
        });
        assert_eq!(
            calls.load(Ordering::Relaxed),
            0,
            "repository refresh blocked the GPUI update"
        );
        cx.run_until_parked();

        shell.read_with(cx, |shell, cx| {
            for pane in shell.panes.iter() {
                let Screen::Commits {
                    view,
                    generation: pane_generation,
                    ..
                } = pane
                else {
                    panic!("test registered only commit panes");
                };
                assert_eq!(view.read(cx).total(), 1);
                assert_eq!(pane_generation.get(), generation);
            }
            assert!(shell.error.is_none());
            assert_eq!(shell.refresh_pending, 0);
            assert_eq!(shell.running.as_deref(), Some("running next write"));
        });
        assert_eq!(calls.load(Ordering::Relaxed), 2);
    }

    #[gpui::test]
    fn two_registered_panes_are_stacked_into_equal_nonzero_boxes(cx: &mut TestAppContext) {
        let shell = shell(None, cx);
        shell.update(cx, |shell, cx| {
            let commits = cx.new(|_| Commits::new(Vec::new(), Rc::new(Host::new())));
            shell.panes.register(
                "second",
                Screen::commits(commits, Source::Fixtures, Generation::default(), "second"),
            );
            shell.sync_modes();
        });
        let observed = shell.clone();
        let handle = cx.update(|cx| {
            gpui_component::init(cx);
            cx.set_global(config::Active(Rc::new(Host::new())));
            cx.open_window(
                gpui::WindowOptions {
                    window_bounds: Some(gpui::WindowBounds::Windowed(gpui::Bounds {
                        origin: Default::default(),
                        size: gpui::size(gpui::px(800.0), gpui::px(600.0)),
                    })),
                    ..Default::default()
                },
                move |_, _| shell,
            )
            .unwrap()
        });
        let mut cx = gpui::VisualTestContext::from_window(handle.into(), cx);
        cx.run_until_parked();
        let first = cx.debug_bounds("pane-0").expect("first pane was not drawn");
        let second = cx
            .debug_bounds("pane-1")
            .expect("second pane was not drawn");

        assert!(first.size.height > gpui::px(0.0));
        assert!(second.size.height > gpui::px(0.0));
        assert_eq!(first.origin.x, second.origin.x);
        assert_eq!(first.size.width, second.size.width);
        assert_eq!(first.bottom(), second.top());
        assert!(
            (f32::from(first.size.height) - f32::from(second.size.height)).abs() < 1.0,
            "pane heights differ: {} and {}",
            first.size.height,
            second.size.height
        );

        cx.simulate_event(gpui::ScrollWheelEvent {
            position: first.center(),
            delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(0.0), gpui::px(1.0))),
            ..Default::default()
        });
        assert_eq!(
            observed.read_with(&cx, |shell, _| shell.panes.focused_index()),
            0
        );
        cx.simulate_click(second.center(), gpui::Modifiers::default());
        assert_eq!(
            observed.read_with(&cx, |shell, _| shell.panes.focused_index()),
            1
        );
        cx.simulate_click(first.center(), gpui::Modifiers::default());
        assert_eq!(
            observed.read_with(&cx, |shell, _| shell.panes.focused_index()),
            0
        );
        cx.simulate_keystrokes("ctrl-j");
        assert_eq!(
            observed.read_with(&cx, |shell, _| shell.panes.focused_index()),
            1
        );
    }

    #[gpui::test]
    fn files_focus_reaches_the_registered_pane_and_says_so_when_there_is_none(
        cx: &mut TestAppContext,
    ) {
        let shell = shell(None, cx);
        shell.update(cx, |shell, cx| {
            let files = cx.new(|_| {
                crate::views::files::Files::from_prepared(crate::views::files::prepare(
                    Status::default(),
                    "gitten (main)",
                ))
            });
            shell.panes.register(
                "files",
                Screen::files(files, Generation::default(), "gitten (main) · 0 changed"),
            );
            // Registration focuses what it adds; a launch starts on the root.
            shell.panes.focus(0);
            shell.sync_modes();
        });
        shell.read_with(cx, |shell, _| {
            assert_eq!(shell.active_view_name(), "commits")
        });

        // Named dispatch — the same path the `2` key resolves through.
        shell.update(cx, |shell, cx| shell.run_command("files.focus", cx));
        shell.read_with(cx, |shell, app| {
            assert_eq!(shell.panes.focused_index(), 1);
            assert_eq!(shell.modes.top(), "files");
            assert_eq!(
                shell.active_label(app).as_ref(),
                "gitten (main) · 0 changed"
            );
        });

        // And with the pane gone again, the key is answered with a sentence,
        // not silence.
        shell.update(cx, |shell, cx| {
            shell.panes.close_focused();
            shell.sync_modes();
            shell.notice = None;
            shell.run_command("files.focus", cx);
        });
        shell.read_with(cx, |shell, _| {
            assert!(shell.notice.is_some(), "a missing pane went unsaid");
        });
    }

    #[gpui::test]
    fn native_input_owns_the_innermost_mode_until_it_closes(cx: &mut TestAppContext) {
        let shell = shell(None, cx);
        let input = cx.new(|cx| input::Input::new("message", "type", "draft", cx));
        shell.update(cx, |shell, cx| shell.open_input(input.clone(), cx));
        assert_eq!(
            shell.read_with(cx, |shell, _| shell.modes.top().to_string()),
            input::MODE
        );
        shell.update(cx, |shell, cx| shell.run_command("select.all", cx));
        assert_eq!(
            input.read_with(cx, |input, _| input.selected_text()),
            Some("draft".into())
        );

        shell.update(cx, |shell, cx| shell.run_command("input.cancel", cx));
        shell.read_with(cx, |shell, _| {
            assert!(shell.input.is_none());
            assert_eq!(shell.modes.top(), "commits");
        });
    }

    // ---------------------------------------------------------- the search

    /// A history to filter: alternating authors and subjects, so any query
    /// keeps a known half and discards the rest.
    fn search_commit(n: usize) -> Commit {
        let even = n.is_multiple_of(2);
        Commit {
            sha: format!("{n:040x}"),
            short: format!("abc00{n}"),
            parents: Box::from(&[][..]),
            author: Arc::from(if even { "ada" } else { "grace" }),
            timestamp: 1_700_000_000 + n as i64,
            subject: if even {
                format!("engine note {n}")
            } else {
                format!("compiler pass {n}")
            },
        }
    }

    fn search_history() -> Vec<Commit> {
        (0..30).map(search_commit).collect()
    }

    /// [`shell`] is built with an empty pane; this one carries rows, which is
    /// what every search test needs to see move. Replacing by name keeps the
    /// root slot — and focus — exactly where they were.
    fn commits_shell(cx: &mut TestAppContext) -> gpui::Entity<DevShell> {
        let shell = shell(None, cx);
        shell.update(cx, |shell, cx| {
            let view = cx.new(|_| Commits::new(search_history(), Rc::new(Host::new())));
            // A live edit reconciles against the file's settings, like every
            // other reader of the host.
            cx.set_global(config::Active(Rc::new(Host::new())));
            shell.panes.register(
                "commits",
                Screen::commits(view, Source::Fixtures, Generation::default(), "~/src"),
            );
            shell.sync_modes();
        });
        shell
    }

    /// The focused commits pane, for reading what a search did to it.
    fn commits_view(shell: &gpui::Entity<DevShell>, cx: &TestAppContext) -> gpui::Entity<Commits> {
        match shell.read_with(cx, |shell, _| shell.active().cloned()) {
            Some(Screen::Commits { view, .. }) => view.clone(),
            _ => panic!("no commits pane under the keyboard"),
        }
    }

    /// Opens the prompt and types `query` into it the way the platform would:
    /// through the field's own edit path, whose event the live filter rides.
    #[track_caller]
    fn type_query(shell: &gpui::Entity<DevShell>, cx: &mut TestAppContext, query: &str) {
        shell.update(cx, |shell, cx| shell.run_command("commits.search", cx));
        let typed = shell.read_with(cx, |shell, _| shell.input.clone());
        let Some(field) = typed else {
            panic!("no field opened");
        };
        field.update(cx, |field, cx| field.replace(None, query, cx));
        cx.run_until_parked();
    }

    #[gpui::test]
    fn slash_opens_a_live_search_and_enter_leaves_what_it_found(cx: &mut TestAppContext) {
        let shell = commits_shell(cx);
        type_query(&shell, cx, "engine");

        shell.read_with(cx, |shell, _| {
            assert_eq!(shell.modes.top(), input::MODE, "the field owns the keys");
            assert!(
                matches!(shell.prompt, Some(super::Prompt::Search { .. })),
                "{:?}",
                shell.prompt
            );
        });
        let view = commits_view(&shell, cx);
        view.read_with(cx, |v, _| {
            assert_eq!(v.rows(), 15, "the list filtered while typing");
            assert_eq!(v.filter_note().as_deref(), Some("15/30"));
        });

        // Enter closes with the query standing; the count stays in the title.
        shell.update(cx, |shell, cx| shell.run_command("input.accept", cx));
        shell.read_with(cx, |shell, app| {
            assert!(shell.input.is_none());
            assert_eq!(shell.modes.top(), "commits");
            assert_eq!(shell.active_label(app).as_ref(), "~/src · 15/30");
        });
        view.read_with(cx, |v, _| {
            assert_eq!(v.rows(), 15, "accept kept the filter")
        });
    }

    #[gpui::test]
    fn esc_clears_the_filter_along_with_the_prompt(cx: &mut TestAppContext) {
        let shell = commits_shell(cx);
        type_query(&shell, cx, "engine");
        let view = commits_view(&shell, cx);
        view.read_with(cx, |v, _| assert_eq!(v.rows(), 15));

        // The real exit key: `back` finds the input and cancels it.
        shell.update(cx, |shell, cx| shell.back(cx));
        shell.read_with(cx, |shell, app| {
            assert!(shell.input.is_none());
            assert_eq!(shell.active_label(app).as_ref(), "~/src", "restored");
        });
        view.read_with(cx, |v, _| assert_eq!(v.rows(), 30, "nothing left standing"));
    }

    #[gpui::test]
    fn a_second_slash_edits_the_standing_query(cx: &mut TestAppContext) {
        let shell = commits_shell(cx);
        type_query(&shell, cx, "engine");
        shell.update(cx, |shell, cx| shell.run_command("input.accept", cx));

        // Reopen: the pane still holds the query, so the field starts from
        // it rather than from nothing.
        type_query(&shell, cx, "");
        let value = shell.read_with(cx, |shell, app| {
            shell
                .input
                .as_ref()
                .map(|field| field.read(app).value().to_string())
        });
        assert_eq!(value.as_deref(), Some("engine"));

        // Narrowing from there filters live from the longer query.
        let typed = shell.read_with(cx, |shell, _| shell.input.clone().unwrap());
        typed.update(cx, |field, cx| field.replace(None, " note 3", cx));
        cx.run_until_parked();
        let view = commits_view(&shell, cx);
        view.read_with(cx, |v, _| {
            assert!(matches!(v.query(), Some(q) if q.contains("note 3")));
            assert!(v.rows() < 15, "the edited query applied live");
        });
    }

    #[gpui::test]
    fn an_empty_accept_takes_the_filter_back_off(cx: &mut TestAppContext) {
        let shell = commits_shell(cx);

        // Accepting an untouched prompt clears nothing because there is
        // nothing to clear — and says so by leaving the label alone.
        type_query(&shell, cx, "");
        shell.update(cx, |shell, cx| shell.run_command("input.accept", cx));
        shell.read_with(cx, |shell, app| {
            assert_eq!(shell.active_label(app).as_ref(), "~/src");
        });

        // A standing filter comes off live the moment the field is emptied:
        // cmd-a and delete are enough, before enter even lands.
        type_query(&shell, cx, "engine");
        let typed = shell.read_with(cx, |shell, _| shell.input.clone().unwrap());
        typed.update(cx, |field, cx| {
            field.select_all_text(true, cx);
            field.replace(None, "", cx);
        });
        cx.run_until_parked();
        let view = commits_view(&shell, cx);
        view.read_with(cx, |v, _| {
            assert_eq!(v.rows(), 30, "cleared while still open")
        });
        shell.update(cx, |shell, cx| shell.run_command("input.accept", cx));
        shell.read_with(cx, |shell, app| {
            assert_eq!(shell.active_label(app).as_ref(), "~/src");
        });
    }

    #[gpui::test]
    fn search_says_so_where_nothing_answers_it(cx: &mut TestAppContext) {
        // No repository data at all is still a commits screen — it answers.
        let shell = shell(None, cx);
        shell.update(cx, |shell, cx| shell.run_command("commits.search", cx));
        shell.read_with(cx, |shell, _| assert!(shell.input.is_some()));

        // But the command belongs to the commits mode: over the working tree
        // it resolves to nothing that can act, and is said rather than done.
        let (shell, _repo, _handle) = files_shell(cx);
        shell.update(cx, |shell, cx| shell.run_command("commits.search", cx));
        shell.read_with(cx, |shell, _| {
            assert!(shell.input.is_none(), "a prompt opened over the files pane");
            assert!(
                shell
                    .notice
                    .as_deref()
                    .unwrap_or_default()
                    .contains("not supported here"),
                "{:?}",
                shell.notice
            );
        });
    }

    #[gpui::test]
    fn the_files_pane_claims_a_fixed_slice_and_equal_shares_split_the_rest(
        cx: &mut TestAppContext,
    ) {
        let shell = shell(None, cx);
        shell.update(cx, |shell, cx| {
            let commits = cx.new(|_| Commits::new(Vec::new(), Rc::new(Host::new())));
            shell.panes.register(
                "second",
                Screen::commits(commits, Source::Fixtures, Generation::default(), "second"),
            );
            let files = cx.new(|_| {
                crate::views::files::Files::from_prepared(crate::views::files::prepare(
                    Status::default(),
                    "",
                ))
            });
            shell.panes.register(
                "files",
                Screen::files(files, Generation::default(), "files"),
            );
            shell.sync_modes();
        });
        let handle = cx.update(|cx| {
            gpui_component::init(cx);
            cx.set_global(config::Active(Rc::new(Host::new())));
            cx.open_window(
                gpui::WindowOptions {
                    window_bounds: Some(gpui::WindowBounds::Windowed(gpui::Bounds {
                        origin: Default::default(),
                        size: gpui::size(gpui::px(800.0), gpui::px(600.0)),
                    })),
                    ..Default::default()
                },
                move |_, _| shell,
            )
            .unwrap()
        });
        let mut cx = gpui::VisualTestContext::from_window(handle.into(), cx);
        cx.run_until_parked();
        let first = cx.debug_bounds("pane-0").expect("first pane was not drawn");
        let second = cx
            .debug_bounds("pane-1")
            .expect("second pane was not drawn");
        let files = cx.debug_bounds("pane-2").expect("files pane was not drawn");

        // The two unclaimed panes stay equal, and the working tree sits in its
        // slice — 0.3 of the column against their 0.35 each.
        assert!(
            (f32::from(first.size.height) - f32::from(second.size.height)).abs() < 1.0,
            "equal shares diverged"
        );
        let ratio = f32::from(files.size.height) / f32::from(first.size.height);
        assert!(
            (ratio - 0.3 / 0.35).abs() < 0.02,
            "the files pane took {ratio} of an equal share"
        );
        // Stacked in registration order, touching, full width.
        assert_eq!(files.origin.y, second.bottom());
        assert_eq!(files.origin.x, first.origin.x);
        assert_eq!(files.size.width, first.size.width);
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

    // ---------------------------------------------------------- the file verbs

    struct RecordingRepo {
        calls: Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl RecordingRepo {
        fn wrote(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    /// Serves every verb by writing down what was asked of it — the fake a
    /// dispatch test needs, standing where the binary implementation stands in
    /// a live window.
    impl Repo for RecordingRepo {
        fn log(&self, _: usize) -> gitten_git::Result<Vec<Commit>> {
            Ok(Vec::new())
        }

        fn pairs(&self, _: &str) -> gitten_git::Result<Vec<Pair>> {
            Ok(Vec::new())
        }

        fn status(&self) -> gitten_git::Result<Status> {
            // A standing tree rather than an empty answer: a real repository
            // still has changes after a write, and the re-acquire a successful
            // job schedules must find rows to put the keyboard back on.
            let mut tree = Status::default();
            tree.staged.push(gitten_core::status::StagedEntry {
                path: "gone.txt".into(),
                change: gitten_core::status::Change::Deleted,
                old_path: None,
                kind: gitten_core::status::Kind::File,
                submodule: Default::default(),
            });
            tree.unstaged.push(gitten_core::status::UnstagedEntry {
                path: "notes.md".into(),
                change: gitten_core::status::Change::Modified,
                kind: gitten_core::status::Kind::File,
                submodule: Default::default(),
            });
            Ok(tree)
        }

        fn describe(&self) -> String {
            "recorded".into()
        }

        fn stage(&self, path: &[u8]) -> gitten_git::Result<()> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("stage {}", String::from_utf8_lossy(path)));
            Ok(())
        }

        fn unstage(&self, path: &[u8]) -> gitten_git::Result<()> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("unstage {}", String::from_utf8_lossy(path)));
            Ok(())
        }

        fn commit(&self, message: &str) -> gitten_git::Result<String> {
            self.calls.lock().unwrap().push(format!("commit {message}"));
            Ok("f00d".into())
        }

        fn discard(&self, path: &[u8]) -> gitten_git::Result<()> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("discard {}", String::from_utf8_lossy(path)));
            Ok(())
        }

        fn remove_untracked(&self, path: &[u8]) -> gitten_git::Result<()> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("delete {}", String::from_utf8_lossy(path)));
            Ok(())
        }

        fn ignore(&self, path: &[u8]) -> gitten_git::Result<()> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("ignore {}", String::from_utf8_lossy(path)));
            Ok(())
        }

        fn stage_many(&self, paths: &[&[u8]]) -> gitten_git::Result<()> {
            let shown = paths
                .iter()
                .map(|p| String::from_utf8_lossy(p).into_owned())
                .collect::<Vec<_>>()
                .join(", ");
            self.calls
                .lock()
                .unwrap()
                .push(format!("stage-many {shown}"));
            Ok(())
        }

        fn unstage_many(&self, paths: &[&[u8]]) -> gitten_git::Result<()> {
            let shown = paths
                .iter()
                .map(|p| String::from_utf8_lossy(p).into_owned())
                .collect::<Vec<_>>()
                .join(", ");
            self.calls
                .lock()
                .unwrap()
                .push(format!("unstage-many {shown}"));
            Ok(())
        }
    }

    /// A shell with the two startup panes and a repository behind the files
    /// pane, whose tree carries one staged change and one unstaged one.
    /// Registration leaves the keyboard on the working tree. Returns the
    /// recording repository and its handle, so a test can both assert what
    /// was asked of it and aim writes of its own.
    fn files_shell(
        cx: &mut TestAppContext,
    ) -> (
        gpui::Entity<DevShell>,
        Arc<RecordingRepo>,
        gitten_git::Handle,
    ) {
        let mut tree = Status::default();
        tree.staged.push(gitten_core::status::StagedEntry {
            path: "gone.txt".into(),
            change: gitten_core::status::Change::Deleted,
            old_path: None,
            kind: gitten_core::status::Kind::File,
            submodule: Default::default(),
        });
        tree.unstaged.push(gitten_core::status::UnstagedEntry {
            path: "notes.md".into(),
            change: gitten_core::status::Change::Modified,
            kind: gitten_core::status::Kind::File,
            submodule: Default::default(),
        });
        tree_shell(cx, tree)
    }

    /// The same shell over any tree a test names — the untracked and
    /// conflict rows the fixed fixture above does not carry.
    fn tree_shell(
        cx: &mut TestAppContext,
        tree: Status,
    ) -> (
        gpui::Entity<DevShell>,
        Arc<RecordingRepo>,
        gitten_git::Handle,
    ) {
        let calls = Arc::default();
        let repo = Arc::new(RecordingRepo {
            calls: Arc::clone(&calls),
        });
        let handle: gitten_git::Handle = repo.clone();
        let shell = shell(None, cx);
        shell.update(cx, |shell, cx| {
            let host = Rc::new(Host::new());
            let files = cx.new(|_| {
                crate::views::files::Files::from_prepared(crate::views::files::prepare(tree, "r"))
            });
            files.update(cx, |f, _| {
                f.run_view("view.bottom", &host); // onto the last row: a file
            });
            shell.panes.register(
                "files",
                Screen::files(files, Generation::default(), "files"),
            );
            shell.sync_modes();
            shell.repo = Some((PathBuf::from("/recorded"), handle.clone()));
            cx.set_global(config::Active(Rc::new(Host::new())));
        });
        (shell, repo, handle)
    }

    /// Puts a caller-visible runner pair into the shell, so a test submits
    /// into the very queue [`DevShell::drain_jobs`] drains.
    fn wire_runner(shell: &gpui::Entity<DevShell>, cx: &mut TestAppContext) -> Submitter {
        let jobs = Runner::new();
        let submitter = jobs.submitter();
        shell.update(cx, |shell, _| {
            shell.jobs = jobs;
            shell.submitter = submitter.clone();
        });
        submitter
    }

    /// Drives the production pump — [`DevShell::drain_jobs`], the same call
    /// the live window makes from its timer — until `done` says an event has
    /// landed. No test-local event reading; what a window does, this does.
    #[track_caller]
    fn pump_until(
        shell: &gpui::Entity<DevShell>,
        cx: &mut TestAppContext,
        done: impl Fn(&DevShell) -> bool,
    ) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            shell.update(cx, |shell, cx| shell.drain_jobs(cx));
            if shell.read_with(cx, |shell, _| done(shell)) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "the pump never saw the event land"
            );
            std::thread::yield_now();
        }
    }

    /// Waits through the pump for one successful write, then lets the refresh
    /// it scheduled apply — the whole production path from queue to screen.
    #[track_caller]
    fn pump_write(shell: &gpui::Entity<DevShell>, cx: &mut TestAppContext) {
        let before = shell.read_with(cx, |shell, _| shell.generation);
        pump_until(shell, cx, |shell| shell.generation > before);
        cx.run_until_parked();
    }

    struct Fails;

    impl Job for Fails {
        fn name(&self) -> &str {
            "fails"
        }

        fn run(self: Box<Self>) -> Result<(), String> {
            Err("git commit: hook declined".into())
        }
    }

    #[gpui::test]
    fn space_acts_on_the_row_by_the_side_it_sits_on(cx: &mut TestAppContext) {
        let (shell, repo, _handle) = files_shell(cx);
        // The cursor starts on the last row: notes.md, under *unstaged*.
        shell.update(cx, |shell, cx| shell.run_command("files.stage", cx));
        pump_write(&shell, cx);
        assert_eq!(repo.wrote(), vec!["stage notes.md"]);
        // The refresh rails ran: the pane re-acquired against the (empty)
        // post-write tree, which is what makes the file visibly move.
        shell.read_with(cx, |shell, _| {
            assert!(shell.generation.get() > 0);
        });

        // Back to the top, down onto gone.txt under *staged*: same key, other
        // direction.
        shell.update(cx, |shell, cx| {
            let Some(Screen::Files { view, .. }) = shell.active() else {
                panic!("files pane lost");
            };
            let host = Rc::new(Host::new());
            view.update(cx, |f, _| {
                f.run_view("view.top", &host);
                f.run_view("view.down", &host);
            });
        });
        shell.update(cx, |shell, cx| shell.run_command("files.stage", cx));
        pump_write(&shell, cx);
        assert_eq!(repo.wrote(), vec!["stage notes.md", "unstage gone.txt"]);
    }

    #[gpui::test]
    fn commit_opens_the_field_and_the_accepted_text_becomes_the_job(cx: &mut TestAppContext) {
        let (shell, repo, _handle) = files_shell(cx);

        shell.update(cx, |shell, cx| shell.run_command("files.commit", cx));
        shell.read_with(cx, |shell, _| {
            assert!(shell.input.is_some(), "no field opened");
            assert_eq!(
                shell.modes.top(),
                input::MODE,
                "the field did not own the keyboard"
            );
        });

        // Typed text, as the platform would have left it; the rest of this
        // test drives the real accept path.
        shell.update(cx, |shell, cx| {
            let text = "fix: the \"thing\"\n\nand a body";
            let field = cx.new(|cx| input::Input::new("commit", "commit message", text, cx));
            shell.open_input(field, cx);
            shell.prompt = Some(super::Prompt::CommitMessage);
        });
        shell.update(cx, |shell, cx| shell.run_command("input.accept", cx));

        pump_write(&shell, cx);
        shell.read_with(cx, |shell, _| {
            assert!(shell.input.is_none(), "the field stayed open");
        });
        assert_eq!(
            repo.wrote(),
            vec![format!("commit fix: the \"thing\"\n\nand a body")],
            "the message arrived byte-for-byte"
        );
    }

    #[gpui::test]
    fn an_empty_commit_refuses_at_accept_without_submitting_anything(cx: &mut TestAppContext) {
        let (shell, repo, _handle) = files_shell(cx);
        shell.update(cx, |shell, cx| shell.run_command("files.commit", cx));
        // The field opens empty and is accepted empty — the shape of hitting
        // enter on an untouched prompt.
        shell.update(cx, |shell, cx| shell.run_command("input.accept", cx));
        shell.read_with(cx, |shell, _| {
            assert!(shell.input.is_none());
            assert!(
                shell
                    .notice
                    .as_deref()
                    .unwrap_or_default()
                    .contains("message"),
                "the refusal went unsaid: {:?}",
                shell.notice
            );
        });
        // Nothing ever reached the queue, however long the worker waits.
        std::thread::sleep(Duration::from_millis(100));
        shell.read_with(cx, |shell, _| {
            if let Some(event) = shell.jobs.try_next() {
                panic!("a refused commit still submitted: {event:?}");
            }
        });
        assert!(repo.wrote().is_empty());
    }

    #[gpui::test]
    fn a_failed_job_lands_on_the_error_band_and_advances_nothing(cx: &mut TestAppContext) {
        let shell = shell(None, cx);
        let submit = wire_runner(&shell, cx);
        assert!(submit.submit(Box::new(Fails)).is_ok());

        // The production pump, not a test-local read of the queue.
        pump_until(&shell, cx, |shell| shell.error.is_some());
        shell.read_with(cx, |shell, _| {
            assert_eq!(
                shell.error.as_deref(),
                Some("git commit: hook declined"),
                "the repository's own words reached the band"
            );
            assert!(shell.running.is_none(), "the job still reads as running");
            assert_eq!(
                shell.generation,
                Generation::default(),
                "a failure advanced the generation"
            );
        });
    }

    #[gpui::test]
    fn a_successful_job_bumps_the_generation_and_reacquires_through_the_pump(
        cx: &mut TestAppContext,
    ) {
        let (shell, repo, handle) = files_shell(cx);
        let submit = wire_runner(&shell, cx);
        assert!(submit
            .submit(Box::new(gitten_app::verbs::Write::stage(
                &handle,
                b"notes.md".to_vec()
            )))
            .is_ok());

        let before = shell.read_with(cx, |shell, _| shell.generation);
        pump_write(&shell, cx);
        assert_eq!(
            repo.wrote(),
            vec!["stage notes.md"],
            "the write ran off-thread against the real handle"
        );
        shell.read_with(cx, |shell, _cx| {
            assert!(shell.generation > before, "the bump never happened");
            assert!(shell.error.is_none());
            // Re-acquisition applied: every pane's generation caught up with
            // the shell's, through refresh_stale and no test help.
            for screen in shell.panes.iter() {
                match screen {
                    Screen::Files { generation, .. } => {
                        assert_eq!(generation.get(), shell.generation);
                    }
                    Screen::Commits { source, .. } => {
                        assert!(matches!(source, Source::Fixtures), "a fixture stays put")
                    }
                    other => panic!(
                        "unexpected pane kind: {}",
                        match other {
                            Screen::Custom(_) => "custom",
                            Screen::Diff { .. } => "diff",
                            Screen::Commits { .. } | Screen::Files { .. } => unreachable!(),
                        }
                    ),
                }
            }
        });
    }

    #[gpui::test]
    fn an_extension_pane_stages_through_the_same_writes_a_builtin_gets(cx: &mut TestAppContext) {
        // Rule 1, exercised rather than asserted: a pane that shipped with no
        // verb of its own runs a stage-equivalent from inside its `run`, off
        // the rails dispatch hands it, and the result is indistinguishable —
        // queued, run, generation bumped, pane re-acquired.
        let (shell, repo, _handle) = files_shell(cx);
        let ran = Rc::new(Cell::new(false));
        let refreshed = Rc::new(Cell::new(Generation::default()));
        shell.update(cx, |shell, cx| {
            let commits = cx.new(|_| Commits::new(Vec::new(), Rc::new(Host::new())));
            shell.register_pane(
                "extension",
                ExtensionPane {
                    view: commits,
                    ran: ran.clone(),
                    generation: refreshed.clone(),
                },
                cx,
            );
        });

        shell.update(cx, |shell, cx| shell.run_command("extension.stage", cx));
        assert!(ran.get(), "the extension could not reach the rails");
        pump_write(&shell, cx);
        assert_eq!(repo.wrote(), vec!["stage notes.md"]);
        shell.read_with(cx, |shell, _| {
            assert!(shell.generation.get() > 0);
        });
        // And the write's generation reached the extension pane's own refresh,
        // exactly as it reached every built-in pane's.
        let target = shell.read_with(cx, |shell, _| shell.generation);
        assert_eq!(refreshed.get(), target);
    }

    #[gpui::test]
    fn the_file_verbs_say_so_where_they_cannot_act(cx: &mut TestAppContext) {
        // No repository at all (a fixture), commits pane focused.
        let shell = shell(None, cx);
        shell.update(cx, |shell, cx| shell.run_command("files.stage", cx));
        shell.read_with(cx, |shell, _| {
            assert!(shell
                .notice
                .as_deref()
                .unwrap_or_default()
                .contains("not supported here"));
        });
        shell.update(cx, |shell, cx| {
            shell.notice = None;
            shell.run_command("files.commit", cx);
        });
        shell.read_with(cx, |shell, _| {
            assert!(shell
                .notice
                .as_deref()
                .unwrap_or_default()
                .contains("not supported here"));
        });

        // A files pane over a fixture: the pane is right, the repository is
        // still missing. The tree carries one untracked file so there is a
        // row to act on — the missing repository is what this wants to hear
        // about, not an empty tree.
        shell.update(cx, |shell, cx| {
            let mut tree = Status::default();
            tree.untracked.push(gitten_core::status::UntrackedEntry {
                path: "loose.txt".into(),
            });
            let host = Rc::new(Host::new());
            let files = cx.new(|_| {
                crate::views::files::Files::from_prepared(crate::views::files::prepare(tree, ""))
            });
            files.update(cx, |f, _| {
                f.run_view("view.bottom", &host); // onto loose.txt
            });
            shell.panes.register(
                "files",
                Screen::files(files, Generation::default(), "files"),
            );
            shell.sync_modes();
        });
        shell.update(cx, |shell, cx| shell.run_command("files.stage", cx));
        shell.read_with(cx, |shell, _| {
            assert!(
                shell
                    .notice
                    .as_deref()
                    .unwrap_or_default()
                    .contains("fixture"),
                "{:?}",
                shell.notice
            );
        });

        // And a clean tree: nothing under the keyboard to act on.
        let (shell, repo, _handle) = files_shell(cx);
        shell.update(cx, |shell, cx| {
            let Some(Screen::Files { view, .. }) = shell.active() else {
                panic!("files pane lost");
            };
            view.update(cx, |f, _| f.run_view("view.top", &Rc::new(Host::new())));
        });
        shell.update(cx, |shell, cx| shell.run_command("files.stage", cx));
        shell.read_with(cx, |shell, _| {
            assert!(
                shell
                    .notice
                    .as_deref()
                    .unwrap_or_default()
                    .contains("nothing selected"),
                "{:?}",
                shell.notice
            );
        });
        assert!(repo.wrote().is_empty());
    }

    #[gpui::test]
    fn discard_asks_on_the_first_press_and_acts_on_the_second(cx: &mut TestAppContext) {
        let (shell, repo, _handle) = files_shell(cx);
        // The keyboard starts on notes.md under *unstaged*: the question is
        // "discard", and it is asked once in the band.
        shell.update(cx, |shell, cx| shell.run_command("files.discard", cx));
        shell.read_with(cx, |shell, _| {
            assert_eq!(
                shell.notice.as_deref(),
                Some("discard notes.md? press again to confirm"),
                "{:?}",
                shell.notice
            );
        });
        // Nothing was queued for a first press, however long the worker waits.
        std::thread::sleep(Duration::from_millis(50));
        assert!(repo.wrote().is_empty());

        // The second press on the same row spends the arm and rides the rails:
        // job off-thread, generation bump, re-acquire — everything stage rides.
        shell.update(cx, |shell, cx| shell.run_command("files.discard", cx));
        pump_write(&shell, cx);
        assert_eq!(repo.wrote(), vec!["discard notes.md"]);
    }

    #[gpui::test]
    fn an_untracked_discard_says_delete_because_that_is_what_it_does(cx: &mut TestAppContext) {
        let mut tree = Status::default();
        tree.staged.push(gitten_core::status::StagedEntry {
            path: "gone.txt".into(),
            change: gitten_core::status::Change::Deleted,
            old_path: None,
            kind: gitten_core::status::Kind::File,
            submodule: Default::default(),
        });
        tree.untracked.push(gitten_core::status::UntrackedEntry {
            path: "loose.txt".into(),
        });
        let (shell, repo, _handle) = tree_shell(cx, tree);
        // The keyboard is on loose.txt, which no earlier version exists for.
        shell.update(cx, |shell, cx| shell.run_command("files.discard", cx));
        shell.read_with(cx, |shell, _| {
            assert_eq!(
                shell.notice.as_deref(),
                Some("delete loose.txt? press again to confirm"),
                "{:?}",
                shell.notice
            );
        });

        shell.update(cx, |shell, cx| shell.run_command("files.discard", cx));
        pump_write(&shell, cx);
        assert_eq!(
            repo.wrote(),
            vec!["delete loose.txt"],
            "the untracked mechanics ran, not a checkout"
        );
    }

    #[gpui::test]
    fn moving_the_keyboard_disarms_and_a_staged_row_refuses_aloud(cx: &mut TestAppContext) {
        let (shell, repo, _handle) = files_shell(cx);

        // Arm on the unstaged row...
        shell.update(cx, |shell, cx| shell.run_command("files.discard", cx));
        // ...then move onto the staged twin section's file. The move itself
        // disarmed; what lands here is a refusal, not an execution.
        shell.update(cx, |shell, cx| {
            let Some(Screen::Files { view, .. }) = shell.active() else {
                panic!("files pane lost");
            };
            view.update(cx, |f, _| {
                f.run_view("view.top", &Rc::new(Host::new()));
                f.run_view("view.down", &Rc::new(Host::new())); // gone.txt, staged
            });
        });
        shell.update(cx, |shell, cx| shell.run_command("files.discard", cx));
        shell.read_with(cx, |shell, _| {
            let notice = shell.notice.as_deref().unwrap_or_default();
            assert!(
                notice.contains("staged") && notice.contains("unstage"),
                "the staged row said why it refused: {notice:?}"
            );
        });
        assert!(repo.wrote().is_empty(), "a refusal queued nothing");

        // Back on the unstaged row, the dance runs from the top: ask, then act.
        shell.update(cx, |shell, cx| {
            let Some(Screen::Files { view, .. }) = shell.active() else {
                panic!("files pane lost");
            };
            view.update(cx, |f, _| {
                f.run_view("view.bottom", &Rc::new(Host::new())); // notes.md
            });
        });
        shell.update(cx, |shell, cx| shell.run_command("files.discard", cx));
        shell.read_with(cx, |shell, _| {
            assert!(shell
                .notice
                .as_deref()
                .unwrap_or_default()
                .contains("press again"));
        });
        shell.update(cx, |shell, cx| shell.run_command("files.discard", cx));
        pump_write(&shell, cx);
        assert_eq!(repo.wrote(), vec!["discard notes.md"]);
    }

    #[gpui::test]
    fn stage_all_takes_every_row_by_the_side_the_keyboard_sits_in(cx: &mut TestAppContext) {
        let (shell, repo, _handle) = files_shell(cx);

        // Keyboard on the unstaged row: stage everything that side holds,
        // unstaged and untracked together, as one job.
        shell.update(cx, |shell, cx| shell.run_command("files.stage-all", cx));
        pump_write(&shell, cx);
        assert_eq!(repo.wrote(), vec!["stage-many notes.md"]);

        // Onto the staged section and the same key unstages the other side.
        shell.update(cx, |shell, cx| {
            let Some(Screen::Files { view, .. }) = shell.active() else {
                panic!("files pane lost");
            };
            view.update(cx, |f, _| {
                f.run_view("view.top", &Rc::new(Host::new()));
                f.run_view("view.down", &Rc::new(Host::new()));
            });
        });
        shell.update(cx, |shell, cx| shell.run_command("files.stage-all", cx));
        pump_write(&shell, cx);
        assert_eq!(
            repo.wrote(),
            vec!["stage-many notes.md", "unstage-many gone.txt"],
            "the cursor's side decided both directions"
        );
    }

    #[gpui::test]
    fn staging_everything_when_there_is_nothing_says_so_and_queues_nothing(
        cx: &mut TestAppContext,
    ) {
        let (shell, repo, _handle) = files_shell(cx);
        // A clean tree flattens to nothing, so the cursor sits nowhere.
        shell.update(cx, |shell, cx| {
            let Some(Screen::Files { view, .. }) = shell.active() else {
                panic!("files pane lost");
            };
            view.update(cx, |f, cx| {
                f.replace_prepared(
                    crate::views::files::prepare(Status::default(), "r"),
                    &config::host(cx),
                );
            });
        });
        shell.update(cx, |shell, cx| shell.run_command("files.stage-all", cx));
        shell.read_with(cx, |shell, _| {
            assert!(
                shell
                    .notice
                    .as_deref()
                    .unwrap_or_default()
                    .contains("nothing"),
                "{:?}",
                shell.notice
            );
        });
        assert!(repo.wrote().is_empty());
    }

    #[gpui::test]
    fn ignore_answers_for_an_untracked_file_and_nowhere_else(cx: &mut TestAppContext) {
        let (shell, repo, _handle) = files_shell(cx);
        // notes.md is tracked-and-modified: .gitignore governs nothing here,
        // and the command says so rather than succeeding at nothing.
        shell.update(cx, |shell, cx| shell.run_command("files.ignore", cx));
        shell.read_with(cx, |shell, _| {
            assert!(
                shell
                    .notice
                    .as_deref()
                    .unwrap_or_default()
                    .contains("untracked"),
                "{:?}",
                shell.notice
            );
        });
        assert!(repo.wrote().is_empty());

        // The row it does answer: an untracked file goes to .gitignore as one
        // write job; the refresh after it drops the entry from status.
        let mut tree = Status::default();
        tree.untracked.push(gitten_core::status::UntrackedEntry {
            path: "loose.txt".into(),
        });
        let (shell, repo, _handle) = tree_shell(cx, tree);
        shell.update(cx, |shell, cx| shell.run_command("files.ignore", cx));
        pump_write(&shell, cx);
        assert_eq!(repo.wrote(), vec!["ignore loose.txt"]);
    }
}
