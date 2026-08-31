mod chrome;
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
use gitten_core::refs::ResetMode;
use gitten_core::theme;
use gitten_core::{Commit, FileDiff};
use gpui::*;
use gpui_component::*;
use stats::Stats;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

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
/// The branch chip's height in the title strip: a row's worth, so it reads
/// as a label and not as a button, inside a 32px band with air either side.
const CHIP_H: f32 = 22.0;
/// The shortest a sidebar section may be squeezed to when the three do not
/// fit: its header and two rows — the selected one and a neighbour, which
/// is the least a list can show and still be seen to scroll.
const SECTION_MIN_H: f32 = chrome::HEADER_H + 2.0 * graph::ROW_H;

/// A sidebar section's natural height: its header plus one row per line it
/// draws, with a floor of one row for the empty state's line ("working tree
/// clean", "nothing stashed"). Arithmetic and not measurement, because a view
/// cannot know its own size during `render` — and a list row is a fixed
/// [`graph::ROW_H`] precisely so that sums like this one are exact.
fn section_height(rows: usize) -> f32 {
    chrome::HEADER_H + rows.max(1) as f32 * graph::ROW_H
}

/// The shortest a section may be squeezed to: [`SECTION_MIN_H`], unless the
/// section is naturally shorter than that — a `min_h` above the basis wins
/// the layout, and an empty list padded to two rows is air nobody asked for.
fn section_floor(rows: usize) -> f32 {
    SECTION_MIN_H.min(section_height(rows))
}

/// The diff header's text, spelled once per change of what it says — see
/// [`DevShell::header_memo`]. The path is already cut where
/// [`chrome::path_spans`] wants it.
#[derive(Clone)]
struct HeaderText {
    dir: SharedString,
    name: SharedString,
    adds: SharedString,
    dels: SharedString,
    hunk: Option<SharedString>,
}

impl HeaderText {
    fn of(s: &views::diff::FileSummary) -> Self {
        let (dir, name) = gitten_core::path::split_dir_name(&s.path);
        Self {
            dir: dir.to_string().into(),
            name: name.to_string().into(),
            adds: format!("+{}", s.adds).into(),
            dels: format!("−{}", s.dels).into(),
            hunk: (s.hunks > 0).then(|| format!("hunk {}/{}", s.hunk, s.hunks).into()),
        }
    }
}

/// The stack's three content-sized panes, in reading order: the mode name the
/// registry knows them by, the key that focuses each, the header's label and
/// the element id. Static, so a frame spells none of them — and the label is
/// the design's word, which is `STASH` for a stack the mode calls `stashes`.
/// The commit list is the stack's fourth section and draws after these; it is
/// not in this table because it sizes itself differently — the flexible foot,
/// not content height — and because its slot is the registry's, which an
/// extension pane can take over.
const STACK_TOP: [(&str, &str, &str, &str); 3] = [
    ("status", "1", "STATUS", "side-status"),
    ("files", "2", "FILES", "side-files"),
    ("branches", "3", "BRANCHES", "side-branches"),
];

/// The stack's content-sized pane *under* the commit list: the stash, whose
/// key is last because parking is where a session's work ends. A separate
/// table from [`STACK_TOP`] because drawing order is not registry order —
/// the commit list is the flexible middle, and the foot renders after it.
const STACK_FOOT: [(&str, &str, &str, &str); 1] = [("stashes", "5", "STASH", "side-stashes")];

/// The repository as the title strip spells it: `(parent, name)` with the
/// parent under `~` when it is under home and ending in `/`, so the two halves
/// concatenate back into the path — `("~/src/", "plait")`. A path with no name
/// to give (the filesystem root) puts everything in the bright half rather
/// than drawing nothing.
fn repo_title(path: &std::path::Path, home: Option<&std::path::Path>) -> (String, String) {
    let shown = match home.and_then(|home| path.strip_prefix(home).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    };
    // Drop a trailing slash so the cut lands on the name — unless the slash
    // *is* the path, which is the root and the one name it has.
    let trimmed = shown.trim_end_matches('/');
    let shown = match trimmed.is_empty() {
        true => shown.as_str(),
        false => trimmed,
    };
    let (dir, name) = gitten_core::path::split_dir_name(shown);
    match name.is_empty() {
        true => (String::new(), format!("{dir}{name}")),
        false => (dir.to_string(), name.to_string()),
    }
}

/// `$HOME`, read once for the process: the title strip asks every frame and an
/// environment lookup is not a per-frame cost worth paying for a string that
/// does not change.
fn home() -> Option<&'static std::path::Path> {
    static HOME: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
    HOME.get_or_init(|| std::env::var_os("HOME").map(std::path::PathBuf::from))
        .as_deref()
}
/// How long the commits cursor may keep moving before the main view loads the
/// commit it settled on. A fast run through the list schedules one timer per
/// row but only the newest request ever survives its guard to load — see
/// [`DevShell::schedule_main_diff`].
const DIFF_DEBOUNCE: Duration = Duration::from_millis(150);

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
    /// The same field over the same staged content, aimed one step back:
    /// what a `files.amend` accept turns into an amend job.
    AmendMessage,
    /// A `/` query over one pane, named by its registration name — a name and
    /// not a type, so the slot stays open to whatever pane learns to answer a
    /// search next. Every edit filters that pane live (see
    /// [`DevShell::search_edited`]); accepting keeps the last edit standing,
    /// cancelling clears it.
    Search { target: String },
    /// A branch name gathered over the branches pane — `branches.new` names
    /// a branch from nothing; `branches.rename` starts from the row's own
    /// name and accepts a replacement. The target is the pane registration
    /// name, the same promise [`Prompt::Search`] keeps: the answer belongs
    /// to the pane it was typed over.
    BranchName { target: String, what: BranchPrompt },
    /// A tag name gathered over a pane: accepting names whatever the pane
    /// had under the keyboard when the field opened, carried as a **revspec**
    /// — a sha from the commits pane, a branch name from the branches one —
    /// because `git tag` aims at both the same way. Captured at open time,
    /// so a cursor move inside the field cannot re-aim it. The target is the
    /// pane registration name, same as [`Prompt::BranchName`].
    TagName { target: String, at: String },
}

/// What an accepted [`Prompt::BranchName`] does with its text.
#[derive(Debug)]
enum BranchPrompt {
    /// Create a branch by this name at HEAD. Creating never checks out.
    New,
    /// Create a branch by this name growing from `start` — a revspec, the
    /// commits pane's way of saying "from the commit I was on". Creating
    /// never checks out, here either.
    NewAt { start: String },
    /// Rename the carried branch — its bytes, exactly as the panel read
    /// them — to the accepted text.
    Rename { from: Vec<u8> },
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

/// `repo.refresh`: the queue's own finish does the re-acquire wave after
/// every write — the generation bump is what turns every pane stale — and
/// this job is that finish with no write in front of it. lazygit's `R`,
/// refreshed: the band says so, because unlike a write nothing on screen
/// changed to prove it ran.
struct RefreshAll;

impl Job for RefreshAll {
    fn name(&self) -> &str {
        "refresh"
    }

    fn confirmation(&self) -> Option<String> {
        Some("refreshed".into())
    }

    fn run(self: Box<Self>) -> Result<(), String> {
        Ok(())
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

/// Which of the window's two regions the keyboard is in.
///
/// Two targets, because the design has two: the lists — the left stack's
/// four panes, all on screen at once — and the diff filling the rest. The focused region carries the accent edge, and
/// [`Modes`] is rebuilt from this — a list's keys move that list, the diff's
/// keys scroll the diff — which is why there is no third state to forget to
/// route.
///
/// Which *list* has the keyboard is not a spot of its own: it is
/// [`panes::Panes`]' focused tenant, the same registry that decides what the
/// stack's commit section draws when an extension pane takes it over. One
/// section and another differ in where they draw, never in what the keyboard
/// means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Spot {
    /// Some list — a stack section, an extension pane standing in for the
    /// commit list — holds the keyboard.
    List,
    /// The diff main view.
    Main,
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
    /// The diff main view. The one tenant that is not a stack list: it fills
    /// the right side of the window whatever the stack shows.
    ///
    /// Its revspec lives behind a cell because it *changes* while the tenant
    /// does not — every selection change re-aims the same view at another
    /// commit, so the screen is built once with `None` (nothing loaded yet)
    /// and re-aimed from [`DevShell::schedule_main_diff`]. A fixture or patch
    /// launch builds it once with its own source and nothing ever rewrites it.
    Diff {
        view: Entity<views::diff::Diff>,
        source: Rc<RefCell<Option<Source>>>,
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
    /// The stash stack. Same story as [`Screen::Files`]: always about this
    /// window's repository, so no `Source`, and a fixture gets none.
    Stashes {
        view: Entity<views::stashes::Stashes>,
        generation: Rc<Cell<Generation>>,
        label: Rc<RefCell<String>>,
    },
    /// The branches. Same story as [`Screen::Files`]: repository-shaped, so
    /// a fixture never gets one.
    Branches {
        view: Entity<views::branches::Branches>,
        generation: Rc<Cell<Generation>>,
        label: Rc<RefCell<String>>,
    },
    /// The status line: where HEAD sits. It acquires nothing — it reads the
    /// branches pane's model, which already pays for the window's one `head`
    /// read — so it has a label and a branch handle and no refresh of its
    /// own: a branches refresh is its refresh.
    Status {
        view: Entity<views::status::Status>,
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
        source: Option<Source>,
        generation: Generation,
        label: impl Into<String>,
    ) -> Self {
        Self::Diff {
            view,
            source: Rc::new(RefCell::new(source)),
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

    fn stashes(
        view: Entity<views::stashes::Stashes>,
        generation: Generation,
        label: impl Into<String>,
    ) -> Self {
        Self::Stashes {
            view,
            generation: Rc::new(Cell::new(generation)),
            label: Rc::new(RefCell::new(label.into())),
        }
    }

    fn branches(
        view: Entity<views::branches::Branches>,
        generation: Generation,
        label: impl Into<String>,
    ) -> Self {
        Self::Branches {
            view,
            generation: Rc::new(Cell::new(generation)),
            label: Rc::new(RefCell::new(label.into())),
        }
    }

    fn status(view: Entity<views::status::Status>, label: impl Into<String>) -> Self {
        Self::Status {
            view,
            label: Rc::new(RefCell::new(label.into())),
        }
    }

    fn any(&self) -> AnyView {
        match self {
            Screen::Commits { view, .. } => view.clone().into(),
            Screen::Diff { view, .. } => view.clone().into(),
            Screen::Files { view, .. } => view.clone().into(),
            Screen::Stashes { view, .. } => view.clone().into(),
            Screen::Branches { view, .. } => view.clone().into(),
            Screen::Status { view, .. } => view.clone().into(),
            Screen::Custom(pane) => pane.any(),
        }
    }

    /// Which mode's bindings are live. The name the keymap and `gitten.toml` use.
    fn mode(&self) -> &'static str {
        match self {
            Screen::Commits { .. } => "commits",
            Screen::Diff { .. } => "diff",
            Screen::Files { .. } => "files",
            Screen::Stashes { .. } => "stashes",
            Screen::Branches { .. } => "branches",
            Screen::Status { .. } => "status",
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
            Screen::Diff { label, .. }
            | Screen::Files { label, .. }
            | Screen::Stashes { label, .. } => label.borrow().clone(),
            Screen::Branches { label, .. } => label.borrow().clone(),
            Screen::Status { label, .. } => label.borrow().clone(),
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
            // The status pane reads other panes' models and owns no
            // acquisition, so a refresh wave has nothing to hand it.
            Screen::Status { .. } => None,
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
                // `None` is "nothing loaded yet" — a window that opened on a
                // list before its first selection scheduled a diff — and a
                // `.diff` fixture was never acquired from a repository at
                // all. Anything else re-acquires its own revspec, exactly as
                // the stacked pane did.
                let source = source.borrow().clone()?;
                if generation.get() >= target || matches!(source, Source::Fixtures) {
                    return None;
                }
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
            Screen::Stashes {
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
                        // The whole of the blocking half: one `git stash list`
                        // beside the describe the label keeps naming.
                        let described = std::thread::scope(|s| {
                            let title = s.spawn(|| repo.describe());
                            let stashes = repo.stashes()?;
                            Ok::<_, String>(views::stashes::prepare(
                                &stashes,
                                &title.join().unwrap_or_default(),
                            ))
                        })?;
                        Ok(described)
                    },
                    move |prepared: views::stashes::Prepared, host, cx| {
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
            Screen::Branches {
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
                let theme = host.theme.clone();
                Some(Refresh::new(
                    target,
                    move || {
                        // The whole of the blocking half: the two ref
                        // listings and HEAD's state, run beside each other —
                        // four independent processes, one spawn floor. The
                        // theme rides along because the dots are coloured at
                        // flatten, once, and not per frame.
                        let prepared = std::thread::scope(|s| {
                            let title = s.spawn(|| repo.describe());
                            let local = s.spawn(|| repo.branches());
                            let remote = s.spawn(|| repo.remote_branches());
                            let head = s.spawn(|| repo.head());
                            let described = title.join().unwrap_or_default();
                            let local = local
                                .join()
                                .unwrap_or_else(|p| std::panic::resume_unwind(p))?;
                            let remote = remote
                                .join()
                                .unwrap_or_else(|p| std::panic::resume_unwind(p))?;
                            // A failed HEAD read must not take the listing
                            // down: the rows are still true, only the top
                            // row's honesty is lost, and that loss is said.
                            let head = match head
                                .join()
                                .unwrap_or_else(|p| std::panic::resume_unwind(p))
                            {
                                Ok(head) => Some(head),
                                Err(e) => {
                                    eprintln!("gitten: head read failed, showing attached: {e}");
                                    None
                                }
                            };
                            Ok::<_, String>(views::branches::prepare(
                                local, remote, head, &theme, &described,
                            ))
                        });
                        prepared
                    },
                    move |prepared: views::branches::Prepared, host, cx| {
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
            Screen::Stashes { view, .. } => view.read(cx).list_bounds(),
            Screen::Branches { view, .. } => view.read(cx).list_bounds(),
            Screen::Status { .. } => Bounds::default(),
            Screen::Custom(pane) => pane.list_bounds(cx),
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
            Screen::Stashes { view, .. } => view.read(cx).pan_pixels(dx),
            Screen::Branches { view, .. } => view.read(cx).pan_pixels(dx),
            Screen::Status { .. } => false,
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
            Screen::Stashes { view, .. } => view.update(cx, |s, c| {
                let known = s.run_view(command, host);
                if known {
                    c.notify();
                }
                known
            }),
            Screen::Branches { view, .. } => view.update(cx, |b, c| {
                let known = b.run_view(command, host);
                if known {
                    c.notify();
                }
                known
            }),
            // The status pane has no verbs: it says where HEAD is and takes
            // no commands. Every global still resolves over it — cycling,
            // the number keys, the pane moves — and a pane-specific verb is
            // answered by the caller's sentence, the same as a command no
            // screen owns.
            Screen::Status { .. } => false,
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
            Screen::Stashes { view, .. } => view.update(cx, |v, _| v.scroll_pixels(dy, host)),
            Screen::Branches { view, .. } => view.update(cx, |b, _| b.scroll_pixels(dy, host)),
            Screen::Status { .. } => false,
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
            Screen::Stashes { view, .. } => view.update(cx, |s, _| match all {
                true => s.select_all(),
                false => s.select_none(),
            }),
            Screen::Branches { view, .. } => view.update(cx, |b, _| match all {
                true => b.select_all(),
                false => b.select_none(),
            }),
            Screen::Status { .. } => false,
            Screen::Custom(pane) => pane.select(all, cx),
        }
    }

    fn custom(pane: impl Pane + 'static) -> Self {
        Self::Custom(Rc::new(pane))
    }
}

/// The keymap mode the reset question pushes while it stands — see
/// [`DevShell::sync_modes`]. Its name is the keymap's `[reset]` section in
/// `gitten.toml`.
const RESET_MODE: &str = "reset";

/// The keymap mode the message overlay pushes while it stands — see
/// [`DevShell::sync_modes`]. It binds nothing itself: the overlay is a reading
/// pane, and its exits (`esc`, the copy) answer to whatever the keymap already
/// says, so a config file can rebind them and the panel follows.
const MESSAGE_MODE: &str = "message";

/// What the band says, and why it is saying it. Two, because the two sentences
/// are not the same sentence: an info describes what was tried, and a question
/// is the one the keyboard is about to spend — the loudest thing on screen,
/// because quiet is what hid the arm.
#[derive(Clone, Debug)]
enum Notice {
    Info(String),
    Question(String),
}

impl Notice {
    /// The band's sentence, whichever of the two it is.
    fn text(&self) -> &str {
        match self {
            Notice::Info(text) | Notice::Question(text) => text,
        }
    }
}

/// A notice is its text — what `as_deref` hands back out of the band, the same
/// `&str` a `String` notice did, so a reader cannot tell the two apart and a
/// test does not have to.
impl std::ops::Deref for Notice {
    type Target = str;

    fn deref(&self) -> &str {
        self.text()
    }
}

/// A refusal, kept whole. The band shows [`GitError::summary`]; the message
/// overlay shows [`GitError::full`] — the argv prefix is part of the answer
/// when the text is being read rather than glanced at, and the summary is the
/// glance.
#[derive(Clone, Debug, PartialEq)]
struct GitError {
    /// The first line of git's own words, argv prefix stripped.
    summary: SharedString,
    /// Everything git said, verbatim — the argv prefix included, because
    /// "which command" is part of the answer when the text is being read
    /// rather than glanced at.
    full: SharedString,
}

impl GitError {
    fn new(full: impl Into<SharedString>) -> Self {
        let full = full.into();
        // The acquisition layer's shape is `git {args}: {stderr}` — strip that
        // prefix and the summary is git's first line, not the argv's. An
        // error that arrived by another road is already its own summary.
        let body = match full.strip_prefix("git ") {
            Some(rest) => match rest.find(": ") {
                Some(at) => &rest[at + ": ".len()..],
                None => rest,
            },
            None => full.as_ref(),
        };
        let summary = body
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or(body);
        Self {
            summary: summary.into(),
            full,
        }
    }
}

/// An error reads as its headline: the same `&str` the band shows, so a test
/// (or a reader) asks the error for words and gets the glance, not the record.
impl std::ops::Deref for GitError {
    type Target = str;

    fn deref(&self) -> &str {
        &self.summary
    }
}

struct DevShell {
    /// The app half of the title, drawn bright: which program this is. Which
    /// *view* it is showing is the focused region's to say, because a commit
    /// list that handed the keyboard to its diff is still a diff while that
    /// region owns it.
    which: &'static str,
    /// The window's panes, by stable name. The left stack renders four named
    /// residents at once — files, branches, stashes, commits — or whichever
    /// extension pane has taken the commit list's region over; focus decides
    /// whose keys are live, and every list keeps its own cursor and scroll
    /// state whatever is on screen. See [`DevShell::list_order`] for the
    /// order the keys walk.
    panes: panes::Panes<Screen>,
    /// The diff main view: always on screen, right of the stack. Built once
    /// at startup (empty when the window opened on a list) and re-aimed at
    /// each selection through [`DevShell::schedule_main_diff`] — never rebuilt,
    /// which is what keeps its scroll state and presentation across commits.
    ///
    /// A launch whose first acquisition was itself a diff (`gitten diff …`,
    /// a fixture, a patch file) builds this from those rows instead and has
    /// no commit list at all — see [`DevShell::has_column`].
    main: Screen,
    /// Whether the window has a commit list. False only for a diff/fixture/
    /// patch launch, where there is no acquired commit list to show; the
    /// diff then fills the whole width, the stack's other panes still stand
    /// (a fixture has no repository, so it has none of them either) and the
    /// spot never leaves [`Spot::Main`] when no list exists at all.
    has_column: bool,
    /// Which region holds the keyboard.
    spot: Spot,
    /// The commit the main view is of — set the moment a selection is
    /// scheduled, so the diff header is true during the load, not only after
    /// it. Its subject shrinks behind the file name in that header; its
    /// author and sha live in the list row the keyboard came from, which is
    /// where a reader looks for them. `None` until the first selection (or
    /// forever, over a fixture).
    head: RefCell<Option<Commit>>,
    /// The newest main-view request. Each schedule bumps it; a timer or a
    /// finished load applies only if it still equals the value it left with,
    /// which is how a fast cursor run collapses to exactly one load — the
    /// settled row's.
    request: Cell<u64>,
    /// True between scheduling a main-view load and its rows landing.
    loading: Cell<bool>,
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
    running: Option<(String, std::time::Instant)>,
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
    error: Option<GitError>,
    /// Whether the error's full text is on screen — `message.show` opened it,
    /// `esc` or anything that clears the error closes it.
    show_message: bool,
    /// One sentence about what a key just did — an unbound chord, a command
    /// that resolved to nothing this screen can do, or a write that named
    /// its own finish (the sync verbs: pushed, pulled, fetched). Cleared by
    /// the next key, so it cannot go stale. Same band as
    /// [`DevShell::error`], which wins — and an armed question in it is the
    /// error's ink and not this, because the one sentence a second press
    /// spends is the one being read: see [`DevShell::set_question`].
    notice: Option<Notice>,
    /// Where `gitten.toml` is. Held because picking a theme goes through the same
    /// reload a save does — see [`config::reload`] for why there is only one
    /// path.
    config: std::path::PathBuf,
    /// Startup logging, and nothing else: whether [`start::mark`] has already
    /// stamped the first render. One bool read per frame afterwards.
    first_render: Cell<bool>,
    /// The title strip's two halves — `("~/src/", "plait")` — cut once per
    /// repository and read per frame. Keyed on the path, because the tests
    /// swap `repo` in place and a memo that trusted construction would lie.
    title_memo: RefCell<Option<(std::path::PathBuf, SharedString, SharedString)>>,
    /// The diff header's spelled-out strings, kept beside the summary they
    /// were spelled from: a cursor sitting still is the common frame, and it
    /// must not re-split the path and re-format three numbers to say the
    /// same thing again.
    header_memo: RefCell<Option<(views::diff::FileSummary, HeaderText)>>,
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
    /// The help panel's row scroll. The handle is the shell's and not the
    /// panel's because the panel is a pure element — see [`help::overlay`] —
    /// and the keyboard has to reach its tail, which is the one piece of
    /// state a pure element cannot hold. Reset when help opens: the rows are
    /// a different projection every time — the active modes' — and an offset
    /// the last reading left is a promise about rows that no longer exist.
    help_scroll: ScrollHandle,
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
    /// The screen commands act on: the focused list, or the diff, by where
    /// the keyboard is. Every dispatch decision reads through here,
    /// which is what makes routing a change of [`Spot`] and nothing else.
    fn active(&self) -> Option<&Screen> {
        Some(match self.spot {
            Spot::List => self.panes.focused(),
            Spot::Main => &self.main,
        })
    }

    /// The commits list. With the stack holding the other lists, the commit
    /// list is on screen whatever the keyboard is doing — so main-view
    /// loading reads its selection from here even while the files pane is
    /// focused, which is the design's point: moving through the working tree
    /// does not take the commit list away. `None` only while an extension
    /// pane has taken its region over, or on a launch with no commit list at
    /// all.
    fn column_commits(&self) -> Option<Entity<views::commits::Commits>> {
        if !self.has_column || matches!(self.panes.focused(), Screen::Custom(_)) {
            return None;
        }
        match self.panes.get("commits") {
            Some(Screen::Commits { view, .. }) => Some(view.clone()),
            _ => None,
        }
    }

    /// The pane names that are lists, in the order the number keys name them
    /// and the stack draws them — lazygit's: status, files, branches,
    /// commits, then the stash at the foot, then whatever an extension
    /// registered. The pane moves' walk ([`DevShell::pane_walk`]) and the
    /// ctrl-j cycle both read this, so a pane out of this order is a pane
    /// the keyboard visits out of order; a diff-shaped launch has no commits
    /// and still has the sidebar, so both "is there a list to focus" and the
    /// cycle order read through here rather than through `has_column`.
    fn list_order(&self) -> Vec<&str> {
        let mut names: Vec<&str> = ["status", "files", "branches", "commits", "stashes"]
            .into_iter()
            .filter(|name| self.panes.position(name).is_some())
            .collect();
        let builtins = ["status", "files", "branches", "stashes", "commits"];
        names.extend(self.panes.names().filter(|name| !builtins.contains(name)));
        names
    }

    /// Moves the keyboard to a region. A fixture window has no list to give
    /// the keyboard back to, so a `Spot::Main` there is forever.
    fn set_spot(&mut self, spot: Spot, cx: &mut App) {
        if spot == Spot::List && self.list_order().is_empty() {
            return;
        }
        if self.spot != spot {
            self.spot = spot;
            self.sync_modes(cx);
            self.sync_focus(cx);
        }
    }

    /// Tells every list whether it holds the keyboard. A row's bar is accent
    /// only in the focused pane and the view cannot ask the shell during
    /// render, so this runs from the two places focus actually moves —
    /// [`DevShell::set_spot`] and [`DevShell::focus_pane`] — and once at
    /// startup. The keyboard is in exactly one list when `spot` is the list
    /// region, and in none when it is the diff.
    fn sync_focus(&mut self, cx: &mut App) {
        let at = match self.spot {
            Spot::List => Some(self.panes.focused_index()),
            Spot::Main => None,
        };
        for (i, screen) in self.panes.iter().enumerate() {
            let focused = at == Some(i);
            match screen {
                Screen::Files { view, .. } => view.update(cx, |v, _| v.set_focused(focused)),
                Screen::Branches { view, .. } => view.update(cx, |v, _| v.set_focused(focused)),
                Screen::Stashes { view, .. } => view.update(cx, |v, _| v.set_focused(focused)),
                Screen::Commits { view, .. } => view.update(cx, |v, _| v.set_focused(focused)),
                // The diff draws the cursor for its own rows: it takes focus
                // through the same seam the sidebar panes do, and no further.
                Screen::Diff { view, .. } => view.update(cx, |v, _| v.set_focused(focused)),
                Screen::Status { .. } | Screen::Custom(_) => {}
            }
        }
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
    /// Called on every change of region focus or help state — the places
    /// [`Modes`] can change — and at the tail of [`DevShell::run_command`],
    /// because a cursor move inside a list can end a standing question.
    fn sync_modes(&mut self, cx: &App) {
        self.modes = Modes::new();
        // Cycling the lists needs more than one of them to be worth a key.
        if self.list_order().len() > 1 {
            self.modes.push(panes::MODE);
        }
        if let Some(screen) = self.active() {
            self.modes.push(screen.mode());
        }
        // The reset question, lazygit's menu: while the commits view has a
        // reset armed, its three letters capture s/m/h for the strengths —
        // `h` included, which outside the question is the pane move. The
        // question is the pane's own state and survives nothing that moves
        // the cursor, so this reads it rather than mirrors it.
        if let Some(Screen::Commits { view, .. }) = self.active() {
            if view.read(cx).armed() {
                self.modes.push(RESET_MODE);
            }
        }
        if self.input.is_some() {
            self.modes.push(input::MODE);
        }
        if self.help {
            self.modes.push(help::MODE);
        }
        if self.show_message && self.error.is_some() {
            self.modes.push(MESSAGE_MODE);
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
        self.notice = Some(Notice::Info(message.into()));
    }

    /// An armed question — the sentence a second press spends, asked once in
    /// the band and answered by the next press or a move of the cursor.
    fn set_question(&mut self, message: impl Into<String>) {
        self.notice = Some(Notice::Question(message.into()));
    }

    fn open_input(&mut self, input: Entity<input::Input>, cx: &mut Context<Self>) {
        // Whatever the previous prompt was filtering live stops now; what its
        // last edit did to its pane is that prompt's close to decide.
        self.search_live = None;
        if let Some(previous) = self.input.replace(input) {
            previous.update(cx, |input, cx| input.cancel(cx));
        }
        self.sync_modes(cx);
        // The field speaks its own exits, because the status hints are blanked
        // while it stands and a prompt that hides how to leave it is a modal
        // with no door. Resolved here and once: `sync_modes` has just pushed
        // the input mode, so `live_keys_for` answers what a press means right
        // now — a key an inner mode took over is never named — and the field
        // does not re-walk the keymap per frame for a keyboard it holds.
        let host = config::host(cx);
        let accept = host
            .keys
            .live_keys_for("input.accept", &self.modes)
            .into_iter()
            .next();
        let cancel = host
            .keys
            .live_keys_for("input.cancel", &self.modes)
            .into_iter()
            .next();
        if let Some(field) = self.input.as_ref() {
            field.update(cx, |field, _| field.set_exits(accept, cancel));
        }
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
        self.sync_modes(cx);
        match (accept, self.prompt.take()) {
            (true, Some(Prompt::CommitMessage)) => self.commit_message(text),
            (true, Some(Prompt::AmendMessage)) => self.amend_message(text),
            // A search keeps what was typed on accept and clears on cancel —
            // `esc` means "forget it", not "keep half of it".
            (_, Some(Prompt::Search { target })) => {
                self.finish_search(&target, accept.then_some(text), cx)
            }
            // A name is spent only on accept, and only if the pane it was
            // opened over still exists — the registration name in the slot
            // is the promise about where the answer belongs.
            (true, Some(Prompt::BranchName { target, what })) => {
                self.branch_named(&target, what, text)
            }
            (true, Some(Prompt::TagName { target, at })) => self.tag_named(&target, at, text),
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

    /// `files.amend`: the same field commit's key opens, aimed one step back
    /// — accepting rewrites HEAD to hold the staged changes under this text.
    /// The refusals are shared on purpose: no repository, an empty message.
    /// Whether HEAD has anything to amend is the trait's to answer, where
    /// the honest "no commits yet" lives next to git's own errors.
    fn begin_amend_message(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.active(), Some(Screen::Files { .. })) {
            self.set_notice("files.amend is not supported here");
            return;
        }
        if self.repo.is_none() {
            self.set_notice("a fixture has no repository to amend in");
            return;
        }
        let input = cx.new(|cx| input::Input::new("amend", "amend message", "", cx));
        self.open_input(input, cx);
        // After `open_input`, which may have cancelled a previous prompt.
        self.prompt = Some(Prompt::AmendMessage);
    }

    /// The accepted amend text, as a job. Empty refused again here — the
    /// trait refuses it too, but saying so beside the field that just closed
    /// beats making the reader find out twice. Amending a commit some remote
    /// already holds is tonight the reader's own decision: nothing tracks
    /// push state yet, and saying so beats a guard that guesses.
    fn amend_message(&mut self, message: String) {
        if message.trim().is_empty() {
            self.set_notice("a commit needs a message");
            return;
        }
        let Some(writes) = self.writes() else {
            self.set_notice("a fixture has no repository to amend in");
            return;
        };
        if !writes.send(Box::new(gitten_app::verbs::Write::amend(
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
            self.set_question(views::files::discard_question(section, &shown));
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

    /// `diff.stage-hunk` / `diff.unstage-hunk` / `diff.discard-hunk`: act on
    /// the hunk the keyboard sits on, wherever in it the cursor is — header,
    /// context, changed line, all one address.
    ///
    /// Three gates before anything runs, each said rather than answered
    /// badly. Only a working-tree diff has an index to aim at; a commit's
    /// diff is between two snapshots and has neither an index nor a worktree
    /// in reach. A file git tracks nowhere — untracked work — cannot travel
    /// as a patch, because `git apply --cached` creates an entry only from a
    /// patch carrying its mode and the line model does not carry one;
    /// whole-file verbs already serve it from the files pane, and status —
    /// the same read the files pane draws from — is what tells the two
    /// apart, because absence of old line numbers cannot: at `[diff] context
    /// = 0` an addition to a tracked file carries none either. And discard
    /// confirms exactly as `files.discard` does: first press arms the row
    /// and asks once, any move of the keyboard disarms, second press builds
    /// the job.
    fn hunk_verb(&mut self, command: &str, cx: &mut Context<Self>) {
        let Some(Screen::Diff { view, source, .. }) = self.active() else {
            self.set_notice(format!("{command} is not supported here"));
            return;
        };
        // Copied out of the cell before the refusals: the match arms below
        // speak into the band, and the borrow must not be alive while they do.
        let source = source.borrow().clone();
        match source.as_ref() {
            None => {
                self.set_notice("no diff is showing");
                return;
            }
            Some(Source::Repo { arg, .. }) if arg.is_empty() => {}
            Some(Source::Repo { .. }) => {
                self.set_notice(
                    "only the working-tree diff can act on hunks — this one is between commits",
                );
                return;
            }
            Some(Source::Fixtures) => {
                self.set_notice("a fixture has no repository behind it");
                return;
            }
            Some(Source::Patch { .. }) => {
                self.set_notice("a patch file has no repository behind it");
                return;
            }
        }
        let Some(writes) = self.writes() else {
            // The fixture and patch sources were refused above, so this is
            // the one case left: the window itself has no repository open.
            self.set_notice("no repository is open");
            return;
        };
        let view = view.clone();
        let host = config::host(cx);
        // Meet the list where its last drag left it, like every reader of the
        // cursor: the hunk acted on is the one being *looked at*.
        view.update(cx, |d, _| d.reconcile(&host));
        let row = view.read(cx).cursor_row_id();
        let Some((path, hunk)) = view.read(cx).current_hunk() else {
            self.set_notice("the keyboard is not on a hunk");
            return;
        };
        // A hunk whose every line is an addition *looks* like a creation —
        // but only status knows whether it is one. Absence of old line
        // numbers is not evidence: at `[diff] context = 0` a mid-file
        // addition to a tracked modified file carries no old numbers either,
        // and refusing it here would claim "adds a new file" over work that
        // is merely new rows. So ask the same read the files pane draws from,
        // and only for hunks that could be creations — every other shape
        // pays nothing. An untracked path keeps the refusal that names the
        // pane serving whole-file verbs; anything else synthesizes normally,
        // and if the patch still cannot land, git's own refusal says why.
        let creation = !hunk.lines.iter().any(|l| l.old_no.is_some())
            && writes
                .repo
                .status()
                .map(|s| {
                    s.untracked
                        .iter()
                        .any(|e| e.path.as_bytes() == path.as_bytes())
                })
                // A status that cannot be read is not evidence of a
                // creation: send the patch and let git answer it.
                .unwrap_or(false);
        if creation {
            self.set_notice(match command {
                "diff.stage-hunk" | "diff.unstage-hunk" => {
                    "that hunk adds a new file — stage or unstage it whole from the files pane"
                }
                _ => "that hunk creates the file — discard it whole from the files pane",
            });
            return;
        }
        // DESTRUCTIVE asks twice, on the same spot.
        if command == "diff.discard-hunk"
            && !view.update(cx, |d, _| d.confirm_or_arm_discard_hunk(row))
        {
            self.set_question(format!(
                "discard this hunk of {path}? press again to confirm"
            ));
            return;
        }
        if command == "diff.discard-hunk" {
            self.notice = None; // the question is spent; the running band speaks next
        }
        let patch = gitten_core::patch::emit(&path, &[&hunk]);
        let built = match command {
            "diff.stage-hunk" => gitten_app::verbs::Write::stage_patch(&writes.repo, patch),
            "diff.unstage-hunk" => gitten_app::verbs::Write::unstage_patch(&writes.repo, patch),
            _ => gitten_app::verbs::Write::discard_patch(&writes.repo, patch),
        };
        match built {
            Ok(job) => {
                if !writes.send(Box::new(job)) {
                    self.set_notice("the job queue is shutting down");
                }
            }
            Err(e) => self.set_notice(e),
        }
    }

    /// `files.stash`: park what the tracked working tree holds on the stash
    /// stack and start again from HEAD. No message tonight — the entry gets
    /// git's own `WIP on …`, which is honest about what it was; a prompt for
    /// one is future work. Nothing here reads the pane: parking addresses the
    /// repository, whatever pane the key was pressed over.
    fn stash_working_tree(&mut self, _cx: &mut Context<Self>) {
        let Some(writes) = self.writes() else {
            self.set_notice("a fixture has no working tree to park");
            return;
        };
        if !writes.send(Box::new(gitten_app::verbs::Write::stash_push(
            &writes.repo,
            None,
        ))) {
            self.set_notice("the job queue is shutting down");
        }
    }

    // ---------------------------------------------------- the branch verbs

    /// `commits.reset-soft` / `commits.reset-mixed` / `commits.reset-hard`:
    /// move the branch onto the commit the keyboard is on. The target goes
    /// through [`Commits::current`] — the sha under the keyboard, wherever
    /// filtering left it — never a row index.
    ///
    /// Every strength asks twice, and asks exactly as `files.discard` does:
    /// first press arms the row and says so in the band, any cursor move,
    /// wheel or refresh disarms, second press on the same commit builds the
    /// job. Soft and mixed destroy nothing — every abandoned commit stays in
    /// the reflog — but "recoverable" is a promise to someone who knows where
    /// the reflog is, and the keypress gives no hint it moved history at all.
    /// A commit list that silently loses its top rows reads as data loss no
    /// matter what the reflog knows, so the question is asked in the band
    /// where the eyes are.
    /// `commits.reset-menu`: open the reset question on the commit the
    /// keyboard is on — lazygit's `g`. The question is the pane's armed slot
    /// plus the [reset] mode its arming pushes; the band carries the three
    /// letters, and `esc` drops it. Asking while one already stands closes
    /// it: the same key opens and dismisses, and nothing but a strength
    /// letter or `esc` executes anything.
    fn reset_menu(&mut self, cx: &mut Context<Self>) {
        let Some(Screen::Commits { view, .. }) = self.active() else {
            self.set_notice("commits.reset-menu is not supported here");
            return;
        };
        let view = view.clone();
        let host = config::host(cx);
        view.update(cx, |v, _| v.reconcile(&host));
        let Some(commit) = view.read(cx).current().cloned() else {
            self.set_notice("nothing selected to reset to");
            return;
        };
        if view.update(cx, |v, _| v.confirm_or_arm_reset(&commit.sha)) {
            // The question was standing and this press spent it.
            self.set_notice("reset cancelled");
            return;
        }
        self.set_question(Self::reset_question(&commit));
        // The arm just opened; the question's letters are live this frame.
        self.sync_modes(cx);
    }

    /// The band's sentence for a standing reset question: the target and the
    /// three answers, each in the ink of nothing — the band is `dim`, and the
    /// letters are read, not hunted.
    fn reset_question(commit: &Commit) -> String {
        format!(
            "reset to {}? s soft · m mixed · h hard · esc cancels",
            commit.short
        )
    }

    fn reset_selected(&mut self, command: &str, cx: &mut Context<Self>) {
        let Some(Screen::Commits { view, .. }) = self.active() else {
            self.set_notice(format!("{command} is not supported here"));
            return;
        };
        let view = view.clone();
        let host = config::host(cx);
        // Meet the list where it actually is, like open-diff: a scrollbar
        // drag moved the offset without moving the cursor.
        view.update(cx, |v, _| v.reconcile(&host));
        let Some(commit) = view.read(cx).current().cloned() else {
            self.set_notice("nothing selected to reset to");
            return;
        };
        let Some(writes) = self.writes() else {
            self.set_notice("a fixture has no repository to reset in");
            return;
        };
        let mode = match command {
            "commits.reset-soft" => ResetMode::Soft,
            "commits.reset-mixed" => ResetMode::Mixed,
            _ => ResetMode::Hard,
        };
        // A strength letter answers a standing question and does nothing
        // else. The check is *before* any arming — the arm is `g`'s to set,
        // and a letter that opened a question by itself would be two presses
        // deciding a hard reset. Only a stale mode after a cursor move can
        // resolve a strength tonight; the mode stack is re-synced so it
        // stops.
        if !view.read(cx).armed() {
            self.set_notice("no reset is being asked — press g to ask");
            self.sync_modes(cx);
            return;
        }
        if !view.update(cx, |v, _| v.confirm_or_arm_reset(&commit.sha)) {
            // Armed on a different commit — the cursor moved since `g`
            // without a command running to drop the arm. The row moved; the
            // question asks again rather than landing on the wrong sha.
            self.set_question(Self::reset_question(&commit));
            return;
        }
        self.notice = None; // the question is spent; the running band speaks next
        let job =
            gitten_app::verbs::Write::reset(&writes.repo, mode, commit.sha.clone().into_bytes());
        if !writes.send(Box::new(job)) {
            self.set_notice("the job queue is shutting down");
        }
    }

    /// `commits.revert`: land the inverse of the commit the keyboard is on
    /// as a new commit. Nothing existing moves or is destroyed — dropping
    /// the result undoes the undo — so there is no confirmation dance; a
    /// conflicted revert refuses with git's own words and leaves its
    /// question in the working tree.
    fn revert_selected(&mut self, cx: &mut Context<Self>) {
        let Some(Screen::Commits { view, .. }) = self.active() else {
            self.set_notice("commits.revert is not supported here");
            return;
        };
        let view = view.clone();
        let host = config::host(cx);
        view.update(cx, |v, _| v.reconcile(&host));
        let Some(commit) = view.read(cx).current().cloned() else {
            self.set_notice("nothing selected to revert");
            return;
        };
        let Some(writes) = self.writes() else {
            self.set_notice("a fixture has no repository to revert in");
            return;
        };
        let job = gitten_app::verbs::Write::revert(&writes.repo, commit.sha.into_bytes());
        if !writes.send(Box::new(job)) {
            self.set_notice("the job queue is shutting down");
        }
    }

    /// `commits.cherry-pick`: apply the commit under the keyboard onto the
    /// current branch as a new commit. Nothing existing moves and the
    /// original stays where it is — dropping the copy undoes the pick — so
    /// there is no confirmation dance; a conflicted pick refuses with git's
    /// own words and leaves its question in the working tree, found by the
    /// re-acquire every finish schedules.
    ///
    /// Detached HEAD refuses here rather than in git's sentence: a pick
    /// lands on *the current branch*, and the reader aimed at a row of
    /// history, so the honest answer names where the result would have gone.
    fn cherry_pick_selected(&mut self, cx: &mut Context<Self>) {
        let Some(Screen::Commits { view, .. }) = self.active() else {
            self.set_notice("commits.cherry-pick is not supported here");
            return;
        };
        let view = view.clone();
        let host = config::host(cx);
        // Meet the list where it actually is, like every verb above: a
        // scrollbar drag moved the offset without moving the cursor.
        view.update(cx, |v, _| v.reconcile(&host));
        let Some(commit) = view.read(cx).current().cloned() else {
            self.set_notice("nothing selected to cherry-pick");
            return;
        };
        let Some(writes) = self.writes() else {
            self.set_notice("a fixture has no repository to cherry-pick in");
            return;
        };
        use gitten_core::refs::HeadState;
        match writes.repo.head() {
            Ok(HeadState::Branch { .. }) => {}
            Ok(HeadState::Detached { .. }) => {
                self.set_notice("HEAD is detached here; a cherry-pick needs a branch to land on");
                return;
            }
            Err(e) => {
                self.set_notice(e);
                return;
            }
        }
        let job = gitten_app::verbs::Write::cherry_pick(&writes.repo, commit.sha.into_bytes());
        if !writes.send(Box::new(job)) {
            self.set_notice("the job queue is shutting down");
        }
    }

    /// `commits.squash-up` / `commits.fixup-up` / `commits.drop-commit`:
    /// rewrite the branch with the commit under the keyboard folded into
    /// its parent or gone entirely.
    ///
    /// The plan is composed over the same window of history the pane drew —
    /// [`gitten_core::rebase::compose`] refuses anything it cannot cover
    /// whole: merges (a rebase would flatten them), side commits
    /// interleaved into the window (a wholesale plan would drop their
    /// changes), a root under the keyboard. Those refusals arrive here as
    /// sentences instead of jobs.
    ///
    /// All three rewrite history — commits leave the branch, recoverable
    /// only through the reflog — so each asks twice, exactly as reset-hard
    /// does: first press arms the row and says so, any cursor move, wheel
    /// or refresh disarms, second press on the same commit builds the job.
    fn rewrite_selected(&mut self, command: &str, cx: &mut Context<Self>) {
        use gitten_core::rebase::{compose, Rewrite};
        let kind = match command {
            "commits.squash-up" => Rewrite::SquashUp,
            "commits.fixup-up" => Rewrite::FixupUp,
            _ => Rewrite::Drop,
        };
        let Some(Screen::Commits { view, .. }) = self.active() else {
            self.set_notice(format!("{command} is not supported here"));
            return;
        };
        let view = view.clone();
        let host = config::host(cx);
        // Meet the list where it actually is, like every verb above: a
        // scrollbar drag moved the offset without moving the cursor.
        view.update(cx, |v, _| v.reconcile(&host));
        let Some(commit) = view.read(cx).current().cloned() else {
            self.set_notice("nothing selected to rewrite");
            return;
        };
        let Some(writes) = self.writes() else {
            self.set_notice("a fixture has no repository to rewrite");
            return;
        };
        if !view.update(cx, |v, _| v.confirm_or_arm_rewrite(&commit.sha)) {
            let asked = match kind {
                Rewrite::SquashUp => format!(
                    "squash {} into its parent? press again to confirm",
                    commit.short
                ),
                Rewrite::FixupUp => format!(
                    "fixup {} into its parent? press again to confirm",
                    commit.short
                ),
                Rewrite::Drop => format!("drop {}? press again to confirm", commit.short),
            };
            self.set_question(asked);
            return;
        }
        self.notice = None; // the question is spent; the running band speaks next

        // The same window acquisition loaded the pane from; composing over
        // less would be composing over a lie.
        const LOG_WINDOW: usize = 5000;
        let history = match writes.repo.log(LOG_WINDOW) {
            Ok(history) => history,
            Err(e) => {
                self.set_notice(e);
                return;
            }
        };
        let index = history.iter().position(|c| c.sha == commit.sha);
        let (upstream, script) = match index.map(|i| compose(kind, &history, i)) {
            Some(Ok(composed)) => composed,
            Some(Err(reason)) => {
                self.set_notice(reason);
                return;
            }
            None => {
                self.set_notice(format!(
                    "{} is older than the {} commits loaded, so a plan built \
                     from this window could not be complete",
                    commit.short, LOG_WINDOW
                ));
                return;
            }
        };
        let job = gitten_app::verbs::Write::rebase_todo(&writes.repo, upstream, script);
        if !writes.send(Box::new(job)) {
            self.set_notice("the job queue is shutting down");
        }
    }

    /// `rebase.abort` / `rebase.continue`: drive the rebase git is holding
    /// mid-flight. Abort puts branch, index and working tree back where the
    /// rewrite started; continue carries it onward once a human has
    /// resolved whatever stopped it — and a further conflict comes back
    /// refused in git's words with the state standing, ready to drive again.
    /// Neither reads a pane and neither asks twice: both only ever mean
    /// something while a stranded state exists, and git answers "no rebase
    /// in progress" verbatim when there is none.
    fn rebase_abort_command(&mut self, _cx: &mut Context<Self>) {
        let Some(writes) = self.writes() else {
            self.set_notice("a fixture has no repository to abort in");
            return;
        };
        if !writes.send(Box::new(gitten_app::verbs::Write::rebase_abort(
            &writes.repo,
        ))) {
            self.set_notice("the job queue is shutting down");
        }
    }

    fn rebase_continue_command(&mut self, _cx: &mut Context<Self>) {
        let Some(writes) = self.writes() else {
            self.set_notice("a fixture has no repository to continue in");
            return;
        };
        if !writes.send(Box::new(gitten_app::verbs::Write::rebase_continue(
            &writes.repo,
        ))) {
            self.set_notice("the job queue is shutting down");
        }
    }

    /// `commits.cherry-pick-abort` / `commits.cherry-pick-continue`: drive
    /// the cherry-pick git is holding mid-flight — the same door as
    /// rebase.abort / rebase.continue, on its own names. Rebase's capitals
    /// answer a *rebase* state; run over `CHERRY_PICK_HEAD` they come back
    /// "no rebase in progress", true and useless. Abort puts branch, index
    /// and working tree back where the pick started; continue carries it
    /// onward once conflicts are resolved, a further conflict refused in
    /// git's words with the state standing.
    fn cherry_pick_abort_command(&mut self, _cx: &mut Context<Self>) {
        let Some(writes) = self.writes() else {
            self.set_notice("a fixture has no repository to abort in");
            return;
        };
        if !writes.send(Box::new(gitten_app::verbs::Write::cherry_pick_abort(
            &writes.repo,
        ))) {
            self.set_notice("the job queue is shutting down");
        }
    }

    fn cherry_pick_continue_command(&mut self, _cx: &mut Context<Self>) {
        let Some(writes) = self.writes() else {
            self.set_notice("a fixture has no repository to continue in");
            return;
        };
        if !writes.send(Box::new(gitten_app::verbs::Write::cherry_pick_continue(
            &writes.repo,
        ))) {
            self.set_notice("the job queue is shutting down");
        }
    }

    /// `commits.rebase-onto`: move the branch HEAD is on onto the row the
    /// keyboard is on — plain rebase, no plan, on the same terms as every
    /// other write: a dirty tree is git's refusal verbatim, a conflict
    /// leaves its state standing for [`Write::rebase_abort`] to undo.
    ///
    /// The key lives in [branches], because that is where the thing aimed at
    /// lives — lazygit keeps its rebase key there too. Rewrites this
    /// branch's own commits, so it asks twice like the fold verbs do.
    fn rebase_branch_selected(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.active(), Some(Screen::Branches { .. })) {
            self.set_notice("commits.rebase-onto is not supported here");
            return;
        }
        let Some(target) = self.branches_target(cx) else {
            self.set_notice("nothing selected to rebase onto");
            return;
        };
        let shown = match &target {
            views::branches::Target::Local(name) => name.to_string_lossy().into_owned(),
            views::branches::Target::Remote { remote, branch } => {
                format!("{}/{}", remote.to_string_lossy(), branch.to_string_lossy())
            }
            views::branches::Target::Detached => String::from("(detached)"),
        };
        let Some(Screen::Branches { view, .. }) = self.active() else {
            unreachable!("checked above");
        };
        if matches!(target, views::branches::Target::Detached) {
            self.set_notice("HEAD is detached here; check out a branch first");
            return;
        }
        let Some(writes) = self.writes() else {
            self.set_notice("a fixture has no repository to rebase in");
            return;
        };
        if !view.update(cx, |b, _| b.confirm_or_arm_rebase(&target)) {
            self.set_question(format!(
                "rebase this branch onto {shown}? press again to confirm"
            ));
            return;
        }
        self.notice = None; // the question is spent; the running band speaks next
        let upstream = match target {
            views::branches::Target::Local(name) => name.as_bytes().to_vec(),
            views::branches::Target::Remote { remote, branch } => {
                // The full refname git resolves, joined from the halves the
                // model keeps apart because either may hold a slash.
                let mut full = remote.as_bytes().to_vec();
                full.push(b'/');
                full.extend_from_slice(branch.as_bytes());
                full
            }
            views::branches::Target::Detached => unreachable!("refused above"),
        };
        let job = gitten_app::verbs::Write::rebase_onto(&writes.repo, upstream);
        if !writes.send(Box::new(job)) {
            self.set_notice("the job queue is shutting down");
        }
    }

    // ---------------------------------------------------- the branch verbs

    /// The focused branches pane's target — what the keyboard is on, as
    /// verbs aim at it. `None` when the pane is not up at all.
    fn branches_target(&self, cx: &App) -> Option<views::branches::Target> {
        match self.active() {
            Some(Screen::Branches { view, .. }) => view.read(cx).current(),
            _ => None,
        }
    }

    /// `branches.checkout`: move HEAD onto the row the keyboard is on.
    ///
    /// A remote-tracking row checks out too, and detaches onto the fetched
    /// commit — git's own answer to "look at what the server has", and the
    /// reason [`views::branches::Target`] carries remotes at all. The one
    /// refusal said here is the detached row itself: already a place, not a
    /// branch to move to. Everything else — dirty tree, unknown name — is
    /// git's sentence, surfaced verbatim by the job.
    fn checkout_branch(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.active(), Some(Screen::Branches { .. })) {
            self.set_notice("branches.checkout is not supported here");
            return;
        }
        let Some(target) = self.branches_target(cx) else {
            self.set_notice("nothing selected to check out");
            return;
        };
        if matches!(target, views::branches::Target::Detached) {
            self.set_notice("HEAD is already detached here");
            return;
        }
        let Some(writes) = self.writes() else {
            self.set_notice("a fixture has no repository to check out in");
            return;
        };
        let name = match target {
            views::branches::Target::Local(name) => name,
            views::branches::Target::Remote { remote, branch } => {
                // The full refname git resolves: `origin/main`, joined from
                // the halves the model keeps apart because either may hold
                // a slash.
                let mut full = remote.as_bytes().to_vec();
                full.push(b'/');
                full.extend_from_slice(branch.as_bytes());
                gitten_core::status::PathBytes::from_bytes(&full)
            }
            views::branches::Target::Detached => unreachable!("refused above"),
        };
        let job = gitten_app::verbs::Write::checkout(&writes.repo, name.as_bytes().to_vec());
        if !writes.send(Box::new(job)) {
            self.set_notice("the job queue is shutting down");
        }
    }

    /// `repo.push` / `repo.pull` / `repo.fetch`: the repository's sync verbs.
    /// No row is read and none is needed — pull lets git resolve the current
    /// branch's own upstream, fetch takes every remote, and push's aiming is
    /// [`Write::push_current`](gitten_app::verbs::Write::push_current)'s,
    /// whose refusals arrive here as sentences instead of jobs.
    fn sync_remote(&mut self, command: &str, _cx: &mut Context<Self>) {
        let Some(writes) = self.writes() else {
            self.set_notice("a fixture has no repository to sync");
            return;
        };
        let job = match command {
            "repo.pull" => gitten_app::verbs::Write::pull(&writes.repo),
            "repo.fetch" => gitten_app::verbs::Write::fetch(&writes.repo, None),
            _ => match gitten_app::verbs::Write::push_current(&writes.repo) {
                Ok(job) => job,
                Err(reason) => {
                    self.set_notice(reason);
                    return;
                }
            },
        };
        if !writes.send(Box::new(job)) {
            self.set_notice("the job queue is shutting down");
        }
    }

    /// `stashes.apply` / `stashes.pop` / `stashes.drop`: act on the row the
    /// keyboard is on, addressed by its index — which is also why only the
    /// drop asks twice. Apply and pop are recoverable in every direction that
    /// matters (a kept entry, an apply that refused); a drop is final, so it
    /// arms like a discard and any cursor move, wheel or refresh disarms it —
    /// after a drop the numbers shift, and a yes aimed at yesterday's
    /// numbering is exactly the accident the double press exists to prevent.
    fn stash_selected(&mut self, command: &str, cx: &mut Context<Self>) {
        let Some(Screen::Stashes { view, .. }) = self.active() else {
            self.set_notice(format!("{command} is not supported here"));
            return;
        };
        let under = view
            .read(cx)
            .current()
            .map(|r| (r.index, r.title.to_string()));
        let Some((index, shown)) = under else {
            self.set_notice("nothing selected on the stash stack");
            return;
        };
        let Some(writes) = self.writes() else {
            self.set_notice("a fixture has no stash stack to act on");
            return;
        };
        if command == "stashes.drop" && !view.update(cx, |s, _| s.confirm_or_arm_drop(index)) {
            // First press on this row: asked, not acted.
            self.set_question(views::stashes::drop_question(&shown));
            return;
        }
        if command == "stashes.drop" {
            self.notice = None; // the question is spent; the running band speaks next
        }
        let job = match command {
            "stashes.apply" => gitten_app::verbs::Write::stash_apply(&writes.repo, index),
            "stashes.pop" => gitten_app::verbs::Write::stash_pop(&writes.repo, index),
            _ => gitten_app::verbs::Write::stash_drop(&writes.repo, index),
        };
        if !writes.send(Box::new(job)) {
            self.set_notice("the job queue is shutting down");
        }
    }

    /// `branches.new`: gather a name over the pane; accept creates at HEAD.
    /// Creating never checks out — HEAD stays where it was, which is why
    /// this needs no confirmation dance.
    fn begin_branch_new(&mut self, cx: &mut Context<Self>) {
        self.begin_branch_prompt(BranchPrompt::New, "branch name", "", cx);
    }

    /// `commits.new-branch`: the branches pane's own field, aimed one pane
    /// over — the branch grows from the commit under the keyboard, whose sha
    /// is captured at open time like a tag's. Same pane guard, said the same
    /// way: a verb is its pane's, and the sentence names it.
    fn begin_commit_branch_prompt(&mut self, cx: &mut Context<Self>) {
        let Some(Screen::Commits { view, .. }) = self.active() else {
            self.set_notice("commits.new-branch is not supported here");
            return;
        };
        let host = config::host(cx);
        view.update(cx, |v, _| v.reconcile(&host));
        let Some(commit) = view.read(cx).current().cloned() else {
            self.set_notice("nothing selected to branch from");
            return;
        };
        if self.writes().is_none() {
            self.set_notice("a fixture has no repository to create branches in");
            return;
        }
        self.begin_named_branch_prompt(
            BranchPrompt::NewAt { start: commit.sha },
            "new branch from commit",
            cx,
        );
    }

    /// Opens the shared field for a [`BranchPrompt`] over *this* pane,
    /// whichever kind it is — the branches variant pins the guard to its own
    /// pane; this one trusts the caller, who has already checked.
    fn begin_named_branch_prompt(
        &mut self,
        what: BranchPrompt,
        label: &str,
        cx: &mut Context<Self>,
    ) {
        if self.repo.is_none() {
            self.set_notice("a fixture has no repository to create branches in");
            return;
        }
        let input = cx.new(|cx| input::Input::new(label, "branch name", "", cx));
        self.open_input(input, cx);
        // After `open_input`, which may have cancelled a previous prompt.
        self.prompt = Some(Prompt::BranchName {
            target: self.panes.focused_name().to_string(),
            what,
        });
    }

    /// `branches.new-tag`: the commits pane's tag field, aimed at the branch
    /// row. The tag's target is the branch *name* — a revspec git resolves
    /// the same way it resolves a sha — so no commit read rides along, and
    /// the tag moves with the branch the way a branch-shaped tag should.
    fn begin_branch_tag_prompt(&mut self, cx: &mut Context<Self>) {
        let Some(views::branches::Target::Local(name)) = self.branches_target(cx) else {
            self.set_notice("only a local branch can be tagged here");
            return;
        };
        if self.writes().is_none() {
            self.set_notice("a fixture has no repository to tag in");
            return;
        }
        let shown = name.to_string_lossy().into_owned();
        let input = cx.new(|cx| input::Input::new("new tag", "tag name", "", cx));
        self.open_input(input, cx);
        // After `open_input`, which may have cancelled a previous prompt.
        self.prompt = Some(Prompt::TagName {
            target: self.panes.focused_name().to_string(),
            at: shown,
        });
    }

    /// `commits.checkout`: detach onto the commit under the keyboard,
    /// lazygit's space. Not asked twice — nothing is destroyed, HEAD's old
    /// branch keeps its name and the branches pane's space walks back — and
    /// refused over a fixture like every write, by the absence of rails.
    fn checkout_commit(&mut self, cx: &mut Context<Self>) {
        let Some(Screen::Commits { view, .. }) = self.active() else {
            self.set_notice("commits.checkout is not supported here");
            return;
        };
        let host = config::host(cx);
        view.update(cx, |v, _| v.reconcile(&host));
        let Some(commit) = view.read(cx).current().cloned() else {
            self.set_notice("nothing selected to check out");
            return;
        };
        let Some(writes) = self.writes() else {
            self.set_notice("a fixture has no repository to check out in");
            return;
        };
        let job = gitten_app::verbs::Write::checkout(&writes.repo, commit.sha.clone().into_bytes());
        if !writes.send(Box::new(job)) {
            self.set_notice("the job queue is shutting down");
        }
    }

    /// `branches.rename`: the same field, pre-filled with the row's own
    /// name — editing what is there beats retyping it, and accepting
    /// unchanged text answers with git's "already exists", which says more
    /// than a client-side veto would.
    fn begin_branch_rename(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.active(), Some(Screen::Branches { .. })) {
            self.set_notice("branches.rename is not supported here");
            return;
        }
        let Some(views::branches::Target::Local(name)) = self.branches_target(cx) else {
            self.set_notice("only a local branch can be renamed");
            return;
        };
        // Pre-fill only when the bytes *are* text. A legal Latin-1 name
        // decodes lossily into something with U+FFFD in it — a different
        // name than the branch has — and accepting the field unchanged
        // would then rename the branch to its own mojibake. The bytes are
        // carried in the prompt regardless; an empty field is the honest
        // shape for a name this field cannot show.
        let initial = std::str::from_utf8(name.as_bytes()).unwrap_or("");
        self.begin_branch_prompt(
            BranchPrompt::Rename {
                from: name.as_bytes().to_vec(),
            },
            "rename branch",
            initial,
            cx,
        );
    }

    /// Opens the shared field for a [`BranchPrompt`]. The slot carries the
    /// pane registration name so the answer routes back to its pane, the
    /// same promise `/` keeps.
    fn begin_branch_prompt(
        &mut self,
        what: BranchPrompt,
        label: &str,
        initial: &str,
        cx: &mut Context<Self>,
    ) {
        if !matches!(self.active(), Some(Screen::Branches { .. })) {
            self.set_notice("this command belongs to the branches pane");
            return;
        }
        if self.repo.is_none() {
            self.set_notice("a fixture has no repository to create branches in");
            return;
        }
        let input = cx.new(|cx| {
            let mut input = input::Input::new(label, "branch name", initial, cx);
            // A pre-filled name arrives selected: typing replaces it, which
            // is what a rename wants — editing beats retyping, but keeping
            // the old name glued to the front of whatever was typed serves
            // nobody.
            if !initial.is_empty() {
                input.select_all_text(true, cx);
            }
            input
        });
        self.open_input(input, cx);
        // After `open_input`, which may have cancelled a previous prompt.
        self.prompt = Some(Prompt::BranchName {
            target: self.panes.focused_name().to_string(),
            what,
        });
    }

    /// The accepted branch name, as a job. Empty refused again here — the
    /// trait refuses it too, but saying so beside the field that just closed
    /// beats making the reader find out twice.
    fn branch_named(&mut self, target: &str, what: BranchPrompt, text: String) {
        if text.trim().is_empty() {
            self.set_notice("a branch needs a name");
            return;
        }
        if self.panes.position(target).is_none() {
            self.set_notice("the pane the branch was asked over is gone");
            return;
        }
        let Some(writes) = self.writes() else {
            self.set_notice("a fixture has no repository to create branches in");
            return;
        };
        let job = match what {
            BranchPrompt::New => {
                gitten_app::verbs::Write::create_branch(&writes.repo, text.into_bytes(), None)
            }
            BranchPrompt::NewAt { start } => gitten_app::verbs::Write::create_branch(
                &writes.repo,
                text.into_bytes(),
                Some(start.into_bytes()),
            ),
            BranchPrompt::Rename { from } => {
                gitten_app::verbs::Write::rename_branch(&writes.repo, from, text.into_bytes())
            }
        };
        if !writes.send(Box::new(job)) {
            self.set_notice("the job queue is shutting down");
        }
    }

    /// `commits.new-tag`: gather a name over the pane; accept names the
    /// commit under the keyboard. The sha is captured when the field opens,
    /// so nothing a cursor does while the field holds the keyboard can
    /// re-aim the tag.
    ///
    /// Tonight's tag is lightweight: the shared field gathers a name and
    /// nothing else, and an annotated tag wants a message field — a second
    /// prompt away, not invented here ahead of a pane that asks for it.
    fn begin_tag_prompt(&mut self, cx: &mut Context<Self>) {
        let Some(Screen::Commits { view, .. }) = self.active() else {
            self.set_notice("commits.new-tag is not supported here");
            return;
        };
        let host = config::host(cx);
        // Meet the list where its last drag left it, like every verb above.
        view.update(cx, |v, _| v.reconcile(&host));
        let Some(commit) = view.read(cx).current().cloned() else {
            self.set_notice("nothing selected to tag");
            return;
        };
        if self.writes().is_none() {
            self.set_notice("a fixture has no repository to tag in");
            return;
        }
        let input = cx.new(|cx| input::Input::new("new tag", "tag name", "", cx));
        self.open_input(input, cx);
        // After `open_input`, which may have cancelled a previous prompt.
        self.prompt = Some(Prompt::TagName {
            target: self.panes.focused_name().to_string(),
            at: commit.sha,
        });
    }

    /// The accepted tag name, as a job. Empty refused again here — the trait
    /// refuses it too, but saying so beside the field that just closed beats
    /// making the reader find out twice — and what is queued is the trimmed
    /// text, because git would hold the padding as part of the name. A
    /// duplicate rides on to git and comes back in its words ("tag 'v1'
    /// already exists"), which says more than a client-side veto would.
    fn tag_named(&mut self, target: &str, at: String, text: String) {
        let name = text.trim();
        if name.is_empty() {
            self.set_notice("a tag needs a name");
            return;
        }
        if self.panes.position(target).is_none() {
            self.set_notice("the pane the tag was asked over is gone");
            return;
        }
        let Some(writes) = self.writes() else {
            self.set_notice("a fixture has no repository to tag in");
            return;
        };
        let job = gitten_app::verbs::Write::create_tag(
            &writes.repo,
            name.as_bytes().to_vec(),
            at.into_bytes(),
            None,
        );
        if !writes.send(Box::new(job)) {
            self.set_notice("the job queue is shutting down");
        }
    }

    /// `branches.delete`: the destructive verb of this pane, confirmed on
    /// the keyboard exactly as `files.discard` is. First press arms the row
    /// and asks once in the band; second press on the same row deletes —
    /// merged work only, because an unmerged branch comes back refused in
    /// git's own words ("not fully merged") and that sentence is the force
    /// decision's proper home, not tonight's keymap.
    ///
    /// Remote rows refuse outright, on purpose: a tracking ref is the
    /// remote's shadow, and deleting it here would be a fetch's prune done
    /// by hand under a key that reads as something stronger.
    fn delete_branch_selected(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.active(), Some(Screen::Branches { .. })) {
            self.set_notice("branches.delete is not supported here");
            return;
        }
        let Some(target) = self.branches_target(cx) else {
            self.set_notice("nothing selected to delete");
            return;
        };
        let shown = match &target {
            views::branches::Target::Local(name) => String::from_utf8_lossy(name.as_bytes()),
            views::branches::Target::Remote { remote, branch } => {
                format!("{}/{}", remote.to_string_lossy(), branch.to_string_lossy()).into()
            }
            views::branches::Target::Detached => {
                self.set_notice("a detached HEAD is not a branch");
                return;
            }
        };
        if matches!(target, views::branches::Target::Remote { .. }) {
            self.set_notice("a remote branch is its remote's to delete — fetch prunes it here");
            return;
        }
        let Some(Screen::Branches { view, .. }) = self.active() else {
            unreachable!("checked above");
        };
        let Some(writes) = self.writes() else {
            self.set_notice("a fixture has no repository to delete branches from");
            return;
        };
        // Arm, or spend the arm. False means the question was just asked.
        if !view.update(cx, |b, _| b.confirm_or_arm_delete(&target)) {
            self.set_question(format!("delete branch {shown}? press again to confirm"));
            return;
        }
        self.notice = None; // the question is spent; the running band speaks next
        let name = match target {
            views::branches::Target::Local(name) => name.as_bytes().to_vec(),
            _ => unreachable!("remotes and detached refuse above"),
        };
        let job = gitten_app::verbs::Write::delete_branch(&writes.repo, name, false);
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
            // Filtering re-anchors the cursor by sha; when the anchor does
            // not survive, the keyboard lands somewhere else, and that
            // somewhere is what the main view should be loading.
            self.sync_main_diff(cx);
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
        self.sync_main_diff(cx);
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
        self.sync_modes(cx);
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
                    self.running = Some((format!("running {name}"), Instant::now()));
                    self.error = None;
                    // The seconds will not tick by themselves — GPUI draws
                    // nothing at rest — so a job that runs longer than a
                    // heartbeat needs a notifier of its own. It dies with the
                    // job: one tick past `running` going `None` at most.
                    cx.spawn(async move |shell, cx| loop {
                        cx.background_executor().timer(Duration::from_secs(1)).await;
                        let live = shell.update(cx, |shell, cx| {
                            if shell.running.is_some() {
                                cx.notify();
                                true
                            } else {
                                false
                            }
                        });
                        if !live.unwrap_or(false) {
                            break;
                        }
                    })
                    .detach();
                }
                JobEvent::Finished {
                    outcome: Err(error),
                    generation,
                    ..
                } => {
                    self.running = None;
                    self.error = Some(GitError::new(error));
                    // A refusal is not proof the repository stood still: git
                    // can answer nonzero with work already left behind, and
                    // the conflicted revert is the case that proves it — its
                    // unmerged paths sit in the index waiting for a human,
                    // who cannot resolve what no pane shows. Nothing on this
                    // queue reads, so every finish schedules the same
                    // re-acquire wave a success does.
                    if generation > self.generation {
                        self.generation = generation;
                        self.refresh_stale(cx);
                    }
                }
                JobEvent::Finished {
                    outcome: Ok(()),
                    generation,
                    done,
                    ..
                } => {
                    self.running = None;
                    if generation > self.generation {
                        self.generation = generation;
                        self.refresh_stale(cx);
                    }
                    // A job that named its finish gets its sentence in this
                    // band — the sync verbs' pushed/pulled/fetched. Said
                    // once, beside the facts the refresh above puts back on
                    // screen; the next key clears it like any other notice.
                    if let Some(done) = done {
                        // A write's finish is the band's own sentence: said
                        // once, beside the facts the refresh puts back on
                        // screen, and cleared by the next key like any other.
                        self.notice = Some(Notice::Info(done));
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
            // The diff main view rides the same wave: a working-tree revspec
            // in the main view is exactly as stale as any pane's after a
            // write. Commit-sha sources pay one cheap no-op re-acquire.
            .chain(std::iter::once(&self.main))
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
                self.error = self.refresh_error.take().map(GitError::new);
            } else {
                self.refresh_error = None;
            }
        }
        // A refresh may have re-anchored the commits cursor — the list it was
        // on changed under it — which is a selection change as far as the
        // main view is concerned.
        self.sync_main_diff(cx);
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
    ///
    /// The main view is the only diff there is, so this reads its revspec off
    /// [`DevShell::main`] directly rather than off whatever holds the
    /// keyboard: a picker in the title bar acts on what the title bar's
    /// pickers describe.
    fn set_overrides(&mut self, next: Overrides, cx: &mut Context<Self>) {
        let Some(rediff) = self.rediff.clone() else {
            return;
        };
        let Some(revision) = (match &self.main {
            Screen::Diff { source, .. } => match source.borrow().as_ref() {
                Some(Source::Repo { arg, .. }) => Some(arg.clone()),
                _ => None,
            },
            _ => None,
        }) else {
            return;
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
                let Screen::Diff { view, .. } = &self.main else {
                    return;
                };
                let view = view.clone();
                view.update(cx, |d, cx| d.replace(files, &host, cx));
                let load = view.read(cx).load.clone();
                if let Some(stats) = &mut self.stats {
                    stats.reloaded(load);
                }
            }
            // The old rows stay on screen, which is the right failure: they are
            // still a true diff, just not the one that was asked for.
            Err(e) => self.error = Some(GitError::new(e)),
        }
        cx.notify();
    }

    /// Costs no re-diff and no `prepare` — only where the lines break moves —
    /// which is why this one needs none of `set_overrides`' machinery.
    fn set_wrap(&mut self, index: usize, cx: &mut Context<Self>) {
        let Screen::Diff { view, .. } = &self.main else {
            return;
        };
        let view = view.clone();
        let host = config::host(cx);
        view.update(cx, |d, cx| d.set_wrap(index, &host, cx));
        cx.notify();
    }

    fn set_layout(&mut self, index: usize, cx: &mut Context<Self>) {
        let Screen::Diff { view, .. } = &self.main else {
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
            "message.show" => {
                // Only meaningful while an error stands: the overlay is the
                // error's full text, and there is nothing else to show.
                if self.error.is_some() {
                    self.show_message = !self.show_message;
                }
            }
            "quit" => cx.quit(),
            "help" => {
                self.help = !self.help;
                // Reopening starts at the top: the rows are a different
                // projection every time — the active modes' — and an offset
                // the last reading left is a promise about rows that no
                // longer exist.
                if self.help {
                    help::scroll_to_end(&self.help_scroll, false);
                }
                self.sync_modes(cx);
            }
            // While the panel stands, the movement verbs are the panel's: the
            // rows under it are occluded, and one of these keys moving the list
            // underneath instead would scroll something the reader cannot see.
            // The names are the map's — bound in the help mode in `core` — so
            // the routing here is the only client-side half of it.
            "view.scroll-down" | "view.scroll-up" if self.help => {
                help::scroll_by(
                    &self.help_scroll,
                    match command {
                        "view.scroll-up" => -1.0,
                        _ => 1.0,
                    },
                );
            }
            "view.top" | "view.bottom" if self.help => {
                help::scroll_to_end(&self.help_scroll, command == "view.bottom");
            }
            "back" => self.back(cx),
            "theme.cycle" => self.cycle_theme(cx),
            "input.accept" => self.close_input(true, cx),
            "input.cancel" => self.close_input(false, cx),
            "pane.next" => self.cycle_pane(1, cx),
            "pane.prev" => self.cycle_pane(-1, cx),
            // lazygit's pane moves, on h/l and the arrows: a walk across
            // every pane the window has, in reading order — the stack's
            // lists top to bottom, then the diff last. At either end the
            // move is answered and stays.
            "pane.left" => self.pane_walk(-1, cx),
            "pane.right" => self.pane_walk(1, cx),
            "status.focus" => self.focus_named("status", cx),
            // lazygit's R: refresh everything. The wave itself is the queue
            // finish's; this just rings the bell.
            "repo.refresh" => {
                let sent = self.writes().map(|w| w.send(Box::new(RefreshAll)));
                match sent {
                    Some(true) => {}
                    Some(false) => self.set_notice("the job queue is shutting down"),
                    None => self.set_notice("a fixture has nothing to refresh"),
                }
            }
            // lazygit's 0: the main view, from wherever the keyboard was.
            "diff.focus" => self.set_spot(Spot::Main, cx),
            "commits.focus" => self.focus_named("commits", cx),
            "files.focus" => self.focus_named("files", cx),
            "stashes.focus" => self.focus_named("stashes", cx),
            "branches.focus" => self.focus_named("branches", cx),
            "commits.open-diff" => self.focus_main(cx),
            "commits.search" => self.begin_search(cx),
            // History's verbs, aimed at the commit the keyboard is on. Reset
            // and revert read the pane; the write goes through the queue.
            // The reset question is lazygit's menu: `g` opens it, and the
            // three strengths answer it only while it stands — see
            // [`DevShell::sync_modes`] for the mode that captures s/m/h.
            "commits.reset-menu" => self.reset_menu(cx),
            "commits.reset-soft" | "commits.reset-mixed" | "commits.reset-hard" => {
                self.reset_selected(command, cx)
            }
            "commits.revert" => self.revert_selected(cx),
            "commits.cherry-pick" => self.cherry_pick_selected(cx),
            "commits.new-tag" => self.begin_tag_prompt(cx),
            "commits.new-branch" => self.begin_commit_branch_prompt(cx),
            // lazygit's space on a commit: detached checkout. HEAD's old
            // branch keeps its name, so the branches pane walks you back.
            "commits.checkout" => self.checkout_commit(cx),
            // History's rewrites, composed over the pane's own window of
            // log and run through the queue. All three ask twice.
            "commits.squash-up" | "commits.fixup-up" | "commits.drop-commit" => {
                self.rewrite_selected(command, cx)
            }
            // The working tree's verbs. Context comes from the focused pane,
            // the write from the job queue — and where either is missing, the
            // same honest sentence an unknown command gets.
            "files.stage" => self.stage_or_unstage(cx),
            "files.commit" => self.begin_commit_message(cx),
            "files.amend" => self.begin_amend_message(cx),
            "files.discard" => self.discard_selected(cx),
            "files.stage-all" => self.stage_all(cx),
            "files.ignore" => self.ignore_selected(cx),
            "files.stash" => self.stash_working_tree(cx),
            // The diff pane's verbs, aimed at the hunk under its keyboard.
            "diff.stage-hunk" | "diff.unstage-hunk" | "diff.discard-hunk" => {
                self.hunk_verb(command, cx)
            }
            // The stash stack's verbs.
            "stashes.apply" | "stashes.pop" | "stashes.drop" => self.stash_selected(command, cx),
            // The branches panel's verbs, over the same two rails.
            "branches.checkout" => self.checkout_branch(cx),
            "branches.new" => self.begin_branch_new(cx),
            "branches.rename" => self.begin_branch_rename(cx),
            "branches.new-tag" => self.begin_branch_tag_prompt(cx),
            "branches.delete" => self.delete_branch_selected(cx),
            // Aimed at the branch row the keyboard is on, so it lives with
            // the branches verbs even though the command name sits in the
            // commits family — the name says what happens to history; the
            // pane says where the aim comes from.
            "commits.rebase-onto" => self.rebase_branch_selected(cx),
            // The way out of a stranded rebase. Repository-level like the
            // sync verbs: whichever pane the keyboard sits over, they act
            // on the rebase state git is holding, never on a row.
            "rebase.abort" => self.rebase_abort_command(cx),
            "rebase.continue" => self.rebase_continue_command(cx),
            // The way out of a stranded cherry-pick — the same repository-
            // level shape, on its own names: rebase's answer to a pick state
            // is git's "no rebase in progress".
            "commits.cherry-pick-abort" => self.cherry_pick_abort_command(cx),
            "commits.cherry-pick-continue" => self.cherry_pick_continue_command(cx),
            // The repository-level sync verbs: whatever pane the keyboard
            // sits over, they act on the branch HEAD is on — which is why
            // their keys are globals.
            "repo.push" | "repo.pull" | "repo.fetch" => self.sync_remote(command, cx),
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
        // The keyboard may just have moved the commits cursor — or a refresh
        // may have re-anchored it under the last command. Either way this is
        // the one hook every command leaves through, so it is where the main
        // view learns its selection changed — and where the mode stack learns
        // the reset question ended: a cursor move disarms it inside the view,
        // and the question's letters must stop capturing the moment it is
        // gone. One read on the no-op path.
        self.sync_modes(cx);
        self.sync_main_diff(cx);
        cx.notify();
    }

    /// Closes the help, the picker over it, the input field, or the diff
    /// region's hold on the keyboard.
    ///
    /// One key for all of it, because all of it is "get me out of this" — and
    /// **innermost first**, or a picker left open after its context is popped
    /// keeps occluding nothing: invisible, but still in `self.open`, where
    /// [`DevShell::on_wheel`] swallows every event for it forever. So an open
    /// menu is the whole of this `esc`: closed, pending dropped with it.
    ///
    /// With nothing stacked above, `esc` hands the keyboard back from the
    /// diff to the stack — lazygit's way out of a main view. The lists
    /// themselves are never closed any more: they are the stack's residents,
    /// and closing one would leave the window half empty rather than one pane
    /// lighter. A selection is inside a list, so it goes after the region
    /// switch; the diff's own selection stays until its rows are replaced.
    fn back(&mut self, cx: &mut Context<Self>) {
        if self.show_message {
            self.show_message = false;
            self.sync_modes(cx);
            cx.notify();
            return;
        }
        if self.help {
            self.help = false;
            self.sync_modes(cx);
            return;
        }
        // An error is a message, not a context: it stands until dismissed, and
        // `esc` dismisses it before `back` moves anything else.
        if self.error.is_some() {
            self.error = None;
            self.sync_modes(cx);
            cx.notify();
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
        if self.spot == Spot::Main {
            self.set_spot(Spot::List, cx);
            cx.notify();
            return;
        }
        // The reset question, before anything else a list could say: `esc`
        // on a standing question is "never mind", not "move the cursor".
        if let Some(Screen::Commits { view, .. }) = self.active() {
            if view.read(cx).armed() {
                view.update(cx, |v, _| v.disarm());
                cx.notify();
                return;
            }
        }
        if let Some(screen) = self.active() {
            if screen.select(false, cx) {
                // There was a selection and it is gone; that is the whole of
                // this `esc`.
                cx.notify();
            }
        }
    }

    fn focus_pane(&mut self, at: usize, cx: &mut Context<Self>) {
        // Focusing a list means looking at that list: the keyboard goes
        // with it, out of the diff if it was there. The spot moves even
        // when the tenant was *already* the focused one — walking back
        // left from the diff lands on the pane that held the keyboard
        // before it left, and `5` from the diff must reach the stash it
        // names — which is why the registry's "no change" answer is not
        // allowed to end this method before the spot has.
        if self.panes.focus(at) || self.spot != Spot::List {
            self.set_spot(Spot::List, cx);
            self.sync_modes(cx);
            self.sync_focus(cx);
            cx.notify();
        }
    }

    /// Cycles the lists — what ctrl-j/ctrl-k do, walking `1 → 2 → 3 → 4`:
    /// the stack's four panes in drawing order, then any pane an extension
    /// registered. The command names still say *pane*: they were
    /// named for the panes that used to stack, and a rename would break
    /// every `[keys]` file in flight.
    fn cycle_pane(&mut self, by: isize, cx: &mut Context<Self>) {
        let order: Vec<String> = self.list_order().iter().map(|s| s.to_string()).collect();
        if order.len() < 2 {
            self.set_notice("this window has no second list to cycle to");
            return;
        }
        // The focused pane's place in that order; a diff standing in as the
        // root of a diff-shaped launch is not in it, and cycling from there
        // starts at the first sidebar pane.
        let focused = self.panes.focused_name().to_string();
        let current = order.iter().position(|name| *name == focused).unwrap_or(0);
        let next = (current as isize + by).rem_euclid(order.len() as isize) as usize;
        let name = order[next].clone();
        self.focus_named(&name, cx);
    }

    /// Walks the keyboard one pane over — what h/l and the arrows run. The
    /// order is the window's reading order: the stack's lists top to bottom
    /// ([`DevShell::list_order`], the same walk the number keys spell out),
    /// then the diff as the last stop. Left of the diff is the stack's foot;
    /// right of the last list is the diff; an edge answers and stays, which
    /// is what a walk that refuses to wrap must do to keep h/l a line and
    /// not a ring — the number keys already cover the jumping.
    fn pane_walk(&mut self, by: isize, cx: &mut Context<Self>) {
        let order: Vec<String> = self.list_order().iter().map(|s| s.to_string()).collect();
        if order.is_empty() {
            // A diff-shaped launch: the diff is the only pane there is.
            return;
        }
        match self.spot {
            Spot::Main => {
                if by < 0 {
                    let name = order[order.len() - 1].clone();
                    self.focus_named(&name, cx);
                }
            }
            Spot::List => {
                let focused = self.panes.focused_name().to_string();
                let Some(at) = order.iter().position(|name| *name == focused) else {
                    // The focused tenant is not in the walk order — it can
                    // only be an extension registered after this frame's
                    // order was read. The stack's top is the honest home.
                    if by < 0 {
                        self.focus_named(&order[0], cx);
                    }
                    return;
                };
                let next = at as isize + by;
                if next >= order.len() as isize {
                    self.set_spot(Spot::Main, cx);
                } else if next >= 0 {
                    let name = order[next as usize].clone();
                    self.focus_named(&name, cx);
                }
            }
        }
    }

    /// Focuses a tenant by its stable registration name — what `files.focus`
    /// and friends run: the named pane takes the keyboard, wherever it draws.
    /// Said, not swallowed, when nothing is registered under the name: a
    /// fixture has no working tree to show, and the honest answer to the key
    /// is the same sentence an unbound one gets.
    fn focus_named(&mut self, name: &str, cx: &mut Context<Self>) {
        match self.panes.position(name) {
            Some(at) => self.focus_pane(at, cx),
            None => self.set_notice(format!("no {name} pane")),
        }
    }

    /// `commits.open-diff`: hand the keyboard to the diff region, carrying the
    /// commit under the cursor. The load itself is already riding the
    /// debounce from the cursor move that got here — enter *flushes* it,
    /// because pressing enter on a row means that row and not whichever one a
    /// fast run was settling toward. From the diff, `esc` walks back through
    /// [`DevShell::back`].
    fn focus_main(&mut self, cx: &mut Context<Self>) {
        let Some(view) = self.column_commits() else {
            self.set_notice("no commit selected");
            return;
        };
        let host = config::host(cx);
        // Meet the list where it actually is: a scrollbar drag moved the offset
        // without moving the cursor, and "this commit" means the one being
        // *looked at*.
        view.update(cx, |v, _| v.reconcile(&host));
        if let Some(commit) = view.read(cx).current().cloned() {
            self.schedule_main_diff(commit, true, cx);
        }
        self.set_spot(Spot::Main, cx);
        cx.notify();
    }

    /// After anything that may have moved the commits cursor — a key, a
    /// search edit, a refresh that re-anchored the list: if the commit under
    /// the keyboard is not the one the main view names, schedule its diff.
    /// The no-op path is one read of the current row.
    fn sync_main_diff(&mut self, cx: &mut Context<Self>) {
        let Some(view) = self.column_commits() else {
            return;
        };
        let Some(commit) = view.read(cx).current().cloned() else {
            return;
        };
        let shown = self
            .head
            .borrow()
            .as_ref()
            .is_some_and(|h| h.sha == commit.sha);
        if shown {
            return;
        }
        self.schedule_main_diff(commit, false, cx);
    }

    /// Aims the main view at `commit`.
    ///
    /// **Load-on-settle, by timer guard.** Every schedule bumps
    /// [`DevShell::request`] and spawns one timer ([`DIFF_DEBOUNCE`], zero
    /// when flushing); a waking timer proceeds only if its request is still
    /// the newest, so a fast cursor run leaves one live timer — the settled
    /// row's — and the dead ones cost a wake and a compare each. The
    /// acquisition then runs on the background executor behind a second copy
    /// of the same guard, so an older load can never land over a newer one.
    ///
    /// The header is written *now*, not on arrival: the strip naming the
    /// commit whose diff is coming is what makes the load visible in frame
    /// one instead of an empty right half. An earlier failure stays on the
    /// error band until the new rows replace them — a refusal to load does
    /// not unname what is on screen.
    fn schedule_main_diff(&mut self, commit: Commit, immediate: bool, cx: &mut Context<Self>) {
        let shown = self
            .head
            .borrow()
            .as_ref()
            .is_some_and(|h| h.sha == commit.sha);
        // Already on screen and resting: nothing to schedule. Shown and still
        // loading: only a flush (enter) escalates the pending load to now.
        if shown && !(immediate && self.loading.get()) {
            return;
        }
        let Some((path, repo)) = self.repo.clone() else {
            return;
        };
        let req = self.request.get() + 1;
        self.request.set(req);
        self.loading.set(true);
        *self.head.borrow_mut() = Some(commit.clone());
        let source = Source::Repo {
            path,
            arg: commit.sha.clone(),
        };
        if let Screen::Diff {
            source: aim, label, ..
        } = &self.main
        {
            *aim.borrow_mut() = Some(source.clone());
            // The title names what is *coming*, the same promise the strip
            // makes — and the same shape `open_diff` labelled its pane with.
            label.replace(format!(
                "{} {}",
                &commit.sha[..commit.sha.len().min(8)],
                commit.subject
            ));
        }
        let delay = match immediate {
            true => Duration::ZERO,
            false => DIFF_DEBOUNCE,
        };
        cx.spawn(async move |shell, cx| {
            cx.background_executor().timer(delay).await;
            // Load half, on the executor: one acquisition plus one prepare.
            // Built inside the guard so a superseded request never spawns it.
            let mut job = None;
            let live = shell
                .update(cx, |shell, cx| {
                    if shell.request.get() != req {
                        return false;
                    }
                    // An owned host crosses the thread boundary — the same
                    // copy a pane refresh carries into its load half.
                    let host = (*config::host(cx)).clone();
                    let repo = repo.clone();
                    let source = source.clone();
                    job = Some(cx.background_spawn(async move {
                        let loaded = gitten_app::acquire::reacquire(
                            View::Diff,
                            &source,
                            &host,
                            Some(repo.as_ref()),
                            &Overrides::default(),
                        )?;
                        let Data::Diff(files) = &loaded.data else {
                            let e: String = "acquisition returned the wrong view".into();
                            return Err(e);
                        };
                        let prepared = views::diff::prepare_files(files, &host);
                        Ok((loaded.data, prepared, loaded.label))
                    }));
                    true
                })
                .unwrap_or(false);
            let Some(job) = job.filter(|_| live) else {
                return;
            };
            let outcome = job.await;
            // Apply half, guarded twice more: a newer request wins, and a
            // window that went away updates nothing.
            _ = shell.update(cx, move |shell, cx| {
                if shell.request.get() != req {
                    return;
                }
                shell.loading.set(false);
                match outcome {
                    Ok((Data::Diff(files), prepared, label)) => {
                        let Screen::Diff {
                            view, generation, ..
                        } = &shell.main
                        else {
                            return;
                        };
                        let host = config::host(cx);
                        view.update(cx, |d, cx| d.replace_prepared(files, prepared, &host, cx));
                        generation.set(shell.generation);
                        // The rows were acquired with the file's own settings,
                        // so the picks say so too — a stale override would be a
                        // strip describing an algorithm that did not produce
                        // this diff.
                        shell.over = Overrides::default();
                        // A success clears whatever the previous load said.
                        shell.error = None;
                        if let Screen::Diff { label: cell, .. } = &shell.main {
                            cell.replace(label);
                        }
                    }
                    Ok(_) => {}
                    Err(e) => shell.error = Some(GitError::new(e)),
                }
                cx.notify();
            });
        })
        .detach();
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
            Some(Screen::Stashes { view, .. }) => {
                let view = view.clone();
                view.update(cx, |s, _| s.reconcile(&host));
                let text = view.read(cx).cursor_text();
                if !text.is_empty() {
                    // The address first, then the message — same rule.
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                }
            }
            Some(Screen::Branches { view, .. }) => {
                let view = view.clone();
                view.update(cx, |b, _| b.reconcile(&host));
                // The bare refname — the spelling every git command takes.
                let text = view.read(cx).cursor_text();
                if !text.is_empty() {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                }
            }
            Some(Screen::Custom(pane)) => {
                let writes = self.writes();
                pane.run("copy.selection", &host, writes.as_ref(), cx);
            }
            // The status pane has no row to copy: nothing selected, nothing
            // under a cursor.
            Some(Screen::Status { .. }) | None => {}
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
            // While the help panel stands it owns the keyboard the same way a
            // native field does: resolved against its mode *alone*, so a chord
            // the map does not give it runs nothing underneath — a pane's `D`
            // reads as "not bound" instead of arming a discard behind a screen
            // that is only describing it. `Resolve::None` below says so.
            false if self.help => host.keys.resolve_mode_any(help::MODE, &typed),
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
        // Over one region's rows or the other's, and not over the title bar
        // or a dropdown above them. Focus that region before resolving the
        // wheel: otherwise an unfocused pane's native list scroller would
        // become a second, unconfigured input path when this capture handler
        // stood aside.
        let in_stack = self.has_column.then(|| {
            self.panes
                .iter()
                .position(|screen| screen.list_bounds(cx).contains(&ev.position))
        });
        let over_main = self.main.list_bounds(cx).contains(&ev.position);
        let screen = match (in_stack.flatten(), over_main) {
            // The stack keeps its per-list hit test: whichever list is
            // showing owns its own box.
            (Some(at), _) => {
                self.focus_pane(at, cx);
                self.panes.focused().clone()
            }
            (None, true) => {
                self.set_spot(Spot::Main, cx);
                self.main.clone()
            }
            (None, false) => return,
        };
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

        // Everything below drives the diff main view, which is always on
        // screen now — the pickers no longer come and go with what holds the
        // keyboard.
        let Screen::Diff { view, source, .. } = &self.main else {
            return vec![theme_picker];
        };
        let source = source.borrow().clone();

        let names = view.read(cx).layout_names();
        let layouts = controls::Picker::new("layout", &names, view.read(cx).layout_index());

        // Straight off the registry in `core`, so a wrap an extension registers
        // is in this menu the day it exists.
        let wrap_names = view.read(cx).wrap_names(host);
        let wrap = controls::Picker::new("wrap", &wrap_names, view.read(cx).wrap_index());

        // An algorithm or a whitespace rule only means something when a
        // repository produced these rows; a fixture was diffed by somebody
        // else and says so by drawing the control inert.
        let from_repo = self.rediff.is_some() && matches!(source, Some(Source::Repo { .. }));
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
        .enabled(from_repo);

        let ws_names: Vec<&str> = Whitespace::ALL.iter().map(|w| w.name()).collect();
        let ws = self.over.whitespace.unwrap_or(host.differ.whitespace);
        let whitespace = controls::Picker::new(
            "whitespace",
            &ws_names,
            Whitespace::ALL.iter().position(|w| *w == ws).unwrap_or(0),
        )
        .enabled(from_repo);

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

    /// The stack's flexible foot: the commit list, or whichever extension
    /// pane took its region over. Focused-and-not-a-sidebar-name is exactly
    /// "a custom pane stands here", so the header's name and the accent
    /// follow it without a third state to check.
    fn commits_section(
        &self,
        host: &Rc<Host>,
        focused_name: &str,
        cx: &mut Context<Self>,
        out: &mut Vec<AnyElement>,
    ) {
        if !self.has_column {
            return;
        }
        let c = host.theme.chrome;
        let custom = matches!(self.panes.focused(), Screen::Custom(_));
        let screen = match custom {
            true => self.panes.focused(),
            false => self
                .panes
                .get("commits")
                .unwrap_or_else(|| self.panes.focused()),
        };
        let focused = self.spot == Spot::List && (custom || focused_name == "commits");
        // Right-edge furniture: the branch the list is of — the design pins
        // it here, where a checkout rewrites it in place — and a live
        // filter's count when there is one, because the filter is the thing
        // that changed most recently and the thing a count is about. Both
        // read from state the refresh wave already paid for.
        let right = match screen {
            Screen::Commits { view, .. } => {
                let note = view.read(cx).filter_note();
                let branch = (!note.is_some())
                    .then(|| {
                        self.panes.get("branches").and_then(|s| match s {
                            Screen::Branches { view, .. } => view.read(cx).head_info(),
                            _ => None,
                        })
                    })
                    .flatten()
                    .map(|info| info.branch);
                note.map(|note| {
                    div()
                        .flex_none()
                        // The filter is the thing that changed most recently
                        // and the thing the count is about — read, not glanced
                        // at, so it clears the furniture floor.
                        .text_color(rgb(host.theme.quiet_on(c.title_bg)))
                        .child(SharedString::from(note))
                        .into_any_element()
                })
                .or_else(|| {
                    // Dim, not accent: the accent is the keyboard's mark, and
                    // a branch name is a fact about the strip it sits on — raw
                    // dim is under the text floor there, so it resolves.
                    branch.map(|branch| {
                        div()
                            .flex_none()
                            .text_color(rgb(host.theme.dim_on(theme::Surface::Title)))
                            .child(branch)
                            .into_any_element()
                    })
                })
            }
            _ => None,
        };
        let name: SharedString = match custom {
            true => screen.label(cx).into(),
            false => "COMMITS".into(),
        };
        out.push(
            div()
                .id("side-commits")
                .debug_selector(|| "side-commits".to_string())
                .flex_grow(1.0)
                // A zero basis: the section takes the space the content-sized
                // sections leave, from nothing — never its own content's idea
                // of a height, which a virtualized list has not got. The
                // floor is the least a list can show and still be seen to
                // scroll.
                .flex_basis(px(0.0))
                .min_h(px(SECTION_MIN_H))
                .flex()
                .flex_col()
                .overflow_hidden()
                .capture_any_mouse_down(cx.listener(move |this, _, _, cx| {
                    // A click always means "the keyboard comes back here" —
                    // including from the diff. Focusing first keeps a
                    // takeover standing; the spot follows either way, which
                    // `focus_named` alone would not do when the pane was
                    // already the focused one.
                    if !custom {
                        this.focus_named("commits", cx);
                    }
                    this.set_spot(Spot::List, cx);
                }))
                .child(chrome::pane_header(host, "4", name, None, focused, right))
                .child(
                    div()
                        .min_h_0()
                        .flex_grow(1.0)
                        .overflow_hidden()
                        .child(screen.any()),
                )
                .into_any_element(),
        );
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

        // **The two regions**: the left stack — files, branches, stashes and
        // the commit list — and the diff filling the rest, lazygit's shape.
        // Every region is drawn with the same furniture: a short header
        // naming the pane with *the number of the key that focuses it*, and
        // a hairline under it. The keyboard's region says so through its
        // header — the bar on its left edge, the keycap and name in the
        // accent — and through the selected row's bar; the regions themselves
        // are parted by one hairline, so the one accent on screen is where
        // the keyboard is and nothing else.
        //
        // The stack's short panes render at once and are **as tall as their
        // content**: five files take five rows and the branches sit directly
        // under them, the way the design stacks them — not a quarter of the
        // column each with air nobody asked for. The height is arithmetic
        // (header plus rows), because a view cannot measure itself during
        // `render`; and when they do not fit, each shrinks from that basis
        // to a floor of two rows and its `uniform_list` scrolls. No
        // measurement, no second frame.
        //
        // The commit list is the stack's one *flexible* section: it takes
        // whatever the short panes leave, the way lazygit's log does — it is
        // the reason one opens the window, and the one list whose height
        // nobody would want fixed. It is also a registry slot: commits by
        // default, an extension pane standing in when one is focused — the
        // one place a compiled-in tenant takes a region of its own, exactly
        // as it did when it had a column to itself.
        let sidebar = {
            // Owned, not borrowed: the loops below call back into `self` for
            // the commit section, and a borrow of the panes would stand
            // across it.
            let focused_name = self.panes.focused_name().to_string();
            let mut sections: Vec<AnyElement> = Vec::new();
            for (name, number, label, id) in STACK_TOP {
                let Some(screen) = self.panes.get(name) else {
                    continue;
                };
                let focused = self.spot == Spot::List && focused_name == name;
                // The count is the header's only right-edge furniture: the
                // working tree's distinct changed paths, spelled once per
                // refresh and read here for free.
                let count = match screen {
                    Screen::Files { view, .. } => {
                        Some(SharedString::from(view.read(cx).changed().to_string()))
                    }
                    _ => None,
                };
                let rows = match screen {
                    Screen::Files { view, .. } => view.read(cx).rows(),
                    Screen::Branches { view, .. } => view.read(cx).rows(),
                    Screen::Stashes { view, .. } => view.read(cx).rows(),
                    _ => 0,
                };
                sections.push(
                    div()
                        .id(id)
                        .debug_selector(move || id.to_string())
                        .flex_shrink(1.0)
                        .h(px(section_height(rows)))
                        // The floor never exceeds the basis: a minimum
                        // above the natural height wins the layout and an
                        // empty section would be padded to two rows.
                        .min_h(px(section_floor(rows)))
                        .flex()
                        .flex_col()
                        .overflow_hidden()
                        .capture_any_mouse_down(cx.listener(move |this, _, _, cx| {
                            this.focus_named(name, cx);
                        }))
                        .child(chrome::pane_header(
                            &host,
                            number,
                            label.into(),
                            count,
                            focused,
                            None,
                        ))
                        .child(
                            div()
                                .min_h_0()
                                .flex_grow(1.0)
                                .overflow_hidden()
                                .child(screen.any()),
                        )
                        .into_any_element(),
                );
            }
            // The flexible middle, then the content-sized foot — lazygit's
            // order: the stash under the commits, where parking ends a
            // session's work.
            self.commits_section(&host, &focused_name, cx, &mut sections);
            for (name, number, label, id) in STACK_FOOT {
                let Some(screen) = self.panes.get(name) else {
                    continue;
                };
                let focused = self.spot == Spot::List && focused_name == name;
                let count: Option<SharedString> = None;
                let rows = match screen {
                    Screen::Stashes { view, .. } => view.read(cx).rows(),
                    _ => 0,
                };
                sections.push(
                    div()
                        .id(id)
                        .debug_selector(move || id.to_string())
                        .flex_shrink(1.0)
                        .h(px(section_height(rows)))
                        .min_h(px(section_floor(rows)))
                        .flex()
                        .flex_col()
                        .overflow_hidden()
                        .capture_any_mouse_down(cx.listener(move |this, _, _, cx| {
                            this.focus_named(name, cx);
                        }))
                        .child(chrome::pane_header(
                            &host,
                            number,
                            label.into(),
                            count,
                            focused,
                            None,
                        ))
                        .child(
                            div()
                                .min_h_0()
                                .flex_grow(1.0)
                                .overflow_hidden()
                                .child(screen.any()),
                        )
                        .into_any_element(),
                );
            }
            (!sections.is_empty()).then(|| {
                div()
                    .id("sidebar")
                    .flex_none()
                    .w(relative(chrome::SIDEBAR_SHARE))
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .debug_selector(|| "sidebar".to_string())
                    .children(sections)
            })
        };
        let Screen::Diff {
            view: main_view, ..
        } = &self.main
        else {
            unreachable!("the main view is always a diff");
        };
        let head = self.head.borrow().clone();
        // The band's "loading diff" is the one home for the word: an accent
        // here competed with it and said the same thing twice at once.
        // The header names the file the keyboard is in, with that file's
        // change counts and the hunk's place among its siblings — the same
        // three facts the design's fifth pane carries. The commit's subject
        // rides along dim and shrinking: a revspec launch (`HEAD~2..HEAD`)
        // has no row in any list to name it, and a diff that cannot say what
        // it is of is a diff nobody trusts after a scroll.
        let main_focused = self.spot == Spot::Main;
        let main_region = div()
            .id("main")
            .flex_grow(1.0)
            .min_w_0()
            .relative()
            .min_h_0()
            .flex()
            .flex_col()
            .overflow_hidden()
            .border_l_1()
            .border_color(rgb(c.border))
            .debug_selector(|| "main".to_string())
            .capture_any_mouse_down(cx.listener(|this, _, _, cx| this.set_spot(Spot::Main, cx)))
            .child({
                let summary = main_view.read(cx).file_summary();
                // Spelled once per change of summary, not per frame: the
                // memo answers while the keyboard sits still, which is the
                // frame that happens most.
                let text = summary.as_ref().map(|s| {
                    let mut memo = self.header_memo.borrow_mut();
                    match memo.as_ref() {
                        Some((key, text)) if key == s => text.clone(),
                        _ => {
                            let text = HeaderText::of(s);
                            *memo = Some((s.clone(), text.clone()));
                            text
                        }
                    }
                });
                let (adds, dels, hunk) = match &text {
                    Some(t) => (Some(t.adds.clone()), Some(t.dels.clone()), t.hunk.clone()),
                    None => (None, None, None),
                };
                // File path, then the counts, then the subject last and
                // shrinking: the path is the one thing that must not
                // truncate, and the eye finds counts at the right edge.
                let right = div()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_3()
                    .children(head.map(|commit| {
                        div()
                            .flex_shrink(1.0)
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis_start()
                            // Read when it is there — the commit the list is
                            // sitting on — so it clears the furniture floor.
                            .text_color(rgb(host.theme.quiet_on(c.title_bg)))
                            .child(commit.subject.clone())
                    }))
                    .children(adds.map(|adds| {
                        div()
                            .flex_none()
                            .text_color(rgb(host.theme.diff.adds_fg))
                            .child(adds)
                    }))
                    .children(dels.map(|dels| {
                        div()
                            .flex_none()
                            .text_color(rgb(host.theme.diff.dels_fg))
                            .child(dels)
                    }))
                    .children(hunk.map(|h| {
                        div()
                            .flex_none()
                            // A count is read, so through `quiet_on` — raw
                            // `faint` is under the floor on this strip.
                            .text_color(rgb(host.theme.quiet_on(c.title_bg)))
                            .child(h)
                    }));
                // The name is a path, so it is drawn as one: directory dim,
                // filename in the header's own ink — the same cut the files
                // rows make, so the eye lands on the same word in both.
                let name_ink = match main_focused {
                    true => c.fg,
                    false => c.dim,
                };
                let name = match &text {
                    Some(t) => chrome::path_spans(
                        &host,
                        t.dir.clone(),
                        t.name.clone(),
                        name_ink,
                        theme::Surface::Title,
                    ),
                    None => div().text_color(rgb(name_ink)).child("DIFF"),
                };
                chrome::pane_header_with(
                    &host,
                    "6",
                    name.into_any_element(),
                    None,
                    main_focused,
                    Some(right.into_any_element()),
                )
            })
            // A flexed box for the view itself: every view roots at
            // `size_full`, which under this region would read the *container*
            // height and slide the last rows behind the header without it.
            .child(
                div()
                    .min_h_0()
                    .flex_grow(1.0)
                    .overflow_hidden()
                    .child(main_view.clone()),
            );

        let which = self.active_view_name();
        let strip = self.strip(&host, cx);
        let error = self.error.as_ref().map(|e| e.summary.clone());
        let notice = self.notice.clone();
        let running = self.running.as_ref().map(|(label, at)| {
            // Whole seconds, and only once there is one to say: a job that
            // answers inside its first second reads as if it never ran.
            match at.elapsed().as_secs() {
                0 => SharedString::from(label.as_str()),
                s => SharedString::from(format!("{label} · {s}s")),
            }
        });
        let running = running
            .or_else(|| (self.refresh_pending > 0).then(|| "refreshing repository".into()))
            // The main view's own load, which does not ride the job queue:
            // said here rather than invented for it, because the band is the
            // one place a background something is spoken of.
            .or_else(|| self.loading.get().then(|| "loading diff".into()));
        let input = self.input.clone();

        // The title is the repository and where HEAD is, and nothing else.
        // The app's name is the icon's job, the view's name is the status
        // badge's and the version is the bar's; a strip that said all three
        // again was chrome reading its own labels aloud. The path is drawn
        // the way every path here is — parent dim, the name bright — and a
        // launch with no repository behind it (a fixture, a patch) keeps the
        // acquisition label, which is the only name it has.
        let title: AnyElement = match &self.repo {
            Some((path, _)) => {
                // Cut once per repository — see [`DevShell::title_memo`].
                let mut memo = self.title_memo.borrow_mut();
                let (dir, name) = match memo.as_ref() {
                    Some((at, dir, name)) if at == path => (dir.clone(), name.clone()),
                    _ => {
                        let (dir, name) = repo_title(path, home());
                        let (dir, name) = (SharedString::from(dir), SharedString::from(name));
                        *memo = Some((path.clone(), dir.clone(), name.clone()));
                        (dir, name)
                    }
                };
                chrome::path_spans(&host, dir, name, c.fg, theme::Surface::Title).into_any_element()
            }
            None => div()
                .whitespace_nowrap()
                // From the *start*: the label is a path and a revspec, and
                // `…/git HEAD~2..HEAD` is the half worth keeping.
                .text_ellipsis_start()
                .child(self.active_label(cx))
                .into_any_element(),
        };

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
                    // The strip names the repository and is read; raw dim is
                    // under the text floor here (3.37), so it resolves against
                    // the strip it is drawn on.
                    .text_color(rgb(host.theme.dim_on(theme::Surface::Title)))
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
                            .child(title),
                    )
                    // The branch chip, the design's `⎇ main · ↑2 ↓0`: where
                    // HEAD sits and how far it has drifted, read from the
                    // branches pane's own prepared head — one small struct
                    // per frame, no second git call anywhere. Outlined and
                    // not filled: the one filled chip is the status badge,
                    // and two would compete. A detached HEAD or a fixture
                    // draws nothing: absence is the honest state.
                    .children(self.panes.get("branches").and_then(|screen| {
                        let Screen::Branches { view, .. } = screen else {
                            return None;
                        };
                        let info = view.read(cx).head_info()?;
                        Some(
                            div()
                                .flex_none()
                                .flex()
                                .items_center()
                                .h(px(CHIP_H))
                                .px_2()
                                .border_1()
                                .border_color(rgb(c.border))
                                .rounded(px(chrome::RADIUS))
                                .whitespace_nowrap()
                                // Both halves were spelled at prepare; a
                                // frame clones two refcounts.
                                .child(div().flex_none().text_color(rgb(c.fg)).child(info.chip))
                                .children(info.drift.map(|drift| {
                                    // A drift figure is read, not glanced at —
                                    // and raw dim is under the floor on the
                                    // strip it sits on.
                                    div()
                                        .flex_none()
                                        .text_color(rgb(host.theme.dim_on(theme::Surface::Title)))
                                        .child(drift)
                                })),
                        )
                    }))
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
            // The two regions in one row: the left stack, the diff. A fixture
            // has no stack — no repository to list — and the diff fills the
            // window; a repository has both.
            .child(
                div()
                    .min_h_0()
                    .flex_grow(1.0)
                    .flex()
                    .children(sidebar)
                    .child(main_region),
            )
            .children(input)
            // The menu itself is deferred at priority 1. Its transparent
            // priority-0 backdrop blocks the rest of the window without
            // covering the menu, so capture can leave overlay wheel ownership
            // alone without exposing the native list scroller underneath.
            .children(self.open.is_some().then(controls::picker_backdrop))
            // The status bar: where the keyboard is, and what the nearest
            // keys do. A sentence owed to the user — an error, a job's own
            // finish, an armed question — takes the hints' place rather than
            // a band of its own: it is the one strip already being read, and
            // an armed question competing with key hints for a row is two
            // things saying "look at me" where one will do. An error wins
            // over a notice: it describes what failed, the notice describes
            // what was tried since. A prompt empties the hints honestly —
            // its field owns the keyboard and speaks for itself.
            .child({
                let message = error
                    .map(|e| (e, c.error))
                    // A question takes the error's ink and not this: quiet is
                    // what hid the arm, and the one sentence a second press
                    // spends is the one being read.
                    .or_else(|| {
                        notice.as_ref().map(|n| match n {
                            Notice::Info(text) => (
                                text.as_str().into(),
                                host.theme.dim_on(theme::Surface::Status),
                            ),
                            Notice::Question(text) => (text.as_str().into(), c.error),
                        })
                    })
                    .or_else(|| {
                        running.map(|n| (n.into(), host.theme.dim_on(theme::Surface::Status)))
                    });
                let badge: SharedString = match self.input.is_some() {
                    true => "PROMPT".into(),
                    false => which.to_uppercase().into(),
                };
                let (hints, truncated) = match (&message, self.input.is_some()) {
                    (Some(_), _) | (None, true) => (Vec::new(), false),
                    (None, false) => {
                        let width = f32::from(window.viewport_size().width);
                        chrome::hints(
                            &host,
                            &self.modes,
                            which,
                            chrome::hints_budget(&host, width, &badge),
                        )
                    }
                };
                // An error says how to leave, where it stands: `esc` dismisses,
                // the message key opens the full text. Live keys only — a hint
                // naming a dead key is the one lie a panel of keys must never
                // tell.
                let exits = self
                    .error
                    .as_ref()
                    .and_then(|_| {
                        host.keys
                            .live_keys_for("message.show", &self.modes)
                            .into_iter()
                            .next()
                    })
                    .map(|key| SharedString::from(format!("· esc dismiss · {key} full text")));
                match message {
                    Some((text, ink)) => div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap_3()
                        .h(px(chrome::STATUS_H))
                        .px_2()
                        .bg(rgb(c.status_bg))
                        .border_t_1()
                        .border_color(rgb(c.border))
                        .text_color(rgb(host.theme.dim_on(theme::Surface::Status)))
                        .child(div().min_w_0().truncate().text_color(rgb(ink)).child(text))
                        // An error says how to leave, in the faint ink of
                        // furniture: the summary is the sentence, this is the
                        // small print. No live key, no piece — the help
                        // overlay's rule.
                        .children(
                            exits.map(|e| div().flex_none().text_color(rgb(c.faint)).child(e)),
                        )
                        .into_any_element(),
                    None => chrome::status_bar(&host, badge, &hints, truncated, chrome::version())
                        .into_any_element(),
                }
            })
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
                    .text_color(rgb(host.theme.dim_on(theme::Surface::Status)))
                    .child(
                        div()
                            .flex()
                            .gap_6()
                            .child(div().text_color(rgb(c.accent)).child(frames))
                            .child(rows)
                            .child(heap),
                    )
                    // Read — it is the load number — so through `quiet_on`.
                    .child(
                        div()
                            .text_color(rgb(host.theme.quiet_on(c.status_bg)))
                            .child(load),
                    )
            }))
            // The help overlay, last so it paints over everything: deferred, so
            // it escapes the regions' paint order; occluding, so the rows under
            // it get neither the clicks nor the wheel. Its rows come from the
            // same projection the terminal draws, which is why neither client
            // can drift from the other.
            .children(
                self.help
                    .then(|| help::overlay(&config::host(cx), &self.modes, &self.help_scroll)),
            )
            // The message overlay, over even the help: it exists because the
            // band's one truncated line was not the whole of git's answer, so
            // the whole of the answer is the one thing it must show.
            .children(
                self.show_message
                    .then_some(self.error.as_ref())
                    .flatten()
                    .map(|error| message_overlay(error, &host)),
            );
        root
    }
}

/// The error's whole answer, word-wrapped, over everything. The heading is
/// git's own first line in the error's ink; the body is everything git said,
/// argv prefix included — the band's one truncated line is the glance, this is
/// the reading. No `whitespace_nowrap`: a long answer wraps, because a panel
/// that clips its tail is the band with more room.
fn message_overlay(error: &GitError, host: &Host) -> AnyElement {
    let c = &host.theme.chrome;
    div()
        .occlude()
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .max_w(px(720.0))
                .max_h_full()
                .overflow_hidden()
                .bg(rgb(c.title_bg))
                .border_1()
                .border_color(rgb(c.faint))
                .rounded(px(4.))
                .p(px(16.0))
                .text_size(px(host.font.size))
                .font_family(host.font.family.clone())
                .text_color(rgb(c.dim))
                // The glance, then the record. No `whitespace_nowrap`: a long
                // answer wraps, because a panel that clips its tail is the
                // band with more room.
                .child(div().text_color(rgb(c.error)).child(error.summary.clone()))
                .child(error.full.clone()),
        )
        .into_any_element()
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
            // Canonicalised once, here: `.` is what every launch is handed by
            // default and has no name to put in a title, and a syscall on the
            // render path is not the place to find one.
            let path = path.canonicalize().unwrap_or_else(|_| path.clone());
            (Some(rediff), Some((path, repo)))
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
                                Some(source.clone()),
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
                let has_column = matches!(screen, Screen::Commits { .. });
                // The diff main view. A launch that opened on a *list* starts
                // it empty — its rows arrive with the first selection's
                // scheduled load, and the header names the commit from frame
                // one. A launch that opened on a diff (`gitten diff …`, a
                // fixture, a patch) *is* this screen: same rows, no commit
                // list.
                let main_screen = match &screen {
                    Screen::Commits { .. } => {
                        let e = cx.new(|cx| views::diff::Diff::new(Vec::new(), host.clone(), cx));
                        Screen::diff(e, None, Generation::default(), "")
                    }
                    other => other.clone(),
                };
                let mut initial_panes = panes::Panes::new(which_name, screen);

                // The working tree gets its compact pane, above wherever a
                // diff later opens. One blocking `git status` here, beside the
                // rest of startup acquisition; from the next write on, the
                // generation-guarded refresh path keeps it current. A fixture
                // has no repository and so no pane at all.
                if let Some((path, handle)) = &repo {
                    start::mark("files status begin");
                    // The repository's own name, cut the way `describe` cuts
                    // it — canonicalised first, so `.` still has one. The
                    // status pane's bright half; solved once here rather
                    // than re-canonicalising per frame.
                    let named = path.canonicalize().unwrap_or_else(|_| path.clone());
                    let repo_name = named
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let described = std::thread::scope(|s| {
                        // Beside, not behind — describe, status and the stash
                        // stack spawn together and are joined only once all
                        // three are back. Joining each in sequence would put
                        // three git processes on the launch path one after
                        // another.
                        let title = s.spawn(|| handle.describe());
                        let status = s.spawn(|| handle.status());
                        let parked = s.spawn(|| handle.stashes());
                        let title = title.join().unwrap_or_default();
                        let files_prepared = match status
                            .join()
                            .unwrap_or_else(|p| std::panic::resume_unwind(p))
                        {
                            Ok(status) => views::files::prepare(status, &title),
                            // Shown as a clean tree rather than failing the
                            // window: one bad status must not take the launch.
                            Err(e) => {
                                eprintln!("gitten: status failed, showing an empty pane: {e}");
                                views::files::prepare(Default::default(), &title)
                            }
                        };
                        // The same trade for the stack: a failed read is an
                        // empty pane and a line on stderr, not a lost launch.
                        let stashes_prepared = match parked
                            .join()
                            .unwrap_or_else(|p| std::panic::resume_unwind(p))
                        {
                            Ok(stashes) => views::stashes::prepare(&stashes, &title),
                            Err(e) => {
                                eprintln!("gitten: stashes failed, showing an empty pane: {e}");
                                views::stashes::prepare(&[], &title)
                            }
                        };
                        (files_prepared, stashes_prepared)
                    });
                    start::mark("files status done");
                    let (files_prepared, stashes_prepared) = described;
                    // Registration order is the *startup* order — commits is
                    // the root tenant, then the three sidebar panes join it.
                    // The number keys and ctrl-j walk the design's order
                    // (files → branches → stashes → commits), derived in
                    // [`DevShell::list_order`], so this list's order is only
                    // about who was here first. Registration focuses what it
                    // adds; the `focus(0)` calls put the keyboard back where
                    // it launched.
                    let files_label = files_prepared.label.clone();
                    initial_panes.register(
                        "files",
                        Screen::files(
                            cx.new(|_| views::files::Files::from_prepared(files_prepared)),
                            Generation::default(),
                            files_label,
                        ),
                    );
                    initial_panes.focus(0);
                    start::mark("files pane built");

                    // The branches panel beside it — three reads run side by
                    // side, behind the same spawn floor the files pane pays.
                    // A failed read shows an empty panel rather than failing
                    // the launch, for the same reason a bad status does.
                    start::mark("branches read begin");
                    let described = handle.describe();
                    let prepared = std::thread::scope(|s| {
                        let local = s.spawn(|| handle.branches());
                        let remote = s.spawn(|| handle.remote_branches());
                        let head = s.spawn(|| handle.head());
                        let local = local
                            .join()
                            .unwrap_or_else(|p| std::panic::resume_unwind(p));
                        let remote = remote
                            .join()
                            .unwrap_or_else(|p| std::panic::resume_unwind(p));
                        let head = head.join().unwrap_or_else(|p| std::panic::resume_unwind(p));
                        match (local, remote) {
                            (Ok(local), Ok(remote)) => {
                                let head = match head {
                                    Ok(head) => Some(head),
                                    Err(e) => {
                                        eprintln!(
                                            "gitten: head read failed, showing attached: {e}"
                                        );
                                        None
                                    }
                                };
                                views::branches::prepare(
                                    local,
                                    remote,
                                    head,
                                    &host.theme,
                                    &described,
                                )
                            }
                            (Err(e), _) | (_, Err(e)) => {
                                eprintln!("gitten: branch reads failed, empty panel: {e}");
                                views::branches::prepare(
                                    Vec::new(),
                                    Vec::new(),
                                    None,
                                    &host.theme,
                                    &described,
                                )
                            }
                        }
                    });
                    start::mark("branches read done");
                    let label = prepared.label.clone();
                    let branches = cx.new(|_| views::branches::Branches::from_prepared(prepared));
                    initial_panes.register(
                        "branches",
                        Screen::branches(branches.clone(), Generation::default(), label),
                    );
                    initial_panes.focus(0);
                    start::mark("branches pane built");

                    // The status line, after branches: it reads who HEAD is
                    // from the branches pane's model — one `head` read in the
                    // window, however many panes say it — so it is built once
                    // that model exists, and never refreshes anything of its
                    // own.
                    initial_panes.register(
                        "status",
                        Screen::status(
                            cx.new(|_| views::status::Status::new(repo_name, Some(branches))),
                            described.clone(),
                        ),
                    );
                    initial_panes.focus(0);
                    start::mark("status pane built");

                    // The stack, last in the cycle like its key is last on the
                    // number row.
                    let stashes_label = stashes_prepared.label.clone();
                    initial_panes.register(
                        "stashes",
                        Screen::stashes(
                            cx.new(|_| views::stashes::Stashes::from_prepared(stashes_prepared)),
                            Generation::default(),
                            stashes_label,
                        ),
                    );
                    initial_panes.focus(0);
                    start::mark("stashes pane built");
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
                    main: main_screen,
                    has_column,
                    spot: match has_column {
                        true => Spot::List,
                        false => Spot::Main,
                    },
                    head: RefCell::new(None),
                    request: Cell::new(0),
                    loading: Cell::new(false),
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
                    show_message: false,
                    input: None,
                    prompt: None,
                    search_live: None,
                    over: Overrides::default(),
                    open: None,
                    error: None,
                    notice: None,
                    config: shell_config_path,
                    first_render: Cell::new(false),
                    title_memo: RefCell::new(None),
                    header_memo: RefCell::new(None),
                    modes: Modes::new(),
                    pending: Vec::new(),
                    help: false,
                    help_scroll: ScrollHandle::default(),
                    focus,
                    focused: None,
                    seen_host: None,
                    ongoing: Cell::default(),
                });
                {
                    let shell = shell.clone();
                    shell.update(cx, |shell, cx| {
                        shell.sync_modes(cx);
                        shell.sync_focus(cx);
                        // Frame one already names its commit: schedule the
                        // newest one's diff through the same guarded rails
                        // every later selection rides. The header and the
                        // band are up before the first paint; the rows land
                        // one debounce later.
                        shell.sync_main_diff(cx);
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
    use super::{
        config, input, panes, DevShell, GitError, Notice, Open, Pane, Refresh, Screen, Writes,
    };
    use crate::views::commits::Commits;
    use gitten_app::cli::Source;
    use gitten_app::jobs::{Event as JobEvent, Generation, Job, Runner, Submitter};
    use gitten_core::command::{Code, Key, Keymap, Modes, Resolve};
    use gitten_core::host::Host;
    use gitten_core::status::Status;
    use gitten_core::Commit;
    use gitten_git::{Pair, Repo};
    use gpui::ScrollHandle;
    use gpui::{AppContext as _, TestAppContext};
    use std::cell::{Cell, RefCell};
    use std::path::PathBuf;
    use std::rc::Rc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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
                    generation,
                    outcome: Ok(()),
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
            // The live host, so anything a test dispatches through
            // `config::host` finds one — tests that care replace it.
            cx.set_global(config::Active(Rc::new(Host::new())));
            let host = config::host(cx);
            let commits = cx.new(|_| Commits::new(Vec::new(), host.clone()));
            let diff = cx.new(|cx| crate::views::diff::Diff::new(Vec::new(), host.clone(), cx));
            let jobs = Runner::new();
            DevShell {
                which: "commits",
                panes: panes::Panes::new(
                    "commits",
                    Screen::commits(commits, Source::Fixtures, Generation::default(), "repo"),
                ),
                main: Screen::diff(diff, None, Generation::default(), ""),
                has_column: true,
                spot: super::Spot::List,
                head: RefCell::new(None),
                request: Cell::new(0),
                loading: Cell::new(false),
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
                show_message: false,
                input: None,
                prompt: None,
                search_live: None,
                over: Default::default(),
                open: which,
                error: None,
                notice: None,
                config: std::path::PathBuf::new(),
                first_render: Cell::new(false),
                title_memo: RefCell::new(None),
                header_memo: RefCell::new(None),
                modes: Modes::new(),
                pending: vec![vec![Key::char('g')]],
                help: false,
                help_scroll: ScrollHandle::default(),
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
    fn the_lists_learn_focus_when_it_moves_and_not_in_render(cx: &mut TestAppContext) {
        // A row's bar is accent only in the pane holding the keyboard; the
        // flag that says so is written where focus moves, so a test can read
        // it without a frame ever being drawn.
        let shell = shell(None, cx);
        let second = cx.update(|cx| cx.new(|_| Commits::new(Vec::new(), Rc::new(Host::new()))));
        shell.update(cx, |s, cx| {
            s.panes.register(
                "second",
                Screen::commits(
                    second.clone(),
                    Source::Fixtures,
                    Generation::default(),
                    "second",
                ),
            );
            s.sync_modes(cx);
            s.sync_focus(cx);
        });
        let first = shell.read_with(cx, |s, _| match s.panes.get("commits") {
            Some(Screen::Commits { view, .. }) => view.clone(),
            _ => panic!("no commits list"),
        });
        // Registration focuses the newcomer.
        assert!(!first.read_with(cx, |v, _| v.focused()));
        assert!(second.read_with(cx, |v, _| v.focused()));

        shell.update(cx, |s, cx| s.focus_named("commits", cx));
        assert!(first.read_with(cx, |v, _| v.focused()));
        assert!(!second.read_with(cx, |v, _| v.focused()));
        shell.update(cx, |s, cx| s.focus_named("second", cx));
        assert!(!first.read_with(cx, |v, _| v.focused()));
        assert!(second.read_with(cx, |v, _| v.focused()));

        // The diff holds the keyboard: no list is focused, and the memory of
        // which one was comes back with `esc`.
        shell.update(cx, |s, cx| s.set_spot(super::Spot::Main, cx));
        assert!(!second.read_with(cx, |v, _| v.focused()));
        shell.update(cx, |s, cx| s.back(cx));
        assert!(second.read_with(cx, |v, _| v.focused()));
    }

    #[gpui::test]
    fn esc_hands_the_keyboard_back_from_the_diff_and_never_closes_a_list(cx: &mut TestAppContext) {
        let shell = shell(None, cx);
        // From the column, esc with nothing standing does nothing at all —
        // and closes nothing, because the lists are the column's residents.
        shell.update(cx, |s, cx| s.back(cx));
        shell.read_with(cx, |s, _| {
            assert_eq!(s.panes.len(), 1);
            assert_eq!(s.spot, super::Spot::List);
        });

        // Enter hands the keyboard to the diff region; `esc` brings it back.
        shell.update(cx, |s, cx| s.run_command("commits.open-diff", cx));
        shell.read_with(cx, |s, _| {
            assert_eq!(s.spot, super::Spot::Main);
            assert_eq!(s.modes.top(), "diff", "the diff owns the keys");
        });
        shell.update(cx, |s, cx| s.back(cx));
        shell.read_with(cx, |s, _| assert_eq!(s.spot, super::Spot::List));

        // A second registered list survives every esc.
        shell.update(cx, |s, cx| {
            let commits = cx.new(|_| Commits::new(Vec::new(), Rc::new(Host::new())));
            s.panes.register(
                "second",
                Screen::commits(commits, Source::Fixtures, Generation::default(), "second"),
            );
            s.sync_modes(cx);
        });
        for _ in 0..2 {
            shell.update(cx, |s, cx| s.back(cx));
        }
        shell.read_with(cx, |s, _| {
            assert_eq!(s.panes.len(), 2, "esc closed a list");
            assert_eq!(s.open, None);
        });
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
            shell.sync_modes(cx);
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
    fn the_pane_moves_walk_every_pane_in_reading_order(cx: &mut TestAppContext) {
        let shell = shell(None, cx);
        // The whole stack, so the walk has all six stops: five lists and
        // the diff.
        shell.update(cx, |shell, cx| {
            let host = config::host(cx);
            let branches = cx.new(|_| {
                crate::views::branches::Branches::from_prepared(crate::views::branches::prepare(
                    Vec::new(),
                    Vec::new(),
                    None,
                    &host.theme,
                    "t",
                ))
            });
            shell.panes.register(
                "status",
                Screen::status(
                    cx.new(|_| crate::views::status::Status::new("t", Some(branches.clone()))),
                    "t",
                ),
            );
            shell.panes.register(
                "files",
                Screen::files(
                    cx.new(|_| {
                        crate::views::files::Files::from_prepared(crate::views::files::prepare(
                            Default::default(),
                            "t",
                        ))
                    }),
                    Generation::default(),
                    "files",
                ),
            );
            shell.panes.register(
                "branches",
                Screen::branches(branches, Generation::default(), "branches"),
            );
            shell.panes.register(
                "stashes",
                Screen::stashes(
                    cx.new(|_| {
                        crate::views::stashes::Stashes::from_prepared(
                            crate::views::stashes::prepare(&[], "t"),
                        )
                    }),
                    Generation::default(),
                    "stashes",
                ),
            );
            shell.run_command("status.focus", cx);
        });

        // Right from the top walks down the stack and lands on the diff:
        // status → files → branches → commits → stashes → diff.
        for expected in ["files", "branches", "commits", "stashes"] {
            shell.update(cx, |shell, cx| shell.run_command("pane.right", cx));
            shell.read_with(cx, |shell, _| {
                assert_eq!(
                    shell.panes.focused_name(),
                    expected,
                    "walking right from the top"
                );
                assert_eq!(shell.spot, super::Spot::List);
            });
        }
        shell.update(cx, |shell, cx| shell.run_command("pane.right", cx));
        shell.read_with(cx, |shell, _| {
            assert_eq!(
                shell.spot,
                super::Spot::Main,
                "right of the foot is the diff"
            );
        });
        // And the edge is an edge: right on the diff answers, moves nothing.
        shell.update(cx, |shell, cx| shell.run_command("pane.right", cx));
        shell.read_with(cx, |shell, _| assert_eq!(shell.spot, super::Spot::Main));

        // Left from the diff walks back up the stack, and the top is the
        // other edge.
        for expected in ["stashes", "commits", "branches", "files", "status"] {
            shell.update(cx, |shell, cx| shell.run_command("pane.left", cx));
            shell.read_with(cx, |shell, _| {
                assert_eq!(
                    shell.panes.focused_name(),
                    expected,
                    "walking left from the diff"
                );
                assert_eq!(shell.spot, super::Spot::List);
            });
        }
        shell.update(cx, |shell, cx| shell.run_command("pane.left", cx));
        shell.read_with(cx, |shell, _| {
            assert_eq!(
                shell.panes.focused_name(),
                "status",
                "the top is the left edge"
            );
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
            shell.running = Some(("running next write".into(), Instant::now()));
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
            assert_eq!(
                shell.running.as_ref().map(|(label, _)| label.as_str()),
                Some("running next write")
            );
        });
        assert_eq!(calls.load(Ordering::Relaxed), 2);
    }

    #[gpui::test]
    fn the_stack_and_the_main_view_sit_side_by_side(cx: &mut TestAppContext) {
        let shell = shell(None, cx);
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
        let stack = cx.debug_bounds("sidebar").expect("the stack was not drawn");
        let main = cx
            .debug_bounds("main")
            .expect("the main view was not drawn");

        // Side by side, both full height, the stack in its slice of the
        // width — 0.32 against the main view's 0.68.
        assert!(stack.size.height > gpui::px(0.0));
        assert!(main.size.height > gpui::px(0.0));
        assert_eq!(stack.origin.y, main.origin.y);
        let width = f32::from(stack.size.width) + f32::from(main.size.width);
        let share = f32::from(stack.size.width) / width;
        assert!(
            (share - super::chrome::SIDEBAR_SHARE).abs() < 0.01,
            "the stack took {share} of the width"
        );
        assert_eq!(stack.right(), main.origin.x);

        // A click moves the keyboard between exactly the two regions. The
        // commit section answers for the whole stack: focusing it puts the
        // keyboard back in the list region.
        cx.simulate_click(main.center(), gpui::Modifiers::default());
        assert_eq!(
            observed.read_with(&cx, |shell, _| shell.spot),
            super::Spot::Main
        );
        let commits_section = cx
            .debug_bounds("side-commits")
            .expect("the commit section was not drawn");
        cx.simulate_click(commits_section.center(), gpui::Modifiers::default());
        assert_eq!(
            observed.read_with(&cx, |shell, _| shell.spot),
            super::Spot::List
        );

        // And ctrl-j cycles the stack's lists without leaving the stack.
        // Registration focuses what it adds, so the keyboard goes back to the
        // root first — where ctrl-j finds it.
        observed.update(&mut cx, |shell, cx| {
            let commits = cx.new(|_| Commits::new(Vec::new(), Rc::new(Host::new())));
            shell.panes.register(
                "second",
                Screen::commits(commits, Source::Fixtures, Generation::default(), "second"),
            );
            shell.panes.focus(0);
            shell.sync_modes(cx);
        });
        cx.simulate_keystrokes("ctrl-j");
        assert_eq!(
            observed.read_with(&cx, |shell, _| shell.panes.focused_name().to_string()),
            "second"
        );
        assert_eq!(
            observed.read_with(&cx, |shell, _| shell.spot),
            super::Spot::List
        );
    }

    /// The design's whole arrangement: stack, diff — two regions side by
    /// side, the stack's five sections stacked inside the first in lazygit's
    /// order — status, files, branches, then the flexible commit list, then
    /// the stash at the foot. Drawn from the same geometry the click
    /// hit-tests read, which is what makes it a test of the real layout and
    /// not of a copy of it.
    #[gpui::test]
    fn the_window_is_two_regions_stack_and_diff(cx: &mut TestAppContext) {
        let shell = shell(None, cx);
        shell.update(cx, |shell, cx| {
            let host = config::host(cx);
            let branches = cx.new(|_| {
                crate::views::branches::Branches::from_prepared(crate::views::branches::prepare(
                    Vec::new(),
                    Vec::new(),
                    None,
                    &host.theme,
                    "t",
                ))
            });
            shell.panes.register(
                "status",
                Screen::status(
                    cx.new(|_| crate::views::status::Status::new("t", Some(branches.clone()))),
                    "t",
                ),
            );
            shell.panes.register(
                "files",
                Screen::files(
                    cx.new(|_| {
                        crate::views::files::Files::from_prepared(crate::views::files::prepare(
                            Default::default(),
                            "t",
                        ))
                    }),
                    Generation::default(),
                    "files",
                ),
            );
            shell.panes.register(
                "branches",
                Screen::branches(branches, Generation::default(), "branches"),
            );
            shell.panes.register(
                "stashes",
                Screen::stashes(
                    cx.new(|_| {
                        crate::views::stashes::Stashes::from_prepared(
                            crate::views::stashes::prepare(&[], "t"),
                        )
                    }),
                    Generation::default(),
                    "stashes",
                ),
            );
            shell.panes.focus(0);
            shell.sync_modes(cx);
        });
        let observed = shell.clone();
        let handle = cx.update(|cx| {
            gpui_component::init(cx);
            cx.set_global(config::Active(Rc::new(Host::new())));
            cx.open_window(
                gpui::WindowOptions {
                    window_bounds: Some(gpui::WindowBounds::Windowed(gpui::Bounds {
                        origin: Default::default(),
                        size: gpui::size(gpui::px(1200.0), gpui::px(600.0)),
                    })),
                    ..Default::default()
                },
                move |_, _| shell,
            )
            .unwrap()
        });
        let mut cx = gpui::VisualTestContext::from_window(handle.into(), cx);
        cx.run_until_parked();
        let sidebar = cx
            .debug_bounds("sidebar")
            .expect("the sidebar was not drawn");
        let main = cx
            .debug_bounds("main")
            .expect("the main view was not drawn");

        // Left to right, no gaps, both the same height: one window row, two
        // regions.
        assert_eq!(sidebar.right(), main.origin.x, "sidebar then diff");
        assert_eq!(sidebar.origin.y, main.origin.y);
        assert_eq!(sidebar.size.height, main.size.height);

        // The short sections are as tall as their content — all empty here,
        // so a header and the one row the empty-state line takes — and each
        // sits directly under the one before, from the top of the stack.
        let status = cx.debug_bounds("side-status").expect("no status section");
        let files = cx.debug_bounds("side-files").expect("no files section");
        let branches = cx
            .debug_bounds("side-branches")
            .expect("no branches section");
        let stashes = cx.debug_bounds("side-stashes").expect("no stashes section");
        let natural = gpui::px(super::section_height(0));
        assert_eq!(status.size.height, natural, "an empty section is one row");
        assert_eq!(files.size.height, natural);
        assert_eq!(branches.size.height, natural);
        assert_eq!(stashes.size.height, natural);
        assert_eq!(status.origin.y, sidebar.origin.y);
        assert_eq!(status.bottom(), files.origin.y, "status then files");
        assert_eq!(files.bottom(), branches.origin.y, "files then branches");

        // The commit list is the stack's flexible middle: under the top
        // sections, above the stash foot, taking the space between.
        let commits = cx.debug_bounds("side-commits").expect("no commits section");
        assert_eq!(branches.bottom(), commits.origin.y, "branches then commits");
        assert_eq!(commits.bottom(), stashes.origin.y, "commits then stashes");
        assert_eq!(
            stashes.bottom(),
            sidebar.bottom(),
            "the stash ends the stack"
        );
        assert!(commits.size.height > natural, "the middle is the tall one");

        // Clicking a section's rows focuses *that* pane, and the keyboard
        // moves with it.
        cx.simulate_click(files.center(), gpui::Modifiers::default());
        assert_eq!(
            observed.read_with(&cx, |shell, _| shell.panes.focused_name().to_string()),
            "files"
        );
        cx.simulate_click(branches.center(), gpui::Modifiers::default());
        assert_eq!(
            observed.read_with(&cx, |shell, _| shell.panes.focused_name().to_string()),
            "branches",
        );
    }

    #[gpui::test]
    fn files_focus_reaches_the_registered_pane_and_says_so_when_there_is_none(
        cx: &mut TestAppContext,
    ) {
        // Bound before the name `shell` is taken by a list-carrying one.
        let bare = shell(None, cx);
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
            shell.sync_modes(cx);
        });
        shell.read_with(cx, |shell, _| {
            assert_eq!(shell.active_view_name(), "commits")
        });

        // Named dispatch — the same path the `2` key resolves through. It
        // swaps the list into the column and takes the keyboard with it.
        shell.update(cx, |shell, cx| shell.run_command("files.focus", cx));
        shell.read_with(cx, |shell, app| {
            assert_eq!(shell.panes.focused_name(), "files");
            assert_eq!(shell.modes.top(), "files");
            assert_eq!(
                shell.active_label(app).as_ref(),
                "gitten (main) · 0 changed"
            );
        });

        // And with no such resident — a fixture has no working tree — the key
        // is answered with a sentence, not silence.
        bare.update(cx, |shell, cx| shell.run_command("files.focus", cx));
        bare.read_with(cx, |shell, _| {
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

    /// A commits pane with rows *and* a repository behind it — what the
    /// history verbs need that the fixture-backed [commits] shell lacks.
    /// The keyboard starts on row 0, whose sha is forty zeros and whose
    /// short form is `abc000`.
    fn history_shell(cx: &mut TestAppContext) -> (gpui::Entity<DevShell>, Arc<RecordingRepo>) {
        let calls = Arc::default();
        let repo = Arc::new(RecordingRepo::new(Arc::clone(&calls)));
        let handle: gitten_git::Handle = repo.clone();
        let shell = shell(None, cx);
        shell.update(cx, |shell, cx| {
            let view = cx.new(|_| Commits::new(search_history(), Rc::new(Host::new())));
            cx.set_global(config::Active(Rc::new(Host::new())));
            shell.panes.register(
                "commits",
                Screen::commits(view, Source::Fixtures, Generation::default(), "~/src"),
            );
            shell.sync_modes(cx);
            shell.repo = Some((PathBuf::from("/recorded"), handle));
        });
        (shell, repo)
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
            shell.sync_modes(cx);
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
                    .as_ref()
                    .map(Notice::text)
                    .unwrap_or_default()
                    .contains("not supported here"),
                "{:?}",
                shell.notice
            );
        });
    }

    // ------------------------------------------------------- the main view

    /// Installs `raw` as the main view's rows, with no repository behind it —
    /// the shape a fixture window's main view has.
    fn install_main(shell: &gpui::Entity<DevShell>, raw: &str, cx: &mut TestAppContext) {
        shell.update(cx, |shell, cx| {
            let host = Rc::new(Host::new());
            let view = cx.new(|cx| {
                crate::views::diff::Diff::new(
                    gitten_core::parse_unified_diff(raw),
                    host.clone(),
                    cx,
                )
            });
            shell.main = Screen::diff(
                view,
                Some(Source::Fixtures),
                Generation::default(),
                "fixture",
            );
        });
    }

    const ONE_HUNK: &str = "\
diff --git a/one.txt b/one.txt
--- a/one.txt
+++ b/one.txt
@@ -1,3 +1,3 @@
 alpha
-beta
+BETA
 gamma
";

    /// The commits list under the keyboard, however far down the registry it
    /// sits after other lists registered beside it.
    fn column_commits(shell: &gpui::Entity<DevShell>, cx: &TestAppContext) -> String {
        let view = shell.read_with(cx, |shell, _| {
            let at = shell.panes.position("commits").expect("no commits list");
            match shell.panes.iter().nth(at) {
                Some(Screen::Commits { view, .. }) => view.clone(),
                _ => panic!("the column's resident is not a commits list"),
            }
        });
        view.read_with(cx, |v, _| {
            v.current().map(|c| c.sha.clone()).unwrap_or_default()
        })
    }

    #[gpui::test]
    fn swapping_lists_preserves_each_list_cursor(cx: &mut TestAppContext) {
        let shell = commits_shell(cx);
        // The commit cursor moves two rows down...
        for _ in 0..2 {
            shell.update(cx, |shell, cx| shell.run_command("view.down", cx));
        }
        assert_eq!(column_commits(&shell, cx), search_commit(2).sha);

        // ...a files list swaps in and its own cursor moves to its bottom...
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
        shell.update(cx, |shell, cx| {
            let files = cx.new(|_| {
                crate::views::files::Files::from_prepared(crate::views::files::prepare(tree, "r"))
            });
            shell.panes.register(
                "files",
                Screen::files(files, Generation::default(), "files"),
            );
            shell.sync_modes(cx);
        });
        shell.update(cx, |shell, cx| shell.run_command("view.bottom", cx));

        // ...and both survive every swap back. The views are never rebuilt —
        // swapping is focusing.
        shell.update(cx, |shell, cx| shell.run_command("commits.focus", cx));
        assert_eq!(column_commits(&shell, cx), search_commit(2).sha);
        shell.update(cx, |shell, cx| shell.run_command("files.focus", cx));
        shell.read_with(cx, |shell, cx| match shell.active() {
            Some(Screen::Files { view, .. }) => {
                assert_eq!(
                    view.read(cx).current_file().map(|f| f.path_text.as_ref()),
                    Some("notes.md"),
                    "the files cursor did not survive the swaps"
                );
            }
            _ => panic!("the files list is not showing"),
        });
    }

    #[gpui::test]
    fn keys_follow_the_region_the_list_moves_lists_and_j_scrolls_the_diff(cx: &mut TestAppContext) {
        let shell = commits_shell(cx);
        install_main(&shell, ONE_HUNK, cx);
        // From the column, `j` moves the commit list and touches nothing else.
        shell.update(cx, |shell, cx| shell.run_command("view.down", cx));
        assert_eq!(column_commits(&shell, cx), search_commit(1).sha);
        shell.read_with(cx, |shell, cx| match &shell.main {
            Screen::Diff { view, .. } => assert_eq!(view.read(cx).cursor(), 0),
            _ => panic!("main view lost"),
        });

        // Enter hands the keyboard to the diff region...
        shell.update(cx, |shell, cx| shell.run_command("commits.open-diff", cx));
        shell.read_with(cx, |shell, _| {
            assert_eq!(shell.spot, super::Spot::Main);
            assert_eq!(shell.modes.top(), "diff");
        });

        // ...and now `j` scrolls the diff, leaving the list where it was.
        shell.update(cx, |shell, cx| shell.run_command("view.down", cx));
        shell.read_with(cx, |shell, cx| match &shell.main {
            Screen::Diff { view, .. } => assert_eq!(view.read(cx).cursor(), 1),
            _ => panic!("main view lost"),
        });
        assert_eq!(column_commits(&shell, cx), search_commit(1).sha);
    }

    #[gpui::test]
    fn a_fast_cursor_run_loads_only_the_commit_it_settles_on(cx: &mut TestAppContext) {
        let (shell, repo) = history_shell(cx);
        // Five rows of cursor movement inside one debounce window: every row
        // re-aims the request, and only the last aim survives its guard.
        for _ in 0..5 {
            shell.update(cx, |shell, cx| shell.run_command("view.down", cx));
        }
        shell.read_with(cx, |shell, _| {
            assert_eq!(shell.request.get(), 5, "each row re-aimed the request");
            assert!(shell.loading.get(), "the settled load is in flight");
        });
        assert!(
            repo.diffs_wrote().is_empty(),
            "a load ran before the cursor settled"
        );

        // Settled: one timer fires, one acquisition runs, for the final row.
        cx.executor().advance_clock(super::DIFF_DEBOUNCE);
        cx.run_until_parked();
        shell.read_with(cx, |shell, _| assert!(!shell.loading.get()));
        assert_eq!(repo.diffs_wrote(), vec![search_commit(5).sha]);
    }

    #[gpui::test]
    fn startup_names_and_loads_the_newest_commit(cx: &mut TestAppContext) {
        let (shell, repo) = history_shell(cx);
        // What main() runs before frame one: schedule through the same rails
        // every later selection rides.
        shell.update(cx, |shell, cx| shell.sync_main_diff(cx));
        // Named before anything loaded — the header strip is true in frame one.
        shell.read_with(cx, |shell, _| {
            assert_eq!(
                shell.head.borrow().as_ref().map(|c| c.sha.clone()),
                Some(search_commit(0).sha)
            );
            assert!(shell.loading.get());
        });
        cx.executor().advance_clock(super::DIFF_DEBOUNCE);
        cx.run_until_parked();
        shell.read_with(cx, |shell, _| {
            assert!(!shell.loading.get(), "the startup load never came home");
            assert_eq!(
                shell.head.borrow().as_ref().map(|c| c.sha.clone()),
                Some(search_commit(0).sha)
            );
        });
        assert_eq!(repo.diffs_wrote(), vec![search_commit(0).sha]);
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
        head: std::sync::Mutex<gitten_core::refs::HeadState>,
        /// Where main sits against origin/main, moved by the sync verbs the
        /// way a real remote moves it — fetch reveals behind, pull closes
        /// the distance, push spends ahead. Interior-mutable so a test can
        /// set the starting gap.
        distance: std::sync::Mutex<(u32, u32)>,
        /// Set when a verb has left the index conflicted — the state a
        /// refused revert leaves behind, which only the next status read
        /// reveals. Interior-mutable so a test arms it before the verb runs.
        conflict: AtomicBool,
        /// Paths status reports as known to no part of git. Interior-mutable
        /// so a test arms exactly the files its diff fixture talks about —
        /// the fact hunk verbs classify creations by.
        untracked: std::sync::Mutex<Vec<String>>,
        /// What `log` answers — the window of history a rewrite composes
        /// over. Empty until a test serves it.
        log_answer: std::sync::Mutex<Vec<Commit>>,
        /// Which revspecs `pairs` was asked to diff, in order — the record a
        /// main-view debounce test reads. Separate from [`Self::calls`] so
        /// write assertions never see a read.
        diffs: std::sync::Mutex<Vec<String>>,
    }

    impl RecordingRepo {
        fn new(calls: Arc<std::sync::Mutex<Vec<String>>>) -> Self {
            Self {
                calls,
                head: std::sync::Mutex::new(gitten_core::refs::HeadState::Branch {
                    name: gitten_core::refs::RefName::from("main"),
                    commit: None,
                }),
                distance: std::sync::Mutex::new((0, 0)),
                conflict: AtomicBool::new(false),
                untracked: std::sync::Mutex::new(Vec::new()),
                log_answer: std::sync::Mutex::new(Vec::new()),
                diffs: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn wrote(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }

        /// The revspecs `pairs` was asked for — one entry per diff acquisition.
        fn diffs_wrote(&self) -> Vec<String> {
            self.diffs.lock().unwrap().clone()
        }

        /// Serves `log`'s answer — the same commits the pane shows, which is
        /// what makes a composed plan checkable against what was asked for.
        fn serve_log(&self, commits: Vec<Commit>) {
            *self.log_answer.lock().unwrap() = commits;
        }

        /// Makes the next `revert` refuse the way git does on a conflict:
        /// nonzero, its own words, and unmerged paths left for the status
        /// read that follows to find.
        fn arm_conflict(&self) {
            self.conflict.store(true, Ordering::SeqCst);
        }

        /// Resolves it — the state `git add` leaves behind on a real
        /// machine, which is what a continue needs to find.
        fn clear_conflict(&self) {
            self.conflict.store(false, Ordering::SeqCst);
        }

        /// Detaches HEAD, for the refusal half of the sync tests.
        fn detach(&self) {
            *self.head.lock().unwrap() = gitten_core::refs::HeadState::Detached {
                commit: "0123456789abcdef".into(),
            };
        }

        /// Names paths the next `status` read reports as untracked — the
        /// fact a hunk verb classifies a creation by.
        fn arm_untracked(&self, paths: &[&str]) {
            *self.untracked.lock().unwrap() = paths.iter().map(|p| p.to_string()).collect();
        }

        /// The tracking pair the branches read reports for main.
        fn counts(&self) -> (u32, u32) {
            *self.distance.lock().unwrap()
        }
    }

    /// One modelled local branch, for the fakes' ref pictures.
    fn branch_ref(name: &str, head: bool) -> gitten_core::refs::Branch {
        gitten_core::refs::Branch {
            name: gitten_core::refs::RefName::from(name),
            commit: "0123456789abcdef0123456789abcdef01234567".into(),
            upstream: None,
            head,
        }
    }

    /// Serves every verb by writing down what was asked of it — the fake a
    /// dispatch test needs, standing where the binary implementation stands in
    /// a live window.
    impl Repo for RecordingRepo {
        fn log(&self, _: usize) -> gitten_git::Result<Vec<Commit>> {
            Ok(self.log_answer.lock().unwrap().clone())
        }

        fn pairs(&self, revspec: &str) -> gitten_git::Result<Vec<Pair>> {
            self.diffs.lock().unwrap().push(revspec.to_string());
            Ok(Vec::new())
        }

        fn status(&self) -> gitten_git::Result<Status> {
            // A standing tree rather than an empty answer: a real repository
            // still has changes after a write, and the re-acquire a successful
            // job schedules must find rows to put the keyboard back on.
            let mut tree = Status::default();
            // The refused revert's leftover: unmerged paths in the index,
            // found by the re-read the failure schedules.
            if self.conflict.load(Ordering::SeqCst) {
                tree.conflicts.push(gitten_core::status::ConflictEntry {
                    path: "poem.txt".into(),
                    state: gitten_core::status::ConflictKind::BothModified,
                    kind: gitten_core::status::Kind::File,
                    submodule: Default::default(),
                });
            }
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
            for path in self.untracked.lock().unwrap().iter() {
                tree.untracked.push(gitten_core::status::UntrackedEntry {
                    path: path.as_str().into(),
                });
            }
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

        fn reset(
            &self,
            mode: gitten_core::refs::ResetMode,
            target: &[u8],
        ) -> gitten_git::Result<()> {
            self.calls.lock().unwrap().push(format!(
                "reset {} {}",
                mode.flag(),
                String::from_utf8_lossy(target)
            ));
            Ok(())
        }

        fn revert(&self, commit: &[u8]) -> gitten_git::Result<()> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("revert {}", String::from_utf8_lossy(commit)));
            if self.conflict.load(Ordering::SeqCst) {
                // git's own sentence for the case, close enough to be
                // recognised: refused, and the question left in the tree.
                return Err("error: could not revert 0000000...".into());
            }
            Ok(())
        }

        fn rebase_todo(
            &self,
            upstream: &[u8],
            script: &gitten_git::TodoScript,
        ) -> gitten_git::Result<()> {
            // The plan travels in the record lossily — these shas are hex
            // and the assertions read them; the real bytes are covered by
            // the git crate's own tests.
            self.calls.lock().unwrap().push(format!(
                "rebase onto {} with plan {}",
                String::from_utf8_lossy(upstream),
                String::from_utf8_lossy(&script.emit())
            ));
            Ok(())
        }

        fn rebase_onto(&self, upstream: &[u8]) -> gitten_git::Result<()> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("rebase onto {}", String::from_utf8_lossy(upstream)));
            Ok(())
        }

        fn rebase_abort(&self) -> gitten_git::Result<()> {
            self.calls.lock().unwrap().push("rebase abort".into());
            Ok(())
        }

        fn rebase_continue(&self) -> gitten_git::Result<()> {
            self.calls.lock().unwrap().push("rebase continue".into());
            Ok(())
        }

        fn amend(&self, message: &str) -> gitten_git::Result<String> {
            self.calls.lock().unwrap().push(format!("amend {message}"));
            Ok("f00d".into())
        }

        fn discard(&self, path: &[u8]) -> gitten_git::Result<()> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("discard {}", String::from_utf8_lossy(path)));
            Ok(())
        }

        // The patch verbs: the bytes are the payload and no test reads them
        // back here — recording the size says "it arrived whole" without a
        // wall of patch text in the assertion.
        fn stage_patch(&self, patch: &[u8]) -> gitten_git::Result<()> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("stage-patch {} bytes", patch.len()));
            Ok(())
        }

        fn unstage_patch(&self, patch: &[u8]) -> gitten_git::Result<()> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("unstage-patch {} bytes", patch.len()));
            Ok(())
        }

        fn discard_patch(&self, patch: &[u8]) -> gitten_git::Result<()> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("discard-patch {} bytes", patch.len()));
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

        fn stash_push(&self, message: Option<&str>) -> gitten_git::Result<usize> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("stash push {message:?}"));
            Ok(0)
        }

        fn stash_apply(&self, index: usize) -> gitten_git::Result<()> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("stash apply stash@{index}"));
            Ok(())
        }

        fn stash_pop(&self, index: usize) -> gitten_git::Result<()> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("stash pop stash@{index}"));
            Ok(())
        }

        fn stash_drop(&self, index: usize) -> gitten_git::Result<()> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("stash drop stash@{index}"));
            Ok(())
        }
        fn checkout(&self, name: &[u8]) -> gitten_git::Result<()> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("checkout {}", String::from_utf8_lossy(name)));
            Ok(())
        }

        fn create_branch(&self, name: &[u8], _start: Option<&[u8]>) -> gitten_git::Result<()> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("create {}", String::from_utf8_lossy(name)));
            Ok(())
        }

        fn delete_branch(&self, name: &[u8], force: bool) -> gitten_git::Result<()> {
            self.calls.lock().unwrap().push(format!(
                "delete {}{}",
                String::from_utf8_lossy(name),
                if force { " -D" } else { "" }
            ));
            Ok(())
        }

        fn rename_branch(&self, from: &[u8], to: &[u8]) -> gitten_git::Result<()> {
            self.calls.lock().unwrap().push(format!(
                "rename {} {}",
                String::from_utf8_lossy(from),
                String::from_utf8_lossy(to)
            ));
            Ok(())
        }

        fn cherry_pick(&self, sha: &[u8]) -> gitten_git::Result<()> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("cherry-pick {}", String::from_utf8_lossy(sha)));
            if self.conflict.load(Ordering::SeqCst) {
                // git's own sentence for a pick that cannot apply its
                // change: refused, and the question left in the tree.
                return Err("error: could not apply 0000000...".into());
            }
            Ok(())
        }

        fn cherry_pick_abort(&self) -> gitten_git::Result<()> {
            self.calls.lock().unwrap().push("cherry-pick abort".into());
            Ok(())
        }

        fn cherry_pick_continue(&self) -> gitten_git::Result<()> {
            self.calls
                .lock()
                .unwrap()
                .push("cherry-pick continue".into());
            Ok(())
        }

        fn create_tag(
            &self,
            name: &[u8],
            target: &[u8],
            _message: Option<&str>,
        ) -> gitten_git::Result<()> {
            // Lossy on purpose, like every record here: these names are hex
            // and text and the assertions read them; the byte discipline is
            // the git crate's own tests to hold.
            self.calls.lock().unwrap().push(format!(
                "tag {} at {}",
                String::from_utf8_lossy(name),
                String::from_utf8_lossy(target)
            ));
            Ok(())
        }

        fn branches(&self) -> gitten_git::Result<Vec<gitten_core::refs::Branch>> {
            let (ahead, behind) = *self.distance.lock().unwrap();
            let mut main = branch_ref("main", true);
            main.upstream = Some(gitten_core::refs::Upstream {
                remote: gitten_core::refs::RefName::from("origin"),
                branch: gitten_core::refs::RefName::from("main"),
                ahead: Some(ahead),
                behind: Some(behind),
            });
            Ok(vec![branch_ref("feature", false), main])
        }

        fn remote_branches(&self) -> gitten_git::Result<Vec<gitten_core::refs::RemoteBranch>> {
            Ok(vec![gitten_core::refs::RemoteBranch {
                remote: gitten_core::refs::RefName::from("origin"),
                branch: gitten_core::refs::RefName::from("main"),
                commit: "0123456789abcdef0123456789abcdef01234567".into(),
            }])
        }

        fn head(&self) -> gitten_git::Result<gitten_core::refs::HeadState> {
            Ok(self.head.lock().unwrap().clone())
        }

        fn remotes(&self) -> gitten_git::Result<Vec<gitten_core::refs::Remote>> {
            Ok(vec![gitten_core::refs::Remote {
                name: gitten_core::refs::RefName::from("origin"),
                urls: vec!["https://example.invalid/x".into()],
            }])
        }

        fn push(&self, remote: &[u8], branch: &[u8]) -> gitten_git::Result<()> {
            self.calls.lock().unwrap().push(format!(
                "push {} {}",
                String::from_utf8_lossy(remote),
                String::from_utf8_lossy(branch)
            ));
            *self.distance.lock().unwrap() = (0, 0);
            Ok(())
        }

        fn pull(&self) -> gitten_git::Result<()> {
            self.calls.lock().unwrap().push("pull".into());
            *self.distance.lock().unwrap() = (0, 0);
            Ok(())
        }

        fn fetch(&self, remote: Option<&[u8]>) -> gitten_git::Result<()> {
            let named = match remote {
                Some(remote) => String::from_utf8_lossy(remote).into_owned(),
                None => "--all".into(),
            };
            self.calls.lock().unwrap().push(format!("fetch {named}"));
            // A fetched stranger commit: the only honest first reading.
            *self.distance.lock().unwrap() = (0, 1);
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
        let repo = Arc::new(RecordingRepo::new(Arc::clone(&calls)));
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
            shell.sync_modes(cx);
            shell.repo = Some((PathBuf::from("/recorded"), handle.clone()));
            cx.set_global(config::Active(Rc::new(Host::new())));
        });
        (shell, repo, handle)
    }

    /// A shell over a two-entry stash stack, with the repository recording
    /// behind it and the keyboard on the stash pane's newest row. The same
    /// shape startup builds: root pane, then files, then the stack.
    fn stashes_shell(cx: &mut TestAppContext) -> (gpui::Entity<DevShell>, Arc<RecordingRepo>) {
        let stashes = vec![
            gitten_core::refs::Stash {
                index: 0,
                commit: "c0ffee0".into(),
                message: "On main: hand written".into(),
            },
            gitten_core::refs::Stash {
                index: 1,
                commit: "c0ffee1".into(),
                message: "WIP on main: abc1234 seed".into(),
            },
        ];
        let calls = Arc::default();
        let repo = Arc::new(RecordingRepo {
            calls: Arc::clone(&calls),
            head: std::sync::Mutex::new(gitten_core::refs::HeadState::Branch {
                name: "main".into(),
                commit: Some("abc1234".into()),
            }),
            distance: std::sync::Mutex::new((0, 0)),
            conflict: AtomicBool::new(false),
            untracked: std::sync::Mutex::new(Vec::new()),
            log_answer: std::sync::Mutex::new(Vec::new()),
            diffs: std::sync::Mutex::new(Vec::new()),
        });
        let handle: gitten_git::Handle = repo.clone();
        let shell = shell(None, cx);
        shell.update(cx, |shell, cx| {
            let host = Rc::new(Host::new());
            let view = cx.new(|_| {
                crate::views::stashes::Stashes::from_prepared(crate::views::stashes::prepare(
                    &stashes, "r",
                ))
            });
            view.update(cx, |s, _| {
                s.run_view("view.top", &host);
            });
            let files = cx.new(|_| {
                crate::views::files::Files::from_prepared(crate::views::files::prepare(
                    Status::default(),
                    "r",
                ))
            });
            shell.panes.register(
                "files",
                Screen::files(files, Generation::default(), "r · 0 changed"),
            );
            shell.panes.register(
                "stashes",
                Screen::stashes(view, Generation::default(), "r · 2 parked"),
            );
            shell.sync_modes(cx);
            shell.repo = Some((PathBuf::from("/recorded"), handle));
            cx.set_global(config::Active(Rc::new(Host::new())));
        });
        (shell, repo)
    }

    /// A shell over a working-tree diff pane: the keyboard on the first line
    /// row of the first hunk, the repository recording behind it. `arg` is
    /// the diff's revspec — empty for the working tree, a commit for the
    /// refusal tests.
    fn diff_shell(
        cx: &mut TestAppContext,
        arg: &'static str,
    ) -> (gpui::Entity<DevShell>, Arc<RecordingRepo>) {
        let raw = "\
diff --git a/one.txt b/one.txt
--- a/one.txt
+++ b/one.txt
@@ -1,3 +1,3 @@
 alpha
-beta
+BETA
 gamma
";
        let calls = Arc::default();
        let repo = Arc::new(RecordingRepo::new(Arc::clone(&calls)));
        let handle: gitten_git::Handle = repo.clone();
        let shell = shell(None, cx);
        shell.update(cx, |shell, cx| {
            let host = Rc::new(Host::new());
            let view = cx.new(|cx| {
                crate::views::diff::Diff::new(
                    gitten_core::parse_unified_diff(raw),
                    host.clone(),
                    cx,
                )
            });
            // Off the file header and onto the hunk's first line — where
            // space means "this hunk".
            view.update(cx, |d, _| {
                d.run_view("view.down", &host);
            });
            // The main region is where a diff lives now: installed there and
            // handed the keyboard, exactly as a selection's enter would.
            shell.main = Screen::diff(
                view,
                Some(Source::Repo {
                    path: PathBuf::from("/recorded"),
                    arg: arg.into(),
                }),
                Generation::default(),
                "diff",
            );
            shell.set_spot(super::Spot::Main, cx);
            shell.sync_modes(cx);
            shell.repo = Some((PathBuf::from("/recorded"), handle));
            cx.set_global(config::Active(Rc::new(Host::new())));
        });
        (shell, repo)
    }

    /// A shell whose diff is an untracked file's whole-addition hunk.
    ///
    /// The fake's status names `fresh.txt` untracked — the fact the refusal
    /// classifies by; a fixture whose status stayed silent would send the
    /// patch to git instead, as a tracked file's hunk deserves.
    fn creation_diff_shell(
        cx: &mut TestAppContext,
    ) -> (gpui::Entity<DevShell>, Arc<RecordingRepo>) {
        let (shell, repo) = addition_diff_shell(
            cx,
            "\
diff --git a/fresh.txt b/fresh.txt
--- /dev/null
+++ b/fresh.txt
@@ -0,0 +1,2 @@
+first
+second
",
        );
        repo.arm_untracked(&["fresh.txt"]);
        (shell, repo)
    }

    /// A shell whose diff is one whole-addition hunk — the shape an
    /// untracked file's diff and a `[diff] context = 0` insertion share,
    /// which is exactly why classification is not the numbers' job. Status
    /// stays as the fake holds it: nothing here is untracked unless a test
    /// arms it.
    fn addition_diff_shell(
        cx: &mut TestAppContext,
        raw: &str,
    ) -> (gpui::Entity<DevShell>, Arc<RecordingRepo>) {
        let calls = Arc::default();
        let repo = Arc::new(RecordingRepo::new(Arc::clone(&calls)));
        let handle: gitten_git::Handle = repo.clone();
        let shell = shell(None, cx);
        shell.update(cx, |shell, cx| {
            let host = Rc::new(Host::new());
            let view = cx.new(|cx| {
                crate::views::diff::Diff::new(
                    gitten_core::parse_unified_diff(raw),
                    host.clone(),
                    cx,
                )
            });
            view.update(cx, |d, _| {
                d.run_view("view.down", &host);
            });
            shell.main = Screen::diff(
                view,
                Some(Source::Repo {
                    path: PathBuf::from("/recorded"),
                    arg: String::new(),
                }),
                Generation::default(),
                "diff",
            );
            shell.set_spot(super::Spot::Main, cx);
            shell.sync_modes(cx);
            shell.repo = Some((PathBuf::from("/recorded"), handle));
            cx.set_global(config::Active(Rc::new(Host::new())));
        });
        (shell, repo)
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

        // Back to the top — which is gone.txt under *staged*, the heading
        // above it being furniture the cursor skips: same key, other
        // direction.
        shell.update(cx, |shell, cx| {
            let Some(Screen::Files { view, .. }) = shell.active() else {
                panic!("files pane lost");
            };
            let host = Rc::new(Host::new());
            view.update(cx, |f, _| {
                f.run_view("view.top", &host);
            });
        });
        shell.update(cx, |shell, cx| shell.run_command("files.stage", cx));
        pump_write(&shell, cx);
        assert_eq!(repo.wrote(), vec!["stage notes.md", "unstage gone.txt"]);
    }

    // ------------------------------------------------------- the stash verbs

    #[gpui::test]
    fn space_applies_the_stash_row_through_the_pump(cx: &mut TestAppContext) {
        let (shell, repo) = stashes_shell(cx);
        // The keyboard starts on stash@{0}, the newest entry.
        shell.update(cx, |shell, cx| shell.run_command("stashes.apply", cx));
        pump_write(&shell, cx);
        assert_eq!(repo.wrote(), vec!["stash apply stash@0"]);
        // And the generation rails ran, as they do after every write.
        shell.read_with(cx, |shell, _| assert!(shell.generation.get() > 0));
    }

    #[gpui::test]
    fn pop_runs_without_asking_and_names_its_own_index(cx: &mut TestAppContext) {
        let (shell, repo) = stashes_shell(cx);
        shell.update(cx, |shell, cx| {
            let Some(Screen::Stashes { view, .. }) = shell.active() else {
                panic!("stashes pane lost");
            };
            let host = Rc::new(Host::new());
            view.update(cx, |v, _| {
                v.run_view("view.bottom", &host); // onto stash@{1}
            });
        });
        shell.update(cx, |shell, cx| shell.run_command("stashes.pop", cx));
        pump_write(&shell, cx);
        assert_eq!(
            repo.wrote(),
            vec!["stash pop stash@1"],
            "the index travels, whatever row it came from"
        );
    }

    #[gpui::test]
    fn a_drop_arms_then_confirms_on_the_second_press_of_the_same_row(cx: &mut TestAppContext) {
        let (shell, repo) = stashes_shell(cx);

        // First press: asked, in the band; nothing written.
        shell.update(cx, |shell, cx| shell.run_command("stashes.drop", cx));
        shell.read_with(cx, |shell, _| {
            assert!(repo.wrote().is_empty());
            assert!(
                shell
                    .notice
                    .as_ref()
                    .map(Notice::text)
                    .unwrap_or_default()
                    .contains("drop stash@{0}? press again"),
                "{:?}",
                shell.notice
            );
        });

        // Second press on the same row: spent, written, question cleared.
        shell.update(cx, |shell, cx| shell.run_command("stashes.drop", cx));
        pump_write(&shell, cx);
        assert_eq!(repo.wrote(), vec!["stash drop stash@0"]);
        shell.read_with(cx, |shell, _| assert!(shell.notice.is_none()));
    }

    #[gpui::test]
    fn a_cursor_move_disarms_an_armed_stash_drop(cx: &mut TestAppContext) {
        let (shell, repo) = stashes_shell(cx);
        shell.update(cx, |shell, cx| {
            let Some(Screen::Stashes { view, .. }) = shell.active() else {
                panic!("stashes pane lost");
            };
            let host = Rc::new(Host::new());
            view.update(cx, |v, _| {
                v.run_view("view.bottom", &host); // arm target: stash@{1}
            });
        });
        shell.update(cx, |shell, cx| shell.run_command("stashes.drop", cx));
        shell.read_with(cx, |shell, _| {
            assert!(
                shell
                    .notice
                    .as_ref()
                    .map(Notice::text)
                    .unwrap_or_default()
                    .contains("drop stash@{1}?"),
                "{:?}",
                shell.notice
            );
        });

        // One step up: the keyboard left the question's row, so the next
        // press asks about the row it lands on instead of executing.
        shell.update(cx, |shell, cx| {
            let Some(Screen::Stashes { view, .. }) = shell.active() else {
                panic!("stashes pane lost");
            };
            let host = Rc::new(Host::new());
            view.update(cx, |v, _| {
                v.run_view("view.up", &host);
            });
        });
        shell.update(cx, |shell, cx| shell.run_command("stashes.drop", cx));
        shell.read_with(cx, |shell, _| {
            assert!(repo.wrote().is_empty(), "nothing was dropped");
            assert!(
                shell
                    .notice
                    .as_ref()
                    .map(Notice::text)
                    .unwrap_or_default()
                    .contains("drop stash@{0}?"),
                "the question followed the keyboard: {:?}",
                shell.notice
            );
        });
    }

    #[gpui::test]
    fn the_stash_verbs_say_so_outside_the_stash_pane(cx: &mut TestAppContext) {
        let (shell, repo, _handle) = files_shell(cx);
        shell.update(cx, |shell, cx| shell.run_command("stashes.apply", cx));
        shell.read_with(cx, |shell, _| {
            assert!(repo.wrote().is_empty());
            assert!(
                shell
                    .notice
                    .as_ref()
                    .map(Notice::text)
                    .unwrap_or_default()
                    .contains("not supported here"),
                "{:?}",
                shell.notice
            );
        });
    }

    #[gpui::test]
    fn files_stash_parks_the_tree_and_the_refresh_rails_run(cx: &mut TestAppContext) {
        let (shell, repo, _handle) = files_shell(cx);
        shell.update(cx, |shell, cx| shell.run_command("files.stash", cx));
        // The whole production path: job queued, drained by the same pump the
        // window runs, generation bumped, every repository pane re-acquired.
        pump_write(&shell, cx);
        assert_eq!(repo.wrote(), vec!["stash push None"]);
        shell.read_with(cx, |shell, _| assert!(shell.generation.get() > 0));

        // Over a fixture there is nothing to park, and the key says so.
        shell.update(cx, |shell, _| shell.repo = None);
        shell.update(cx, |shell, cx| shell.run_command("files.stash", cx));
        shell.read_with(cx, |shell, _| {
            assert!(
                shell
                    .notice
                    .as_deref()
                    .unwrap_or_default()
                    .contains("no working tree to park"),
                "{:?}",
                shell.notice
            );
        });
    }

    #[gpui::test]
    fn stashes_focus_reaches_the_registered_pane_and_says_so_when_there_is_none(
        cx: &mut TestAppContext,
    ) {
        let bare = shell(None, cx);
        let (shell, _repo) = stashes_shell(cx);
        // Registration left the keyboard on the stack; named dispatch gets
        // back there from anywhere.
        shell.update(cx, |shell, cx| {
            shell.panes.focus(0);
            shell.sync_modes(cx);
        });
        shell.update(cx, |shell, cx| shell.run_command("stashes.focus", cx));
        shell.read_with(cx, |shell, app| {
            assert_eq!(shell.panes.focused_name(), "stashes");
            assert_eq!(shell.modes.top(), "stashes");
            assert_eq!(shell.active_label(app).as_ref(), "r · 2 parked");
        });

        // And with no such resident — a fixture has no stash stack — the key
        // is answered with a sentence, not silence.
        bare.update(cx, |shell, cx| shell.run_command("stashes.focus", cx));
        bare.read_with(cx, |shell, _| {
            assert!(shell.notice.is_some(), "a missing pane went unsaid");
        });
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
    fn a_failed_write_lands_on_the_error_band_and_still_reacquires(cx: &mut TestAppContext) {
        let (shell, _repo, _handle) = files_shell(cx);
        let submit = wire_runner(&shell, cx);
        assert!(submit.submit(Box::new(Fails)).is_ok());

        // The production pump, not a test-local read of the queue — through
        // the failure *and* the re-acquire wave it schedules, which is the
        // point: a refused write may have left work behind.
        pump_until(&shell, cx, |shell| shell.error.is_some());
        cx.run_until_parked();
        shell.read_with(cx, |shell, _| {
            assert_eq!(
                shell.error.as_deref(),
                // The summary, not the record: git's argv is stripped, git's
                // words are not.
                Some("hook declined"),
                "the repository's own words reached the band"
            );
            assert!(shell.running.is_none(), "the job still reads as running");
            assert!(
                shell.generation > Generation::default(),
                "a refusal left the panes believing they are current"
            );
            assert_eq!(shell.refresh_pending, 0, "the wave never came home");
        });
    }

    /// An error keeps its record whole and reads as its first line: git's
    /// argv is the prefix the band strips, and git's words are what survives.
    #[test]
    fn an_error_arrives_whole_and_reads_as_its_first_line() {
        let e = GitError::new("git push origin main: error: failed to push some refs\nhint: …");
        assert_eq!(
            e.full, "git push origin main: error: failed to push some refs\nhint: …",
            "the record is kept verbatim"
        );
        assert_eq!(
            e.summary, "error: failed to push some refs",
            "the first non-empty line of git's own words"
        );

        let bare = GitError::new("fatal: not a git repository");
        assert_eq!(bare.summary, "fatal: not a git repository");
    }

    /// `esc` peels the message overlay first, the error second, and only then
    /// falls through to the ladder the key was already on.
    #[gpui::test]
    async fn esc_peels_the_overlay_then_the_error_then_the_ladder(cx: &mut TestAppContext) {
        let shell = shell(None, cx);
        shell.update(cx, |shell, _| {
            shell.error = Some(GitError::new("git commit: hook declined"));
            shell.show_message = true;
        });

        // First `esc`: the overlay closes, the error stands.
        shell.update(cx, |shell, cx| shell.back(cx));
        shell.read_with(cx, |shell, _| {
            assert!(!shell.show_message, "the overlay closed");
            assert!(shell.error.is_some(), "the error outlives its overlay");
        });

        // Second: the error itself is gone.
        shell.update(cx, |shell, cx| shell.back(cx));
        shell.read_with(cx, |shell, _| assert!(shell.error.is_none()));
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
                    // A branches pane carries no source of its own — it is
                    // always about this window's repository, like files.
                    Screen::Branches { generation, .. } => {
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
                            Screen::Stashes { .. } => "stashes",
                            Screen::Status { .. } => "status",
                            Screen::Commits { .. }
                            | Screen::Files { .. }
                            | Screen::Branches { .. } => {
                                unreachable!()
                            }
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
            shell.sync_modes(cx);
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

        // And a clean tree: nothing under the keyboard to act on. The only
        // way to have nothing there — the cursor never rests on a heading.
        let (shell, repo, _handle) = tree_shell(cx, Status::default());
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
                shell.notice.as_ref().map(Notice::text),
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
                shell.notice.as_ref().map(Notice::text),
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
                f.run_view("view.top", &Rc::new(Host::new())); // gone.txt, staged
            });
        });
        shell.update(cx, |shell, cx| shell.run_command("files.discard", cx));
        shell.read_with(cx, |shell, _| {
            let notice = shell.notice.as_ref().map(Notice::text).unwrap_or_default();
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
                f.run_view("view.top", &Rc::new(Host::new())); // gone.txt
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

    // ---------------------------------------------------------- the hunk verbs

    #[gpui::test]
    fn space_stages_the_hunk_under_the_keyboard(cx: &mut TestAppContext) {
        let (shell, repo) = diff_shell(cx, "");
        wire_runner(&shell, cx);

        // Space: the hunk under the keyboard goes to the index as one job,
        // named for the patch's size because there is no path to name.
        shell.update(cx, |shell, cx| shell.run_command("diff.stage-hunk", cx));
        pump_write(&shell, cx);
        let wrote = repo.wrote();
        assert_eq!(wrote.len(), 1, "{wrote:?}");
        assert!(
            wrote[0].starts_with("stage-patch ") && wrote[0].ends_with(" bytes"),
            "{wrote:?}"
        );
        // One write, one finish: the band is clear and the count moved.
        shell.read_with(cx, |shell, _| {
            assert_eq!(shell.generation.get(), 1);
            assert_eq!(shell.running, None);
            assert_eq!(shell.error, None);
        });
    }

    #[gpui::test]
    fn u_unstages_the_hunk_back_out_of_the_index(cx: &mut TestAppContext) {
        // A fresh pane rather than the staged one above: a successful write
        // re-acquires every repository pane, and the fake behind this shell
        // answers that read with an empty tree — after which there is no
        // hunk under the keyboard, exactly as on a cleaned-up working tree.
        let (shell, repo) = diff_shell(cx, "");
        wire_runner(&shell, cx);

        shell.update(cx, |shell, cx| shell.run_command("diff.unstage-hunk", cx));
        pump_write(&shell, cx);
        assert!(
            repo.wrote()[0].starts_with("unstage-patch "),
            "{:?}",
            repo.wrote()
        );
    }

    #[gpui::test]
    fn a_commit_diff_has_nothing_to_stage_and_says_so(cx: &mut TestAppContext) {
        let (shell, repo) = diff_shell(cx, "abc1234");
        wire_runner(&shell, cx);

        for command in ["diff.stage-hunk", "diff.unstage-hunk", "diff.discard-hunk"] {
            shell.update(cx, |shell, cx| shell.run_command(command, cx));
            shell.read_with(cx, |shell, _| {
                let notice = shell.notice.as_ref().map(Notice::text).unwrap_or_default();
                assert!(notice.contains("between commits"), "{command}: {notice:?}");
            });
        }
        assert!(repo.wrote().is_empty(), "nothing was queued");
    }

    #[gpui::test]
    fn discarding_a_hunk_asks_twice_on_the_same_spot(cx: &mut TestAppContext) {
        let (shell, repo) = diff_shell(cx, "");
        wire_runner(&shell, cx);

        shell.update(cx, |shell, cx| shell.run_command("diff.discard-hunk", cx));
        shell.read_with(cx, |shell, _| {
            let notice = shell.notice.as_ref().map(Notice::text).unwrap_or_default();
            assert!(
                notice.contains("press again") && notice.contains("hunk"),
                "{notice:?}"
            );
        });
        assert!(repo.wrote().is_empty(), "the question queued nothing");

        // Second press on the same spot spends the arm and runs.
        shell.update(cx, |shell, cx| shell.run_command("diff.discard-hunk", cx));
        pump_write(&shell, cx);
        assert_eq!(repo.wrote().len(), 1, "{:?}", repo.wrote());
        assert!(
            repo.wrote()[0].starts_with("discard-patch "),
            "{:?}",
            repo.wrote()
        );
    }

    #[gpui::test]
    fn an_untracked_file_refuses_hunk_verbs_and_names_the_pane_that_serves_it(
        cx: &mut TestAppContext,
    ) {
        let (shell, repo) = creation_diff_shell(cx);
        wire_runner(&shell, cx);

        for command in ["diff.stage-hunk", "diff.unstage-hunk", "diff.discard-hunk"] {
            shell.update(cx, |shell, cx| shell.run_command(command, cx));
            shell.read_with(cx, |shell, _| {
                let notice = shell.notice.as_ref().map(Notice::text).unwrap_or_default();
                assert!(notice.contains("files pane"), "{command}: {notice:?}");
            });
        }
        assert!(repo.wrote().is_empty(), "the refusal queued nothing");
    }

    #[gpui::test]
    fn a_tracked_file_s_addition_hunk_travels_even_with_no_old_numbers(cx: &mut TestAppContext) {
        // The shape `[diff] context = 0` makes of an insertion mid-file:
        // every line an addition, no old number anywhere — which looks like
        // a creation and is not one. Status says the file is tracked work,
        // so the hunk synthesizes and goes to git; only status can tell
        // this apart from fresh.txt, and nothing else was asked.
        let raw = "\
diff --git a/added.txt b/added.txt
--- a/added.txt
+++ b/added.txt
@@ -3,0 +4,2 @@
+added one
+added two
";
        let (shell, repo) = addition_diff_shell(cx, raw);
        wire_runner(&shell, cx);

        shell.update(cx, |shell, cx| shell.run_command("diff.stage-hunk", cx));
        let notice = shell.read_with(cx, |shell, _| {
            shell.notice.as_ref().map(|n| n.text().to_string())
        });
        assert_ne!(
            notice.as_deref(),
            Some("that hunk adds a new file — stage or unstage it whole from the files pane"),
            "the numbers alone must not classify this a creation"
        );
        // The verb submits; the write lands when the queue drains, as in
        // every window.
        pump_write(&shell, cx);
        let wrote = repo.wrote();
        assert_eq!(wrote.len(), 1, "notice was {notice:?}");
        assert!(
            wrote[0].starts_with("stage-patch "),
            "the hunk went to git: {:?}",
            wrote[0]
        );
    }

    #[gpui::test]
    fn hunk_verbs_off_a_diff_pane_are_answered_like_unknown_commands(cx: &mut TestAppContext) {
        let (shell, _repo, _handle) = tree_shell(cx, Status::default());

        shell.update(cx, |shell, cx| shell.run_command("diff.stage-hunk", cx));
        shell.read_with(cx, |shell, _| {
            assert_eq!(
                shell.notice.as_ref().map(Notice::text),
                Some("diff.stage-hunk is not supported here")
            );
        });
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

    // ------------------------------------------------- the history verbs

    #[gpui::test]
    fn soft_and_mixed_resets_ask_twice_too(cx: &mut TestAppContext) {
        // A reset that silently shortened the commit list read as data loss
        // to the one person whose opinion of this UI counts, so every strength
        // asks the same question hard does: arm, say it in the band, spend.
        let (shell, repo) = history_shell(cx);
        let target = "0".repeat(40);

        // The question is the asking: `g` opens it — nothing written, the
        // band says what is being asked — and `s` is the answer, soft.
        shell.update(cx, |shell, cx| shell.run_command("commits.reset-menu", cx));
        std::thread::sleep(Duration::from_millis(50));
        shell.read_with(cx, |shell, _| {
            assert!(
                repo.wrote().is_empty(),
                "opening the question wrote nothing"
            );
            assert!(
                shell
                    .notice
                    .as_deref()
                    .unwrap_or_default()
                    .contains("reset to abc000? s soft"),
                "the question went unsaid: {:?}",
                shell.notice
            );
        });
        shell.update(cx, |shell, cx| shell.run_command("commits.reset-soft", cx));
        pump_write(&shell, cx);
        assert_eq!(repo.wrote(), vec![format!("reset --soft {target}")]);

        // The answer spent the question: the mode is gone, so `m` outside a
        // standing question reaches nothing — `g` must open again first.
        shell.update(cx, |shell, cx| shell.run_command("commits.reset-mixed", cx));
        std::thread::sleep(Duration::from_millis(50));
        shell.read_with(cx, |shell, _| {
            assert_eq!(repo.wrote().len(), 1, "no strength fires without its g");
            // And the orphaned letter does not execute and does not ask:
            // the asking is `g`'s, and a letter that opened a question by
            // itself would be two presses deciding a hard reset.
            assert!(
                shell
                    .notice
                    .as_deref()
                    .unwrap_or_default()
                    .contains("no reset is being asked"),
                "the orphaned strength went unsaid: {:?}",
                shell.notice
            );
        });
        shell.update(cx, |shell, cx| shell.run_command("commits.reset-menu", cx));
        shell.update(cx, |shell, cx| shell.run_command("commits.reset-mixed", cx));
        pump_write(&shell, cx);
        assert_eq!(
            repo.wrote(),
            vec![
                format!("reset --soft {target}"),
                format!("reset --mixed {target}")
            ],
            "the second press of each strength is the yes"
        );
    }

    #[gpui::test]
    fn a_hard_reset_asks_twice_then_rides_the_pump(cx: &mut TestAppContext) {
        let (shell, repo) = history_shell(cx);
        let view = match shell.read_with(cx, |shell, _| shell.active().cloned()) {
            Some(Screen::Commits { view, .. }) => view,
            _ => panic!("no commits pane"),
        };

        // `g` opens the question; nothing has run and the band asks, naming
        // the three answers — `h` among them, captured only while the
        // question stands.
        shell.update(cx, |shell, cx| shell.run_command("commits.reset-menu", cx));
        std::thread::sleep(Duration::from_millis(50));
        shell.read_with(cx, |shell, _| {
            assert!(repo.wrote().is_empty(), "nothing was reset yet");
            assert!(
                shell
                    .notice
                    .as_deref()
                    .unwrap_or_default()
                    .contains("reset to abc000? s soft · m mixed · h hard"),
                "the question went unsaid: {:?}",
                shell.notice
            );
        });
        view.read_with(cx, |v, _| {
            assert_eq!(v.armed_sha(), Some("0".repeat(40)));
        });

        // `h` answers it — and the whole production path runs: job queued by
        // dispatch, drained by the same pump the window runs, generation
        // bumped, panes re-acquired. The asking was g; the letter is the yes.
        shell.update(cx, |shell, cx| shell.run_command("commits.reset-hard", cx));
        pump_write(&shell, cx);
        assert_eq!(
            repo.wrote(),
            vec![format!("reset --hard {}", "0".repeat(40))]
        );
        shell.read_with(cx, |shell, _| assert!(shell.generation.get() > 0));
    }

    #[gpui::test]
    fn a_cursor_move_disarms_a_hard_reset_before_any_yes_can_land(cx: &mut TestAppContext) {
        let (shell, repo) = history_shell(cx);
        shell.update(cx, |shell, cx| shell.run_command("commits.reset-menu", cx));
        shell.update(cx, |shell, cx| shell.run_command("commits.reset-hard", cx));
        pump_write(&shell, cx);
        assert_eq!(
            repo.wrote(),
            vec![format!("reset --hard {}", "0".repeat(40))]
        );

        // The keyboard moves off the answered row — without a command in
        // between, exactly as a wheel does it. The question dies with the
        // cursor; the mode stack has not heard yet.
        shell.update(cx, |shell, cx| {
            let Some(Screen::Commits { view, .. }) = shell.active() else {
                panic!("no commits pane");
            };
            let host = Rc::new(Host::new());
            view.update(cx, |v, _| v.run_view("view.down", &host));
        });
        // The stale mode still resolves `h` — and it may not fire and may
        // not ask: the question it answered is gone, and the asking is g's.
        shell.update(cx, |shell, cx| shell.run_command("commits.reset-hard", cx));
        std::thread::sleep(Duration::from_millis(50));
        shell.read_with(cx, |shell, _| {
            assert_eq!(
                repo.wrote().len(),
                1,
                "a stale yes reached a different commit: {:?}",
                repo.wrote()
            );
            assert!(
                shell
                    .notice
                    .as_deref()
                    .unwrap_or_default()
                    .contains("no reset is being asked"),
                "{:?}",
                shell.notice
            );
        });
    }

    // ---------------------------------------------- the rebase rewrites

    /// Five straight-line commits, newest first, with shas readable in
    /// assertions: `"00…"·40` at HEAD down to `"44…"·40` at the root, each
    /// commit's parent exactly the next one's sha — the straight line
    /// [`gitten_core::rebase::compose`] demands.
    fn linear_chain() -> Vec<Commit> {
        let sha = |k: u8| format!("{:02x}", k).repeat(20);
        (0..5u8)
            .map(|k| Commit {
                sha: sha(k),
                short: format!("abc0{k}"),
                parents: match k {
                    4 => Vec::new(),
                    _ => vec![sha(k + 1)],
                }
                .into_boxed_slice(),
                author: "".into(),
                timestamp: 0,
                subject: format!("s{k}"),
            })
            .collect()
    }

    /// A commits pane over a repository whose `log` answers with that same
    /// straight line — the pair the rewrite verbs compose from.
    fn rebase_shell(cx: &mut TestAppContext) -> (gpui::Entity<DevShell>, Arc<RecordingRepo>) {
        let calls = Arc::default();
        let repo = Arc::new(RecordingRepo::new(Arc::clone(&calls)));
        repo.serve_log(linear_chain());
        let handle: gitten_git::Handle = repo.clone();
        let shell = shell(None, cx);
        shell.update(cx, |shell, cx| {
            let view = cx.new(|_| Commits::new(linear_chain(), Rc::new(Host::new())));
            cx.set_global(config::Active(Rc::new(Host::new())));
            shell.panes.register(
                "commits",
                Screen::commits(view, Source::Fixtures, Generation::default(), "~/src"),
            );
            shell.sync_modes(cx);
            shell.repo = Some((PathBuf::from("/recorded"), handle));
        });
        (shell, repo)
    }

    #[gpui::test]
    fn squash_up_asks_twice_then_rides_the_pump_with_a_whole_plan(cx: &mut TestAppContext) {
        let (shell, repo) = rebase_shell(cx);

        // First press on HEAD: asked, not acted.
        shell.update(cx, |shell, cx| shell.run_command("commits.squash-up", cx));
        std::thread::sleep(Duration::from_millis(50));
        shell.read_with(cx, |shell, _| {
            assert!(repo.wrote().is_empty());
            assert!(
                shell
                    .notice
                    .as_deref()
                    .unwrap_or_default()
                    .contains("squash abc00 into its parent?"),
                "{:?}",
                shell.notice
            );
        });

        // Second press composes the plan and queues the job through the
        // production dispatch: folding HEAD into its parent replays the
        // parent first (a plan may not open with a squash) and sits on the
        // grandparent's sha.
        shell.update(cx, |shell, cx| shell.run_command("commits.squash-up", cx));
        pump_write(&shell, cx);
        assert_eq!(
            repo.wrote(),
            vec![format!(
                "rebase onto {} with plan pick {}\nsquash {}\n",
                "02".repeat(20),
                "01".repeat(20),
                "00".repeat(20)
            )]
        );
        shell.read_with(cx, |shell, _| assert!(shell.generation.get() > 0));
    }

    #[gpui::test]
    fn drop_composes_a_plan_that_omits_only_the_selected_commit(cx: &mut TestAppContext) {
        let (shell, repo) = rebase_shell(cx);

        // The keyboard moves to the second-newest commit…
        shell.update(cx, |shell, cx| {
            let Some(Screen::Commits { view, .. }) = shell.active() else {
                panic!("no commits pane");
            };
            let host = Rc::new(Host::new());
            view.update(cx, |v, _| v.run_view("view.down", &host));
        });

        // …and two presses drop exactly it: the plan sits on its parent,
        // replays everything newer, and carries no line for the dropped
        // commit itself.
        for _ in 0..2 {
            shell.update(cx, |shell, cx| shell.run_command("commits.drop-commit", cx));
        }
        pump_write(&shell, cx);
        assert_eq!(
            repo.wrote(),
            vec![format!(
                "rebase onto {} with plan pick {}\n",
                "02".repeat(20),
                "00".repeat(20)
            )]
        );
    }

    #[gpui::test]
    fn a_merge_under_the_keyboard_refuses_in_words_and_queues_nothing(cx: &mut TestAppContext) {
        let calls = Arc::default();
        let repo = Arc::new(RecordingRepo::new(Arc::clone(&calls)));
        let mut merged = linear_chain();
        merged[1] = Commit {
            parents: vec!["03".repeat(20), "ee".repeat(20)].into_boxed_slice(),
            ..merged[1].clone()
        };
        repo.serve_log(merged.clone());
        let handle: gitten_git::Handle = repo.clone();
        let shell = shell(None, cx);
        shell.update(cx, |shell, cx| {
            let view = cx.new(|_| Commits::new(merged, Rc::new(Host::new())));
            cx.set_global(config::Active(Rc::new(Host::new())));
            shell.panes.register(
                "commits",
                Screen::commits(view, Source::Fixtures, Generation::default(), "~/src"),
            );
            shell.sync_modes(cx);
            shell.repo = Some((PathBuf::from("/recorded"), handle));
        });

        // The keyboard moves onto the merge itself, then arm, spend, refuse:
        // the merge would be flattened, said in words, with no job queued
        // behind the sentence.
        shell.update(cx, |shell, cx| {
            let Some(Screen::Commits { view, .. }) = shell.active() else {
                panic!("no commits pane");
            };
            let host = Rc::new(Host::new());
            view.update(cx, |v, _| v.run_view("view.down", &host));
        });
        for _ in 0..2 {
            shell.update(cx, |shell, cx| shell.run_command("commits.drop-commit", cx));
        }
        std::thread::sleep(Duration::from_millis(50));
        shell.read_with(cx, |shell, _| {
            assert!(repo.wrote().is_empty(), "{:?}", repo.wrote());
            assert!(
                shell
                    .notice
                    .as_deref()
                    .unwrap_or_default()
                    .contains("flatten"),
                "{:?}",
                shell.notice
            );
        });
    }

    /// A branches pane with `main` under HEAD and `other` beside it — the
    /// aim `commits.rebase-onto` takes from the pane the keyboard is over.
    fn rebase_branches_shell(
        cx: &mut TestAppContext,
    ) -> (gpui::Entity<DevShell>, Arc<RecordingRepo>) {
        let calls = Arc::default();
        let repo = Arc::new(RecordingRepo::new(Arc::clone(&calls)));
        let handle: gitten_git::Handle = repo.clone();
        let shell = shell(None, cx);
        shell.update(cx, |shell, cx| {
            let prepared = crate::views::branches::prepare(
                vec![branch_ref("main", true), branch_ref("other", false)],
                Vec::new(),
                None,
                &gitten_core::theme::Theme::default(),
                "test",
            );
            let label = prepared.label.clone();
            let view = cx.new(|_| crate::views::branches::Branches::from_prepared(prepared));
            cx.set_global(config::Active(Rc::new(Host::new())));
            shell.panes.register(
                "branches",
                Screen::branches(view, Generation::default(), label),
            );
            shell.sync_modes(cx);
            shell.repo = Some((PathBuf::from("/recorded"), handle));
        });
        (shell, repo)
    }

    #[gpui::test]
    fn rebase_onto_asks_twice_then_moves_this_branch_onto_the_selection(cx: &mut TestAppContext) {
        let (shell, repo) = rebase_branches_shell(cx);

        // The keyboard walks past the heading and past main (HEAD's own
        // row) onto `other`.
        shell.update(cx, |shell, cx| {
            let host = Rc::new(Host::new());
            for _ in 0..5 {
                let under = match shell.active() {
                    Some(Screen::Branches { view, .. }) => view.read(cx).cursor_text(),
                    _ => panic!("no branches pane"),
                };
                if under == "other" {
                    break;
                }
                shell.active().unwrap().run("view.down", &host, None, cx);
            }
            assert_eq!(
                match shell.active() {
                    Some(Screen::Branches { view, .. }) => view.read(cx).cursor_text(),
                    _ => unreachable!(),
                },
                "other",
                "the keyboard never reached the branch"
            );
        });

        shell.update(cx, |shell, cx| shell.run_command("commits.rebase-onto", cx));
        std::thread::sleep(Duration::from_millis(50));
        shell.read_with(cx, |shell, _| {
            assert!(repo.wrote().is_empty());
            assert!(
                shell
                    .notice
                    .as_deref()
                    .unwrap_or_default()
                    .contains("rebase this branch onto other?"),
                "{:?}",
                shell.notice
            );
        });

        shell.update(cx, |shell, cx| shell.run_command("commits.rebase-onto", cx));
        pump_write(&shell, cx);
        assert_eq!(repo.wrote(), vec!["rebase onto other"]);
        shell.read_with(cx, |shell, _| assert!(shell.generation.get() > 0));
    }

    #[gpui::test]
    fn abort_and_continue_reach_the_queue_by_their_own_names(cx: &mut TestAppContext) {
        // The way out of a stranded rebase is two named commands, reachable
        // whatever pane holds the keyboard — repository-level verbs, like
        // push and pull. No selection read, no confirmation dance: both
        // only mean something while git holds a state, and git's own answer
        // says so when there is none.
        let (shell, repo) = history_shell(cx);

        shell.update(cx, |shell, cx| shell.run_command("rebase.abort", cx));
        pump_write(&shell, cx);
        assert_eq!(repo.wrote(), vec!["rebase abort"]);
        shell.read_with(cx, |shell, _| assert!(shell.generation.get() > 0));

        shell.update(cx, |shell, cx| shell.run_command("rebase.continue", cx));
        pump_write(&shell, cx);
        assert_eq!(repo.wrote(), vec!["rebase abort", "rebase continue"]);
        shell.read_with(cx, |shell, _| assert!(shell.generation.get() > 1));
    }

    #[gpui::test]
    fn revert_lands_the_inverse_through_the_pump_without_asking(cx: &mut TestAppContext) {
        let (shell, repo) = history_shell(cx);
        shell.update(cx, |shell, cx| shell.run_command("commits.revert", cx));
        pump_write(&shell, cx);
        assert_eq!(repo.wrote(), vec![format!("revert {}", "0".repeat(40))]);
        shell.read_with(cx, |shell, _| assert!(shell.generation.get() > 0));
    }

    #[gpui::test]
    fn cherry_pick_rides_the_pump_and_refuses_a_detached_head(cx: &mut TestAppContext) {
        // Same terms as revert: nothing existing moves, so no confirmation
        // dance — the keypress is the job, through production dispatch.
        let (shell, repo) = history_shell(cx);
        shell.update(cx, |shell, cx| shell.run_command("commits.cherry-pick", cx));
        pump_write(&shell, cx);
        assert_eq!(
            repo.wrote(),
            vec![format!("cherry-pick {}", "0".repeat(40))]
        );
        shell.read_with(cx, |shell, _| {
            assert!(shell
                .notice
                .as_deref()
                .unwrap_or_default()
                .contains("picked"));
        });

        // Detached HEAD refuses before anything is queued: a pick lands on
        // *the current branch*, and detached means there is none.
        repo.detach();
        shell.update(cx, |shell, cx| shell.run_command("commits.cherry-pick", cx));
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(repo.wrote().len(), 1, "nothing was queued");
        shell.read_with(cx, |shell, _| {
            assert!(
                shell
                    .notice
                    .as_deref()
                    .unwrap_or_default()
                    .contains("detached"),
                "the refusal went unsaid: {:?}",
                shell.notice
            );
        });
    }

    #[gpui::test]
    fn a_new_tag_opens_the_field_and_the_accepted_text_becomes_the_job(cx: &mut TestAppContext) {
        let (shell, repo) = history_shell(cx);

        shell.update(cx, |shell, cx| shell.run_command("commits.new-tag", cx));
        shell.read_with(cx, |shell, _| {
            assert!(shell.input.is_some(), "no field opened");
            assert_eq!(shell.modes.top(), input::MODE, "the field owns the keys");
            assert!(matches!(shell.prompt, Some(super::Prompt::TagName { .. })));
        });

        // Typed text, as the platform would have left it; the real accept
        // path carries it into the job aimed at the row captured at open.
        shell.update(cx, |shell, cx| {
            let field = cx.new(|cx| input::Input::new("new tag", "tag name", "v1", cx));
            shell.open_input(field, cx);
            shell.prompt = Some(super::Prompt::TagName {
                target: "commits".into(),
                at: "0".repeat(40),
            });
        });
        shell.update(cx, |shell, cx| shell.run_command("input.accept", cx));
        pump_write(&shell, cx);
        assert_eq!(repo.wrote(), vec![format!("tag v1 at {}", "0".repeat(40))]);
        shell.read_with(cx, |shell, _| assert!(shell.generation.get() > 0));

        // An empty accept refuses beside the field and queues nothing.
        shell.update(cx, |shell, cx| shell.run_command("commits.new-tag", cx));
        shell.update(cx, |shell, cx| shell.run_command("input.accept", cx));
        shell.read_with(cx, |shell, _| {
            assert!(shell.input.is_none());
            assert!(
                shell
                    .notice
                    .as_ref()
                    .map(Notice::text)
                    .unwrap_or_default()
                    .contains("name"),
                "the refusal went unsaid: {:?}",
                shell.notice
            );
        });
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(repo.wrote().len(), 1, "the empty accept queued nothing");

        // Padding around the name is field noise, not part of it: what is
        // queued is the trimmed name.
        shell.update(cx, |shell, cx| {
            let field = cx.new(|cx| input::Input::new("new tag", "tag name", " v2 ", cx));
            shell.open_input(field, cx);
            shell.prompt = Some(super::Prompt::TagName {
                target: "commits".into(),
                at: "0".repeat(40),
            });
        });
        shell.update(cx, |shell, cx| shell.run_command("input.accept", cx));
        pump_write(&shell, cx);
        let expected = format!("tag v2 at {}", "0".repeat(40));
        assert_eq!(
            repo.wrote().last().map(String::as_str),
            Some(expected.as_str()),
            "the padding never reached git"
        );
    }

    #[gpui::test]
    fn a_conflicted_revert_says_what_git_said_and_shows_what_it_left(cx: &mut TestAppContext) {
        // Both panes over one repository: the commits pane to aim revert
        // from — registered last, so it holds the keyboard — and the status
        // pane that has to show what the refusal left behind.
        let calls = Arc::default();
        let repo = Arc::new(RecordingRepo::new(Arc::clone(&calls)));
        repo.arm_conflict();
        let handle: gitten_git::Handle = repo.clone();
        let shell = shell(None, cx);
        let files = shell.update(cx, |shell, cx| {
            cx.set_global(config::Active(Rc::new(Host::new())));
            let files = cx.new(|_| {
                crate::views::files::Files::from_prepared(crate::views::files::prepare(
                    Status::default(),
                    "r",
                ))
            });
            shell.panes.register(
                "files",
                Screen::files(files.clone(), Generation::default(), "files"),
            );
            let commits = cx.new(|_| Commits::new(search_history(), Rc::new(Host::new())));
            shell.panes.register(
                "commits",
                Screen::commits(commits, Source::Fixtures, Generation::default(), "~/src"),
            );
            shell.sync_modes(cx);
            shell.repo = Some((PathBuf::from("/recorded"), handle));
            files
        });

        shell.update(cx, |shell, cx| shell.run_command("commits.revert", cx));
        pump_until(&shell, cx, |shell| shell.error.is_some());
        cx.run_until_parked();

        shell.read_with(cx, |shell, _| {
            assert!(
                shell
                    .error
                    .as_deref()
                    .unwrap_or_default()
                    .contains("could not revert"),
                "git's own words reached the band: {:?}",
                shell.error
            );
        });
        files.read_with(cx, |f, _| {
            assert_eq!(
                f.paths_in(crate::views::files::Section::Conflicts),
                vec![gitten_core::status::PathBytes::from("poem.txt")],
                "the unmerged path the revert left is on screen"
            );
        });
    }

    #[gpui::test]
    fn a_conflicted_cherry_pick_says_what_git_said_and_shows_what_it_left(cx: &mut TestAppContext) {
        // The same shape as the conflicted revert: the pick refuses with
        // git's own words in the band, and the status pane re-acquires
        // through the drain_jobs failure arm — a refusal is not proof the
        // repository stood still, and an unmerged path nobody can see is a
        // question nobody can answer.
        let calls = Arc::default();
        let repo = Arc::new(RecordingRepo::new(Arc::clone(&calls)));
        repo.arm_conflict();
        let handle: gitten_git::Handle = repo.clone();
        let shell = shell(None, cx);
        let files = shell.update(cx, |shell, cx| {
            cx.set_global(config::Active(Rc::new(Host::new())));
            let files = cx.new(|_| {
                crate::views::files::Files::from_prepared(crate::views::files::prepare(
                    Status::default(),
                    "r",
                ))
            });
            shell.panes.register(
                "files",
                Screen::files(files.clone(), Generation::default(), "files"),
            );
            let commits = cx.new(|_| Commits::new(search_history(), Rc::new(Host::new())));
            shell.panes.register(
                "commits",
                Screen::commits(commits, Source::Fixtures, Generation::default(), "~/src"),
            );
            shell.sync_modes(cx);
            shell.repo = Some((PathBuf::from("/recorded"), handle));
            files
        });

        shell.update(cx, |shell, cx| shell.run_command("commits.cherry-pick", cx));
        pump_until(&shell, cx, |shell| shell.error.is_some());
        cx.run_until_parked();

        shell.read_with(cx, |shell, _| {
            assert!(
                shell
                    .error
                    .as_deref()
                    .unwrap_or_default()
                    .contains("could not apply"),
                "git's own words reached the band: {:?}",
                shell.error
            );
        });
        files.read_with(cx, |f, _| {
            assert_eq!(
                f.paths_in(crate::views::files::Section::Conflicts),
                vec![gitten_core::status::PathBytes::from("poem.txt")],
                "the unmerged path the pick left is on screen"
            );
        });
    }

    #[gpui::test]
    fn a_stranded_pick_is_walked_out_by_its_own_commands_not_rebases(cx: &mut TestAppContext) {
        // A conflicted pick leaves git holding the question under
        // CHERRY_PICK_HEAD. Rebase's abort/continue answer that state with
        // git's "no rebase in progress" — true and useless — so the way out
        // is the pair beside the pick key, dispatching to the pick's own
        // verbs.
        let (shell, repo) = history_shell(cx);

        // Abort: the refused pick's question, put back where it started.
        repo.arm_conflict();
        shell.update(cx, |shell, cx| shell.run_command("commits.cherry-pick", cx));
        pump_until(&shell, cx, |shell| shell.error.is_some());
        assert_eq!(
            repo.wrote(),
            vec![format!("cherry-pick {}", "0".repeat(40))]
        );
        let generation = shell.read_with(cx, |shell, _| shell.generation.get());
        shell.update(cx, |shell, cx| {
            shell.run_command("commits.cherry-pick-abort", cx)
        });
        pump_write(&shell, cx);
        assert_eq!(
            repo.wrote().last().map(String::as_str),
            Some("cherry-pick abort"),
            "the pick's own abort ran"
        );
        shell.read_with(cx, |shell, _| {
            assert!(shell.generation.get() > generation);
        });

        // Continue: the conflict resolved by hand, the pick lands as its
        // own commit — the verb runs, the band re-acquires.
        repo.clear_conflict();
        let generation = shell.read_with(cx, |shell, _| shell.generation.get());
        shell.update(cx, |shell, cx| {
            shell.run_command("commits.cherry-pick-continue", cx)
        });
        pump_write(&shell, cx);
        assert_eq!(
            repo.wrote().last().map(String::as_str),
            Some("cherry-pick continue")
        );
        shell.read_with(cx, |shell, _| {
            assert!(shell.generation.get() > generation);
            assert!(
                shell
                    .notice
                    .as_deref()
                    .unwrap_or_default()
                    .contains("continued"),
                "the finish said so: {:?}",
                shell.notice
            );
        });
    }

    #[gpui::test]
    fn the_history_verbs_say_so_outside_the_commits_pane(cx: &mut TestAppContext) {
        let (shell, _repo, _handle) = files_shell(cx);
        for command in [
            "commits.reset-soft",
            "commits.reset-hard",
            "commits.revert",
            "commits.cherry-pick",
            "commits.new-tag",
        ] {
            shell.update(cx, |shell, cx| shell.run_command(command, cx));
        }
        shell.read_with(cx, |shell, _| {
            assert!(shell
                .notice
                .as_deref()
                .unwrap_or_default()
                .contains("not supported here"));
        });
    }

    #[gpui::test]
    fn amend_opens_the_field_and_the_accepted_text_becomes_the_job(cx: &mut TestAppContext) {
        let (shell, repo, _handle) = files_shell(cx);

        shell.update(cx, |shell, cx| shell.run_command("files.amend", cx));
        shell.read_with(cx, |shell, _| {
            assert!(shell.input.is_some(), "no field opened");
            assert_eq!(shell.modes.top(), input::MODE, "the field owns the keys");
        });

        // Typed text, as the platform would have left it; the real accept
        // path carries it into the job.
        shell.update(cx, |shell, cx| {
            let field = cx.new(|cx| input::Input::new("amend", "amend message", "rewritten", cx));
            shell.open_input(field, cx);
            shell.prompt = Some(super::Prompt::AmendMessage);
        });
        shell.update(cx, |shell, cx| shell.run_command("input.accept", cx));
        pump_write(&shell, cx);
        assert_eq!(repo.wrote(), vec!["amend rewritten"]);
    }

    #[gpui::test]
    fn an_empty_amend_refuses_at_accept_without_submitting_anything(cx: &mut TestAppContext) {
        let (shell, repo, _handle) = files_shell(cx);
        shell.update(cx, |shell, cx| shell.run_command("files.amend", cx));
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
        std::thread::sleep(Duration::from_millis(100));
        assert!(repo.wrote().is_empty());
    }

    // ------------------------------------------------------- the branch verbs

    /// A shell with the branches pane registered over the recording
    /// repository — `feature` and `main` local (main under HEAD),
    /// `origin/main` remote. The keyboard starts on `feature`, row 1.
    fn branches_shell(
        cx: &mut TestAppContext,
    ) -> (
        gpui::Entity<DevShell>,
        Arc<RecordingRepo>,
        gitten_git::Handle,
    ) {
        let calls = Arc::default();
        let repo = Arc::new(RecordingRepo::new(Arc::clone(&calls)));
        let handle: gitten_git::Handle = repo.clone();
        let shell = shell(None, cx);
        shell.update(cx, |shell, cx| {
            let prepared = crate::views::branches::prepare(
                vec![branch_ref("feature", false), branch_ref("main", true)],
                vec![gitten_core::refs::RemoteBranch {
                    remote: gitten_core::refs::RefName::from("origin"),
                    branch: gitten_core::refs::RefName::from("main"),
                    commit: "0123456789abcdef0123456789abcdef01234567".into(),
                }],
                Some(gitten_core::refs::HeadState::Branch {
                    name: gitten_core::refs::RefName::from("main"),
                    commit: None,
                }),
                &gitten_core::theme::Theme::default(),
                "r",
            );
            // The pane opens past the `LOCAL` heading, on feature: the
            // cursor never rests on a heading, so no step is needed.
            let view = cx.new(|_| crate::views::branches::Branches::from_prepared(prepared));
            shell.panes.register(
                "branches",
                Screen::branches(view, Generation::default(), "r · 2 local · 1 remote"),
            );
            shell.sync_modes(cx);
            shell.repo = Some((PathBuf::from("/recorded"), handle.clone()));
            cx.set_global(config::Active(Rc::new(Host::new())));
        });
        (shell, repo, handle)
    }

    /// Walks the keyboard onto a named branch row.
    #[track_caller]
    fn onto(shell: &gpui::Entity<DevShell>, name: &str, cx: &mut TestAppContext) {
        shell.update(cx, |shell, cx| {
            let Some(Screen::Branches { view, .. }) = shell.active() else {
                panic!("branches pane lost");
            };
            let host = Rc::new(Host::new());
            view.update(cx, |b, _| {
                b.run_view("view.top", &host);
                loop {
                    let hit = b.current().is_some_and(|t| match t {
                        crate::views::branches::Target::Local(n) => n.as_bytes() == name.as_bytes(),
                        crate::views::branches::Target::Remote { remote, branch } => {
                            format!("{}/{}", remote.to_string_lossy(), branch.to_string_lossy())
                                == name
                        }
                        _ => false,
                    });
                    if hit || !b.run_view("view.down", &host) {
                        break;
                    }
                }
            });
        });
    }

    #[gpui::test]
    fn checkout_rides_the_rails_and_every_pane_reacquires(cx: &mut TestAppContext) {
        let (shell, repo, _handle) = branches_shell(cx);
        // The keyboard is already on feature; space's command is what a key
        // resolves to, run through named dispatch like every other verb.
        shell.update(cx, |shell, cx| shell.run_command("branches.checkout", cx));

        pump_write(&shell, cx);
        assert_eq!(
            repo.wrote(),
            vec!["checkout feature"],
            "the write ran off-thread against the real handle"
        );
        // The chain that matters: success bumped the generation, and every
        // repository pane — the working tree included — re-acquired against
        // the new HEAD through refresh_stale, no test help.
        shell.read_with(cx, |shell, _| {
            assert!(shell.generation > Generation::default(), "no bump");
            for screen in shell.panes.iter() {
                match screen {
                    Screen::Files { generation, .. }
                    | Screen::Branches { generation, .. }
                    | Screen::Stashes { generation, .. } => {
                        assert_eq!(generation.get(), shell.generation);
                    }
                    Screen::Commits { .. }
                    | Screen::Diff { .. }
                    | Screen::Status { .. }
                    | Screen::Custom(_) => {}
                }
            }
        });
    }

    // ------------------------------------------------------- the sync verbs

    /// The tracking line the branches pane draws for main right now.
    #[track_caller]
    fn main_upstream_line(shell: &gpui::Entity<DevShell>, cx: &TestAppContext) -> String {
        shell.read_with(cx, |shell, cx| {
            let Some(Screen::Branches { view, .. }) = shell.active() else {
                panic!("branches pane lost");
            };
            view.read(cx)
                .row_slice()
                .iter()
                .find_map(|r| match r {
                    crate::views::branches::Row::Local(l) if l.name.as_bytes() == b"main" => {
                        l.counts.clone()
                    }
                    _ => None,
                })
                .unwrap_or_default()
                .to_string()
        })
    }

    #[gpui::test]
    fn the_sync_keys_run_through_the_pump_and_the_counts_follow(cx: &mut TestAppContext) {
        let (shell, repo, _handle) = branches_shell(cx);

        // f first: the remote has moved on its own, and fetch is how this
        // side learns. The verb runs off-thread through the real queue...
        shell.update(cx, |shell, cx| shell.run_command("repo.fetch", cx));
        pump_write(&shell, cx);
        assert_eq!(repo.wrote(), vec!["fetch --all"]);
        assert_eq!(repo.counts(), (0, 1));
        // ...the finish names itself in the band...
        shell.read_with(cx, |shell, _| {
            assert_eq!(shell.notice.as_ref().map(Notice::text), Some("fetched"));
        });
        // ...and the branches panel re-acquired through the production
        // drain_jobs rails: one behind, drawn where the counts live.
        let line = main_upstream_line(&shell, cx);
        assert!(line.contains("↓1"), "{line}");

        // p closes that distance git's way — fast-forward or nothing.
        shell.update(cx, |shell, cx| shell.run_command("repo.pull", cx));
        pump_write(&shell, cx);
        assert_eq!(repo.wrote(), vec!["fetch --all", "pull"]);
        assert_eq!(repo.counts(), (0, 0));
        shell.read_with(cx, |shell, _| {
            assert_eq!(shell.notice.as_ref().map(Notice::text), Some("pulled"));
        });
        let line = main_upstream_line(&shell, cx);
        assert!(!line.contains('↓'), "in sync reads as a bare name: {line}");

        // A local commit opens the other direction; P spends it. Origin
        // stands in only because the fake tracks nothing — here it tracks.
        *repo.distance.lock().unwrap() = (1, 0);
        shell.update(cx, |shell, cx| shell.run_command("repo.push", cx));
        pump_write(&shell, cx);
        assert_eq!(
            repo.wrote(),
            vec!["fetch --all", "pull", "push origin main"]
        );
        assert_eq!(repo.counts(), (0, 0));
        shell.read_with(cx, |shell, _| {
            assert_eq!(
                shell.notice.as_ref().map(Notice::text),
                Some("pushed origin main")
            );
        });
        let line = main_upstream_line(&shell, cx);
        assert!(!line.contains('↑'), "{line}");
    }

    #[gpui::test]
    fn the_sync_keys_say_so_where_they_cannot_act(cx: &mut TestAppContext) {
        // A fixture has no repository behind any pane; every sync key says
        // so instead of pretending.
        let shell = shell(None, cx);
        shell.update(cx, |shell, cx| shell.run_command("repo.push", cx));
        shell.read_with(cx, |shell, _| {
            assert!(shell
                .notice
                .as_deref()
                .unwrap_or_default()
                .contains("fixture"));
        });

        // Detached HEAD is a place, not a branch: nothing under HEAD to aim
        // a push at, refused before any job exists.
        let (shell, repo, _handle) = branches_shell(cx);
        repo.detach();
        shell.update(cx, |shell, cx| shell.run_command("repo.push", cx));
        assert_eq!(repo.wrote(), Vec::<String>::new());
        shell.read_with(cx, |shell, _| {
            assert!(shell
                .notice
                .as_deref()
                .unwrap_or_default()
                .contains("detached"));
        });
    }

    #[gpui::test]
    fn checkout_says_so_where_it_cannot_act(cx: &mut TestAppContext) {
        // Over another pane entirely.
        let (shell, repo, _handle) = branches_shell(cx);
        shell.update(cx, |shell, _| {
            shell.panes.focus(0); // the commits root
        });
        shell.update(cx, |shell, cx| shell.run_command("branches.checkout", cx));
        shell.read_with(cx, |shell, _| {
            assert!(shell
                .notice
                .as_deref()
                .unwrap_or_default()
                .contains("not supported here"));
        });
        assert!(repo.wrote().is_empty());

        // On the detached row itself: a place, not a branch to move to.
        let (shell, repo, _handle) = branches_shell(cx);
        shell.update(cx, |shell, cx| {
            let host = config::host(cx);
            let Some(Screen::Branches { view, .. }) = shell.active() else {
                panic!("branches pane lost");
            };
            view.update(cx, |b, _| {
                b.replace_prepared(
                    crate::views::branches::prepare(
                        vec![branch_ref("main", false)],
                        Vec::new(),
                        Some(gitten_core::refs::HeadState::Detached {
                            commit: "0123456789abcdef".into(),
                        }),
                        &gitten_core::theme::Theme::default(),
                        "",
                    ),
                    &host,
                );
            });
        });
        shell.update(cx, |shell, cx| {
            // The fixture above carries one local branch, so row 1 is it;
            // back up to the detached row itself.
            let host = Rc::new(Host::new());
            let Some(Screen::Branches { view, .. }) = shell.active() else {
                panic!("branches pane lost");
            };
            view.update(cx, |b, _| {
                b.run_view("view.top", &host); // the detached row
            });
        });
        shell.update(cx, |shell, cx| shell.run_command("branches.checkout", cx));
        shell.read_with(cx, |shell, _| {
            assert!(shell
                .notice
                .as_deref()
                .unwrap_or_default()
                .contains("detached"));
        });
        assert!(repo.wrote().is_empty());

        // No repository behind the window at all.
        let (shell, repo, _handle) = branches_shell(cx);
        shell.update(cx, |shell, _| shell.repo = None);
        shell.update(cx, |shell, cx| shell.run_command("branches.checkout", cx));
        shell.read_with(cx, |shell, _| {
            assert!(shell
                .notice
                .as_deref()
                .unwrap_or_default()
                .contains("fixture"));
        });
        assert!(repo.wrote().is_empty());
    }

    #[gpui::test]
    fn new_branch_opens_a_field_and_the_accepted_name_becomes_the_job(cx: &mut TestAppContext) {
        let (shell, repo, _handle) = branches_shell(cx);

        shell.update(cx, |shell, cx| shell.run_command("branches.new", cx));
        shell.read_with(cx, |shell, _| {
            assert!(shell.input.is_some(), "no field opened");
            assert!(matches!(
                shell.prompt,
                Some(super::Prompt::BranchName { .. })
            ));
        });

        // Typed as the platform would leave it, then accepted through the
        // real accept path.
        let typed = shell.read_with(cx, |shell, _| shell.input.clone().unwrap());
        typed.update(cx, |field, cx| field.replace(None, "hotfix", cx));
        shell.update(cx, |shell, cx| shell.run_command("input.accept", cx));
        pump_write(&shell, cx);
        assert_eq!(repo.wrote(), vec!["create hotfix"]);
        shell.read_with(cx, |shell, _| assert!(shell.input.is_none()));
    }

    #[gpui::test]
    fn rename_pre_fills_the_rows_own_name_and_accepts_a_replacement(cx: &mut TestAppContext) {
        let (shell, repo, _handle) = branches_shell(cx);
        // The keyboard sits on feature.

        shell.update(cx, |shell, cx| shell.run_command("branches.rename", cx));
        let typed = shell.read_with(cx, |shell, _app| shell.input.clone().unwrap());
        assert_eq!(
            typed.read_with(cx, |field, _| field.value().to_string()),
            "feature",
            "the field started from the row's own name"
        );

        typed.update(cx, |field, cx| field.replace(None, "f2", cx));
        shell.update(cx, |shell, cx| shell.run_command("input.accept", cx));
        pump_write(&shell, cx);
        assert_eq!(
            repo.wrote(),
            vec!["rename feature f2"],
            "the old bytes travelled with the job"
        );

        // A remote row refuses to be renamed before any field opens.
        let (shell, repo, _handle) = branches_shell(cx);
        onto(&shell, "origin/main", cx);
        shell.update(cx, |shell, cx| shell.run_command("branches.rename", cx));
        shell.read_with(cx, |shell, _| {
            assert!(shell.input.is_none(), "a prompt opened over a remote");
            assert!(shell
                .notice
                .as_deref()
                .unwrap_or_default()
                .contains("local"));
        });
        assert!(repo.wrote().is_empty());

        // An empty accept is refused without submitting anything — proven by
        // the production pump running dry, not by hoping the worker waited.
        let (shell, repo, _handle) = branches_shell(cx);
        shell.update(cx, |shell, cx| shell.run_command("branches.new", cx));
        shell.update(cx, |shell, cx| shell.run_command("input.accept", cx));
        pump_until(&shell, cx, |_| true);
        assert!(repo.wrote().is_empty(), "an unnamed branch was submitted");
        shell.read_with(cx, |shell, _| {
            assert!(shell
                .notice
                .as_ref()
                .map(Notice::text)
                .unwrap_or_default()
                .contains("name"));
        });
    }

    #[gpui::test]
    fn a_non_utf8_rename_opens_empty_rather_than_pre_filling_mojibake(cx: &mut TestAppContext) {
        // The one way the lossy pre-fill could corrupt something: accept on
        // an untouched field would rename a legal Latin-1 branch to its own
        // U+FFFD spelling — a different refname. Empty is what honesty
        // looks like here; the bytes still ride the prompt for whoever
        // actually types a replacement.
        let calls = Arc::default();
        let repo = Arc::new(RecordingRepo::new(Arc::clone(&calls)));
        let handle: gitten_git::Handle = repo.clone();
        let shell = shell(None, cx);
        shell.update(cx, |shell, cx| {
            let prepared = crate::views::branches::prepare(
                vec![gitten_core::refs::Branch {
                    name: gitten_core::refs::RefName::from_bytes(b"f\xe9ature"),
                    ..branch_ref("unused", false)
                }],
                Vec::new(),
                None,
                &gitten_core::theme::Theme::default(),
                "r",
            );
            let view = cx.new(|_| crate::views::branches::Branches::from_prepared(prepared));
            view.update(cx, |b, _| {
                b.run_view("view.down", &Rc::new(Host::new())); // onto f<latin1-e>ature
            });
            shell.panes.register(
                "branches",
                Screen::branches(view, Generation::default(), "branches"),
            );
            shell.sync_modes(cx);
            shell.repo = Some((PathBuf::from("/recorded"), handle.clone()));
            cx.set_global(config::Active(Rc::new(Host::new())));
        });

        shell.update(cx, |shell, cx| shell.run_command("branches.rename", cx));
        let typed = shell.read_with(cx, |shell, _app| shell.input.clone().unwrap());
        assert_eq!(
            typed.read_with(cx, |field, _| field.value().to_string()),
            "",
            "the mojibake was never offered as if it were the name"
        );

        // Typing a replacement still aims at the real bytes.
        typed.update(cx, |field, cx| field.replace(None, "ok", cx));
        shell.update(cx, |shell, cx| shell.run_command("input.accept", cx));
        pump_write(&shell, cx);
        // RecordingRepo logs through from_utf8_lossy; the *bytes* are proven
        // at the verb layer — here the old side is what matters: it is the
        // branch that was under the keyboard.
        assert_eq!(repo.wrote(), vec!["rename f\u{FFFD}ature ok"]);
    }

    #[gpui::test]
    fn delete_asks_once_then_acts_and_remote_rows_refuse_outright(cx: &mut TestAppContext) {
        let (shell, repo, _handle) = branches_shell(cx);

        // First press on feature: asked, in the band, nothing queued.
        shell.update(cx, |shell, cx| shell.run_command("branches.delete", cx));
        shell.read_with(cx, |shell, _| {
            assert_eq!(
                shell.notice.as_ref().map(Notice::text),
                Some("delete branch feature? press again to confirm"),
                "{:?}",
                shell.notice
            );
        });
        // The production pump runs dry: nothing was ever submitted, and the
        // drain through the real path is what proves it.
        pump_until(&shell, cx, |_| true);
        assert!(repo.wrote().is_empty());

        // Second press on the same row spends the arm.
        shell.update(cx, |shell, cx| shell.run_command("branches.delete", cx));
        pump_write(&shell, cx);
        assert_eq!(repo.wrote(), vec!["delete feature"]);
        // A remote row refuses before arming — deliberate scope tonight:
        // a tracking ref is its remote's shadow, pruned by fetch.
        let (shell, repo, _handle) = branches_shell(cx);
        onto(&shell, "origin/main", cx);
        shell.update(cx, |shell, cx| shell.run_command("branches.delete", cx));
        shell.read_with(cx, |shell, _| {
            assert!(shell
                .notice
                .as_deref()
                .unwrap_or_default()
                .contains("remote"));
        });
        assert!(repo.wrote().is_empty());

        // And a cursor move between presses disarms, exactly as discard does.
        let (shell, repo, _handle) = branches_shell(cx);
        shell.update(cx, |shell, cx| shell.run_command("branches.delete", cx)); // arm feature
        onto(&shell, "main", cx); // move
        shell.update(cx, |shell, cx| shell.run_command("branches.delete", cx));
        shell.read_with(cx, |shell, _| {
            assert!(
                shell
                    .notice
                    .as_ref()
                    .map(Notice::text)
                    .unwrap_or_default()
                    .contains("main"),
                "the question moved to the new row: {:?}",
                shell.notice
            );
        });
        assert!(repo.wrote().is_empty(), "the stale arm never fired");
    }

    #[gpui::test]
    fn branches_focus_reaches_its_registered_pane(cx: &mut TestAppContext) {
        let (shell, _repo, _handle) = branches_shell(cx);
        // Registration focused the branches pane; go back to the root first.
        shell.update(cx, |shell, cx| shell.run_command("pane.prev", cx));
        shell.read_with(cx, |shell, _| {
            assert_ne!(shell.active_view_name(), "branches");
        });

        // Named dispatch — the same path the `3` key resolves through.
        shell.update(cx, |shell, cx| shell.run_command("branches.focus", cx));
        shell.read_with(cx, |shell, app| {
            assert_eq!(shell.panes.focused_index(), 1);
            assert_eq!(shell.modes.top(), "branches");
            assert_eq!(
                shell.active_label(app).as_ref(),
                "r · 2 local · 1 remote",
                "the label carries the count"
            );
        });
    }

    #[gpui::test]
    fn a_refresh_reanchors_the_keyboard_on_its_branch(cx: &mut TestAppContext) {
        let (shell, _repo, _handle) = branches_shell(cx);
        onto(&shell, "feature", cx);

        // A refresh that adds a branch above shifts every row; the keyboard
        // follows its branch, not its index.
        shell.update(cx, |shell, cx| {
            let Some(Screen::Branches { view, .. }) = shell.active() else {
                panic!("branches pane lost");
            };
            view.update(cx, |b, cx| {
                let prepared = crate::views::branches::prepare(
                    vec![
                        branch_ref("aaa", false),
                        branch_ref("feature", false),
                        branch_ref("main", true),
                    ],
                    Vec::new(),
                    Some(gitten_core::refs::HeadState::Branch {
                        name: gitten_core::refs::RefName::from("main"),
                        commit: None,
                    }),
                    &gitten_core::theme::Theme::default(),
                    "",
                );
                b.replace_prepared(prepared, &config::host(cx));
            });
        });

        shell.read_with(cx, |shell, cx| {
            let Some(Screen::Branches { view, .. }) = shell.active() else {
                panic!("branches pane lost");
            };
            match view.read(cx).current() {
                Some(crate::views::branches::Target::Local(name)) => {
                    assert_eq!(name.as_bytes(), b"feature", "the anchor held");
                }
                other => panic!("still on a branch, got {other:?}"),
            }
        });

        // And a refresh whose branch vanished clamps instead of lying.
        shell.update(cx, |shell, cx| {
            let Some(Screen::Branches { view, .. }) = shell.active() else {
                panic!("branches pane lost");
            };
            view.update(cx, |b, cx| {
                b.replace_prepared(
                    crate::views::branches::prepare(
                        vec![branch_ref("main", true)],
                        Vec::new(),
                        None,
                        &gitten_core::theme::Theme::default(),
                        "",
                    ),
                    &config::host(cx),
                );
            });
        });
        shell.read_with(cx, |shell, cx| {
            let Some(Screen::Branches { view, .. }) = shell.active() else {
                panic!("branches pane lost");
            };
            assert!(view.read(cx).current().is_some(), "clamped onto a row");
        });
    }
}

#[cfg(test)]
mod title_tests {
    use super::{repo_title, section_floor, section_height, SECTION_MIN_H};
    use std::path::Path;

    #[test]
    fn a_repository_under_home_is_spelled_from_tilde_and_cut_at_its_name() {
        assert_eq!(
            repo_title(
                Path::new("/Users/me/src/plait"),
                Some(Path::new("/Users/me"))
            ),
            ("~/src/".to_string(), "plait".to_string())
        );
    }

    #[test]
    fn a_repository_elsewhere_keeps_its_whole_parent() {
        assert_eq!(
            repo_title(Path::new("/srv/git/plait"), Some(Path::new("/Users/me"))),
            ("/srv/git/".to_string(), "plait".to_string())
        );
        assert_eq!(
            repo_title(Path::new("/srv/git/plait"), None),
            ("/srv/git/".to_string(), "plait".to_string())
        );
    }

    #[test]
    fn home_itself_and_the_root_still_have_a_bright_half() {
        assert_eq!(
            repo_title(Path::new("/Users/me"), Some(Path::new("/Users/me"))),
            (String::new(), "~".to_string())
        );
        assert_eq!(
            repo_title(Path::new("/"), None),
            (String::new(), "/".to_string())
        );
    }

    #[test]
    fn a_section_is_its_header_plus_its_rows_and_never_shorter_than_one_row() {
        assert_eq!(
            section_height(0),
            section_height(1),
            "the empty line is a row"
        );
        assert_eq!(
            section_height(5) - section_height(1),
            4.0 * crate::graph::ROW_H
        );
        assert!(section_height(2) >= SECTION_MIN_H, "the floor is two rows");
    }

    #[test]
    fn the_floor_never_exceeds_the_natural_height() {
        assert_eq!(
            section_floor(0),
            section_height(0),
            "an empty section is not padded"
        );
        assert_eq!(section_floor(1), section_height(1));
        assert_eq!(section_floor(2), SECTION_MIN_H);
        assert_eq!(
            section_floor(40),
            SECTION_MIN_H,
            "a long list still squeezes to two rows"
        );
    }
}
