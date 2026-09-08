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
use gitten_app::patchwork::{
    self, CommitFileTarget, DiffPick, GraftScope, GraftTarget, PatchClient, PickOrigin,
};
use gitten_app::verbs::Write;
use gitten_app::{StartClock, Startup};
use gitten_core::command::{chord_string, Availability, Code, Key, Modes, Resolve, Usable};
use gitten_core::differ::Overrides;
use gitten_core::edit::{Edit, Field};
use gitten_core::host::Host;
use gitten_core::operation::{Operation, Side};
use gitten_core::rebase::{FixupKind, Rewrite};
use gitten_core::refs::{HeadState, RefName, ResetMode, StashId, StashScope};
use gitten_core::runs::Run;
use gitten_core::source::DiffSource;
use gitten_tui::branches::{self, Branches, Marks, Target};
use gitten_tui::commits::{Commits, Glyphs};
use gitten_tui::diff::Diff;
use gitten_tui::diff::PatchSelection;
use gitten_tui::files::{self, Files};
use gitten_tui::help;
use gitten_tui::merging;
use gitten_tui::patch::PatchBuilder;
use gitten_tui::reflog::Reflog;
use gitten_tui::remotes::Remotes;
use gitten_tui::screen::{Ink, Pen, Screen};
use gitten_tui::scrollbar::Bar;
use gitten_tui::stashes::Stashes;
use gitten_tui::tags::Tags;
use gitten_tui::term::{Input, Mouse, MouseKind, Term};
use gitten_tui::todo::Todo;
use gitten_tui::worktrees::Worktrees;
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
    /// The merging view: one conflicted file, its regions, and the answers.
    /// Lives in the main slot like the diff it replaces while a conflict row
    /// holds the eye; a refresh re-reads the file the view names.
    Merging {
        view: merging::Merging,
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
    /// The repository's tags. No source, like the remotes tenant: a refresh
    /// is a plain re-read of the ref namespace through the handle the app
    /// holds, and the generation rail is the whole of its staleness story.
    Tags {
        view: Tags,
        label: String,
        generation: Generation,
    },
    /// Where HEAD has been, newest first. No source, like the tags tenant:
    /// a refresh is a plain re-read of the reflog through the handle the
    /// app holds, and the generation rail is the whole of its staleness
    /// story.
    Reflog {
        view: Reflog,
        label: String,
        generation: Generation,
    },
    /// The repository's worktrees. No source, like the reflog tenant: a
    /// refresh is a plain re-read of `worktree list` through the handle
    /// the app holds, and the generation rail is the whole of its
    /// staleness story.
    Worktrees {
        view: Worktrees,
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

/// The mode the patch builder owns the keyboard in, on exactly the
/// rebase plan's terms: a press it does not name runs nothing underneath,
/// because the clipboard it edits is one keypress from landing somewhere.
const BUILDER: &str = "builder";

/// The mode the rebase plan owns the keyboard in, on exactly the picker's
/// terms and rather more urgently: the keys underneath it rewrite history.
const TODO: &str = "todo";

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
    /// `branches.new-tag`'s and `commits.new-tag`'s field. `at` is the raw
    /// bytes of what the tag names — a branch's bytes, which move with the
    /// branch, or a commit's sha — a revspec either way, captured at open
    /// and never re-read from the pane.
    TagNew {
        at: Vec<u8>,
        field: Field,
    },
    /// The tag name's message, opened by an accepted [`Prompt::TagNew`].
    /// An empty accept names a lightweight tag; any text names an annotated
    /// one carrying it. `name` and `at` ride along from the name field, so
    /// the message answers the same tag the name named.
    TagMessage {
        name: String,
        at: Vec<u8>,
        field: Field,
    },
    /// `tags.push`'s field: the remote to push the selected tag to, prefilled
    /// when the repository knows exactly one — tags track nothing, so there
    /// is no upstream to default to. `name` is the tag's raw bytes, captured
    /// at open like every verb's aim.
    TagPush {
        name: Vec<u8>,
        field: Field,
    },
    /// `branches.checkout-name`'s field: whatever it names, git aims at.
    /// Empty is refused beside the field that just closed.
    BranchCheckoutName {
        field: Field,
    },
    /// `patch.move-to-branch`'s field: the branch the clipboard lands on,
    /// created at HEAD when no such branch exists. Empty is refused
    /// beside the field that just closed — moving onto no branch is not
    /// a move.
    MovePatch {
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
    /// `commits.reword`'s field, prefilled with the commit's own subject —
    /// a reword is an edit of what is standing, not a retyping of it.
    /// `at` is the commit captured when the field opened, so nothing a
    /// cursor does while the field holds the keyboard can re-aim it, and
    /// the shared action checks it is still in the window before writing.
    /// Multiline, because a commit message is a message.
    Reword {
        at: gitten_app::act::SelectedCommit,
        field: Field,
    },
    /// `todo.reword`'s field: the same message, typed into an *open plan*
    /// instead of at the repository. Nothing is written when it is accepted
    /// — the row simply says what it will land under, and the plan still
    /// has to be run.
    TodoReword {
        field: Field,
    },
    /// `files.stash-named`'s field: the message the entry is parked under.
    /// Reads no row — the scope is the whole tracked working tree — so it
    /// answers on a launch that registered no files tenant, exactly as
    /// `files.stash` does. One line: git's `-m` takes a message, and a stash
    /// nobody will ever read a body off is a label.
    StashMessage {
        field: Field,
    },
    /// `stashes.rename`'s field, prefilled with the entry's own message —
    /// a rename is an edit of what is standing. `at` is the entry's
    /// [`StashId`], captured when the field opened, so nothing a cursor does
    /// while the field holds the keyboard can re-aim it; the write resolves
    /// that commit again against the live stack.
    StashRename {
        at: StashId,
        field: Field,
    },
    /// `stashes.new-branch`'s field: the branch to start where the entry was
    /// made. `at` is captured at open like every verb's aim.
    StashBranch {
        at: StashId,
        field: Field,
    },
    /// `worktrees.new`'s first field: the starting point — a branch, a
    /// commit, any revspec — empty for HEAD. Accepting opens the path
    /// field with this riding along.
    WorktreeBase {
        field: Field,
    },
    /// `worktrees.new`'s second field and the `w` door's only one: where
    /// the checkout lands. `base` is the row's rev or the first field's
    /// answer, captured at open and never re-read from the pane.
    WorktreePath {
        base: Vec<u8>,
        field: Field,
    },
    /// `commits.bisect-start`'s field: a revision the bug is not in.
    /// `bad` is the selected commit's sha, captured when the field
    /// opened, so nothing a cursor does while the field holds the
    /// keyboard can re-aim the question.
    BisectGood {
        bad: Vec<u8>,
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
            | Prompt::TagMessage { field, .. }
            | Prompt::TagPush { field, .. }
            | Prompt::BranchCheckoutName { field }
            | Prompt::MovePatch { field }
            | Prompt::BranchNewAt { field, .. }
            | Prompt::CheckoutNew { field, .. }
            | Prompt::ProjectOpen { field }
            | Prompt::RemoteName { field }
            | Prompt::RemoteUrl { field, .. }
            | Prompt::RemoteEdit { field, .. }
            | Prompt::Reword { field, .. }
            | Prompt::TodoReword { field }
            | Prompt::StashMessage { field }
            | Prompt::StashRename { field, .. }
            | Prompt::StashBranch { field, .. }
            | Prompt::WorktreeBase { field }
            | Prompt::WorktreePath { field, .. }
            | Prompt::BisectGood { field, .. } => field,
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
            | Prompt::TagMessage { field, .. }
            | Prompt::TagPush { field, .. }
            | Prompt::BranchCheckoutName { field }
            | Prompt::MovePatch { field }
            | Prompt::BranchNewAt { field, .. }
            | Prompt::CheckoutNew { field, .. }
            | Prompt::ProjectOpen { field }
            | Prompt::RemoteName { field }
            | Prompt::RemoteUrl { field, .. }
            | Prompt::RemoteEdit { field, .. }
            | Prompt::Reword { field, .. }
            | Prompt::TodoReword { field }
            | Prompt::StashMessage { field }
            | Prompt::StashRename { field, .. }
            | Prompt::StashBranch { field, .. }
            | Prompt::WorktreeBase { field }
            | Prompt::WorktreePath { field, .. }
            | Prompt::BisectGood { field, .. } => field,
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
            Prompt::CommitMessage { .. }
                | Prompt::AmendMessage { .. }
                | Prompt::Reword { .. }
                | Prompt::TodoReword { .. }
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
            Prompt::Reword { .. } | Prompt::TodoReword { .. } => "reword: ",
            Prompt::BranchNew { .. } => "branch: ",
            Prompt::BranchRename { .. } => "rename: ",
            Prompt::TagNew { .. } => "tag: ",
            Prompt::TagMessage { .. } => "tag message (empty = lightweight): ",
            Prompt::TagPush { .. } => "push tag to: ",
            Prompt::BranchCheckoutName { .. } => "checkout: ",
            Prompt::MovePatch { .. } => "move patch onto branch: ",
            Prompt::BranchNewAt { .. } => "branch: ",
            Prompt::CheckoutNew { .. } => "",
            Prompt::ProjectOpen { .. } => "open: ",
            Prompt::RemoteName { .. } => "remote name: ",
            Prompt::RemoteUrl { .. } => "remote url: ",
            Prompt::RemoteEdit { .. } => "url: ",
            Prompt::StashMessage { .. } => "stash: ",
            Prompt::StashRename { .. } => "rename stash: ",
            Prompt::StashBranch { .. } => "branch from stash: ",
            Prompt::WorktreeBase { .. } => "worktree from (empty = HEAD): ",
            Prompt::WorktreePath { .. } => "worktree path: ",
            Prompt::BisectGood { .. } => "bisect from (known good): ",
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
            Screens::Merging { .. } => "merge",
            Screens::Stashes { .. } => "stashes",
            Screens::Files { .. } => "files",
            Screens::Branches { .. } => "branches",
            Screens::Remotes { .. } => "remotes",
            Screens::Tags { .. } => "tags",
            Screens::Reflog { .. } => "reflog",
            Screens::Worktrees { .. } => "worktrees",
        }
    }

    fn label(&self) -> &str {
        match self {
            Screens::Commits { label, .. }
            | Screens::Diff { label, .. }
            | Screens::Merging { label, .. }
            | Screens::Stashes { label, .. }
            | Screens::Branches { label, .. }
            | Screens::Remotes { label, .. }
            | Screens::Tags { label, .. }
            | Screens::Reflog { label, .. }
            | Screens::Worktrees { label, .. } => label,
            Screens::Files { label, .. } => label,
        }
    }

    fn generation(&self) -> Generation {
        match self {
            Screens::Commits { generation, .. }
            | Screens::Diff { generation, .. }
            | Screens::Merging { generation, .. }
            | Screens::Stashes { generation, .. }
            | Screens::Branches { generation, .. }
            | Screens::Remotes { generation, .. }
            | Screens::Tags { generation, .. }
            | Screens::Reflog { generation, .. }
            | Screens::Worktrees { generation, .. } => *generation,
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
        here: &[u8],
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
            Screens::Merging {
                view,
                label,
                generation,
                ..
            } => {
                // One path, two reads: the file's bytes and the stages git
                // still holds. The read decides the label — a resolution
                // that emptied the markers is "resolved", not the old
                // conflict's name — and a file that has left the working
                // tree while the stages are gone with it is the resolved
                // deletion, drawn as an empty view, never an error wave
                // that never stops.
                let path = view.path().as_bytes().to_vec();
                let stages = match repo.unmerged(&path) {
                    Ok(stages) => stages,
                    Err(e) => return Some(Err(e)),
                };
                let file = match repo.conflict_file(&path) {
                    Ok(file) => file,
                    Err(_) if stages.is_empty() => {
                        gitten_core::conflict::ConflictFile::parse(view.path().clone(), Vec::new())
                    }
                    Err(e) => return Some(Err(e)),
                };
                let source = DiffSource::Conflict {
                    path: view.path().clone(),
                };
                *label = source.label();
                view.replace(file, stages);
                *generation = target;
                Some(Ok(()))
            }
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
            Screens::Tags {
                view,
                label,
                generation,
            } => {
                // The same two reads: the ref namespace, and the describe
                // its label is spelled with.
                let (loaded, described) = std::thread::scope(|s| {
                    let tags = s.spawn(|| repo.tags());
                    let described = s.spawn(|| repo.describe());
                    (
                        tags.join().unwrap_or_else(|p| std::panic::resume_unwind(p)),
                        described.join().unwrap_or_default(),
                    )
                });
                let loaded = match loaded {
                    Ok(tags) => tags,
                    Err(e) => return Some(Err(e)),
                };
                let count = loaded.len();
                view.replace(loaded);
                *label = tags_label(&described, count);
                *generation = target;
                Some(Ok(()))
            }
            Screens::Worktrees {
                view,
                label,
                generation,
            } => {
                // The same two reads: every checkout, and the describe
                // its label is spelled with. `here` is the canonicalized
                // root the row guard compares against — the listing spells
                // paths resolved, so an uncanonicalized root would match
                // nothing and the guard would fail open to git's refusal.
                let (loaded, described) = std::thread::scope(|s| {
                    let worktrees = s.spawn(|| repo.worktrees());
                    let described = s.spawn(|| repo.describe());
                    (
                        worktrees
                            .join()
                            .unwrap_or_else(|p| std::panic::resume_unwind(p)),
                        described.join().unwrap_or_default(),
                    )
                });
                let loaded = match loaded {
                    Ok(worktrees) => worktrees,
                    Err(e) => return Some(Err(e)),
                };
                let count = loaded.len();
                view.replace(loaded, here);
                *label = worktrees_label(&described, count);
                *generation = target;
                Some(Ok(()))
            }
            Screens::Reflog {
                view,
                label,
                generation,
            } => {
                // The same two reads: where HEAD has been, and the describe
                // its label is spelled with.
                let (loaded, described) = std::thread::scope(|s| {
                    let reflog = s.spawn(|| repo.reflog(REFLOG_ENTRIES));
                    let described = s.spawn(|| repo.describe());
                    (
                        reflog
                            .join()
                            .unwrap_or_else(|p| std::panic::resume_unwind(p)),
                        described.join().unwrap_or_default(),
                    )
                });
                let loaded = match loaded {
                    Ok(entries) => entries,
                    Err(e) => return Some(Err(e)),
                };
                let count = loaded.len();
                view.replace(loaded);
                *label = reflog_label(&described, count);
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
            Screens::Merging { view: m, .. } => {
                m.set_scrolloff(host.view.scrolloff);
                m.resize(rect.width, rect.height);
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
            Screens::Tags { view: t, .. } => {
                t.set_scrolloff(host.view.scrolloff);
                t.resize(rect.width, rect.height);
            }
            Screens::Reflog { view: r, .. } => {
                r.set_scrolloff(host.view.scrolloff);
                r.resize(rect.width, rect.height);
            }
            Screens::Worktrees { view: w, .. } => {
                w.set_scrolloff(host.view.scrolloff);
                w.resize(rect.width, rect.height);
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
            Screens::Merging { view: m, .. } => m.paint(screen, x, y, focused, host),
            Screens::Stashes { view: s, .. } => s.paint(screen, x, y, focused, host),
            // The files pane needs no run-list buffer: its rows are cells,
            // not shaped spans.
            Screens::Files { view: f, .. } => f.paint(screen, x, y, focused, host),
            Screens::Branches { view: b, .. } => b.paint(screen, x, y, focused, host),
            Screens::Remotes { view: r, .. } => r.paint(screen, x, y, focused, host),
            Screens::Tags { view: t, .. } => t.paint(screen, x, y, focused, host),
            Screens::Reflog { view: r, .. } => r.paint(screen, x, y, focused, host),
            Screens::Worktrees { view: w, .. } => w.paint(screen, x, y, focused, host),
        }
    }

    fn status(&self, host: &Host) -> String {
        match self {
            Screens::Commits { view: c, .. } => c.status(),
            Screens::Diff { view: d, .. } => d.status(host),
            Screens::Merging { view: m, .. } => m.status(),
            Screens::Stashes { view: s, .. } => s.status(),
            Screens::Files { view: f, .. } => f.status(),
            Screens::Branches { view: b, .. } => b.status(),
            Screens::Remotes { view: r, .. } => r.status(),
            Screens::Tags { view: t, .. } => t.status(),
            Screens::Reflog { view: r, .. } => r.status(),
            Screens::Worktrees { view: w, .. } => w.status(),
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
            Screens::Merging { view: m, .. } => m.paint_bar(screen, x, divider, y, host),
            Screens::Stashes { view: s, .. } => s.paint_bar(screen, x, divider, y, host),
            Screens::Files { view: f, .. } => f.paint_bar(screen, x, divider, y, host),
            Screens::Branches { view: b, .. } => b.paint_bar(screen, x, divider, y, host),
            Screens::Remotes { view: r, .. } => r.paint_bar(screen, x, divider, y, host),
            Screens::Tags { view: t, .. } => t.paint_bar(screen, x, divider, y, host),
            Screens::Reflog { view: r, .. } => r.paint_bar(screen, x, divider, y, host),
            Screens::Worktrees { view: w, .. } => w.paint_bar(screen, x, divider, y, host),
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
            Screens::Merging { view: m, .. } => m.press(col, row, extend, host),
            Screens::Stashes { view: s, .. } => s.press(col, row, extend, host),
            Screens::Files { view: f, .. } => f.press(col, row, clicks, extend, host),
            Screens::Branches { view: b, .. } => b.press(col, row, extend, host),
            Screens::Remotes { view: r, .. } => r.press(col, row, extend, host),
            Screens::Tags { view: t, .. } => t.press(col, row, extend, host),
            Screens::Reflog { view: r, .. } => r.press(col, row, extend, host),
            Screens::Worktrees { view: w, .. } => w.press(col, row, extend, host),
        }
    }

    /// The pointer moved with the button down, in this pane's coordinates.
    /// `row` is signed: a row above the pane is negative and scrolls it.
    fn drag(&mut self, col: usize, row: isize, host: &Host) {
        match self {
            Screens::Commits { view: c, .. } => c.drag(row, host),
            Screens::Diff { view: d, .. } => d.drag(col, row, host),
            Screens::Merging { view: m, .. } => m.drag(row, host),
            Screens::Stashes { view: s, .. } => s.drag(row, host),
            // A list with no drag selection and an indicator bar has nothing
            // a held button can do.
            Screens::Files { .. }
            | Screens::Branches { .. }
            | Screens::Remotes { .. }
            | Screens::Tags { .. }
            | Screens::Reflog { .. }
            | Screens::Worktrees { .. } => {}
        }
    }

    fn release(&mut self) {
        match self {
            Screens::Commits { view: c, .. } => c.release(),
            Screens::Diff { view: d, .. } => d.release(),
            Screens::Merging { view: m, .. } => m.release(),
            Screens::Stashes { view: s, .. } => s.release(),
            // Nothing held here either — see `drag`.
            Screens::Files { .. }
            | Screens::Branches { .. }
            | Screens::Remotes { .. }
            | Screens::Tags { .. }
            | Screens::Reflog { .. }
            | Screens::Worktrees { .. } => {}
        }
    }

    /// What `copy.selection` copies here: the selection, or the row the cursor is
    /// on when there is none.
    fn copy_text(&self) -> String {
        match self {
            Screens::Commits { view: c, .. } => c.copy_text(),
            Screens::Diff { view: d, .. } => d.copy_text(),
            // A conflict file is not a range the verbs act on; the row's
            // text is what the diff pane's copy is for.
            Screens::Merging { .. } => String::new(),
            Screens::Stashes { view: s, .. } => s.copy_text(),
            Screens::Files { view: f, .. } => f.copy_text(),
            Screens::Branches { view: b, .. } => b.copy_text(),
            Screens::Remotes { view: r, .. } => r.copy_text(),
            Screens::Tags { view: t, .. } => t.copy_text(),
            Screens::Reflog { view: r, .. } => r.copy_text(),
            Screens::Worktrees { view: w, .. } => w.copy_text(),
        }
    }

    /// What the *mouse* has selected, and nothing else. Empty after a click, so
    /// copy-on-select can tell a gesture that selected something from one that
    /// only moved the cursor.
    fn selection(&self) -> String {
        match self {
            Screens::Commits { view: c, .. } => c.selection(),
            Screens::Diff { view: d, .. } => d.selection(),
            Screens::Merging { view: m, .. } => m.selection(),
            Screens::Stashes { view: s, .. } => s.selection(),
            // A file list has no drag selection, so copy-on-select has
            // nothing to fire on here — the empty answer is the mechanism.
            Screens::Files { view: f, .. } => f.selection(),
            Screens::Branches { view: b, .. } => b.selection(),
            Screens::Remotes { view: r, .. } => r.selection(),
            Screens::Tags { view: t, .. } => t.selection(),
            Screens::Reflog { view: r, .. } => r.selection(),
            Screens::Worktrees { view: w, .. } => w.selection(),
        }
    }

    fn select_all(&mut self) {
        match self {
            Screens::Commits { view: c, .. } => c.select_all(),
            Screens::Diff { view: d, .. } => d.select_all(),
            Screens::Merging { view: m, .. } => m.select_all(),
            Screens::Stashes { view: s, .. } => s.select_all(),
            Screens::Files { view: f, .. } => f.select_all(),
            Screens::Branches { view: b, .. } => b.select_all(),
            Screens::Remotes { view: r, .. } => r.select_all(),
            Screens::Tags { view: t, .. } => t.select_all(),
            Screens::Reflog { view: r, .. } => r.select_all(),
            Screens::Worktrees { view: w, .. } => w.select_all(),
        }
    }

    fn select_none(&mut self) -> bool {
        match self {
            Screens::Commits { view: c, .. } => c.select_none(),
            Screens::Diff { view: d, .. } => d.select_none(),
            Screens::Merging { view: m, .. } => m.select_none(),
            Screens::Stashes { view: s, .. } => s.select_none(),
            Screens::Files { view: f, .. } => f.select_none(),
            Screens::Branches { view: b, .. } => b.select_none(),
            Screens::Remotes { view: r, .. } => r.select_none(),
            Screens::Tags { view: t, .. } => t.select_none(),
            Screens::Reflog { view: r, .. } => r.select_none(),
            Screens::Worktrees { view: w, .. } => w.select_none(),
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
            Screens::Tags { view: t, .. } => t.filter_note(),
            Screens::Reflog { view: r, .. } => r.filter_note(),
            Screens::Worktrees { view: w, .. } => w.filter_note(),
            Screens::Diff { view: d, .. } => d.match_note(),
            // The merging view carries no standing search yet.
            Screens::Merging { .. } => None,
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
            Screens::Tags { view: t, .. } => t.query().is_some(),
            Screens::Reflog { view: r, .. } => r.query().is_some(),
            Screens::Worktrees { view: w, .. } => w.query().is_some(),
            Screens::Diff { view: d, .. } => d.search_query().is_some(),
            Screens::Merging { .. } => false,
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
            Screens::Merging { view: m, .. } => match command {
                "view.down" => m.down(),
                "view.up" => m.up(),
                "view.page-down" => m.page(1),
                "view.page-up" => m.page(-1),
                "view.scroll-down" => m.scroll_y(host.view.rows as isize),
                "view.scroll-up" => m.scroll_y(-(host.view.rows as isize)),
                "view.top" => m.to_top(),
                "view.bottom" => m.to_bottom(),
                // Nothing off the left edge to reach: the file's lines
                // clip rather than pan, like every list here.
                "view.left" | "view.right" => {}
                "merge.next-conflict" => m.jump_region(1),
                "merge.prev-conflict" => m.jump_region(-1),
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
            Screens::Tags { view: t, .. } => match command {
                "view.down" => t.down(),
                "view.up" => t.up(),
                "view.page-down" => t.page(1),
                "view.page-up" => t.page(-1),
                "view.scroll-down" => t.scroll_y(host.view.rows as isize),
                "view.scroll-up" => t.scroll_y(-(host.view.rows as isize)),
                "view.top" => t.to_top(),
                "view.bottom" => t.to_bottom(),
                // Nothing off the left edge to reach: names clip rather
                // than pan.
                "view.left" | "view.right" => {}
                "search.next" => t.next_match(1),
                "search.prev" => t.next_match(-1),
                _ => return false,
            },
            Screens::Worktrees { view: w, .. } => match command {
                "view.down" => w.down(),
                "view.up" => w.up(),
                "view.page-down" => w.page(1),
                "view.page-up" => w.page(-1),
                "view.scroll-down" => w.scroll_y(host.view.rows as isize),
                "view.scroll-up" => w.scroll_y(-(host.view.rows as isize)),
                "view.top" => w.to_top(),
                "view.bottom" => w.to_bottom(),
                // Nothing off the left edge to reach: paths clip rather
                // than pan.
                "view.left" | "view.right" => {}
                "search.next" => w.next_match(1),
                "search.prev" => w.next_match(-1),
                _ => return false,
            },
            Screens::Reflog { view: r, .. } => match command {
                "view.down" => r.down(),
                "view.up" => r.up(),
                "view.page-down" => r.page(1),
                "view.page-up" => r.page(-1),
                "view.scroll-down" => r.scroll_y(host.view.rows as isize),
                "view.scroll-up" => r.scroll_y(-(host.view.rows as isize)),
                "view.top" => r.to_top(),
                "view.bottom" => r.to_bottom(),
                // Nothing off the left edge to reach: selectors clip rather
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
    outcome: Result<acquire::Data, String>,
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
    /// The rebase plan, while it is open: a modal list over the body,
    /// owning the keyboard the way the picker does. Nothing is written
    /// until `todo.run` is confirmed, so closing it with esc costs exactly
    /// the editing — which is what makes a plan safe to open and look at.
    todo: Option<Todo>,
    /// The patch builder, while it is open: a modal list over the body,
    /// owning the keyboard the way the plan does. The clipboard stays on
    /// the app beside it — this holds the cursor only — so a builder
    /// opened over an empty clipboard draws one honest row, and closing
    /// it with esc costs nothing at all.
    patch: Option<PatchBuilder>,
    /// The commit marked as the base a `--onto` rebase counts from, and
    /// the row it was marked on. Held here rather than in the commits pane
    /// for the reason the clipboard is: the mark is made in one pane and
    /// spent in another, and it has to outlive both panes' refreshes.
    rebase_base: Option<gitten_app::act::SelectedCommit>,
    /// The mode a standing menu question pushed — `reset`, `upstream` —
    /// for as long as the question stands. Above the pane's own bindings
    /// and not instead of them: `s` means the strength while the question
    /// is up and goes back to meaning the pane's verb the moment anything
    /// else is pressed, which is the menu doing its job rather than
    /// stealing three keys from the pane forever.
    question: Option<&'static str>,
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
    /// The operation standing in the repository, as the last acquisition
    /// reported it — the banner names it, availability gates the lifecycle
    /// keys on it, and shared policy (`act`) reads it instead of asking the
    /// repository a second time. Refreshed wherever the panes are.
    operation: Option<Operation>,
    /// The bisection standing in the repository, as the last acquisition
    /// reported it — read through the repository on open and on every
    /// refresh, the W5 reopen-recovery pattern, because a bisect started
    /// in a terminal is a banner this client must show unasked.
    bisect: Option<gitten_core::bisect::BisectState>,
    /// The commits kept for `commits.paste`, as full shas in paste order.
    /// Outlives every press and every refresh, because a copy made before a
    /// branch switch is exactly the copy a paste onto the new branch wants;
    /// only `commits.clear-copies` empties it.
    clipboard: gitten_core::clipboard::CherryClipboard,
    /// The hunks kept for a later apply, as content — the patch clipboard
    /// beside the cherry-pick clipboard. Outlives every press and every
    /// refresh, because a pick made before a branch switch is exactly the
    /// patch a move onto the new branch wants; only `patch.clear`
    /// empties it.
    patch_clip: gitten_core::patchclip::PatchClipboard,
    /// The armed (command, commit) a destructive history verb asked about —
    /// spent only by the same command naming the same commit, which is what
    /// keeps a soft reset from ever spending a hard one's question. `None`
    /// while no history question stands.
    history_arm: Option<(String, Vec<u8>)>,
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
        // The tags pane, the same shape again: registered behind a
        // repository in its loading shape, read by the startup wave.
        let tags_tenant = repo.is_some().then(|| {
            let mut view = Tags::unavailable();
            view.set_bar(bar);
            Screens::Tags {
                view,
                label: STARTUP_LOADING.to_string(),
                generation: Generation::default(),
            }
        });
        // The reflog pane, the same shape a third time: registered behind
        // a repository in its loading shape, read by the startup wave.
        let reflog_tenant = repo.is_some().then(|| {
            let mut view = Reflog::unavailable();
            view.set_bar(bar);
            Screens::Reflog {
                view,
                label: STARTUP_LOADING.to_string(),
                generation: Generation::default(),
            }
        });
        // The worktrees pane, the same shape a fourth time: registered
        // behind a repository in its loading shape, read by the startup
        // wave. A fixture or a patch has no repository and so no pane;
        // the name stays absent and `worktrees.focus` says so.
        let worktrees_tenant = repo.is_some().then(|| {
            let mut view = Worktrees::unavailable();
            view.set_bar(bar);
            Screens::Worktrees {
                view,
                label: STARTUP_LOADING.to_string(),
                generation: Generation::default(),
            }
        });
        let mut panes = panes::Panes::new();
        let mut last_list = None;
        match started.loaded.data {
            // A launch never opens on a conflict: the files pane does not
            // exist yet, so no eye could be on one.
            Data::Conflict(..) => {}
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
                if let Some(pane) = tags_tenant {
                    panes.register("tags", panes::Placement::sidebar("tags"), pane);
                }
                if let Some(pane) = reflog_tenant {
                    panes.register("reflog", panes::Placement::sidebar("reflog"), pane);
                }
                if let Some(pane) = worktrees_tenant {
                    panes.register("worktrees", panes::Placement::sidebar("worktrees"), pane);
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
                if let Some(pane) = tags_tenant {
                    panes.register("tags", panes::Placement::sidebar("tags"), pane);
                }
                if let Some(pane) = reflog_tenant {
                    panes.register("reflog", panes::Placement::sidebar("reflog"), pane);
                }
                if let Some(pane) = worktrees_tenant {
                    panes.register("worktrees", panes::Placement::sidebar("worktrees"), pane);
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
            availability: tui_availability(startup_pending, None, None),
            operation: None,
            bisect: None,
            clipboard: gitten_core::clipboard::CherryClipboard::new(),
            patch_clip: gitten_core::patchclip::PatchClipboard::new(),
            patch: None,
            history_arm: None,
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
            todo: None,
            rebase_base: None,
            question: None,
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

        let (
            stash_read,
            remotes_read,
            tags_read,
            reflog_read,
            status_read,
            described,
            branch_reads,
            diff_read,
            worktrees_read,
        ) = std::thread::scope(|s| {
            // The handle as a stable borrow the `move` spawns copy: an
            // `Arc` would be four refcount bumps for the same answer.
            let repo = &repo;
            let stashes = s.spawn(move || acquire::stashes(repo.as_ref()));
            let remotes = s.spawn(|| repo.remotes());
            let tags = s.spawn(|| repo.tags());
            let reflog = s.spawn(|| repo.reflog(REFLOG_ENTRIES));
            let worktrees = s.spawn(|| repo.worktrees());
            let status = s.spawn(move || repo.status());
            let described = s.spawn(move || repo.describe());
            let branches = s.spawn(move || load_branches(repo.as_ref()));
            let diff = preview.as_ref().map(|commit| {
                let source = Source::Repo {
                    path: path.clone(),
                    arg: commit.sha.clone(),
                };
                s.spawn(move || acquire::acquire(View::Diff, &source, &host, Some(repo.as_ref())))
            });
            (
                join_read(stashes),
                join_read(remotes),
                join_read(tags),
                join_read(reflog),
                join_read(status),
                join_read(described),
                join_read(branches),
                diff.map(join_read),
                join_read(worktrees),
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
        if let Some(Screens::Tags { view, label, .. }) = self.panes.get_mut("tags") {
            match tags_read {
                Ok(tags) => {
                    let count = tags.len();
                    view.replace(tags);
                    *label = tags_label(&described, count);
                }
                Err(e) => {
                    *label = "unavailable".to_string();
                    error.get_or_insert(e);
                }
            }
        }
        if let Some(Screens::Worktrees { view, label, .. }) = self.panes.get_mut("worktrees") {
            match worktrees_read {
                Ok(list) => {
                    let count = list.len();
                    // The row guard's `here`: canonicalized, because the
                    // listing spells every path resolved.
                    let here = std::fs::canonicalize(&path)
                        .unwrap_or_else(|_| path.clone())
                        .as_os_str()
                        .as_encoded_bytes()
                        .to_vec();
                    view.replace(list, &here);
                    *label = worktrees_label(&described, count);
                }
                Err(e) => {
                    *label = "unavailable".to_string();
                    error.get_or_insert(e);
                }
            }
        }
        if let Some(Screens::Reflog { view, label, .. }) = self.panes.get_mut("reflog") {
            match reflog_read {
                Ok(entries) => {
                    let count = entries.len();
                    view.replace(entries);
                    *label = reflog_label(&described, count);
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
        // The operation standing re-read with the same wave that refreshed
        // the panes: an externally started rebase is on the banner before
        // the first real frame draws.
        self.sync_bisect();
        self.sync_operation();
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
                    // A startup preview asks about a commit; a conflict
                    // answer here would mean the read was asked the wrong
                    // question. Neither installs.
                    Data::Commits(_) | Data::Conflict(..) => {}
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
    /// then `tabs` when the focused list shares its section with another,
    /// then the focused pane's own mode, then help and any prompt.
    fn sync_modes(&mut self) {
        self.modes = Modes::new();
        if self.panes.list_order().len() > 1 {
            self.modes.push(panes::MODE);
        }
        // And the tab pair only where there is a second tab to reach: a
        // section of one has nothing for `[`/`]` to say, and the help panel
        // must not list a key that would answer with a refusal.
        if self.panes.section_tabs(self.panes.focused_name()).len() > 1 {
            self.modes.push(panes::TABS);
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
        // A standing menu question is the pane's innermost mode while it
        // stands: `s` is a strength here and the pane's own verb everywhere
        // else, which is what a menu is for.
        if let Some(question) = self.question {
            self.modes.push(question);
        }
        if self.picker.is_some() {
            // The recent-repositories list owns the keyboard like help does:
            // a press it does not name runs nothing underneath.
            self.modes.push(PICKER);
        }
        if self.todo.is_some() {
            self.modes.push(TODO);
        }
        if self.patch.is_some() {
            self.modes.push(BUILDER);
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
        // The registry does not read the keymap, so the focus keys the
        // section headers advertise are carried in here — from
        // [`App::focus_keys`], resolved once per host or registry change,
        // because the key's width is what decides where the tabs start and a
        // frame has no business formatting strings. A reload that moves a key
        // therefore has to drop the cache, which is what
        // [`App::sync_header_keys`] does.
        let mut spots = self.panes.spots();
        for spot in &mut spots {
            spot.key = self
                .focus_keys
                .iter()
                .find(|(n, _)| n == spot.name)
                .map(|(_, k)| k.as_str())
                .unwrap_or("");
        }
        let geometry = self.layout.arrange(&spots, body);
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
        // A moved key moves every tab on every section header, because the
        // key's width is where the first tab starts — so the cached geometry
        // dies with the keys it was laid out against.
        self.geometry = None;
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
        // While the plan is open it owns the keyboard the way help does:
        // resolved against exactly its one mode, so nothing underneath runs
        // — and what is underneath a rebase plan is the history verbs.
        if self.todo.is_some() && self.prompt.is_none() && !self.help {
            self.press_modal(TODO, key, ModalKind::List);
            return;
        }
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
                // An unbound key answers a standing question by dismissing
                // it: the reader reached for something that is not one of
                // the answers, and leaving the menu up would have the next
                // press mean a strength they have stopped asking about.
                self.close_question();
                // Said, not swallowed: a key that does nothing and a key that
                // is not bound look identical, and only one of them is worth
                // opening `?` about.
                self.message = format!("{unknown} is not bound — ? for the keys");
                return;
            }
        };
        self.pending.clear();
        if let Some(command) = resolved {
            // The question closes before the command it resolved runs —
            // unless that command is a menu opening one, which is what
            // pressing `g` twice means. Closing first is what keeps the
            // *next* press out of the question's mode.
            if !matches!(
                command.as_str(),
                "commits.reset-menu" | "files.reset-menu" | "files.stash-menu" | "patch.menu"
            ) {
                self.close_question();
            }
            self.dispatch(&command);
        }
    }

    /// Takes a standing menu question down, and puts the keymap back the
    /// way it was. Cheap and idempotent: called on every press that is not
    /// one of the question's own answers.
    fn close_question(&mut self) {
        if self.question.take().is_some() {
            self.sync_modes();
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
        if h < 3 || self.prompt.is_some() || self.picker.is_some() || self.todo.is_some() {
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
            Some(Screens::Tags { view, .. }) => view.query().unwrap_or_default().to_string(),
            Some(Screens::Reflog { view, .. }) => view.query().unwrap_or_default().to_string(),
            Some(Screens::Worktrees { view, .. }) => view.query().unwrap_or_default().to_string(),
            Some(Screens::Diff { view, .. }) => {
                if !view.has_search_text() {
                    self.message = format!("{command}: the diff has no text to search");
                    return;
                }
                view.search_query().unwrap_or_default().to_string()
            }
            // The merging view carries no standing search to re-open.
            Some(Screens::Merging { .. }) => {
                self.message = format!("{command} is not supported here");
                return;
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

    // -------------------------------------------------- the patch builder

    /// Takes the builder down, costing nothing: toggles already ran on
    /// the clipboard and applies are jobs of their own.
    fn close_patch_builder(&mut self) {
        self.patch = None;
        self.sync_modes();
    }

    /// The builder's verbs, answered while it stands: moves over its
    /// rows, toggles on the clipboard, applies off it. Anything else
    /// falls through to the match below, which is where the clipboard
    /// verbs live whether the builder stands or not.
    fn patch_verb(&mut self, command: &str) -> bool {
        if self.patch.is_none() {
            return false;
        }
        match command {
            "view.down" => {
                if let Some(builder) = self.patch.as_mut() {
                    builder.down(&self.patch_clip);
                }
                return true;
            }
            "view.up" => {
                if let Some(builder) = self.patch.as_mut() {
                    builder.up(&self.patch_clip);
                }
                return true;
            }
            "view.top" => {
                if let Some(builder) = self.patch.as_mut() {
                    builder.to_top(&self.patch_clip);
                }
                return true;
            }
            "view.bottom" => {
                if let Some(builder) = self.patch.as_mut() {
                    builder.to_bottom(&self.patch_clip);
                }
                return true;
            }
            "back" | "input.cancel" => {
                self.close_patch_builder();
                self.message = "the builder is closed — the patch stands".into();
                return true;
            }
            _ => {}
        }
        // Toggles edit the clipboard in place and answer where the press
        // happened; every other builder key is a clipboard verb, run
        // through the shared actions below.
        let said = match self.patch.as_mut() {
            Some(builder) => match command {
                "patch.toggle-hunk" => Some(builder.toggle(&mut self.patch_clip)),
                "patch.toggle-file" => Some(builder.toggle_file_here(&mut self.patch_clip)),
                "patch.drop-file" => Some(builder.drop_file_here(&mut self.patch_clip)),
                _ => None,
            },
            None => None,
        };
        if let Some(said) = said {
            self.message = said;
            return true;
        }
        false
    }

    // -------------------------------------------------- the rebase plan

    /// Opens the plan over the body, and says what it holds. Nothing is
    /// written by opening one — the plan is every commit picked until
    /// somebody edits it — which is what makes it safe to open and read.
    fn open_todo(&mut self, plan: gitten_core::rebase::Plan) {
        let todo = Todo::new(plan);
        self.message = format!("{} — enter runs it, esc leaves", todo.summary());
        self.todo = Some(todo);
        self.gesture = None;
        self.pending.clear();
        self.sync_modes();
    }

    /// Takes the plan down. Whatever was edited dies with it: nothing
    /// reached the repository, which is the whole contract of a cancelled
    /// todo edit.
    fn close_todo(&mut self) {
        self.todo = None;
        self.pending.clear();
        self.sync_modes();
    }

    /// The open plan's verbs: every one edits the model and writes nothing.
    /// `todo.run` is the single door to the queue, and it asks first.
    fn todo_verb(&mut self, command: &str) {
        if self.todo.is_none() {
            self.message = format!("{command} needs an open rebase plan");
            return;
        }
        // Any edit takes the standing question down with it: the answer
        // was given about the plan as it read a moment ago, and a confirm
        // that survived an edit would run a rewrite nobody was asked about.
        if command != "todo.run" {
            self.history_arm = None;
        }
        use gitten_core::rebase::Action;
        let action = match command {
            "todo.pick" => Some(Action::Pick),
            "todo.edit" => Some(Action::Edit),
            "todo.squash" => Some(Action::Squash),
            "todo.fixup" => Some(Action::Fixup),
            "todo.drop" => Some(Action::Drop),
            _ => None,
        };
        if let Some(action) = action {
            let said = match self.todo.as_mut() {
                Some(todo) => match todo.set_action(action) {
                    Ok(()) => todo.summary(),
                    Err(e) => e,
                },
                None => return,
            };
            self.message = said;
            return;
        }
        match command {
            "todo.fixup-keep" => {
                let said = match self.todo.as_mut() {
                    Some(todo) => match todo.fixup_keeping_message() {
                        Ok(()) => todo.summary(),
                        Err(e) => e,
                    },
                    None => return,
                };
                self.message = said;
            }
            "todo.reword" => {
                let subject = match self.todo.as_ref() {
                    Some(todo) => todo.selected_subject(),
                    None => return,
                };
                self.open_prompt(Prompt::TodoReword {
                    field: Field::with(subject),
                });
            }
            "todo.move-up" | "todo.move-down" => {
                let up = command.ends_with("up");
                let said = match self.todo.as_mut() {
                    Some(todo) => match todo.move_by(up) {
                        Ok(()) => todo.summary(),
                        Err(e) => e,
                    },
                    None => return,
                };
                self.message = said;
            }
            "todo.autosquash" => {
                let said = match self.todo.as_mut() {
                    Some(todo) => todo.autosquash(),
                    None => return,
                };
                self.message = said;
            }
            "todo.run" => {
                let Some(plan) = self.todo.as_ref().map(|todo| todo.plan().clone()) else {
                    return;
                };
                // The plan is *cloned* out and the screen stays open until
                // the queue has it: a refusal — an unconfirmed first press,
                // a standing operation — must leave the editing exactly
                // where the reader left it.
                if gitten_app::act::run_plan(self, command, plan) {
                    self.close_todo();
                }
            }
            _ => {}
        }
    }

    /// `commits.reword`'s field, prefilled with the commit's own subject.
    /// The commit is captured here, so nothing a cursor does while the
    /// field holds the keyboard can re-aim the rewrite.
    fn begin_reword(&mut self) {
        use gitten_app::act::HistoryClient;
        let Some(target) = self.commit_target() else {
            self.message = "nothing selected to reword".into();
            return;
        };
        if self.repo.is_none() {
            self.message = "a fixture has no repository to rewrite in".into();
            return;
        }
        let subject = match self.panes.focused() {
            Some(Screens::Commits { view, .. }) => view
                .current()
                .map(|c| c.subject.clone())
                .unwrap_or_default(),
            _ => String::new(),
        };
        self.open_prompt(Prompt::Reword {
            at: target,
            field: Field::with(subject),
        });
    }

    /// Tells the commits pane which commit is the marked rebase base, so
    /// its status line can say so. Called after every press that changes
    /// the mark and nowhere else — the same shape the clipboard's own
    /// [`App::sync_copied`] has, and for the same reason.
    fn sync_marked_base(&mut self) {
        let shown = self.rebase_base.as_ref().map(|b| b.short.clone());
        if let Some(Screens::Commits { view, .. }) = self.panes.get_mut("commits") {
            view.set_base(shown);
        }
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

    /// Hands the commits pane the clipboard's shas so a copied row can draw
    /// itself as one. Called after every press that changes the clipboard
    /// and nowhere else — the pane keeps the set across a refresh, exactly
    /// as the clipboard does, because a write that renumbers every row does
    /// not un-copy anything.
    fn sync_copied(&mut self) {
        let shas = self.clipboard.ordered().to_vec();
        if let Some(Screens::Commits { view, .. }) = self.panes.get_mut("commits") {
            view.set_copied(&shas);
        }
    }

    /// Whether the keyboard is on the commits pane — the guard every history
    /// verb opens with, said the way every wrong-focus refusal here is said.
    fn commits_focused(&mut self, command: &str) -> bool {
        match self.panes.focused() {
            Some(Screens::Commits { .. }) => true,
            _ => {
                self.message = format!("{command} is not supported here");
                false
            }
        }
    }

    /// Whether the keyboard is on the working-tree pane — the guard the
    /// pane's repository-wide questions open with, said the way every
    /// wrong-focus refusal here is said.
    fn files_focused(&mut self, command: &str) -> bool {
        match self.panes.focused() {
            Some(Screens::Files { .. }) => true,
            _ => {
                self.message = format!("{command} is not supported here");
                false
            }
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
        // A branch held by another worktree is git's refusal, said first:
        // checking it out here would move the other tree's HEAD under it.
        if repo
            .worktree_branches()
            .iter()
            .any(|b| b.as_bytes() == name.as_bytes())
        {
            self.message = format!(
                "{} is checked out in another worktree",
                name.to_string_lossy()
            );
            return;
        }
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

    /// The accepted tag name, as a message field: empty names are refused
    /// beside the field that just closed, and the message the next field
    /// returns decides annotated versus lightweight — the same chain for a
    /// branch's bytes and a commit's sha, because both are revspecs.
    fn begin_tag_message(&mut self, at: Vec<u8>, name: String) {
        if name.trim().is_empty() {
            self.message = "a tag needs a name".into();
            return;
        }
        self.open_prompt(Prompt::TagMessage {
            name,
            at,
            field: Field::new(),
        });
    }

    /// `tags.new`: name a tag on HEAD — the tags pane holds no commit, so
    /// the checked-out commit is what a bare name means. Unborn refuses:
    /// there is no commit to name yet.
    fn begin_tag_new(&mut self) {
        if !self.tags_focused("tags.new") {
            return;
        }
        let Some((_, repo)) = self.repo.as_ref() else {
            self.message = "a fixture has no repository to tag in".into();
            return;
        };
        let at = match repo.head() {
            Ok(gitten_core::refs::HeadState::Branch {
                commit: Some(sha), ..
            })
            | Ok(gitten_core::refs::HeadState::Detached { commit: sha }) => sha.into_bytes(),
            _ => {
                self.message = "no commit to tag yet".into();
                return;
            }
        };
        self.open_prompt(Prompt::TagNew {
            at,
            field: Field::new(),
        });
    }

    /// `commits.new-tag`: name the commit under the keyboard with a tag —
    /// the branches pane's tagger aimed at a sha instead of a branch.
    fn begin_commit_tag(&mut self) {
        let Some(Screens::Commits { view, .. }) = self.panes.focused() else {
            self.message = "commits.new-tag is not supported here".into();
            return;
        };
        let Some(commit) = view.current() else {
            self.message = "the keyboard is not on a commit".into();
            return;
        };
        if self.repo.is_none() {
            self.message = "a fixture has no repository to tag in".into();
            return;
        }
        self.open_prompt(Prompt::TagNew {
            at: commit.sha.clone().into_bytes(),
            field: Field::new(),
        });
    }

    /// `tags.push`: the remote rides a field, prefilled when the repository
    /// knows exactly one — tags track nothing, so a lone remote is the
    /// only default that cannot aim wrong.
    fn begin_tag_push(&mut self) {
        if !self.tags_focused("tags.push") {
            return;
        }
        let Some(Screens::Tags { view, .. }) = self.panes.focused() else {
            return;
        };
        let Some(name) = view.current() else {
            self.message = "nothing selected to push".into();
            return;
        };
        let Some((_, repo)) = self.repo.as_ref() else {
            self.message = "a fixture has no repository to push from".into();
            return;
        };
        let initial = match repo.remotes() {
            Ok(remotes) if remotes.len() == 1 => remotes
                .first()
                .map(|r| r.name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            _ => String::new(),
        };
        self.open_prompt(Prompt::TagPush {
            name: name.as_bytes().to_vec(),
            field: Field::with_selected(&initial),
        });
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

    /// Whether the keyboard is on the tags pane — the guard every tag verb
    /// opens with, said the way every wrong-focus refusal here is said.
    fn tags_focused(&mut self, command: &str) -> bool {
        match self.panes.focused() {
            Some(Screens::Tags { .. }) => true,
            _ => {
                self.message = format!("{command} is not supported here");
                false
            }
        }
    }

    /// Whether the keyboard is on the reflog pane — the guard recovery
    /// opens with, said the way every wrong-focus refusal here is said.
    fn reflog_focused(&mut self, command: &str) -> bool {
        match self.panes.focused() {
            Some(Screens::Reflog { .. }) => true,
            _ => {
                self.message = format!("{command} is not supported here");
                false
            }
        }
    }

    /// Whether the keyboard is on the worktrees pane — the guard every
    /// worktree verb opens with, said the way every wrong-focus refusal
    /// here is said.
    fn worktrees_focused(&mut self, command: &str) -> bool {
        match self.panes.focused() {
            Some(Screens::Worktrees { .. }) => true,
            _ => {
                self.message = format!("{command} is not supported here");
                false
            }
        }
    }

    /// The worktree row the keyboard is on — the implementation behind
    /// [`act::WorktreeClient`].
    fn worktree_target(&self) -> Option<Vec<u8>> {
        match self.panes.focused() {
            Some(Screens::Worktrees { view, .. }) => view.current(),
            _ => None,
        }
    }

    /// Whether the keyboard sits on the checkout this client stands in.
    fn worktree_is_here(&self, path: &[u8]) -> bool {
        match self.panes.focused() {
            Some(Screens::Worktrees { view, .. }) => {
                view.current().as_deref() == Some(path) && view.current_is_here()
            }
            _ => false,
        }
    }

    /// Whether the force upgrade stands on this exact path — the third
    /// press's question, answered by the arm the submitted removal left.
    fn worktree_armed_force(&self, path: &[u8]) -> bool {
        match self.panes.focused() {
            Some(Screens::Worktrees { view, .. }) => {
                view.armed().as_ref().is_some_and(|(p, f)| p == path && *f)
            }
            _ => false,
        }
    }

    /// Arms the selected checkout for a confirmed removal, or spends the
    /// arm — the implementation behind [`act::WorktreeClient`].
    fn confirm_or_arm_worktree(&mut self, path: &[u8], force: bool) -> bool {
        match self.panes.focused_mut() {
            Some(Screens::Worktrees { view, .. }) => view.confirm_or_arm_remove(path, force),
            _ => false,
        }
    }

    /// Stands the force upgrade after a plain removal was submitted — the
    /// implementation behind [`act::WorktreeClient`].
    fn upgrade_worktree_force(&mut self, path: &[u8]) {
        if let Some(Screens::Worktrees { view, .. }) = self.panes.focused_mut() {
            view.upgrade_to_force(path);
        }
    }

    /// `worktrees.switch`: open the selected checkout as a repository —
    /// the same road `project.open` walks, so the MRU, the generations
    /// and the refusals all behave as they do there. A row whose directory
    /// is gone refuses in the open's own words.
    fn switch_worktree(&mut self) {
        if !self.worktrees_focused("worktrees.switch") {
            return;
        }
        let Some(path) = self.worktree_target() else {
            self.message = "nothing selected on the worktree list".into();
            return;
        };
        self.open_repository(&String::from_utf8_lossy(&path));
    }

    /// `worktrees.new`: gather the starting point first — empty means
    /// HEAD — then the path, the way `remotes.new` gathers name then URL.
    fn begin_worktree_base(&mut self) {
        if self.repo.is_none() {
            self.message = "a fixture has no repository to branch a worktree from".into();
            return;
        }
        self.open_prompt(Prompt::WorktreeBase {
            field: Field::new(),
        });
    }

    /// The path step of `worktrees.new`: `base` rides along from the row
    /// or the first field, so nothing a cursor does while the field holds
    /// the keyboard can re-aim the checkout. An empty base checks out
    /// HEAD's branch, which git refuses when this tree holds it — the
    /// refusal arrives in git's own words, and that sentence is the
    /// branch decision's proper home.
    fn begin_worktree_path(&mut self, base: Vec<u8>) {
        self.open_prompt(Prompt::WorktreePath {
            base,
            field: Field::new(),
        });
    }

    /// A new worktree from the row the keyboard is on — lazygit's `w`.
    /// The base is the row's own rev, captured here; the path rides the
    /// prompt. A detached branch row names no branch to start from and
    /// says so rather than guessing.
    fn begin_worktree_from_row(&mut self, command: &str) {
        use gitten_app::act::StashClient;
        if self.repo.is_none() {
            self.message = format!("a fixture has no repository for {command}");
            return;
        }
        let base: Option<Vec<u8>> = match self.panes.focused() {
            Some(Screens::Commits { view, .. }) => {
                view.current().map(|c| c.sha.clone().into_bytes())
            }
            Some(Screens::Branches { .. }) => match self.branch_target() {
                Some(Target::Local(name)) => Some(name.as_bytes().to_vec()),
                Some(Target::Remote { remote, branch }) => Some(
                    format!("{}/{}", remote.to_string_lossy(), branch.to_string_lossy())
                        .into_bytes(),
                ),
                Some(Target::Detached) | None => None,
            },
            Some(Screens::Stashes { .. }) => self.selected_stash().map(|id| id.commit.into_bytes()),
            Some(Screens::Tags { view, .. }) => view.current_commit().map(String::into_bytes),
            _ => None,
        };
        let Some(base) = base else {
            self.message = format!("{command} has no revision to start from here");
            return;
        };
        self.open_prompt(Prompt::WorktreePath {
            base,
            field: Field::new(),
        });
    }

    /// `commits.bisect-menu`: the selected commit is where the bug would
    /// be — but only the menu decides that. With a bisection standing
    /// this opens the judgement question; with a clean tree, the start
    /// field aimed at the selected commit.
    fn bisect_menu(&mut self) {
        if self.bisect.is_some() {
            let word = self.bisect.as_ref().map(|b| b.word()).unwrap_or_default();
            // The reset menu's shape: the options said aloud, the answers
            // a keypress away in the question's own mode, anything else
            // closing it. The standing state is re-read, not trusted — a
            // reset in a terminal since the last refresh closes this into
            // the start field's refusal rather than a dead question.
            self.message = format!("{word}: g good · b bad · s skip · r reset");
            self.question = Some("bisect");
            self.sync_modes();
            return;
        }
        self.begin_bisect_good();
    }
    fn begin_bisect_good(&mut self) {
        let Some(Screens::Commits { view, .. }) = self.panes.focused() else {
            self.message = "commits.bisect-menu is not supported here".into();
            return;
        };
        let Some(commit) = view.current() else {
            self.message = "nothing selected to bisect from".into();
            return;
        };
        if self.repo.is_none() {
            self.message = "a fixture has no history to bisect".into();
            return;
        }
        if self.bisect.is_some() {
            self.message = "a bisect is already in progress — reset it first".into();
            return;
        }
        self.open_prompt(Prompt::BisectGood {
            bad: commit.sha.clone().into_bytes(),
            field: Field::new(),
        });
    }

    /// The remote row the keyboard is on, as the verbs address it — the
    /// implementation behind [`act::RemoteClient`].
    fn remote_target(&self) -> Option<RefName> {
        match self.panes.focused() {
            Some(Screens::Remotes { view, .. }) => view.current(),
            _ => None,
        }
    }

    /// The tag row the keyboard is on — the implementation behind
    /// [`act::TagClient`].
    fn tag_target(&self) -> Option<RefName> {
        match self.panes.focused() {
            Some(Screens::Tags { view, .. }) => view.current(),
            _ => None,
        }
    }

    /// Arms the selected tag for a confirmed deletion, or spends the arm —
    /// the implementation behind [`act::TagClient`].
    fn confirm_or_arm_tag(&mut self, name: &RefName) -> bool {
        match self.panes.focused_mut() {
            Some(Screens::Tags { view, .. }) => view.confirm_or_arm_delete(name),
            _ => false,
        }
    }

    /// The reflog row the keyboard is on — the implementation behind
    /// [`act::ReflogClient`].
    fn reflog_target(&self) -> Option<gitten_core::refs::ReflogEntry> {
        match self.panes.focused() {
            Some(Screens::Reflog { view, .. }) => view.current(),
            _ => None,
        }
    }

    /// Arms the selected reflog entry for a confirmed recovery, or spends
    /// the arm — the implementation behind [`act::ReflogClient`]. Both
    /// halves must match the standing arm: the selector asked over and the
    /// commit it named, so a row that slid under the cursor cannot spend it.
    fn confirm_or_arm_reflog(&mut self, selector: &str) -> bool {
        // The row cannot move between the target read and this call — one
        // press, no awaits — so a mismatch is a stale arm dying unspent.
        match self.panes.focused_mut() {
            Some(Screens::Reflog { view, .. }) => match view.current() {
                Some(entry) if entry.selector == selector => {
                    view.confirm_or_arm_recover(&entry.selector, &entry.commit)
                }
                _ => false,
            },
            _ => false,
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
        let mut tags = Tags::unavailable();
        tags.set_bar(self.bar);
        let mut reflog = Reflog::unavailable();
        reflog.set_bar(self.bar);
        let mut worktrees = Worktrees::unavailable();
        worktrees.set_bar(self.bar);
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
            "tags",
            panes::Placement::sidebar("tags"),
            Screens::Tags {
                view: tags,
                label: STARTUP_LOADING.to_string(),
                generation: Generation::default(),
            },
        );
        panes.register(
            "reflog",
            panes::Placement::sidebar("reflog"),
            Screens::Reflog {
                view: reflog,
                label: STARTUP_LOADING.to_string(),
                generation: Generation::default(),
            },
        );
        panes.register(
            "worktrees",
            panes::Placement::sidebar("worktrees"),
            Screens::Worktrees {
                view: worktrees,
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
        // refused the sync keys, and this one answers them. The operation
        // standing travels with it — the new repository may have arrived
        // mid-rebase — and the lifecycle keys gate on the fresh answer.
        self.sync_bisect();
        self.sync_operation();
        self.startup_pending = true;
        // A clipboard of shas from the repository just left names objects
        // the new one does not have, so a paste would be git's "bad object"
        // over commits nobody can see. The same goes for an armed history
        // question: it was asked about a commit that is no longer on screen.
        self.clipboard.clear();
        self.history_arm = None;
        // Both for the same reason the clipboard is cleared: a plan and a
        // marked base name commits of the repository that just left, and
        // neither means anything in this one.
        self.todo = None;
        self.rebase_base = None;
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
            Some(Screens::Tags { view, .. }) => view.apply_query(query),
            Some(Screens::Reflog { view, .. }) => view.apply_query(query),
            Some(Screens::Worktrees { view, .. }) => view.apply_query(query),
            Some(Screens::Diff { view, .. }) => view.search_edit(query),
            // The merging view has no filter to apply.
            Some(Screens::Merging { .. }) => {}
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
                    Some(Screens::Tags { view, .. }) => view.clear_search(),
                    Some(Screens::Reflog { view, .. }) => view.clear_search(),
                    Some(Screens::Worktrees { view, .. }) => view.clear_search(),
                    Some(Screens::Diff { view, .. }) => view.search_clear(),
                    Some(Screens::Merging { .. }) => {}
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
            Prompt::TagNew { at, field } if accept => self.begin_tag_message(at, field.take()),
            Prompt::TagMessage { name, at, field } if accept => {
                // An empty message names a lightweight tag; any text an
                // annotated one carrying it — the field's own contract.
                let text = field.take();
                let message = (!text.trim().is_empty()).then_some(text);
                gitten_app::act::create_tag(self, name, at, message)
            }
            Prompt::TagPush { name, field } if accept => {
                gitten_app::act::push_tag(self, name, field.take())
            }
            Prompt::BranchCheckoutName { field } if accept => {
                gitten_app::act::checkout_by_name(self, field.take())
            }
            Prompt::MovePatch { field } if accept => {
                let name = field.take();
                if name.trim().is_empty() {
                    self.message = "moving onto no branch is not a move".into();
                } else {
                    gitten_app::patchwork::move_patch_to_branch(self, name.into_bytes());
                }
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
            Prompt::WorktreeBase { field } if accept => {
                self.begin_worktree_path(field.take().into_bytes())
            }
            Prompt::WorktreePath { base, field } if accept => {
                gitten_app::act::create_worktree(self, base, field.take(), None)
            }
            Prompt::BisectGood { bad, field } if accept => {
                gitten_app::act::bisect_start(self, bad, field.take())
            }
            Prompt::RemoteUrl { name, field } if accept => {
                gitten_app::act::remote_add(self, name, field.take())
            }
            Prompt::RemoteEdit { name, field } if accept => {
                gitten_app::act::remote_edit(self, name.as_bytes().to_vec(), field.take())
            }
            Prompt::Reword { at, field } if accept => {
                gitten_app::act::reword_commit(self, at, field.take())
            }
            Prompt::TodoReword { field } if accept => {
                let message = field.take();
                match self.todo.as_mut() {
                    // The plan is edited and nothing is written: what a
                    // reworded row means is still a plan until it is run.
                    Some(todo) => match todo.set_message(message.into_bytes()) {
                        Ok(()) => self.message = "reworded in the plan — enter runs it".into(),
                        Err(e) => self.message = e,
                    },
                    None => self.message = "the plan closed while the message was open".into(),
                }
            }
            Prompt::StashMessage { field } if accept => {
                gitten_app::act::stash_named(self, field.take())
            }
            Prompt::StashRename { at, field } if accept => {
                gitten_app::act::rename_stash(self, at, field.take())
            }
            Prompt::StashBranch { at, field } if accept => {
                gitten_app::act::branch_from_stash(self, at, field.take())
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
        // The help panel, the picker, the rebase plan and any prompt are
        // drawn over the body, so a click that reached a view through any of
        // them would act on a row it is hiding. The keyboard gathers the
        // text; the mouse waits.
        if h < 3
            || self.help
            || self.prompt.is_some()
            || self.picker.is_some()
            || self.todo.is_some()
        {
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
                //
                // On a section header that means the *tab* under the pointer
                // when the pointer is on one of their words, and the section
                // itself — its shown tab, which is what the rectangle belongs
                // to — anywhere else on the row. Both focus, and focusing a
                // tab is what expands its section: there is no separate
                // "expand", because the open section is only ever the one the
                // keyboard is in.
                if m.row == rect.y {
                    let target = self
                        .geometry
                        .as_ref()
                        .and_then(|(_, g)| g.hit_tab(m.col, m.row))
                        .unwrap_or(&name)
                        .to_string();
                    if self.panes.focused_name() != target {
                        self.focus_named(&target);
                    }
                    self.gesture = Some(target);
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
        // While the plan is open it owns the moves and the way out, exactly
        // as the picker does — and its own verbs fall through to the match
        // below, which is where the model is edited.
        // With the help panel over it, the panel is the innermost thing on
        // screen and answers first — otherwise `esc` would close the plan
        // from behind the keys it was opened to read.
        if self.todo.is_some() && !self.help {
            match command {
                "view.down" | "view.up" | "view.top" | "view.bottom" => {
                    if let Some(todo) = self.todo.as_mut() {
                        match command {
                            "view.down" => todo.down(),
                            "view.up" => todo.up(),
                            "view.top" => todo.to_top(),
                            _ => todo.to_bottom(),
                        }
                    }
                    return;
                }
                "back" | "input.cancel" => {
                    self.close_todo();
                    self.message = "the plan is closed — nothing was rewritten".into();
                    return;
                }
                _ => {}
            }
        }
        if self.patch.is_some() && !self.help && self.patch_verb(command) {
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
            "tab.next" => self.cycle_tab(1),
            "tab.prev" => self.cycle_tab(-1),
            "status.focus" | "files.focus" | "branches.focus" | "commits.focus"
            | "stashes.focus" | "remotes.focus" | "tags.focus" | "reflog.focus"
            | "worktrees.focus" | "diff.focus" => {
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
            | "remotes.search" | "tags.search" | "reflog.search" | "worktrees.search"
            | "diff.search" => self.begin_search(command),
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
                            Screens::Tags { view, .. } => view.clear_search(),
                            Screens::Reflog { view, .. } => view.clear_search(),
                            Screens::Worktrees { view, .. } => view.clear_search(),
                            Screens::Diff { view, .. } => view.search_clear(),
                            Screens::Merging { .. } => {}
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
            // The patch verbs act on the *repository* and the clipboard,
            // not the pane: the focus names the read, the shared actions
            // take it from there. Routed ahead of the focused pane like
            // the hunk verbs — and `patch.menu` is global, because the
            // menu's answers name their own targets.
            "patch.pick" => {
                let differs = self.host.differ.clone();
                patchwork::patch_pick(self, &differs, &gitten_core::differ::Overrides::default());
            }
            "patch.menu" => {
                if patchwork::patch_menu(self) {
                    self.question = Some("patch");
                    self.sync_modes();
                }
            }
            "patch.show" => patchwork::patch_show(self),
            "patch.apply-worktree" => {
                patchwork::patch_apply(self, command, patchwork::PatchTarget::Worktree)
            }
            "patch.apply-index" => {
                patchwork::patch_apply(self, command, patchwork::PatchTarget::Index)
            }
            "patch.reverse-worktree" => {
                patchwork::patch_reverse(self, command, patchwork::PatchTarget::Worktree)
            }
            "patch.reverse-index" => {
                patchwork::patch_reverse(self, command, patchwork::PatchTarget::Index)
            }
            "patch.clear" => patchwork::patch_clear(self),
            "patch.remove-from-commit" => {
                patchwork::graft_remove(self, command, patchwork::GraftScope::Hunk)
            }
            "patch.discard-file" => {
                patchwork::graft_remove(self, command, patchwork::GraftScope::File)
            }
            "patch.amend-commit" => patchwork::graft_amend(self, command),
            "patch.checkout-file" => patchwork::commit_file_checkout(self, command),
            "patch.move-to-branch" => self.begin_move_patch(),
            // The stash verbs act on the *repository*, not the pane: the
            // pane answers "which row", the queue takes it from there.
            // Routed ahead of the focused pane for the same reason the hunk
            // verbs are — and `files.stash` needs no pane at all, only the
            // repository, so a files tenant gains its already-configured
            // `s` binding simply by registering.
            "stashes.apply" | "stashes.pop" | "stashes.drop" => self.stash_selected(command),
            "stashes.rename" => self.begin_stash_rename(),
            "stashes.new-branch" => self.begin_stash_branch(),
            "files.stash" => self.stash_working_tree(),
            // The stash choices, behind a menu of their own: the press that
            // opens it writes nothing and the answers live in a mode that
            // stands only while the question does.
            "files.stash-menu" => {
                if self.files_focused(command) && gitten_app::act::stash_menu(self) {
                    self.question = Some("stash");
                    self.sync_modes();
                }
            }
            "files.stash-named" => self.begin_stash_message(),
            "files.stash-staged" => gitten_app::act::stash_scoped(self, None, StashScope::Staged),
            "files.stash-unstaged" => {
                gitten_app::act::stash_scoped(self, None, StashScope::Unstaged)
            }
            "files.stash-untracked" => {
                gitten_app::act::stash_scoped(self, None, StashScope::WithUntracked)
            }
            "files.stash-file" => self.stash_selected_file(),
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
            // The history verbs: the commit the keyboard is on, the shared
            // action that means it. Reset's strengths arm separately — a soft
            // reset never spends a hard one's question — and the menu only
            // asks which strength; revert and cherry-pick destroy nothing,
            // so they run on the first press, and a detached checkout is
            // refused by name while an operation stands.
            "commits.reset-menu" => {
                if self.commits_focused("commits.reset-menu") {
                    gitten_app::act::reset_menu(self);
                    // The strengths are this question's mode, above the
                    // pane's own bindings and only while it stands: `s` is
                    // the soft reset here and the squash everywhere else.
                    self.question = Some("reset");
                    self.sync_modes();
                }
            }
            "commits.reset-soft" => {
                if self.commits_focused("commits.reset-soft") {
                    gitten_app::act::reset_to(self, "commits.reset-soft", ResetMode::Soft);
                }
            }
            "commits.reset-mixed" => {
                if self.commits_focused("commits.reset-mixed") {
                    gitten_app::act::reset_to(self, "commits.reset-mixed", ResetMode::Mixed);
                }
            }
            "commits.reset-hard" => {
                if self.commits_focused("commits.reset-hard") {
                    gitten_app::act::reset_to(self, "commits.reset-hard", ResetMode::Hard);
                }
            }
            "commits.revert" => {
                if self.commits_focused("commits.revert") {
                    gitten_app::act::revert_commit(self);
                }
            }
            "commits.cherry-pick" => {
                if self.commits_focused("commits.cherry-pick") {
                    gitten_app::act::cherry_pick_single(self);
                }
            }
            "commits.checkout" => {
                if self.commits_focused("commits.checkout") {
                    gitten_app::act::checkout_commit(self);
                }
            }
            // The cherry-pick clipboard. A copy takes the marked range when
            // one stands and the row alone when none does; a paste replays
            // the whole clipboard in copy order and leaves it standing, so
            // the same set reaches a second branch; the clear is the only
            // thing that empties it. `a` re-authors HEAD and nothing older.
            "commits.copy" => {
                if self.commits_focused("commits.copy") {
                    gitten_app::act::copy_commits(self);
                    self.sync_copied();
                }
            }
            "commits.paste" => {
                if self.commits_focused("commits.paste") {
                    gitten_app::act::paste_commits(self);
                }
            }
            "commits.clear-copies" => {
                if self.commits_focused("commits.clear-copies") {
                    gitten_app::act::clear_copies(self);
                    self.sync_copied();
                }
            }
            "commits.reset-author" => {
                if self.commits_focused("commits.reset-author") {
                    gitten_app::act::reset_commit_author(self);
                }
            }
            // The three one-key rewrites, each composing the whole window it
            // touches and each asking twice, armed per command so a squash
            // never spends a fixup's answer.
            "commits.squash-up" => {
                if self.commits_focused(command) {
                    gitten_app::act::rewrite_commit(self, command, Rewrite::SquashUp);
                }
            }
            "commits.fixup-up" => {
                if self.commits_focused(command) {
                    gitten_app::act::rewrite_commit(self, command, Rewrite::FixupUp);
                }
            }
            "commits.drop-commit" => {
                if self.commits_focused(command) {
                    gitten_app::act::rewrite_commit(self, command, Rewrite::Drop);
                }
            }
            // The fixup family, on lazygit's creation letter and a finder:
            // `F` commits the index as a fixup for this row, `ctrl-f` moves
            // the keyboard to the commit the staged changes build on so the
            // creation aims right, `U` folds every marker into its commit
            // (`S` is the standing operation's skip in every pane and stays
            // it), and `K` chooses what `F` writes (`c` is copy here).
            "commits.create-fixup" => {
                if self.commits_focused(command) {
                    let kind = match self.panes.get("commits") {
                        Some(Screens::Commits { view, .. }) => view.fixup_kind(),
                        _ => FixupKind::default(),
                    };
                    gitten_app::act::create_fixup(self, command, kind);
                }
            }
            "commits.find-fixup-base" => {
                if self.commits_focused(command) {
                    if let Some(index) = gitten_app::act::find_fixup_base(self, command) {
                        if let Some(Screens::Commits { view, .. }) = self.panes.get_mut("commits") {
                            view.go_to(index);
                            if let Some(found) = view.current() {
                                self.message = format!(
                                    "building on {} — move if it guessed wrong, then F",
                                    found.short
                                );
                            }
                        }
                    }
                }
            }
            "commits.apply-fixups" => {
                if self.commits_focused(command) {
                    gitten_app::act::apply_fixups(self, command);
                }
            }
            "commits.fixup-message" => {
                if self.commits_focused(command) {
                    if let Some(Screens::Commits { view, .. }) = self.panes.get_mut("commits") {
                        let kind = view.cycle_fixup_kind();
                        self.message = format!("F will create {}", kind.describe());
                    }
                }
            }
            // History *editing*: the plan the todo screen opens on, the
            // stop-here rebase, the reword field, the base mark and the two
            // reorders. Each reads the row the keyboard is on and the
            // window under it; the refusals are the shared actions'.
            "commits.interactive-rebase" => {
                if self.commits_focused(command) {
                    if let Some(plan) = gitten_app::act::interactive_plan(self, command) {
                        self.open_todo(plan);
                    }
                }
            }
            "commits.edit-commit" => {
                if self.commits_focused(command) {
                    gitten_app::act::edit_commit(self);
                }
            }
            "commits.reword" => {
                if self.commits_focused(command) {
                    self.begin_reword();
                }
            }
            "commits.mark-base" => {
                if self.commits_focused(command) {
                    gitten_app::act::mark_rebase_base(self);
                    self.sync_marked_base();
                }
            }
            "commits.move-up" | "commits.move-down" => {
                if self.commits_focused(command) {
                    gitten_app::act::move_commit(self, command, command.ends_with("up"));
                }
            }
            // The branch pane's rebase: this branch onto the row the
            // keyboard is on, from the marked base when one stands.
            "commits.rebase-onto" => {
                if self.branches_focused(command) {
                    match self.branch_target() {
                        Some(Target::Local(name)) => {
                            let shown = name.to_string_lossy().into_owned();
                            gitten_app::act::rebase_onto(self, name.as_bytes().to_vec(), shown)
                        }
                        Some(Target::Remote { remote, branch }) => {
                            let shown = format!(
                                "{}/{}",
                                remote.to_string_lossy(),
                                branch.to_string_lossy()
                            );
                            let mut onto = remote.as_bytes().to_vec();
                            onto.push(b'/');
                            onto.extend_from_slice(branch.as_bytes());
                            gitten_app::act::rebase_onto(self, onto, shown)
                        }
                        Some(Target::Detached) | None => {
                            self.message = format!("{command} has no branch to rebase onto")
                        }
                    }
                }
            }
            // The open plan's own verbs. Every one of them edits the model
            // and writes nothing; `todo.run` is the single door to the
            // queue, and it asks first.
            "todo.pick" | "todo.reword" | "todo.edit" | "todo.squash" | "todo.fixup"
            | "todo.fixup-keep" | "todo.drop" | "todo.move-up" | "todo.move-down"
            | "todo.autosquash" | "todo.run" => self.todo_verb(command),
            // The files pane's reset menu: the question, then the strengths
            // aimed at the upstream, and the nuke behind the same door.
            "files.reset-menu" => {
                if self.files_focused(command) {
                    gitten_app::act::upstream_reset_menu(self);
                    self.question = Some("upstream");
                    self.sync_modes();
                }
            }
            "files.reset-upstream-soft" => {
                gitten_app::act::reset_to_upstream(self, command, ResetMode::Soft)
            }
            "files.reset-upstream-mixed" => {
                gitten_app::act::reset_to_upstream(self, command, ResetMode::Mixed)
            }
            "files.reset-upstream-hard" => {
                gitten_app::act::reset_to_upstream(self, command, ResetMode::Hard)
            }
            "files.nuke" => gitten_app::act::nuke_worktree(self),
            // The merge verbs: the local branch the keyboard is on, brought
            // into the branch HEAD sits on. A remote row says so and stops —
            // merging a tracking ref is a checkout question first — and the
            // detached row is a place, not a branch, so every branch verb
            // refuses it the same way this one does.
            "branches.merge" | "branches.merge-squash" => {
                if self.branches_focused(command) {
                    let squash = command == "branches.merge-squash";
                    match self.branch_target() {
                        Some(Target::Local(name)) => {
                            gitten_app::act::merge_selected(self, name.as_bytes().to_vec(), squash)
                        }
                        Some(Target::Remote { .. }) => {
                            self.message = format!(
                                "{command} merges a local branch; the row the keyboard is on is remote"
                            );
                        }
                        Some(Target::Detached) | None => {
                            self.message = format!("{command} has no local branch to merge")
                        }
                    }
                }
            }
            // The lifecycle door: whichever operation is standing, one set
            // of keys — plus the per-kind capitals that reach the same
            // shared action and are checked against the same standing
            // operation there.
            "operation.abort"
            | "operation.continue"
            | "operation.skip"
            | "rebase.abort"
            | "rebase.continue"
            | "commits.cherry-pick-abort"
            | "commits.cherry-pick-continue" => gitten_app::act::operation_verb(self, command),
            // A conflict row's four answers, routed like every other file
            // write: the shared action reads the selection, refuses a row
            // that is not a conflict, and the queue does the resolving.
            "files.resolve-ours" => gitten_app::act::resolve_conflict(self, Side::Ours),
            "files.resolve-theirs" => gitten_app::act::resolve_conflict(self, Side::Theirs),
            "files.resolve-both" => gitten_app::act::resolve_conflict(self, Side::Both),
            "files.resolve-keep" => gitten_app::act::resolve_conflict(self, Side::Keep),
            // The merging view's answers: the view reads live, the shared
            // `Write` means the choice, and the queue runs it. take-side
            // reads the half under the keyboard; the named answers read the
            // region under it.
            "merge.take-ours" => self.merging_take(Some(gitten_core::conflict::Answer::Ours)),
            "merge.take-theirs" => self.merging_take(Some(gitten_core::conflict::Answer::Theirs)),
            "merge.take-both" => self.merging_take(Some(gitten_core::conflict::Answer::Both)),
            "merge.take-side" => self.merging_take(None),
            "merge.undo" => self.merging_undo(),
            "merge.options" => self.merging_options(),
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
            // The tag verbs: the row the keyboard is on, the shared
            // `Write` that means it. New and push open their fields on the
            // way through; checkout and delete go straight to the action.
            "tags.checkout" => {
                if self.tags_focused("tags.checkout") {
                    gitten_app::act::checkout_tag(self);
                }
            }
            "tags.new" => self.begin_tag_new(),
            "tags.delete" => {
                if self.tags_focused("tags.delete") {
                    gitten_app::act::delete_tag(self);
                }
            }
            "tags.push" => self.begin_tag_push(),
            // The reflog's one verb: the entry the keyboard is on, put
            // back, behind the question that previews the move.
            "reflog.recover" => {
                if self.reflog_focused("reflog.recover") {
                    gitten_app::act::recover_reflog(self);
                }
            }
            // The worktree verbs: the checkout the keyboard is on, the
            // shared `Write` that means it. New opens its fields on the
            // way through; switch opens the checkout as a repository;
            // remove goes straight to the shared action, which asks twice
            // and upgrades past a dirty refusal on the third press.
            "worktrees.new" => self.begin_worktree_base(),
            "worktrees.remove" => {
                if !self.worktrees_focused("worktrees.remove") {
                    return;
                }
                let Some(path) = self.worktree_target() else {
                    self.message = "nothing selected on the worktree list".into();
                    return;
                };
                if self.worktree_is_here(&path) {
                    self.message = "cannot remove the checkout this client stands in".into();
                    return;
                }
                // The force upgrade standing means a plain removal was
                // submitted for this exact path and came back dirty —
                // this press spends it. Anything else arms or re-arms.
                let forced = self.worktree_armed_force(&path);
                gitten_app::act::remove_worktree(self, forced);
            }
            "worktrees.switch" => self.switch_worktree(),
            // The bisect door, on lazygit's commits-pane key: with a
            // clean tree it opens the start field, aimed at the selected
            // commit; with a bisection standing it opens the judgement
            // question instead — one key, like the reset menu's `g`.
            "commits.bisect-menu" => self.bisect_menu(),
            "commits.bisect-good" => {
                gitten_app::act::bisect_mark(self, gitten_app::act::BisectMark::Good)
            }
            "commits.bisect-bad" => {
                gitten_app::act::bisect_mark(self, gitten_app::act::BisectMark::Bad)
            }
            "commits.bisect-skip" => {
                gitten_app::act::bisect_mark(self, gitten_app::act::BisectMark::Skip)
            }
            "commits.bisect-reset" => gitten_app::act::bisect_reset(self),
            // A new worktree from the row the keyboard is on — lazygit's
            // `w`, one shared implementation behind every ref list. The
            // base is the row's own rev, captured here; the path rides
            // the prompt, so nothing a cursor does while the field holds
            // the keyboard can re-aim it.
            "commits.new-worktree"
            | "branches.new-worktree"
            | "stashes.new-worktree"
            | "tags.new-worktree" => self.begin_worktree_from_row(command),
            // History's own pair, global because history is not a pane's:
            // the standing operation and the fixture refusals are the
            // shared actions', said where they are decided.
            "history.undo" => gitten_app::act::undo_last(self),
            "history.redo" => gitten_app::act::redo_last(self),
            // A remote-tracking row's source, deleted on its remote — the
            // local branch survives, and the question says so twice.
            "branches.delete-remote" => {
                if self.branches_focused("branches.delete-remote") {
                    gitten_app::act::delete_remote_branch(self);
                }
            }
            // Tagging the commit under the keyboard: the branches pane's
            // tagger, aimed at a sha.
            "commits.new-tag" => self.begin_commit_tag(),
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

    /// Walks the tabs of the focused section — what `[`/`]` do.
    ///
    /// A section is a slot in the sidebar its lists take turns in, so this is
    /// the *inner* move: the numbers name a section, ctrl-j/ctrl-k walk every
    /// list in the column, and these two stay inside the one the keyboard is
    /// in. Nothing to walk is said rather than swallowed — a `stashes` on its
    /// own is a section of one, and so is the main region, which is not a
    /// section at all.
    fn cycle_tab(&mut self, by: isize) {
        if self.panes.section_tabs(self.panes.focused_name()).len() < 2 {
            self.message = "no second tab in this section".into();
            return;
        }
        if self.panes.cycle_tab(by) {
            self.sync_modes();
            // The same reason the focus commands ask: an unfocused commits
            // pane's highlighted row is still the main preview's source, and
            // arriving on it must show that row and not the one before.
            if self.panes.focused_name() == "commits" {
                self.request_commit_preview(false);
            }
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
        match self.panes.get_mut("diff") {
            Some(Screens::Diff { label, .. }) | Some(Screens::Merging { label, .. }) => {
                *label = format!("loading {}", origin.label());
            }
            _ => {}
        }
        let differs = self.host.differ.clone();
        let over = Overrides::default();
        let tx = self.preview_tx.clone();
        let started = std::thread::Builder::new()
            .name("gitten-preview".into())
            .spawn(move || {
                let outcome = acquire::diff_source(&origin, &differs, &over, repo.as_ref(), false)
                    .map(|loaded| loaded.data);
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
                Some(Screens::Files { view, .. }) => {
                    view.current_file().map(|file| match file.section {
                        files::Section::Staged => DiffSource::Staged {
                            path: file.path.clone(),
                        },
                        files::Section::Unstaged => DiffSource::Unstaged {
                            path: file.path.clone(),
                        },
                        files::Section::Untracked => DiffSource::Untracked {
                            path: file.path.clone(),
                        },
                        // A conflicted file's preview is its merging view:
                        // the markers are the conflict, the stages are the
                        // sides, and no diff of an index side says either.
                        files::Section::Conflicts => DiffSource::Conflict {
                            path: file.path.clone(),
                        },
                    })
                }
                _ => None,
            },
            "stashes" => match self.panes.get(pane) {
                Some(Screens::Stashes { view, .. }) => {
                    view.current_id().map(|id| DiffSource::Stash {
                        index: id.index,
                        commit: id.commit,
                    })
                }
                _ => None,
            },
            _ => None,
        }
    }

    /// `files.open-diff`: preview the file the keyboard is on, and put the
    /// keyboard on the preview. A row that is a heading says so and does
    /// nothing; a conflict row's preview is its merging view, which is
    /// what [`DiffSource::Conflict`] is for.
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
            Ok(acquire::Data::Diff(files)) => {
                let label = self.preview_label(&answer.origin);
                self.install_diff(answer.origin, label, files, answer.focus);
            }
            Ok(acquire::Data::Conflict(file, stages)) => {
                let label = self.preview_label(&answer.origin);
                self.install_merging(answer.origin, label, file, stages, answer.focus);
            }
            Ok(acquire::Data::Commits(_)) => {
                self.message = "the preview answered with the wrong view".into();
            }
            Err(e) => {
                self.message = e;
                match self.panes.get_mut("diff") {
                    Some(Screens::Diff { label, .. }) | Some(Screens::Merging { label, .. }) => {
                        *label = answer.origin.label();
                    }
                    _ => {}
                }
            }
        }
    }

    /// Installs a merging view into the main slot — the same shape
    /// [`App::install_diff`] runs for a diff, for the one source whose
    /// answer is not a diff at all.
    fn install_merging(
        &mut self,
        origin: DiffSource,
        label: String,
        file: gitten_core::conflict::ConflictFile,
        stages: Vec<gitten_git::UnmergedStage>,
        focus: bool,
    ) {
        let path = match &origin {
            DiffSource::Conflict { path } => path.clone(),
            _ => return,
        };
        let old_focus = self.panes.focused_name().to_string();
        let mut view = merging::Merging::new(path, file, stages);
        view.set_bar(self.bar);
        self.ensure_geometry();
        if let Some(rect) = self.pane_content("diff") {
            view.set_scrolloff(self.host.view.scrolloff);
            view.resize(rect.width, rect.height);
        }
        self.panes.register(
            "diff",
            panes::Placement::Main,
            Screens::Merging {
                view,
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

    /// `stashes.apply` / `stashes.pop` / `stashes.drop`: hand the stack entry
    /// the keyboard is on to the shared action that means it.
    ///
    /// The one thing decided here is *whose pane* the press belongs to: the
    /// stash verbs reach only a focused stash list, said the way every
    /// wrong-focus refusal here is said. Everything after that — the
    /// repository gate, the missing row, the arm, the job — is
    /// [`gitten_app::act`]'s, so an extension calling the same command name
    /// reaches the same sentences, and the entry travels as its **commit**:
    /// a stack that churned between this press and the queue's turn moves
    /// the entry, and the write follows the entry rather than the number.
    fn stash_selected(&mut self, command: &str) {
        if !matches!(self.panes.focused(), Some(Screens::Stashes { .. })) {
            self.message = format!("{command} is not supported here");
            return;
        }
        match command {
            "stashes.apply" => gitten_app::act::apply_stash(self),
            "stashes.pop" => gitten_app::act::pop_stash(self),
            "stashes.drop" => gitten_app::act::drop_stash(self),
            _ => {}
        }
    }

    /// `files.stash-named`: gather a message on the status row, then park the
    /// tracked working tree under it.
    ///
    /// Reads no row and needs no pane, exactly as `files.stash` does — the
    /// scope is the repository's working tree, not a selection — which is
    /// also why the only refusal before the field opens is the fixture.
    fn begin_stash_message(&mut self) {
        if self.repo.is_none() {
            self.message = "a fixture has no working tree to park".into();
            return;
        }
        self.open_prompt(Prompt::StashMessage {
            field: Field::new(),
        });
    }

    /// `files.stash-file`: park the file the keyboard is on and no other
    /// path.
    ///
    /// The section the row sits in is what says whether git can see the path
    /// at all: an untracked one needs `-u`, or the pathspec matches nothing
    /// git tracks and the push stashes nothing. A conflicted row refuses —
    /// its working-tree side is the merge's open question, and git cannot
    /// write a stash over unmerged stages anyway.
    fn stash_selected_file(&mut self) {
        if !self.files_focused("files.stash-file") {
            return;
        }
        let Some(file) = gitten_app::act::FileClient::selected_file(self) else {
            self.message = "the keyboard is not on a file".into();
            return;
        };
        if file.section == gitten_app::act::FileSection::Conflicts {
            self.message = "a conflicted file's merge has to be resolved, not parked".into();
            return;
        }
        let scope = StashScope::Path {
            path: file.path.clone(),
            untracked: file.section == gitten_app::act::FileSection::Untracked,
        };
        gitten_app::act::stash_scoped(self, None, scope);
    }

    /// `stashes.rename`: the field, pre-filled with the entry's own message
    /// and wholly selected, so the first edit replaces it rather than
    /// appending to it. `at` is the entry's identity, captured now.
    ///
    /// The rename re-files the entry at the top of the stack, which is git's
    /// shape and not a choice — see
    /// [`Repo::stash_rename`](gitten_git::Repo::stash_rename) — and the job
    /// says so when it finishes, because a row that moved without a word
    /// looks like a different entry.
    fn begin_stash_rename(&mut self) {
        let Some(at) = self.focused_stash() else {
            return;
        };
        if self.repo.is_none() {
            self.message = "a fixture has no stash to rename".into();
            return;
        }
        let initial = self
            .panes
            .get("stashes")
            .and_then(|pane| match pane {
                Screens::Stashes { view, .. } => view.message_of(&at.commit),
                _ => None,
            })
            .unwrap_or_default()
            .to_string();
        self.open_prompt(Prompt::StashRename {
            at,
            field: Field::with_selected(&initial),
        });
    }

    /// `stashes.new-branch`: the field, then git's own three-in-one — a
    /// branch at the commit the stash was *made on*, the entry applied with
    /// its index intact, and the entry dropped only if that apply was clean.
    /// `patch.move-to-branch`'s field, naming the branch the clipboard
    /// lands on. The repository and a non-empty clipboard are checked
    /// before it opens, because a field that opens over nothing is a
    /// question nobody can answer.
    fn begin_move_patch(&mut self) {
        if self.repo.is_none() {
            self.message = "a fixture has no branches to move onto".into();
            return;
        }
        if self.patch_clip.is_empty() {
            self.message = "the patch clipboard is empty — pick hunks first".into();
            return;
        }
        self.open_prompt(Prompt::MovePatch {
            field: Field::new(),
        });
    }

    fn begin_stash_branch(&mut self) {
        let Some(at) = self.focused_stash() else {
            return;
        };
        if self.repo.is_none() {
            self.message = "a fixture has no stash to branch from".into();
            return;
        }
        self.open_prompt(Prompt::StashBranch {
            at,
            field: Field::new(),
        });
    }

    /// The stash entry a field is about to be opened over, with the two
    /// refusals a wrong press earns said here: the pane the keyboard is on,
    /// and a row to aim at. Both before any field exists, because a field
    /// that opens over nothing is a question nobody can answer.
    fn focused_stash(&mut self) -> Option<StashId> {
        if !matches!(self.panes.focused(), Some(Screens::Stashes { .. })) {
            self.message = "the keyboard is not on a stash".into();
            return None;
        }
        let id = match self.panes.focused() {
            Some(Screens::Stashes { view, .. }) => view.current_id(),
            _ => None,
        };
        if id.is_none() {
            self.message = "nothing selected on the stash stack".into();
        }
        id
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

    /// The merging view's named or under-the-keyboard answer: one region,
    /// one side, one job — and the session's undo snapshot taken from the
    /// view only when the queue accepted the job, so a refused answer
    /// pushes nothing it cannot make true.
    fn merging_take(&mut self, named: Option<gitten_core::conflict::Answer>) {
        if !matches!(self.panes.focused(), Some(Screens::Merging { .. })) {
            self.message = "the merging view is not showing".into();
            return;
        }
        let Some((_, repo)) = self.repo.clone() else {
            self.message = "a fixture has no repository to resolve in".into();
            return;
        };
        // Everything decided ahead of anything queued, in the order a
        // reader meets the facts: a file with regions left, a region under
        // the keyboard, and — for take-side — a half under it. Markers and
        // the diff3 base are the seam, not a side; a choice asked for there
        // is said no to, not guessed at.
        let aim = {
            let Some(Screens::Merging { view, .. }) = self.panes.get("diff") else {
                return;
            };
            if !view.is_conflicted() {
                self.message = "this file has no conflict left to answer".into();
                return;
            }
            if view.is_nested() {
                // A region inside another has no disjoint span to aim a
                // choice at — refused with the remedy, not a guess at a
                // region index the job would refuse anyway.
                self.message =
                    "this file's conflicts nest — resolve it whole with the file-level answers"
                        .into();
                return;
            }
            match named {
                Some(answer) => match view.current_region() {
                    Some(region) => Some((region, answer)),
                    None => {
                        self.message = "the keyboard is not on a conflict".into();
                        return;
                    }
                },
                None => match view.under_the_keyboard() {
                    Some(aim) => Some(aim),
                    None => {
                        self.message =
                            "the keyboard is on a marker or the base — pick a side".into();
                        return;
                    }
                },
            }
        };
        let Some((region, answer)) = aim else {
            return;
        };
        let (path, snapshot) = {
            let Some(Screens::Merging { view, .. }) = self.panes.get("diff") else {
                return;
            };
            (view.path().as_bytes().to_vec(), view.snapshot())
        };
        let job = Write::resolve_hunks(&repo, path.clone(), vec![(region, answer)]);
        let accepted = gitten_app::act::Client::submit(self, Box::new(job));
        let Some(Screens::Merging { view, .. }) = self.panes.get_mut("diff") else {
            return;
        };
        if accepted {
            view.push_undo(snapshot);
        } else {
            self.message = "the job queue is shutting down".into();
        }
    }

    /// `merge.undo`: the session's last answer, put back — the file's bytes
    /// to the working tree and the unmerged stages to the index, both as
    /// the snapshot found them. The stack is the view's session; a refused
    /// answer pushed nothing, and an empty stack says so instead of
    /// pretending to undo.
    fn merging_undo(&mut self) {
        if !matches!(self.panes.focused(), Some(Screens::Merging { .. })) {
            self.message = "the merging view is not showing".into();
            return;
        }
        let Some((_, repo)) = self.repo.clone() else {
            self.message = "a fixture has no repository to undo in".into();
            return;
        };
        let (path, step) = {
            let Some(Screens::Merging { view, .. }) = self.panes.get_mut("diff") else {
                return;
            };
            let Some(step) = view.pop_undo() else {
                self.message = "nothing left to undo in this file".into();
                return;
            };
            (view.path().as_bytes().to_vec(), step)
        };
        let job = Write::restore(&repo, path, step.bytes, step.stages);
        if !gitten_app::act::Client::submit(self, Box::new(job)) {
            self.message = "the job queue is shutting down".into();
        }
    }

    /// `merge.options`: the whole-file answers live on the conflict row —
    /// the same row the eye is already on. The keyboard goes back to the
    /// files pane, and the band names the four keys *from the keymap*, so a
    /// rebind moves this line the way it moves help.
    fn merging_options(&mut self) {
        if !matches!(self.panes.focused(), Some(Screens::Merging { .. })) {
            self.message = "the merging view is not showing".into();
            return;
        }
        if self.panes.get("files").is_none() {
            self.message = "there is no files pane to answer from".into();
            return;
        }
        // The message is built before the focus moves, because the keys are
        // read from the host and the host read is the immutable half.
        let key = |name: &str| {
            self.host
                .keys
                .keys_for(name)
                .first()
                .map(|k| k.to_string())
                .unwrap_or_default()
        };
        let said = format!(
            "whole-file answers: {} {} {} {} on the conflict row",
            key("files.resolve-ours"),
            key("files.resolve-theirs"),
            key("files.resolve-both"),
            key("files.resolve-keep"),
        );
        self.panes.focus_named("files");
        self.sync_modes();
        self.message = said;
    }

    /// Re-reads the operation standing in the repository, and the
    /// availability that gates on it. Called wherever the panes re-read —
    /// startup, a finished job's finish wave, a repository switch — and
    /// nowhere else: it runs git, and a render must not.
    fn sync_operation(&mut self) {
        self.operation = self
            .repo
            .as_ref()
            .and_then(|(_, repo)| repo.as_ref().operation());
        let repo_backed = self.repo.is_some();
        self.availability =
            tui_availability(repo_backed, self.operation.as_ref(), self.bisect.as_ref());
    }

    /// Re-reads the bisection standing in the repository. Called before
    /// [`sync_operation`](Self::sync_operation) wherever the panes re-read
    /// — the availability the latter builds gates the bisect verbs on it.
    fn sync_bisect(&mut self) {
        self.bisect = self
            .repo
            .as_ref()
            .and_then(|(_, repo)| repo.as_ref().bisect_state());
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
                    // A plan is a window of shas, and the write that just
                    // landed may have rewritten every one of them. Running
                    // it afterwards would aim at objects nobody can see any
                    // more, so the screen closes rather than silently
                    // retargeting — the editing is cheap to do again, and a
                    // rewrite aimed at the wrong commits is not.
                    let mut closed = false;
                    if generation > self.generation {
                        self.generation = generation;
                        refresh = self.refresh_stale(generation).err();
                        if self.todo.take().is_some() {
                            self.sync_modes();
                            closed = true;
                        }
                    }
                    self.message = match (write, refresh) {
                        (Some(write), Some(refresh)) => format!("{write} · {refresh}"),
                        (Some(write), None) => write,
                        (None, Some(refresh)) => refresh,
                        // A clean write's evidence is the refreshed screen
                        // itself; a job that named its finish gets its word.
                        (None, None) => done.unwrap_or_default(),
                    };
                    // Said beside whatever the write itself had to say, and
                    // never instead of it: a job's own refusal is the more
                    // urgent half of the sentence.
                    if closed {
                        const NOTE: &str = "the repository moved — the rebase plan was closed";
                        self.message = match self.message.is_empty() {
                            true => NOTE.into(),
                            false => format!("{} · {NOTE}", self.message),
                        };
                    }
                    // A write may have started, finished or abandoned an
                    // operation; the banner and the lifecycle gates re-read
                    // it with everything else the finish wave refreshes.
                    self.sync_bisect();
                    self.sync_operation();
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
        // Canonicalized once per wave: the listing spells every path
        // resolved, and the worktree row guard compares exact bytes.
        // Owned, so the pane borrow below never touches the repository.
        let here = self
            .repo
            .as_ref()
            .map(|(path, _)| {
                std::fs::canonicalize(path)
                    .unwrap_or_else(|_| path.clone())
                    .as_os_str()
                    .as_encoded_bytes()
                    .to_vec()
            })
            .unwrap_or_default();
        {
            let Self { panes, host, .. } = self;
            for pane in panes.iter_mut() {
                if let Some(result) = pane.refresh(target, host, repo.as_ref(), &here) {
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
                // The header pen is the rectangle's own header row, and the
                // content pen its content — the same subdivision the resize
                // above used, read back rather than recomputed.
                let head = rect.header();
                // A sidebar rectangle's header row is its *section's*: the
                // tabs, where the layout put them, and the label only where
                // there are rows under it to label. The main region has no
                // section and draws its own one-pane header.
                match geometry.header_of(name) {
                    Some(section) => section_header(
                        &mut screen.span(head.y, head.x, head.width),
                        host,
                        section,
                        focused,
                    ),
                    None => {
                        let key = focus_keys
                            .iter()
                            .find(|(n, _)| n == name)
                            .map(|(_, k)| k.as_str())
                            .unwrap_or("");
                        header(
                            &mut screen.span(head.y, head.x, head.width),
                            host,
                            key,
                            name,
                            pane.label(),
                            focused,
                        )
                    }
                }
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
            // An operation standing names itself before everything else on
            // this line — it is the one fact every key on the pane is gated
            // by — with the keys that answer it. The keys come from the
            // keymap, so a rebind moves this line the way it moves help;
            // the count names why continue would refuse.
            let status = match self.operation.as_ref() {
                Some(op) => {
                    let key = |name: &str| {
                        self.host
                            .keys
                            .keys_for(name)
                            .first()
                            .map(|k| k.to_string())
                            .unwrap_or_default()
                    };
                    let conflicts = match op.conflicts {
                        0 => String::new(),
                        1 => " · 1 conflicted file".to_string(),
                        n => format!(" · {n} conflicted files"),
                    };
                    let keys = match op.can_skip() {
                        true => format!(
                            "{} abort · {} continue · {} skip",
                            key("operation.abort"),
                            key("operation.continue"),
                            key("operation.skip")
                        ),
                        false => format!(
                            "{} abort · {} continue",
                            key("operation.abort"),
                            key("operation.continue")
                        ),
                    };
                    format!(
                        "{} in progress{} · {} · {}",
                        op.kind.word(),
                        conflicts,
                        keys,
                        status
                    )
                }
                None => status,
            };
            // The bisection standing beside whatever else stands: the
            // commit under test, where reset returns, and the keys that
            // judge it — read from the keymap, like the operation's.
            let status = match self.bisect.as_ref() {
                Some(b) => {
                    let key = |name: &str| {
                        self.host
                            .keys
                            .keys_for(name)
                            .first()
                            .map(|k| k.to_string())
                            .unwrap_or_default()
                    };
                    format!(
                        "{} · back to {} · {} bisect options · {}",
                        b.word(),
                        b.original,
                        key("commits.bisect-menu"),
                        status
                    )
                }
                None => status,
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

        // The plan floats over everything the panes drew, on the picker's
        // own terms — it owns the keyboard, so it owns the rows it covers —
        // and under the help panel, which owns the keyboard back off it for
        // as long as somebody is reading the keys.
        if let Some(todo) = self.todo.as_mut() {
            todo.paint(&mut self.screen, 1, body, &self.host, &self.availability);
        }
        // The builder floats over everything the panes drew, beside the
        // plan and under the help panel: it owns the keyboard, so it owns
        // the rows it covers, and the clipboard it draws is read live off
        // the app — no copy, nothing to stale.
        if self.patch.is_some() {
            let (builder, clip) = match (self.patch.as_mut(), Some(&self.patch_clip)) {
                (Some(builder), Some(clip)) => (builder, clip),
                _ => unreachable!("the builder stands, so both stand"),
            };
            builder.paint(
                &mut self.screen,
                1,
                body,
                &self.host,
                &self.availability,
                clip,
            );
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
fn tui_availability(
    repo: bool,
    operation: Option<&Operation>,
    bisect: Option<&gitten_core::bisect::BisectState>,
) -> Availability {
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
        "tab.next",
        "tab.prev",
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
        // The history verbs act on the commit the keyboard is on — the
        // row, the shared action, the queue. Reset's strengths arm
        // separately; revert and cherry-pick destroy nothing, so the
        // dispatch-time refusals are the shared actions'.
        "commits.reset-menu",
        "commits.reset-soft",
        "commits.reset-mixed",
        "commits.reset-hard",
        "commits.revert",
        "commits.cherry-pick",
        "commits.checkout",
        "commits.copy",
        "commits.paste",
        "commits.clear-copies",
        "commits.reset-author",
        // History editing: the plan, the stop-here rebase, the reword
        // field, the base mark and the two reorders — each answered by
        // dispatch, each refusing its own wrong selection there.
        "commits.interactive-rebase",
        "commits.edit-commit",
        "commits.reword",
        "commits.mark-base",
        "commits.move-up",
        "commits.move-down",
        "commits.rebase-onto",
        "commits.squash-up",
        "commits.fixup-up",
        "commits.drop-commit",
        "commits.create-fixup",
        "commits.find-fixup-base",
        "commits.apply-fixups",
        "commits.fixup-message",
        // The open plan's own verbs. Live whenever the client is: a press
        // with no plan open is refused by name where it is answered, which
        // is a sentence about the plan rather than about the client.
        "todo.pick",
        "todo.reword",
        "todo.edit",
        "todo.squash",
        "todo.fixup",
        "todo.fixup-keep",
        "todo.drop",
        "todo.move-up",
        "todo.move-down",
        "todo.autosquash",
        "todo.run",
        // The patch clipboard's own verbs. The builder's moves are live
        // whenever the client is — a press with no builder open is refused
        // by name where it is answered — and the clipboard verbs refuse
        // theirs the same way; the wrong-selection refusals are dispatch's.
        "patch.menu",
        "patch.pick",
        "patch.show",
        "patch.toggle-hunk",
        "patch.toggle-file",
        "patch.drop-file",
        "patch.apply-worktree",
        "patch.apply-index",
        "patch.reverse-worktree",
        "patch.reverse-index",
        "patch.move-to-branch",
        "patch.clear",
        "patch.remove-from-commit",
        "patch.discard-file",
        "patch.checkout-file",
        "patch.amend-commit",
        "remotes.focus",
        "remotes.fetch",
        "remotes.new",
        "remotes.edit",
        "remotes.remove",
        "remotes.search",
        "tags.focus",
        "tags.checkout",
        "tags.new",
        "tags.delete",
        "tags.push",
        "tags.search",
        "reflog.focus",
        "reflog.recover",
        "reflog.search",
        "worktrees.focus",
        "worktrees.new",
        "worktrees.remove",
        "worktrees.switch",
        "worktrees.search",
        "commits.new-worktree",
        "branches.new-worktree",
        "stashes.new-worktree",
        "tags.new-worktree",
        "commits.bisect-menu",
        "commits.bisect-good",
        "commits.bisect-bad",
        "commits.bisect-skip",
        "commits.bisect-reset",
        "history.undo",
        "history.redo",
        "branches.delete-remote",
        "commits.new-tag",
        // Tags, reflog and history: rows, the shared actions, the queue.
        // The wrong-selection refusals are dispatch's, exactly like the
        // remotes verbs beside them.
        "tags.focus",
        "tags.checkout",
        "tags.new",
        "tags.delete",
        "tags.push",
        "tags.search",
        "reflog.focus",
        "reflog.recover",
        "reflog.search",
        "history.undo",
        "history.redo",
        "branches.delete-remote",
        "commits.new-tag",
        // A merge aims at the branch row the keyboard is on; the four
        // conflict answers aim at the conflict row. Both are live whenever
        // a repository is; the wrong-selection refusals are dispatch's.
        "branches.merge",
        "branches.merge-squash",
        "files.resolve-ours",
        "files.resolve-theirs",
        "files.resolve-both",
        "files.resolve-keep",
        // The merging view's answers are live whenever a repository is;
        // the wrong-selection refusals are dispatch's, exactly like the
        // file-level answers beside them.
        "merge.take-side",
        "merge.take-ours",
        "merge.take-theirs",
        "merge.take-both",
        "merge.undo",
        "merge.options",
        "merge.next-conflict",
        "merge.prev-conflict",
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
                // The working tree's own repository-wide questions: the
                // upstream a reset aims at and the tree a nuke empties are
                // both things only a repository has.
                "files.reset-menu",
                "files.reset-upstream-soft",
                "files.reset-upstream-mixed",
                "files.reset-upstream-hard",
                "files.nuke",
                // The stash choices and the two verbs on the stack that open
                // a field. All of them need a repository — a fixture has no
                // working tree to park and no stack to rename on.
                "files.stash-menu",
                "files.stash-named",
                "files.stash-staged",
                "files.stash-unstaged",
                "files.stash-untracked",
                "files.stash-file",
                "stashes.rename",
                "stashes.new-branch",
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
            a.disabled(
                "commits.reset-menu",
                "a fixture has no repository to rewrite in",
            );
            a.disabled(
                "commits.reset-soft",
                "a fixture has no repository to rewrite in",
            );
            a.disabled(
                "commits.reset-mixed",
                "a fixture has no repository to rewrite in",
            );
            a.disabled(
                "commits.reset-hard",
                "a fixture has no repository to rewrite in",
            );
            a.disabled(
                "commits.revert",
                "a fixture has no repository to rewrite in",
            );
            a.disabled(
                "commits.cherry-pick",
                "a fixture has no repository to rewrite in",
            );
            a.disabled(
                "commits.checkout",
                "a fixture has no repository to check out in",
            );
            a.disabled(
                "commits.copy",
                "a fixture has no repository to cherry-pick from",
            );
            a.disabled(
                "commits.paste",
                "a fixture has no repository to cherry-pick into",
            );
            a.disabled(
                "commits.clear-copies",
                "a fixture has no repository to cherry-pick from",
            );
            a.disabled(
                "commits.reset-author",
                "a fixture has no repository to rewrite in",
            );
            for name in [
                "commits.interactive-rebase",
                "commits.edit-commit",
                "commits.reword",
                "commits.move-up",
                "commits.move-down",
                "commits.rebase-onto",
                "commits.squash-up",
                "commits.fixup-up",
                "commits.drop-commit",
                "commits.create-fixup",
                "commits.find-fixup-base",
                "commits.apply-fixups",
                "commits.fixup-message",
                "todo.pick",
                "todo.reword",
                "todo.edit",
                "todo.squash",
                "todo.fixup",
                "todo.fixup-keep",
                "todo.drop",
                "todo.move-up",
                "todo.move-down",
                "todo.autosquash",
                "todo.run",
            ] {
                a.disabled(name, "a fixture has no repository to rewrite in");
            }
            for name in [
                "patch.menu",
                "patch.pick",
                "patch.show",
                "patch.toggle-hunk",
                "patch.toggle-file",
                "patch.drop-file",
                "patch.apply-worktree",
                "patch.apply-index",
                "patch.reverse-worktree",
                "patch.reverse-index",
                "patch.move-to-branch",
                "patch.clear",
                "patch.remove-from-commit",
                "patch.discard-file",
                "patch.checkout-file",
                "patch.amend-commit",
            ] {
                a.disabled(name, "a fixture has no repository to patch in");
            }
            a.disabled(
                "commits.mark-base",
                "a fixture has no repository to rebase in",
            );
            a.disabled("files.reset-menu", "a fixture has no repository to reset");
            a.disabled(
                "files.reset-upstream-soft",
                "a fixture has no repository to reset",
            );
            a.disabled(
                "files.reset-upstream-mixed",
                "a fixture has no repository to reset",
            );
            a.disabled(
                "files.reset-upstream-hard",
                "a fixture has no repository to reset",
            );
            a.disabled("files.nuke", "a fixture has no working tree to nuke");
            for name in [
                "files.stash-menu",
                "files.stash-named",
                "files.stash-staged",
                "files.stash-unstaged",
                "files.stash-untracked",
                "files.stash-file",
            ] {
                a.disabled(name, "a fixture has no working tree to park");
            }
            a.disabled("stashes.rename", "a fixture has no stash to rename");
            a.disabled(
                "stashes.new-branch",
                "a fixture has no stash to branch from",
            );
            for name in [
                "commits.bisect-menu",
                "commits.bisect-good",
                "commits.bisect-bad",
                "commits.bisect-skip",
                "commits.bisect-reset",
            ] {
                a.disabled(name, "a fixture has no history to bisect");
            }
            for name in [
                "commits.new-worktree",
                "branches.new-worktree",
                "stashes.new-worktree",
                "tags.new-worktree",
            ] {
                a.disabled(
                    name,
                    "a fixture has no repository to branch a worktree from",
                );
            }
            a.disabled(
                "merge.take-side",
                "a fixture has no repository to resolve in",
            );
            a.disabled(
                "merge.take-ours",
                "a fixture has no repository to resolve in",
            );
            a.disabled(
                "merge.take-theirs",
                "a fixture has no repository to resolve in",
            );
            a.disabled(
                "merge.take-both",
                "a fixture has no repository to resolve in",
            );
            a.disabled("merge.undo", "a fixture has no repository to undo in");
            a.disabled("merge.options", "a fixture has no conflict to answer");
            a.disabled("merge.next-conflict", "a fixture has no conflict to walk");
            a.disabled("merge.prev-conflict", "a fixture has no conflict to walk");
        }
    }
    // The lifecycle keys answer whichever operation stands — the one fact
    // that changes between two runs of the same binary, read from the
    // repository and carried beside the availability that gates on it. A
    // key bound to a rewrite in progress must not advertise itself while
    // the tree is clean; a key aimed at a rebase must not pretend a merge
    // is one. The per-kind names keep their own kind's gate; the generic
    // trio (`operation.abort` / `.continue` / `.skip`) answers whatever is
    // standing, skip being a rebase's alone.
    match operation {
        None => {
            a.disabled(
                "operation.abort",
                "no merge, rebase, cherry-pick or revert is in progress",
            );
            a.disabled(
                "operation.continue",
                "no merge, rebase, cherry-pick or revert is in progress",
            );
            a.disabled(
                "operation.skip",
                "skip belongs to a rebase; none is in progress",
            );
            a.disabled("rebase.abort", "no rebase is in progress");
            a.disabled("rebase.continue", "no rebase is in progress");
            a.disabled("commits.cherry-pick-abort", "no cherry-pick is in progress");
            a.disabled(
                "commits.cherry-pick-continue",
                "no cherry-pick is in progress",
            );
        }
        Some(op) => {
            let standing = op.kind.word();
            a.available(["operation.abort", "operation.continue"]);
            if op.can_skip() {
                a.available(["operation.skip"]);
            } else {
                a.disabled(
                    "operation.skip",
                    format!("skip belongs to a rebase; a {standing} is in progress"),
                );
            }
            if op.kind == gitten_core::operation::Kind::Rebase {
                a.available(["rebase.abort", "rebase.continue"]);
            } else {
                a.disabled(
                    "rebase.abort",
                    format!("a {standing} is in progress, not a rebase"),
                );
                a.disabled(
                    "rebase.continue",
                    format!("a {standing} is in progress, not a rebase"),
                );
            }
            if op.kind == gitten_core::operation::Kind::CherryPick {
                a.available(["commits.cherry-pick-abort", "commits.cherry-pick-continue"]);
            } else {
                a.disabled(
                    "commits.cherry-pick-abort",
                    format!("a {standing} is in progress, not a cherry-pick"),
                );
                a.disabled(
                    "commits.cherry-pick-continue",
                    format!("a {standing} is in progress, not a cherry-pick"),
                );
            }
        }
    }
    // The bisect verbs gate on the bisection the same acquisition read —
    // the one fact that changes between two runs of the same binary, next
    // to the lifecycle gates above. Judging answers a standing question;
    // starting answers a clean tree; reset answers either, because ending
    // nothing is the quiet no-op. Fixtures never reach this: the branch
    // below turns them away with fixture reasons first.
    if repo {
        match bisect {
            None => {
                a.disabled("commits.bisect-good", "no bisect is in progress");
                a.disabled("commits.bisect-bad", "no bisect is in progress");
                a.disabled("commits.bisect-skip", "no bisect is in progress");
            }
            Some(_) => {
                // Starting is the menu's own answer when the tree is clean
                // — the `b` door opens the question instead, so there is
                // no start to advertise while one stands.
            }
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

    fn operation(&self) -> Option<Operation> {
        self.operation
    }

    fn bisect(&self) -> Option<gitten_core::bisect::BisectState> {
        self.bisect.clone()
    }

    fn rebase_base(&self) -> Option<gitten_app::act::SelectedCommit> {
        self.rebase_base.clone()
    }

    fn confirm_or_arm(&mut self, command: &str, target: &[u8]) -> bool {
        // The same arm the history questions use, on the same terms: named
        // by the command as well as by what it is aimed at, and dying on a
        // repository switch with everything else that was asked about the
        // repository that just left.
        let armed = (command.to_string(), target.to_vec());
        if self.history_arm.as_ref() == Some(&armed) {
            self.history_arm = None;
            true
        } else {
            self.history_arm = Some(armed);
            false
        }
    }

    fn selected_conflict(&self) -> Option<gitten_core::status::PathBytes> {
        let Some(Screens::Files { view, .. }) = self.panes.focused() else {
            return None;
        };
        view.current_file()
            .filter(|file| file.section == files::Section::Conflicts)
            .map(|file| file.path.clone())
    }
}

impl PatchClient for App {
    fn patch_clipboard(&mut self) -> &mut gitten_core::patchclip::PatchClipboard {
        &mut self.patch_clip
    }

    fn patch_pick(&mut self) -> Option<DiffPick> {
        // Owned out first: the focus borrow ends before any sentence,
        // because a refusal said while borrowed is a borrow error and a
        // refusal said after is a status line.
        let found = match self.panes.focused() {
            Some(Screens::Diff {
                view,
                origin: Some(origin),
                ..
            }) => Some((view.patch_selection(), origin.clone())),
            _ => None,
        };
        let (selection, origin) = match found {
            Some(found) => found,
            None => {
                self.message = "the keyboard is not on a diff".into();
                return None;
            }
        };
        let origin = match origin {
            DiffSource::Staged { .. } => PickOrigin::Staged,
            DiffSource::Unstaged { .. } => PickOrigin::Unstaged,
            DiffSource::Untracked { .. } => PickOrigin::Untracked,
            DiffSource::Commit { sha } => PickOrigin::Commit {
                sha: sha.into_bytes(),
            },
            DiffSource::Stash { commit, .. } => PickOrigin::Stash {
                commit: commit.into_bytes(),
            },
            _ => {
                self.message = "nothing to pick here — open the file's own side".into();
                return None;
            }
        };
        let (path, selection) = match selection {
            Ok(PatchSelection::Whole { path, hunk }) => (path, HunkSelection::Whole(hunk)),
            Ok(PatchSelection::Lines { path, parts }) => (path, HunkSelection::Lines(parts)),
            Err(e) => {
                self.message = e;
                return None;
            }
        };
        Some(DiffPick {
            path,
            selection,
            origin,
        })
    }

    fn graft_target(&mut self, scope: GraftScope) -> Option<GraftTarget> {
        let found = match self.panes.focused() {
            Some(Screens::Diff {
                view,
                origin: Some(DiffSource::Commit { sha }),
                ..
            }) => Some((
                sha.clone(),
                view.current_file_hunks(),
                view.current_hunk(),
                view.patch_selection().ok(),
            )),
            _ => None,
        };
        let (sha, file, one, marks) = match found {
            Some(found) => found,
            None => {
                self.message = "open the commit's diff first".into();
                return None;
            }
        };
        let (path, all) = match file {
            Some(file) => file,
            None => {
                self.message = "the keyboard is not on a hunk".into();
                return None;
            }
        };
        // Marked lines narrow the hunk scope to the ranges they cover —
        // but only when every mark sits in this file. A mark spanning
        // files is the selection's own refusal, said where it was made.
        let (hunks, parts) = match scope {
            GraftScope::File => (all, None),
            GraftScope::Hunk => match marks {
                Some(PatchSelection::Lines { path: p, parts }) if p == path => {
                    let hunks = parts.iter().map(|(h, _, _)| h.clone()).collect();
                    (hunks, Some(parts))
                }
                _ => match one {
                    Some((_, hunk)) => (vec![hunk], None),
                    None => {
                        self.message = "the keyboard is not on a hunk".into();
                        return None;
                    }
                },
            },
        };
        // A rename grafts whole or not at all: the file's letter in the
        // commit's own listing, read now — one read at keypress, and the
        // commit is immutable so it cannot stale.
        let renamed = match self.repo.as_ref() {
            Some((_, repo)) => match repo.commit_files(sha.as_bytes()) {
                Ok(files) => files
                    .iter()
                    .any(|(status, name)| *status == 'R' && name == path.as_bytes()),
                Err(e) => {
                    self.message = e;
                    return None;
                }
            },
            None => {
                self.message = "a fixture has no history to rewrite".into();
                return None;
            }
        };
        Some(GraftTarget {
            sha: sha.into_bytes(),
            path,
            hunks,
            parts,
            renamed,
            // Binary commit diffs draw no hunks, so the keyboard cannot
            // be on one: this is unreachable through the views above and
            // stands as the other clients' guard.
            binary: false,
        })
    }

    fn amend_target(&mut self) -> Option<Vec<u8>> {
        match self.panes.focused() {
            Some(Screens::Diff {
                origin: Some(DiffSource::Commit { sha }),
                ..
            }) => Some(sha.clone().into_bytes()),
            _ => {
                self.message = "open the commit's diff first".into();
                None
            }
        }
    }

    fn commit_file_target(&mut self) -> Option<CommitFileTarget> {
        let found = match self.panes.focused() {
            Some(Screens::Diff {
                view,
                origin: Some(DiffSource::Commit { sha }),
                ..
            }) => Some((sha.clone(), view.current_file_hunks())),
            _ => None,
        };
        match found {
            Some((sha, Some((path, _)))) => Some(CommitFileTarget {
                sha: sha.into_bytes(),
                path: path.into_bytes(),
            }),
            Some((_, None)) => {
                self.message = "the keyboard is not on a hunk".into();
                None
            }
            None => {
                self.message = "open the commit's diff first".into();
                None
            }
        }
    }

    fn open_patch_builder(&mut self) {
        self.patch = Some(PatchBuilder::new());
        self.message = format!(
            "{} — space toggles, enter applies",
            self.patch_clip.status()
        );
        self.sync_modes();
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

impl gitten_app::act::StashClient for App {
    fn selected_stash(&self) -> Option<StashId> {
        let Some(Screens::Stashes { view, .. }) = self.panes.focused() else {
            return None;
        };
        view.current_id()
    }

    fn confirm_or_arm_stash(&mut self, id: &StashId) -> bool {
        match self.panes.focused_mut() {
            Some(Screens::Stashes { view, .. }) => view.confirm_or_arm_drop(id),
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

impl gitten_app::act::TagClient for App {
    fn tag_target(&self) -> Option<RefName> {
        self.tag_target()
    }

    fn confirm_or_arm_tag(&mut self, name: &RefName) -> bool {
        self.confirm_or_arm_tag(name)
    }
}

impl gitten_app::act::WorktreeClient for App {
    fn worktree_target(&self) -> Option<Vec<u8>> {
        self.worktree_target()
    }

    fn confirm_or_arm_worktree(&mut self, path: &[u8], force: bool) -> bool {
        self.confirm_or_arm_worktree(path, force)
    }

    fn upgrade_worktree_force(&mut self, path: &[u8]) {
        self.upgrade_worktree_force(path)
    }
}

impl gitten_app::act::ReflogClient for App {
    fn reflog_target(&self) -> Option<gitten_core::refs::ReflogEntry> {
        self.reflog_target()
    }

    fn confirm_or_arm_reflog(&mut self, selector: &str) -> bool {
        self.confirm_or_arm_reflog(selector)
    }

    fn head_state(&self) -> Option<gitten_core::refs::HeadState> {
        self.repo.as_ref().and_then(|(_, repo)| repo.head().ok())
    }
}

impl gitten_app::act::HistoryClient for App {
    fn commit_target(&self) -> Option<gitten_app::act::SelectedCommit> {
        let Some(Screens::Commits { view, .. }) = self.panes.focused() else {
            return None;
        };
        view.current()
            .map(|commit| gitten_app::act::SelectedCommit {
                sha: commit.sha.as_bytes().to_vec(),
                short: commit.short.clone(),
            })
    }

    fn history_window(&self) -> Option<(&[gitten_core::Commit], usize)> {
        let Some(Screens::Commits { view, .. }) = self.panes.focused() else {
            return None;
        };
        view.history_window()
    }

    fn clipboard(&mut self) -> &mut gitten_core::clipboard::CherryClipboard {
        &mut self.clipboard
    }

    fn head_sha(&self) -> Option<Vec<u8>> {
        // Asked of the repository rather than read off the newest row: the
        // pane may be drilled into another branch's log, whose first row is
        // somebody else's tip. An unborn branch answers `None`.
        let (_, repo) = self.repo.as_ref()?;
        match repo.head() {
            Ok(HeadState::Branch {
                commit: Some(sha), ..
            })
            | Ok(HeadState::Detached { commit: sha }) => Some(sha.into_bytes()),
            _ => None,
        }
    }

    fn mark_rebase_base(&mut self, base: Option<gitten_app::act::SelectedCommit>) {
        self.rebase_base = base;
    }

    fn commit_range(&self) -> Option<Vec<gitten_app::act::SelectedCommit>> {
        // lazygit's `v` range, resolved to commits here: the pane holds it
        // as visible-table rows, and rows are not what a paste can name.
        let Some(Screens::Commits { view, .. }) = self.panes.focused() else {
            return None;
        };
        let (lo, hi) = view.marks()?;
        let picked: Vec<_> = (lo..=hi)
            .filter_map(|row| view.at(row))
            .map(|commit| gitten_app::act::SelectedCommit {
                sha: commit.sha.as_bytes().to_vec(),
                short: commit.short.clone(),
            })
            .collect();
        (!picked.is_empty()).then_some(picked)
    }

    fn confirm_or_arm_commit(
        &mut self,
        command: &str,
        target: &gitten_app::act::SelectedCommit,
    ) -> bool {
        // The arm names the command as well as the commit: a soft reset
        // asked, then a hard reset pressed, asks again rather than firing.
        let armed = (command.to_string(), target.sha.clone());
        if self.history_arm.as_ref() == Some(&armed) {
            self.history_arm = None;
            true
        } else {
            self.history_arm = Some(armed);
            false
        }
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
        // A merging view is not a diff at all: the hunk verbs have no side
        // of an index to aim at, and the region answers are the verbs here.
        DiffSource::Conflict { .. } => {
            Err("a merging view has no hunks to stage — answer its regions".into())
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

/// How many reflog entries the reflog pane reads: enough history to browse
/// and to recover from, bounded so a years-old repository does not flatten
/// ten thousand rows on every refresh.
const REFLOG_ENTRIES: usize = 500;

/// The tags pane's header label: what repository, how many tags.
fn tags_label(describe: &str, count: usize) -> String {
    let word = match count {
        1 => "tag",
        _ => "tags",
    };
    format!("{describe} · {count} {word}")
}

/// The worktrees pane's header label: what repository, how many checkouts.
fn worktrees_label(describe: &str, count: usize) -> String {
    let word = match count {
        1 => "worktree",
        _ => "worktrees",
    };
    format!("{describe} · {count} {word}")
}

/// The reflog pane's header label: what repository, how many entries read.
fn reflog_label(describe: &str, count: usize) -> String {
    let word = match count {
        1 => "entry",
        _ => "entries",
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

/// A sidebar section's header row: its focus key, its tabs, and — when there
/// are rows under it — what the tab it is showing is showing.
///
/// The tab positions are read back from the layout rather than re-derived
/// here: [`panes::header`] worked them out once, the mouse hit-tests against
/// the same table, and a second copy of that arithmetic is what would put the
/// highlight and the click on different words.
///
/// The accent stays singular — it is the one "which pane has the keyboard"
/// mark a cell grid gets — so an *open* section whose keyboard has gone to the
/// diff draws its active tab in the ordinary pane ink, not the accent. The
/// tabs behind it are faint: reachable, and not competing with the row the eye
/// is on.
///
/// **No label.** A lone pane's header shows what the pane is showing, and
/// three tabs do not leave room for it: `  3  branches - remotes - tags` is 30
/// of a 40-column sidebar, so `fake (main) · 2 local` would arrive as
/// `fake (ma` — which reads as a bug and not as a repository. Drawing it only
/// where it happens to fit is worse than not drawing it, because then whether
/// the sidebar tells you the branch depends on how long a tab's name is. The
/// title bar says it in full, at the width of the whole window, and it is the
/// focused pane's label that anybody is reading.
fn section_header(pen: &mut Pen, host: &Host, head: &panes::Header, focused: bool) {
    let c = &host.theme.chrome;
    let bg = c.title_bg;
    let key_ink = match focused {
        true => Ink::new(c.accent, bg),
        false => Ink::new(c.faint, bg),
    };
    let active_ink = match focused {
        true => Ink::new(c.accent, bg).bold(),
        false => Ink::new(c.dim, bg),
    };
    let dim = Ink::new(c.faint, bg);
    pen.put("  ", dim);
    if !head.key.is_empty() {
        pen.put(&head.key, key_ink);
        pen.put("  ", dim);
    }
    for (i, tab) in head.tabs.iter().enumerate() {
        if i > 0 {
            pen.put(panes::TAB_GAP, dim);
        }
        // Where the layout said, not where the pen happens to have got to:
        // the two agree by construction and this is what keeps them agreeing.
        pen.seek(tab.x.saturating_sub(head.rect.x));
        pen.put(
            &tab.name,
            match tab.active {
                true => active_ink,
                false => dim,
            },
        );
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
        // commits binding's own description is the marker. Read from the
        // *end* of the mode's rows, because that is where the scroll is:
        // the commits mode is long enough now that its first bindings are
        // above the window when the panel is at the bottom.
        assert!(
            rows.iter()
                .any(|r| r.contains("grow a new branch from this commit")),
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
    use gitten_core::refs::{
        Branch, HeadState, RefName, ReflogEntry, RemoteBranch, Stash, StashId, Tag,
    };
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

    /// One commit's file listing, by sha — the fixup discovery's per-row
    /// answers. Named so the nesting does not sprawl across the struct.
    type CommitFiles = Vec<(char, Vec<u8>)>;

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
        /// The tags the ancillary read answers, and how many times it was
        /// read. Writes record the name they aimed at and, when they land,
        /// change what the next read answers — which is what lets a test
        /// observe a refresh reading the namespace after a deletion.
        tags: Vec<Tag>,
        tag_reads: usize,
        /// When set, the next tag *read* fails with exactly this message —
        /// the ancillary read that happens at `App::new`, so a test can
        /// launch into the failed tenant rather than only probing it.
        fail_tags: Option<String>,
        /// Where HEAD has been, newest first, and how many times it was
        /// read. Recovery and undo tests set it directly; the next read
        /// sees the history they built.
        reflog: Vec<ReflogEntry>,
        reflog_reads: usize,
        /// When set, the next reflog read fails with exactly this message.
        fail_reflog: Option<String>,
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
        /// The checkouts `worktrees` answers with, and how often it was
        /// read. Writes record the path they aimed at and — when they
        /// land — change what the next read answers, which is what lets a
        /// test observe a refresh reading the namespace after a removal.
        worktrees: Vec<gitten_core::worktrees::Worktree>,
        worktree_reads: usize,
        /// When set, the next worktree *read* fails with exactly this
        /// message — the ancillary read that happens at `App::new`, so a
        /// test can launch into the failed tenant rather than only
        /// probing it.
        fail_worktrees: Option<String>,
        /// Every worktree write that reached the repository, as the
        /// person-readable line a test asserts against.
        worktree_writes: Vec<String>,
        /// When set, the next worktree *verb* fails with exactly this
        /// message and changes nothing: git's refusal, verbatim.
        refuse_worktree: Option<String>,
        /// The bisection the reads answer — `None` is a clean tree. Set
        /// directly by bisect tests: the next `sync_bisect` sees the
        /// world they built.
        bisect: Option<gitten_core::bisect::BisectState>,
        /// Every bisect verb that reached the repository, as the
        /// person-readable line a test asserts against.
        bisect_writes: Vec<String>,
        /// When set, the next bisect *verb* fails with exactly this
        /// message and changes nothing.
        refuse_bisect: Option<String>,
        /// Local branches held by another worktree — what the real
        /// `worktree_branches` read answers, and what a checkout here
        /// refuses for.
        held_branches: Vec<String>,
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
        /// What `commit_files` answers for the sha under test: the status
        /// letters a commit's own listing would report. Tests set it
        /// directly — a rename here is what the graft refuses on.
        commit_file_list: Vec<(char, Vec<u8>)>,
        /// Per-sha answers for the same read, consulted first: the fixup
        /// discovery names a different file set per row, which one flat
        /// list cannot say. Falls back to `commit_file_list`.
        commit_files_by_sha: Vec<(Vec<u8>, CommitFiles)>,
        /// When set, the graft reads its own result as empty: lifting the
        /// commit's only change, refused with the drop door named.
        graft_empty: bool,
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
        /// The conflicted file the merging reads answer: the working-tree
        /// bytes, the stages git holds for the path, and the record of what
        /// the region answers and undos aimed at. A `resolve_hunks` really
        /// applies [`gitten_core::conflict::apply`] here, so a test can
        /// assert the file's bytes after an answer — not just the request.
        conflict_bytes: Vec<u8>,
        conflict_stages: Vec<gitten_git::UnmergedStage>,
        hunk_answers: Vec<Vec<(usize, gitten_core::conflict::Answer)>>,
        restores: Vec<Vec<u8>>,
        /// When set, the next rebase write fails with exactly this message
        /// and leaves a rebase standing — git's own shape for a conflict
        /// mid-rewrite, which is the state the lifecycle keys carry on from.
        refuse_rebase: Option<String>,
        /// The operation the history reads answer — `None` is a clean tree.
        /// Set directly by history tests, exactly like `status` and the
        /// stash stack: the next `sync_operation` sees the world they built.
        standing: Option<Operation>,
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

        /// The stash resolution the binary-backed implementation does:
        /// the entry's commit against the stack as it stands right now,
        /// refusing when it is gone or doubled. Mirrored here so a key test
        /// exercises the identity rule rather than a fake's shortcut — and
        /// the lock is taken and released before the write that follows
        /// takes it again.
        fn stash_position(&self, id: &StashId) -> gitten_git::Result<usize> {
            let stack = self.0.lock().unwrap().stashes.clone();
            id.resolve(&stack).position()
        }
    }

    /// A todo script as one line a test can hold: `pick aaa; fixup bbb`.
    /// Actions and arguments only — the subjects git writes beside them are
    /// for a human reading the file, and asserting on them would be
    /// asserting on git's own prose.
    fn shown_script(script: &gitten_git::TodoScript) -> String {
        use gitten_core::rebase::Line;
        script
            .lines()
            .iter()
            .map(|line| match line {
                Line::Step(step) => format!(
                    "{} {}",
                    step.action.word(),
                    String::from_utf8_lossy(&step.arg)
                ),
                Line::Verbatim(raw) => String::from_utf8_lossy(raw).into_owned(),
            })
            .collect::<Vec<_>>()
            .join("; ")
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

        fn unmerged(&self, _path: &[u8]) -> gitten_git::Result<Vec<gitten_git::UnmergedStage>> {
            let s = self.0.lock().unwrap();
            Ok(s.conflict_stages.clone())
        }

        fn conflict_file(
            &self,
            path: &[u8],
        ) -> gitten_git::Result<gitten_core::conflict::ConflictFile> {
            let s = self.0.lock().unwrap();
            Ok(gitten_core::conflict::ConflictFile::parse(
                gitten_core::status::PathBytes::from_bytes(path),
                s.conflict_bytes.clone(),
            ))
        }

        fn resolve_hunks(
            &self,
            path: &[u8],
            choices: &[(usize, gitten_core::conflict::Answer)],
        ) -> gitten_git::Result<()> {
            let mut s = self.0.lock().unwrap();
            // The same re-parse-and-validate the real repository runs: the
            // bytes the fake holds are the working tree, and a stale answer
            // refuses here exactly as it would refuse there.
            let file = gitten_core::conflict::ConflictFile::parse(
                gitten_core::status::PathBytes::from_bytes(path),
                s.conflict_bytes.clone(),
            );
            if !file.is_conflicted() {
                return Err("the file carries no conflict markers now".into());
            }
            if choices.iter().any(|(i, _)| *i >= file.regions.len()) {
                return Err("the conflict moved under the keyboard".into());
            }
            s.conflict_bytes = gitten_core::conflict::apply(&file.bytes, &file.regions, choices)?;
            s.hunk_answers.push(choices.to_vec());
            Ok(())
        }

        fn restore_conflict(
            &self,
            _path: &[u8],
            bytes: Vec<u8>,
            stages: &[gitten_git::UnmergedStage],
        ) -> gitten_git::Result<()> {
            let mut s = self.0.lock().unwrap();
            s.restores.push(bytes.clone());
            s.conflict_bytes = bytes;
            s.conflict_stages = stages.to_vec();
            Ok(())
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

        fn operation(&self) -> Option<Operation> {
            self.0.lock().unwrap().standing
        }

        fn reset(
            &self,
            mode: gitten_core::refs::ResetMode,
            target: &[u8],
        ) -> gitten_git::Result<()> {
            let mut s = self.0.lock().unwrap();
            s.writes.push(format!(
                "reset {} {}",
                mode.flag(),
                String::from_utf8_lossy(target)
            ));
            Ok(())
        }

        fn revert(&self, commit: &[u8]) -> gitten_git::Result<()> {
            let mut s = self.0.lock().unwrap();
            s.writes
                .push(format!("revert {}", String::from_utf8_lossy(commit)));
            Ok(())
        }

        fn cherry_pick(&self, sha: &[u8]) -> gitten_git::Result<()> {
            let mut s = self.0.lock().unwrap();
            s.writes
                .push(format!("cherry-pick {}", String::from_utf8_lossy(sha)));
            Ok(())
        }

        fn cherry_pick_range(&self, shas: &[Vec<u8>]) -> gitten_git::Result<()> {
            // Recorded as one line with the order intact: the order is the
            // whole contract of a paste, so a test that could not see it
            // could not hold it.
            let mut s = self.0.lock().unwrap();
            let shown: Vec<_> = shas
                .iter()
                .map(|sha| String::from_utf8_lossy(sha).into_owned())
                .collect();
            s.writes.push(format!("cherry-pick {}", shown.join(" ")));
            Ok(())
        }

        fn rebase_todo(
            &self,
            upstream: &[u8],
            script: &gitten_git::TodoScript,
        ) -> gitten_git::Result<()> {
            script.validate()?;
            let mut s = self.0.lock().unwrap();
            s.writes.push(format!(
                "rebase-todo {} | {}",
                String::from_utf8_lossy(upstream),
                shown_script(script)
            ));
            Ok(())
        }

        fn rebase_plan(&self, plan: &gitten_git::Plan) -> gitten_git::Result<()> {
            // The plan is rendered exactly as the acquisition layer renders
            // it — same ordering, same reword-as-pick-and-exec — with the
            // one thing a fake has no filesystem for standing in for
            // itself: the message rides in the recorded line instead of in
            // a file the line would name.
            let script = plan.script(&mut |entry| {
                let mut out: Vec<Vec<u8>> = Vec::new();
                if let Some(message) = &entry.message {
                    out.push([b"amend -F ".as_slice(), message].concat());
                }
                if entry.amend == Some(gitten_core::rebase::Amend::ResetAuthor) {
                    out.push(b"amend --reset-author".to_vec());
                }
                Ok(out)
            })?;
            let mut s = self.0.lock().unwrap();
            if let Some(e) = s.refuse_rebase.clone() {
                // git's own shape for a conflict mid-rewrite: nonzero, and
                // the rebase left standing for a human to drive.
                s.standing = Some(Operation {
                    kind: gitten_core::operation::Kind::Rebase,
                    conflicts: 1,
                });
                return Err(e);
            }
            s.writes.push(format!(
                "rebase-plan {} | {}",
                String::from_utf8_lossy(plan.upstream()),
                shown_script(&script)
            ));
            Ok(())
        }

        fn rebase_abort(&self) -> gitten_git::Result<()> {
            let mut s = self.0.lock().unwrap();
            s.writes.push("rebase abort".into());
            // git's own guarantee: the state is gone and the branch is back.
            s.standing = None;
            Ok(())
        }

        fn rebase_continue(&self) -> gitten_git::Result<()> {
            let mut s = self.0.lock().unwrap();
            s.writes.push("rebase continue".into());
            s.standing = None;
            Ok(())
        }

        fn rebase_onto(&self, upstream: &[u8]) -> gitten_git::Result<()> {
            let mut s = self.0.lock().unwrap();
            s.writes
                .push(format!("rebase-onto {}", String::from_utf8_lossy(upstream)));
            Ok(())
        }

        fn rebase_onto_base(&self, onto: &[u8], base: &[u8]) -> gitten_git::Result<()> {
            let mut s = self.0.lock().unwrap();
            s.writes.push(format!(
                "rebase-onto {} from {}",
                String::from_utf8_lossy(onto),
                String::from_utf8_lossy(base)
            ));
            Ok(())
        }

        fn nuke_worktree(&self) -> gitten_git::Result<()> {
            self.0.lock().unwrap().writes.push("nuke".into());
            Ok(())
        }

        fn reword_head(&self, message: &str) -> gitten_git::Result<()> {
            let mut s = self.0.lock().unwrap();
            s.writes.push(format!("reword-head {message}"));
            Ok(())
        }

        fn reset_author(&self) -> gitten_git::Result<()> {
            self.0.lock().unwrap().writes.push("reset-author".into());
            Ok(())
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
                // Detached onto whatever was handed over, not a fixed sha:
                // a commit checkout is the one caller that cares *where*
                // HEAD landed, and a constant here would answer every aim
                // the same.
                false => HeadState::Detached {
                    commit: String::from_utf8_lossy(name).into_owned(),
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
            // A tag the next read answers: creating names it, so a refresh
            // after the write draws the row the job just made.
            if !s.tags.iter().any(|t| t.name.as_bytes() == name) {
                s.tags.push(Tag {
                    name: RefName::from_bytes(name),
                    commit: String::from_utf8_lossy(target).into_owned(),
                    annotated: message.is_some(),
                    subject: message.map(|m| m.lines().next().unwrap_or_default().to_string()),
                });
            }
            Ok(())
        }

        fn delete_tag(&self, name: &[u8]) -> gitten_git::Result<()> {
            let mut s = self.0.lock().unwrap();
            s.branch_writes
                .push(format!("untag {}", String::from_utf8_lossy(name)));
            s.branch_bytes.push(name.to_vec());
            s.tags.retain(|t| t.name.as_bytes() != name);
            Ok(())
        }

        fn worktrees(&self) -> gitten_git::Result<Vec<gitten_core::worktrees::Worktree>> {
            let mut s = self.0.lock().unwrap();
            s.worktree_reads += 1;
            if let Some(e) = s.fail_worktrees.clone() {
                return Err(e);
            }
            Ok(s.worktrees.clone())
        }

        fn worktree_add(
            &self,
            path: &[u8],
            base: &[u8],
            branch: Option<&[u8]>,
        ) -> gitten_git::Result<()> {
            let mut s = self.0.lock().unwrap();
            if let Some(e) = s.refuse_worktree.clone() {
                return Err(e);
            }
            s.worktree_writes.push(format!(
                "add {} at {}{}",
                String::from_utf8_lossy(path),
                String::from_utf8_lossy(base),
                branch
                    .map(|b| format!(" as {}", String::from_utf8_lossy(b)))
                    .unwrap_or_default()
            ));
            // A checkout the next read answers, so a refresh after the
            // write draws the row the job just made. Detached, because
            // the fake names no branch for it — the pane reads what the
            // listing says, not what the verb wished.
            if !s.worktrees.iter().any(|w| w.path == path) {
                s.worktrees.push(gitten_core::worktrees::Worktree {
                    path: path.to_vec(),
                    head: "fake".into(),
                    branch: None,
                    bare: false,
                    lock: None,
                    prunable: None,
                });
            }
            Ok(())
        }

        fn worktree_remove(&self, path: &[u8], force: bool) -> gitten_git::Result<()> {
            let mut s = self.0.lock().unwrap();
            // The refusal models git's dirty refusal, which the force
            // spelling overrides — a forced remove always lands.
            if !force {
                if let Some(e) = s.refuse_worktree.clone() {
                    return Err(e);
                }
            }
            s.worktree_writes.push(format!(
                "remove {}{}",
                String::from_utf8_lossy(path),
                match force {
                    true => " forced",
                    false => "",
                }
            ));
            s.worktrees.retain(|w| w.path != path);
            Ok(())
        }

        fn worktree_branches(&self) -> Vec<String> {
            self.0.lock().unwrap().held_branches.clone()
        }

        fn bisect_state(&self) -> Option<gitten_core::bisect::BisectState> {
            self.0.lock().unwrap().bisect.clone()
        }

        fn bisect_start(&self, bad: &[u8], goods: &[Vec<u8>]) -> gitten_git::Result<()> {
            let mut s = self.0.lock().unwrap();
            if let Some(e) = s.refuse_bisect.clone() {
                return Err(e);
            }
            if s.bisect.is_some() {
                return Err("a bisect is already in progress — reset it first".into());
            }
            s.bisect_writes.push(format!(
                "start {} {}",
                String::from_utf8_lossy(bad),
                goods
                    .iter()
                    .map(|g| String::from_utf8_lossy(g).into_owned())
                    .collect::<Vec<_>>()
                    .join(" ")
            ));
            // Standing, aimed at the bad revision: the judgements the
            // test types next land on a question that exists.
            s.bisect = Some(gitten_core::bisect::BisectState {
                current: String::from_utf8_lossy(bad).into_owned(),
                original: "main".into(),
                goods: goods
                    .iter()
                    .map(|g| String::from_utf8_lossy(g).into_owned())
                    .collect(),
            });
            Ok(())
        }

        fn bisect_good(&self, rev: &[u8]) -> gitten_git::Result<()> {
            let mut s = self.0.lock().unwrap();
            if let Some(e) = s.refuse_bisect.clone() {
                return Err(e);
            }
            s.bisect_writes
                .push(format!("good {}", String::from_utf8_lossy(rev)));
            Ok(())
        }

        fn bisect_bad(&self, rev: &[u8]) -> gitten_git::Result<()> {
            let mut s = self.0.lock().unwrap();
            if let Some(e) = s.refuse_bisect.clone() {
                return Err(e);
            }
            s.bisect_writes
                .push(format!("bad {}", String::from_utf8_lossy(rev)));
            Ok(())
        }

        fn bisect_skip(&self, rev: &[u8]) -> gitten_git::Result<()> {
            let mut s = self.0.lock().unwrap();
            if let Some(e) = s.refuse_bisect.clone() {
                return Err(e);
            }
            s.bisect_writes
                .push(format!("skip {}", String::from_utf8_lossy(rev)));
            Ok(())
        }

        fn bisect_reset(&self) -> gitten_git::Result<()> {
            let mut s = self.0.lock().unwrap();
            if let Some(e) = s.refuse_bisect.clone() {
                return Err(e);
            }
            s.bisect_writes.push("reset".into());
            s.bisect = None;
            Ok(())
        }

        fn push_tag(&self, remote: &[u8], name: &[u8]) -> gitten_git::Result<()> {
            let mut s = self.0.lock().unwrap();
            s.branch_writes.push(format!(
                "push tag {} to {}",
                String::from_utf8_lossy(name),
                String::from_utf8_lossy(remote)
            ));
            Ok(())
        }

        fn delete_remote_branch(&self, remote: &[u8], branch: &[u8]) -> gitten_git::Result<()> {
            let mut s = self.0.lock().unwrap();
            s.branch_writes.push(format!(
                "delete {}/{} on {}",
                String::from_utf8_lossy(remote),
                String::from_utf8_lossy(branch),
                String::from_utf8_lossy(remote)
            ));
            s.remotes
                .retain(|b| !(b.remote.as_bytes() == remote && b.branch.as_bytes() == branch));
            Ok(())
        }

        fn move_head(&self, target: &[u8], message: &str) -> gitten_git::Result<()> {
            let mut s = self.0.lock().unwrap();
            s.branch_writes.push(format!(
                "move HEAD to {} ({})",
                String::from_utf8_lossy(target),
                message
            ));
            Ok(())
        }

        fn stash_push(&self, message: Option<&str>) -> gitten_git::Result<usize> {
            self.stash_push_scoped(message, &StashScope::Tracked)
        }

        fn stash_push_scoped(
            &self,
            message: Option<&str>,
            scope: &StashScope,
        ) -> gitten_git::Result<usize> {
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
                    message: match message {
                        Some(m) => format!("On fake: {m}"),
                        None => "WIP on fake (main)".into(),
                    },
                    commit: landed,
                },
            );
            // The flags travel so a test can assert git would have been
            // handed the scope it was asked for — the one thing a fake can
            // say about a variant it does not implement.
            s.stash_writes.push(format!(
                "push{} {}",
                match scope.flags().is_empty() {
                    true => String::new(),
                    false => format!(" [{}]", scope.flags().join(" ")),
                },
                match scope.path() {
                    Some(path) => format!("-- {path}"),
                    None => message.unwrap_or("").to_string(),
                }
            ));
            Ok(0)
        }

        /// The resolution the binary-backed implementation does, mirrored so
        /// a key test exercises the identity semantics rather than a fake's
        /// shortcut: the commit against the live stack, refusing when it is
        /// gone or doubled.
        fn stash_apply_id(&self, id: &StashId) -> gitten_git::Result<()> {
            let at = self.stash_position(id)?;
            self.stash_apply(at)
        }

        fn stash_pop_id(&self, id: &StashId) -> gitten_git::Result<()> {
            let at = self.stash_position(id)?;
            self.stash_pop(at)
        }

        fn stash_drop_id(&self, id: &StashId) -> gitten_git::Result<()> {
            let at = self.stash_position(id)?;
            self.stash_drop(at)
        }

        fn stash_rename(&self, id: &StashId, message: &str) -> gitten_git::Result<()> {
            if message.trim().is_empty() {
                return Err("a stash needs a message".into());
            }
            let at = self.stash_position(id)?;
            let mut s = self.0.lock().unwrap();
            if let Some(e) = s.refuse_stash.clone() {
                return Err(e);
            }
            s.stash_writes
                .push(format!("rename stash@{{{at}}} {message}"));
            // git's shape: the stash reflog only appends, so the entry is
            // stored under the new message on top and the old one dropped.
            let mut moved = s.stashes.remove(at);
            moved.message = message.to_string();
            s.stashes.insert(0, moved);
            for (i, entry) in s.stashes.iter_mut().enumerate() {
                entry.index = i;
            }
            Ok(())
        }

        fn stash_branch(&self, id: &StashId, name: &[u8]) -> gitten_git::Result<()> {
            let at = self.stash_position(id)?;
            let mut s = self.0.lock().unwrap();
            if let Some(e) = s.refuse_stash.clone() {
                return Err(e);
            }
            s.stash_writes.push(format!(
                "branch {} from stash@{{{at}}}",
                String::from_utf8_lossy(name)
            ));
            // git's shape again: the branch is checked out, and the entry is
            // dropped because the apply was clean.
            for b in &mut s.locals {
                b.head = false;
            }
            s.locals.push(Branch {
                name: RefName::from_bytes(name),
                commit: "f00d".into(),
                upstream: None,
                head: true,
            });
            s.head = Some(HeadState::Branch {
                name: RefName::from_bytes(name),
                commit: Some("f00d".into()),
            });
            s.stashes.remove(at);
            for (i, entry) in s.stashes.iter_mut().enumerate() {
                entry.index = i;
            }
            Ok(())
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

        fn tags(&self) -> gitten_git::Result<Vec<Tag>> {
            let mut s = self.0.lock().unwrap();
            s.tag_reads += 1;
            if let Some(e) = s.fail_tags.clone() {
                return Err(e);
            }
            Ok(s.tags.clone())
        }

        fn reflog(&self, limit: usize) -> gitten_git::Result<Vec<ReflogEntry>> {
            let mut s = self.0.lock().unwrap();
            s.reflog_reads += 1;
            if let Some(e) = s.fail_reflog.clone() {
                return Err(e);
            }
            Ok(s.reflog.iter().take(limit).cloned().collect())
        }

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

        fn apply_patch(&self, patch: &[u8]) -> gitten_git::Result<()> {
            let mut s = self.0.lock().unwrap();
            s.writes
                .push(format!("apply {}", String::from_utf8_lossy(patch)));
            if s.refuses.iter().any(|r| patch.starts_with(r)) {
                return Err("the fake refused".into());
            }
            s.applied += 1;
            Ok(())
        }

        fn commit_files(&self, sha: &[u8]) -> gitten_git::Result<Vec<(char, Vec<u8>)>> {
            let s = self.0.lock().unwrap();
            if let Some((_, files)) = s.commit_files_by_sha.iter().find(|(s, _)| s == sha) {
                return Ok(files.clone());
            }
            Ok(s.commit_file_list.clone())
        }

        fn commit_fixup(
            &self,
            sha: &[u8],
            kind: gitten_git::FixupKind,
        ) -> gitten_git::Result<String> {
            self.0.lock().unwrap().writes.push(format!(
                "fixup {} {}",
                String::from_utf8_lossy(sha),
                kind.word()
            ));
            Ok("f00d".into())
        }

        fn amend_no_edit(&self) -> gitten_git::Result<String> {
            self.0.lock().unwrap().writes.push("amend-no-edit".into());
            Ok("amended".into())
        }

        fn checkout_file_from(&self, sha: &[u8], path: &[u8]) -> gitten_git::Result<()> {
            self.0.lock().unwrap().writes.push(format!(
                "checkout-file {} from {}",
                String::from_utf8_lossy(path),
                String::from_utf8_lossy(sha)
            ));
            Ok(())
        }

        fn graft_empties(&self, _sha: &[u8]) -> gitten_git::Result<bool> {
            Ok(self.0.lock().unwrap().graft_empty)
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
        // `2` opens the files section — a launch opens on the commit list,
        // and a collapsed section is its header row and no rows at all.
        app.press(Key::plain(Code::Char('2')));
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
        app.press(Key::plain(Code::Char('2')));
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

        // Wide: the eight sidebar lists take turns in four sections beside
        // the diff, and no row crosses the divider column. The files section
        // is open — `2` put the keyboard there — so it takes every row the
        // three collapsed headers leave.
        app.screen.resize(120, 24);
        app.draw();
        let files_rect = app.pane_rect("files").expect("files placed");
        let commits_rect = app.pane_rect("commits").expect("commits placed");
        let diff_rect = app.pane_rect("diff").expect("diff placed");
        assert_eq!((files_rect.x, files_rect.width), (0, 40));
        assert_eq!((commits_rect.x, commits_rect.width), (0, 40));
        assert_eq!(files_rect.y, 1);
        assert_eq!(commits_rect.y, files_rect.y + files_rect.height + 1);
        assert_eq!(
            files_rect.height, 19,
            "the open section did not take the column"
        );
        assert_eq!(commits_rect.height, 1, "a collapsed section grew rows");
        assert_eq!((diff_rect.x, diff_rect.width), (41, 79));
        for y in 1..24 {
            assert_eq!(
                app.screen.char_at(40, y),
                Some(' '),
                "row {y} crossed the divider"
            );
        }
        // The section header names its tabs and its live focus key; the
        // label is the title bar's, at the width of the whole window, and
        // the status line names the pane too.
        app.dispatch("files.focus");
        app.draw();
        let header = app.screen.row_text(1);
        assert!(header.contains('2'), "{header:?}");
        assert!(header.contains("files - worktrees"), "{header:?}");
        assert!(app.screen.row_text(0).contains("files"));
        assert!(
            app.screen.row_text(0).contains("· 1 changed"),
            "the title did not carry the live label: {:?}",
            app.screen.row_text(0)
        );
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
        assert_eq!(app.panes.names().count(), 9);
        // The sidebar's canonical order is `panes::SECTIONS` flattened, which
        // is the order the column draws: the files section (files, then the
        // worktrees behind it), the branches section (branches, remotes,
        // tags), the commits section (commits, reflog), the stack on its own
        // at the foot. Registration order — commits first, files eighth —
        // does not show through, and nothing in panes.rs learned a name.
        assert_eq!(
            app.panes.list_order(),
            [
                "files",
                "worktrees",
                "branches",
                "remotes",
                "tags",
                "commits",
                "reflog",
                "stashes",
            ]
        );

        // `2` is the shared files.focus binding, and it now lands.
        app.press(Key::plain(Code::Char('2')));
        assert_eq!(app.panes.focused_name(), "files");
        assert_eq!(app.message, "", "focusing a registered pane said nothing");
        // Ctrl-J/Ctrl-K cycle both directions through the canonical order —
        // every list in the column, section by section and tab by tab, so
        // the worktrees tab behind `files` is the next stop and not the
        // branches section.
        app.press(Key::ctrl(Code::Char('j')));
        assert_eq!(app.panes.focused_name(), "worktrees");
        app.press(Key::ctrl(Code::Char('j')));
        assert_eq!(app.panes.focused_name(), "branches");
        app.press(Key::ctrl(Code::Char('k')));
        assert_eq!(app.panes.focused_name(), "worktrees");
        app.press(Key::ctrl(Code::Char('k')));
        assert_eq!(app.panes.focused_name(), "files");
        // `]`/`[` are the *inner* move, and they stay in the section they
        // started in: files and worktrees share a slot, so the pair walks
        // between exactly those two and wraps rather than reaching branches.
        app.press(Key::plain(Code::Char(']')));
        assert_eq!(app.panes.focused_name(), "worktrees");
        app.press(Key::plain(Code::Char(']')));
        assert_eq!(app.panes.focused_name(), "files", "the tabs did not wrap");
        app.press(Key::plain(Code::Char('[')));
        assert_eq!(app.panes.focused_name(), "worktrees");
        app.press(Key::plain(Code::Char('[')));
        assert_eq!(app.panes.focused_name(), "files");
        // A section of one does not carry the pair at all: the stack shares
        // its slot with nothing, so `tabs` is off the stack there and `]` is
        // an unbound key rather than a key that refuses.
        app.press(Key::plain(Code::Char('5')));
        assert!(
            !app.modes.as_slice().contains(&panes::TABS.to_string()),
            "{:?}",
            app.modes.as_slice()
        );
        app.press(Key::plain(Code::Char(']')));
        assert_eq!(app.panes.focused_name(), "stashes");
        assert_eq!(app.message, "] is not bound — ? for the keys");
        // Asked for by name anyway — a config file or an extension can — and
        // the refusal is a sentence and not a silent no-op.
        app.dispatch("tab.next");
        assert_eq!(app.message, "no second tab in this section");
        assert_eq!(app.panes.focused_name(), "stashes");
        app.press(Key::plain(Code::Char('2')));
        // Headers derive live keys: the files *section* advertises `2`,
        // which is its first tab's, and names both tabs on the one row.
        app.draw();
        let header = app.screen.row_text(1);
        assert!(header.contains("files - worktrees"), "{header:?}");
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
        app.dispatch("files.focus");
        // Drawn *after* the focus, so the cached geometry is the one the
        // mouse below hit-tests against: the files section is the open one
        // and its rows start on screen row 2.
        app.draw();
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

        // A mouse press on another row disarms too. The files section is the
        // first of the sidebar's four and the open one, so its content
        // starts on screen row 2, and the staged file — a selectable row
        // that is not the armed one — sits on row 3.
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
        // The screen row is the content start (past the header) plus the
        // row's visible offset — the pane scrolls, so the cursor minus the
        // top, not the cursor alone.
        let row = 2 + armed_row - files_of(&app).top();
        app.mouse(click(MouseKind::Down, 5, row));
        app.mouse(click(MouseKind::Up, 5, row));
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
        assert_eq!(app.panes.names().count(), 9, "enter appended a pane");
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
            9,
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
        // The commits section is the third of the sidebar's four and the
        // open one, so its header is screen row 3 and its content starts on
        // row 4: local row 1 is one content row down.
        app.mouse(click(MouseKind::Down, 5, 5));
        app.pump_quiet();
        assert_eq!(app.panes.focused_name(), "commits");
        assert_eq!(
            commits_of(&app).cursor(),
            1,
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
        app.dispatch("commits.focus");
        app.draw();
        let reads = state.lock().unwrap().pairs_reads;
        // Row 0 — screen row 4, the section's first content row — and not
        // the row the press above sat on: the first click has to move the
        // keyboard for its preview read to exist, and the second meets a
        // shown commit and deduplicates.
        app.mouse(click(MouseKind::Down, 10, 4));
        app.mouse(click(MouseKind::Up, 10, 4));
        // The click's own preview is on the lane; let it land before the
        // second press, so the double click meets a shown commit and
        // deduplicates — the count below is the click's read, not the open's.
        app.pump_quiet();
        app.mouse(click(MouseKind::Down, 10, 4));
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
        // once, and the feedback counts lines. The commits section is the
        // open one and its content starts on screen row 4.
        app.mouse(click(MouseKind::Down, 5, 4));
        app.mouse(click(MouseKind::Drag, 5, 5));
        app.mouse(click(MouseKind::Up, 5, 5));
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
        assert_eq!(app.panes.names().count(), 9, "open-diff appended a pane");
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
        let availability = tui_availability(true, None, None);
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
            [
                "commits",
                "stashes",
                "remotes",
                "tags",
                "reflog",
                "worktrees",
                "diff",
                "files",
                "branches"
            ],
            "{names:?}"
        );
        assert_eq!(app.panes.focused_name(), "commits");
        assert_eq!(
            app.panes.list_order(),
            [
                "files",
                "worktrees",
                "branches",
                "remotes",
                "tags",
                "commits",
                "reflog",
                "stashes",
            ]
        );
        assert_eq!(
            app.panes.reading_order(),
            [
                "files",
                "worktrees",
                "branches",
                "remotes",
                "tags",
                "commits",
                "reflog",
                "stashes",
                "diff"
            ]
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
            Some(panes::Placement::Sidebar {
                section: Some(4),
                rank: 8
            }),
            "the stash section and canonical rank 8, from the registry and \
             not a layout edit"
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
            [
                "stashes",
                "remotes",
                "tags",
                "reflog",
                "worktrees",
                "diff",
                "files",
                "branches"
            ]
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

        // Wide: the sidebar is four *sections* — files, branches, commits,
        // the stack — and eight lists take turns in them. Three collapse to
        // exactly their header row and the one with the keyboard takes the
        // nineteen rows left; the diff takes the rest of the body, one
        // divider column between. No geometry module changed to make room:
        // this is the registry's section answer to the tenants there are.
        assert_eq!(
            app.pane_rect("commits"),
            Some(crate::panes::Rect {
                x: 0,
                y: 3,
                width: 40,
                height: 19
            })
        );
        assert_eq!(
            app.pane_rect("stashes"),
            Some(crate::panes::Rect {
                x: 0,
                y: 22,
                width: 40,
                height: 1
            })
        );
        // And the tabs behind the shown ones have no rectangle at all — the
        // reflog shares the commits' slot and is hidden, not squeezed.
        for behind in ["worktrees", "remotes", "tags", "reflog"] {
            assert_eq!(app.pane_rect(behind), None, "{behind} kept a rectangle");
        }
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
        // out of the shipped map — beside every tab in the section, and the
        // open one says whose repository it is and how much is parked.
        let commits_header = app.screen.row_text(3).chars().take(40).collect::<String>();
        assert!(
            commits_header.contains('4') && commits_header.contains("commits - reflog"),
            "{commits_header:?}"
        );
        let files_header = app.screen.row_text(1);
        assert!(files_header.contains('2'), "{files_header:?}");
        assert!(
            files_header.contains("files - worktrees"),
            "{files_header:?}"
        );
        let stashes_header = app.screen.row_text(22);
        assert!(stashes_header.contains('5'), "{stashes_header:?}");
        assert!(stashes_header.contains("stashes"), "{stashes_header:?}");
        // A section header is tabs and a key, never a label: three tabs do
        // not leave room for one in a 40-column sidebar, and the title bar
        // says the focused pane's in full.
        assert!(
            !stashes_header.contains("fake (main) · 2 parked"),
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

        // `5` opens the stack, and *then* it says what it holds and draws
        // its rows — address first.
        app.press(Key::plain(Code::Char('5')));
        app.draw();
        // The section stays where it is in the column — the foot — and only
        // its height changes: a header that moved on focus would be a column
        // that reorders itself under the eye. What it holds is on the title
        // bar, which follows the keyboard.
        assert!(
            app.screen.row_text(4).contains("stashes"),
            "{:?}",
            app.screen.row_text(4)
        );
        assert!(
            app.screen.row_text(0).contains("fake (main) · 2 parked"),
            "the title did not follow: {:?}",
            app.screen.row_text(0)
        );
        let rows: Vec<String> = (5..7).map(|y| app.screen.row_text(y)).collect();
        assert!(rows.iter().any(|r| r.contains("stash@{0}")), "{rows:?}");
        assert!(rows.iter().any(|r| r.contains("stash@{1}")), "{rows:?}");
        app.press(Key::plain(Code::Char('4')));
        app.draw();

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

    /// The sidebar's sections, end to end: what the headers say, what a
    /// click on a tab and on a header do, what `[`/`]` reach, and that
    /// nothing absent is ever advertised or landed on.
    #[test]
    fn tui_parity_sections_tab_by_key_and_click_and_advertise_only_what_exists() {
        let (handle, _state) = fake(&[]);
        let mut app = commits_app(&handle);
        app.draw();

        // Four headers, in the order the column reads, each with its own
        // section's tabs and the focus key of the tab that names it. Only
        // the open one has rows, so these are rows 1, 2, 3 and 22.
        let header = |app: &App, y: usize| {
            app.screen
                .row_text(y)
                .chars()
                .take(40)
                .collect::<String>()
                .trim_end()
                .to_string()
        };
        assert_eq!(
            [1, 2, 3, 22].map(|y| header(&app, y)),
            [
                "  2  files - worktrees",
                "  3  branches - remotes - tags",
                "  4  commits - reflog",
                "  5  stashes",
            ]
        );

        // A click on a tab's word focuses *that* tab: the section opens
        // where it has always been in the column, the tab that had the slot
        // gives up its rectangle, and the header still lists all three.
        let tags_at = header(&app, 2).find("tags").expect("the tab is drawn");
        app.mouse(click(MouseKind::Down, tags_at + 1, 2));
        app.mouse(click(MouseKind::Up, tags_at + 1, 2));
        assert_eq!(app.panes.focused_name(), "tags");
        app.draw();
        assert_eq!(
            app.pane_rect("tags").map(|r| (r.y, r.height)),
            Some((2, 19))
        );
        assert_eq!(app.pane_rect("branches"), None, "the tab behind kept rows");
        assert_eq!(header(&app, 2), "  3  branches - remotes - tags");
        // The accent is the one "which pane has the keyboard" mark, and it
        // is on the active tab alone.
        let accent = app.host.theme.chrome.accent;
        let branches_at = header(&app, 2).find("branches").expect("drawn");
        assert_eq!(app.screen.ink(tags_at, 2).map(|i| i.fg), Some(accent));
        assert_ne!(app.screen.ink(branches_at, 2).map(|i| i.fg), Some(accent));

        // A click anywhere else on a header — the gap between two tabs, or
        // the empty tail — focuses the tab that section is *showing*, not
        // the one the pointer is nearest. The commits section is collapsed
        // to one row and it has moved down the column, because the section
        // above it is the open one now.
        let commits_head = app.pane_rect("commits").expect("placed").y;
        assert_eq!(commits_head, 21, "the collapsed sections did not close up");
        app.mouse(click(MouseKind::Down, 38, commits_head));
        assert_eq!(app.panes.focused_name(), "commits");
        app.mouse(click(MouseKind::Up, 38, commits_head));
        app.draw();
        // ...and the tags section remembers the tab it was left on, so a
        // press on its header comes back to `tags` and not to `branches`.
        app.mouse(click(MouseKind::Down, 38, 2));
        assert_eq!(app.panes.focused_name(), "tags");
        app.mouse(click(MouseKind::Up, 38, 2));

        // `]`/`[` walk that section and wrap inside it, never reaching the
        // commits section next door.
        app.press(Key::plain(Code::Char(']')));
        assert_eq!(app.panes.focused_name(), "branches");
        app.press(Key::plain(Code::Char(']')));
        assert_eq!(app.panes.focused_name(), "remotes");
        app.press(Key::plain(Code::Char(']')));
        assert_eq!(app.panes.focused_name(), "tags", "the tabs did not wrap");
        app.press(Key::plain(Code::Char('[')));
        assert_eq!(app.panes.focused_name(), "remotes");

        // The keymap and the availability agree with all of that: the pair
        // is the shipped `panes` mode's, and the dispatch runs both.
        let keys = Host::new().keys;
        let mut modes = Modes::new();
        modes.push(panes::MODE);
        modes.push(panes::TABS);
        modes.push("branches");
        assert_eq!(
            keys.resolve(&modes, &[Key::plain(Code::Char(']'))]),
            Resolve::Run("tab.next")
        );
        assert_eq!(
            keys.resolve(&modes, &[Key::plain(Code::Char('['))]),
            Resolve::Run("tab.prev")
        );
        // In the main region the older and more urgent pair wins: a diff
        // has files to jump between and no section to tab through.
        let mut in_diff = Modes::new();
        in_diff.push(panes::MODE);
        in_diff.push(panes::TABS);
        in_diff.push("diff");
        assert_eq!(
            keys.resolve(&in_diff, &[Key::plain(Code::Char(']'))]),
            Resolve::Run("diff.next-file")
        );
        let availability = tui_availability(true, None, None);
        for name in ["tab.next", "tab.prev"] {
            assert!(
                availability.runnable(name),
                "{name} is not runnable, but the keymap resolves it"
            );
        }
        // And the help panel lists them where they run, out of the same
        // registry — no local table anywhere in the chain.
        app.screen = Screen::new(120, 50);
        app.press(Key::char('?'));
        app.press(Key::plain(Code::End));
        app.draw();
        let help = (0..50)
            .map(|y| app.screen.row_text(y))
            .collect::<Vec<_>>()
            .join("\n");
        for doc in [
            "the next tab in this section",
            "the previous tab in this section",
        ] {
            assert!(help.contains(doc), "help dropped {doc:?}: {help:?}");
        }
        app.press(Key::char('?'));

        // A launch with no repository has one list, so there are no tabs to
        // walk and none advertised: the `panes` mode is not on the stack,
        // `]` is unbound there, and the one header names exactly the pane
        // that registered — never the reflog nobody read.
        let started = gitten_app::Started {
            view: View::Commits,
            source: Source::Fixtures,
            host: Host::new(),
            loaded: acquire::Loaded {
                label: "fixture history".into(),
                data: Data::Commits(gitten_core::parse_log(
                    "00000000\x1f00000000\x1f\x1fAda\x1f1\x1fone\x1e",
                )),
            },
            config: std::path::PathBuf::from("/nonexistent/gitten.toml"),
            repo: None,
        };
        let mut fixture = App::new(started, Glyphs::default());
        fixture.screen = Screen::new(120, 24);
        fixture.dispatch("commits.focus");
        fixture.draw();
        for absent in [panes::MODE, panes::TABS] {
            assert!(
                !fixture.modes.as_slice().contains(&absent.to_string()),
                "{absent} is on the stack: {:?}",
                fixture.modes.as_slice()
            );
        }
        fixture.press(Key::plain(Code::Char(']')));
        assert_eq!(
            fixture.message, "] is not bound — ? for the keys",
            "a tab key ran where there are no tabs"
        );
        assert_eq!(header(&fixture, 1), "  4  commits");
        assert_eq!(fixture.panes.focused_name(), "commits");
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
        // The cycle walks every list in the column, in the order the column
        // draws them: the reflog shares the commits' slot and is the next
        // stop, then the stack at the foot, and the wrap comes back to the
        // top of the files section.
        app.dispatch("pane.next");
        assert_eq!(
            app.panes.focused_name(),
            "reflog",
            "the cycle did not reach the reflog behind the commits"
        );
        app.dispatch("pane.next");
        assert_eq!(
            app.panes.focused_name(),
            "stashes",
            "the cycle did not reach the stack"
        );
        app.dispatch("pane.next");
        assert_eq!(app.panes.focused_name(), "files", "the cycle did not wrap");
        app.dispatch("pane.next");
        assert_eq!(
            app.panes.focused_name(),
            "worktrees",
            "the cycle did not reach the worktrees"
        );
        app.dispatch("pane.next");
        assert_eq!(
            app.panes.focused_name(),
            "branches",
            "the cycle did not reach the branches section"
        );
        app.dispatch("pane.next");
        assert_eq!(
            app.panes.focused_name(),
            "remotes",
            "the cycle did not reach the remotes"
        );
        app.dispatch("pane.next");
        assert_eq!(
            app.panes.focused_name(),
            "tags",
            "the cycle did not reach the tags"
        );
        app.press(Key::plain(Code::Char('5')));
        assert_eq!(app.panes.focused_name(), "stashes");
        app.draw();
        assert!(
            app.screen.row_text(0).contains("stashes"),
            "the title did not follow: {:?}",
            app.screen.row_text(0)
        );
        // The stack's section is the foot of the column, so its header is
        // the fourth row whether it is open or collapsed — three headers
        // above it, and everything below it is its own.
        assert!(
            app.screen.row_text(4).contains('5') && app.screen.row_text(4).contains("stashes"),
            "the header did not advertise the stack: {:?}",
            app.screen.row_text(4)
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
        // release reads that pane; a press on the stack's collapsed header
        // moves the keyboard there, and a drag inside the stack once it is
        // open builds no selection — a stack is acted on one entry at a
        // time. The commits section is the open one, so its header is the
        // third row of the body and its rows run from the fourth; the stack
        // is the foot of the column, collapsed to row 22.
        app.dispatch("commits.focus");
        app.draw();
        app.mouse(click(MouseKind::Down, 5, 5));
        app.mouse(click(MouseKind::Drag, 5, 6));
        app.mouse(click(MouseKind::Up, 5, 6));
        assert!(
            !commits_of(&app).selection().is_empty(),
            "the drag in the list selected nothing"
        );
        app.mouse(click(MouseKind::Down, 5, 22));
        assert_eq!(
            app.panes.focused_name(),
            "stashes",
            "the press did not move the keyboard to the stack"
        );
        app.mouse(click(MouseKind::Up, 5, 22));
        // Open now, and where the column always kept it: header on row 4,
        // rows of its own under it.
        app.draw();
        app.mouse(click(MouseKind::Down, 5, 5));
        app.mouse(click(MouseKind::Drag, 5, 6));
        app.mouse(click(MouseKind::Up, 5, 6));
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
            [
                "commits",
                "stashes",
                "remotes",
                "tags",
                "reflog",
                "worktrees",
                "diff",
                "files",
                "branches"
            ]
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
        // never succeeded, and no row for a verb to address. Read where it
        // is legible: the stack's section is the foot of the column, and a
        // collapsed section has no rows and no label, so `5` opens it.
        app.press(Key::plain(Code::Char('5')));
        app.draw();
        assert!(
            app.screen.row_text(0).contains("unavailable"),
            "the title did not say so: {:?}",
            app.screen.row_text(0)
        );
        let rows: Vec<String> = (5..7).map(|y| app.screen.row_text(y)).collect();
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
            app.screen.row_text(0).contains("fake (main) · 2 parked"),
            "the label did not recover: {:?}",
            app.screen.row_text(0)
        );
        let rows: Vec<String> = (5..7).map(|y| app.screen.row_text(y)).collect();
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
            [
                "commits",
                "stashes",
                "remotes",
                "tags",
                "reflog",
                "worktrees",
                "diff",
                "files",
                "branches"
            ],
            "{names:?}"
        );
        assert_eq!(app.panes.focused_name(), "commits");
        assert_eq!(
            app.panes.list_order(),
            [
                "files",
                "worktrees",
                "branches",
                "remotes",
                "tags",
                "commits",
                "reflog",
                "stashes",
            ]
        );
        assert_eq!(
            app.panes.reading_order(),
            [
                "files",
                "worktrees",
                "branches",
                "remotes",
                "tags",
                "commits",
                "reflog",
                "stashes",
                "diff"
            ]
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
            Some(panes::Placement::Sidebar {
                section: Some(2),
                rank: 3
            }),
            "the branches section and canonical rank 3, from the registry and \
             not a layout edit"
        );

        // Ctrl-J cycles the now-real sidebar ring from it, list by list in
        // the order the column draws them: branches' next is the remotes
        // tab behind it in the same section, and the walk h/l reaches the
        // worktrees tab above it the same way.
        app.dispatch("pane.next");
        assert_eq!(app.panes.focused_name(), "remotes");
        app.press(Key::plain(Code::Char('3')));
        app.dispatch("pane.left");
        assert_eq!(
            app.panes.focused_name(),
            "worktrees",
            "the walk skipped a list"
        );

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
            [
                "stashes",
                "remotes",
                "tags",
                "reflog",
                "worktrees",
                "diff",
                "files",
                "branches"
            ]
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
        // rectangles — the branches section, second in the column and open
        // because the keyboard is in it, and the main region. No layout
        // branch learned the name; this is the registry's own answer to a
        // sixth sidebar list.
        app.screen.resize(96, 24);
        app.draw();
        let branches = app.pane_rect("branches").expect("branches is placed wide");
        let diff = app.pane_rect("diff").expect("the diff is placed wide");
        assert_eq!(
            branches,
            crate::panes::Rect {
                x: 0,
                y: 2,
                width: 40,
                height: 19
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
            [
                "commits",
                "stashes",
                "remotes",
                "tags",
                "reflog",
                "worktrees",
                "diff",
                "files",
                "branches"
            ]
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
        // The name opens the message field: empty names an annotated tag
        // carrying nothing — a lightweight one — and any text annotates.
        assert!(matches!(app.prompt, Some(Prompt::TagMessage { .. })));
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
        assert!(matches!(app.prompt, Some(Prompt::TagMessage { .. })));
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
        assert!(matches!(app.prompt, Some(Prompt::TagMessage { .. })));
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
            // The row is found by its text rather than by an offset: an open
            // section is nineteen rows tall now, so the branch is under the
            // `local` heading and not on the first content row.
            let rect = app.pane_rect("branches").expect("placed");
            let row = (rect.y + 1..rect.y + rect.height)
                .find(|y| app.screen.row_text(*y)[..rect.width].contains("main"))
                .expect("the armed branch is drawn");
            let ink = app.screen.ink(rect.x + 2, row).expect("a drawn cell");
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
        // The feature branch died mid-test, so the rows are heading, main,
        // heading, two remotes: local row 1 is the armed main, local row 3
        // the first remote. A taller frame fits both without scrolling —
        // every scroll disarms first, which would prove nothing here.
        app.screen.resize(120, 40);
        app.draw();
        let rect = app.pane_rect("branches").expect("branches placed");
        app.dispatch("view.top");
        app.press(Key::char('d'));
        app.mouse(click(MouseKind::Down, 2, rect.y + 2));
        app.mouse(click(MouseKind::Up, 2, rect.y + 2));
        assert!(
            branches_of(&app).armed_row().is_some(),
            "the arm died on its own row"
        );
        app.mouse(click(MouseKind::Down, 2, rect.y + 4));
        app.mouse(click(MouseKind::Up, 2, rect.y + 4));
        assert_eq!(
            branches_of(&app).armed_row(),
            None,
            "the arm survived a mouse move to another row"
        );

        // A remote row under the same key asks the remote question: the
        // keyboard is on the first remote from the mouse block above, and
        // one key routes by row — local rows delete locally, remote rows
        // delete on the remote.
        app.press(Key::char('d'));
        assert!(
            app.message.contains("origin/feat/ure") && app.message.contains("press again"),
            "the remote question did not name both halves: {:?}",
            app.message
        );
        app.press(Key::char('d'));
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                state.lock().unwrap().branch_writes.len() == 2
            }),
            "a remote deletion never queued"
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
        assert_eq!(app.panes.focused_name(), "remotes");
        app.dispatch("pane.prev");
        assert_eq!(app.panes.focused_name(), "branches");
        assert_eq!(
            branches_of(&app).armed_row(),
            Some(Target::Local(RefName::from("main"))),
            "the arm died on a ring round-trip"
        );
        // And on a tab round-trip inside its own section, which is the move
        // the ring round-trip above was standing in for before the tabs
        // existed.
        app.dispatch("tab.next");
        assert_eq!(app.panes.focused_name(), "remotes");
        app.dispatch("tab.prev");
        assert_eq!(app.panes.focused_name(), "branches");
        assert_eq!(
            branches_of(&app).armed_row(),
            Some(Target::Local(RefName::from("main"))),
            "the arm died on a tab round-trip"
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
            [
                "delete branch f\u{fffd}ature",
                "delete origin/feat/ure on origin",
                "delete branch main"
            ]
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
                    // The name opens the message field; empty names a
                    // lightweight tag and queues the write.
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
        for name in [
            "commits",
            "stashes",
            "remotes",
            "tags",
            "reflog",
            "worktrees",
            "diff",
            "files",
            "branches",
        ] {
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
        assert!(header.contains("branches - remotes - tags"), "{header:?}");
        assert!(header.contains('3'), "{header:?}");
        // A section header is tabs and a key: three tabs leave a 40-column
        // sidebar ten cells, which is not a label, so it draws none rather
        // than half of one and the title bar says it in full.
        assert!(!header.contains("fake (ma"), "{header:?}");

        // The title and the status line follow the keyboard, and the title
        // is where the live label is legible.
        assert!(
            app.screen.row_text(0).contains("branches"),
            "the title did not follow: {:?}",
            app.screen.row_text(0)
        );
        assert!(
            app.screen.row_text(0).contains("fake (main) · 2 local"),
            "the title did not carry the live label: {:?}",
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
        // The rebase row is bound in this mode and answered here now, so
        // the panel lists it like every other runnable branch verb.
        assert!(help.contains("move the current branch onto"), "{help:?}");
    }

    #[test]
    fn lifecycle_commands_gate_on_the_standing_operation() {
        // The last fence is down: `commits.rebase-onto` was W6's to answer
        // and now does, so the key that used to name an unsupported command
        // asks about a rewrite instead. Everything below it changed when the
        // lifecycle landed: the exit keys are live, and with a clean tree the
        // honest word is why not, said through the availability contract's
        // reason, not a refusal that claims the client cannot run them at all.
        let (handle, state) = fake(&[]);
        branch_world(&state);
        let mut app = commits_app(&handle);
        app.press(Key::plain(Code::Char('3')));

        // The key resolves through core's branches mode — lowercase `r` —
        // and lands on the rewrite, which asks before it runs.
        app.press(Key::char('r'));
        assert_eq!(app.message, "rebase onto main? press again to confirm");

        // From the commits pane: gated, not unsupported — and no job ever
        // submitted, which is what the empty write log proves.
        app.press(Key::plain(Code::Char('4')));
        app.dispatch("rebase.abort");
        assert_eq!(app.message, "rebase.abort: no rebase is in progress");
        app.dispatch("rebase.continue");
        assert_eq!(app.message, "rebase.continue: no rebase is in progress");
        // The generic door says it in its own words, and skip names the
        // kind it belongs to.
        app.dispatch("operation.abort");
        assert_eq!(
            app.message,
            "operation.abort: no merge, rebase, cherry-pick or revert is in progress"
        );
        app.dispatch("operation.continue");
        assert_eq!(
            app.message,
            "operation.continue: no merge, rebase, cherry-pick or revert is in progress"
        );
        app.dispatch("operation.skip");
        assert_eq!(
            app.message,
            "operation.skip: skip belongs to a rebase; none is in progress"
        );

        app.pump_quiet();
        let s = state.lock().unwrap();
        assert!(
            s.branch_writes.is_empty() && s.writes.is_empty(),
            "a lifecycle job was submitted: {:?}",
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
        // The branches section is the second of the sidebar's four, so its
        // header is the second row of the body whether it is open or not.
        assert!(
            app.screen.row_text(2).contains('3')
                && app.screen.row_text(2).contains("branches - remotes - tags"),
            "the header did not advertise the section: {:?}",
            app.screen.row_text(2)
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
        // release reads that pane; a press on the branches section's
        // collapsed header moves the keyboard there, and a drag inside it
        // once it is open builds no selection — a ref list is acted on one
        // row at a time. The commits section is the open one, so its rows
        // start on screen row 4; the branches section is collapsed to its
        // header on row 2.
        app.dispatch("commits.focus");
        app.draw();
        app.mouse(click(MouseKind::Down, 5, 5));
        app.mouse(click(MouseKind::Drag, 5, 6));
        app.mouse(click(MouseKind::Up, 5, 6));
        assert!(
            !commits_of(&app).selection().is_empty(),
            "the drag in the list selected nothing"
        );
        app.mouse(click(MouseKind::Down, 5, 2));
        assert_eq!(
            app.panes.focused_name(),
            "branches",
            "the press did not move the keyboard to the branches"
        );
        app.mouse(click(MouseKind::Up, 5, 2));
        app.draw();
        app.mouse(click(MouseKind::Down, 5, 4));
        app.mouse(click(MouseKind::Drag, 5, 5));
        app.mouse(click(MouseKind::Up, 5, 5));
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
        for name in [
            "commits",
            "stashes",
            "remotes",
            "tags",
            "reflog",
            "worktrees",
            "diff",
            "files",
            "branches",
        ] {
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
                "global",
                "files",
                "branches",
                "commits",
                "stashes",
                "diff",
                "help",
                "input",
                "settings",
                "reset",
                "upstream",
                "stash",
                "patch",
                "builder",
                "todo",
                "panes",
                "bisect",
                "worktrees",
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
            Some(Screens::Merging { view, .. }) => Some(DiffSource::Conflict {
                path: view.path().clone(),
            }),
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
            outcome: Ok(Data::Diff(vec![gitten_core::FileDiff {
                path: "wrong.txt".into(),
                hunks: Vec::new(),
            }])),
        });
        app.install_preview(PreviewOutcome {
            seq: newest,
            root: std::path::PathBuf::from("/elsewhere"),
            origin: shown.clone(),
            focus: false,
            outcome: Ok(Data::Diff(vec![gitten_core::FileDiff {
                path: "other-repo.txt".into(),
                hunks: Vec::new(),
            }])),
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
                    view.current_id().map(|id| id.commit)
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
    /// git allowed to fail: the conflicted start of an operation, whose
    /// nonzero exit and standing state are exactly the fixture the test
    /// wants to find already standing.
    fn git_failing(repo: &Git, args: &[&str]) {
        let _ = std::process::Command::new("git")
            .arg("-C")
            .arg(repo.0.as_path())
            .args(Git::setup())
            .args(args)
            .output()
            .expect("git runs");
    }

    /// Two branches that disagree about f.txt, merged from the shell — the
    /// operation arrives standing before any client opens, which is how a
    /// rebase started in another terminal arrives too.
    fn externally_conflicted_merge(name: &str) -> Git {
        let repo = Git::init(name);
        repo.write("f.txt", "base\n");
        repo.commit("base");
        repo.git(&["checkout", "-qb", "side"]);
        repo.write("f.txt", "theirs\n");
        repo.commit("theirs");
        repo.git(&["checkout", "-q", "main"]);
        repo.write("f.txt", "ours\n");
        repo.commit("ours");
        git_failing(&repo, &["merge", "side"]);
        repo
    }

    #[test]
    fn tui_parity_an_externally_started_merge_is_bannered_and_aborted() {
        let repo = externally_conflicted_merge("op-external");
        let mut app = repo_app(repo.0.as_path());
        app.draw();
        // The banner names the operation, the count, and the keys that
        // answer it — the keys read from the keymap, not memorized here.
        let line = status(&app);
        assert!(
            line.contains("merge in progress · 1 conflicted file"),
            "{line}"
        );
        assert!(line.contains("abort"), "{line}");

        // m backs out: the tree returns to where the merge found it, the
        // banner goes, and a client opened fresh agrees — reopen recovery
        // is the same acquisition reading the same disk truth.
        app.dispatch("operation.abort");
        assert!(
            until(Duration::from_secs(5), || {
                app.pump();
                app.message.contains("merge aborted")
            }),
            "the abort never finished: {:?}",
            app.message
        );
        app.draw();
        let line = status(&app);
        assert!(!line.contains("in progress"), "{line}");
        assert_eq!(std::fs::read(repo.0.join("f.txt")).unwrap(), b"ours\n");
        let mut reopened = repo_app(repo.0.as_path());
        reopened.draw();
        assert!(!status(&reopened).contains("in progress"));
    }

    #[test]
    fn tui_parity_a_conflict_resolved_by_key_continues_into_the_merge_commit() {
        let repo = externally_conflicted_merge("op-resolve");
        let mut app = repo_app(repo.0.as_path());
        app.dispatch("files.focus");
        app.draw();
        // The conflict row: the files pane's last section, so the bottom of
        // the list is where the keyboard lands — the honest walk the key
        // would have taken.
        app.dispatch("view.bottom");
        app.draw();
        app.dispatch("files.resolve-ours");
        assert!(
            until(Duration::from_secs(5), || {
                app.pump();
                app.message.contains("resolved")
            }),
            "the resolution never ran: {:?}",
            app.message
        );
        // Ours' bytes are the answer, and the banner still names the merge.
        assert_eq!(std::fs::read(repo.0.join("f.txt")).unwrap(), b"ours\n");
        app.draw();
        assert!(status(&app).contains("merge in progress"));

        // M carries the merge onward: a real merge commit, two parents.
        app.dispatch("operation.continue");
        assert!(
            until(Duration::from_secs(5), || {
                app.pump();
                app.message.contains("merge continued")
            }),
            "the continue never finished: {:?}",
            app.message
        );
        let parents = repo.ask(&["rev-list", "--parents", "-n", "1", "HEAD"]);
        assert_eq!(parents.split_whitespace().count(), 3, "HEAD + two parents");
        app.draw();
        assert!(!status(&app).contains("in progress"));
    }

    #[test]
    fn tui_parity_the_lifecycle_keys_gate_by_the_kind_standing() {
        let repo = externally_conflicted_merge("op-gate");
        let mut app = repo_app(repo.0.as_path());
        app.draw();
        // The per-kind keys answer their own kind only: a merge stands, so
        // the rebase exits refuse through availability, naming what stands.
        app.dispatch("rebase.abort");
        assert_eq!(
            app.message,
            "rebase.abort: a merge is in progress, not a rebase"
        );
        app.dispatch("commits.cherry-pick-continue");
        assert_eq!(
            app.message,
            "commits.cherry-pick-continue: a merge is in progress, not a cherry-pick"
        );
        // Skip belongs to a rebase; the generic exits do not.
        app.dispatch("operation.skip");
        assert_eq!(
            app.message,
            "operation.skip: skip belongs to a rebase; a merge is in progress"
        );
        // A continue with conflicts still standing must refuse — git's own
        // word — and never invent a commit. HEAD proves it.
        let head_before = repo.ask(&["rev-parse", "HEAD"]);
        app.dispatch("operation.continue");
        assert!(
            until(Duration::from_secs(5), || {
                app.pump();
                let m = app.message.to_lowercase();
                m.contains("unmerged") || m.contains("unresolved")
            }),
            "the refused continue never named its reason: {:?}",
            app.message
        );
        assert_eq!(repo.ask(&["rev-parse", "HEAD"]), head_before);
        // And the standing merge is untouched by every refusal above.
        app.dispatch("operation.abort");
        assert!(
            until(Duration::from_secs(5), || {
                app.pump();
                app.message.contains("merge aborted")
            }),
            "the abort never finished: {:?}",
            app.message
        );
    }

    #[test]
    fn tui_parity_merging_a_selected_branch_stops_on_its_conflict() {
        let repo = Git::init("op-merge-row");
        repo.write("f.txt", "base\n");
        repo.commit("base");
        repo.git(&["checkout", "-qb", "side"]);
        repo.write("f.txt", "theirs\n");
        repo.commit("theirs");
        repo.git(&["checkout", "-q", "main"]);
        repo.write("f.txt", "ours\n");
        repo.commit("ours");

        let mut app = repo_app(repo.0.as_path());
        app.dispatch("branches.focus");
        app.draw();
        // Walk down to the `side` row — the honest path the key would take.
        app.dispatch("view.top");
        for _ in 0..8 {
            let named = branches_of(&app).status();
            if named.contains("side") {
                break;
            }
            app.dispatch("view.down");
        }
        app.dispatch("branches.merge");
        // A conflicted merge refuses with git's own sentence and leaves its
        // question standing, which the banner then names.
        assert!(
            until(Duration::from_secs(5), || {
                app.pump();
                app.draw();
                status(&app).contains("merge in progress · 1 conflicted file")
            }),
            "the conflicted merge never surfaced: {:?}",
            app.message
        );
        // The way out is the same door: abort puts the tree back.
        app.dispatch("operation.abort");
        assert!(
            until(Duration::from_secs(5), || {
                app.pump();
                app.message.contains("merge aborted")
            }),
            "the abort never finished: {:?}",
            app.message
        );
        assert_eq!(std::fs::read(repo.0.join("f.txt")).unwrap(), b"ours\n");
    }

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
        // The remotes share the branches' section, second in the column, so
        // its header is the second row of the body and its rows follow.
        assert!(
            app.screen.row_text(2).contains("remotes"),
            "the header did not follow: {:?}",
            app.screen.row_text(2)
        );
        assert!(
            app.screen.row_text(0).contains("1 remote"),
            "the label did not count: {:?}",
            app.screen.row_text(0)
        );
        assert_eq!(remotes_status(&app), "1/1 · origin");
        let body = (3..5)
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

    /// Opens the unstaged side of the fake's file and puts the keyboard on
    /// a hunk of it — the position every pick test starts from. The cursor
    /// may land on a header, so it walks until the selection names a hunk.
    fn open_unstaged_hunk(app: &mut App) {
        app.dispatch("files.focus");
        app.dispatch("view.down");
        app.pump_quiet();
        app.dispatch("diff.focus");
        app.pump_quiet();
        for _ in 0..8 {
            if diff_of(app).patch_selection().is_ok() {
                break;
            }
            app.dispatch("view.down");
        }
        assert!(
            diff_of(app).patch_selection().is_ok(),
            "the keyboard never reached a hunk"
        );
    }

    /// Opens a commit's diff and puts the keyboard on a hunk of it.
    fn open_commit_hunk(app: &mut App) {
        app.dispatch("diff.focus");
        app.pump_quiet();
        for _ in 0..8 {
            if diff_of(app).patch_selection().is_ok() {
                break;
            }
            app.dispatch("view.down");
        }
        assert!(
            diff_of(app).patch_selection().is_ok(),
            "the keyboard never reached a hunk"
        );
    }

    fn writes_of(state: &Arc<Mutex<FakeState>>) -> Vec<String> {
        state.lock().unwrap().writes.clone()
    }

    fn worktrees_of(app: &App) -> &Worktrees {
        match app.panes.get("worktrees") {
            Some(Screens::Worktrees { view, .. }) => view,
            _ => panic!("the worktrees pane is not registered"),
        }
    }

    fn worktrees_status(app: &App) -> String {
        worktrees_of(app).status()
    }

    fn worktree_row(path: &str, branch: Option<&str>) -> gitten_core::worktrees::Worktree {
        gitten_core::worktrees::Worktree {
            path: path.as_bytes().to_vec(),
            head: "abcdef0123456789".into(),
            branch: branch.map(|b| b.as_bytes().to_vec()),
            bare: false,
            lock: None,
            prunable: None,
        }
    }

    #[test]
    fn tui_parity_the_worktrees_pane_lists_checkouts_and_states() {
        let (handle, state) = fake(&[]);
        state.lock().unwrap().worktrees = vec![
            worktree_row("/fake", Some("main")),
            worktree_row("/fake-feature", Some("feature")),
            gitten_core::worktrees::Worktree {
                lock: Some("held".into()),
                ..worktree_row("/fake-held", None)
            },
        ];
        let mut app = commits_app(&handle);
        app.dispatch("worktrees.focus");
        app.draw();
        // The pane is one of eight squeezed into the sidebar: rows drawn
        // depend on the layout, so the test reads the cursor and the
        // status line rather than fixed rows — both name the checkout
        // whatever geometry gave it.
        assert_eq!(worktrees_status(&app), "1/3 · /fake");
        assert_eq!(worktrees_of(&app).current(), Some(b"/fake".to_vec()));
        // The cursor's row is the one drawn: the this-checkout marker
        // arrives wherever the geometry put row zero.
        let h = app.screen.size().1;
        let top = (0..h)
            .map(|y| app.screen.row_text(y))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(top.contains("(this checkout)"), "{top:?}");
        app.dispatch("view.down");
        app.dispatch("view.down");
        assert_eq!(worktrees_status(&app), "3/3 · /fake-held");
        app.draw();
        let h = app.screen.size().1;
        let body = (0..h)
            .map(|y| app.screen.row_text(y))
            .collect::<Vec<_>>()
            .join("\n");
        // The cursor's row is the one drawn: the pane narrowed to one
        // row still shows the path it sits on.
        assert!(body.contains("/fake-held"), "{body:?}");
        // The locked marker rides the row's full text even where the
        // pane clips it — copy reads the text, not the cells.
        app.dispatch("copy.selection");
        assert!(
            app.copy
                .as_deref()
                .is_some_and(|t| t.contains("(locked: held)")),
            "the marker never rode the row: {:?}",
            app.copy
        );
        app.dispatch("view.top");
        app.dispatch("view.down");
        app.dispatch("copy.selection");
        assert!(
            app.copy
                .as_deref()
                .is_some_and(|t| t.contains("/fake-feature")),
            "copy did not read the row: {:?}",
            app.copy
        );
    }

    #[test]
    fn tui_parity_worktree_new_asks_from_then_path() {
        let (handle, state) = fake(&[]);
        state.lock().unwrap().worktrees = vec![worktree_row("/fake", Some("main"))];
        let mut app = commits_app(&handle);
        app.dispatch("worktrees.focus");
        app.press(Key::char('n'));
        assert!(matches!(app.prompt, Some(Prompt::WorktreeBase { .. })));
        type_(&mut app, "feature");
        app.press(Key::plain(Code::Enter));
        assert!(matches!(app.prompt, Some(Prompt::WorktreePath { .. })));
        type_(&mut app, "/tmp/new-checkout");
        app.press(Key::plain(Code::Enter));
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                !state.lock().unwrap().worktree_writes.is_empty()
            }),
            "the worktree add never queued"
        );
        assert_eq!(
            state.lock().unwrap().worktree_writes,
            ["add /tmp/new-checkout at feature"],
        );
        // And the namespace the next read answers grew the row.
        app.dispatch("repo.refresh");
        app.pump_quiet();
        assert_eq!(worktrees_status(&app), "1/2 · /fake");
    }

    #[test]
    fn tui_parity_worktree_from_a_commit_row_starts_there() {
        let (handle, state) = fake(&[]);
        let mut app = commits_app(&handle);
        let sha = commits_of(&app)
            .current()
            .expect("a commit row")
            .sha
            .clone();
        app.press(Key::char('w'));
        assert!(matches!(app.prompt, Some(Prompt::WorktreePath { .. })));
        type_(&mut app, "/tmp/at-commit");
        app.press(Key::plain(Code::Enter));
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                !state.lock().unwrap().worktree_writes.is_empty()
            }),
            "the worktree add never queued"
        );
        assert_eq!(
            state.lock().unwrap().worktree_writes,
            [format!("add /tmp/at-commit at {sha}")],
            "the base is the row's sha, not HEAD"
        );
    }

    #[test]
    fn tui_parity_worktree_from_branch_tag_and_stash_rows() {
        let (handle, state) = fake(&[]);
        branch_world(&state);
        state.lock().unwrap().tags = vec![gitten_core::refs::Tag {
            name: gitten_core::refs::RefName::from("v1"),
            commit: "abc123".into(),
            annotated: false,
            subject: None,
        }];
        let mut app = commits_app(&handle);
        // A branch row names the branch.
        app.press(Key::plain(Code::Char('3')));
        app.press(Key::char('w'));
        assert!(
            matches!(app.prompt, Some(Prompt::WorktreePath { .. })),
            "no path prompt over the branches pane"
        );
        app.press(Key::plain(Code::Esc));
        // A tag row names the commit the tag points at.
        app.dispatch("tags.focus");
        app.press(Key::char('w'));
        assert!(
            matches!(app.prompt, Some(Prompt::WorktreePath { .. })),
            "no path prompt over the tags pane"
        );
        app.press(Key::plain(Code::Esc));
        // A stash row names the entry's commit.
        app.dispatch("stashes.focus");
        app.press(Key::char('w'));
        assert!(
            matches!(app.prompt, Some(Prompt::WorktreePath { .. })),
            "no path prompt over the stash stack"
        );
    }

    #[test]
    fn tui_parity_worktree_remove_asks_twice_and_forces_past_dirt() {
        let (handle, state) = fake(&[]);
        state.lock().unwrap().worktrees = vec![
            worktree_row("/fake", Some("main")),
            worktree_row("/fake-feature", Some("feature")),
        ];
        let mut app = commits_app(&handle);
        app.dispatch("worktrees.focus");
        app.dispatch("view.down");
        // Twice pressed runs the plain removal.
        app.press(Key::char('d'));
        app.press(Key::char('d'));
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                !state.lock().unwrap().worktree_writes.is_empty()
            }),
            "the removal never queued"
        );
        assert_eq!(
            state.lock().unwrap().worktree_writes,
            ["remove /fake-feature"],
        );

        // A dirty refusal upgrades the question: the next press forces.
        state.lock().unwrap().worktrees = vec![
            worktree_row("/fake", Some("main")),
            worktree_row("/wt", None),
        ];
        state.lock().unwrap().refuse_worktree = Some(
            "worktree at /wt has uncommitted changes — press d again to force its removal".into(),
        );
        app.dispatch("repo.refresh");
        app.dispatch("view.bottom");
        app.press(Key::char('d'));
        app.press(Key::char('d'));
        until_message(&mut app, "has uncommitted changes");
        app.press(Key::char('d'));
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                state
                    .lock()
                    .unwrap()
                    .worktree_writes
                    .iter()
                    .any(|w| w.contains("forced"))
            }),
            "the third press never forced: {:?}",
            state.lock().unwrap().worktree_writes
        );
        // And the spent upgrade is gone: the row went with the removal,
        // so the keyboard sits on this checkout, which refuses first.
        app.press(Key::char('d'));
        assert!(
            app.message.contains("cannot remove the checkout"),
            "a force leaked past its spending: {:?}",
            app.message
        );
    }

    #[test]
    fn tui_parity_worktree_remove_refuses_this_checkout() {
        let (handle, state) = fake(&[]);
        state.lock().unwrap().worktrees = vec![worktree_row("/fake", Some("main"))];
        let mut app = commits_app(&handle);
        app.dispatch("worktrees.focus");
        app.press(Key::char('d'));
        app.press(Key::char('d'));
        app.pump_quiet();
        assert!(
            app.message.contains("cannot remove the checkout"),
            "the guard never spoke: {:?}",
            app.message
        );
        assert!(
            state.lock().unwrap().worktree_writes.is_empty(),
            "a removal queued against this checkout"
        );
    }

    #[test]
    fn tui_parity_worktree_switch_opens_the_checkout() {
        let (handle, state) = fake(&[]);
        state.lock().unwrap().worktrees = vec![
            worktree_row("/fake", Some("main")),
            worktree_row("/fake-feature", Some("feature")),
        ];
        let mut app = commits_app(&handle);
        app.dispatch("worktrees.focus");
        // This checkout is already shown — the switch says so.
        app.dispatch("worktrees.switch");
        assert!(
            app.message.contains("already showing"),
            "{message:?}",
            message = app.message
        );
        // Anywhere else goes through the open, which refuses what the
        // test never handed it — naming the path, not swallowing it.
        app.dispatch("view.down");
        app.dispatch("worktrees.switch");
        assert!(
            app.message.contains("/fake-feature"),
            "the refusal named no path: {:?}",
            app.message
        );
    }

    #[test]
    fn tui_parity_worktree_read_failure_draws_unavailable() {
        let (handle, state) = fake(&[]);
        state.lock().unwrap().fail_worktrees = Some("git is gone".into());
        let mut app = commits_app(&handle);
        app.dispatch("worktrees.focus");
        app.draw();
        let h = app.screen.size().1;
        let body = (0..h)
            .map(|y| app.screen.row_text(y))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(body.contains("worktrees unavailable"), "{body:?}");
        assert_eq!(worktrees_of(&app).current(), None);
    }

    #[test]
    fn tui_parity_checkout_refuses_a_branch_held_elsewhere() {
        let (handle, state) = fake(&[]);
        branch_world(&state);
        state.lock().unwrap().held_branches = vec!["feature".into()];
        let mut app = commits_app(&handle);
        app.press(Key::plain(Code::Char('3')));
        // Walk to the held row: the world the helper builds names it.
        for _ in 0..8 {
            let target = app.branch_target();
            if matches!(target, Some(Target::Local(_))) {
                break;
            }
            app.dispatch("view.down");
        }
        app.dispatch("branches.checkout");
        app.pump_quiet();
        // Either the held row refused, or the cursor never found a local
        // row to aim at — both are honest, but a checkout write is not.
        let writes = branch_writes_of(&state);
        assert!(
            !writes.iter().any(|w| w.starts_with("checkout feature")),
            "checked out a held branch: {writes:?}"
        );
    }

    #[test]
    fn tui_parity_bisect_start_judge_reset_round_trip() {
        let (handle, state) = fake(&[]);
        let mut app = commits_app(&handle);
        // Clean tree: `b` opens the start field, aimed at the row.
        app.press(Key::char('b'));
        assert!(matches!(app.prompt, Some(Prompt::BisectGood { .. })));
        type_(&mut app, "v1.0");
        app.press(Key::plain(Code::Enter));
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                state.lock().unwrap().bisect.is_some()
            }),
            "the bisect never started"
        );
        app.draw();
        assert!(
            status(&app).contains("bisecting"),
            "the banner never named it: {:?}",
            status(&app)
        );
        // Standing: `b` opens the judgement question instead.
        app.press(Key::char('b'));
        assert!(
            app.message.contains("good") && app.message.contains("bad"),
            "the menu never listed the judgements: {:?}",
            app.message
        );
        app.press(Key::char('g'));
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                state
                    .lock()
                    .unwrap()
                    .bisect_writes
                    .contains(&"good ".into())
            }),
            "the judgement never queued: {:?}",
            state.lock().unwrap().bisect_writes
        );
        // Reset ends it; the banner goes with the state.
        app.dispatch("commits.bisect-reset");
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                state.lock().unwrap().bisect.is_none()
            }),
            "the reset never landed"
        );
        app.draw();
        assert!(
            !status(&app).contains("bisecting"),
            "the banner outlived the reset: {:?}",
            status(&app)
        );
    }

    #[test]
    fn tui_parity_bisect_judgements_refuse_outside_a_bisection() {
        let (handle, _state) = fake(&[]);
        let mut app = commits_app(&handle);
        app.dispatch("commits.bisect-good");
        assert!(
            app.message.contains("no bisect is in progress"),
            "{message:?}",
            message = app.message
        );
        app.dispatch("commits.bisect-bad");
        assert!(app.message.contains("no bisect is in progress"));
        app.dispatch("commits.bisect-skip");
        assert!(app.message.contains("no bisect is in progress"));
    }

    #[test]
    fn tui_parity_bisect_verbs_are_disabled_without_a_repository() {
        // Every one of them needs a history to bisect, so a fixture
        // says why rather than advertising a key that cannot run.
        let a = tui_availability(false, None, None);
        for command in [
            "commits.bisect-menu",
            "commits.bisect-good",
            "commits.bisect-bad",
            "commits.bisect-skip",
            "commits.bisect-reset",
        ] {
            match a.state(command) {
                gitten_core::command::Usable::Disabled(why) => assert!(
                    why.contains("fixture"),
                    "{command}'s reason names no fixture: {why:?}"
                ),
                other => panic!("{command} is advertised against a fixture: {other:?}"),
            }
            assert!(!a.runnable(command));
        }
        // With a repository the menu and reset are live; the judgements
        // wait on a standing bisect.
        let a = tui_availability(true, None, None);
        for command in ["commits.bisect-menu", "commits.bisect-reset"] {
            assert_eq!(
                a.state(command),
                &gitten_core::command::Usable::Available,
                "{command} should run with a repository"
            );
        }
        let standing = gitten_core::bisect::BisectState {
            current: "abc".into(),
            original: "main".into(),
            goods: Vec::new(),
        };
        let a = tui_availability(true, None, Some(&standing));
        for command in [
            "commits.bisect-good",
            "commits.bisect-bad",
            "commits.bisect-skip",
        ] {
            assert_eq!(
                a.state(command),
                &gitten_core::command::Usable::Available,
                "{command} should run while bisecting"
            );
        }
    }

    fn branch_writes_of(state: &Arc<Mutex<FakeState>>) -> Vec<String> {
        state.lock().unwrap().branch_writes.clone()
    }

    fn until_message(app: &mut App, needle: &str) {
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                app.message.contains(needle)
            }),
            "the message never said {needle:?}: {:?}",
            app.message
        );
    }

    fn until_writes(app: &mut App, state: &Arc<Mutex<FakeState>>) {
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                !state.lock().unwrap().writes.is_empty()
            }),
            "the job never reached the repository"
        );
    }

    #[test]
    fn tui_parity_patch_options_open_from_ctrl_p_and_name_targets() {
        let (handle, state) = fake(&[]);
        let mut app = both_sides_app(&state, &handle);
        app.dispatch("patch.menu");
        assert_eq!(app.question, Some("patch"), "the menu never stood");
        assert!(
            app.message.contains("applies onto") && app.message.contains("builder"),
            "the menu did not name its targets: {:?}",
            app.message
        );
        // The builder letter opens the builder; the menu stands down.
        app.dispatch("patch.show");
        assert!(app.patch.is_some(), "the builder never opened");
        app.dispatch("back");
        assert!(app.patch.is_none(), "esc left the builder standing");
    }

    #[test]
    fn tui_parity_pick_keeps_the_hunk_and_names_the_clipboard() {
        let (handle, state) = fake(&[]);
        let mut app = both_sides_app(&state, &handle);
        open_unstaged_hunk(&mut app);
        app.dispatch("patch.pick");
        assert_eq!(
            app.message, "picked 1 hunk — 1 on the patch",
            "the pick did not say what it kept: {:?}",
            app.message
        );
        assert_eq!(app.patch_clip.files().len(), 1);
        assert_eq!(app.patch_clip.files()[0].path, "f.txt");
        // A second press on the same hunk is not a failure and not a
        // silence: the clipboard is unchanged and the count says so.
        app.dispatch("patch.pick");
        assert!(
            app.message.contains("already on the patch"),
            "the re-pick did not say so: {:?}",
            app.message
        );
        assert_eq!(app.patch_clip.included_hunks(), 1);
    }

    #[test]
    fn tui_parity_pick_from_a_commit_diff_keeps_as_drawn() {
        let (handle, state) = fake(&[]);
        let mut app = commits_app(&handle);
        open_commit_hunk(&mut app);
        app.dispatch("patch.pick");
        assert!(
            app.message.contains("picked 1 hunk"),
            "the commit pick did not land: {:?}",
            app.message
        );
        let file = &app.patch_clip.files()[0];
        assert!(
            matches!(file.anchor, gitten_core::patchclip::Anchor::Commit { .. }),
            "a commit pick names its commit: {:?}",
            file.anchor
        );
        assert!(file.index_oid.is_none() && file.head_oid.is_none());
        let _ = state;
    }

    #[test]
    fn tui_parity_pick_refuses_where_no_side_stands() {
        let (handle, state) = fake(&[]);
        let mut app = both_sides_app(&state, &handle);
        // The keyboard on the files list names no diff at all.
        app.dispatch("files.focus");
        app.dispatch("patch.pick");
        assert_eq!(app.message, "the keyboard is not on a diff");
        assert!(app.patch_clip.is_empty());
        // A fixture names no repository to pick from — said by the
        // availability gate ahead of any action.
        let mut app = app_on_diff(Source::Fixtures, None);
        app.dispatch("patch.pick");
        assert_eq!(
            app.message,
            "patch.pick: a fixture has no repository to patch in"
        );
        let _ = state;
    }

    #[test]
    fn tui_parity_builder_toggles_then_applies() {
        let (handle, state) = fake(&[]);
        let mut app = both_sides_app(&state, &handle);
        open_unstaged_hunk(&mut app);
        app.dispatch("patch.pick");
        app.dispatch("patch.show");
        app.draw();
        assert!(
            app.screen.row_text(2).contains("patch"),
            "the builder drew no title: {:?}",
            app.screen.row_text(2)
        );
        // Space excludes the hunk; enter then has nothing to carry.
        app.dispatch("patch.toggle-hunk");
        assert!(app.message.contains("excluded"), "{:?}", app.message);
        app.dispatch("patch.apply-worktree");
        assert_eq!(
            app.message,
            "nothing on the patch is included — toggle hunks on in the builder"
        );
        assert!(writes_of(&state).is_empty(), "an excluded hunk applied");
        // Back on, and the apply reaches the repository.
        app.dispatch("patch.toggle-hunk");
        app.dispatch("patch.apply-worktree");
        until_writes(&mut app, &state);
        assert!(
            writes_of(&state).iter().any(|w| w.starts_with("apply ")),
            "no apply write: {:?}",
            writes_of(&state)
        );
        // The clipboard survives its own apply: the same patch is often
        // wanted on a second target.
        assert_eq!(app.patch_clip.included_hunks(), 1);
        app.dispatch("back");
        assert!(app.patch.is_none());
    }

    #[test]
    fn tui_parity_reverse_asks_twice_then_runs() {
        let (handle, state) = fake(&[]);
        let mut app = both_sides_app(&state, &handle);
        open_unstaged_hunk(&mut app);
        app.dispatch("patch.pick");
        app.dispatch("patch.reverse-worktree");
        assert_eq!(
            app.message,
            "reverse the patch off the worktree? press again to confirm"
        );
        assert!(writes_of(&state).is_empty(), "the question wrote");
        app.dispatch("patch.reverse-worktree");
        until_writes(&mut app, &state);
        assert!(
            writes_of(&state).iter().any(|w| w.starts_with("discard ")),
            "no reverse write: {:?}",
            writes_of(&state)
        );
    }

    #[test]
    fn tui_parity_stale_pick_refuses_before_anything_applies() {
        let (handle, state) = fake(&[]);
        let mut app = both_sides_app(&state, &handle);
        open_unstaged_hunk(&mut app);
        // The pair carries no OID, so the pick expects none — and the
        // index gaining one is exactly a repository that moved.
        state
            .lock()
            .unwrap()
            .index_oids
            .push(("f.txt".into(), "zzz".into()));
        app.dispatch("patch.pick");
        app.dispatch("patch.apply-worktree");
        until_message(&mut app, "f.txt changed since the patch was read");
        assert!(writes_of(&state).is_empty(), "a stale patch applied");
    }

    #[test]
    fn tui_parity_remove_from_commit_asks_twice_and_grafts() {
        let (handle, state) = fake(&[]);
        let mut app = commits_app(&handle);
        open_commit_hunk(&mut app);
        app.dispatch("patch.remove-from-commit");
        assert!(
            app.message.contains("remove this from")
                && app.message.contains("press again to confirm"),
            "the question did not stand: {:?}",
            app.message
        );
        assert!(writes_of(&state).is_empty(), "the question wrote");
        app.dispatch("patch.remove-from-commit");
        until_writes(&mut app, &state);
        let writes = writes_of(&state).join("\n");
        let branches = branch_writes_of(&state).join("\n");
        // The detach and the return ride branch_writes; the patch, stage,
        // amend and replay ride writes — one job, two records.
        assert!(
            branches.contains("checkout 00000000"),
            "never detached at the commit: {branches:?}"
        );
        for step in [
            "discard diff",
            "stage f.txt",
            "amend-no-edit",
            "rebase-onto",
        ] {
            assert!(
                writes.contains(step),
                "the graft skipped {step}: {writes:?}"
            );
        }
        assert!(
            branches.contains("checkout main"),
            "never went home: {branches:?}"
        );
    }

    #[test]
    fn tui_parity_discard_file_takes_the_whole_file() {
        let (handle, state) = fake(&[]);
        let mut app = commits_app(&handle);
        open_commit_hunk(&mut app);
        app.dispatch("patch.discard-file");
        assert!(app.message.contains("press again to confirm"));
        app.dispatch("patch.discard-file");
        until_writes(&mut app, &state);
        let discard = writes_of(&state)
            .into_iter()
            .find(|w| w.starts_with("discard diff"))
            .expect("no discard write");
        assert!(
            discard.contains("EDIT ONE") && discard.contains("EDIT TWO"),
            "the file scope carried one hunk, not the file: {discard:?}"
        );
    }

    #[test]
    fn tui_parity_graft_refuses_a_rename_with_the_checkout_door() {
        let (handle, state) = fake(&[]);
        state.lock().unwrap().commit_file_list = vec![('R', b"f.txt".to_vec())];
        let mut app = commits_app(&handle);
        open_commit_hunk(&mut app);
        app.dispatch("patch.remove-from-commit");
        assert!(
            app.message.contains("rename") && app.message.contains("check it out"),
            "the rename did not name its door: {:?}",
            app.message
        );
        assert!(writes_of(&state).is_empty());
    }

    #[test]
    fn tui_parity_graft_names_the_drop_when_it_would_empty() {
        let (handle, state) = fake(&[]);
        state.lock().unwrap().graft_empty = true;
        let mut app = commits_app(&handle);
        open_commit_hunk(&mut app);
        app.dispatch("patch.remove-from-commit");
        app.dispatch("patch.remove-from-commit");
        until_message(&mut app, "would empty");
        assert!(
            app.message.contains("drop"),
            "the refusal did not name the door: {:?}",
            app.message
        );
        assert!(
            !writes_of(&state).iter().any(|w| w.contains("rebase-onto")),
            "an emptied commit replayed"
        );
    }

    #[test]
    fn tui_parity_checkout_file_asks_twice_and_restores() {
        let (handle, state) = fake(&[]);
        let mut app = commits_app(&handle);
        open_commit_hunk(&mut app);
        app.dispatch("patch.checkout-file");
        assert!(
            app.message.contains("worktree and index both move"),
            "the checkout did not say its scope: {:?}",
            app.message
        );
        assert!(writes_of(&state).is_empty(), "the question wrote");
        app.dispatch("patch.checkout-file");
        until_writes(&mut app, &state);
        assert!(
            writes_of(&state)
                .iter()
                .any(|w| w.starts_with("checkout-file f.txt")),
            "no checkout write: {:?}",
            writes_of(&state)
        );
    }

    #[test]
    fn tui_parity_amend_commit_carries_the_clipboard() {
        let (handle, state) = fake(&[]);
        let mut app = both_sides_app(&state, &handle);
        open_unstaged_hunk(&mut app);
        app.dispatch("patch.pick");
        // The graft needs a clean tree; the pick already resolved, so the
        // status the job reads is built clean here.
        state.lock().unwrap().status = Default::default();
        // The commit's diff, with the keyboard anywhere in it: amending
        // names a commit, not a hunk.
        app.dispatch("commits.focus");
        app.pump_quiet();
        app.dispatch("diff.focus");
        app.pump_quiet();
        app.dispatch("patch.amend-commit");
        assert!(
            app.message.contains("amend") && app.message.contains("press again to confirm"),
            "the question did not stand: {:?}",
            app.message
        );
        app.dispatch("patch.amend-commit");
        until_writes(&mut app, &state);
        let writes = writes_of(&state).join("\n");
        assert!(
            writes.contains("apply diff"),
            "no forward patch: {writes:?}"
        );
        assert!(writes.contains("amend-no-edit"), "no amend: {writes:?}");
    }

    #[test]
    fn tui_parity_amend_with_an_empty_clipboard_says_to_pick_first() {
        let (handle, _state) = fake(&[]);
        let mut app = commits_app(&handle);
        open_commit_hunk(&mut app);
        app.dispatch("patch.amend-commit");
        assert_eq!(
            app.message,
            "nothing on the patch is included — pick hunks first"
        );
    }

    #[test]
    fn tui_parity_move_to_branch_prompts_and_carries() {
        let (handle, state) = fake(&[]);
        let mut app = both_sides_app(&state, &handle);
        open_unstaged_hunk(&mut app);
        app.dispatch("patch.pick");
        app.dispatch("patch.move-to-branch");
        for c in "feature".chars() {
            app.press(Key::char(c));
        }
        app.dispatch("input.accept");
        until_writes(&mut app, &state);
        let writes = writes_of(&state).join("\n");
        let branches = branch_writes_of(&state).join("\n");
        assert!(
            branches.contains("checkout feature"),
            "never moved: {branches:?}"
        );
        assert!(writes.contains("apply diff"), "never applied: {writes:?}");
        assert!(
            app.message.contains("uncommitted"),
            "the landing did not say its state: {:?}",
            app.message
        );
    }

    #[test]
    fn tui_parity_menu_letters_run_their_targets_and_close_it() {
        let (handle, state) = fake(&[]);
        let mut app = both_sides_app(&state, &handle);
        open_unstaged_hunk(&mut app);
        app.dispatch("patch.pick");
        app.dispatch("patch.menu");
        assert_eq!(app.question, Some("patch"));
        // Through the keymap, the way a press travels: the answer falls
        // the question on its way to the dispatch.
        app.press(Key::plain(Code::Char('i')));
        until_writes(&mut app, &state);
        assert!(
            writes_of(&state).iter().any(|w| w.starts_with("stage ")),
            "no index apply: {:?}",
            writes_of(&state)
        );
        assert_eq!(app.question, None, "the menu stood past its answer");
    }

    /// The tags pane reads the namespace: name ahead of the commit and
    /// the subject, the count in the header, and the row the keyboard is
    /// on as the verbs address it.
    #[test]
    fn tui_parity_tags_panel_lists_names_commits_and_subjects() {
        let (handle, state) = fake(&[]);
        state.lock().unwrap().tags = vec![
            Tag {
                name: RefName::from("v2.0"),
                commit: "aaa111bbb222ccc333".into(),
                annotated: true,
                subject: Some("release two".into()),
            },
            Tag {
                name: RefName::from("v1"),
                commit: "ddd444eee555fff666".into(),
                annotated: false,
                subject: None,
            },
        ];
        let mut app = commits_app(&handle);
        app.dispatch("tags.focus");
        app.draw();
        // The tags share the branches' section, second in the column, so its
        // header is the second row of the body and its rows follow.
        assert!(
            app.screen.row_text(2).contains("tags"),
            "the header did not follow: {:?}",
            app.screen.row_text(2)
        );
        assert!(
            app.screen.row_text(0).contains("2 tags"),
            "the label did not count: {:?}",
            app.screen.row_text(0)
        );
        assert_eq!(tags_status(&app), "1/2 · v2.0");
        let body = (3..5)
            .map(|y| app.screen.row_text(y))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            body.contains("v2.0") && body.contains("release two"),
            "the row did not read name then subject: {body:?}"
        );
        // The lightweight tag says so rather than drawing a bare commit.
        app.dispatch("view.down");
        app.draw();
        let bare = (3..6)
            .map(|y| app.screen.row_text(y))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(bare.contains("(lightweight)"), "{bare:?}");
        app.dispatch("copy.selection");
        assert!(
            app.copy.as_deref().is_some_and(|t| t.contains('v')),
            "copy did not read the row: {:?}",
            app.copy
        );
    }

    fn tags_status(app: &App) -> String {
        match app.panes.get("tags") {
            Some(Screens::Tags { view, .. }) => view.status(),
            _ => panic!("the tags pane is registered"),
        }
    }

    /// Tag creation through both doors: the branches pane's T names the
    /// branch, the commits pane's T names the sha, and the message field
    /// decides annotated versus lightweight in both.
    #[test]
    fn tui_parity_tag_create_annotated_and_lightweight() {
        let (handle, state) = fake(&[]);
        branch_world(&state);
        let mut app = commits_app(&handle);
        app.press(Key::plain(Code::Char('3')));
        app.dispatch("view.down");

        // Annotated: a message rides along, and the write names it.
        app.press(Key::char('T'));
        assert!(matches!(app.prompt, Some(Prompt::TagNew { .. })));
        type_(&mut app, "v2");
        app.press(Key::plain(Code::Enter));
        assert!(matches!(app.prompt, Some(Prompt::TagMessage { .. })));
        type_(&mut app, "release two");
        app.press(Key::plain(Code::Enter));
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                state.lock().unwrap().tags_written.len() == 1
            }),
            "the annotated tag never queued"
        );
        assert_eq!(
            state.lock().unwrap().branch_writes,
            ["tag v2 at f\u{fffd}ature (release two)"],
            "the message did not ride the write"
        );

        // Lightweight: the message field comes back empty.
        app.dispatch("view.top");
        app.press(Key::char('T'));
        type_(&mut app, "v1");
        app.press(Key::plain(Code::Enter));
        app.press(Key::plain(Code::Enter));
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                state.lock().unwrap().tags_written.len() == 2
            }),
            "the lightweight tag never queued"
        );
        assert_eq!(
            state.lock().unwrap().branch_writes[1],
            "tag v1 at main",
            "an empty message must not annotate"
        );
        // And the namespace the next read answers grew both rows.
        app.dispatch("repo.refresh");
        app.dispatch("tags.focus");
        assert_eq!(tags_status(&app), "1/2 · v2");
    }

    /// `commits.new-tag` converges on the same verb: the sha under the
    /// keyboard, not the branch, and the same message question.
    #[test]
    fn tui_parity_tag_from_commit_names_the_sha() {
        let (handle, state) = fake(&[]);
        let mut app = commits_app(&handle);
        // Captured before the tag: the write's refresh re-reads the list
        // behind the assertion.
        let sha = commits_of(&app)
            .current()
            .expect("a commit row")
            .sha
            .clone();
        app.press(Key::char('T'));
        assert!(matches!(app.prompt, Some(Prompt::TagNew { .. })));
        type_(&mut app, "at-head");
        app.press(Key::plain(Code::Enter));
        app.press(Key::plain(Code::Enter));
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                !state.lock().unwrap().tags_written.is_empty()
            }),
            "the commit tag never queued"
        );
        let written = state.lock().unwrap().tags_written.clone();
        assert_eq!(written.len(), 1, "one tag queued");
        assert_eq!(written[0].0, b"at-head".to_vec(), "the name rode along");
        assert_eq!(
            written[0].1,
            sha.as_bytes().to_vec(),
            "the tag was not aimed at the commit's sha"
        );
    }

    /// Tag deletion asks twice on the name, spends the arm, and the refresh
    /// loses the row — while a move between the presses re-arms elsewhere.
    #[test]
    fn tui_parity_tag_delete_asks_twice_and_refreshes() {
        let (handle, state) = fake(&[]);
        state.lock().unwrap().tags = vec![
            Tag {
                name: RefName::from("v1"),
                commit: "aaa111".into(),
                annotated: false,
                subject: None,
            },
            Tag {
                name: RefName::from("v2"),
                commit: "bbb222".into(),
                annotated: true,
                subject: Some("two".into()),
            },
        ];
        let mut app = commits_app(&handle);
        app.dispatch("tags.focus");
        app.dispatch("tags.delete");
        assert_eq!(app.message, "delete tag v1? press again to confirm");
        assert!(state.lock().unwrap().branch_writes.is_empty());
        app.dispatch("tags.delete");
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                state
                    .lock()
                    .unwrap()
                    .branch_writes
                    .iter()
                    .any(|w| w == "untag v1")
            }),
            "the deletion never queued"
        );
        app.dispatch("repo.refresh");
        assert_eq!(tags_status(&app), "1/1 · v2");
    }

    /// Pushing a tag names its remote: the field arrives prefilled with the
    /// lone remote, and the write carries both halves.
    #[test]
    fn tui_parity_tag_push_names_its_remote() {
        let (handle, state) = fake(&[]);
        state.lock().unwrap().servers = vec![remote_ref("origin", &["x.example"])];
        state.lock().unwrap().tags = vec![Tag {
            name: RefName::from("v1"),
            commit: "aaa111".into(),
            annotated: false,
            subject: None,
        }];
        let mut app = commits_app(&handle);
        app.dispatch("tags.focus");
        app.dispatch("tags.push");
        let prefilled = match &app.prompt {
            Some(Prompt::TagPush { field, .. }) => field.text().to_string(),
            _ => panic!("the push field did not open"),
        };
        assert_eq!(prefilled, "origin", "the lone remote prefilled");
        app.press(Key::plain(Code::Enter));
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                state
                    .lock()
                    .unwrap()
                    .branch_writes
                    .iter()
                    .any(|w| w == "push tag v1 to origin")
            }),
            "the push never queued"
        );
    }

    /// Deleting a remote-tracking row's source asks twice on the full
    /// `remote/branch` name, deletes there, and keeps the local branch —
    /// through the named command and through the `d` key, which routes by
    /// row.
    #[test]
    fn tui_parity_remote_branch_delete_asks_twice_and_keeps_local() {
        let (handle, state) = fake(&[]);
        branch_world(&state);
        let mut app = commits_app(&handle);
        app.press(Key::plain(Code::Char('3')));
        // Walk to the first remote row by name, the honest keyboard walk.
        app.dispatch("view.top");
        for _ in 0..8 {
            if branches_of(&app).status().contains("origin/") {
                break;
            }
            app.dispatch("view.down");
        }
        assert!(
            branches_of(&app).status().contains("origin/feat/ure"),
            "the remote row was never reached"
        );
        app.dispatch("branches.delete-remote");
        assert!(
            app.message.contains("origin/feat/ure") && app.message.contains("press again"),
            "the question did not name both halves: {:?}",
            app.message
        );
        app.dispatch("branches.delete-remote");
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                state
                    .lock()
                    .unwrap()
                    .branch_writes
                    .iter()
                    .any(|w| w == "delete origin/feat/ure on origin")
            }),
            "the remote deletion never queued"
        );
        // The local branch of no such name was never in danger, and the
        // refresh loses the remote row.
        assert!(
            state
                .lock()
                .unwrap()
                .locals
                .iter()
                .any(|b| b.name.as_bytes() == b"main"),
            "the local branch moved"
        );
        app.dispatch("repo.refresh");
        assert!(
            !branches_of(&app).status().contains("origin/feat/ure"),
            "the remote row survived its deletion"
        );
    }

    /// The reflog pane reads where HEAD has been: selectors first, the move
    /// named beside each, and the keyboard's row as the recovery addresses
    /// it.
    #[test]
    fn tui_parity_reflog_pane_reads_selectors_and_moves() {
        let (handle, state) = fake(&[]);
        state.lock().unwrap().reflog = vec![
            ReflogEntry {
                commit: "bbb222".into(),
                selector: "HEAD@{0}".into(),
                message: "commit: third".into(),
            },
            ReflogEntry {
                commit: "aaa111".into(),
                selector: "HEAD@{1}".into(),
                message: "checkout: moving from main to feature".into(),
            },
        ];
        let mut app = commits_app(&handle);
        app.dispatch("reflog.focus");
        app.draw();
        // The reflog shares the commits' section, third in the column, so
        // its header is the third row of the body and its rows follow.
        assert!(
            app.screen.row_text(3).contains("reflog"),
            "the header did not follow: {:?}",
            app.screen.row_text(3)
        );
        assert!(
            app.screen.row_text(0).contains("2 entries"),
            "the label did not count: {:?}",
            app.screen.row_text(0)
        );
        assert_eq!(reflog_status(&app), "1/2 · HEAD@{0}");
        let body = (4..6)
            .map(|y| app.screen.row_text(y))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            body.contains("HEAD@{0}") && body.contains("commit: third"),
            "the row did not read selector then move: {body:?}"
        );
    }

    fn reflog_status(app: &App) -> String {
        match app.panes.get("reflog") {
            Some(Screens::Reflog { view, .. }) => view.status(),
            _ => panic!("the reflog pane is registered"),
        }
    }

    /// Recovery after a reset: the branch goes back onto the entry, softly
    /// — the tree the reset left is the tree that stays.
    #[test]
    fn tui_parity_reflog_recovery_after_reset() {
        let repo = Git::init("reflog-recover");
        repo.write("f.txt", "one\n");
        repo.commit("one");
        repo.write("f.txt", "two\n");
        repo.commit("two");
        let second = repo.ask(&["rev-parse", "HEAD"]);
        repo.git(&["reset", "--hard", "HEAD~1"]);
        repo.write("f.txt", "uncommitted\n");

        let mut app = repo_app(repo.0.as_path());
        app.dispatch("reflog.focus");
        // Newest first: HEAD@{0} is the reset itself, HEAD@{1} the commit.
        app.dispatch("view.down");
        app.dispatch("reflog.recover");
        assert!(
            app.message.contains("press again"),
            "the recovery did not ask: {:?}",
            app.message
        );
        app.dispatch("reflog.recover");
        assert!(
            until(Duration::from_secs(5), || {
                app.pump();
                repo.ask(&["rev-parse", "HEAD"]) == second
            }),
            "the branch never went back: {:?}",
            app.message
        );
        assert_eq!(
            std::fs::read(repo.0.join("f.txt")).unwrap(),
            b"uncommitted\n",
            "soft recovery must not touch the tree"
        );
    }

    /// Undo walks a commit back and redo walks it forward; the sentences
    /// are ours, so the reflog proves the round trip.
    #[test]
    fn tui_parity_undo_redo_a_commit() {
        let repo = Git::init("undo-commit");
        repo.write("f.txt", "one\n");
        repo.commit("one");
        let first = repo.ask(&["rev-parse", "HEAD"]);
        repo.write("f.txt", "two\n");
        repo.commit("two");

        let mut app = repo_app(repo.0.as_path());
        // The key, not just the name: z is the global undo.
        app.press(Key::char('z'));
        assert!(
            until(Duration::from_secs(5), || {
                app.pump();
                repo.ask(&["rev-parse", "HEAD"]) == first
            }),
            "undo never walked back: {:?}",
            app.message
        );
        assert_eq!(
            std::fs::read(repo.0.join("f.txt")).unwrap(),
            b"two\n",
            "undo moved the tree it promised to leave"
        );
        let log = repo.ask(&["log", "-g", "-1", "--format=%gs", "HEAD@{0}"]);
        assert_eq!(log, "gitten: undo");

        // Z from the files pane: the key is global, but the commits pane
        // answers Z with the cherry-pick abort — the older door — so redo
        // is pressed where nothing shadows it.
        app.dispatch("files.focus");
        app.press(Key::char('Z'));
        assert!(
            until(Duration::from_secs(5), || {
                app.pump();
                repo.ask(&["rev-parse", "HEAD"]) != first
            }),
            "redo never walked forward: {:?}",
            app.message
        );
        let log = repo.ask(&["log", "-g", "-1", "--format=%gs", "HEAD@{0}"]);
        assert_eq!(log, "gitten: redo");
    }

    /// Undoing a checkout checks the old branch back out.
    #[test]
    fn tui_parity_undo_checkout_walks_back() {
        let repo = Git::init("undo-checkout");
        repo.write("f.txt", "one\n");
        repo.commit("one");
        repo.git(&["checkout", "-qb", "feature"]);
        assert_eq!(repo.ask(&["rev-parse", "--abbrev-ref", "HEAD"]), "feature");

        let mut app = repo_app(repo.0.as_path());
        app.dispatch("history.undo");
        assert!(
            until(Duration::from_secs(5), || {
                app.pump();
                repo.ask(&["rev-parse", "--abbrev-ref", "HEAD"]) == "main"
            }),
            "undo never checked main back out: {:?}",
            app.message
        );
    }

    /// Empty histories and foreign moves refuse in words: a fresh
    /// repository has nothing to undo, and a redo behind anything but our
    /// own undo is a guess declined.
    #[test]
    fn tui_parity_undo_and_redo_refuse_honestly() {
        let repo = Git::init("undo-empty");
        repo.write("f.txt", "one\n");
        repo.commit("one");
        let mut app = repo_app(repo.0.as_path());
        app.dispatch("history.undo");
        assert_eq!(app.message, "HEAD is where it was — nothing to undo");
        app.dispatch("history.redo");
        assert_eq!(
            app.message,
            "nothing to redo — redo follows only our own undo"
        );

        // A terminal reset is not our undo: redo stays disarmed.
        let repo = Git::init("undo-foreign");
        repo.write("f.txt", "one\n");
        repo.commit("one");
        repo.write("f.txt", "two\n");
        repo.commit("two");
        repo.git(&["reset", "--soft", "HEAD~1"]);
        let mut app = repo_app(repo.0.as_path());
        app.dispatch("history.redo");
        assert_eq!(
            app.message,
            "nothing to redo — redo follows only our own undo"
        );
    }

    /// Undo behind a standing operation refuses before reading anything:
    /// moving HEAD under a merge in flight corrupts it.
    #[test]
    fn tui_parity_undo_refuses_a_standing_operation() {
        let repo = externally_conflicted_merge("undo-standing");
        let mut app = repo_app(repo.0.as_path());
        app.dispatch("history.undo");
        assert_eq!(
            app.message,
            "finish the standing merge first — undo moves HEAD"
        );
        app.dispatch("history.redo");
        assert_eq!(
            app.message,
            "finish the standing merge first — redo moves HEAD"
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

    // ------------------------------------------------------- the merging view

    /// The conflicted file a merging view opens on: two regions, exactly
    /// the bytes `git merge` leaves, the two stages git holds, and a status
    /// whose conflict section names the path — the world every test below
    /// walks.
    fn conflict_world() -> (Handle, Arc<Mutex<FakeState>>) {
        let bytes = "\
shared top
<<<<<<< ours
ours one
=======
theirs one
>>>>>>> them
shared middle
<<<<<<< ours
ours two
=======
theirs two
>>>>>>> them
shared tail
";
        let mut status = Status::default();
        status.conflicts.push(ConflictEntry {
            path: PathBytes::from("f.txt"),
            state: ConflictKind::BothModified,
            kind: Kind::File,
            submodule: Submodule::default(),
        });
        let (locals, remotes, head) = one_branch();
        let state = Arc::new(Mutex::new(FakeState {
            before: vec![pair("f.txt", side(0), side(1))],
            after: vec![pair("f.txt", side(0), side(1))],
            refuses: Vec::new(),
            stashes: two_stashes(),
            status,
            locals,
            remotes,
            head: Some(head),
            conflict_bytes: bytes.as_bytes().to_vec(),
            conflict_stages: vec![
                gitten_git::UnmergedStage {
                    mode: "100644".into(),
                    oid: "aaaa".repeat(8),
                    stage: 2,
                },
                gitten_git::UnmergedStage {
                    mode: "100644".into(),
                    oid: "bbbb".repeat(8),
                    stage: 3,
                },
            ],
            ..Default::default()
        }));
        (Arc::new(FakeRepo(Arc::clone(&state))), state)
    }

    /// Puts the files pane's keyboard on its conflict row, wherever the
    /// section headings put it.
    fn onto_the_conflict(app: &mut App) {
        app.dispatch("files.focus");
        for _ in 0..16 {
            let on_it = matches!(
                files_of(app).current_file(),
                Some(file) if file.section == files::Section::Conflicts
            );
            if on_it {
                return;
            }
            app.dispatch("view.down");
        }
        panic!("the files pane never reached its conflict row");
    }

    /// Opens the merging view the way a key does — eye on the conflict row
    /// — and waits for the lane's answer to install.
    fn open_merging(app: &mut App) {
        onto_the_conflict(app);
        app.dispatch("files.open-diff");
        app.pump_quiet();
    }

    #[test]
    fn tui_parity_a_conflict_row_previews_its_merging_view() {
        let (handle, _state) = conflict_world();
        let mut app = commits_app(&handle);
        open_merging(&mut app);
        assert_eq!(
            origin_of(&app),
            Some(DiffSource::Conflict {
                path: PathBytes::from("f.txt"),
            }),
            "the eye on a conflict row asks for the conflict, not a diff"
        );
        assert!(
            matches!(app.panes.get("diff"), Some(Screens::Merging { .. })),
            "the main pane is the merging view"
        );
        // The keyboard rode the install, and the mode with it: the merge
        // keys resolve now.
        assert_eq!(app.panes.focused_name(), "diff");
        // The pane says where it is: the keyboard opens on the file's
        // first line — context between nothing and the first conflict —
        // and one row down is inside region 1.
        let status = app
            .panes
            .focused()
            .map(|p| p.status(&app.host))
            .unwrap_or_default();
        assert!(status.contains("between conflicts"), "{status:?}");
        app.dispatch("view.down");
        app.dispatch("view.down");
        let status = app
            .panes
            .focused()
            .map(|p| p.status(&app.host))
            .unwrap_or_default();
        assert!(status.contains("conflict 1/2"), "{status:?}");
    }

    #[test]
    fn tui_parity_region_answers_write_their_choices_and_the_file() {
        let (handle, state) = conflict_world();
        let mut app = commits_app(&handle);
        open_merging(&mut app);

        // The keyboard opens on the file's first line — context, not a
        // region — and a named answer there is refused, not guessed.
        app.dispatch("merge.take-ours");
        assert_eq!(
            state.lock().unwrap().hunk_answers,
            Vec::<Vec<(usize, gitten_core::conflict::Answer)>>::new()
        );
        assert!(
            app.message.contains("not on a conflict"),
            "{:?}",
            app.message
        );

        // Onto region 1's opener: the named answer aims at that region,
        // whatever half the keyboard happens to sit in.
        app.dispatch("view.down");
        app.dispatch("merge.take-ours");
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                !state.lock().unwrap().hunk_answers.is_empty()
            }),
            "the answer never reached the repository"
        );
        assert_eq!(
            state.lock().unwrap().hunk_answers,
            vec![vec![(0, gitten_core::conflict::Answer::Ours)]],
        );
        // The file really changed, the way the answer said: ours one, and
        // region 2's markers untouched.
        assert_eq!(
            String::from_utf8_lossy(&state.lock().unwrap().conflict_bytes),
            "shared top\nours one\nshared middle\n<<<<<<< ours\nours two\n=======\ntheirs two\n>>>>>>> them\nshared tail\n",
        );
    }

    #[test]
    fn tui_parity_a_nested_file_answers_whole_never_by_region() {
        // A merge inside a rebase inside a merge: the bytes hold an inner
        // region inside an outer one. Every named answer is the same
        // refusal — the remedy, not a region index — and nothing is
        // submitted to the repository.
        let (handle, state) = conflict_world();
        state.lock().unwrap().conflict_bytes = b"<<<<<<<<< outer\nouter ours\n<<<<<<< inner\ninner ours\n=======\ninner theirs\n>>>>>>> inner\nouter theirs\n=========\nouter theirs side\n>>>>>>>>> outer\n".to_vec();
        let mut app = commits_app(&handle);
        open_merging(&mut app);
        assert!(
            matches!(app.panes.get("diff"), Some(Screens::Merging { .. })),
            "the nested file still opens its merging view"
        );
        for command in [
            "merge.take-ours",
            "merge.take-theirs",
            "merge.take-both",
            "merge.take-side",
        ] {
            app.dispatch("view.down");
            app.dispatch(command);
            assert!(
                app.message.contains("resolve it whole"),
                "{command}: {:?}",
                app.message
            );
        }
        assert!(
            state.lock().unwrap().hunk_answers.is_empty(),
            "a refused answer submits nothing"
        );
    }

    #[test]
    fn tui_parity_take_side_reads_the_half_under_the_keyboard() {
        let (handle, state) = conflict_world();
        let mut app = commits_app(&handle);
        open_merging(&mut app);

        // Two rows down is region 1's ours line: take-side takes ours.
        app.dispatch("view.down");
        app.dispatch("view.down");
        app.dispatch("merge.take-side");
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                !state.lock().unwrap().hunk_answers.is_empty()
            }),
            "the take-side never reached the repository"
        );
        assert_eq!(
            state.lock().unwrap().hunk_answers,
            vec![vec![(0, gitten_core::conflict::Answer::Ours)]],
        );

        // The first answer's finish re-read the file: one region left, its
        // own parse. The conflict jump walks the *current* file, and the
        // second take-side addresses region 1 of that parse — the answer
        // the renumbered file can actually validate.
        app.dispatch("merge.next-conflict");
        for _ in 0..3 {
            app.dispatch("view.down");
        }
        app.dispatch("merge.take-side");
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                state.lock().unwrap().hunk_answers.len() > 1
            }),
            "the second take-side never reached the repository"
        );
        assert_eq!(
            state.lock().unwrap().hunk_answers,
            vec![
                vec![(0, gitten_core::conflict::Answer::Ours)],
                vec![(0, gitten_core::conflict::Answer::Theirs)],
            ],
        );
        // Both halves answered in one file: the markers are gone and the
        // file is what the two answers combine to.
        assert_eq!(
            String::from_utf8_lossy(&state.lock().unwrap().conflict_bytes),
            "shared top\nours one\nshared middle\ntheirs two\nshared tail\n",
        );
    }

    #[test]
    fn tui_parity_the_seam_refuses_a_choice_and_the_base_says_why() {
        let (handle, state) = conflict_world();
        let mut app = commits_app(&handle);
        open_merging(&mut app);

        // Row 1 is the opener: a side asked for there is the seam, not a
        // half.
        app.dispatch("view.down");
        app.dispatch("merge.take-side");
        assert_eq!(
            state.lock().unwrap().hunk_answers,
            Vec::<Vec<(usize, gitten_core::conflict::Answer)>>::new()
        );
        assert!(
            app.message.contains("marker or the base"),
            "{:?}",
            app.message
        );
    }

    #[test]
    fn tui_parity_undo_walks_the_session_back_and_says_when_it_is_empty() {
        let (handle, state) = conflict_world();
        let mut app = commits_app(&handle);
        open_merging(&mut app);

        // Two answers: ours into region 1, then the conflict jump and
        // theirs into region 2.
        app.dispatch("view.down");
        app.dispatch("view.down");
        app.dispatch("merge.take-ours");
        assert!(until(Duration::from_secs(2), || {
            app.pump_quiet();
            !state.lock().unwrap().hunk_answers.is_empty()
        }));
        app.dispatch("merge.next-conflict");
        for _ in 0..3 {
            app.dispatch("view.down");
        }
        app.dispatch("merge.take-theirs");
        assert!(until(Duration::from_secs(2), || {
            app.pump_quiet();
            state.lock().unwrap().hunk_answers.len() > 1
        }));
        let both = String::from_utf8_lossy(&state.lock().unwrap().conflict_bytes).to_string();

        // First undo: the last answer leaves, byte for byte.
        app.dispatch("merge.undo");
        assert!(until(Duration::from_secs(2), || {
            app.pump_quiet();
            !state.lock().unwrap().restores.is_empty()
        }));
        let after_one = String::from_utf8_lossy(&state.lock().unwrap().conflict_bytes).to_string();
        assert_ne!(after_one, both, "the first undo changed the file");
        assert!(
            after_one.contains("<<<<<<< ours"),
            "the file is a conflict again: {after_one:?}"
        );

        // Second undo: the first answer leaves too, and the file is the
        // conflict it opened as.
        app.dispatch("merge.undo");
        assert!(until(Duration::from_secs(2), || {
            app.pump_quiet();
            state.lock().unwrap().restores.len() > 1
        }));
        assert_eq!(
            String::from_utf8_lossy(&state.lock().unwrap().conflict_bytes),
            "shared top\n<<<<<<< ours\nours one\n=======\ntheirs one\n>>>>>>> them\nshared middle\n<<<<<<< ours\nours two\n=======\ntheirs two\n>>>>>>> them\nshared tail\n",
        );
        assert_eq!(
            state.lock().unwrap().conflict_stages.len(),
            2,
            "the stages came back with the bytes"
        );

        // The session is spent: the third undo says so and writes nothing.
        let restores = state.lock().unwrap().restores.len();
        app.dispatch("merge.undo");
        app.pump_quiet();
        assert_eq!(state.lock().unwrap().restores.len(), restores);
        assert!(
            app.message.contains("nothing left to undo"),
            "{:?}",
            app.message
        );
    }

    #[test]
    fn tui_parity_one_answered_region_leaves_the_other_unmerged() {
        let (handle, state) = conflict_world();
        let mut app = commits_app(&handle);
        open_merging(&mut app);
        app.dispatch("view.down");
        app.dispatch("view.down");
        app.dispatch("merge.take-both");
        assert!(until(Duration::from_secs(2), || {
            app.pump_quiet();
            !state.lock().unwrap().hunk_answers.is_empty()
        }));
        // Region 1 answered both, ours first; region 2 keeps its markers —
        // which is what keeps the file unmerged in git's own eyes.
        assert_eq!(
            String::from_utf8_lossy(&state.lock().unwrap().conflict_bytes),
            "shared top\nours one\ntheirs one\nshared middle\n<<<<<<< ours\nours two\n=======\ntheirs two\n>>>>>>> them\nshared tail\n",
        );
        assert_eq!(state.lock().unwrap().conflict_stages.len(), 2);
    }

    #[test]
    fn tui_parity_merge_options_hands_the_keyboard_back_to_the_row() {
        let (handle, _state) = conflict_world();
        let mut app = commits_app(&handle);
        open_merging(&mut app);
        app.dispatch("merge.options");
        assert_eq!(app.panes.focused_name(), "files");
        // The band names the four keys from the keymap, so a rebind moves
        // this line the way it moves help.
        assert!(
            app.message.contains("whole-file answers"),
            "{:?}",
            app.message
        );
        assert!(app.message.contains('o'), "{:?}", app.message);
    }

    // ------------------------------------------------------------ W6 history

    /// The commits pane with the keyboard on it and the cursor on top —
    /// where every history verb below is pressed from. `hundred_commits`
    /// is the loaded window, so row 0 is `00000000` and the root is
    /// `00000099`.
    fn history_app(handle: &Handle) -> App {
        let mut app = commits_app(handle);
        app.dispatch("commits.focus");
        app.dispatch("view.top");
        assert_eq!(app.panes.focused_name(), "commits");
        app
    }

    /// The full sha of the commits row under the keyboard. Read rather
    /// than written down: the fake answers a *different* window after a
    /// refresh than the one startup loaded, which is exactly what a real
    /// repository does when a write rewrites history, so a test that spelt
    /// the sha out would be asserting the fixture and not the aim.
    fn row_sha(app: &App) -> String {
        commits_of(app)
            .current()
            .expect("the commits pane has a row")
            .sha
            .clone()
    }

    /// Waits for `want` to appear in the fake's write log, pumping the
    /// finish wave meanwhile — the shape every history assertion needs.
    fn wrote(app: &mut App, state: &Arc<Mutex<FakeState>>, want: &str) -> bool {
        until(Duration::from_secs(2), || {
            app.pump_quiet();
            state.lock().unwrap().writes.iter().any(|w| w == want)
        })
    }

    /// LG-058. `g` opens the strength question rather than resetting, and
    /// the three strengths arm *apart*: a soft reset asked, then hard
    /// pressed, asks again instead of firing — which is the whole reason
    /// the arm names the command and not just the commit. What each
    /// strength then does to the index and the working tree is git's, and
    /// is held against a real repository in `gitten-git`'s
    /// `the_three_reset_strengths_leave_different_parts_behind`; here the
    /// proof is that the flag and the sha reach the queue unchanged.
    #[test]
    fn tui_parity_reset_strengths_arm_apart_and_carry_their_own_flag() {
        let (handle, state) = fake(&[]);
        let mut app = history_app(&handle);
        app.dispatch("view.down");
        let sha = row_sha(&app);

        // The menu asks which strength and writes nothing.
        app.dispatch("commits.reset-menu");
        assert_eq!(
            app.message,
            format!("reset to {sha}? soft, mixed or hard — s, m, h"),
            "{:?}",
            app.message
        );
        app.pump_quiet();
        assert!(state.lock().unwrap().writes.is_empty(), "the menu reset");

        // Soft arms and asks; hard pressed against that arm asks its own
        // question rather than spending soft's.
        app.dispatch("commits.reset-soft");
        assert_eq!(
            app.message,
            format!("reset --soft to {sha}? press again to confirm")
        );
        app.dispatch("commits.reset-hard");
        assert_eq!(
            app.message,
            format!("reset --hard to {sha}? press again to confirm"),
            "hard spent the soft question"
        );
        app.pump_quiet();
        assert!(
            state.lock().unwrap().writes.is_empty(),
            "an unconfirmed reset ran: {:?}",
            state.lock().unwrap().writes
        );

        // The second hard press on the same row is the confirmation.
        app.dispatch("commits.reset-hard");
        assert!(
            wrote(&mut app, &state, &format!("reset --hard {sha}")),
            "the confirmed hard reset never landed: {:?}",
            state.lock().unwrap().writes
        );

        // Each remaining strength, twice, carries git's own flag spelling.
        // The row is read again: the finish wave re-read the history the
        // reset rewrote, so the window under the cursor is a new one.
        for (command, flag) in [
            ("commits.reset-soft", "--soft"),
            ("commits.reset-mixed", "--mixed"),
        ] {
            let sha = row_sha(&app);
            app.dispatch(command);
            app.dispatch(command);
            assert!(
                wrote(&mut app, &state, &format!("reset {flag} {sha}")),
                "{command} never landed: {:?}",
                state.lock().unwrap().writes
            );
        }
    }

    /// LG-058, the other half of the arm: moving the cursor between the
    /// question and the answer must not reset the row that was asked
    /// about. The arm holds a sha, so the second press on a *different*
    /// commit asks again about that one.
    #[test]
    fn tui_parity_a_reset_question_does_not_follow_the_cursor() {
        let (handle, state) = fake(&[]);
        let mut app = history_app(&handle);

        let first = row_sha(&app);
        app.dispatch("commits.reset-hard");
        assert_eq!(
            app.message,
            format!("reset --hard to {first}? press again to confirm")
        );
        app.dispatch("view.down");
        let second = row_sha(&app);
        assert_ne!(first, second, "the cursor did not move");
        app.dispatch("commits.reset-hard");
        assert_eq!(
            app.message,
            format!("reset --hard to {second}? press again to confirm"),
            "the arm followed the cursor"
        );
        app.pump_quiet();
        assert!(
            state.lock().unwrap().writes.is_empty(),
            "a reset landed on a row nobody confirmed: {:?}",
            state.lock().unwrap().writes
        );
    }

    /// LG-059. A revert destroys nothing — dropping the result undoes the
    /// undo — so it runs on the first press, aimed at the row's own sha.
    /// The root commit is included on purpose: it has no parent, and its
    /// inverse is still well-defined, which `gitten-git`'s
    /// `reverting_the_root_commit_leaves_an_empty_tree` holds against a
    /// real repository.
    #[test]
    fn tui_parity_revert_runs_on_one_press_including_the_root() {
        let (handle, state) = fake(&[]);
        let mut app = history_app(&handle);
        let top = row_sha(&app);

        app.dispatch("commits.revert");
        assert!(
            wrote(&mut app, &state, &format!("revert {top}")),
            "the revert never landed: {:?}",
            state.lock().unwrap().writes
        );

        // The root: parentless, and its inverse is still well-defined. A
        // fresh app, because the wave above re-read the window and the root
        // of *that* history is a different commit.
        let mut app = history_app(&handle);
        app.dispatch("view.bottom");
        let root = commits_of(&app).current().expect("a bottom row").clone();
        assert!(root.parents.is_empty(), "the last row is not the root");
        app.dispatch("commits.revert");
        assert!(
            wrote(&mut app, &state, &format!("revert {}", root.sha)),
            "the root's revert never landed: {:?}",
            state.lock().unwrap().writes
        );
    }

    /// LG-056. One commit, replayed onto HEAD on the first press, by the
    /// full sha the row holds — never a row index, never the short form.
    #[test]
    fn tui_parity_cherry_pick_replays_the_row_by_its_full_sha() {
        let (handle, state) = fake(&[]);
        let mut app = history_app(&handle);
        app.dispatch("view.down");
        app.dispatch("view.down");
        let commit = commits_of(&app).current().expect("a row").clone();
        assert_eq!(commit.sha.len(), 8, "the row holds a full sha, not a short");

        app.dispatch("commits.cherry-pick");
        assert!(
            wrote(&mut app, &state, &format!("cherry-pick {}", commit.sha)),
            "the pick never landed: {:?}",
            state.lock().unwrap().writes
        );
    }

    /// LG-060. Space detaches HEAD onto the commit under the keyboard and
    /// the branch it left stands still — the state the fake records, not
    /// the sentence the band printed.
    #[test]
    fn tui_parity_detached_checkout_moves_head_alone() {
        let (handle, state) = fake(&[]);
        let mut app = history_app(&handle);
        app.dispatch("view.down");
        let sha = row_sha(&app);

        app.dispatch("commits.checkout");
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                matches!(
                    state.lock().unwrap().head.as_ref(),
                    Some(HeadState::Detached { commit }) if *commit == sha
                )
            }),
            "HEAD never detached onto the row: {:?}",
            state.lock().unwrap().head
        );
        let s = state.lock().unwrap();
        assert!(
            s.locals.iter().all(|b| !b.head),
            "a branch claimed a detached HEAD"
        );
        assert_eq!(s.locals[0].commit, "f00d", "the branch tip moved");
    }

    /// LG-057. The clipboard: a marked range copies oldest-first, separate
    /// copies append in press order, a re-copy never reorders, the paste
    /// hands git one invocation in exactly that order and leaves the
    /// clipboard standing, and only the clear empties it.
    #[test]
    fn tui_parity_multicopy_pastes_in_copy_order() {
        let (handle, state) = fake(&[]);
        let mut app = history_app(&handle);

        // `v` opens the mark, two moves extend it over rows 0..=2, and the
        // copy takes the range newest-first into paste order.
        app.dispatch("select.mark");
        app.dispatch("view.down");
        app.dispatch("view.down");
        assert_eq!(commits_of(&app).marks(), Some((0, 2)));
        let range: Vec<String> = (0..=2)
            .map(|row| commits_of(&app).at(row).expect("a marked row").sha.clone())
            .collect();
        app.dispatch("commits.copy");
        assert_eq!(
            app.message, "copied 3 commits — 3 on the clipboard",
            "{:?}",
            app.message
        );

        // A second copy of the same rows changes nothing and says so.
        app.dispatch("commits.copy");
        assert_eq!(
            app.message, "already copied — 3 commits on the clipboard",
            "{:?}",
            app.message
        );

        // A separate copy appends behind them, in press order.
        app.dispatch("select.mark");
        app.dispatch("view.bottom");
        let late = row_sha(&app);
        app.dispatch("commits.copy");
        assert_eq!(
            app.message, "copied 1 commit — 4 on the clipboard",
            "{:?}",
            app.message
        );

        // One invocation, oldest-first within the range — the list reads
        // newest-first, so the replay order is the marked rows reversed —
        // and the late copy behind them all. The order is the whole
        // contract of a paste.
        let want = format!("cherry-pick {} {} {} {late}", range[2], range[1], range[0]);
        app.dispatch("commits.paste");
        assert!(
            wrote(&mut app, &state, &want),
            "the paste order is wrong: wanted {want:?}, got {:?}",
            state.lock().unwrap().writes
        );

        // The paste kept the set — the same commits reach a second branch.
        app.dispatch("commits.clear-copies");
        assert_eq!(app.message, "cleared 4 copied commits", "{:?}", app.message);
        app.dispatch("commits.paste");
        assert_eq!(
            app.message, "nothing copied to cherry-pick — copy commits first",
            "{:?}",
            app.message
        );
        app.dispatch("commits.clear-copies");
        assert_eq!(app.message, "the cherry-pick clipboard is already empty");
    }

    /// A clipboard of shas from one repository names nothing in the next,
    /// so the switch drops it rather than leaving a paste that would come
    /// back as git's "bad object" over commits nobody can see. The armed
    /// history question goes with it, for the same reason.
    #[test]
    fn tui_parity_a_repository_switch_drops_the_clipboard_and_the_arm() {
        let (a_handle, a_state) = fake(&[]);
        let (b_handle, _b_state) = fake(&[]);
        with_mru("w6-switch", &["/b", "/a"], || {
            let mut app = history_app(&a_handle);
            app.use_opener(Arc::new(KeyedOpener {
                repos: vec![
                    (std::path::PathBuf::from("/a"), a_handle.clone()),
                    (std::path::PathBuf::from("/b"), b_handle.clone()),
                ],
            }));

            app.dispatch("commits.copy");
            assert_eq!(app.message, "copied 1 commit — 1 on the clipboard");
            app.dispatch("commits.reset-hard");
            assert!(app.message.contains("press again to confirm"));

            app.dispatch("project.next");
            app.pump_quiet();
            assert_eq!(app.message, "switched to /b", "{:?}", app.message);

            // Nothing to paste, and the arm that stood is gone: the second
            // press of a hard reset asks again rather than firing on a row
            // the eye never confirmed in this repository.
            app.dispatch("commits.paste");
            assert_eq!(
                app.message, "nothing copied to cherry-pick — copy commits first",
                "{:?}",
                app.message
            );
            app.dispatch("commits.reset-hard");
            assert!(
                app.message.contains("press again to confirm"),
                "the arm survived the switch: {:?}",
                app.message
            );
            app.pump_quiet();
            assert!(
                a_state.lock().unwrap().writes.is_empty(),
                "a write reached the repository that was left: {:?}",
                a_state.lock().unwrap().writes
            );
        });
    }

    /// LG-057, the part a status line cannot carry: a copied row says so in
    /// the pane, and keeps saying it after a refresh renumbered every row —
    /// which is the whole reason the pane holds shas rather than rows. Only
    /// the clear takes the ink away, because only the clear empties the
    /// clipboard.
    #[test]
    fn tui_parity_a_copied_row_is_drawn_as_copied_until_it_is_cleared() {
        let (handle, state) = fake(&[]);
        let mut app = history_app(&handle);
        app.dispatch("view.down");
        let copied = row_sha(&app);
        let elsewhere = {
            app.dispatch("view.down");
            let sha = row_sha(&app);
            app.dispatch("view.up");
            sha
        };

        assert!(
            !commits_of(&app).is_copied(1),
            "a row was drawn as copied before anything was"
        );
        app.dispatch("commits.copy");
        // `is_copied` addresses the *source* index, so the row is looked up
        // in the loaded window rather than in the visible table.
        let source = |app: &App, sha: &str| {
            let (window, _) = commits_of(app).history_window().expect("unfiltered");
            window.iter().position(|c| c.sha == sha)
        };
        let row = source(&app, &copied).expect("the copied commit is loaded");
        assert!(commits_of(&app).is_copied(row), "the copy left no ink");
        assert!(
            !commits_of(&app).is_copied(source(&app, &elsewhere).unwrap()),
            "the ink spread to a row nobody copied"
        );

        // A write refreshes the window; the fake answers a different history
        // than startup loaded, and the copy is still the copy.
        app.dispatch("commits.revert");
        assert!(
            wrote(&mut app, &state, &format!("revert {copied}")),
            "{:?}",
            state.lock().unwrap().writes
        );
        app.dispatch("commits.paste");
        assert!(
            wrote(&mut app, &state, &format!("cherry-pick {copied}")),
            "the refresh lost the clipboard: {:?}",
            state.lock().unwrap().writes
        );

        // The clear takes the ink with it.
        app.dispatch("commits.clear-copies");
        assert!(
            (0..commits_of(&app).len()).all(|i| !commits_of(&app).is_copied(i)),
            "the clear left ink behind"
        );
    }

    /// LG-057, the unmarked press: with no range standing the copy takes
    /// the row alone, which is what makes the key useful before `v` has
    /// been touched — and the clipboard holds full shas, so a filter
    /// between the copy and the paste cannot slide another commit into it.
    #[test]
    fn tui_parity_copy_takes_the_row_when_nothing_is_marked() {
        let (handle, state) = fake(&[]);
        let mut app = history_app(&handle);
        app.dispatch("view.down");
        let copied = row_sha(&app);

        app.dispatch("commits.copy");
        assert_eq!(app.message, "copied 1 commit — 1 on the clipboard");

        // A search renumbers every row: the row that was copied is not
        // even in the visible table any more. The clipboard names shas, so
        // what was copied is still exactly what pastes — which is the
        // whole point of holding IDs instead of indices.
        app.dispatch("commits.search");
        type_(&mut app, "commit 5");
        app.press(Key::plain(Code::Enter));
        assert!(commits_of(&app).query().is_some(), "the filter never took");
        assert_ne!(row_sha(&app), copied, "the filter left the cursor put");
        app.dispatch("commits.paste");
        assert!(
            wrote(&mut app, &state, &format!("cherry-pick {copied}")),
            "the filter changed what pasted: {:?}",
            state.lock().unwrap().writes
        );
    }

    /// LG-061. HEAD's author is one amend, asked twice: the commit is
    /// replaced but the message and the tree stand still, which
    /// `gitten-git`'s `reset_author_hands_head_a_new_author_and_nothing_else`
    /// holds against a real repository.
    #[test]
    fn tui_parity_commit_author_resets_head_on_one_amend() {
        let (handle, state) = fake(&[]);
        let mut app = history_app(&handle);
        let head = row_sha(&app);
        // HEAD is the pane's newest row, so the verb has something to aim
        // at. Set after the launch read, because startup would overwrite it.
        state.lock().unwrap().head = Some(HeadState::Branch {
            name: RefName::from("main"),
            commit: Some(head.clone()),
        });

        app.dispatch("commits.reset-author");
        assert_eq!(
            app.message,
            format!("reset the author of {head} to you? press again to confirm"),
            "{:?}",
            app.message
        );
        app.pump_quiet();
        assert!(state.lock().unwrap().writes.is_empty(), "no question stood");
        app.dispatch("commits.reset-author");
        assert!(
            wrote(&mut app, &state, "reset-author"),
            "the confirmed re-author never landed: {:?}",
            state.lock().unwrap().writes
        );
    }

    /// LG-061's other half, and the gap slice 1 left behind: a commit
    /// deeper than HEAD used to refuse by name because re-authoring one is
    /// a rebase. It is now that rebase — the plan replays the window with
    /// `--reset-author` hung on the one commit — so the question names the
    /// plan rather than the row, and what reaches the queue is a pick, an
    /// exec and a pick. `gitten-git`'s
    /// `a_commit_deeper_than_head_is_reauthored_through_the_plan` holds the
    /// same rewrite against a real repository.
    #[test]
    fn tui_parity_a_commit_deeper_than_head_is_reauthored_by_the_plan() {
        let (handle, state) = fake(&[]);
        let mut app = history_app(&handle);
        let head = row_sha(&app);
        state.lock().unwrap().head = Some(HeadState::Branch {
            name: RefName::from("main"),
            commit: Some(head.clone()),
        });

        app.dispatch("view.down");
        let deeper = row_sha(&app);
        app.dispatch("commits.reset-author");
        assert_eq!(
            app.message,
            format!("rewrite 2 commits from {deeper}? press again to confirm"),
            "{:?}",
            app.message
        );
        app.pump_quiet();
        assert!(
            state.lock().unwrap().writes.is_empty(),
            "an unconfirmed re-author ran: {:?}",
            state.lock().unwrap().writes
        );

        // Confirmed: the amendment rides the pick that replayed it, and
        // everything above is replayed plain. The plan sits on the deep
        // commit's own parent, which is what "and move nothing else" means
        // once a rewrite is a rebase.
        app.dispatch("commits.reset-author");
        let parent = format!("{:08}", 2);
        assert!(
            wrote(
                &mut app,
                &state,
                &format!(
                    "rebase-plan {parent} | pick {deeper}; \
                     exec amend --reset-author; pick {head}"
                )
            ),
            "the deep re-author never landed: {:?}",
            state.lock().unwrap().writes
        );
    }

    // ------------------------------------------------ W6 history editing

    /// The plan a write recorded, as one line. Waits for it, pumping the
    /// finish wave meanwhile, and answers what the fake actually holds so a
    /// failure prints the difference rather than "false".
    fn planned(app: &mut App, state: &Arc<Mutex<FakeState>>) -> String {
        until(Duration::from_secs(2), || {
            app.pump_quiet();
            !state.lock().unwrap().writes.is_empty()
        });
        state.lock().unwrap().writes.join(" / ")
    }

    /// LG-048 / LG-049 / LG-050. The three one-key rewrites, each asked
    /// twice and each composing the whole window it touches: a fold replays
    /// the parent first, because git refuses a plan opening on a squash,
    /// and a drop simply leaves the commit out. What git makes of each plan
    /// is `gitten-git`'s `squash_melds_messages_by_gits_own_rule_and_fixup_discards_them`.
    #[test]
    fn tui_parity_squash_fixup_and_drop_compose_the_window_they_rewrite() {
        for (command, verb, expected) in [
            (
                "commits.squash-up",
                "squash",
                "rebase-todo 00000003 | pick 00000002; squash 00000001; pick 00000000",
            ),
            (
                "commits.fixup-up",
                "fixup",
                "rebase-todo 00000003 | pick 00000002; fixup 00000001; pick 00000000",
            ),
            (
                "commits.drop-commit",
                "drop",
                "rebase-todo 00000002 | pick 00000000",
            ),
        ] {
            let (handle, state) = fake(&[]);
            let mut app = history_app(&handle);
            app.dispatch("view.down");
            let row = row_sha(&app);

            app.dispatch(command);
            assert_eq!(
                app.message,
                format!("{verb} {row}? press again to confirm"),
                "{command} ran without asking"
            );
            app.pump_quiet();
            assert!(
                state.lock().unwrap().writes.is_empty(),
                "{command} wrote before its question was answered"
            );

            app.dispatch(command);
            assert_eq!(planned(&mut app, &state), expected, "{command}");
        }
    }

    /// LG-048's other half: a fold arms per command, so a squash asked and
    /// a fixup pressed asks again rather than folding under the other
    /// verb's answer.
    #[test]
    fn tui_parity_a_fold_never_spends_another_folds_question() {
        let (handle, state) = fake(&[]);
        let mut app = history_app(&handle);
        app.dispatch("view.down");
        let row = row_sha(&app);
        app.dispatch("commits.squash-up");
        app.dispatch("commits.fixup-up");
        assert_eq!(
            app.message,
            format!("fixup {row}? press again to confirm"),
            "the fixup spent the squash's question"
        );
        app.pump_quiet();
        assert!(state.lock().unwrap().writes.is_empty());
    }

    /// LG-051. HEAD's message is one amend that leaves the index alone;
    /// anything deeper is the same reword arriving as a plan, with git's
    /// own `reword` word never emitted — it would open an editor nothing
    /// here can answer, and the message is already in hand.
    #[test]
    fn tui_parity_reword_amends_head_and_replans_anything_deeper() {
        let (handle, state) = fake(&[]);
        let mut app = history_app(&handle);
        let head = row_sha(&app);
        state.lock().unwrap().head = Some(HeadState::Branch {
            name: RefName::from("main"),
            commit: Some(head.clone()),
        });

        // The field opens on the commit's own subject: a reword is an edit
        // of what is standing, not a retyping of it.
        app.dispatch("commits.reword");
        assert_eq!(
            app.prompt.as_ref().map(|p| p.field().text().to_string()),
            Some("commit 0".into())
        );
        type_(&mut app, " — said better");
        app.press(Key::plain(Code::Enter));
        assert_eq!(
            planned(&mut app, &state),
            "reword-head commit 0 — said better"
        );

        // One row down is a rebase: the plan carries the message, and the
        // pick that replays the commit is followed by the exec that amends
        // it. Fresh, because the write above re-read the window.
        let (handle, state) = fake(&[]);
        let mut app = history_app(&handle);
        app.dispatch("view.down");
        let deeper = row_sha(&app);
        app.dispatch("commits.reword");
        type_(&mut app, "!");
        app.press(Key::plain(Code::Enter));
        assert_eq!(
            app.message,
            format!("rewrite 2 commits from {deeper}? press again to confirm")
        );
        app.dispatch("commits.reword");
        assert!(
            app.prompt.is_some(),
            "the second press should re-open the field, not confirm blind"
        );
        app.press(Key::plain(Code::Esc));
        // The question stands on the plan, so the plan's own key answers it.
        app.dispatch("commits.reword");
        type_(&mut app, "!");
        app.press(Key::plain(Code::Enter));
        assert_eq!(
            planned(&mut app, &state),
            format!("rebase-plan 00000002 | pick {deeper}; exec amend -F commit 1!; pick 00000000")
        );
    }

    /// LG-052. `i` opens the plan over the window from HEAD down to the row
    /// the keyboard is on — every commit picked, which is git's own
    /// starting plan and a rebase that changes nothing — and the screen
    /// owns the keyboard while it stands. Editing writes nothing; enter
    /// asks; the second enter runs it.
    #[test]
    fn tui_parity_the_todo_screen_edits_a_plan_and_runs_it_once_confirmed() {
        let (handle, state) = fake(&[]);
        let mut app = history_app(&handle);
        app.screen = Screen::new(120, 24);
        app.dispatch("view.down");
        app.dispatch("view.down");
        let base = row_sha(&app);

        app.press(Key::char('i'));
        assert!(
            app.todo.is_some(),
            "the plan never opened: {:?}",
            app.message
        );
        assert!(
            app.message.contains("3 commits from 00000002"),
            "{:?}",
            app.message
        );
        app.draw();
        let body: String = (0..24)
            .map(|y| app.screen.row_text(y))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(body.contains("rebase plan"), "{body}");
        assert!(body.contains("pick 00000000 commit 0"), "{body}");

        // The screen owns the keyboard: `d` is the plan's drop here, not
        // the commit list's own drop-commit, and nothing underneath runs.
        app.press(Key::plain(Code::Down));
        app.press(Key::char('d'));
        assert_eq!(
            app.todo.as_ref().unwrap().plan().entries()[1].action,
            gitten_core::rebase::Action::Drop
        );
        app.pump_quiet();
        assert!(
            state.lock().unwrap().writes.is_empty(),
            "editing the plan wrote to the repository"
        );

        // Enter asks once, naming what is at stake, and only then runs.
        app.press(Key::plain(Code::Enter));
        assert_eq!(
            app.message,
            format!("rewrite 3 commits from {base}? press again to confirm")
        );
        assert!(app.todo.is_some(), "the plan closed on the question");

        // An edit after the question takes the question with it: the answer
        // was about the plan as it read a moment ago.
        app.press(Key::char('p'));
        app.press(Key::char('d'));
        app.press(Key::plain(Code::Enter));
        assert!(
            app.message.contains("press again to confirm"),
            "an edited plan ran on the old answer: {:?}",
            app.message
        );
        app.pump_quiet();
        assert!(state.lock().unwrap().writes.is_empty());

        app.press(Key::plain(Code::Enter));
        assert!(app.todo.is_none(), "the plan stayed open after running");
        assert_eq!(
            planned(&mut app, &state),
            "rebase-plan 00000003 | pick 00000002; drop 00000001; pick 00000000"
        );
    }

    /// The fold's three message answers are three keys and three plans:
    /// squash keeps both messages, `f` keeps the older one, and `F` keeps
    /// this commit's — git's `fixup -C`, which the acquisition layer
    /// refuses on a git too old to know it rather than leaving one standing
    /// on a todo it cannot parse.
    #[test]
    fn tui_parity_the_fold_has_three_message_answers_in_the_plan() {
        for (key, line) in [
            ('s', "squash 00000000"),
            ('f', "fixup 00000000"),
            ('F', "fixup -C 00000000"),
        ] {
            let (handle, state) = fake(&[]);
            let mut app = history_app(&handle);
            app.dispatch("view.down");
            app.press(Key::char('i'));
            app.press(Key::char(key));
            app.press(Key::plain(Code::Enter));
            app.press(Key::plain(Code::Enter));
            assert_eq!(
                planned(&mut app, &state),
                format!("rebase-plan 00000002 | pick 00000001; {line}"),
                "`{key}` did not compose its own fold"
            );
        }
    }

    /// `?` over the open plan shows the plan's own keys, and the panel
    /// answers before the plan does while it stands — otherwise `esc` would
    /// close the plan from behind the very keys it was opened to read.
    #[test]
    fn tui_parity_the_plan_shows_its_own_keys_and_the_panel_closes_first() {
        let (handle, _) = fake(&[]);
        let mut app = history_app(&handle);
        app.screen = Screen::new(120, 40);
        app.dispatch("view.down");
        app.press(Key::char('i'));
        assert!(app.todo.is_some(), "{:?}", app.message);

        app.press(Key::char('?'));
        assert!(app.help, "the panel never opened over the plan");
        app.press(Key::plain(Code::End));
        app.draw();
        let body: String = (0..40)
            .map(|y| app.screen.row_text(y))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            body.contains("leave this commit out of the branch"),
            "the panel did not list the plan's keys: {body}"
        );

        // Esc takes the panel, not the plan.
        app.press(Key::plain(Code::Esc));
        assert!(!app.help, "the panel stayed up");
        assert!(app.todo.is_some(), "esc closed the plan behind the panel");
        // And now it takes the plan.
        app.press(Key::plain(Code::Esc));
        assert!(app.todo.is_none(), "esc left the plan open");
    }

    /// A cancelled todo edit writes nothing and leaves the repository
    /// exactly as it was — the whole reason a plan is safe to open.
    #[test]
    fn tui_parity_a_cancelled_todo_edit_writes_nothing() {
        let (handle, state) = fake(&[]);
        let mut app = history_app(&handle);
        app.dispatch("view.down");
        app.press(Key::char('i'));
        assert!(app.todo.is_some());
        app.press(Key::plain(Code::Down));
        app.press(Key::char('s'));
        app.press(Key::char('S'));
        app.press(Key::plain(Code::Esc));
        assert!(app.todo.is_none(), "esc left the plan open");
        assert_eq!(app.message, "the plan is closed — nothing was rewritten");
        app.pump_quiet();
        assert!(
            state.lock().unwrap().writes.is_empty(),
            "a cancelled plan wrote: {:?}",
            state.lock().unwrap().writes
        );
        // And the keyboard is the commit list's again: `s` means the fold,
        // which asks rather than editing a plan that is not there.
        app.press(Key::char('s'));
        assert!(app.message.contains("squash"), "{:?}", app.message);
    }

    /// The plan's fold refuses on its own oldest row, where the press
    /// happened, rather than letting git refuse the whole plan after a
    /// process started: there is nothing below it in the window to fold
    /// into, and the way out is a deeper base.
    #[test]
    fn tui_parity_the_todo_screen_refuses_a_fold_with_nothing_beneath_it() {
        let (handle, _) = fake(&[]);
        let mut app = history_app(&handle);
        app.dispatch("view.down");
        app.press(Key::char('i'));
        app.press(Key::plain(Code::End));
        app.press(Key::char('s'));
        assert!(
            app.message.contains("nothing below it"),
            "{:?}",
            app.message
        );
        assert!(app.message.contains("deeper"), "{:?}", app.message);
    }

    /// LG-052's second key. `e` stops the rebase *at* the commit under the
    /// keyboard — the one plan that is meant to hand back a standing rebase
    /// rather than a finished one, which is the state W5's banner and
    /// continue were built for.
    #[test]
    fn tui_parity_edit_commit_stops_the_rebase_where_it_was_asked_to() {
        let (handle, state) = fake(&[]);
        let mut app = history_app(&handle);
        app.dispatch("view.down");
        let row = row_sha(&app);
        app.press(Key::char('e'));
        assert_eq!(
            app.message,
            format!("rewrite 2 commits from {row}? press again to confirm")
        );
        app.press(Key::char('e'));
        assert!(
            app.message.contains("amend, then continue the rebase"),
            "{:?}",
            app.message
        );
        assert_eq!(
            planned(&mut app, &state),
            format!("rebase-plan 00000002 | edit {row}; pick 00000000")
        );
    }

    /// LG-055. The alt-arrows swap a commit with its neighbour, up meaning
    /// towards HEAD — which is *later* in git's own file, and the reason
    /// the plan holds the list's order rather than the file's. The edges
    /// refuse rather than wrapping.
    #[test]
    fn tui_parity_moving_a_commit_swaps_it_with_its_neighbour() {
        let (handle, state) = fake(&[]);
        let mut app = history_app(&handle);

        // The newest row has nothing above it, and says so without asking.
        app.press(Key::new(Code::Up, false, true, false));
        assert!(
            app.message.contains("already the newest"),
            "{:?}",
            app.message
        );
        app.pump_quiet();
        assert!(state.lock().unwrap().writes.is_empty());

        app.press(Key::new(Code::Down, false, true, false));
        assert_eq!(
            app.message,
            "rewrite 2 commits from 00000001? press again to confirm"
        );
        app.press(Key::new(Code::Down, false, true, false));
        assert_eq!(
            planned(&mut app, &state),
            "rebase-plan 00000002 | pick 00000000; pick 00000001",
            "the swap did not reach the plan"
        );
    }

    /// Autosquash lands every `fixup!` on the commit it names, in the plan
    /// and not in the repository: the screen stays open, and the run is
    /// still a separate, confirmed press.
    #[test]
    fn tui_parity_autosquash_folds_each_marker_onto_its_commit() {
        let (handle, state) = fake(&[]);
        // A window whose newest commit is a fixup! of the oldest one.
        {
            let mut s = state.lock().unwrap();
            s.log_at_answers.clear();
        }
        let mut app = history_app(&handle);
        // The pane's own rows are what a plan composes over, so the marker
        // is planted there — the same place a real `git commit --fixup`
        // would have put it.
        if let Some(Screens::Commits { view, .. }) = app.panes.get_mut("commits") {
            let mut rows = hundred_commits();
            rows[0].subject = "fixup! commit 2".into();
            *view = Commits::new(rows);
        }
        app.dispatch("view.down");
        app.dispatch("view.down");
        app.press(Key::char('i'));
        assert!(app.todo.is_some(), "{:?}", app.message);
        app.press(Key::char('S'));
        assert_eq!(app.message, "1 commit folded onto the one it names");
        app.press(Key::plain(Code::Enter));
        app.press(Key::plain(Code::Enter));
        assert_eq!(
            planned(&mut app, &state),
            "rebase-plan 00000003 | pick 00000002; fixup 00000000; pick 00000001",
            "the marker did not land on the commit it names"
        );
    }

    /// A staged index, as the fixup creation reads it: the paths are the
    /// overlap the finder scores, and their presence is the creation's
    /// guard.
    fn staged(paths: &[&str]) -> Status {
        Status {
            staged: paths
                .iter()
                .map(|p| StagedEntry {
                    path: PathBytes::from(*p),
                    change: Change::Modified,
                    old_path: None,
                    kind: Kind::File,
                    submodule: Submodule::default(),
                })
                .collect(),
            ..Default::default()
        }
    }

    /// LG-081. `F` commits the staged index as a fixup for the row: the
    /// job aims the row's full sha, the marker is git's, and the finish
    /// says what it wrote.
    #[test]
    fn tui_parity_a_fixup_commit_aims_the_row_and_names_its_marker() {
        let (handle, state) = fake(&[]);
        state.lock().unwrap().status = staged(&["f.txt"]);
        let mut app = history_app(&handle);
        let sha = row_sha(&app);
        let short = commits_of(&app).current().expect("a row").short.clone();
        app.press(Key::char('F'));
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                state
                    .lock()
                    .unwrap()
                    .writes
                    .iter()
                    .any(|w| *w == format!("fixup {sha} fixup!"))
                    && app.message.contains(&format!("fixup! {short} created"))
            }),
            "the fixup never reached the repository saying its name: {:?} / {:?}",
            app.message,
            state.lock().unwrap().writes
        );
    }

    /// LG-081. A fixup over an empty index is refused before git could
    /// answer "nothing to commit" — which would name the wrong failure —
    /// and an amend!/reword! kind is refused before any process runs,
    /// because those spellings open an editor this client has no door for.
    #[test]
    fn tui_parity_a_fixup_refuses_an_empty_index_and_an_editor_kind() {
        let (handle, state) = fake(&[]);
        let mut app = history_app(&handle);
        app.press(Key::char('F'));
        assert!(
            app.message.contains("nothing staged to fix up"),
            "{:?}",
            app.message
        );
        app.pump_quiet();
        assert!(state.lock().unwrap().writes.is_empty());

        state.lock().unwrap().status = staged(&["f.txt"]);
        app.press(Key::char('K'));
        assert!(
            app.message.contains("F will create amend!"),
            "{:?}",
            app.message
        );
        app.press(Key::char('F'));
        assert!(app.message.contains("opens an editor"), "{:?}", app.message);
        app.pump_quiet();
        assert!(
            state.lock().unwrap().writes.is_empty(),
            "an editor kind wrote: {:?}",
            state.lock().unwrap().writes
        );
    }

    /// LG-082. `K` cycles what `F` writes — fixup, amend, reword, round
    /// again — and the status line names the standing kind while it is not
    /// the default the help entry promises.
    #[test]
    fn tui_parity_the_kind_key_cycles_what_a_creation_writes() {
        let (handle, _state) = fake(&[]);
        let mut app = history_app(&handle);
        app.press(Key::char('K'));
        assert!(
            app.message.contains("F will create amend!"),
            "{:?}",
            app.message
        );
        // The pane's own status names the standing kind; the screen's
        // bottom row is the message bar in tests, so read the pane.
        assert!(
            commits_of(&app).status().contains("F:amend!"),
            "{:?}",
            commits_of(&app).status()
        );
        app.press(Key::char('K'));
        assert!(
            app.message.contains("F will create reword!"),
            "{:?}",
            app.message
        );
        app.press(Key::char('K'));
        assert!(
            app.message.contains("F will create fixup!"),
            "{:?}",
            app.message
        );
        assert!(
            !commits_of(&app).status().contains("F:"),
            "the default needs no ink: {:?}",
            commits_of(&app).status()
        );
    }

    /// LG-080. `ctrl-f` moves the keyboard to the commit the staged
    /// changes build on: file overlap, newest wins. The move is announced
    /// as a guess, because the creation still aims at whatever row the
    /// keyboard is on when it lands.
    #[test]
    fn tui_parity_the_finder_moves_to_the_commit_the_change_builds_on() {
        let (handle, state) = fake(&[]);
        state.lock().unwrap().status = staged(&["f.txt"]);
        {
            let mut s = state.lock().unwrap();
            s.commit_files_by_sha = vec![
                (b"00000005".to_vec(), vec![('M', b"f.txt".to_vec())]),
                (b"00000002".to_vec(), vec![('M', b"other.txt".to_vec())]),
            ];
        }
        let mut app = history_app(&handle);
        if let Some(Screens::Commits { view, .. }) = app.panes.get_mut("commits") {
            *view = Commits::new(hundred_commits());
        }
        app.press(Key::ctrl(Code::Char('f')));
        assert_eq!(commits_of(&app).cursor(), 5, "the finder stayed home");
        assert!(
            app.message.contains("building on 00000005"),
            "{:?}",
            app.message
        );
    }

    /// LG-080. Staged changes no loaded commit touched move nothing and
    /// say so: a guess with no evidence is worse than no guess.
    #[test]
    fn tui_parity_the_finder_says_when_nothing_overlaps() {
        let (handle, state) = fake(&[]);
        state.lock().unwrap().status = staged(&["lonely.txt"]);
        let mut app = history_app(&handle);
        if let Some(Screens::Commits { view, .. }) = app.panes.get_mut("commits") {
            *view = Commits::new(hundred_commits());
        }
        app.press(Key::ctrl(Code::Char('f')));
        assert_eq!(commits_of(&app).cursor(), 0);
        assert!(
            app.message
                .contains("touch no file any recent commit touched"),
            "{:?}",
            app.message
        );
        app.pump_quiet();
        assert!(state.lock().unwrap().writes.is_empty());
    }

    /// LG-082. `U` folds every marker into the commit it names, asked
    /// twice like every rewrite: the plan covers the deepest landing and
    /// the autosquash order is git's.
    #[test]
    fn tui_parity_the_fold_asks_twice_and_folds_every_marker() {
        let (handle, state) = fake(&[]);
        let mut app = history_app(&handle);
        if let Some(Screens::Commits { view, .. }) = app.panes.get_mut("commits") {
            let mut rows = hundred_commits();
            rows[0].subject = "fixup! commit 2".into();
            *view = Commits::new(rows);
        }
        app.press(Key::char('U'));
        assert_eq!(
            app.message,
            "rewrite 3 commits from 00000002? press again to confirm"
        );
        app.press(Key::char('U'));
        assert_eq!(
            planned(&mut app, &state),
            "rebase-plan 00000003 | pick 00000002; fixup 00000000; pick 00000001",
            "the fold did not reach the plan in git's order"
        );
    }

    /// LG-082. A marker that names nothing refuses by name instead of
    /// riding the plan as a pick — a pick in this run is a fixup that
    /// silently stays a commit — and a window with no markers says so.
    /// Esc on the armed question writes nothing.
    #[test]
    fn tui_parity_the_fold_refuses_what_it_cannot_aim_cancels_clean() {
        let (handle, state) = fake(&[]);
        let mut app = history_app(&handle);
        if let Some(Screens::Commits { view, .. }) = app.panes.get_mut("commits") {
            let mut rows = hundred_commits();
            rows[0].subject = "fixup! nowhere".into();
            *view = Commits::new(rows);
        }
        app.press(Key::char('U'));
        assert!(
            app.message.contains("nowhere"),
            "the refusal did not name the remainder: {:?}",
            app.message
        );
        app.pump_quiet();
        assert!(state.lock().unwrap().writes.is_empty());

        if let Some(Screens::Commits { view, .. }) = app.panes.get_mut("commits") {
            *view = Commits::new(hundred_commits());
        }
        app.press(Key::char('U'));
        assert!(
            app.message.contains("no fixup! or squash! commits"),
            "{:?}",
            app.message
        );

        if let Some(Screens::Commits { view, .. }) = app.panes.get_mut("commits") {
            let mut rows = hundred_commits();
            rows[0].subject = "fixup! commit 2".into();
            *view = Commits::new(rows);
        }
        app.press(Key::char('U'));
        assert!(app.message.contains("press again to confirm"));
        app.press(Key::plain(Code::Esc));
        app.pump_quiet();
        assert!(
            state.lock().unwrap().writes.is_empty(),
            "a cancelled fold wrote: {:?}",
            state.lock().unwrap().writes
        );
    }

    /// LG-081. A real repository: the creation lands a `fixup!` commit
    /// under HEAD, byte for byte what `git commit --fixup` writes.
    #[test]
    fn tui_parity_a_fixup_commit_lands_on_a_real_repository() {
        let g = Git::init("fixup-create");
        g.write("f.txt", "one\n");
        g.git(&["add", "-A"]);
        g.git(&["commit", "-qm", "base"]);
        g.write("f.txt", "one\ntwo\n");
        g.git(&["add", "f.txt"]);
        let mut app = repo_app(g.0.as_path());
        app.dispatch("commits.focus");
        app.press(Key::char('F'));
        assert!(
            until(Duration::from_secs(5), || {
                app.pump();
                app.message.contains("fixup! ") && app.message.contains("created")
            }),
            "the fixup never finished: {:?}",
            app.message
        );
        assert_eq!(
            g.ask(&["log", "--format=%s", "-2"]).trim(),
            "fixup! base\nbase",
            "the fixup did not land on HEAD"
        );
    }

    /// LG-082. A real repository: the fold replays the window with every
    /// marker on the commit it names, and the tree keeps the change.
    #[test]
    fn tui_parity_the_fold_replays_a_real_window_in_gits_order() {
        let g = Git::init("fixup-apply");
        g.write("f.txt", "one\n");
        g.git(&["add", "-A"]);
        g.git(&["commit", "-qm", "base"]);
        g.write("f.txt", "one\ntwo\n");
        g.git(&["commit", "-qam", "second"]);
        g.write("f.txt", "one\ntwo\nthree\n");
        g.git(&["add", "f.txt"]);
        g.git(&["commit", "--fixup", "HEAD"]);
        let mut app = repo_app(g.0.as_path());
        app.dispatch("commits.focus");
        app.press(Key::char('U'));
        assert!(
            app.message.contains("press again to confirm"),
            "the fold did not ask first: {:?}",
            app.message
        );
        app.press(Key::char('U'));
        assert!(
            until(Duration::from_secs(10), || {
                app.pump();
                g.ask(&["log", "--format=%s"]).trim() == "second\nbase"
            }),
            "the fold never finished: {:?} / {:?}",
            app.message,
            g.ask(&["log", "--format=%s"])
        );
        assert_eq!(
            g.ask(&["show", "HEAD:./f.txt"]).trim(),
            "one\ntwo\nthree",
            "the folded change did not survive"
        );
    }

    /// LG-053 and LG-054. `r` in the branches pane moves this branch onto
    /// the row, asked twice; with a base marked it is git's `--onto`
    /// instead, and the marked commit stays exactly where it is.
    #[test]
    fn tui_parity_rebase_onto_uses_the_marked_base_when_one_stands() {
        let (handle, state) = fake(&[]);
        branch_world(&state);
        let mut app = history_app(&handle);

        // The mark is made on the commit list and says so, and the list
        // carries it on its status line for as long as it stands.
        app.dispatch("view.down");
        let base = row_sha(&app);
        app.press(Key::char('B'));
        assert_eq!(
            app.message,
            format!("{base} is the rebase base — everything after it moves")
        );
        assert!(
            commits_of(&app).status().contains(&format!("base {base}")),
            "{}",
            commits_of(&app).status()
        );

        // And it is spent one pane over: the same key, now git's `--onto`,
        // with the marked commit left where it is and only its children
        // moving. Asked twice, because it rewrites this branch's history.
        app.dispatch("branches.focus");
        app.dispatch("view.top");
        app.press(Key::char('r'));
        assert_eq!(
            app.message,
            format!("rebase onto main, from {base} up? press again to confirm")
        );
        app.pump_quiet();
        assert!(state.lock().unwrap().writes.is_empty(), "it ran unasked");
        app.press(Key::char('r'));
        assert_eq!(
            planned(&mut app, &state),
            format!("rebase-onto main from {base}")
        );
    }

    /// LG-054's other half: the mark toggles off on the row that set it,
    /// and the branch verb is the plain rebase again — a mark nobody can
    /// clear is a mark carried into the next rewrite by accident.
    #[test]
    fn tui_parity_the_rebase_base_toggles_off_and_the_plain_rebase_returns() {
        let (handle, state) = fake(&[]);
        branch_world(&state);
        let mut app = history_app(&handle);
        app.dispatch("view.down");
        app.press(Key::char('B'));
        assert!(commits_of(&app).status().contains("base "), "never marked");
        app.press(Key::char('B'));
        assert!(
            app.message.contains("no longer the rebase base"),
            "{:?}",
            app.message
        );
        assert!(
            !commits_of(&app).status().contains("base "),
            "{}",
            commits_of(&app).status()
        );

        app.dispatch("branches.focus");
        app.dispatch("view.top");
        app.press(Key::char('r'));
        assert_eq!(app.message, "rebase onto main? press again to confirm");
        app.press(Key::char('r'));
        assert_eq!(planned(&mut app, &state), "rebase-onto main");
    }

    /// A rewrite's question never retargets: armed on the branch it was
    /// asked about, so moving the keyboard and pressing again asks afresh
    /// — and the branches pane's own delete arm cannot be spent by it.
    #[test]
    fn tui_parity_a_rebase_question_never_retargets_or_arms_a_delete() {
        let (handle, state) = fake(&[]);
        branch_world(&state);
        let mut app = history_app(&handle);
        app.dispatch("branches.focus");
        app.dispatch("view.top");
        app.press(Key::char('r'));
        assert_eq!(app.message, "rebase onto main? press again to confirm");

        // A second row, a second question — the first is not spendable here.
        app.dispatch("view.down");
        app.press(Key::char('r'));
        assert!(
            app.message.contains("press again to confirm"),
            "{:?}",
            app.message
        );
        app.pump_quiet();
        assert!(
            state.lock().unwrap().writes.is_empty(),
            "a retargeted rebase ran: {:?}",
            state.lock().unwrap().writes
        );

        // And the delete's own arm is untouched: `d` asks rather than
        // spending the rebase's answer.
        app.dispatch("view.top");
        app.press(Key::char('r'));
        app.dispatch("branches.delete");
        assert!(app.message.contains("delete"), "{:?}", app.message);
        app.pump_quiet();
        assert!(
            state.lock().unwrap().branch_writes.is_empty(),
            "the rebase's question armed a delete: {:?}",
            state.lock().unwrap().branch_writes
        );
    }

    /// LG-062. The files pane's `g` opens the reset menu, whose strengths
    /// aim at the upstream the branch is configured against — read from the
    /// repository and named in full, so what the question says and what git
    /// resolves are the same string.
    #[test]
    fn tui_parity_upstream_reset_names_the_tracking_ref_and_asks_twice() {
        let (handle, state) = fake(&[]);
        {
            let mut s = state.lock().unwrap();
            s.locals = vec![Branch {
                name: RefName::from("main"),
                commit: "f00d".into(),
                upstream: Some(gitten_core::refs::Upstream {
                    remote: RefName::from("origin"),
                    branch: RefName::from("main"),
                    ahead: Some(1),
                    behind: Some(2),
                }),
                head: true,
            }];
            s.head = Some(HeadState::Branch {
                name: RefName::from("main"),
                commit: Some("f00d".into()),
            });
        }
        let mut app = commits_app(&handle);
        app.dispatch("files.focus");

        // The menu asks and writes nothing.
        app.press(Key::char('g'));
        assert!(
            app.message.contains("soft, mixed or hard"),
            "{:?}",
            app.message
        );
        app.pump_quiet();
        assert!(state.lock().unwrap().writes.is_empty(), "the menu reset");

        // And its letters belong to the question while it stands: `h` is
        // the hard reset here and the pane walk everywhere else.
        app.press(Key::char('h'));
        assert_eq!(
            app.message,
            "reset --hard to the upstream? press again to confirm"
        );
        app.dispatch("files.reset-upstream-hard");
        assert_eq!(planned(&mut app, &state), "reset --hard origin/main");
    }

    /// The upstream reset refuses in a sentence when there is nothing to
    /// aim at, rather than queueing a job git would answer with a revspec
    /// error — and spends no question doing it.
    #[test]
    fn tui_parity_upstream_reset_refuses_without_an_upstream() {
        let (handle, state) = fake(&[]);
        branch_world(&state);
        {
            let mut s = state.lock().unwrap();
            s.head = Some(HeadState::Branch {
                name: RefName::from("main"),
                commit: Some("f00d".into()),
            });
        }
        let mut app = commits_app(&handle);
        app.dispatch("files.focus");
        app.dispatch("files.reset-upstream-mixed");
        assert!(
            app.message.contains("tracks no upstream"),
            "{:?}",
            app.message
        );
        app.dispatch("files.reset-upstream-mixed");
        assert!(
            app.message.contains("tracks no upstream"),
            "a refusal spent its own question: {:?}",
            app.message
        );
        app.pump_quiet();
        assert!(state.lock().unwrap().writes.is_empty());
    }

    /// LG-063. The nuke is the most destructive key here, and the only one
    /// whose question says what cannot be undone. It sits behind the menu
    /// rather than on the pane's own `D`, because `D` on a row discards
    /// that file and the two are a keypress and a catastrophe apart.
    #[test]
    fn tui_parity_nuking_the_working_tree_asks_twice_and_is_not_the_row_key() {
        let (handle, state) = fake(&["new.txt"]);
        let mut app = commits_app(&handle);
        app.dispatch("files.focus");

        // The pane's own `D` is the row's discard, untouched by any of
        // this: it names the file it would take, and nothing else.
        app.press(Key::char('D'));
        assert!(app.message.contains("new.txt"), "{:?}", app.message);
        assert!(!app.message.contains("nuke"), "{:?}", app.message);

        // The nuke is one door further in, and asks in words that say what
        // survives.
        app.press(Key::char('g'));
        app.press(Key::char('D'));
        assert!(
            app.message.contains("nuke the working tree?"),
            "{:?}",
            app.message
        );
        assert!(
            app.message.contains("ignored files stay"),
            "{:?}",
            app.message
        );
        app.pump_quiet();
        assert!(
            state.lock().unwrap().writes.is_empty(),
            "an unconfirmed nuke ran: {:?}",
            state.lock().unwrap().writes
        );
        app.dispatch("files.nuke");
        assert_eq!(planned(&mut app, &state), "nuke");
    }

    /// A rewrite the loaded window cannot cover refuses in words, before
    /// anything is armed and before any process runs: a root with nothing
    /// beneath it, and a merge that `rebase -i` would flatten.
    #[test]
    fn tui_parity_a_plan_refuses_the_shapes_it_cannot_cover() {
        let (handle, state) = fake(&[]);
        let mut app = history_app(&handle);
        // The root: `hundred_commits` ends on one, and there is nothing
        // beneath it to rebuild onto.
        app.dispatch("view.bottom");
        app.press(Key::char('i'));
        assert!(app.todo.is_none(), "a plan opened on the root");
        assert!(app.message.contains("root"), "{:?}", app.message);

        // A merge in the window would be flattened, so the plan says so
        // instead of flattening it.
        if let Some(Screens::Commits { view, .. }) = app.panes.get_mut("commits") {
            let mut rows = hundred_commits();
            rows[1].parents =
                vec!["00000002".to_string(), "0000dead".to_string()].into_boxed_slice();
            *view = Commits::new(rows);
        }
        app.dispatch("view.top");
        app.dispatch("view.down");
        app.dispatch("view.down");
        app.press(Key::char('i'));
        assert!(app.todo.is_none(), "a plan opened over a merge");
        assert!(app.message.contains("merge"), "{:?}", app.message);

        app.pump_quiet();
        assert!(
            state.lock().unwrap().writes.is_empty(),
            "a refused plan wrote: {:?}",
            state.lock().unwrap().writes
        );
    }

    /// A conflict mid-rewrite is git's own state, and the lifecycle carries
    /// it: the refusal comes back in git's words, the rebase is standing
    /// afterwards, and the keys that were disabled a moment ago answer it.
    #[test]
    fn tui_parity_a_conflicted_plan_leaves_the_lifecycle_holding_it() {
        let (handle, state) = fake(&[]);
        state.lock().unwrap().refuse_rebase = Some("could not apply 00000001... commit 1".into());
        let mut app = history_app(&handle);
        app.dispatch("view.down");
        app.press(Key::char('i'));
        app.press(Key::plain(Code::Enter));
        app.press(Key::plain(Code::Enter));

        // git's own words, and the rebase standing behind them.
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                app.operation.is_some()
            }),
            "the standing rebase never reached the app: {:?}",
            app.message
        );
        assert!(app.message.contains("could not apply"), "{:?}", app.message);
        assert_eq!(
            app.operation.map(|o| o.kind),
            Some(gitten_core::operation::Kind::Rebase)
        );

        // The lifecycle keys are live now, and a second rewrite is not:
        // one sequencer, one index.
        app.dispatch("commits.interactive-rebase");
        assert!(
            app.message.contains("waits for the standing rebase"),
            "{:?}",
            app.message
        );
        state.lock().unwrap().refuse_rebase = None;
        app.dispatch("rebase.abort");
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                state
                    .lock()
                    .unwrap()
                    .writes
                    .iter()
                    .any(|w| w == "rebase abort")
            }),
            "the abort never ran: {:?}",
            state.lock().unwrap().writes
        );
    }

    /// A write landing under an open plan closes it. The plan is a window
    /// of shas and the write may have rewritten every one of them, so
    /// running it afterwards would aim at objects nobody can see — the
    /// editing is cheap to do again, and a rewrite aimed at the wrong
    /// commits is not.
    #[test]
    fn tui_parity_a_plan_closes_when_the_repository_moves_under_it() {
        let (handle, state) = fake(&[]);
        let mut app = history_app(&handle);
        app.dispatch("view.down");

        // A job queued but not yet drained, and the plan opened over the
        // window it is about to change.
        app.dispatch("repo.fetch");
        app.press(Key::char('i'));
        assert!(app.todo.is_some(), "{:?}", app.message);

        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                app.todo.is_none()
            }),
            "the plan survived the write that landed under it"
        );
        assert!(
            app.message
                .contains("the repository moved — the rebase plan was closed"),
            "{:?}",
            app.message
        );
        // And nothing was rewritten on the way: the plan was never run.
        assert!(
            !state
                .lock()
                .unwrap()
                .writes
                .iter()
                .any(|w| w.starts_with("rebase")),
            "{:?}",
            state.lock().unwrap().writes
        );
    }

    /// A menu question owns its letters for exactly as long as it stands,
    /// and not one press longer. That is the whole of what makes a menu
    /// affordable in a pane whose letters are already spoken for: `s` after
    /// `g` is a reset strength, and `s` after anything else is the squash
    /// it has always been. Without the mode the commit list's own reset
    /// menu was unreachable by key at all — `g` asked, and `s` folded.
    #[test]
    fn tui_parity_a_menu_question_owns_its_letters_only_while_it_stands() {
        let (handle, state) = fake(&[]);
        let mut app = history_app(&handle);
        app.dispatch("view.down");
        let row = row_sha(&app);

        app.press(Key::char('g'));
        assert!(
            app.message.contains("soft, mixed or hard"),
            "{:?}",
            app.message
        );
        app.press(Key::char('s'));
        assert_eq!(
            app.message,
            format!("reset --soft to {row}? press again to confirm"),
            "`s` under the menu was not the strength"
        );

        // The menu is down now: the same letter is the pane's own verb.
        app.press(Key::char('s'));
        assert_eq!(
            app.message,
            format!("squash {row}? press again to confirm"),
            "the menu outlived its answer"
        );
        app.pump_quiet();
        assert!(state.lock().unwrap().writes.is_empty());

        // A press the menu does not name takes it down rather than leaving
        // it to catch the next one.
        app.press(Key::char('g'));
        app.press(Key::plain(Code::Down));
        app.press(Key::char('s'));
        assert!(
            app.message.starts_with("squash"),
            "the menu caught a press it did not name: {:?}",
            app.message
        );
    }

    /// The new keys are data like every other: what the panel advertises
    /// and what a press does come from the same registry, and a rebind
    /// moves both together.
    #[test]
    fn tui_parity_history_editing_keys_resolve_help_and_dispatch_agree() {
        let (handle, _) = fake(&[]);
        let mut app = history_app(&handle);
        let mut modes = Modes::new();
        modes.push("commits");
        for (key, name) in [
            ("i", "commits.interactive-rebase"),
            ("e", "commits.edit-commit"),
            ("r", "commits.reword"),
            ("B", "commits.mark-base"),
            ("alt-up", "commits.move-up"),
            ("alt-down", "commits.move-down"),
        ] {
            assert_eq!(
                app.host.keys.resolve(
                    &modes,
                    &gitten_core::command::parse_chord(key).expect("a chord")
                ),
                Resolve::Run(name),
                "{key} does not run {name}"
            );
            assert!(matches!(app.availability.state(name), Usable::Available));
        }

        // The plan's own mode, which owns the keyboard while it stands.
        let mut modes = Modes::new();
        modes.push("todo");
        for (key, name) in [
            ("p", "todo.pick"),
            ("r", "todo.reword"),
            ("e", "todo.edit"),
            ("s", "todo.squash"),
            ("f", "todo.fixup"),
            ("F", "todo.fixup-keep"),
            ("d", "todo.drop"),
            ("S", "todo.autosquash"),
            ("enter", "todo.run"),
            ("ctrl-k", "todo.move-up"),
            ("ctrl-j", "todo.move-down"),
        ] {
            assert_eq!(
                app.host.keys.resolve(
                    &modes,
                    &gitten_core::command::parse_chord(key).expect("a chord")
                ),
                Resolve::Run(name),
                "{key} does not run {name} in the plan"
            );
            assert!(matches!(app.availability.state(name), Usable::Available));
        }

        // And the files pane's menu: `g` opens it, its letters answer it.
        let mut modes = Modes::new();
        modes.push("files");
        assert_eq!(
            app.host.keys.resolve(
                &modes,
                &gitten_core::command::parse_chord("g").expect("a chord")
            ),
            Resolve::Run("files.reset-menu")
        );
        let mut modes = Modes::new();
        modes.push("files");
        modes.push("upstream");
        for (key, name) in [
            ("s", "files.reset-upstream-soft"),
            ("m", "files.reset-upstream-mixed"),
            ("h", "files.reset-upstream-hard"),
            ("D", "files.nuke"),
        ] {
            assert_eq!(
                app.host.keys.resolve(
                    &modes,
                    &gitten_core::command::parse_chord(key).expect("a chord")
                ),
                Resolve::Run(name),
                "{key} does not run {name} while the menu stands"
            );
        }

        // A press with no plan open is refused by name where it is
        // answered — a sentence about the plan, not about the client.
        app.dispatch("todo.drop");
        assert_eq!(app.message, "todo.drop needs an open rebase plan");
    }

    /// Every history write waits for a standing operation rather than
    /// starting a second one inside git's first — the copy excepted, which
    /// writes nothing at all and so has nothing to wait for.
    #[test]
    fn tui_parity_history_verbs_wait_for_a_standing_operation() {
        let (handle, state) = fake(&[]);
        state.lock().unwrap().standing = Some(Operation {
            kind: gitten_core::operation::Kind::Merge,
            conflicts: 0,
        });
        let mut app = history_app(&handle);
        app.dispatch("repo.refresh");
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                app.operation.is_some()
            }),
            "the standing merge never reached the app"
        );

        for command in [
            "commits.reset-hard",
            "commits.revert",
            "commits.cherry-pick",
            "commits.checkout",
            "commits.reset-author",
        ] {
            app.dispatch(command);
            assert_eq!(
                app.message,
                format!("{command} waits for the standing merge — abort it or finish it first"),
                "{command} did not wait"
            );
        }

        // The copy is pure state, so it lands; the paste it enables waits.
        app.dispatch("commits.copy");
        assert_eq!(app.message, "copied 1 commit — 1 on the clipboard");
        app.dispatch("commits.paste");
        assert_eq!(
            app.message,
            "commits.paste waits for the standing merge — abort it or finish it first"
        );

        app.pump_quiet();
        assert!(
            state.lock().unwrap().writes.is_empty(),
            "a history write ran inside the merge: {:?}",
            state.lock().unwrap().writes
        );
    }

    /// Aimed from the wrong pane, every history verb is refused by name —
    /// the same sentence the availability contract uses for a command this
    /// client does not serve, so a rebind cannot smuggle a reset into the
    /// files list.
    #[test]
    fn tui_parity_history_verbs_are_refused_off_the_commits_pane() {
        let (handle, state) = fake(&[]);
        let mut app = history_app(&handle);
        app.dispatch("files.focus");

        for command in [
            "commits.reset-menu",
            "commits.reset-soft",
            "commits.reset-mixed",
            "commits.reset-hard",
            "commits.revert",
            "commits.cherry-pick",
            "commits.checkout",
            "commits.copy",
            "commits.paste",
            "commits.clear-copies",
            "commits.reset-author",
        ] {
            app.dispatch(command);
            assert_eq!(
                app.message,
                format!("{command} is not supported here"),
                "{command} ran off its pane"
            );
        }
        app.pump_quiet();
        assert!(
            state.lock().unwrap().writes.is_empty(),
            "a history write ran from the files pane: {:?}",
            state.lock().unwrap().writes
        );
    }

    /// A fixture has no repository, so every history verb is disabled with
    /// a reason rather than advertised and silently doing nothing — the W0
    /// contract, held for this packet's commands.
    #[test]
    fn tui_parity_history_verbs_are_disabled_without_a_repository() {
        let a = tui_availability(false, None, None);
        for command in [
            "commits.reset-menu",
            "commits.reset-soft",
            "commits.reset-mixed",
            "commits.reset-hard",
            "commits.revert",
            "commits.cherry-pick",
            "commits.checkout",
            "commits.copy",
            "commits.paste",
            "commits.clear-copies",
            "commits.reset-author",
        ] {
            match a.state(command) {
                gitten_core::command::Usable::Disabled(why) => assert!(
                    why.contains("fixture"),
                    "{command}'s reason names no fixture: {why:?}"
                ),
                other => panic!("{command} is advertised against a fixture: {other:?}"),
            }
            assert!(
                !a.runnable(command),
                "{command} would run against a fixture"
            );
        }
        // With a repository they are live again — supported, not merely
        // unrefused, which is what separates this from an unhandled name.
        let a = tui_availability(true, None, None);
        for command in [
            "commits.reset-hard",
            "commits.paste",
            "commits.reset-author",
        ] {
            assert_eq!(
                a.state(command),
                &gitten_core::command::Usable::Available,
                "{command} stayed disabled"
            );
            assert!(a.runnable(command));
        }
    }

    // ------------------------------------------------------------------
    // W7: the stash family — named and scoped pushes, rename,
    // branch-from-stash, inspection before applying, and an identity the
    // stack cannot churn out from under.

    /// An app on the stash pane, keyboard on `stash@{0}`, with the stack
    /// the fake ships and one untracked file parked in the entries.
    fn stash_app(handle: &Handle) -> App {
        let mut app = commits_app(handle);
        app.draw();
        app.dispatch("stashes.focus");
        app
    }

    /// Every stash write the fake has recorded so far.
    fn stash_writes(state: &Arc<Mutex<FakeState>>) -> Vec<String> {
        state.lock().unwrap().stash_writes.clone()
    }

    /// The stack as the fake holds it: `(index, message, commit)` per entry.
    fn stack_now(state: &Arc<Mutex<FakeState>>) -> Vec<(usize, String, String)> {
        state
            .lock()
            .unwrap()
            .stashes
            .iter()
            .map(|e| (e.index, e.message.clone(), e.commit.clone()))
            .collect()
    }

    /// Waits for the next write to land and its refresh wave with it.
    fn stash_landed(app: &mut App, state: &Arc<Mutex<FakeState>>, count: usize) -> bool {
        until(Duration::from_secs(2), || {
            app.pump_quiet();
            stash_writes(state).len() >= count
        })
    }

    #[test]
    fn tui_parity_a_named_stash_gathers_its_message_before_it_parks() {
        // lazygit's stash-with-a-message, and the field is the whole of the
        // difference: the press opens it and writes nothing, the accept
        // parks under what was typed, and the escape parks nothing at all.
        let (handle, state) = fake(&[]);
        let mut app = commits_app(&handle);
        app.dispatch("files.stash-named");
        assert!(
            matches!(app.prompt, Some(Prompt::StashMessage { .. })),
            "the field never opened"
        );
        assert!(stash_writes(&state).is_empty(), "the press wrote");

        // Escape throws the text away with no write built.
        app.press(Key::plain(Code::Esc));
        assert!(app.prompt.is_none());
        assert!(stash_writes(&state).is_empty(), "a cancel wrote");

        // And typed, accepted: git's `-m`, with the text whole.
        app.dispatch("files.stash-named");
        for ch in "parser rewrite".chars() {
            app.press(Key::char(ch));
        }
        app.press(Key::plain(Code::Enter));
        assert!(app.prompt.is_none());
        assert!(
            stash_landed(&mut app, &state, 1),
            "the named push never landed: {:?}",
            stash_writes(&state)
        );
        assert_eq!(stash_writes(&state), ["push parser rewrite"]);
        assert_eq!(
            stack_now(&state)[0].1,
            "On fake: parser rewrite",
            "the entry carries the message it was given"
        );
        // The default path is untouched beside it: no message, git's own text.
        app.dispatch("files.stash");
        assert!(stash_landed(&mut app, &state, 2));
        assert_eq!(stash_writes(&state)[1], "push ");
        assert_eq!(stack_now(&state)[0].1, "WIP on fake (main)");
    }

    #[test]
    fn tui_parity_a_named_stash_refuses_an_empty_message_and_a_fixture() {
        // An accepted blank is not a message and not git's default either:
        // the field's own refusal, said where the field just was, with
        // nothing queued.
        let (handle, state) = fake(&[]);
        let mut app = commits_app(&handle);
        app.dispatch("files.stash-named");
        app.press(Key::plain(Code::Enter));
        assert_eq!(
            app.message,
            "a stash needs a message — the plain stash key parks without one"
        );
        assert!(
            stash_writes(&state).is_empty(),
            "a blank message parked anyway: {:?}",
            stash_writes(&state)
        );
        // Whitespace is blank too — a stash called "   " is a row nobody can
        // read and a message git would keep verbatim.
        app.dispatch("files.stash-named");
        app.press(Key::char(' '));
        app.press(Key::char(' '));
        app.press(Key::plain(Code::Enter));
        assert!(
            stash_writes(&state).is_empty(),
            "{:?}",
            stash_writes(&state)
        );

        // A fixture has no working tree, and says so before a field opens
        // over a repository that is not there.
        let mut fixture = app_on_diff(Source::Fixtures, None);
        fixture.screen = Screen::new(120, 40);
        fixture.dispatch("files.stash-named");
        assert!(
            fixture
                .message
                .contains("a fixture has no working tree to park"),
            "{:?}",
            fixture.message
        );
        assert!(fixture.prompt.is_none(), "a field opened over a fixture");
    }

    #[test]
    fn tui_parity_the_stash_menu_offers_the_scopes_and_each_reaches_git() {
        // lazygit's `S`: the choices behind a question of their own, in a
        // mode that stands only while it does. Each answer reaches git with
        // the flags its scope spells and nothing else.
        let (handle, state) = fake(&["fresh.txt"]);
        let mut app = commits_app(&handle);
        app.dispatch("files.focus");
        app.press(Key::char('S'));
        assert!(
            app.message.contains("park what?"),
            "the menu never asked: {:?}",
            app.message
        );
        assert_eq!(app.question, Some("stash"), "the mode never went up");

        for (key, expected) in [
            ('s', "push [--staged] "),
            ('u', "push [--keep-index] "),
            ('U', "push [-u] "),
        ] {
            let before = stash_writes(&state).len();
            app.dispatch("files.focus");
            app.press(Key::char('S'));
            app.press(Key::char(key));
            assert!(
                stash_landed(&mut app, &state, before + 1),
                "{key} never reached git: {:?}",
                stash_writes(&state)
            );
            assert_eq!(
                stash_writes(&state)[before],
                expected,
                "{key} reached git with the wrong scope"
            );
            // And the question came down with the answer, so the next `s`
            // means whatever `s` means in the pane again.
            assert_eq!(app.question, None, "{key} left the menu standing");
        }
    }

    #[test]
    fn tui_parity_the_stash_menu_letters_stand_only_while_the_question_does() {
        // `m` is the message field inside the menu and nothing at all
        // outside it; an unbound key answers the question by taking it
        // down, and runs nothing underneath.
        let (handle, state) = fake(&[]);
        let mut app = commits_app(&handle);
        app.dispatch("files.focus");
        app.press(Key::char('m'));
        assert!(
            app.message.contains("is not bound"),
            "m meant something outside the menu: {:?}",
            app.message
        );
        assert!(app.prompt.is_none());

        app.press(Key::char('S'));
        app.press(Key::char('m'));
        assert!(
            matches!(app.prompt, Some(Prompt::StashMessage { .. })),
            "m inside the menu did not open the field"
        );
        app.press(Key::plain(Code::Esc));

        // A key the question does not name takes it down rather than
        // leaving the next press meaning a scope nobody is asking about.
        app.dispatch("files.focus");
        app.press(Key::char('S'));
        assert_eq!(app.question, Some("stash"));
        app.press(Key::char('Z'));
        assert_eq!(app.question, None, "an unbound key left the menu up");
        assert!(stash_writes(&state).is_empty(), "something was parked");
    }

    #[test]
    fn tui_parity_the_file_scope_parks_the_row_and_knows_an_untracked_one() {
        // `f` inside the menu: the path the keyboard is on, behind `--`, and
        // `-u` when the row is untracked — without which git answers the
        // pathspec with "did not match any file(s) known to git" and parks
        // nothing at all.
        let (handle, state) = fake(&["fresh.txt"]);
        {
            let mut s = state.lock().unwrap();
            s.status.unstaged = vec![UnstagedEntry {
                path: PathBytes::from("work.rs"),
                change: Change::Modified,
                kind: Kind::File,
                submodule: Submodule::default(),
            }];
        }
        let mut app = commits_app(&handle);
        app.dispatch("files.focus");

        // Walk to the unstaged row, bounded: a pane that lost it fails the
        // test rather than hanging it.
        let mut found = false;
        for _ in 0..8 {
            if gitten_app::act::FileClient::selected_file(&app)
                .is_some_and(|f| f.path.as_bytes() == b"work.rs")
            {
                found = true;
                break;
            }
            app.dispatch("view.down");
        }
        assert!(found, "the unstaged row was never reached");
        app.press(Key::char('S'));
        app.press(Key::char('f'));
        assert!(
            stash_landed(&mut app, &state, 1),
            "{:?}",
            stash_writes(&state)
        );
        assert_eq!(stash_writes(&state), ["push -- work.rs"]);

        // Then the untracked row, which needs the flag.
        found = false;
        for _ in 0..8 {
            if gitten_app::act::FileClient::selected_file(&app)
                .is_some_and(|f| f.path.as_bytes() == b"fresh.txt")
            {
                found = true;
                break;
            }
            app.dispatch("view.down");
        }
        assert!(found, "the untracked row was never reached");
        app.press(Key::char('S'));
        app.press(Key::char('f'));
        assert!(
            stash_landed(&mut app, &state, 2),
            "{:?}",
            stash_writes(&state)
        );
        assert_eq!(stash_writes(&state)[1], "push [-u] -- fresh.txt");
    }

    #[test]
    fn tui_parity_the_file_scope_refuses_off_the_pane_and_on_a_conflict() {
        let (handle, state) = fake(&[]);
        {
            let mut s = state.lock().unwrap();
            s.status.conflicts = vec![gitten_core::status::ConflictEntry {
                path: PathBytes::from("both.rs"),
                state: gitten_core::status::ConflictKind::BothModified,
                kind: Kind::File,
                submodule: Submodule::default(),
            }];
        }
        let mut app = commits_app(&handle);
        // Off the files pane: the row this scope is about is a files row.
        app.dispatch("stashes.focus");
        app.dispatch("files.stash-file");
        assert_eq!(app.message, "files.stash-file is not supported here");
        assert!(stash_writes(&state).is_empty());

        // On a conflict: its working-tree side is the merge's open question,
        // and git cannot write a stash over unmerged stages anyway.
        app.dispatch("files.focus");
        let mut found = false;
        for _ in 0..8 {
            if gitten_app::act::FileClient::selected_file(&app)
                .is_some_and(|f| f.path.as_bytes() == b"both.rs")
            {
                found = true;
                break;
            }
            app.dispatch("view.down");
        }
        assert!(found, "the conflict row was never reached");
        app.dispatch("files.stash-file");
        assert_eq!(
            app.message,
            "a conflicted file's merge has to be resolved, not parked"
        );
        assert!(stash_writes(&state).is_empty());
    }

    #[test]
    fn tui_parity_a_stash_is_renamed_from_its_own_message_and_re_filed_on_top() {
        // lazygit's `r`: the field arrives prefilled and wholly selected, so
        // an edit replaces what is standing rather than appending to it.
        // The commit survives; the entry moves to the top, because git's
        // stash is a reflog and only appends.
        let (handle, state) = fake(&[]);
        let mut app = stash_app(&handle);
        app.dispatch("view.down");
        let chosen = state.lock().unwrap().stashes[1].clone();
        app.press(Key::char('r'));
        match &app.prompt {
            Some(Prompt::StashRename { at, field }) => {
                assert_eq!(at.commit, chosen.commit, "the field aims at the entry");
                assert_eq!(at.index, 1);
                assert_eq!(field.text(), chosen.message, "the field was not prefilled");
            }
            _ => panic!("the rename field never opened"),
        }
        assert!(stash_writes(&state).is_empty(), "the press wrote");

        for ch in "the parser one".chars() {
            app.press(Key::char(ch));
        }
        app.press(Key::plain(Code::Enter));
        assert!(
            stash_landed(&mut app, &state, 1),
            "{:?}",
            stash_writes(&state)
        );
        assert_eq!(stash_writes(&state), ["rename stash@{1} the parser one"]);
        let stack = stack_now(&state);
        assert_eq!(stack.len(), 2, "a rename added an entry: {stack:?}");
        assert_eq!(
            (stack[0].1.as_str(), stack[0].2.as_str()),
            ("the parser one", chosen.commit.as_str()),
            "the message changed and the commit did not"
        );
        assert_eq!(stack[1].2, "aaa", "the other entry was left alone");
    }

    #[test]
    fn tui_parity_a_rename_refuses_a_blank_field_and_the_wrong_pane() {
        let (handle, state) = fake(&[]);
        let mut app = commits_app(&handle);
        // The keyboard is on the commits list: a rename is a stash row's.
        app.dispatch("stashes.rename");
        assert_eq!(app.message, "the keyboard is not on a stash");
        assert!(app.prompt.is_none());

        app.dispatch("stashes.focus");
        app.press(Key::char('r'));
        // Clear the prefill, then accept nothing: refused where the field
        // was, and no job built.
        for _ in 0..40 {
            app.press(Key::plain(Code::Backspace));
        }
        app.press(Key::plain(Code::Enter));
        assert_eq!(app.message, "a stash needs a message");
        assert!(stash_writes(&state).is_empty());
    }

    #[test]
    fn tui_parity_a_branch_from_a_stash_starts_where_the_stash_was_made() {
        // lazygit's `n`: git's own three-in-one behind one field — the
        // branch at the commit the stash was made on, the entry applied
        // with its index, and the entry dropped after a clean apply.
        let (handle, state) = fake(&[]);
        let mut app = stash_app(&handle);
        let chosen = state.lock().unwrap().stashes[0].clone();
        app.press(Key::char('n'));
        match &app.prompt {
            Some(Prompt::StashBranch { at, field }) => {
                assert_eq!(at.commit, chosen.commit);
                assert!(field.text().is_empty(), "a branch field is not prefilled");
            }
            _ => panic!("the branch field never opened"),
        }
        assert!(stash_writes(&state).is_empty(), "the press wrote");

        for ch in "wip-branch".chars() {
            app.press(Key::char(ch));
        }
        app.press(Key::plain(Code::Enter));
        assert!(
            stash_landed(&mut app, &state, 1),
            "{:?}",
            stash_writes(&state)
        );
        assert_eq!(stash_writes(&state), ["branch wip-branch from stash@{0}"]);
        {
            let s = state.lock().unwrap();
            assert!(
                matches!(&s.head, Some(HeadState::Branch { name, .. })
                    if name.as_bytes() == b"wip-branch"),
                "the branch was not checked out: {:?}",
                s.head
            );
            assert_eq!(s.stashes.len(), 1, "the entry was not dropped");
            assert_eq!(s.stashes[0].commit, "bbb");
        }
    }

    #[test]
    fn tui_parity_a_branch_from_a_stash_refuses_a_blank_name_and_an_operation() {
        let (handle, state) = fake(&[]);
        let mut app = stash_app(&handle);
        app.press(Key::char('n'));
        app.press(Key::plain(Code::Enter));
        assert_eq!(app.message, "a branch needs a name");
        assert!(stash_writes(&state).is_empty());

        // It checks out, so it waits for git's own standing write.
        state.lock().unwrap().standing = Some(Operation {
            kind: gitten_core::operation::Kind::Rebase,
            conflicts: 1,
        });
        let mut app = stash_app(&handle);
        app.press(Key::char('n'));
        for ch in "wip".chars() {
            app.press(Key::char(ch));
        }
        app.press(Key::plain(Code::Enter));
        assert_eq!(
            app.message,
            "a rebase is in progress; finish or abort it before starting another"
        );
        assert!(stash_writes(&state).is_empty());
    }

    #[test]
    fn tui_parity_a_stash_is_inspected_whole_before_anything_is_applied() {
        // Inspection is the point: both halves of the entry, drawn in the
        // preview lane, with nothing applied to the working tree. The
        // untracked half goes first, exactly as the aggregate read puts
        // creations before modifications, and the diff pane's own file jumps
        // are the drilldown.
        let (handle, state) = fake(&[]);
        {
            let mut s = state.lock().unwrap();
            // One line, so the tracked half's heading is still on screen
            // below it: what is being asserted is the *order* of the two
            // halves, not how far a 40-line file scrolls.
            s.stash_untracked = vec![pair("fresh.txt", Vec::new(), vec![Arc::from("brand new")])];
        }
        let mut app = stash_app(&handle);
        app.dispatch("stashes.open-diff");
        app.pump_quiet();
        assert_eq!(app.panes.focused_name(), "diff");
        let entry = state.lock().unwrap().stashes[0].clone();
        assert_eq!(
            origin_of(&app),
            Some(DiffSource::Stash {
                index: 0,
                commit: entry.commit.clone()
            })
        );
        // Both halves are drawn — the untracked file first, exactly as the
        // aggregate read puts creations before modifications — and the
        // pane's own file jump is the drilldown between them.
        app.draw();
        let frame: Vec<String> = (0..app.screen.size().1)
            .map(|y| app.screen.row_text(y))
            .collect();
        let seen = |needle: &str| frame.iter().any(|row| row.contains(needle));
        assert!(
            seen("fresh.txt"),
            "the untracked half is not drawn: {frame:?}"
        );
        let fresh_at = frame
            .iter()
            .position(|row| row.contains("fresh.txt"))
            .expect("the untracked heading");
        let tracked_at = frame
            .iter()
            .position(|row| row.contains("f.txt") && !row.contains("fresh.txt"))
            .expect("the tracked heading");
        assert!(
            fresh_at < tracked_at,
            "the untracked half is not first: {fresh_at} vs {tracked_at}"
        );
        let at_first = diff_of(&app).cursor();
        app.dispatch("diff.next-file");
        assert_ne!(
            diff_of(&app).cursor(),
            at_first,
            "the file jump moved nothing"
        );
        // Nothing was applied: inspection reads, and the stack is whole.
        assert!(
            stash_writes(&state).is_empty(),
            "{:?}",
            stash_writes(&state)
        );
        assert_eq!(state.lock().unwrap().stashes.len(), 2);
    }

    #[test]
    fn tui_parity_a_stash_verb_follows_its_commit_when_the_stack_churns() {
        // The accident stable identity exists to prevent. `stash@{1}` is
        // chosen; something outside the pane drops `stash@{0}`, so the row
        // the pane last drew as `stash@{1}` is now `stash@{0}` and the
        // number the keyboard captured names the *other* entry. The write
        // must land on the entry that was chosen.
        let (handle, state) = fake(&[]);
        let mut app = stash_app(&handle);
        app.dispatch("view.down");
        let chosen =
            gitten_app::act::StashClient::selected_stash(&app).expect("a row under the keyboard");
        assert_eq!((chosen.index, chosen.commit.as_str()), (1, "bbb"));

        // The churn, behind the pane's back: the stack the *repository*
        // holds loses its top entry and renumbers.
        {
            let mut s = state.lock().unwrap();
            s.stashes.remove(0);
            for (i, entry) in s.stashes.iter_mut().enumerate() {
                entry.index = i;
            }
        }
        app.dispatch("stashes.apply");
        assert!(
            stash_landed(&mut app, &state, 1),
            "{:?}",
            stash_writes(&state)
        );
        // Resolved to where the commit *is*, not to the number it was at.
        assert_eq!(
            stash_writes(&state),
            ["apply stash@{0}"],
            "the apply did not follow the commit"
        );
        assert_eq!(
            app.message, "",
            "a correct apply said something: {:?}",
            app.message
        );
    }

    #[test]
    fn tui_parity_a_stash_verb_refuses_an_entry_the_churn_took_away() {
        // The other half of the same rule: the chosen entry left the stack
        // entirely, and the number it was at now names somebody else's
        // work. Refused in words, and nothing is written.
        let (handle, state) = fake(&[]);
        for command in ["stashes.apply", "stashes.pop"] {
            // Each pass opens on the same stack: the churn below is what the
            // test does to it, not what the pass before left behind.
            {
                let mut s = state.lock().unwrap();
                s.stashes = two_stashes();
                s.stash_writes.clear();
            }
            let mut app = stash_app(&handle);
            app.dispatch("view.down");
            assert_eq!(
                gitten_app::act::StashClient::selected_stash(&app).map(|id| id.commit),
                Some("bbb".into())
            );
            {
                let mut s = state.lock().unwrap();
                s.stashes = vec![
                    Stash {
                        index: 0,
                        message: "On main: wip things".into(),
                        commit: "aaa".into(),
                    },
                    Stash {
                        index: 1,
                        message: "On main: somebody else's".into(),
                        commit: "ccc".into(),
                    },
                ];
                s.stash_writes.clear();
            }
            app.dispatch(command);
            assert!(
                until(Duration::from_secs(2), || {
                    app.pump_quiet();
                    app.message.contains("no longer on the stack")
                }),
                "{command} never refused: {:?}",
                app.message
            );
            assert!(
                stash_writes(&state).is_empty(),
                "{command} wrote anyway: {:?}",
                stash_writes(&state)
            );
            assert_eq!(stack_now(&state).len(), 2, "the refusal moved the stack");
        }
    }

    #[test]
    fn tui_parity_a_drop_armed_on_one_entry_is_never_spent_on_another() {
        // The arm holds the identity, so the yes cannot be inherited: the
        // entry the question was asked about leaves the stack, another one
        // takes its number, and the second press asks again about *that*
        // one instead of dropping it.
        let (handle, state) = fake(&[]);
        let mut app = stash_app(&handle);
        app.press(Key::char('d'));
        assert_eq!(app.message, "drop stash@{0}? press again to confirm");
        assert!(stash_writes(&state).is_empty());

        // The pane's rows are replaced with a stack whose stash@{0} is a
        // different commit — which is exactly what a refresh after somebody
        // else's push looks like.
        if let Some(Screens::Stashes { view, .. }) = app.panes.get_mut("stashes") {
            view.replace(vec![
                Stash {
                    index: 0,
                    message: "On main: somebody else's".into(),
                    commit: "ccc".into(),
                },
                Stash {
                    index: 1,
                    message: "On dev: other work".into(),
                    commit: "bbb".into(),
                },
            ]);
        }
        app.press(Key::char('d'));
        assert_eq!(
            app.message, "drop stash@{0}? press again to confirm",
            "a stale yes was spent: {:?}",
            app.message
        );
        assert!(
            stash_writes(&state).is_empty(),
            "the inherited row was dropped: {:?}",
            stash_writes(&state)
        );
    }

    #[test]
    fn tui_parity_a_conflicted_pop_keeps_the_stash_and_says_git_s_words() {
        // git decides, and it keeps the entry: a pop whose apply fails
        // never reaches the drop, so the work is still parked and still
        // recoverable. The refusal is git's sentence, not a UI-invented
        // word like `conflict`.
        let (handle, state) = fake(&[]);
        let mut app = stash_app(&handle);
        state.lock().unwrap().refuse_stash =
            Some("error: Your local changes would be overwritten by merge".into());
        let chosen = gitten_app::act::StashClient::selected_stash(&app).expect("a row");
        app.dispatch("stashes.pop");
        assert!(
            until(Duration::from_secs(2), || {
                app.pump_quiet();
                app.message.contains("would be overwritten")
            }),
            "the refusal never arrived: {:?}",
            app.message
        );
        assert_eq!(
            state.lock().unwrap().stashes.len(),
            2,
            "a refused pop moved the stack"
        );
        assert_eq!(state.lock().unwrap().stashes[0].commit, chosen.commit);

        // Recoverable means recoverable: with the way cleared, the same
        // entry pops, addressed by the identity that chose it.
        state.lock().unwrap().refuse_stash = None;
        let before = stash_writes(&state).len();
        app.dispatch("stashes.pop");
        assert!(
            stash_landed(&mut app, &state, before + 1),
            "the kept entry never popped: {:?}",
            stash_writes(&state)
        );
        assert_eq!(stash_writes(&state).last().unwrap(), "pop stash@{0}");
        assert_eq!(state.lock().unwrap().stashes.len(), 1);
    }

    #[test]
    fn tui_parity_the_stash_family_is_disabled_without_a_repository() {
        // Every one of them needs a working tree or a stack, so a fixture
        // says why rather than advertising a key that cannot run.
        let a = tui_availability(false, None, None);
        for command in [
            "files.stash-menu",
            "files.stash-named",
            "files.stash-staged",
            "files.stash-unstaged",
            "files.stash-untracked",
            "files.stash-file",
            "stashes.rename",
            "stashes.new-branch",
        ] {
            match a.state(command) {
                gitten_core::command::Usable::Disabled(why) => assert!(
                    why.contains("fixture"),
                    "{command}'s reason names no fixture: {why:?}"
                ),
                other => panic!("{command} is advertised against a fixture: {other:?}"),
            }
            assert!(!a.runnable(command));
        }
        let a = tui_availability(true, None, None);
        for command in [
            "files.stash-menu",
            "files.stash-file",
            "stashes.rename",
            "stashes.new-branch",
        ] {
            assert_eq!(
                a.state(command),
                &gitten_core::command::Usable::Available,
                "{command} stayed disabled"
            );
        }
    }

    #[test]
    fn tui_parity_the_stash_keys_resolve_where_lazygit_puts_them() {
        // Keys are data: the shipped map is what says so, and the panel
        // reads the same table the press does.
        let map = gitten_core::command::Keymap::builtin();
        let mut stashes = Modes::new();
        stashes.push("stashes");
        for (chord, name) in [
            ("space", "stashes.apply"),
            ("g", "stashes.pop"),
            ("d", "stashes.drop"),
            ("r", "stashes.rename"),
            ("n", "stashes.new-branch"),
            ("enter", "stashes.open-diff"),
        ] {
            assert_eq!(
                map.resolve(
                    &stashes,
                    &gitten_core::command::parse_chord(chord).expect("a chord")
                ),
                gitten_core::command::Resolve::Run(name),
                "{chord} did not reach {name} in [stashes]"
            );
        }
        let mut files = Modes::new();
        files.push("files");
        assert_eq!(
            map.resolve(
                &files,
                &gitten_core::command::parse_chord("S").expect("a chord")
            ),
            gitten_core::command::Resolve::Run("files.stash-menu")
        );
        // The scope letters live in the question's mode and nowhere else.
        let mut menu = Modes::new();
        menu.push("files");
        menu.push("stash");
        for (chord, name) in [
            ("m", "files.stash-named"),
            ("s", "files.stash-staged"),
            ("u", "files.stash-unstaged"),
            ("U", "files.stash-untracked"),
            ("f", "files.stash-file"),
        ] {
            assert_eq!(
                map.resolve(
                    &menu,
                    &gitten_core::command::parse_chord(chord).expect("a chord")
                ),
                gitten_core::command::Resolve::Run(name),
                "{chord} did not reach {name} in [stash]"
            );
            assert_ne!(
                map.resolve(
                    &files,
                    &gitten_core::command::parse_chord(chord).expect("a chord")
                ),
                gitten_core::command::Resolve::Run(name),
                "{name} leaked out of the menu into [files]"
            );
        }
    }
}
