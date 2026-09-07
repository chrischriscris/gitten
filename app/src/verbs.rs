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
use gitten_core::operation::Side;
use gitten_core::rebase::{FixupKind, Plan, TodoScript};
use gitten_core::refs::{HeadState, Remote, ResetMode, StashId, StashScope};
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

/// The band's count of commits, said the way a person says it.
fn many_commits(n: usize) -> String {
    match n {
        1 => "1 commit".into(),
        n => format!("{n} commits"),
    }
}

/// The band's count of paths, said the way a person says it.
fn many(n: usize) -> String {
    match n {
        1 => "1 path".into(),
        n => format!("{n} paths"),
    }
}

/// The sha `HEAD` holds, when the repository can say — the one fact a plan
/// built over a window of history needs to still be true when it runs.
///
/// Detached is a sha like any other here: what a rebase replays is measured
/// from where HEAD points, not from whether a branch name is attached to it.
/// `None` for an unborn branch and for a read that failed, which are the two
/// ways a repository has of not having a HEAD to compare.
fn head_sha(repo: &dyn Repo) -> Option<String> {
    match repo.head().ok()? {
        HeadState::Branch { commit, .. } => commit,
        HeadState::Detached { commit } => Some(commit),
    }
}

/// A sha as a sentence says it: git's own eight characters, and the whole
/// thing when it is shorter than that.
fn abbreviated(sha: &str) -> &str {
    &sha[..sha.len().min(8)]
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

    /// Writes exactly what a synthesized patch describes into the working
    /// tree — `git apply`, the fourth corner the other three patch verbs
    /// leave open. Bytes end to end, emptiness refused before the queue,
    /// drift refused by git's own sentence at apply time.
    pub fn apply_patch(repo: &Handle, patch: Vec<u8>) -> Result<Self, String> {
        if patch.is_empty() {
            return Err("an empty patch applies nothing".into());
        }
        Ok(Self::named("apply patch".into(), repo, move |r| {
            r.apply_patch(&patch)
        }))
    }

    /// Restores one path to the version a commit holds —
    /// `git checkout <sha> -- <path>`, worktree and index together, which
    /// is git's semantic and is said as such wherever this is offered.
    /// DESTRUCTIVE when the path differs: the caller confirms before this
    /// job is ever built.
    pub fn checkout_file_from_commit(repo: &Handle, sha: Vec<u8>, path: Vec<u8>) -> Self {
        let shown = String::from_utf8_lossy(&path).into_owned();
        Self::named(format!("checkout {shown} from commit"), repo, move |r| {
            r.checkout_file_from(&sha, &path)
        })
    }

    /// Folds `files` into the commit `sha` names and replays what followed
    /// it: detach at the commit, run each patch against its content
    /// (`reverse` aims removals, forward aims additions), stage the paths,
    /// amend without rewording, restore the reader's position, and replay
    /// the descendants onto the replacement.
    ///
    /// One job, because the steps are one decision and the queue's finish
    /// wave is the only honest place for the refresh: halfway generations
    /// would draw a detached HEAD mid-graft as a state. A rebase that
    /// stops on a conflict is a clean finish carrying the standing rebase
    /// in its announcement — the lifecycle owns it from there — and
    /// anything earlier failing restores the reader's position before
    /// reporting, so a failed graft never strands a detached HEAD behind
    /// it. A graft that finds the reader detached refuses outright: there
    /// is no branch to carry the rewrite, and amending one would leave it
    /// dangling. Files with empty patches are skipped; all empty is refused
    /// before the queue.
    pub fn graft_files(
        repo: &Handle,
        sha: Vec<u8>,
        files: Vec<(Vec<u8>, Vec<u8>)>,
        reverse: bool,
    ) -> Result<Self, String> {
        let live: Vec<(Vec<u8>, Vec<u8>)> =
            files.into_iter().filter(|(_, p)| !p.is_empty()).collect();
        if live.is_empty() {
            return Err("an empty patch grafts nothing".into());
        }
        let short = String::from_utf8_lossy(&sha);
        let short = short.chars().take(8).collect::<String>();
        Ok(Self::named(
            format!("graft {} files into {short}", live.len()),
            repo,
            move |r| graft(r, &sha, &live, reverse),
        ))
    }

    /// Carries `patches` onto `branch`, creating it at HEAD first when it
    /// does not exist yet: checkout (a dirty tree it cannot carry is git's own
    /// refusal), then each patch onto the worktree in order, uncommitted —
    /// the commit is the reader's next keypress, not this job's last
    /// step. When creation was requested and the checkout fails, the new
    /// branch is removed again, so a failed move leaves no empty branch
    /// behind it.
    pub fn move_patch_to_branch(
        repo: &Handle,
        branch: Vec<u8>,
        create: bool,
        patches: Vec<(Vec<u8>, Vec<u8>)>,
    ) -> Result<Self, String> {
        if patches.iter().all(|(_, p)| p.is_empty()) {
            return Err("an empty patch moves nothing".into());
        }
        let shown = String::from_utf8_lossy(&branch).into_owned();
        let job = Self::named(format!("move patch onto {shown}"), repo, move |r| {
            if create {
                r.create_branch(&branch, None)?;
            }
            if let Err(e) = r.checkout(&branch) {
                if create {
                    let _ = r.delete_branch(&branch, true);
                }
                return Err(e);
            }
            for (_, patch) in &patches {
                if !patch.is_empty() {
                    r.apply_patch(patch)?;
                }
            }
            Ok(())
        })
        .announcing(format!(
            "patch on {shown} — uncommitted, commit it or leave it"
        ));
        Ok(job)
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

    /// Commits the index as a fixup for `sha`: the message is git's marker
    /// (`fixup! <subject>` and its `amend!` / `reword!` siblings), so no
    /// prompt stands between the key and the commit. Non-destructive — it
    /// only adds — so like [`Write::revert`] it takes no confirmation
    /// dance; the finish announces the marker it wrote, because a key that
    /// silently grows history is a key nobody trusts.
    pub fn fixup_commit(repo: &Handle, sha: Vec<u8>, short: String, kind: FixupKind) -> Self {
        let word = kind.word().to_string();
        Self::named(format!("{word} {short}"), repo, move |r| {
            r.commit_fixup(&sha, kind).map(|_| ())
        })
        .announcing(format!("{word} {short} created"))
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

    /// Replaces HEAD's message and nothing else. The narrow sibling of
    /// [`Write::amend`], and narrow on purpose: a keypress that said
    /// *reword* must not commit whatever happens to be staged, which is
    /// what a bare amend would do.
    pub fn reword_head(repo: &Handle, message: String) -> Self {
        Self::named("reword HEAD".into(), repo, move |r| r.reword_head(&message))
            .announcing("reworded HEAD")
    }

    /// Rewrites this branch from an editable [`Plan`] — the todo UI's own
    /// verb, and the only one that can carry a reworded message or an
    /// amendment down to the layer with a filesystem to put them in. The
    /// plan refuses every shape git would before any process runs; a
    /// conflict, or an `edit` the plan asked for, hands back a standing
    /// rebase for the lifecycle keys to carry on from.
    /// DESTRUCTIVE: the caller confirms before this job is ever built.
    ///
    /// **Where HEAD was when this was built is part of the job.** A plan is
    /// a window of history read at some earlier moment, and every row in it
    /// names a sha. The queue's own generation rail catches a plan staled by
    /// a *write of ours*; it says nothing about a commit typed in a terminal
    /// or an amend in another client while the plan stood open, and after
    /// one of those every sha in the plan names a commit the branch no
    /// longer has. Replaying them is a rewrite nobody described. So HEAD is
    /// read here — at the confirmation, which is where this is built — and
    /// read again in the job, and a difference refuses before git runs.
    ///
    /// `None` is the honest answer from a repository that cannot say where
    /// HEAD is: an unborn branch, or a read that failed. Nothing to compare
    /// then, and the rebase itself is what refuses.
    pub fn rebase_plan(repo: &Handle, plan: Plan) -> Self {
        let shown = String::from_utf8_lossy(plan.upstream()).into_owned();
        let count = plan.len();
        let confirmed_at = head_sha(repo.as_ref());
        Self::named(format!("rebase {count} onto {shown}"), repo, move |r| {
            if let Some(was) = confirmed_at.as_deref() {
                let now = head_sha(r);
                if now.as_deref() != Some(was) {
                    return Err(format!(
                        "HEAD was {} when this plan was confirmed and is {} now — \
                         something outside this queue rewrote the branch; \
                         reopen the plan",
                        abbreviated(was),
                        now.as_deref().map(abbreviated).unwrap_or("nowhere"),
                    ));
                }
            }
            r.rebase_plan(&plan)
        })
        .announcing(format!("rewrote {} from {shown}", many_commits(count)))
    }

    /// Replays everything after `base` onto `onto` — `git rebase --onto`,
    /// with the marked commit left exactly where it is and its children
    /// moved. DESTRUCTIVE: the caller confirms before this job is ever
    /// built, and names both ends of it, because a base that is not an
    /// ancestor of HEAD replays a range nobody meant.
    pub fn rebase_onto_base(repo: &Handle, onto: Vec<u8>, base: Vec<u8>) -> Self {
        let target = String::from_utf8_lossy(&onto).into_owned();
        let from = String::from_utf8_lossy(&base).into_owned();
        Self::named(
            format!("rebase onto {target} from {from}"),
            repo,
            move |r| r.rebase_onto_base(&onto, &base),
        )
        .announcing(format!("rebased onto {target}, from {from} up"))
    }

    /// Throws the whole working tree away — every uncommitted byte, tracked
    /// and untracked, with no stash and no reflog behind it. Ignored files
    /// stay. DESTRUCTIVE, the most so here: the caller confirms.
    pub fn nuke_worktree(repo: &Handle) -> Self {
        Self::named("nuke the working tree".into(), repo, |r| r.nuke_worktree())
            .announcing("the working tree is back at HEAD")
    }

    /// Moves the current branch onto whatever its upstream holds, at the
    /// strength given — the files pane's reset, aimed past every row at the
    /// remote-tracking ref this branch is configured against.
    ///
    /// The aim is *read*, never assumed: the branch under HEAD, then its
    /// configured upstream, and each way that can be missing refuses here
    /// with a sentence instead of queueing a job git would answer with a
    /// revspec error. The ref is named in full (`origin/main`, not
    /// `@{upstream}`) so what a confirmation says and what git resolves are
    /// the same string.
    pub fn reset_upstream(repo: &Handle, mode: ResetMode) -> Result<Self, String> {
        let branch = match repo.head()? {
            HeadState::Branch { name, .. } => name,
            HeadState::Detached { .. } => {
                return Err("detached HEAD has no branch, so it has no upstream".into())
            }
        };
        let upstream = repo
            .branches()?
            .iter()
            .find(|b| b.name.as_bytes() == branch.as_bytes())
            .and_then(|b| b.upstream.clone())
            .ok_or_else(|| {
                format!(
                    "{} tracks no upstream to reset to",
                    branch.to_string_lossy()
                )
            })?;
        let mut target = upstream.remote.as_bytes().to_vec();
        target.push(b'/');
        target.extend_from_slice(upstream.branch.as_bytes());
        Ok(Self::reset(repo, mode, target))
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

    /// Replays every copied commit onto the current branch, in the order
    /// the clipboard arranged them — one `git cherry-pick` over the whole
    /// slice, so a conflict stops the sequence where git stopped it and the
    /// remainder stands for [`Write::cherry_pick_abort`] or
    /// [`Write::cherry_pick_continue`] to carry. Nothing existing moves, so
    /// no confirmation precedes it; the clipboard survives the paste, since
    /// the same set is often wanted on a second branch.
    pub fn cherry_pick_range(repo: &Handle, shas: Vec<Vec<u8>>) -> Self {
        // The band counts rather than lists: a dozen shas is not a sentence,
        // and the pane the picks land in shows them by name a frame later.
        let n = shas.len();
        let shown = if n == 1 {
            String::from_utf8_lossy(&shas[0]).into_owned()
        } else {
            format!("{n} commits")
        };
        Self::named(format!("cherry-pick {shown}"), repo, move |r| {
            r.cherry_pick_range(&shas)
        })
        .announcing(format!("picked {shown}"))
    }

    /// Hands HEAD's authorship to the current user and moves nothing else —
    /// the tree and the message stand byte-still. A rewrite all the same, so
    /// the caller confirms before this job is ever built; and HEAD only,
    /// because a deeper commit's author is a rebase.
    pub fn reset_author(repo: &Handle) -> Self {
        Self::named("reset author".into(), repo, |r| r.reset_author())
            // The new sha lands in a pane the key may not be over — the
            // author key lives on the commits list, the band is everywhere.
            .announcing("reset HEAD's author")
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

    /// Merges `target` into the current branch — regular (`--no-edit`,
    /// fast-forwarding when git can) or squash (`--squash`, staging the
    /// collision and committing nothing, which is why a squash's finish
    /// names the commit the reader still owes). A conflict comes back
    /// refused in git's words with its question standing for
    /// [`Write::merge_abort`] or [`Write::merge_continue`].
    pub fn merge(repo: &Handle, target: Vec<u8>, squash: bool) -> Self {
        let shown = String::from_utf8_lossy(&target).into_owned();
        let (name, done) = match squash {
            false => (format!("merge {shown}"), format!("merged {shown}")),
            true => (
                format!("squash-merge {shown}"),
                format!("squash-merged {shown}; commit to finish"),
            ),
        };
        Self::named(name, repo, move |r| r.merge(&target, squash)).announcing(done)
    }

    /// Abandons an in-progress merge — branch, index and working tree back
    /// where the merge started, git's own guarantee.
    pub fn merge_abort(repo: &Handle) -> Self {
        Self::named("merge abort".into(), repo, |r| r.merge_abort()).announcing("merge aborted")
    }

    /// Finishes an in-progress regular merge once the conflicts are
    /// resolved; the message editor is answered `true` by the trait. A
    /// squash merge has no merge state to continue and nothing offers this
    /// for one.
    pub fn merge_continue(repo: &Handle) -> Self {
        Self::named("merge continue".into(), repo, |r| r.merge_continue())
            .announcing("merge continued")
    }

    /// Steps over the commit a rebase stopped on — that commit's changes
    /// leave the branch. The one lifecycle verb that destroys work rather
    /// than restoring it, which is why only a rebase ever offers it.
    pub fn rebase_skip(repo: &Handle) -> Self {
        Self::named("rebase skip".into(), repo, |r| r.rebase_skip())
            .announcing("rebase skipped the commit")
    }

    /// Abandons an in-progress revert — the tree back where the revert
    /// started, git's own guarantee.
    pub fn revert_abort(repo: &Handle) -> Self {
        Self::named("revert abort".into(), repo, |r| r.revert_abort()).announcing("revert aborted")
    }

    /// Finishes an in-progress revert once the conflicts are resolved.
    pub fn revert_continue(repo: &Handle) -> Self {
        Self::named("revert continue".into(), repo, |r| r.revert_continue())
            .announcing("revert continued")
    }

    /// Records one conflicted path as resolved, taking `side`'s answer —
    /// the acquisition layer owns the how; this names it for the status
    /// line and announces the choice, since a resolution that came from a
    /// keypress should say which one it took.
    pub fn resolve(repo: &Handle, path: Vec<u8>, side: Side) -> Self {
        let shown = String::from_utf8_lossy(&path).into_owned();
        let label = match side {
            Side::Ours => "ours",
            Side::Theirs => "theirs",
            Side::Both => "both",
            Side::Keep => "kept",
        };
        Self::named(format!("resolve {shown} ({label})"), repo, move |r| {
            r.resolve(&path, side)
        })
        .announcing(format!("resolved {shown} ({label})"))
    }

    /// Applies region answers to a conflicted file — the merging view's
    /// half-answered file, staged region by region. The repo re-reads and
    /// re-validates against the file as it stands now; this job only names
    /// the choices for the status line.
    pub fn resolve_hunks(
        repo: &Handle,
        path: Vec<u8>,
        choices: Vec<(usize, gitten_core::conflict::Answer)>,
    ) -> Self {
        let shown = String::from_utf8_lossy(&path).into_owned();
        let named = choices
            .iter()
            .map(|(region, answer)| {
                let word = match answer {
                    gitten_core::conflict::Answer::Ours => "ours",
                    gitten_core::conflict::Answer::Theirs => "theirs",
                    gitten_core::conflict::Answer::Both => "both",
                };
                format!("{region}:{word}")
            })
            .collect::<Vec<_>>()
            .join(" ");
        Self::named(format!("resolve {shown} ({named})"), repo, move |r| {
            r.resolve_hunks(&path, &choices)
        })
        .announcing(format!("resolved {shown} ({named})"))
    }

    /// Puts a conflicted path back the way a region answer found it — the
    /// bytes on disk and the unmerged stages in the index, both as they
    /// were read before the choice this undoes.
    pub fn restore(
        repo: &Handle,
        path: Vec<u8>,
        bytes: Vec<u8>,
        stages: Vec<gitten_git::UnmergedStage>,
    ) -> Self {
        let shown = String::from_utf8_lossy(&path).into_owned();
        Self::named(format!("undo {shown}"), repo, move |r| {
            r.restore_conflict(&path, bytes, &stages)
        })
        .announcing(format!("undo recorded for {shown}"))
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
    /// at survives.
    pub fn delete_tag(repo: &Handle, name: Vec<u8>) -> Self {
        let shown = String::from_utf8_lossy(&name).into_owned();
        Self::named(format!("untag {shown}"), repo, move |r| r.delete_tag(&name))
    }

    /// Pushes one tag to the named remote. The `tag` word rides inside the
    /// verb (see [`Repo::push_tag`](gitten_git::Repo::push_tag)), so a
    /// caller can never push a branch by spelling a name two things share.
    pub fn push_tag(repo: &Handle, remote: Vec<u8>, name: Vec<u8>) -> Self {
        let shown = String::from_utf8_lossy(&name).into_owned();
        let at = String::from_utf8_lossy(&remote).into_owned();
        Self::named(format!("push tag {shown} to {at}"), repo, move |r| {
            r.push_tag(&remote, &name)
        })
    }

    /// Deletes the branch from the named remote, keeping the local branch.
    pub fn delete_remote_branch(repo: &Handle, remote: Vec<u8>, branch: Vec<u8>) -> Self {
        let shown = String::from_utf8_lossy(&branch).into_owned();
        let at = String::from_utf8_lossy(&remote).into_owned();
        Self::named(format!("delete {shown} from {at}"), repo, move |r| {
            r.delete_remote_branch(&remote, &branch)
        })
    }

    /// Points HEAD's ref at `target` with `message` as the reflog sentence —
    /// undo's and redo's verb. The label names the direction, so the queue
    /// and the status line read as prose rather than as a git invocation.
    ///
    /// **Where HEAD was when this was built is part of the job.** `target`
    /// is a positional selector — `HEAD@{1}` names whatever the reflog held
    /// when it was read, not a sha — so a branch switch in a terminal
    /// between dispatch and execution retargets the undo onto the other
    /// branch's walk. HEAD is read here — at the confirmation, which is
    /// where this is built — and read again in the job, and a difference
    /// refuses before git runs. Same binding as
    /// [`Write::rebase_plan`](Self::rebase_plan); undo and redo both
    /// re-derive their selector from a fresh reflog read, so the recovery
    /// is simply trying again.
    pub fn move_head(repo: &Handle, label: String, message: &'static str, target: Vec<u8>) -> Self {
        let confirmed_at = head_sha(repo.as_ref());
        Self::named(label, repo, move |r| {
            if let Some(was) = confirmed_at.as_deref() {
                let now = head_sha(r);
                if now.as_deref() != Some(was) {
                    return Err(format!(
                        "HEAD was {} when this undo was confirmed and is {} now — \
                         something outside this queue moved it; \
                         try again for a fresh reading",
                        abbreviated(was),
                        now.as_deref().map(abbreviated).unwrap_or("nowhere"),
                    ));
                }
            }
            r.move_head(&target, message)
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

    /// Parks a chosen part of the working tree — [`StashScope`] says which,
    /// and which part is left standing.
    ///
    /// The band names the scope rather than the flag: a reader watching a
    /// job run wants to know *what went*, and `--keep-index` is a fact about
    /// git's command line.
    pub fn stash_push_scoped(repo: &Handle, message: Option<String>, scope: StashScope) -> Self {
        let shown = scope.label();
        Self::named(format!("stash {shown}"), repo, move |r| {
            r.stash_push_scoped(message.as_deref(), &scope).map(|_| ())
        })
    }

    /// Restores the entry `id` names, keeping it — [`Write::stash_apply`]
    /// aimed by commit, so a stack that churned between the keypress and the
    /// queue's turn cannot retarget it. The band says the number the reader
    /// saw; the write resolves the commit again for itself.
    pub fn stash_apply_entry(repo: &Handle, id: StashId) -> Self {
        Self::named(
            format!("stash apply stash@{{{}}}", id.index),
            repo,
            move |r| r.stash_apply_id(&id),
        )
    }

    /// [`Write::stash_pop`] aimed by commit. A conflicted restore is git's
    /// refusal with the entry kept — see [`Repo::stash_pop`].
    pub fn stash_pop_entry(repo: &Handle, id: StashId) -> Self {
        Self::named(
            format!("stash pop stash@{{{}}}", id.index),
            repo,
            move |r| r.stash_pop_id(&id),
        )
    }

    /// [`Write::stash_drop`] aimed by commit — the verb the identity matters
    /// most for, because a drop aimed at a stale number destroys work nobody
    /// chose. DESTRUCTIVE: the caller confirms before this is ever built.
    pub fn stash_drop_entry(repo: &Handle, id: StashId) -> Self {
        Self::named(
            format!("stash drop stash@{{{}}}", id.index),
            repo,
            move |r| r.stash_drop_id(&id),
        )
    }

    /// Gives a stash entry a new message, keeping its commit. Announces,
    /// because a rename re-files the entry at the top of the stack — see
    /// [`Repo::stash_rename`] for why git leaves no other shape — and a row
    /// that moved without a word looks like a different entry.
    pub fn stash_rename(repo: &Handle, id: StashId, message: String) -> Self {
        let shown = message.clone();
        Self::named(format!("rename stash@{{{}}}", id.index), repo, move |r| {
            r.stash_rename(&id, &message)
        })
        .announcing(format!("renamed to {shown}, now at the top of the stack"))
    }

    /// Starts a branch from a stash entry: the stash's own base commit,
    /// checked out under `name`, with the entry applied and its index
    /// intact. Announces — the effect is a checkout the eye may be nowhere
    /// near.
    pub fn stash_branch(repo: &Handle, id: StashId, name: Vec<u8>) -> Self {
        let shown = String::from_utf8_lossy(&name).into_owned();
        let at = id.index;
        Self::named(
            format!("branch {shown} from stash@{{{at}}}"),
            repo,
            move |r| r.stash_branch(&id, &name),
        )
        .announcing(format!("{shown} starts where stash@{{{at}}} was made"))
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

    /// Checks out the remote-tracking ref `remote/branch` as a local branch
    /// that tracks it — [`Repo::checkout_tracking`]'s job. The local name is
    /// git's choice (the branch's own), so it is not an argument here.
    pub fn checkout_tracking(repo: &Handle, remote: Vec<u8>, branch: Vec<u8>) -> Self {
        let shown = shown_pair(&remote, &branch);
        Self::named(format!("checkout {shown} (tracking)"), repo, move |r| {
            r.checkout_tracking(&remote, &branch)
        })
        // HEAD's branch changes and the branches pane may not be focused —
        // the key lives over a remote row — so this one says what it did.
        .announcing(format!("checked out {shown} as a tracking branch"))
    }

    /// Checks out the branch HEAD sat on before this one. Nothing here to
    /// confirm: it only ever moves HEAD along the reflog, and the tree it
    /// lands on is whatever that checkout makes of the changes — git's own
    /// refusals (none recorded, diverged trees) surface verbatim.
    pub fn checkout_previous(repo: &Handle) -> Self {
        Self::named("checkout previous".into(), repo, |r| r.checkout_previous())
            .announcing("checked out the previous branch")
    }

    /// Checks out `name` over any local changes. DESTRUCTIVE: the caller
    /// confirms before this job is ever built.
    pub fn checkout_force(repo: &Handle, name: Vec<u8>) -> Self {
        let shown = String::from_utf8_lossy(&name).into_owned();
        Self::named(format!("force-checkout {shown}"), repo, move |r| {
            r.checkout_force(&name)
        })
        .announcing(format!("checked out {shown}, local changes discarded"))
    }

    /// Makes local branch `local` track `remote/branch` — the link only,
    /// never a fetch or a merge, which is why no confirmation precedes it.
    pub fn set_upstream(repo: &Handle, local: Vec<u8>, remote: Vec<u8>, branch: Vec<u8>) -> Self {
        let shown = shown_pair(&remote, &branch);
        let local_shown = String::from_utf8_lossy(&local).into_owned();
        Self::named(format!("track {shown} on {local_shown}"), repo, move |r| {
            r.set_upstream(&local, &remote, &branch)
        })
        .announcing(format!("{local_shown} now tracks {shown}"))
    }

    /// Severs local branch `local`'s tracking link. Recoverable by setting
    /// one again, so no confirmation precedes it.
    pub fn unset_upstream(repo: &Handle, local: Vec<u8>) -> Self {
        let shown = String::from_utf8_lossy(&local).into_owned();
        Self::named(format!("untrack {shown}"), repo, move |r| {
            r.unset_upstream(&local)
        })
        .announcing(format!("{shown} no longer tracks an upstream"))
    }

    /// Fast-forwards local branch `local` onto `remote/branch` — never
    /// sideways; which git verb the checked-out case needs is the trait's
    /// decision, read fresh from HEAD. A divergence comes back refused in
    /// git's words with the branch left standing.
    pub fn fast_forward(repo: &Handle, local: Vec<u8>, remote: Vec<u8>, branch: Vec<u8>) -> Self {
        let shown = shown_pair(&remote, &branch);
        let local_shown = String::from_utf8_lossy(&local).into_owned();
        Self::named(
            format!("fast-forward {local_shown} to {shown}"),
            repo,
            move |r| r.fast_forward(&local, &remote, &branch),
        )
        .announcing(format!("fast-forwarded {local_shown} to {shown}"))
    }

    /// Introduces a remote by name and URL. A duplicate name is git's own
    /// refusal, verbatim.
    pub fn add_remote(repo: &Handle, name: Vec<u8>, url: Vec<u8>) -> Self {
        let shown = String::from_utf8_lossy(&name).into_owned();
        Self::named(format!("add remote {shown}"), repo, move |r| {
            r.add_remote(&name, &url)
        })
        .announcing(format!("added remote {shown}"))
    }

    /// Points remote `name` at `url`.
    pub fn set_remote_url(repo: &Handle, name: Vec<u8>, url: Vec<u8>) -> Self {
        let shown = String::from_utf8_lossy(&name).into_owned();
        Self::named(format!("set-url {shown}"), repo, move |r| {
            r.set_remote_url(&name, &url)
        })
        .announcing(format!("{shown} now points at the new URL"))
    }

    /// Forgets remote `name`, its remote-tracking branches going with it.
    /// DESTRUCTIVE: the caller confirms before this job is ever built.
    pub fn remove_remote(repo: &Handle, name: Vec<u8>) -> Self {
        let shown = String::from_utf8_lossy(&name).into_owned();
        Self::named(format!("remove remote {shown}"), repo, move |r| {
            r.remove_remote(&name)
        })
        .announcing(format!("removed remote {shown}"))
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

/// The steps [`Write::graft_patch`] runs as one job. Each step names its
/// own failure; the reader's position is restored before any error
/// leaves, except the one state that is meant to stand: a replay stopped
/// on a conflict, which the lifecycle owns from there and the refresh
/// wave will draw.
fn graft(
    r: &dyn Repo,
    sha: &[u8],
    files: &[(Vec<u8>, Vec<u8>)],
    reverse: bool,
) -> Result<(), String> {
    if !r.status()?.is_empty() {
        return Err("stow or commit the working tree first — a graft needs a clean tree".into());
    }
    if r.operation().is_some() {
        return Err("a standing operation waits — abort it or finish it first".into());
    }
    let home = r.head()?;
    if matches!(&home, HeadState::Branch { commit: None, .. }) {
        return Err("no commits to rewrite yet".into());
    }
    // A detached HEAD has no branch to carry the rewrite: the dance
    // below would detach at the commit, amend a replacement, and then
    // check out the old commit again — the replacement dangling
    // unreferenced while the visible state reads byte-identical to
    // before. Refuse up front, naming the door, before anything moves.
    if let HeadState::Detached { commit } = &home {
        return Err(format!(
            "checkout a branch first — grafting onto a detached HEAD would leave the rewrite dangling at {}",
            abbreviated(commit)
        ));
    }
    // Detach at the commit being rewritten: its content is then the
    // worktree, so every patch aims at exactly what it was built from.
    r.checkout(sha)?;
    let restore = |r: &dyn Repo| {
        // Back to the detached commit's own content first: a bare
        // checkout refuses to overwrite the graft's half-applied work,
        // stranding the reader detached. Reset hard — the commit is
        // untouched, only worktree and index move, and the graft required
        // a clean tree so nothing but its own work can be in the way —
        // then go home.
        let _ = r.reset(ResetMode::Hard, sha);
        let _ = match &home {
            HeadState::Branch { name, .. } => r.checkout(name.as_bytes()),
            HeadState::Detached { commit } => r.checkout(commit.as_bytes()),
        };
    };
    let amended = (|| {
        for (path, patch) in files {
            if reverse {
                r.discard_patch(patch)?;
            } else {
                r.apply_patch(patch)?;
            }
            r.stage(path)?;
        }
        // Lifting a commit's only change does not rewrite it — it
        // deletes it, and git's own amend refuses an empty result for
        // exactly that reason. The graft refuses first, naming the door
        // that owns it, before the position or the history moves.
        if r.graft_empties(sha)? {
            let short = String::from_utf8_lossy(sha);
            let short = short.chars().take(8).collect::<String>();
            return Err(format!(
                "removing this would empty {short} — drop the commit instead"
            ));
        }
        r.amend_no_edit()
    })();
    let new = match amended {
        Ok(new) => new,
        Err(e) => {
            restore(r);
            return Err(e);
        }
    };
    match &home {
        HeadState::Detached { commit } => {
            r.checkout(commit.as_bytes())?;
            Ok(())
        }
        HeadState::Branch { name, commit } => {
            r.checkout(name.as_bytes())?;
            // The rewritten commit was the tip: no descendants to replay,
            // so the branch steps onto the replacement — what the replay
            // would have meant with an empty range.
            if commit.as_deref() == Some(String::from_utf8_lossy(sha).as_ref()) {
                r.reset(ResetMode::Hard, new.as_bytes())?;
                return Ok(());
            }
            match r.rebase_onto_base(new.as_bytes(), sha) {
                Ok(()) => Ok(()),
                Err(e) => {
                    if r.operation().is_some() {
                        Err(format!(
                            "the replay stopped on a conflict — resolve it and continue, or abort: {e}"
                        ))
                    } else {
                        Err(e)
                    }
                }
            }
        }
    }
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
        fn stash_push_scoped(
            &self,
            message: Option<&str>,
            scope: &StashScope,
        ) -> gitten_git::Result<usize> {
            self.0.lock().unwrap().push(format!(
                "stash push {:?} {:?} [{}]",
                message,
                scope.label(),
                scope.flags().join(" ")
            ));
            Ok(0)
        }
        fn stash_apply_id(&self, id: &StashId) -> gitten_git::Result<()> {
            self.0
                .lock()
                .unwrap()
                .push(format!("stash apply {}", id.commit));
            Ok(())
        }
        fn stash_pop_id(&self, id: &StashId) -> gitten_git::Result<()> {
            self.0
                .lock()
                .unwrap()
                .push(format!("stash pop {}", id.commit));
            Ok(())
        }
        fn stash_drop_id(&self, id: &StashId) -> gitten_git::Result<()> {
            self.0
                .lock()
                .unwrap()
                .push(format!("stash drop {}", id.commit));
            Ok(())
        }
        fn stash_rename(&self, id: &StashId, message: &str) -> gitten_git::Result<()> {
            self.0
                .lock()
                .unwrap()
                .push(format!("stash rename {} {message}", id.commit));
            Ok(())
        }
        fn stash_branch(&self, id: &StashId, name: &[u8]) -> gitten_git::Result<()> {
            self.0.lock().unwrap().push(format!(
                "stash branch {} {}",
                String::from_utf8_lossy(name),
                id.commit
            ));
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

    /// A repository whose HEAD moves out from under the queue: the first
    /// read answers `before` — the confirmation's read — and every later one
    /// answers `after`, which is exactly the shape of a commit typed in a
    /// terminal while a plan stood open.
    struct Moving {
        before: String,
        after: String,
        reads: Arc<Mutex<usize>>,
        calls: Arc<Mutex<Vec<String>>>,
    }

    impl Repo for Moving {
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
            "moving".into()
        }
        fn head(&self) -> gitten_git::Result<HeadState> {
            let mut reads = self.reads.lock().unwrap();
            *reads += 1;
            let commit = match *reads {
                1 => self.before.clone(),
                _ => self.after.clone(),
            };
            Ok(HeadState::Branch {
                name: RefName::from("main"),
                commit: Some(commit),
            })
        }
        fn rebase_plan(&self, _plan: &Plan) -> gitten_git::Result<()> {
            self.calls.lock().unwrap().push("rebase_plan".into());
            Ok(())
        }
        fn move_head(&self, _target: &[u8], _message: &str) -> gitten_git::Result<()> {
            self.calls.lock().unwrap().push("move_head".into());
            Ok(())
        }
    }

    /// Three commits, newest first, as a loaded window reads.
    fn window() -> Vec<Commit> {
        ["head", "mid", "under", "root"]
            .iter()
            .enumerate()
            .map(|(i, name)| Commit {
                sha: format!("{name}-sha"),
                short: (*name).into(),
                parents: match i {
                    3 => Box::new([]) as Box<[String]>,
                    _ => Box::new([format!("{}-sha", ["head", "mid", "under", "root"][i + 1])]),
                },
                author: "a".into(),
                timestamp: 0,
                subject: format!("{name} subject"),
            })
            .collect()
    }

    #[test]
    fn a_confirmed_plan_refuses_a_head_that_moved_outside_the_queue() {
        // The plan is built and confirmed against a window whose newest sha
        // is what HEAD held then. Nothing of ours writes in between — no
        // generation bumps, no queue activity at all — and HEAD moves
        // anyway. The job must refuse rather than replay four shas the
        // branch no longer has.
        let calls = Arc::new(Mutex::new(Vec::new()));
        let repo: Handle = Arc::new(Moving {
            before: "head-sha".into(),
            after: "somebody-elses-sha".into(),
            reads: Arc::new(Mutex::new(0)),
            calls: Arc::clone(&calls),
        });
        let plan = Plan::over(&window(), 2).expect("a window of three picks");
        let runner = Runner::new();
        assert!(runner
            .submitter()
            .submit(Box::new(Write::rebase_plan(&repo, plan)))
            .is_ok());

        let deadline = Instant::now() + Duration::from_secs(2);
        let refusal = loop {
            assert!(Instant::now() < deadline, "the job never finished");
            if let Some(Event::Finished { outcome, .. }) = runner.try_next() {
                break outcome;
            }
            std::thread::yield_now();
        };
        let Err(said) = refusal else {
            panic!("a plan confirmed at a sha HEAD no longer holds was replayed");
        };
        assert!(
            said.contains("head-sha") && said.contains("somebody"),
            "the refusal names both shas: {said}"
        );
        assert!(
            recorded(&calls).is_empty(),
            "git was reached anyway: {:?}",
            recorded(&calls)
        );
    }

    #[test]
    fn a_confirmed_plan_runs_when_head_is_where_it_was_left() {
        // The same job against a HEAD that did not move: the comparison must
        // not become a refusal every plan trips over.
        let calls = Arc::new(Mutex::new(Vec::new()));
        let repo: Handle = Arc::new(Moving {
            before: "head-sha".into(),
            after: "head-sha".into(),
            reads: Arc::new(Mutex::new(0)),
            calls: Arc::clone(&calls),
        });
        let plan = Plan::over(&window(), 2).expect("a window of three picks");
        let runner = Runner::new();
        assert!(runner
            .submitter()
            .submit(Box::new(Write::rebase_plan(&repo, plan)))
            .is_ok());

        let deadline = Instant::now() + Duration::from_secs(2);
        while recorded(&calls).is_empty() {
            assert!(Instant::now() < deadline, "the plan never ran");
            std::thread::yield_now();
        }
        assert_eq!(recorded(&calls), vec!["rebase_plan"]);
    }

    #[test]
    fn a_confirmed_undo_refuses_a_head_that_moved_outside_the_queue() {
        // The undo job is built against a HEAD, then somebody switches
        // branches in a terminal before the queue runs it: `HEAD@{1}`
        // would now name the other branch's walk. The job must refuse
        // rather than move a ref the reader never meant.
        let calls = Arc::new(Mutex::new(Vec::new()));
        let repo: Handle = Arc::new(Moving {
            before: "head-sha".into(),
            after: "somebody-elses-sha".into(),
            reads: Arc::new(Mutex::new(0)),
            calls: Arc::clone(&calls),
        });
        let runner = Runner::new();
        assert!(runner
            .submitter()
            .submit(Box::new(Write::move_head(
                &repo,
                "undo (commit)".into(),
                gitten_core::refs::UNDO_MESSAGE,
                b"HEAD@{1}".to_vec()
            )))
            .is_ok());

        let deadline = Instant::now() + Duration::from_secs(2);
        let refusal = loop {
            assert!(Instant::now() < deadline, "the job never finished");
            if let Some(Event::Finished { outcome, .. }) = runner.try_next() {
                break outcome;
            }
            std::thread::yield_now();
        };
        let Err(said) = refusal else {
            panic!("an undo confirmed at a sha HEAD no longer holds was replayed");
        };
        assert!(
            said.contains("head-sha") && said.contains("somebody"),
            "the refusal names both shas: {said}"
        );
        assert!(
            recorded(&calls).is_empty(),
            "git was reached anyway: {:?}",
            recorded(&calls)
        );
    }

    #[test]
    fn a_confirmed_undo_runs_when_head_is_where_it_was_left() {
        // The same job against a HEAD that did not move: the comparison
        // must not become a refusal every undo trips over.
        let calls = Arc::new(Mutex::new(Vec::new()));
        let repo: Handle = Arc::new(Moving {
            before: "head-sha".into(),
            after: "head-sha".into(),
            reads: Arc::new(Mutex::new(0)),
            calls: Arc::clone(&calls),
        });
        let runner = Runner::new();
        assert!(runner
            .submitter()
            .submit(Box::new(Write::move_head(
                &repo,
                "undo (commit)".into(),
                gitten_core::refs::UNDO_MESSAGE,
                b"HEAD@{1}".to_vec()
            )))
            .is_ok());

        let deadline = Instant::now() + Duration::from_secs(2);
        while recorded(&calls).is_empty() {
            assert!(Instant::now() < deadline, "the undo never ran");
            std::thread::yield_now();
        }
        assert_eq!(recorded(&calls), vec!["move_head"]);
    }

    #[test]
    fn the_scoped_and_identified_stash_verbs_reach_the_trait_by_commit() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let repo: Handle = Arc::new(Recording(Arc::clone(&calls)));
        let runner = Runner::new();
        let submit = runner.submitter();
        let id = StashId {
            index: 1,
            commit: "cafebabe".into(),
        };

        let jobs: Vec<Box<dyn Job>> = vec![
            Box::new(Write::stash_push_scoped(
                &repo,
                Some("index only".into()),
                StashScope::Staged,
            )),
            Box::new(Write::stash_push_scoped(
                &repo,
                None,
                StashScope::Path {
                    path: "notes.md".into(),
                    untracked: true,
                },
            )),
            Box::new(Write::stash_apply_entry(&repo, id.clone())),
            Box::new(Write::stash_pop_entry(&repo, id.clone())),
            Box::new(Write::stash_drop_entry(&repo, id.clone())),
            Box::new(Write::stash_rename(
                &repo,
                id.clone(),
                "the parser one".into(),
            )),
            Box::new(Write::stash_branch(&repo, id.clone(), b"wip".to_vec())),
        ];
        for job in jobs {
            assert!(submit.submit(job).is_ok());
        }

        let deadline = Instant::now() + Duration::from_secs(2);
        while recorded(&calls).len() < 7 {
            assert!(
                Instant::now() < deadline,
                "jobs did not run: {:?}",
                recorded(&calls)
            );
            std::thread::yield_now();
        }
        // The scope travels as the concept and spells its own flags where
        // git is called; the entry travels as its **commit**, so nothing a
        // renumbering does between the keypress and here can retarget it.
        assert_eq!(
            recorded(&calls),
            vec![
                "stash push Some(\"index only\") \"the staged side\" [--staged]",
                "stash push None \"notes.md\" [-u]",
                "stash apply cafebabe",
                "stash pop cafebabe",
                "stash drop cafebabe",
                "stash rename cafebabe the parser one",
                "stash branch wip cafebabe",
            ]
        );

        let mut band = Vec::new();
        while let Some(event) = runner.try_next() {
            match event {
                Event::Started { name } => band.push(name),
                Event::Finished { done, .. } => {
                    if let Some(done) = done {
                        band.push(format!("done: {done}"));
                    }
                }
            }
        }
        // The band names the scope rather than the flag, and the number the
        // reader saw rather than the commit they did not.
        assert_eq!(
            band,
            vec![
                "stash the staged side",
                "stash notes.md",
                "stash apply stash@{1}",
                "stash pop stash@{1}",
                "stash drop stash@{1}",
                "rename stash@{1}",
                "done: renamed to the parser one, now at the top of the stack",
                "branch wip from stash@{1}",
                "done: wip starts where stash@{1} was made",
            ]
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
    fn reset_upstream_names_the_tracking_ref_in_full() {
        // The aim is read, and named the way git resolves it: `up/main`,
        // not `@{upstream}`, so the question a reader confirms and the
        // revspec git is handed are the same string.
        let fake = Arc::new(SyncFake::tracked(Some("up"), &["up"]));
        let repo: Handle = fake.clone();
        let job = Write::reset_upstream(&repo, ResetMode::Hard).expect("an aim");
        assert_eq!(job.name(), "reset --hard up/main");
        assert_eq!(fake.said(), Vec::<String>::new(), "nothing ran yet");

        // No upstream configured: a sentence, not a job git would answer
        // with a revspec error.
        let fake = Arc::new(SyncFake::tracked(None, &["origin"]));
        let repo: Handle = fake.clone();
        let err = Write::reset_upstream(&repo, ResetMode::Mixed)
            .err()
            .expect("refused");
        assert!(err.contains("no upstream"), "{err}");

        // Detached HEAD is not a branch, so it tracks nothing.
        let fake = Arc::new(SyncFake::detached());
        let repo: Handle = fake.clone();
        let err = Write::reset_upstream(&repo, ResetMode::Soft)
            .err()
            .expect("refused");
        assert!(err.contains("detached"), "{err}");
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
