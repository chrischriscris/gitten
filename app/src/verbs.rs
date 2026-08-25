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
use gitten_core::rebase::TodoScript;
use gitten_core::refs::{HeadState, Remote, ResetMode};
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
    /// The sentence a clean finish announces, when this verb's effect lands
    /// somewhere the eye is not. `None` for everything whose result shows
    /// itself in the pane it changed — a staged file needs no announcer.
    done: Option<String>,
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
            done: None,
        }
    }

    /// Names the sentence a clean finish announces. The sync verbs' extra
    /// word: their effect is counts on branches the reader may not be
    /// looking at, and quiet about those reads as nothing happened.
    fn announcing(mut self, done: impl Into<String>) -> Self {
        self.done = Some(done.into());
        self
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

    /// Stages exactly what a synthesized patch describes onto the index —
    /// `git apply --cached`. The bytes are [`gitten_core::patch::emit`]'s
    /// output and travel untouched; an empty patch is refused here rather
    /// than queued, because "nothing selected" is a sentence for now and
    /// git's answer to zero bytes says nothing at all.
    pub fn stage_patch(repo: &Handle, patch: Vec<u8>) -> Result<Self, String> {
        if patch.is_empty() {
            return Err("an empty patch stages nothing".into());
        }
        Ok(Self::named("stage patch".into(), repo, move |r| {
            r.stage_patch(&patch)
        }))
    }

    /// Removes exactly what the patch describes from the index — the
    /// `--cached --reverse` spelling of [`Write::stage_patch`], on the same
    /// terms: bytes end to end, emptiness refused before the queue.
    pub fn unstage_patch(repo: &Handle, patch: Vec<u8>) -> Result<Self, String> {
        if patch.is_empty() {
            return Err("an empty patch unstages nothing".into());
        }
        Ok(Self::named("unstage patch".into(), repo, move |r| {
            r.unstage_patch(&patch)
        }))
    }

    /// Removes exactly what the patch describes from the working tree —
    /// `git apply --reverse` without `--cached`, so nothing staged moves.
    /// DESTRUCTIVE: the caller confirms before this job is ever built.
    pub fn discard_patch(repo: &Handle, patch: Vec<u8>) -> Result<Self, String> {
        if patch.is_empty() {
            return Err("an empty patch discards nothing".into());
        }
        Ok(Self::named("discard patch".into(), repo, move |r| {
            r.discard_patch(&patch)
        }))
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

    /// Rewrites HEAD to hold the staged changes under `message` — commit's
    /// mechanics aimed one step back. The replacement OID ends here for the
    /// same reason [`Write::commit`]'s does: the finish line is a generation
    /// bump, and the new sha arrives with the refreshed pane.
    pub fn amend(repo: &Handle, message: String) -> Self {
        Self::named("amend".into(), repo, move |r| r.amend(&message).map(|_| ()))
            // The rewritten history shows up in a pane that may not be focused —
            // the key lives over the working tree — so this one says what it did.
            .announcing("amended HEAD")
    }

    /// Rewrites this branch by installing `script` as git's own
    /// interactive-rebase plan over `upstream` — reorder, squash, fixup,
    /// drop and exec between picks, exactly as the plan says. The plan was
    /// composed in [`gitten_core::rebase::compose`], which refuses every
    /// shape it cannot complete; the trait refuses again before any process
    /// runs. A conflict mid-rewrite comes back refused in git's words with
    /// rebase state left standing; [`Write::rebase_abort`] undoes that.
    /// DESTRUCTIVE: the caller confirms before this job is ever built.
    pub fn rebase_todo(repo: &Handle, upstream: Vec<u8>, script: TodoScript) -> Self {
        let shown = String::from_utf8_lossy(&upstream).into_owned();
        Self::named(format!("rebase onto {shown}"), repo, move |r| {
            r.rebase_todo(&upstream, &script)
        })
        .announcing(format!("rebased onto {shown}"))
    }

    /// Moves the current branch onto `upstream`, replaying its own commits:
    /// the non-interactive sibling of [`Write::rebase_todo`], on the same
    /// honesty terms — a dirty tree is git's refusal verbatim, a conflict
    /// leaves its question standing, no force anywhere. DESTRUCTIVE: the
    /// caller confirms before this job is ever built.
    pub fn rebase_onto(repo: &Handle, upstream: Vec<u8>) -> Self {
        let shown = String::from_utf8_lossy(&upstream).into_owned();
        Self::named(format!("rebase onto {shown}"), repo, move |r| {
            r.rebase_onto(&upstream)
        })
        .announcing(format!("rebased onto {shown}"))
    }

    /// Abandons an in-progress rebase and puts branch, index and working
    /// tree back where they started — git's own guarantee. Nothing here to
    /// confirm: it only ever runs after a refusal named the state it cleans.
    pub fn rebase_abort(repo: &Handle) -> Self {
        Self::named("rebase abort".into(), repo, |r| r.rebase_abort()).announcing("rebase aborted")
    }

    /// Carries an in-progress rebase onward once a human has resolved
    /// whatever stopped it — both editors answered `true` by the trait, so
    /// continuing from here means "carry on with what is here", never
    /// "open another window". A further conflict comes back refused in
    /// git's words with the state still standing, ready to be driven again.
    pub fn rebase_continue(repo: &Handle) -> Self {
        Self::named("rebase continue".into(), repo, |r| r.rebase_continue())
            .announcing("rebase continued")
    }

    /// Applies one commit onto the current branch as a new commit. Nothing
    /// existing moves — dropping the copy undoes the pick — so no
    /// confirmation precedes it, and a conflict comes back refused in git's
    /// own words with its question left standing for
    /// [`Write::cherry_pick_abort`] or [`Write::cherry_pick_continue`].
    pub fn cherry_pick(repo: &Handle, sha: Vec<u8>) -> Self {
        let shown = String::from_utf8_lossy(&sha).into_owned();
        Self::named(format!("cherry-pick {shown}"), repo, move |r| {
            r.cherry_pick(&sha)
        })
        .announcing(format!("picked {shown}"))
    }

    /// Abandons an in-progress cherry-pick and puts branch, index and
    /// working tree back where the pick started — git's own guarantee.
    /// Nothing here to confirm: it only ever runs after a refusal named the
    /// state it cleans.
    pub fn cherry_pick_abort(repo: &Handle) -> Self {
        Self::named("cherry-pick abort".into(), repo, |r| r.cherry_pick_abort())
            .announcing("cherry-pick aborted")
    }

    /// Carries an in-progress cherry-pick onward once a human has resolved
    /// whatever stopped it; a further conflict comes back refused in git's
    /// words with the state still standing, ready to drive again.
    pub fn cherry_pick_continue(repo: &Handle) -> Self {
        Self::named("cherry-pick continue".into(), repo, |r| {
            r.cherry_pick_continue()
        })
        .announcing("cherry-pick continued")
    }

    /// Moves the current branch onto `target`, taking as much of the index
    /// and working tree along as `mode` says. Soft and mixed keep every
    /// change on disk or in the reflog; hard destroys unstaged work, which
    /// is why the caller confirms before this job is ever built.
    pub fn reset(repo: &Handle, mode: ResetMode, target: Vec<u8>) -> Self {
        let shown = String::from_utf8_lossy(&target).into_owned();
        Self::named(format!("reset {} {shown}", mode.flag()), repo, move |r| {
            r.reset(mode, &target)
        })
        // The branch moved somewhere the files pane cannot show; the band
        // carries the destination, in git's own flag spelling.
        .announcing(format!("reset {} to {shown}", mode.flag()))
    }

    /// Undoes one commit by landing its inverse as a new commit. Nothing is
    /// destroyed — dropping the result undoes the undo — so no confirmation
    /// precedes it, and a conflict comes back refused in git's own words.
    pub fn revert(repo: &Handle, commit: Vec<u8>) -> Self {
        let shown = String::from_utf8_lossy(&commit).into_owned();
        Self::named(format!("revert {shown}"), repo, move |r| r.revert(&commit))
            .announcing(format!("reverted {shown}"))
    }

    /// Moves HEAD onto the named branch. The name is bytes end to end — what
    /// the panel read is what git is aimed at.
    pub fn checkout(repo: &Handle, name: Vec<u8>) -> Self {
        let shown = String::from_utf8_lossy(&name).into_owned();
        Self::named(format!("checkout {shown}"), repo, move |r| {
            r.checkout(&name)
        })
    }

    /// Creates a local branch at `start`, or at HEAD when there is none.
    /// Nothing is checked out; HEAD stays where it was.
    pub fn create_branch(repo: &Handle, name: Vec<u8>, start: Option<Vec<u8>>) -> Self {
        let shown = String::from_utf8_lossy(&name).into_owned();
        Self::named(format!("branch {shown}"), repo, move |r| {
            r.create_branch(&name, start.as_deref())
        })
    }

    /// Deletes a local branch. Merged work only unless `force` — an unmerged
    /// one comes back refused in git's own words.
    pub fn delete_branch(repo: &Handle, name: Vec<u8>, force: bool) -> Self {
        let shown = String::from_utf8_lossy(&name).into_owned();
        let word = match force {
            true => "force-delete branch",
            false => "delete branch",
        };
        Self::named(format!("{word} {shown}"), repo, move |r| {
            r.delete_branch(&name, force)
        })
    }

    /// Renames a local branch — git's `-m`, which moves ref, config and
    /// upstream link together.
    pub fn rename_branch(repo: &Handle, from: Vec<u8>, to: Vec<u8>) -> Self {
        let from_shown = String::from_utf8_lossy(&from).into_owned();
        let to_shown = String::from_utf8_lossy(&to).into_owned();
        Self::named(
            format!("rename {from_shown} → {to_shown}"),
            repo,
            move |r| r.rename_branch(&from, &to),
        )
    }

    /// Names `target` with a tag — annotated carrying `message` when one is
    /// given, lightweight otherwise. The name travels as bytes end to end; a
    /// duplicate comes back refused in git's own words.
    pub fn create_tag(
        repo: &Handle,
        name: Vec<u8>,
        target: Vec<u8>,
        message: Option<String>,
    ) -> Self {
        let shown = String::from_utf8_lossy(&name).into_owned();
        Self::named(format!("tag {shown}"), repo, move |r| {
            r.create_tag(&name, &target, message.as_deref())
        })
    }

    /// Deletes one tag — a name and not a home, so every commit it pointed
    /// at survives. No tags pane exists yet for anything built-in to aim
    /// this from; it sits here on the same rails as its siblings so the
    /// tags pane (a future wave) and any extension reach it through the one
    /// door, never a private path.
    #[allow(dead_code)]
    pub fn delete_tag(repo: &Handle, name: Vec<u8>) -> Self {
        let shown = String::from_utf8_lossy(&name).into_owned();
        Self::named(format!("untag {shown}"), repo, move |r| r.delete_tag(&name))
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

    // ------------------------------------------------------------ the sync

    /// Sends `branch` to `remote` — `git push -q`, adding `--set-upstream`
    /// exactly when the branch tracks nothing yet. Whether it does is the
    /// trait's decision, read fresh from the repository, never remembered
    /// here; this wrapper's whole job is to carry the two names as bytes and
    /// say the band's words.
    pub fn push(repo: &Handle, remote: Vec<u8>, branch: Vec<u8>) -> Self {
        let shown = shown_pair(&remote, &branch);
        Self::named(format!("push {shown}"), repo, move |r| {
            r.push(&remote, &branch)
        })
        .announcing(format!("pushed {shown}"))
    }

    /// Fast-forwards the current branch onto its upstream — `git pull
    /// --ff-only`. Which branch pulls from where is the repository's own
    /// configuration, so there are no arguments to pass and no refusals to
    /// pre-empt: a divergence comes back in git's words, never auto-rebased.
    pub fn pull(repo: &Handle) -> Self {
        Self::named("pull".into(), repo, |r| r.pull()).announcing("pulled")
    }

    /// Updates remote-tracking refs — `remote` when named, every remote this
    /// repository knows when not. Fetching moves nothing but those refs,
    /// which is what makes it safe behind a single unconfirmed key.
    pub fn fetch(repo: &Handle, remote: Option<Vec<u8>>) -> Self {
        let shown = match &remote {
            Some(remote) => format!(" {}", String::from_utf8_lossy(remote)),
            None => String::new(),
        };
        Self::named(format!("fetch{shown}"), repo, move |r| {
            r.fetch(remote.as_deref())
        })
        .announcing(format!("fetched{shown}"))
    }

    /// `repo.push`'s verb, aimed: HEAD's branch, sent to the remote its
    /// upstream names. When the branch tracks nothing yet, `origin` stands
    /// in if the repository has one, else its sole remote — a guess among
    /// several servers is how work lands on somebody else's machine, so
    /// none is made. The reads ride the same [`Repo`] every client drives;
    /// they run here, before the queue, where a refusal costs one sentence
    /// instead of a job.
    pub fn push_current(repo: &Handle) -> Result<Self, String> {
        let branch = match repo.head()? {
            HeadState::Branch { name, .. } => name,
            HeadState::Detached { .. } => return Err("detached HEAD has no branch to push".into()),
        };
        let tracked = repo
            .branches()?
            .iter()
            .find(|b| b.name.as_bytes() == branch.as_bytes())
            .and_then(|b| b.upstream.as_ref())
            .map(|u| u.remote.as_bytes().to_vec());
        let remote = match tracked {
            Some(remote) => remote,
            None => default_remote(&repo.remotes()?)?,
        };
        Ok(Self::push(repo, remote, branch.as_bytes().to_vec()))
    }
}

/// Two byte-names as a person reads them, once: the band's words and the
/// finish line's both.
fn shown_pair(remote: &[u8], branch: &[u8]) -> String {
    format!(
        "{} {}",
        String::from_utf8_lossy(remote),
        String::from_utf8_lossy(branch)
    )
}

/// The remote a first push means when no configuration says: `origin` if the
/// repository has one, its only remote when exactly one, a refusal otherwise.
fn default_remote(remotes: &[Remote]) -> Result<Vec<u8>, String> {
    if let Some(origin) = remotes.iter().find(|r| r.name.as_bytes() == b"origin") {
        return Ok(origin.name.as_bytes().to_vec());
    }
    if let [only] = remotes {
        return Ok(only.name.as_bytes().to_vec());
    }
    Err(
        "this branch has no upstream and no single remote stands out; \
         push it from the branches panel to set one"
            .into(),
    )
}

impl Job for Write {
    fn name(&self) -> &str {
        &self.name
    }

    fn confirmation(&self) -> Option<String> {
        self.done.clone()
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
    use gitten_core::refs::{Branch, HeadState, RefName, Remote, Upstream};
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
        fn reset(&self, mode: ResetMode, target: &[u8]) -> gitten_git::Result<()> {
            self.0.lock().unwrap().push(format!(
                "reset {} {}",
                mode.flag(),
                String::from_utf8_lossy(target)
            ));
            Ok(())
        }
        fn revert(&self, commit: &[u8]) -> gitten_git::Result<()> {
            self.0
                .lock()
                .unwrap()
                .push(format!("revert {}", String::from_utf8_lossy(commit)));
            Ok(())
        }
        fn amend(&self, message: &str) -> gitten_git::Result<String> {
            self.0.lock().unwrap().push(format!("amend {message}"));
            Ok("f00d".into())
        }
        fn checkout(&self, name: &[u8]) -> gitten_git::Result<()> {
            self.0
                .lock()
                .unwrap()
                .push(format!("checkout {}", String::from_utf8_lossy(name)));
            Ok(())
        }
        fn create_branch(&self, name: &[u8], start: Option<&[u8]>) -> gitten_git::Result<()> {
            let start = start.map(|s| format!(" at {}", String::from_utf8_lossy(s)));
            self.0.lock().unwrap().push(format!(
                "branch {}{}",
                String::from_utf8_lossy(name),
                start.unwrap_or_default()
            ));
            Ok(())
        }
        fn delete_branch(&self, name: &[u8], force: bool) -> gitten_git::Result<()> {
            let word = match force {
                true => "force-delete",
                false => "delete",
            };
            self.0
                .lock()
                .unwrap()
                .push(format!("{word} {}", String::from_utf8_lossy(name)));
            Ok(())
        }
        fn rename_branch(&self, from: &[u8], to: &[u8]) -> gitten_git::Result<()> {
            self.0.lock().unwrap().push(format!(
                "rename {} {}",
                String::from_utf8_lossy(from),
                String::from_utf8_lossy(to)
            ));
            Ok(())
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
    fn the_history_verbs_reach_the_trait_and_announce_where_history_went() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let repo: Handle = Arc::new(Recording(Arc::clone(&calls)));
        let runner = Runner::new();
        let submit = runner.submitter();

        let mut jobs: Vec<Box<dyn Job>> = vec![
            Box::new(Write::reset(&repo, ResetMode::Soft, b"abc1234".to_vec())),
            Box::new(Write::reset(&repo, ResetMode::Hard, b"HEAD~1".to_vec())),
            Box::new(Write::revert(&repo, b"abc1234".to_vec())),
            Box::new(Write::amend(&repo, "rewritten\n\nbody".into())),
        ];
        for job in jobs.drain(..) {
            assert!(submit.submit(job).is_ok());
        }

        let deadline = Instant::now() + Duration::from_secs(2);
        while recorded(&calls).len() < 4 {
            assert!(
                Instant::now() < deadline,
                "jobs did not run: {:?}",
                recorded(&calls)
            );
            std::thread::yield_now();
        }
        // The mode travels as the concept, and git's flag spelling is chosen
        // where git is called — the band borrows it only to speak.
        assert_eq!(
            recorded(&calls),
            vec![
                "reset --soft abc1234",
                "reset --hard HEAD~1",
                "revert abc1234",
                "amend rewritten\n\nbody",
            ]
        );

        let (mut started, mut finished) = (Vec::new(), Vec::new());
        while let Some(event) = runner.try_next() {
            match event {
                Event::Started { name } => started.push(name),
                Event::Finished { done, .. } => finished.push(done),
            }
        }
        assert_eq!(
            started,
            vec![
                "reset --soft abc1234",
                "reset --hard HEAD~1",
                "revert abc1234",
                "amend"
            ]
        );
        // Their effects land in panes the keyboard was elsewhere over, so
        // each says what it did and where history went.
        assert_eq!(
            finished,
            vec![
                Some("reset --soft to abc1234".into()),
                Some("reset --hard to HEAD~1".into()),
                Some("reverted abc1234".into()),
                Some("amended HEAD".into()),
            ]
        );
    }

    #[test]
    fn the_patch_verbs_refuse_empty_and_reach_the_trait_bytes_intact() {
        // Empty in, refused out — before anything is queued, so no band ever
        // flashes "running" for work that cannot happen. Non-empty, the
        // patch travels whole: recorded as raw bytes, because a lossy log
        // could never tell a pass-through from a mangling.
        struct Patches(Arc<Mutex<Vec<Vec<u8>>>>);

        impl Repo for Patches {
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
                "patches".into()
            }
            fn stage_patch(&self, p: &[u8]) -> gitten_git::Result<()> {
                self.0
                    .lock()
                    .unwrap()
                    .push([b"s".to_vec(), p.to_vec()].concat());
                Ok(())
            }
            fn unstage_patch(&self, p: &[u8]) -> gitten_git::Result<()> {
                self.0
                    .lock()
                    .unwrap()
                    .push([b"u".to_vec(), p.to_vec()].concat());
                Ok(())
            }
            fn discard_patch(&self, p: &[u8]) -> gitten_git::Result<()> {
                self.0
                    .lock()
                    .unwrap()
                    .push([b"d".to_vec(), p.to_vec()].concat());
                Ok(())
            }
        }

        let calls = Arc::default();
        let repo: Handle = Arc::new(Patches(Arc::clone(&calls)));

        for verb in ["stage", "unstage", "discard"] {
            let err = (match verb {
                "stage" => Write::stage_patch(&repo, Vec::new()),
                "unstage" => Write::unstage_patch(&repo, Vec::new()),
                _ => Write::discard_patch(&repo, Vec::new()),
            })
            .err()
            .expect("empty refuses");
            assert!(err.contains("empty patch"), "{verb}: {err}");
        }
        assert!(calls.lock().unwrap().is_empty(), "refusals queued nothing");

        let patch = b"diff --git a/f b/f\n".to_vec();
        let mut jobs: Vec<Box<dyn Job>> = vec![
            Box::new(Write::stage_patch(&repo, patch.clone()).expect("non-empty")),
            Box::new(Write::unstage_patch(&repo, patch.clone()).expect("non-empty")),
            Box::new(Write::discard_patch(&repo, patch).expect("non-empty")),
        ];
        let runner = Runner::new();
        let submit = runner.submitter();
        for job in jobs.drain(..) {
            assert!(submit.submit(job).is_ok());
        }

        let deadline = Instant::now() + Duration::from_secs(2);
        while calls.lock().unwrap().len() < 3 {
            assert!(
                Instant::now() < deadline,
                "jobs did not run: {:?}",
                calls.lock().unwrap()
            );
            std::thread::yield_now();
        }
        assert_eq!(
            *calls.lock().unwrap(),
            vec![
                [b"s".to_vec(), b"diff --git a/f b/f\n".to_vec()].concat(),
                [b"u".to_vec(), b"diff --git a/f b/f\n".to_vec()].concat(),
                [b"d".to_vec(), b"diff --git a/f b/f\n".to_vec()].concat(),
            ],
            "every byte arrived undecoded"
        );

        // And the running bands say the verbs' own words — there is no path
        // to name, so the patch is not named either.
        let mut names = Vec::new();
        while let Some(event) = runner.try_next() {
            if let Event::Started { name } = event {
                names.push(name);
            }
        }
        assert_eq!(names, vec!["stage patch", "unstage patch", "discard patch"]);
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
            fn delete_branch(&self, _: &[u8], _: bool) -> gitten_git::Result<()> {
                Err("git branch -d feature: not fully merged".into())
            }
        }
        let repo: Handle = Arc::new(Broken);
        // The error text is the repository's own, verbatim — the layer that
        // knows why a write failed is the layer that says so.
        let job: Box<dyn Job> = Box::new(Write::commit(&repo, "m".into()));
        assert_eq!(job.run(), Err("hook declined".into()));
        let job: Box<dyn Job> = Box::new(Write::delete_branch(&repo, b"feature".to_vec(), false));
        assert_eq!(
            job.run(),
            Err("git branch -d feature: not fully merged".into())
        );
    }

    #[test]
    fn the_branch_verbs_reach_the_trait_bytes_intact() {
        // A Latin-1 branch name is legal git and illegal UTF-8; the verb's
        // one job on the way through is to not touch it. Recorded as raw
        // bytes for exactly that reason — a lossy log could never tell a
        // pass-through from a mangling.
        #[derive(Default)]
        struct Bytes(Arc<Mutex<Vec<Vec<u8>>>>);

        impl Bytes {
            fn push(&self, parts: &[&[u8]]) {
                let mut line = Vec::new();
                for (i, part) in parts.iter().enumerate() {
                    if i > 0 {
                        line.push(b' ');
                    }
                    line.extend_from_slice(part);
                }
                self.0.lock().unwrap().push(line);
            }
        }

        impl Repo for Bytes {
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
                "bytes".into()
            }
            fn checkout(&self, name: &[u8]) -> gitten_git::Result<()> {
                self.push(&[b"checkout", name]);
                Ok(())
            }
            fn create_branch(&self, name: &[u8], start: Option<&[u8]>) -> gitten_git::Result<()> {
                match start {
                    Some(start) => self.push(&[b"branch", name, b"at", start]),
                    None => self.push(&[b"branch", name]),
                }
                Ok(())
            }
            fn delete_branch(&self, name: &[u8], force: bool) -> gitten_git::Result<()> {
                match force {
                    true => self.push(&[b"delete!", name]),
                    false => self.push(&[b"delete", name]),
                }
                Ok(())
            }
            fn rename_branch(&self, from: &[u8], to: &[u8]) -> gitten_git::Result<()> {
                self.push(&[b"rename", from, to]);
                Ok(())
            }
        }

        let bytes = Arc::default();
        let repo: Handle = Arc::new(Bytes(Arc::clone(&bytes)));
        let runner = Runner::new();
        let submit = runner.submitter();
        let mut jobs: Vec<Box<dyn Job>> = vec![
            Box::new(Write::checkout(&repo, b"f\xe9ature".to_vec())),
            Box::new(Write::create_branch(&repo, b"feature".to_vec(), None)),
            Box::new(Write::create_branch(
                &repo,
                b"pinned".to_vec(),
                Some(b"HEAD~1".to_vec()),
            )),
            Box::new(Write::delete_branch(&repo, b"old".to_vec(), false)),
            Box::new(Write::delete_branch(&repo, b"stubborn".to_vec(), true)),
            Box::new(Write::rename_branch(
                &repo,
                b"a".to_vec(),
                b"b\xe9".to_vec(),
            )),
        ];
        for job in jobs.drain(..) {
            assert!(submit.submit(job).is_ok());
        }

        let deadline = Instant::now() + Duration::from_secs(2);
        while bytes.lock().unwrap().len() < 6 {
            assert!(
                Instant::now() < deadline,
                "jobs did not run: {:?}",
                bytes.lock().unwrap()
            );
            std::thread::yield_now();
        }
        assert_eq!(
            *bytes.lock().unwrap(),
            vec![
                b"checkout f\xe9ature".to_vec(),
                b"branch feature".to_vec(),
                b"branch pinned at HEAD~1".to_vec(),
                b"delete old".to_vec(),
                b"delete! stubborn".to_vec(),
                b"rename a b\xe9".to_vec(),
            ],
            "every byte arrived undecoded"
        );

        // And the band names are the verbs' own words over the display
        // spelling of the same bytes.
        let mut names = Vec::new();
        while let Some(event) = runner.try_next() {
            if let Event::Started { name } = event {
                names.push(name);
            }
        }
        assert_eq!(
            names,
            vec![
                "checkout f\u{FFFD}ature",
                "branch feature",
                "branch pinned",
                "delete branch old",
                "force-delete branch stubborn",
                "rename a → b\u{FFFD}",
            ]
        );
    }

    // ------------------------------------------------------------ the sync

    /// Serves the sync verbs and the reads [`Write::push_current`] aims
    /// them with. Recorded as raw byte lines, for the same reason its
    /// sibling above records bytes: a lossy log could never tell a
    /// pass-through from a mangling.
    struct SyncFake {
        calls: Arc<Mutex<Vec<Vec<u8>>>>,
        head: HeadState,
        branches: Vec<Branch>,
        remotes: Vec<Remote>,
    }

    impl SyncFake {
        /// One branch under HEAD, tracking `upstream` when named.
        fn tracked(upstream: Option<&str>, remotes: &[&str]) -> Self {
            let main = Branch {
                name: RefName::from("main"),
                commit: "0123".into(),
                upstream: upstream.map(|remote| Upstream {
                    remote: RefName::from(remote),
                    branch: RefName::from("main"),
                    ahead: Some(0),
                    behind: Some(0),
                }),
                head: true,
            };
            Self {
                calls: Arc::default(),
                head: HeadState::Branch {
                    name: RefName::from("main"),
                    commit: Some("0123".into()),
                },
                branches: vec![main],
                remotes: remotes
                    .iter()
                    .map(|r| Remote {
                        name: RefName::from(*r),
                        urls: vec!["https://example.invalid/x".into()],
                    })
                    .collect(),
            }
        }

        fn detached() -> Self {
            let mut me = Self::tracked(None, &[]);
            me.head = HeadState::Detached {
                commit: "0123".into(),
            };
            me
        }

        fn said(&self) -> Vec<String> {
            self.calls
                .lock()
                .unwrap()
                .iter()
                .map(|c| String::from_utf8_lossy(c).into_owned())
                .collect()
        }
    }

    impl Repo for SyncFake {
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
            "sync".into()
        }
        fn head(&self) -> gitten_git::Result<HeadState> {
            Ok(self.head.clone())
        }
        fn branches(&self) -> gitten_git::Result<Vec<Branch>> {
            Ok(self.branches.clone())
        }
        fn remotes(&self) -> gitten_git::Result<Vec<Remote>> {
            Ok(self.remotes.clone())
        }
        fn push(&self, remote: &[u8], branch: &[u8]) -> gitten_git::Result<()> {
            let mut line = b"push ".to_vec();
            line.extend_from_slice(remote);
            line.push(b' ');
            line.extend_from_slice(branch);
            self.calls.lock().unwrap().push(line);
            Ok(())
        }
        fn pull(&self) -> gitten_git::Result<()> {
            self.calls.lock().unwrap().push(b"pull".to_vec());
            Ok(())
        }
        fn fetch(&self, remote: Option<&[u8]>) -> gitten_git::Result<()> {
            let mut line = b"fetch ".to_vec();
            line.extend_from_slice(remote.unwrap_or(b"--all"));
            self.calls.lock().unwrap().push(line);
            Ok(())
        }
    }

    #[test]
    fn the_sync_verbs_reach_the_trait_bytes_intact_and_announce_their_finish() {
        let fake = Arc::new(SyncFake::tracked(Some("up"), &["up"]));
        let repo: Handle = fake.clone();
        let runner = Runner::new();
        let submit = runner.submitter();

        let mut jobs: Vec<Box<dyn Job>> = vec![
            Box::new(Write::push(&repo, b"o\xe9".to_vec(), b"m\xe9".to_vec())),
            Box::new(Write::pull(&repo)),
            Box::new(Write::fetch(&repo, None)),
            Box::new(Write::fetch(&repo, Some(b"or\xedgin".to_vec()))),
        ];
        for job in jobs.drain(..) {
            assert!(submit.submit(job).is_ok());
        }

        let deadline = Instant::now() + Duration::from_secs(2);
        while fake.said().len() < 4 {
            assert!(
                Instant::now() < deadline,
                "jobs did not run: {:?}",
                fake.said()
            );
            std::thread::yield_now();
        }
        assert_eq!(
            fake.said(),
            vec![
                "push o\u{FFFD} m\u{FFFD}",
                "pull",
                "fetch --all",
                "fetch or\u{FFFD}gin",
            ],
            "every byte arrived undecoded"
        );

        // The band's words going out, and the finish line's coming back:
        // present tense while running, past tense once it landed.
        let (mut started, mut finished) = (Vec::new(), Vec::new());
        while let Some(event) = runner.try_next() {
            match event {
                Event::Started { name } => started.push(name),
                Event::Finished { done, .. } => finished.push(done),
            }
        }
        assert_eq!(
            started,
            vec![
                "push o\u{FFFD} m\u{FFFD}",
                "pull",
                "fetch",
                "fetch or\u{FFFD}gin"
            ]
        );
        assert_eq!(
            finished,
            vec![
                Some("pushed o\u{FFFD} m\u{FFFD}".into()),
                Some("pulled".into()),
                Some("fetched".into()),
                Some("fetched or\u{FFFD}gin".into()),
            ]
        );
    }

    #[test]
    fn push_current_aims_where_the_repository_says() {
        // An upstream wins over any stand-in: what the configuration names
        // is where the branch goes.
        let fake = Arc::new(SyncFake::tracked(Some("up"), &["up", "other"]));
        let repo: Handle = fake.clone();
        let job = Write::push_current(&repo).expect("an aim");
        assert_eq!(job.name(), "push up main");
        assert_eq!(fake.said(), Vec::<String>::new(), "nothing ran yet");

        // No upstream: origin stands in when the repository has one.
        let fake = Arc::new(SyncFake::tracked(None, &["web", "origin"]));
        let repo: Handle = fake.clone();
        assert_eq!(
            Write::push_current(&repo).expect("an aim").name(),
            "push origin main"
        );

        // No origin either: the sole remote is unambiguous.
        let fake = Arc::new(SyncFake::tracked(None, &["solo"]));
        let repo: Handle = fake.clone();
        assert_eq!(
            Write::push_current(&repo).expect("an aim").name(),
            "push solo main"
        );

        // Several servers and no configuration: refusing beats guessing.
        let fake = Arc::new(SyncFake::tracked(None, &["one", "two"]));
        let repo: Handle = fake.clone();
        let err = Write::push_current(&repo).err().expect("refused");
        assert!(err.contains("no upstream"), "{err}");
        assert_eq!(fake.said(), Vec::<String>::new(), "refused before running");

        // Detached HEAD is not a branch; nothing to send.
        let fake = Arc::new(SyncFake::detached());
        let repo: Handle = fake.clone();
        let err = Write::push_current(&repo).err().expect("refused");
        assert!(err.contains("detached"), "{err}");
    }
}
