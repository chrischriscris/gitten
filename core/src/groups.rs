//! Directory grouping for the file list, and the staged-hunk fraction.
//!
//! The guide-v2 workspace groups changed files by directory — one label per
//! directory, one compact row per file — rather than by index side. The current
//! shell flattens [`Status`](crate::status::Status) by section
//! (staged/unstaged/untracked/conflicts); this module is the shared seam both
//! presentations read, so a second client or an extension groups the same way
//! without re-cutting paths in a renderer.
//!
//! Pure data, like everything in this crate: no I/O, no acquisition, no window.
//! The hunk counts a fraction is computed *from* arrive from outside — the
//! app layer joins its already-acquired per-side reads — and only the
//! derivation of empty/partial/full from two numbers lives here.

use crate::status::{Change, ConflictKind, Kind, PathBytes, Status};

/// Which of the four status lists a grouped row came from.
///
/// Kept on every row because the directory view drops the section headings but
/// must not drop what they said: whether a click stages or unstages, and which
/// ink the trailing status carries, are both a function of the side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Staged,
    Unstaged,
    Untracked,
    Conflict,
}

/// One file row inside a directory group.
///
/// The `(section, path)` pair is the identity — the same path can sit in
/// `staged` *and* `unstaged` at once (edited, staged, edited again), and those
/// are two rows with different verbs, never one row counted twice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupedEntry {
    pub section: Section,
    pub path: PathBytes,
    /// What happened on this side. `None` for conflicted rows, which carry
    /// [`GroupedEntry::conflict`] instead — inventing a [`Change`] for a merge
    /// state would answer a question nobody asked.
    pub change: Option<Change>,
    /// The name it had before, for a renamed or copied staged row.
    pub old_path: Option<PathBytes>,
    /// What it is. Untracked rows read as [`Kind::File`]: an untracked path
    /// has no mode git trusts until it is added, and guessing conservatively
    /// beats dropping the entry.
    pub kind: Kind,
    /// Which sides disagree, on conflicted rows only.
    pub conflict: Option<ConflictKind>,
}

/// One directory label and the rows beneath it.
///
/// `dir` is the byte prefix including the trailing `/` — empty for top-level
/// files — so heading plus filename concatenate back to the path. Raw bytes,
/// like every path here: display via [`PathBytes::to_string_lossy`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirGroup {
    pub dir: PathBytes,
    pub entries: Vec<GroupedEntry>,
}

/// Cuts raw path bytes the way [`crate::path::split_dir_name`] cuts the
/// string: after the last `/`, directory half carrying the slash, bare names
/// getting an empty directory, a trailing slash emptying the name.
///
/// Byte-level so non-UTF-8 paths group without decoding — the two halves
/// concatenate back to the input either way.
pub fn split_dir_name_bytes(path: &[u8]) -> (&[u8], &[u8]) {
    match path.iter().rposition(|b| *b == b'/') {
        Some(at) => path.split_at(at + 1),
        None => (&[], path),
    }
}

/// Groups a whole [`Status`] by directory.
///
/// Walk order is the status order — staged, unstaged, untracked, conflicts —
/// and groups appear in first-seen order within that walk. Entries inside a
/// group keep walk order too: no sorting here, because a client that wants its
/// rows alphabetical can sort without every other client inheriting the bill.
///
/// Grouping is flatten-once work: call this per refresh beside the section
/// flatten, never on the render path.
pub fn group_by_dir(status: &Status) -> Vec<DirGroup> {
    let mut groups: Vec<DirGroup> = Vec::new();
    let mut push = |section: Section,
                    path: PathBytes,
                    change: Option<Change>,
                    old_path: Option<PathBytes>,
                    kind: Kind,
                    conflict: Option<ConflictKind>| {
        let (dir, _) = split_dir_name_bytes(path.as_bytes());
        let group = match groups.iter_mut().find(|g| g.dir.as_bytes() == dir) {
            Some(g) => g,
            None => {
                groups.push(DirGroup {
                    dir: PathBytes::from_bytes(dir),
                    entries: Vec::new(),
                });
                groups.last_mut().expect("just pushed")
            }
        };
        group.entries.push(GroupedEntry {
            section,
            path,
            change,
            old_path,
            kind,
            conflict,
        });
    };
    for e in &status.staged {
        push(
            Section::Staged,
            e.path.clone(),
            Some(e.change),
            e.old_path.clone(),
            e.kind,
            None,
        );
    }
    for e in &status.unstaged {
        push(
            Section::Unstaged,
            e.path.clone(),
            Some(e.change),
            None,
            e.kind,
            None,
        );
    }
    for e in &status.untracked {
        push(
            Section::Untracked,
            e.path.clone(),
            Some(Change::Added),
            None,
            Kind::File,
            None,
        );
    }
    for e in &status.conflicts {
        push(
            Section::Conflict,
            e.path.clone(),
            None,
            None,
            e.kind,
            Some(e.state),
        );
    }
    groups
}

/// How much of a file's hunks sit in the index, derived from two counts.
///
/// The counts come from outside — staged-side hunks over staged-plus-unstaged
/// hunks, joined by the app layer over its per-side reads. This enum is the one
/// place the checkbox states are decided, so the sidebar and the inspector
/// cannot disagree about what "partial" means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageFraction {
    /// Nothing to stage and nothing staged: `total == 0`, or `staged == 0`
    /// with nothing to compare against. Checkbox empty, no fraction shown.
    Empty,
    /// `staged == 0 < total`. Checkbox empty; the row stages on click.
    Unstaged { total: u32 },
    /// `0 < staged < total`. Mixed checkbox plus a `staged/total` fraction;
    /// a click stages the remainder.
    Partial { staged: u32, total: u32 },
    /// `0 < staged == total`. Checked; a click unstages the file.
    Full { total: u32 },
}

/// Derives the checkbox state from `(staged_hunks, total_hunks)`.
///
/// A `staged` count past `total` cannot happen from honest reads; it reads as
/// [`StageFraction::Full`] rather than failing, because a sidebar must never
/// fail to show a repository over one surprising number.
pub fn stage_fraction(staged_hunks: u32, total_hunks: u32) -> StageFraction {
    if total_hunks == 0 || staged_hunks == 0 {
        if total_hunks == 0 {
            return StageFraction::Empty;
        }
        return StageFraction::Unstaged { total: total_hunks };
    }
    if staged_hunks < total_hunks {
        return StageFraction::Partial {
            staged: staged_hunks,
            total: total_hunks,
        };
    }
    StageFraction::Full { total: total_hunks }
}

impl StageFraction {
    /// How many hunks the commit would take. The inspector's Commit gate is
    /// this summed over files being greater than zero — staged *content*, not
    /// just a non-empty staged file list.
    pub fn staged_count(self) -> u32 {
        match self {
            StageFraction::Empty | StageFraction::Unstaged { .. } => 0,
            StageFraction::Partial { staged, .. } => staged,
            StageFraction::Full { total } => total,
        }
    }

    pub fn is_partial(self) -> bool {
        matches!(self, StageFraction::Partial { .. })
    }

    pub fn is_full(self) -> bool {
        matches!(self, StageFraction::Full { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::{Submodule, UnstagedEntry, UntrackedEntry};

    fn staged(path: &str, change: Change) -> crate::status::StagedEntry {
        crate::status::StagedEntry {
            path: PathBytes::from(path),
            change,
            old_path: None,
            kind: Kind::File,
            submodule: Submodule::default(),
        }
    }

    fn unstaged(path: &str, change: Change) -> UnstagedEntry {
        UnstagedEntry {
            path: PathBytes::from(path),
            change,
            kind: Kind::File,
            submodule: Submodule::default(),
        }
    }

    fn status_of(staged: Vec<crate::status::StagedEntry>) -> Status {
        Status {
            staged,
            ..Status::default()
        }
    }

    #[test]
    fn byte_cut_agrees_with_the_string_cut_on_valid_utf8() {
        // The contract with shell FileEntry: one cut, in one place. Every
        // valid-UTF-8 byte path must cut exactly where split_dir_name cuts it.
        for p in [
            "internal/ai/commit.go",
            "README",
            "docs/",
            "/",
            "a/b/c/d.rs",
            "trailing space /name.rs",
        ] {
            let (dir, name) = crate::path::split_dir_name(p);
            let (bdir, bname) = split_dir_name_bytes(p.as_bytes());
            assert_eq!(bdir, dir.as_bytes(), "dir half of {p}");
            assert_eq!(bname, name.as_bytes(), "name half of {p}");
        }
    }

    #[test]
    fn files_group_under_one_label_per_directory() {
        let s = status_of(vec![
            staged("shell/src/diff/view.rs", Change::Modified),
            staged("shell/src/diff/scroll.rs", Change::Modified),
            staged("core/src/groups.rs", Change::Added),
            staged("README", Change::Modified),
        ]);
        let groups = group_by_dir(&s);
        let dirs: Vec<String> = groups
            .iter()
            .map(|g| g.dir.to_string_lossy().into_owned())
            .collect();
        assert_eq!(dirs, vec!["shell/src/diff/", "core/src/", ""]);
        assert_eq!(groups[0].entries.len(), 2);
        // Heading plus filename concatenate back to the path.
        for g in &groups {
            for e in &g.entries {
                let dir = g.dir.to_string_lossy();
                let path = e.path.to_string_lossy();
                let (_, name) = crate::path::split_dir_name(&path);
                assert_eq!(format!("{dir}{name}"), path);
            }
        }
    }

    #[test]
    fn twin_paths_stay_two_rows_with_their_own_sides() {
        // Edited, staged, edited again: the same path in staged AND unstaged.
        // Grouping must not merge them — one stages the remainder, the other
        // unstages the whole.
        let s = Status {
            staged: vec![staged("src/main.rs", Change::Modified)],
            unstaged: vec![unstaged("src/main.rs", Change::Modified)],
            ..Status::default()
        };
        let groups = group_by_dir(&s);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].entries.len(), 2);
        assert_eq!(groups[0].entries[0].section, Section::Staged);
        assert_eq!(groups[0].entries[1].section, Section::Unstaged);
    }

    #[test]
    fn non_utf8_paths_group_by_bytes_not_by_replacement_char() {
        // Two distinct byte paths that decode to the same lossy text must
        // still group — and stay distinct rows — by their bytes.
        let a = PathBytes::from_bytes(b"src/\xffile.rs");
        let b = PathBytes::from_bytes(b"src/\xfeile.rs");
        assert_ne!(a, b);
        let s = Status {
            untracked: vec![UntrackedEntry { path: a }, UntrackedEntry { path: b }],
            ..Status::default()
        };
        let groups = group_by_dir(&s);
        assert_eq!(groups.len(), 1, "same byte dir groups once");
        assert_eq!(groups[0].dir.as_bytes(), b"src/");
        assert_eq!(groups[0].entries.len(), 2, "distinct bytes stay distinct");
        assert!(
            groups[0]
                .entries
                .iter()
                .all(|e| e.change == Some(Change::Added)),
            "untracked rows read as added"
        );
    }

    #[test]
    fn renames_carry_the_old_name_and_conflicts_carry_state() {
        let mut renamed = staged("new/name.rs", Change::Renamed);
        renamed.old_path = Some(PathBytes::from("old/name.rs"));
        let s = Status {
            staged: vec![renamed],
            conflicts: vec![crate::status::ConflictEntry {
                path: PathBytes::from("new/name.rs"),
                state: ConflictKind::BothModified,
                kind: Kind::File,
                submodule: Submodule::default(),
            }],
            ..Status::default()
        };
        let groups = group_by_dir(&s);
        assert_eq!(groups.len(), 1);
        let row = &groups[0].entries[0];
        assert_eq!(
            row.old_path
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned()),
            Some("old/name.rs".to_string())
        );
        let crow = &groups[0].entries[1];
        assert_eq!(crow.section, Section::Conflict);
        assert_eq!(crow.change, None, "no invented Change for a merge state");
        assert_eq!(crow.conflict, Some(ConflictKind::BothModified));
    }

    #[test]
    fn fraction_states_come_from_two_numbers() {
        assert_eq!(stage_fraction(0, 0), StageFraction::Empty);
        assert_eq!(stage_fraction(0, 3), StageFraction::Unstaged { total: 3 });
        assert_eq!(
            stage_fraction(1, 3),
            StageFraction::Partial {
                staged: 1,
                total: 3
            }
        );
        assert_eq!(stage_fraction(3, 3), StageFraction::Full { total: 3 });
        // A surprising count degrades to Full, never to a missing row.
        assert_eq!(stage_fraction(5, 3), StageFraction::Full { total: 3 });
    }

    #[test]
    fn staged_count_is_what_the_commit_gate_sums() {
        assert_eq!(stage_fraction(0, 0).staged_count(), 0);
        assert_eq!(stage_fraction(0, 3).staged_count(), 0);
        assert_eq!(stage_fraction(2, 3).staged_count(), 2);
        assert_eq!(stage_fraction(3, 3).staged_count(), 3);
        assert!(stage_fraction(2, 3).is_partial());
        assert!(stage_fraction(3, 3).is_full());
        assert!(!stage_fraction(0, 3).is_partial());
    }
}
