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
use plait_git::Repo;
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
/// The repository comes in, injected: every read goes through the caller's
/// [`Repo`] handle and this function never opens one itself. That is what
/// makes it testable without a repository at all — a fake in — and what lets a
/// client keep one handle alive across every acquisition it makes. `None` for
/// a `Source::Repo` is an error rather than an open, because inventing a
/// handle here would quietly defeat the one a client is holding.
///
/// Errors are strings a client prints beside its usage, because every one of
/// them is something the person typing can fix: a path that is not a
/// repository, a revspec with nothing in it, a fixture that has not been
/// generated.
pub fn acquire(
    view: View,
    source: &Source,
    host: &Host,
    repo: Option<&dyn Repo>,
) -> Result<Loaded, String> {
    match (view, source) {
        (View::Diff, Source::Repo { path, arg }) => {
            let repo = repo_else(path, repo)?;
            // The label is one more `git` process and the last thing anyone is
            // waiting for, so it runs *beside* acquisition rather than behind
            // it: one spawn floor (~7ms) off every repository open. `describe`
            // is infallible, so joining it afterwards is all the coordination
            // there is — and a scope thread borrows for exactly as long as
            // this call.
            std::thread::scope(|s| {
                let title = s.spawn(|| describe(repo, arg));
                let files = plait_git::diff(repo, arg, &host.differ, &Overrides::default())?;
                if files.is_empty() {
                    let what = match arg.is_empty() {
                        true => "(working tree)",
                        false => arg.as_str(),
                    };
                    return Err(format!("no changes for {} {what}", path.display()));
                }
                // No algorithm in the label: a client has a control that says which
                // one, and that stays true when you change it.
                Ok(Loaded {
                    label: joined(title),
                    data: Data::Diff(files),
                })
            })
        }
        (View::Diff, Source::Fixtures) => {
            let files = plait_core::parse_unified_diff(&read_fixture(DIFF_FIXTURE));
            if files.is_empty() {
                return Err(format!(
                    "{DIFF_FIXTURE} is missing or empty — ./fixtures/gen.sh 1000 1000"
                ));
            }
            Ok(Loaded {
                label: "fixtures".into(),
                data: Data::Diff(files),
            })
        }
        (View::Commits, Source::Repo { path, arg }) => {
            let repo = repo_else(path, repo)?;
            // Beside, not behind: the same overlap the diff view has, because
            // the graph waits on `git log` either way.
            std::thread::scope(|s| {
                let title = s.spawn(|| repo.describe());
                let commits = repo.log(arg.parse().unwrap_or(5000))?;
                if commits.is_empty() {
                    return Err(format!("no commits in {}", path.display()));
                }
                Ok(Loaded {
                    label: joined(title),
                    data: Data::Commits(commits),
                })
            })
        }
        (View::Commits, Source::Fixtures) => {
            let commits = plait_core::parse_log(&read_fixture(LOG_FIXTURE));
            if commits.is_empty() {
                return Err(format!(
                    "{LOG_FIXTURE} is missing or empty — ./fixtures/dump.sh ."
                ));
            }
            Ok(Loaded {
                label: "fixtures".into(),
                data: Data::Commits(commits),
            })
        }
    }
}

/// Joins the thread fetching the title.
///
/// `describe` returns a `String` and cannot fail, so the only thing left in
/// that `join` is a panic — resumed here, on the caller's thread, exactly as it
/// came out of the inline call this used to be.
fn joined(title: std::thread::ScopedJoinHandle<'_, String>) -> String {
    title
        .join()
        .unwrap_or_else(|p| std::panic::resume_unwind(p))
}

/// The injected handle, or an error naming what was asked for.
///
/// The only way through is a handle the caller holds; there is no fallback
/// open here, or a client's own handle would be silently ignored whenever a
/// path happened to be present.
fn repo_else<'a>(path: &Path, repo: Option<&'a dyn Repo>) -> Result<&'a dyn Repo, String> {
    repo.ok_or_else(|| format!("no repository opened for {}", path.display()))
}

fn describe(repo: &dyn Repo, revspec: &str) -> String {
    format!("{} {revspec}", repo.describe()).trim().into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use plait_core::status::Status;
    use plait_git::{Handle, Pair};
    use std::path::PathBuf;
    use std::sync::Arc;

    /// A repository that exists only as this struct. Every assertion these
    /// tests make lands on data it produced, which is what proves acquisition
    /// goes through the injected handle and never around it.
    struct Fake {
        label: &'static str,
    }

    impl Default for Fake {
        fn default() -> Self {
            Self {
                label: "fake (main)",
            }
        }
    }

    fn commit(sha: &str) -> Commit {
        Commit {
            sha: sha.into(),
            short: sha.into(),
            parents: Box::from(&[][..]),
            author: "fake".into(),
            timestamp: 0,
            subject: "from the fake".into(),
        }
    }

    impl Repo for Fake {
        fn log(&self, limit: usize) -> plait_git::Result<Vec<Commit>> {
            // Honours the limit, so a caller's argument is observable.
            Ok((0..limit.min(2))
                .map(|i| commit(&format!("f{i}")))
                .collect())
        }

        fn pairs(&self, _revspec: &str) -> plait_git::Result<Vec<Pair>> {
            // Two changes separated by ten shared lines: narrow context keeps
            // them as two hunks, wide context merges them into one — which is
            // what makes the host's configured context observable through
            // acquisition without any repository at all.
            let mut old: Vec<Arc<str>> = vec!["changed here".into()];
            let mut new: Vec<Arc<str>> = vec!["CHANGED HERE".into()];
            for i in 0..10 {
                let line: Arc<str> = format!("shared {i}").into();
                old.push(Arc::clone(&line));
                new.push(line);
            }
            old.push("and there".into());
            new.push("AND THERE".into());
            Ok(vec![Pair {
                path: "fake.txt".into(),
                old_path: None,
                status: 'M',
                old,
                new,
                binary: false,
            }])
        }

        fn status(&self) -> plait_git::Result<Status> {
            Ok(Status::default())
        }

        fn describe(&self) -> String {
            self.label.into()
        }
    }

    /// A real handle against a path, for the tests that want actual git.
    fn real(path: &str) -> Handle {
        plait_git::open(Path::new(path))
    }

    fn here() -> Source {
        Source::Repo {
            path: PathBuf::from("."),
            arg: "HEAD~1..HEAD".into(),
        }
    }

    #[test]
    fn an_injected_repo_is_the_one_asked() {
        // `/nonexistent` cannot be read by anything; if this succeeds, every
        // byte of it came from the fake.
        let source = Source::Repo {
            path: PathBuf::from("/nonexistent"),
            arg: "3".into(),
        };
        let loaded = acquire(View::Commits, &source, &Host::new(), Some(&Fake::default()))
            .expect("the fake has history");
        let Data::Commits(commits) = loaded.data else {
            panic!("a commits view loads commits");
        };
        assert_eq!(commits.len(), 2, "the fake honours the parsed limit");
        // The commits view labels with the repository alone; only a diff
        // names its revspec.
        assert_eq!(loaded.label, "fake (main)");
    }

    #[test]
    fn the_injected_repos_pairs_are_diffed_by_the_configured_differ() {
        // Same fake pairs twice, two context settings: only the host's differ
        // can be responsible for the hunk counts differing.
        let count = |context: usize| {
            let mut host = Host::new();
            host.differ.context = context;
            let loaded = acquire(
                View::Diff,
                &Source::Repo {
                    path: PathBuf::from("/nonexistent"),
                    arg: String::new(),
                },
                &host,
                Some(&Fake::default()),
            )
            .unwrap();
            let Data::Diff(files) = loaded.data else {
                panic!("a diff view loads files");
            };
            assert_eq!(files[0].path, "fake.txt");
            files[0].hunks.len()
        };
        assert_eq!(count(1), 2, "narrow context keeps two hunks apart");
        assert_eq!(count(12), 1, "wide context merges them");
    }

    #[test]
    fn a_repo_source_without_a_handle_is_an_error_and_not_an_open() {
        let source = Source::Repo {
            path: PathBuf::from("."),
            arg: String::new(),
        };
        let err = acquire(View::Commits, &source, &Host::new(), None).unwrap_err();
        assert!(err.contains("no repository opened"), "{err}");
    }

    #[test]
    fn a_diff_of_this_repository_arrives_as_parsed_files() {
        let host = Host::new();
        let repo = real(".");
        let loaded = acquire(View::Diff, &here(), &host, Some(repo.as_ref()))
            .expect("this repo has history");
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
        let repo = real(".");
        let a = acquire(View::Diff, &here(), &host, Some(repo.as_ref())).unwrap();
        let mut other = Host::new();
        other.differ.context = 12;
        let b = acquire(View::Diff, &here(), &other, Some(repo.as_ref())).unwrap();
        let hunks = |l: &Loaded| match &l.data {
            Data::Diff(f) => f.iter().map(|f| f.hunks.len()).sum::<usize>(),
            _ => 0,
        };
        assert!(
            hunks(&a) >= hunks(&b),
            "more context merges hunks; it did not reach the differ"
        );
    }

    #[test]
    fn a_path_that_is_not_a_repository_is_a_message_and_not_a_panic() {
        let host = Host::new();
        let repo = real("/nonexistent");
        let source = Source::Repo {
            path: PathBuf::from("/nonexistent"),
            arg: String::new(),
        };
        assert!(acquire(View::Commits, &source, &host, Some(repo.as_ref())).is_err());
        assert!(acquire(View::Diff, &source, &host, Some(repo.as_ref())).is_err());
    }

    #[test]
    fn an_empty_revspec_says_which_revspec_it_meant() {
        let host = Host::new();
        // A revspec that resolves to nothing changed. The message has to name
        // it, or "no changes" is indistinguishable from a broken argument.
        let source = Source::Repo {
            path: PathBuf::from("."),
            arg: "HEAD..HEAD".into(),
        };
        let err = acquire(View::Diff, &source, &host, Some(real(".").as_ref())).unwrap_err();
        assert!(err.contains("HEAD..HEAD"), "{err}");
    }

    #[test]
    fn a_missing_fixture_says_how_to_make_one() {
        // Only meaningful from a directory without fixtures, so assert on the
        // shape of the message rather than on whether this run has them.
        let host = Host::new();
        if let Err(e) = acquire(View::Diff, &Source::Fixtures, &host, None) {
            assert!(e.contains("fixtures/"), "{e}");
            assert!(
                e.contains("./fixtures/"),
                "the message did not say how: {e}"
            );
        }
    }

    #[test]
    fn a_commit_limit_is_the_second_positional() {
        let host = Host::new();
        let source = Source::Repo {
            path: PathBuf::from("."),
            arg: "3".into(),
        };
        let loaded = acquire(View::Commits, &source, &host, Some(real(".").as_ref())).unwrap();
        assert_eq!(loaded.data.len(), 3);
    }
}
