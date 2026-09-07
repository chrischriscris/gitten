//! The patch clipboard's decisions: picking, applying, grafting, moving.
//!
//! `act` holds what a verb decides for clients with panes of their own;
//! this holds the patch family, which no earlier packet needed. A client
//! supplies the things it genuinely owns — the hunks under its keyboard,
//! the clipboard it keeps across presses, the builder it opens — through
//! [`PatchClient`], and nothing else here knows a screen exists.
//!
//! Three flows share one clipboard. A pick carries hunks from any diff to
//! anywhere: the worktree, the index, another branch, a historical commit.
//! A graft folds picked work into a commit that already exists, replaying
//! what followed it. A move carries picked work onto another branch,
//! uncommitted. What separates them is the target, never the text.

use crate::act::{
    check_context, resolve_side_hunks, CheckedPatch, Client, ExpectOid, HunkSide, OidSide,
    PatchVerb,
};
use crate::verbs::Write;
use gitten_core::patchclip::{Anchor, PatchClipboard};
use gitten_core::Hunk;
use gitten_git::Handle;

/// Where a pick came from, in the diff's own terms. Combined, revspec,
/// conflict, fixture and patch diffs name no pickable read — the client
/// refuses those before this is ever built.
#[derive(Clone, Debug)]
pub enum PickOrigin {
    Staged,
    Unstaged,
    Untracked,
    Commit { sha: Vec<u8> },
    Stash { commit: Vec<u8> },
}

/// What the keyboard picked in a diff: the path, the drawn selection, and
/// the read it was drawn from. The hunks are clones of what the screen
/// drew; working-tree origins are re-read and matched before anything is
/// kept, while a commit or stash read is immutable and kept as drawn.
#[derive(Clone, Debug)]
pub struct DiffPick {
    pub path: String,
    pub selection: crate::act::HunkSelection,
    pub origin: PickOrigin,
}

/// What a graft aims at in a focused commit diff: the commit, the path,
/// and the hunks in the keyboard's scope — one hunk under it for a
/// removal, the file's whole hunks for a discard. `parts` carries marked
/// line ranges when the keyboard marked lines; `None` means whole hunks.
#[derive(Clone, Debug)]
pub struct GraftTarget {
    pub sha: Vec<u8>,
    pub path: String,
    pub hunks: Vec<Hunk>,
    pub parts: Option<Vec<(Hunk, usize, usize)>>,
    /// A rename grafts whole or not at all — a partial patch cannot rename
    /// — so the client names it and this refuses with the checkout door.
    pub renamed: bool,
    /// A binary file has no lines to window; it grafts whole through
    /// checkout, never through a patch.
    pub binary: bool,
}

/// The file under the keyboard in a focused commit diff, for the verbs
/// that act on a whole file from a commit.
#[derive(Clone, Debug)]
pub struct CommitFileTarget {
    pub sha: Vec<u8>,
    pub path: Vec<u8>,
}

/// Where an apply or a reverse lands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PatchTarget {
    Worktree,
    Index,
}

/// The client-owned selection and confirmation state the patch family
/// needs. The clipboard lives across presses — picked in one diff,
/// applied from another — so the client holds it beside the cherry-pick
/// clipboard it already keeps, and the status names both.
pub trait PatchClient: Client {
    /// The patch clipboard the client keeps across presses.
    fn patch_clipboard(&mut self) -> &mut PatchClipboard;
    /// What the keyboard picked in the focused diff, or `None` when the
    /// focus names no pickable read. A combined or fixture diff is `None`
    /// with its own sentence, said by the client that knows the focus.
    fn patch_pick(&self) -> Option<DiffPick>;
    /// What a graft aims at in a focused commit diff, or `None` with the
    /// client's own sentence when the focus is not one.
    fn graft_target(&self) -> Option<GraftTarget>;
    /// The file under the keyboard in a focused commit diff, or `None`
    /// with the client's own sentence.
    fn commit_file_target(&self) -> Option<CommitFileTarget>;
    /// Opens the builder over the clipboard. Required rather than
    /// defaulted: a client that offers the key and forgets the answer is
    /// the silent no-op this trait exists to make impossible.
    fn open_patch_builder(&mut self);
}

/// `patch.pick`: keeps the keyboard's hunks on the clipboard, resolved
/// against a fresh read for working-tree origins.
///
/// A pick never applies — the clipboard is the carrier, and carrying is
/// not landing — so this refuses nothing destructive and confirms
/// nothing. What it refuses is a pick that would land nowhere: a binary
/// or a rename from the working tree, a selection with no changed lines,
/// a read with no context to aim by. A commit or stash pick is kept as
/// drawn: the read is immutable, so there is nothing to go stale.
pub fn patch_pick(
    client: &mut impl PatchClient,
    differs: &gitten_core::differ::Differs,
    over: &gitten_core::differ::Overrides,
) {
    let Some(pick) = client.patch_pick() else {
        return;
    };
    let Some(repo) = client.repo() else {
        client.say("a fixture has no repository to pick from".into());
        return;
    };
    let (anchor, index_oid, head_oid, hunks) = match pick.origin {
        PickOrigin::Commit { sha } => (
            Anchor::Commit {
                sha: String::from_utf8_lossy(&sha).into_owned(),
            },
            None,
            None,
            drawn_hunks(pick.selection),
        ),
        PickOrigin::Stash { commit } => (
            Anchor::Stash {
                commit: String::from_utf8_lossy(&commit).into_owned(),
            },
            None,
            None,
            drawn_hunks(pick.selection),
        ),
        PickOrigin::Staged | PickOrigin::Unstaged | PickOrigin::Untracked => {
            let side = match pick.origin {
                PickOrigin::Staged => HunkSide::Staged,
                PickOrigin::Unstaged => HunkSide::Unstaged,
                _ => HunkSide::Untracked,
            };
            let resolved = match resolve_side_hunks(
                &repo,
                side,
                &pick.path,
                &pick.selection,
                gitten_core::patch::Unselected::KeepRemovals,
                differs,
                over,
            ) {
                Ok(resolved) => resolved,
                Err(e) => {
                    client.say(e);
                    return;
                }
            };
            if resolved.binary {
                client.say(format!(
                    "{} is binary — pick it whole from the files pane",
                    pick.path
                ));
                return;
            }
            if resolved
                .old_path
                .as_ref()
                .is_some_and(|old| old != &pick.path)
            {
                client.say(format!(
                    "{} is a rename — pick it whole from the files pane",
                    pick.path
                ));
                return;
            }
            if let Err(e) = check_context(&resolved.chosen) {
                client.say(e);
                return;
            }
            if resolved.chosen.is_empty() {
                client.say("no changed lines in the selection".into());
                return;
            }
            let anchor = match side {
                HunkSide::Staged => Anchor::Staged,
                HunkSide::Unstaged => Anchor::Unstaged,
                _ => Anchor::Untracked,
            };
            // The preimage side is what a forward apply matches: the
            // staged patch against HEAD's blob, the unstaged one against
            // the index's. The other OID rides along for the reverse.
            let (index_oid, head_oid) = match side {
                HunkSide::Staged => (resolved.new_oid, resolved.old_oid),
                HunkSide::Unstaged => (resolved.old_oid, None),
                _ => (None, None),
            };
            // The reverse of an unstaged pick is aimed by context alone —
            // no OID names the worktree — so HEAD's blob is fetched as the
            // one extra identity a later reverse could still check. It
            // costs one read at pick time and fails the pick at nothing.
            let head_oid = match side {
                HunkSide::Unstaged => repo
                    .head_blob_oid(pick.path.as_bytes())
                    .unwrap_or(None)
                    .or(head_oid),
                _ => head_oid,
            };
            (anchor, index_oid, head_oid, resolved.chosen)
        }
    };
    if hunks.is_empty() {
        client.say("no changed lines in the selection".into());
        return;
    }
    let added =
        client
            .patch_clipboard()
            .pick(pick.path.clone(), anchor, index_oid, head_oid, hunks);
    let held = client.patch_clipboard().included_hunks();
    if added == 0 {
        client.say(format!(
            "already on the patch — {held} hunks held, press c to clear"
        ));
    } else {
        client.say(format!(
            "picked {added} hunk{} — {held} on the patch",
            plural(added)
        ));
    }
}

/// The drawn selection as whole hunks — the immutable origins' shape.
/// Line windows need a verb's keep rule, which a pick has not chosen yet,
/// so marked lines on a commit or stash pick keep the whole hunks they
/// touch: narrowing is the builder's work, and the builder draws from
/// these same hunks.
fn drawn_hunks(selection: crate::act::HunkSelection) -> Vec<Hunk> {
    use crate::act::HunkSelection;
    match selection {
        HunkSelection::Whole(hunk) => vec![hunk],
        HunkSelection::Lines(parts) => parts.into_iter().map(|(h, _, _)| h).collect(),
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// What one clipboard file's write re-checks, by anchor and direction.
/// Forward matches the preimage — staged against HEAD's blob, unstaged
/// against the index's. Reverse matches the postimage — staged against
/// the index's; an unstaged reverse is aimed by context alone, because no
/// OID names the worktree. Immutable reads expect nothing: a commit never
/// moves under its patch.
fn expects_for(file: &gitten_core::patchclip::PickedFile, reverse: bool) -> Vec<ExpectOid> {
    let mut expect = Vec::new();
    let path = file.path.clone().into_bytes();
    match (&file.anchor, reverse) {
        (Anchor::Staged, false) => {
            expect.push(ExpectOid {
                side: OidSide::Head,
                path,
                oid: file.head_oid.clone(),
            });
        }
        (Anchor::Staged, true) => {
            expect.push(ExpectOid {
                side: OidSide::Index,
                path,
                oid: file.index_oid.clone(),
            });
        }
        (Anchor::Unstaged, false) => {
            expect.push(ExpectOid {
                side: OidSide::Index,
                path,
                oid: file.index_oid.clone(),
            });
        }
        _ => {}
    }
    expect
}

/// Builds one checked write per included file, or refuses with the
/// clipboard's own sentence. Files with nothing included are named and
/// skipped — they were toggled off in the builder, which is an answer,
/// not an oversight — and all files empty is one refusal, not silence.
fn checked_writes(
    client: &mut impl PatchClient,
    target: PatchTarget,
    reverse: bool,
) -> Option<Vec<CheckedPatch>> {
    let repo = client.repo()?;
    let clip = client.patch_clipboard();
    let emitted = clip.emit();
    let live: Vec<(String, Vec<u8>)> = emitted
        .into_iter()
        .filter(|(_, patch)| !patch.is_empty())
        .collect();
    if live.is_empty() {
        if clip.is_empty() {
            client.say("the patch clipboard is empty — pick hunks first".into());
        } else {
            client.say("nothing on the patch is included — toggle hunks on in the builder".into());
        }
        return None;
    }
    let verb = match (target, reverse) {
        (PatchTarget::Worktree, false) => PatchVerb::ApplyWorktree,
        (PatchTarget::Index, false) => PatchVerb::ApplyIndex,
        (PatchTarget::Worktree, true) => PatchVerb::ReverseWorktree,
        (PatchTarget::Index, true) => PatchVerb::ReverseIndex,
    };
    let name = match (target, reverse) {
        (PatchTarget::Worktree, false) => "apply patch onto the worktree",
        (PatchTarget::Index, false) => "apply patch onto the index",
        (PatchTarget::Worktree, true) => "reverse patch off the worktree",
        (PatchTarget::Index, true) => "reverse patch off the index",
    };
    Some(
        live.into_iter()
            .map(|(path, patch)| {
                let expect = clip
                    .files()
                    .iter()
                    .find(|f| f.path == path)
                    .map(|f| expects_for(f, reverse))
                    .unwrap_or_default();
                CheckedPatch {
                    name: format!("{name}: {path}"),
                    repo: Handle::clone(&repo),
                    verb,
                    patch,
                    expect,
                }
            })
            .collect(),
    )
}

/// Applies the clipboard forwards onto `target`. Additive — the reverse
/// undoes it — so nothing confirms.
pub fn patch_apply(client: &mut impl PatchClient, _command: &str, target: PatchTarget) {
    let Some(jobs) = checked_writes(client, target, false) else {
        return;
    };
    for job in jobs {
        if !client.submit(Box::new(job)) {
            client.say("the job queue is shutting down".into());
            return;
        }
    }
}

/// Reverses the clipboard off `target`. Off the worktree that destroys
/// lines, so it asks twice; off the index it only rewrites what an apply
/// wrote, and asks nothing.
pub fn patch_reverse(client: &mut impl PatchClient, command: &str, target: PatchTarget) {
    if target == PatchTarget::Worktree && !client.confirm_or_arm(command, b"patch worktree") {
        client.ask("reverse the patch off the worktree? press again to confirm".into());
        return;
    }
    let Some(jobs) = checked_writes(client, target, true) else {
        return;
    };
    for job in jobs {
        if !client.submit(Box::new(job)) {
            client.say("the job queue is shutting down".into());
            return;
        }
    }
}

/// `patch.clear`: empties the clipboard. Applying never clears — the same
/// patch is often wanted on a second target — so only this takes it away,
/// and clearing an empty one is worth saying rather than silently
/// accepting.
pub fn patch_clear(client: &mut impl PatchClient) {
    if client.patch_clipboard().is_empty() {
        client.say("the patch clipboard is already empty".into());
        return;
    }
    client.patch_clipboard().clear();
    client.say("the patch clipboard is empty".into());
}

/// `patch.menu` (`ctrl+p`): the question, not the write. The answers are
/// commands of their own — apply, reverse, builder, clear — so the menu
/// names them and the keys below it run them.
pub fn patch_menu(client: &mut impl PatchClient) -> bool {
    if client.repo().is_none() {
        client.say("a fixture has no repository to patch in".into());
        return false;
    }
    let status = client.patch_clipboard().status();
    client.ask(format!(
        "{status} — a applies onto the worktree, i onto the index, \
             r reverses off the worktree, u off the index, b opens the builder, \
             c clears it"
    ));
    true
}

/// `patch.show`: opens the builder over the clipboard. Showing is never
/// refused — an empty builder says empty, which is a fact and not a
/// failure — but a fixture holds no repository to aim from, and the
/// builder's applies would be lies there.
pub fn patch_show(client: &mut impl PatchClient) {
    if client.repo().is_none() {
        client.say("a fixture has no repository to patch in".into());
        return;
    }
    client.open_patch_builder();
}

/// Folds the clipboard's patches into the commit `sha` names — the amend
/// half of grafting. The clipboard carries work picked anywhere; the
/// commit is the graft target standing under the keyboard. Asked twice,
/// because history moves and the replay that follows moves with it.
pub fn graft_amend(client: &mut impl PatchClient, command: &str) {
    let Some(target) = client.graft_target() else {
        return;
    };
    let Some(repo) = client.repo() else {
        client.say("a fixture has no history to rewrite".into());
        return;
    };
    let emitted = client.patch_clipboard().emit();
    let live: Vec<(Vec<u8>, Vec<u8>)> = emitted
        .into_iter()
        .filter(|(_, patch)| !patch.is_empty())
        .map(|(path, patch)| (path.into_bytes(), patch))
        .collect();
    if live.is_empty() {
        client.say("nothing on the patch is included — pick hunks first".into());
        return;
    }
    let armed = [target.sha.clone()]
        .into_iter()
        .chain(live.iter().map(|(p, _)| p.clone()))
        .collect::<Vec<_>>()
        .concat();
    if !client.confirm_or_arm(command, &armed) {
        client.ask(format!(
            "amend {} with the patch? press again to confirm",
            short(&target.sha)
        ));
        return;
    }
    match Write::graft_files(&repo, target.sha, live, false) {
        Ok(job) => {
            if !client.submit(Box::new(job)) {
                client.say("the job queue is shutting down".into());
            }
        }
        Err(e) => client.say(e),
    }
}

/// Removes the graft target's hunks from its own commit — the removal half
/// of grafting. `parts` windows the hunks with the reverse keep; whole
/// hunks emit as drawn. Asked twice: the commit moves and its children
/// move with it.
pub fn graft_remove(client: &mut impl PatchClient, command: &str) {
    let Some(target) = client.graft_target() else {
        return;
    };
    if target.binary {
        client.say(format!(
            "{} is binary — check it out from the commit instead of grafting it",
            target.path
        ));
        return;
    }
    if target.renamed {
        client.say(format!(
            "{} is a rename — check it out from the commit instead of grafting it",
            target.path
        ));
        return;
    }
    let Some(repo) = client.repo() else {
        client.say("a fixture has no history to rewrite".into());
        return;
    };
    let mut chosen: Vec<Hunk> = Vec::new();
    match &target.parts {
        Some(parts) => {
            for (drawn, lo, hi) in parts {
                if let Some(window) = gitten_core::patch::line_window(
                    drawn,
                    *lo,
                    *hi,
                    gitten_core::patch::Unselected::KeepAdditions,
                ) {
                    chosen.push(window);
                }
            }
            if chosen.is_empty() {
                client.say("no changed lines in the selection".into());
                return;
            }
        }
        None => chosen = target.hunks.clone(),
    }
    if chosen.is_empty() {
        client.say("nothing selected to remove".into());
        return;
    }
    if let Err(e) = check_context(&chosen) {
        client.say(e);
        return;
    }
    let refs: Vec<&Hunk> = chosen.iter().collect();
    let patch = gitten_core::patch::emit(&target.path, &refs);
    if patch.is_empty() {
        client.say("nothing selected to remove".into());
        return;
    }
    let mut armed = target.sha.clone();
    armed.extend_from_slice(target.path.as_bytes());
    if !client.confirm_or_arm(command, &armed) {
        client.ask(format!(
            "remove this from {}? press again to confirm",
            short(&target.sha)
        ));
        return;
    }
    match Write::graft_files(
        &repo,
        target.sha,
        vec![(target.path.into_bytes(), patch)],
        true,
    ) {
        Ok(job) => {
            if !client.submit(Box::new(job)) {
                client.say("the job queue is shutting down".into());
            }
        }
        Err(e) => client.say(e),
    }
}

/// `patch.move-to-branch`: carries the clipboard onto `branch`, creating
/// it at HEAD first when asked. The work lands uncommitted — the commit
/// is the reader's next keypress. A checkout the dirty tree cannot carry
/// is git's own refusal, in its own words.
pub fn move_patch_to_branch(client: &mut impl PatchClient, branch: Vec<u8>, create: bool) {
    let Some(repo) = client.repo() else {
        client.say("a fixture has no branches to move onto".into());
        return;
    };
    let emitted = client.patch_clipboard().emit();
    let live: Vec<(Vec<u8>, Vec<u8>)> = emitted
        .into_iter()
        .filter(|(_, patch)| !patch.is_empty())
        .map(|(path, patch)| (path.into_bytes(), patch))
        .collect();
    if live.is_empty() {
        client.say("nothing on the patch is included — pick hunks first".into());
        return;
    }
    match Write::move_patch_to_branch(&repo, branch, create, live) {
        Ok(job) => {
            if !client.submit(Box::new(job)) {
                client.say("the job queue is shutting down".into());
            }
        }
        Err(e) => client.say(e),
    }
}

/// Restores one file to the version a commit holds — worktree and index
/// together, which is git's semantic and is said when asked. Asked twice:
/// staged work on this path is replaced, not kept.
pub fn commit_file_checkout(client: &mut impl PatchClient, command: &str) {
    let Some(target) = client.commit_file_target() else {
        return;
    };
    let Some(repo) = client.repo() else {
        client.say("a fixture has no commits to check out from".into());
        return;
    };
    let mut armed = target.sha.clone();
    armed.extend_from_slice(&target.path);
    if !client.confirm_or_arm(command, &armed) {
        client.ask(format!(
            "check out {} from {}? worktree and index both move — press again to confirm",
            String::from_utf8_lossy(&target.path),
            short(&target.sha)
        ));
        return;
    }
    let job = Write::checkout_file_from_commit(&repo, target.sha, target.path);
    if !client.submit(Box::new(job)) {
        client.say("the job queue is shutting down".into());
    }
}

fn short(sha: &[u8]) -> String {
    const HEX: usize = 8;
    let text = String::from_utf8_lossy(sha);
    text.chars().take(HEX).collect()
}
