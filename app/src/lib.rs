//! Everything a client needs before it can draw, and nothing that draws.
//!
//! plait is a `core` and a set of clients. A client is a window, a browser tab,
//! a terminal — or one somebody else writes, which is the point. This crate is
//! what stops "write your own client" meaning "reimplement the startup".
//!
//! ```text
//!   plait-core   the pipeline. no dependencies, no I/O, no idea a UI exists
//!   plait-git    acquisition. the only crate that talks to a repository
//!   plait-app    the config file, the command line, and loading  ← you are here
//!   ─────────────────────────────────────────────────────────────────────────
//!   a client     drawing, and input
//! ```
//!
//! Before it existed, a client had to write its own argument parsing, its own
//! `--fixtures` arm, its own error strings — and it could not read `plait.toml`
//! **at all**, because the parser lived behind GPUI. So the window was the only
//! client that could be configured, which is not a property a config format
//! should have.
//!
//! # What a client is now
//!
//! ```no_run
//! use plait_app::{acquire, cli, Startup};
//!
//! // Arguments, `plait.toml`, `--help`, `config`, acquisition — all of it.
//! let start = match Startup::new("plait-tui", cli::View::Diff)
//!     .blurb("plait in the terminal you started it from")
//!     .go()
//! {
//!     Ok(started) => started,
//!     Err(exit) => exit.finish(),   // prints, and does not come back
//! };
//!
//! match start.loaded.data {
//!     acquire::Data::Diff(files) => { /* draw */ }
//!     acquire::Data::Commits(commits) => { /* draw */ }
//! }
//! ```
//!
//! Everything above drawing has happened: the same flags, the same
//! `plait.toml`, the same differ, the same error messages as every other
//! client.
//!
//! # What is deliberately *not* here
//!
//! **How a reload reaches the views.** [`config::watch`] is shared, because
//! watching a file is I/O; what to do when it fires is not, because the GPUI
//! client swaps a global and a terminal one drops a flag into its event loop.
//!
//! **Anything about rows or cells.** That is `core`. If something in here starts
//! knowing what a row looks like, it has moved to the wrong crate.

pub mod acquire;
pub mod cli;
pub mod config;
pub mod jobs;

use cli::{Request, Source, View};
use plait_core::host::Host;
use std::path::Path;
use std::time::Instant;

/// How wide a row may get before it is clipped.
///
/// A rendering budget, so a client may override it — but three clients picked
/// the same 2000 independently, which makes it a default rather than a
/// coincidence. Text layout is linear in length, a 9.6-million-character line
/// was measured in the wild, and nobody reads past column 2000.
pub const MAX_LINE_CHARS: usize = 2000;

/// Narrowest wrap budget worth having.
///
/// Narrower than its own gutter, a presentation would ask for one character a
/// row — a diff turned into a column of letters, and a row count that grows
/// without bound. Overflowing the edge is the better failure.
pub const MIN_WRAP_COLS: usize = 8;

/// Where a startup's repository handles come from.
///
/// One method, because that is all a factory is: a path in, a [`Handle`] out.
/// The seam exists so a client — or a test — can put its own implementation of
/// acquisition behind the same shared startup instead of hardcoding this
/// crate's; everything downstream keeps holding `Arc<dyn Repo>` and cannot
/// tell which factory ran. Object-safe for the same reason [`Repo`](plait_git::Repo)
/// is: the value crosses threads and outlives the call.
pub trait Opener: Send + Sync {
    /// Opens (or claims) the repository at `root`.
    ///
    /// Infallible like the default it may replace: opening runs nothing, and
    /// a path that is not a repository fails at the first read.
    fn open(&self, root: &Path) -> plait_git::Handle;
}

/// The shipped opener: `plait-git`'s binary-backed implementation.
///
/// What every client gets when it says nothing about the matter.
pub struct GitOpener;

impl Opener for GitOpener {
    fn open(&self, root: &Path) -> plait_git::Handle {
        plait_git::open(root)
    }
}

/// The shared startup: arguments, config, acquisition.
///
/// A builder rather than a function of six arguments, because what differs
/// between clients is small and optional — the name it prints, the view it opens
/// on with nothing typed, the flags it documents — and a client that wants none
/// of that should say none of it.
pub struct Startup {
    binary: &'static str,
    default: View,
    blurb: String,
    extra: String,
    args: Vec<String>,
    /// Where repository handles come from. The binary opener unless a client
    /// replaces it; see [`Opener`].
    opener: std::sync::Arc<dyn Opener>,
}

/// Everything the startup produced.
pub struct Started {
    pub view: View,
    pub source: Source,
    /// The configured host: the file has been read and its warnings printed.
    pub host: Host,
    pub loaded: acquire::Loaded,
    /// Where the config file was found, for a client that wants to watch it.
    pub config: std::path::PathBuf,
    /// One handle for every read this client makes, opened once and held for
    /// the process.
    ///
    /// Persistent on purpose: a re-acquire — a different algorithm from a
    /// control, a commit's diff opened off the graph — goes through this same
    /// handle rather than through a fresh one per call, so whatever an
    /// implementation amortises across calls survives the call. `None` for
    /// `--fixtures`, which has no repository behind it; a client hands it back
    /// to [`acquire::acquire`] unchanged, and tests hand in their own.
    pub repo: Option<plait_git::Handle>,
}

impl Started {
    /// What a client puts in a title bar: `plait · diff · plait main HEAD~2..HEAD`.
    ///
    /// It names the debug build, because a timing read off one is meaningless
    /// and the shell's overlay has said so since it existed.
    pub fn title(&self, binary: &str) -> String {
        let build = match cfg!(debug_assertions) {
            true => "  ·  DEBUG BUILD — timings meaningless, use --release",
            false => "",
        };
        format!(
            "{binary} · {} · {}{build}",
            self.view.name(),
            self.loaded.label
        )
    }

    /// A key naming this exact view of this exact source, for a saved position.
    pub fn session_key(&self) -> String {
        self.source.key(self.view)
    }
}

/// Why a startup did not produce a [`Started`].
///
/// Two of the three are *successes* — `--help` and `config` both did what was
/// asked and there is nothing to draw — which is why this is not an error type.
/// A client matches on it rather than printing it.
pub enum Exit {
    /// The usage text, already formatted. Print it and leave.
    Help(String),
    /// The config file as TOML. Print it and leave.
    Config(String),
    /// Something went wrong, with the usage text after it.
    Failed(String),
}

impl Exit {
    /// Prints to the right stream and exits with the right status: usage and
    /// config on stdout so they can be redirected, failures on stderr.
    pub fn finish(self) -> ! {
        match self {
            Exit::Help(text) | Exit::Config(text) => {
                print!("{text}");
                std::process::exit(0)
            }
            Exit::Failed(text) => {
                eprint!("{text}");
                std::process::exit(1)
            }
        }
    }
}

/// Stage timing for the road to the first frame.
///
/// [`Startup::go`] reports the stages every client shares — arguments parsed,
/// host built, config loaded, data acquired — and a client marks the ones only
/// it has with the same clock: the terminal takes its tty, the window its
/// surface. One prefix, `plait-start:`, so a run's whole breakdown is one grep
/// however many crates printed it.
///
/// Off unless `PLAIT_START_LOG` says otherwise (`0` off, the same rule
/// `PLAIT_STATS` follows). Nothing costs anything then: one `getenv` at
/// construction and a `None` check per stage afterwards — no timer, no
/// allocation.
pub struct StartClock {
    /// When the current stage began. Reset by every report, so the stages
    /// chain: each one is timed against the previous, not against process
    /// start, and the numbers sum to the whole road to the first frame.
    at: Option<Instant>,
}

impl StartClock {
    pub fn new() -> Self {
        Self {
            at: std::env::var_os("PLAIT_START_LOG")
                .is_some_and(|v| v != "0")
                .then(Instant::now),
        }
    }

    /// Ends one stage and says how long it took. A no-op once disarmed.
    pub fn stage(&mut self, what: &str) {
        if let Some(at) = self.at.as_mut() {
            let took = at.elapsed();
            *at = Instant::now();
            eprintln!("plait-start: {what} in {took:.0?}");
        }
    }
}

impl Default for StartClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Startup {
    /// `binary` is what usage and errors call this client; `default` is the view
    /// it opens on when nothing is typed.
    pub fn new(binary: &'static str, default: View) -> Self {
        Self {
            binary,
            default,
            blurb: "a git client".into(),
            extra: String::new(),
            args: std::env::args().skip(1).collect(),
            opener: std::sync::Arc::new(GitOpener),
        }
    }

    /// The one line under the name in the usage text.
    pub fn blurb(mut self, blurb: impl Into<String>) -> Self {
        self.blurb = blurb.into();
        self
    }

    /// Lines documenting whatever only this client has — a `--port`, a title
    /// bar, an overlay.
    pub fn extra(mut self, extra: impl Into<String>) -> Self {
        self.extra = extra.into();
        self
    }

    /// Arguments other than the process's own. For tests, and for a client that
    /// took its own flags out first.
    pub fn args(mut self, args: Vec<String>) -> Self {
        self.args = args;
        self
    }

    /// Opens repositories with `opener` instead of [`GitOpener`].
    ///
    /// The one seam a client needs to put its own acquisition behind the
    /// shared startup — a wrapper around another binary, an embedded
    /// implementation, a test's fake — without any of the rest of this crate
    /// changing hands.
    pub fn opener(mut self, opener: std::sync::Arc<dyn Opener>) -> Self {
        self.opener = opener;
        self
    }

    /// The arguments as they stand, so a client can take its own flags out
    /// before the shared parse sees them.
    pub fn take(&mut self) -> &mut Vec<String> {
        &mut self.args
    }

    pub fn usage(&self) -> String {
        cli::usage(self.binary, &self.blurb, &self.extra)
    }

    /// Parses, configures, acquires.
    ///
    /// Warnings from the config file go to stderr as they are found — a colour
    /// that did not parse is worth saying at once, and is never worth refusing
    /// to start over.
    pub fn go(self) -> Result<Started, Exit> {
        let mut clock = StartClock::new();
        let request = cli::parse(&self.args, self.default);
        clock.stage("args parsed");
        if let Request::Help = request {
            return Err(Exit::Help(self.usage()));
        }

        // The file is read for `config` too — `plait config` prints the file you
        // have *plus* every default, which is what makes it a starting point
        // rather than a dump of the built-ins.
        let path = config::path();
        let mut host = Host::new();
        clock.stage("host built");
        for w in config::load(&mut host, &path) {
            eprintln!("{}: {w}", self.binary);
        }
        clock.stage("config loaded");

        let (view, source) = match request {
            Request::Config => return Err(Exit::Config(config::dump(&host))),
            Request::Open { view, source } => (view, source),
            Request::Help => unreachable!("returned above"),
        };

        // One handle for every read this client makes, from whichever opener
        // was injected. Opening runs nothing, so a directory that is not a
        // repository fails at the first read — in the same words it always
        // failed there — and nothing is put on the road to the first frame to
        // learn that early. Help and config returned above: neither opens a
        // repository, so neither invokes the factory.
        let repo = match &source {
            Source::Repo { path, .. } => Some(self.opener.open(path)),
            Source::Fixtures => None,
        };

        match acquire::acquire(view, &source, &host, repo.as_deref()) {
            Ok(loaded) => {
                clock.stage("acquired");
                Ok(Started {
                    view,
                    source,
                    host,
                    loaded,
                    config: path,
                    repo,
                })
            }
            Err(e) => Err(Exit::Failed(format!(
                "{}: {e}\n\n{}",
                self.binary,
                self.usage()
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plait_core::status::Status;
    use plait_git::{Pair, Repo};
    use std::sync::Arc;

    fn start(line: &str) -> Result<Started, Exit> {
        Startup::new("plait-test", View::Commits)
            .args(line.split_whitespace().map(String::from).collect())
            .go()
    }

    /// A repository that exists only as this struct: every read answers from
    /// it, and its path need not exist at all.
    struct Fake;

    impl Repo for Fake {
        fn log(&self, limit: usize) -> plait_git::Result<Vec<plait_core::Commit>> {
            Ok((0..limit.min(1))
                .map(|_| plait_core::Commit {
                    sha: "f0".into(),
                    short: "f0".into(),
                    parents: Box::from(&[][..]),
                    author: "fake".into(),
                    timestamp: 0,
                    subject: "from the fake".into(),
                })
                .collect())
        }

        fn pairs(&self, _revspec: &str) -> plait_git::Result<Vec<Pair>> {
            Ok(vec![Pair {
                path: "fake.txt".into(),
                old_path: None,
                status: 'A',
                old: Vec::new(),
                new: vec!["fake contents".into()],
                binary: false,
            }])
        }

        fn status(&self) -> plait_git::Result<Status> {
            Ok(Status::default())
        }

        fn describe(&self) -> String {
            "fake (main)".into()
        }
    }

    /// An opener with exactly one answer, handed out by clone so a test can
    /// prove identity against whatever came back on `Started`.
    struct OneFake(plait_git::Handle);

    impl Opener for OneFake {
        fn open(&self, _root: &Path) -> plait_git::Handle {
            Arc::clone(&self.0)
        }
    }

    /// An opener that must never be reached.
    struct Exploding;

    impl Opener for Exploding {
        fn open(&self, _root: &Path) -> plait_git::Handle {
            panic!("nothing before acquisition should open a repository")
        }
    }

    #[test]
    fn help_is_a_success_with_nothing_to_draw() {
        let Err(Exit::Help(text)) = start("--help") else {
            panic!("--help did not produce usage");
        };
        assert!(text.contains("plait-test commits"));
    }

    #[test]
    fn config_prints_a_file_that_reads_back() {
        let Err(Exit::Config(text)) = start("config") else {
            panic!("config did not produce a file");
        };
        // The round trip `config.rs` guarantees, exercised from the outside.
        let mut host = Host::new();
        let warn = config::apply(&mut host, &text);
        assert!(warn.is_empty(), "{warn:?}");
    }

    #[test]
    fn a_failure_names_the_binary_and_shows_the_usage() {
        let Err(Exit::Failed(text)) = start("commits /nonexistent") else {
            panic!("a missing repository started anyway");
        };
        assert!(text.starts_with("plait-test: "), "{text}");
        assert!(
            text.contains("plait-test commits"),
            "the usage was not shown"
        );
    }

    #[test]
    fn a_real_start_produces_everything_a_client_needs() {
        let started =
            start("diff . HEAD~1..HEAD").unwrap_or_else(|_| panic!("this repo has history"));
        assert_eq!(started.view, View::Diff);
        assert!(!started.loaded.data.is_empty());
        assert!(started.title("plait-test").contains("plait-test · diff · "));
        assert!(started.session_key().starts_with("diff:"));
        // The host is the configured one, not `Host::new()` handed back.
        assert!(!started.host.differ.selected().is_empty());
        // The handle survives the startup, for every read after this one.
        assert!(
            started.repo.is_some(),
            "a repository source keeps its handle"
        );
    }

    #[test]
    fn a_client_takes_its_own_flags_before_the_shared_parse_sees_them() {
        // A revspec and not the bare working tree: `diff .` is "no changes" on
        // whatever clean checkout runs the tests, and this wants an acquisition
        // that succeeds everywhere the repo has history at all.
        let mut s = Startup::new("plait-test", View::Diff).args(
            "diff --port 9000 . HEAD~1..HEAD"
                .split_whitespace()
                .map(String::from)
                .collect(),
        );
        let port = cli::take_value(s.take(), "--port").unwrap();
        assert_eq!(port.as_deref(), Some("9000"));
        let started = s.go().unwrap_or_else(|_| panic!("start"));
        assert_eq!(started.view, View::Diff);
    }

    #[test]
    fn an_injected_opener_opens_and_whatever_it_opens_is_retained() {
        // `/nonexistent` cannot be read by anything. If startup succeeds, the
        // acquisition went through the fake — and if the handle it kept is
        // *this* Arc, every later read a client makes goes through it too.
        let fake: plait_git::Handle = Arc::new(Fake);
        let started = Startup::new("plait-test", View::Commits)
            .args(vec!["commits".into(), "/nonexistent".into()])
            .opener(Arc::new(OneFake(Arc::clone(&fake))))
            .go()
            .unwrap_or_else(|_| panic!("the fake has history"));
        assert_eq!(started.loaded.label, "fake (main)", "acquired from it");
        match &started.loaded.data {
            acquire::Data::Commits(commits) => {
                assert_eq!(commits[0].subject, "from the fake");
            }
            other => panic!("a commits view loads commits, got {other:?}"),
        }
        assert!(
            Arc::ptr_eq(started.repo.as_ref().expect("retained"), &fake),
            "the same handle comes back on Started"
        );
    }

    #[test]
    fn help_and_config_never_touch_the_opener() {
        for line in ["--help", "config"] {
            let result = Startup::new("plait-test", View::Commits)
                .args(vec![line.into()])
                .opener(Arc::new(Exploding))
                .go();
            assert!(
                !matches!(result, Err(Exit::Failed(_))),
                "{line} must not fail, and must not have opened anything"
            );
            assert!(
                matches!(result, Err(Exit::Help(_)) | Err(Exit::Config(_))),
                "{line} is a success with nothing to draw"
            );
        }
    }
}
