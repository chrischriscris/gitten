//! Turning what the command line said into data a client can draw.
//!
//! One function, and it is the whole of what sits between
//! [`cli::parse`](crate::cli::parse) and a view. It uses the host's own
//! `Differs`, which is the point: *which algorithm ran* is a configured choice
//! and this is the one place it is made, so `[diff] algorithm` in `gitten.toml`
//! means the same thing in a window, a terminal and an agent's command line.
//!
//! It returns **`Vec<FileDiff>`, not prepared rows.** Every client wants
//! something different one stage later — the shell keeps the parsed diff so a
//! layout change can rebuild, the web API prepares immediately and holds a
//! window of rows, the terminal does both — and `prepare` is one call away in
//! `core`. Stopping here is what keeps this from being a fourth opinion about
//! what a client needs.

use crate::cli::{Source, View};
use gitten_core::differ::{Differs, Overrides};
use gitten_core::host::Host;
use gitten_core::refs::Stash;
use gitten_core::source::DiffSource;
use gitten_core::status::Status;
use gitten_core::{Commit, FileDiff};
use gitten_git::Repo;
use std::collections::HashMap;
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
    /// One conflicted file: the working-tree bytes parsed into regions, and
    /// the stages git still holds for the path — the undo's raw material
    /// and the delete/modify answer's evidence.
    Conflict(
        gitten_core::conflict::ConflictFile,
        Vec<gitten_git::UnmergedStage>,
    ),
}

impl Data {
    /// How many rows the view will be given, for a load message. Not the number
    /// of rows on screen — wrapping and the presentation both change that.
    pub fn len(&self) -> usize {
        match self {
            Data::Commits(c) => c.len(),
            Data::Diff(f) => f.len(),
            Data::Conflict(file, _) => file.regions.len(),
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

/// Acquires one diff by **source**, not by revspec — the preview door.
///
/// [`acquire`](crate::acquire::acquire) answers "what does this view say",
/// from the command line's own grammar; this one answers "what does this
/// file, this side, this commit, this stash say", from
/// [`DiffSource`]'s. Same pipeline behind both — [`Repo`] acquires the
/// content, the host's own `Differs` decides which lines correspond, and
/// nothing here invents text a read did not answer with.
///
/// The [`Differs`] and the overrides come in separately from a whole
/// [`Host`] on purpose: a client that previews off the input path (as the
/// terminal does) runs this on a thread, and a thread takes what it needs.
/// `Differs` is `Clone`, and a clone shares the answer cache — the diff a
/// preview computes is the diff a refresh would otherwise compute again.
///
/// `allow_empty` is the same distinction [`reacquire`](crate::acquire::reacquire)
/// draws: a preview refuses to install an empty answer that says the side
/// moved under the selection, while a refresh of an already-open source may
/// honestly land empty.
pub fn diff_source(
    source: &DiffSource,
    differs: &gitten_core::differ::Differs,
    over: &Overrides,
    repo: &dyn Repo,
    allow_empty: bool,
) -> Result<Loaded, String> {
    use gitten_git::diff_pairs;
    // The conflict source is not a diff and never becomes one: its answer
    // is the file's own markers and the stages behind them. It also carries
    // its own empty answer — a file whose markers are gone is "resolved",
    // which is a state a merging view wants to draw, not an error.
    if let DiffSource::Conflict { path } = source {
        let file = repo.conflict_file(path.as_bytes())?;
        let stages = repo.unmerged(path.as_bytes())?;
        return Ok(Loaded {
            label: source.label(),
            data: Data::Conflict(file, stages),
        });
    }
    let pairs = match source {
        DiffSource::Staged { path } => repo.pairs_staged(Some(path.as_bytes()))?,
        DiffSource::Unstaged { path } => repo.pairs_unstaged(Some(path.as_bytes()))?,
        DiffSource::Untracked { path } => match repo.pair_untracked(path.as_bytes())? {
            Some(pair) => vec![pair],
            // An unreadable file — deleted between the list and the
            // preview, a broken symlink — is not an empty diff and not a
            // crash: it is nothing to show, said plainly.
            None => {
                return Err(format!(
                    "{} is not readable — deleted, or not a file",
                    path.to_string_lossy()
                ))
            }
        },
        // A bare revision is "what did this commit change" to
        // [`Repo::pairs`], merges included — and a stash commit is one, so
        // its tracked half is the same read. The untracked half is the
        // third parent's own answer, and goes first, exactly as the
        // aggregate read puts creations before modifications.
        DiffSource::Commit { sha } => repo.pairs(sha)?,
        DiffSource::Stash { commit, .. } => {
            let tracked = repo.pairs(commit)?;
            let untracked = repo
                .pairs_stash_untracked(commit)
                .map_err(|e| format!("the stash's untracked files: {e}"))?;
            let mut pairs = Vec::with_capacity(tracked.len() + untracked.len());
            pairs.extend(untracked);
            pairs.extend(tracked);
            pairs
        }
        DiffSource::Revspec { arg } => repo.pairs(arg)?,
        // The conflict source answered above; detached content has no
        // repository behind it and no read to run: a caller asking is the
        // bug, and the message is the usage.
        DiffSource::Conflict { .. } | DiffSource::Fixture | DiffSource::Patch => {
            return Err("this diff has no repository behind it".into())
        }
    };
    if pairs.is_empty() && !allow_empty {
        let what = match source {
            DiffSource::Staged { path } => format!("nothing staged for {}", path),
            DiffSource::Unstaged { path } => format!("nothing unstaged for {}", path),
            DiffSource::Untracked { path } => format!("nothing untracked named {}", path),
            DiffSource::Commit { sha } => format!("{sha} changed nothing"),
            DiffSource::Stash { index, .. } => format!("stash@{{{index}}} holds no changes"),
            DiffSource::Revspec { arg } if arg.is_empty() => {
                "no changes for (working tree)".to_string()
            }
            DiffSource::Revspec { arg } => format!("no changes for {arg}"),
            // All unreachable: the conflict source returned above, and the
            // detached sources refused above.
            DiffSource::Conflict { .. } | DiffSource::Fixture | DiffSource::Patch => {
                unreachable!("refused above")
            }
        };
        return Err(what);
    }
    Ok(Loaded {
        label: source.label(),
        data: Data::Diff(diff_pairs(&pairs, differs, over)),
    })
}

/// Per-path staged/total hunk counts for the paths a [`Status`] says are
/// staged — the `(staged, total)` behind the sidebar's `n/m` fractions and
/// the inspector's staged summary.
///
/// Only staged paths are read: an unstaged-only file never shows a fraction
/// (its box is empty, its `Unstaged` needs no denominator), and untracked or
/// conflicted paths have no index side to count. A staged-only path's total
/// is its staged count — one read; a twin's total adds the unstaged side's —
/// two. Every read goes through [`gitten_git::diff_pairs`], so unchanged
/// blob pairs are remembered work, not recomputed diffs.
///
/// A path whose read fails is skipped, not refused: counts are display state,
/// and a file the sidebar already lists must not lose its row because a hunk
/// count raced a write. Callers fall back to file-level boxes — staged rows
/// checked, unstaged rows empty — which is the truth the counts refine.
pub fn side_hunk_counts(
    repo: &dyn Repo,
    differs: &Differs,
    over: &Overrides,
    status: &Status,
) -> HashMap<Vec<u8>, (u32, u32)> {
    fn hunks_of(
        differs: &Differs,
        over: &Overrides,
        pairs: gitten_git::Result<Vec<gitten_git::Pair>>,
    ) -> Option<u32> {
        let pairs = pairs.ok()?;
        Some(
            gitten_git::diff_pairs(&pairs, differs, over)
                .iter()
                .map(|f| f.hunks.len() as u32)
                .sum(),
        )
    }
    let mut out = HashMap::new();
    for entry in &status.staged {
        let path = entry.path.as_bytes();
        let Some(staged) = hunks_of(differs, over, repo.pairs_staged(Some(path))) else {
            continue;
        };
        let total = match status.unstaged.iter().any(|u| u.path.as_bytes() == path) {
            true => staged + hunks_of(differs, over, repo.pairs_unstaged(Some(path))).unwrap_or(0),
            false => staged,
        };
        out.insert(path.to_vec(), (staged, total));
    }
    out
}

/// The command line's own source, as an explicit diff source — what a
/// launch's diff pane was acquired from. An empty revspec keeps meaning the
/// combined HEAD→worktree read; the model just says so by name.
impl From<&Source> for DiffSource {
    fn from(source: &Source) -> Self {
        match source {
            Source::Repo { arg, .. } => DiffSource::Revspec { arg: arg.clone() },
            Source::Fixtures => DiffSource::Fixture,
            Source::Patch { .. } => DiffSource::Patch,
        }
    }
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

/// What the ancillary stash read loaded: the repository's own description
/// and its stack, kept as separate fields — the label names the repository
/// the way every other read's does, the stack is the data a stash pane draws.
#[derive(Debug)]
pub struct LoadedStashes {
    pub label: String,
    pub stashes: Vec<Stash>,
}

/// Reads one repository's stash stack beside its description.
///
/// The ancillary read of a repository-backed launch: commits and diffs are
/// views someone asked for, the stash stack is the pane every repository
/// gets, and it is acquired through the same door as everything else — a
/// [`Repo`] handle in, already-loaded data out. Description and stack run
/// beside each other, one spawn floor for the two of them, the same overlap
/// the startup views run their title read with.
///
/// **An empty stack is a successful read.** Before the first push and after
/// the last pop or drop, nothing parked is the state of the world and not a
/// failure — the opposite of a startup view, where an empty answer usually
/// means the arguments were wrong. The repository's own error comes back
/// verbatim: a read that failed says so, and is never flattened into an
/// empty list that would draw as success.
pub fn stashes(repo: &dyn Repo) -> Result<LoadedStashes, String> {
    std::thread::scope(|s| {
        let title = s.spawn(|| repo.describe());
        let stashes = repo.stashes()?;
        Ok(LoadedStashes {
            label: joined(title),
            stashes,
        })
    })
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
    use gitten_core::source::DiffSource;
    use gitten_core::status::Status;
    use gitten_git::{Handle, Pair};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

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
                // Constant by construction: this repository does not exist,
                // so nothing in it can change, which is exactly what makes
                // the pair's blob identity stable across acquisitions —
                // and what lets the diff-cache tests observe a hit.
                old_oid: Some("1111111111111111111111111111111111111111".into()),
                new_oid: Some("2222222222222222222222222222222222222222".into()),
                old_final_newline: true,
                new_final_newline: true,
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

    /// A repository whose stash read is the test's to script: a queue of
    /// answers, drained one per call — two stashes, then an empty stack — or
    /// a refusal that stands. Nothing else about it is reachable, so every
    /// byte a test asserts on arrived through the helper under test.
    struct StashFake {
        label: &'static str,
        answers: Mutex<Vec<Vec<Stash>>>,
        refuse: Option<&'static str>,
    }

    impl Default for StashFake {
        fn default() -> Self {
            Self {
                label: "scratch (main)",
                answers: Mutex::new(Vec::new()),
                refuse: None,
            }
        }
    }

    impl Repo for StashFake {
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
            self.label.into()
        }

        fn stashes(&self) -> gitten_git::Result<Vec<Stash>> {
            match self.refuse {
                Some(e) => Err(e.to_string()),
                None => Ok(self.answers.lock().unwrap().pop().unwrap_or_default()),
            }
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

    /// **One file, two sides, three contents.** The acceptance shape the
    /// preview contract is built on: a scratch file with HEAD=A, index=B,
    /// worktree=C yields precisely A→B from the staged source and B→C from
    /// the unstaged source — and neither answer is the aggregate read's,
    /// which is the defect this packet exists to remove. A real repository,
    /// because the claim is about what git answers, not what a fake was told.
    #[test]
    fn a_file_previews_as_head_to_index_and_index_to_worktree() {
        let repo = Scratch::new("sides");
        repo.commit(b"alpha\nshared\nomega\n"); // HEAD: A
        std::fs::write(repo.0.join("f.txt"), b"ALPHA\nshared\nomega\n").expect("index");
        repo.git(&["add", "f.txt"]); // index: B
        std::fs::write(repo.0.join("f.txt"), b"ALPHA\nSHARED\nOMEGA\n").expect("worktree"); // C
        let handle = gitten_git::open(&repo.0);
        let differ = Host::new().differ;

        let staged = diff_source(
            &DiffSource::Staged {
                path: "f.txt".into(),
            },
            &differ,
            &Overrides::default(),
            handle.as_ref(),
            false,
        )
        .expect("the staged side has the change");
        let unstaged = diff_source(
            &DiffSource::Unstaged {
                path: "f.txt".into(),
            },
            &differ,
            &Overrides::default(),
            handle.as_ref(),
            false,
        )
        .expect("the unstaged side has the change");

        // A→B, byte for byte, and B→C likewise.
        let changed = |l: &Loaded| {
            let Data::Diff(files) = &l.data else {
                panic!("a preview loads a diff");
            };
            assert_eq!(files.len(), 1);
            files[0]
                .hunks
                .iter()
                .flat_map(|h| &h.lines)
                .filter(|l| l.kind != gitten_core::LineKind::Context)
                .map(|l| (l.kind, l.text.to_string()))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            changed(&staged),
            vec![
                (gitten_core::LineKind::Removed, "alpha".to_string()),
                (gitten_core::LineKind::Added, "ALPHA".to_string()),
            ],
            "the staged side is HEAD\u{2192}index, not HEAD\u{2192}worktree"
        );
        assert_eq!(
            changed(&unstaged),
            vec![
                (gitten_core::LineKind::Removed, "shared".to_string()),
                (gitten_core::LineKind::Removed, "omega".to_string()),
                (gitten_core::LineKind::Added, "SHARED".to_string()),
                (gitten_core::LineKind::Added, "OMEGA".to_string()),
            ],
            "the unstaged side is index\u{2192}worktree, not HEAD\u{2192}worktree"
        );
    }

    /// A file known to no part of git previews from disk alone, through the
    /// same pipeline: empty old side, the file's own lines as the new one.
    #[test]
    fn an_untracked_file_previews_from_disk_and_nowhere_else() {
        let repo = Scratch::new("loose");
        repo.commit(b"seed\n");
        std::fs::write(repo.0.join("g.txt"), b"brand new\nlines\n").expect("untracked");
        let handle = gitten_git::open(&repo.0);
        let loaded = diff_source(
            &DiffSource::Untracked {
                path: "g.txt".into(),
            },
            &Host::new().differ,
            &Overrides::default(),
            handle.as_ref(),
            false,
        )
        .expect("the file is readable");
        let Data::Diff(files) = loaded.data else {
            panic!("a preview loads a diff");
        };
        assert_eq!(files[0].path, "g.txt");
        assert_eq!(
            files[0]
                .hunks
                .iter()
                .flat_map(|h| &h.lines)
                .map(|l| (l.kind, l.text.to_string()))
                .collect::<Vec<_>>(),
            vec![
                (gitten_core::LineKind::Added, "brand new".to_string()),
                (gitten_core::LineKind::Added, "lines".to_string()),
            ]
        );
    }

    /// **Unborn and empty are states, not crashes.** An index that holds a
    /// file on a branch with no commits previews against the empty tree; a
    /// side with nothing in it says what is missing; a file that is not on
    /// disk says so; and no path anywhere panics.
    #[test]
    fn an_unborn_repository_previews_its_index_and_says_what_is_missing() {
        let repo = Scratch::new("unborn");
        std::fs::write(repo.0.join("f.txt"), b"only the index holds me\n").expect("staged");
        repo.git(&["add", "f.txt"]);
        let handle = gitten_git::open(&repo.0);
        let differ = Host::new().differ;

        let staged = diff_source(
            &DiffSource::Staged {
                path: "f.txt".into(),
            },
            &differ,
            &Overrides::default(),
            handle.as_ref(),
            false,
        )
        .expect("an unborn branch's index reads against the empty tree");
        let Data::Diff(files) = staged.data else {
            panic!("a preview loads a diff");
        };
        assert_eq!(
            files[0]
                .hunks
                .iter()
                .flat_map(|h| &h.lines)
                .map(|l| (l.kind, l.text.to_string()))
                .collect::<Vec<_>>(),
            vec![(
                gitten_core::LineKind::Added,
                "only the index holds me".to_string()
            )]
        );

        // A side with nothing in it is a message, not an empty success and
        // not a git refusal about a revision that does not exist.
        let unstaged = diff_source(
            &DiffSource::Unstaged {
                path: "f.txt".into(),
            },
            &differ,
            &Overrides::default(),
            handle.as_ref(),
            false,
        );
        assert_eq!(
            unstaged.unwrap_err(),
            "nothing unstaged for f.txt",
            "the empty side names itself"
        );
        let missing = diff_source(
            &DiffSource::Untracked {
                path: "ghost.txt".into(),
            },
            &differ,
            &Overrides::default(),
            handle.as_ref(),
            false,
        );
        assert!(
            missing.unwrap_err().contains("not readable"),
            "a file that is not on disk says so"
        );
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
            // A conflict read is not lines of a diff; the line-shaped test
            // helpers above never acquire one.
            Data::Conflict(..) => Vec::new(),
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

    /// A repository with one hunk per side read: `a.rs` staged only,
    /// `b.rs` a twin, `c.rs` unstaged only, `gone.rs` staged but unreadable.
    struct Sides;

    fn one_hunk_pair(path: &str, old_mark: &str, new_mark: &str, tag: &str) -> Pair {
        Pair {
            path: path.into(),
            old_path: None,
            status: 'M',
            old: vec![old_mark.into()],
            new: vec![new_mark.into()],
            old_oid: Some(format!("staged-{tag}")),
            new_oid: Some(format!("new-{tag}")),
            old_final_newline: true,
            new_final_newline: true,
            binary: false,
        }
    }

    impl Repo for Sides {
        fn log(&self, _limit: usize) -> gitten_git::Result<Vec<Commit>> {
            Ok(Vec::new())
        }

        fn pairs(&self, _revspec: &str) -> gitten_git::Result<Vec<Pair>> {
            Ok(Vec::new())
        }

        fn pairs_staged(&self, path: Option<&[u8]>) -> gitten_git::Result<Vec<Pair>> {
            let path = path
                .map(|p| String::from_utf8_lossy(p).into_owned())
                .unwrap_or_default();
            match path.as_str() {
                "a.rs" => Ok(vec![one_hunk_pair("a.rs", "old a", "new a", "a")]),
                "b.rs" => Ok(vec![one_hunk_pair("b.rs", "old b", "new b", "b")]),
                _ => Err("staged side unreadable".into()),
            }
        }

        fn pairs_unstaged(&self, path: Option<&[u8]>) -> gitten_git::Result<Vec<Pair>> {
            let path = path
                .map(|p| String::from_utf8_lossy(p).into_owned())
                .unwrap_or_default();
            match path.as_str() {
                "b.rs" => Ok(vec![one_hunk_pair("b.rs", "new b", "newer b", "b2")]),
                _ => Err("unstaged side unreadable".into()),
            }
        }

        fn status(&self) -> gitten_git::Result<Status> {
            use gitten_core::status::{
                Change, Kind, PathBytes, StagedEntry, Submodule, UnstagedEntry,
            };
            let staged = |path: &str| StagedEntry {
                path: PathBytes::from(path),
                change: Change::Modified,
                old_path: None,
                kind: Kind::File,
                submodule: Submodule::default(),
            };
            let unstaged = |path: &str| UnstagedEntry {
                path: PathBytes::from(path),
                change: Change::Modified,
                kind: Kind::File,
                submodule: Submodule::default(),
            };
            Ok(Status {
                staged: vec![staged("a.rs"), staged("b.rs"), staged("gone.rs")],
                unstaged: vec![unstaged("b.rs"), unstaged("c.rs")],
                ..Status::default()
            })
        }

        fn describe(&self) -> String {
            "sides".into()
        }
    }

    #[test]
    fn staged_paths_count_their_sides_and_nothing_else() {
        use gitten_core::differ::{Differs, Overrides};
        let repo = Sides;
        let status = repo.status().unwrap();
        let counts = side_hunk_counts(&repo, &Differs::default(), &Overrides::default(), &status);
        // One read for a staged-only path, two for a twin; an unstaged-only
        // path is never read, and a failed read skips the path instead of
        // refusing the whole map.
        assert_eq!(counts.get(b"a.rs".as_slice()), Some(&(1, 1)));
        assert_eq!(counts.get(b"b.rs".as_slice()), Some(&(1, 2)));
        assert!(!counts.contains_key(b"c.rs".as_slice()));
        assert!(!counts.contains_key(b"gone.rs".as_slice()));
    }

    /// A differ that counts how often it was asked, and answers with one
    /// whole-file replace. Shared counter, because the registry takes
    /// ownership of the implementation it is handed.
    struct Counting(Arc<AtomicUsize>, &'static str);

    impl gitten_core::differ::Differ for Counting {
        fn name(&self) -> &'static str {
            self.1
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
        host.differ
            .register(Counting(Arc::clone(&calls), "counting"));
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

    /// A refresh after an unrelated write re-acquires everything and re-diffs
    /// only what changed. Through a real repository that is hard to count;
    /// through this fake it is exact: the pair's blob OIDs never change, so
    /// the second acquisition must find its answer waiting.
    #[test]
    fn an_unchanged_pair_is_not_diffed_twice_by_repeated_acquisition() {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut host = Host::new();
        host.differ
            .register(Counting(Arc::clone(&calls), "counting"));
        assert!(host.differ.select("counting"));
        let source = Source::Repo {
            path: PathBuf::from("/nonexistent"),
            arg: String::new(),
        };
        let hunks = |loaded: &Loaded| match &loaded.data {
            Data::Diff(f) => f[0].hunks.clone(),
            _ => panic!("a diff view loads files"),
        };

        let first = reacquire(
            View::Diff,
            &source,
            &host,
            Some(&Fake::default()),
            &Overrides::default(),
        )
        .unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 1);

        // The refresh: same repository state, same settings — the shape of
        // every shell redraw after somebody staged a file elsewhere.
        let second = reacquire(
            View::Diff,
            &source,
            &host,
            Some(&Fake::default()),
            &Overrides::default(),
        )
        .unwrap();
        assert_eq!(
            calls.load(Ordering::Relaxed),
            1,
            "the second acquisition hit the cache"
        );
        assert_eq!(hunks(&first), hunks(&second), "a hit is what a miss said");

        // A context change re-diffs: the key covers every setting that could
        // alter the answer, so a stale hunk can never survive one.
        host.differ.context = 12;
        reacquire(
            View::Diff,
            &source,
            &host,
            Some(&Fake::default()),
            &Overrides::default(),
        )
        .unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 2, "context moved, so a miss");
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

    /// **Absence is data.** The newest-first stack the read gives arrives
    /// through the helper unchanged — description, indices, messages and
    /// full commits alike — and the read after it, an emptied stack, is
    /// `Ok` and not an error: before the first push and after the last pop,
    /// nothing parked is the state of the world.
    #[test]
    fn stash_acquisition_preserves_stack_data_and_accepts_absence() {
        let two = vec![
            Stash {
                index: 0,
                message: "On main: wip things".into(),
                commit: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            },
            Stash {
                index: 1,
                message: "On dev: other work".into(),
                commit: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
            },
        ];
        let repo = StashFake {
            // Popped from the tail: the first read answers two stashes, the
            // second the emptied stack.
            answers: Mutex::new(vec![Vec::new(), two]),
            ..Default::default()
        };

        let loaded = stashes(&repo).expect("two parked");
        assert_eq!(loaded.label, "scratch (main)", "the description ran beside");
        let parked = &loaded.stashes;
        assert_eq!(parked.len(), 2, "{parked:?}");
        // Newest first as the read gave them, every field the read carried.
        assert_eq!(parked[0].index, 0);
        assert_eq!(parked[0].message, "On main: wip things");
        assert_eq!(
            parked[0].commit, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "the full identity is what a refresh anchors by"
        );
        assert_eq!(parked[1].index, 1);
        assert_eq!(parked[1].message, "On dev: other work");
        assert_eq!(parked[1].commit, "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");

        // The stack drained between reads: an empty answer is success.
        let drained = stashes(&repo).expect("an empty stack is not an error");
        assert!(drained.stashes.is_empty());
        assert_eq!(drained.label, "scratch (main)");
    }

    /// A read that failed says so in the repository's own words, and is
    /// never translated into the empty list that would draw as success.
    #[test]
    fn stash_acquisition_preserves_the_repository_refusal() {
        let repo = StashFake {
            refuse: Some("fatal: bad object refs/stash"),
            ..Default::default()
        };
        let err = stashes(&repo).unwrap_err();
        assert_eq!(err, "fatal: bad object refs/stash");
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

    /// The **staging round trip**, over a real repository: one committed file
    /// with two distant edits, one emitted hunk staged through the write seam
    /// and the shared runner, and the mirror verb putting the index back. The
    /// distant second edit is the point — a patch that could not tell its
    /// chosen hunk from its neighbour would stage both and look like it
    /// worked, so the staged side is checked against git's own answer, which
    /// neither door gets to argue with.
    ///
    /// `Scratch::git` is repository setup and read-only oracle only; the
    /// stage and unstage under test travel through
    /// [`Write::stage_patch`](crate::verbs::Write::stage_patch) and its
    /// sibling, behind the same [`Handle`] acquisition uses and the same
    /// [`Runner`](crate::jobs::Runner) every client submits to. No tty, no
    /// window, no terminal.
    #[test]
    fn a_hunk_stages_and_unstages_round_trip_in_a_throwaway_repository() {
        use crate::jobs::{Event, Runner};
        use crate::verbs::Write;
        use gitten_core::{parse_unified_diff, DiffLine, LineKind};
        use std::time::Duration;

        fn wait(runner: &Runner, count: usize) -> Vec<Event> {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let mut events = Vec::new();
            while events.len() < count && std::time::Instant::now() < deadline {
                if let Some(event) = runner.try_next() {
                    events.push(event);
                } else {
                    std::thread::yield_now();
                }
            }
            assert_eq!(events.len(), count, "the worker did not report in time");
            events
        }

        /// The changed lines of a stretch of hunks, as `(kind, text)` — the
        /// part of a hunk that says what an edit *was*, without the context
        /// that moves with the configured width.
        fn changed<'a>(lines: impl IntoIterator<Item = &'a DiffLine>) -> Vec<(LineKind, String)> {
            lines
                .into_iter()
                .filter(|l| l.kind != LineKind::Context)
                .map(|l| (l.kind, l.text.to_string()))
                .collect()
        }

        let host = Host::new();
        let repo = Scratch::new("hunks");
        let committed: String = (0..40).map(|i| format!("line {i}\n")).collect();
        repo.commit(committed.as_bytes());
        let edited: String = (0..40)
            .map(|i| match i {
                4 => "EDIT ONE".to_string(),
                34 => "EDIT TWO".to_string(),
                _ => format!("line {i}"),
            })
            .fold(String::new(), |mut all, line| {
                all.push_str(&line);
                all.push('\n');
                all
            });
        std::fs::write(repo.0.join("f.txt"), &edited).expect("wrote the worktree");

        let handle = gitten_git::open(&repo.0);
        let source = Source::Repo {
            path: repo.0.clone(),
            arg: String::new(),
        };
        let loaded = acquire(View::Diff, &source, &host, Some(handle.as_ref()))
            .expect("the worktree has two edits");
        let Data::Diff(files) = loaded.data else {
            panic!("a diff view loads files");
        };
        assert_eq!(files.len(), 1, "{:?}", files.iter().map(|f| &f.path));
        assert_eq!(files[0].path, "f.txt");
        assert_eq!(files[0].hunks.len(), 2, "distant edits stay two hunks");

        // Stage exactly the first hunk, through the write seam and the
        // shared queue — the only path a client is allowed to reach.
        let patch = gitten_core::patch::emit(&files[0].path, &[&files[0].hunks[0]]);
        let runner = Runner::new();
        let submit = runner.submitter();
        let job = Write::stage_patch(&handle, patch.clone()).expect("a non-empty patch");
        assert!(submit.submit(Box::new(job)).is_ok(), "queued");
        let events = wait(&runner, 2);
        let Event::Finished {
            generation,
            outcome: Ok(()),
            ..
        } = &events[1]
        else {
            panic!("a clean stage: {:?}", events[1]);
        };
        assert_eq!(
            generation.get(),
            1,
            "the first finish is the first generation"
        );

        // The index holds exactly the chosen hunk: git's own staged diff —
        // the read-only oracle — is the emitted patch and nothing else, and
        // the working tree never moved, because `--cached` cannot.
        let staged = parse_unified_diff(&repo.git(&["diff", "--cached"]));
        assert_eq!(
            staged.len(),
            1,
            "{}",
            repo.git(&["diff", "--cached", "--stat"])
        );
        let chosen = changed(files[0].hunks[0].lines.iter());
        assert_eq!(
            changed(staged[0].hunks.iter().flat_map(|h| &h.lines)),
            chosen
        );
        assert!(
            chosen.contains(&(LineKind::Added, "EDIT ONE".into())),
            "the chosen hunk's own edit travelled: {chosen:?}"
        );
        assert!(
            !chosen.contains(&(LineKind::Added, "EDIT TWO".into())),
            "the emitted selection includes the distant neighbour: {chosen:?} — the staged side is pinned by the oracle comparison above"
        );
        assert_eq!(
            std::fs::read_to_string(repo.0.join("f.txt")).expect("the worktree reads"),
            edited,
            "staging rewrote the working tree"
        );

        // Re-acquire the working-tree diff, and emit the staged hunk from
        // what it now answers: the same edit, which the mirror verb
        // reverses through the same seam.
        let again = reacquire(
            View::Diff,
            &source,
            &host,
            Some(handle.as_ref()),
            &Overrides::default(),
        )
        .expect("the worktree still differs from HEAD");
        let Data::Diff(files) = again.data else {
            panic!("a diff view loads files");
        };
        let chosen = files[0]
            .hunks
            .iter()
            .find(|h| h.lines.iter().any(|l| *l.text == *"EDIT ONE"))
            .expect("the staged edit still reads against HEAD");
        let reverse = gitten_core::patch::emit(&files[0].path, &[chosen]);
        let job = Write::unstage_patch(&handle, reverse).expect("a non-empty patch");
        assert!(submit.submit(Box::new(job)).is_ok(), "queued");
        let events = wait(&runner, 2);
        let Event::Finished {
            generation,
            outcome: Ok(()),
            ..
        } = &events[1]
        else {
            panic!("a clean unstage: {:?}", events[1]);
        };
        assert_eq!(
            generation.get(),
            2,
            "the second finish is the second generation"
        );

        // The index is back where HEAD is; the working tree keeps both
        // edits, exactly as they were before either verb ran.
        assert!(
            repo.git(&["diff", "--cached"]).trim().is_empty(),
            "an unstaged index is an empty staged diff: {:?}",
            repo.git(&["diff", "--cached"])
        );
        assert_eq!(
            std::fs::read_to_string(repo.0.join("f.txt")).expect("the worktree reads"),
            edited,
            "unstaging touched the working tree"
        );
    }
}
