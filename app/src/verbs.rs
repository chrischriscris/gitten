//! The write verbs, as jobs.
//!
//! Reads happen wherever acquisition needs them and are measured in
//! milliseconds; a write is rare, deliberate and latency-insensitive, so each
//! one crosses the same [`Job`](crate::jobs::Job) seam the runner executes.
//! This module is the wrapper and nothing else: it captures a [`Handle`] clone
//! plus the verb's arguments, names itself for the running band, and calls the
//! trait. No client learns whether the implementation shelled out, and an
//! extension composes these exact words — or its own, over the same handle and
//! the same queue — without a line changing here.

use crate::jobs::Job;
use gitten_git::{Handle, Repo};

/// The write itself: a closure over the trait, so an extension's verb and a
/// built-in's differ in nothing but the words they call.
type Op = Box<dyn FnOnce(&dyn Repo) -> Result<(), String> + Send>;

/// One repository write, ready for a [`Submitter`](crate::jobs::Submitter).
///
/// Built through [`Write::stage`], [`Write::discard`] and their siblings;
/// anything else is the constructor with a closure of your own, which is what
/// keeps this from being a list of built-ins wearing a struct.
pub struct Write {
    name: String,
    repo: Handle,
    op: Op,
}

/// The band's count of paths, said the way a person says it.
fn many(n: usize) -> String {
    match n {
        1 => "1 path".into(),
        n => format!("{n} paths"),
    }
}

impl Write {
    fn named(
        name: String,
        repo: &Handle,
        op: impl FnOnce(&dyn Repo) -> Result<(), String> + Send + 'static,
    ) -> Self {
        Self {
            name,
            repo: Handle::clone(repo),
            op: Box::new(op),
        }
    }

    /// Stages one path — `git add --`, which picks up untracked files,
    /// modifications and deletions alike.
    pub fn stage(repo: &Handle, path: Vec<u8>) -> Self {
        let shown = String::from_utf8_lossy(&path).into_owned();
        Self::named(format!("stage {shown}"), repo, move |r| r.stage(&path))
    }

    /// Unstages one path — `git reset` against HEAD; the working tree is
    /// never touched.
    pub fn unstage(repo: &Handle, path: Vec<u8>) -> Self {
        let shown = String::from_utf8_lossy(&path).into_owned();
        Self::named(format!("unstage {shown}"), repo, move |r| r.unstage(&path))
    }

    /// Stages every path as one job — the stage-all command's shape. One job,
    /// not one per path, because each finish is a generation bump and a
    /// re-acquire wave, and forty of those for one keypress is forty lies
    /// about what happened.
    pub fn stage_many(repo: &Handle, paths: Vec<Vec<u8>>) -> Self {
        Self::named(format!("stage {}", many(paths.len())), repo, move |r| {
            let refs: Vec<&[u8]> = paths.iter().map(|p| p.as_slice()).collect();
            r.stage_many(&refs)
        })
    }

    /// Unstages every path as one job — [`Write::stage_many`]'s mirror.
    pub fn unstage_many(repo: &Handle, paths: Vec<Vec<u8>>) -> Self {
        Self::named(format!("unstage {}", many(paths.len())), repo, move |r| {
            let refs: Vec<&[u8]> = paths.iter().map(|p| p.as_slice()).collect();
            r.unstage_many(&refs)
        })
    }

    /// Checks out one path's working-tree state away. DESTRUCTIVE: unstaged
    /// work ends here, which is why the caller confirms before this job is
    /// ever built.
    pub fn discard(repo: &Handle, path: Vec<u8>) -> Self {
        let shown = String::from_utf8_lossy(&path).into_owned();
        Self::named(format!("discard {shown}"), repo, move |r| r.discard(&path))
    }

    /// Deletes one untracked file — discard's mechanics for a file git has
    /// no earlier version of. Destructive in the plain sense: nothing is
    /// recoverable from the object database, because nothing was ever in it.
    pub fn remove_untracked(repo: &Handle, path: Vec<u8>) -> Self {
        let shown = String::from_utf8_lossy(&path).into_owned();
        Self::named(format!("delete {shown}"), repo, move |r| {
            r.remove_untracked(&path)
        })
    }

    /// Appends one path to `.gitignore`. Not destructive — the file stays on
    /// disk — but it edits a user-authored file, so it rides the same queue
    /// and answers through the same bands as everything else that writes.
    pub fn ignore(repo: &Handle, path: Vec<u8>) -> Self {
        let shown = String::from_utf8_lossy(&path).into_owned();
        Self::named(format!("ignore {shown}"), repo, move |r| r.ignore(&path))
    }

    /// Commits the index with `message`. The returned OID has no consumer on
    /// the job rails — a successful finish bumps the generation and every
    /// repository pane re-acquires, which is how the new commit becomes
    /// visible — so it ends here; the trait still answers it for whoever asks
    /// directly.
    pub fn commit(repo: &Handle, message: String) -> Self {
        Self::named("commit".into(), repo, move |r| {
            r.commit(&message).map(|_| ())
        })
    }

    /// Parks the tracked working tree on the stash stack — `git stash push`.
    /// The returned index (always `0`) has no consumer here, for the same
    /// reason [`Write::commit`]'s OID does not: a clean finish is one
    /// generation bump and every pane re-reads.
    pub fn stash_push(repo: &Handle, message: Option<String>) -> Self {
        Self::named("stash push".into(), repo, move |r| {
            r.stash_push(message.as_deref()).map(|_| ())
        })
    }

    /// Restores stash `index`, keeping the entry.
    pub fn stash_apply(repo: &Handle, index: usize) -> Self {
        Self::named(format!("stash apply stash@{index}"), repo, move |r| {
            r.stash_apply(index)
        })
    }

    /// Restores stash `index` and drops it when the restore was clean —
    /// git's sequencing, surfaced through this job's error when it declines.
    pub fn stash_pop(repo: &Handle, index: usize) -> Self {
        Self::named(format!("stash pop stash@{index}"), repo, move |r| {
            r.stash_pop(index)
        })
    }

    /// Deletes stash `index` off the stack. DESTRUCTIVE: the caller confirms
    /// before this job is ever built.
    pub fn stash_drop(repo: &Handle, index: usize) -> Self {
        Self::named(format!("stash drop stash@{index}"), repo, move |r| {
            r.stash_drop(index)
        })
    }
}

impl Job for Write {
    fn name(&self) -> &str {
        &self.name
    }

    fn run(self: Box<Self>) -> Result<(), String> {
        let this = *self;
        (this.op)(this.repo.as_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::{Event, Runner};
    use gitten_core::status::Status;
    use gitten_core::Commit;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    /// Records every verb aimed at it, answering success — the shape a fake
    /// behind a real window would need to prove what the UI asked for.
    struct Recording(Arc<Mutex<Vec<String>>>);

    impl Repo for Recording {
        fn log(&self, _: usize) -> gitten_git::Result<Vec<Commit>> {
            Ok(Vec::new())
        }
        fn pairs(&self, _: &str) -> gitten_git::Result<Vec<gitten_git::Pair>> {
            Ok(Vec::new())
        }
        fn status(&self) -> gitten_git::Result<Status> {
            Ok(Status::default())
        }
        fn describe(&self) -> String {
            "recording".into()
        }
        fn stage(&self, path: &[u8]) -> gitten_git::Result<()> {
            self.0
                .lock()
                .unwrap()
                .push(format!("stage {}", String::from_utf8_lossy(path)));
            Ok(())
        }
        fn unstage(&self, path: &[u8]) -> gitten_git::Result<()> {
            self.0
                .lock()
                .unwrap()
                .push(format!("unstage {}", String::from_utf8_lossy(path)));
            Ok(())
        }
        fn discard(&self, path: &[u8]) -> gitten_git::Result<()> {
            self.0
                .lock()
                .unwrap()
                .push(format!("discard {}", String::from_utf8_lossy(path)));
            Ok(())
        }
        fn remove_untracked(&self, path: &[u8]) -> gitten_git::Result<()> {
            self.0
                .lock()
                .unwrap()
                .push(format!("delete {}", String::from_utf8_lossy(path)));
            Ok(())
        }
        fn ignore(&self, path: &[u8]) -> gitten_git::Result<()> {
            self.0
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
            self.0.lock().unwrap().push(format!("stage-many {shown}"));
            Ok(())
        }
        fn unstage_many(&self, paths: &[&[u8]]) -> gitten_git::Result<()> {
            let shown = paths
                .iter()
                .map(|p| String::from_utf8_lossy(p).into_owned())
                .collect::<Vec<_>>()
                .join(", ");
            self.0.lock().unwrap().push(format!("unstage-many {shown}"));
            Ok(())
        }
        fn commit(&self, message: &str) -> gitten_git::Result<String> {
            self.0.lock().unwrap().push(format!("commit {message}"));
            Ok("f00d".into())
        }
        fn stash_push(&self, message: Option<&str>) -> gitten_git::Result<usize> {
            self.0
                .lock()
                .unwrap()
                .push(format!("stash push {:?}", message));
            Ok(0)
        }
        fn stash_apply(&self, index: usize) -> gitten_git::Result<()> {
            self.0
                .lock()
                .unwrap()
                .push(format!("stash apply stash@{index}"));
            Ok(())
        }
        fn stash_pop(&self, index: usize) -> gitten_git::Result<()> {
            self.0
                .lock()
                .unwrap()
                .push(format!("stash pop stash@{index}"));
            Ok(())
        }
        fn stash_drop(&self, index: usize) -> gitten_git::Result<()> {
            self.0
                .lock()
                .unwrap()
                .push(format!("stash drop stash@{index}"));
            Ok(())
        }
    }

    fn recorded(calls: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
        calls.lock().unwrap().clone()
    }

    #[test]
    fn each_verb_reaches_the_repo_through_a_job_named_for_itself() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let repo: Handle = Arc::new(Recording(Arc::clone(&calls)));
        let runner = Runner::new();
        let submit = runner.submitter();

        assert!(submit
            .submit(Box::new(Write::stage(&repo, b"a.txt".to_vec())))
            .is_ok());
        assert!(submit
            .submit(Box::new(Write::unstage(&repo, b"b c.txt".to_vec())))
            .is_ok());
        assert!(submit
            .submit(Box::new(Write::commit(&repo, "one\ntwo\n".into())))
            .is_ok());

        // FIFO, off this thread, every call landed whole.
        let deadline = Instant::now() + Duration::from_secs(2);
        while recorded(&calls).len() < 3 {
            assert!(
                Instant::now() < deadline,
                "jobs did not run: {:?}",
                recorded(&calls)
            );
            std::thread::yield_now();
        }
        assert_eq!(
            recorded(&calls),
            vec!["stage a.txt", "unstage b c.txt", "commit one\ntwo\n"]
        );

        // And the names a running band shows are the verbs', paths spelled out.
        let mut names = Vec::new();
        while let Some(event) = runner.try_next() {
            if let Event::Started { name } = event {
                names.push(name);
            }
        }
        assert_eq!(names, vec!["stage a.txt", "unstage b c.txt", "commit"]);
    }

    #[test]
    fn the_file_verbs_reach_the_trait_and_name_themselves_for_the_band() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let repo: Handle = Arc::new(Recording(Arc::clone(&calls)));
        let runner = Runner::new();
        let submit = runner.submitter();

        assert!(submit
            .submit(Box::new(Write::discard(&repo, b"src/x.rs".to_vec())))
            .is_ok());
        assert!(submit
            .submit(Box::new(Write::remove_untracked(
                &repo,
                b"notes.md".to_vec()
            )))
            .is_ok());
        assert!(submit
            .submit(Box::new(Write::ignore(&repo, b"notes.md".to_vec())))
            .is_ok());
        // Bulk: one job over many paths, named for its count — a stage-all
        // keypress reads as one thing happening, not forty.
        assert!(submit
            .submit(Box::new(Write::stage_many(
                &repo,
                vec![b"a.txt".to_vec(), b"b c.txt".to_vec()]
            )))
            .is_ok());
        assert!(submit
            .submit(Box::new(Write::unstage_many(
                &repo,
                vec![b"a.txt".to_vec()]
            )))
            .is_ok());

        let deadline = Instant::now() + Duration::from_secs(2);
        while recorded(&calls).len() < 5 {
            assert!(
                Instant::now() < deadline,
                "jobs did not run: {:?}",
                recorded(&calls)
            );
            std::thread::yield_now();
        }
        assert_eq!(
            recorded(&calls),
            vec![
                "discard src/x.rs",
                "delete notes.md",
                "ignore notes.md",
                "stage-many a.txt, b c.txt",
                "unstage-many a.txt",
            ]
        );

        let mut names = Vec::new();
        while let Some(event) = runner.try_next() {
            if let Event::Started { name } = event {
                names.push(name);
            }
        }
        assert_eq!(
            names,
            vec![
                "discard src/x.rs",
                "delete notes.md",
                "ignore notes.md",
                "stage 2 paths",
                "unstage 1 path",
            ],
            "the band names are the verbs' own words"
        );
    }

    #[test]
    fn the_stash_verbs_reach_the_trait_and_address_by_index() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let repo: Handle = Arc::new(Recording(Arc::clone(&calls)));
        let runner = Runner::new();
        let submit = runner.submitter();

        assert!(submit
            .submit(Box::new(Write::stash_push(&repo, None)))
            .is_ok());
        assert!(submit
            .submit(Box::new(Write::stash_push(
                &repo,
                Some("hand written".into())
            )))
            .is_ok());
        assert!(submit
            .submit(Box::new(Write::stash_apply(&repo, 1)))
            .is_ok());
        assert!(submit.submit(Box::new(Write::stash_pop(&repo, 2))).is_ok());
        assert!(submit.submit(Box::new(Write::stash_drop(&repo, 3))).is_ok());

        let deadline = Instant::now() + Duration::from_secs(2);
        while recorded(&calls).len() < 5 {
            assert!(
                Instant::now() < deadline,
                "jobs did not run: {:?}",
                recorded(&calls)
            );
            std::thread::yield_now();
        }
        // The index is the address, and it travels as a number — the refname
        // is derived where git is called, never stored here.
        assert_eq!(
            recorded(&calls),
            vec![
                "stash push None",
                "stash push Some(\"hand written\")",
                "stash apply stash@1",
                "stash pop stash@2",
                "stash drop stash@3",
            ]
        );

        let mut names = Vec::new();
        while let Some(event) = runner.try_next() {
            if let Event::Started { name } = event {
                names.push(name);
            }
        }
        assert_eq!(
            names,
            vec![
                "stash push",
                "stash push",
                "stash apply stash@1",
                "stash pop stash@2",
                "stash drop stash@3",
            ],
            "the band names are the verbs' own words"
        );
    }

    #[test]
    fn a_failed_write_fails_the_job_without_inventing_words() {
        struct Broken;
        impl Repo for Broken {
            fn log(&self, _: usize) -> gitten_git::Result<Vec<Commit>> {
                Ok(Vec::new())
            }
            fn pairs(&self, _: &str) -> gitten_git::Result<Vec<gitten_git::Pair>> {
                Ok(Vec::new())
            }
            fn status(&self) -> gitten_git::Result<Status> {
                Ok(Status::default())
            }
            fn describe(&self) -> String {
                "broken".into()
            }
            fn commit(&self, _: &str) -> gitten_git::Result<String> {
                Err("hook declined".into())
            }
        }
        let repo: Handle = Arc::new(Broken);
        // The error text is the repository's own, verbatim — the layer that
        // knows why a write failed is the layer that says so.
        let job: Box<dyn Job> = Box::new(Write::commit(&repo, "m".into()));
        assert_eq!(job.run(), Err("hook declined".into()));
    }
}
