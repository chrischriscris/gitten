//! Turning what the command line said into data a client can draw.
//!
//! One function, and it is the whole of what sits between
//! [`cli::parse`](crate::cli::parse) and a view. It uses the host's own
//! `Differs`, which is the point: *which algorithm ran* is a configured choice
//! and this is the one place it is made, so `[diff] algorithm` in `gitten.toml`
//! means the same thing in a window, a browser and a terminal.
//!
//! It returns **`Vec<FileDiff>`, not prepared rows.** Every client wants
//! something different one stage later — the shell keeps the parsed diff so a
//! layout change can rebuild, the browser prepares immediately and holds a
//! window of rows, the terminal does both — and `prepare` is one call away in
//! `core`. Stopping here is what keeps this from being a fourth opinion about
//! what a client needs.

use crate::cli::{Source, View};
use gitten_core::differ::Overrides;
use gitten_core::host::Host;
use gitten_core::{Commit, FileDiff};
use gitten_git::Repo;
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

/// Reads a patch from a named file, or from standard input for `None`.
///
/// Lossy like [`read_fixture`], and for the same reason: a patch came out of
/// git or someone's mailer and guarantees no encoding. The label is what the
/// title bar calls it — the path as it was typed, or `-`.
fn read_patch(file: Option<&Path>) -> Result<(String, String), String> {
    match file {
        Some(path) => {
            let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
            Ok((
                String::from_utf8_lossy(&bytes).into_owned(),
                path.display().to_string(),
            ))
        }
        None => {
            use std::io::Read;
            let mut bytes = Vec::new();
            std::io::stdin()
                .lock()
                .read_to_end(&mut bytes)
                .map_err(|e| format!("standard input: {e}"))?;
            Ok((String::from_utf8_lossy(&bytes).into_owned(), "-".into()))
        }
    }
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
/// generated, a patch that holds no diff.
pub fn acquire(
    view: View,
    source: &Source,
    host: &Host,
    repo: Option<&dyn Repo>,
) -> Result<Loaded, String> {
    acquire_with(view, source, host, repo, &Overrides::default(), false)
}

/// Re-acquires an already-open view after repository state changed.
///
/// Unlike startup, an empty answer is valid: a successful write may have made
/// the working tree clean or removed the last commit matching a temporary view.
pub fn reacquire(
    view: View,
    source: &Source,
    host: &Host,
    repo: Option<&dyn Repo>,
    overrides: &Overrides,
) -> Result<Loaded, String> {
    acquire_with(view, source, host, repo, overrides, true)
}

fn acquire_with(
    view: View,
    source: &Source,
    host: &Host,
    repo: Option<&dyn Repo>,
    overrides: &Overrides,
    allow_empty: bool,
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
                let files = gitten_git::diff(repo, arg, &host.differ, overrides)?;
                if files.is_empty() && !allow_empty {
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
            let files = gitten_core::parse_unified_diff(&read_fixture(DIFF_FIXTURE));
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
        (View::Diff, Source::Patch { file }) => {
            let (raw, label) = read_patch(file.as_deref())?;
            let files = gitten_core::parse_unified_diff(&raw);
            if files.is_empty() {
                return Err(match file {
                    Some(path) => format!("{} holds no unified diff", path.display()),
                    None => "standard input held no unified diff \
                             — pipe one in:  git diff | gitten diff -"
                        .into(),
                });
            }
            Ok(Loaded {
                label,
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
                if commits.is_empty() && !allow_empty {
                    return Err(format!("no commits in {}", path.display()));
                }
                Ok(Loaded {
                    label: joined(title),
                    data: Data::Commits(commits),
                })
            })
        }
        (View::Commits, Source::Fixtures) => {
            let commits = gitten_core::parse_log(&read_fixture(LOG_FIXTURE));
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
        // A patch is one diff and has no history, so this is a message rather
        // than an arm: the person typing asked for something that does not
        // exist, and the usage after it shows what does.
        (View::Commits, Source::Patch { .. }) => {
            Err("a patch is one diff and has no history — open it with `diff`".into())
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
    use gitten_core::status::Status;
    use gitten_git::{Handle, Pair};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// A repository that exists only as this struct. Every assertion these
    /// tests make lands on data it produced, which is what proves acquisition
    /// goes through the injected handle and never around it.
    struct Fake {
        label: &'static str,
    }

    struct Empty;

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
        fn log(&self, limit: usize) -> gitten_git::Result<Vec<Commit>> {
            // Honours the limit, so a caller's argument is observable.
            Ok((0..limit.min(2))
                .map(|i| commit(&format!("f{i}")))
                .collect())
        }

        fn pairs(&self, _revspec: &str) -> gitten_git::Result<Vec<Pair>> {
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

        fn status(&self) -> gitten_git::Result<Status> {
            Ok(Status::default())
        }

        fn describe(&self) -> String {
            self.label.into()
        }
    }

    impl Repo for Empty {
        fn log(&self, _limit: usize) -> gitten_git::Result<Vec<Commit>> {
            Ok(Vec::new())
        }

        fn pairs(&self, _revspec: &str) -> gitten_git::Result<Vec<Pair>> {
            Ok(Vec::new())
        }

        fn status(&self) -> gitten_git::Result<Status> {
            Ok(Status::default())
        }

        fn describe(&self) -> String {
            "empty".into()
        }
    }

    /// A real handle against a path, for the tests that want actual git.
    fn real(path: &str) -> Handle {
        gitten_git::open(Path::new(path))
    }

    fn here() -> Source {
        Source::Repo {
            path: PathBuf::from("."),
            arg: "HEAD~1..HEAD".into(),
        }
    }

    /// A throwaway repository, for the one property that needs two doors and a
    /// real commit to check.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("gitten-app-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("a temp dir");
            let me = Scratch(dir);
            me.git(&["init", "-q", "."]);
            // Never rewrite endings on the way in or out: this repository exists
            // to hold the exact bytes it was given.
            me.git(&["config", "core.autocrlf", "false"]);
            me
        }

        fn git(&self, args: &[&str]) -> String {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&self.0)
                .args(["-c", "user.email=t@t", "-c", "user.name=t"])
                .args(args)
                .output()
                .expect("git runs");
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).into_owned()
        }

        fn commit(&self, content: &[u8]) {
            std::fs::write(self.0.join("f.txt"), content).expect("wrote the file");
            self.git(&["add", "f.txt"]);
            self.git(&["commit", "-qm", "x"]);
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn lines_of(loaded: &Loaded) -> Vec<(gitten_core::LineKind, String)> {
        match &loaded.data {
            Data::Diff(files) => files
                .iter()
                .flat_map(|f| &f.hunks)
                .flat_map(|h| &h.lines)
                .map(|l| (l.kind, l.text.to_string()))
                .collect(),
            Data::Commits(_) => Vec::new(),
        }
    }

    /// **The two doors must describe the same commit identically.** A repository
    /// and a `.diff` of that repository are the same change arriving by different
    /// routes, and for a long time they disagreed: acquisition stripped the `\r`
    /// of a CRLF line and `parse_unified_diff` did too, by different mechanisms
    /// and with different consequences. The repository door reported a changed
    /// file with `+0 -0` and no hunks — indistinguishable from a binary file —
    /// while the patch door reported the right counts over the wrong text.
    ///
    /// Line endings are what makes this checkable at all: it is the one change
    /// git can express that lives entirely in the bytes a careless parser drops.
    #[test]
    fn a_line_ending_change_reads_the_same_from_a_repo_and_from_a_patch_of_it() {
        let host = Host::new();
        let repo = Scratch::new("crlf");
        repo.commit(b"alpha\nbeta\ngamma\n");
        repo.commit(b"alpha\r\nbeta\r\ngamma\r\n");

        let handle = gitten_git::open(&repo.0);
        let from_repo = acquire(
            View::Diff,
            &Source::Repo {
                path: repo.0.clone(),
                arg: "HEAD~1..HEAD".into(),
            },
            &host,
            Some(handle.as_ref()),
        )
        .expect("the repository has the change");

        // git's own answer, as the arbiter neither door gets to argue with.
        let numstat = repo.git(&["diff", "--numstat", "HEAD~1..HEAD"]);
        assert!(numstat.starts_with("3\t3\t"), "git said {numstat:?}");

        let patch = repo.0.join("crlf.diff");
        std::fs::write(&patch, repo.git(&["diff", "HEAD~1..HEAD"])).expect("wrote the patch");
        let from_patch = acquire(
            View::Diff,
            &Source::Patch { file: Some(patch) },
            &host,
            None,
        )
        .expect("the patch parses");

        let expected = [
            (gitten_core::LineKind::Removed, "alpha".to_string()),
            (gitten_core::LineKind::Removed, "beta".to_string()),
            (gitten_core::LineKind::Removed, "gamma".to_string()),
            (gitten_core::LineKind::Added, "alpha\r".to_string()),
            (gitten_core::LineKind::Added, "beta\r".to_string()),
            (gitten_core::LineKind::Added, "gamma\r".to_string()),
        ];
        assert_eq!(lines_of(&from_repo), expected, "the repository door");
        assert_eq!(lines_of(&from_patch), expected, "the patch door");
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

    /// A differ that counts how often it was asked, and answers with one
    /// whole-file replace. Shared counter, because the registry takes
    /// ownership of the implementation it is handed.
    struct Counting(Arc<AtomicUsize>);

    impl gitten_core::differ::Differ for Counting {
        fn name(&self) -> &'static str {
            "counting"
        }

        fn diff(
            &self,
            _path: &str,
            old: &[Arc<str>],
            new: &[Arc<str>],
        ) -> Vec<gitten_core::differ::Edit> {
            self.0.fetch_add(1, Ordering::Relaxed);
            vec![gitten_core::differ::Edit {
                old_start: 0,
                old_end: old.len() as u32,
                new_start: 0,
                new_end: new.len() as u32,
            }]
        }
    }

    #[test]
    fn no_repo_can_replace_the_configured_differ() {
        // The fake's `pairs` could answer with anything, but a `Repo` has no
        // way to say *which lines correspond* — that decision happens after
        // acquisition, through the host's registry. Registering a counting
        // differ and selecting it makes the authority observable: the count
        // moves, and the hunks are the counting differ's shape (one edit, so
        // one hunk at any context), which nothing in `pairs` chose.
        let calls = Arc::new(AtomicUsize::new(0));
        let mut host = Host::new();
        host.differ.register(Counting(Arc::clone(&calls)));
        assert!(
            host.differ.select("counting"),
            "a registered extension algorithm is selectable"
        );

        for context in [0, 1, 12] {
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
            assert_eq!(files.len(), 1);
            assert_eq!(
                files[0].hunks.len(),
                1,
                "context {context}: one whole-file edit assembles to one hunk"
            );
        }
        assert_eq!(
            calls.load(Ordering::Relaxed),
            3,
            "every file went through the configured differ, once per acquire"
        );
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
    fn refresh_accepts_empty_commits_and_diffs_that_startup_rejects() {
        let source = Source::Repo {
            path: PathBuf::from("/nonexistent"),
            arg: String::new(),
        };
        for view in [View::Commits, View::Diff] {
            assert!(acquire(view, &source, &Host::new(), Some(&Empty)).is_err());
            let loaded = reacquire(
                view,
                &source,
                &Host::new(),
                Some(&Empty),
                &Overrides::default(),
            )
            .expect("an empty refresh is valid");
            assert!(loaded.data.is_empty());
        }
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
        // `gitten.toml` has to reach the thing that actually diffs.
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

    /// A small real diff, so a patch arm has something honest to parse.
    const PATCH: &str = "\
diff --git a/src/main.rs b/src/main.rs
index 3e7a1b2..9c4d0f1 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,3 +1,4 @@
 fn main() {
+    let answer = 42;
     println!(\"hello\");
 }
";

    fn patch_file(name: &str, contents: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("gitten-patch-test-{name}"));
        std::fs::write(&path, contents).expect("wrote the test patch");
        path
    }

    #[test]
    fn a_patch_file_arrives_parsed_without_a_repository() {
        let host = Host::new();
        let source = Source::Patch {
            file: Some(patch_file("ok.diff", PATCH)),
        };
        let loaded = acquire(View::Diff, &source, &host, None).expect("the patch parses");
        assert!(matches!(loaded.data, Data::Diff(_)));
        assert!(!loaded.data.is_empty());
        assert!(loaded.label.ends_with("ok.diff"), "{}", loaded.label);
    }

    #[test]
    fn an_empty_patch_says_so_rather_than_opening_on_nothing() {
        let host = Host::new();
        let source = Source::Patch {
            file: Some(patch_file("empty.diff", "")),
        };
        let err = acquire(View::Diff, &source, &host, None).unwrap_err();
        assert!(err.contains("no unified diff"), "{err}");
        assert!(err.contains("empty.diff"), "{err}");
    }

    #[test]
    fn a_patch_is_not_history_and_says_what_to_do_instead() {
        let host = Host::new();
        let source = Source::Patch {
            file: Some(patch_file("hist.diff", PATCH)),
        };
        let err = acquire(View::Commits, &source, &host, None).unwrap_err();
        assert!(err.contains("diff"), "{err}");
    }
}
