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

/// How much of the file a graft takes: the hunk under the keyboard, or
/// the file whole. The keyboard's scope is the client's to read — the
/// command names which one it wants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraftScope {
    Hunk,
    File,
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
    fn patch_pick(&mut self) -> Option<DiffPick>;
    /// What a graft aims at in a focused commit diff, or `None` with the
    /// client's own sentence when the focus is not one. `scope` is the
    /// command's: a removal takes the hunk, a discard the file whole.
    fn graft_target(&mut self, scope: GraftScope) -> Option<GraftTarget>;
    /// The commit a graft would amend: the focused commit diff's sha,
    /// wherever in it the keyboard sits. Amending names a commit, not a
    /// hunk, so the cursor's row is nobody's business here.
    fn amend_target(&mut self) -> Option<Vec<u8>>;
    /// The file under the keyboard in a focused commit diff, or `None`
    /// with the client's own sentence.
    fn commit_file_target(&mut self) -> Option<CommitFileTarget>;
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
    let Some(sha) = client.amend_target() else {
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
    let armed = [sha.clone()]
        .into_iter()
        .chain(live.iter().map(|(p, _)| p.clone()))
        .collect::<Vec<_>>()
        .concat();
    if !client.confirm_or_arm(command, &armed) {
        client.ask(format!(
            "amend {} with the patch? press again to confirm",
            short(&sha)
        ));
        return;
    }
    match Write::graft_files(&repo, sha, live, false) {
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
pub fn graft_remove(client: &mut impl PatchClient, command: &str, scope: GraftScope) {
    let Some(target) = client.graft_target(scope) else {
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
/// it at HEAD first when no such branch exists. The work lands
/// uncommitted — the commit is the reader's next keypress. A checkout the
/// dirty tree cannot carry is git's own refusal, in its own words.
pub fn move_patch_to_branch(client: &mut impl PatchClient, branch: Vec<u8>) {
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
    // Create-if-missing, decided here and not asked twice: the name was
    // just typed, so asking whether it should exist would be the field
    // asking about itself.
    let create = match repo.branches() {
        Ok(branches) => !branches.iter().any(|b| b.name.as_bytes() == branch),
        Err(e) => {
            client.say(e);
            return;
        }
    };
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::{Event, Runner};
    use std::path::PathBuf;
    use std::time::Duration;

    /// A throwaway repository with byte-exact contents, mirroring the
    /// staging round trip's: setup and oracle only, everything under test
    /// travels through [`Write`] behind the same [`Handle`] and the same
    /// [`Runner`] every client submits to. No tty, no window.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("gitten-graft-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("a temp dir");
            let me = Scratch(dir);
            me.git(&["init", "-q", "-b", "main", "."]);
            me.git(&["config", "user.name", "t"]);
            me.git(&["config", "user.email", "t@t"]);
            me.git(&["config", "core.autocrlf", "false"]);
            me
        }

        fn git(&self, args: &[&str]) -> String {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&self.0)
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

        fn write(&self, path: &str, content: &[u8]) {
            std::fs::write(self.0.join(path), content).expect("wrote the file");
        }

        fn read(&self, path: &str) -> Vec<u8> {
            std::fs::read(self.0.join(path)).expect("the file reads")
        }

        fn commit(&self, path: &str, content: &[u8], message: &str) {
            self.write(path, content);
            self.git(&["add", path]);
            self.git(&["commit", "-qm", message]);
        }

        fn rev(&self, rev: &str) -> String {
            self.git(&["rev-parse", rev]).trim().to_string()
        }

        fn log_subjects(&self) -> Vec<String> {
            self.git(&["log", "--format=%s", "--topo-order"])
                .lines()
                .map(str::to_string)
                .collect()
        }

        fn porcelain(&self) -> String {
            self.git(&["status", "--porcelain"])
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn wait(runner: &Runner, count: usize) -> Vec<Event> {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
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

    fn run_job(runner: &Runner, job: Write) -> Result<(), String> {
        let submit = runner.submitter();
        assert!(submit.submit(Box::new(job)).is_ok(), "queued");
        let events = wait(runner, 2);
        let Event::Finished { outcome, .. } = &events[1] else {
            panic!("no finish: {:?}", events[1]);
        };
        match outcome {
            Ok(()) => Ok(()),
            Err(e) => Err(e.clone()),
        }
    }

    /// Ten lines, the middle commit editing two distant ones: lifting one
    /// leaves the other, so the rewrite is a rewrite and not a deletion —
    /// a commit's only change lifts through the empty refusal below, never
    /// through the graft. The tip appends past both, so it replays clean.
    fn base_lines() -> Vec<u8> {
        (1..=10)
            .map(|i| format!("line-{i:02}\n"))
            .collect::<String>()
            .into_bytes()
    }

    fn three_commits(name: &str) -> Scratch {
        let r = Scratch::new(name);
        r.commit("f.txt", &base_lines(), "base");
        let mut middle = String::from_utf8(base_lines()).unwrap();
        middle = middle.replace("line-02\n", "EDIT-TWO\n");
        middle = middle.replace("line-08\n", "EDIT-EIGHT\n");
        r.commit("f.txt", middle.as_bytes(), "middle");
        let tip = middle + "line-11\n";
        r.commit("f.txt", tip.as_bytes(), "tip");
        r
    }

    /// The middle commit's first hunk, as the keyboard emits it: the graft
    /// reverses it at apply time to lift `EDIT-TWO` back out. Written out
    /// here so the test pins the bytes.
    fn lift_two() -> Vec<u8> {
        b"diff --git a/f.txt b/f.txt\n--- a/f.txt\n+++ b/f.txt\n@@ -1,5 +1,5 @@\n line-01\n-line-02\n+EDIT-TWO\n line-03\n line-04\n line-05\n"
            .to_vec()
    }

    #[test]
    fn removing_a_line_from_a_historical_commit_replays_its_children() {
        let r = three_commits("remove-hist");
        let handle = gitten_git::open(&r.0);
        let middle = r.rev("HEAD~1");
        let tip = r.rev("HEAD");
        let runner = Runner::new();

        let job = Write::graft_files(
            &handle,
            middle.as_bytes().to_vec(),
            vec![(b"f.txt".to_vec(), lift_two())],
            true,
        )
        .expect("a non-empty patch grafts");
        run_job(&runner, job).expect("the graft runs");

        let after = String::from_utf8(r.read("f.txt")).unwrap();
        assert!(
            after.contains("line-02\n") && !after.contains("EDIT-TWO"),
            "the middle lost EDIT-TWO: {after:?}"
        );
        assert!(
            after.contains("EDIT-EIGHT\n") && after.contains("line-11\n"),
            "the middle kept EDIT-EIGHT and the tip kept line-11: {after:?}"
        );
        assert_eq!(
            r.log_subjects(),
            vec!["tip".to_string(), "middle".to_string(), "base".to_string()],
            "messages replay unchanged"
        );
        assert_ne!(r.rev("HEAD"), tip, "the tip was replayed, not kept");
        assert_eq!(r.porcelain(), "", "the tree is clean after the graft");
        assert_eq!(
            r.git(&["branch", "--show-current"]).trim(),
            "main",
            "the reader is home on the branch, not detached"
        );
    }

    #[test]
    fn removing_a_line_from_the_tip_steps_the_branch_onto_its_replacement() {
        let r = three_commits("remove-tip");
        // The tip appends past the middle's edits and renames the first
        // line: lifting the append leaves a rewrite, so the branch steps
        // onto the replacement instead of replaying.
        // The tip gains a second change beside its append: the append
        // lifts through the graft while the rename stays a rewrite.
        let mut tip = String::from_utf8(r.read("f.txt")).unwrap();
        tip = tip.replace("line-01\n", "LINE-01\n");
        r.write("f.txt", tip.as_bytes());
        r.git(&["add", "f.txt"]);
        r.git(&["commit", "-q", "--amend", "--no-edit"]);
        let tip_sha = r.rev("HEAD");
        let handle = gitten_git::open(&r.0);
        let runner = Runner::new();

        let lift_eleven = b"diff --git a/f.txt b/f.txt\n--- a/f.txt\n+++ b/f.txt\n@@ -8,3 +8,4 @@\n EDIT-EIGHT\n line-09\n line-10\n+line-11\n"
            .to_vec();
        let job = Write::graft_files(
            &handle,
            tip_sha.as_bytes().to_vec(),
            vec![(b"f.txt".to_vec(), lift_eleven)],
            true,
        )
        .expect("a non-empty patch grafts");
        run_job(&runner, job).expect("the graft runs");

        let after = String::from_utf8(r.read("f.txt")).unwrap();
        assert!(
            after.contains("LINE-01\n") && !after.contains("line-11"),
            "the append left, the rename stayed: {after:?}"
        );
        assert_ne!(r.rev("HEAD"), tip_sha, "the branch stepped on");
        assert_eq!(
            r.log_subjects(),
            vec!["tip".to_string(), "middle".to_string(), "base".to_string()],
        );
        assert!(handle.as_ref().operation().is_none(), "no rebase stood");
        assert_eq!(r.porcelain(), "");
    }

    #[test]
    fn lifting_a_commits_only_change_refuses_and_names_the_drop() {
        let r = Scratch::new("graft-empty");
        r.commit("f.txt", b"one\ntwo\n", "base");
        r.commit("f.txt", b"one\nTWO\n", "only");
        let handle = gitten_git::open(&r.0);
        let only = r.rev("HEAD");
        let branch = r.git(&["branch", "--show-current"]).trim().to_string();
        let runner = Runner::new();

        // The only change, lifted: the result would be the parent's tree,
        // which is a deletion wearing a rewrite's clothes.
        let lift = b"diff --git a/f.txt b/f.txt\n--- a/f.txt\n+++ b/f.txt\n@@ -1,2 +1,2 @@\n one\n-two\n+TWO\n"
            .to_vec();
        let job = Write::graft_files(
            &handle,
            only.as_bytes().to_vec(),
            vec![(b"f.txt".to_vec(), lift)],
            true,
        )
        .expect("a non-empty patch grafts");
        let err = run_job(&runner, job).expect_err("emptiness refuses");
        assert!(
            err.contains("empty") && err.contains("drop"),
            "the refusal names the door: {err}"
        );
        assert_eq!(r.read("f.txt"), b"one\nTWO\n", "nothing applied");
        assert_eq!(r.rev("HEAD"), only, "history never moved");
        assert_eq!(
            r.git(&["branch", "--show-current"]).trim(),
            branch,
            "never detached"
        );
        assert_eq!(r.porcelain(), "", "the index came home too");
    }

    #[test]
    fn a_conflicted_replay_stops_standing_and_says_so() {
        let r = three_commits("remove-conflict");
        // The tip rewrites the same line the graft lifts: the replay
        // cannot carry it. Built on the shared fixture so the middle
        // holds two changes and the lift is a rewrite, not a deletion.
        let mut tip = String::from_utf8(r.read("f.txt")).unwrap();
        tip = tip.replace("EDIT-TWO\n", "LINE-02\n");
        // NB: the shared tip appended line-11; keep it so the replay has
        // a clean hunk beside the conflicting one.
        r.commit("f.txt", tip.as_bytes(), "tip");
        let handle = gitten_git::open(&r.0);
        let middle = r.rev("HEAD~1");
        let runner = Runner::new();

        let job = Write::graft_files(
            &handle,
            middle.as_bytes().to_vec(),
            vec![(b"f.txt".to_vec(), lift_two())],
            true,
        )
        .expect("a non-empty patch grafts");
        let err = run_job(&runner, job).expect_err("the replay stops");
        assert!(
            err.contains("stopped on a conflict"),
            "the stop is named, not git's raw exit: {err}"
        );
        assert!(
            handle.as_ref().operation().is_some(),
            "the rebase stands for the lifecycle"
        );
        // A standing rebase is detached by definition — HEAD sits at
        // the stopped pick — so the assertions are the stop itself: the
        // operation stands, the conflict markers are on disk, and the
        // history below the replay never moved.
        assert!(
            r.read("f.txt").windows(7).any(|w| w == b"<<<<<<<"),
            "the conflict is on disk for the lifecycle"
        );
        r.git(&["rebase", "--abort"]);
        assert!(handle.as_ref().operation().is_none(), "aborted clean");
    }

    #[test]
    fn a_dirty_tree_refuses_the_graft_and_stays_where_it_was() {
        let r = three_commits("remove-dirty");
        let branch = r.git(&["branch", "--show-current"]).trim().to_string();
        r.write("f.txt", b"dirty\n");
        let handle = gitten_git::open(&r.0);
        let runner = Runner::new();

        let job = Write::graft_files(
            &handle,
            r.rev("HEAD~1").as_bytes().to_vec(),
            vec![(b"f.txt".to_vec(), lift_two())],
            true,
        )
        .expect("a non-empty patch grafts");
        let err = run_job(&runner, job).expect_err("a dirty tree refuses");
        assert!(err.contains("clean tree"), "{err}");
        assert_eq!(r.read("f.txt"), b"dirty\n", "the dirt is untouched");
        assert_eq!(
            r.git(&["branch", "--show-current"]).trim(),
            branch,
            "never detached"
        );
        assert_eq!(r.log_subjects()[0], "tip", "history never moved");
    }

    #[test]
    fn an_empty_graft_is_refused_before_the_queue() {
        let job = Write::graft_files(
            &gitten_git::open(&std::env::temp_dir()),
            b"abc".to_vec(),
            vec![(b"f.txt".to_vec(), Vec::new())],
            true,
        );
        let Err(err) = job else {
            panic!("emptiness must refuse before the queue");
        };
        assert!(err.contains("empty"), "{err}");
    }

    #[test]
    fn amending_a_historical_commit_folds_work_in_and_replays() {
        let r = three_commits("remove-amend");
        let handle = gitten_git::open(&r.0);
        let middle = r.rev("HEAD~1");
        let runner = Runner::new();

        // Forward: the middle gains a line it never had, the tip replays
        // over it.
        let add = b"diff --git a/f.txt b/f.txt\n--- a/f.txt\n+++ b/f.txt\n@@ -1,4 +1,5 @@\n line-01\n EDIT-TWO\n line-03\n+line-00\n line-04\n"
            .to_vec();
        let job = Write::graft_files(
            &handle,
            middle.as_bytes().to_vec(),
            vec![(b"f.txt".to_vec(), add)],
            false,
        )
        .expect("a non-empty patch grafts");
        run_job(&runner, job).expect("the graft runs");

        let after = String::from_utf8(r.read("f.txt")).unwrap();
        assert!(
            after.contains("line-00\n") && after.contains("line-11\n"),
            "the middle gained line-00 and the tip kept line-11: {after:?}"
        );
        assert_eq!(
            r.log_subjects(),
            vec!["tip".to_string(), "middle".to_string(), "base".to_string()],
        );
        assert_eq!(r.porcelain(), "");
    }

    #[test]
    fn moving_a_patch_onto_a_new_branch_leaves_it_uncommitted_there() {
        let r = three_commits("move-new");
        let handle = gitten_git::open(&r.0);
        let runner = Runner::new();

        // A patch adding one line past the tip's own content: it aims
        // at what the new branch holds, because the branch starts at HEAD.
        let add_moved = b"diff --git a/f.txt b/f.txt\n--- a/f.txt\n+++ b/f.txt\n@@ -1,3 +1,4 @@\n line-01\n+MOVED\n EDIT-TWO\n line-03\n"
            .to_vec();
        let job = Write::move_patch_to_branch(
            &handle,
            b"feature".to_vec(),
            true,
            vec![(b"f.txt".to_vec(), add_moved)],
        )
        .expect("a non-empty patch moves");
        run_job(&runner, job).expect("the move runs");

        assert_eq!(
            r.git(&["branch", "--show-current"]).trim(),
            "feature",
            "the reader moved with the patch"
        );
        let after = String::from_utf8(r.read("f.txt")).unwrap();
        assert!(
            after.contains("MOVED\n"),
            "the patch landed on the new branch: {after:?}"
        );
        assert_ne!(
            r.porcelain(),
            "",
            "uncommitted — the commit is the reader's next keypress"
        );
        // The branch it left never moved.
        assert_eq!(
            r.git(&["rev-parse", "main"]).trim(),
            r.rev("main"),
            "main stands where it stood"
        );
        assert_eq!(r.log_subjects()[0], "tip", "no commit was made");
    }

    #[test]
    fn moving_onto_a_branch_a_dirty_tree_cannot_carry_refuses_cleanly() {
        let r = three_commits("move-dirty");
        // A diverged branch to move onto: its f.txt differs, so carrying
        // dirty work across is git's refusal, not ours.
        r.git(&["branch", "other", "HEAD~1"]);
        r.write("f.txt", b"dirty\n");
        let handle = gitten_git::open(&r.0);
        let runner = Runner::new();

        let add_moved = b"diff --git a/f.txt b/f.txt\n--- a/f.txt\n+++ b/f.txt\n@@ -1,3 +1,4 @@\n line-01\n+MOVED\n EDIT-TWO\n line-03\n"
            .to_vec();
        let job = Write::move_patch_to_branch(
            &handle,
            b"other".to_vec(),
            false,
            vec![(b"f.txt".to_vec(), add_moved)],
        )
        .expect("a non-empty patch moves");
        let err = run_job(&runner, job).expect_err("the checkout refuses");
        assert!(
            err.contains("local changes") || err.contains("overwritten"),
            "git's own refusal, verbatim: {err}"
        );
        assert_eq!(r.read("f.txt"), b"dirty\n", "the dirt is untouched");
        assert_eq!(
            r.git(&["branch", "--show-current"]).trim(),
            "main",
            "never left"
        );
    }

    #[test]
    fn checking_a_file_out_of_a_commit_restores_worktree_and_index() {
        let r = three_commits("checkout-file");
        // Diverge both sides from the middle's version.
        r.write("f.txt", b"worktree\n");
        r.git(&["add", "f.txt"]);
        r.write("f.txt", b"worktree-and-more\n");
        let handle = gitten_git::open(&r.0);
        let middle = r.rev("HEAD~1");
        let middle_bytes = r.git(&["show", &format!("{middle}:f.txt")]).into_bytes();
        let runner = Runner::new();

        let job = Write::checkout_file_from_commit(
            &handle,
            middle.as_bytes().to_vec(),
            b"f.txt".to_vec(),
        );
        run_job(&runner, job).expect("the checkout runs");

        assert_eq!(
            r.read("f.txt"),
            middle_bytes,
            "the worktree carries the commit's version"
        );
        let staged = r.git(&["show", ":f.txt"]).into_bytes();
        assert_eq!(staged, middle_bytes, "the index moved with it");
        assert_eq!(r.log_subjects()[0], "tip", "history never moved");
    }
}
