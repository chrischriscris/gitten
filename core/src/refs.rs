//! The names git keeps: branches, stashes, remotes, tags and the reflog.
//!
//! These are the shapes the read side of a repository fills — what a branch
//! panel, a stash panel or a push button needs to know before it can offer a
//! verb. Like [`crate::status`], they are pure data: acquisition lives in
//! `gitten-git`, drawing lives in a client, and neither gets to teach these
//! types about the other.
//!
//! Two modelling rules run through all of them:
//!
//! **Names are bytes.** A branch name is addressed back to git by every verb
//! that will ever hang off it — checkout, push, delete — so it travels
//! exactly as git emitted it, undecoded, for the reason [`PathBytes`] spells
//! out.
//!
//! **Absence is data, not an error.** Detached HEAD is a state. A branch
//! whose upstream was deleted on the server still has an upstream — its
//! counts are simply unknowable. A repository with no stashes answers an
//! empty list.

use crate::status::PathBytes;
use std::borrow::Cow;

// ---------------------------------------------------------------------- names

/// A branch, tag or remote name exactly as git emitted it: raw bytes, never
/// decoded.
///
/// Git attaches no encoding to a ref name any more than to a pathname, and
/// real repositories carry ones that are not valid UTF-8. Verbs aim at these
/// names — checking out a branch hands its bytes back to git — so a lossy
/// decode at the boundary would mangle the one thing the verb needed. It is
/// [`PathBytes`] under another word on purpose: the machinery is identical
/// because the discipline is identical, and a second type would be thirty
/// lines of the same guarantees drifting apart.
pub type RefName = crate::status::PathBytes;

/// What the keyboard is on, as verbs aim at it: bytes, never display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A local branch, named relative to `refs/heads`.
    Local(RefName),
    /// A remote-tracking branch. Checkout may aim here — git detaches onto
    /// the fetched commit — but rename and delete refuse tonight, on purpose.
    Remote { remote: RefName, branch: RefName },
    /// The detached-HEAD row: a place, not a branch, and every branch verb
    /// says so rather than guessing which branch was meant.
    Detached,
}

// ----------------------------------------------------------------------- head

/// Where `HEAD` points right now.
///
/// Detached — checked out on a commit rather than a branch — is a state of
/// HEAD and not a failure to read it: half a bisect, a rebase in progress and
/// "just looking at yesterday" all live here. Modelling it as an error would
/// make every one of those sessions look broken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeadState {
    /// Attached to a branch, named relative to `refs/heads`.
    ///
    /// `commit` is `None` only in a repository with no commits yet, where the
    /// branch exists as a name and nothing else — an unborn branch, which is
    /// what every fresh `git init` produces and not a state worth refusing to
    /// open over.
    Branch {
        name: RefName,
        commit: Option<String>,
    },
    /// Detached: HEAD holds a commit id directly.
    Detached { commit: String },
}

// --------------------------------------------------------------------- history

/// How far a reset takes the index and the working tree along.
///
/// The three strengths git itself names, and nothing about git beyond them:
/// which parts of the repository follow the branch pointer backwards is the
/// whole of what the word means here, so it lives in `core` where every
/// client and an extension can aim it without learning this crate exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetMode {
    /// The branch moves alone. Everything staged stays staged, everything in
    /// the working tree stays put — the changes simply become changes *against
    /// the new place*.
    Soft,
    /// The branch and the index move together; the working tree keeps its
    /// files as they are, so the reset's own step comes back as unstaged work.
    Mixed,
    /// Branch, index and working tree all move. Anything unstaged is gone,
    /// which is why this one strength is confirmed twice in every client.
    Hard,
}

impl ResetMode {
    /// git's own flag spelling, for the band that names a running job.
    pub fn flag(self) -> &'static str {
        match self {
            ResetMode::Soft => "--soft",
            ResetMode::Mixed => "--mixed",
            ResetMode::Hard => "--hard",
        }
    }
}

// -------------------------------------------------------------------- branches

/// A local branch, as `refs/heads` holds it.
///
/// One row of a branches panel: what it is called, where it points, whether
/// HEAD sits on it, and — when it tracks one — how it sits against its
/// upstream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Branch {
    /// The branch name, relative to `refs/heads`.
    pub name: RefName,
    /// The commit it points at, full object id.
    pub commit: String,
    /// The remote branch it pulls from and pushes to, when it tracks one.
    pub upstream: Option<Upstream>,
    /// HEAD is attached here. Exactly one branch carries this in a normal
    /// session, none while detached — see [`HeadState::Detached`].
    pub head: bool,
}

impl Branch {
    /// The name as a panel displays it, e.g. `main`.
    pub fn display(&self) -> Cow<'_, str> {
        self.name.to_string_lossy()
    }
}

/// The remote branch a local branch pulls from and pushes to, with the counts
/// that say whether it has moved.
///
/// Both halves come from the branch's own configuration, so the pair survives
/// a remote whose URL changed and a branch name containing slashes — joining
/// the two back together by string surgery is how a remote named `a/b` gets
/// misread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upstream {
    /// The remote half, as the remote is named locally, e.g. `origin`. This
    /// is what a push addresses, so it stays bytes.
    pub remote: RefName,
    /// The branch on that remote, relative to its `refs/heads`, e.g. `main`.
    pub branch: RefName,
    /// Commits this branch has that the upstream lacks — what a push would
    /// send.
    ///
    /// `None` means git cannot compare, which has a cause of its own: the
    /// upstream's ref no longer exists locally ("gone", deleted on the
    /// server, or never fetched). A zero and an unknowable are different
    /// facts, and a panel that showed `0` would invite a push that fixes
    /// nothing.
    pub ahead: Option<u32>,
    /// Commits the upstream has that this branch lacks — what a pull would
    /// bring. `None` under the same conditions as [`Upstream::ahead`].
    pub behind: Option<u32>,
}

/// A branch as some remote holds it, from `refs/remotes/<remote>/<branch>`.
///
/// It is the counterpart a local branch's [`Upstream`] points at, and the
/// thing a fetch updates. Its two names are kept apart rather than joined
/// into `origin/main`, because the join is lossy: remotes may contain
/// slashes, so one string cannot say where the remote ends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteBranch {
    /// The remote it came from, as named locally.
    pub remote: RefName,
    /// The branch name on that remote.
    pub branch: RefName,
    /// The commit it points at, full object id, as of the last fetch.
    pub commit: String,
}

// --------------------------------------------------------------------- stashes

/// One stash: work parked on the stash stack, newest first.
///
/// `index` is the position on that stack, the `n` of `stash@{n}` — which is
/// also how a pop or an apply addresses it back to git.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stash {
    /// Position on the stack, newest first: `0` is the most recent.
    pub index: usize,
    /// What it says about itself — the message given at `git stash push`, or
    /// the `WIP on …` git writes when none was given.
    pub message: String,
    /// The commit the stash hangs on, full object id.
    pub commit: String,
}

/// Which changes a stash push takes — and, by omission, which it must leave
/// standing in the index and the working tree.
///
/// The omission is the whole point of the type. "Stash the staged side"
/// means *and leave my unstaged work where it is*; a variant that swept it
/// along would be a keypress that took away more than it named, which is
/// the one thing a stash must never do. So the scopes are named by what
/// they take, and every one of them is a claim about what stays.
///
/// The flags are git's own spelling, kept here beside the concept for the
/// reason [`ResetMode::flag`] is: which words reach the command line is one
/// fact, and a client that offers a menu of these should not have to know
/// any of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StashScope {
    /// Every tracked change, staged and unstaged — `git stash push`. The
    /// default, and the one a bare "stash" means. Untracked files stay:
    /// they are in no tree and no index, so nothing about "park my changes"
    /// says they were meant.
    Tracked,
    /// Every tracked change *plus* the untracked files — `-u`. Ignored files
    /// still stay, which is git's own line and the right one: an ignored
    /// file is not work.
    WithUntracked,
    /// Only what the index holds — `--staged`. The unstaged half of a file
    /// stays in the working tree, unstaged.
    ///
    /// One honest limit, and it is git's: a path with changes on *both*
    /// sides cannot have its staged side lifted out alone, because the
    /// worktree side of that path is a patch git then cannot reverse. Git
    /// says so and fails; the entry it had already written stays on the
    /// stack, and nothing in the index or the working tree is touched.
    Staged,
    /// Only what the working tree holds past the index — `--keep-index`.
    /// The staged work stays staged and stays in the tree, which is the
    /// promise this scope makes.
    ///
    /// The *entry* git writes carries both sides, because `--keep-index`
    /// records the whole difference from HEAD and then puts the index back.
    /// Nothing is taken away that was not named — that is the invariant —
    /// but an entry made this way, applied later onto a tree that still has
    /// the staged work, is git applying a change that is already there.
    Unstaged,
    /// One path, both its sides — `git stash push -- <path>`. Every other
    /// path stays exactly as it was: still on disk, still staged if it was
    /// staged.
    ///
    /// The same honest limit as [`StashScope::Unstaged`], and git's again:
    /// the *entry* records the whole working tree, and only the named path is
    /// reverted out of it. Nothing excluded is taken away — that is the
    /// invariant — but an entry made this way is not a patch of one file, and
    /// applying it later brings the rest of that moment back with it.
    Path {
        path: PathBytes,
        /// The path is untracked, so the push needs `-u` to see it at all:
        /// git answers a pathspec naming nothing it tracks with "did not
        /// match any file(s) known to git" and stashes nothing.
        untracked: bool,
    },
}

impl StashScope {
    /// git's own flags for this scope, in the order they go on the command
    /// line. Empty for the two that need none.
    pub fn flags(&self) -> &'static [&'static str] {
        match self {
            StashScope::Tracked => &[],
            StashScope::WithUntracked => &["-u"],
            StashScope::Staged => &["--staged"],
            StashScope::Unstaged => &["--keep-index"],
            StashScope::Path {
                untracked: false, ..
            } => &[],
            StashScope::Path {
                untracked: true, ..
            } => &["-u"],
        }
    }

    /// The one path this scope is confined to, when it is confined to one.
    pub fn path(&self) -> Option<&PathBytes> {
        match self {
            StashScope::Path { path, .. } => Some(path),
            _ => None,
        }
    }

    /// What the running band and a refusal call this scope — what it takes,
    /// said the way a person would.
    pub fn label(&self) -> String {
        match self {
            StashScope::Tracked => "the working tree".into(),
            StashScope::WithUntracked => "the working tree and its new files".into(),
            StashScope::Staged => "the staged side".into(),
            StashScope::Unstaged => "the unstaged side".into(),
            StashScope::Path { path, .. } => path.to_string_lossy().into_owned(),
        }
    }
}

/// A stash entry as a verb aims at it: the commit that survives stack churn,
/// and the position it sat at when the keyboard chose it.
///
/// **The commit is the identity and the index is only a memory of where it
/// was.** Every push, pop and drop renumbers the stack — the former
/// `stash@{1}` *is* `stash@{0}` after one drop — so an index captured when a
/// row was selected and spent when a key was pressed can name a different
/// entry entirely. The commit cannot: a stash's commit is written once and
/// never moves.
///
/// The index is still carried, for two reasons that are not nostalgia.
/// `git stash pop` and `git stash drop` accept **only** a `stash@{n}`
/// reference and refuse a raw object id, so something has to turn the
/// identity back into a position at the moment of the write; and when one
/// commit appears twice on the stack, the remembered position is the only
/// thing that says which of them the keyboard was on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StashId {
    /// Where it sat when it was chosen. A hint, re-checked before any write.
    pub index: usize,
    /// The stash commit, full object id — the identity.
    pub commit: String,
}

impl StashId {
    /// The entry the keyboard is on, as both its identities.
    pub fn of(stash: &Stash) -> Self {
        Self {
            index: stash.index,
            commit: stash.commit.clone(),
        }
    }

    /// Where this entry sits on `stack` *now* — the question every stash
    /// write asks immediately before it runs, against a freshly read stack.
    ///
    /// The rule, in order:
    ///
    /// - the remembered position still carries this commit: [`StashAt::Same`];
    /// - exactly one other position carries it: [`StashAt::Moved`], and the
    ///   write goes there, because the commit is what was chosen;
    /// - no position carries it: [`StashAt::Gone`] — dropped, popped clean,
    ///   or the stack was cleared, and there is nothing to act on;
    /// - two or more carry it and the remembered position is not one of
    ///   them: [`StashAt::Ambiguous`]. Two identical stash commits are
    ///   possible — same tree, same parent, same second — and which one was
    ///   meant is then genuinely unknowable, so nothing is guessed.
    pub fn resolve(&self, stack: &[Stash]) -> StashAt {
        let carrying: Vec<usize> = stack
            .iter()
            .filter(|entry| entry.commit == self.commit)
            .map(|entry| entry.index)
            .collect();
        if carrying.contains(&self.index) {
            return StashAt::Same(self.index);
        }
        match carrying.as_slice() {
            [] => StashAt::Gone,
            [only] => StashAt::Moved(*only),
            _ => StashAt::Ambiguous,
        }
    }
}

/// Where a [`StashId`] turned out to be, read against the live stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StashAt {
    /// Still at the position the keyboard left it.
    Same(usize),
    /// On the stack, at a new position: something pushed, popped or dropped
    /// underneath, and the commit is what followed the entry there. The
    /// write aims here — this *is* the entry that was chosen.
    Moved(usize),
    /// No entry on the stack carries this commit any more.
    Gone,
    /// More than one entry carries it, and none of them is where the
    /// keyboard left it.
    Ambiguous,
}

impl StashAt {
    /// The position to address, or the sentence that says why there is
    /// none. The words are here rather than in a client because every
    /// client's refusal is the same refusal.
    pub fn position(self) -> Result<usize, String> {
        match self {
            StashAt::Same(at) | StashAt::Moved(at) => Ok(at),
            StashAt::Gone => {
                Err("that stash is no longer on the stack — it was popped or dropped".into())
            }
            StashAt::Ambiguous => Err(
                "two stashes on the stack are the same commit and neither is where this \
                 one was — refresh and choose again"
                    .into(),
            ),
        }
    }
}

// -------------------------------------------------------------------- remotes

/// A remote this repository knows by name, with the URLs configured for it.
///
/// URLs are display text: every verb addresses the remote by [`Remote::name`]
/// and lets git resolve the address, so a URL is never aimed at anything and
/// decodes without risk. The same URL serving both directions appears once;
/// an explicit distinct push URL appears beside the fetch one, in config
/// order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remote {
    /// The short name verbs use, e.g. `origin`.
    pub name: RefName,
    /// Where it points, fetch URLs first.
    pub urls: Vec<String>,
}

// ----------------------------------------------------------------------- tags

/// A tag, resolved to the commit it names.
///
/// Annotated tags point at a tag *object* which points at a commit; this is
/// the commit either way, because that is what showing a tag in history
/// means. Whether the tag is annotated and the subject line it carries
/// are modelled beside the commit, because the tags panel shows both —
/// a tag without a message is lightweight, and deletion asks the same
/// question either way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tag {
    /// The tag name, relative to `refs/tags`.
    pub name: RefName,
    /// The commit it ultimately names, full object id.
    pub commit: String,
    /// Whether git stored a tag object (`-a`), rather than a bare ref.
    pub annotated: bool,
    /// The tag message's subject line, decoded lossily for display — `None`
    /// for lightweight tags, which carry no message at all.
    pub subject: Option<String>,
}

// --------------------------------------------------------------------- reflog

/// One entry of HEAD's reflog: where HEAD moved, newest first.
///
/// The reflog is the record of *where you have been* — commits, checkouts,
/// resets, rebases — and `selector` is how an entry is addressed back to git,
/// the way `index` addresses a [`Stash`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReflogEntry {
    /// The commit HEAD pointed at, abbreviated as git abbreviates it.
    pub commit: String,
    /// The address of this entry, e.g. `HEAD@{3}`.
    pub selector: String,
    /// What moved HEAD — `commit: …`, `checkout: …`, `rebase …`.
    pub message: String,
}

// ------------------------------------------------------------------ undo/redo

/// The reflog message our own undo writes through `update-ref -m`, so a
/// later redo can tell our walk-back from the reader's own terminal resets.
/// A reader's `git reset --soft` in another window reads `reset: moving
/// to …`; only this exact sentence arms the redo.
pub const UNDO_MESSAGE: &str = "gitten: undo";

/// The reflog message our own redo writes, for the same reason in reverse:
/// an undo offered after a redo must see the redo's sentence and stop,
/// rather than walking the same two entries forever.
pub const REDO_MESSAGE: &str = "gitten: redo";

/// The last HEAD move, and how to walk it back.
///
/// A checkout is walked back by checking out where it came from — a branch
/// name or a sha, whatever the message names. Every other move (commit,
/// amend, reset, merge, rebase finish, cherry-pick, revert) left HEAD on
/// the same ref it started on, so walking back is pointing that ref at the
/// earlier entry — `update-ref`, never a reset flag, so the index and the
/// working tree are exactly where the reader left them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UndoKind {
    /// Check out where the last move came from.
    Checkout { from: String },
    /// Point HEAD's ref at the earlier entry's selector, e.g. `HEAD@{1}`.
    Move { selector: String },
}

/// Classifies the step from `before` (`HEAD@{1}`) to `after` (`HEAD@{0}`),
/// or `None` when there is nothing to walk back: the two entries name the
/// same commit, so the move was a no-op in terms of position (a reset onto
/// itself) and walking back would append a reflog entry that says nothing.
/// Abbreviated shas compare as strings because git's abbreviations are
/// unique prefixes — equal text is the same object.
pub fn undo_for(before: &ReflogEntry, after: &ReflogEntry) -> Option<UndoKind> {
    const PREFIX: &str = "checkout: moving from ";
    if before.commit == after.commit {
        return None;
    }
    if let Some(rest) = after.message.strip_prefix(PREFIX) {
        // `moving from X to Y`: the target is everything before ` to `.
        // A branch name never contains it; a sha never does either.
        let from = rest.split(" to ").next().unwrap_or(rest);
        return Some(UndoKind::Checkout {
            from: from.to_string(),
        });
    }
    Some(UndoKind::Move {
        selector: before.selector.clone(),
    })
}

/// The selector a redo should walk forward to, or `None` when redo is not
/// armed: the newest entry is not our own undo, or HEAD has moved since —
/// a commit, a checkout, anything — so the forward step no longer names
/// where the undo came from. `head_sha` is HEAD's full sha; the entry's
/// abbreviated commit must prefix it, the same unique-prefix comparison
/// [`undo_for`] relies on.
pub fn redo_selector(entries: &[ReflogEntry], head_sha: &str) -> Option<String> {
    let [after, before] = entries else {
        return None;
    };
    if after.message != UNDO_MESSAGE {
        return None;
    }
    if !head_sha.starts_with(after.commit.as_str()) {
        return None;
    }
    Some(before.selector.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ref_name_keeps_its_bytes_like_a_path_does() {
        // Same discipline as status::PathBytes — asserted here so the alias
        // cannot quietly lose it.
        let raw = b"f\xe9ature"; // Latin-1 é: a legal byte in a ref name
        let name = RefName::from_bytes(raw);
        assert_eq!(name.as_bytes(), raw, "addressing keeps the bytes");
        assert!(name.to_string_lossy().contains('\u{FFFD}'), "display loses");
    }

    #[test]
    fn detached_head_is_a_value_and_not_a_failure() {
        let head = HeadState::Detached {
            commit: "abc123".into(),
        };
        assert_ne!(
            head,
            HeadState::Branch {
                name: RefName::from("main"),
                commit: None,
            }
        );
    }

    fn reflog(commit: &str, selector: &str, message: &str) -> ReflogEntry {
        ReflogEntry {
            commit: commit.into(),
            selector: selector.into(),
            message: message.into(),
        }
    }

    #[test]
    fn a_commit_is_walked_back_by_pointing_at_the_earlier_entry() {
        let before = reflog("aaa111", "HEAD@{1}", "commit: second");
        let after = reflog("bbb222", "HEAD@{0}", "commit: third");
        assert_eq!(
            undo_for(&before, &after),
            Some(UndoKind::Move {
                selector: "HEAD@{1}".into()
            })
        );
    }

    #[test]
    fn a_checkout_is_walked_back_by_checking_out_where_it_came_from() {
        let before = reflog("aaa111", "HEAD@{1}", "commit: on main");
        let after = reflog(
            "bbb222",
            "HEAD@{0}",
            "checkout: moving from main to feature",
        );
        assert_eq!(
            undo_for(&before, &after),
            Some(UndoKind::Checkout {
                from: "main".into()
            })
        );
    }

    #[test]
    fn a_checkout_from_a_detached_sha_walks_back_to_the_sha() {
        let before = reflog("aaa111", "HEAD@{1}", "commit: on main");
        let after = reflog(
            "bbb222",
            "HEAD@{0}",
            "checkout: moving from aaa111b to main",
        );
        assert_eq!(
            undo_for(&before, &after),
            Some(UndoKind::Checkout {
                from: "aaa111b".into()
            })
        );
    }

    #[test]
    fn a_move_onto_itself_has_nothing_to_walk_back() {
        let entry = reflog("aaa111", "HEAD@{1}", "reset: moving to aaa111");
        let same = reflog("aaa111", "HEAD@{0}", "reset: moving to aaa111");
        assert_eq!(undo_for(&entry, &same), None);
    }

    #[test]
    fn redo_arms_only_behind_our_own_undo_on_an_unmoved_head() {
        let undo = reflog("aaa111", "HEAD@{0}", "gitten: undo");
        let before = reflog("bbb222", "HEAD@{1}", "commit: third");
        assert_eq!(
            redo_selector(&[undo.clone(), before.clone()], "aaa1119999"),
            Some("HEAD@{1}".into())
        );
        // Somebody else's reset is not our undo.
        let foreign = reflog("aaa111", "HEAD@{0}", "reset: moving to aaa111");
        assert_eq!(
            redo_selector(&[foreign, before.clone()], "aaa1119999"),
            None
        );
        // HEAD moved on: the forward step no longer names where we came from.
        assert_eq!(
            redo_selector(&[undo.clone(), before.clone()], "ccc333"),
            None
        );
        // A redo's own sentence stops the walk instead of looping it.
        let redo = reflog("bbb222", "HEAD@{0}", "gitten: redo");
        let older = reflog("aaa111", "HEAD@{1}", "gitten: undo");
        assert_eq!(redo_selector(&[redo, older], "bbb2220000"), None);
    }

    #[test]
    fn redo_needs_two_entries() {
        let undo = reflog("aaa111", "HEAD@{0}", "gitten: undo");
        assert_eq!(redo_selector(&[undo], "aaa1119999"), None);
        assert_eq!(redo_selector(&[], "aaa1119999"), None);
    }

    fn stack(commits: &[&str]) -> Vec<Stash> {
        commits
            .iter()
            .enumerate()
            .map(|(index, commit)| Stash {
                index,
                message: format!("on main: {commit}"),
                commit: (*commit).into(),
            })
            .collect()
    }

    #[test]
    fn a_stash_scope_names_what_it_takes_and_spells_its_own_flags() {
        // The flags are git's; the point of the table is that a client
        // offering the menu never learns one of them.
        assert_eq!(StashScope::Tracked.flags(), &[] as &[&str]);
        assert_eq!(StashScope::WithUntracked.flags(), &["-u"]);
        assert_eq!(StashScope::Staged.flags(), &["--staged"]);
        assert_eq!(StashScope::Unstaged.flags(), &["--keep-index"]);
        // A tracked path needs no flag; an untracked one needs -u, or git
        // answers the pathspec with "did not match any file(s) known to
        // git" and stashes nothing at all.
        let tracked = StashScope::Path {
            path: "src/x.rs".into(),
            untracked: false,
        };
        let fresh = StashScope::Path {
            path: "notes.md".into(),
            untracked: true,
        };
        assert_eq!(tracked.flags(), &[] as &[&str]);
        assert_eq!(fresh.flags(), &["-u"]);
        assert_eq!(
            tracked.path().map(|p| p.as_bytes()),
            Some(b"src/x.rs".as_slice())
        );
        assert_eq!(StashScope::Tracked.path(), None);
        assert_eq!(StashScope::Staged.label(), "the staged side");
        assert_eq!(fresh.label(), "notes.md");
    }

    #[test]
    fn a_scoped_path_addresses_bytes_and_labels_lossily() {
        // Same discipline as everywhere: the pathspec git receives is exact,
        // the sentence a person reads is decoded.
        let raw = b"caf\xe9.txt";
        let scope = StashScope::Path {
            path: PathBytes::from_bytes(raw),
            untracked: false,
        };
        assert_eq!(scope.path().map(|p| p.as_bytes()), Some(raw.as_slice()));
        assert!(scope.label().contains('\u{FFFD}'), "the label decodes");
    }

    #[test]
    fn a_stash_identity_follows_its_commit_when_the_stack_renumbers() {
        // Chosen at stash@{1}. A push lands on top and every entry shifts
        // up one: the number now names somebody else's work, the commit
        // still names this.
        let chosen = StashId::of(&stack(&["c0", "c1", "c2"])[1]);
        assert_eq!(chosen.index, 1);
        assert_eq!(
            chosen.resolve(&stack(&["c0", "c1", "c2"])),
            StashAt::Same(1)
        );
        assert_eq!(
            chosen.resolve(&stack(&["new", "c0", "c1", "c2"])),
            StashAt::Moved(2),
            "a push under the selection moves the entry, not the choice"
        );
        assert_eq!(
            chosen.resolve(&stack(&["c1", "c2"])),
            StashAt::Moved(0),
            "the entry above it was dropped"
        );
        assert_eq!(StashAt::Moved(2).position(), Ok(2));
    }

    #[test]
    fn a_stash_identity_that_left_the_stack_refuses_instead_of_renumbering() {
        // The accident this whole type exists to prevent: stash@{1} chosen,
        // stash@{1} dropped elsewhere, and the number now names the entry
        // that used to be stash@{2}. Acting on it would apply the wrong
        // work; saying so costs a keypress.
        let chosen = StashId::of(&stack(&["c0", "c1", "c2"])[1]);
        assert_eq!(chosen.resolve(&stack(&["c0", "c2"])), StashAt::Gone);
        let said = StashAt::Gone.position().expect_err("gone is a refusal");
        assert!(said.contains("no longer on the stack"), "{said}");
    }

    #[test]
    fn two_identical_stash_commits_are_unknowable_rather_than_guessed() {
        // Same tree, same parent, same second: git writes the same commit
        // twice and the stack carries it twice. While the remembered
        // position is one of them the choice is still clear; once it is
        // not, which one was meant cannot be recovered.
        let chosen = StashId::of(&stack(&["c0", "twin", "twin"])[1]);
        assert_eq!(
            chosen.resolve(&stack(&["c0", "twin", "twin"])),
            StashAt::Same(1)
        );
        assert_eq!(
            chosen.resolve(&stack(&["twin", "twin"])),
            StashAt::Same(1),
            "still one of them: the remembered position wins"
        );
        let drifted = StashId {
            index: 5,
            commit: "twin".into(),
        };
        assert_eq!(
            drifted.resolve(&stack(&["twin", "twin"])),
            StashAt::Ambiguous
        );
        let said = StashAt::Ambiguous
            .position()
            .expect_err("ambiguous is a refusal");
        assert!(said.contains("choose again"), "{said}");
    }

    #[test]
    fn an_upstream_that_cannot_be_compared_says_so_instead_of_zero() {
        // gone ≠ in sync: ahead None must not read as "nothing to push".
        let gone = Upstream {
            remote: RefName::from("origin"),
            branch: RefName::from("main"),
            ahead: None,
            behind: None,
        };
        let synced = Upstream {
            ahead: Some(0),
            behind: Some(0),
            ..gone.clone()
        };
        assert_ne!(gone, synced);
        assert_eq!(gone.ahead, None);
    }
}
