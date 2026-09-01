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
use gitten_core::status::PathBytes;
use gitten_git::Handle;

/// The client services every shared action needs. Drawing and input stay in
/// the client; this is the narrow window shared policy reaches through.
pub trait Client {
    /// A refusal or a result, in the client's own furniture.
    fn say(&mut self, message: String);
    /// A destructive question standing until it is answered or dropped.
    fn ask(&mut self, question: String);
    /// The repository, absent when the client is showing a fixture.
    fn repo(&self) -> Option<Handle>;
    /// Queues the job. `false` means the queue is shutting down.
    fn submit(&mut self, job: Box<dyn Job>) -> bool;
}

/// The client-owned selection and confirmation state needed by branch actions.
pub trait BranchClient: Client {
    /// The branch row the keyboard is on. Clients refuse a command aimed at the
    /// wrong pane before entering the shared action; `None` means no selected row.
    fn branch_target(&self) -> Option<Target>;
    /// Arms this target, or spends an arm already standing on it.
    fn confirm_or_arm_branch(&mut self, target: &Target) -> bool;
}

/// Which presentation section a working-tree path occupies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileSection {
    Staged,
    Unstaged,
    Untracked,
    Conflicts,
}

/// The selected working-tree row, with its path preserved for git and already
/// prepared for user-facing questions by the client presentation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectedFile {
    pub section: FileSection,
    pub path: PathBytes,
    pub shown: String,
}

/// The client-owned reads and confirmation state needed by file actions.
pub trait FileClient: Client {
    fn selected_file(&self) -> Option<SelectedFile>;
    fn cursor_section(&self) -> Option<FileSection>;
    fn paths_in(&self, section: FileSection) -> Vec<PathBytes>;
    /// Arms this file target, or spends an identical arm already standing.
    fn confirm_or_arm_file(&mut self, target: &SelectedFile) -> bool;
}

/// `branches.delete`, for every client.
///
/// The guard order is load-bearing and is why this is shared rather than
/// described: a detached HEAD is refused before a remote is, because "not a
/// branch" is a truer thing to say than "its remote's to delete"; and the arm
/// is spent only after the repository is known to exist, so a fixture cannot
/// consume a question it can never answer.
pub fn delete_branch(client: &mut impl BranchClient) {
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
    if !client.confirm_or_arm_branch(&target) {
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
/// `files.stage`: stage the selected path, or unstage it when it is staged.
pub fn stage_or_unstage(client: &mut impl FileClient) {
    let Some(file) = client.selected_file() else {
        client.say("nothing selected to stage".into());
        return;
    };
    let Some(repo) = client.repo() else {
        client.say("a fixture has no working tree to stage in".into());
        return;
    };
    let bytes = file.path.as_bytes().to_vec();
    let job = match file.section {
        FileSection::Staged => Write::unstage(&repo, bytes),
        _ => Write::stage(&repo, bytes),
    };
    if !client.submit(Box::new(job)) {
        client.say("the job queue is shutting down".into());
    }
}

/// `files.stage-all`: act on every path on the cursor's side of the index.
pub fn stage_all(client: &mut impl FileClient) {
    let staging = client.cursor_section() != Some(FileSection::Staged);
    let mut targets = if staging {
        client.paths_in(FileSection::Unstaged)
    } else {
        client.paths_in(FileSection::Staged)
    };
    if staging {
        targets.extend(client.paths_in(FileSection::Untracked));
    }
    if targets.is_empty() {
        client.say(
            if staging {
                "nothing unstaged or untracked to stage"
            } else {
                "nothing staged to unstage"
            }
            .into(),
        );
        return;
    }
    let Some(repo) = client.repo() else {
        client.say("a fixture has no working tree to act on".into());
        return;
    };
    let bytes = targets
        .into_iter()
        .map(|path| path.as_bytes().to_vec())
        .collect();
    let job = if staging {
        Write::stage_many(&repo, bytes)
    } else {
        Write::unstage_many(&repo, bytes)
    };
    if !client.submit(Box::new(job)) {
        client.say("the job queue is shutting down".into());
    }
}

/// `files.discard`: refuse unsafe sections and ask twice for one file target.
pub fn discard_file(client: &mut impl FileClient) {
    let Some(file) = client.selected_file() else {
        client.say("nothing selected to discard".into());
        return;
    };
    match file.section {
        FileSection::Staged => {
            client.say("that change is staged — unstage it before discarding".into());
            return;
        }
        FileSection::Conflicts => {
            client.say("a conflicted file needs its merge resolved, not discarded".into());
            return;
        }
        FileSection::Untracked | FileSection::Unstaged => {}
    }
    let Some(repo) = client.repo() else {
        client.say("a fixture has no working tree to discard from".into());
        return;
    };
    if !client.confirm_or_arm_file(&file) {
        let verb = if file.section == FileSection::Untracked {
            "delete"
        } else {
            "discard"
        };
        client.ask(format!("{verb} {}? press again to confirm", file.shown));
        return;
    }
    let bytes = file.path.as_bytes().to_vec();
    let job = if file.section == FileSection::Untracked {
        Write::remove_untracked(&repo, bytes)
    } else {
        Write::discard(&repo, bytes)
    };
    if !client.submit(Box::new(job)) {
        client.say("the job queue is shutting down".into());
    }
}

/// `files.ignore`: append one selected untracked path to `.gitignore`.
pub fn ignore_file(client: &mut impl FileClient) {
    let Some(file) = client.selected_file() else {
        client.say("only an untracked file can be ignored".into());
        return;
    };
    if file.section != FileSection::Untracked {
        client.say("only an untracked file can be ignored".into());
        return;
    }
    let Some(repo) = client.repo() else {
        client.say("a fixture has no repository to ignore in".into());
        return;
    };
    let job = Write::ignore(&repo, file.path.as_bytes().to_vec());
    if !client.submit(Box::new(job)) {
        client.say("the job queue is shutting down".into());
    }
}

/// `files.stash`: park the tracked working tree with git's default message.
pub fn stash_working_tree(client: &mut impl Client) {
    let Some(repo) = client.repo() else {
        client.say("a fixture has no working tree to park".into());
        return;
    };
    if !client.submit(Box::new(Write::stash_push(&repo, None))) {
        client.say("the job queue is shutting down".into());
    }
}

/// Turn accepted commit text into its write job.
pub fn commit_message(client: &mut impl Client, message: String) {
    if message.trim().is_empty() {
        client.say("a commit needs a message".into());
        return;
    }
    let Some(repo) = client.repo() else {
        client.say("a fixture has no repository to commit in".into());
        return;
    };
    if !client.submit(Box::new(Write::commit(&repo, message))) {
        client.say("the job queue is shutting down".into());
    }
}

/// Turn accepted amend text into its write job.
pub fn amend_message(client: &mut impl Client, message: String) {
    if message.trim().is_empty() {
        client.say("a commit needs a message".into());
        return;
    }
    let Some(repo) = client.repo() else {
        client.say("a fixture has no repository to amend in".into());
        return;
    };
    if !client.submit(Box::new(Write::amend(&repo, message))) {
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
    use std::sync::{mpsc, Arc};

    struct EmptyRepo(mpsc::Sender<String>);

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

        fn commit(&self, message: &str) -> gitten_git::Result<String> {
            self.0.send(format!("commit:{message}")).unwrap();
            Ok("commit".into())
        }
        fn amend(&self, message: &str) -> gitten_git::Result<String> {
            self.0.send(format!("amend:{message}")).unwrap();
            Ok("amend".into())
        }

        fn describe(&self) -> String {
            "empty".into()
        }
    }

    struct Fake {
        target: Option<Target>,
        selected: Option<SelectedFile>,
        cursor: Option<FileSection>,
        paths: [Vec<PathBytes>; 4],
        repo: Option<Handle>,
        records: mpsc::Receiver<String>,
        confirmations: VecDeque<bool>,
        confirm_calls: usize,
        said: Vec<String>,
        asked: Vec<String>,
        events: Vec<String>,
        jobs: Vec<String>,
        submit_ok: bool,
        run_jobs: bool,
    }

    impl Fake {
        fn with(target: Option<Target>) -> Self {
            let (record, records) = mpsc::channel();
            Self {
                target,
                selected: None,
                cursor: None,
                paths: Default::default(),
                repo: Some(Arc::new(EmptyRepo(record))),
                records,
                confirmations: VecDeque::new(),
                confirm_calls: 0,
                said: Vec::new(),
                asked: Vec::new(),
                events: Vec::new(),
                jobs: Vec::new(),
                submit_ok: true,
                run_jobs: false,
            }
        }

        fn file(section: FileSection, path: &[u8], shown: &str) -> SelectedFile {
            SelectedFile {
                section,
                path: PathBytes::from_bytes(path),
                shown: shown.into(),
            }
        }

        fn path_index(section: FileSection) -> usize {
            match section {
                FileSection::Staged => 0,
                FileSection::Unstaged => 1,
                FileSection::Untracked => 2,
                FileSection::Conflicts => 3,
            }
        }

        fn set_paths(&mut self, section: FileSection, paths: &[&[u8]]) {
            self.paths[Self::path_index(section)] = paths
                .iter()
                .map(|path| PathBytes::from_bytes(path))
                .collect();
        }
    }

    impl Client for Fake {
        fn say(&mut self, message: String) {
            self.events.push(format!("say:{message}"));
            self.said.push(message);
        }

        fn ask(&mut self, question: String) {
            self.events.push(format!("ask:{question}"));
            self.asked.push(question);
        }

        fn repo(&self) -> Option<Handle> {
            self.repo.clone()
        }

        fn submit(&mut self, job: Box<dyn Job>) -> bool {
            self.events.push(format!("submit:{}", job.name()));
            self.jobs.push(job.name().to_string());
            if self.run_jobs {
                job.run().unwrap();
            }
            self.submit_ok
        }
    }

    impl BranchClient for Fake {
        fn branch_target(&self) -> Option<Target> {
            self.target.clone()
        }

        fn confirm_or_arm_branch(&mut self, _: &Target) -> bool {
            self.confirm_calls += 1;
            self.confirmations.pop_front().unwrap_or(false)
        }
    }

    impl FileClient for Fake {
        fn selected_file(&self) -> Option<SelectedFile> {
            self.selected.clone()
        }

        fn cursor_section(&self) -> Option<FileSection> {
            self.cursor
        }

        fn paths_in(&self, section: FileSection) -> Vec<PathBytes> {
            self.paths[Self::path_index(section)].clone()
        }

        fn confirm_or_arm_file(&mut self, _: &SelectedFile) -> bool {
            self.confirm_calls += 1;
            self.confirmations.pop_front().unwrap_or(false)
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

    #[test]
    fn stage_selected_chooses_the_job_from_its_section() {
        let mut none = Fake::with(None);
        stage_or_unstage(&mut none);
        assert_eq!(none.said, ["nothing selected to stage"]);
        assert!(none.jobs.is_empty());

        for (section, expected) in [
            (FileSection::Staged, "unstage file"),
            (FileSection::Unstaged, "stage file"),
            (FileSection::Untracked, "stage file"),
            (FileSection::Conflicts, "stage file"),
        ] {
            let mut client = Fake::with(None);
            client.selected = Some(Fake::file(section, b"file", "file"));
            stage_or_unstage(&mut client);
            assert_eq!(client.jobs, [expected]);
            assert!(client.said.is_empty());
        }
    }

    #[test]
    fn stage_all_chooses_cursor_side_targets_and_refuses_empty_sets() {
        let mut staging = Fake::with(None);
        staging.cursor = Some(FileSection::Unstaged);
        staging.set_paths(FileSection::Unstaged, &[b"modified"]);
        staging.set_paths(FileSection::Untracked, &[b"new"]);
        staging.set_paths(FileSection::Conflicts, &[b"conflict"]);
        stage_all(&mut staging);
        assert_eq!(staging.jobs, ["stage 2 paths"]);

        let mut unstaging = Fake::with(None);
        unstaging.cursor = Some(FileSection::Staged);
        unstaging.set_paths(FileSection::Staged, &[b"one", b"two"]);
        unstaging.set_paths(FileSection::Unstaged, &[b"ignored"]);
        stage_all(&mut unstaging);
        assert_eq!(unstaging.jobs, ["unstage 2 paths"]);

        let mut no_stage = Fake::with(None);
        stage_all(&mut no_stage);
        assert_eq!(no_stage.said, ["nothing unstaged or untracked to stage"]);
        assert!(no_stage.jobs.is_empty());

        let mut no_unstage = Fake::with(None);
        no_unstage.cursor = Some(FileSection::Staged);
        stage_all(&mut no_unstage);
        assert_eq!(no_unstage.said, ["nothing staged to unstage"]);
        assert!(no_unstage.jobs.is_empty());
    }

    #[test]
    fn discard_refuses_before_confirmation_in_the_existing_order() {
        let mut none = Fake::with(None);
        discard_file(&mut none);
        assert_eq!(none.events, ["say:nothing selected to discard"]);
        assert_eq!(none.confirm_calls, 0);
        assert!(none.jobs.is_empty());

        for (section, refusal) in [
            (
                FileSection::Staged,
                "that change is staged — unstage it before discarding",
            ),
            (
                FileSection::Conflicts,
                "a conflicted file needs its merge resolved, not discarded",
            ),
        ] {
            let mut client = Fake::with(None);
            client.selected = Some(Fake::file(section, b"file", "file"));
            discard_file(&mut client);
            assert_eq!(client.said, [refusal]);
            assert_eq!(client.confirm_calls, 0);
            assert!(client.jobs.is_empty());
        }
    }

    #[test]
    fn discard_asks_then_uses_discard_or_delete_for_the_same_target() {
        for (section, question, job) in [
            (
                FileSection::Unstaged,
                "discard shown path? press again to confirm",
                "discard raw-path",
            ),
            (
                FileSection::Untracked,
                "delete shown path? press again to confirm",
                "delete raw-path",
            ),
        ] {
            let mut client = Fake::with(None);
            client.selected = Some(Fake::file(section, b"raw-path", "shown path"));
            client.confirmations = [false, true].into();
            discard_file(&mut client);
            assert_eq!(client.asked, [question]);
            assert!(client.jobs.is_empty());
            discard_file(&mut client);
            assert_eq!(
                client.events,
                [format!("ask:{question}"), format!("submit:{job}")]
            );
            assert_eq!(client.jobs, [job]);
        }
    }

    #[test]
    fn ignore_accepts_only_an_untracked_selection() {
        for selected in [
            None,
            Some(Fake::file(FileSection::Staged, b"tracked", "tracked")),
        ] {
            let mut client = Fake::with(None);
            client.selected = selected;
            ignore_file(&mut client);
            assert_eq!(client.said, ["only an untracked file can be ignored"]);
            assert!(client.jobs.is_empty());
        }

        let mut client = Fake::with(None);
        client.selected = Some(Fake::file(FileSection::Untracked, b"new", "new"));
        ignore_file(&mut client);
        assert_eq!(client.jobs, ["ignore new"]);
    }

    #[test]
    fn stash_commit_and_amend_build_the_named_jobs() {
        let mut stash = Fake::with(None);
        stash_working_tree(&mut stash);
        assert_eq!(stash.jobs, ["stash push"]);

        let mut commit = Fake::with(None);
        commit_message(&mut commit, "message".into());
        assert_eq!(commit.jobs, ["commit"]);

        let mut amend = Fake::with(None);
        amend_message(&mut amend, "message".into());
        assert_eq!(amend.jobs, ["amend"]);
    }

    #[test]
    fn commit_and_amend_reject_empty_text_but_preserve_nonempty_text() {
        for message in ["", " \t\n "] {
            let mut commit = Fake::with(None);
            commit_message(&mut commit, message.into());
            assert_eq!(commit.said, ["a commit needs a message"]);
            assert!(commit.jobs.is_empty());

            let mut amend = Fake::with(None);
            amend_message(&mut amend, message.into());
            assert_eq!(amend.said, ["a commit needs a message"]);
            assert!(amend.jobs.is_empty());
        }

        let mut client = Fake::with(None);
        client.run_jobs = true;
        commit_message(&mut client, "  keep commit space  ".into());
        amend_message(&mut client, "  keep amend space  ".into());
        assert_eq!(
            client.records.try_iter().collect::<Vec<_>>(),
            ["commit:  keep commit space  ", "amend:  keep amend space  "]
        );
    }

    #[test]
    fn every_repository_action_preserves_its_fixture_refusal() {
        let mut stage = Fake::with(None);
        stage.selected = Some(Fake::file(FileSection::Unstaged, b"file", "file"));
        stage.repo = None;
        stage_or_unstage(&mut stage);
        assert_eq!(stage.said, ["a fixture has no working tree to stage in"]);

        let mut all = Fake::with(None);
        all.set_paths(FileSection::Unstaged, &[b"file"]);
        all.repo = None;
        stage_all(&mut all);
        assert_eq!(all.said, ["a fixture has no working tree to act on"]);

        let mut discard = Fake::with(None);
        discard.selected = Some(Fake::file(FileSection::Unstaged, b"file", "file"));
        discard.confirmations.push_back(true);
        discard.repo = None;
        discard_file(&mut discard);
        assert_eq!(
            discard.said,
            ["a fixture has no working tree to discard from"]
        );
        assert_eq!(discard.confirm_calls, 0);

        let mut ignore = Fake::with(None);
        ignore.selected = Some(Fake::file(FileSection::Untracked, b"file", "file"));
        ignore.repo = None;
        ignore_file(&mut ignore);
        assert_eq!(ignore.said, ["a fixture has no repository to ignore in"]);

        let mut stash = Fake::with(None);
        stash.repo = None;
        stash_working_tree(&mut stash);
        assert_eq!(stash.said, ["a fixture has no working tree to park"]);

        let mut commit = Fake::with(None);
        commit.repo = None;
        commit_message(&mut commit, "message".into());
        assert_eq!(commit.said, ["a fixture has no repository to commit in"]);

        let mut amend = Fake::with(None);
        amend.repo = None;
        amend_message(&mut amend, "message".into());
        assert_eq!(amend.said, ["a fixture has no repository to amend in"]);
    }

    #[test]
    fn every_working_tree_action_reports_queue_rejection() {
        fn assert_shutdown(client: &Fake) {
            assert_eq!(client.said, ["the job queue is shutting down"]);
            assert_eq!(
                client.events.last().unwrap(),
                "say:the job queue is shutting down"
            );
            assert_eq!(client.jobs.len(), 1);
        }

        let mut stage = Fake::with(None);
        stage.selected = Some(Fake::file(FileSection::Unstaged, b"file", "file"));
        stage.submit_ok = false;
        stage_or_unstage(&mut stage);
        assert_shutdown(&stage);

        let mut all = Fake::with(None);
        all.set_paths(FileSection::Unstaged, &[b"file"]);
        all.submit_ok = false;
        stage_all(&mut all);
        assert_shutdown(&all);

        let mut discard = Fake::with(None);
        discard.selected = Some(Fake::file(FileSection::Unstaged, b"file", "file"));
        discard.confirmations.push_back(true);
        discard.submit_ok = false;
        discard_file(&mut discard);
        assert_shutdown(&discard);

        let mut ignore = Fake::with(None);
        ignore.selected = Some(Fake::file(FileSection::Untracked, b"file", "file"));
        ignore.submit_ok = false;
        ignore_file(&mut ignore);
        assert_shutdown(&ignore);

        let mut stash = Fake::with(None);
        stash.submit_ok = false;
        stash_working_tree(&mut stash);
        assert_shutdown(&stash);

        let mut commit = Fake::with(None);
        commit.submit_ok = false;
        commit_message(&mut commit, "message".into());
        assert_shutdown(&commit);

        let mut amend = Fake::with(None);
        amend.submit_ok = false;
        amend_message(&mut amend, "message".into());
        assert_shutdown(&amend);
    }
}
