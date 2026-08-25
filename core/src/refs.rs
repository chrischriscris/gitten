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
/// means. Whether the tag carried a message of its own is deliberately not
/// modelled — no panel has asked yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tag {
    /// The tag name, relative to `refs/tags`.
    pub name: RefName,
    /// The commit it ultimately names, full object id.
    pub commit: String,
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
