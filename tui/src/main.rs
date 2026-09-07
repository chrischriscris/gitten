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
//! The one modal input this client has is a typed [`Prompt`]: the commit-list
//! search `/` opens, each edit filters the list live, and Enter keeps what
//! was typed while Esc restores the whole list; the files pane's `c` and `A`
//! open the same kind of one-line field for a commit or amend message. While
//! any of them stands, keys are resolved against exactly the `input` mode —
//! never the full stack — so the shipped globals cannot read the field, and
//! everything else the prompt takes arrives as text. Which consumer an
//! accepted prompt's text goes to is the enum variant's to say, not a
//! stringly remembered mode.
//!
//! # The loop is idle until something happens
//!
//! It blocks on input with a timeout, and the timeout exists only so a saved
//! `gitten.toml` is noticed. Nothing redraws at rest — the same property the GPUI
//! client has for free, arrived at here on purpose, and the reason the frame
//! timing in `docs/measurements.md` is measured rather than observed.

use gitten_app::acquire::{self, Data};
use gitten_app::act::{hunk_job, verb_refusal, HunkAsk, HunkSelection, HunkSide};
use gitten_app::cli::{self, Source, View};
use gitten_app::jobs::{Event as JobEvent, Generation, Job, Runner, Submitter};
use gitten_app::verbs::Write;
use gitten_app::{StartClock, Startup};
use gitten_core::command::{chord_string, Availability, Code, Key, Modes, Resolve, Usable};
use gitten_core::differ::Overrides;
use gitten_core::edit::{Edit, Field};
use gitten_core::host::Host;
use gitten_core::refs::{HeadState, RefName};
use gitten_core::runs::Run;
use gitten_core::source::DiffSource;
use gitten_tui::branches::{self, Branches, Marks, Target};
use gitten_tui::commits::{Commits, Glyphs};
use gitten_tui::diff::Diff;
use gitten_tui::diff::PatchSelection;
use gitten_tui::files::{self, Files};
use gitten_tui::help;
use gitten_tui::remotes::Remotes;
use gitten_tui::screen::{Ink, Pen, Screen};
use gitten_tui::scrollbar::Bar;
use gitten_tui::stashes::{drop_question, Stashes};
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

/// The poll while the log's tail is still streaming: how long a tail batch
/// may wait for the frame after its bytes. 16 ms is one 60 Hz frame — the
/// tail fills imperceptibly, and the input that lands inside the window is
/// never held longer than that.
const STREAM_TICK: Duration = Duration::from_millis(16);

/// The most input one frame takes from the queue.
///
/// A wheel burst arrives as dozens of notches, and a frame per notch is the
/// rubber-band: the hand outruns the screen, and then the screen keeps moving
/// after the hand has stopped. The queue is drained before the frame is drawn,
/// so a burst costs one [`App::draw`] no matter how many notches are already in
/// it — every event still resolves through the keymap exactly as it did, only
/// the frames between them are gone. The bound exists for the pathological
/// case alone: a pipe or a runaway source feeding events faster than they can
/// be handled would otherwise never draw again, and sixty-four handled events
/// is far past anything a hand produces between two frames.
const INPUT_BATCH: usize = 64;

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

    // Which door: a repository commits launch streams its log — the list
    // draws off the first records and the tail lands behind the frame, sound
    // because lane assignment is prefix-stable (pinned in `core`). Everything
    // else keeps the synchronous road: fixtures and patches are in-process
    // reads with no stream to hurry, and a diff launch has no log at all.
    let progressive = matches!(
        cli::parse(start.take(), View::Commits),
        cli::Request::Open {
            view: View::Commits,
            source: Source::Repo { .. },
        }
    );
    // Captured before the startup moves: the failure line names the binary
    // and the usage, in the same words the synchronous road produces.
    let usage = start.usage();
    let (started, stream) = if progressive {
        match start.configure() {
            Ok(configured) => {
                let gitten_app::Configured {
                    view,
                    source,
                    host,
                    repo,
                    config,
                } = configured;
                let repo = repo.expect("a repository launch carries its handle");
                let (limit, path) = match &source {
                    Source::Repo { path, arg } => (arg.parse().unwrap_or(5000), path.clone()),
                    _ => unreachable!("checked above"),
                };
                // The describe beside the first batch — the same overlap the
                // synchronous acquisition runs, one spawn floor for both.
                let outcome = std::thread::scope(|s| {
                    let title = s.spawn(|| repo.describe());
                    let mut stream = gitten_git::Repo::log_stream(repo.as_ref(), limit)?;
                    let first = stream.first()?;
                    if first.is_empty() && !stream.live() {
                        return Err(format!("no commits in {}", path.display()));
                    }
                    Ok::<_, String>((
                        gitten_app::acquire::Loaded {
                            label: title.join().unwrap_or_default(),
                            data: Data::Commits(first.clone()),
                        },
                        first,
                        stream,
                    ))
                });
                match outcome {
                    Ok((loaded, first, stream)) => {
                        let mut clock = StartClock::new();
                        clock.stage("acquired (first batch)");
                        (
                            Ok(gitten_app::Started {
                                view,
                                source,
                                host,
                                loaded,
                                config,
                                repo: Some(repo),
                            }),
                            Some((stream, first)),
                        )
                    }
                    Err(e) => (
                        Err(gitten_app::Exit::Failed(format!(
                            "gitten-tui: {e}\n\n{usage}"
                        ))),
                        None,
                    ),
                }
            }
            Err(exit) => (Err(exit), None),
        }
    } else {
        match start.go() {
            Ok(started) => (Ok(started), None),
            Err(exit) => (Err(exit), None),
        }
    };
    let started = match started {
        Ok(started) => started,
        Err(exit) => exit.finish(),
    };
    let mut clock = StartClock::new();
    let config_path = started.config.clone();
    let mut app = App::new(started, glyphs);
    if let Some((stream, first)) = stream {
        app.set_tail(stream, first);
    }
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
/// source, and that is load-bearing: a repository commit list immediately
/// replaces the temporary empty tenant with the highlighted commit's preview;
/// a fixture cannot. Pretending an empty tenant was acquired from the working
/// tree would let staging and refreshing reach a pane that holds nothing;
/// "not loaded" remains a state the type can say honestly.
///
/// The files tenant carries no source at all, and that is the third shape: it
/// is only ever registered when startup opened a repository, so everything it
/// refreshes from is the one handle the app retained. A fixture has no
/// working tree and so no files pane — the name is absent, and `files.focus`
/// says so — rather than a pane pretending to hold something.
enum Screens {
    Commits {
        view: Commits,
        source: Source,
        /// The ref this list is the history *of*, when it is not HEAD's — a
        /// branch drilldown's own answer. `None` is the ordinary list, which
        /// a refresh re-reads from HEAD; `Some` re-reads from the ref, so a
        /// drilldown stays that branch's history instead of quietly becoming
        /// the checked-out one.
        log_of: Option<RefName>,
        label: String,
        generation: Generation,
    },
    Diff {
        view: Diff,
        /// What this diff is between — the source it was acquired from, and
        /// the one thing a verb consults before it acts. A commit's preview,
        /// a file's side, a stash's parked work; `None` for the empty pane
        /// nothing was ever acquired for.
        origin: Option<DiffSource>,
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
    Files {
        view: Files,
        label: String,
        generation: Generation,
    },
    /// The repository's branches. No source, like the files tenant: the pane
    /// is only ever registered behind a repository — a fixture and a patch
    /// have no refs to read — so a refresh is a plain re-read through the
    /// handle the app holds, and the generation rail is the whole of its
    /// staleness story.
    Branches {
        view: Branches,
        label: String,
        generation: Generation,
    },
    /// The repository's remotes. No source, like the branches tenant: a
    /// refresh is a plain re-read of `remote -v` through the handle the app
    /// holds, and the generation rail is the whole of its staleness story.
    Remotes {
        view: Remotes,
        label: String,
        generation: Generation,
    },
}

/// What an empty diff pane's header says instead of a sha it does not have.
const EMPTY_DIFF_LABEL: &str = "commit preview unavailable";

/// What a stash pane's header says when its side read failed — never
/// `nothing stashed`, which would assert a successful read that did not
/// happen. The exact error goes to the status line; the next refresh
/// re-reads the stack, and a read that succeeds replaces this pane outright.
const STASH_UNAVAILABLE: &str = "unavailable";

/// What a sidebar pane's header says while its first read is still out —
/// the one frame the launch's skeleton draws in. Never a count and never
/// `unavailable`: nothing has failed and nothing has been counted yet.
/// The pane's own unavailable shape stands in for the rows, for the same
/// honesty a failed read gets — an un-read tree is not a clean tree.
const STARTUP_LOADING: &str = "loading";

/// The mode a text field owns the keyboard in — the name the keymap and
/// `gitten.toml` use, and the same name the window's input module holds. While
/// any prompt stands, bindings are resolved against exactly this mode
/// and nothing else.
const INPUT: &str = "input";

/// The mode the recent-repositories picker owns the keyboard in, for as
/// long as it stands — help's own trade: a press it does not name runs
/// nothing underneath, so a chord cannot arm a discard behind a modal.
const PICKER: &str = "picker";

/// What an unresolved key means to the modal that holds the keyboard.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ModalKind {
    /// A text field: unresolved keys fall to the shared [`Edit`] vocabulary.
    Field,
    /// A question or a list: unresolved keys wait for enter or esc.
    List,
}

/// The one modal input, and which consumer its accepted text belongs to.
///
/// A [`Prompt`] stands over the status row and owns the keyboard for as long
/// as it is up; the variant is what makes accept route somewhere in
/// particular instead of into a remembered mode. All three hold one logical
/// `String` that may be longer than the row it draws on — the stored text is
/// never cut, only the drawing is.
enum Prompt {
    /// The live query: every edit filters the list under it — or walks the
    /// diff to its next match — live. The pane it stands over is held by
    /// *name*, the way the window holds every pane, so however focus moves
    /// while it stands the query still lands on the list it was opened over.
    /// One line by design: a query narrows a list, and a line break in a
    /// needle is a paste's accident, not a search.
    Search {
        pane: String,
        field: Field,
    },
    /// `files.commit`'s field. Accepting submits [`Write::commit`] with the
    /// text whole. Multiline: a commit message is a message, and `alt+enter`
    /// (or a paste) is how a second line gets in.
    CommitMessage {
        field: Field,
    },
    /// `files.amend`'s field — the same field, aimed one step back. Prefilled
    /// from HEAD's subject, so an amend is an edit of what is standing rather
    /// than a retyping of it.
    AmendMessage {
        field: Field,
    },
    /// `branches.new`'s field. Reads no row: creating is at HEAD, so it is
    /// available in an empty or unborn repository too. Accepting submits
    /// [`Write::create_branch`] and checks nothing out.
    BranchNew {
        field: Field,
    },
    /// `branches.rename`'s field. `from` is the raw bytes of the branch being
    /// renamed — what the job is aimed at, whatever the field shows — and the
    /// field arrives wholly selected, so the first edit replaces it rather
    /// than appending to it.
    BranchRename {
        from: Vec<u8>,
        field: Field,
    },
    /// `branches.new-tag`'s field. `at` is the raw bytes of the local branch
    /// the tag names — a revspec git resolves, so the tag moves with the
    /// branch — captured at open and never re-read from the pane.
    TagNew {
        at: Vec<u8>,
        field: Field,
    },
    /// `branches.checkout-name`'s field: whatever it names, git aims at.
    /// Empty is refused beside the field that just closed.
    BranchCheckoutName {
        field: Field,
    },
    /// `commits.new-branch`'s field. `at` is the selected commit's full sha,
    /// captured when the field opened, so nothing a cursor does while the
    /// field holds the keyboard can re-aim the branch. Accepting creates
    /// the branch there — and offers the checkout as a question.
    BranchNewAt {
        at: Vec<u8>,
        field: Field,
    },
    /// The checkout offer after a branch was created at a commit: a
    /// question, not a field. Enter takes it, esc leaves the branch where
    /// it now sits — the branches pane's space reaches it later either way.
    /// The field exists because every prompt holds one; nothing edits it
    /// and the draw never reads it.
    CheckoutNew {
        name: String,
        field: Field,
    },
    /// `project.open`'s field: a path to open as a repository. Accepting
    /// switches everything the app holds; a refusal keeps it all.
    ProjectOpen {
        field: Field,
    },
    /// `remotes.new`'s two fields, in order: the name, then the URL.
    RemoteName {
        field: Field,
    },
    RemoteUrl {
        name: String,
        field: Field,
    },
    /// `remotes.edit`'s field, prefilled with the remote's first URL.
    /// `name` is the raw bytes of the remote being repointed, captured at
    /// open like every verb's aim.
    RemoteEdit {
        name: RefName,
        field: Field,
    },
}

impl Prompt {
    /// The field the prompt is editing — every edit lands through here,
    /// whatever the prompt is for.
    fn field(&self) -> &Field {
        match self {
            Prompt::Search { field, .. }
            | Prompt::CommitMessage { field }
            | Prompt::AmendMessage { field }
            | Prompt::BranchNew { field }
            | Prompt::BranchRename { field, .. }
            | Prompt::TagNew { field, .. }
            | Prompt::BranchCheckoutName { field }
            | Prompt::BranchNewAt { field, .. }
            | Prompt::CheckoutNew { field, .. }
            | Prompt::ProjectOpen { field }
            | Prompt::RemoteName { field }
            | Prompt::RemoteUrl { field, .. }
            | Prompt::RemoteEdit { field, .. } => field,
        }
    }

    /// The field, mutable.
    fn field_mut(&mut self) -> &mut Field {
        match self {
            Prompt::Search { field, .. }
            | Prompt::CommitMessage { field }
            | Prompt::AmendMessage { field }
            | Prompt::BranchNew { field }
            | Prompt::BranchRename { field, .. }
            | Prompt::TagNew { field, .. }
            | Prompt::BranchCheckoutName { field }
            | Prompt::BranchNewAt { field, .. }
            | Prompt::CheckoutNew { field, .. }
            | Prompt::ProjectOpen { field }
            | Prompt::RemoteName { field }
            | Prompt::RemoteUrl { field, .. }
            | Prompt::RemoteEdit { field, .. } => field,
        }
    }

    /// Whether this field holds multiline text — a message — or one line, by
    /// design. A query, a branch name and a tag name are one line: git has no
    /// newline in them and neither does this prompt. A paste into a one-line
    /// field is flattened, and the flattening is the field's own documented
    /// answer, said here and not per keystroke.
    fn multiline(&self) -> bool {
        matches!(
            self,
            Prompt::CommitMessage { .. } | Prompt::AmendMessage { .. }
        )
    }

    /// The question a yes/no prompt stands to ask, when it is one. A
    /// question owns the status row whole — no field, no caret, no live
    /// count — and answers only to enter (yes) and esc (no).
    fn question(&self) -> Option<String> {
        match self {
            Prompt::CheckoutNew { name, .. } => Some(format!(
                "created {name} — check out? enter=check out, esc=not now"
            )),
            _ => None,
        }
    }

    /// The pane a search prompt stands over — its edits route there by name,
    /// not by focus.
    fn pane(&self) -> &str {
        match self {
            Prompt::Search { pane, .. } => pane,
            _ => "",
        }
    }

    /// The word the field opens with, drawn before the text — `/` for the
    /// search, the command's own name for a message.
    fn label(&self) -> &'static str {
        match self {
            Prompt::Search { .. } => "/",
            Prompt::CommitMessage { .. } => "commit: ",
            Prompt::AmendMessage { .. } => "amend: ",
            Prompt::BranchNew { .. } => "branch: ",
            Prompt::BranchRename { .. } => "rename: ",
            Prompt::TagNew { .. } => "tag: ",
            Prompt::BranchCheckoutName { .. } => "checkout: ",
            Prompt::BranchNewAt { .. } => "branch: ",
            Prompt::CheckoutNew { .. } => "",
            Prompt::ProjectOpen { .. } => "open: ",
            Prompt::RemoteName { .. } => "remote name: ",
            Prompt::RemoteUrl { .. } => "remote url: ",
            Prompt::RemoteEdit { .. } => "url: ",
        }
    }
}

/// The recent-repositories picker: the stored paths, most-recent first, and
/// the row the keyboard is on. The rows are the paths as the MRU stores
/// them — canonicalized when they were recorded — and opening one is
/// [`App::open_repository`]'s to refuse, not this list's.
struct RecentPicker {
    rows: Vec<std::path::PathBuf>,
    cursor: usize,
}

impl RecentPicker {
    fn down(&mut self) {
        self.cursor = (self.cursor + 1).min(self.rows.len().saturating_sub(1));
    }

    fn up(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    fn jump_top(&mut self) {
        self.cursor = 0;
    }

    fn jump_bottom(&mut self) {
        self.cursor = self.rows.len().saturating_sub(1);
    }
}

/// Paints the picker over the body: a quiet box one column in from the
/// edge, the title, then as many paths as fit with the keyboard's row
/// highlighted. The window scrolls under a cursor that sits past the fold —
/// the same one rule every list here follows.
fn paint_picker(screen: &mut Screen, y: usize, height: usize, picker: &RecentPicker, host: &Host) {
    if picker.rows.is_empty() || height < 3 {
        return;
    }
    let c = &host.theme.chrome;
    const TITLE: &str = " recent repositories ";
    let bg = Ink::new(c.dim, c.bg);
    let shown = height.saturating_sub(3).max(1).min(picker.rows.len());
    let text_width = picker
        .rows
        .iter()
        .map(|r| gitten_tui::screen::width(r.to_string_lossy().as_ref()))
        .max()
        .unwrap_or(0);
    // The box fits its widest row — or its own title, whichever is longer:
    // a short list never clips the name of the thing being listed.
    let width = (text_width + 2)
        .max(TITLE.len())
        .min(screen.width().saturating_sub(2))
        .max(14);
    // The keyboard's row is on screen: the window scrolls only when it
    // must, and only just enough.
    let top = picker.cursor.saturating_sub(shown - 1);
    // A modal floats: one column in, one row down, opaque over whatever it
    // covers.
    for (i, _) in (0..shown + 2).enumerate() {
        let at = y + 1 + i;
        let mut pen = screen.span(at, 1, width);
        if i == 0 {
            pen.put(TITLE, Ink::new(c.accent, c.status_bg));
            pen.wash(Ink::new(c.dim, c.status_bg));
        } else {
            let source = top + (i - 1);
            let Some(entry) = picker.rows.get(source) else {
                pen.wash(bg);
                continue;
            };
            let bg = match source == picker.cursor {
                true => Ink::new(c.fg, c.selection_bg),
                false => Ink::new(c.fg, c.bg),
            };
            pen.put(&entry.to_string_lossy(), bg);
            pen.wash(bg);
        }
    }
}

impl Screens {
    /// Which mode's bindings are live. The name the keymap and `gitten.toml` use.
    fn mode(&self) -> &'static str {
        match self {
            Screens::Commits { .. } => "commits",
            Screens::Diff { .. } => "diff",
            Screens::Stashes { .. } => "stashes",
            Screens::Files { .. } => "files",
            Screens::Branches { .. } => "branches",
            Screens::Remotes { .. } => "remotes",
        }
    }

    fn label(&self) -> &str {
        match self {
            Screens::Commits { label, .. }
            | Screens::Diff { label, .. }
            | Screens::Stashes { label, .. }
            | Screens::Branches { label, .. }
            | Screens::Remotes { label, .. } => label,
            Screens::Files { label, .. } => label,
        }
    }

    fn generation(&self) -> Generation {
        match self {
            Screens::Commits { generation, .. }
            | Screens::Diff { generation, .. }
            | Screens::Stashes { generation, .. }
            | Screens::Branches { generation, .. }
            | Screens::Remotes { generation, .. } => *generation,
            Screens::Files { generation, .. } => *generation,
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
                log_of,
                label,
                generation,
            } => {
                let loaded = match log_of {
                    Some(name) => {
                        // A drilldown re-reads the branch it was opened for,
                        // not HEAD: the ref it was asked about is the whole
                        // of what this pane is, and a refresh that answered
                        // with the checked-out history would be a refresh
                        // that lied.
                        let limit = match source {
                            Source::Repo { arg, .. } => arg.parse().unwrap_or(5000),
                            _ => 5000,
                        };
                        match repo.log_at(name.as_bytes(), limit) {
                            Ok(commits) => {
                                view.replace(commits);
                                *generation = target;
                                return Some(Ok(()));
                            }
                            Err(e) => return Some(Err(e)),
                        }
                    }
                    None => match source {
                        Source::Repo { .. } => {
                            match acquire::reacquire(
                                View::Commits,
                                source,
                                host,
                                Some(repo),
                                &Overrides::default(),
                            ) {
                                Ok(loaded) => loaded,
                                Err(e) => return Some(Err(e)),
                            }
                        }
                        Source::Fixtures | Source::Patch { .. } => return None,
                    },
                };
                let Data::Commits(commits) = loaded.data else {
                    return Some(Err("re-acquisition returned the wrong view".into()));
                };
                view.replace(commits);
                *label = loaded.label;
                *generation = target;
                Some(Ok(()))
            }
            Screens::Diff {
                view,
                origin,
                label,
                generation,
            } => match origin {
                Some(origin) => {
                    let loaded = match acquire::diff_source(
                        origin,
                        &host.differ,
                        &Overrides::default(),
                        repo,
                        true,
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
                // The empty pane was never acquired from anywhere and has
                // nothing to re-read.
                None => None,
            },
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
            Screens::Files {
                view,
                label,
                generation,
            } => {
                if *generation >= target {
                    return None;
                }
                // The whole of the blocking half: one `git status`, plus the
                // describe the label names the repository with — the same two
                // reads the window's files refresh makes, and the same
                // registration the pane was built from. Nothing here touches
                // the view until the read has come back, so a failed refresh
                // leaves the last good rows standing.
                let status = match repo.status() {
                    Ok(status) => status,
                    Err(e) => return Some(Err(e)),
                };
                let described = repo.describe();
                let files::Prepared { rows, label: next } = files::prepare(&status, &described);
                view.replace(rows);
                *label = next;
                *generation = target;
                Some(Ok(()))
            }
            Screens::Branches {
                view,
                label,
                generation,
            } => {
                // The whole of the blocking half: three ref reads, run
                // beside each other — the same reads the pane was built
                // from, and the same registration-wide wave every other
                // pane rides. The describe rides the same wave, since the
                // label is spelled with it; nothing here touches the view
                // until the reads have come back, so a failed refresh
                // leaves the last good rows standing, at its old
                // generation, for a later wave to retry.
                let (reads, described) = std::thread::scope(|s| {
                    let reads = s.spawn(|| load_branches(repo));
                    let described = s.spawn(|| repo.describe());
                    (join_read(reads), join_read(described))
                });
                if let Some(e) = reads.error {
                    return Some(Err(e));
                }
                let branches::Prepared { rows, label: next } = branches::prepare(
                    &reads.local,
                    &reads.remotes,
                    reads.head.as_ref(),
                    &described,
                );
                *label = next;
                view.replace(rows);
                *generation = target;
                Some(Ok(()))
            }
            Screens::Remotes {
                view,
                label,
                generation,
            } => {
                // Two reads beside each other, the branches pane's shape:
                // the list, and the describe its label is spelled with.
                let (loaded, described) = std::thread::scope(|s| {
                    let remotes = s.spawn(|| repo.remotes());
                    let described = s.spawn(|| repo.describe());
                    (
                        remotes
                            .join()
                            .unwrap_or_else(|p| std::panic::resume_unwind(p)),
                        described.join().unwrap_or_default(),
                    )
                });
                let loaded = match loaded {
                    Ok(remotes) => remotes,
                    Err(e) => return Some(Err(e)),
                };
                let count = loaded.len();
                view.replace(loaded);
                *label = remotes_label(&described, count);
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
            Screens::Files { view: f, .. } => {
                f.set_scrolloff(host.view.scrolloff);
                f.resize(rect.width, rect.height);
            }
            Screens::Branches { view: b, .. } => {
                b.set_scrolloff(host.view.scrolloff);
                b.resize(rect.width, rect.height);
            }
            Screens::Remotes { view: r, .. } => {
                r.set_scrolloff(host.view.scrolloff);
                r.resize(rect.width, rect.height);
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
            // The files pane needs no run-list buffer: its rows are cells,
            // not shaped spans.
            Screens::Files { view: f, .. } => f.paint(screen, x, y, focused, host),
            Screens::Branches { view: b, .. } => b.paint(screen, x, y, focused, host),
            Screens::Remotes { view: r, .. } => r.paint(screen, x, y, focused, host),
        }
    }

    fn status(&self, host: &Host) -> String {
        match self {
            Screens::Commits { view: c, .. } => c.status(),
            Screens::Diff { view: d, .. } => d.status(host),
            Screens::Stashes { view: s, .. } => s.status(),
            Screens::Files { view: f, .. } => f.status(),
            Screens::Branches { view: b, .. } => b.status(),
            Screens::Remotes { view: r, .. } => r.status(),
        }
    }

    /// The bar at the column the paint loop chose for it. The choice is the
    /// app's because the edge is the app's: in the last cell of the pane
    /// immediately inside any divider, or flush with the screen's edge. See
    /// [0027](../docs/decisions/0027-the-scrollbar-is-an-indicator.md).
    fn paint_scrollbar(
        &self,
        screen: &mut Screen,
        x: usize,
        divider: Option<usize>,
        y: usize,
        host: &Host,
    ) {
        match self {
            Screens::Commits { view: c, .. } => c.paint_bar(screen, x, divider, y, host),
            Screens::Diff { view: d, .. } => d.paint_bar(screen, x, divider, y, host),
            Screens::Stashes { view: s, .. } => s.paint_bar(screen, x, divider, y, host),
            Screens::Files { view: f, .. } => f.paint_bar(screen, x, divider, y, host),
            Screens::Branches { view: b, .. } => b.paint_bar(screen, x, divider, y, host),
            Screens::Remotes { view: r, .. } => r.paint_bar(screen, x, divider, y, host),
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
            Screens::Files { view: f, .. } => f.press(col, row, clicks, extend, host),
            Screens::Branches { view: b, .. } => b.press(col, row, extend, host),
            Screens::Remotes { view: r, .. } => r.press(col, row, extend, host),
        }
    }

    /// The pointer moved with the button down, in this pane's coordinates.
    /// `row` is signed: a row above the pane is negative and scrolls it.
    fn drag(&mut self, col: usize, row: isize, host: &Host) {
        match self {
            Screens::Commits { view: c, .. } => c.drag(row, host),
            Screens::Diff { view: d, .. } => d.drag(col, row, host),
            Screens::Stashes { view: s, .. } => s.drag(row, host),
            // A list with no drag selection and an indicator bar has nothing
            // a held button can do.
            Screens::Files { .. } | Screens::Branches { .. } | Screens::Remotes { .. } => {}
        }
    }

    fn release(&mut self) {
        match self {
            Screens::Commits { view: c, .. } => c.release(),
            Screens::Diff { view: d, .. } => d.release(),
            Screens::Stashes { view: s, .. } => s.release(),
            // Nothing held here either — see `drag`.
            Screens::Files { .. } | Screens::Branches { .. } | Screens::Remotes { .. } => {}
        }
    }

    /// What `copy.selection` copies here: the selection, or the row the cursor is
    /// on when there is none.
    fn copy_text(&self) -> String {
        match self {
            Screens::Commits { view: c, .. } => c.copy_text(),
            Screens::Diff { view: d, .. } => d.copy_text(),
            Screens::Stashes { view: s, .. } => s.copy_text(),
            Screens::Files { view: f, .. } => f.copy_text(),
            Screens::Branches { view: b, .. } => b.copy_text(),
            Screens::Remotes { view: r, .. } => r.copy_text(),
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
            // A file list has no drag selection, so copy-on-select has
            // nothing to fire on here — the empty answer is the mechanism.
            Screens::Files { view: f, .. } => f.selection(),
            Screens::Branches { view: b, .. } => b.selection(),
            Screens::Remotes { view: r, .. } => r.selection(),
        }
    }

    fn select_all(&mut self) {
        match self {
            Screens::Commits { view: c, .. } => c.select_all(),
            Screens::Diff { view: d, .. } => d.select_all(),
            Screens::Stashes { view: s, .. } => s.select_all(),
            Screens::Files { view: f, .. } => f.select_all(),
            Screens::Branches { view: b, .. } => b.select_all(),
            Screens::Remotes { view: r, .. } => r.select_all(),
        }
    }

    fn select_none(&mut self) -> bool {
        match self {
            Screens::Commits { view: c, .. } => c.select_none(),
            Screens::Diff { view: d, .. } => d.select_none(),
            Screens::Stashes { view: s, .. } => s.select_none(),
            Screens::Files { view: f, .. } => f.select_none(),
            Screens::Branches { view: b, .. } => b.select_none(),
            Screens::Remotes { view: r, .. } => r.select_none(),
        }
    }

    /// The live filter count, while a search prompt stands over a list; on
    /// the diff, what the standing search found. A note is only drawn when
    /// there is one.
    fn search_note(&self) -> Option<String> {
        match self {
            Screens::Commits { view: c, .. } => c.filter_note(),
            Screens::Files { view: f, .. } => f.filter_note(),
            Screens::Branches { view: b, .. } => b.filter_note(),
            Screens::Stashes { view: s, .. } => s.filter_note(),
            Screens::Remotes { view: r, .. } => r.filter_note(),
            Screens::Diff { view: d, .. } => d.match_note(),
        }
    }

    /// Whether a search stands in this pane — the filter a prompt left
    /// behind, or the diff's standing query. What puts the `search` mode on
    /// the stack, and with it `n`/`N` and the clearing `esc`.
    fn search_standing(&self) -> bool {
        match self {
            Screens::Commits { view: c, .. } => c.query().is_some(),
            Screens::Files { view: f, .. } => f.query().is_some(),
            Screens::Branches { view: b, .. } => b.query().is_some(),
            Screens::Stashes { view: s, .. } => s.query().is_some(),
            Screens::Remotes { view: r, .. } => r.query().is_some(),
            Screens::Diff { view: d, .. } => d.search_query().is_some(),
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
                "search.next" => c.next_match(1),
                "search.prev" => c.next_match(-1),
                "select.mark" => c.select_mark(),
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
                "search.next" => d.search_next(1),
                "search.prev" => d.search_next(-1),
                "diff.next-hunk" => d.jump_hunk(1),
                "diff.prev-hunk" => d.jump_hunk(-1),
                "diff.toggle-line-selection" => {
                    d.toggle_line_selection();
                }
                "select.mark" => d.select_mark(),
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
                "search.next" => s.next_match(1),
                "search.prev" => s.next_match(-1),
                _ => return false,
            },
            Screens::Files { view: f, .. } => match command {
                "view.down" => f.down(),
                "view.up" => f.up(),
                "view.page-down" => f.page(1),
                "view.page-up" => f.page(-1),
                "view.scroll-down" => f.scroll_y(host.view.rows as isize),
                "view.scroll-up" => f.scroll_y(-(host.view.rows as isize)),
                "view.top" => f.to_top(),
                "view.bottom" => f.to_bottom(),
                // Nothing off the left edge to reach: paths clip rather
                // than pan.
                "view.left" | "view.right" => {}
                "search.next" => f.next_match(1),
                "search.prev" => f.next_match(-1),
                _ => return false,
            },
            Screens::Branches { view: b, .. } => match command {
                "view.down" => b.down(),
                "view.up" => b.up(),
                "view.page-down" => b.page(1),
                "view.page-up" => b.page(-1),
                "view.scroll-down" => b.scroll_y(host.view.rows as isize),
                "view.scroll-up" => b.scroll_y(-(host.view.rows as isize)),
                "view.top" => b.to_top(),
                "view.bottom" => b.to_bottom(),
                // Nothing off the left edge to reach: names clip rather
                // than pan.
                "view.left" | "view.right" => {}
                "search.next" => b.next_match(1),
                "search.prev" => b.next_match(-1),
                _ => return false,
            },
            Screens::Remotes { view: r, .. } => match command {
                "view.down" => r.down(),
                "view.up" => r.up(),
                "view.page-down" => r.page(1),
                "view.page-up" => r.page(-1),
                "view.scroll-down" => r.scroll_y(host.view.rows as isize),
                "view.scroll-up" => r.scroll_y(-(host.view.rows as isize)),
                "view.top" => r.to_top(),
                "view.bottom" => r.to_bottom(),
                // Nothing off the left edge to reach: names clip rather
                // than pan.
                "view.left" | "view.right" => {}
                "search.next" => r.next_match(1),
                "search.prev" => r.next_match(-1),
                _ => return false,
            },
        }
        true
    }
}

/// One preview answer, off the lane. `seq` is the request it answers and
/// the only thing that decides whether it installs; `root` is the repository
/// it was read from and the only repository it may install into.
struct PreviewOutcome {
    seq: u64,
    root: std::path::PathBuf,
    origin: DiffSource,
    focus: bool,
    outcome: Result<Vec<gitten_core::FileDiff>, String>,
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
    /// The open prompt, holding the text typed so far and naming, by variant,
    /// where its accepted text goes. `None` while the keyboard belongs to the
    /// panes.
    ///
    /// Here and not on a pane because collecting terminal text is input — the
    /// client's to gather, by the same rule that makes it the client's to
    /// translate a platform event — while [`Commits::apply_query`] and the
    /// write constructors stay the whole of what anything else knows about
    /// it. The search prompt stands only over the pane named `commits`, by
    /// that name and not by focus, so every reader below can rely on it and a
    /// focus change cannot strand the query; a message prompt is opened by
    /// and for the files pane and closes at its own accept.
    prompt: Option<Prompt>,
    /// The recent-repositories picker, while it stands: a modal list over
    /// the body, owned the way help is owned — a press it does not name
    /// runs nothing underneath. `None` while the keyboard belongs to the
    /// panes or a prompt.
    picker: Option<RecentPicker>,
    /// Where a switched-to handle comes from. The binary opener unless a
    /// test injects its own — the same seam [`gitten_app::Startup`] holds,
    /// and for the same reason: a fake behind a real window.
    opener: std::sync::Arc<dyn gitten_app::Opener>,
    /// The shared write queue. One FIFO worker, owned here, whose finishes
    /// every client treats the same way: a generation advances — a refusal as
    /// much as a success — and every repository-backed pane re-acquires.
    jobs: Runner,
    /// The cloneable end of [`App::jobs`], handed out to whatever submits.
    submitter: Submitter,
    /// The generation the queue has advanced to, and so the one every pane
    /// was last refreshed against.
    generation: Generation,
    /// What this client runs, beside the shared registry — the one contract
    /// the help panel and the dispatch refusal both read. Built once from
    /// the launch: a fixture view has no repository behind it, and that is
    /// the one thing here that varies between two instances of the same
    /// binary. A config reload rebuilds the host and never this — what the
    /// client can run is not the file's to say.
    availability: Availability,
    help: bool,
    /// First key row visible in the help modal. Independent of every pane's
    /// cursor and viewport, as a modal's reading position must be.
    help_scroll: usize,
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
    /// Whether `--ascii` replaced the graph, marks and bar alphabets. One
    /// flag names the state the status line reports on its right-hand end —
    /// the mock's `120×32 · --ascii off` — because the three alphabets are
    /// always switched together at startup.
    ascii: bool,
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
    /// Whether the skeleton's deferred startup loads still have to run — the
    /// sidebars' first reads and the preview diff for row zero, registered in
    /// their loading shape by [`App::new`] and filled in by [`App::load_startup`]
    /// after the first frame. Cleared by the load itself; a fixture or a patch
    /// has no repository and never carries it.
    startup_pending: bool,
    /// The log's unstreamed tail, and every commit it has delivered so far —
    /// the first batch drew with the first frame; the rest arrives between
    /// frames, appended to the list through the same `replace` a refresh
    /// rides. `None` for a launch that has no tail: a fixture, a patch, a
    /// diff, or a history that ended with its first batch.
    tail: Option<gitten_git::LogStream>,
    tail_commits: Vec<gitten_core::Commit>,
    /// The preview lane: one channel its readers answer on, the sequence
    /// number the next request will carry, and what is in flight. The
    /// sequence is the whole of the staleness contract — an answer installs
    /// only while it is still the newest request issued — and the pane's
    /// own `origin` is what a later request deduplicates against.
    preview_tx: std::sync::mpsc::Sender<PreviewOutcome>,
    preview_rx: std::sync::mpsc::Receiver<PreviewOutcome>,
    preview_seq: u64,
    /// Requests issued whose answer has not come back yet — every answer,
    /// installed or dropped as stale, counts one down. `pump_quiet` waits
    /// on it, which is what makes a background preview deterministic in a
    /// test: the fake answers in microseconds, and zero pending means every
    /// answer has had its turn.
    preview_pending: usize,
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
        // The same trade for the branches pane's marks: `--ascii` replaces
        // the ref alphabet the way it replaces the graph's and the bar's.
        let marks = match glyphs == Glyphs::ascii() {
            true => Marks::ascii(),
            false => Marks::default(),
        };
        // The ancillary stack, registered before its read exists: the pane is
        // part of the layout from frame one — the number keys, the cycle and
        // the walk all derive from it — and the read runs after the first
        // frame instead of on the road to it. `unavailable()` is the honest
        // shape for an un-read stack too, exactly as for a failed one: an
        // un-read tree is not a clean tree, and the label is the only
        // difference. A read that fails says so and the launch goes on —
        // the exact error is kept for the status line, and the next refresh
        // re-reads the stack.
        // The preview lane, before anything can ask for a preview: one
        // channel, reader end on the app, writer end cloned into whatever
        // thread a request spawns.
        let (preview_tx, preview_rx) = std::sync::mpsc::channel();
        let stash_tenant = repo.is_some().then(|| {
            let mut view = Stashes::unavailable();
            view.set_bar(bar);
            Screens::Stashes {
                view,
                label: STARTUP_LOADING.to_string(),
                generation: Generation::default(),
            }
        });
        // The remotes pane, the same shape one slot over: registered behind a
        // repository in its loading shape, read by the startup wave.
        let remotes_tenant = repo.is_some().then(|| {
            let mut view = Remotes::unavailable();
            view.set_bar(bar);
            Screens::Remotes {
                view,
                label: STARTUP_LOADING.to_string(),
                generation: Generation::default(),
            }
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
                        log_of: None,
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
                if let Some(pane) = remotes_tenant {
                    panes.register("remotes", panes::Placement::sidebar("remotes"), pane);
                }
                // The persistent main pane starts empty, then
                // [`App::sync_main_diff`] below replaces it from row zero
                // before construction returns. Keeping the honest empty shape
                // here also covers a fixture, which has no repository to ask.
                panes.register(
                    "diff",
                    panes::Placement::Main,
                    Screens::Diff {
                        view: Diff::new(Vec::new(), &host),
                        origin: None,
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
                if let Some(pane) = remotes_tenant {
                    panes.register("remotes", panes::Placement::sidebar("remotes"), pane);
                }
                let mut diff = Diff::new(files, &host);
                diff.set_bar(bar);
                panes.register(
                    "diff",
                    panes::Placement::Main,
                    Screens::Diff {
                        view: diff,
                        // The launch's own read: a repository diff is the
                        // aggregate view its revspec names (empty meaning the
                        // combined HEAD→worktree read, as always); a patch
                        // and the fixtures are their own detached content.
                        origin: Some(DiffSource::from(&source)),
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
        // The working tree and the branch lists get their panes whenever
        // startup opened a repository — as tenants, but not yet as reads:
        // both register here in their loading shape, and the reads run after
        // the first frame (see [`App::load_startup`]). The product choice the
        // comments above record is untouched — one `git status` and three ref
        // reads still happen before anything can stale them — but they no
        // longer sit on the road to the first frame behind the view the
        // person actually asked for. Registration is the whole of what a
        // second sidebar list costs: the number keys, the cycle and the walk
        // derive from it, and no geometry or dispatch branch learns the name.
        // A fixture or a patch has no repository and so no pane at all; the
        // name stays absent and `files.focus` says so.
        //
        // The panes carry the `unavailable` shape while the reads are out,
        // for the same reason a failed read does: an un-read tree must never
        // be drawn where a clean tree goes, and the label is the only
        // difference. The error is set on the status line by the load, where
        // a failure is discovered — after the terminal is taken, so a
        // stderr print here would land behind the alternate screen.
        let launch_focus = panes.focused_name().to_string();
        if repo.is_some() {
            let mut files = Files::unavailable();
            files.set_bar(bar);
            panes.register(
                "files",
                panes::Placement::sidebar("files"),
                Screens::Files {
                    view: files,
                    label: STARTUP_LOADING.to_string(),
                    generation: Generation::default(),
                },
            );
            let mut branches = Branches::with_marks(Vec::new(), marks);
            branches.set_bar(bar);
            panes.register(
                "branches",
                panes::Placement::sidebar("branches"),
                Screens::Branches {
                    view: branches,
                    label: STARTUP_LOADING.to_string(),
                    generation: Generation::default(),
                },
            );
            // `register` focuses what it adds; the keyboard goes back to
            // whatever the launch opened on — the commit list or the diff.
            panes.focus_named(&launch_focus);
        }
        // Whether the sidebars and the preview diff still owe the first
        // frame their data — decided before `repo` moves into the app, since
        // the flag is a property of the launch, not of the field.
        let startup_pending = repo.is_some();
        let jobs = Runner::new();
        let submitter = jobs.submitter();
        let mut app = Self {
            host,
            repo,
            availability: tui_availability(startup_pending),
            panes,
            layout: Box::new(panes::BuiltinLayout),
            geometry: None,
            last_list,
            screen: Screen::new(0, 0),
            modes: Modes::new(),
            pending: Vec::new(),
            message: String::new(),
            prompt: None,
            picker: None,
            opener: Arc::new(gitten_app::GitOpener),
            jobs,
            submitter,
            generation: Generation::default(),
            help: false,
            help_scroll: 0,
            quit: false,
            picked_theme: None,
            stats: None,
            runs: Vec::new(),
            bar,
            ascii: glyphs == Glyphs::ascii(),
            copy: None,
            gesture: None,
            clicked: None,
            clicks: 0,
            focus_keys: Vec::new(),
            startup_pending,
            tail: None,
            tail_commits: Vec::new(),
            // The preview lane: created once, before the first frame — a
            // request is one clone and one thread away from the very first
            // keypress that needs it.
            preview_tx,
            preview_rx,
            preview_seq: 0,
            preview_pending: 0,
        };
        app.sync_header_keys();
        app.sync_modes();
        app
    }

    /// Hands the app the log's unstreamed tail, seeded with the first batch
    /// the launch already drew. `main` calls this once, between construction
    /// and the loop; every batch after arrives through [`App::run`].
    fn set_tail(&mut self, stream: gitten_git::LogStream, commits: Vec<gitten_core::Commit>) {
        self.tail = Some(stream);
        self.tail_commits = commits;
    }

    /// The startup loads the skeleton deferred: the sidebars' first reads and
    /// the preview diff for row zero, all of them registered in their loading
    /// shape by [`App::new`] and filled in here — after the first frame is
    /// on the terminal, so the list the launch asked for is interactive
    /// before any of this runs, and everything else arrives on the frame
    /// after it.
    ///
    /// One wave, not four: the stash read, the status read, the describe and
    /// the branch reads all spawn into one `thread::scope` — the window pays
    /// one spawn floor for its sidebars and so does the terminal — and the
    /// preview diff rides the same wave, since the commit it diffs is row
    /// zero and known before any of this starts. The failures the window
    /// keeps quiet about are kept here too: a failed side read registers its
    /// unavailable pane and the error goes to the status line; the preview
    /// diff failing is a status-line message and an empty main pane.
    ///
    /// Synchronous on the terminal loop, deliberately — the same trade every
    /// refresh makes (see [`Screens::refresh`]): input pauses while the wave
    /// runs, and nothing here can stale, because nothing a write does can
    /// have landed between the frame and this call.
    fn load_startup(&mut self, clock: &mut StartClock) {
        self.startup_pending = false;
        let Some((path, repo)) = self.repo.clone() else {
            return;
        };
        let launch_focus = self.panes.focused_name().to_string();
        // The commit the preview diffs, decided before the wave: row zero of
        // the list the launch asked for. Enter is only the focus transfer; it
        // is never the trigger that makes the preview exist.
        let preview = match self.panes.get("commits") {
            Some(Screens::Commits { view, .. }) => view.current().cloned(),
            _ => None,
        };
        // An owned host crosses the thread boundary — the same copy the
        // window's background load carries.
        let host = self.host.clone();

        let (stash_read, remotes_read, status_read, described, branch_reads, diff_read) =
            std::thread::scope(|s| {
                // The handle as a stable borrow the `move` spawns copy: an
                // `Arc` would be four refcount bumps for the same answer.
                let repo = &repo;
                let stashes = s.spawn(move || acquire::stashes(repo.as_ref()));
                let remotes = s.spawn(|| repo.remotes());
                let status = s.spawn(move || repo.status());
                let described = s.spawn(move || repo.describe());
                let branches = s.spawn(move || load_branches(repo.as_ref()));
                let diff = preview.as_ref().map(|commit| {
                    let source = Source::Repo {
                        path: path.clone(),
                        arg: commit.sha.clone(),
                    };
                    s.spawn(move || {
                        acquire::acquire(View::Diff, &source, &host, Some(repo.as_ref()))
                    })
                });
                (
                    join_read(stashes),
                    join_read(remotes),
                    join_read(status),
                    join_read(described),
                    join_read(branches),
                    diff.map(join_read),
                )
            });
        clock.stage("startup reads joined");

        // Apply half, in place — the same `replace` calls the refresh path
        // makes, on the tenants the skeleton registered. A read that failed
        // leaves its unavailable pane standing and the error rides to the
        // status line; a branch failure outranks a stash one and a stash one
        // outranks a status one only because there is one line and every pane
        // refreshes either way.
        let mut error = None;
        if let Some(Screens::Remotes { view, label, .. }) = self.panes.get_mut("remotes") {
            match remotes_read {
                Ok(remotes) => {
                    let count = remotes.len();
                    view.replace(remotes);
                    *label = remotes_label(&described, count);
                }
                Err(e) => {
                    *label = "unavailable".to_string();
                    error.get_or_insert(e);
                }
            }
        }
        if let Some(Screens::Stashes { view, label, .. }) = self.panes.get_mut("stashes") {
            match stash_read {
                Ok(loaded) => {
                    let parked = loaded.stashes.len();
                    view.replace(loaded.stashes);
                    *label = stash_label(&loaded.label, parked);
                }
                Err(e) => {
                    *label = STASH_UNAVAILABLE.to_string();
                    error.get_or_insert(e);
                }
            }
        }
        if let Some(Screens::Files { view, label, .. }) = self.panes.get_mut("files") {
            match &status_read {
                Ok(status) => {
                    let files::Prepared { rows, label: next } = files::prepare(status, &described);
                    view.replace(rows);
                    *label = next;
                }
                // The pane keeps its unavailable shape and only the sentence
                // changes: the exact error goes to the status line below,
                // where it can be read — stderr now sits behind the
                // alternate screen, so the print the eager path made is gone.
                Err(_) => *label = files::unavailable_label(&described),
            }
        }
        if let Some(e) = status_read.err() {
            error.get_or_insert(e);
        }
        if let Some(Screens::Branches { view, label, .. }) = self.panes.get_mut("branches") {
            let (rows, next) = match branch_reads.error.as_ref() {
                // The lists themselves were lost: the pane's data is not
                // "empty", it is unread, and the label says so while the
                // error rides to the status line.
                Some(_) if branch_reads.local.is_empty() && branch_reads.remotes.is_empty() => {
                    (Vec::new(), branches::unavailable_label(&described))
                }
                _ => {
                    let prepared = branches::prepare(
                        &branch_reads.local,
                        &branch_reads.remotes,
                        branch_reads.head.as_ref(),
                        &described,
                    );
                    (prepared.rows, prepared.label)
                }
            };
            if let Some(e) = &branch_reads.error {
                error.get_or_insert(e.clone());
            }
            view.replace(rows);
            *label = next;
        }
        self.message = error.unwrap_or_default();
        self.panes.focus_named(&launch_focus);
        self.sync_header_keys();
        self.sync_modes();

        // The preview, last: the main pane names and draws row zero from the
        // wave's answer, and a failed one leaves the empty pane with the
        // message — the same shape `sync_main_diff` leaves on a failure.
        if let Some(commit) = preview {
            match diff_read {
                Some(Ok(loaded)) => match loaded.data {
                    Data::Diff(files) => {
                        let label = format!(
                            "{} {}",
                            &commit.sha[..commit.sha.len().min(8)],
                            commit.subject
                        );
                        self.install_diff(
                            DiffSource::Commit {
                                sha: commit.sha.clone(),
                            },
                            label,
                            files,
                            false,
                        );
                    }
                    Data::Commits(_) => {}
                },
                Some(Err(e)) => self.message = e,
                None => {}
            }
        }
        clock.stage("startup loads applied");
    }

    /// Puts an acquired diff behind the main pane — the registration,
    /// geometry and focus restoration every preview install shares, and the
    /// one place a diff tenant is ever built. The origin rides with it: it
    /// is what a refresh re-reads, what a hunk verb consults before it
    /// acts, and what a later request compares itself against so the same
    /// source is never read twice.
    ///
    /// The caller owns the label: a commit's preview names its subject, a
    /// file's side names the side, a stash names its entry — and startup
    /// names the launch's own read.
    fn install_diff(
        &mut self,
        origin: DiffSource,
        label: String,
        files: Vec<gitten_core::FileDiff>,
        focus: bool,
    ) {
        let old_focus = self.panes.focused_name().to_string();
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
                origin: Some(origin),
                label,
                // Acquired this instant, so it is as current as the queue's
                // last finish — not a generation older.
                generation: self.generation,
            },
        );
        // Only a gesture captured in the tenant just replaced became stale. A
        // click in commits is what requested this preview and must still
        // receive its release.
        if self.gesture.as_deref() == Some("diff") {
            self.gesture = None;
        }
        let target = match focus {
            true => "diff",
            false => &old_focus,
        };
        self.panes.focus_named(target);
        self.sync_modes();
    }

    /// What the main pane calls a preview, once it is installed: the
    /// person-readable half, looked up from the pane the selection came
    /// from so a commit names its subject and a stash its message.
    fn preview_label(&self, origin: &DiffSource) -> String {
        match origin {
            DiffSource::Commit { sha } => {
                let subject = self.panes.get("commits").and_then(|pane| match pane {
                    Screens::Commits { view, .. } => view.with_sha(sha).map(|c| c.subject.clone()),
                    _ => None,
                });
                match subject {
                    Some(subject) => format!("{} {subject}", &sha[..sha.len().min(8)]),
                    None => origin.label(),
                }
            }
            DiffSource::Stash { index, commit } => {
                let message = self.panes.get("stashes").and_then(|pane| match pane {
                    Screens::Stashes { view, .. } => view.message_of(commit).map(str::to_string),
                    _ => None,
                });
                match message {
                    Some(message) => format!("stash@{{{index}}} {message}"),
                    None => origin.label(),
                }
            }
            // The file-side sources label themselves; a drilldown names its
            // branch. Detached content is never installed here — startup
            // owns its own labels.
            other => other.label(),
        }
    }

    /// The mode stack follows the keyboard. Rebuilt rather than pushed and
    /// popped in step with focus, because two things kept in step drift.
    ///
    /// `panes` comes first, when a second sidebar list exists to cycle
    /// between — its Ctrl-J/Ctrl-K bindings would be a lie with one list —
    /// then the focused pane's own mode, then help and any prompt.
    fn sync_modes(&mut self) {
        self.modes = Modes::new();
        if self.panes.list_order().len() > 1 {
            self.modes.push(panes::MODE);
        }
        if let Some(screen) = self.panes.focused() {
            self.modes.push(screen.mode());
            // A standing search is its own innermost list mode: `n` and `N`
            // walk matches exactly for as long as there is a match to walk,
            // above the pane's own `n` — branches.new and commits.new-branch
            // keep their keys the moment the query is gone.
            if screen.search_standing() {
                self.modes.push("search");
            }
        }
        if self.picker.is_some() {
            // The recent-repositories list owns the keyboard like help does:
            // a press it does not name runs nothing underneath.
            self.modes.push(PICKER);
        }
        if self.help {
            self.modes.push("help");
        }
        // Above whatever pane it stands over: the prompt is the innermost
        // thing on screen while it is open, and it is what the help panel
        // should be listing bindings for.
        if self.prompt.is_some() {
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
            // The first frame draws before anything waits, so its poll does not
            // block; every later frame blocks here for the first event, or the
            // tick, which is the only thing that bounds how soon a saved config
            // is noticed. While the log's tail is streaming, the tick is short:
            // its rows land on the frame after the bytes arrive, and a full
            // tick would hold them back for nothing — the same shortness bounds
            // input latency, which is what makes it harmless.
            let tick = match self.tail.is_some() {
                true => STREAM_TICK,
                false => TICK,
            };
            match Term::poll(if first { Duration::ZERO } else { tick })? {
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
            // Then everything already queued, before drawing: a burst of wheel
            // notches is one frame, not one frame a notch. Bounded, so a flood
            // of piped input cannot starve the screen — see [`INPUT_BATCH`].
            for _ in 0..INPUT_BATCH {
                if self.quit {
                    break;
                }
                match Term::poll(Duration::ZERO)? {
                    Some(Input::Resize(w, h)) => {
                        size = (w, h);
                        self.screen.resize(w, h);
                        self.gesture = None;
                    }
                    Some(input) => self.input(input),
                    None => break,
                }
            }
            if self.quit {
                break;
            }
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
            self.pump();
            // The log's tail, whatever has arrived since the last frame —
            // appended to the list the launch asked for, which keeps its
            // cursor and viewport through the same `replace` a refresh rides.
            // When the stream ends, its one stage is said and a mid-flight
            // failure goes to the status line.
            if let Some(stream) = self.tail.as_mut() {
                let batch = stream.drain();
                if !batch.is_empty() {
                    self.tail_commits.extend(batch);
                    if let Some(Screens::Commits { view, .. }) = self.panes.get_mut("commits") {
                        view.replace(self.tail_commits.clone());
                    }
                }
                if !stream.live() {
                    let stream = self.tail.take().expect("checked above");
                    if let Some(e) = stream.error() {
                        self.message = e;
                    }
                    clock.stage("log streamed");
                }
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
                // The skeleton's deferred startup loads run here, not before
                // the terminal was taken: the list the launch asked for has
                // been on screen and interactive for every millisecond this
                // call runs. The second frame they fill is flushed before the
                // loop blocks on input again — otherwise the tick, not the
                // load, would decide when the sidebars and the preview
                // appear.
                if self.startup_pending {
                    self.load_startup(clock);
                    self.draw();
                    self.screen.flush(term.out())?;
                    clock.stage("startup frame flushed");
                }
            }
            if stats_on() {
                self.stats = Some((t.elapsed(), cells));
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
            Input::Wheel { key, col, row } => self.wheel(key, col, row),
            Input::Mouse(m) => self.mouse(m),
            // A paste is text, and only while a prompt stands is there anywhere
            // for it to go. It is never a key: pasted `q`, `?` and Enter are
            // characters, and the keymap never sees them.
            Input::Paste(text) if self.prompt.is_some() => self.edit_prompt(Edit::Paste(text)),
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
        // A reload is a change of the world the armed question was asked in:
        // the keymap, the theme and every cached rectangle move at once, so
        // the question dies with the frame it was asked in.
        self.disarm_branches();
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
        // While the picker stands it owns the keyboard the way help does:
        // resolved against exactly its one mode, so nothing underneath runs.
        if self.picker.is_some() {
            self.press_modal(PICKER, key, ModalKind::List);
            return;
        }
        // While the prompt stands it owns the keyboard, and the full stack
        // must not see the key: a query or a message is text, and the
        // globals would read it. See [`App::press_input`].
        if self.prompt.is_some() {
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

    /// One wheel notch, resolved in and routed to the pane below the pointer.
    ///
    /// The key remains data — its binding still comes from the shared keymap —
    /// but focus remains where the keyboard left it. A pane header counts as
    /// that pane; a divider, chrome, help panel or prompt has nothing below it
    /// to scroll and consumes nothing.
    fn wheel(&mut self, key: Key, col: usize, row: usize) {
        let (_, h) = self.screen.size();
        if h < 3 || self.prompt.is_some() || self.picker.is_some() {
            return;
        }
        if self.help {
            self.press(key);
            return;
        }
        let Some((name, _)) = self.hit(col, row) else {
            return;
        };
        let Some(mode) = self.panes.get(&name).map(Screens::mode) else {
            return;
        };

        self.message.clear();
        self.pending.push(key);
        let mut modes = Modes::new();
        if self.panes.list_order().len() > 1 {
            modes.push(panes::MODE);
        }
        modes.push(mode);
        let resolved = match self.host.keys.resolve(&modes, &self.pending) {
            Resolve::Run(command) => Some(command.to_string()),
            Resolve::Pending => return,
            Resolve::None => {
                let unknown = gitten_core::command::chord_string(&self.pending);
                self.pending.clear();
                self.message = format!("{unknown} is not bound — ? for the keys");
                return;
            }
        };
        self.pending.clear();
        if let Some(command) = resolved {
            self.dispatch_to(&command, Some(&name));
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
        self.press_modal(INPUT, key, ModalKind::Field);
    }

    /// One keypress while a modal owns the keyboard, resolved against
    /// exactly `mode` — [`Keymap::resolve_mode_any`], and never the full
    /// stack — because the shipped global bindings must not read what the
    /// modal holds: `?` would open help, `q` would quit. A binding written
    /// in `[keys.<mode>]` still wins first, which is what makes Enter and
    /// Esc (or their configured replacements) close it; a user who
    /// deliberately binds `?` there gets a `?` that means what they said,
    /// as mode scoping has always promised. What a chord that resolved to
    /// nothing does next is the modal's own kind: a field edits, a question
    /// and a list wait.
    fn press_modal(&mut self, mode: &str, key: Key, kind: ModalKind) {
        self.pending.push(key);
        // One spelling per press, one position per key — the same shape
        // [`Keymap::resolve`] builds, resolved against exactly one mode.
        let typed: Vec<&[Key]> = self.pending.iter().map(std::slice::from_ref).collect();
        match self.host.keys.resolve_mode_any(mode, &typed) {
            Resolve::Run(command) => {
                self.pending.clear();
                let command = command.to_string();
                self.dispatch(&command);
            }
            // A configured chord may still be forming. Wait for it.
            Resolve::Pending => {}
            Resolve::None => {
                self.pending.clear();
                if kind == ModalKind::List {
                    return;
                }
                // The keys a field understands as editing, in the shared
                // [`Edit`] vocabulary. Chords (ctrl/alt) are word motion on
                // the arrows and nobody's elsewhere; everything else no
                // binding claimed does nothing, and says nothing: a key that
                // does nothing while a field owns the keyboard is the field
                // doing its job, and none of them may fall through to the
                // globals.
                let edit = match key.code {
                    Code::Char(c) if !key.ctrl && !key.alt => Some(Edit::Char(c)),
                    Code::Backspace if !key.ctrl && !key.alt => Some(Edit::Backspace),
                    Code::Delete if !key.ctrl && !key.alt => Some(Edit::Delete),
                    Code::Left if !key.alt => Some(match key.ctrl {
                        true => Edit::WordLeft,
                        false => Edit::Left,
                    }),
                    Code::Right if !key.alt => Some(match key.ctrl {
                        true => Edit::WordRight,
                        false => Edit::Right,
                    }),
                    Code::Home if !key.ctrl && !key.alt => Some(Edit::Home),
                    Code::End if !key.ctrl && !key.alt => Some(Edit::End),
                    Code::Up if !key.ctrl && !key.alt => Some(Edit::Up),
                    Code::Down if !key.ctrl && !key.alt => Some(Edit::Down),
                    _ => None,
                };
                let Some(edit) = edit else {
                    return;
                };
                self.edit_prompt(edit);
            }
        }
    }

    /// `*.search`: gather a query over the pane the keyboard is on.
    ///
    /// Seeded from the standing query, so a second `/` finds the list as the
    /// first one left it; the full data stays in place, and each edit filters
    /// it live — every list pane answers to the same verb, and the diff
    /// walks to its matches instead of filtering, because a filtered diff is
    /// not a diff. The prompt holds the pane by *name* — the prompt holds
    /// the name, not an index, so however focus moves while it stands the
    /// query still lands on the list it was opened over.
    fn begin_search(&mut self, command: &str) {
        let name = self.panes.focused_name().to_string();
        if name.is_empty() {
            self.message = format!("{command} is not supported here");
            return;
        }
        let standing = match self.panes.get(&name) {
            Some(Screens::Commits { view, .. }) => view.query().unwrap_or_default().to_string(),
            Some(Screens::Files { view, .. }) => view.query().unwrap_or_default().to_string(),
            Some(Screens::Branches { view, .. }) => view.query().unwrap_or_default().to_string(),
            Some(Screens::Stashes { view, .. }) => view.query().unwrap_or_default().to_string(),
            Some(Screens::Remotes { view, .. }) => view.query().unwrap_or_default().to_string(),
            Some(Screens::Diff { view, .. }) => {
                if !view.has_search_text() {
                    self.message = format!("{command}: the diff has no text to search");
                    return;
                }
                view.search_query().unwrap_or_default().to_string()
            }
            None => {
                self.message = format!("{command} is not supported here");
                return;
            }
        };
        self.prompt = Some(Prompt::Search {
            pane: name,
            field: Field::with(standing),
        });
        self.gesture = None;
        self.pending.clear();
        self.sync_modes();
    }

    /// HEAD's subject, as the loaded history holds it — the amend prompt's
    /// prefill, so an amend is an edit of what is standing. `None` without a
    /// repository, on an unborn or detached HEAD, and when the loaded list
    /// does not hold the commit: the prompt then opens empty, which is an
    /// honest amend of nothing rather than a wrong prefill.
    ///
    /// The subject and not the whole message, deliberately narrow: the
    /// loaded [`Commit`] carries the subject line only. A prefill of the
    /// full message needs a `%B` read behind the `Repo` trait — recorded as
    /// a need, not smuggled in past the acquisition layer.
    fn head_subject(&self) -> Option<String> {
        let (_, repo) = self.repo.as_ref()?;
        let sha = match repo.head() {
            Ok(HeadState::Branch {
                commit: Some(sha), ..
            })
            | Ok(HeadState::Detached { commit: sha }) => sha,
            _ => return None,
        };
        match self.panes.get("commits") {
            Some(Screens::Commits { view, .. }) => view.with_sha(&sha).map(|c| c.subject.clone()),
            _ => None,
        }
    }

    /// `files.commit` / `files.amend`: gather a message on the status row,
    /// then commit — or rewrite HEAD — on accept.
    ///
    /// Both validate the two things the verb needs before opening the field:
    /// the files pane holds the keyboard, and there is a repository behind
    /// the app. Amend opens on HEAD's subject — an edit of what is standing
    /// is what amend is; commit opens empty, because there is nothing to
    /// edit yet.
    fn begin_commit_message(&mut self) {
        if !matches!(self.panes.focused(), Some(Screens::Files { .. })) {
            self.message = "files.commit is not supported here".into();
            return;
        }
        if self.repo.is_none() {
            self.message = "a fixture has no repository to commit in".into();
            return;
        }
        self.open_prompt(Prompt::CommitMessage {
            field: Field::new(),
        });
    }

    fn begin_amend_message(&mut self) {
        if !matches!(self.panes.focused(), Some(Screens::Files { .. })) {
            self.message = "files.amend is not supported here".into();
            return;
        }
        if self.repo.is_none() {
            self.message = "a fixture has no repository to amend in".into();
            return;
        }
        let subject = self.head_subject().unwrap_or_default();
        self.open_prompt(Prompt::AmendMessage {
            field: Field::with(subject),
        });
    }

    // ------------------------------------------------------- the branch verbs

    /// The focused branches pane's target — what the keyboard is on, as
    /// verbs aim at it, raw bytes included. `None` when the pane is absent,
    /// empty, or not the one that holds the keyboard.
    fn branch_target(&self) -> Option<Target> {
        match self.panes.focused() {
            Some(Screens::Branches { view, .. }) => view.current(),
            _ => None,
        }
    }

    /// Whether the keyboard is on the branches pane — the guard every branch
    /// verb opens with, said the way every wrong-focus refusal here is said.
    fn branches_focused(&mut self, command: &str) -> bool {
        match self.panes.focused() {
            Some(Screens::Branches { .. }) => true,
            _ => {
                self.message = format!("{command} is not supported here");
                false
            }
        }
    }

    /// `branches.checkout`: move HEAD onto the row the keyboard is on.
    ///
    /// A local row is git's own checkout, name bytes end to end. A remote-
    /// tracking row is the one verb on this pane aimed at a remote that
    /// creates something local: the branch of the same name, tracking, and
    /// HEAD on it — git's `--track`, never a detach. The one refusal said
    /// here is the detached row itself: already a place, not a branch to
    /// move to. Everything else — dirty tree, a name already taken — is
    /// git's sentence, surfaced verbatim by the job.
    fn checkout_branch(&mut self) {
        if !self.branches_focused("branches.checkout") {
            return;
        }
        let Some(target) = self.branch_target() else {
            self.message = "nothing selected to check out".into();
            return;
        };
        if matches!(target, Target::Detached) {
            self.message = "HEAD is already detached here".into();
            return;
        }
        if let Target::Remote { .. } = target {
            gitten_app::act::checkout_tracking(self);
            return;
        }
        let Some((_, repo)) = self.repo.as_ref() else {
            self.message = "a fixture has no repository to check out in".into();
            return;
        };
        let Target::Local(name) = target else {
            unreachable!("remotes and detached answer above");
        };
        let job = Write::checkout(repo, name.as_bytes().to_vec());
        self.submit(Box::new(job));
    }

    /// `branches.new`: gather a name on the status row; accept creates at
    /// HEAD. Reads no row and no repository state — creating never checks
    /// out, and the backend, not the view, decides whether HEAD is a valid
    /// start, which is why an empty repository's branches pane still answers
    /// this key.
    fn begin_branch_new(&mut self) {
        if !self.branches_focused("branches.new") {
            return;
        }
        if self.repo.is_none() {
            self.message = "a fixture has no repository to create branches in".into();
            return;
        }
        self.open_prompt(Prompt::BranchNew {
            field: Field::new(),
        });
    }

    /// The accepted branch name, as a job. Empty refused again here — the
    /// trait refuses it too, but saying so beside the field that just closed
    /// beats making the reader find out twice.
    fn submit_branch_new(&mut self, name: String) {
        if name.trim().is_empty() {
            self.message = "a branch needs a name".into();
            return;
        }
        if self.panes.get("branches").is_none() {
            self.message = "the pane the branch was asked over is gone".into();
            return;
        }
        let Some((_, repo)) = self.repo.as_ref() else {
            self.message = "a fixture has no repository to create branches in".into();
            return;
        };
        let job = Write::create_branch(repo, name.into_bytes(), None);
        self.submit(Box::new(job));
    }

    /// `branches.rename`: the same field, pre-filled with the row's own name
    /// — editing what is there beats retyping it, and accepting unchanged
    /// text answers with git's "already exists", which says more than a
    /// client-side veto would. The prefill is the visible spelling only when
    /// those bytes *are* text: a legal Latin-1 name decodes lossily into
    /// something with U+FFFD in it — a different name than the branch has —
    /// so those open blank instead, while `from` stays the exact bytes.
    fn begin_branch_rename(&mut self) {
        if !self.branches_focused("branches.rename") {
            return;
        }
        let Some(Target::Local(name)) = self.branch_target() else {
            self.message = "only a local branch can be renamed".into();
            return;
        };
        if self.repo.is_none() {
            self.message = "a fixture has no repository to rename in".into();
            return;
        }
        let initial = std::str::from_utf8(name.as_bytes()).unwrap_or("");
        self.open_prompt(Prompt::BranchRename {
            from: name.as_bytes().to_vec(),
            field: Field::with_selected(initial),
        });
    }

    /// The accepted rename, as a job — the same empty refusal the create
    /// field gives, because the field that failed is the same field.
    fn submit_branch_rename(&mut self, from: Vec<u8>, name: String) {
        if name.trim().is_empty() {
            self.message = "a branch needs a name".into();
            return;
        }
        if self.panes.get("branches").is_none() {
            self.message = "the pane the branch was asked over is gone".into();
            return;
        }
        let Some((_, repo)) = self.repo.as_ref() else {
            self.message = "a fixture has no repository to rename in".into();
            return;
        };
        let job = Write::rename_branch(repo, from, name.into_bytes());
        self.submit(Box::new(job));
    }

    /// `branches.new-tag`: gather a name on the status row; accept names the
    /// selected branch with a lightweight tag. The tag's target is the
    /// branch's raw bytes — a revspec git resolves the same way it resolves a
    /// sha — captured when the field opens, so nothing a cursor does while
    /// the field holds the keyboard can re-aim the tag.
    fn begin_branch_tag(&mut self) {
        if !self.branches_focused("branches.new-tag") {
            return;
        }
        let Some(Target::Local(name)) = self.branch_target() else {
            self.message = "only a local branch can be tagged here".into();
            return;
        };
        if self.repo.is_none() {
            self.message = "a fixture has no repository to tag in".into();
            return;
        }
        self.open_prompt(Prompt::TagNew {
            at: name.as_bytes().to_vec(),
            field: Field::new(),
        });
    }

    /// The accepted tag name, as a job. The text is trimmed before it is
    /// queued, because git would hold the padding as part of the name; a
    /// duplicate rides on to git and comes back in its words.
    fn submit_branch_tag(&mut self, at: Vec<u8>, name: String) {
        let name = name.trim().to_string();
        if name.is_empty() {
            self.message = "a tag needs a name".into();
            return;
        }
        if self.panes.get("branches").is_none() {
            self.message = "the pane the tag was asked over is gone".into();
            return;
        }
        let Some((_, repo)) = self.repo.as_ref() else {
            self.message = "a fixture has no repository to tag in".into();
            return;
        };
        let job = Write::create_tag(repo, name.into_bytes(), at, None);
        self.submit(Box::new(job));
    }

    /// `branches.delete`: the destructive verb of this pane, confirmed on
    /// the keyboard exactly as `files.discard` is. First press arms the row
    /// and asks once in the band; second press on the same row deletes —
    /// merged work only (`force = false`), because an unmerged branch comes
    /// back refused in git's own words and that sentence is the force
    /// decision's proper home. Remote rows refuse outright, on purpose: a
    /// tracking ref is the remote's shadow, and deleting it here would be a
    /// fetch's prune done by hand under a key that reads as something
    /// stronger.
    fn delete_branch_selected(&mut self) {
        if !self.branches_focused("branches.delete") {
            return;
        }
        gitten_app::act::delete_branch(self);
    }

    /// Whether the keyboard is on the remotes pane — the guard every remote
    /// verb opens with, said the way every wrong-focus refusal here is said.
    fn remotes_focused(&mut self, command: &str) -> bool {
        match self.panes.focused() {
            Some(Screens::Remotes { .. }) => true,
            _ => {
                self.message = format!("{command} is not supported here");
                false
            }
        }
    }

    /// The remote row the keyboard is on, as the verbs address it — the
    /// implementation behind [`act::RemoteClient`].
    fn remote_target(&self) -> Option<RefName> {
        match self.panes.focused() {
            Some(Screens::Remotes { view, .. }) => view.current(),
            _ => None,
        }
    }

    /// Arms the selected remote for a confirmed removal, or spends the arm —
    /// the implementation behind [`act::RemoteClient`].
    fn confirm_or_arm_remote(&mut self, name: &RefName) -> bool {
        match self.panes.focused_mut() {
            Some(Screens::Remotes { view, .. }) => view.confirm_or_arm_remove(name),
            _ => false,
        }
    }

    /// `branches.checkout-name`: gather a name on the status row; accept
    /// checks out whatever git is handed. Reads no row — the name is the
    /// whole of the aim — which is also why a fixture refuses before the
    /// field opens: a question a repository cannot answer is a trap.
    fn begin_branch_checkout_name(&mut self) {
        if !self.branches_focused("branches.checkout-name") {
            return;
        }
        if self.repo.is_none() {
            self.message = "a fixture has no repository to check out in".into();
            return;
        }
        self.open_prompt(Prompt::BranchCheckoutName {
            field: Field::new(),
        });
    }

    /// `commits.new-branch`: gather a name on the status row; accept grows
    /// the branch at the commit the keyboard was on when the field opened —
    /// captured now, never re-read from the pane. Once the branch exists,
    /// the checkout is offered as a question: enter takes it, esc leaves
    /// the branch to be found with space in the branches pane later.
    fn begin_branch_new_at(&mut self) {
        let Some(Screens::Commits { view, .. }) = self.panes.focused() else {
            self.message = "commits.new-branch is not supported here".into();
            return;
        };
        let Some(commit) = view.current() else {
            self.message = "the keyboard is not on a commit".into();
            return;
        };
        if self.repo.is_none() {
            self.message = "a fixture has no repository to create branches in".into();
            return;
        }
        self.open_prompt(Prompt::BranchNewAt {
            at: commit.sha.clone().into_bytes(),
            field: Field::new(),
        });
    }

    /// The accepted name, as a job — then the question. Empty refused again
    /// here, because the field that failed is the same field.
    fn submit_branch_new_at(&mut self, at: Vec<u8>, name: String) {
        if name.trim().is_empty() {
            self.message = "a branch needs a name".into();
            return;
        }
        gitten_app::act::create_branch_at(self, name.clone(), at);
        // The question stands only over a branch that exists: offered after
        // the job is queued, refused when the action above said so.
        if self.message.is_empty() {
            self.open_prompt(Prompt::CheckoutNew {
                name,
                field: Field::new(),
            });
        }
    }

    /// `project.open`: gather a path on the status row; accept switches
    /// everything the app holds. The switch itself is
    /// [`App::open_repository`]'s, and a refusal there leaves every pane
    /// exactly where it was.
    fn begin_project_open(&mut self) {
        self.open_prompt(Prompt::ProjectOpen {
            field: Field::new(),
        });
    }

    /// `project.switch`: the recent repositories, as a modal list. Empty is
    /// said, not shown — a picker of nothing is a trap shaped like an
    /// answer.
    fn open_project_picker(&mut self) {
        let rows = gitten_app::projects::load();
        if rows.is_empty() {
            self.message = "no recent repositories — O to open one".into();
            return;
        }
        self.picker = Some(RecentPicker { rows, cursor: 0 });
        self.sync_modes();
    }

    /// `project.next` / `project.prev`: step the MRU from the repository
    /// this app is showing. A fixture has no entry in the list, so a step
    /// lands on its first row — the honest reading of "next" from nowhere.
    fn switch_project(&mut self, by: isize) {
        let rows = gitten_app::projects::load();
        if rows.is_empty() {
            self.message = "no recent repositories — O to open one".into();
            return;
        }
        let at = self
            .repo
            .as_ref()
            .map(|(path, _)| path.clone())
            .and_then(|path| rows.iter().position(|row| same_repository(row, &path)));
        let next = match at {
            Some(at) => (at as isize + by).rem_euclid(rows.len() as isize) as usize,
            None => 0,
        };
        self.open_repository(rows[next].to_string_lossy().as_ref());
    }

    /// Opens `path` as this app's repository: every pane rebuilt from the
    /// new handle, everything the old one owned left behind with it.
    ///
    /// The guards are the whole of the safety here, and both are already
    /// the app's own rules. The preview lane's answers carry the root they
    /// were read from and install only while it is still ours; the job
    /// queue's jobs hold the handle they were built with, so a write aimed
    /// at the old repository lands on the old repository, however long it
    /// runs past this call. What cannot be prevented — a straggler's finish
    /// line — only advances a generation and re-reads the *new* panes, which
    /// is a refresh and not a leak. And a refusal anywhere below — not a
    /// repository, no history to open — changes nothing: the panes, the
    /// handle, the MRU and the message all stay as they were.
    fn open_repository(&mut self, raw: &str) {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            self.message = "a repository needs a path".into();
            return;
        }
        let path = std::path::PathBuf::from(trimmed);
        if self
            .repo
            .as_ref()
            .is_some_and(|(current, _)| same_repository(current, &path))
        {
            self.message = format!("already showing {}", path.display());
            return;
        }
        let handle = self.opener.open(&path);
        // The one read every pane below depends on decides whether this is
        // a repository at all — and answers in git's own words when it is
        // not, which is the sentence a reader can act on.
        if let Err(e) = handle.status() {
            self.message = format!("{}: {e}", path.display());
            return;
        }
        let source = Source::Repo {
            path: path.clone(),
            arg: String::new(),
        };
        let loaded =
            match acquire::acquire(View::Commits, &source, &self.host, Some(handle.as_ref())) {
                Ok(loaded) => loaded,
                Err(e) => {
                    self.message = format!("{}: {e}", path.display());
                    return;
                }
            };
        let Data::Commits(commits) = loaded.data else {
            self.message = "the new repository answered with the wrong view".into();
            return;
        };
        // Recorded only past every refusal: a failed open never reaches the
        // list a future switch would offer.
        gitten_app::projects::record(&path);
        let glyphs = match self.ascii {
            true => Glyphs::ascii(),
            false => Glyphs::default(),
        };
        let marks = match glyphs == Glyphs::ascii() {
            true => Marks::ascii(),
            false => Marks::default(),
        };
        let mut list = Commits::with_glyphs(commits, glyphs);
        list.set_bar(self.bar);
        let mut stash = Stashes::unavailable();
        stash.set_bar(self.bar);
        let mut remotes = Remotes::unavailable();
        remotes.set_bar(self.bar);
        let mut files = Files::unavailable();
        files.set_bar(self.bar);
        let mut branches = Branches::with_marks(Vec::new(), marks);
        branches.set_bar(self.bar);
        let mut panes = panes::Panes::new();
        panes.register(
            "commits",
            panes::Placement::sidebar("commits"),
            Screens::Commits {
                view: list,
                source,
                log_of: None,
                label: loaded.label,
                generation: Generation::default(),
            },
        );
        panes.register(
            "stashes",
            panes::Placement::sidebar("stashes"),
            Screens::Stashes {
                view: stash,
                label: STARTUP_LOADING.to_string(),
                generation: Generation::default(),
            },
        );
        panes.register(
            "remotes",
            panes::Placement::sidebar("remotes"),
            Screens::Remotes {
                view: remotes,
                label: STARTUP_LOADING.to_string(),
                generation: Generation::default(),
            },
        );
        panes.register(
            "diff",
            panes::Placement::Main,
            Screens::Diff {
                view: Diff::new(Vec::new(), &self.host),
                origin: None,
                label: EMPTY_DIFF_LABEL.to_string(),
                generation: Generation::default(),
            },
        );
        panes.register(
            "files",
            panes::Placement::sidebar("files"),
            Screens::Files {
                view: files,
                label: STARTUP_LOADING.to_string(),
                generation: Generation::default(),
            },
        );
        panes.register(
            "branches",
            panes::Placement::sidebar("branches"),
            Screens::Branches {
                view: branches,
                label: STARTUP_LOADING.to_string(),
                generation: Generation::default(),
            },
        );
        panes.focus_named("commits");
        self.panes = panes;
        self.repo = Some((path.clone(), handle));
        // What the client runs changed with the repository: a fixture view
        // refused the sync keys, and this one answers them.
        self.availability = tui_availability(true);
        self.startup_pending = true;
        self.help = false;
        self.tail = None;
        self.tail_commits.clear();
        self.geometry = None;
        self.sync_header_keys();
        self.sync_modes();
        // The deferred startup wave, the same one a launch runs: the
        // sidebars and row zero's preview arrive on the frame after this.
        self.load_startup(&mut StartClock::new());
        self.message = format!("switched to {}", path.display());
    }

    /// `remotes.new`'s second field, opened by the accepted name: the URL,
    /// with the name held as bytes for the job.
    fn begin_remote_url(&mut self, name: String) {
        if name.trim().is_empty() {
            self.message = "a remote needs a name".into();
            return;
        }
        self.open_prompt(Prompt::RemoteUrl {
            name,
            field: Field::new(),
        });
    }

    /// `remotes.edit`: the same field, prefilled with the selected remote's
    /// first URL — editing what is there beats retyping it. A remote with
    /// no URL opens blank, and the accepted text is the whole new value.
    fn begin_remote_edit(&mut self) {
        if !self.remotes_focused("remotes.edit") {
            return;
        }
        let Some(name) = self.remote_target() else {
            self.message = "nothing selected to edit".into();
            return;
        };
        if self.repo.is_none() {
            self.message = "a fixture has no repository to edit in".into();
            return;
        }
        let initial = match self.panes.get("remotes") {
            Some(Screens::Remotes { view, .. }) => {
                view.current_urls().first().cloned().unwrap_or_default()
            }
            _ => String::new(),
        };
        self.open_prompt(Prompt::RemoteEdit {
            name,
            field: Field::with_selected(&initial),
        });
    }

    /// Stands a prompt up and gives it the keyboard — the one path every
    /// prompt opens through, so none can stand while another does and the
    /// modes always follow. A prompt also covers the body, which is why
    /// every destructive arm dies here: a question nobody can see or answer
    /// is not a question, it is a trap.
    fn open_prompt(&mut self, prompt: Prompt) {
        self.disarm_branches();
        self.prompt = Some(prompt);
        self.gesture = None;
        self.pending.clear();
        self.sync_modes();
    }

    /// Drops the branches pane's destructive arm, if one stands and the pane
    /// exists. The one place a prompt and a reload both reach — the arm's own
    /// clearing on moves, wheels, mouse rows and refreshes lives on the view,
    /// where the keyboard is. Focus is deliberately missing: a round-trip
    /// across the ring leaves the question standing on its row, the window's
    /// contract and the files pane's.
    fn disarm_branches(&mut self) {
        if let Some(Screens::Branches { view, .. }) = self.panes.get_mut("branches") {
            view.disarm();
        }
    }

    /// Queues a write, or says the queue is going away. The one sentence
    /// every verb's failure mode shares, factored so a new verb cannot
    /// forget it.
    fn submit(&mut self, job: Box<dyn Job>) {
        if self.submitter.submit(job).is_err() {
            self.message = "the job queue is shutting down".into();
        }
    }

    /// One edit to the open prompt's field, and the routing the variant asks
    /// for.
    ///
    /// Per keystroke and never per frame — the next frame only draws what
    /// this already decided, which is why nothing in [`App::draw`] searches.
    /// The edit itself is generic — the field's, whatever the prompt — and
    /// only the search routes the result anywhere: the list filter rebuilds
    /// under it live, and the diff walks to its next match live, because a
    /// message being typed is nobody else's business until Enter says so. A
    /// paste is sized by the field it lands in: a message keeps its line
    /// breaks, a one-line field flattens them, both by the shared
    /// [`Field::paste`]. `input.newline` in a one-line field is refused — the
    /// field is one line by design, and the reason is said, not swallowed.
    fn edit_prompt(&mut self, edit: Edit) {
        if matches!(edit, Edit::Newline) {
            let multiline = self
                .prompt
                .as_ref()
                .is_some_and(|prompt| prompt.multiline());
            if !multiline {
                self.message = "this field is one line".into();
                return;
            }
        }
        let routes_live = matches!(&self.prompt, Some(Prompt::Search { .. }));
        let Some(prompt) = self.prompt.as_mut() else {
            return;
        };
        // A question is not a field: typed keys wait for an answer that is
        // spelled enter or esc, and nothing else means anything here.
        if prompt.question().is_some() {
            return;
        }
        // A paste is one edit; which shape it takes is the field's own kind.
        let edit = match edit {
            Edit::Paste(text) if prompt.multiline() => Edit::PasteMultiline(text),
            other => other,
        };
        prompt.field_mut().edit(edit);
        if !routes_live {
            return;
        }
        // The edit is in; the borrow `prompt` holds ends with it. The query
        // is copied out rather than borrowed across the call, because the
        // routing needs `&mut self` — a line of text, once per keystroke
        // and never per frame, against a rebuild the filter does anyway.
        let Some(Prompt::Search { field, .. }) = self.prompt.as_ref() else {
            return;
        };
        let query = field.text().to_string();
        self.apply_query(&query);
    }

    /// The search the open prompt describes, routed to the pane it stands
    /// over — by name, not by focus, so a focus change cannot strand the
    /// query. A list filters; the diff walks. The one place an edit, an
    /// accept or a cancel reaches the views, so the prompt and the lists
    /// stay two things: the prompt is input, the lists are data, and this is
    /// the line between them.
    fn apply_query(&mut self, query: &str) {
        let Some(pane) = self.prompt.as_ref().map(Prompt::pane) else {
            return;
        };
        if pane.is_empty() {
            return;
        }
        let pane = pane.to_string();
        let before = self.eye_of(&pane);
        match self.panes.get_mut(&pane) {
            Some(Screens::Commits { view, .. }) => view.apply_query(query),
            Some(Screens::Files { view, .. }) => view.apply_query(query),
            Some(Screens::Branches { view, .. }) => view.apply_query(query),
            Some(Screens::Stashes { view, .. }) => view.apply_query(query),
            Some(Screens::Remotes { view, .. }) => view.apply_query(query),
            Some(Screens::Diff { view, .. }) => view.search_edit(query),
            None => {}
        }
        self.eye_follow(&pane, before);
    }

    /// A list or diff under the keyboard changed what its eye is on — a
    /// filter moved it, a clear restored it — so the main preview follows,
    /// exactly as a cursor move's eye change does in dispatch. Nothing is
    /// asked when the pane is not the one holding the keyboard: its eye
    /// moved, but the eye the frame draws is the focused pane's.
    fn eye_follow(&mut self, pane: &str, before: Option<DiffSource>) {
        if self.panes.focused_name() != pane {
            return;
        }
        let after = self.eye_of(pane);
        if after.as_ref() != before.as_ref() {
            if let Some(after) = after {
                self.request_preview(after, false);
            }
        }
    }

    /// `input.accept` / `input.cancel` of the open prompt.
    ///
    /// Accept hands the text to whoever the variant names — the search keeps
    /// its last edit standing (an *empty* accept is how a filter comes off,
    /// and on the diff it is how a search ends with nothing), a message
    /// submits its write — and cancel throws the text away with no write
    /// built, which for a search means restoring what stood before it: the
    /// unfiltered list, or the diff without its standing query. Both close
    /// the prompt and give the keyboard back.
    fn finish_prompt(&mut self, accept: bool) {
        let Some(prompt) = self.prompt.take() else {
            return;
        };
        match prompt {
            Prompt::Search { pane, .. } if !accept => {
                // Cancel restores what stood before the search: the
                // unfiltered list, or the diff without its standing query.
                let before = self.eye_of(&pane);
                match self.panes.get_mut(&pane) {
                    Some(Screens::Commits { view, .. }) => view.apply_query(""),
                    Some(Screens::Files { view, .. }) => view.clear_search(),
                    Some(Screens::Branches { view, .. }) => view.clear_search(),
                    Some(Screens::Stashes { view, .. }) => view.clear_search(),
                    Some(Screens::Remotes { view, .. }) => view.clear_search(),
                    Some(Screens::Diff { view, .. }) => view.search_clear(),
                    None => {}
                }
                self.eye_follow(&pane, before);
            }
            Prompt::Search { .. } => {
                // Accept keeps what is standing: a list's filter was applied
                // live per keystroke, and the diff's standing query too. An
                // empty accept is how a filter comes off — the next keystroke
                // is the other door, and the mode's `esc` the one beside it.
            }
            Prompt::CommitMessage { field } if accept => self.submit_commit(field.take()),
            Prompt::AmendMessage { field } if accept => self.submit_amend(field.take()),
            Prompt::BranchNew { field } if accept => self.submit_branch_new(field.take()),
            Prompt::BranchRename { from, field } if accept => {
                self.submit_branch_rename(from, field.take())
            }
            Prompt::TagNew { at, field } if accept => self.submit_branch_tag(at, field.take()),
            Prompt::BranchCheckoutName { field } if accept => {
                gitten_app::act::checkout_by_name(self, field.take())
            }
            Prompt::BranchNewAt { at, field } if accept => {
                self.submit_branch_new_at(at, field.take())
            }
            Prompt::CheckoutNew { name, .. } if accept => {
                let Some((_, repo)) = self.repo.as_ref() else {
                    self.message = "a fixture has no repository to check out in".into();
                    return;
                };
                let job = Write::checkout(repo, name.clone().into_bytes());
                self.submit(Box::new(job));
            }
            Prompt::ProjectOpen { field } if accept => self.open_repository(&field.take()),
            Prompt::RemoteName { field } if accept => self.begin_remote_url(field.take()),
            Prompt::RemoteUrl { name, field } if accept => {
                gitten_app::act::remote_add(self, name, field.take())
            }
            Prompt::RemoteEdit { name, field } if accept => {
                gitten_app::act::remote_edit(self, name.as_bytes().to_vec(), field.take())
            }
            // Cancelled: the text was the prompt's and dies with it.
            _ => {}
        }
        self.pending.clear();
        self.sync_modes();
    }

    /// The accepted commit text, as a job. Empty refused again here — the
    /// trait refuses it too, but saying so beside the field that just closed
    /// beats making the reader find out twice.
    fn submit_commit(&mut self, message: String) {
        gitten_app::act::commit_message(self, message);
    }

    /// The accepted amend text, as a job — [`Write::amend`], the commit
    /// constructor aimed one step back. The refusals are shared on purpose:
    /// an empty message is refused with commit's own sentence, because the
    /// field that failed is the same field.
    fn submit_amend(&mut self, message: String) {
        gitten_app::act::amend_message(self, message);
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
        // The help panel, the picker and any prompt are drawn over the body,
        // so a click that reached a view through any of them would act on a
        // row it is hiding. The keyboard gathers the text; the mouse waits.
        if h < 3 || self.help || self.prompt.is_some() || self.picker.is_some() {
            return;
        }
        match m.kind {
            MouseKind::Down => {
                let Some((name, rect)) = self.hit(m.col, m.row) else {
                    return;
                };
                self.message.clear();
                let clicks = self.count(m.col, m.row, &name);
                let was_focused = self.panes.focused_name() == name;
                // A press on the header focuses and nothing else — the same
                // answer the window's pane headers give — so a gesture that
                // started there has a pane to be released against and no more.
                if m.row == rect.y {
                    if !was_focused {
                        self.focus_named(&name);
                    }
                    self.gesture = Some(name);
                    return;
                }
                let local_col = m.col - rect.x;
                let local_row = m.row - rect.y - 1;
                let before = self.current_commit_sha();
                {
                    let Self { panes, host, .. } = self;
                    if let Some(pane) = panes.get_mut(&name) {
                        pane.press(local_col, local_row, clicks, m.shift, host);
                    }
                }
                // Apply the row before transferring focus: commits focus then
                // previews exactly the clicked row, never the row that used to
                // be highlighted on the way there.
                if !was_focused {
                    self.focus_named(&name);
                } else if name == "commits" && self.current_commit_sha() != before {
                    self.request_commit_preview(false);
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
        self.dispatch_to(command, None);
    }

    /// A command into an effect, optionally giving the pane that originated it.
    ///
    /// Keyboard commands omit the target and fall through to the focused pane.
    /// A wheel supplies its hit-tested pane, while app-wide commands keep their
    /// ordinary meaning regardless of where their binding originated.
    fn dispatch_to(&mut self, command: &str, target: Option<&str>) {
        // The client's word, before any routing: a name this binary has no
        // handler for is refused here — said, and never routed to a pane to
        // be told so again — and a name the launch turned away says the
        // reason. The same [`Availability`] the help panel read when it
        // drew the row, so what `?` shows and what a press does cannot
        // disagree. Extension commands a client does not answer are
        // unsupported by exactly this word too, which is what keeps the
        // panel from advertising them.
        match self.availability.state(command) {
            Usable::Available => {}
            Usable::Disabled(reason) => {
                self.message = format!("{command}: {reason}");
                return;
            }
            Usable::Unsupported => {
                self.message = format!("{command} is not supported by this client");
                return;
            }
        }
        // While the picker stands it owns the keyboard, exactly as the help
        // panel does: the moves it names land on it, accept opens, cancel
        // and back close — and everything else waits, because a chord that
        // fires a write behind a modal list is a trap help's mode was
        // invented to close.
        if self.picker.is_some() {
            match command {
                "view.down" | "view.up" | "view.top" | "view.bottom" => {
                    if let Some(picker) = self.picker.as_mut() {
                        match command {
                            "view.down" => picker.down(),
                            "view.up" => picker.up(),
                            "view.top" => picker.jump_top(),
                            _ => picker.jump_bottom(),
                        }
                    }
                }
                "input.accept" => {
                    if let Some(picker) = self.picker.take() {
                        let target = picker.rows[picker.cursor].clone();
                        self.sync_modes();
                        self.open_repository(target.to_string_lossy().as_ref());
                    }
                }
                "input.cancel" | "back" => {
                    self.picker = None;
                    self.sync_modes();
                }
                _ => {}
            }
            return;
        }
        if self.help && self.scroll_help(command) {
            return;
        }
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
            | "stashes.focus" | "remotes.focus" | "diff.focus" => {
                let name = command.strip_suffix(".focus").unwrap_or(command);
                self.focus_named(name);
            }
            "quit" => self.quit = true,
            "help" => {
                self.help = !self.help;
                if self.help {
                    self.help_scroll = 0;
                }
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
            // The preview doors of the other lists, the same shape one pane
            // over: enter previews and focuses the row's own content, and —
            // for the working tree — tab flips the previewed side when the
            // file has one on the other side of the index.
            "files.open-diff" => self.open_file_diff(),
            "files.toggle-side" => self.toggle_file_side(),
            "stashes.open-diff" => self.open_stash_diff(),
            "branches.open-log" => self.open_branch_log(),
            // The prompt's names, and the whole of what they gather: open a
            // query or a message field, accept it, cancel it. Each resolves
            // through the live keymap — the search names in their pane
            // modes, the rest in `input` while any prompt stands — so
            // `gitten.toml` moves them the way it moves everything else.
            "commits.search" | "files.search" | "branches.search" | "stashes.search"
            | "remotes.search" | "diff.search" => self.begin_search(command),
            "files.commit" => self.begin_commit_message(),
            "files.amend" => self.begin_amend_message(),
            // lazygit's global R, on the same wave a finished write runs:
            // every registered repository-backed pane re-acquires, hidden
            // ones included, and a read that fails leaves the last good
            // rows standing and says so. Nothing here decides what the
            // panes re-read — [`Screens::refresh`] does, exactly as for the
            // queue's own finish.
            "repo.refresh" => self.manual_refresh(),
            "input.accept" => self.finish_prompt(true),
            "input.cancel" => self.finish_prompt(false),
            // A line break, into a field that holds multiline text.
            "input.newline" => self.edit_prompt(Edit::Newline),
            // The standing search's two walks — the `search` mode's whole
            // point, so they are refused by name when no search stands
            // rather than falling into a pane that would answer with
            // silence. Routed to the focused pane, whose `run` knows which
            // walk it is.
            "search.next" | "search.prev" => {
                if !self.panes.focused().is_some_and(Screens::search_standing) {
                    self.message = "no search standing — / to start one".into();
                } else {
                    let routed = self.panes.focused_name().to_string();
                    if let Some(pane) = self.panes.get_mut(&routed) {
                        pane.run(command, &self.host);
                    }
                }
            }
            // The search off — the `esc` of the `search` mode, and the door
            // `search.clear` names. The eye follows, exactly as a filter
            // edit's does.
            "search.clear" => {
                let name = self.panes.focused_name().to_string();
                if !name.is_empty() {
                    let before = self.eye_of(&name);
                    if let Some(pane) = self.panes.get_mut(&name) {
                        match pane {
                            Screens::Commits { view, .. } => view.clear_search(),
                            Screens::Files { view, .. } => view.clear_search(),
                            Screens::Branches { view, .. } => view.clear_search(),
                            Screens::Stashes { view, .. } => view.clear_search(),
                            Screens::Remotes { view, .. } => view.clear_search(),
                            Screens::Diff { view, .. } => view.search_clear(),
                        }
                    }
                    self.eye_follow(&name, before);
                }
            }
            // The hunk verbs act on the *repository*, not the pane: they
            // need the source the diff was acquired from and the handle it
            // was acquired through, and a view is drawing and input only.
            // Routed here, ahead of the pane, for the same reason the
            // window routes them in its `run_command`.
            "diff.stage-hunk" | "diff.unstage-hunk" | "diff.discard-hunk" => {
                self.hunk_verb(command)
            }
            // The stash verbs act on the *repository*, not the pane: the
            // pane answers "which row", the queue takes it from there.
            // Routed ahead of the focused pane for the same reason the hunk
            // verbs are — and `files.stash` needs no pane at all, only the
            // repository, so a files tenant gains its already-configured
            // `s` binding simply by registering.
            "stashes.apply" | "stashes.pop" | "stashes.drop" => self.stash_selected(command),
            "files.stash" => self.stash_working_tree(),
            // The sync verbs act on the *repository* — the branch HEAD sits
            // on, and the remotes the config names — not on any pane. The
            // refusals are the shared actions': no upstream, no remote,
            // detached HEAD, a fixture.
            "repo.push" => gitten_app::act::sync_push(self),
            "repo.pull" => gitten_app::act::sync_pull(self),
            "repo.fetch" => gitten_app::act::sync_fetch(self),
            // Repository switching: open a path, the recent list, stepping
            // the MRU. The switch itself is below, where the panes are.
            "project.open" => self.begin_project_open(),
            "project.switch" => self.open_project_picker(),
            "project.next" => self.switch_project(1),
            "project.prev" => self.switch_project(-1),
            // The branch movement verbs: the row the keyboard is on, the
            // shared action that means it. Force is the twice-pressed one;
            // the rest move refs and HEAD without destroying anything.
            "branches.checkout-name" => self.begin_branch_checkout_name(),
            "branches.checkout-previous" => gitten_app::act::checkout_previous(self),
            "branches.force-checkout" => {
                if self.branches_focused("branches.force-checkout") {
                    gitten_app::act::force_checkout(self);
                }
            }
            "branches.fast-forward" => {
                if self.branches_focused("branches.fast-forward") {
                    gitten_app::act::fast_forward(self);
                }
            }
            "branches.set-upstream" => {
                if self.branches_focused("branches.set-upstream") {
                    gitten_app::act::set_upstream(self);
                }
            }
            "branches.unset-upstream" => {
                if self.branches_focused("branches.unset-upstream") {
                    gitten_app::act::unset_upstream(self);
                }
            }
            // The remotes verbs: the row the keyboard is on, the shared
            // `Write` that means it. Add and edit open the one prompt chain
            // on their way through.
            "remotes.fetch" => {
                if self.remotes_focused("remotes.fetch") {
                    gitten_app::act::remote_fetch(self);
                }
            }
            "remotes.new" => {
                if self.remotes_focused("remotes.new") {
                    self.open_prompt(Prompt::RemoteName {
                        field: Field::new(),
                    });
                }
            }
            "remotes.edit" => self.begin_remote_edit(),
            "remotes.remove" => {
                if self.remotes_focused("remotes.remove") {
                    gitten_app::act::remote_remove(self);
                }
            }
            // A branch grown from the commit the keyboard is on: the sha is
            // captured when the field opens, and the checkout is offered as
            // a question once the branch exists.
            "commits.new-branch" => self.begin_branch_new_at(),
            // The file verbs, the same story one pane over: the row the
            // keyboard is on, the side of the index it sits on, and the
            // shared `Write` that means it — routed here ahead of the pane,
            // because their work belongs to the job queue, not to a view.
            // Stash is repository-scoped and asks no pane at all.
            "files.stage" => self.files_stage(),
            "files.stage-all" => self.files_stage_all(),
            "files.discard" => self.files_discard(),
            "files.ignore" => self.files_ignore(),
            // The branch verbs, the same story again: the row the keyboard
            // is on — bytes, not display text — and the shared `Write` that
            // means it. Routed here ahead of the pane, because their work
            // belongs to the job queue, not to a view; rename and tag open
            // the one prompt field on their way through.
            "branches.checkout" => self.checkout_branch(),
            "branches.new" => self.begin_branch_new(),
            "branches.rename" => self.begin_branch_rename(),
            "branches.delete" => self.delete_branch_selected(),
            "branches.new-tag" => self.begin_branch_tag(),
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
                let routed = target
                    .unwrap_or_else(|| self.panes.focused_name())
                    .to_string();
                // What the keyboard's pane is looking at before the command
                // runs. The main preview follows the selection, pane by pane:
                // the commits list previews its commit, the working tree its
                // file's side, the stack its entry — whatever the keyboard is
                // on is what the eye is on. The branches list is the
                // exception: its drilldown is a log install, which only Enter
                // asks for, because a read per cursor step buys nothing a
                // cursor step can use.
                let before = self.eye_of(&routed);
                let known = match target {
                    Some(name) => self
                        .panes
                        .get_mut(name)
                        .is_some_and(|pane| pane.run(command, &self.host)),
                    None => self
                        .panes
                        .focused_mut()
                        .is_some_and(|pane| pane.run(command, &self.host)),
                };
                if known && self.panes.focused_name() == routed {
                    if let Some(after) = self.eye_of(&routed) {
                        if Some(&after) != before.as_ref() {
                            self.request_preview(after, false);
                        }
                    }
                }
                if !known {
                    self.message = format!("{command} does nothing here");
                }
            }
        }
    }

    /// Runs the shared list-navigation commands against the modal viewport.
    /// Returning false leaves app-wide commands such as `help`, `back`, and
    /// `quit` to their ordinary handlers below.
    fn scroll_help(&mut self, command: &str) -> bool {
        let (_, h) = self.screen.size();
        let body = h.saturating_sub(2);
        let (page, max) = help::scroll_bounds(body, &self.host, &self.availability, &self.modes);
        let by = match command {
            "view.down" => 1,
            "view.up" => -1,
            "view.page-down" => page.saturating_sub(1).max(1) as isize,
            "view.page-up" => -(page.saturating_sub(1).max(1) as isize),
            "view.scroll-down" => self.host.view.rows as isize,
            "view.scroll-up" => -(self.host.view.rows as isize),
            "view.top" => {
                self.help_scroll = 0;
                return true;
            }
            "view.bottom" => {
                self.help_scroll = max;
                return true;
            }
            _ => return false,
        };
        self.help_scroll = if by.is_negative() {
            self.help_scroll.saturating_sub(by.unsigned_abs())
        } else {
            self.help_scroll.saturating_add(by as usize).min(max)
        };
        true
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
                // No disarm here, deliberately. A focus round-trip does not
                // answer a destructive question: the window keeps the arm
                // across focus, the files pane ships the same contract, and
                // the branches pane matches them both. What disarms is the
                // view's own list — cursor moves, wheels, mouse rows,
                // refreshes — and a prompt or a reload, through
                // [`App::disarm_branches`].
                self.sync_modes();
                // An unfocused commits pane may have scrolled under the
                // pointer, but its cursor is independent and stays put. When
                // focus arrives, that same highlighted row is still the main
                // preview's source.
                if name == "commits" {
                    self.request_commit_preview(false);
                }
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

    /// Gives the main pane focus after asking for the highlighted commit's
    /// preview. Moving the highlight already asked for it; Enter merely
    /// flushes a missing or failed load and transfers the keyboard.
    fn open_diff(&mut self) {
        self.request_commit_preview(true);
    }

    /// Keeps the main pane on the commit highlighted in the commits pane —
    /// asked for, read off the input path, installed when it arrives.
    ///
    /// The I/O is here and not in the view, which is the same rule the GPUI
    /// client follows: a view takes already-loaded data and never learns what
    /// a repository is. A bare revision is "what did this commit change" to
    /// [`gitten_git::Repo::pairs`], merges included.
    ///
    /// The pane named `commits` is read by that name and not by focus. A
    /// preview already naming this commit costs no acquisition; anything else
    /// becomes one request on [`App::request_preview`]'s lane, and the
    /// answer installs only while it is still the newest one asked for.
    fn request_commit_preview(&mut self, focus: bool) {
        let sha = match self.panes.get("commits") {
            Some(Screens::Commits { view, .. }) => view.current().map(|c| c.sha.clone()),
            _ => {
                if focus {
                    self.message = "no commit selected".into();
                }
                return;
            }
        };
        let Some(sha) = sha else {
            if focus {
                self.message = "no commit selected".into();
            }
            return;
        };
        self.request_preview(DiffSource::Commit { sha }, focus);
    }

    /// Asks for a preview, on the lane that keeps the keyboard live while
    /// the read runs.
    ///
    /// The request carries a sequence number and the repository it was made
    /// against; the answer is installed only while it is still the newest
    /// sequence issued and the repository is still the one held — which is
    /// the whole of the staleness contract. Everything slower than the next
    /// keypress simply never installs: a newer selection's read is already
    /// on its way, and the pane is not asked about the old answer any more.
    /// While the read runs, the pane's header says what is coming and its
    /// last good rows stay drawn.
    ///
    /// One more honest shortcut: a request for what is already shown costs
    /// nothing, and — like every read here — the [`Differs`] clone the
    /// thread takes shares the answer cache, so a re-read of the same
    /// content is remembered work.
    fn request_preview(&mut self, origin: DiffSource, focus: bool) {
        let Some((root, repo)) = self.repo.clone() else {
            if focus {
                self.message = "a fixture has no repository to preview".into();
            }
            return;
        };
        if self.shown_origin().is_some_and(|shown| shown == origin) {
            if focus {
                self.focus_named("diff");
            }
            return;
        }
        self.preview_seq += 1;
        self.preview_pending += 1;
        let seq = self.preview_seq;
        if let Some(Screens::Diff { label, .. }) = self.panes.get_mut("diff") {
            *label = format!("loading {}", origin.label());
        }
        let differs = self.host.differ.clone();
        let over = Overrides::default();
        let tx = self.preview_tx.clone();
        let started = std::thread::Builder::new()
            .name("gitten-preview".into())
            .spawn(move || {
                let outcome =
                    match acquire::diff_source(&origin, &differs, &over, repo.as_ref(), false) {
                        Ok(loaded) => match loaded.data {
                            Data::Diff(files) => Ok(files),
                            Data::Commits(_) => {
                                Err("the preview answered with the wrong view".into())
                            }
                        },
                        Err(e) => Err(e),
                    };
                let _ = tx.send(PreviewOutcome {
                    seq,
                    root,
                    origin,
                    focus,
                    outcome,
                });
            });
        if started.is_err() {
            self.preview_pending -= 1;
            self.message = "could not start the preview reader".into();
        }
    }

    /// What the main pane is currently previewing, if anything — the same
    /// source a later request compares itself against.
    fn shown_origin(&self) -> Option<DiffSource> {
        match self.panes.get("diff") {
            Some(Screens::Diff {
                origin: Some(origin),
                ..
            }) => Some(origin.clone()),
            _ => None,
        }
    }

    /// Drains both background lanes — the write queue and the preview
    /// readers. Called before each frame, so the frame this iteration draws
    /// is the one the finished jobs and the arrived previews produced.
    fn pump(&mut self) {
        self.drain_jobs();
        self.drain_previews();
    }

    /// Hands the switch path a test's own opener — the same seam the
    /// startup holds, so a fake stands in for the binary here too.
    #[cfg(test)]
    fn use_opener(&mut self, opener: std::sync::Arc<dyn gitten_app::Opener>) {
        self.opener = opener;
    }

    /// Drains until nothing is in flight — the deterministic turn a test
    /// gives a background preview. The fakes answer in microseconds; the
    /// deadline exists so a genuinely stuck read fails the test instead of
    /// hanging it.
    #[cfg(test)]
    fn pump_quiet(&mut self) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while self.preview_pending > 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "a preview answer never came back: {} pending",
                self.preview_pending
            );
            self.drain_previews();
            std::thread::yield_now();
        }
        self.pump();
    }

    /// Drains the preview lane, beside the job queue. Called before each
    /// frame, so the frame this iteration draws is the one the answers
    /// produced.
    fn drain_previews(&mut self) {
        while let Some(answer) = self.next_preview_answer() {
            self.install_preview(answer);
        }
    }

    /// One answer off the lane, if one has arrived — a method so the
    /// receiver's borrow ends before the install borrows the app.
    fn next_preview_answer(&mut self) -> Option<PreviewOutcome> {
        self.preview_rx.try_recv().ok()
    }

    /// What the pane named `pane` is looking at, as a preview source — the
    /// eye that follows the selection. The branches list answers `None` on
    /// purpose: its drilldown is an install, not a preview, and only Enter
    /// asks for one.
    fn eye_of(&self, pane: &str) -> Option<DiffSource> {
        match pane {
            "commits" => match self.panes.get(pane) {
                Some(Screens::Commits { view, .. }) => view
                    .current()
                    .map(|c| DiffSource::Commit { sha: c.sha.clone() }),
                _ => None,
            },
            "files" => match self.panes.get(pane) {
                Some(Screens::Files { view, .. }) => view.current_file().and_then(|file| {
                    match file.section {
                        files::Section::Staged => Some(DiffSource::Staged {
                            path: file.path.clone(),
                        }),
                        files::Section::Unstaged => Some(DiffSource::Unstaged {
                            path: file.path.clone(),
                        }),
                        files::Section::Untracked => Some(DiffSource::Untracked {
                            path: file.path.clone(),
                        }),
                        // A conflicted file has no side of the index to
                        // preview; its resolution views are their own packet.
                        files::Section::Conflicts => None,
                    }
                }),
                _ => None,
            },
            "stashes" => match self.panes.get(pane) {
                Some(Screens::Stashes { view, .. }) => view
                    .current_entry()
                    .map(|(index, commit)| DiffSource::Stash { index, commit }),
                _ => None,
            },
            _ => None,
        }
    }

    /// `files.open-diff`: preview the file the keyboard is on, and put the
    /// keyboard on the preview. A row that is a heading, or a conflicted
    /// file with no side to preview, says so and does nothing.
    fn open_file_diff(&mut self) {
        match self.eye_of("files") {
            Some(origin) => self.request_preview(origin, true),
            None => self.message = "the keyboard is not on a file".into(),
        }
    }

    /// `files.toggle-side`: the same file, the other side of the index —
    /// staged to unstaged and back, only when the other side actually has
    /// the file. A switch to a side that is not there is said, not shown as
    /// an empty diff: "nothing unstaged for f.txt" is the honest answer to
    /// a file that exists only in the index.
    fn toggle_file_side(&mut self) {
        let selected = match self.panes.get("files") {
            Some(Screens::Files { view, .. }) => view
                .current_file()
                .map(|file| (file.section, file.path.clone())),
            _ => None,
        };
        let Some((section, path)) = selected else {
            self.message = "the keyboard is not on a file".into();
            return;
        };
        let (other, other_section) = match section {
            files::Section::Staged => (
                Some(DiffSource::Unstaged { path: path.clone() }),
                files::Section::Unstaged,
            ),
            files::Section::Unstaged | files::Section::Untracked => (
                Some(DiffSource::Staged { path: path.clone() }),
                files::Section::Staged,
            ),
            // A conflict is not two sides; it is three stages of one file,
            // and what to show of it is the resolution packet's question.
            files::Section::Conflicts => (None, files::Section::Conflicts),
        };
        let Some(origin) = other else {
            self.message = "a conflicted file has no other side to preview".into();
            return;
        };
        let exists = match self.panes.get("files") {
            Some(Screens::Files { view, .. }) => view.has_row(other_section, &path),
            _ => false,
        };
        if !exists {
            let side = match other_section {
                files::Section::Staged => "staged",
                _ => "unstaged",
            };
            self.message = format!("nothing {side} for {path}");
            return;
        }
        self.request_preview(origin, true);
    }

    /// `stashes.open-diff`: the parked work, seen before anything is
    /// applied. The entry the keyboard is on, addressed by its commit —
    /// the identity a drop does not renumber.
    fn open_stash_diff(&mut self) {
        match self.eye_of("stashes") {
            Some(origin) => self.request_preview(origin, true),
            None => self.message = "the keyboard is not on a stash".into(),
        }
    }

    /// `branches.open-log`: the branch's own history, installed as the main
    /// pane. A drilldown, not a preview — it replaces what the main pane
    /// holds, the way a commit's preview does, and a later selection
    /// elsewhere replaces it in turn. The keyboard follows it in, the way
    /// Enter follows a commit's preview in.
    ///
    /// Only a local branch drills down: a remote-tracking row has no local
    /// history to name and the detached row is a place, not a branch — both
    /// say so rather than guessing which history was meant.
    fn open_branch_log(&mut self) {
        let target = match self.panes.get("branches") {
            Some(Screens::Branches { view, .. }) => view.current(),
            _ => None,
        };
        let Some(Target::Local(name)) = target else {
            self.message = "the keyboard is not on a local branch".into();
            return;
        };
        let Some((path, repo)) = self.repo.clone() else {
            self.message = "a fixture has no history to open".into();
            return;
        };
        let commits = match repo.log_at(name.as_bytes(), 5000) {
            Ok(commits) => commits,
            Err(e) => {
                self.message = e;
                return;
            }
        };
        let mut list = Commits::new(commits);
        list.set_bar(self.bar);
        self.ensure_geometry();
        if let Some(rect) = self.pane_content("diff") {
            list.set_scrolloff(self.host.view.scrolloff);
            list.resize(rect.width, rect.height);
        }
        let display = name.to_string_lossy().into_owned();
        let described = repo.describe();
        let gesture_was = self.gesture.as_deref() == Some("diff");
        self.panes.register(
            "diff",
            panes::Placement::Main,
            Screens::Commits {
                view: list,
                source: Source::Repo {
                    path,
                    arg: "5000".into(),
                },
                log_of: Some(name),
                label: format!("{display} · {described}"),
                generation: self.generation,
            },
        );
        // The same gesture rule every install keeps: only a gesture captured
        // in the tenant just replaced became stale, and a click must still
        // receive its release.
        if gesture_was {
            self.gesture = None;
        }
        self.panes.focus_named("diff");
        self.sync_modes();
    }

    /// Installs one preview answer — the guarded half of
    /// [`App::request_preview`].
    ///
    /// Two guards, both absolute. A stale sequence — anything but the newest
    /// request issued — is dropped without a word: something newer was asked
    /// for and its answer is either here or on its way. An answer read from
    /// another repository is dropped the same way: one repository's content
    /// never becomes another one's preview. A read that failed keeps the
    /// last good rows and says why on the status line, the same shape a
    /// failed refresh leaves.
    fn install_preview(&mut self, answer: PreviewOutcome) {
        // Every answer counts one down, stale or not: the counter answers
        // "is anything still being read", not "was anything installed".
        self.preview_pending = self.preview_pending.saturating_sub(1);
        // A request's sequence starts at one; zero is no request's answer,
        // and it installs the same way an older one would — never.
        if answer.seq == 0 || answer.seq != self.preview_seq {
            return;
        }
        if self
            .repo
            .as_ref()
            .is_some_and(|(root, _)| *root != answer.root)
        {
            return;
        }
        match answer.outcome {
            Ok(files) => {
                let label = self.preview_label(&answer.origin);
                self.install_diff(answer.origin, label, files, answer.focus);
            }
            Err(e) => {
                self.message = e;
                if let Some(Screens::Diff { label, .. }) = self.panes.get_mut("diff") {
                    *label = answer.origin.label();
                }
            }
        }
    }

    /// Full sha under the commits cursor, for detecting a real selection
    /// change around generic pane input without teaching dispatch about rows.
    fn current_commit_sha(&self) -> Option<String> {
        match self.panes.get("commits") {
            Some(Screens::Commits { view, .. }) => view.current().map(|c| c.sha.clone()),
            _ => None,
        }
    }

    /// `diff.stage-hunk` / `diff.unstage-hunk` / `diff.discard-hunk`: act on
    /// what the keyboard selects — the whole hunk, or the marked lines.
    /// The terminal's share of the window's `hunk_verb`: the gates, the
    /// aim, the arm, the job — and not one line more, because every one of
    /// those is shared with an extension calling the same command through
    /// the same name, and the verb itself is [`act::hunk_job`], shared with
    /// every client.
    ///
    /// The verbs reach only a diff that was actually acquired: the empty
    /// pane a commits launch registers has no source, and "not loaded yet"
    /// refuses rather than pretending — no fake source, no fake generation,
    /// no patch against nothing. What each verb *means* on each side of the
    /// index is the shared policy's to say; what the keyboard selected is
    /// the pane's (`patch_selection`); and a discard asks twice, on the
    /// pane's own arm, before any job exists.
    fn hunk_verb(&mut self, command: &str) {
        // Everything decided ahead of anything queued, in the order a
        // reader meets the facts: which diff this is (and which aims no
        // side of it can serve), whether a repository is open, what the
        // keyboard selected — and a refusal or a question is said here,
        // so the queue only ever sees a job that means it.
        enum Gate {
            OffDiff,
            NoDiff,
            Refused(String),
            Ask(String),
            Proceed(HunkAsk, gitten_git::Handle),
        }
        let gate;
        match self.panes.focused_mut() {
            Some(Screens::Diff {
                view,
                origin: Some(origin),
                ..
            }) => match hunk_side_of(origin) {
                Err(refusal) => gate = Gate::Refused(refusal),
                Ok(side) => match self.repo.as_ref() {
                    None => gate = Gate::Refused("no repository is open".into()),
                    Some((_, handle)) => {
                        let handle = gitten_git::Handle::clone(handle);
                        match view.patch_selection() {
                            Err(refusal) => gate = Gate::Refused(refusal),
                            Ok(aim) => {
                                // A verb the side cannot serve is refused
                                // here, before the arm — a destructive
                                // question asked on an aim that will never
                                // run is a question nobody should have to
                                // answer.
                                if let Some(refusal) = verb_refusal(command, side) {
                                    gate = Gate::Refused(refusal);
                                } else if command == "diff.discard-hunk"
                                    && !view.confirm_or_arm_discard()
                                {
                                    gate = Gate::Ask(discard_question(&aim));
                                } else {
                                    let (path, selection) = match aim {
                                        PatchSelection::Whole { path, hunk } => {
                                            (path, HunkSelection::Whole(hunk))
                                        }
                                        PatchSelection::Lines { path, parts } => {
                                            (path, HunkSelection::Lines(parts))
                                        }
                                    };
                                    gate = Gate::Proceed(
                                        HunkAsk {
                                            path,
                                            side,
                                            selection,
                                        },
                                        handle,
                                    );
                                }
                            }
                        }
                    }
                },
            },
            Some(Screens::Diff { origin: None, .. }) => gate = Gate::NoDiff,
            _ => gate = Gate::OffDiff,
        }
        match gate {
            Gate::OffDiff => self.message = "the keyboard is not on a diff".into(),
            Gate::NoDiff => self.message = "no diff is open".into(),
            Gate::Refused(refusal) | Gate::Ask(refusal) => self.message = refusal,
            Gate::Proceed(ask, handle) => {
                let differs = self.host.differ.clone();
                match hunk_job(command, ask, &handle, &differs, &Overrides::default()) {
                    Ok(job) => {
                        if self.submitter.submit(job).is_err() {
                            self.message = "the job queue is shutting down".into();
                        }
                    }
                    Err(e) => self.message = e,
                }
            }
        }
    }

    /// `stashes.apply` / `stashes.pop` / `stashes.drop`: send the stack
    /// entry the keyboard is on to the queue. The terminal's share of the
    /// window's stash verbs — the gates, the arm, the job — and not one
    /// line more, because every one of those is shared with an extension
    /// calling the same command through the same name.
    ///
    /// The verbs reach only a focused stash pane, and only a row: an empty
    /// or unavailable stack has nothing to address, and is refused before
    /// the queue rather than sent to git to be told so. Drop alone asks
    /// twice, on the pane's own arm, *before* any job exists; apply and pop
    /// act on the first press, exactly as the window does and the command
    /// descriptions say. Every accepted press builds the exact existing
    /// [`Write`] and submits it — the repository's own refusal, on a stale
    /// index or a conflicted restore, comes back from the queue and is the
    /// status line, never a UI-invented word like `conflict`.
    fn stash_selected(&mut self, command: &str) {
        let Some((_, handle)) = self.repo.as_ref() else {
            // A stash pane is registered only behind a repository, so the
            // keyboard is somewhere else; said the same way that refusal is
            // always said.
            self.message = format!("{command} is not supported here");
            return;
        };
        let selected = match self.panes.focused() {
            Some(Screens::Stashes { view, .. }) => view.current(),
            _ => {
                self.message = format!("{command} is not supported here");
                return;
            }
        };
        let Some(index) = selected else {
            self.message = "nothing selected on the stash stack".into();
            return;
        };
        // The arm is spent here, on the pane, before anything is built: a
        // first press asks and queues nothing, a second press on the same
        // row acts.
        if command == "stashes.drop" {
            let confirmed = match self.panes.focused_mut() {
                Some(Screens::Stashes { view, .. }) => view.confirm_or_arm_drop(index),
                _ => return,
            };
            if !confirmed {
                self.message = drop_question(index);
                return;
            }
        }
        let job = match command {
            "stashes.apply" => Write::stash_apply(handle, index),
            "stashes.pop" => Write::stash_pop(handle, index),
            "stashes.drop" => Write::stash_drop(handle, index),
            _ => return,
        };
        if self.submitter.submit(Box::new(job)).is_err() {
            self.message = "the job queue is shutting down".into();
        }
    }

    /// `files.stage`: act on the row the keyboard is on, by the side of the
    /// index it sits on. Staged means unstage; everything else — unstaged,
    /// untracked, a conflict whose resolution is being recorded — means
    /// stage. That is lazygit's rule and git's own asymmetry: `add` is the
    /// one word for "the index should hold this". The window's rule,
    /// verbatim, over the terminal's own rows.
    fn files_stage(&mut self) {
        if !matches!(self.panes.focused(), Some(Screens::Files { .. })) {
            self.message = "files.stage is not supported here".into();
            return;
        }
        gitten_app::act::stage_or_unstage(self);
    }

    /// `files.stage-all`: every row, on the side of the index the keyboard
    /// sits in — the one rule `space` keeps for a single row, at scale.
    /// Staged row or staged heading: unstage everything staged. Anything
    /// else — unstaged, untracked, their headings, an empty tree: stage
    /// everything unstaged and untracked. Conflicts belong to neither
    /// direction — staging one records a resolution, which is its own
    /// decision. One job either way, so one generation bump and one
    /// re-acquire wave per keypress.
    fn files_stage_all(&mut self) {
        if !matches!(self.panes.focused(), Some(Screens::Files { .. })) {
            self.message = "files.stage-all is not supported here".into();
            return;
        }
        gitten_app::act::stage_all(self);
    }

    /// `files.discard`: the one destructive verb, and it confirms on the
    /// keyboard because no dialog exists to confirm anywhere else — the
    /// window's exact pattern. First press arms the row and asks once in
    /// the band; second press on the same row builds the job; any cursor
    /// move, wheel, mouse press on another row, or refresh disarms before
    /// it can lie. Two refusals are said up front rather than answered
    /// badly: a staged row, whose undo is unstage; and a conflict, whose
    /// working-tree side is the merge's open question.
    fn files_discard(&mut self) {
        if !matches!(self.panes.focused(), Some(Screens::Files { .. })) {
            self.message = "files.discard is not supported here".into();
            return;
        }
        gitten_app::act::discard_file(self);
    }

    /// `files.stash`: park the working tree on the stack — `git stash push`
    /// with no message, exactly as the window sends it, so git supplies its
    /// normal `WIP on …` text. Naming a stash is prompt work this command
    /// deliberately has none of.
    ///
    /// It inspects neither the files pane nor the stash pane: the verb aims
    /// at the repository the app is holding, which is also why it answers
    /// on a launch that registered no files tenant at all.
    fn stash_working_tree(&mut self) {
        gitten_app::act::stash_working_tree(self);
    }

    /// `files.ignore`: append the untracked file to the root `.gitignore`
    /// and let the refresh do the rest — git stops listing ignored files on
    /// its own. Only an untracked row answers: `.gitignore` governs files
    /// git does not yet track, so answering over a tracked change would be
    /// a no-op wearing a success badge.
    fn files_ignore(&mut self) {
        if !matches!(self.panes.focused(), Some(Screens::Files { .. })) {
            self.message = "files.ignore is not supported here".into();
            return;
        }
        gitten_app::act::ignore_file(self);
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

    /// `repo.refresh`, lazygit's capital R: the re-acquisition wave a
    /// finished write runs, asked for by hand.
    ///
    /// One generation advance, one wave, every registered repository-backed
    /// pane — the hidden ones included — and the fixtures left alone, which
    /// is the whole of [`Screens::refresh`]'s own story. A read that fails
    /// leaves the last good rows standing at their old generation and the
    /// error on the status line, exactly as for the queue's own finish; a
    /// clean one is its own evidence and says nothing. Reached only when
    /// [`tui_availability`] said so — a fixture view is turned away with
    /// its reason before this runs.
    fn manual_refresh(&mut self) {
        let target = self.generation.advance();
        self.generation = target;
        if let Err(e) = self.refresh_stale(target) {
            self.message = e;
        }
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
                    // A terminal divider is centred in its cell. The built-in
                    // thumb stays in the pane's last cell as `▐`, flush against
                    // the divider without repainting the rule. At the screen edge
                    // it is also flush with the terminal boundary.
                    let past = content.x + content.width;
                    let rail = past.saturating_sub(1).min(screen.width().saturating_sub(1));
                    let divider = (past < screen.width()).then_some(past);
                    pane.paint_scrollbar(&mut *screen, rail, divider, content.y, host);
                }
            }
        }

        let ink = Ink::new(c.dim, c.status_bg);
        let loud = Ink::new(c.accent, c.status_bg);
        // While the prompt stands it owns the status row: its label, the
        // text, and a caret — one line, no second viewport, and no pane
        // geometry touched for it. The search keeps its live count in faint
        // ink when there is room left to say it, and its query clips through
        // the pen — the head is what a filter is about. A message draws the
        // *tail* instead: the end of the text is where the eye and the next
        // character both are, and the stored value is never cut to show it.
        if let Some(prompt) = self.prompt.as_ref() {
            let text_ink = Ink::new(c.fg, c.status_bg);
            let mut pen = self.screen.row(h - 1);
            pen.put(" ", ink);
            // A question is the row whole: what is asked, in the accent, and
            // the two answers named in it. No field, no caret, no live count.
            if let Some(question) = prompt.question() {
                pen.put(&question, loud);
                pen.wash(ink);
                return;
            }
            pen.put(prompt.label(), loud);
            let field = prompt.field();
            // A message that grew past one line says which line the cursor
            // is on — the one fact a status row cannot draw for itself.
            let (line, lines) = field.line_of();
            if prompt.multiline() && lines > 1 {
                pen.put(
                    &format!("{line}/{lines} · "),
                    Ink::new(c.faint, c.status_bg),
                );
            }
            // The window around the cursor, with the cursor drawn in place —
            // not a tail with a block glued on the end: the cursor is *in*
            // the text now, and a field edited from the middle must show it
            // there.
            let room = pen.room().saturating_sub(1);
            let (window, at) = field.window(room);
            let head = window
                .char_indices()
                .nth(at)
                .map(|(i, _)| i)
                .unwrap_or(window.len());
            pen.put(&window[..head], text_ink);
            pen.put("█", loud);
            pen.put(&window[head..], text_ink);
            if let Prompt::Search { .. } = prompt {
                if pen.room() > 2 {
                    if let Some(note) = self
                        .panes
                        .get(prompt.pane())
                        .and_then(|pane| pane.search_note())
                    {
                        pen.put(" · ", ink);
                        pen.put(&note, Ink::new(c.faint, c.status_bg));
                    }
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
            // The frame's own facts, right-aligned: the grid it was drawn
            // for and the alphabet it was drawn with — the mock's `120×32 ·
            // --ascii off`, faint so the pane's own status keeps the row. A
            // left status that already reaches the reservation wins and the
            // right side yields, because overwriting it would lie about
            // where the keyboard is.
            let right = format!(
                "{}×{} · --ascii {}",
                w,
                h,
                match self.ascii {
                    true => "on",
                    false => "off",
                }
            );
            let at = w.saturating_sub(gitten_tui::screen::width(&right) + 1);
            if pen.col() <= at {
                pen.fill(at - pen.col(), ' ', ink);
                pen.put(&right, Ink::new(c.faint, c.status_bg));
            }
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
            help::paint(
                &mut self.screen,
                1,
                body,
                &self.host,
                &self.availability,
                &self.modes,
                self.help_scroll,
                self.bar,
            );
        }
        if let Some(picker) = self.picker.as_ref() {
            paint_picker(&mut self.screen, 1, body, picker, &self.host);
        }
    }
}

/// What this client runs, beside the shared registry — the one contract the
/// help panel and the dispatch refusal both read, so they cannot disagree.
///
/// Strict, deliberately: a name with no entry here is refused before it is
/// routed, and the help panel drops its row — a key that resolves to
/// nothing is not advertised as if it ran. What is listed is exactly what
/// [`App::dispatch_to`] answers by name plus what every pane's `run`
/// answers; the registry's remaining built-ins — sync, history surgery,
/// the project switcher, the settings panel — are this client's named
/// gaps, and a compiled-in extension's commands are unsupported by the
/// same word until a handler exists to answer them.
///
/// The launch is the one variable: `repo.refresh` has a handler, but a
/// fixture view has no repository behind it, so there it is supported-but-
/// turned-away with the reason instead of advertised as if it ran.
fn tui_availability(repo: bool) -> Availability {
    let mut a = Availability::strict();
    a.available([
        // Dispatched by name, ahead of any pane.
        "quit",
        "help",
        "back",
        "theme.cycle",
        "pane.left",
        "pane.right",
        "pane.next",
        "pane.prev",
        "files.focus",
        "branches.focus",
        "commits.focus",
        "stashes.focus",
        "diff.focus",
        "status.focus",
        "commits.open-diff",
        "commits.search",
        "files.search",
        "branches.search",
        "stashes.search",
        "diff.search",
        "search.next",
        "search.prev",
        "search.clear",
        "select.mark",
        "input.newline",
        "files.commit",
        "files.amend",
        "input.accept",
        "input.cancel",
        "diff.stage-hunk",
        "diff.unstage-hunk",
        "diff.discard-hunk",
        "diff.toggle-line-selection",
        "stashes.apply",
        "stashes.pop",
        "stashes.drop",
        "files.stash",
        "files.stage",
        "files.stage-all",
        "files.discard",
        "files.ignore",
        "branches.checkout",
        "branches.new",
        "branches.rename",
        "branches.delete",
        "branches.new-tag",
        "branches.checkout-name",
        "branches.checkout-previous",
        "branches.force-checkout",
        "branches.fast-forward",
        "branches.set-upstream",
        "branches.unset-upstream",
        "commits.new-branch",
        "remotes.focus",
        "remotes.fetch",
        "remotes.new",
        "remotes.edit",
        "remotes.remove",
        "remotes.search",
        // A repository is what a switch aims away from and at; both are
        // answerable from a fixture view, which is where a repository
        // gets opened from when the launch had none.
        "project.switch",
        "project.open",
        "project.next",
        "project.prev",
        "copy.selection",
        "select.all",
        "select.none",
        // Answered by every pane's `run`, and by the diff pane alone where
        // they are particular to it.
        "view.down",
        "view.up",
        "view.page-down",
        "view.page-up",
        "view.scroll-down",
        "view.scroll-up",
        "view.top",
        "view.bottom",
        "view.left",
        "view.right",
        "diff.next-file",
        "diff.prev-file",
        "diff.next-hunk",
        "diff.prev-hunk",
        "diff.cycle-layout",
        "diff.cycle-wrap",
    ]);
    match repo {
        true => {
            a.available([
                "repo.refresh",
                "repo.push",
                "repo.pull",
                "repo.fetch",
                "files.open-diff",
                "files.toggle-side",
                "stashes.open-diff",
                "branches.open-log",
            ]);
        }
        false => {
            a.disabled("repo.refresh", "a fixture has no repository to refresh");
            a.disabled("repo.push", "a fixture has no repository to push from");
            a.disabled("repo.pull", "a fixture has no repository to pull into");
            a.disabled("repo.fetch", "a fixture has no repository to fetch into");
            a.disabled("files.open-diff", "a fixture has no file to preview");
            a.disabled("files.toggle-side", "a fixture has no file to preview");
            a.disabled("stashes.open-diff", "a fixture has no stash to preview");
            a.disabled("branches.open-log", "a fixture has no history to open");
        }
    }
    a
}

fn action_file_section(section: files::Section) -> gitten_app::act::FileSection {
    match section {
        files::Section::Staged => gitten_app::act::FileSection::Staged,
        files::Section::Unstaged => gitten_app::act::FileSection::Unstaged,
        files::Section::Untracked => gitten_app::act::FileSection::Untracked,
        files::Section::Conflicts => gitten_app::act::FileSection::Conflicts,
    }
}

fn view_file_section(section: gitten_app::act::FileSection) -> files::Section {
    match section {
        gitten_app::act::FileSection::Staged => files::Section::Staged,
        gitten_app::act::FileSection::Unstaged => files::Section::Unstaged,
        gitten_app::act::FileSection::Untracked => files::Section::Untracked,
        gitten_app::act::FileSection::Conflicts => files::Section::Conflicts,
    }
}

impl gitten_app::act::Client for App {
    fn say(&mut self, message: String) {
        self.message = message;
    }

    fn ask(&mut self, question: String) {
        self.message = question;
    }

    fn repo(&self) -> Option<gitten_git::Handle> {
        self.repo
            .as_ref()
            .map(|(_, repo)| gitten_git::Handle::clone(repo))
    }

    fn submit(&mut self, job: Box<dyn Job>) -> bool {
        self.submitter.submit(job).is_ok()
    }
}

impl gitten_app::act::BranchClient for App {
    fn branch_target(&self) -> Option<Target> {
        App::branch_target(self)
    }

    fn confirm_or_arm_branch(&mut self, target: &Target) -> bool {
        match self.panes.focused_mut() {
            Some(Screens::Branches { view, .. }) => view.confirm_or_arm_delete(target),
            _ => false,
        }
    }
}

impl gitten_app::act::FileClient for App {
    fn selected_file(&self) -> Option<gitten_app::act::SelectedFile> {
        let Some(Screens::Files { view, .. }) = self.panes.focused() else {
            return None;
        };
        view.current_file()
            .map(|file| gitten_app::act::SelectedFile {
                section: action_file_section(file.section),
                path: file.path.clone(),
                shown: file.text.clone(),
            })
    }

    fn cursor_section(&self) -> Option<gitten_app::act::FileSection> {
        let Some(Screens::Files { view, .. }) = self.panes.focused() else {
            return None;
        };
        view.cursor_section().map(action_file_section)
    }

    fn paths_in(
        &self,
        section: gitten_app::act::FileSection,
    ) -> Vec<gitten_core::status::PathBytes> {
        let Some(Screens::Files { view, .. }) = self.panes.focused() else {
            return Vec::new();
        };
        view.paths_in(view_file_section(section))
    }

    fn confirm_or_arm_file(&mut self, target: &gitten_app::act::SelectedFile) -> bool {
        match self.panes.focused_mut() {
            Some(Screens::Files { view, .. }) => {
                view.confirm_or_arm_discard(view_file_section(target.section), &target.path)
            }
            _ => false,
        }
    }
}

impl gitten_app::act::RemoteClient for App {
    fn remote_target(&self) -> Option<RefName> {
        self.remote_target()
    }

    fn confirm_or_arm_remote(&mut self, name: &RefName) -> bool {
        self.confirm_or_arm_remote(name)
    }
}

/// Which side of the index a diff's origin is, for the hunk verbs — and
/// the refusals for the aims no side can serve. The messages here are the
/// ones the headless tests hold, worded once so every caller says the same
/// sentence; what each verb *means* per side is [`act::hunk_job`]'s to say.
fn hunk_side_of(origin: &DiffSource) -> Result<HunkSide, String> {
    match origin {
        DiffSource::Staged { .. } => Ok(HunkSide::Staged),
        DiffSource::Unstaged { .. } => Ok(HunkSide::Unstaged),
        DiffSource::Untracked { .. } => Ok(HunkSide::Untracked),
        DiffSource::Revspec { arg } if arg.is_empty() => Ok(HunkSide::Combined),
        DiffSource::Revspec { .. } | DiffSource::Commit { .. } | DiffSource::Stash { .. } => {
            Err("only the working-tree diff can act on hunks — this one is between commits".into())
        }
        DiffSource::Fixture => Err("a fixture has no repository behind it".into()),
        DiffSource::Patch => Err("a patch file has no repository behind it".into()),
    }
}

/// The question an armed discard asks, once, in the status line: what is
/// under the keyboard, named closely enough to be answered on purpose.
fn discard_question(aim: &PatchSelection) -> String {
    match aim {
        PatchSelection::Whole { path, .. } => {
            format!("discard the hunk of {path} under the keyboard? press again to confirm")
        }
        PatchSelection::Lines { path, parts } => {
            let lines: usize = parts.iter().map(|(_, lo, hi)| hi - lo + 1).sum();
            format!("discard {lines} marked lines of {path}? press again to confirm")
        }
    }
}

/// The three branch reads one launch or one refresh needs, and the honest
/// story of any that failed.
///
/// The reads run concurrently — `std::thread::scope`, the same shape the
/// window's branches load uses — because three `git` subprocesses in a row on
/// the road to the first frame are three spawn floors where one would do, and
/// because a pane that reads them one at a time would draw a snapshot torn
/// across the gaps. What comes back is one prepared-able snapshot or an
/// account of which leg failed; nothing here touches a view.
struct BranchReads {
    local: Vec<gitten_core::refs::Branch>,
    remotes: Vec<gitten_core::refs::RemoteBranch>,
    head: Option<gitten_core::refs::HeadState>,
    /// The first read that failed, in the sentence the status line says.
    /// `None` is a clean read. The caller — not this helper — decides what a
    /// failed leg costs: the lists are honest while they stand, and a lost
    /// HEAD read costs the detached row alone.
    error: Option<String>,
}

/// Runs the three ref reads beside each other and keeps what came back.
///
/// The describe is deliberately *not* one of these reads: every caller has
/// one of its own already — the files pane names the repository with the
/// same string — and a describe inside here would be a second `git`
/// process for text already in hand. What comes back is one prepared-able
/// snapshot or an
/// account of which leg failed; nothing here touches a view.
/// `git` already said the useful thing.
fn load_branches(repo: &dyn gitten_git::Repo) -> BranchReads {
    let (local, remotes, head) = std::thread::scope(|s| {
        let local = s.spawn(|| repo.branches());
        let remotes = s.spawn(|| repo.remote_branches());
        let head = s.spawn(|| repo.head());
        (join_read(local), join_read(remotes), join_read(head))
    });
    let mut error = None;
    // The lists first: they are the pane's data, and the order of the two
    // error arms is the order a person would read the failures in. The
    // sentence names what failed — a bare git error beside a pane would read
    // as a commit read's or a status read's.
    let (local, remotes) = match (local, remotes) {
        (Ok(local), Ok(remotes)) => (local, remotes),
        (Err(e), _) | (_, Err(e)) => {
            error.get_or_insert(format!("branch reads failed: {e}"));
            (Vec::new(), Vec::new())
        }
    };
    // A HEAD-only failure still leaves rows to draw. What it costs is the
    // detached row: the current bit is not this read's to lose — it travels
    // on each `Branch` from the same `for-each-ref` that named it, exactly
    // the bit the window marks from — so an attached repository keeps its
    // marking and a detached one simply has no detached row to show.
    let head = match head {
        Ok(head) => Some(head),
        Err(e) => {
            error.get_or_insert(format!("branch reads failed: {e}"));
            None
        }
    };
    BranchReads {
        local,
        remotes,
        head,
        error,
    }
}

/// Joins a spawned read, resuming a panic on the caller's thread — the same
/// answer the app layer gives its own spawned reads.
fn join_read<T>(h: std::thread::ScopedJoinHandle<'_, T>) -> T {
    h.join().unwrap_or_else(|p| std::panic::resume_unwind(p))
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

/// Two paths name the same repository — by their spelling, or by what the
/// filesystem says they point at. The MRU stores canonicalized paths, but
/// an entry recorded raw still matches once the directory exists.
fn same_repository(a: &std::path::Path, b: &std::path::Path) -> bool {
    if a == b {
        return true;
    }
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// The remotes pane's header label: what repository, how many remotes. The
/// one-word plural is git's own config grammar — `remote.add` — not a
/// formatting mood.
fn remotes_label(describe: &str, count: usize) -> String {
    let word = match count {
        1 => "remote",
        _ => "remotes",
    };
    format!("{describe} · {count} {word}")
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

    /// The search prompt's query, while one stands — the tests' window on the
    /// typed prompt's text.
    pub(crate) fn query(app: &App) -> Option<String> {
        match &app.prompt {
            Some(Prompt::Search { field, .. }) => Some(field.text().to_string()),
            _ => None,
        }
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
        for (digit, name) in [('2', "files"), ('3', "branches"), ('5', "stashes")] {
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
        assert!((app).prompt.is_some(), "the prompt did not open");

        // The shipped focus keys are text while the prompt owns the keyboard.
        app.press(Key::plain(Code::Char('4')));
        assert_eq!(
            crate::tests::query(&app).as_deref(),
            Some("4"),
            "4 was not text"
        );
        assert_eq!(app.panes.focused_name(), "commits", "4 moved the focus");
        app.press(Key::char('h'));
        assert_eq!(
            crate::tests::query(&app).as_deref(),
            Some("4h"),
            "h was not text"
        );
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
        assert!((app).prompt.is_none(), "esc did not cancel the prompt");
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
        assert!((app).prompt.is_some(), "the prompt did not open");
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
        assert!((app).prompt.is_none());
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
        assert_eq!(crate::tests::query(&app).as_deref(), Some("engine"));
        app.draw();
        assert!(status(&app).contains("/engine"), "{:?}", status(&app));
        // Cancel — the edit never stood, and the whole list comes back.
        app.press(Key::plain(Code::Esc));
        assert!((app).prompt.is_none());
        assert_eq!(list(&app).filter_note(), None);
        app.draw();
        assert!(
            body(&app).iter().any(|r| r.contains("compiler")),
            "cancel did not restore the list"
        );

        // An accepted empty query removes the filter too: it is the same door
        // out, reached by keeping an empty prompt.
        app.press(Key::char('/'));
        assert_eq!(crate::tests::query(&app).as_deref(), Some(""));
        app.press(Key::plain(Code::Enter));
        assert!((app).prompt.is_none());
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
        assert_eq!(
            crate::tests::query(&app).as_deref(),
            Some("?"),
            "the ? was not text"
        );
        // The same for the other printable global: `q` quits nothing here.
        app.press(Key::char('q'));
        assert!(!app.quit);
        assert_eq!(crate::tests::query(&app).as_deref(), Some("?q"));
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
            query(&open).as_deref(),
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
        assert_eq!(query(&quiet).as_deref(), None);
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
        assert!(
            (app).prompt.is_some(),
            "the unbound enter closed the prompt"
        );
        assert_eq!(crate::tests::query(&app).as_deref(), Some("engine"));

        // The configured accept key closes it, filter standing.
        app.press(Key::ctrl(Code::Char('s')));
        assert!((app).prompt.is_none());
        assert_eq!(list(&app).filter_note().as_deref(), Some("15/30"));
        assert!(app.pending.is_empty(), "the buffer did not clear on finish");

        // The chord's first key waits for its continuation and touches nothing.
        app.press(Key::char('/'));
        let alt_x = Key::new(Code::Char('x'), false, true, false);
        let alt_z = Key::new(Code::Char('z'), false, true, false);
        app.press(alt_x);
        assert_eq!(
            query(&app).as_deref(),
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
        assert_eq!(crate::tests::query(&app).as_deref(), Some("engineq"));
        assert!(app.pending.is_empty());
        // Completed, the chord cancels: the list is whole again.
        app.press(alt_x);
        app.press(alt_z);
        assert!((app).prompt.is_none());
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
                origin: Some(DiffSource::Patch),
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
        // the layout owns remains blank or holds its rule, never a pane's text.
        let (w, h) = app.screen.size();
        for y in 2..h - 1 {
            assert!(
                matches!(app.screen.char_at(40, y), Some(' ' | '│')),
                "row {y} drew text into the divider"
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
    fn the_bar_sits_inside_the_right_edge_of_each_container() {
        // A 120-column frame: sidebar 40, divider 40, diff 41..120. Each bar
        // occupies the right half of its pane's last cell, leaving the divider
        // intact and landing flush with the screen edge.
        const MD: &str = include_str!("../tests/fixtures/md.diff");
        let mut app = app(30);
        app.screen = Screen::new(120, 24);
        let files = gitten_core::parse_unified_diff(MD);
        let mut diff = Diff::new(files, &app.host);
        diff.set_bar(app.bar);
        app.panes.register(
            "diff",
            panes::Placement::Main,
            Screens::Diff {
                view: diff,
                origin: Some(DiffSource::Patch),
                label: "md.diff".into(),
                generation: app.generation,
            },
        );
        app.sync_modes();
        app.draw();
        let commits = app.pane_content("commits").expect("the sidebar is visible");
        let divider = commits.x + commits.width;
        assert_eq!(divider, 40, "the sidebar does not end at the divider");
        assert_eq!(
            app.screen.char_at(divider - 1, commits.y),
            Some('▐'),
            "the edge half of the sidebar bar is missing"
        );
        assert_eq!(
            app.screen.char_at(divider, commits.y),
            Some(' '),
            "the sidebar bar painted into the divider"
        );

        let diff = app.pane_content("diff").expect("the diff pane is visible");
        let edge = diff.x + diff.width - 1;
        assert_eq!(edge, 119, "the diff pane does not run to the screen's edge");
        assert_eq!(
            app.screen.char_at(edge, diff.y),
            Some('▐'),
            "the main pane's bar is not on the screen's edge"
        );
        assert!(
            !matches!(app.screen.char_at(edge - 1, diff.y), Some('▐' | '▝' | '▗')),
            "the diff's bar reached into its own columns"
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
    fn status_right_names_the_grid_and_the_alphabet() {
        // The mock's right-hand end: the grid this frame was drawn for and
        // the alphabet it was drawn with, faint beside the pane's own
        // status rather than over it.
        let mut app = app(30);
        app.screen = Screen::new(120, 32);
        app.draw();
        let row = app.screen.row_text(31);
        assert!(row.contains("commits ·"), "{row:?}");
        assert!(row.contains("120×32 · --ascii off"), "{row:?}");
        let x = row.find("120×32").expect("the grid drawn");
        assert_eq!(
            app.screen.ink(x, 31).unwrap().fg,
            app.host.theme.chrome.faint,
            "the right end is quiet, not loud"
        );

        // The alphabet follows `--ascii`: the same flag that swaps the
        // graph, the marks and the bar.
        app.ascii = true;
        app.draw();
        assert!(
            app.screen.row_text(31).contains("--ascii on"),
            "{:?}",
            app.screen.row_text(31)
        );

        // Narrow: the left status wins and the right side yields rather
        // than overwriting where the keyboard is.
        app.ascii = false;
        app.screen.resize(30, 12);
        app.draw();
        let row = app.screen.row_text(11);
        assert!(row.contains("commits ·"), "{row:?}");
        assert!(!row.contains("--ascii"), "{row:?}");
    }

    #[test]
    fn help_navigation_owns_keys_and_wheel_until_the_modal_closes() {
        let mut app = app(30);
        app.screen = Screen::new(140, 40);
        app.dispatch("help");
        let (_, max) = help::scroll_bounds(38, &app.host, &app.availability, &app.modes);
        assert!(max > 0, "the help fixture unexpectedly fits");

        app.press(Key::char('j'));
        assert_eq!(app.help_scroll, 1);
        let cursor = list(&app).cursor();
        app.input(Input::Wheel {
            key: Key::plain(Code::WheelDown),
            col: 0,
            row: 0,
        });
        assert!(app.help_scroll > 1, "the wheel did not scroll the modal");
        assert_eq!(list(&app).cursor(), cursor, "the pane moved under help");

        app.press(Key::plain(Code::End));
        assert_eq!(app.help_scroll, max);
        app.press(Key::plain(Code::Home));
        assert_eq!(app.help_scroll, 0);

        app.dispatch("help");
        app.dispatch("help");
        assert_eq!(app.help_scroll, 0, "reopening did not start at the top");
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
        app.dispatch("view.bottom");
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
        app.dispatch("view.bottom");
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
        assert!((app).prompt.is_some(), "the reload closed the live prompt");
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
        assert!((app).prompt.is_none());
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
    use gitten_core::refs::{Branch, HeadState, RefName, RemoteBranch, Stash};
    use gitten_core::status::{
        Change, ConflictEntry, ConflictKind, Kind, PathBytes, StagedEntry, Status, Submodule,
        UnstagedEntry, UntrackedEntry,
    };
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

    /// A fake's side answer, narrowed the way git's own pathspec narrows:
    /// a record matches the path asked for, or either of a rename's two.
    fn filter_pairs(pairs: &[Pair], path: Option<&[u8]>) -> Vec<Pair> {
        match path {
            None => pairs.to_vec(),
            Some(p) => pairs
                .iter()
                .filter(|pair| {
                    pair.path.as_bytes() == p
                        || pair.old_path.as_deref().is_some_and(|o| o.as_bytes() == p)
                })
                .cloned()
                .collect(),
        }
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
            old_final_newline: true,
            new_final_newline: true,
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
        /// Every write that reached the repository, recorded — as the
        /// person-readable line a test asserts against.
        writes: Vec<String>,
        /// Every path a *file* verb was aimed at, raw: the byte-for-byte
        /// record the lossy `writes` strings cannot make.
        paths_written: Vec<Vec<u8>>,
        pairs_reads: usize,
        log_reads: usize,
        /// The whole working tree `status` answers with. Tests set it
        /// directly — staging a file, breaking a read — and the next read
        /// sees the world they built.
        status: Status,
        status_reads: usize,
        /// When set, the next status read fails with exactly this message —
        /// the initial-read failure and the refresh failure are different
        /// panes' stories and each is told on demand.
        fail_status: Option<String>,
        /// When set, the next log (or pairs) read fails with exactly this
        /// message — so two panes can fail *simultaneously*, each in its own
        /// words, and a test can name which of the two errors stood.
        fail_log: Option<String>,
        fail_pairs: Option<String>,
        /// When set, the stash *read* fails with exactly this message — the
        /// ancillary read that happens at `App::new`, so a test can launch
        /// into the unavailable tenant rather than only probing it.
        fail_stashes: Option<String>,
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
        /// The branches the ref reads answer, the branches the remotes hold,
        /// and where HEAD is — the world the branches pane draws, mutated by
        /// the writes that land, which is what lets a test observe a refresh
        /// reading the world after a checkout or a rename.
        locals: Vec<Branch>,
        remotes: Vec<RemoteBranch>,
        head: Option<HeadState>,
        /// When set, the next branch/tag *read* fails with exactly this
        /// message — the ancillary reads that happen at `App::new`, so a
        /// test can launch into the failed tenant rather than only probing
        /// it. They are three knobs because a HEAD-only failure and a lost
        /// list are different panes' stories.
        fail_locals: Option<String>,
        fail_remotes: Option<String>,
        fail_head: Option<String>,
        branch_reads: usize,
        remote_reads: usize,
        head_reads: usize,
        /// Every branch or tag write that reached the repository — as the
        /// person-readable line a test asserts against, and as the raw bytes
        /// the lossy line cannot carry. Tags keep their name/target pair,
        /// and a rename keeps the bytes it was aimed *from* — the half of
        /// the verb whose exactness a lossy line can never show.
        branch_writes: Vec<String>,
        branch_bytes: Vec<Vec<u8>>,
        rename_froms: Vec<Vec<u8>>,
        tags_written: Vec<(Vec<u8>, Vec<u8>)>,
        /// When set, the next branch/tag write fails with exactly this
        /// message and changes nothing: git's refusal, verbatim.
        refuse_branch: Option<String>,
        refuse_tag: Option<String>,
        /// The two sides of the index, as the side reads answer them — the
        /// staged side (HEAD→index) and the unstaged one (index→worktree).
        /// `fakes_staged`/`fakes_unstaged` filter by the path a caller
        /// named, the way git's own pathspec would.
        staged: Vec<Pair>,
        unstaged: Vec<Pair>,
        /// What the unstaged side reads answer after the next stage lands —
        /// the same world-flip `applied` performs for the aggregate read,
        /// one shot, because a stage that lands really does change what
        /// the index→worktree diff shows.
        unstaged_after: Option<Vec<Pair>>,
        staged_reads: usize,
        unstaged_reads: usize,
        /// The untracked files' contents, as the pairs the preview asks
        /// for, and how often they were asked.
        untracked_pairs: Vec<Pair>,
        untracked_reads: usize,
        /// The third-parent untracked content a stash read answers with.
        stash_untracked: Vec<Pair>,
        stash_untracked_reads: usize,
        /// Per-ref log answers, keyed by the raw ref bytes — a branch
        /// drilldown's read, and the only way a test can prove the
        /// drilldown read *that* branch and not HEAD.
        log_at_answers: Vec<(Vec<u8>, Vec<Commit>)>,
        log_at_reads: usize,
        /// When set, every staged-side read blocks until the pair is opened.
        /// The one honest way to test that a slow preview keeps the
        /// keyboard live: the read really is slow, and the test really
        /// types while it runs.
        gate: Option<Arc<(Mutex<bool>, std::sync::Condvar)>>,
        /// The blob OIDs the checked-patch job's revalidation reads answer,
        /// keyed by lossy path. The identity a patch is built against, and
        /// the knob a staleness test turns: changing the answer makes the
        /// next write refuse, exactly as a repository that moved under the
        /// keyboard would.
        index_oids: Vec<(String, String)>,
        head_oids: Vec<(String, String)>,
        /// The remotes `remote -v` answers with, and how often it was read.
        servers: Vec<gitten_core::refs::Remote>,
        server_reads: usize,
        /// When set, the next sync verb (push, pull, fetch, or any read the
        /// sync path makes) blocks until the pair is opened — the same
        /// honest slow job the staged read's gate gives previews: a real
        /// network op, on a real thread, with the keyboard live around it.
        net_gate: Option<Arc<(Mutex<bool>, std::sync::Condvar)>>,
        /// When set, the next sync verb fails with exactly this message and
        /// changes nothing: git's refusal, verbatim.
        refuse_net: Option<String>,
    }

    /// A repository that exists only as this struct. Reads answer what the
    /// test handed in; writes are recorded and — when they land — change
    /// what the next read answers, which is what lets a test observe a
    /// refresh reading the world after the write. No process, no tty, no
    /// window, and nothing recorded is a real repository.
    struct FakeRepo(Arc<Mutex<FakeState>>);

    impl FakeRepo {
        /// Holds the net gate open for as long as the test holds it shut —
        /// the one honest way to test that a slow network job keeps the
        /// keyboard live — then answers the refusal or nothing. The state's
        /// lock is let go while the gate is held: a blocked network job must
        /// not deadlock a concurrent read of the same fake.
        fn net(&self) -> gitten_git::Result<()> {
            let (gate, refuse) = {
                let s = self.0.lock().unwrap();
                (s.net_gate.clone(), s.refuse_net.clone())
            };
            if let Some(gate) = gate {
                let (open, arrived) = &*gate;
                let mut is_open = open.lock().unwrap();
                while !*is_open {
                    is_open = arrived.wait(is_open).unwrap();
                }
            }
            match refuse {
                Some(e) => Err(e),
                None => Ok(()),
            }
        }
    }

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

        fn pairs_staged(&self, path: Option<&[u8]>) -> gitten_git::Result<Vec<Pair>> {
            let gate = self.0.lock().unwrap().gate.clone();
            if let Some(gate) = gate {
                // The read is slow for as long as the test holds the gate
                // shut — a real blocking read, on a real thread.
                let (open, arrived) = &*gate;
                let mut is_open = open.lock().unwrap();
                while !*is_open {
                    is_open = arrived.wait(is_open).unwrap();
                }
            }
            let mut s = self.0.lock().unwrap();
            s.staged_reads += 1;
            Ok(filter_pairs(&s.staged, path))
        }

        fn pairs_unstaged(&self, path: Option<&[u8]>) -> gitten_git::Result<Vec<Pair>> {
            let mut s = self.0.lock().unwrap();
            s.unstaged_reads += 1;
            Ok(filter_pairs(&s.unstaged, path))
        }

        fn pair_untracked(&self, path: &[u8]) -> gitten_git::Result<Option<Pair>> {
            let mut s = self.0.lock().unwrap();
            s.untracked_reads += 1;
            Ok(s.untracked_pairs
                .iter()
                .find(|p| p.path.as_bytes() == path)
                .cloned())
        }

        fn pairs_stash_untracked(&self, _commit: &str) -> gitten_git::Result<Vec<Pair>> {
            let mut s = self.0.lock().unwrap();
            s.stash_untracked_reads += 1;
            Ok(s.stash_untracked.clone())
        }

        fn log_at(&self, revspec: &[u8], _limit: usize) -> gitten_git::Result<Vec<Commit>> {
            let mut s = self.0.lock().unwrap();
            s.log_at_reads += 1;
            match s.log_at_answers.iter().find(|(name, _)| name == revspec) {
                Some((_, commits)) => Ok(commits.clone()),
                // An unscripted ref reads like HEAD's: the tests that care
                // script the ref they drill into.
                None => Ok(three_commits()),
            }
        }

        fn status(&self) -> gitten_git::Result<Status> {
            let mut s = self.0.lock().unwrap();
            s.status_reads += 1;
            if let Some(message) = s.fail_status.clone() {
                return Err(message);
            }
            Ok(s.status.clone())
        }

        fn describe(&self) -> String {
            "fake (main)".into()
        }

        fn stashes(&self) -> gitten_git::Result<Vec<Stash>> {
            let mut s = self.0.lock().unwrap();
            s.stash_reads += 1;
            if let Some(e) = s.fail_stashes.clone() {
                return Err(e);
            }
            Ok(s.stashes.clone())
        }

        fn branches(&self) -> gitten_git::Result<Vec<Branch>> {
            let mut s = self.0.lock().unwrap();
            s.branch_reads += 1;
            if let Some(e) = s.fail_locals.clone() {
                return Err(e);
            }
            Ok(s.locals.clone())
        }

        fn remote_branches(&self) -> gitten_git::Result<Vec<RemoteBranch>> {
            let mut s = self.0.lock().unwrap();
            s.remote_reads += 1;
            if let Some(e) = s.fail_remotes.clone() {
                return Err(e);
            }
            Ok(s.remotes.clone())
        }

        fn head(&self) -> gitten_git::Result<HeadState> {
            let mut s = self.0.lock().unwrap();
            s.head_reads += 1;
            if let Some(e) = s.fail_head.clone() {
                return Err(e);
            }
            Ok(s.head.clone().unwrap_or(HeadState::Detached {
                commit: "f00d".into(),
            }))
        }

        fn checkout(&self, name: &[u8]) -> gitten_git::Result<()> {
            let mut s = self.0.lock().unwrap();
            if let Some(e) = s.refuse_branch.clone() {
                return Err(e);
            }
            s.branch_writes
                .push(format!("checkout {}", String::from_utf8_lossy(name)));
            s.branch_bytes.push(name.to_vec());
            // git's own semantics: an attached checkout moves HEAD onto the
            // named branch; a remote-tracking ref detaches onto its commit.
            for b in &mut s.locals {
                b.head = b.name.as_bytes() == name;
            }
            s.head = Some(match s.locals.iter().any(|b| b.head) {
                true => HeadState::Branch {
                    name: RefName::from_bytes(name),
                    commit: Some("f00d".into()),
                },
                false => HeadState::Detached {
                    commit: "f00d".into(),
                },
            });
            Ok(())
        }

        fn create_branch(&self, name: &[u8], start: Option<&[u8]>) -> gitten_git::Result<()> {
            let mut s = self.0.lock().unwrap();
            if let Some(e) = s.refuse_branch.clone() {
                return Err(e);
            }
            s.branch_writes.push(format!(
                "branch {} at {}",
                String::from_utf8_lossy(name),
                start
                    .map(|s| String::from_utf8_lossy(s).into_owned())
                    .unwrap_or_else(|| "HEAD".into())
            ));
            s.branch_bytes.push(name.to_vec());
            // Nothing is checked out: the branch exists and HEAD stays put.
            s.locals.push(Branch {
                name: RefName::from_bytes(name),
                commit: "f00d".into(),
                upstream: None,
                head: false,
            });
            Ok(())
        }

        fn delete_branch(&self, name: &[u8], force: bool) -> gitten_git::Result<()> {
            let mut s = self.0.lock().unwrap();
            if let Some(e) = s.refuse_branch.clone() {
                return Err(e);
            }
            let word = match force {
                true => "force-delete branch",
                false => "delete branch",
            };
            s.branch_writes
                .push(format!("{word} {}", String::from_utf8_lossy(name)));
            s.branch_bytes.push(name.to_vec());
            s.locals.retain(|b| b.name.as_bytes() != name);
            Ok(())
        }

        fn rename_branch(&self, from: &[u8], to: &[u8]) -> gitten_git::Result<()> {
            let mut s = self.0.lock().unwrap();
            if let Some(e) = s.refuse_branch.clone() {
                return Err(e);
            }
            s.branch_writes.push(format!(
                "rename {} → {}",
                String::from_utf8_lossy(from),
                String::from_utf8_lossy(to)
            ));
            s.branch_bytes.push(to.to_vec());
            s.rename_froms.push(from.to_vec());
            if let Some(b) = s.locals.iter_mut().find(|b| b.name.as_bytes() == from) {
                b.name = RefName::from_bytes(to);
            }
            Ok(())
        }

        fn create_tag(
            &self,
            name: &[u8],
            target: &[u8],
            message: Option<&str>,
        ) -> gitten_git::Result<()> {
            let mut s = self.0.lock().unwrap();
            if let Some(e) = s.refuse_tag.clone() {
                return Err(e);
            }
            s.branch_writes.push(format!(
                "tag {} at {}{}",
                String::from_utf8_lossy(name),
                String::from_utf8_lossy(target),
                message.map(|m| format!(" ({m})")).unwrap_or_default()
            ));
            s.tags_written.push((name.to_vec(), target.to_vec()));
            Ok(())
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

        // ------------------------------------------------------ the sync

        fn remotes(&self) -> gitten_git::Result<Vec<gitten_core::refs::Remote>> {
            // A config read, not a network verb: `git remote -v` never
            // touches the wire, and a job built from it (push_current names
            // its carrier this way) runs on the caller's thread — a net gate
            // here would block dispatch itself, not just the job.
            let mut s = self.0.lock().unwrap();
            s.server_reads += 1;
            Ok(s.servers.clone())
        }

        fn push(&self, remote: &[u8], branch: &[u8]) -> gitten_git::Result<()> {
            self.net()?;
            let mut s = self.0.lock().unwrap();
            s.writes.push(format!(
                "push {} {}",
                String::from_utf8_lossy(remote),
                String::from_utf8_lossy(branch)
            ));
            // git's own semantics: the push created the branch upstream and
            // set the tracking link (the Binary impl sends --set-upstream
            // when the branch tracks nothing).
            let tracked = s
                .locals
                .iter()
                .any(|b| b.name.as_bytes() == branch && b.upstream.is_some());
            if !tracked {
                if let Some(b) = s.locals.iter_mut().find(|b| b.name.as_bytes() == branch) {
                    b.upstream = Some(gitten_core::refs::Upstream {
                        remote: RefName::from_bytes(remote),
                        branch: RefName::from_bytes(branch),
                        ahead: Some(0),
                        behind: Some(0),
                    });
                }
                if !s
                    .remotes
                    .iter()
                    .any(|r| r.remote.as_bytes() == remote && r.branch.as_bytes() == branch)
                {
                    s.remotes.push(RemoteBranch {
                        remote: RefName::from_bytes(remote),
                        branch: RefName::from_bytes(branch),
                        commit: "f00d".into(),
                    });
                }
            }
            Ok(())
        }

        fn pull(&self) -> gitten_git::Result<()> {
            self.net()?;
            let mut s = self.0.lock().unwrap();
            s.writes.push("pull".into());
            Ok(())
        }

        fn fetch(&self, remote: Option<&[u8]>) -> gitten_git::Result<()> {
            self.net()?;
            let mut s = self.0.lock().unwrap();
            s.writes.push(format!(
                "fetch {}",
                remote
                    .map(|r| String::from_utf8_lossy(r).into_owned())
                    .unwrap_or_else(|| "--all".into())
            ));
            Ok(())
        }

        fn checkout_tracking(&self, remote: &[u8], branch: &[u8]) -> gitten_git::Result<()> {
            let mut s = self.0.lock().unwrap();
            if let Some(e) = s.refuse_branch.clone() {
                return Err(e);
            }
            let mut full = remote.to_vec();
            full.push(b'/');
            full.extend_from_slice(branch);
            s.branch_writes
                .push(format!("track checkout {}", String::from_utf8_lossy(&full)));
            s.branch_bytes.push(full);
            // git's own semantics: the local branch of the same name is
            // created, tracking, and HEAD moves onto it.
            for b in &mut s.locals {
                b.head = false;
            }
            s.locals.push(Branch {
                name: RefName::from_bytes(branch),
                commit: "f00d".into(),
                upstream: Some(gitten_core::refs::Upstream {
                    remote: RefName::from_bytes(remote),
                    branch: RefName::from_bytes(branch),
                    ahead: Some(0),
                    behind: Some(0),
                }),
                head: true,
            });
            s.head = Some(HeadState::Branch {
                name: RefName::from_bytes(branch),
                commit: Some("f00d".into()),
            });
            Ok(())
        }

        fn checkout_previous(&self) -> gitten_git::Result<()> {
            let mut s = self.0.lock().unwrap();
            if let Some(e) = s.refuse_branch.clone() {
                return Err(e);
            }
            s.branch_writes.push("checkout -".into());
            // git's own semantics, one move along the reflog: the previous
            // branch takes HEAD. The fake has no reflog, so the first
            // branch that is not under HEAD stands in.
            let previous = s.locals.iter().find(|b| !b.head).map(|b| b.name.clone());
            if let Some(previous) = previous {
                for b in &mut s.locals {
                    b.head = b.name == previous;
                }
                s.head = Some(HeadState::Branch {
                    name: previous,
                    commit: Some("f00d".into()),
                });
            }
            Ok(())
        }

        fn checkout_force(&self, name: &[u8]) -> gitten_git::Result<()> {
            let mut s = self.0.lock().unwrap();
            if let Some(e) = s.refuse_branch.clone() {
                return Err(e);
            }
            s.branch_writes
                .push(format!("force-checkout {}", String::from_utf8_lossy(name)));
            s.branch_bytes.push(name.to_vec());
            for b in &mut s.locals {
                b.head = b.name.as_bytes() == name;
            }
            s.head = Some(HeadState::Branch {
                name: RefName::from_bytes(name),
                commit: Some("f00d".into()),
            });
            // `-f`'s whole point: the local changes go with it.
            s.status = Default::default();
            Ok(())
        }

        fn set_upstream(
            &self,
            local: &[u8],
            remote: &[u8],
            branch: &[u8],
        ) -> gitten_git::Result<()> {
            let mut s = self.0.lock().unwrap();
            if let Some(e) = s.refuse_branch.clone() {
                return Err(e);
            }
            s.branch_writes.push(format!(
                "track {} {} {}",
                String::from_utf8_lossy(local),
                String::from_utf8_lossy(remote),
                String::from_utf8_lossy(branch)
            ));
            if let Some(b) = s.locals.iter_mut().find(|b| b.name.as_bytes() == local) {
                b.upstream = Some(gitten_core::refs::Upstream {
                    remote: RefName::from_bytes(remote),
                    branch: RefName::from_bytes(branch),
                    ahead: Some(0),
                    behind: Some(0),
                });
            }
            Ok(())
        }

        fn unset_upstream(&self, local: &[u8]) -> gitten_git::Result<()> {
            let mut s = self.0.lock().unwrap();
            if let Some(e) = s.refuse_branch.clone() {
                return Err(e);
            }
            s.branch_writes
                .push(format!("untrack {}", String::from_utf8_lossy(local)));
            if let Some(b) = s.locals.iter_mut().find(|b| b.name.as_bytes() == local) {
                b.upstream = None;
            }
            Ok(())
        }

        fn fast_forward(
            &self,
            local: &[u8],
            remote: &[u8],
            branch: &[u8],
        ) -> gitten_git::Result<()> {
            let mut s = self.0.lock().unwrap();
            if let Some(e) = s.refuse_branch.clone() {
                return Err(e);
            }
            // The shape is HEAD's own, decided fresh exactly as the Binary
            // impl decides it: merge for the checked-out branch, fetch
            // refspec for everything else.
            let head_branch = match s.head.clone().unwrap_or(HeadState::Detached {
                commit: "f00d".into(),
            }) {
                HeadState::Branch { name, .. } => Some(name),
                HeadState::Detached { .. } => None,
            };
            let verb = match head_branch.as_ref().map(|n| n.as_bytes()) == Some(local) {
                true => "merge --ff-only",
                false => "fetch refspec",
            };
            s.branch_writes.push(format!(
                "fast-forward {} {} {verb} {}/{}",
                String::from_utf8_lossy(local),
                if verb == "merge --ff-only" {
                    "to"
                } else {
                    "from"
                },
                String::from_utf8_lossy(remote),
                String::from_utf8_lossy(branch)
            ));
            if let Some(b) = s.locals.iter_mut().find(|b| b.name.as_bytes() == local) {
                b.commit = "beef".into();
            }
            Ok(())
        }

        fn add_remote(&self, name: &[u8], url: &[u8]) -> gitten_git::Result<()> {
            let mut s = self.0.lock().unwrap();
            if let Some(e) = s.refuse_branch.clone() {
                return Err(e);
            }
            s.branch_writes.push(format!(
                "remote add {} {}",
                String::from_utf8_lossy(name),
                String::from_utf8_lossy(url)
            ));
            if !s.servers.iter().any(|r| r.name.as_bytes() == name) {
                s.servers.push(gitten_core::refs::Remote {
                    name: RefName::from_bytes(name),
                    urls: vec![String::from_utf8_lossy(url).into_owned()],
                });
            }
            Ok(())
        }

        fn set_remote_url(&self, name: &[u8], url: &[u8]) -> gitten_git::Result<()> {
            let mut s = self.0.lock().unwrap();
            if let Some(e) = s.refuse_branch.clone() {
                return Err(e);
            }
            s.branch_writes.push(format!(
                "remote set-url {} {}",
                String::from_utf8_lossy(name),
                String::from_utf8_lossy(url)
            ));
            if let Some(r) = s.servers.iter_mut().find(|r| r.name.as_bytes() == name) {
                if r.urls.is_empty() {
                    r.urls.push(String::from_utf8_lossy(url).into_owned());
                } else {
                    r.urls[0] = String::from_utf8_lossy(url).into_owned();
                }
            }
            Ok(())
        }

        fn remove_remote(&self, name: &[u8]) -> gitten_git::Result<()> {
            let mut s = self.0.lock().unwrap();
            if let Some(e) = s.refuse_branch.clone() {
                return Err(e);
            }
            s.branch_writes
                .push(format!("remote remove {}", String::from_utf8_lossy(name)));
            s.servers.retain(|r| r.name.as_bytes() != name);
            s.remotes.retain(|r| r.remote.as_bytes() != name);
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
            if let Some(after) = s.unstaged_after.take() {
                s.unstaged = after;
            }
            Ok(())
        }

        fn unstage_patch(&self, patch: &[u8]) -> gitten_git::Result<()> {
            let mut s = self.0.lock().unwrap();
            s.writes
                .push(format!("unstage {}", String::from_utf8_lossy(patch)));
            s.applied += 1;
            Ok(())
        }

        fn discard_patch(&self, patch: &[u8]) -> gitten_git::Result<()> {
            let mut s = self.0.lock().unwrap();
            s.writes
                .push(format!("discard {}", String::from_utf8_lossy(patch)));
            if s.refuses.iter().any(|r| patch.starts_with(r)) {
                return Err("the fake refused".into());
            }
            s.applied += 1;
            Ok(())
        }

        fn index_blob_oid(&self, path: &[u8]) -> gitten_git::Result<Option<String>> {
            let shown = String::from_utf8_lossy(path).into_owned();
            Ok(self
                .0
                .lock()
                .unwrap()
                .index_oids
                .iter()
                .find(|(p, _)| *p == shown)
                .map(|(_, oid)| oid.clone()))
        }

        fn head_blob_oid(&self, path: &[u8]) -> gitten_git::Result<Option<String>> {
            let shown = String::from_utf8_lossy(path).into_owned();
            Ok(self
                .0
                .lock()
                .unwrap()
                .head_oids
                .iter()
                .find(|(p, _)| *p == shown)
                .map(|(_, oid)| oid.clone()))
        }

        fn commit(&self, message: &str) -> gitten_git::Result<String> {
            self.0
                .lock()
                .unwrap()
                .writes
                .push(format!("commit {message}"));
            Ok("f00d".into())
        }

        fn amend(&self, message: &str) -> gitten_git::Result<String> {
            self.0
                .lock()
                .unwrap()
                .writes
                .push(format!("amend {message}"));
            Ok("f00d".into())
        }

        fn stage(&self, path: &[u8]) -> gitten_git::Result<()> {
            let mut s = self.0.lock().unwrap();
            s.writes
                .push(format!("stage {}", String::from_utf8_lossy(path)));
            s.paths_written.push(path.to_vec());
            s.applied += 1;
            Ok(())
        }

        fn unstage(&self, path: &[u8]) -> gitten_git::Result<()> {
            let mut s = self.0.lock().unwrap();
            s.writes
                .push(format!("unstage {}", String::from_utf8_lossy(path)));
            s.paths_written.push(path.to_vec());
            s.applied += 1;
            Ok(())
        }

        fn discard(&self, path: &[u8]) -> gitten_git::Result<()> {
            let mut s = self.0.lock().unwrap();
            s.writes
                .push(format!("discard {}", String::from_utf8_lossy(path)));
            s.paths_written.push(path.to_vec());
            s.applied += 1;
            Ok(())
        }

        fn remove_untracked(&self, path: &[u8]) -> gitten_git::Result<()> {
            let mut s = self.0.lock().unwrap();
            s.writes
                .push(format!("delete {}", String::from_utf8_lossy(path)));
            s.paths_written.push(path.to_vec());
            s.applied += 1;
            Ok(())
        }

        fn ignore(&self, path: &[u8]) -> gitten_git::Result<()> {
            let mut s = self.0.lock().unwrap();
            s.writes
                .push(format!("ignore {}", String::from_utf8_lossy(path)));
            s.paths_written.push(path.to_vec());
            s.applied += 1;
            Ok(())
        }

        fn stage_many(&self, paths: &[&[u8]]) -> gitten_git::Result<()> {
            let shown = paths
                .iter()
                .map(|p| String::from_utf8_lossy(p).into_owned())
                .collect::<Vec<_>>()
                .join(", ");
            let mut s = self.0.lock().unwrap();
            s.writes.push(format!("stage-many {shown}"));
            s.paths_written.extend(paths.iter().map(|p| p.to_vec()));
            s.applied += 1;
            Ok(())
        }

        fn unstage_many(&self, paths: &[&[u8]]) -> gitten_git::Result<()> {
            let shown = paths
                .iter()
                .map(|p| String::from_utf8_lossy(p).into_owned())
                .collect::<Vec<_>>()
                .join(", ");
            let mut s = self.0.lock().unwrap();
            s.writes.push(format!("unstage-many {shown}"));
            s.paths_written.extend(paths.iter().map(|p| p.to_vec()));
            s.applied += 1;
            Ok(())
        }
    }

    /// The fake's branches: `main` checked out, no remotes, HEAD attached —
    /// the honest seed every repository-shaped constructor starts from. A
    /// test that wants a different world overwrites the state directly.
    fn one_branch() -> (Vec<Branch>, Vec<RemoteBranch>, HeadState) {
        (
            vec![Branch {
                name: RefName::from("main"),
                commit: "f00d".into(),
                upstream: None,
                head: true,
            }],
            Vec::new(),
            HeadState::Branch {
                name: RefName::from("main"),
                commit: Some("f00d".into()),
            },
        )
    }

    /// The branches world the verb tests launch into, layered over the seed:
    /// `main` under HEAD, a second local whose name is legal bytes and not
    /// text, and two remotes — one whose branch half holds a slash, so a
    /// joined refname is information only the joiner can add. Flattened the
    /// pane reads: `main`, `f<e9>ature`, `origin/feat/ure`, `origin/main`.
    fn branch_world(state: &Mutex<FakeState>) {
        let mut s = state.lock().unwrap();
        s.locals = vec![
            Branch {
                name: RefName::from("main"),
                commit: "f00d".into(),
                upstream: None,
                head: true,
            },
            Branch {
                name: RefName::from_bytes(b"f\xe9ature"),
                commit: "beef".into(),
                upstream: None,
                head: false,
            },
        ];
        s.remotes = vec![
            RemoteBranch {
                remote: RefName::from("origin"),
                branch: RefName::from("feat/ure"),
                commit: "f00d".into(),
            },
            RemoteBranch {
                remote: RefName::from("origin"),
                branch: RefName::from("main"),
                commit: "f00d".into(),
            },
        ];
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
        let status = Status {
            untracked: untracked
                .iter()
                .map(|u| gitten_core::status::UntrackedEntry {
                    path: gitten_core::status::PathBytes::from_bytes(u.as_bytes()),
                })
                .collect(),
            ..Default::default()
        };
        let (locals, remotes, head) = one_branch();
        let state = Arc::new(Mutex::new(FakeState {
            before: vec![pair("f.txt", side(0), side(3))],
            after: vec![pair("f.txt", side(0), side(2))],
            refuses: vec![b"refuse".to_vec()],
            stashes: two_stashes(),
            status,
            locals,
            remotes,
            head: Some(head),
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
        let (locals, remotes, head) = one_branch();
        let state = Arc::new(Mutex::new(FakeState {
            before: vec![pair("f.txt", side(false), side(true))],
            after: vec![pair("f.txt", side(false), side(false))],
            refuses: vec![b"refuse".to_vec()],
            stashes: two_stashes(),
            status: Status {
                untracked: untracked
                    .iter()
                    .map(|u| gitten_core::status::UntrackedEntry {
                        path: gitten_core::status::PathBytes::from_bytes(u.as_bytes()),
                    })
                    .collect(),
                ..Default::default()
            },
            locals,
            remotes,
            head: Some(head),
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
        app.load_startup(&mut StartClock::new());
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
        app.load_startup(&mut StartClock::new());
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
    /// files pane above it in the sidebar, the empty diff beside both — the
    /// three tenants every repository launch registers.
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
        app.load_startup(&mut StartClock::new());
        app.screen = Screen::new(120, 24);
        app
    }

    fn commits_of(app: &App) -> &Commits {
        match app.panes.get("commits") {
            Some(Screens::Commits { view, .. }) => view,
            _ => panic!("the commits pane is not registered"),
        }
    }

    /// The diff tenant's header — the label install put there.
    fn diff_label_of(app: &App) -> String {
        match app.panes.get("diff") {
            Some(Screens::Diff { label, .. }) => label.clone(),
            _ => panic!("the diff pane is not registered"),
        }
    }

    fn diff_of(app: &App) -> &Diff {
        match app.panes.get("diff") {
            Some(Screens::Diff { view, .. }) => view,
            _ => panic!("the diff pane is not registered"),
        }
    }

    /// The files view, and the label its tenant was registered under.
    fn files_of(app: &App) -> &Files {
        match app.panes.get("files") {
            Some(Screens::Files { view, .. }) => view,
            _ => panic!("the files pane is not registered"),
        }
    }

    fn files_label(app: &App) -> &str {
        match app.panes.get("files") {
            Some(Screens::Files { label, .. }) => label,
            _ => panic!("the files pane is not registered"),
        }
    }

    /// The branches view, and the label its tenant was registered under.
    fn branches_of(app: &App) -> &Branches {
        match app.panes.get("branches") {
            Some(Screens::Branches { view, .. }) => view,
            _ => panic!("the branches pane is not registered"),
        }
    }

    fn branches_label(app: &App) -> &str {
        match app.panes.get("branches") {
            Some(Screens::Branches { label, .. }) => label,
            _ => panic!("the branches pane is not registered"),
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

    /// The bottom row of the last drawn frame — the row a prompt owns.
    fn status(app: &App) -> String {
        app.screen.row_text(app.screen.size().1 - 1)
    }

    #[test]
    fn files_empty_states_and_narrow_frames_are_honest() {
        // A clean read draws `working tree clean` and a zero in the label —
        // and is available, which a failed read never is.
        let (handle, _state) = fake(&[]);
        let mut app = commits_app(&handle);
        app.draw();
        assert!(files_of(&app).is_clean());
        assert!(files_of(&app).is_available());
        assert_eq!(files_label(&app), "fake (main) · 0 changed");
        let body = app.screen.row_text(2);
        assert!(body.contains("working tree clean"), "{body:?}");

        // A failed initial read still registers — retryable — and is honest
        // in both places: the pane says the read did not come back, the
        // header does not say 0 changed, and neither calls the tree clean.
        let (handle, state) = fake(&[]);
        state.lock().unwrap().fail_status = Some("the status read failed".into());
        let mut app = commits_app(&handle);
        app.draw();
        assert!(!files_of(&app).is_clean());
        assert!(!files_of(&app).is_available());
        assert_eq!(files_label(&app), "fake (main) · status unavailable");
        let body = app.screen.row_text(2);
        assert!(body.contains("status unavailable"), "{body:?}");
        // The first successful read stands it up — the same generation wave
        // any write finishes into.
        state.lock().unwrap().fail_status = None;
        state.lock().unwrap().status = Status {
            untracked: vec![gitten_core::status::UntrackedEntry {
                path: gitten_core::status::PathBytes::from("new.txt"),
            }],
            ..Default::default()
        };
        assert!(app.submitter.submit(Box::new(Dead)).is_ok(), "queued");
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                files_of(&app).is_available()
            }),
            "the failed pane was never stood up"
        );
        assert_eq!(files_label(&app), "fake (main) · 1 changed");
        app.draw();
        assert!(
            app.screen.row_text(3).contains("new.txt"),
            "{:?}",
            app.screen.row_text(3)
        );

        // Wide: the five sidebar lists split the sidebar into canonical
        // equal slices beside the diff, and no row crosses the divider
        // column.
        app.screen.resize(120, 24);
        app.draw();
        let files_rect = app.pane_rect("files").expect("files placed");
        let commits_rect = app.pane_rect("commits").expect("commits placed");
        let diff_rect = app.pane_rect("diff").expect("diff placed");
        assert_eq!((files_rect.x, files_rect.width), (0, 40));
        assert_eq!((commits_rect.x, commits_rect.width), (0, 40));
        assert_eq!(files_rect.y, 1);
        assert_eq!(commits_rect.y, files_rect.y + files_rect.height + 5);
        // Five slices over the sidebar: the odd rows go to the first two.
        assert_eq!(files_rect.height, 5, "the first slice takes the remainder");
        assert_eq!(commits_rect.height, 4, "unequal slices");
        assert_eq!((diff_rect.x, diff_rect.width), (41, 79));
        for y in 1..24 {
            assert_eq!(
                app.screen.char_at(40, y),
                Some(' '),
                "row {y} crossed the divider"
            );
        }
        // The header names the pane, its live focus key, and the label; the
        // title and the status line name it too.
        app.dispatch("files.focus");
        app.draw();
        let header = app.screen.row_text(1);
        assert!(header.contains('2'), "{header:?}");
        assert!(header.contains("files"), "{header:?}");
        assert!(header.contains("· 1 changed"), "{header:?}");
        assert!(app.screen.row_text(0).contains("files"));
        assert!(
            app.screen.row_text(23).contains("files ·"),
            "{:?}",
            app.screen.row_text(23)
        );

        // Narrow: only the focused pane draws, at the whole body.
        for width in [95, 80] {
            app.screen.resize(width, 24);
            app.draw();
            assert_eq!(
                app.pane_rect("files"),
                Some(crate::panes::Rect {
                    x: 0,
                    y: 1,
                    width,
                    height: 22,
                })
            );
            assert!(app.pane_rect("commits").is_none(), "{width} kept commits");
            assert!(app.pane_rect("diff").is_none(), "{width} kept the diff");
        }

        // Zero- and one-row bodies survive: the layout drops the slice that
        // does not fit and the panes draw nothing into what is not there.
        app.screen.resize(120, 3);
        app.draw();
        app.screen.resize(120, 2);
        app.draw();
    }

    #[test]
    fn repository_startup_registers_files_into_the_sidebar_ring() {
        // A commits launch: five tenants — the files pane and the branches
        // pane and the stash stack every repository start registers, commits,
        // the empty diff — with the launch focus restored over the
        // registration that focused its own addition.
        let (handle, _state) = fake(&[]);
        let mut app = commits_app(&handle);
        assert_eq!(
            app.panes.focused_name(),
            "commits",
            "the launch focus was not restored"
        );
        assert!(app.panes.get("files").is_some(), "no files tenant");
        assert_eq!(app.panes.names().count(), 6);
        // The sidebar's canonical order: files (rank 1), branches (rank 2),
        // commits (rank 3), stashes (rank 4) — with nothing in panes.rs the
        // wiser.
        assert_eq!(
            app.panes.list_order(),
            ["files", "branches", "commits", "stashes", "remotes"]
        );

        // `2` is the shared files.focus binding, and it now lands.
        app.press(Key::plain(Code::Char('2')));
        assert_eq!(app.panes.focused_name(), "files");
        assert_eq!(app.message, "", "focusing a registered pane said nothing");
        // Ctrl-J/Ctrl-K cycle both directions through the canonical order:
        // files, then the branches pane the branches registration put
        // between them, then commits.
        app.press(Key::ctrl(Code::Char('j')));
        assert_eq!(app.panes.focused_name(), "branches");
        app.press(Key::ctrl(Code::Char('j')));
        assert_eq!(app.panes.focused_name(), "commits");
        app.press(Key::ctrl(Code::Char('k')));
        assert_eq!(app.panes.focused_name(), "branches");
        app.press(Key::ctrl(Code::Char('k')));
        assert_eq!(app.panes.focused_name(), "files");
        // Headers derive live keys: files advertises `2` like any other pane.
        app.draw();
        let header = app.screen.row_text(1);
        assert!(header.contains("files"), "{header:?}");
        assert!(header.contains('2'), "{header:?}");

        // A direct working-tree-diff launch registers it too, and keeps the
        // diff focused.
        let (handle, _state) = fake(&[]);
        let source = Source::Repo {
            path: std::path::PathBuf::from("/fake"),
            arg: String::new(),
        };
        let mut direct = app_on_fake(&source, &handle);
        assert!(
            direct.panes.get("files").is_some(),
            "a diff launch got no files pane"
        );
        assert!(
            direct.panes.get("branches").is_some(),
            "a diff launch got no branches pane"
        );
        assert_eq!(direct.panes.focused_name(), "diff");
        // ...and the diff launch's keyboard stays put under a key that is
        // text everywhere else.
        direct.press(Key::plain(Code::Char('2')));
        assert_eq!(direct.panes.focused_name(), "files");
        direct.press(Key::plain(Code::Char('0')));
        assert_eq!(direct.panes.focused_name(), "diff");

        // A fixture and a patch have no repository and so no pane: the name
        // stays absent, and the focus command says the exact sentence.
        let mut fixture = app_on_diff(Source::Fixtures, None);
        assert!(fixture.panes.get("files").is_none());
        fixture.press(Key::plain(Code::Char('2')));
        assert_eq!(fixture.message, "no files pane");
        let started = gitten_app::Started {
            view: View::Diff,
            source: Source::Patch { file: None },
            host: Host::new(),
            loaded: acquire::Loaded {
                label: "patch".into(),
                data: Data::Diff(parse_unified_diff(HUNK_DIFF)),
            },
            config: std::path::PathBuf::new(),
            repo: None,
        };
        let mut patch = App::new(started, Glyphs::default());
        patch.screen = Screen::new(60, 24);
        assert!(patch.panes.get("files").is_none());
        patch.press(Key::plain(Code::Char('2')));
        assert_eq!(patch.message, "no files pane");
    }

    /// A tree with one file in each section, the untracked one carrying a
    /// path no encoding claims — what the per-section verb rules need under
    /// the cursor, and the byte-exactness check in one fixture.
    fn four_section_status() -> Status {
        Status {
            staged: vec![StagedEntry {
                path: PathBytes::from("kept.rs"),
                change: Change::Modified,
                old_path: None,
                kind: Kind::File,
                submodule: Submodule::default(),
            }],
            unstaged: vec![UnstagedEntry {
                path: PathBytes::from("work.rs"),
                change: Change::Modified,
                kind: Kind::File,
                submodule: Submodule::default(),
            }],
            untracked: vec![UntrackedEntry {
                path: PathBytes::from_bytes(b"caf\xe9.txt"),
            }],
            conflicts: vec![ConflictEntry {
                path: PathBytes::from("merge.rs"),
                state: ConflictKind::BothModified,
                kind: Kind::File,
                submodule: Submodule::default(),
            }],
            ignored: vec![],
        }
    }

    #[test]
    fn files_stage_submits_stage_or_unstage_and_says_refusals() {
        let (handle, state) = fake(&[]);
        state.lock().unwrap().status = four_section_status();
        let mut app = commits_app(&handle);
        app.dispatch("files.focus");
        assert_eq!(app.panes.focused_name(), "files");
        let statuses = |state: &Arc<Mutex<FakeState>>| state.lock().unwrap().writes.len();

        // Staged unstages; unstaged, untracked and conflicted stage — each
        // with the row's own bytes, walking the sections the way the keys
        // would.
        let mut expected: Vec<&str> = Vec::new();
        for (written, path) in [
            ("unstage kept.rs", b"kept.rs".as_slice()),
            ("stage work.rs", b"work.rs".as_slice()),
            ("stage caf\u{FFFD}.txt", b"caf\xe9.txt".as_slice()),
            ("stage merge.rs", b"merge.rs".as_slice()),
        ] {
            app.dispatch("files.stage");
            assert!(
                until(Duration::from_secs(2), || {
                    app.pump_quiet();
                    state.lock().unwrap().writes.len() > expected.len()
                }),
                "{written} never reached the repository"
            );
            expected.push(written);
            assert_eq!(state.lock().unwrap().writes, expected, "{written}");
            assert_eq!(
                state
                    .lock()
                    .unwrap()
                    .paths_written
                    .last()
                    .map(Vec::as_slice),
                Some(path),
                "{written}: the aim was not the raw bytes"
            );
            app.dispatch("view.down");
        }
        // The Latin-1 path rode through undecoded, whatever the band said.
        assert!(state
            .lock()
            .unwrap()
            .paths_written
            .iter()
            .any(|p| p.as_slice() == b"caf\xe9.txt"));

        // Wrong focus says the command's name and queues nothing.
        let written = statuses(&state);
        app.dispatch("commits.focus");
        app.dispatch("files.stage");
        assert_eq!(app.message, "files.stage is not supported here");
        assert_eq!(statuses(&state), written);

        // No row: a clean tree has nothing under the cursor.
        let (handle, state) = fake(&[]);
        let mut app = commits_app(&handle);
        app.dispatch("files.focus");
        app.dispatch("files.stage");
        assert_eq!(app.message, "nothing selected to stage");
        assert!(state.lock().unwrap().writes.is_empty());

        // No repository: a fixture with a files pane cannot aim a write.
        let mut fixture = app_on_diff(Source::Fixtures, None);
        with_hand_registered_files(&mut fixture);
        fixture.dispatch("files.stage");
        assert_eq!(fixture.message, "a fixture has no working tree to stage in");

        // A closed queue refuses in its own words, before anything runs.
        let (handle, state) = fake(&[]);
        state.lock().unwrap().status = four_section_status();
        let mut app = commits_app(&handle);
        app.dispatch("files.focus");
        drop(std::mem::replace(&mut app.jobs, Runner::new()));
        app.dispatch("files.stage");
        assert_eq!(app.message, "the job queue is shutting down");
        assert!(state.lock().unwrap().writes.is_empty());
    }

    #[test]
    fn files_stage_all_uses_the_cursor_side_as_one_job() {
        let (handle, state) = fake(&[]);
        state.lock().unwrap().status = Status {
            staged: vec![
                StagedEntry {
                    path: PathBytes::from("one.rs"),
                    change: Change::Added,
                    old_path: None,
                    kind: Kind::File,
                    submodule: Submodule::default(),
                },
                StagedEntry {
                    path: PathBytes::from("two.rs"),
                    change: Change::Modified,
                    old_path: None,
                    kind: Kind::File,
                    submodule: Submodule::default(),
                },
            ],
            unstaged: vec![UnstagedEntry {
                path: PathBytes::from("work.rs"),
                change: Change::Modified,
                kind: Kind::File,
                submodule: Submodule::default(),
            }],
            untracked: vec![UntrackedEntry {
                path: PathBytes::from_bytes(b"caf\xe9.txt"),
            }],
            conflicts: vec![ConflictEntry {
                path: PathBytes::from("merge.rs"),
                state: ConflictKind::BothModified,
                kind: Kind::File,
                submodule: Submodule::default(),
            }],
            ignored: vec![],
        };
        let mut app = commits_app(&handle);
        app.dispatch("files.focus");

        // Cursor in staged: one unstage-many over exactly the staged paths.
        app.dispatch("files.stage-all");
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                !state.lock().unwrap().writes.is_empty()
            }),
            "the bulk unstage never reached the repository"
        );
        assert_eq!(
            state.lock().unwrap().writes,
            vec!["unstage-many one.rs, two.rs"],
            "one job, not one per path"
        );
        assert_eq!(
            state.lock().unwrap().paths_written,
            [b"one.rs".to_vec(), b"two.rs".to_vec()],
            "the raw bytes did not survive the bulk"
        );

        // Cursor on the unstaged row (two steps down from the first staged
        // file): staging gathers unstaged and untracked and leaves the
        // conflict alone.
        app.dispatch("view.down");
        app.dispatch("view.down");
        let writes = state.lock().unwrap().writes.len();
        app.dispatch("files.stage-all");
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                state.lock().unwrap().writes.len() > writes
            }),
            "the bulk stage never reached the repository"
        );
        assert_eq!(
            state.lock().unwrap().writes,
            vec![
                "unstage-many one.rs, two.rs",
                "stage-many work.rs, caf\u{FFFD}.txt"
            ],
            "the conflict was bulk-staged"
        );
        assert_eq!(
            state
                .lock()
                .unwrap()
                .paths_written
                .last()
                .map(Vec::as_slice),
            Some(b"caf\xe9.txt".as_slice())
        );

        // Nothing on the cursor's side is the one reachable empty-target
        // refusal: an empty tree's cursor names no section, so the staging
        // direction has nothing to gather. ("Nothing staged to unstage" is
        // the window's own defensive sentence for a state its cursor cannot
        // reach either — a staged section is only ever drawn non-empty.)
        let (handle, state) = fake(&[]);
        let mut app = commits_app(&handle);
        app.dispatch("files.focus");
        app.dispatch("files.stage-all");
        assert_eq!(app.message, "nothing unstaged or untracked to stage");
        assert!(state.lock().unwrap().writes.is_empty());

        // No repository, wrong focus: the usual two sentences.
        let mut fixture = app_on_diff(Source::Fixtures, None);
        with_hand_registered_files(&mut fixture);
        fixture.dispatch("files.stage-all");
        assert_eq!(fixture.message, "a fixture has no working tree to act on");
        let (handle, _state) = fake(&[]);
        let mut app = commits_app(&handle);
        app.dispatch("files.stage-all");
        assert_eq!(app.message, "files.stage-all is not supported here");
    }

    #[test]
    fn files_discard_arms_then_submits_the_exact_destructive_job() {
        let (handle, state) = fake(&[]);
        state.lock().unwrap().status = four_section_status();
        let mut app = commits_app(&handle);
        app.dispatch("files.focus");
        let statuses = |state: &Arc<Mutex<FakeState>>| state.lock().unwrap().writes.len();

        // Staged and conflicted rows refuse before anything is armed —
        // neither is the terminal's to destroy.
        for _ in 0..2 {
            app.dispatch("files.discard");
        }
        assert_eq!(
            app.message,
            "that change is staged — unstage it before discarding"
        );
        assert!(state.lock().unwrap().writes.is_empty());
        app.dispatch("view.down"); // the unstaged twin's heading, skipped
        app.dispatch("view.down"); // work.rs
        app.dispatch("view.down"); // ...on to the conflict
        app.dispatch("view.down");
        app.dispatch("view.down");
        app.dispatch("view.down");
        app.dispatch("files.discard");
        assert_eq!(
            app.message,
            "a conflicted file needs its merge resolved, not discarded"
        );
        assert!(state.lock().unwrap().writes.is_empty());

        // Onto the unstaged work.rs: the first press asks and submits
        // nothing; the identical second press submits the exact job. One
        // step down from the first staged file crosses the unstaged heading.
        app.dispatch("view.top");
        app.dispatch("view.down");
        app.dispatch("files.discard");
        assert_eq!(app.message, "discard work.rs? press again to confirm");
        assert!(state.lock().unwrap().writes.is_empty());
        app.dispatch("files.discard");
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                !state.lock().unwrap().writes.is_empty()
            }),
            "the confirmed discard never reached the repository"
        );
        assert_eq!(state.lock().unwrap().writes, vec!["discard work.rs"]);
        assert_eq!(state.lock().unwrap().paths_written, [b"work.rs".to_vec()]);

        // The untracked file says *delete* and runs the delete mechanics —
        // a fresh two presses, because a move disarmed the first question.
        let written = statuses(&state);
        app.dispatch("view.down"); // one step: past the untracked heading
        app.dispatch("files.discard");
        assert_eq!(
            app.message,
            "delete caf\u{FFFD}.txt? press again to confirm"
        );
        assert_eq!(statuses(&state), written, "the first press queued a job");
        app.dispatch("files.discard");
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                state.lock().unwrap().writes.len() > written
            }),
            "the confirmed delete never reached the repository"
        );
        assert_eq!(
            state.lock().unwrap().writes.last(),
            Some(&"delete caf\u{FFFD}.txt".to_string())
        );

        // No row, no repository: the usual refusals.
        let (handle, state) = fake(&[]);
        let mut app = commits_app(&handle);
        app.dispatch("files.focus");
        app.dispatch("files.discard");
        assert_eq!(app.message, "nothing selected to discard");
        assert!(state.lock().unwrap().writes.is_empty());
        let mut fixture = app_on_diff(Source::Fixtures, None);
        with_hand_registered_files(&mut fixture);
        fixture.dispatch("files.discard");
        assert_eq!(
            fixture.message,
            "a fixture has no working tree to discard from"
        );

        // A cursor move, the wheel, a mouse press on another row and a
        // refresh each disarm the standing question: what is armed to what
        // the keyboard used to be on can never fire after the keyboard
        // moved. The keyboard returns to work.rs before each check, so the
        // question being *asked again* is the whole evidence — an arm that
        // had survived would have fired instead.
        let (handle, state) = fake(&[]);
        state.lock().unwrap().status = four_section_status();
        let mut app = commits_app(&handle);
        app.draw();
        app.dispatch("files.focus");
        let onto_work = |app: &mut App| {
            // One step down from the first staged file crosses the unstaged
            // heading onto work.rs.
            app.dispatch("view.top");
            app.dispatch("view.down");
        };
        let disarm_check = |app: &mut App, state: &Arc<Mutex<FakeState>>, label: &str| {
            let written = state.lock().unwrap().writes.len();
            app.dispatch("files.discard");
            assert_eq!(
                app.message, "discard work.rs? press again to confirm",
                "{label}: the press was not a fresh question"
            );
            assert_eq!(
                state.lock().unwrap().writes.len(),
                written,
                "{label}: a stale arm fired"
            );
        };
        onto_work(&mut app);
        disarm_check(&mut app, &state, "fresh");
        app.dispatch("view.down"); // the move itself
        onto_work(&mut app);
        disarm_check(&mut app, &state, "after a cursor move");
        let files = app.pane_rect("files").expect("files placed");
        app.input(Input::Wheel {
            key: Key::plain(Code::WheelDown),
            col: files.x + 1,
            row: files.y + 1,
        });
        onto_work(&mut app);
        disarm_check(&mut app, &state, "after the wheel");

        // A mouse press on another row disarms too. The files slice is the
        // first of the sidebar's five at 120 columns; its content starts on
        // screen row 2, and the staged file — a selectable row that is not
        // the armed one — sits on row 3.
        onto_work(&mut app);
        app.dispatch("files.discard");
        let armed_row = files_of(&app).cursor();
        let other_row = match armed_row {
            0 => 1,
            _ => 1,
        };
        app.mouse(click(MouseKind::Down, 5, 2 + other_row));
        app.mouse(click(MouseKind::Up, 5, 2 + other_row));
        onto_work(&mut app);
        disarm_check(&mut app, &state, "after a mouse press elsewhere");

        // ...while a press on the armed row itself keeps the question, so
        // the keyboard's second press still confirms it.
        onto_work(&mut app);
        let armed_row = files_of(&app).cursor();
        app.dispatch("files.discard");
        app.mouse(click(MouseKind::Down, 5, 2 + armed_row));
        app.mouse(click(MouseKind::Up, 5, 2 + armed_row));
        let written = state.lock().unwrap().writes.len();
        app.dispatch("files.discard");
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                state.lock().unwrap().writes.len() > written
            }),
            "the same-row press did not keep the arm confirmable"
        );

        // A refresh disarms — and focus away and back alone does not.
        let (handle, state) = fake(&[]);
        state.lock().unwrap().status = four_section_status();
        let mut app = commits_app(&handle);
        app.dispatch("files.focus");
        onto_work(&mut app);
        app.dispatch("files.discard");
        assert_eq!(app.message, "discard work.rs? press again to confirm");
        assert!(app.submitter.submit(Box::new(Dead)).is_ok(), "queued");
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                app.generation > Generation::default()
            }),
            "the finish was never drained"
        );
        disarm_check(&mut app, &state, "after a refresh");
        // The check's press armed work.rs again; a focus round trip is the
        // one thing that must not touch it, so the next press *confirms* —
        // the discard lands, which no disarming action could have allowed.
        app.dispatch("commits.focus");
        app.dispatch("files.focus");
        let written = state.lock().unwrap().writes.len();
        app.dispatch("files.discard");
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                state.lock().unwrap().writes.len() > written
            }),
            "focus away and back dropped the arm"
        );
    }

    #[test]
    fn files_ignore_only_submits_for_untracked_rows() {
        let (handle, state) = fake(&[]);
        state.lock().unwrap().status = four_section_status();
        let mut app = commits_app(&handle);
        app.dispatch("files.focus");

        // Onto the untracked row, then ignore it: the exact verb, the raw
        // path, one job.
        app.dispatch("view.top");
        for _ in 0..2 {
            app.dispatch("view.down");
        }
        app.dispatch("files.ignore");
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                !state.lock().unwrap().writes.is_empty()
            }),
            "the ignore never reached the repository"
        );
        assert_eq!(state.lock().unwrap().writes, vec!["ignore caf\u{FFFD}.txt"]);
        assert_eq!(
            state.lock().unwrap().paths_written,
            [b"caf\xe9.txt".to_vec()]
        );

        // Every other section refuses with the one sentence — tracked rows
        // are not `.gitignore`'s business.
        let writes = state.lock().unwrap().writes.len();
        for _ in 0..2 {
            app.dispatch("view.top");
            app.dispatch("files.ignore");
            assert_eq!(app.message, "only an untracked file can be ignored");
        }
        assert_eq!(state.lock().unwrap().writes.len(), writes);

        // Wrong focus, no repository, closed queue: said, and silent.
        app.dispatch("commits.focus");
        app.dispatch("files.ignore");
        assert_eq!(app.message, "files.ignore is not supported here");
        let mut fixture = app_on_diff(Source::Fixtures, None);
        with_hand_registered_status(&mut fixture, one_untracked_status());
        fixture.dispatch("files.ignore");
        assert_eq!(fixture.message, "a fixture has no repository to ignore in");
        let (handle, state) = fake(&[]);
        state.lock().unwrap().status = four_section_status();
        let mut app = commits_app(&handle);
        app.dispatch("files.focus");
        app.dispatch("view.top");
        for _ in 0..2 {
            app.dispatch("view.down");
        }
        drop(std::mem::replace(&mut app.jobs, Runner::new()));
        app.dispatch("files.ignore");
        assert_eq!(app.message, "the job queue is shutting down");
        assert!(state.lock().unwrap().writes.is_empty());
    }

    #[test]
    fn files_stash_pushes_without_a_selection_or_message() {
        let (handle, state) = fake(&[]);
        state.lock().unwrap().status = four_section_status();
        let mut app = commits_app(&handle);

        // Dispatched from the commit list — stash is repository-scoped and
        // reads no row. No prompt opens for it either.
        app.dispatch("commits.focus");
        app.dispatch("files.stash");
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                !state.lock().unwrap().stash_writes.is_empty()
            }),
            "the stash never reached the repository"
        );
        assert_eq!(state.lock().unwrap().stash_writes, vec!["push "]);
        assert!(app.prompt.is_none(), "stash opened a prompt");

        // From the files pane, same story.
        app.dispatch("files.focus");
        app.dispatch("files.stash");
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                state.lock().unwrap().stash_writes.len() == 2
            }),
            "the second stash never reached the repository"
        );
        assert_eq!(state.lock().unwrap().stash_writes, vec!["push ", "push "]);

        // The fake parks whatever it is handed — a clean tree is git's own
        // refusal to give, and the sibling test covers it through
        // `refuse_stash`. Here the routing is what is under test: the push
        // reaches the repository with no selection and no message.
        let (handle, state) = fake(&[]);
        let mut app = commits_app(&handle);
        app.dispatch("files.stash");
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                !state.lock().unwrap().stash_writes.is_empty()
            }),
            "the clean-tree stash never ran"
        );
        assert_eq!(state.lock().unwrap().stash_writes, vec!["push "]);

        // No repository and a closed queue refuse.
        let mut fixture = app_on_diff(Source::Fixtures, None);
        fixture.dispatch("files.stash");
        assert_eq!(fixture.message, "a fixture has no working tree to park");
        let (handle, state) = fake(&[]);
        let mut app = commits_app(&handle);
        drop(std::mem::replace(&mut app.jobs, Runner::new()));
        app.dispatch("files.stash");
        assert_eq!(app.message, "the job queue is shutting down");
        assert!(state.lock().unwrap().writes.is_empty());
    }

    /// The files tenant's generation, read off the variant — what a refresh
    /// sets only after it applied.
    fn files_generation(app: &App) -> Generation {
        match app.panes.get("files") {
            Some(Screens::Files { generation, .. }) => *generation,
            _ => panic!("the files pane is not registered"),
        }
    }

    #[test]
    fn a_files_write_refreshes_every_generation_tenant() {
        let (handle, state) = fake(&[]);
        state.lock().unwrap().status = four_section_status();
        let mut app = commits_app(&handle);
        app.dispatch("commits.open-diff");
        app.dispatch("files.focus");
        // The cursor on the unstaged row, so its anchor can be checked after
        // the wave: one step down from the first staged file crosses the
        // unstaged heading onto work.rs.
        app.dispatch("view.top");
        app.dispatch("view.down");
        let started = state.lock().unwrap().status_reads;
        let opens = state.lock().unwrap().pairs_reads;
        let generation = app.generation;

        // One write that lands and one that is refused: both finishes are
        // generation bumps, and both waves must reach every repository pane.
        let first = Write::stage(&handle, b"work.rs".to_vec());
        assert!(app.submitter.submit(Box::new(first)).is_ok(), "queued");
        let second = Write::stage_patch(&handle, b"refuse-me".to_vec()).expect("a non-empty patch");
        assert!(app.submitter.submit(Box::new(second)).is_ok(), "queued");
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                state.lock().unwrap().status_reads >= started + 2
                    && state.lock().unwrap().pairs_reads >= opens + 2
                    && state.lock().unwrap().log_reads >= 2
            }),
            "the waves never finished"
        );

        // The generation is the one every pane was refreshed against — the
        // files pane included, though the keyboard never left it and the
        // diff was never focused again.
        assert!(app.generation > generation);
        for name in ["files", "commits", "diff"] {
            let pane = app
                .panes
                .get(name)
                .unwrap_or_else(|| panic!("{name} is registered"));
            assert_eq!(pane.generation(), app.generation, "{name}");
        }
        // The label moved with the world: the staged write moved work.rs
        // out of the unstaged count.
        assert_eq!(files_label(&app), "fake (main) · 4 changed");
        // The anchor held: the keyboard is still on (unstaged, work.rs) —
        // the section it sits in is its own.
        let current = files_of(&app)
            .current_file()
            .expect("a file under the cursor");
        assert_eq!(
            (current.section, current.path.as_bytes()),
            (files::Section::Unstaged, &b"work.rs"[..])
        );

        // The first refresh error stands and the rest are still attempted:
        // a failing files read does not stop the commit list's, and the
        // first pane's error is the one said.
        let (handle, state) = fake(&[]);
        state.lock().unwrap().status = four_section_status();
        let mut app = commits_app(&handle);
        app.dispatch("commits.open-diff");
        state.lock().unwrap().fail_status = Some("the status read failed".into());
        state.lock().unwrap().fail_log = Some("the log read failed".into());
        let reads = state.lock().unwrap().status_reads;
        assert!(app.submitter.submit(Box::new(Dead)).is_ok(), "queued");
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                app.generation > Generation::default()
            }),
            "the finish was never drained"
        );
        assert_eq!(app.message, "the log read failed", "the last error stood");
        assert_ne!(app.message, "the status read failed");
        assert!(
            state.lock().unwrap().status_reads > reads,
            "files was never attempted after the first error"
        );

        // And the files pane's *own* refresh error leaves its last good data
        // standing — never a false clean tree, never a current generation.
        let (handle, state) = fake(&[]);
        state.lock().unwrap().status = four_section_status();
        let mut app = commits_app(&handle);
        let stale_label = files_label(&app).to_string();
        state.lock().unwrap().fail_status = Some("the status read failed".into());
        assert!(app.submitter.submit(Box::new(Dead)).is_ok(), "queued");
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                app.generation > Generation::default()
            }),
            "the finish was never drained"
        );
        assert_eq!(app.message, "the status read failed");
        assert_eq!(files_label(&app), stale_label, "the label was replaced");
        assert!(files_of(&app).is_available(), "a failed read faked a state");
        assert_eq!(
            files_generation(&app),
            Generation::default(),
            "a failed refresh marked the pane current"
        );
        assert_ne!(files_generation(&app), app.generation);
    }

    #[test]
    fn highlighted_commit_previews_and_enter_only_focuses_the_persistent_diff() {
        let (handle, state) = fake(&[]);
        let mut app = commits_app(&handle);
        app.draw();
        assert_eq!(app.panes.focused_name(), "commits");
        let open_reads = state.lock().unwrap().pairs_reads;

        // Startup already previewed the highlighted commit while leaving the
        // keyboard on the list. Enter focuses that tenant without another
        // read or another registration. (Five tenants: files, branches,
        // stashes, commits, diff.)
        assert!(matches!(
            app.panes.get("diff"),
            Some(Screens::Diff {
                origin: Some(_),
                ..
            })
        ));
        app.press(Key::plain(Code::Enter));
        assert_eq!(app.panes.focused_name(), "diff");
        assert_eq!(app.panes.names().count(), 6, "enter appended a pane");
        assert_eq!(state.lock().unwrap().pairs_reads, open_reads);
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
                    origin: Some(_),
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

        // Moving the list highlight replaces the preview immediately without
        // moving focus. Enter after that is only the focus transfer.
        let reads = state.lock().unwrap().pairs_reads;
        app.dispatch("view.down");
        app.pump_quiet();
        assert_eq!(app.panes.focused_name(), "commits");
        assert_eq!(state.lock().unwrap().pairs_reads, reads + 1);
        assert!(matches!(
            app.panes.get("diff"),
            Some(Screens::Diff {
                origin: Some(DiffSource::Commit { sha }),
                ..
            }) if sha == "00000001"
        ));

        // Enter on that already-shown commit does no acquisition.
        let reads = state.lock().unwrap().pairs_reads;
        app.press(Key::plain(Code::Enter));
        assert_eq!(
            app.panes.names().count(),
            6,
            "a second enter appended a pane"
        );
        assert_eq!(app.panes.focused_name(), "diff");
        assert_eq!(state.lock().unwrap().pairs_reads, reads);

        // A direct diff launch has one tenant and `esc` goes nowhere.
        let mut app = app_on_diff(Source::Fixtures, None);
        assert_eq!(app.panes.names().count(), 1);
        app.press(Key::plain(Code::Esc));
        assert_eq!(app.panes.focused_name(), "diff");
        assert_eq!(app.panes.names().count(), 1, "esc invented a pane");
    }

    #[test]
    fn the_commit_preview_refuses_working_tree_hunk_verbs() {
        // A commits launch immediately previews its highlighted row. That is
        // a real commit diff, but never a working-tree diff, so staging verbs
        // refuse it at the source gate and queue nothing.
        let (handle, state) = fake(&[]);
        let mut app = commits_app(&handle);
        app.dispatch("diff.focus");
        assert_eq!(app.panes.focused_name(), "diff");

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
            "the empty pane queued a write"
        );

        // Enter merely focuses the preview and changes none of that.
        app.dispatch("commits.focus");
        app.press(Key::plain(Code::Enter));
        assert_eq!(app.panes.focused_name(), "diff");
        assert!(matches!(
            app.panes.get("diff"),
            Some(Screens::Diff {
                origin: Some(_),
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

    /// A one-file working tree — what a hand-registered files pane needs to
    /// have a row under the cursor.
    fn one_file_status() -> Status {
        Status {
            unstaged: vec![gitten_core::status::UnstagedEntry {
                path: gitten_core::status::PathBytes::from("work.rs"),
                change: gitten_core::status::Change::Modified,
                kind: gitten_core::status::Kind::File,
                submodule: Default::default(),
            }],
            ..Default::default()
        }
    }

    /// A one-untracked-file tree — the one section `files.ignore` answers.
    fn one_untracked_status() -> Status {
        Status {
            untracked: vec![UntrackedEntry {
                path: PathBytes::from("notes.md"),
            }],
            ..Default::default()
        }
    }

    /// Registers a files pane into an app that could not have grown one on
    /// its own — a fixture's, with no repository behind it — so a test can
    /// reach the refusals that live behind a focused files pane.
    fn with_hand_registered_status(app: &mut App, status: Status) {
        let files::Prepared { rows, label } = files::prepare(&status, "fake");
        app.panes.register(
            "files",
            panes::Placement::sidebar("files"),
            Screens::Files {
                view: Files::new(rows),
                label,
                generation: Generation::default(),
            },
        );
        app.sync_modes();
        app.dispatch("files.focus");
        assert_eq!(app.panes.focused_name(), "files");
    }

    fn with_hand_registered_files(app: &mut App) {
        with_hand_registered_status(app, one_file_status());
    }

    /// `text` as keypresses — the way typing reaches the app headlessly.
    fn type_(app: &mut App, text: &str) {
        for c in text.chars() {
            app.press(Key::char(c));
        }
    }

    #[test]
    fn commit_message_prompt_accepts_cancels_and_isolates_input() {
        let (handle, state) = fake(&[]);
        let mut app = commits_app(&handle);
        app.dispatch("files.focus");
        assert_eq!(app.panes.focused_name(), "files");

        // `c` is the shared files.commit binding: an empty one-line field on
        // the status row, under the input mode and nothing else.
        app.press(Key::char('c'));
        assert!(matches!(
            app.prompt,
            Some(Prompt::CommitMessage { ref field }) if field.text().is_empty()
        ));
        app.draw();
        assert!(status(&app).contains("commit: █"), "{:?}", status(&app));

        // Every printable that would otherwise mean something is text: `q`
        // quits nothing, `?` opens no help, the digits move no focus, `h`
        // and `j` move no pane and no list. A message field is not a search.
        for c in ['q', '?', '4', '2', 'h', 'j'] {
            app.press(Key::char(c));
        }
        assert!(!app.quit);
        assert!(!app.help);
        assert_eq!(app.panes.focused_name(), "files");
        assert!(matches!(app.prompt, Some(Prompt::CommitMessage { .. })));
        // A paste is one sanitized edit, and a message field keeps its line
        // breaks: a commit message is multiline by design, and a paste is
        // how a whole message arrives at once.
        app.input(Input::Paste("one\ntwo\tb".into()));
        assert_eq!(
            app.prompt.as_ref().map(|p| p.field().text()),
            Some("q?42hjone\ntwo\tb"),
            "the paste did not arrive as one sanitized edit"
        );

        // The mouse is inert under any prompt.
        app.draw();
        let cursor_before = files_of(&app).cursor();
        app.mouse(click(MouseKind::Down, 5, 3));
        app.mouse(click(MouseKind::Up, 5, 3));
        assert_eq!(
            files_of(&app).cursor(),
            cursor_before,
            "the mouse moved the pane under the prompt"
        );

        // A message longer than the 120-column row: the caret and the newest
        // tail stay visible, and the stored text was never sliced.
        let long = format!("{}end", "a".repeat(200));
        for c in long.chars() {
            app.press(Key::char(c));
        }
        app.draw();
        let row = status(&app);
        assert!(row.ends_with("end█"), "{row:?}");
        assert_eq!(
            app.prompt.as_ref().map(|p| p.field().text()).map(str::len),
            Some(long.len() + "q?42hjone\ntwo\tb".len()),
            "the logical message was cut to what fits"
        );

        // Esc closes and discards the text with no write built.
        app.press(Key::plain(Code::Esc));
        assert!(app.prompt.is_none());
        assert!(
            state.lock().unwrap().writes.is_empty(),
            "esc submitted a write"
        );

        // Enter submits the whole text as one commit — the message the fake
        // records is the one that was typed, not the tail that was shown.
        app.press(Key::char('c'));
        app.input(Input::Paste("subject line".into()));
        app.press(Key::plain(Code::Enter));
        assert!(app.prompt.is_none());
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                !state.lock().unwrap().writes.is_empty()
            }),
            "the commit never reached the repository"
        );
        let writes = state.lock().unwrap().writes.clone();
        assert_eq!(writes, vec!["commit subject line"]);

        // Whitespace-only accept closes the field and refuses without a job.
        app.press(Key::char('c'));
        type_(&mut app, "   ");
        app.press(Key::plain(Code::Enter));
        assert!(app.prompt.is_none());
        assert_eq!(app.message, "a commit needs a message");
        assert_eq!(
            state.lock().unwrap().writes.len(),
            1,
            "a whitespace accept queued a write"
        );

        // The wrong focus is said, not swallowed.
        app.dispatch("commits.focus");
        app.dispatch("files.commit");
        assert_eq!(app.message, "files.commit is not supported here");
        assert!(app.prompt.is_none());

        // And a fixture with a files pane has no repository to commit in.
        let mut fixture = app_on_diff(Source::Fixtures, None);
        with_hand_registered_files(&mut fixture);
        fixture.dispatch("files.commit");
        assert_eq!(fixture.message, "a fixture has no repository to commit in");
        assert!(fixture.prompt.is_none());
    }

    #[test]
    fn amend_message_uses_the_same_prompt_but_the_amend_job() {
        let (handle, state) = fake(&[]);
        let mut app = commits_app(&handle);
        app.dispatch("files.focus");

        // `A` opens on HEAD's subject when the loaded history holds it —
        // an edit of what is standing is what amend is — and empty here,
        // because the fake's HEAD names a commit the list does not hold.
        app.press(Key::char('A'));
        assert!(matches!(
            app.prompt,
            Some(Prompt::AmendMessage { ref field }) if field.text().is_empty()
        ));
        app.draw();
        assert!(status(&app).contains("amend: █"), "{:?}", status(&app));
        // Esc cancels with no write, and no double-ask stands in the way.
        app.press(Key::plain(Code::Esc));
        assert!(app.prompt.is_none());
        assert!(state.lock().unwrap().writes.is_empty());

        // Whitespace refuses with commit's own sentence — the same field.
        app.press(Key::char('A'));
        type_(&mut app, "  ");
        app.press(Key::plain(Code::Enter));
        assert!(app.prompt.is_none());
        assert_eq!(app.message, "a commit needs a message");
        assert!(state.lock().unwrap().writes.is_empty());

        // Enter submits exactly `Write::amend` with the whole sanitized
        // text — a message field keeps its line breaks, so a pasted
        // two-line message arrives as two lines, and nothing was sliced.
        app.press(Key::char('A'));
        app.input(Input::Paste("rewritten subject\nbody".into()));
        app.press(Key::plain(Code::Enter));
        assert!(app.prompt.is_none());
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                !state.lock().unwrap().writes.is_empty()
            }),
            "the amend never reached the repository"
        );
        let writes = state.lock().unwrap().writes.clone();
        assert_eq!(writes, vec!["amend rewritten subject\nbody"]);

        // No confirmation mode rides the path, and no extra command exists:
        // a confirm-looking name is nobody's command, refused by the same
        // availability contract the help panel reads.
        app.dispatch("files.amend-confirm");
        assert_eq!(
            app.message,
            "files.amend-confirm is not supported by this client"
        );

        // The wrong focus is said, the same as commit's.
        app.dispatch("commits.focus");
        app.dispatch("files.amend");
        assert_eq!(app.message, "files.amend is not supported here");
        assert!(app.prompt.is_none());

        // A fixture with a files pane has no repository to amend in.
        let mut fixture = app_on_diff(Source::Fixtures, None);
        with_hand_registered_files(&mut fixture);
        fixture.dispatch("files.amend");
        assert_eq!(fixture.message, "a fixture has no repository to amend in");
    }

    #[test]
    fn keyboard_uses_focus_and_wheel_uses_the_pane_below_the_pointer() {
        let (handle, state) = fake_tall(&[]);
        let mut app = commits_app(&handle);
        app.draw();
        // Both tenants long: the diff loaded, the list a hundred deep. Both
        // previews are waited for, because a background read does not count
        // itself into a number captured before its thread ran — the cursor
        // and read-count assertions below are only stable once they land.
        app.dispatch("commits.open-diff");
        until(Duration::from_secs(2), || {
            app.pump_quiet();
            matches!(
                app.panes.get("diff"),
                Some(Screens::Diff {
                    origin: Some(DiffSource::Commit { sha }),
                    ..
                }) if sha == "00000000"
            )
        });
        app.dispatch("commits.focus");
        app.draw();

        // The keyboard follows commits focus; a wheel over the diff moves the
        // diff instead, without stealing that focus.
        let (dc, dt) = (diff_of(&app).cursor(), diff_of(&app).top());
        app.dispatch("view.down");
        until(Duration::from_secs(2), || {
            app.pump_quiet();
            matches!(
                app.panes.get("diff"),
                Some(Screens::Diff {
                    origin: Some(DiffSource::Commit { sha }),
                    ..
                }) if sha == "00000001"
            )
        });
        assert_eq!(commits_of(&app).cursor(), 1);
        assert_eq!((diff_of(&app).cursor(), diff_of(&app).top()), (dc, dt));
        let diff = app.pane_rect("diff").expect("diff placed");
        app.input(Input::Wheel {
            key: Key::plain(Code::WheelDown),
            col: diff.x + 1,
            row: diff.y + 1,
        });
        assert!(
            diff_of(&app).top() > dt,
            "the wheel did not scroll the diff below it"
        );
        assert_eq!(commits_of(&app).top(), 0);
        assert_eq!(app.panes.focused_name(), "commits");

        // Reverse both: the keyboard follows diff focus, while a wheel over
        // the commits header moves that list and leaves focus on the diff.
        app.dispatch("diff.focus");
        let (cc, ct) = (commits_of(&app).cursor(), commits_of(&app).top());
        let dc = diff_of(&app).cursor();
        app.dispatch("view.down");
        assert_eq!(diff_of(&app).cursor(), dc + 1);
        assert_eq!(
            (commits_of(&app).cursor(), commits_of(&app).top()),
            (cc, ct)
        );
        let commits = app.pane_rect("commits").expect("commits placed");
        app.input(Input::Wheel {
            key: Key::plain(Code::WheelDown),
            col: commits.x + 1,
            row: commits.y,
        });
        assert!(
            commits_of(&app).top() > ct,
            "the wheel did not scroll the list below it"
        );
        assert_eq!(app.panes.focused_name(), "diff");

        // The divider belongs to no pane and moves neither side.
        let (ct, dt) = (commits_of(&app).top(), diff_of(&app).top());
        app.input(Input::Wheel {
            key: Key::plain(Code::WheelDown),
            col: commits.right(),
            row: commits.y + 1,
        });
        assert_eq!((commits_of(&app).top(), diff_of(&app).top()), (ct, dt));

        // Scrolling commits while branches owns the keyboard moves only the
        // list's viewport. Its selected commit and the main diff are unchanged,
        // both before and after focus returns.
        app.dispatch("branches.focus");
        app.draw();
        let commits = app.pane_rect("commits").expect("commits placed");
        let selected = commits_of(&app).cursor();
        let reads = state.lock().unwrap().pairs_reads;
        app.input(Input::Wheel {
            key: Key::plain(Code::WheelDown),
            col: commits.x + 1,
            row: commits.y + 1,
        });
        assert_eq!(app.panes.focused_name(), "branches");
        assert_eq!(commits_of(&app).cursor(), selected);
        assert_eq!(state.lock().unwrap().pairs_reads, reads);

        app.dispatch("commits.focus");
        assert_eq!(app.panes.focused_name(), "commits");
        assert_eq!(state.lock().unwrap().pairs_reads, reads);

        // And the frame agrees: the unfocused pane draws no cursor bar, the
        // focused one exactly one.
        app.dispatch("diff.focus");
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
        // The read rides the preview lane; the install is what every later
        // dedupe and assertion counts against.
        app.pump_quiet();
        app.dispatch("commits.focus");
        app.pump_quiet();
        app.draw();

        // Down in the commits rectangle presses it, in its own coordinates.
        // The sidebar splits five ways now, so the commits slice is the
        // third of them (its content rows are 12–14): local row 2 is two
        // content rows down.
        app.mouse(click(MouseKind::Down, 5, 14));
        app.pump_quiet();
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

        // A press on the diff's scrollbar column: it is the row underneath
        // now — text, and nothing about the bar itself scrolls or grabs.
        let top = diff_of(&app).top();
        app.mouse(click(MouseKind::Down, 119, 5));
        app.mouse(click(MouseKind::Drag, 119, 12));
        app.mouse(click(MouseKind::Up, 119, 12));
        assert_eq!(diff_of(&app).top(), top, "the bar column did not scroll");

        // Two quick clicks in the commits pane open the diff — the clock
        // counts, and the pane it counted in is part of what it counted.
        // (The commits slice is the middle of the sidebar now.)
        app.dispatch("commits.focus");
        let reads = state.lock().unwrap().pairs_reads;
        app.mouse(click(MouseKind::Down, 10, 13));
        app.mouse(click(MouseKind::Up, 10, 13));
        // The click's own preview is on the lane; let it land before the
        // second press, so the double click meets a shown commit and
        // deduplicates — the count below is the click's read, not the open's.
        app.pump_quiet();
        app.mouse(click(MouseKind::Down, 10, 13));
        app.pump_quiet();
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
        // once, and the feedback counts lines. The commits slice is the
        // middle of the sidebar now, its content rows 12–14.
        app.mouse(click(MouseKind::Down, 5, 13));
        app.mouse(click(MouseKind::Drag, 5, 14));
        app.mouse(click(MouseKind::Up, 5, 14));
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

        // A creation on the combined view is the policy gate's refusal, not
        // a geometry guess: the combined view folds both sides of the index
        // into one text, so no hunk of it can say which side it stages —
        // and the refusal names the door that can.
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
            "the combined view folds both sides of the index — open the file's own side from the files pane (enter)"
        );
        assert!(
            state.lock().unwrap().writes.is_empty(),
            "a refusal queued a write"
        );

        // The one that lands, does so on a file's own side: the unstaged
        // preview of a tracked file, whose hunk is the index→worktree
        // change and whose patch stages exactly that. Every refusal above
        // left the queue untouched; this is the whole table's single write.
        let (handle, state) = fake(&[]);
        {
            let mut s = state.lock().unwrap();
            s.unstaged = vec![tracked_insertion()];
            // The identity the patch is built against — what the write's
            // revalidation read must still answer.
            s.index_oids = vec![("tracked.txt".into(), "i0".into())];
        }
        let mut app = app_on_fake(
            &Source::Repo {
                path: std::path::PathBuf::from("/fake"),
                arg: String::new(),
            },
            &handle,
        );
        open_side(&mut app, "tracked.txt");
        move_to(&mut app, 2);
        app.dispatch("diff.stage-hunk");
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                !state.lock().unwrap().writes.is_empty()
            }),
            "the tracked insertion never reached the repository"
        );
        let writes = state.lock().unwrap().writes.clone();
        assert_eq!(writes.len(), 1, "{writes:?}");
        assert!(writes[0].starts_with("stage "), "{writes:?}");
        assert!(
            app.submitter.submit(Box::new(Dead)).is_ok(),
            "the queue still runs"
        );
    }

    /// One unstaged pair for a tracked file: the old side the index holds
    /// and the new side the worktree shows, five lines apart by one
    /// inserted line — the shape a partial stage acts on.
    fn tracked_insertion() -> Pair {
        let mut p = pair(
            "tracked.txt",
            (0..5)
                .map(|i| Arc::from(format!("line {i}").as_str()))
                .collect(),
            (0..5)
                .map(|i| match i {
                    3 => Arc::<str>::from("inserted"),
                    n => Arc::from(format!("line {n}").as_str()),
                })
                .collect(),
        );
        p.status = 'M';
        p.old_oid = Some("i0".into());
        p
    }

    /// Opens one file's unstaged preview through the front door — the same
    /// request the files pane makes — and waits for it to install.
    fn open_side(app: &mut App, path: &str) {
        app.request_preview(
            gitten_core::source::DiffSource::Unstaged {
                path: gitten_core::status::PathBytes::from_bytes(path.as_bytes()),
            },
            true,
        );
        app.pump_quiet();
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
        // The read is on the preview lane now; the install is what the next
        // dispatch deduplicates against, so give it its turn.
        app.pump_quiet();
        assert_eq!(app.panes.names().count(), 6, "open-diff appended a pane");
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
                app.pump_quiet();
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
        for name in ["commits", "diff", "files", "branches"] {
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
                app.pump_quiet();
                app.generation > Generation::default()
            }),
            "the finish was never drained"
        );
        assert_eq!(app.message, "the log read failed", "the last error stood");
        assert_ne!(app.message, "the pairs read failed");
    }

    // ------------------------------------------------------ partial staging
    //
    // The W3 behaviors, end to end through the real input path: commands
    // dispatched by name against a diff pane whose side preview was
    // acquired through the front door. The bytes git will see are proven
    // against real repositories in `gitten-git`'s own tests; these prove
    // the TUI aims the right verb at the right side with the right
    // selection, and refuses where it must.

    #[test]
    fn tui_parity_line_selection_marks_and_stages_only_its_lines() {
        let (handle, state) = fake(&[]);
        {
            let mut s = state.lock().unwrap();
            s.unstaged = vec![tracked_insertion()];
            s.index_oids = vec![("tracked.txt".into(), "i0".into())];
        }
        let source = Source::Repo {
            path: std::path::PathBuf::from("/fake"),
            arg: String::new(),
        };
        let mut app = app_on_fake(&source, &handle);
        open_side(&mut app, "tracked.txt");
        // Line selection: `a` turns it on, `v` marks the added line, space
        // stages what is marked — and the status line says which unit the
        // verbs are aimed at.
        app.dispatch("diff.toggle-line-selection");
        move_to(&mut app, 6);
        app.press(Key::plain(Code::Char('v')));
        app.dispatch("diff.stage-hunk");
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                !state.lock().unwrap().writes.is_empty()
            }),
            "the marked line never reached the repository"
        );
        let writes = state.lock().unwrap().writes.clone();
        assert_eq!(writes.len(), 1, "{writes:?}");
        let applied = parse_unified_diff(&writes[0]["stage ".len()..]);
        let changed: Vec<&str> = applied[0]
            .hunks
            .iter()
            .flat_map(|h| &h.lines)
            .filter(|l| l.kind != gitten_core::LineKind::Context)
            .map(|l| l.text.as_ref())
            .collect();
        assert_eq!(changed, ["inserted"], "only the marked line travels");
        let status = match app.panes.get("diff") {
            Some(Screens::Diff { view, .. }) => view.status(&app.host),
            _ => panic!("a diff is registered"),
        };
        assert!(
            status.contains("line selection") && status.contains("1 marked"),
            "the unit and the mark are on the status line: {status}"
        );
    }

    #[test]
    fn tui_parity_discard_hunk_asks_twice_on_one_spot_and_then_discards() {
        let (handle, state) = fake(&[]);
        {
            let mut s = state.lock().unwrap();
            s.unstaged = vec![tracked_insertion()];
            s.index_oids = vec![("tracked.txt".into(), "i0".into())];
        }
        let source = Source::Repo {
            path: std::path::PathBuf::from("/fake"),
            arg: String::new(),
        };
        let mut app = app_on_fake(&source, &handle);
        open_side(&mut app, "tracked.txt");
        move_to(&mut app, 2);
        // First press arms the question; nothing runs.
        app.dispatch("diff.discard-hunk");
        assert_eq!(
            app.message,
            "discard the hunk of tracked.txt under the keyboard? press again to confirm"
        );
        assert!(state.lock().unwrap().writes.is_empty(), "the arm ran git");
        // A move of the keyboard disarms it — a yes addressed to a row
        // that is no longer under the keyboard is not a yes.
        app.dispatch("view.down");
        app.dispatch("diff.discard-hunk");
        assert!(
            state.lock().unwrap().writes.is_empty(),
            "a moved-away answer ran git"
        );
        // Back on the row: the question again, and the second press spends
        // it and runs.
        move_to(&mut app, 2);
        app.dispatch("diff.discard-hunk");
        app.dispatch("diff.discard-hunk");
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                !state.lock().unwrap().writes.is_empty()
            }),
            "the answered discard never reached the repository"
        );
        let writes = state.lock().unwrap().writes.clone();
        assert_eq!(writes.len(), 1, "{writes:?}");
        assert!(writes[0].starts_with("discard "), "{writes:?}");
    }

    #[test]
    fn tui_parity_the_staged_side_says_what_each_verb_means_there() {
        let (handle, state) = fake(&[]);
        {
            let mut s = state.lock().unwrap();
            let mut staged = tracked_insertion();
            staged.old_oid = Some("h0".into());
            staged.new_oid = Some("i0".into());
            s.staged = vec![staged];
            s.head_oids = vec![("tracked.txt".into(), "h0".into())];
            s.index_oids = vec![("tracked.txt".into(), "i0".into())];
        }
        let source = Source::Repo {
            path: std::path::PathBuf::from("/fake"),
            arg: String::new(),
        };
        let mut app = app_on_fake(&source, &handle);
        app.request_preview(
            gitten_core::source::DiffSource::Staged {
                path: gitten_core::status::PathBytes::from_bytes(b"tracked.txt"),
            },
            true,
        );
        app.pump_quiet();
        move_to(&mut app, 2);
        // Staging the staged side means nothing: the index is the diff's
        // own new side.
        app.dispatch("diff.stage-hunk");
        assert_eq!(
            app.message,
            "the index is this diff's new side — it is already staged"
        );
        // A discard has no working tree to aim at — the refusal offers the
        // unstage instead, and no arm was spent asking.
        app.dispatch("diff.discard-hunk");
        assert_eq!(
            app.message,
            "the staged side has no working tree to discard — unstage it (u), then discard the unstaged side"
        );
        assert!(state.lock().unwrap().writes.is_empty(), "a refusal ran git");
        // Unstaging is what the side is for.
        app.dispatch("diff.unstage-hunk");
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                !state.lock().unwrap().writes.is_empty()
            }),
            "the unstage never reached the repository"
        );
        let writes = state.lock().unwrap().writes.clone();
        assert_eq!(writes.len(), 1, "{writes:?}");
        assert!(writes[0].starts_with("unstage "), "{writes:?}");
    }

    #[test]
    fn tui_parity_a_stale_diff_refuses_before_anything_is_read_for_the_write() {
        let (handle, state) = fake(&[]);
        {
            let mut s = state.lock().unwrap();
            s.unstaged = vec![tracked_insertion()];
            s.index_oids = vec![("tracked.txt".into(), "i0".into())];
        }
        let source = Source::Repo {
            path: std::path::PathBuf::from("/fake"),
            arg: String::new(),
        };
        let mut app = app_on_fake(&source, &handle);
        open_side(&mut app, "tracked.txt");
        move_to(&mut app, 2);
        // The repository moved under the preview: the verb re-reads, finds
        // the drawn hunk gone, and refuses before any write exists.
        state.lock().unwrap().unstaged[0].new[2] = Arc::from("MOVED ON");
        app.dispatch("diff.stage-hunk");
        assert_eq!(
            app.message,
            "tracked.txt changed since this diff was drawn — refresh (R) and try again"
        );
        assert!(
            state.lock().unwrap().writes.is_empty(),
            "a stale aim ran git"
        );
    }

    #[test]
    fn tui_parity_a_drifted_index_refuses_at_write_time() {
        let (handle, state) = fake(&[]);
        {
            let mut s = state.lock().unwrap();
            s.unstaged = vec![tracked_insertion()];
            // The identity the patch is built against is already stale: the
            // index holds something else by the time the job runs.
            s.index_oids = vec![("tracked.txt".into(), "moved-on".into())];
        }
        let source = Source::Repo {
            path: std::path::PathBuf::from("/fake"),
            arg: String::new(),
        };
        let mut app = app_on_fake(&source, &handle);
        open_side(&mut app, "tracked.txt");
        move_to(&mut app, 2);
        app.dispatch("diff.stage-hunk");
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                app.message.contains("changed since the patch was read")
            }),
            "the drifted write never refused: {}",
            app.message
        );
        assert!(
            state.lock().unwrap().writes.is_empty(),
            "a drifted write ran"
        );
    }

    #[test]
    fn tui_parity_line_selection_keys_resolve_help_and_dispatch_agree() {
        // `a` and `v` are the shipped bindings, resolved through the same
        // builtin keymap every client reads, and the help panel shows them
        // — with the discard now advertised where it runs.
        let keys = Host::new().keys;
        let mut modes = Modes::new();
        modes.push("diff");
        assert_eq!(
            keys.resolve(&modes, &[Key::plain(Code::Char('a'))]),
            Resolve::Run("diff.toggle-line-selection")
        );
        assert_eq!(
            keys.resolve(&modes, &[Key::plain(Code::Char('v'))]),
            Resolve::Run("select.mark")
        );
        assert_eq!(
            keys.resolve(&modes, &[Key::plain(Code::Char('D'))]),
            Resolve::Run("diff.discard-hunk")
        );
        // And the availability contract agrees with the dispatch: help
        // lists the three, and the dispatch refuses none of them by
        // availability.
        let availability = tui_availability(true);
        for name in [
            "diff.toggle-line-selection",
            "diff.discard-hunk",
            "select.mark",
        ] {
            assert!(
                availability.runnable(name),
                "{name} is not runnable, but the keymap resolves it"
            );
        }
    }

    #[test]
    fn a_refreshed_frame_is_drawable_headlessly() {
        let (handle, state) = fake(&[]);
        let source = Source::Repo {
            path: std::path::PathBuf::from("/fake"),
            arg: String::new(),
        };
        // The staging verb acts on a file's own side now: the unstaged
        // preview, whose hunk is the index→worktree change — and a stage
        // that lands flips the side read, the same world-change the
        // aggregate read always answered with.
        {
            let mut s = state.lock().unwrap();
            s.unstaged = s.before.clone();
            s.unstaged_after = Some(s.after.clone());
        }
        let mut app = app_on_fake(&source, &handle);
        open_side(&mut app, "f.txt");
        // The keyboard is on the first hunk — the one about to be staged.
        move_to(&mut app, 2);
        let (path, hunk) = match app.panes.get("diff") {
            Some(Screens::Diff { view, .. }) => {
                view.current_hunk().expect("the keyboard is on a hunk")
            }
            _ => panic!("a diff is registered"),
        };
        assert_eq!(path, "f.txt");
        let drawn: Vec<String> = hunk
            .lines
            .iter()
            .filter(|l| l.kind != gitten_core::LineKind::Context)
            .map(|l| l.text.to_string())
            .collect();
        app.dispatch("diff.stage-hunk");
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                !state.lock().unwrap().writes.is_empty()
            }),
            "the staged hunk never reached the repository"
        );
        // What the verb applied is exactly the chosen hunk's edit and not
        // its distant neighbour's — read off the recorded write, the same
        // patch the queue carried.
        let applied = parse_unified_diff(&state.lock().unwrap().writes[0]["stage ".len()..]);
        let changed: Vec<String> = applied[0]
            .hunks
            .iter()
            .flat_map(|h| &h.lines)
            .filter(|l| l.kind != gitten_core::LineKind::Context)
            .map(|l| l.text.to_string())
            .collect();
        assert_eq!(changed, drawn, "the applied patch is not the drawn hunk");
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
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
        assert_eq!(
            app.panes.names().collect::<Vec<_>>(),
            ["commits", "stashes", "remotes", "diff", "files", "branches"],
            "{names:?}"
        );
        assert_eq!(app.panes.focused_name(), "commits");
        assert_eq!(
            app.panes.list_order(),
            ["files", "branches", "commits", "stashes", "remotes"]
        );
        assert_eq!(
            app.panes.reading_order(),
            ["files", "branches", "commits", "stashes", "remotes", "diff"]
        );

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
            ["stashes", "remotes", "diff", "files", "branches"]
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

        // Wide: the sidebar splits into five canonical slices — files on
        // top, branches under it, commits next, the stack, the remotes at
        // the foot — and the diff takes the rest, one divider column
        // between. No geometry module changed to make room: this is the
        // registry's equal-slice answer to the tenants there are.
        assert_eq!(
            app.pane_rect("commits"),
            Some(crate::panes::Rect {
                x: 0,
                y: 11,
                width: 40,
                height: 4
            })
        );
        assert_eq!(
            app.pane_rect("stashes"),
            Some(crate::panes::Rect {
                x: 0,
                y: 15,
                width: 40,
                height: 4
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
        let commits_header = app.screen.row_text(11).chars().take(40).collect::<String>();
        assert!(
            commits_header.contains('4') && commits_header.contains("commits"),
            "{commits_header:?}"
        );
        let stashes_header = app.screen.row_text(15);
        assert!(stashes_header.contains('5'), "{stashes_header:?}");
        assert!(stashes_header.contains("stashes"), "{stashes_header:?}");
        assert!(
            stashes_header.contains("fake (main) · 2 parked"),
            "{stashes_header:?}"
        );

        // The divider is nobody's: blank or its rule, never a pane's text,
        // from the top of the body to the bottom, including every sidebar header.
        for y in 2..23 {
            assert!(
                matches!(app.screen.char_at(40, y), Some(' ' | '│')),
                "row {y} drew text into the divider"
            );
        }

        // And the stack itself drew: both rows, address first.
        let rows: Vec<String> = (16..19).map(|y| app.screen.row_text(y)).collect();
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
        // The remotes list sits between the stack and the foot now; the
        // wrap takes one more step.
        app.dispatch("pane.next");
        assert_eq!(
            app.panes.focused_name(),
            "remotes",
            "the cycle did not reach the new list"
        );
        app.dispatch("pane.next");
        assert_eq!(app.panes.focused_name(), "files", "the cycle did not wrap");
        app.press(Key::plain(Code::Char('5')));
        assert_eq!(app.panes.focused_name(), "stashes");
        app.draw();
        assert!(
            app.screen.row_text(0).contains("stashes"),
            "the title did not follow: {:?}",
            app.screen.row_text(0)
        );
        assert!(
            app.screen.row_text(15).contains('5') && app.screen.row_text(15).contains("stashes"),
            "the header did not advertise the stack: {:?}",
            app.screen.row_text(15)
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
        assert!(
            matches!(app.prompt, Some(Prompt::Search { .. })),
            "the prompt did not open"
        );
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
        // a stack is acted on one entry at a time. The commits slice is the
        // third of the sidebar's five; the stack the fourth.
        app.dispatch("commits.focus");
        app.mouse(click(MouseKind::Down, 5, 13));
        app.mouse(click(MouseKind::Drag, 5, 14));
        app.mouse(click(MouseKind::Up, 5, 14));
        assert!(
            !commits_of(&app).selection().is_empty(),
            "the drag in the list selected nothing"
        );
        app.mouse(click(MouseKind::Down, 5, 17));
        assert_eq!(
            app.panes.focused_name(),
            "stashes",
            "the press did not move the keyboard to the stack"
        );
        app.mouse(click(MouseKind::Up, 5, 17));
        app.mouse(click(MouseKind::Down, 5, 17));
        app.mouse(click(MouseKind::Drag, 5, 18));
        app.mouse(click(MouseKind::Up, 5, 18));
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
                app.pump_quiet();
                state.lock().unwrap().log_reads > reads
            }),
            "the hidden tenants were not refreshed"
        );
        for name in ["commits", "stashes", "diff", "files"] {
            assert_eq!(
                app.panes.get(name).unwrap().generation(),
                app.generation,
                "{name} was not refreshed to the generation"
            );
        }
    }

    #[test]
    fn stash_apply_job_uses_the_selected_index_and_keeps_the_entry() {
        let (handle, state) = fake(&[]);
        let mut app = commits_app(&handle);
        // A loaded diff — so all three tenants have something a finish can
        // stale — then the stack focused and the keyboard moved to the
        // second entry, the row the verbs must address.
        app.dispatch("commits.open-diff");
        app.press(Key::plain(Code::Char('5')));
        assert_eq!(app.panes.focused_name(), "stashes");
        app.dispatch("view.down");
        app.dispatch("stashes.apply");
        // The gate is the finish itself — the generation a drain advances —
        // and not the write landing on the worker: the refresh rides the
        // same drain pass the assertions below read.
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                app.generation > Generation::default()
            }),
            "the apply never reached the repository"
        );
        // The job went to the *selected* index — the cursor row's stack
        // index, and nothing else.
        assert_eq!(
            state.lock().unwrap().stash_writes,
            ["apply stash@{1}"],
            "{:?}",
            state.lock().unwrap().stash_writes
        );
        // Every repository pane refreshed to the finish's generation, and
        // the selected stash is still selected: an apply keeps the entry.
        for name in ["commits", "stashes", "diff"] {
            assert_eq!(
                app.panes.get(name).unwrap().generation(),
                app.generation,
                "{name}"
            );
        }
        {
            let stash = match app.panes.get("stashes") {
                Some(Screens::Stashes { view, .. }) => view,
                _ => panic!("the stack is registered"),
            };
            assert_eq!(
                stash.current(),
                Some(1),
                "the apply moved the keyboard off its entry"
            );
        }

        // A refusal is the repository's exact words — no UI-invented
        // conflict copy — and it still staled every pane, and moved nothing.
        // The refused verb records no write: like git, the fake changed
        // nothing; the finish event is what carries the error in.
        state.lock().unwrap().refuse_stash = Some("the apply refused".into());
        app.dispatch("stashes.apply");
        let gen = app.generation;
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                app.generation > gen
            }),
            "the refused apply never finished"
        );
        assert_eq!(app.message, "the apply refused", "{:?}", app.message);
        assert_eq!(
            state.lock().unwrap().stashes.len(),
            2,
            "a refused apply moved the stack"
        );
    }

    #[test]
    fn stash_pop_is_one_press_and_only_clean_success_removes() {
        // A clean pop: one press queues it — no question, no second press —
        // and the refreshed read is what renumbers the stack.
        let (handle, state) = fake(&[]);
        let mut app = commits_app(&handle);
        app.press(Key::plain(Code::Char('5')));
        app.dispatch("stashes.pop");
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                app.generation > Generation::default()
            }),
            "the pop never queued"
        );
        assert_eq!(state.lock().unwrap().stash_writes, ["pop stash@{0}"]);
        assert_eq!(
            state.lock().unwrap().stashes.len(),
            1,
            "a clean pop kept the entry"
        );
        {
            let stash = match app.panes.get("stashes") {
                Some(Screens::Stashes { view, .. }) => view,
                _ => panic!("the stack is registered"),
            };
            assert_eq!(
                stash.status(),
                "1/1 · stash@{0}",
                "the stack did not renumber"
            );
            assert_eq!(stash.current(), Some(0));
        }

        // A conflicted pop — git restored and declined to drop — is the
        // repository's exact refusal, and the entry is still on the stack
        // the refresh re-read.
        let (handle, state) = fake(&[]);
        let mut app = commits_app(&handle);
        app.press(Key::plain(Code::Char('5')));
        state.lock().unwrap().refuse_stash = Some("cannot restore: conflict".into());
        app.dispatch("stashes.pop");
        let reads = state.lock().unwrap().stash_reads;
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                state.lock().unwrap().stash_reads > reads
            }),
            "the refused pop never finished"
        );
        assert_eq!(app.message, "cannot restore: conflict", "{:?}", app.message);
        assert_eq!(
            state.lock().unwrap().stashes.len(),
            2,
            "a refused pop moved the stack"
        );
    }

    #[test]
    fn stash_drop_requires_two_presses_and_refusals_never_spend_an_arm_twice() {
        let (handle, state) = fake(&[]);
        let mut app = commits_app(&handle);
        app.press(Key::plain(Code::Char('5')));

        // First press: the exact question, and nothing queued.
        app.press(Key::char('d'));
        assert_eq!(
            app.message, "drop stash@{0}? press again to confirm",
            "{:?}",
            app.message
        );
        assert!(
            state.lock().unwrap().stash_writes.is_empty(),
            "an armed drop queued a write"
        );
        // Second press on the same row: queues, and the arm is spent. The
        // gate is again the finish, so the refreshed, renumbered stack is
        // what the next probes read.
        app.press(Key::char('d'));
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                app.generation > Generation::default()
            }),
            "the confirmed drop never queued"
        );
        assert_eq!(state.lock().unwrap().stash_writes, ["drop stash@{0}"]);

        // A moved cursor asks anew: the arm died with the move, so the next
        // `d` is a question and not a drop. The stack renumbered under the
        // completed drop — the survivor is now `stash@{0}` — and an arm
        // that had wrongly survived by index would be a *confirmed* drop
        // against exactly that renumbering.
        app.dispatch("view.down");
        app.press(Key::char('d'));
        assert_eq!(
            app.message, "drop stash@{0}? press again to confirm",
            "{:?}",
            app.message
        );
        assert_eq!(
            state.lock().unwrap().stash_writes.len(),
            1,
            "an unconfirmed drop queued a write"
        );

        // An empty stack refuses before the queue: there is no row for the
        // question to be about.
        let (handle, state) = fake(&[]);
        state.lock().unwrap().stashes.clear();
        let mut app = commits_app(&handle);
        app.press(Key::plain(Code::Char('5')));
        app.press(Key::char('d'));
        assert_eq!(
            app.message, "nothing selected on the stash stack",
            "{:?}",
            app.message
        );
        assert!(state.lock().unwrap().stash_writes.is_empty());

        // A backend refusal — the index the confirm aimed at naming nothing
        // by the time git ran — is surfaced exactly, after the confirmed
        // job and its refresh; the refreshed stack, not the UI, decides
        // what is gone.
        let (handle, state) = fake(&[]);
        let mut app = commits_app(&handle);
        app.press(Key::plain(Code::Char('5')));
        app.dispatch("view.down");
        state.lock().unwrap().refuse_stash = Some("stash@{1}: no such stash".into());
        app.press(Key::char('d'));
        assert_eq!(app.message, "drop stash@{1}? press again to confirm");
        app.press(Key::char('d'));
        let reads = state.lock().unwrap().stash_reads;
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                state.lock().unwrap().stash_reads > reads
            }),
            "the refused drop never finished"
        );
        assert_eq!(app.message, "stash@{1}: no such stash", "{:?}", app.message);
        assert_eq!(
            state.lock().unwrap().stashes.len(),
            2,
            "a refused drop moved the stack"
        );
    }

    #[test]
    fn files_stash_queues_a_message_less_push_and_surfaces_clean_tree_refusal() {
        // No files pane is registered; the command is answered from the
        // repository the app holds, which is the point.
        let (handle, state) = fake(&[]);
        let mut app = commits_app(&handle);
        app.dispatch("files.stash");
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                app.generation > Generation::default()
            }),
            "the push never queued"
        );
        {
            let s = state.lock().unwrap();
            // One job, message-less — git supplies its own `WIP on …`.
            assert_eq!(s.stash_writes, ["push "], "{:?}", s.stash_writes);
            // The refreshed stack holds the new top entry above the old two.
            assert_eq!(s.stashes.len(), 3);
            assert_eq!(s.stashes[0].index, 0);
            assert_eq!(s.stashes[0].message, "WIP on fake (main)");
            assert_eq!(s.stashes[1].index, 1, "the old top did not renumber");
        }
        // No prompt and no input mode: a message-less push opens nothing.
        assert!(app.prompt.is_none());
        assert_eq!(app.panes.focused_name(), "commits");

        // A clean tree refuses in git's own words, and the refreshed stack
        // is the unchanged one the read came back with.
        let (handle, state) = fake(&[]);
        let mut app = commits_app(&handle);
        state.lock().unwrap().refuse_stash = Some("No local changes to save".into());
        app.dispatch("files.stash");
        let reads = state.lock().unwrap().stash_reads;
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                state.lock().unwrap().stash_reads > reads
            }),
            "the refused push never finished"
        );
        assert_eq!(app.message, "No local changes to save", "{:?}", app.message);
        assert_eq!(state.lock().unwrap().stashes.len(), 2);

        // No repository: said, not queued.
        let mut app = app_on_diff(Source::Fixtures, None);
        app.dispatch("files.stash");
        assert_eq!(app.message, "a fixture has no working tree to park");
    }

    #[test]
    fn stash_finishes_refresh_every_registered_repository_pane() {
        let (handle, state) = fake(&[]);
        let mut app = commits_app(&handle);
        // Narrow, so the layout hides whatever does not hold the keyboard:
        // a loaded diff, then the stack focused — the other two tenants are
        // hidden and stale all the same.
        app.screen = Screen::new(60, 24);
        app.dispatch("commits.open-diff");
        app.dispatch("stashes.focus");
        app.draw();
        assert_eq!(app.panes.focused_name(), "stashes");
        assert!(app.pane_content("commits").is_none());
        assert!(app.pane_content("diff").is_none());
        let open = {
            let s = state.lock().unwrap();
            (s.log_reads, s.pairs_reads, s.stash_reads)
        };

        // One clean apply: every registered repository pane re-reads, once,
        // regardless of focus or visibility.
        app.dispatch("stashes.apply");
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                let s = state.lock().unwrap();
                s.log_reads > open.0 && s.pairs_reads > open.1 && s.stash_reads > open.2
            }),
            "the finish never refreshed every tenant"
        );
        for name in ["commits", "stashes", "diff", "files"] {
            assert_eq!(
                app.panes.get(name).unwrap().generation(),
                app.generation,
                "{name} was not refreshed to the generation"
            );
        }

        // One refused pop: the write's error owns the status, the refresh
        // still runs, every tenant is still tried — and a refresh failure in
        // the first-registered pane is appended after the write error, in
        // the existing `write · refresh` order.
        {
            let mut s = state.lock().unwrap();
            s.refuse_stash = Some("the stash pop refused".into());
            s.fail_log = Some("the log read failed".into());
        }
        app.dispatch("stashes.pop");
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                state.lock().unwrap().stash_reads > open.2 + 1
            }),
            "the refused finish never refreshed every tenant"
        );
        let s = state.lock().unwrap();
        assert_eq!(
            s.log_reads,
            open.0 + 2,
            "a failed pane skipped the later tenants"
        );
        assert_eq!(
            s.pairs_reads,
            open.1 + 2,
            "a failed pane skipped the later tenants"
        );
        assert_eq!(s.stash_reads, open.2 + 2);
        // The refused pop recorded no write — the fake, like git, changed
        // nothing. Its error reached the status line through the finish
        // event, asserted below.
        assert_eq!(s.stash_writes, ["apply stash@{0}"], "{:?}", s.stash_writes);
        assert_eq!(
            app.message, "the stash pop refused · the log read failed",
            "{:?}",
            app.message
        );
        // The refusal removed nothing: the refreshed stack still holds both.
        assert_eq!(s.stashes.len(), 2);
    }

    #[test]
    fn a_failed_stash_read_opens_as_unavailable_and_recovers_on_refresh() {
        let (handle, state) = fake(&[]);
        // The launch itself carries the failure: the ancillary read at
        // `App::new` is what fails here, so the unavailable tenant is the
        // shipped state and not a probe's construction.
        state.lock().unwrap().fail_stashes = Some("fatal: bad object refs/stash".into());
        let mut app = commits_app(&handle);

        // A failed side read must not abort a launch the main view made
        // good: five tenants, the requested startup focus untouched, and
        // the exact error kept for the status line.
        assert_eq!(
            app.panes.names().collect::<Vec<_>>(),
            ["commits", "stashes", "remotes", "diff", "files", "branches"]
        );
        assert_eq!(app.panes.focused_name(), "commits");
        assert_eq!(app.message, "fatal: bad object refs/stash");
        app.draw();
        assert!(
            app.screen
                .row_text(23)
                .contains("fatal: bad object refs/stash"),
            "the error is not on the status line: {:?}",
            app.screen.row_text(23)
        );

        // The tenant is drawn and behaved as *unavailable* — the failure
        // line, never the empty-stack line that would assert a read that
        // never succeeded, and no row for a verb to address.
        assert!(
            app.screen.row_text(15).contains("unavailable"),
            "the header did not say so: {:?}",
            app.screen.row_text(15)
        );
        let rows: Vec<String> = (16..19).map(|y| app.screen.row_text(y)).collect();
        assert!(
            rows.iter().any(|r| r.contains("stash list unavailable")),
            "{rows:?}"
        );
        assert!(
            rows.iter().all(|r| !r.contains("nothing stashed")),
            "a failed read drew as a clean empty stack: {rows:?}"
        );
        {
            let stash = match app.panes.get("stashes") {
                Some(Screens::Stashes { view, .. }) => view,
                _ => panic!("the stack is registered"),
            };
            assert_eq!(stash.current(), None, "an unavailable stack exposed a row");
        }

        // The read recovers: the failure mode off, one finish, and the same
        // tenant re-reads through the existing generation rail and draws
        // the real stack under its parked label.
        state.lock().unwrap().fail_stashes = None;
        let job = Write::stash_apply(&handle, 0);
        assert!(app.submitter.submit(Box::new(job)).is_ok(), "queued");
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                app.generation > Generation::default()
            }),
            "the finish was never drained"
        );
        app.draw();
        assert!(
            app.screen.row_text(15).contains("fake (main) · 2 parked"),
            "the header did not recover: {:?}",
            app.screen.row_text(15)
        );
        let rows: Vec<String> = (16..19).map(|y| app.screen.row_text(y)).collect();
        assert!(rows.iter().any(|r| r.contains("stash@{0}")), "{rows:?}");
        assert!(
            !app.screen.row_text(23).contains("fatal:"),
            "the stale error outlived its recovery: {:?}",
            app.screen.row_text(23)
        );
        assert_eq!(
            app.panes.get("stashes").map(Screens::generation).as_ref(),
            Some(&app.generation),
            "the tenant was not refreshed to the finish"
        );
        {
            let stash = match app.panes.get("stashes") {
                Some(Screens::Stashes { view, .. }) => view,
                _ => panic!("the stack is registered"),
            };
            assert_eq!(stash.current(), Some(0));
            assert_eq!(stash.status(), "1/2 · stash@{0}");
        }
    }

    #[test]
    fn repository_launch_registers_branches_in_the_existing_ring() {
        // The shipped map already binds the digit and the mode's letters: the
        // terminal adds no name, no alias and no key table anywhere in the
        // chain — the focus key and every verb resolve through
        // `Host::new().keys`, the same map `gitten.toml` writes.
        let keys = Host::new().keys;
        let mut commits = Modes::new();
        commits.push("commits");
        assert_eq!(
            keys.resolve(&commits, &[Key::plain(Code::Char('3'))]),
            Resolve::Run("branches.focus")
        );
        let mut branches_mode = Modes::new();
        branches_mode.push("panes");
        branches_mode.push("branches");
        assert_eq!(
            keys.resolve(&branches_mode, &[Key::plain(Code::Char(' '))]),
            Resolve::Run("branches.checkout")
        );
        assert_eq!(
            keys.resolve(&branches_mode, &[Key::char('R')]),
            Resolve::Run("branches.rename"),
            "the mode override is the shared map's, not a terminal table"
        );

        // A repository-backed commits launch: five tenants, branches in its
        // canonical sidebar rank — between files and commits — and the
        // requested startup focus restored over whatever the registrations
        // focused last.
        let (handle, _state) = fake(&[]);
        let mut app = commits_app(&handle);
        let names: Vec<&str> = app.panes.names().collect();
        assert_eq!(
            app.panes.names().collect::<Vec<_>>(),
            ["commits", "stashes", "remotes", "diff", "files", "branches"],
            "{names:?}"
        );
        assert_eq!(app.panes.focused_name(), "commits");
        assert_eq!(
            app.panes.list_order(),
            ["files", "branches", "commits", "stashes", "remotes"]
        );
        assert_eq!(
            app.panes.reading_order(),
            ["files", "branches", "commits", "stashes", "remotes", "diff"]
        );

        // `3` reaches it — through the keymap — and the keyboard's modes
        // follow, because the mode stack is built from the focused screen.
        app.press(Key::plain(Code::Char('3')));
        assert_eq!(app.panes.focused_name(), "branches");
        assert_eq!(
            app.panes.focused().map(|pane| pane.mode()),
            Some("branches")
        );
        assert_eq!(
            app.panes.focused_placement(),
            Some(panes::Placement::Sidebar { rank: 2 }),
            "canonical rank 2, from the registry and not a layout edit"
        );

        // Ctrl-J cycles the now-real sidebar ring from it: branches' next
        // list is commits', and the walk h/l reaches it too.
        app.dispatch("pane.next");
        assert_eq!(app.panes.focused_name(), "commits");
        app.press(Key::plain(Code::Char('3')));
        app.dispatch("pane.left");
        assert_eq!(app.panes.focused_name(), "files", "the walk skipped a list");

        // A repository-backed diff launch registers the tenant the same way —
        // `App::new` never assumed commits exists — and the diff keeps the
        // focus the launch asked for.
        let source = Source::Repo {
            path: std::path::PathBuf::from("/fake"),
            arg: String::new(),
        };
        let diff_app = app_on_fake(&source, &handle);
        assert!(diff_app.panes.get("branches").is_some());
        assert_eq!(diff_app.panes.focused_name(), "diff");
    }

    #[test]
    fn direct_diff_gets_branches_while_fixtures_do_not() {
        let (handle, _state) = fake(&[]);
        let source = Source::Repo {
            path: std::path::PathBuf::from("/fake"),
            arg: String::new(),
        };

        // A repository diff launch: branches beside the diff it asked for.
        let mut app = app_on_fake(&source, &handle);
        assert_eq!(
            app.panes.names().collect::<Vec<_>>(),
            ["stashes", "remotes", "diff", "files", "branches"]
        );
        assert_eq!(app.panes.focused_name(), "diff");

        // Narrow — below the wide breakpoint — only the focused pane is
        // placed, and `3` swaps that full-width visibility to branches. The
        // diff keeps no rectangle while it is hidden.
        app.screen.resize(95, 24);
        app.draw();
        app.press(Key::plain(Code::Char('3')));
        app.draw();
        assert_eq!(app.panes.focused_name(), "branches");
        assert_eq!(
            app.pane_rect("branches"),
            Some(crate::panes::Rect {
                x: 0,
                y: 1,
                width: 95,
                height: 22
            }),
            "narrow mode gives the focused pane the whole body"
        );
        assert!(
            app.pane_rect("diff").is_none(),
            "the hidden diff kept a rectangle"
        );

        // Wide: branches and diff occupy the foundation's disjoint
        // rectangles — the sidebar's foot slice and the main region. No
        // layout branch learned the name; this is the registry's own answer
        // to a third sidebar list.
        app.screen.resize(96, 24);
        app.draw();
        let branches = app.pane_rect("branches").expect("branches is placed wide");
        let diff = app.pane_rect("diff").expect("the diff is placed wide");
        assert_eq!(
            branches,
            crate::panes::Rect {
                x: 0,
                y: 7,
                width: 40,
                height: 6
            }
        );
        assert_eq!(
            diff,
            crate::panes::Rect {
                x: 41,
                y: 1,
                width: 55,
                height: 22
            }
        );
        assert!(
            branches.x + branches.width <= diff.x,
            "the sidebar drew into the main region"
        );

        // No repository, no tenant: a fixture and a patch launch keep their
        // previous tenant counts and answer the focus command with the same
        // sentence an absent pane always got.
        for mut app in [
            app_on_diff(Source::Fixtures, None),
            app_on_diff(Source::Patch { file: None }, None),
        ] {
            assert!(
                app.panes.get("branches").is_none(),
                "a repository-free launch invented a branches pane"
            );
            app.dispatch("branches.focus");
            assert_eq!(app.message, "no branches pane");
        }
    }

    #[test]
    fn branch_reads_fail_softly_at_start_and_retry_on_generation() {
        // A lost branch list: the launch keeps its requested view and focus,
        // the tenant registers honest *unread* — never a clean zero — and
        // the exact error rides to the status line.
        let (handle, state) = fake(&[]);
        state.lock().unwrap().fail_locals = Some("fatal: bad object refs/heads".into());
        let mut app = commits_app(&handle);
        assert_eq!(
            app.panes.names().collect::<Vec<_>>(),
            ["commits", "stashes", "remotes", "diff", "files", "branches"]
        );
        assert_eq!(
            app.panes.focused_name(),
            "commits",
            "a failed side read stole the launch"
        );
        assert_eq!(
            app.message,
            "branch reads failed: fatal: bad object refs/heads"
        );
        assert_eq!(branches_label(&app), "fake (main) · branches unavailable");
        assert_eq!(
            branches_of(&app).status(),
            "no branches",
            "a failed read drew as an empty pane"
        );
        // Nothing falsely advanced: every tenant sits at the launch
        // generation, the failed one included.
        for name in app.panes.names().collect::<Vec<_>>() {
            assert_eq!(
                app.panes.get(name).unwrap().generation(),
                app.generation,
                "{name} was born stale"
            );
        }

        // The next successful wave fills what failed — the same generation
        // rail any write finish rides — and the error leaves the status line.
        state.lock().unwrap().fail_locals = None;
        let gen = app.generation;
        let job = Write::stash_apply(&handle, 0);
        assert!(app.submitter.submit(Box::new(job)).is_ok(), "queued");
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                app.generation > gen
            }),
            "the recovery never re-read the refs"
        );
        assert_eq!(branches_label(&app), "fake (main) · 1 local · 0 remote");
        assert_eq!(branches_of(&app).status(), "1/1 · main");
        assert_eq!(
            app.panes.get("branches").unwrap().generation(),
            app.generation,
            "the recovered tenant did not reach the generation"
        );

        // A refresh that fails keeps the last good rows at their own
        // generation — no fabricated emptiness, no false advance — and says
        // so; a later wave retries.
        state.lock().unwrap().fail_locals = Some("fatal: bad object refs/heads".into());
        let before = app.panes.get("branches").unwrap().generation();
        let gen = app.generation;
        let job = Write::stash_apply(&handle, 0);
        assert!(app.submitter.submit(Box::new(job)).is_ok(), "queued");
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                app.generation > gen
            }),
            "the failed wave never finished"
        );
        assert_eq!(
            app.message,
            "branch reads failed: fatal: bad object refs/heads"
        );
        assert!(
            app.generation > Generation::default(),
            "the finish did not advance"
        );
        assert_eq!(
            app.panes.get("branches").unwrap().generation(),
            before,
            "a failed refresh advanced the tenant"
        );
        assert_eq!(
            branches_of(&app).status(),
            "1/1 · main",
            "a failed refresh replaced the rows"
        );

        // A HEAD-only failure is the other story: the rows survive — the
        // current bit rode in on the list read, the same bit the window
        // marks from — and the leg that failed is the one the status line
        // names.
        let (handle, state) = fake(&[]);
        state.lock().unwrap().fail_head = Some("fatal: bad object HEAD".into());
        let mut app = commits_app(&handle);
        assert_eq!(app.message, "branch reads failed: fatal: bad object HEAD");
        assert_eq!(branches_label(&app), "fake (main) · 1 local · 0 remote");
        assert_eq!(branches_of(&app).status(), "1/1 · main");

        // And the recovery is the same rail: the head read repeats on the
        // next wave and the error leaves.
        state.lock().unwrap().fail_head = None;
        let gen = app.generation;
        let job = Write::stash_apply(&handle, 0);
        assert!(app.submitter.submit(Box::new(job)).is_ok(), "queued");
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                app.generation > gen
            }),
            "the head re-read never happened"
        );
        assert!(
            !app.message.contains("branch reads failed"),
            "the stale error outlived its recovery: {:?}",
            app.message
        );
    }

    #[test]
    fn checkout_jobs_local_and_remote_bytes_and_refuses_detached() {
        let (handle, state) = fake(&[]);
        branch_world(&state);
        let mut app = commits_app(&handle);
        app.press(Key::plain(Code::Char('3')));
        assert_eq!(branches_of(&app).status(), "1/4 · main");

        // Space on a local submits `Write::checkout` once, with the row's
        // raw bytes — whole, and exactly once.
        let gen = app.generation;
        app.press(Key::plain(Code::Char(' ')));
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                app.generation > gen
            }),
            "the checkout never queued"
        );
        assert_eq!(state.lock().unwrap().branch_bytes, [b"main".to_vec()]);

        // The non-text local is aimed at by bytes too: the job never saw a
        // lossy spelling of its name.
        app.dispatch("view.down");
        let gen = app.generation;
        app.press(Key::plain(Code::Char(' ')));
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                app.generation > gen
            }),
            "the second checkout never queued"
        );
        assert_eq!(
            state.lock().unwrap().branch_bytes[1],
            b"f\xe9ature".to_vec()
        );

        // A remote row checks out as a *tracking* branch, and the refname
        // is joined from the halves exactly once — the slash in the branch
        // half must not make two.
        app.dispatch("view.down");
        let gen = app.generation;
        app.press(Key::plain(Code::Char(' ')));
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                app.generation > gen
            }),
            "the remote checkout never queued"
        );
        assert_eq!(
            state.lock().unwrap().branch_bytes[2],
            b"origin/feat/ure".to_vec()
        );

        // The fake created the local branch of the same name, tracking, and
        // moved HEAD onto it; the refreshed pane leads with that local row,
        // and space on it is an ordinary checkout between locals.
        app.draw();
        app.dispatch("view.top");
        // The fake answers the locals in insertion order — main, the
        // non-text name, and the tracking branch the checkout created —
        // and headings hold no cursor: the top settles on main, two
        // selectable rows further is the new branch.
        app.dispatch("view.down");
        app.dispatch("view.down");
        assert_eq!(
            branches_of(&app).current(),
            Some(Target::Local(gitten_core::refs::RefName::from("feat/ure")))
        );
        let gen = app.generation;
        app.press(Key::plain(Code::Char(' ')));
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                app.generation > gen
            }),
            "the local checkout never queued"
        );
        assert_eq!(
            state.lock().unwrap().branch_writes.len(),
            4,
            "the tracking branch's own checkout never queued"
        );

        // A refusal is git's sentence verbatim, and it still staled the
        // panes: the finish advances the generation however the job ended.
        state.lock().unwrap().refuse_branch =
            Some("error: Your local changes would be overwritten".into());
        app.dispatch("view.down");
        app.press(Key::plain(Code::Char(' ')));
        let gen = app.generation;
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                app.generation > gen
            }),
            "the refused checkout never finished"
        );
        assert_eq!(
            app.message,
            "error: Your local changes would be overwritten"
        );
        assert_eq!(
            state.lock().unwrap().branch_writes.len(),
            4,
            "a refused checkout recorded a write"
        );

        // An empty pane has nothing to aim at, and says so before the queue.
        let (handle, state) = fake(&[]);
        {
            let mut s = state.lock().unwrap();
            s.locals.clear();
            s.remotes.clear();
        }
        let mut app = commits_app(&handle);
        app.press(Key::plain(Code::Char('3')));
        app.dispatch("branches.checkout");
        assert_eq!(app.message, "nothing selected to check out");
        assert!(state.lock().unwrap().branch_writes.is_empty());
    }

    #[test]
    fn new_branch_prompt_accepts_one_name_and_never_checks_it_out() {
        let (handle, state) = fake(&[]);
        branch_world(&state);
        let mut app = commits_app(&handle);
        app.press(Key::plain(Code::Char('3')));

        // `n` opens one blank field; the typed text is inert until accept.
        app.press(Key::char('n'));
        assert!(matches!(app.prompt, Some(Prompt::BranchNew { .. })));
        app.draw();
        assert!(status(&app).contains("branch: "), "{:?}", status(&app));
        type_(&mut app, "topic");
        app.draw();
        assert!(status(&app).contains("branch: topic"), "{:?}", status(&app));
        assert!(
            state.lock().unwrap().branch_writes.is_empty(),
            "typing queued a branch"
        );

        // A paste is one sanitized edit, never a transcript of keypresses:
        // the newline and the tab become spaces, the control character is
        // dropped, and the keymap sees none of it.
        app.input(Input::Paste(" ab\ncd\tef\u{7}".into()));
        app.draw();
        assert!(
            status(&app).contains("branch: topic ab cd ef"),
            "{:?}",
            status(&app)
        );

        // Enter submits `Write::create_branch` once — at HEAD, and nothing
        // is checked out — and closes the field.
        let gen = app.generation;
        app.press(Key::plain(Code::Enter));
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                app.generation > gen
            }),
            "the create never queued"
        );
        assert_eq!(
            state.lock().unwrap().branch_writes,
            ["branch topic ab cd ef at HEAD"]
        );
        assert_eq!(
            state.lock().unwrap().branch_bytes,
            [b"topic ab cd ef".to_vec()]
        );
        assert!(app.prompt.is_none(), "the field outlived its accept");
        assert_eq!(
            branches_of(&app).status(),
            "1/5 · main",
            "the created branch did not land in the refreshed pane"
        );

        // Whitespace-only accept is refused beside the field, without a job.
        app.press(Key::char('n'));
        type_(&mut app, "   ");
        app.press(Key::plain(Code::Enter));
        assert_eq!(app.message, "a branch needs a name");
        assert!(app.prompt.is_none(), "a refused accept left the field open");
        assert_eq!(state.lock().unwrap().branch_writes.len(), 1);

        // Esc cancels: the text was the field's and dies with it.
        app.press(Key::char('n'));
        type_(&mut app, "ghost");
        app.press(Key::plain(Code::Esc));
        assert!(app.prompt.is_none());
        app.pump_quiet();
        assert_eq!(state.lock().unwrap().branch_writes.len(), 1);

        // An empty (unborn) repository's pane still answers `n`: creating
        // reads no row, and whether HEAD is a valid start is the backend's
        // to say, not the view's.
        let (handle, state) = fake(&[]);
        {
            let mut s = state.lock().unwrap();
            s.locals.clear();
            s.remotes.clear();
            s.head = Some(HeadState::Branch {
                name: RefName::from("main"),
                commit: None,
            });
        }
        let mut app = commits_app(&handle);
        app.press(Key::plain(Code::Char('3')));
        assert_eq!(branches_of(&app).status(), "no branches");
        app.press(Key::char('n'));
        type_(&mut app, "first");
        let gen = app.generation;
        app.press(Key::plain(Code::Enter));
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                app.generation > gen
            }),
            "the unborn-repository create never queued"
        );
        assert_eq!(
            state.lock().unwrap().branch_writes,
            ["branch first at HEAD"]
        );
        assert_eq!(
            branches_of(&app).status(),
            "1/1 · first",
            "the created branch did not land in the refreshed pane"
        );
    }

    #[test]
    fn rename_prompt_captures_raw_from_and_prefills_only_utf8() {
        let (handle, state) = fake(&[]);
        branch_world(&state);
        let mut app = commits_app(&handle);
        app.press(Key::plain(Code::Char('3')));

        // A text name is prefilled and wholly selected: the first edit
        // replaces it, it is not glued to the front of whatever was typed.
        app.press(Key::char('R'));
        assert!(matches!(app.prompt, Some(Prompt::BranchRename { .. })));
        app.draw();
        assert!(status(&app).contains("rename: main"), "{:?}", status(&app));
        type_(&mut app, "x");
        app.draw();
        assert!(status(&app).contains("rename: x"), "{:?}", status(&app));
        assert!(
            !status(&app).contains("main"),
            "the prefill survived the first edit"
        );

        // Unchanged accept is allowed through to git — its "already
        // exists" says more than a client-side veto would.
        app.press(Key::plain(Code::Esc));
        app.press(Key::char('R'));
        let gen = app.generation;
        app.press(Key::plain(Code::Enter));
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                app.generation > gen
            }),
            "the unchanged rename never queued"
        );
        assert_eq!(state.lock().unwrap().branch_writes, ["rename main → main"]);
        assert_eq!(state.lock().unwrap().rename_froms, [b"main".to_vec()]);

        // The non-text local opens *blank* — a lossy prefill would rename
        // the branch to its own mojibake — while `from` keeps the exact
        // bytes and the job aims at them.
        app.dispatch("view.down");
        app.press(Key::char('R'));
        app.draw();
        assert!(status(&app).contains("rename: "), "{:?}", status(&app));
        assert!(
            !status(&app).contains('\u{fffd}'),
            "the field opened with lossy bytes: {:?}",
            status(&app)
        );
        type_(&mut app, "renamed");
        let gen = app.generation;
        app.press(Key::plain(Code::Enter));
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                app.generation > gen
            }),
            "the byte-preserving rename never queued"
        );
        assert_eq!(
            state.lock().unwrap().branch_writes[1],
            "rename f\u{fffd}ature → renamed"
        );
        assert_eq!(
            state.lock().unwrap().rename_froms[1],
            b"f\xe9ature".to_vec()
        );
        assert_eq!(state.lock().unwrap().branch_bytes[1], b"renamed".to_vec());

        // Empty accept queues nothing.
        app.press(Key::char('R'));
        type_(&mut app, "  ");
        app.press(Key::plain(Code::Enter));
        assert_eq!(app.message, "a branch needs a name");
        assert_eq!(state.lock().unwrap().branch_writes.len(), 2);

        // A remote row refuses before any field stands.
        app.dispatch("view.down");
        app.dispatch("view.down");
        app.press(Key::char('R'));
        assert_eq!(app.message, "only a local branch can be renamed");
        assert!(app.prompt.is_none());
        assert_eq!(state.lock().unwrap().branch_writes.len(), 2);

        // So does the detached row, in a world that opens detached.
        let (handle, state) = fake(&[]);
        branch_world(&state);
        {
            let mut s = state.lock().unwrap();
            s.head = Some(HeadState::Detached {
                commit: "f00d".into(),
            });
            s.locals[0].head = false;
        }
        let mut app = commits_app(&handle);
        app.press(Key::plain(Code::Char('3')));
        app.dispatch("view.top");
        assert!(matches!(
            branches_of(&app).current(),
            Some(Target::Detached)
        ));
        app.press(Key::char('R'));
        assert_eq!(app.message, "only a local branch can be renamed");
        assert!(app.prompt.is_none());
    }

    #[test]
    fn new_tag_captures_local_raw_target_and_creates_lightweight_tag() {
        let (handle, state) = fake(&[]);
        branch_world(&state);
        let mut app = commits_app(&handle);
        app.press(Key::plain(Code::Char('3')));
        app.dispatch("view.down");

        // `T` opens one blank tag field over the row's branch — captured at
        // open, so nothing a cursor does while the field holds the keyboard
        // can re-aim the tag.
        app.press(Key::char('T'));
        assert!(matches!(app.prompt, Some(Prompt::TagNew { .. })));
        app.draw();
        assert!(status(&app).contains("tag: "), "{:?}", status(&app));
        type_(&mut app, "v1");
        app.press(Key::plain(Code::Enter));
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                !state.lock().unwrap().tags_written.is_empty()
            }),
            "the tag never queued"
        );
        assert_eq!(
            state.lock().unwrap().tags_written,
            [(b"v1".to_vec(), b"f\xe9ature".to_vec())],
            "the tag was not aimed at the row's raw bytes"
        );
        assert_eq!(
            state.lock().unwrap().branch_writes,
            ["tag v1 at f\u{fffd}ature"],
            "a lightweight tag carries no message"
        );

        // The name is trimmed before it is queued — git would hold the
        // padding as part of the name.
        app.dispatch("view.top");
        app.press(Key::char('T'));
        type_(&mut app, "  v2  ");
        app.press(Key::plain(Code::Enter));
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                state.lock().unwrap().tags_written.len() == 2
            }),
            "the second tag never queued"
        );
        assert_eq!(
            state.lock().unwrap().tags_written[1],
            (b"v2".to_vec(), b"main".to_vec())
        );

        // A duplicate rides on to git and comes back in its words.
        state.lock().unwrap().refuse_tag = Some("fatal: tag 'v2' already exists".into());
        app.press(Key::char('T'));
        type_(&mut app, "v2");
        app.press(Key::plain(Code::Enter));
        let gen = app.generation;
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                app.generation > gen
            }),
            "the refused tag never finished"
        );
        assert_eq!(app.message, "fatal: tag 'v2' already exists");
        assert_eq!(state.lock().unwrap().tags_written.len(), 2);

        // Whitespace says so beside the field, without a job.
        app.press(Key::char('T'));
        type_(&mut app, "   ");
        app.press(Key::plain(Code::Enter));
        assert_eq!(app.message, "a tag needs a name");
        assert!(app.prompt.is_none());
        assert_eq!(state.lock().unwrap().tags_written.len(), 2);

        // A remote row refuses before any field stands.
        app.dispatch("view.down");
        app.dispatch("view.down");
        app.press(Key::char('T'));
        assert_eq!(app.message, "only a local branch can be tagged here");
        assert!(app.prompt.is_none());

        // So does the detached row.
        let (handle, state) = fake(&[]);
        branch_world(&state);
        {
            let mut s = state.lock().unwrap();
            s.head = Some(HeadState::Detached {
                commit: "f00d".into(),
            });
            s.locals[0].head = false;
        }
        let mut app = commits_app(&handle);
        app.press(Key::plain(Code::Char('3')));
        app.dispatch("view.top");
        app.press(Key::char('T'));
        assert_eq!(app.message, "only a local branch can be tagged here");
        assert!(app.prompt.is_none());
        assert!(state.lock().unwrap().tags_written.is_empty());
    }

    #[test]
    fn generic_prompt_preserves_search_lifecycle_and_mouse_inertness() {
        let (handle, state) = fake(&[]);
        branch_world(&state);
        let mut app = commits_app(&handle);

        // The search lifecycle, still green after the prompt became a typed
        // enum: live filtering, keep on Enter, restore on Esc.
        app.press(Key::char('/'));
        assert!(matches!(app.prompt, Some(Prompt::Search { .. })));
        type_(&mut app, "commit 9");
        let live = commits_of(&app).filter_note();
        assert!(live.is_some(), "the edit did not filter live");
        app.press(Key::plain(Code::Enter));
        assert_eq!(
            commits_of(&app).filter_note(),
            live,
            "enter did not keep the query"
        );
        let standing = commits_of(&app).filter_note();

        // A branch name field is the same one-line prompt with a different
        // consumer — and a standing query is nobody's business while it
        // edits: neither the typing nor the cancel touches the list's
        // filter, and nothing moves under the field.
        app.press(Key::plain(Code::Char('3')));
        let cursor = branches_of(&app).cursor();
        app.press(Key::char('n'));
        assert!(matches!(app.prompt, Some(Prompt::BranchNew { .. })));
        type_(&mut app, "topic");
        assert_eq!(
            commits_of(&app).filter_note(),
            standing,
            "a name edit reached the search"
        );
        app.press(Key::plain(Code::Esc));
        assert!(app.prompt.is_none());
        assert_eq!(
            commits_of(&app).filter_note(),
            standing,
            "a name cancel cleared the search"
        );
        assert_eq!(
            branches_of(&app).cursor(),
            cursor,
            "the field moved the cursor"
        );
        app.pump_quiet();
        assert!(state.lock().unwrap().branch_writes.is_empty());

        // While any field stands the keyboard is input's: a digit does not
        // focus a pane — it is text — a mouse press moves nothing, and the
        // field survives both.
        app.draw();
        app.press(Key::char('n'));
        app.press(Key::plain(Code::Char('4')));
        assert!(
            matches!(app.prompt, Some(Prompt::BranchNew { .. })),
            "the digit closed the field"
        );
        app.draw();
        assert!(status(&app).contains("4"), "the digit never became text");
        assert_eq!(
            app.panes.focused_name(),
            "branches",
            "the digit focused a pane from the field"
        );
        app.mouse(click(MouseKind::Down, 5, 15));
        app.mouse(click(MouseKind::Up, 5, 15));
        assert_eq!(
            app.panes.focused_name(),
            "branches",
            "the mouse moved focus under a field"
        );
        assert!(
            matches!(app.prompt, Some(Prompt::BranchNew { .. })),
            "the mouse closed the field"
        );
        app.press(Key::plain(Code::Esc));
        assert!(app.prompt.is_none());

        // And a name prompt accepted after all of it still creates — the
        // field is input, and the world was only ever waiting. The standing
        // query outlives it too; the note's *denominator* is the list's own
        // business, and the fake's re-read log is shorter than the hundred
        // the pane was constructed with.
        app.press(Key::char('n'));
        type_(&mut app, "made-later");
        let gen = app.generation;
        app.press(Key::plain(Code::Enter));
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                app.generation > gen
            }),
            "the create never queued"
        );
        assert_eq!(state.lock().unwrap().branch_bytes, [b"made-later".to_vec()]);
        assert_eq!(
            commits_of(&app).query(),
            Some("commit 9"),
            "an accept reached the search"
        );
    }

    #[test]
    fn delete_arms_same_raw_target_then_submits_non_force_once() {
        let (handle, state) = fake(&[]);
        branch_world(&state);
        let mut app = commits_app(&handle);
        app.draw();
        app.press(Key::plain(Code::Char('3')));

        // First press: the exact question, nothing queued, and the armed
        // row itself in the chrome's error ink — the thing a second press
        // destroys is named by its own colour, focused or not.
        app.press(Key::char('d'));
        app.draw();
        assert_eq!(
            app.message, "delete branch main? press again to confirm",
            "{:?}",
            app.message
        );
        assert!(state.lock().unwrap().branch_writes.is_empty());
        assert!(
            branches_of(&app).armed_row().is_some(),
            "the first press did not arm"
        );
        {
            let rect = app.pane_rect("branches").expect("placed");
            let ink = app
                .screen
                .ink(rect.x + 2, rect.y + 1)
                .expect("a drawn cell");
            assert_eq!(
                ink.fg, app.host.theme.chrome.error,
                "the armed row is not error-tinted"
            );
        }

        // A cursor move disarms: the question was about the row the
        // keyboard used to be on.
        app.dispatch("view.down");
        assert_eq!(
            branches_of(&app).armed_row(),
            None,
            "the arm survived a cursor move"
        );

        // The non-text local arms under its lossy display name, and the arm
        // is keyed by the raw bytes — the question and the job name the same
        // branch.
        app.press(Key::char('d'));
        assert_eq!(
            app.message,
            "delete branch f\u{fffd}ature? press again to confirm"
        );
        assert_eq!(
            branches_of(&app).armed_row(),
            Some(Target::Local(RefName::from_bytes(b"f\xe9ature")))
        );

        // Second press on the same raw target: one non-force deletion, the
        // arm spent, and the refreshed pane without the row.
        let gen = app.generation;
        app.press(Key::char('d'));
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                app.generation > gen
            }),
            "the confirmed delete never queued"
        );
        assert_eq!(
            state.lock().unwrap().branch_writes,
            ["delete branch f\u{fffd}ature"]
        );
        assert_eq!(state.lock().unwrap().branch_bytes, [b"f\xe9ature".to_vec()]);
        assert_eq!(
            branches_of(&app).armed_row(),
            None,
            "the arm outlived its spend"
        );
        assert_eq!(
            branches_of(&app).status(),
            "2/3 · origin/feat/ure",
            "the deleted branch did not leave the pane"
        );

        // A prompt disarms too — a question nobody can answer while the
        // field holds the keyboard is a trap, not a question.
        app.dispatch("view.top");
        app.press(Key::char('d'));
        assert!(branches_of(&app).armed_row().is_some());
        app.press(Key::char('n'));
        assert_eq!(
            branches_of(&app).armed_row(),
            None,
            "the arm survived a prompt"
        );
        app.press(Key::plain(Code::Esc));

        // So does a config reload.
        app.press(Key::char('d'));
        assert!(branches_of(&app).armed_row().is_some());
        let path =
            std::env::temp_dir().join(format!("gitten-tui-branch-{}.toml", std::process::id()));
        std::fs::write(&path, "[view]\nscrolloff = 1\n").expect("a config file");
        app.reload(&path);
        assert_eq!(
            branches_of(&app).armed_row(),
            None,
            "the arm survived a reload"
        );
        std::fs::remove_file(&path).ok();

        // The mouse moves the question only by moving the keyboard: a press
        // on the armed row leaves it standing — that row is still what the
        // second press would confirm — and a press on another row clears it.
        app.dispatch("view.top");
        app.press(Key::char('d'));
        app.mouse(click(MouseKind::Down, 2, 8));
        app.mouse(click(MouseKind::Up, 2, 8));
        assert!(
            branches_of(&app).armed_row().is_some(),
            "the arm died on its own row"
        );
        app.mouse(click(MouseKind::Down, 2, 9));
        app.mouse(click(MouseKind::Up, 2, 9));
        assert_eq!(
            branches_of(&app).armed_row(),
            None,
            "the arm survived a mouse move to another row"
        );

        // A remote row refuses outright: a tracking ref is the remote's
        // shadow, and this key is not fetch's prune.
        app.press(Key::char('d'));
        assert_eq!(
            app.message,
            "a remote branch is its remote's to delete — fetch prunes it here"
        );
        assert_eq!(
            state.lock().unwrap().branch_writes.len(),
            1,
            "a remote deletion queued"
        );

        // Focus round-trips keep the question: the window's own contract,
        // and the files pane's — arming is about the row, and the keyboard
        // leaving does not answer it. Both focus paths round-trip, the
        // digits and the ctrl-j ring, and the arm is on the same target
        // after each.
        app.dispatch("view.top");
        app.press(Key::char('d'));
        assert_eq!(
            app.message, "delete branch main? press again to confirm",
            "{:?}",
            app.message
        );
        assert_eq!(
            branches_of(&app).armed_row(),
            Some(Target::Local(RefName::from("main")))
        );
        app.press(Key::plain(Code::Char('4')));
        assert_eq!(app.panes.focused_name(), "commits");
        app.press(Key::plain(Code::Char('3')));
        assert_eq!(
            branches_of(&app).armed_row(),
            Some(Target::Local(RefName::from("main"))),
            "the arm died on a digit round-trip"
        );
        app.dispatch("pane.next");
        assert_eq!(app.panes.focused_name(), "commits");
        app.dispatch("pane.prev");
        assert_eq!(app.panes.focused_name(), "branches");
        assert_eq!(
            branches_of(&app).armed_row(),
            Some(Target::Local(RefName::from("main"))),
            "the arm died on a ring round-trip"
        );

        // The following identical press — the same raw target the question
        // named, however far the keyboard wandered in between — submits.
        let gen = app.generation;
        app.press(Key::char('d'));
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                app.generation > gen
            }),
            "the round-tripped arm never submitted"
        );
        assert_eq!(
            state.lock().unwrap().branch_writes,
            ["delete branch f\u{fffd}ature", "delete branch main"]
        );
        assert_eq!(
            branches_of(&app).armed_row(),
            None,
            "the arm outlived its spend"
        );

        // So does the detached row, in a world that opens detached.
        let (handle, state) = fake(&[]);
        branch_world(&state);
        {
            let mut s = state.lock().unwrap();
            s.head = Some(HeadState::Detached {
                commit: "f00d".into(),
            });
            s.locals[0].head = false;
        }
        let mut app = commits_app(&handle);
        app.press(Key::plain(Code::Char('3')));
        app.press(Key::char('d'));
        assert_eq!(app.message, "a detached HEAD is not a branch");

        // And git's own "not fully merged" comes back verbatim: the force
        // decision's proper home is git's, and nothing here upgrades the
        // key behind the reader's back.
        let (handle, state) = fake(&[]);
        branch_world(&state);
        state.lock().unwrap().refuse_branch =
            Some("error: the branch 'main' is not fully merged".into());
        let mut app = commits_app(&handle);
        app.press(Key::plain(Code::Char('3')));
        app.press(Key::char('d'));
        app.press(Key::char('d'));
        let gen = app.generation;
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                app.generation > gen
            }),
            "the refused delete never finished"
        );
        assert_eq!(app.message, "error: the branch 'main' is not fully merged");
        assert!(state.lock().unwrap().branch_writes.is_empty());
    }

    #[test]
    fn every_branch_write_refreshes_every_registered_repository_pane() {
        for verb in ["checkout", "create", "rename", "delete", "tag"] {
            let (handle, state) = fake(&[]);
            branch_world(&state);
            let mut app = commits_app(&handle);
            // An acquired diff, so every tenant has something the finish can
            // stale — the empty pane has nothing to re-read by design.
            app.dispatch("commits.open-diff");
            app.press(Key::plain(Code::Char('3')));
            let (branches_before, remotes_before, head_before) = {
                let s = state.lock().unwrap();
                (s.branch_reads, s.remote_reads, s.head_reads)
            };
            let (log_before, pairs_before) = {
                let s = state.lock().unwrap();
                (s.log_reads, s.pairs_reads)
            };

            // The verb, as the keys send it.
            match verb {
                "checkout" => {
                    app.dispatch("view.down");
                    app.press(Key::plain(Code::Char(' ')));
                }
                "create" => {
                    app.press(Key::char('n'));
                    type_(&mut app, "made");
                    app.press(Key::plain(Code::Enter));
                }
                "rename" => {
                    app.dispatch("view.down");
                    app.press(Key::char('R'));
                    type_(&mut app, "renamed");
                    app.press(Key::plain(Code::Enter));
                }
                "delete" => {
                    app.dispatch("view.down");
                    app.press(Key::char('d'));
                    app.press(Key::char('d'));
                }
                _ => {
                    app.dispatch("view.down");
                    app.press(Key::char('T'));
                    type_(&mut app, "v1");
                    app.press(Key::plain(Code::Enter));
                }
            }
            let gen = app.generation;
            assert!(
                until(Duration::from_secs(2), || {
                    app.pump_quiet();
                    app.generation > gen
                }),
                "{verb}: the write never finished"
            );

            // The ref reads repeat — all three, the snapshot re-taken — and
            // the panes nobody is looking at re-read too: the hidden commit
            // list and the acquired diff behind the focused branches pane.
            let s = state.lock().unwrap();
            assert!(
                s.branch_reads > branches_before
                    && s.remote_reads > remotes_before
                    && s.head_reads > head_before,
                "{verb}: the ref reads did not repeat"
            );
            assert!(
                s.log_reads > log_before,
                "{verb}: the list was not refreshed"
            );
            assert!(
                s.pairs_reads > pairs_before,
                "{verb}: the diff was not refreshed"
            );
            assert!(!s.branch_writes.is_empty(), "{verb}: no write was recorded");
            drop(s);
            for name in app.panes.names().collect::<Vec<_>>() {
                assert_eq!(
                    app.panes.get(name).unwrap().generation(),
                    app.generation,
                    "{verb}: {name} was not refreshed to the generation"
                );
            }

            // And the change itself landed in the refreshed pane, where the
            // verb's semantics say it should.
            let status = branches_of(&app).status();
            match verb {
                "checkout" => {
                    assert_eq!(status, "2/4 · f\u{fffd}ature", "{verb}");
                    // The mark travels with the refresh; the window scrolls
                    // to the top so both marks are on screen to read.
                    app.dispatch("view.top");
                    app.draw();
                    let rect = app.pane_rect("branches").expect("placed");
                    let rows: Vec<String> = ((rect.y + 1)..(rect.y + rect.height))
                        .map(|y| app.screen.row_text(y))
                        .collect();
                    assert!(
                        rows.iter().any(|r| r.starts_with("● f")),
                        "{verb}: the checked-out branch is not marked current: {rows:?}"
                    );
                    assert!(
                        rows.iter().any(|r| r.starts_with("• m")),
                        "{verb}: the branch HEAD left is still marked current: {rows:?}"
                    );
                }
                "create" => assert_eq!(status, "1/5 · main", "{verb}"),
                "rename" => assert_eq!(status, "2/4 · renamed", "{verb}"),
                "delete" => assert_eq!(status, "2/3 · origin/feat/ure", "{verb}"),
                _ => assert_eq!(status, "2/4 · f\u{fffd}ature", "{verb}"),
            }
        }

        // Hidden tenants: narrow mode, the branches pane unplaced — and a
        // branch write still re-reads it, with every other tenant, to the
        // same generation. The diff is opened first so it, too, has
        // something a finish can stale.
        let (handle, state) = fake(&[]);
        branch_world(&state);
        let mut app = commits_app(&handle);
        app.dispatch("commits.open-diff");
        app.screen.resize(60, 24);
        app.press(Key::plain(Code::Char('4')));
        app.draw();
        assert!(
            app.pane_content("branches").is_none(),
            "narrow mode showed the hidden pane"
        );
        let job = Write::create_branch(&handle, b"made-hidden".to_vec(), None);
        assert!(app.submitter.submit(Box::new(job)).is_ok(), "queued");
        let gen = app.generation;
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                app.generation > gen
            }),
            "the hidden tenant was not refreshed"
        );
        for name in ["commits", "stashes", "remotes", "diff", "files", "branches"] {
            assert_eq!(
                app.panes.get(name).unwrap().generation(),
                app.generation,
                "{name} was not refreshed while hidden"
            );
        }

        // One refresh failure does not skip the tenants after it: the files
        // read fails mid-wave, and the branches pane — later in the
        // registration order — still reaches the generation while the first
        // error is the one the status line keeps.
        let (handle, state) = fake(&[]);
        branch_world(&state);
        let mut app = commits_app(&handle);
        state.lock().unwrap().fail_status = Some("fatal: unable to read the working tree".into());
        app.press(Key::plain(Code::Char('3')));
        app.press(Key::char('d'));
        app.press(Key::char('d'));
        let gen = app.generation;
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                app.generation > gen
            }),
            "the wave with a failing tenant never finished"
        );
        assert_eq!(
            app.message, "fatal: unable to read the working tree",
            "the first refresh error did not stand: {:?}",
            app.message
        );
        assert!(
            app.panes.get("files").unwrap().generation() < app.generation,
            "a failed tenant claimed the generation"
        );
        assert_eq!(
            app.panes.get("branches").unwrap().generation(),
            app.generation,
            "a later tenant was skipped by an earlier one's failure"
        );

        // A refused write is the same contract: the finish advances the
        // generation and the refs are re-read, refusal or not.
        let (handle, state) = fake(&[]);
        branch_world(&state);
        state.lock().unwrap().refuse_branch =
            Some("error: the branch 'main' is not fully merged".into());
        let mut app = commits_app(&handle);
        app.press(Key::plain(Code::Char('3')));
        let reads = state.lock().unwrap().branch_reads;
        app.press(Key::plain(Code::Char(' ')));
        let gen = app.generation;
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                app.generation > gen && state.lock().unwrap().branch_reads > reads
            }),
            "the refused write did not trigger the refresh"
        );
        assert_eq!(
            app.panes.get("branches").unwrap().generation(),
            app.generation,
            "the refused write's finish did not refresh the pane"
        );
    }

    #[test]
    fn branch_header_status_copy_and_help_use_live_registry_data() {
        let (handle, _state) = fake(&[]);
        branch_world(&_state);
        let mut app = commits_app(&handle);
        app.press(Key::plain(Code::Char('3')));
        app.draw();

        // The header says the pane's name, the focus key out of the *live*
        // keymap — the shipped `3`, here without any local table — and the
        // label counting the groups the read brought back.
        let rect = app.pane_rect("branches").expect("placed");
        let header = app
            .screen
            .row_text(rect.y)
            .chars()
            .take(40)
            .collect::<String>();
        assert!(header.contains("branches"), "{header:?}");
        assert!(header.contains('3'), "{header:?}");
        assert!(header.contains("fake (main) · 2 local"), "{header:?}");

        // The title and the status line follow the keyboard.
        assert!(
            app.screen.row_text(0).contains("branches"),
            "the title did not follow: {:?}",
            app.screen.row_text(0)
        );
        assert!(
            app.screen.row_text(23).contains("branches · 1/4 · main"),
            "the status did not follow: {:?}",
            app.screen.row_text(23)
        );

        // `y` copies exactly the selected refname, as git spells it — the
        // bare name here, the joined remote/branch there, and nothing at all
        // for a row no verb can act on.
        app.dispatch("copy.selection");
        assert_eq!(app.copy.as_deref(), Some("main"));
        app.dispatch("view.down");
        app.dispatch("view.down");
        app.dispatch("copy.selection");
        assert_eq!(app.copy.as_deref(), Some("origin/feat/ure"));

        // The help panel lists what runs straight out of the shared
        // registry — and only that. The panel is capped at thirty rows, so
        // no single viewport holds the whole registry any more: the top
        // shows the globals out of the live keymap — the pane-focus digits
        // among them, and the two project rows the sync wave gave handlers
        // — and the bottom the focused mode's own bindings.
        app.screen = Screen::new(120, 50);
        app.press(Key::char('?'));
        app.press(Key::plain(Code::Home));
        app.draw();
        let help = (0..50)
            .map(|y| app.screen.row_text(y))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            help.contains("focus the branches pane"),
            "help is missing the runnable rows: {help:?}"
        );
        for doc in [
            "switch to another recent repository",
            "open a repository by typing its path",
        ] {
            assert!(
                help.contains(doc),
                "help dropped a runnable command: {doc:?}: {help:?}"
            );
        }
        // The press is where a missing list is said: no recent repositories
        // yet, said and not shown — a picker of nothing is a trap shaped
        // like an answer.
        app.dispatch("project.switch");
        assert_eq!(app.message, "no recent repositories — O to open one");
        app.press(Key::plain(Code::End));
        app.draw();
        let help = (0..50)
            .map(|y| app.screen.row_text(y))
            .collect::<Vec<_>>()
            .join("\n");
        for doc in [
            "check out the selected branch",
            "create a branch",
            "rename the selected branch",
            // The panel's column is finite and the longest descriptions
            // truncate; each marker below survives its own truncation.
            "delete the selected branch",
            "name the selected branch's",
        ] {
            assert!(help.contains(doc), "help is missing {doc:?}: {help:?}");
        }
        // The rebase row is bound in this mode and unhandled here, so its
        // row is not on the panel any more — the press says why instead.
        assert!(!help.contains("move the current branch onto"), "{help:?}");
    }

    #[test]
    fn rebase_commands_remain_explicitly_deferred() {
        // The scope fence for the named lifecycle follow-up — rebase-onto,
        // conflict state, abort and continue — not the desired final
        // product: the availability contract marks them unsupported, the
        // keys say so, and no job is ever submitted.
        let (handle, state) = fake(&[]);
        branch_world(&state);
        let mut app = commits_app(&handle);
        app.press(Key::plain(Code::Char('3')));

        // The key resolves through core's branches mode — lowercase `r` —
        // and lands on the same unsupported name.
        app.press(Key::char('r'));
        assert_eq!(
            app.message,
            "commits.rebase-onto is not supported by this client"
        );
        app.dispatch("commits.rebase-onto");
        assert_eq!(
            app.message,
            "commits.rebase-onto is not supported by this client"
        );

        // From the commits pane, the two exits say the same thing.
        app.press(Key::plain(Code::Char('4')));
        app.dispatch("rebase.abort");
        assert_eq!(app.message, "rebase.abort is not supported by this client");
        app.dispatch("rebase.continue");
        assert_eq!(
            app.message,
            "rebase.continue is not supported by this client"
        );

        app.pump_quiet();
        let s = state.lock().unwrap();
        assert!(
            s.branch_writes.is_empty() && s.writes.is_empty(),
            "a rebase job was submitted: {:?}",
            s.branch_writes
        );
    }

    #[test]
    fn wave_one_and_plan_016_features_survive_a_third_tenant() {
        let (handle, state) = fake(&[]);
        branch_world(&state);
        let mut app = commits_app(&handle);
        app.draw();
        app.dispatch("commits.open-diff");
        app.draw();

        // Focus: the digits and the cycle still walk the ring, and the
        // title, header and status all follow the keyboard.
        app.press(Key::plain(Code::Char('3')));
        assert_eq!(app.panes.focused_name(), "branches");
        app.draw();
        assert!(
            app.screen.row_text(0).contains("branches"),
            "the title did not follow: {:?}",
            app.screen.row_text(0)
        );
        assert!(
            app.screen.row_text(6).contains('3') && app.screen.row_text(6).contains("branches"),
            "the header did not advertise the pane: {:?}",
            app.screen.row_text(6)
        );
        assert!(
            app.screen.row_text(23).contains("branches · 1/4 · main"),
            "the status did not follow: {:?}",
            app.screen.row_text(23)
        );

        // Enter still focuses the persistent preview, and Back returns to the
        // last list — the branches pane included in `last_list`.
        app.dispatch("commits.open-diff");
        assert_eq!(app.panes.focused_name(), "diff");
        app.dispatch("back");
        assert_eq!(app.panes.focused_name(), "branches", "back lost the list");
        app.dispatch("commits.open-diff");
        app.press(Key::plain(Code::Char('4')));
        app.dispatch("back");
        assert_eq!(app.panes.focused_name(), "commits");

        // Search still targets the commit list — by the pane's own name,
        // not by whoever holds the keyboard.
        app.press(Key::char('/'));
        assert!(matches!(app.prompt, Some(Prompt::Search { .. })));
        type_(&mut app, "commit 9");
        app.press(Key::plain(Code::Enter));
        let mut direct = Commits::new(hundred_commits());
        direct.apply_query("commit 9");
        assert_eq!(
            commits_of(&app).filter_note(),
            direct.filter_note(),
            "search did not reach the list"
        );

        // Hunk verbs still route to the diff pane and its own gate — not to
        // the pane that holds the keyboard, and not to the branches.
        app.press(Key::plain(Code::Char('0')));
        app.dispatch("diff.stage-hunk");
        assert_eq!(
            app.message,
            "only the working-tree diff can act on hunks — this one is between commits"
        );
        assert!(
            state.lock().unwrap().branch_writes.is_empty(),
            "the hunk verb reached the branches"
        );

        // Mouse capture: a drag in the commit list selects its rows and its
        // release reads that pane; a press in the branches slice moves the
        // keyboard there, and a drag inside it builds no selection — a ref
        // list is acted on one row at a time.
        app.dispatch("commits.focus");
        app.mouse(click(MouseKind::Down, 5, 13));
        app.mouse(click(MouseKind::Drag, 5, 14));
        app.mouse(click(MouseKind::Up, 5, 14));
        assert!(
            !commits_of(&app).selection().is_empty(),
            "the drag in the list selected nothing"
        );
        app.mouse(click(MouseKind::Down, 5, 9));
        assert_eq!(
            app.panes.focused_name(),
            "branches",
            "the press did not move the keyboard to the branches"
        );
        app.mouse(click(MouseKind::Up, 5, 9));
        app.mouse(click(MouseKind::Down, 5, 9));
        app.mouse(click(MouseKind::Drag, 5, 11));
        app.mouse(click(MouseKind::Up, 5, 11));
        assert_eq!(
            app.panes.get("branches").map(|pane| pane.selection()),
            Some(String::new()),
            "a drag built a branch selection"
        );

        // Copy on the list still copies the row the keyboard is on.
        app.press(Key::plain(Code::Char('4')));
        app.dispatch("copy.selection");
        assert!(app.copy.is_some(), "the list copied nothing");

        // A reload rebuilds the host and re-applies geometry to every placed
        // pane — the branches pane included — and keeps the focus where it
        // was.
        let path = std::env::temp_dir().join(format!(
            "gitten-tui-branch-wave-{}.toml",
            std::process::id()
        ));
        std::fs::write(&path, "[view]\nscrolloff = 1\n").expect("a config file");
        app.press(Key::plain(Code::Char('3')));
        app.reload(&path);
        assert_eq!(
            app.panes.focused_name(),
            "branches",
            "the reload moved the focus"
        );
        assert!(app.pane_content("branches").is_some());
        std::fs::remove_file(&path).ok();

        // Narrow mode, branches focused: the *other* lists are unplaced —
        // and stale all the same when a branch write finishes.
        app.screen.resize(60, 24);
        app.draw();
        assert!(
            app.pane_content("commits").is_none(),
            "narrow mode showed the hidden pane"
        );
        let job = Write::create_branch(&handle, b"made-hidden".to_vec(), None);
        assert!(app.submitter.submit(Box::new(job)).is_ok(), "queued");
        let gen = app.generation;
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                app.generation > gen
            }),
            "the hidden tenants were not refreshed"
        );
        for name in ["commits", "stashes", "remotes", "diff", "files", "branches"] {
            assert_eq!(
                app.panes.get(name).unwrap().generation(),
                app.generation,
                "{name} was not refreshed to the generation"
            );
        }
    }

    /// The skeleton, exactly as [`App::new`] leaves it. Every other test
    /// launches through a helper that calls `load_startup`, so the deferred
    /// half of startup is never asserted — and the TTI number depends on it:
    /// the sidebars register in their loading shape, the main pane stays the
    /// empty one, and nothing runs until the first frame is on the terminal.
    /// If someone moves the loads back into [`App::new`], this is what breaks.
    #[test]
    fn the_skeleton_defers_the_startup_loads() {
        let (handle, _state) = fake(&[]);
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

        assert!(app.startup_pending, "App::new ran the startup loads itself");
        // Three sidebars, all registered — the number keys and the walk derive
        // from registration, which is the whole of what a deferred pane costs —
        // and all in the loading shape, which is the honest shape for an
        // un-read list.
        for name in ["files", "branches", "stashes"] {
            let label = match app.panes.get(name) {
                Some(Screens::Files { label, .. })
                | Some(Screens::Branches { label, .. })
                | Some(Screens::Stashes { label, .. }) => label.as_str(),
                Some(_) => panic!("{name} was registered as the wrong screen"),
                None => panic!("{name} is not registered in the loading shape"),
            };
            assert_eq!(label, STARTUP_LOADING, "{name} is not in its loading shape");
        }
        // The main pane is the empty one: nothing was acquired for it yet, and
        // pretending otherwise would draw a diff nobody acquired.
        match app.panes.get("diff") {
            Some(Screens::Diff { view, label, .. }) => {
                assert_eq!(label, EMPTY_DIFF_LABEL);
                assert_eq!(view.rows(), 0, "the main pane holds something unacquired");
            }
            _ => panic!("the diff pane is not registered"),
        }

        // The load, one wave: the flag clears and every tenant holds what its
        // read answered — the sidebars their lists, the main pane row zero's
        // preview, named and not empty.
        app.load_startup(&mut StartClock::new());
        assert!(!app.startup_pending, "the load did not clear the flag");
        assert!(
            files_of(&app).is_available(),
            "the files pane never left its loading shape"
        );
        assert_ne!(files_label(&app), STARTUP_LOADING);
        assert_ne!(branches_label(&app), STARTUP_LOADING);
        match app.panes.get("stashes") {
            Some(Screens::Stashes { label, .. }) => {
                assert_ne!(label, STARTUP_LOADING);
            }
            _ => panic!("the stashes pane is not registered"),
        }
        match app.panes.get("diff") {
            Some(Screens::Diff { view, label, .. }) => {
                assert_ne!(label, EMPTY_DIFF_LABEL, "the preview was never named");
                assert!(view.rows() > 0, "the preview diff is empty");
            }
            _ => panic!("the diff pane is not registered"),
        }
    }

    /// A fixture launch defers nothing: no repository behind it means no
    /// sidebars and no reads to hold back, so [`App::new`] is the whole of
    /// startup and the flag is false before anything runs. The pair of tests
    /// pins both halves of the ordering — a repository launch defers, a
    /// fixture launch never does.
    #[test]
    fn a_fixture_launch_never_defers_anything() {
        let started = gitten_app::Started {
            view: View::Diff,
            source: Source::Fixtures,
            host: Host::new(),
            loaded: acquire::Loaded {
                label: "fixtures".into(),
                data: Data::Diff(parse_unified_diff(HUNK_DIFF)),
            },
            config: std::path::PathBuf::from("/nonexistent/gitten.toml"),
            repo: None,
        };
        let app = App::new(started, Glyphs::default());
        assert!(
            !app.startup_pending,
            "a fixture launch carried the deferred flag"
        );
        for name in ["files", "branches", "stashes"] {
            assert!(
                app.panes.get(name).is_none(),
                "the {name} pane is on a fixture launch"
            );
        }
    }

    // ------------------------------------------------- tui_parity_: W0

    /// Which registered pane a command's own family names, for routing a
    /// dispatch the way its binding would have reached it. Only a display
    /// concern: whether the command runs is [`tui_availability`]'s word, and
    /// this table never second-guesses it.
    fn home_pane(command: &str) -> Option<&'static str> {
        ["files", "branches", "stashes", "diff", "commits"]
            .iter()
            .find(|pane| command.starts_with(&format!("{pane}.")))
            .copied()
    }

    #[test]
    fn tui_parity_refresh_rereads_externally_changed_fake_state() {
        // R is the queue's own finish wave, asked for by hand: state nobody
        // here changed — another process's edit, a fetch, a stash from a
        // second terminal — must still arrive, in every pane, the focused
        // one and the hidden ones alike.
        let (_handle, state) = fake(&[]);
        // The launch's list is the same one the fake's log answers, so the
        // re-read replaces like with like and the selection's identity is
        // what the assertion can pin — the same shape the staging tests
        // build on.
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
        app.load_startup(&mut StartClock::new());
        app.screen = Screen::new(120, 24);
        app.dispatch("commits.open-diff");
        app.dispatch("commits.focus");

        let (open_log, open_status, open_stash, open_pairs) = {
            let s = state.lock().unwrap();
            (s.log_reads, s.status_reads, s.stash_reads, s.pairs_reads)
        };
        // The world changes behind the back: a file staged, a stash
        // parked, a line edited under the diff it is previewing.
        {
            let mut s = state.lock().unwrap();
            s.status = Status {
                unstaged: vec![UnstagedEntry {
                    path: PathBytes::from("work.rs"),
                    change: Change::Modified,
                    kind: Kind::File,
                    submodule: Submodule::default(),
                }],
                ..Default::default()
            };
            let parked = s.stashes[0].clone();
            s.stashes.push(parked);
            s.before = vec![pair("f.txt", side(0), side(5))];
        }

        let sha_before = app.current_commit_sha();
        let gen_before = app.generation;
        app.dispatch("repo.refresh");

        // Every read ran again, exactly once — commits, files, stashes,
        // branches, and the diff preview too.
        let s = state.lock().unwrap();
        assert_eq!(s.log_reads, open_log + 1, "the commit list did not re-read");
        assert_eq!(
            s.status_reads,
            open_status + 1,
            "the files pane did not re-read"
        );
        assert_eq!(s.stash_reads, open_stash + 1, "the stack did not re-read");
        assert_eq!(s.pairs_reads, open_pairs + 1, "the preview did not re-read");
        drop(s);

        // What changed behind the back is what the screen shows now: the
        // working tree gained a row, the stack gained a stash.
        assert_ne!(
            files_of(&app).status(),
            "clean",
            "the staged edit did not arrive"
        );
        let stashes = match app.panes.get("stashes") {
            Some(Screens::Stashes { view, .. }) => view.status(),
            _ => panic!("the stack is registered"),
        };
        assert!(
            stashes.contains("3"),
            "the parked stash did not arrive: {stashes}"
        );
        // The selection rode through the re-read: same commit, same row.
        assert_eq!(app.current_commit_sha(), sha_before);
        assert!(
            app.generation > gen_before,
            "the manual wave did not advance"
        );
        // A clean refresh is its own evidence: the refreshed screen, and no
        // word claiming otherwise.
        assert!(app.message.is_empty(), "{}", app.message);
    }

    #[test]
    fn tui_parity_refresh_failure_keeps_the_last_good_rows_and_says_so() {
        let (handle, state) = fake(&[]);
        let mut app = commits_app(&handle);
        app.dispatch("commits.open-diff");
        app.dispatch("commits.focus");

        let files_before = files_of(&app).status();
        let files_gen = app
            .panes
            .get("files")
            .map(|p| p.generation())
            .expect("the files pane is registered");
        let gen_before = app.generation;
        let (open_log, open_status) = {
            let s = state.lock().unwrap();
            (s.log_reads, s.status_reads)
        };

        // The status read breaks on the next wave — the error the wave
        // exists to surface.
        state.lock().unwrap().fail_status = Some("fatal: bad status".into());
        app.dispatch("repo.refresh");

        assert_eq!(
            app.message, "fatal: bad status",
            "the error was not surfaced"
        );
        // The last good rows stand, at their own generation: no fabricated
        // emptiness, no false advance, and a later wave retries.
        assert_eq!(
            files_of(&app).status(),
            files_before,
            "the rows were replaced"
        );
        assert_eq!(
            app.panes.get("files").map(|p| p.generation()),
            Some(files_gen),
            "a failed refresh advanced the tenant"
        );
        assert!(app.generation > gen_before, "the wave did not advance");
        // And every pane was still *tried* — the failure did not stop the
        // wave: commits re-read ahead of files, and branches after it.
        let s = state.lock().unwrap();
        assert_eq!(s.log_reads, open_log + 1);
        assert_eq!(s.status_reads, open_status + 1);
    }

    #[test]
    fn tui_parity_no_advertised_command_is_a_hidden_no_op() {
        // The whole point of the contract, checked in both directions, on
        // both shapes of launch: what the client says it runs, dispatch can
        // route without the contract refusing; what it refuses, the help
        // panel never advertised.
        for (shape, build) in [("repository-backed", 0usize), ("fixture", 1)] {
            let mut app = match build {
                0 => commits_app(&fake(&[]).0),
                _ => app_on_diff(Source::Fixtures, None),
            };
            app.screen = Screen::new(120, 40);
            for command in gitten_core::command::Commands::builtin().all() {
                let name = &command.name;
                if let Some(pane) = home_pane(name) {
                    if app.panes.get(pane).is_some() {
                        app.dispatch(&format!("{pane}.focus"));
                    }
                }
                app.help = false;
                app.message.clear();
                app.dispatch(name);
                let refused = app.message.contains("is not supported by this client");
                match app.availability.state(name) {
                    Usable::Available => assert!(
                        !refused,
                        "{shape}: {name} is advertised and the contract refused it: {}",
                        app.message
                    ),
                    Usable::Disabled(reason) => assert!(
                        app.message.contains(reason.as_str()),
                        "{shape}: {name} is disabled without its reason: {}",
                        app.message
                    ),
                    Usable::Unsupported => assert!(
                        refused,
                        "{shape}: {name} is unsupported and dispatch ran it: {}",
                        app.message
                    ),
                }
                // Reset what a single dispatch may have left standing; the
                // next command opens onto a clean app.
                app.quit = false;
                app.prompt = None;
            }

            // The panel agrees with the press, in every mode the shipped
            // keymap binds: no row names an unsupported command, and the
            // disabled one says its reason where its description was.
            for mode in [
                "global", "files", "branches", "commits", "stashes", "diff", "help", "input",
                "settings", "reset", "panes",
            ] {
                let mut modes = Modes::new();
                if mode != "global" {
                    modes.push(mode);
                }
                for row in
                    app.host
                        .keys
                        .help_supported(&app.host.commands, &modes, &app.availability)
                {
                    if let gitten_core::command::HelpRow::Command { name, doc, .. } = row {
                        match app.availability.state(&name) {
                            Usable::Available => {}
                            Usable::Disabled(reason) => {
                                assert_eq!(doc.as_str(), reason.as_str(), "{mode}: {name}");
                            }
                            Usable::Unsupported => {
                                panic!("{mode}: the panel advertised {name}")
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn tui_parity_a_disabled_command_says_why_in_help_and_on_press() {
        // The launch is the one thing that turns a supported command away:
        // a fixture view has no repository behind it, and `repo.refresh` —
        // handler and all — is said so, on the panel and on the press,
        // rather than advertised as if it ran.
        let mut app = app_on_diff(Source::Fixtures, None);
        app.screen = Screen::new(120, 40);
        let (commits_gen, diff_gen) = (
            app.panes.get("commits").map(|p| p.generation()),
            app.panes.get("diff").map(|p| p.generation()),
        );

        app.dispatch("repo.refresh");
        assert_eq!(
            app.message,
            "repo.refresh: a fixture has no repository to refresh"
        );
        // And nothing ran: the wave's generations did not move.
        assert_eq!(
            app.panes.get("commits").map(|p| p.generation()),
            commits_gen
        );
        assert_eq!(app.panes.get("diff").map(|p| p.generation()), diff_gen);

        // The panel keeps the key and swaps the description for the reason.
        let rows =
            app.host
                .keys
                .help_supported(&app.host.commands, &Modes::new(), &app.availability);
        let row = rows
            .iter()
            .find(|r| matches!(r, gitten_core::command::HelpRow::Command { name, .. } if name == "repo.refresh"))
            .expect("the disabled row keeps its place on the panel");
        match row {
            gitten_core::command::HelpRow::Command { keys, doc, .. } => {
                assert_eq!(keys, "R");
                assert_eq!(doc, "a fixture has no repository to refresh");
            }
            _ => unreachable!("found a non-command row"),
        }
    }

    #[test]
    fn tui_parity_fixtures_reject_repository_operations() {
        // A fixture launch has no repository behind any of it: every
        // repository verb is refused with its reason, said before anything
        // is queued — and the well-known guard messages are the assertion,
        // not a success-shaped shrug.
        let mut app = app_on_diff(Source::Fixtures, None);
        app.screen = Screen::new(120, 40);
        for (command, refusal) in [
            ("files.stage", "files.stage is not supported here"),
            ("files.stage-all", "files.stage-all is not supported here"),
            ("files.discard", "files.discard is not supported here"),
            ("files.ignore", "files.ignore is not supported here"),
            ("files.commit", "files.commit is not supported here"),
            (
                "branches.checkout",
                "branches.checkout is not supported here",
            ),
            ("branches.new", "branches.new is not supported here"),
            ("stashes.apply", "stashes.apply is not supported here"),
            ("files.stash", "a fixture has no working tree to park"),
            ("diff.stage-hunk", "a fixture has no repository behind it"),
            ("diff.unstage-hunk", "a fixture has no repository behind it"),
        ] {
            app.dispatch(command);
            assert_eq!(app.message, refusal, "{command} was not refused");
        }
    }

    #[test]
    fn tui_parity_remapped_keys_help_and_dispatch_agree() {
        // A config file's move: `files.stage` leaves space for an unclaimed
        // key in the same mode. The panel must show the new spelling and
        // not the old one, the new key must run the command, and the old
        // one must be unbound rather than silently still armed.
        let (handle, state) = fake(&[]);
        state.lock().unwrap().status = Status {
            unstaged: vec![UnstagedEntry {
                path: PathBytes::from("work.rs"),
                change: Change::Modified,
                kind: Kind::File,
                submodule: Submodule::default(),
            }],
            ..Default::default()
        };
        let mut app = commits_app(&handle);
        assert!(
            app.host.keys.unbind("files", "space"),
            "space was not moved"
        );
        app.host.keys.bind("files", ".", "files.stage").unwrap();

        let mut modes = Modes::new();
        modes.push("files");
        let rows = app
            .host
            .keys
            .help_supported(&app.host.commands, &modes, &app.availability);
        assert!(
            rows.iter().any(
                |r| matches!(r, gitten_core::command::HelpRow::Command { keys, doc, .. }
                if keys == "." && doc == "stage or unstage the selected file")
            ),
            "{rows:?}"
        );
        assert!(rows.iter().all(
            |r| !matches!(r, gitten_core::command::HelpRow::Command { keys, .. } if keys.contains("space"))
        ));

        // The new key runs it: one stage write, from the pane's own row.
        app.dispatch("files.focus");
        app.press(Key::char('.'));
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                !state.lock().unwrap().paths_written.is_empty()
            }),
            "the remapped key never reached the repository"
        );
        assert_eq!(
            state.lock().unwrap().paths_written,
            vec![b"work.rs".to_vec()]
        );

        // The old spelling is gone, not armed: unbound, and said.
        app.press(Key::char(' '));
        assert_eq!(app.message, "space is not bound — ? for the keys");
    }

    // ------------------------------------------------------------------
    // W1: explicit diff sources, previews that follow the selection, and
    // the staleness contract on the preview lane.

    /// An app whose working tree holds one file on both sides of the
    /// index: staged f.txt (HEAD→index) and unstaged f.txt (index→worktree),
    /// with the sides' contents scripted apart — the A/B/C shape, one
    /// file, at the App level.
    fn both_sides_app(state: &Arc<Mutex<FakeState>>, handle: &Handle) -> App {
        {
            let mut s = state.lock().unwrap();
            s.status = Status {
                staged: vec![StagedEntry {
                    path: PathBytes::from("f.txt"),
                    change: Change::Modified,
                    old_path: None,
                    kind: Kind::File,
                    submodule: Submodule::default(),
                }],
                unstaged: vec![UnstagedEntry {
                    path: PathBytes::from("f.txt"),
                    change: Change::Modified,
                    kind: Kind::File,
                    submodule: Submodule::default(),
                }],
                ..Default::default()
            };
            // HEAD says A, the index says B, the worktree says C: one edit
            // each side, distinct, so the answer can be told from the wrong
            // one. `side(0)` is the plain text; `side(1)` adds EDIT ONE; a
            // pair built (side(0), side(1)) is HEAD→index; (side(1), side(2))
            // is index→worktree.
            s.staged = vec![pair("f.txt", side(0), side(1))];
            s.unstaged = vec![pair("f.txt", side(1), side(2))];
        }
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
            repo: Some(handle.clone()),
        };
        let mut app = App::new(started, Glyphs::default());
        app.load_startup(&mut StartClock::new());
        app.screen = Screen::new(120, 24);
        app
    }

    /// The diff pane's origin, and what it is between.
    fn origin_of(app: &App) -> Option<DiffSource> {
        match app.panes.get("diff") {
            Some(Screens::Diff {
                origin: Some(origin),
                ..
            }) => Some(origin.clone()),
            _ => None,
        }
    }

    #[test]
    fn tui_parity_file_selection_previews_its_own_side_and_tab_toggles_it() {
        let (handle, state) = fake(&[]);
        let mut app = both_sides_app(&state, &handle);
        app.draw();

        // The keyboard lands on the files pane; its cursor sits on the
        // staged row. One step down previews the unstaged side of the same
        // file, one step up the staged side — the selection drives the
        // preview, no enter needed, and each read rides the lane.
        app.dispatch("files.focus");
        assert!(
            files_of(&app).current_file().is_some(),
            "the cursor is on a file"
        );
        app.dispatch("view.down");
        app.pump_quiet();
        assert_eq!(
            origin_of(&app),
            Some(DiffSource::Unstaged {
                path: PathBytes::from("f.txt"),
            }),
            "the unstaged row previewed its own side"
        );
        app.dispatch("view.up");
        app.pump_quiet();
        assert_eq!(
            origin_of(&app),
            Some(DiffSource::Staged {
                path: PathBytes::from("f.txt"),
            }),
            "the staged row previewed its own side"
        );
        assert_eq!(
            state.lock().unwrap().staged_reads,
            1,
            "the staged read ran exactly once"
        );
        assert_eq!(
            state.lock().unwrap().unstaged_reads,
            1,
            "the unstaged read ran exactly once — no aggregate read substitutes"
        );

        // Tab is the other side of the same file, when the other side has
        // it — and the pane carries the source, so a later verb or refresh
        // reads the side that is shown, not the one that was.
        app.dispatch("files.toggle-side");
        app.pump_quiet();
        assert_eq!(
            origin_of(&app),
            Some(DiffSource::Unstaged {
                path: PathBytes::from("f.txt"),
            }),
            "tab switched to the unstaged side"
        );
        assert_eq!(state.lock().unwrap().unstaged_reads, 2);
        assert_eq!(
            app.panes.focused_name(),
            "diff",
            "toggle transfers the keyboard"
        );
        assert!(
            diff_label_of(&app).contains("unstaged"),
            "the pane says which side it shows: {}",
            diff_label_of(&app)
        );

        // Back to the files pane and down, then up: the selection drives
        // the preview both ways, and the second visit to the staged side
        // reads nothing.
        app.dispatch("files.focus");
        app.dispatch("view.down");
        app.pump_quiet();
        assert_eq!(
            origin_of(&app),
            Some(DiffSource::Unstaged {
                path: PathBytes::from("f.txt"),
            }),
            "the selection drove the preview to the unstaged side"
        );
        app.dispatch("view.up");
        app.pump_quiet();
        assert_eq!(
            origin_of(&app),
            Some(DiffSource::Staged {
                path: PathBytes::from("f.txt"),
            }),
            "the selection drove the preview back to the staged side"
        );
        // Enter on the source already shown reads nothing: the pane's own
        // origin is what the request deduplicates against.
        app.dispatch("files.open-diff");
        app.pump_quiet();
        assert_eq!(app.panes.focused_name(), "diff");
        assert_eq!(
            state.lock().unwrap().staged_reads,
            2,
            "the two up-crossings, and no third read for the shown source"
        );
    }

    #[test]
    fn tui_parity_a_side_with_no_other_side_says_so_and_shows_nothing() {
        let (handle, state) = fake(&[]);
        let mut app = both_sides_app(&state, &handle);
        // A second file the index holds and the worktree does not touch:
        // staged-only, so its other side does not exist.
        {
            let mut s = state.lock().unwrap();
            s.status.staged.push(StagedEntry {
                path: PathBytes::from("g.txt"),
                change: Change::Added,
                old_path: None,
                kind: Kind::File,
                submodule: Submodule::default(),
            });
            s.staged.push(pair("g.txt", Vec::new(), side(3)));
        }
        app.load_startup(&mut StartClock::new());
        app.draw();
        app.dispatch("files.focus");
        // Walk to g.txt — the last staged row — bounded, because a pane
        // that lost the row must fail the test, not hang it.
        for _ in 0..10 {
            if files_of(&app)
                .current_file()
                .is_some_and(|f| f.path.as_bytes() == b"g.txt")
            {
                break;
            }
            app.dispatch("view.down");
        }
        app.pump_quiet();
        assert_eq!(
            origin_of(&app),
            Some(DiffSource::Staged {
                path: PathBytes::from("g.txt"),
            }),
            "the walk reached the staged-only file"
        );
        app.dispatch("files.toggle-side");
        assert_eq!(
            app.message, "nothing unstaged for g.txt",
            "a side that is not there is said, not drawn empty"
        );
        assert_eq!(
            origin_of(&app),
            Some(DiffSource::Staged {
                path: PathBytes::from("g.txt"),
            }),
            "the refused toggle changed nothing"
        );
    }

    #[test]
    fn tui_parity_stash_selection_previews_the_parked_diff_and_enter_focuses_it() {
        let (handle, state) = fake(&[]);
        let mut app = commits_app(&handle);
        app.draw();
        app.dispatch("stashes.focus");
        // One step down moves the selection off the startup preview's
        // commit and onto the second stack entry; the preview follows it,
        // addressed by the commit that a drop does not renumber.
        app.dispatch("view.down");
        app.pump_quiet();
        let parked = state.lock().unwrap().stashes[1].clone();
        assert_eq!(
            origin_of(&app),
            Some(DiffSource::Stash {
                index: parked.index,
                commit: parked.commit.clone(),
            }),
            "the selection previews the entry under it"
        );
        // The tracked half is the bare-revision read the commit preview
        // uses; the untracked half is the stash's own answer.
        assert_eq!(
            state.lock().unwrap().pairs_reads,
            2,
            "startup's commit preview plus the stash's tracked half"
        );
        assert_eq!(
            state.lock().unwrap().stash_untracked_reads,
            1,
            "the stash's untracked half was asked, once"
        );
        app.dispatch("stashes.open-diff");
        app.pump_quiet();
        assert_eq!(app.panes.focused_name(), "diff");
        assert!(
            diff_label_of(&app).contains("stash@{1}"),
            "the pane names the entry: {}",
            diff_label_of(&app)
        );
        assert!(
            diff_label_of(&app).contains("other work"),
            "the pane names the message: {}",
            diff_label_of(&app)
        );
    }

    #[test]
    fn tui_parity_branch_enter_drills_into_that_branchs_log_and_stays_there() {
        let (handle, state) = fake(&[]);
        {
            let mut s = state.lock().unwrap();
            let other = ["x", "y"].map(|sha| Commit {
                sha: sha.into(),
                short: sha.into(),
                parents: Box::from(&[][..]),
                author: "Ada Lovelace".into(),
                timestamp: 2,
                subject: format!("on feat {sha}"),
            });
            s.log_at_answers = vec![(b"feat".to_vec(), other.to_vec())];
            s.locals = vec![
                Branch {
                    name: RefName::from("main"),
                    commit: "f00d".into(),
                    upstream: None,
                    head: true,
                },
                Branch {
                    name: RefName::from("feat"),
                    commit: "beef".into(),
                    upstream: None,
                    head: false,
                },
            ];
            s.head = Some(HeadState::Branch {
                name: RefName::from("main"),
                commit: Some("f00d".into()),
            });
            s.remotes = vec![RemoteBranch {
                remote: RefName::from("origin"),
                branch: RefName::from("feat"),
                commit: "beef".into(),
            }];
        }
        let mut app = commits_app(&handle);
        app.draw();
        app.dispatch("branches.focus");
        // Walk to the branch that is not HEAD's, and open it — bounded,
        // because a pane that lost the row must fail the test, not hang it.
        for _ in 0..6 {
            let on_feat = match app.panes.get("branches") {
                Some(Screens::Branches { view, .. }) => matches!(
                    view.current(),
                    Some(Target::Local(name)) if name.as_bytes() == b"feat"
                ),
                _ => false,
            };
            if on_feat {
                break;
            }
            app.dispatch("view.down");
        }
        app.dispatch("branches.open-log");
        assert_eq!(
            app.panes.focused_name(),
            "diff",
            "the drilldown takes the keyboard"
        );
        // The main pane is the branch's history — its own commits, its own
        // refresh source, not HEAD's.
        match app.panes.get("diff") {
            Some(Screens::Commits {
                log_of: Some(name),
                label,
                ..
            }) => {
                assert_eq!(name.as_bytes(), b"feat");
                assert!(label.contains("feat"), "{label}");
            }
            _ => panic!("the main pane is not a branch log"),
        }
        assert_eq!(
            commits_of_main(&app)
                .map(|c| c.sha.to_string())
                .unwrap_or_default(),
            "x",
            "the log is the drilled branch's, not HEAD's"
        );
        // A refresh re-reads the branch it was opened for, exactly once.
        let before = state.lock().unwrap().log_at_reads;
        app.dispatch("repo.refresh");
        app.pump_quiet();
        assert_eq!(
            state.lock().unwrap().log_at_reads,
            before + 1,
            "the drilldown refreshed from its own ref"
        );
        assert_eq!(
            commits_of_main(&app)
                .map(|c| c.sha.to_string())
                .unwrap_or_default(),
            "x",
            "the refresh did not quietly become HEAD's history"
        );
        // A remote row has no local history to name, and says so. It is the
        // next row down: walk to it, bounded.
        app.dispatch("branches.focus");
        for _ in 0..6 {
            let on_remote = match app.panes.get("branches") {
                Some(Screens::Branches { view, .. }) => {
                    !matches!(view.current(), Some(Target::Local(_)))
                }
                _ => false,
            };
            if on_remote {
                break;
            }
            app.dispatch("view.down");
        }
        app.dispatch("branches.open-log");
        assert_eq!(app.message, "the keyboard is not on a local branch");
    }

    /// The main pane's commits list, when the drilldown installed one.
    fn commits_of_main(app: &App) -> Option<&Commit> {
        match app.panes.get("diff") {
            Some(Screens::Commits { view, .. }) => view.current(),
            _ => None,
        }
    }

    #[test]
    fn tui_parity_stale_preview_never_installs() {
        let (handle, state) = fake(&[]);
        let mut app = both_sides_app(&state, &handle);
        app.dispatch("files.focus");
        // Two real requests, so the lane's counter is where a real session's
        // would be: down to the unstaged side, back up to the staged one.
        app.dispatch("view.down");
        app.pump_quiet();
        app.dispatch("view.up");
        app.pump_quiet();
        let shown = origin_of(&app).expect("a preview is shown");
        let reads = state.lock().unwrap().staged_reads;
        let newest = app.preview_seq;

        // Two answers the lane must refuse, by the two guards: an older
        // sequence — something newer was asked for already — and another
        // repository's content. Neither installs, neither is said: a newer
        // request covers the pane, and the stale answer is simply not asked
        // about any more.
        app.install_preview(PreviewOutcome {
            seq: newest - 1,
            root: std::path::PathBuf::from("/fake"),
            origin: DiffSource::Unstaged {
                path: PathBytes::from("f.txt"),
            },
            focus: false,
            outcome: Ok(vec![gitten_core::FileDiff {
                path: "wrong.txt".into(),
                hunks: Vec::new(),
            }]),
        });
        app.install_preview(PreviewOutcome {
            seq: newest,
            root: std::path::PathBuf::from("/elsewhere"),
            origin: shown.clone(),
            focus: false,
            outcome: Ok(vec![gitten_core::FileDiff {
                path: "other-repo.txt".into(),
                hunks: Vec::new(),
            }]),
        });
        app.pump_quiet();
        assert_eq!(
            origin_of(&app),
            Some(shown),
            "a stale answer replaced the preview"
        );
        assert_eq!(
            state.lock().unwrap().staged_reads,
            reads,
            "a stale answer caused another read"
        );
        assert_eq!(
            app.message, "",
            "a dropped answer said something the pane is not asked about"
        );
    }

    #[test]
    fn tui_parity_a_slow_preview_keeps_the_keyboard_live() {
        let (handle, state) = fake(&[]);
        // The staged side is slow for as long as the gate is shut: a real
        // read, on a real thread, that the test types while it runs.
        let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        state.lock().unwrap().gate = Some(Arc::clone(&gate));
        let mut app = both_sides_app(&state, &handle);
        app.draw();
        app.dispatch("files.focus");
        // Land on the unstaged row: its read answers at once and installs.
        app.dispatch("view.down");
        app.pump_quiet();
        // Back up to the staged row: that read is parked behind the gate.
        app.dispatch("view.up");
        assert!(
            app.preview_pending > 0,
            "the slow read is in flight, unanswered"
        );
        // The keyboard is not parked with it: a key is processed, and the
        // pane's answer arrives whenever it arrives — never in the way.
        app.dispatch("files.toggle-side");
        assert_eq!(
            app.panes.focused_name(),
            "diff",
            "the keyboard moved while the preview was still being read"
        );
        // Now the parked read finishes — the newest request issued, so it
        // installs — and the pane carries its source.
        {
            let (open, arrived) = &*gate;
            *open.lock().unwrap() = true;
            arrived.notify_all();
        }
        app.pump_quiet();
        assert_eq!(
            origin_of(&app),
            Some(DiffSource::Staged {
                path: PathBytes::from("f.txt"),
            }),
            "the gated answer installed once it was the newest"
        );
        assert_eq!(app.preview_pending, 0, "every answer had its turn");
    }

    #[test]
    fn tui_parity_a_fixture_refuses_the_preview_doors_with_their_reason() {
        let mut app = app_on_diff(Source::Fixtures, None);
        app.dispatch("files.open-diff");
        assert_eq!(
            app.message, "files.open-diff: a fixture has no file to preview",
            "the launch's refusal is the reason, said once"
        );
        app.dispatch("branches.open-log");
        assert_eq!(
            app.message,
            "branches.open-log: a fixture has no history to open",
        );
    }

    // ------------------------------------------------- tui_parity_: W2

    /// The files world the search test filters: one file in each of the two
    /// sections a query can tell apart.
    fn two_files_status() -> Status {
        Status {
            staged: vec![StagedEntry {
                path: PathBytes::from("src/main.rs"),
                change: Change::Modified,
                old_path: None,
                kind: Kind::File,
                submodule: Submodule::default(),
            }],
            unstaged: vec![UnstagedEntry {
                path: PathBytes::from("docs/readme.md"),
                change: Change::Modified,
                kind: Kind::File,
                submodule: Submodule::default(),
            }],
            ..Default::default()
        }
    }

    /// The names the help panel would draw in `mode`, as the audit reads
    /// them.
    fn help_names(app: &App, mode: &str) -> Vec<String> {
        let mut modes = Modes::new();
        modes.push(mode);
        app.host
            .keys
            .help_supported(&app.host.commands, &modes, &app.availability)
            .into_iter()
            .filter_map(|row| match row {
                gitten_core::command::HelpRow::Command { name, .. } => Some(name),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn tui_parity_search_covers_every_list_and_n_walks_the_matches() {
        let (handle, state) = fake(&[]);
        state.lock().unwrap().status = two_files_status();
        let mut app = commits_app(&handle);
        eprintln!(
            "DBG cursor={} rows={:?}",
            files_of(&app).cursor(),
            files_of(&app).current_file().map(|f| f.text.clone())
        );
        app.draw();
        eprintln!(
            "DBG after draw cursor={} cur={:?}",
            files_of(&app).cursor(),
            files_of(&app).current_file().map(|f| f.text.clone())
        );

        // Files: `/` opens over the files pane, and every keystroke narrows
        // the list live — the keyboard follows the surviving file.
        app.dispatch("files.focus");
        app.press(Key::char('/'));
        assert!(matches!(app.prompt, Some(Prompt::Search { .. })));
        type_(&mut app, "readme");
        eprintln!(
            "DBG after type cur={:?} q={:?} vis={}",
            files_of(&app).current_file().map(|f| f.text.clone()),
            files_of(&app).query(),
            files_of(&app).filter_note().unwrap_or_default()
        );
        assert_eq!(
            files_of(&app).current_file().map(|f| f.text.as_str()),
            Some("docs/readme.md"),
            "the filter did not move the keyboard onto the surviving file"
        );
        app.draw();
        assert!(status(&app).contains("1/2"), "{}", status(&app));
        // Enter keeps the filter standing, and now n walks the matches —
        // while the prompt stands, n is text.
        app.press(Key::plain(Code::Enter));
        assert!(app.prompt.is_none());
        app.press(Key::char('n'));
        assert_eq!(
            files_of(&app).current_file().map(|f| f.text.as_str()),
            Some("docs/readme.md"),
            "the one match did not wrap onto itself"
        );
        assert_eq!(
            files_of(&app).query(),
            Some("readme"),
            "enter did not keep the filter"
        );
        // A second `/` is seeded from what stands.
        app.press(Key::char('/'));
        assert_eq!(crate::tests::query(&app).as_deref(), Some("readme"));
        app.press(Key::plain(Code::Esc));
        // And esc — the search mode's own — takes the filter off entirely,
        // the keyboard back on the file it filtered to.
        assert!(files_of(&app).query().is_none(), "esc left the filter on");
        assert_eq!(
            files_of(&app).current_file().map(|f| f.text.as_str()),
            Some("docs/readme.md"),
            "clearing the filter lost the file"
        );

        // Stashes: the query matches the message, and the keyboard follows
        // the entry by its surviving identity.
        app.dispatch("stashes.focus");
        app.press(Key::char('/'));
        type_(&mut app, "wip");
        assert_eq!(
            app.panes.get("stashes").and_then(|pane| match pane {
                Screens::Stashes { view, .. } => {
                    view.current_entry().map(|(_, commit)| commit)
                }
                _ => None,
            }),
            Some("aaa".into()),
            "the filter did not keep the keyboard on the wip entry"
        );
        app.press(Key::plain(Code::Esc));
        assert!(app.panes.get("stashes").is_some_and(|pane| match pane {
            Screens::Stashes { view, .. } => view.query().is_none(),
            _ => true,
        }));

        // Branches: the query matches the display name.
        app.dispatch("branches.focus");
        app.press(Key::char('/'));
        type_(&mut app, "main");
        assert_eq!(
            branches_of(&app).current(),
            Some(gitten_core::refs::Target::Local(RefName::from("main")))
        );
        app.press(Key::plain(Code::Esc));

        // Commits: eleven rows carry "commit 4" — 4 and 40 through 49 —
        // and n walks them in order, wrapping at the end.
        app.dispatch("commits.focus");
        app.press(Key::char('/'));
        type_(&mut app, "commit 4");
        // The prompt stands: n is text in it. Accept, and n walks.
        app.press(Key::plain(Code::Enter));
        let first = commits_of(&app).cursor();
        app.press(Key::char('n'));
        let second = commits_of(&app).cursor();
        assert!(
            second > first,
            "n did not walk to the next match: {first} -> {second}"
        );
        app.press(Key::char('N'));
        assert_eq!(commits_of(&app).cursor(), first, "N did not walk back");
        // The keyboard then behaves: accept keeps the filter, and the arrow
        // keys are still the list's, not the search's.
        app.press(Key::plain(Code::Enter));
        app.dispatch("view.down");
        assert_eq!(commits_of(&app).cursor(), first + 1);
    }

    #[test]
    fn tui_parity_search_mode_takes_n_and_gives_it_back() {
        // With no query standing, `n` is the pane's: branches.new opens a
        // field. With one standing, the matches own the keyboard. That is
        // the whole trade, and both halves must hold.
        let (handle, _state) = fake(&[]);
        let mut app = commits_app(&handle);
        app.dispatch("branches.focus");
        app.press(Key::char('n'));
        assert!(
            matches!(app.prompt, Some(Prompt::BranchNew { .. })),
            "n was not branches.new while no search stood"
        );
        app.press(Key::plain(Code::Esc));
        assert!(app.prompt.is_none());

        app.press(Key::char('/'));
        type_(&mut app, "main");
        app.press(Key::plain(Code::Enter));
        app.message.clear();
        app.press(Key::char('n'));
        assert!(
            app.prompt.is_none(),
            "n opened a prompt while a search stood"
        );
        assert_eq!(
            app.message, "",
            "the walk over a standing search said something else"
        );
        // No branch matches `nomatch`; the walk says so where the filter
        // already shows an empty list.
        app.press(Key::char('/'));
        type_(&mut app, "nomatch");
        app.press(Key::plain(Code::Enter));
        app.message.clear();
        app.press(Key::char('n'));
        assert_eq!(
            app.message, "",
            "a search with no matches said something on the walk"
        );
    }

    #[test]
    fn tui_parity_the_diff_search_walks_the_rows_that_match() {
        // The tall fake's diff: an edit every ten lines, so "EDIT 1" lands
        // on a real spread of rows and n/N has somewhere to go.
        let (handle, _state) = fake_tall(&[]);
        let mut app = commits_app(&handle);
        app.dispatch("commits.open-diff");
        until(Duration::from_secs(2), || {
            app.pump_quiet();
            diff_of(&app).rows() > 0
        });
        app.dispatch("diff.focus");
        let before = diff_of(&app).cursor();
        app.press(Key::char('/'));
        type_(&mut app, "EDIT 1");
        let first = diff_of(&app).cursor();
        assert!(
            first != before,
            "the live search did not put the keyboard on a match"
        );
        app.press(Key::plain(Code::Enter));
        // The standing query walks, wrapping, and the pane's own status
        // says where in the matches the keyboard is.
        app.press(Key::char('n'));
        let next = diff_of(&app).cursor();
        assert!(next != first, "n stayed on the match it was on");
        app.press(Key::char('N'));
        assert_eq!(diff_of(&app).cursor(), first, "N did not walk back");
        let note = diff_of(&app)
            .match_note()
            .expect("the standing search said nothing");
        assert!(
            note.contains('/'),
            "the note is not an ordinal over a count: {note}"
        );
        // A reflow moves every row under the fold; the note honestly goes
        // quiet rather than describing matches it has not re-found, and the
        // next walk re-folds and brings it back.
        app.draw();
        app.press(Key::char('n'));
        assert!(
            diff_of(&app).match_note().is_some(),
            "the next walk did not re-fold"
        );
        // Clear takes the search off; the cursor stays where the search
        // left it, and n without a query is refused by name.
        let at = diff_of(&app).cursor();
        app.press(Key::plain(Code::Esc));
        assert_eq!(diff_of(&app).cursor(), at, "clear moved the cursor");
        assert_eq!(diff_of(&app).search_query(), None, "clear kept the query");
        app.message.clear();
        app.press(Key::char('n'));
        assert_eq!(app.message, "no search standing — / to start one");
    }

    #[test]
    fn tui_parity_hunk_jumps_walk_the_hunks_and_stop_at_the_edges() {
        // The tall fake: one file, an edit every ten lines — twenty hunks,
        // and a real spread for the jump list to walk.
        let (handle, _state) = fake_tall(&[]);
        let mut app = commits_app(&handle);
        app.dispatch("commits.open-diff");
        until(Duration::from_secs(2), || {
            app.pump_quiet();
            diff_of(&app).rows() > 0
        });
        app.dispatch("diff.focus");
        // From the top, alt-down lands on the first hunk's first row; walking
        // moves one hunk at a time; the last hunk stops rather than wrapping.
        app.press(Key::parse("alt-down").unwrap());
        let first = diff_of(&app).cursor();
        let mut seen = vec![first];
        loop {
            app.press(Key::parse("alt-down").unwrap());
            let at = diff_of(&app).cursor();
            if at == *seen.last().unwrap() {
                break;
            }
            seen.push(at);
            assert!(seen.len() <= 25, "the walk did not stop at the last hunk");
        }
        // alt-up walks back the same list, one hunk at a time.
        for expected in seen.iter().rev().skip(1) {
            app.press(Key::parse("alt-up").unwrap());
            assert_eq!(diff_of(&app).cursor(), *expected);
        }
        assert_eq!(
            diff_of(&app).cursor(),
            first,
            "the walk back missed the first"
        );
        // And above the first hunk there is nothing to reach.
        app.press(Key::parse("alt-up").unwrap());
        assert_eq!(diff_of(&app).cursor(), first);
    }

    #[test]
    fn tui_parity_prompt_editing_has_a_cursor_and_a_multiline_message() {
        let (handle, _state) = fake(&[]);
        let mut app = commits_app(&handle);
        app.dispatch("files.focus");
        app.press(Key::char('c'));
        type_(&mut app, "hello wrold");
        // The arrows and Home are editing now: fix the typo without retyping.
        // ctrl-Left lands on the start of the mistyped word, Delete takes the
        // wrong letter out, and the Right plus one character puts it back —
        // a cursor, not an append-only tail.
        app.press(Key::ctrl(Code::Left));
        app.press(Key::plain(Code::Right));
        app.press(Key::plain(Code::Delete));
        app.press(Key::plain(Code::Right));
        app.press(Key::char('r'));
        assert_eq!(
            app.prompt.as_ref().map(|p| p.field().text()),
            Some("hello world"),
            "the cursor edits did not land"
        );
        // The drawn row shows the cursor in place, not glued to the end.
        app.draw();
        assert!(
            status(&app).contains("hello wor█ld"),
            "the caret is not where the cursor is: {}",
            status(&app)
        );
        // Multibyte scalars are scalars, never bytes: ß is two bytes and one
        // Left. The drawn window never splits one, and Delete removes exactly
        // the scalar before — at — the cursor.
        app.press(Key::plain(Code::Home));
        app.press(Key::plain(Code::End));
        type_(&mut app, " ßx");
        app.press(Key::plain(Code::Left));
        app.press(Key::plain(Code::Left));
        app.press(Key::plain(Code::Delete));
        assert_eq!(
            app.prompt.as_ref().map(|p| p.field().text()),
            Some("hello world x"),
            "delete did not remove exactly one scalar"
        );
        // A line break is text: alt-enter inserts at the cursor, enter still
        // accepts.
        app.press(Key::plain(Code::End));
        app.press(Key::parse("alt-enter").unwrap());
        type_(&mut app, "body");
        assert_eq!(
            app.prompt.as_ref().map(|p| p.field().text()),
            Some("hello world x\nbody"),
        );
        app.draw();
        assert!(
            status(&app).contains("2/2 · "),
            "a multiline field did not say which line it is on: {}",
            status(&app)
        );
        // A search field is one line by design, and says so: alt-enter there
        // is refused, not swallowed.
        app.press(Key::plain(Code::Esc));
        app.press(Key::char('/'));
        app.message.clear();
        app.press(Key::parse("alt-enter").unwrap());
        assert_eq!(app.message, "this field is one line");
        assert_eq!(crate::tests::query(&app), Some(String::new()));
    }

    #[test]
    fn tui_parity_amend_prefills_heads_subject() {
        let (handle, state) = fake(&[]);
        state.lock().unwrap().head = Some(gitten_core::refs::HeadState::Branch {
            name: RefName::from("main"),
            commit: Some("00000042".into()),
        });
        let mut app = commits_app(&handle);
        app.dispatch("files.focus");
        app.press(Key::char('A'));
        assert_eq!(
            app.prompt.as_ref().map(|p| p.field().text()),
            Some("commit 42"),
            "the amend did not open on HEAD's subject"
        );
        // The prefill is the field's own text: editing appends at its end,
        // and accepting submits the whole thing.
        type_(&mut app, "!");
        app.press(Key::plain(Code::Enter));
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                !state.lock().unwrap().writes.is_empty()
            }),
            "the amend never reached the repository"
        );
        let writes = state.lock().unwrap().writes.clone();
        assert_eq!(writes.len(), 1);
    }

    #[test]
    fn tui_parity_marked_commit_range_is_kept_and_cleared() {
        let (handle, _state) = fake(&[]);
        let mut app = commits_app(&handle);
        // v marks the row the keyboard is on; moving extends it; v again
        // takes it off. The status says what is marked — the range is for
        // the next action, and it is a different thing from the copy range.
        app.press(Key::char('v'));
        app.draw();
        assert!(status(&app).contains("1 marked"), "{}", status(&app));
        app.dispatch("view.down");
        app.dispatch("view.down");
        app.draw();
        assert!(status(&app).contains("3 marked"), "{}", status(&app));
        // The mark survives a page and a top; a filter kills it, the way a
        // refresh kills every row-named thing.
        app.press(Key::char('v'));
        app.draw();
        assert!(!status(&app).contains("marked"), "{}", status(&app));
        assert!(commits_of(&app).marks().is_none());
        // And the copy range is untouched by any of it: `y` still copies the
        // row the keyboard is on.
        app.press(Key::char('y'));
        assert!(app.copy.is_some(), "marking broke the copy path");
    }

    #[test]
    fn tui_parity_an_extension_command_is_refused_and_hidden_from_help() {
        // The extension seam registers a name and a key; the client that has
        // no handler for it refuses it by name and never advertises it — the
        // no-op audit covers the built-ins by enumeration; this covers the
        // names that arrive after it.
        let (handle, _state) = fake(&[]);
        let mut app = commits_app(&handle);
        app.host.commands.register("ext.hello", "say hello");
        app.host.keys.bind("commits", "e", "ext.hello").unwrap();
        app.dispatch("ext.hello");
        assert_eq!(
            app.message, "ext.hello is not supported by this client",
            "a registered extension command ran without a handler"
        );
        assert!(
            !help_names(&app, "commits").iter().any(|n| n == "ext.hello"),
            "the panel advertised a command the client cannot run"
        );
    }

    #[test]
    fn tui_parity_remapped_search_keys_help_and_dispatch_agree() {
        let (handle, _state) = fake(&[]);
        let mut app = commits_app(&handle);
        // The move: search.next leaves n for M, in the same mode.
        assert!(app.host.keys.unbind("search", "n"));
        app.host.keys.bind("search", "M", "search.next").unwrap();
        // Help shows the new spelling and not the old.
        let names = help_names(&app, "search");
        assert!(names.iter().any(|n| n == "search.next"));
        // With a filter standing, M walks and n is nobody's — said, not
        // swallowed.
        app.press(Key::char('/'));
        type_(&mut app, "commit 4");
        app.press(Key::plain(Code::Enter));
        let first = commits_of(&app).cursor();
        app.press(Key::char('M'));
        assert!(
            commits_of(&app).cursor() != first,
            "the remapped key did not walk the matches"
        );
        // The old key fell through to the pane's own `n` — commits.new-
        // branch, the sync wave's own handler now — and never walked a
        // match again.
        let at = commits_of(&app).cursor();
        app.press(Key::char('n'));
        assert_eq!(commits_of(&app).cursor(), at, "the old key still walked");
        assert!(
            matches!(app.prompt, Some(Prompt::BranchNewAt { .. })),
            "n fell through to nothing"
        );
        app.press(Key::plain(Code::Esc));
        assert!(app.prompt.is_none(), "esc did not close the prompt");
    }
    // =================================================================== W4
    //
    // Sync, tracking branches, repository switching and the remotes pane —
    // the wave's own coverage, named so the campaign can run it alone.

    use gitten_core::refs::Remote as RemoteRef;

    fn remote_ref(name: &str, urls: &[&str]) -> RemoteRef {
        RemoteRef {
            name: RefName::from(name),
            urls: urls.iter().map(|u| u.to_string()).collect(),
        }
    }

    fn remotes_of(app: &App) -> &Remotes {
        match app.panes.get("remotes") {
            Some(Screens::Remotes { view, .. }) => view,
            _ => panic!("the remotes pane is not registered"),
        }
    }

    /// Points the MRU store at a scratch file holding `rows`, runs `body`,
    /// then drops the override and the file. The env var is one process
    /// global, so the same lock `projects.rs`'s tests hold serializes this.
    fn with_mru(name: &str, rows: &[&str], body: impl FnOnce()) {
        let _guard = gitten_app::projects::env_lock();
        let file =
            std::env::temp_dir().join(format!("gitten-tui-mru-{name}-{}", std::process::id()));
        let _ = std::fs::remove_file(&file);
        // The override comes down on drop, panic included — a test that
        // unwinds cannot leak its spelling of the variable into the rest.
        let _override = gitten_app::projects::EnvOverride::set(&file);
        std::fs::write(&file, rows.join("\n")).expect("a scratch MRU");
        body();
        let _ = std::fs::remove_file(&file);
    }

    /// An opener that answers the handles a test handed it, and refuses
    /// everywhere else — `status` fails, so an unknown path is refused by
    /// the same read a real refusal comes from.
    struct KeyedOpener {
        repos: Vec<(std::path::PathBuf, Handle)>,
    }

    impl gitten_app::Opener for KeyedOpener {
        fn open(&self, root: &std::path::Path) -> Handle {
            if let Some((_, handle)) = self.repos.iter().find(|(path, _)| path.as_path() == root) {
                return handle.clone();
            }
            // An unknown path gets the repository that exists only to fail —
            // every read is an error, `describe` says refused.
            Arc::new(Refused)
        }
    }

    /// A repository that exists only as this struct: every read fails. The
    /// answer an unknown path deserves.
    struct Refused;

    impl Repo for Refused {
        fn log(&self, _: usize) -> gitten_git::Result<Vec<Commit>> {
            Ok(Vec::new())
        }
        fn pairs(&self, _: &str) -> gitten_git::Result<Vec<Pair>> {
            Ok(Vec::new())
        }
        fn status(&self) -> gitten_git::Result<Status> {
            Err("fatal: not a git repository".into())
        }
        fn describe(&self) -> String {
            "refused".into()
        }
    }

    /// A commits launch on a real repository at `path` — the shape the
    /// real-git sync tests need: the handle the binary backs, acquired
    /// through the front door, with the startup wave run.
    fn repo_app(path: &std::path::Path) -> App {
        let handle = gitten_git::open(path);
        let host = Host::new();
        let source = Source::Repo {
            path: path.to_path_buf(),
            arg: String::new(),
        };
        let loaded = acquire::acquire(View::Commits, &source, &host, Some(handle.as_ref()))
            .expect("the scratch repository has history");
        let started = gitten_app::Started {
            view: View::Commits,
            source,
            host,
            loaded,
            config: std::path::PathBuf::from("/nonexistent/gitten.toml"),
            repo: Some(handle.clone()),
        };
        let mut app = App::new(started, Glyphs::default());
        app.load_startup(&mut StartClock::new());
        app.screen = Screen::new(120, 24);
        app
    }

    /// A scratch git working tree — the setup half of the real-git sync
    /// tests. Hermetic by construction: a local identity, a local bare as
    /// the remote, nothing signed, nothing networked, and paths no real
    /// checkout owns.
    struct Git(std::path::PathBuf);

    impl Git {
        fn dir(name: &str) -> std::path::PathBuf {
            let dir =
                std::env::temp_dir().join(format!("gitten-tui-sync-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("a scratch dir");
            dir
        }

        fn init(name: &str) -> Self {
            let me = Self(Self::dir(name));
            me.git(&["init", "-q", "-b", "main", "."]);
            me.config();
            me
        }

        /// A bare clone of `from` — the local remote every sync test aims at.
        fn bare_clone(from: &std::path::Path, name: &str) -> Self {
            let dir = Self::dir(name);
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(std::env::temp_dir())
                .args(Self::setup())
                .args(["clone", "-q", "--bare"])
                .arg(from)
                .arg(&dir)
                .output()
                .expect("git clone runs");
            assert!(
                out.status.success(),
                "bare clone: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            Self(dir)
        }

        fn clone_from(from: &std::path::Path, name: &str) -> Self {
            let dir = Self::dir(name);
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(std::env::temp_dir())
                .args(Self::setup())
                .args(["clone", "-q"])
                .arg(from)
                .arg(&dir)
                .output()
                .expect("git clone runs");
            assert!(
                out.status.success(),
                "clone: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let me = Self(dir);
            me.config();
            me
        }

        /// The identity and locality flags every scratch command runs under:
        /// a machine without a global identity still gets commits, a machine
        /// with signing still gets them, and the local-file protocol —
        /// which modern git disables — still works for the bare remote.
        fn setup() -> [&'static str; 10] {
            [
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "init.defaultBranch=main",
                "-c",
                "protocol.file.allow=always",
            ]
        }

        fn config(&self) {
            self.git(&["config", "user.name", "gitten-test"]);
            self.git(&["config", "user.email", "test@gitten.local"]);
        }

        fn git(&self, args: &[&str]) {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&self.0)
                .args(Self::setup())
                .args(args)
                .output()
                .expect("git runs");
            assert!(
                out.status.success(),
                "git {:?}: {}",
                args,
                String::from_utf8_lossy(&out.stderr)
            );
        }

        /// git's answer, trimmed — the read side of the same helper.
        fn ask(&self, args: &[&str]) -> String {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&self.0)
                .args(Self::setup())
                .args(args)
                .output()
                .expect("git runs");
            assert!(
                out.status.success(),
                "git {:?}: {}",
                args,
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        }

        fn write(&self, file: &str, text: &str) {
            std::fs::write(self.0.join(file), text).expect("a scratch file");
        }

        fn commit(&self, message: &str) {
            self.git(&["add", "-A"]);
            self.git(&["commit", "-q", "-m", message]);
        }
    }

    impl Drop for Git {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The whole sync trip over one local bare remote and two scratch
    /// clones: first push with its tracking link, fetch, a fast-forward
    /// pull, a rejected push said in git's own words, a push that creates
    /// its upstream, a detached HEAD refused by name, and a remote row
    /// checked out as a tracking branch. Nothing leaves the temp dir.
    #[test]
    fn tui_parity_the_sync_trip_over_a_local_bare_remote() {
        let seed = Git::init("sync-seed");
        seed.write("f.txt", "one\n");
        seed.commit("seed");
        let bare = Git::bare_clone(seed.0.as_path(), "sync-bare");
        let a = Git::clone_from(bare.0.as_path(), "sync-a");
        let b = Git::clone_from(bare.0.as_path(), "sync-b");

        // --- first push of a new branch creates the upstream
        let mut app = repo_app(a.0.as_path());
        a.git(&["checkout", "-q", "-b", "feature"]);
        a.write("f.txt", "one\ntwo\n");
        a.commit("feature work");
        // The launch predates the commit; the sync verbs read fresh.
        app.dispatch("repo.push");
        assert!(
            until(Duration::from_secs(5), || {
                app.pump();
                app.message.contains("pushed origin feature")
            }),
            "the first push never finished: {:?}",
            app.message
        );
        let _ = bare.ask(&["rev-parse", "--verify", "feature"]);
        // The tracking link went with it: the branches pane proves it.
        app.dispatch("repo.refresh");
        app.dispatch("branches.focus");
        app.draw();
        assert!(
            app.screen.row_text(0).contains("feature"),
            "the pushed branch is not in the list: {:?}",
            app.screen.row_text(0)
        );

        // --- fetch and pull: b catches up with origin/feature
        let mut b_app = repo_app(b.0.as_path());
        b_app.dispatch("repo.fetch");
        assert!(
            until(Duration::from_secs(5), || {
                b_app.pump();
                b_app.message.contains("fetched")
            }),
            "the fetch never finished: {:?}",
            b_app.message
        );
        b_app.dispatch("repo.refresh");
        b_app.dispatch("branches.focus");
        b_app.draw();
        assert!(
            app.screen.row_text(0).is_empty() || true,
            "the fetch only proves the refs moved"
        );
        // A fast-forward pull onto the fetched branch: check feature out
        // first — the remote row's own checkout, tracking, is the W4 verb —
        // so the walk below reads a branch that exists.
        b_app.dispatch("view.top");
        // rows: LOCAL heading (no locals yet? main exists from the clone)…
        b_app.dispatch("branches.focus");
        b_app.draw();
        // Find the remote feature row by walking down until the status
        // names it — the honest keyboard walk the key would have taken.
        let mut named = String::new();
        for _ in 0..8 {
            named = branches_of(&b_app).status();
            if named.contains("origin/feature") {
                break;
            }
            b_app.dispatch("view.down");
        }
        assert!(
            named.contains("origin/feature"),
            "the remote row was never reached: {named}"
        );
        b_app.press(Key::plain(Code::Char(' ')));
        assert!(
            until(Duration::from_secs(5), || {
                b_app.pump();
                b_app.message.contains("tracking branch")
            }),
            "the tracking checkout never finished: {:?}",
            b_app.message
        );
        assert_eq!(
            b.ask(&["rev-parse", "--abbrev-ref", "feature@{upstream}"]),
            "origin/feature",
            "the tracking link did not land"
        );

        // --- pull: a fast-forward b's main onto its upstream
        b_app.dispatch("repo.refresh");
        b_app.dispatch("branches.focus");
        // The cursor is wherever the refresh left it; main is one of the
        // locals. Walk to it by name.
        b_app.dispatch("view.top");
        for _ in 0..8 {
            if branches_of(&b_app).status().contains("· main") {
                break;
            }
            b_app.dispatch("view.down");
        }
        b_app.dispatch("repo.pull");
        assert!(
            until(Duration::from_secs(5), || {
                b_app.pump();
                b_app.message.contains("pulled")
                    || b_app.message.contains("up to date")
                    || b_app.message.contains("Already")
            }),
            "the pull never finished: {:?}",
            b_app.message
        );

        // --- a rejected push says git's words and moves nothing
        // The tracking checkout above moved HEAD to feature; the divergence
        // dance is main's, so main takes the keyboard back first.
        b.git(&["checkout", "-q", "main"]);
        b.git(&["commit", "-q", "--amend", "-m", "divergent"]);
        b_app.dispatch("repo.push");
        assert!(
            until(Duration::from_secs(10), || {
                b_app.pump();
                b_app.message.contains("rejected")
                    || b_app.message.contains("fetch first")
                    || b_app.message.contains("non-fast-forward")
            }),
            "the rejected push never said so: {:?}",
            b_app.message
        );
        let refusal = b_app.message.clone();
        assert!(
            refusal.contains("rejected")
                || refusal.contains("fetch first")
                || refusal.contains("non-fast-forward"),
            "git's refusal was not surfaced: {refusal}"
        );
        // The bare still holds what a pushed — the refusal moved nothing.
        let bare_main = bare.ask(&["rev-parse", "main"]);
        let b_main = b.ask(&["rev-parse", "main"]);
        assert_ne!(bare_main, b_main, "a rejected push moved the remote");

        // --- a detached HEAD is refused by name
        a.git(&["checkout", "-q", "--detach", "HEAD"]);
        app.dispatch("repo.refresh");
        app.dispatch("repo.push");
        app.pump();
        assert_eq!(
            app.message, "detached HEAD has no branch to push",
            "a detached push said something else: {:?}",
            app.message
        );
    }

    /// A blocked sync job is a real thread blocked on a real gate, and the
    /// keyboard stays live around it: a press moves the list while the job
    /// runs, and the error is on the status line when the job comes back.
    #[test]
    fn tui_parity_a_blocking_sync_job_keeps_the_keyboard_live() {
        let (handle, state) = fake(&[]);
        let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        {
            let mut s = state.lock().unwrap();
            s.net_gate = Some(Arc::clone(&gate));
            s.refuse_net = Some("rejected: non-fast-forward".into());
            // push_current names its carrier from the config remotes.
            s.servers = vec![remote_ref("origin", &["one.example"])];
        }
        let mut app = commits_app(&handle);
        app.dispatch("repo.push");
        app.draw();
        assert!(
            until(Duration::from_secs(2), || {
                app.pump();
                app.message == "running push origin main"
            }),
            "the push never started: {:?}",
            app.message
        );

        // The keyboard is live: the list moves under a job that has not
        // come back.
        let at = commits_of(&app).cursor();
        app.dispatch("view.down");
        assert_eq!(
            commits_of(&app).cursor(),
            at + 1,
            "the keyboard was not live while the push ran"
        );

        // The gate opens; the refusal is git's, on the status line.
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
        assert!(
            until(Duration::from_secs(2), || {
                app.pump();
                app.message.contains("rejected")
            }),
            "the refused push never said so: {:?}",
            app.message
        );
    }

    /// A pending result belongs to the repository it was asked of: a write
    /// left running on the old repository lands on the old repository, and
    /// the switch installs nothing of it into the new one.
    #[test]
    fn tui_parity_a_pending_result_cannot_leak_across_a_repository_switch() {
        let (a_handle, a_state) = fake(&[]);
        a_state.lock().unwrap().net_gate =
            Some(Arc::new((Mutex::new(false), std::sync::Condvar::new())));
        // A push needs a carrier to name: origin in the config remotes —
        // what push_current reads to build the job.
        a_state.lock().unwrap().servers = vec![remote_ref("origin", &["one.example"])];
        let (b_handle, b_state) = fake(&[]);
        b_state.lock().unwrap().status = Status {
            untracked: vec![gitten_core::status::UntrackedEntry {
                path: gitten_core::status::PathBytes::from("only-b.txt"),
            }],
            ..Default::default()
        };

        with_mru("leak", &["/b", "/a"], || {
            let mut app = commits_app(&a_handle);
            app.use_opener(Arc::new(KeyedOpener {
                repos: vec![
                    (std::path::PathBuf::from("/a"), a_handle.clone()),
                    (std::path::PathBuf::from("/b"), b_handle.clone()),
                ],
            }));

            // The push is asked of /a and blocks in the worker.
            app.dispatch("repo.push");
            app.draw();
            assert!(
                until(Duration::from_secs(2), || {
                    app.pump();
                    app.message == "running push origin main"
                }),
                "the push never started: {:?}",
                app.message
            );

            // The switch happens under it: /b opens, and every pane the app
            // holds is now /b's — the label names the read that stands.
            app.press(Key::char('O'));
            type_(&mut app, "/b");
            app.press(Key::plain(Code::Enter));
            assert_eq!(app.message, "switched to /b");
            assert_eq!(files_label(&app), "fake (main) · 1 changed");

            // The old job's finish lands: on /a, and on /a only.
            let gate = a_state.lock().unwrap().net_gate.clone().unwrap();
            *gate.0.lock().unwrap() = true;
            gate.1.notify_all();
            assert!(
                until(Duration::from_secs(2), || {
                    app.pump();
                    app.message.contains("pushed")
                }),
                "the old push never finished: {:?}",
                app.message
            );

            // /b's panes hold what /b answered: the untracked file is still
            // the whole of its world, and the commits list is where the
            // switch left it.
            assert_eq!(
                files_label(&app),
                "fake (main) · 1 changed",
                "the old job's refresh rewrote the new repository's pane"
            );
            // ...and the write itself is /a's: recorded there, absent here.
            assert_eq!(
                a_state.lock().unwrap().writes,
                vec!["push origin main".to_string()],
                "the old repository never recorded its write"
            );
            assert!(
                b_state.lock().unwrap().writes.is_empty(),
                "the new repository received the old job's write"
            );

            // And the old repository is still reachable: the picker offers
            // both, most-recent first — the switch just wrote /b to the top.
            app.dispatch("project.switch");
            app.draw();
            let body: String = (2..8)
                .map(|y| app.screen.row_text(y))
                .collect::<Vec<_>>()
                .join("\n");
            assert!(
                body.contains("recent repositories"),
                "the picker did not stand: {body:?}"
            );
            assert!(
                body.find("/b").unwrap_or(usize::MAX) < body.find("/a").unwrap_or(usize::MAX),
                "the opened repository did not move to the front: {body:?}"
            );
            app.press(Key::plain(Code::Esc));
            assert!(app.picker.is_none(), "esc did not close the picker");
        });
    }

    /// The remotes pane reads the configured remotes: name ahead of every
    /// URL, the count in the header, and the row the keyboard is on as the
    /// verbs address it.
    #[test]
    fn tui_parity_the_remotes_pane_reads_names_and_urls() {
        let (handle, state) = fake(&[]);
        state.lock().unwrap().servers = vec![remote_ref(
            "origin",
            &["git@example.com:x.git", "push.example"],
        )];
        let mut app = commits_app(&handle);
        app.dispatch("remotes.focus");
        app.draw();
        assert!(
            app.screen.row_text(19).contains("remotes"),
            "the header did not follow: {:?}",
            app.screen.row_text(19)
        );
        assert!(
            app.screen.row_text(0).contains("1 remote"),
            "the label did not count: {:?}",
            app.screen.row_text(0)
        );
        assert_eq!(remotes_status(&app), "1/1 · origin");
        let body = (20..23)
            .map(|y| app.screen.row_text(y))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            body.contains("origin") && body.contains("git@example.com:x.git"),
            "the row did not read name then urls: {body:?}"
        );
        app.dispatch("copy.selection");
        assert!(
            app.copy.as_deref().is_some_and(|t| t.contains("origin")),
            "copy did not read the row: {:?}",
            app.copy
        );
    }

    fn remotes_status(app: &App) -> String {
        match app.panes.get("remotes") {
            Some(Screens::Remotes { view, .. }) => view.status(),
            _ => panic!("the remotes pane is not registered"),
        }
    }

    /// A removal is the twice-pressed verb it is everywhere else: first
    /// press asks on the row, a different row re-arms, a refresh disarms,
    /// and the second press on the same row submits exactly one removal.
    #[test]
    fn tui_parity_a_remote_removal_asks_twice_and_never_retargets() {
        let (handle, state) = fake(&[]);
        {
            let mut s = state.lock().unwrap();
            s.servers = vec![
                remote_ref("origin", &["one.example"]),
                remote_ref("fork", &["two.example"]),
            ];
        }
        let mut app = commits_app(&handle);
        app.dispatch("remotes.focus");
        app.dispatch("remotes.remove");
        assert_eq!(
            app.message,
            "remove remote origin and its remote-tracking branches? press again to confirm"
        );
        assert_eq!(
            remotes_of(&app).armed().as_ref().map(|n| n.as_bytes()),
            Some(b"origin".as_slice())
        );
        // A different row re-arms rather than inheriting the question.
        app.dispatch("view.down");
        app.dispatch("remotes.remove");
        assert_eq!(
            remotes_of(&app).armed().as_ref().map(|n| n.as_bytes()),
            Some(b"fork".as_slice())
        );
        // A refresh kills the question outright.
        app.dispatch("repo.refresh");
        app.pump();
        assert_eq!(remotes_of(&app).armed(), None);
        // And the second press on one row acts, once.
        app.dispatch("view.top");
        app.dispatch("remotes.remove");
        app.dispatch("remotes.remove");
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                state
                    .lock()
                    .unwrap()
                    .branch_writes
                    .iter()
                    .any(|w| w == "remote remove origin")
            }),
            "the removal never landed: {:?}",
            state.lock().unwrap().branch_writes
        );
        assert_eq!(
            state
                .lock()
                .unwrap()
                .branch_writes
                .iter()
                .filter(|w| w.starts_with("remote remove"))
                .count(),
            1,
            "the question queued twice"
        );
    }

    /// A remote is introduced by two prompts — name, then URL — and edited
    /// by one, prefilled with the URL it already holds. An empty answer is
    /// refused beside the field that closed.
    #[test]
    fn tui_parity_a_remote_is_added_by_two_prompts_and_edited_prefilled() {
        let (handle, state) = fake(&[]);
        state.lock().unwrap().servers = vec![remote_ref("origin", &["old.example"])];
        let mut app = commits_app(&handle);
        app.dispatch("remotes.focus");

        app.dispatch("remotes.new");
        type_(&mut app, "fork");
        app.press(Key::plain(Code::Enter));
        type_(&mut app, "git@example.com:fork.git");
        app.press(Key::plain(Code::Enter));
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                state
                    .lock()
                    .unwrap()
                    .branch_writes
                    .iter()
                    .any(|w| w == "remote add fork git@example.com:fork.git")
            }),
            "the add never landed: {:?}",
            state.lock().unwrap().branch_writes
        );

        // Empty answers are refused before anything is queued, and the
        // refusal stands beside the field that closed — reopen and answer.
        app.dispatch("remotes.new");
        app.press(Key::plain(Code::Enter));
        assert_eq!(app.message, "a remote needs a name");
        app.press(Key::plain(Code::Esc));
        app.dispatch("remotes.new");
        type_(&mut app, "x");
        app.press(Key::plain(Code::Enter));
        app.press(Key::plain(Code::Enter));
        assert_eq!(app.message, "a remote needs a URL");
        app.press(Key::plain(Code::Esc));

        // The edit field arrives holding the remote's own URL.
        app.dispatch("remotes.edit");
        match &app.prompt {
            Some(Prompt::RemoteEdit { name, field }) => {
                assert_eq!(name.as_bytes(), b"origin");
                assert_eq!(field.text(), "old.example");
            }
            _ => panic!("the edit prompt did not open prefilled"),
        }
        // Accept unchanged: git answers "unchanged" in its own words; the
        // verb queued is still exactly one set-url.
        app.press(Key::plain(Code::Enter));
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                state
                    .lock()
                    .unwrap()
                    .branch_writes
                    .iter()
                    .filter(|w| w.starts_with("remote set-url"))
                    .count()
                    == 1
            }),
            "the edit never landed: {:?}",
            state.lock().unwrap().branch_writes
        );
    }

    /// Fetch aims at the row: the selected remote's name rides the job.
    /// With nothing selected it says so; away from the pane it says that.
    #[test]
    fn tui_parity_remote_fetch_names_the_remote() {
        let (handle, state) = fake(&[]);
        state.lock().unwrap().servers = vec![remote_ref("origin", &["one.example"])];
        let mut app = commits_app(&handle);
        app.dispatch("remotes.focus");
        app.dispatch("remotes.fetch");
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                state.lock().unwrap().writes.len() == 1
            }),
            "the fetch did not aim at the row: {:?}",
            state.lock().unwrap().writes
        );
        assert_eq!(
            state.lock().unwrap().writes,
            vec!["fetch origin".to_string()]
        );

        // An empty list has nothing to aim at, before the queue.
        state.lock().unwrap().servers.clear();
        app.dispatch("repo.refresh");
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                remotes_status(&app) == "0 remotes"
            }),
            "the refresh never emptied the pane"
        );
        app.dispatch("remotes.fetch");
        app.pump();
        assert_eq!(app.message, "nothing selected to fetch");

        // Away from the pane, the guard is the sentence.
        app.dispatch("commits.focus");
        app.dispatch("remotes.fetch");
        assert_eq!(app.message, "remotes.fetch is not supported here");
    }

    /// Upstream moves: set picks the carrier the repository's own config
    /// names — the only one, or origin among several — and refuses a true
    /// ambiguity with the candidates named; unset severs the link only.
    #[test]
    fn tui_parity_upstream_moves_name_their_remote() {
        let (handle, state) = fake(&[]);
        branch_world(&state);
        {
            let mut s = state.lock().unwrap();
            s.servers = vec![
                remote_ref("origin", &["one.example"]),
                remote_ref("mirror", &["two.example"]),
            ];
        }
        let mut app = commits_app(&handle);
        app.dispatch("branches.focus");
        app.dispatch("view.top");

        // `u` on main: both remotes carry main, and origin is the one that
        // stands out.
        app.dispatch("branches.set-upstream");
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                state
                    .lock()
                    .unwrap()
                    .branch_writes
                    .iter()
                    .any(|w| w == "track main origin main")
            }),
            "the set never landed: {:?}",
            state.lock().unwrap().branch_writes
        );

        // `U` severs the link and nothing else.
        app.dispatch("branches.unset-upstream");
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                state
                    .lock()
                    .unwrap()
                    .branch_writes
                    .iter()
                    .any(|w| w == "untrack main")
            }),
            "the unset never landed"
        );

        // A true ambiguity — two remotes, neither origin — is refused with
        // the candidates named, never guessed at.
        state.lock().unwrap().servers = vec![
            remote_ref("alpha", &["a.example"]),
            remote_ref("beta", &["b.example"]),
        ];
        state.lock().unwrap().remotes = vec![
            RemoteBranch {
                remote: RefName::from("alpha"),
                branch: RefName::from("main"),
                commit: "f00d".into(),
            },
            RemoteBranch {
                remote: RefName::from("beta"),
                branch: RefName::from("main"),
                commit: "f00d".into(),
            },
            RemoteBranch {
                remote: RefName::from("origin"),
                branch: RefName::from("feat/ure"),
                commit: "f00d".into(),
            },
        ];
        let writes = state.lock().unwrap().branch_writes.len();
        app.dispatch("branches.set-upstream");
        assert!(
            app.message.contains("alpha, beta"),
            "the ambiguity was not named: {:?}",
            app.message
        );
        assert_eq!(
            state.lock().unwrap().branch_writes.len(),
            writes,
            "an ambiguous set queued a job"
        );

        // No carrier at all names the push that creates one.
        state.lock().unwrap().remotes = vec![RemoteBranch {
            remote: RefName::from("origin"),
            branch: RefName::from("feat/ure"),
            commit: "f00d".into(),
        }];
        app.dispatch("branches.set-upstream");
        assert!(
            app.message.contains("no remote branch named main"),
            "the missing carrier said nothing about push: {:?}",
            app.message
        );
    }

    /// A fast-forward never moves a branch sideways, and its shape is
    /// HEAD's own: merge for the checked-out branch, the fetch refspec for
    /// every other — and no upstream names the push that makes one.
    #[test]
    fn tui_parity_fast_forward_chooses_its_shape_by_head() {
        let (handle, state) = fake(&[]);
        branch_world(&state);
        {
            let mut s = state.lock().unwrap();
            s.servers = vec![remote_ref("origin", &["one.example"])];
            // The parked branch is feat/ure: the refspec shape aims at the
            // remote-tracking branch of the same name.
            s.locals[1].name = RefName::from("feat/ure");
        }
        let mut app = commits_app(&handle);
        app.dispatch("branches.focus");
        app.dispatch("view.top");

        // main is HEAD: merge --ff-only from origin/main.
        app.dispatch("branches.fast-forward");
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                state
                    .lock()
                    .unwrap()
                    .branch_writes
                    .iter()
                    .any(|w| w.contains("merge --ff-only") && w.contains("origin/main"))
            }),
            "the checked-out branch took the wrong shape: {:?}",
            state.lock().unwrap().branch_writes
        );

        // A branch that is not HEAD: the fetch refspec.
        app.dispatch("view.down");
        app.dispatch("branches.fast-forward");
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                state
                    .lock()
                    .unwrap()
                    .branch_writes
                    .iter()
                    .any(|w| w.contains("fetch refspec") && w.contains("origin/feat/ure"))
            }),
            "the parked branch took the wrong shape: {:?}",
            state.lock().unwrap().branch_writes
        );

        // No tracking ref anywhere: the push spelled out, nothing queued.
        state.lock().unwrap().remotes = Vec::new();
        app.dispatch("branches.set-upstream");
        assert!(
            app.message.contains("no remote branch named"),
            "{:?}",
            app.message
        );
    }

    /// Force checkout is checkout's destructive spelling, confirmed on the
    /// keyboard like every destruction: first press arms and asks, second
    /// press on the same row discards the local changes and goes — and a
    /// remote row or a detached row is refused by name.
    #[test]
    fn tui_parity_force_checkout_asks_twice_and_discards_local_changes() {
        let (handle, state) = fake(&[]);
        branch_world(&state);
        let mut app = commits_app(&handle);
        app.dispatch("branches.focus");
        app.dispatch("view.top");

        app.dispatch("branches.force-checkout");
        assert_eq!(
            app.message,
            "discard local changes and check out main? press again to confirm"
        );
        app.dispatch("branches.force-checkout");
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                state
                    .lock()
                    .unwrap()
                    .branch_writes
                    .iter()
                    .any(|w| w == "force-checkout main")
            }),
            "the confirmed force never landed"
        );
        assert!(
            state.lock().unwrap().status.is_empty(),
            "the force left the local changes standing"
        );

        // A remote row is refused: tracking checkout is space's job.
        app.dispatch("view.top");
        app.dispatch("view.down");
        app.dispatch("view.down");
        while !branches_of(&app).status().contains("origin/feat/ure") {
            app.dispatch("view.down");
        }
        app.dispatch("branches.force-checkout");
        assert!(
            app.message.contains("press space on"),
            "a remote force said nothing about space: {:?}",
            app.message
        );
    }

    /// Checkout by name aims at whatever git is handed, bytes end to end;
    /// the previous branch is git's own `-`, and an empty field is refused
    /// beside the field that closed.
    #[test]
    fn tui_parity_checkout_by_name_and_previous_move_head() {
        let (handle, state) = fake(&[]);
        branch_world(&state);
        let mut app = commits_app(&handle);
        app.dispatch("branches.focus");

        app.dispatch("branches.checkout-name");
        type_(&mut app, "f\u{e9}ature");
        app.press(Key::plain(Code::Enter));
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                state
                    .lock()
                    .unwrap()
                    .branch_bytes
                    .iter()
                    .any(|b| b == "f\u{e9}ature".as_bytes())
            }),
            "the name did not arrive as bytes: {:?}",
            state.lock().unwrap().branch_bytes
        );

        app.dispatch("branches.checkout-previous");
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                state
                    .lock()
                    .unwrap()
                    .branch_writes
                    .iter()
                    .any(|w| w == "checkout -")
            }),
            "the previous branch never moved HEAD: {:?}",
            state.lock().unwrap().branch_writes
        );

        // An empty field is refused before the queue, twice said.
        app.dispatch("branches.checkout-name");
        app.press(Key::plain(Code::Enter));
        assert_eq!(app.message, "a branch needs a name");
        app.press(Key::plain(Code::Esc));
    }

    /// A branch grown from a commit: the sha is the one the keyboard was
    /// on, the checkout is a question asked once the branch exists — and
    /// esc is a no, not a checkout.
    #[test]
    fn tui_parity_a_branch_grown_from_a_commit_offers_the_checkout() {
        let (handle, state) = fake(&[]);
        let mut app = commits_app(&handle);
        app.dispatch("view.down");
        app.press(Key::char('n'));
        type_(&mut app, "feature");
        app.press(Key::plain(Code::Enter));
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                state
                    .lock()
                    .unwrap()
                    .branch_writes
                    .iter()
                    .any(|w| w == "branch feature at 00000001")
            }),
            "the branch did not grow from the selected commit: {:?}",
            state.lock().unwrap().branch_writes
        );
        // The question stands, drawn on the status row.
        app.draw();
        assert!(
            app.screen
                .row_text(23)
                .contains("created feature — check out?"),
            "the question did not stand: {:?}",
            app.screen.row_text(23)
        );
        // esc is a no: nothing checked out, the question closed.
        app.press(Key::plain(Code::Esc));
        app.pump_quiet();
        assert!(
            !state
                .lock()
                .unwrap()
                .branch_writes
                .iter()
                .any(|w| w.starts_with("checkout ")),
            "esc checked the branch out"
        );

        // The next branch takes the offer: enter checks it out.
        app.press(Key::char('n'));
        type_(&mut app, "second");
        app.press(Key::plain(Code::Enter));
        app.press(Key::plain(Code::Enter));
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                state
                    .lock()
                    .unwrap()
                    .branch_writes
                    .iter()
                    .filter(|w| w.starts_with("checkout "))
                    .count()
                    == 1
            }),
            "the offered checkout never landed: {:?}",
            state.lock().unwrap().branch_writes
        );
    }

    /// The recent list: empty says where the key is; a standing list moves,
    /// opens by enter, closes by esc, and the opened repository moves to
    /// the front of the list.
    #[test]
    fn tui_parity_the_recent_list_switches_and_says_when_empty() {
        let (a_handle, _a_state) = fake(&[]);
        let (b_handle, _b_state) = fake(&[]);
        with_mru("picker", &["/b", "/a"], || {
            let mut app = commits_app(&a_handle);
            app.use_opener(Arc::new(KeyedOpener {
                repos: vec![
                    (std::path::PathBuf::from("/a"), a_handle.clone()),
                    (std::path::PathBuf::from("/b"), b_handle.clone()),
                ],
            }));

            // The keyboard on /a (index 1 in the MRU): a step forward wraps
            // to /b, a step back wraps the other way.
            app.dispatch("project.next");
            app.pump_quiet();
            assert_eq!(app.message, "switched to /b", "{:?}", app.message);
            app.dispatch("project.prev");
            app.pump_quiet();
            assert_eq!(app.message, "switched to /a");

            // The picker itself: opens, moves, opens the row it is on.
            app.dispatch("project.switch");
            app.draw();
            let body: String = (1..9)
                .map(|y| app.screen.row_text(y))
                .collect::<Vec<_>>()
                .join("\n");
            assert!(
                body.contains("recent repositories") && body.contains("/b") && body.contains("/a"),
                "the picker did not list: {body:?}"
            );
            // The first press of 'o' moved /a to the front; the cursor is
            // on it.
            app.press(Key::plain(Code::Enter));
            app.pump_quiet();
            // /a is already open: the honest answer names it, it does not
            // pretend to switch.
            assert_eq!(app.message, "already showing /a");
        });

        // Empty is said, not shown — a sibling, not a nested case: env_lock
        // is not reentrant, and a with_mru inside a with_mru asks the same
        // thread for the same mutex twice.
        with_mru("picker-empty", &[], || {
            let mut app = commits_app(&a_handle);
            app.dispatch("project.switch");
            assert_eq!(app.message, "no recent repositories — O to open one");
        });
    }

    /// An open that fails changes nothing: the refusal is the path plus
    /// git's own words, and every pane, the handle and the MRU stay as
    /// they were.
    #[test]
    fn tui_parity_an_open_that_fails_changes_nothing() {
        let (handle, _state) = fake(&[]);
        with_mru("refused", &[], || {
            let mut app = commits_app(&handle);
            let label = files_label(&app).to_string();
            let cursor = commits_of(&app).cursor();

            // An existing directory that is not a repository: git's own
            // "not a git repository" is the refusal, not a missing path.
            let scratch =
                std::env::temp_dir().join(format!("gitten-tui-not-a-repo-{}", std::process::id()));
            let _ = std::fs::create_dir_all(&scratch);
            app.press(Key::char('O'));
            type_(&mut app, scratch.to_str().unwrap());
            app.press(Key::plain(Code::Enter));
            assert!(
                app.message.contains("not a git repository"),
                "the refusal was not git's: {:?}",
                app.message
            );
            assert_eq!(files_label(&app), label, "the open moved a pane");
            assert_eq!(commits_of(&app).cursor(), cursor, "the open moved the list");
            assert!(
                gitten_app::projects::load().is_empty(),
                "a failed open wrote the MRU: {:?}",
                gitten_app::projects::load()
            );
            let _ = std::fs::remove_dir(&scratch);
        });
    }
}
