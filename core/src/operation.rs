//! An operation git left standing, and the choices a conflicted path offers.
//!
//! Merge, rebase, cherry-pick and revert all stop mid-flight the same way: a
//! nonzero exit, unmerged paths in the index, and state on disk that a *later*
//! process — maybe one that started before this client did — is expected to
//! pick up. What a client needs is the fact of it, in the repository's own
//! words, and nothing more: which of the four writes is standing, and how many
//! paths are still asking. Everything else is git's own state, read through
//! the acquisition layer and never modelled twice.
//!
//! Two deliberate narrownesses. One kind stands at a time — one index, one
//! sequencer, and git refuses a second start rather than nesting them — so
//! this is not a stack. And a conflict is not evidence of a rebase: a merge
//! stops on conflicts exactly as hard, which is why the kind travels beside
//! the count instead of the count standing alone.

/// Which write git stopped inside.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `git merge` stopped — `MERGE_HEAD` on disk.
    Merge,
    /// `git rebase` stopped — `rebase-merge`/`rebase-apply` on disk.
    Rebase,
    /// `git cherry-pick` stopped — `CHERRY_PICK_HEAD` or the sequencer.
    CherryPick,
    /// `git revert` stopped — `REVERT_HEAD` on disk.
    Revert,
}

impl Kind {
    /// The word a status line uses. git's own hyphenation.
    pub fn word(self) -> &'static str {
        match self {
            Kind::Merge => "merge",
            Kind::Rebase => "rebase",
            Kind::CherryPick => "cherry-pick",
            Kind::Revert => "revert",
        }
    }
}

/// Which side of a conflicted path a resolution takes.
///
/// [`Side::Ours`] and [`Side::Theirs`] name the two
/// stages a merge keeps, whatever the operation kind; [`Side::Both`] is the
/// two-stage concatenation (ours, then theirs) and exists only where both
/// stages have content; [`Side::Keep`] records the working tree as it stands,
/// which is the whole of "I edited this by hand".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// Stage 2 — what this side had.
    Ours,
    /// Stage 3 — what the other side had.
    Theirs,
    /// Stage 2 followed by stage 3, one boundary between them.
    Both,
    /// The working tree, recorded as the answer.
    Keep,
}

/// An operation standing mid-flight, as the repository reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Operation {
    /// Which write is standing.
    pub kind: Kind,
    /// Paths git still lists as unmerged — the count a banner names and a
    /// continue is gated on. Zero with an operation standing is a real state
    /// (a rebase between commits carries none), not an error.
    pub conflicts: usize,
}

impl Operation {
    /// Whether skip answers this kind. git can step over a stopped commit
    /// during a rebase; a merge's conflict is the merge itself, and the
    /// sequencer kinds carry no "pretend it did not happen" either.
    pub fn can_skip(self) -> bool {
        self.kind == Kind::Rebase
    }
}
