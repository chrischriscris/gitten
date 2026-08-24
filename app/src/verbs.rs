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
/// Built through [`Write::stage`], [`Write::unstage`] or [`Write::commit`];
/// anything else is the constructor with a closure of your own, which is what
/// keeps this from being a list of built-ins wearing a struct.
pub struct Write {
    name: String,
    repo: Handle,
    op: Op,
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
        fn commit(&self, message: &str) -> gitten_git::Result<String> {
            self.0.lock().unwrap().push(format!("commit {message}"));
            Ok("f00d".into())
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
