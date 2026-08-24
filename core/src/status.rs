//! What the working tree is doing, as git reports it.
//!
//! These are the shapes `git status --porcelain=v2` fills and nothing else.
//! Porcelain v1 folds every change into one pair of letters per path, which is
//! enough to draw a list and useless to act on: staging is a verb about *some*
//! of those changes and not others, and a panel cannot stage what it cannot
//! tell apart. So the model separates what the index says changed against HEAD
//! ([`Status::staged`]) from what the working tree says changed against the
//! index ([`Status::unstaged`]), and gives untracked ([`Status::untracked`])
//! and merge-conflicted ([`Status::conflicts`]) lists of their own.
//!
//! Pure data, like everything in this crate. Parsing lives in `gitten-git`,
//! drawing lives in a client, and neither gets to teach these types about the
//! other.

use std::borrow::Cow;
use std::fmt;

// ------------------------------------------------------------------- pathname

/// A pathname exactly as git emitted it: raw bytes, never decoded.
///
/// Git attaches no encoding to a path and real repositories carry ones that
/// are not valid UTF-8. Decoding lossily at the boundary quietly renames such
/// a file on screen, and a verb aimed at the mangled name then hits nothing —
/// so the bytes travel untouched to whoever can use them, and every consumer
/// that wants text asks for it with [`PathBytes::to_string_lossy`], at the
/// layer that knows *displaying* is the job rather than *addressing*.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct PathBytes(Box<[u8]>);

impl PathBytes {
    /// The path git named, byte for byte.
    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self(bytes.into())
    }

    /// The raw bytes. This is the addressing form: joining it onto a
    /// repository root, handing it back to git, hashing it.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// The display form, lossily decoded. A bad byte becomes U+FFFD and
    /// nothing fails — never fail to show a repository over one byte.
    pub fn to_string_lossy(&self) -> Cow<'_, str> {
        String::from_utf8_lossy(&self.0)
    }
}

impl From<&str> for PathBytes {
    fn from(s: &str) -> Self {
        Self(s.as_bytes().into())
    }
}

impl fmt::Display for PathBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_string_lossy())
    }
}

/// Quoted, like a string literal — a path in a test failure should read as a
/// path, not as a list of numbers.
impl fmt::Debug for PathBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.to_string_lossy())
    }
}

// ---------------------------------------------------------------------- kinds

/// What a path *is*, as far as reading it or staging it cares.
///
/// From the mode git prints per column, not from a stat call: a symlink is a
/// blob of target text and a submodule is a borrowed commit, and both would
/// otherwise arrive as contents nobody asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// An ordinary blob — executable or not, which no panel has needed yet.
    File,
    /// A symlink; its content is the path it points at.
    Symlink,
    /// A submodule: the OID is a commit in another repository, and there is
    /// nothing here to read.
    Submodule,
}

impl Kind {
    /// From git's six-digit octal mode, as printed by `--porcelain=v2`.
    /// Anything unrecognised reads as a plain file — a malformed mode is a
    /// reason to guess conservatively, not to drop the entry.
    pub fn from_git_mode(mode: &str) -> Self {
        match mode {
            "120000" => Kind::Symlink,
            "160000" => Kind::Submodule,
            _ => Kind::File,
        }
    }
}

/// What happened to a path, on one side of the index/worktree split.
///
/// The letter git prints, spelled out — `A` `M` `D` `R` `C` `T`. A `char`
/// would leak the wire format into every consumer; this is the whole reason
/// the model exists in `core` rather than as a parser detail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// New here; there is no earlier version to compare against.
    Added,
    /// Present before, different now.
    Modified,
    /// Was here; gone now.
    Deleted,
    /// Moved, and recognised as the same content.
    Renamed,
    /// Copied — the same shape as a rename, without giving the origin up.
    Copied,
    /// Same path, different species: a file that became a symlink or the
    /// reverse. Diffing one as text is possible and usually not wanted.
    TypeChanged,
}

/// How a merge left both sides, for a conflicted path.
///
/// The seven states git names with the XY pair of a `u` record. Who added and
/// who deleted decides what resolving means — you cannot stage a deletion of a
/// file your side never had — so this is data, not decoration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictKind {
    /// Deleted by both sides; nothing left to resolve but the fact.
    BothDeleted,
    /// Our side added it, theirs never saw it.
    AddedByUs,
    /// They deleted what we modified.
    DeletedByThem,
    /// Their side added it, ours never saw it.
    AddedByThem,
    /// We deleted what they modified.
    DeletedByUs,
    /// Both sides added it, differently.
    BothAdded,
    /// Both sides modified it, differently — the classic conflict.
    BothModified,
}

/// How a borrowed submodule sits in the **working tree**, as git's `S<C><M><U>`
/// state field spells out.
///
/// Its checked-out commit is not the commit the parent's index records
/// ([`Self::commit_changed`]), tracked files inside it were edited
/// ([`Self::modified`]), or files inside it are not tracked by it
/// ([`Self::untracked`]). All false is a clean borrow — which appears in no
/// list at all.
///
/// All three facts compare the submodule against the parent's *index* or
/// against the submodule itself; not one of them says anything about the
/// index against `HEAD`, which is the staged side's whole subject. So they
/// ride on the entries that describe the working tree — [`UnstagedEntry`] and
/// [`ConflictEntry`] — and a [`StagedEntry`] carries [`Self::default`]:
/// copying worktree state onto a staged row would answer a worktree question
/// under a staged heading.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Submodule {
    /// Its checked-out commit differs from the commit the parent's index
    /// records.
    pub commit_changed: bool,
    /// Tracked files inside it have local modifications.
    pub modified: bool,
    /// Files inside it that it does not track.
    pub untracked: bool,
}

// -------------------------------------------------------------------- entries

/// A path the **index** says changed against `HEAD`.
///
/// This is what `git add` has done something about and a commit would take.
/// A rename is an index-level fact — git matched the added path to a deleted
/// one — so the old name travels with the entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedEntry {
    /// The path as it is now.
    pub path: PathBytes,
    /// What the index says happened.
    pub change: Change,
    /// The name it had before, for a [`Change::Renamed`] or
    /// [`Change::Copied`].
    pub old_path: Option<PathBytes>,
    /// What it is, from the mode recorded in the index.
    pub kind: Kind,
    /// Present because the entry shapes share a body, and always
    /// [`Submodule::default`] here: the submodule state field describes the
    /// working tree, and a staged entry describes the index. See
    /// [`Submodule`].
    pub submodule: Submodule,
}

/// A path the **working tree** says changed against the index.
///
/// The complement of [`StagedEntry`]: what a `git add` would pick up right
/// now. One path can appear here *and* there — edited, staged, edited again —
/// which is precisely the case one combined list cannot represent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnstagedEntry {
    pub path: PathBytes,
    /// What the working tree says happened. Only modifications, deletions and
    /// type changes can originate here; an addition the index never heard of
    /// is untracked, not unstaged.
    pub change: Change,
    /// What it is on disk, from the worktree mode. A side that does not exist
    /// prints `000000`, which is where a deletion's kind comes from the side
    /// that did — see the parser.
    pub kind: Kind,
    /// The submodule state field, which describes exactly this: the working
    /// tree against the index.
    pub submodule: Submodule,
}

/// A path git tracks nowhere: in no commit, in no index entry.
///
/// Only the name — an untracked file has no mode git trusts until it is added,
/// and asking for more would mean statting, which is I/O and not this crate's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UntrackedEntry {
    pub path: PathBytes,
}

/// A path a merge left for a human.
///
/// Its stages live in the index simultaneously, which is what no other entry
/// shape can say; [`ConflictEntry::state`] says who disagrees about what.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictEntry {
    pub path: PathBytes,
    /// Which sides disagree, and how.
    pub state: ConflictKind,
    /// What it is in the working tree right now — both-added conflicts have
    /// content, both-deleted ones have not.
    pub kind: Kind,
    /// The submodule state field, which describes exactly this: the working
    /// tree against the index. See [`Submodule`].
    pub submodule: Submodule,
}

// --------------------------------------------------------------------- status

/// The whole working tree, sorted into the four questions a status panel asks.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    /// Changed against `HEAD`, as recorded in the index.
    pub staged: Vec<StagedEntry>,
    /// Changed against the index, as found in the working tree.
    pub unstaged: Vec<UnstagedEntry>,
    /// Known to no part of git.
    pub untracked: Vec<UntrackedEntry>,
    /// Left unresolved by a merge.
    pub conflicts: Vec<ConflictEntry>,
    /// Matched `.gitignore` — collected when git is asked for them, which
    /// gitten deliberately does not by default: `target/` alone would be forty
    /// thousand entries nobody reads.
    pub ignored: Vec<PathBytes>,
}

impl Status {
    /// Nothing to show, in any list.
    pub fn is_empty(&self) -> bool {
        self.staged.is_empty()
            && self.unstaged.is_empty()
            && self.untracked.is_empty()
            && self.conflicts.is_empty()
            && self.ignored.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_come_from_gits_modes() {
        // The two special modes are the whole point: a symlink's "content" is
        // a target path and a submodule's is a foreign commit, and treating
        // either as a plain file reads nonsense onto the screen.
        assert_eq!(Kind::from_git_mode("100644"), Kind::File);
        assert_eq!(Kind::from_git_mode("100755"), Kind::File);
        assert_eq!(Kind::from_git_mode("120000"), Kind::Symlink);
        assert_eq!(Kind::from_git_mode("160000"), Kind::Submodule);
        // Malformed guesses low: a file is the least wrong answer.
        assert_eq!(Kind::from_git_mode(""), Kind::File);
        assert_eq!(Kind::from_git_mode("junk"), Kind::File);
    }

    #[test]
    fn raw_bytes_survive_whatever_they_contain() {
        // `café.txt` in Latin-1 — not valid UTF-8, and exactly what a
        // lossy boundary would have mangled beyond recognition.
        let raw = b"caf\xe9.txt";
        let path = PathBytes::from_bytes(raw);
        assert_eq!(path.as_bytes(), raw, "addressing keeps the bytes");
        assert!(
            path.to_string_lossy().contains('\u{FFFD}'),
            "display decodes lossily instead of failing"
        );
        assert_eq!(path.to_string(), String::from_utf8_lossy(raw));
        assert_eq!(format!("{path:?}"), "\"caf\u{FFFD}.txt\"");
        assert_eq!(PathBytes::from("plain.txt").as_bytes(), b"plain.txt");
    }

    #[test]
    fn a_rename_remembers_the_name_it_had() {
        let e = StagedEntry {
            path: PathBytes::from("after.rs"),
            change: Change::Renamed,
            old_path: Some(PathBytes::from("before.rs")),
            kind: Kind::File,
            submodule: Submodule::default(),
        };
        assert_eq!(
            e.old_path.as_ref().map(|p| p.as_bytes()),
            Some(b"before.rs".as_slice())
        );
    }

    #[test]
    fn the_lists_answer_the_four_questions_separately() {
        // The property the roadmap asked for: one file edited, staged and
        // edited again sits in BOTH of the first two lists, and nothing else
        // blurs into either.
        let s = Status {
            staged: vec![StagedEntry {
                path: PathBytes::from("a.rs"),
                change: Change::Modified,
                old_path: None,
                kind: Kind::File,
                submodule: Submodule::default(),
            }],
            unstaged: vec![UnstagedEntry {
                path: PathBytes::from("a.rs"),
                change: Change::Modified,
                kind: Kind::File,
                submodule: Submodule::default(),
            }],
            untracked: vec![UntrackedEntry {
                path: PathBytes::from("b.rs"),
            }],
            conflicts: vec![ConflictEntry {
                path: PathBytes::from("c.rs"),
                state: ConflictKind::BothModified,
                kind: Kind::File,
                submodule: Submodule::default(),
            }],
            ignored: vec![],
        };
        assert_eq!(s.staged.len(), 1);
        assert_eq!(s.unstaged.len(), 1);
        assert_eq!(s.untracked.len(), 1);
        assert_eq!(s.conflicts.len(), 1);
        assert!(!s.is_empty());
        assert!(Status::default().is_empty(), "a clean tree is empty");
    }
}
