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
use gitten_core::clipboard::CherryClipboard;
use gitten_core::operation::{Operation, Side};
use gitten_core::rebase::{compose, fixup_marks, Amend, FixupKind, Plan, Rewrite};
use gitten_core::refs::{
    redo_selector, undo_for, HeadState, RefName, ReflogEntry, ResetMode, StashId, StashScope,
    Target, UndoKind, REDO_MESSAGE, UNDO_MESSAGE,
};
use gitten_core::status::PathBytes;
use gitten_core::{Commit, Hunk};
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

    /// The operation standing in the repository, as the client last
    /// acquired it — the lifecycle verbs gate on this instead of asking
    /// the repository again. The honest default for a client that tracks
    /// no operations: none.
    fn operation(&self) -> Option<Operation> {
        None
    }

    /// The bisection standing in the repository, as the client last
    /// acquired it — read through the repository on open and on every
    /// refresh, the way the operation above is. The honest default for a
    /// client that tracks none: none.
    fn bisect(&self) -> Option<gitten_core::bisect::BisectState> {
        None
    }

    /// The conflicted path the keyboard is on, when there is one. The
    /// honest default for a client with no conflict selection: none.
    fn selected_conflict(&self) -> Option<PathBytes> {
        None
    }

    /// Arms a destructive question that no *pane row* names — a rebase onto
    /// a branch, a reset toward the upstream, a nuke — or spends the arm
    /// already standing on the same pair.
    ///
    /// Keyed on the command *and* the raw bytes the write is aimed at, for
    /// the reason every arm here is: a soft reset must never spend a hard
    /// one's question, and a rebase onto one branch must never spend the
    /// question asked about another. `target` is empty where the question
    /// is about the repository itself, which is a target too — there is
    /// exactly one working tree to nuke.
    ///
    /// The default never confirms, which is the honest answer for a client
    /// that keeps no such arm: the question stands, the write never runs,
    /// and nothing is destroyed by a client that cannot ask twice. A client
    /// that binds these commands implements it; one that does not never
    /// reaches here at all.
    fn confirm_or_arm(&mut self, _command: &str, _target: &[u8]) -> bool {
        false
    }

    /// The commit marked as a rebase base, when one is. Read from the base
    /// trait because the mark is *made* in a history pane and *spent* in a
    /// branch one, and neither should have to know about the other's
    /// selection. The honest default for a client that marks nothing: none.
    fn rebase_base(&self) -> Option<SelectedCommit> {
        None
    }
}

/// The client-owned selection and confirmation state needed by branch actions.
pub trait BranchClient: Client {
    /// The branch row the keyboard is on. Clients refuse a command aimed at the
    /// wrong pane before entering the shared action; `None` means no selected row.
    fn branch_target(&self) -> Option<Target>;
    /// Arms this target, or spends an arm already standing on it.
    fn confirm_or_arm_branch(&mut self, target: &Target) -> bool;
}

/// The client-owned selection and confirmation state needed by remote actions.
pub trait RemoteClient: Client {
    /// The remote row the keyboard is on, by the name verbs address it with.
    fn remote_target(&self) -> Option<gitten_core::refs::RefName>;
    /// Arms this remote target, or spends an arm already standing on it.
    fn confirm_or_arm_remote(&mut self, name: &gitten_core::refs::RefName) -> bool;
}

/// The client-owned selection and confirmation state needed by stash actions.
pub trait StashClient: Client {
    /// The stack entry the keyboard is on, as both its identities — the
    /// commit that survives churn and the position it was at. `None` on an
    /// empty stack, and on one whose read failed: a pane that could not read
    /// the stack exposes no row to act on, which is a different thing from
    /// a stack read as empty.
    fn selected_stash(&self) -> Option<StashId>;
    /// Arms this entry, or spends an arm already standing on it. Keyed on
    /// the identity and not the number, for the reason the identity exists:
    /// a yes addressed to `stash@{1}` must not be spent on whatever
    /// `stash@{1}` became.
    fn confirm_or_arm_stash(&mut self, id: &StashId) -> bool;
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
/// described: a detached HEAD is refused first, because "not a branch" is
/// the truest thing to say; a remote-tracking row routes to the remote
/// deletion below instead — one key, and the row decides which half of
/// branch deletion it means. The arm is spent only after the repository is
/// known to exist, so a fixture cannot consume a question it can never
/// answer.
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
        delete_remote_branch(client);
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

/// `files.stash-menu`: the question, not the write. The choices stay the
/// client's own mode; this is the sentence, and the one refusal a fixture
/// earns before a menu of verbs it cannot run goes up.
pub fn stash_menu(client: &mut impl Client) -> bool {
    if client.repo().is_none() {
        client.say("a fixture has no working tree to park".into());
        return false;
    }
    client.ask(
        "park what? m names a message, s the staged side, u the unstaged side, \
         U the new files too, f this file alone"
            .into(),
    );
    true
}

/// The scoped pushes: `files.stash-named`, `-staged`, `-unstaged`,
/// `-untracked` and `-file`, all of them one [`StashScope`] apart.
///
/// Nothing confirms. A stash is the *reversible* half of the working tree's
/// verbs — the work is on the stack a keypress later, which is the whole
/// difference between this and `files.discard` — so the question a discard
/// asks would be a question about nothing here. What is refused is the two
/// things that would fail anyway: a fixture, which has no working tree; and
/// nothing at all to park, which the repository answers with a sentence
/// naming the scope rather than a success badge over a no-op.
///
/// A standing operation is deliberately *not* refused, on exactly the terms
/// [`stash_working_tree`] does not refuse it either — the whole family has
/// to answer the same way, and there is nothing left for a guard here to
/// catch. The one mid-operation state with work to park is a conflicted one,
/// and git refuses that itself ("needs merge") because it cannot write an
/// index holding unmerged stages. A rebase stopped at an `edit` has a clean
/// tree until the reader changes something, and parking what they changed is
/// then the thing they asked for rather than an accident.
pub fn stash_scoped(client: &mut impl Client, message: Option<String>, scope: StashScope) {
    let Some(repo) = client.repo() else {
        client.say("a fixture has no working tree to park".into());
        return;
    };
    let job = Write::stash_push_scoped(&repo, message, scope);
    if !client.submit(Box::new(job)) {
        client.say("the job queue is shutting down".into());
    }
}

/// `files.stash-named`'s accepted field: the message, then the tracked push
/// under it.
///
/// A blank field is refused rather than quietly becoming git's own `WIP on
/// …` — the reader pressed the key that *names* a stash, and the plain
/// stash key is the one that does not, which the refusal says so the second
/// attempt is the right one. The text travels whole otherwise, padding
/// included: a message is free text the way a commit message is, and
/// trimming it would be this module editing somebody's words.
pub fn stash_named(client: &mut impl Client, message: String) {
    if message.trim().is_empty() {
        client.say("a stash needs a message — the plain stash key parks without one".into());
        return;
    }
    stash_scoped(client, Some(message), StashScope::Tracked);
}

/// `stashes.apply`: restore the selected entry, keeping it on the stack.
///
/// Aimed by the entry's commit, so the write survives a stack that churned
/// between the keypress and the queue's turn — and refuses honestly when the
/// entry left it entirely. Nothing confirms: an apply adds work to the
/// working tree and takes nothing away, and a conflicted one is git's
/// refusal with everything still standing.
pub fn apply_stash(client: &mut impl StashClient) {
    let Some((repo, id)) = stash_target(client, "apply") else {
        return;
    };
    submit_stash(client, Write::stash_apply_entry(&repo, id));
}

/// `stashes.pop`: restore the selected entry and drop it — but only if the
/// restore was clean, which git decides and nothing here second-guesses.
///
/// No confirmation, and that is deliberate rather than an omission: a pop
/// whose apply fails keeps the entry, so the destructive half never happens
/// without the constructive one. See [`Repo::stash_pop`](gitten_git::Repo::stash_pop).
pub fn pop_stash(client: &mut impl StashClient) {
    let Some((repo, id)) = stash_target(client, "pop") else {
        return;
    };
    submit_stash(client, Write::stash_pop_entry(&repo, id));
}

/// `stashes.drop`: delete the selected entry off the stack.
///
/// The one destructive verb on this pane, so it asks twice — armed on the
/// entry's *identity*, because a yes addressed to `stash@{1}` must never be
/// spent on whatever `stash@{1}` became. The arm is spent only after the
/// repository is known to exist, so a fixture cannot consume a question it
/// can never answer.
pub fn drop_stash(client: &mut impl StashClient) {
    let Some((repo, id)) = stash_target(client, "drop") else {
        return;
    };
    if !client.confirm_or_arm_stash(&id) {
        client.ask(format!(
            "drop stash@{{{}}}? press again to confirm",
            id.index
        ));
        return;
    }
    submit_stash(client, Write::stash_drop_entry(&repo, id));
}

/// `stashes.rename`: give the selected entry a new message.
///
/// The text arrives already gathered by the client's own field. Empty is
/// refused here rather than sent to git, so the sentence names the field
/// that just closed; the repository refuses whitespace again for the callers
/// that never came through here.
pub fn rename_stash(client: &mut impl StashClient, id: StashId, message: String) {
    if message.trim().is_empty() {
        client.say("a stash needs a message".into());
        return;
    }
    let Some(repo) = client.repo() else {
        client.say("a fixture has no stash to rename".into());
        return;
    };
    submit_stash(client, Write::stash_rename(&repo, id, message));
}

/// `stashes.new-branch`: start a branch where the selected entry was made,
/// and apply the entry onto it.
///
/// A standing operation refuses: this checks out, and a checkout inside
/// git's own first write is never the move. The name is the client's field
/// again, empty refused here; a dirty tree the checkout would overwrite is
/// git's refusal, and it leaves the stash exactly where it is.
pub fn branch_from_stash(client: &mut impl StashClient, id: StashId, name: String) {
    if name.trim().is_empty() {
        client.say("a branch needs a name".into());
        return;
    }
    let Some(repo) = client.repo() else {
        client.say("a fixture has no stash to branch from".into());
        return;
    };
    if refuse_while_operating(client) {
        return;
    }
    submit_stash(
        client,
        Write::stash_branch(&repo, id, name.trim().as_bytes().to_vec()),
    );
}

/// The two things every stash verb needs and the two ways it can be refused
/// before one is built: a repository, and a row to aim at.
///
/// The order is the same one this module keeps everywhere — the repository
/// first, because "a fixture has no stack" is truer than "nothing selected"
/// about a pane that has no rows because it has no repository.
fn stash_target(client: &mut impl StashClient, verb: &str) -> Option<(Handle, StashId)> {
    let Some(repo) = client.repo() else {
        client.say(format!("a fixture has no stash to {verb}"));
        return None;
    };
    let Some(id) = client.selected_stash() else {
        client.say("nothing selected on the stash stack".into());
        return None;
    };
    Some((repo, id))
}

/// One submission, one sentence when the queue is gone — said once here
/// rather than five times above.
fn submit_stash(client: &mut impl StashClient, job: Write) {
    if !client.submit(Box::new(job)) {
        client.say("the job queue is shutting down".into());
    }
}

// ------------------------------------------------------------------ sync

/// `repo.push`: send the current branch to its remote.
///
/// The remote choice is [`Write::push_current`]'s — the upstream's remote
/// when there is one, `origin` or the sole remote when the branch tracks
/// nothing — and every refusal it names is surfaced here, once, where an
/// extension asking the same command reads the same sentence.
pub fn sync_push(client: &mut impl Client) {
    let Some(repo) = client.repo() else {
        client.say("a fixture has no repository to push from".into());
        return;
    };
    match Write::push_current(&repo) {
        Ok(job) => {
            if !client.submit(Box::new(job)) {
                client.say("the job queue is shutting down".into());
            }
        }
        Err(reason) => client.say(reason),
    }
}

/// `repo.pull`: fast-forward the current branch onto its upstream.
pub fn sync_pull(client: &mut impl Client) {
    let Some(repo) = client.repo() else {
        client.say("a fixture has no repository to pull into".into());
        return;
    };
    if !client.submit(Box::new(Write::pull(&repo))) {
        client.say("the job queue is shutting down".into());
    }
}

/// `repo.fetch`: update every remote-tracking branch.
pub fn sync_fetch(client: &mut impl Client) {
    let Some(repo) = client.repo() else {
        client.say("a fixture has no repository to fetch into".into());
        return;
    };
    if !client.submit(Box::new(Write::fetch(&repo, None))) {
        client.say("the job queue is shutting down".into());
    }
}

// ------------------------------------------------------------- operations

/// One index, one sequencer: git refuses a second start, and the words it
/// would answer with name a state rather than the way out. The shared
/// pre-check names both instead — the same sentence
/// [`Write::merge`](crate::verbs::Write::merge) arrives at if it ran first.
fn refuse_while_operating(client: &mut impl Client) -> bool {
    if let Some(operation) = client.operation() {
        client.say(format!(
            "a {} is in progress; finish or abort it before starting another",
            operation.kind.word()
        ));
        return true;
    }
    false
}

/// `branches.merge` / `branches.merge-squash`: merge the selected branch
/// into the branch the reader is on. Nothing confirms — a merge grows
/// history and nothing existing moves, and a conflicted one stops with its
/// question standing rather than guessing.
pub fn merge_selected(client: &mut impl Client, target: Vec<u8>, squash: bool) {
    let Some(repo) = client.repo() else {
        client.say("a fixture has no repository to merge into".into());
        return;
    };
    if refuse_while_operating(client) {
        return;
    }
    if !client.submit(Box::new(Write::merge(&repo, target, squash))) {
        client.say("the job queue is shutting down".into());
    }
}

/// Every lifecycle verb, one door: `operation.abort` / `.continue` /
/// `.skip` act on whichever write stands, and the per-kind names already
/// bound (`rebase.abort`, `commits.cherry-pick-abort`, …) reach the same
/// place and are checked against the same standing operation. Availability
/// gates these with the live operation state, so a mismatched press is
/// refused before this runs; this arm is the backstop that says so anyway.
pub fn operation_verb(client: &mut impl Client, command: &str) {
    let Some(repo) = client.repo() else {
        client.say("a fixture has no repository to operate on".into());
        return;
    };
    use gitten_core::operation::Kind;
    let Some(operation) = client.operation() else {
        client.say(format!(
            "{command} needs a merge, rebase, cherry-pick or revert in progress"
        ));
        return;
    };
    let job = match (command, operation.kind) {
        ("operation.abort" | "rebase.abort", Kind::Rebase) => Write::rebase_abort(&repo),
        ("operation.continue" | "rebase.continue", Kind::Rebase) => Write::rebase_continue(&repo),
        ("operation.skip", Kind::Rebase) => Write::rebase_skip(&repo),
        ("operation.abort" | "commits.cherry-pick-abort", Kind::CherryPick) => {
            Write::cherry_pick_abort(&repo)
        }
        ("operation.continue" | "commits.cherry-pick-continue", Kind::CherryPick) => {
            Write::cherry_pick_continue(&repo)
        }
        ("operation.abort", Kind::Merge) => Write::merge_abort(&repo),
        ("operation.continue", Kind::Merge) => Write::merge_continue(&repo),
        ("operation.abort", Kind::Revert) => Write::revert_abort(&repo),
        ("operation.continue", Kind::Revert) => Write::revert_continue(&repo),
        _ => {
            client.say(format!(
                "{command} is for a different operation; a {} is in progress",
                operation.kind.word()
            ));
            return;
        }
    };
    if !client.submit(Box::new(job)) {
        client.say("the job queue is shutting down".into());
    }
}

// --------------------------------------------------------------------- history

/// The commit the keyboard is on, as history verbs aim at it: the raw sha
/// for git, a short display form for the questions a human answers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectedCommit {
    pub sha: Vec<u8>,
    pub short: String,
}

/// The client-owned selection and confirmation state needed by history actions.
pub trait HistoryClient: Client {
    /// The commit row the keyboard is on. Clients refuse a command aimed at
    /// the wrong pane before entering the shared action; `None` means no row.
    fn commit_target(&self) -> Option<SelectedCommit>;
    /// Arms this (command, commit) pair, or spends an arm already standing
    /// on it. The command names the arm, so a soft reset never spends a
    /// hard one's question.
    fn confirm_or_arm_commit(&mut self, command: &str, target: &SelectedCommit) -> bool;
    /// The loaded history window, newest first, and the source index of
    /// the keyboard — what a rewrite composes its plan over. `None` when
    /// the pane cannot offer one: a filtered list, an empty list, a
    /// fixture with no history at all.
    fn history_window(&self) -> Option<(&[Commit], usize)>;
    /// The cherry-pick clipboard the client keeps across presses. Held by
    /// the client rather than built here because the same clipboard has to
    /// outlive every action and be drawn beside the rows it holds — the
    /// *order* in it is [`CherryClipboard`]'s, which is what makes a paste
    /// mean the same thing in every frontend.
    fn clipboard(&mut self) -> &mut CherryClipboard;
    /// HEAD's own commit, when there is one. `None` on an unborn branch and
    /// without a repository. What `commits.reset-author` measures its target
    /// against: the newest row of the *loaded window* is not HEAD when the
    /// pane is drilled into another branch's log, so the question is asked
    /// of HEAD directly rather than inferred from a row's position.
    fn head_sha(&self) -> Option<Vec<u8>>;
    /// Marks (or, with `None`, unmarks) the commit a `--onto` rebase would
    /// count from. Required rather than defaulted: a pane that offers the
    /// key and forgets the answer is the silent no-op this trait exists to
    /// make impossible.
    fn mark_rebase_base(&mut self, base: Option<SelectedCommit>);
    /// The marked range of commit rows, newest first as a commits list
    /// reads. `None` when nothing is marked — the honest default for a
    /// client with no range marking, whose copy then takes the row alone.
    fn commit_range(&self) -> Option<Vec<SelectedCommit>> {
        None
    }
}

/// The guard every history rewrite shares: a repository to write in, and no
/// operation standing. A reset, revert or cherry-pick aimed into a merge or
/// rebase in progress is a second write inside git's first, so it waits for
/// the standing one to be aborted or finished first.
fn history_repo(client: &mut impl Client, command: &str) -> Option<Handle> {
    let Some(repo) = client.repo() else {
        client.say("a fixture has no repository to rewrite in".into());
        return None;
    };
    if let Some(operation) = client.operation() {
        client.say(format!(
            "{command} waits for the standing {} — abort it or finish it first",
            operation.kind.word()
        ));
        return None;
    }
    Some(repo)
}

/// `commits.reset-menu`: the question, not the write. The strengths stay
/// separate commands so each arms and spends its own answer — a soft reset
/// must never spend a hard one's question.
pub fn reset_menu(client: &mut impl HistoryClient) {
    let Some(target) = client.commit_target() else {
        client.say("nothing selected to reset to".into());
        return;
    };
    if client.repo().is_none() {
        client.say("a fixture has no repository to rewrite in".into());
        return;
    }
    client.ask(format!(
        "reset to {}? soft, mixed or hard — s, m, h",
        target.short
    ));
}

/// `commits.reset-soft` / `-mixed` / `-hard`: moves the branch onto the
/// commit the keyboard is on, taking as much of the index and working tree
/// along as the strength says. Hard destroys unstaged work, which is why
/// every strength asks twice — armed on (command, commit), so the second
/// press has to name the same strength at the same commit.
pub fn reset_to(client: &mut impl HistoryClient, command: &str, mode: ResetMode) {
    let Some(target) = client.commit_target() else {
        client.say("nothing selected to reset to".into());
        return;
    };
    let Some(repo) = history_repo(client, command) else {
        return;
    };
    if !client.confirm_or_arm_commit(command, &target) {
        client.ask(format!(
            "reset {} to {}? press again to confirm",
            mode.flag(),
            target.short
        ));
        return;
    }
    let job = Write::reset(&repo, mode, target.sha.clone());
    if !client.submit(Box::new(job)) {
        client.say("the job queue is shutting down".into());
    }
}

/// `commits.revert`: lands the commit's inverse as a new commit. Nothing is
/// destroyed — dropping the result undoes the undo — so no confirmation
/// precedes it, and a conflict comes back refused in git's own words.
pub fn revert_commit(client: &mut impl HistoryClient) {
    const COMMAND: &str = "commits.revert";
    let Some(target) = client.commit_target() else {
        client.say("nothing selected to revert".into());
        return;
    };
    let Some(repo) = history_repo(client, COMMAND) else {
        return;
    };
    let job = Write::revert(&repo, target.sha.clone());
    if !client.submit(Box::new(job)) {
        client.say("the job queue is shutting down".into());
    }
}

/// `commits.cherry-pick`: replays the commit under the keyboard onto HEAD.
/// One commit only; the clipboard's ranges are W6's second half. A conflict
/// stops the pick mid-flight and the lifecycle carries it from there.
pub fn cherry_pick_single(client: &mut impl HistoryClient) {
    const COMMAND: &str = "commits.cherry-pick";
    let Some(target) = client.commit_target() else {
        client.say("nothing selected to cherry-pick".into());
        return;
    };
    let Some(repo) = history_repo(client, COMMAND) else {
        return;
    };
    let job = Write::cherry_pick(&repo, target.sha.clone());
    if !client.submit(Box::new(job)) {
        client.say("the job queue is shutting down".into());
    }
}

/// `commits.squash-up` / `fixup-up` / `drop-commit`: composes the plan
/// over the loaded window and runs it as a scripted interactive rebase.
/// Every shape the plan cannot complete refuses in `compose`'s words
/// before anything is armed — a merge in the window, a fold at the
/// window's edge, a root — and a composable shape still asks twice,
/// armed on (command, commit), because a rewrite destroys. The standing
/// operation (a rebase already mid-flight, a merge unanswered) refuses
/// first: starting a second rewrite inside git's first is never the move.
/// A conflict mid-rewrite comes back refused in git's words with rebase
/// state left standing for the lifecycle to carry on.
pub fn rewrite_commit(client: &mut impl HistoryClient, command: &str, kind: Rewrite) {
    let verb = match kind {
        Rewrite::SquashUp => "squash",
        Rewrite::FixupUp => "fixup",
        Rewrite::Drop => "drop",
    };
    // The window is cloned out before anything else borrows the client
    // mutably: the plan composes over owned commits, so a standing
    // operation or a missing repository can still refuse in its own words
    // after the window is already in hand.
    let Some((window, index)) = client.history_window().map(|(w, i)| (w.to_vec(), i)) else {
        client.say(format!(
            "{command} needs the whole loaded window — clear the search first"
        ));
        return;
    };
    let Some(target) = client.commit_target() else {
        client.say("nothing selected to rewrite".into());
        return;
    };
    let Some(repo) = history_repo(client, command) else {
        return;
    };
    let (upstream, script) = match compose(kind, &window, index) {
        Ok(plan) => plan,
        Err(e) => {
            client.say(e);
            return;
        }
    };
    if !client.confirm_or_arm_commit(command, &target) {
        client.ask(format!("{verb} {}? press again to confirm", target.short));
        return;
    }
    let job = Write::rebase_todo(&repo, upstream, script);
    if !client.submit(Box::new(job)) {
        client.say("the job queue is shutting down".into());
    }
}

/// `commits.create-fixup`: commits the index as a fixup for the commit
/// under the keyboard. The message is git's marker (`fixup! <subject>` and
/// its `amend!` / `reword!` siblings, chosen by the kind key), so there is
/// no prompt to answer and no confirmation dance — like a revert it only
/// adds, and the finish announces the marker it wrote.
///
/// Two refusals before anything is queued. Nothing staged is the common
/// one: a fixup over an empty index is git's "nothing to commit", which
/// names the wrong failure, so the staged read answers first. An
/// amend!/reword! kind is the other: those spellings open an editor, and
/// this client has no external-editor door yet — asking for one must say
/// that, never hang on a prompt nobody can see.
pub fn create_fixup(client: &mut impl HistoryClient, command: &str, kind: FixupKind) {
    let Some(target) = client.commit_target() else {
        client.say("nothing selected to fix up".into());
        return;
    };
    if !matches!(kind, FixupKind::Fixup) {
        client.say(format!(
            "{} creation opens an editor, and this client has no external-editor door yet",
            kind.word()
        ));
        return;
    }
    let Some(repo) = history_repo(client, command) else {
        return;
    };
    let staged = match repo.status() {
        Ok(status) => status.staged,
        Err(e) => {
            client.say(e);
            return;
        }
    };
    if staged.is_empty() {
        client.say("nothing staged to fix up — stage the change first (space)".into());
        return;
    }
    let job = Write::fixup_commit(&repo, target.sha.clone(), target.short.clone(), kind);
    if !client.submit(Box::new(job)) {
        client.say("the job queue is shutting down".into());
    }
}

/// How far back the fixup discovery looks: one `commit_files` read per
/// row, so an unbounded scan would put a whole log's latency on a
/// keypress. A fixup target is all but always close; past the cap the
/// keyboard moves by hand, which is the honest fallback rather than a
/// guess made slowly.
const FIXUP_SEARCH_DEPTH: usize = 100;

/// `commits.find-fixup-base`: moves the keyboard to the commit the staged
/// changes build on, so the creation aims right without hunting history
/// one row at a time.
///
/// The guess is file overlap: the staged paths against every recent
/// commit's files, newest wins, ties stay newest. A guess is announced as
/// one — the creation still aims at whatever row the keyboard is on when
/// it lands, so a wrong guess costs a move and nothing else. No overlap
/// with any loaded commit says so and moves nothing. Returns the window
/// index found, so the client can move its own cursor.
pub fn find_fixup_base(client: &mut impl HistoryClient, command: &str) -> Option<usize> {
    let repo = history_repo(client, command)?;
    let (window, _) = plan_window(client, command)?;
    let staged = match repo.status() {
        Ok(status) => status.staged,
        Err(e) => {
            client.say(e);
            return None;
        }
    };
    if staged.is_empty() {
        client.say("nothing staged to place — stage the change first (space)".into());
        return None;
    }
    let mut best: Option<(usize, usize)> = None;
    for (index, commit) in window.iter().enumerate().take(FIXUP_SEARCH_DEPTH) {
        let files = match repo.commit_files(commit.sha.as_bytes()) {
            Ok(files) => files,
            Err(e) => {
                client.say(e);
                return None;
            }
        };
        let mut score = 0;
        for entry in &staged {
            if files.iter().any(|(_, path)| path == entry.path.as_bytes()) {
                score += 1;
            }
        }
        if score > best.map(|(_, s)| s).unwrap_or(0) {
            best = Some((index, score));
        }
    }
    match best {
        Some((index, _)) => Some(index),
        None => {
            client.say(
                "the staged changes touch no file any recent commit touched — move the keyboard by hand"
                    .into(),
            );
            None
        }
    }
}

/// `commits.apply-fixups`: folds every `fixup!` / `squash!` line in the
/// window into the commit it names, through the same plan machinery the
/// todo screen runs — build over the deepest landing, autosquash, confirm,
/// submit. Markers that name nothing refuse by name instead of riding the
/// plan as picks: a pick in this run is a fixup that silently stays a
/// commit, which is the one outcome this key must never produce.
pub fn apply_fixups(client: &mut impl HistoryClient, command: &str) {
    // The guard's refusals are the point — a fixture has no history to
    // fold, and a second rewrite must never start inside git's first. The
    // plan machinery re-acquires the handle itself.
    if history_repo(client, command).is_none() {
        return;
    }
    let Some((window, _)) = plan_window(client, command) else {
        return;
    };
    let marks = fixup_marks(&window);
    if marks.is_empty() {
        client.say("no fixup! or squash! commits in the window".into());
        return;
    }
    let mut deepest = 0;
    for mark in &marks {
        match mark.target {
            Some(target) => deepest = deepest.max(target),
            None => {
                client.say(format!(
                    "{} names no loaded commit — reword it onto one, or drop it, first",
                    mark.remainder
                ));
                return;
            }
        }
    }
    let mut plan = match Plan::over(&window, deepest) {
        Ok(plan) => plan,
        Err(e) => {
            client.say(e);
            return;
        }
    };
    // Every marker moves by construction — the landings were resolved
    // above — so the count needs no gate. Re-resolving inside the run is
    // the plan machinery's own staleness contract.
    plan.autosquash();
    run_plan(client, command, plan);
}

// -------------------------------------------------------------- the todo plan

/// The window a rewrite composes over, cloned out before anything borrows
/// the client mutably — a plan is built from owned commits, so a standing
/// operation or a missing repository can still refuse in its own words with
/// the window already in hand.
fn plan_window(client: &mut impl HistoryClient, command: &str) -> Option<(Vec<Commit>, usize)> {
    match client.history_window().map(|(w, i)| (w.to_vec(), i)) {
        Some(window) => Some(window),
        None => {
            client.say(format!(
                "{command} needs the whole loaded window — clear the search first"
            ));
            None
        }
    }
}

/// The plan a todo UI opens on: every commit from HEAD down to the row the
/// keyboard is on, each one a `pick` — which is git's own starting plan, and
/// a rebase that changes nothing until somebody edits it.
///
/// `commits.interactive-rebase`. Nothing is written here and nothing is
/// confirmed: the confirmation belongs to the press that *runs* the edited
/// plan, and a UI that asked before it opened would be asking about a
/// rewrite nobody had described yet. The refusals are the plan's own — a
/// merge in the window, a root beneath it, a filtered list — plus the two
/// this module makes everywhere: a fixture has no repository, and a standing
/// operation is git's first write, which a second must not start inside.
pub fn interactive_plan(client: &mut impl HistoryClient, command: &str) -> Option<Plan> {
    // The standing operation refuses first, everywhere in this file: a
    // second rewrite inside git's first is never the move, and the sentence
    // that says so must not be pre-empted by one about the window.
    history_repo(client, command)?;
    let (window, index) = plan_window(client, command)?;
    match Plan::over(&window, index) {
        Ok(plan) => Some(plan),
        Err(e) => {
            client.say(e);
            None
        }
    }
}

/// Runs an edited plan, once the reader has confirmed it.
///
/// The arm names the command and the plan's deepest commit, exactly as
/// every other rewrite here arms: a plan edited, abandoned and re-opened
/// asks again, because the second plan is not the one the first press was
/// about. The count and the base are in the question, because "rewrite 4
/// commits from a1b2c3d" is the only sentence that says what is at stake.
pub fn run_plan(client: &mut impl HistoryClient, command: &str, plan: Plan) -> bool {
    let Some(repo) = history_repo(client, command) else {
        return false;
    };
    if let Err(e) = plan.validate() {
        client.say(e);
        return false;
    }
    let target = SelectedCommit {
        sha: plan.upstream().to_vec(),
        short: plan.base().to_string(),
    };
    if !client.confirm_or_arm_commit(command, &target) {
        client.ask(format!(
            "rewrite {} from {}? press again to confirm",
            commits_count(plan.len()),
            plan.base()
        ));
        return false;
    }
    let job = Write::rebase_plan(&repo, plan);
    if !client.submit(Box::new(job)) {
        client.say("the job queue is shutting down".into());
        return false;
    }
    true
}

/// `commits.edit-commit`: stop the rebase *at* the commit under the
/// keyboard, so the reader can amend it and carry on.
///
/// The one plan that is expected to hand back a standing rebase rather than
/// a finished one — `edit` opens no editor, it stops, and the state it stops
/// in is the state the lifecycle keys were built for. Confirmed like every
/// rewrite, because everything above the stop is replayed either way.
pub fn edit_commit(client: &mut impl HistoryClient) {
    const COMMAND: &str = "commits.edit-commit";
    if history_repo(client, COMMAND).is_none() {
        return;
    }
    let Some((window, index)) = plan_window(client, COMMAND) else {
        return;
    };
    let mut plan = match Plan::over(&window, index) {
        Ok(plan) => plan,
        Err(e) => {
            client.say(e);
            return;
        }
    };
    if let Err(e) = plan.set_action(index, gitten_core::rebase::Action::Edit) {
        client.say(e);
        return;
    }
    if run_plan(client, COMMAND, plan) {
        client.say(format!(
            "stopping at {} — amend, then continue the rebase",
            window[index].short
        ));
    }
}

/// `commits.move-up` / `commits.move-down`: swap the commit under the
/// keyboard with its neighbour.
///
/// Up is towards HEAD, which is *later* in git's own file — the plan holds
/// the same order the list draws, so the two words mean one thing. The
/// window reaches one commit deeper than the pair for a move down, because a
/// plan that does not replay the commit being swapped past cannot swap past
/// it.
pub fn move_commit(client: &mut impl HistoryClient, command: &str, up: bool) {
    if history_repo(client, command).is_none() {
        return;
    }
    let Some((window, index)) = plan_window(client, command) else {
        return;
    };
    // Refused before the window is even built: the edges of the list are
    // not a plan's to refuse, because a deeper window would move HEAD's
    // neighbour and there is no such thing above HEAD.
    if up && index == 0 {
        client.say("that commit is already the newest — nothing above it to swap with".into());
        return;
    }
    let base = match up {
        true => index,
        false => index + 1,
    };
    if base >= window.len() {
        client.say(
            "that commit sits at the edge of the loaded history, so the plan \
             cannot say what lies beneath it"
                .into(),
        );
        return;
    }
    let mut plan = match Plan::over(&window, base) {
        Ok(plan) => plan,
        Err(e) => {
            client.say(e);
            return;
        }
    };
    let moved = match up {
        true => plan.move_up(index),
        false => plan.move_down(index),
    };
    if let Err(e) = moved {
        client.say(e);
        return;
    }
    run_plan(client, command, plan);
}

/// `commits.reword`: replace one commit's message with the bytes a reader
/// typed, and move nothing else.
///
/// Two paths and one sentence. HEAD is `git commit --amend --only`, which
/// keeps the index exactly where it is and works on a root commit. Anything
/// deeper is a rebase carrying the message down as a plan — the same
/// rewrite, arriving where a bare amend cannot reach.
///
/// The target is revalidated before either: the message was typed into a
/// field, and a repository can move under an open field. A row that is no
/// longer in the window refuses rather than rewording whatever now sits at
/// its index.
pub fn reword_commit(client: &mut impl HistoryClient, target: SelectedCommit, message: String) {
    const COMMAND: &str = "commits.reword";
    if message.trim().is_empty() {
        client.say("a commit needs a message".into());
        return;
    }
    if history_repo(client, COMMAND).is_none() {
        return;
    }
    let head = client.head_sha();
    if head.as_deref() == Some(target.sha.as_slice()) {
        let Some(repo) = history_repo(client, COMMAND) else {
            return;
        };
        let job = Write::reword_head(&repo, message);
        if !client.submit(Box::new(job)) {
            client.say("the job queue is shutting down".into());
        }
        return;
    }
    let Some((window, _)) = plan_window(client, COMMAND) else {
        return;
    };
    let Some(index) = window
        .iter()
        .position(|c| c.sha.as_bytes() == target.sha.as_slice())
    else {
        client.say(format!(
            "{} is no longer in the loaded history — nothing was reworded",
            target.short
        ));
        return;
    };
    let mut plan = match Plan::over(&window, index) {
        Ok(plan) => plan,
        Err(e) => {
            client.say(e);
            return;
        }
    };
    if let Err(e) = plan.set_message(index, message.into_bytes()) {
        client.say(e);
        return;
    }
    run_plan(client, COMMAND, plan);
}

/// `commits.mark-base`: mark the commit under the keyboard as the base a
/// `--onto` rebase counts from, or clear the mark by pressing it again on
/// the same row.
///
/// Pure state, like the cherry-pick clipboard, and said either way: a mark
/// nothing announces is a mark nobody knows they are carrying into the next
/// rebase.
pub fn mark_rebase_base(client: &mut impl HistoryClient) {
    let Some(target) = client.commit_target() else {
        client.say("nothing selected to mark".into());
        return;
    };
    if client.repo().is_none() {
        client.say("a fixture has no repository to rebase in".into());
        return;
    }
    if client.rebase_base().as_ref() == Some(&target) {
        client.mark_rebase_base(None);
        client.say(format!("{} is no longer the rebase base", target.short));
        return;
    }
    let shown = target.short.clone();
    client.mark_rebase_base(Some(target));
    client.say(format!(
        "{shown} is the rebase base — everything after it moves"
    ));
}

/// `commits.rebase-onto`: move the branch HEAD sits on onto the branch the
/// keyboard is on, replaying this branch's own commits.
///
/// With a base marked, git's `--onto` instead: the marked commit stays where
/// it is and only its children move. That is the whole reason marking one is
/// worth a key, and it is also why the question names both ends — a base
/// that is not an ancestor of HEAD replays a range nobody meant.
///
/// Asked twice, because it rewrites this branch's own history. A dirty tree
/// and a conflict are git's own sentences, coming back verbatim with
/// whatever state git left standing.
pub fn rebase_onto(client: &mut impl BranchClient, onto: Vec<u8>, shown: String) {
    const COMMAND: &str = "commits.rebase-onto";
    let Some(repo) = client.repo() else {
        client.say("a fixture has no repository to rebase in".into());
        return;
    };
    if refuse_while_operating(client) {
        return;
    }
    let base = client.rebase_base();
    // Armed on the branch it is aimed at, and not on the row: the pane's
    // own arm is the delete's, and a question about a rewrite must never be
    // spendable by a keypress that means destroy the branch.
    if !client.confirm_or_arm(COMMAND, &onto) {
        client.ask(match &base {
            Some(base) => format!(
                "rebase onto {shown}, from {} up? press again to confirm",
                base.short
            ),
            None => format!("rebase onto {shown}? press again to confirm"),
        });
        return;
    }
    let job = match base {
        Some(base) => Write::rebase_onto_base(&repo, onto, base.sha),
        None => Write::rebase_onto(&repo, onto),
    };
    if !client.submit(Box::new(job)) {
        client.say("the job queue is shutting down".into());
    }
}

/// `files.reset-menu`: the question, not the write — which strength, or the
/// nuke. The answers are separate commands so each arms and spends its own,
/// exactly as the commit list's reset strengths do.
pub fn upstream_reset_menu(client: &mut impl FileClient) {
    if client.repo().is_none() {
        client.say("a fixture has no repository to reset".into());
        return;
    }
    client.ask(
        "reset to the upstream? soft, mixed or hard — s, m, h — or D to nuke the working tree"
            .into(),
    );
}

/// `files.reset-upstream-soft` / `-mixed` / `-hard`: move this branch onto
/// whatever its upstream holds, taking as much of the index and working tree
/// along as the strength says.
///
/// The aim is read from the repository — the branch under HEAD, then its
/// configured upstream — so a detached HEAD and an untracked branch each
/// refuse with a sentence rather than a revspec error. Asked twice, armed on
/// the command, because a hard one discards work no reflog holds.
pub fn reset_to_upstream(client: &mut impl FileClient, command: &str, mode: ResetMode) {
    let Some(repo) = client.repo() else {
        client.say("a fixture has no repository to reset".into());
        return;
    };
    if refuse_while_operating(client) {
        return;
    }
    // Built before the arm is spent: an aim that cannot resolve must not
    // consume a question the reader would then have to ask again.
    let job = match Write::reset_upstream(&repo, mode) {
        Ok(job) => job,
        Err(e) => {
            client.say(e);
            return;
        }
    };
    if !client.confirm_or_arm(command, &[]) {
        client.ask(format!(
            "reset {} to the upstream? press again to confirm",
            mode.flag()
        ));
        return;
    }
    if !client.submit(Box::new(job)) {
        client.say("the job queue is shutting down".into());
    }
}

/// `files.nuke`: throw the whole working tree away — every uncommitted byte,
/// tracked and untracked alike.
///
/// The most destructive key in the client, and the only one whose question
/// says what cannot be undone: there is no stash behind it and no reflog
/// entry for a file that was never committed. Ignored files stay, which the
/// question says too, because "everything" would be a lie about the build
/// directory it leaves alone.
pub fn nuke_worktree(client: &mut impl FileClient) {
    const COMMAND: &str = "files.nuke";
    let Some(repo) = client.repo() else {
        client.say("a fixture has no working tree to nuke".into());
        return;
    };
    if refuse_while_operating(client) {
        return;
    }
    if !client.confirm_or_arm(COMMAND, &[]) {
        client.ask(
            "nuke the working tree? every uncommitted change goes, tracked and \
             untracked — ignored files stay. press again to confirm"
                .into(),
        );
        return;
    }
    if !client.submit(Box::new(Write::nuke_worktree(&repo))) {
        client.say("the job queue is shutting down".into());
    }
}

/// `n` commits, said the way a person says it.
fn commits_count(n: usize) -> String {
    match n {
        1 => "1 commit".into(),
        n => format!("{n} commits"),
    }
}

/// `commits.copy`: puts the marked range — or the row alone when nothing is
/// marked — onto the cherry-pick clipboard. Pure state, no write, so no
/// confirmation and no operation gate: a copy under a standing merge is
/// harmless, and the paste that is not says so itself. A repository is
/// still required, because a clipboard filled against a fixture could
/// never be pasted anywhere.
pub fn copy_commits(client: &mut impl HistoryClient) {
    let shas: Vec<Vec<u8>> = match client.commit_range() {
        Some(range) if !range.is_empty() => range.into_iter().map(|c| c.sha).collect(),
        _ => match client.commit_target() {
            Some(target) => vec![target.sha],
            None => {
                client.say("nothing selected to copy".into());
                return;
            }
        },
    };
    if client.repo().is_none() {
        client.say("a fixture has no repository to cherry-pick from".into());
        return;
    }
    let added = client.clipboard().copy_newest_first(&shas);
    let held = client.clipboard().len();
    // A second press on the same rows is not a failure and not a silence:
    // the clipboard is unchanged and the count says what is still on it.
    if added == 0 {
        client.say(format!(
            "already copied — {held} commit{} on the clipboard",
            plural(held)
        ));
    } else {
        client.say(format!(
            "copied {added} commit{} — {held} on the clipboard",
            plural(added)
        ));
    }
}

/// `commits.paste`: replays every copied commit onto the current branch, in
/// the order copied. Nothing existing moves, so nothing is confirmed; the
/// clipboard survives the paste, because the same set is often wanted on a
/// second branch and only an explicit clear takes it away.
pub fn paste_commits(client: &mut impl HistoryClient) {
    const COMMAND: &str = "commits.paste";
    if client.clipboard().is_empty() {
        client.say("nothing copied to cherry-pick — copy commits first".into());
        return;
    }
    let Some(repo) = history_repo(client, COMMAND) else {
        return;
    };
    let shas = client.clipboard().ordered().to_vec();
    let job = Write::cherry_pick_range(&repo, shas);
    if !client.submit(Box::new(job)) {
        client.say("the job queue is shutting down".into());
    }
}

/// `commits.clear-copies`: empties the clipboard. Worth a sentence either
/// way — a clear that says nothing leaves a reader unsure whether the copy
/// ever landed.
pub fn clear_copies(client: &mut impl HistoryClient) {
    let held = client.clipboard().len();
    if held == 0 {
        client.say("the cherry-pick clipboard is already empty".into());
        return;
    }
    client.clipboard().clear();
    client.say(format!("cleared {held} copied commit{}", plural(held)));
}

/// `commits.reset-author`: hands a commit's authorship to the current user
/// and moves nothing else.
///
/// HEAD is one amend and is measured against HEAD's own sha rather than the
/// newest row — a drilled-into branch log's first row is somebody else's
/// tip. Anything deeper is the same rewrite arriving by rebase: the plan
/// replays the window with `--reset-author` hung on the one commit, which is
/// the todo path doing what a bare amend cannot reach. Either way it asks
/// twice, armed on (command, commit) like every other rewrite here.
pub fn reset_commit_author(client: &mut impl HistoryClient) {
    const COMMAND: &str = "commits.reset-author";
    let Some(target) = client.commit_target() else {
        client.say("nothing selected to re-author".into());
        return;
    };
    // The repository and the standing operation refuse ahead of every
    // question about *which* commit: a rewrite inside git's own is refused
    // whether it would have been an amend or a rebase.
    let Some(repo) = history_repo(client, COMMAND) else {
        return;
    };
    let deep = match client.head_sha() {
        Some(head) if head == target.sha => false,
        Some(_) => true,
        None => {
            client.say("there is no HEAD commit to re-author".into());
            return;
        }
    };
    if deep {
        let Some((window, _)) = plan_window(client, COMMAND) else {
            return;
        };
        let Some(index) = window
            .iter()
            .position(|c| c.sha.as_bytes() == target.sha.as_slice())
        else {
            client.say(format!(
                "{} is not in the loaded history — nothing was re-authored",
                target.short
            ));
            return;
        };
        let mut plan = match Plan::over(&window, index) {
            Ok(plan) => plan,
            Err(e) => {
                client.say(e);
                return;
            }
        };
        if let Err(e) = plan.set_amend(index, Amend::ResetAuthor) {
            client.say(e);
            return;
        }
        run_plan(client, COMMAND, plan);
        return;
    }
    if !client.confirm_or_arm_commit(COMMAND, &target) {
        client.ask(format!(
            "reset the author of {} to you? press again to confirm",
            target.short
        ));
        return;
    }
    let job = Write::reset_author(&repo);
    if !client.submit(Box::new(job)) {
        client.say("the job queue is shutting down".into());
    }
}

/// `s` or nothing, for a count the reader is about to read as a noun.
fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// `commits.checkout`: moves HEAD onto the commit under the keyboard — a
/// detached HEAD, said as such, because a checkout that silently stays
/// attached is a branch moved by accident.
pub fn checkout_commit(client: &mut impl HistoryClient) {
    const COMMAND: &str = "commits.checkout";
    let Some(target) = client.commit_target() else {
        client.say("nothing selected to check out".into());
        return;
    };
    let Some(repo) = history_repo(client, COMMAND) else {
        return;
    };
    let job = Write::checkout(&repo, target.sha.clone());
    if !client.submit(Box::new(job)) {
        client.say("the job queue is shutting down".into());
    }
}

/// `files.resolve-ours` / `-theirs` / `-both` / `-keep`: record the
/// selected conflicted path as resolved, taking one side's answer. The
/// resolution is a job like any other write, so the finish wave re-reads
/// the operation and the conflicts it has left.
pub fn resolve_conflict(client: &mut impl Client, side: Side) {
    let Some(repo) = client.repo() else {
        client.say("a fixture has no repository to resolve in".into());
        return;
    };
    let Some(path) = client.selected_conflict() else {
        client.say("the selected file is not a conflict".into());
        return;
    };
    if !client.submit(Box::new(Write::resolve(
        &repo,
        path.as_bytes().to_vec(),
        side,
    ))) {
        client.say("the job queue is shutting down".into());
    }
}

// ------------------------------------------------------- branch movement

/// `branches.checkout` onto a remote-tracking row: create the local branch
/// of the same name, tracking, and put HEAD on it. The one verb on this
/// pane aimed at a remote row that creates something local; a name that is
/// already taken comes back refused in git's own words, never overwritten.
pub fn checkout_tracking(client: &mut impl BranchClient) {
    let Some(Target::Remote { remote, branch }) = client.branch_target() else {
        client.say("the keyboard is not on a remote branch".into());
        return;
    };
    let Some(repo) = client.repo() else {
        client.say("a fixture has no repository to check out in".into());
        return;
    };
    let job = Write::checkout_tracking(
        &repo,
        remote.as_bytes().to_vec(),
        branch.as_bytes().to_vec(),
    );
    if !client.submit(Box::new(job)) {
        client.say("the job queue is shutting down".into());
    }
}

/// `branches.checkout-previous`: put HEAD back on the branch it sat on
/// before this one — git's own `-`, resolved from the reflog.
pub fn checkout_previous(client: &mut impl Client) {
    let Some(repo) = client.repo() else {
        client.say("a fixture has no repository to check out in".into());
        return;
    };
    if !client.submit(Box::new(Write::checkout_previous(&repo))) {
        client.say("the job queue is shutting down".into());
    }
}

/// `branches.force-checkout`: check out the selected local branch over any
/// local changes. The destructive spelling of checkout, confirmed on the
/// keyboard exactly as delete is: first press arms and asks, second press
/// on the same row discards the changes and goes.
pub fn force_checkout(client: &mut impl BranchClient) {
    let Some(target) = client.branch_target() else {
        client.say("nothing selected to check out".into());
        return;
    };
    let shown = match &target {
        Target::Local(name) => name.to_string_lossy().into_owned(),
        Target::Detached => {
            client.say("a detached HEAD is not a branch".into());
            return;
        }
        Target::Remote { remote, branch } => {
            client.say(format!(
                "force-checkout is for local branches — press space on {}/{} to create a tracking branch",
                remote.to_string_lossy(),
                branch.to_string_lossy()
            ));
            return;
        }
    };
    let Some(repo) = client.repo() else {
        client.say("a fixture has no repository to check out in".into());
        return;
    };
    if !client.confirm_or_arm_branch(&target) {
        client.ask(format!(
            "discard local changes and check out {shown}? press again to confirm"
        ));
        return;
    }
    let Target::Local(name) = target else {
        unreachable!("remotes and detached refuse above");
    };
    if !client.submit(Box::new(Write::checkout_force(
        &repo,
        name.as_bytes().to_vec(),
    ))) {
        client.say("the job queue is shutting down".into());
    }
}

/// `branches.checkout-name`: check out whatever the field names, bytes
/// end to end. Empty refused beside the field that just closed; a name git
/// does not know comes back in git's words.
pub fn checkout_by_name(client: &mut impl Client, name: String) {
    if name.trim().is_empty() {
        client.say("a branch needs a name".into());
        return;
    }
    let Some(repo) = client.repo() else {
        client.say("a fixture has no repository to check out in".into());
        return;
    };
    if !client.submit(Box::new(Write::checkout(&repo, name.into_bytes()))) {
        client.say("the job queue is shutting down".into());
    }
}

/// `branches.fast-forward`: move the selected local branch onto the
/// remote-tracking ref of the same name, never sideways. The tracking ref
/// is read fresh — the pane's row is a moment old — and its absence is
/// refused with the push spelled out, because that is the door to it.
pub fn fast_forward(client: &mut impl BranchClient) {
    let Some(Target::Local(name)) = client.branch_target() else {
        client.say("only a local branch can be fast-forwarded".into());
        return;
    };
    let Some(repo) = client.repo() else {
        client.say("a fixture has no repository to fast-forward in".into());
        return;
    };
    let tracking = match repo.remote_branches() {
        Ok(remotes) => remotes
            .iter()
            .find(|r| r.branch.as_bytes() == name.as_bytes())
            .cloned(),
        Err(e) => {
            client.say(e);
            return;
        }
    };
    let Some(tracking) = tracking else {
        client.say(format!(
            "no remote branch named {} — push it (P) to create one",
            name.to_string_lossy()
        ));
        return;
    };
    let job = Write::fast_forward(
        &repo,
        name.as_bytes().to_vec(),
        tracking.remote.as_bytes().to_vec(),
        tracking.branch.as_bytes().to_vec(),
    );
    if !client.submit(Box::new(job)) {
        client.say("the job queue is shutting down".into());
    }
}

/// `branches.set-upstream`: make the selected local branch track the
/// remote-tracking ref of the same name. Which remote stands in is the
/// repository's own configuration, read fresh: exactly one remote carries
/// that branch, it is the answer; `origin` carries it among several, it
/// is the answer, and that much is said. Anything else is a question this
/// one-line answer cannot carry, so it is refused with the candidates
/// named rather than guessed at.
pub fn set_upstream(client: &mut impl BranchClient) {
    let Some(Target::Local(name)) = client.branch_target() else {
        client.say("only a local branch can track a remote branch".into());
        return;
    };
    let Some(repo) = client.repo() else {
        client.say("a fixture has no repository to track in".into());
        return;
    };
    let carriers = match repo.remote_branches() {
        Ok(remotes) => remotes
            .iter()
            .filter(|r| r.branch.as_bytes() == name.as_bytes())
            .map(|r| r.remote.clone())
            .collect::<Vec<_>>(),
        Err(e) => {
            client.say(e);
            return;
        }
    };
    let remote = match carriers.len() {
        0 => {
            client.say(format!(
                "no remote branch named {} — push it (P) to create one",
                name.to_string_lossy()
            ));
            return;
        }
        1 => carriers[0].clone(),
        _ => {
            let origin = carriers.iter().find(|r| r.as_bytes() == b"origin").cloned();
            match origin {
                Some(origin) => origin,
                None => {
                    let shown = carriers
                        .iter()
                        .map(|r| r.to_string_lossy().into_owned())
                        .collect::<Vec<_>>()
                        .join(", ");
                    client.say(format!(
                        "{shown} all carry {} — set the upstream from a config file or the command line",
                        name.to_string_lossy()
                    ));
                    return;
                }
            }
        }
    };
    let job = Write::set_upstream(
        &repo,
        name.as_bytes().to_vec(),
        remote.as_bytes().to_vec(),
        name.as_bytes().to_vec(),
    );
    if !client.submit(Box::new(job)) {
        client.say("the job queue is shutting down".into());
    }
}

/// `branches.unset-upstream`: sever the selected local branch's tracking
/// link. The link only — nothing is fetched, merged or deleted, which is
/// why no confirmation precedes it.
pub fn unset_upstream(client: &mut impl BranchClient) {
    let Some(Target::Local(name)) = client.branch_target() else {
        client.say("only a local branch can stop tracking".into());
        return;
    };
    let Some(repo) = client.repo() else {
        client.say("a fixture has no repository to untrack in".into());
        return;
    };
    if !client.submit(Box::new(Write::unset_upstream(
        &repo,
        name.as_bytes().to_vec(),
    ))) {
        client.say("the job queue is shutting down".into());
    }
}

/// `commits.new-branch`'s accepted name: create the branch at `at` — a
/// revspec git resolves, captured when the field opened. Nothing is checked
/// out; the checkout is the client's own question to offer.
pub fn create_branch_at(client: &mut impl Client, name: String, at: Vec<u8>) {
    if name.trim().is_empty() {
        client.say("a branch needs a name".into());
        return;
    }
    let Some(repo) = client.repo() else {
        client.say("a fixture has no repository to create branches in".into());
        return;
    };
    if !client.submit(Box::new(Write::create_branch(
        &repo,
        name.into_bytes(),
        Some(at),
    ))) {
        client.say("the job queue is shutting down".into());
    }
}

// ----------------------------------------------------------------- remotes

/// `remotes.fetch`: update the selected remote's tracking branches.
pub fn remote_fetch(client: &mut impl RemoteClient) {
    let Some(name) = client.remote_target() else {
        client.say("nothing selected to fetch".into());
        return;
    };
    let Some(repo) = client.repo() else {
        client.say("a fixture has no repository to fetch into".into());
        return;
    };
    if !client.submit(Box::new(Write::fetch(
        &repo,
        Some(name.as_bytes().to_vec()),
    ))) {
        client.say("the job queue is shutting down".into());
    }
}

/// A remote's accepted name and URL, as one job. Both halves are trimmed —
/// git would hold the padding as part of either — and emptiness is refused
/// beside the field that just closed, the same answer every prompt gives.
pub fn remote_add(client: &mut impl Client, name: String, url: String) {
    let (name, url) = (name.trim(), url.trim());
    if name.is_empty() {
        client.say("a remote needs a name".into());
        return;
    }
    if url.is_empty() {
        client.say("a remote needs a URL".into());
        return;
    }
    let Some(repo) = client.repo() else {
        client.say("a fixture has no repository to add a remote to".into());
        return;
    };
    if !client.submit(Box::new(Write::add_remote(
        &repo,
        name.as_bytes().to_vec(),
        url.as_bytes().to_vec(),
    ))) {
        client.say("the job queue is shutting down".into());
    }
}

/// `remotes.edit`'s accepted URL, for the remote `name` the pane held onto.
pub fn remote_edit(client: &mut impl Client, name: Vec<u8>, url: String) {
    let url = url.trim();
    if url.is_empty() {
        client.say("a remote needs a URL".into());
        return;
    }
    let Some(repo) = client.repo() else {
        client.say("a fixture has no repository to edit in".into());
        return;
    };
    if !client.submit(Box::new(Write::set_remote_url(
        &repo,
        name,
        url.as_bytes().to_vec(),
    ))) {
        client.say("the job queue is shutting down".into());
    }
}

/// `remotes.remove`: forget the selected remote. Destructive — its
/// remote-tracking branches go with it — and confirmed on the keyboard
/// exactly as branch deletion is: first press arms and asks, second press
/// on the same row removes.
pub fn remote_remove(client: &mut impl RemoteClient) {
    let Some(name) = client.remote_target() else {
        client.say("nothing selected to remove".into());
        return;
    };
    let Some(repo) = client.repo() else {
        client.say("a fixture has no repository to remove a remote from".into());
        return;
    };
    if !client.confirm_or_arm_remote(&name) {
        client.ask(format!(
            "remove remote {} and its remote-tracking branches? press again to confirm",
            name.to_string_lossy()
        ));
        return;
    }
    if !client.submit(Box::new(Write::remove_remote(
        &repo,
        name.as_bytes().to_vec(),
    ))) {
        client.say("the job queue is shutting down".into());
    }
}

// ------------------------------------------------------------------- tags

/// The client-owned selection and confirmation state needed by tag actions.
pub trait TagClient: Client {
    /// The tag row the keyboard is on, by the name verbs address it with.
    fn tag_target(&self) -> Option<RefName>;
    /// Arms this tag, or spends an arm already standing on it.
    fn confirm_or_arm_tag(&mut self, name: &RefName) -> bool;
}

/// A tag's accepted name: names `target` — a branch, a commit, any revspec
/// git resolves — carrying `message` when one was given (annotated) and
/// nothing when the field came back empty (lightweight). The branches and
/// commits panes converge here: both hold a revspec in the prompt, so both
/// reach the same verb and neither learns what a tag is.
pub fn create_tag(
    client: &mut impl Client,
    name: String,
    target: Vec<u8>,
    message: Option<String>,
) {
    let name = name.trim();
    if name.is_empty() {
        client.say("a tag needs a name".into());
        return;
    }
    let Some(repo) = client.repo() else {
        client.say("a fixture has no repository to tag in".into());
        return;
    };
    if !client.submit(Box::new(Write::create_tag(
        &repo,
        name.as_bytes().to_vec(),
        target,
        message,
    ))) {
        client.say("the job queue is shutting down".into());
    }
}

/// `tags.delete`: forget one tag name. The commits it named survive — a
/// name and not a home — and the question is asked twice, armed on the
/// name, like every other destructive key here.
pub fn delete_tag(client: &mut impl TagClient) {
    let Some(name) = client.tag_target() else {
        client.say("nothing selected to delete".into());
        return;
    };
    let Some(repo) = client.repo() else {
        client.say("a fixture has no repository to delete tags from".into());
        return;
    };
    if !client.confirm_or_arm_tag(&name) {
        client.ask(format!(
            "delete tag {}? press again to confirm",
            name.to_string_lossy()
        ));
        return;
    }
    if !client.submit(Box::new(Write::delete_tag(&repo, name.as_bytes().to_vec()))) {
        client.say("the job queue is shutting down".into());
    }
}

/// `tags.push`'s accepted remote: pushes the selected tag there. The remote
/// rides the prompt the keypress opened (prefilled when the repository
/// knows exactly one), because tags track nothing and there is no upstream
/// to default to — guessing `origin` in a two-remote repository would aim
/// a publishable name at the wrong room.
pub fn push_tag(client: &mut impl TagClient, name: Vec<u8>, remote: String) {
    let remote = remote.trim();
    if remote.is_empty() {
        client.say("a push needs a remote".into());
        return;
    }
    let Some(repo) = client.repo() else {
        client.say("a fixture has no repository to push from".into());
        return;
    };
    if !client.submit(Box::new(Write::push_tag(
        &repo,
        remote.as_bytes().to_vec(),
        name,
    ))) {
        client.say("the job queue is shutting down".into());
    }
}

/// `tags.checkout`: check the selected tag out, detached. The tag is a
/// name for a commit, so this is the commits pane's detached checkout with
/// a different row under the keyboard — same verb, same refusal when the
/// tree cannot move.
pub fn checkout_tag(client: &mut impl TagClient) {
    let Some(name) = client.tag_target() else {
        client.say("nothing selected to check out".into());
        return;
    };
    let Some(repo) = client.repo() else {
        client.say("a fixture has no repository to check out in".into());
        return;
    };
    if !client.submit(Box::new(Write::checkout(&repo, name.as_bytes().to_vec()))) {
        client.say("the job queue is shutting down".into());
    }
}

/// `branches.delete-remote`: delete the remote-tracking row's source on its
/// remote. The local branch of the same name survives — this is the remote
/// half of branch deletion, and the question says so twice: first press
/// arms and names both halves, second press on the same row deletes.
pub fn delete_remote_branch(client: &mut impl BranchClient) {
    let Some(Target::Remote { remote, branch }) = client.branch_target() else {
        client.say("only a remote-tracking row has a remote branch to delete".into());
        return;
    };
    let Some(repo) = client.repo() else {
        client.say("a fixture has no repository to delete from".into());
        return;
    };
    let target = Target::Remote {
        remote: remote.clone(),
        branch: branch.clone(),
    };
    if !client.confirm_or_arm_branch(&target) {
        client.ask(format!(
            "delete {}/{} on {}? the local branch stays — press again to confirm",
            remote.to_string_lossy(),
            branch.to_string_lossy(),
            remote.to_string_lossy()
        ));
        return;
    }
    if !client.submit(Box::new(Write::delete_remote_branch(
        &repo,
        remote.as_bytes().to_vec(),
        branch.as_bytes().to_vec(),
    ))) {
        client.say("the job queue is shutting down".into());
    }
}

// -------------------------------------------------------------- worktrees

/// The client-owned selection and confirmation state needed by worktree
/// actions: the row, and the force upgrade a dirty refusal offers.
pub trait WorktreeClient: Client {
    /// The worktree row the keyboard is on, as the path verbs address it
    /// with — raw bytes, exactly as the listing spelled them.
    fn worktree_target(&self) -> Option<Vec<u8>>;
    /// Arms this path, or spends an arm already standing on it. The arm
    /// carries whether the force spelling is what the next press runs:
    /// a dirty refusal upgrades the question, anything else re-arms it.
    fn confirm_or_arm_worktree(&mut self, path: &[u8], force: bool) -> bool;
    /// Stands the force upgrade on this path after a plain removal was
    /// submitted for it: the refusal it comes back with is what the next
    /// press spends. A no-op unless the arm stands unforced on this path.
    fn upgrade_worktree_force(&mut self, path: &[u8]);
}

/// `worktrees.new`: check a starting point out into a new worktree.
///
/// `base` is what the new checkout holds — the row's rev, or empty for
/// HEAD — and `branch` names a new branch there instead of a detached
/// checkout. An empty path is refused beside the field that just closed;
/// a base the repository does not hold is git's refusal, in git's words.
pub fn create_worktree(
    client: &mut impl Client,
    base: Vec<u8>,
    path: String,
    branch: Option<String>,
) {
    let path = path.trim();
    if path.is_empty() {
        client.say("a worktree needs a path".into());
        return;
    }
    let Some(repo) = client.repo() else {
        client.say("a fixture has no repository to branch a worktree from".into());
        return;
    };
    let branch = branch
        .map(|b| b.trim().to_string())
        .filter(|b| !b.is_empty())
        .map(String::into_bytes);
    if !client.submit(Box::new(Write::worktree_add(
        &repo,
        path.as_bytes().to_vec(),
        base,
        branch,
    ))) {
        client.say("the job queue is shutting down".into());
    }
}

/// `worktrees.remove`: forget the selected checkout. First press asks,
/// second press runs the plain removal and stands the force upgrade — and
/// when that comes back refused for dirt, the third press is the confirmed
/// force rather than a second surprise: the refusal sentence offers it.
pub fn remove_worktree(client: &mut impl WorktreeClient, force: bool) {
    let Some(path) = client.worktree_target() else {
        client.say("nothing selected on the worktree list".into());
        return;
    };
    if client.repo().is_none() {
        client.say("a fixture has no worktrees to remove".into());
        return;
    }
    if !client.confirm_or_arm_worktree(&path, force) {
        client.say(format!(
            "remove {}? press again to confirm",
            String::from_utf8_lossy(&path)
        ));
        return;
    }
    let Some(repo) = client.repo() else {
        client.say("a fixture has no worktrees to remove".into());
        return;
    };
    if !force {
        client.upgrade_worktree_force(&path);
    }
    if !client.submit(Box::new(Write::worktree_remove(&repo, path, force))) {
        client.say("the job queue is shutting down".into());
    }
}

// ---------------------------------------------------------------- bisect

/// `commits.bisect-start`: open the question the bisection answers, with
/// the selected commit where the bug is and the prompt's rev where it is
/// not. A standing bisection refuses before any process runs — one
/// question at a time — and so does a standing merge, rebase, pick or
/// revert: bisecting checks commits out, and an operation owns the tree.
pub fn bisect_start(client: &mut impl Client, bad: Vec<u8>, good: String) {
    if bad.is_empty() {
        client.say("nothing selected to bisect from".into());
        return;
    }
    let good = good.trim();
    if good.is_empty() {
        client.say("a bisect needs a revision the bug is not in".into());
        return;
    }
    let Some(repo) = client.repo() else {
        client.say("a fixture has no history to bisect".into());
        return;
    };
    if client.bisect().is_some() {
        client.say("a bisect is already in progress — reset it first".into());
        return;
    }
    if refuse_while_operating(client) {
        return;
    }
    if !client.submit(Box::new(Write::bisect_start(
        &repo,
        bad,
        vec![good.as_bytes().to_vec()],
    ))) {
        client.say("the job queue is shutting down".into());
    }
}

/// `commits.bisect-good` / `-bad` / `-skip`: judge the checkout the
/// bisection is asking about. Outside a bisection this is git's refusal,
/// said by the verb; the shared action only checks there is a repository
/// to ask in.
pub fn bisect_mark(client: &mut impl Client, verb: BisectMark) {
    let Some(repo) = client.repo() else {
        client.say("a fixture has no bisection to judge".into());
        return;
    };
    let job = match verb {
        BisectMark::Good => Write::bisect_good(&repo, Vec::new()),
        BisectMark::Bad => Write::bisect_bad(&repo, Vec::new()),
        BisectMark::Skip => Write::bisect_skip(&repo, Vec::new()),
    };
    if !client.submit(Box::new(job)) {
        client.say("the job queue is shutting down".into());
    }
}

/// Which judgement a `bisect_mark` carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BisectMark {
    /// The checkout works: `git bisect good`.
    Good,
    /// The checkout shows the bug: `git bisect bad`.
    Bad,
    /// The checkout cannot be judged: `git bisect skip`.
    Skip,
}

/// `commits.bisect-reset`: end the bisection, back where it started.
/// Outside one this is the quiet no-op, so it takes no confirmation.
pub fn bisect_reset(client: &mut impl Client) {
    let Some(repo) = client.repo() else {
        client.say("a fixture has no bisection to end".into());
        return;
    };
    if !client.submit(Box::new(Write::bisect_reset(&repo))) {
        client.say("the job queue is shutting down".into());
    }
}

// ----------------------------------------------------------------- reflog

/// The client-owned selection and confirmation state needed by reflog
/// actions: the entry, and the HEAD it would move.
pub trait ReflogClient: Client {
    /// The reflog row the keyboard is on — selector, commit and message.
    fn reflog_target(&self) -> Option<ReflogEntry>;
    /// Arms this entry, or spends an arm already standing on it.
    fn confirm_or_arm_reflog(&mut self, selector: &str) -> bool;
    /// Where HEAD is now, so recovery can name what moves. The honest
    /// default for a client that tracks no HEAD: none, and recovery
    /// refuses rather than guessing.
    fn head_state(&self) -> Option<HeadState> {
        None
    }
}

/// `reflog.recover`: put the current branch back onto the selected entry —
/// `reset --soft`, so the index and the working tree are untouched — or
/// check the entry out when HEAD is detached. The question previews the
/// move in both directions: what the ref leaves and what it lands on, and
/// what stays (everything uncommitted). A standing operation refuses
/// first: moving HEAD under a merge in flight corrupts it.
pub fn recover_reflog(client: &mut impl ReflogClient) {
    let Some(entry) = client.reflog_target() else {
        client.say("nothing selected to recover".into());
        return;
    };
    let Some(repo) = client.repo() else {
        client.say("a fixture has no repository to recover in".into());
        return;
    };
    if let Some(op) = client.operation() {
        client.say(format!(
            "finish the standing {} first — recovery moves HEAD",
            op.kind.word()
        ));
        return;
    }
    let Some(head) = client.head_state() else {
        client.say("no HEAD to move".into());
        return;
    };
    match head {
        HeadState::Branch { name, commit } => {
            let from = commit.as_deref().unwrap_or("unborn");
            if !client.confirm_or_arm_reflog(&entry.selector) {
                client.ask(format!(
                    "move {} from {} onto {} ({})? index and worktree untouched — press again to confirm",
                    name.to_string_lossy(),
                    from,
                    entry.commit,
                    entry.message
                ));
                return;
            }
            if !client.submit(Box::new(Write::reset(
                &repo,
                ResetMode::Soft,
                entry.commit.as_bytes().to_vec(),
            ))) {
                client.say("the job queue is shutting down".into());
            }
        }
        HeadState::Detached { .. } => {
            if !client.confirm_or_arm_reflog(&entry.selector) {
                client.ask(format!(
                    "check out {} ({})? press again to confirm",
                    entry.commit, entry.message
                ));
                return;
            }
            if !client.submit(Box::new(Write::checkout(
                &repo,
                entry.commit.as_bytes().to_vec(),
            ))) {
                client.say("the job queue is shutting down".into());
            }
        }
    }
}

// --------------------------------------------------------------- undo/redo

/// `history.undo` (`z`): walk the last HEAD move back. A checkout walks
/// back by checking out where it came from; every other move is walked
/// back by pointing HEAD's ref at the earlier entry — `update-ref`, never
/// a reset flag — so uncommitted work is exactly where it was. Refuses
/// behind a fixture, a standing operation, an unreadable reflog, and a
/// move that went nowhere.
pub fn undo_last(client: &mut impl Client) {
    let Some(repo) = client.repo() else {
        client.say("a fixture has no history to undo".into());
        return;
    };
    if let Some(op) = client.operation() {
        client.say(format!(
            "finish the standing {} first — undo moves HEAD",
            op.kind.word()
        ));
        return;
    };
    let entries = match repo.reflog(2) {
        Ok(entries) => entries,
        Err(e) => {
            client.say(e);
            return;
        }
    };
    let [after, before, ..] = entries.as_slice() else {
        // One entry or none: HEAD never moved — a fresh repository, a
        // single commit — so there is nowhere back to walk to.
        client.say("HEAD is where it was — nothing to undo".into());
        return;
    };
    match undo_for(before, after) {
        None => client.say("HEAD is where it was — nothing to undo".into()),
        Some(UndoKind::Checkout { from }) => {
            if !client.submit(Box::new(Write::checkout(&repo, from.as_bytes().to_vec()))) {
                client.say("the job queue is shutting down".into());
            }
        }
        Some(UndoKind::Move { selector }) => {
            let label = format!("undo ({})", after.message);
            if !client.submit(Box::new(Write::move_head(
                &repo,
                label,
                UNDO_MESSAGE,
                selector.as_bytes().to_vec(),
            ))) {
                client.say("the job queue is shutting down".into());
            }
        }
    }
}

/// `history.redo` (`Z`): walk forward again — but only behind our own
/// undo, on an unmoved HEAD. Anything else (a commit, a checkout, the
/// reader's own terminal reset, a redo already standing) reads as "nothing
/// to redo" rather than a guess about which forward step was meant. The
/// check is the reflog's, not the session's, so reopening the client does
/// not disarm a redo that is still honestly armed.
pub fn redo_last(client: &mut impl Client) {
    let Some(repo) = client.repo() else {
        client.say("a fixture has no history to redo".into());
        return;
    };
    if let Some(op) = client.operation() {
        client.say(format!(
            "finish the standing {} first — redo moves HEAD",
            op.kind.word()
        ));
        return;
    };
    let head_sha = match repo.head() {
        Ok(HeadState::Branch {
            commit: Some(sha), ..
        })
        | Ok(HeadState::Detached { commit: sha }) => sha,
        _ => {
            client.say("no HEAD to redo".into());
            return;
        }
    };
    let entries = match repo.reflog(2) {
        Ok(entries) => entries,
        Err(e) => {
            client.say(e);
            return;
        }
    };
    match redo_selector(&entries, &head_sha) {
        None => client.say("nothing to redo — redo follows only our own undo".into()),
        Some(selector) => {
            if !client.submit(Box::new(Write::move_head(
                &repo,
                "redo".into(),
                REDO_MESSAGE,
                selector.as_bytes().to_vec(),
            ))) {
                client.say("the job queue is shutting down".into());
            }
        }
    }
}

/// Turn accepted commit text into its write job./// Turn accepted commit text into its write job.
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

// ----------------------------------------------------------------- hunks

/// Which side of the index a hunk verb is aimed at — the diff source's own
/// word for it, narrowed to the sides a patch can address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HunkSide {
    /// `HEAD`'s tree → the index: the staged side of one path.
    Staged,
    /// The index → the working tree: the unstaged side of one path.
    Unstaged,
    /// The working tree against nothing: a file known to no part of git.
    Untracked,
    /// The combined `HEAD`→worktree aggregate. It cannot aim a stage or an
    /// unstage — its hunks fold both sides of the index into one text —
    /// and only a discard, which addresses the working tree alone, is
    /// answerable here.
    Combined,
}

/// What the keyboard selected on a diff, in the diff's own terms: one
/// whole hunk, or line ranges within the hunks it spans. The hunks are
/// clones of what the screen drew; [`hunk_job`] re-reads the repository
/// and matches them against what git holds now before anything applies.
#[derive(Clone, Debug)]
pub enum HunkSelection {
    Whole(Hunk),
    /// Per touched hunk: the hunk as drawn and the inclusive line range
    /// marked within it.
    Lines(Vec<(Hunk, usize, usize)>),
}

/// A hunk verb's aim: the path it acts on, the side of the index it means,
/// and what was selected.
#[derive(Clone, Debug)]
pub struct HunkAsk {
    pub path: String,
    pub side: HunkSide,
    pub selection: HunkSelection,
}

/// What a fresh re-read of one path's side yields: the hunks the drawn
/// selection matched, the side facts a patch emits with, and the blob
/// identities and shape flags the caller builds its own expectations and
/// refusals from.
pub(crate) struct ResolvedHunks {
    pub chosen: Vec<Hunk>,
    pub sides: gitten_core::patch::Sides,
    pub old_oid: Option<String>,
    pub new_oid: Option<String>,
    pub binary: bool,
    pub old_path: Option<String>,
}

/// Refuses a selection no context can aim: a hunk with no context line
/// and an old side to address leaves `git apply` nothing to locate with.
/// An insertion (no old side to the hunk) and a creation need no context
/// and pass through.
pub(crate) fn check_context(chosen: &[Hunk]) -> Result<(), String> {
    if chosen.iter().any(|h| {
        h.lines
            .iter()
            .all(|l| l.kind != gitten_core::LineKind::Context)
            && h.lines.iter().any(|l| l.old_no.is_some())
    }) {
        return Err(
            "this diff was read with no context — a partial patch cannot be aimed; raise [diff] context"
                .into(),
        );
    }
    Ok(())
}

/// The read half of [`hunk_job`], shared with the patch clipboard: one
/// path re-read now, the drawn selection matched against what git holds
/// by full line content, line windows cut with `keep`. Combined has no
/// per-path read and never reaches here — its caller answers that side
/// alone.
pub(crate) fn resolve_side_hunks(
    repo: &Handle,
    side: HunkSide,
    path: &str,
    selection: &HunkSelection,
    keep: gitten_core::patch::Unselected,
    differs: &gitten_core::differ::Differs,
    over: &gitten_core::differ::Overrides,
) -> Result<ResolvedHunks, String> {
    // One path, re-read now. An empty answer is a side that stopped
    // existing between the preview and this keypress — nothing to aim at,
    // said the way the preview itself says it.
    let pair = match side {
        HunkSide::Unstaged => repo.pairs_unstaged(Some(path.as_bytes()))?.pop(),
        HunkSide::Staged => repo.pairs_staged(Some(path.as_bytes()))?.pop(),
        HunkSide::Untracked => repo.pair_untracked(path.as_bytes())?,
        HunkSide::Combined => unreachable!("the combined aim returned above"),
    }
    .ok_or_else(|| match side {
        HunkSide::Unstaged => format!("nothing unstaged for {} — refresh (R)", path),
        HunkSide::Staged => format!("nothing staged for {} — refresh (R)", path),
        HunkSide::Untracked => format!("{} is not readable — deleted, or not a file", path),
        HunkSide::Combined => unreachable!("the combined aim returned above"),
    })?;

    // The drawn selection, matched against what git holds now. Matching is
    // by the hunks' full line content — kinds, texts and numbers — which is
    // exact whenever the file is as the preview drew it and fails loudly
    // whenever it is not: the diff cache keys on the pair's OIDs, so a
    // fresh re-diff of unchanged content is the preview's own hunks back.
    let fresh = gitten_git::diff_pairs(std::slice::from_ref(&pair), differs, over);
    let hunks = fresh.first().map(|f| f.hunks.as_slice()).unwrap_or(&[]);
    let stale = || {
        format!(
            "{} changed since this diff was drawn — refresh (R) and try again",
            path
        )
    };
    let mut chosen: Vec<Hunk> = Vec::new();
    match selection {
        HunkSelection::Whole(drawn) => {
            let hunk = hunks
                .iter()
                .find(|h| h.lines == drawn.lines)
                .ok_or_else(stale)?;
            chosen.push(hunk.clone());
        }
        HunkSelection::Lines(parts) => {
            for (drawn, lo, hi) in parts {
                let hunk = hunks
                    .iter()
                    .find(|h| h.lines == drawn.lines)
                    .ok_or_else(stale)?;
                // A window with no changed line in it — the keyboard sat on
                // context — stages nothing, and is skipped rather than
                // refused: the rest of the selection still means what it
                // says. One that emptied the whole selection is refused
                // below, where "nothing selected" is one sentence.
                if let Some(window) = gitten_core::patch::line_window(hunk, *lo, *hi, keep) {
                    chosen.push(window);
                }
            }
            if chosen.is_empty() {
                return Err("no changed lines in the selection".into());
            }
        }
    }
    Ok(ResolvedHunks {
        sides: gitten_core::patch::Sides {
            old_lines: pair.old.len(),
            old_final_newline: pair.old_final_newline,
            new_lines: pair.new.len(),
            new_final_newline: pair.new_final_newline,
        },
        old_oid: pair.old_oid.clone(),
        new_oid: pair.new_oid.clone(),
        binary: pair.binary,
        old_path: pair.old_path.clone(),
        chosen,
    })
}

/// The one hunk verb, for every client.
///
/// A partial stage is a read, then a patch, then a checked write, in that
/// order, and this function is the whole of it. The read is fresh — the
/// one path the verb addresses, re-read now, however long ago the preview
/// was drawn — because a patch built from a stale picture aims at content
/// nobody is looking at. The patch is [`gitten_core::patch`]'s, built from
/// the hunks the fresh read still shows; the write carries the blob
/// identities the patch was built against and re-checks them when it runs
/// (see [`CheckedPatch`]), so the gap between the keypress and the queue's
/// turn is covered too. Every refusal is worded here, once, where an
/// extension calling the same verb through the same name reads the same
/// sentence.
/// What a hunk verb on one side of the index means, and where it means
/// nothing — the verb table, pure, so a client can read it *before* it
/// spends a destructive arm and the job build reads it again before it
/// aims. Each row is a fact about the side, not a mood: what the verb
/// would mean there, and why that is nothing. Where a door exists one
/// keypress away, the refusal names it.
pub fn verb_refusal(command: &str, side: HunkSide) -> Option<String> {
    #[derive(Clone, Copy)]
    enum Verb {
        Stage,
        Unstage,
        Discard,
    }
    let verb = match command {
        "diff.stage-hunk" => Verb::Stage,
        "diff.unstage-hunk" => Verb::Unstage,
        "diff.discard-hunk" => Verb::Discard,
        other => return Some(format!("{other} is not a hunk verb")),
    };
    let refusal = match (verb, side) {
        (Verb::Stage, HunkSide::Combined) => {
            Some("the combined view folds both sides of the index — open the file's own side from the files pane (enter)")
        }
        (Verb::Unstage, HunkSide::Combined) => {
            Some("the combined view folds both sides of the index — open the file's staged side from the files pane (enter, then tab)")
        }
        (Verb::Stage, HunkSide::Staged) => {
            Some("the index is this diff's new side — it is already staged")
        }
        (Verb::Unstage, HunkSide::Unstaged) => Some(
            "this side is the index→worktree change — nothing here is staged; tab opens the staged side",
        ),
        (Verb::Unstage, HunkSide::Untracked) => {
            Some("an untracked file has nothing in the index to take out")
        }
        (Verb::Stage, HunkSide::Untracked) => Some(
            "an untracked file stages whole — a creation patch needs the mode git only knows from `git add`; stage it from the files pane",
        ),
        (Verb::Discard, HunkSide::Staged) => Some(
            "the staged side has no working tree to discard — unstage it (u), then discard the unstaged side",
        ),
        (Verb::Discard, HunkSide::Untracked) => {
            Some("an untracked file is all or nothing — remove it whole from the files pane")
        }
        _ => None,
    };
    refusal.map(|s| s.to_string())
}

pub fn hunk_job(
    command: &str,
    ask: HunkAsk,
    repo: &Handle,
    differs: &gitten_core::differ::Differs,
    over: &gitten_core::differ::Overrides,
) -> Result<Box<dyn Job>, String> {
    if let Some(refusal) = verb_refusal(command, ask.side) {
        return Err(refusal);
    }
    // The verb again, for the parts only the job build needs: which
    // unchosen changes a line window keeps, and what the write applies.
    #[derive(Clone, Copy)]
    enum Verb {
        Stage,
        Unstage,
        Discard,
    }
    let verb = match command {
        "diff.stage-hunk" => Verb::Stage,
        "diff.unstage-hunk" => Verb::Unstage,
        _ => Verb::Discard,
    };

    // The combined view is the one aim that cannot re-read a single path:
    // there is no per-path read of HEAD→worktree, only the whole-repo one,
    // and re-reading that per keypress is not a price a verb pays. Its
    // discard keeps the patch the screen drew, with HEAD's blob — read now
    // — as the revalidation anchor; content that drifted under it fails
    // `git apply`'s context check in git's own words.
    if ask.side == HunkSide::Combined {
        let HunkSelection::Whole(hunk) = ask.selection else {
            return Err(
                "the combined view discards whole hunks — open the file's own side for line-level work".into(),
            );
        };
        let expected = repo.head_blob_oid(ask.path.as_bytes())?;
        return Ok(Box::new(CheckedPatch {
            name: format!("discard patch: {}", ask.path),
            repo: Handle::clone(repo),
            verb: PatchVerb::Discard,
            patch: gitten_core::patch::emit(&ask.path, &[&hunk]),
            expect: vec![ExpectOid {
                side: OidSide::Head,
                path: ask.path.clone().into_bytes(),
                oid: expected,
            }],
        }));
    }

    let keep = match verb {
        Verb::Stage => gitten_core::patch::Unselected::KeepRemovals,
        Verb::Unstage | Verb::Discard => gitten_core::patch::Unselected::KeepAdditions,
    };
    let resolved = resolve_side_hunks(
        repo,
        ask.side,
        &ask.path,
        &ask.selection,
        keep,
        differs,
        over,
    )?;
    if resolved.binary {
        return Err(format!(
            "{} is binary — there are no lines to select; stage or discard it whole from the files pane",
            ask.path
        ));
    }
    if resolved
        .old_path
        .as_ref()
        .is_some_and(|old| old != &ask.path)
    {
        return Err(format!(
            "{} is a rename from {} — partial staging of a rename is not supported; stage it whole",
            ask.path,
            resolved.old_path.as_deref().unwrap_or_default()
        ));
    }
    check_context(&resolved.chosen)?;
    let chosen = resolved.chosen;
    let sides = resolved.sides;
    let refs: Vec<&Hunk> = chosen.iter().collect();
    let patch = gitten_core::patch::emit_with(&ask.path, &refs, &sides)?;
    if patch.is_empty() {
        return Err("nothing selected".into());
    }

    // What the write re-checks: the blob the patch was built against must
    // still be the blob the receiving side holds. Stage and discard aim at
    // the index or the worktree over the unstaged pair's old side — the
    // index; an unstage rewrites the index from HEAD's side, so both ends
    // are checked; staging an untracked file requires the index to still
    // have *no* entry under the name. An OID a pair does not carry — a
    // gitlink, a null side — is one that cannot be re-read, so it expects
    // nothing and `git apply`'s own refusal stands guard instead.
    let mut expect = Vec::new();
    match (verb, ask.side) {
        (Verb::Stage, HunkSide::Unstaged) | (Verb::Discard, HunkSide::Unstaged) => {
            expect.push((OidSide::Index, resolved.old_oid.clone()))
        }
        (Verb::Unstage, HunkSide::Staged) => {
            expect.push((OidSide::Head, resolved.old_oid.clone()));
            expect.push((OidSide::Index, resolved.new_oid.clone()));
        }
        _ => unreachable!("every eligible shape is matched above"),
    }
    let verb_name = match verb {
        Verb::Stage => "stage",
        Verb::Unstage => "unstage",
        Verb::Discard => "discard",
    };
    let patch_verb = match verb {
        Verb::Stage => PatchVerb::Stage,
        Verb::Unstage => PatchVerb::Unstage,
        Verb::Discard => PatchVerb::Discard,
    };
    Ok(Box::new(CheckedPatch {
        name: format!("{verb_name} patch: {}", ask.path),
        repo: Handle::clone(repo),
        verb: patch_verb,
        patch,
        expect: expect
            .into_iter()
            .map(|(side, oid)| ExpectOid {
                side,
                path: ask.path.clone().into_bytes(),
                oid,
            })
            .collect(),
    }))
}

/// What [`hunk_job`]'s write applies, once its expectations hold.
/// The clipboard's applies ride the same rail: one checked write per
/// file, whatever the target, because a patch aimed at a moved side is
/// the same mistake wherever it lands.
#[derive(Clone, Copy)]
pub(crate) enum PatchVerb {
    Stage,
    Unstage,
    Discard,
    /// Onto the working tree, forwards — `git apply`.
    ApplyWorktree,
    /// Onto the index, forwards — `git apply --cached`.
    ApplyIndex,
    /// Off the working tree, backwards — `git apply --reverse`.
    ReverseWorktree,
    /// Off the index, backwards — `git apply --cached --reverse`.
    ReverseIndex,
}

/// Which side of the index an expectation reads.
#[derive(Clone, Copy)]
pub(crate) enum OidSide {
    Index,
    Head,
}

/// One identity the write re-checks: the side, the path, and the blob the
/// patch was built against.
pub(crate) struct ExpectOid {
    pub side: OidSide,
    pub path: Vec<u8>,
    pub oid: Option<String>,
}

/// A patch whose assumptions are checked at write time, not build time.
///
/// The gap between the keypress that built a patch and the queue's turn at
/// it is real: another job, another process, another minute. Before one
/// byte applies, every blob the patch was built against is re-read and
/// compared; a mismatch refuses with the refresh spelled out rather than
/// letting `git apply` aim at whatever the side holds now.
pub(crate) struct CheckedPatch {
    pub name: String,
    pub repo: Handle,
    pub verb: PatchVerb,
    pub patch: Vec<u8>,
    pub expect: Vec<ExpectOid>,
}

impl Job for CheckedPatch {
    fn name(&self) -> &str {
        &self.name
    }

    fn run(self: Box<Self>) -> Result<(), String> {
        let repo = self.repo.as_ref();
        for e in &self.expect {
            let now = match e.side {
                OidSide::Index => repo.index_blob_oid(&e.path)?,
                OidSide::Head => repo.head_blob_oid(&e.path)?,
            };
            if now != e.oid {
                return Err(format!(
                    "{} changed since the patch was read — refresh (R) and try again",
                    String::from_utf8_lossy(&e.path)
                ));
            }
        }
        match self.verb {
            PatchVerb::Stage | PatchVerb::ApplyIndex => repo.stage_patch(&self.patch),
            PatchVerb::Unstage | PatchVerb::ReverseIndex => repo.unstage_patch(&self.patch),
            PatchVerb::Discard | PatchVerb::ReverseWorktree => repo.discard_patch(&self.patch),
            PatchVerb::ApplyWorktree => repo.apply_patch(&self.patch),
        }
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
        /// The stack entry the keyboard is on, for the stash verbs.
        stash: Option<StashId>,
        /// The operation the client last acquired, for the verbs that refuse
        /// to start a second write inside git's first.
        standing: Option<Operation>,
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
                stash: None,
                standing: None,
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
        fn operation(&self) -> Option<Operation> {
            self.standing
        }

        fn confirm_or_arm(&mut self, _: &str, _: &[u8]) -> bool {
            self.confirm_calls += 1;
            self.confirmations.pop_front().unwrap_or(false)
        }

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

    impl StashClient for Fake {
        fn selected_stash(&self) -> Option<StashId> {
            self.stash.clone()
        }

        fn confirm_or_arm_stash(&mut self, _: &StashId) -> bool {
            self.confirm_calls += 1;
            self.confirmations.pop_front().unwrap_or(false)
        }
    }

    fn entry(index: usize, commit: &str) -> StashId {
        StashId {
            index,
            commit: commit.into(),
        }
    }

    #[test]
    fn every_stash_scope_builds_a_job_that_names_what_it_takes() {
        for (scope, named) in [
            (StashScope::Tracked, "stash the working tree"),
            (
                StashScope::WithUntracked,
                "stash the working tree and its new files",
            ),
            (StashScope::Staged, "stash the staged side"),
            (StashScope::Unstaged, "stash the unstaged side"),
            (
                StashScope::Path {
                    path: "src/x.rs".into(),
                    untracked: false,
                },
                "stash src/x.rs",
            ),
        ] {
            let mut client = Fake::with(None);
            stash_scoped(&mut client, Some("wip".into()), scope);
            assert_eq!(client.jobs, [named]);
            assert!(client.said.is_empty(), "{:?}", client.said);
        }
    }

    #[test]
    fn a_scoped_stash_over_a_fixture_refuses_and_queues_nothing() {
        let mut client = Fake::with(None);
        client.repo = None;
        stash_scoped(&mut client, None, StashScope::Staged);
        assert_eq!(client.said, ["a fixture has no working tree to park"]);
        assert!(client.jobs.is_empty());
    }

    #[test]
    fn a_scoped_stash_does_not_refuse_a_standing_operation() {
        // The whole family answers the same way, and files.stash does not
        // guard: the one mid-operation state with work to park is a
        // conflicted one, and git refuses that itself.
        let mut client = Fake::with(None);
        client.standing = Some(Operation {
            kind: gitten_core::operation::Kind::Rebase,
            conflicts: 0,
        });
        stash_scoped(&mut client, None, StashScope::Tracked);
        assert_eq!(client.jobs, ["stash the working tree"]);
    }

    #[test]
    fn the_stash_verbs_refuse_a_fixture_before_a_missing_row() {
        // The repository first, because "a fixture has no stack" is truer
        // about a pane with no rows than "nothing selected" is.
        for (name, run) in [
            ("apply", apply_stash as fn(&mut Fake)),
            ("pop", pop_stash as fn(&mut Fake)),
            ("drop", drop_stash as fn(&mut Fake)),
        ] {
            let mut client = Fake::with(None);
            client.repo = None;
            client.stash = Some(entry(0, "abc"));
            client.confirmations.push_back(true);
            run(&mut client);
            assert_eq!(client.said, [format!("a fixture has no stash to {name}")]);
            assert_eq!(client.confirm_calls, 0, "the arm was not spent");
            assert!(client.jobs.is_empty());

            let mut empty = Fake::with(None);
            run(&mut empty);
            assert_eq!(empty.said, ["nothing selected on the stash stack"]);
            assert!(empty.jobs.is_empty());
        }
    }

    #[test]
    fn apply_and_pop_act_on_the_first_press_and_drop_asks_twice() {
        let mut client = Fake::with(None);
        client.stash = Some(entry(2, "cafe"));
        apply_stash(&mut client);
        pop_stash(&mut client);
        assert_eq!(
            client.jobs,
            ["stash apply stash@{2}", "stash pop stash@{2}"]
        );
        assert!(client.asked.is_empty(), "neither destroys anything");

        let mut dropping = Fake::with(None);
        dropping.stash = Some(entry(2, "cafe"));
        dropping.confirmations = [false, true].into();
        drop_stash(&mut dropping);
        assert_eq!(dropping.asked, ["drop stash@{2}? press again to confirm"]);
        assert!(dropping.jobs.is_empty());
        drop_stash(&mut dropping);
        assert_eq!(dropping.asked.len(), 1);
        assert_eq!(dropping.jobs, ["stash drop stash@{2}"]);
    }

    #[test]
    fn a_rename_refuses_an_empty_message_before_it_reaches_git() {
        let mut client = Fake::with(None);
        for blank in ["", "   ", "\n"] {
            rename_stash(&mut client, entry(0, "abc"), blank.into());
        }
        assert_eq!(client.said.len(), 3, "{:?}", client.said);
        assert!(client.said.iter().all(|s| s == "a stash needs a message"));
        assert!(client.jobs.is_empty());

        rename_stash(&mut client, entry(1, "abc"), "the parser one".into());
        assert_eq!(client.jobs, ["rename stash@{1}"]);
    }

    #[test]
    fn a_branch_from_a_stash_refuses_an_empty_name_and_a_standing_operation() {
        let mut blank = Fake::with(None);
        branch_from_stash(&mut blank, entry(0, "abc"), "  ".into());
        assert_eq!(blank.said, ["a branch needs a name"]);
        assert!(blank.jobs.is_empty());

        // It checks out, so it waits for git's own first write like every
        // other checkout here does.
        let mut operating = Fake::with(None);
        operating.standing = Some(Operation {
            kind: gitten_core::operation::Kind::Rebase,
            conflicts: 0,
        });
        branch_from_stash(&mut operating, entry(0, "abc"), "wip".into());
        assert_eq!(
            operating.said,
            ["a rebase is in progress; finish or abort it before starting another"]
        );
        assert!(operating.jobs.is_empty());

        let mut client = Fake::with(None);
        branch_from_stash(&mut client, entry(3, "abc"), " wip ".into());
        assert_eq!(client.jobs, ["branch wip from stash@{3}"]);
    }

    #[test]
    fn a_stash_verb_says_so_when_the_queue_is_gone() {
        for run in [
            apply_stash as fn(&mut Fake),
            pop_stash as fn(&mut Fake),
            drop_stash as fn(&mut Fake),
        ] {
            let mut client = Fake::with(None);
            client.stash = Some(entry(0, "abc"));
            client.submit_ok = false;
            client.confirmations.push_back(true);
            run(&mut client);
            assert_eq!(client.said, ["the job queue is shutting down"]);
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
    fn a_remote_row_routes_to_remote_deletion() {
        // One key, and the row decides: a remote-tracking row under `d`
        // asks the remote question, never the local one.
        let mut client = Fake::with(Some(Target::Remote {
            remote: RefName::from("origin"),
            branch: RefName::from("main"),
        }));
        client.confirmations = [false, true].into();
        delete_branch(&mut client);
        assert_eq!(
            client.asked,
            ["delete origin/main on origin? the local branch stays — press again to confirm"]
        );
        assert!(client.jobs.is_empty());
        delete_branch(&mut client);
        assert_eq!(client.jobs.len(), 1);
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

    // ----------------------------------------------------------- hunk jobs

    use std::sync::Mutex;

    /// A repository that exists only as this struct, for the hunk policy:
    /// the side reads answer what the test handed in, the OID reads answer
    /// the identity the test set, and the three patch verbs record the
    /// bytes they were asked to apply. Nothing here runs git — the same
    /// shape `tui`'s and `verbs`' fakes take.
    #[derive(Default)]
    struct HunkState {
        staged: Vec<gitten_git::Pair>,
        unstaged: Vec<gitten_git::Pair>,
        untracked: Option<gitten_git::Pair>,
        index_oids: Vec<(String, String)>,
        head_oids: Vec<(String, String)>,
        writes: Vec<String>,
    }

    struct HunkFake(Arc<Mutex<HunkState>>);

    impl gitten_git::Repo for HunkFake {
        fn log(&self, _limit: usize) -> gitten_git::Result<Vec<gitten_core::Commit>> {
            Ok(Vec::new())
        }

        fn pairs(&self, _revspec: &str) -> gitten_git::Result<Vec<gitten_git::Pair>> {
            Ok(Vec::new())
        }

        fn status(&self) -> gitten_git::Result<gitten_core::status::Status> {
            Ok(Default::default())
        }

        fn describe(&self) -> String {
            "hunk fake".into()
        }

        fn pairs_staged(&self, path: Option<&[u8]>) -> gitten_git::Result<Vec<gitten_git::Pair>> {
            let s = self.0.lock().unwrap();
            Ok(match path {
                Some(p) => s
                    .staged
                    .iter()
                    .filter(|c| c.path.as_bytes() == p)
                    .cloned()
                    .collect(),
                None => s.staged.clone(),
            })
        }

        fn pairs_unstaged(&self, path: Option<&[u8]>) -> gitten_git::Result<Vec<gitten_git::Pair>> {
            let s = self.0.lock().unwrap();
            Ok(match path {
                Some(p) => s
                    .unstaged
                    .iter()
                    .filter(|c| c.path.as_bytes() == p)
                    .cloned()
                    .collect(),
                None => s.unstaged.clone(),
            })
        }

        fn pair_untracked(&self, path: &[u8]) -> gitten_git::Result<Option<gitten_git::Pair>> {
            let s = self.0.lock().unwrap();
            Ok(s.untracked
                .as_ref()
                .filter(|p| p.path.as_bytes() == path)
                .cloned())
        }

        fn index_blob_oid(&self, path: &[u8]) -> gitten_git::Result<Option<String>> {
            let shown = String::from_utf8_lossy(path).into_owned();
            Ok(self
                .0
                .lock()
                .unwrap()
                .index_oids
                .iter()
                .find(|(p, _)| p == &shown)
                .map(|(_, o)| o.clone()))
        }

        fn head_blob_oid(&self, path: &[u8]) -> gitten_git::Result<Option<String>> {
            let shown = String::from_utf8_lossy(path).into_owned();
            Ok(self
                .0
                .lock()
                .unwrap()
                .head_oids
                .iter()
                .find(|(p, _)| p == &shown)
                .map(|(_, o)| o.clone()))
        }

        fn stage_patch(&self, patch: &[u8]) -> gitten_git::Result<()> {
            self.0
                .lock()
                .unwrap()
                .writes
                .push(format!("stage {}", String::from_utf8_lossy(patch)));
            Ok(())
        }

        fn unstage_patch(&self, patch: &[u8]) -> gitten_git::Result<()> {
            self.0
                .lock()
                .unwrap()
                .writes
                .push(format!("unstage {}", String::from_utf8_lossy(patch)));
            Ok(())
        }

        fn discard_patch(&self, patch: &[u8]) -> gitten_git::Result<()> {
            self.0
                .lock()
                .unwrap()
                .writes
                .push(format!("discard {}", String::from_utf8_lossy(patch)));
            Ok(())
        }
    }

    /// One unstaged pair with an identity — the index holds `i0`, the
    /// worktree replaces the third line — and the hunk the pipeline draws
    /// from it, via the same `diff_pairs` the verb re-reads with, so the
    /// selection matches by construction.
    fn unstaged_fixture() -> (Arc<HunkFake>, Hunk) {
        let pair = gitten_git::Pair {
            path: "f.txt".into(),
            old_path: None,
            status: 'M',
            old: vec![
                Arc::<str>::from("alpha"),
                Arc::<str>::from("keep one"),
                Arc::<str>::from("keep two"),
            ],
            new: vec![
                Arc::<str>::from("alpha"),
                Arc::<str>::from("keep one"),
                Arc::<str>::from("WORKTREE TWO"),
            ],
            old_oid: Some("i0".into()),
            new_oid: None,
            old_final_newline: true,
            new_final_newline: true,
            binary: false,
        };
        let fake = Arc::new(HunkFake(Arc::new(Mutex::new(HunkState {
            unstaged: vec![pair],
            index_oids: vec![("f.txt".into(), "i0".into())],
            ..Default::default()
        }))));
        let differs = gitten_core::differ::Differs::builtin();
        let staged = fake.pairs_unstaged(Some(b"f.txt")).expect("the side");
        let files = gitten_git::diff_pairs(
            &staged,
            &differs,
            &gitten_core::differ::Overrides::default(),
        );
        let hunk = files[0].hunks[0].clone();
        (fake, hunk)
    }

    fn run_hunk(
        repo: &gitten_git::Handle,
        command: &str,
        side: HunkSide,
        selection: HunkSelection,
        path: &str,
    ) -> Result<(), String> {
        let differs = gitten_core::differ::Differs::builtin();
        let job = hunk_job(
            command,
            HunkAsk {
                path: path.into(),
                side,
                selection,
            },
            repo,
            &differs,
            &Default::default(),
        )?;
        job.run()
    }

    #[test]
    fn a_hunk_verb_is_refused_where_it_means_nothing() {
        let (fake, hunk) = unstaged_fixture();
        let whole = HunkSelection::Whole(hunk.clone());
        for (command, side, fragment) in [
            (
                "diff.stage-hunk",
                HunkSide::Combined,
                "combined view folds both sides",
            ),
            (
                "diff.unstage-hunk",
                HunkSide::Combined,
                "combined view folds both sides",
            ),
            ("diff.stage-hunk", HunkSide::Staged, "already staged"),
            (
                "diff.unstage-hunk",
                HunkSide::Unstaged,
                "tab opens the staged side",
            ),
            (
                "diff.unstage-hunk",
                HunkSide::Untracked,
                "nothing in the index",
            ),
            ("diff.stage-hunk", HunkSide::Untracked, "stages whole"),
            (
                "diff.discard-hunk",
                HunkSide::Staged,
                "no working tree to discard",
            ),
            ("diff.discard-hunk", HunkSide::Untracked, "all or nothing"),
            ("diff.nope-hunk", HunkSide::Unstaged, "is not a hunk verb"),
        ] {
            let repo: gitten_git::Handle = fake.clone();
            let err = run_hunk(&repo, command, side, whole.clone(), "f.txt").expect_err("refused");
            assert!(err.contains(fragment), "{command} on {side:?}: {err}");
            assert!(
                fake.0.lock().unwrap().writes.is_empty(),
                "a refusal queued a write"
            );
        }
    }

    #[test]
    fn a_stage_is_built_from_the_unstaged_side_and_checked_at_write() {
        let (fake, hunk) = unstaged_fixture();
        let repo: gitten_git::Handle = fake.clone();
        run_hunk(
            &repo,
            "diff.stage-hunk",
            HunkSide::Unstaged,
            HunkSelection::Whole(hunk.clone()),
            "f.txt",
        )
        .expect("the hunk stages");
        let writes = fake.0.lock().unwrap().writes.clone();
        assert_eq!(writes.len(), 1, "{writes:?}");
        assert!(writes[0].starts_with("stage "), "{writes:?}");
        // The patch says exactly the hunk: the change, its context, no
        // neighbour.
        let applied = gitten_core::parse_unified_diff(&writes[0]["stage ".len()..]);
        assert_eq!(applied.len(), 1);
        assert_eq!(applied[0].path, "f.txt");
        assert_eq!(applied[0].hunks.len(), 1);
        assert_eq!(applied[0].hunks[0].lines, hunk.lines);
    }

    #[test]
    fn a_drifted_index_refuses_the_write() {
        let (fake, hunk) = unstaged_fixture();
        // The patch was built against i0; the index now holds something
        // else. The write checks before it aims.
        fake.0.lock().unwrap().index_oids = vec![("f.txt".into(), "moved-on".into())];
        let repo: gitten_git::Handle = fake.clone();
        let err = run_hunk(
            &repo,
            "diff.stage-hunk",
            HunkSide::Unstaged,
            HunkSelection::Whole(hunk),
            "f.txt",
        )
        .expect_err("the drifted write refuses");
        assert!(err.contains("changed since the patch was read"), "{err}");
        assert!(
            fake.0.lock().unwrap().writes.is_empty(),
            "a refused write applied nothing"
        );
    }

    #[test]
    fn a_stale_selection_refuses_before_anything_is_read_for_the_write() {
        let (fake, hunk) = unstaged_fixture();
        // The drawn hunk names a change the fresh pair no longer shows:
        // the fake's content moved between the preview and the keypress.
        fake.0.lock().unwrap().unstaged[0].new[2] = Arc::from("MOVED ON");
        let repo: gitten_git::Handle = fake.clone();
        let err = run_hunk(
            &repo,
            "diff.stage-hunk",
            HunkSide::Unstaged,
            HunkSelection::Whole(hunk),
            "f.txt",
        )
        .expect_err("the stale selection refuses");
        assert!(err.contains("changed since this diff was drawn"), "{err}");
        assert!(
            fake.0.lock().unwrap().writes.is_empty(),
            "a stale selection queued nothing"
        );
    }

    #[test]
    fn a_line_selection_stages_only_the_marked_lines() {
        // One added line chosen out of a hunk that also removes a line:
        // the removal is unchosen, so it stays in the index as context and
        // the patch inserts the addition alone.
        let (fake, _) = unstaged_fixture();
        // Widen the fixture to a replacement: the hunk now removes and adds.
        {
            let mut s = fake.0.lock().unwrap();
            let pair = &mut s.unstaged[0];
            pair.new[2] = Arc::from("WORKTREE TWO");
            pair.old[2] = Arc::from("keep two");
        }
        let differs = gitten_core::differ::Differs::builtin();
        let staged = fake.pairs_unstaged(Some(b"f.txt")).expect("the side");
        let files = gitten_git::diff_pairs(
            &staged,
            &differs,
            &gitten_core::differ::Overrides::default(),
        );
        let hunk = files[0].hunks[0].clone();
        let plus = hunk
            .lines
            .iter()
            .position(|l| l.kind == gitten_core::LineKind::Added)
            .expect("the addition");
        let repo: gitten_git::Handle = fake.clone();
        run_hunk(
            &repo,
            "diff.stage-hunk",
            HunkSide::Unstaged,
            HunkSelection::Lines(vec![(hunk, plus, plus)]),
            "f.txt",
        )
        .expect("the line stages");
        let writes = fake.0.lock().unwrap().writes.clone();
        let applied = gitten_core::parse_unified_diff(&writes[0]["stage ".len()..]);
        let lines = &applied[0].hunks[0].lines;
        let changed: Vec<&str> = lines
            .iter()
            .filter(|l| l.kind != gitten_core::LineKind::Context)
            .map(|l| l.text.as_ref())
            .collect();
        assert_eq!(changed, ["WORKTREE TWO"], "only the marked line travels");
        let kinds: Vec<gitten_core::LineKind> = lines.iter().map(|l| l.kind).collect();
        assert!(
            kinds.contains(&gitten_core::LineKind::Context),
            "the unchosen removal rides as context, keeping the preimage whole: {kinds:?}"
        );
    }
}
