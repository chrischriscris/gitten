//! Turning what the command line said into data a client can draw.
//!
//! One function, and it is the whole of what sits between
//! [`cli::parse`](crate::cli::parse) and a view. It uses the host's own
//! `Differs`, which is the point: *which algorithm ran* is a configured choice
//! and this is the one place it is made, so `[diff] algorithm` in `plait.toml`
//! means the same thing in a window, a browser and a terminal.
//!
//! It returns **`Vec<FileDiff>`, not prepared rows.** Every client wants
//! something different one stage later — the shell keeps the parsed diff so a
//! layout change can rebuild, the browser prepares immediately and holds a
//! window of rows, the terminal does both — and `prepare` is one call away in
//! `core`. Stopping here is what keeps this from being a fourth opinion about
//! what a client needs.

use crate::cli::{Source, View};
use plait_core::differ::Overrides;
use plait_core::host::Host;
use plait_core::{Commit, FileDiff};
use std::path::Path;

/// Where the fixtures live, for a client that wants to say so in an error.
pub const DIFF_FIXTURE: &str = "fixtures/big.diff";
pub const LOG_FIXTURE: &str = "fixtures/log.txt";

/// What was loaded, and what to call it in a title bar or a status line.
#[derive(Debug)]
pub struct Loaded {
    pub label: String,
    pub data: Data,
}

#[derive(Debug)]
pub enum Data {
    Commits(Vec<Commit>),
    Diff(Vec<FileDiff>),
}

impl Data {
    /// How many rows the view will be given, for a load message. Not the number
    /// of rows on screen — wrapping and the presentation both change that.
    pub fn len(&self) -> usize {
        match self {
            Data::Commits(c) => c.len(),
            Data::Diff(f) => f.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Reads a fixture, never failing over its contents.
///
/// Git guarantees no encoding and real history carries Latin-1 author names;
/// `git/git` panics an implementation that insists on UTF-8. Lossy, always, and
/// a missing file is empty rather than an error so the caller can say something
/// better than `No such file`.
pub fn read_fixture(path: &str) -> String {
    String::from_utf8_lossy(&std::fs::read(path).unwrap_or_default()).into_owned()
}

/// Acquires the data for one view of one source.
///
/// Errors are strings a client prints beside its usage, because every one of
/// them is something the person typing can fix: a path that is not a
/// repository, a revspec with nothing in it, a fixture that has not been
/// generated.
pub fn acquire(view: View, source: &Source, host: &Host) -> Result<Loaded, String> {
    match (view, source) {
        (View::Diff, Source::Repo { path, arg }) => {
            let files = plait_git::diff(path, arg, &host.differ, &Overrides::default())?;
            if files.is_empty() {
                let what = match arg.is_empty() {
                    true => "(working tree)",
                    false => arg.as_str(),
                };
                return Err(format!("no changes for {} {what}", path.display()));
            }
            // No algorithm in the label: a client has a control that says which
            // one, and that stays true when you change it.
            Ok(Loaded { label: describe(path, arg), data: Data::Diff(files) })
        }
        (View::Diff, Source::Fixtures) => {
            let files = plait_core::parse_unified_diff(&read_fixture(DIFF_FIXTURE));
            if files.is_empty() {
                return Err(format!("{DIFF_FIXTURE} is missing or empty — ./fixtures/gen.sh 1000 1000"));
            }
            Ok(Loaded { label: "fixtures".into(), data: Data::Diff(files) })
        }
        (View::Commits, Source::Repo { path, arg }) => {
            let commits = plait_git::log(path, arg.parse().unwrap_or(5000))?;
            if commits.is_empty() {
                return Err(format!("no commits in {}", path.display()));
            }
            Ok(Loaded { label: plait_git::describe(path), data: Data::Commits(commits) })
        }
        (View::Commits, Source::Fixtures) => {
            let commits = plait_core::parse_log(&read_fixture(LOG_FIXTURE));
            if commits.is_empty() {
                return Err(format!("{LOG_FIXTURE} is missing or empty — ./fixtures/dump.sh ."));
            }
            Ok(Loaded { label: "fixtures".into(), data: Data::Commits(commits) })
        }
    }
}

fn describe(repo: &Path, revspec: &str) -> String {
    format!("{} {revspec}", plait_git::describe(repo)).trim().into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn here() -> Source {
        Source::Repo { path: PathBuf::from("."), arg: "HEAD~1..HEAD".into() }
    }

    #[test]
    fn a_diff_of_this_repository_arrives_as_parsed_files() {
        let host = Host::new();
        let loaded = acquire(View::Diff, &here(), &host).expect("this repo has history");
        assert!(!loaded.data.is_empty());
        assert!(matches!(loaded.data, Data::Diff(_)));
        assert!(!loaded.label.is_empty(), "a title bar has nothing to say");
    }

    #[test]
    fn the_hosts_differ_is_the_one_that_runs() {
        // The whole reason acquisition takes a `Host`: `[diff] algorithm` in
        // `plait.toml` has to reach the thing that actually diffs.
        let mut host = Host::new();
        assert!(host.differ.select("myers"));
        host.differ.context = 1;
        let a = acquire(View::Diff, &here(), &host).unwrap();
        let mut other = Host::new();
        other.differ.context = 12;
        let b = acquire(View::Diff, &here(), &other).unwrap();
        let hunks = |l: &Loaded| match &l.data {
            Data::Diff(f) => f.iter().map(|f| f.hunks.len()).sum::<usize>(),
            _ => 0,
        };
        assert!(hunks(&a) >= hunks(&b), "more context merges hunks; it did not reach the differ");
    }

    #[test]
    fn a_path_that_is_not_a_repository_is_a_message_and_not_a_panic() {
        let host = Host::new();
        let source = Source::Repo { path: PathBuf::from("/nonexistent"), arg: String::new() };
        assert!(acquire(View::Commits, &source, &host).is_err());
        assert!(acquire(View::Diff, &source, &host).is_err());
    }

    #[test]
    fn an_empty_revspec_says_which_revspec_it_meant() {
        let host = Host::new();
        // A revspec that resolves to nothing changed. The message has to name
        // it, or "no changes" is indistinguishable from a broken argument.
        let source = Source::Repo { path: PathBuf::from("."), arg: "HEAD..HEAD".into() };
        let err = acquire(View::Diff, &source, &host).unwrap_err();
        assert!(err.contains("HEAD..HEAD"), "{err}");
    }

    #[test]
    fn a_missing_fixture_says_how_to_make_one() {
        // Only meaningful from a directory without fixtures, so assert on the
        // shape of the message rather than on whether this run has them.
        let host = Host::new();
        if let Err(e) = acquire(View::Diff, &Source::Fixtures, &host) {
            assert!(e.contains("fixtures/"), "{e}");
            assert!(e.contains("./fixtures/"), "the message did not say how: {e}");
        }
    }

    #[test]
    fn a_commit_limit_is_the_second_positional() {
        let host = Host::new();
        let source = Source::Repo { path: PathBuf::from("."), arg: "3".into() };
        let loaded = acquire(View::Commits, &source, &host).unwrap();
        assert_eq!(loaded.data.len(), 3);
    }
}
