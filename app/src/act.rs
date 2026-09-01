//! What a verb *decides*, once, for every client.
//!
//! `verbs.rs` holds what a verb *does* — a `Write` over the repository handle.
//! This holds the part above it that was being written twice: which guards run
//! in which order, the words a refusal uses, and when a destructive question is
//! asked rather than answered. A client supplies the things it genuinely owns —
//! what is selected, how a sentence reaches the reader, and how a job is queued —
//! and nothing else about it is visible here.

use crate::jobs::Job;
use crate::verbs::Write;
use gitten_core::refs::Target;
use gitten_git::Handle;

/// The client's side of a verb. Drawing and input stay in the client; this is
/// the narrow window a shared verb reaches them through.
pub trait Acts {
    /// The branch row the keyboard is on. Clients refuse a command aimed at the
    /// wrong pane before entering the shared verb; `None` means no selected row.
    fn branch_target(&self) -> Option<Target>;
    /// A refusal or a result, in the client's own furniture.
    fn say(&mut self, message: String);
    /// A destructive question standing until it is answered or dropped.
    fn ask(&mut self, question: String);
    /// Arms this target, or spends an arm already standing on it. `false` means
    /// the question was just asked and nothing has happened yet.
    fn confirm_or_arm(&mut self, target: &Target) -> bool;
    /// The repository, absent when the client is showing a fixture.
    fn repo(&self) -> Option<Handle>;
    /// Queues the job. `false` means the queue is shutting down.
    fn submit(&mut self, job: Box<dyn Job>) -> bool;
}

/// `branches.delete`, for every client.
///
/// The guard order is load-bearing and is why this is shared rather than
/// described: a detached HEAD is refused before a remote is, because "not a
/// branch" is a truer thing to say than "its remote's to delete"; and the arm
/// is spent only after the repository is known to exist, so a fixture cannot
/// consume a question it can never answer.
pub fn delete_branch(client: &mut impl Acts) {
    let Some(target) = client.branch_target() else {
        client.say("nothing selected to delete".into());
        return;
    };
    let shown = match &target {
        Target::Local(name) => name.to_string_lossy().into_owned(),
        Target::Remote { remote, branch } => {
            format!("{}/{}", remote.to_string_lossy(), branch.to_string_lossy())
        }
        Target::Detached => {
            client.say("a detached HEAD is not a branch".into());
            return;
        }
    };
    if matches!(target, Target::Remote { .. }) {
        client.say("a remote branch is its remote's to delete — fetch prunes it here".into());
        return;
    }
    let Some(repo) = client.repo() else {
        client.say("a fixture has no repository to delete branches from".into());
        return;
    };
    if !client.confirm_or_arm(&target) {
        client.ask(format!("delete branch {shown}? press again to confirm"));
        return;
    }
    let Target::Local(name) = target else {
        unreachable!("remotes and detached refuse above");
    };
    let job = Write::delete_branch(&repo, name.as_bytes().to_vec(), false);
    if !client.submit(Box::new(job)) {
        client.say("the job queue is shutting down".into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gitten_core::refs::RefName;
    use gitten_core::status::Status;
    use gitten_core::Commit;
    use gitten_git::{Pair, Repo};
    use std::collections::VecDeque;
    use std::sync::Arc;

    struct EmptyRepo;

    impl Repo for EmptyRepo {
        fn log(&self, _: usize) -> gitten_git::Result<Vec<Commit>> {
            Ok(Vec::new())
        }

        fn pairs(&self, _: &str) -> gitten_git::Result<Vec<Pair>> {
            Ok(Vec::new())
        }

        fn status(&self) -> gitten_git::Result<Status> {
            Ok(Status::default())
        }

        fn describe(&self) -> String {
            "empty".into()
        }
    }

    struct Fake {
        target: Option<Target>,
        repo: Option<Handle>,
        confirmations: VecDeque<bool>,
        confirm_calls: usize,
        said: Vec<String>,
        asked: Vec<String>,
        jobs: Vec<String>,
    }

    impl Fake {
        fn with(target: Option<Target>) -> Self {
            Self {
                target,
                repo: Some(Arc::new(EmptyRepo)),
                confirmations: VecDeque::new(),
                confirm_calls: 0,
                said: Vec::new(),
                asked: Vec::new(),
                jobs: Vec::new(),
            }
        }
    }

    impl Acts for Fake {
        fn branch_target(&self) -> Option<Target> {
            self.target.clone()
        }

        fn say(&mut self, message: String) {
            self.said.push(message);
        }

        fn ask(&mut self, question: String) {
            self.asked.push(question);
        }

        fn confirm_or_arm(&mut self, _: &Target) -> bool {
            self.confirm_calls += 1;
            self.confirmations.pop_front().unwrap_or(false)
        }

        fn repo(&self) -> Option<Handle> {
            self.repo.clone()
        }

        fn submit(&mut self, job: Box<dyn Job>) -> bool {
            self.jobs.push(job.name().to_string());
            true
        }
    }

    #[test]
    fn deleting_with_nothing_selected_refuses_in_words() {
        let mut client = Fake::with(None);
        delete_branch(&mut client);
        assert_eq!(client.said, ["nothing selected to delete"]);
        assert!(client.jobs.is_empty());
    }

    #[test]
    fn a_detached_head_is_refused_before_a_remote_is() {
        let mut client = Fake::with(Some(Target::Detached));
        delete_branch(&mut client);
        assert_eq!(client.said, ["a detached HEAD is not a branch"]);
        assert!(client.jobs.is_empty());
    }

    #[test]
    fn a_remote_branch_is_refused() {
        let mut client = Fake::with(Some(Target::Remote {
            remote: RefName::from("origin"),
            branch: RefName::from("main"),
        }));
        delete_branch(&mut client);
        assert_eq!(
            client.said,
            ["a remote branch is its remote's to delete — fetch prunes it here"]
        );
        assert!(client.jobs.is_empty());
    }

    #[test]
    fn the_first_press_asks_and_the_second_deletes() {
        let mut client = Fake::with(Some(Target::Local(RefName::from("feature"))));
        client.confirmations = [false, true].into();
        delete_branch(&mut client);
        assert_eq!(
            client.asked,
            ["delete branch feature? press again to confirm"]
        );
        assert!(client.jobs.is_empty());
        delete_branch(&mut client);
        assert_eq!(client.asked.len(), 1);
        assert_eq!(client.jobs, ["delete branch feature"]);
    }

    #[test]
    fn a_fixture_refuses_before_it_spends_the_arm() {
        let mut client = Fake::with(Some(Target::Local(RefName::from("feature"))));
        client.repo = None;
        client.confirmations.push_back(true);
        delete_branch(&mut client);
        assert_eq!(
            client.said,
            ["a fixture has no repository to delete branches from"]
        );
        assert_eq!(client.confirm_calls, 0);
        assert!(client.jobs.is_empty());
    }
}
