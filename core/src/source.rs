//! Which two trees a diff is between, and what may act on it.
//!
//! The aggregate read a revspec names cannot tell a client what it is looking
//! at: `HEAD`→worktree folds staged, unstaged and untracked into one list,
//! which is right for `gitten diff` and wrong for a panel that stages one
//! side of the index. This model is the explicit answer — a source names its
//! two trees, and a caller that must not act on history asks
//! [`DiffSource::working_tree`] instead of string-matching a revspec. An
//! empty argument is data about *which aggregate view* was asked for, never
//! the answer to "may I stage this"; eligibility is decided by what the
//! source is, in one place, and a caller that has to decide differently has
//! to say so in its own words.

use crate::status::PathBytes;

/// One diff's identity: its two trees, and what it may act on.
///
/// Pure data, like everything in this crate — git reads it, clients draw it,
/// and neither teaches the other about the pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffSource {
    /// `HEAD`'s tree → the index: the staged side of one path.
    Staged { path: PathBytes },
    /// The index → the working tree: the unstaged side of one path.
    Unstaged { path: PathBytes },
    /// The working tree vs nothing: a file known to no part of git, whose
    /// contents live on disk and nowhere else. `git diff` cannot see one
    /// and never will; the status pass is what sources it.
    Untracked { path: PathBytes },
    /// What one commit changed, addressed by its full object id.
    Commit { sha: String },
    /// One stash entry against its parent: the stash commit's full object
    /// id addresses it, its place on the stack names it on screen — the
    /// indices renumber under a drop and the commits do not.
    Stash { index: usize, commit: String },
    /// The aggregate read a revspec names. An empty argument is the
    /// combined `HEAD`→worktree view `gitten diff` opens on; a non-empty
    /// one is between commits. Kept for the command line's own behavior —
    /// a client wanting a *side* asks for a side by name.
    Revspec { arg: String },
    /// The fixtures' own diff: content with no repository behind it.
    Fixture,
    /// A patch file's diff: content with no repository behind it.
    Patch,
}

impl DiffSource {
    /// The one path the source is about, when it names one — the file-side
    /// sources do, and nothing else does.
    pub fn path(&self) -> Option<&PathBytes> {
        match self {
            DiffSource::Staged { path }
            | DiffSource::Unstaged { path }
            | DiffSource::Untracked { path } => Some(path),
            _ => None,
        }
    }

    /// The paths a staging verb is allowed to address: exactly the sources
    /// whose *new* side is the index or the working tree. A commit's diff,
    /// a stash's diff, an aggregate read and detached content all refuse —
    /// by what they are, not by what their text looks like.
    pub fn working_tree(&self) -> Option<&PathBytes> {
        match self {
            DiffSource::Staged { path }
            | DiffSource::Unstaged { path }
            | DiffSource::Untracked { path } => Some(path),
            _ => None,
        }
    }

    /// The short display form — what a title bar or a header calls it.
    /// The paths are display-decoded here and only here; everything that
    /// addressed git went through the raw bytes.
    pub fn label(&self) -> String {
        match self {
            DiffSource::Staged { path } => format!("{} · staged", path),
            DiffSource::Unstaged { path } => format!("{} · unstaged", path),
            DiffSource::Untracked { path } => format!("{} · untracked", path),
            DiffSource::Commit { sha } => sha[..sha.len().min(8)].to_string(),
            DiffSource::Stash { index, .. } => format!("stash@{{{index}}}"),
            DiffSource::Revspec { arg } if arg.is_empty() => "(working tree)".into(),
            DiffSource::Revspec { arg } => arg.clone(),
            DiffSource::Fixture => "fixtures".into(),
            DiffSource::Patch => "patch".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eligibility_is_what_the_source_is_and_not_what_its_text_looks_like() {
        // The two file-side sources carry their path and their eligibility
        // however they are spelled; history and detached content carry none,
        // however empty their argument is. An empty revspec string is which
        // aggregate view was asked for, and nothing else.
        let file = DiffSource::Staged {
            path: "f.txt".into(),
        };
        assert_eq!(
            file.working_tree().map(|p| p.as_bytes()),
            Some(b"f.txt".as_slice())
        );
        let empty = DiffSource::Revspec { arg: String::new() };
        assert!(
            empty.working_tree().is_none(),
            "an empty revspec is not a write source"
        );
        for source in [
            DiffSource::Commit { sha: "abc".into() },
            DiffSource::Stash {
                index: 0,
                commit: "def".into(),
            },
            DiffSource::Fixture,
            DiffSource::Patch,
        ] {
            assert!(
                source.working_tree().is_none(),
                "{source:?} is not writable"
            );
        }
    }

    #[test]
    fn paths_are_display_forms_and_labels_are_honest() {
        // A path that is not UTF-8 labels lossily and addresses exactly.
        let raw = b"caf\xe9.txt";
        let source = DiffSource::Untracked {
            path: PathBytes::from_bytes(raw),
        };
        assert_eq!(source.path().map(|p| p.as_bytes()), Some(raw.as_slice()));
        assert!(
            source.label().contains('\u{FFFD}'),
            "the display form decodes lossily: {}",
            source.label()
        );
        assert_eq!(
            DiffSource::Stash {
                index: 2,
                commit: "x".into()
            }
            .label(),
            "stash@{2}"
        );
        let commit = DiffSource::Commit {
            sha: "1234567890abcdef".into(),
        };
        assert_eq!(commit.label(), "12345678");
        assert_eq!(
            DiffSource::Revspec { arg: String::new() }.label(),
            "(working tree)"
        );
    }
}
