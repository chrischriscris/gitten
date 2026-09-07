//! The patch clipboard: hunks kept for a later apply, as content.
//!
//! The cherry-pick clipboard next door keeps commit ids; this one keeps the
//! text itself, because a patch outlives the diff it was picked from and
//! applies where no commit exists yet — another branch's worktree, the
//! index, a historical commit's own content. Every entry names the read it
//! was picked from, so the apply re-checks the read still holds before one
//! byte moves; a commit or stash read is immutable and so always fresh,
//! while a working-tree read carries the blob OIDs the write revalidates.
//!
//! This is the clipboard and not the text selection: the text selection
//! copies what the eye is on, this one stages what the hands will apply.
//! Separate model, separate keys, separate status — a paste that drew from
//! the wrong one would apply lines nobody picked.

use crate::{patch, Hunk};

/// The read a picked file came from — what the apply re-checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Anchor {
    /// `HEAD`'s tree → the index. Revalidated against the index OID.
    Staged,
    /// The index → the working tree. Revalidated against both OIDs: the
    /// patch's preimage is the index side, its postimage the worktree.
    Unstaged,
    /// A file git knows nothing about. There is no OID to re-check, so the
    /// apply trusts nothing and lets `git apply` aim by context alone.
    Untracked,
    /// What one commit changed, by its full object id. Immutable: a commit
    /// never changes under a patch picked from it.
    Commit { sha: String },
    /// One stash entry against its parent, by the stash commit. Immutable
    /// like a commit — the index that renumbers the stack cannot move it.
    Stash { commit: String },
}

impl Anchor {
    /// Whether the read can change under a picked entry. Only the working
    /// tree moves; everything addressed by an object id stands still.
    pub fn is_worktree(&self) -> bool {
        matches!(self, Anchor::Staged | Anchor::Unstaged | Anchor::Untracked)
    }

    /// Where a picked entry says it came from, in the status line's words.
    pub fn word(&self) -> String {
        match self {
            Anchor::Staged => "staged".into(),
            Anchor::Unstaged => "unstaged".into(),
            Anchor::Untracked => "untracked".into(),
            Anchor::Commit { sha } => format!("commit {}", short(sha)),
            Anchor::Stash { .. } => "stash".into(),
        }
    }
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(8)]
}

/// One hunk on the clipboard, and whether the builder still includes it.
/// Exclusion is a toggle, not a removal: the hunk stays picked so a second
/// press brings it back, and what applies is exactly what the builder
/// shows included.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickedHunk {
    pub hunk: Hunk,
    pub included: bool,
}

/// One file on the clipboard: its picked hunks and the read they came
/// from. The OIDs are the working-tree anchors' alone — `None` for a
/// commit or stash entry, and for an untracked read that has no OID to
/// carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickedFile {
    pub path: String,
    pub anchor: Anchor,
    pub index_oid: Option<String>,
    pub head_oid: Option<String>,
    pub hunks: Vec<PickedHunk>,
}

/// Hunches kept for a later apply, grouped by path in pick order.
///
/// A second pick of the same path folds into the standing entry — one file
/// reads once in the builder — and a hunk already held stays where it was
/// put: re-picking never reorders, because a patch that moves under a
/// second press is an apply order nobody can predict. The same rule the
/// cherry-pick clipboard keeps, for the same reason.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PatchClipboard {
    files: Vec<PickedFile>,
}

impl PatchClipboard {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Files held, in pick order — what the builder draws.
    pub fn files(&self) -> &[PickedFile] {
        &self.files
    }

    /// Included hunks across every file — what an apply would carry.
    pub fn included_hunks(&self) -> usize {
        self.files
            .iter()
            .flat_map(|f| &f.hunks)
            .filter(|h| h.included)
            .count()
    }

    /// Picks `hunks` of `path` from `anchor`, folding into the standing
    /// entry when the path is already held. Every picked hunk starts
    /// included; a hunk already held is left alone. Returns how many hunks
    /// were new.
    pub fn pick(
        &mut self,
        path: String,
        anchor: Anchor,
        index_oid: Option<String>,
        head_oid: Option<String>,
        hunks: Vec<Hunk>,
    ) -> usize {
        let entry = match self.files.iter_mut().find(|f| f.path == path) {
            Some(entry) => entry,
            None => {
                self.files.push(PickedFile {
                    path,
                    anchor,
                    index_oid,
                    head_oid,
                    hunks: Vec::new(),
                });
                self.files.last_mut().expect("just pushed")
            }
        };
        let mut added = 0;
        for hunk in hunks {
            if !entry.hunks.iter().any(|h| h.hunk == hunk) {
                entry.hunks.push(PickedHunk {
                    hunk,
                    included: true,
                });
                added += 1;
            }
        }
        added
    }

    /// Flips every hunk of `path` at once. Returns the new state — `true`
    /// when the file is now fully included — or `None` when the path is
    /// not held. A partially included file includes wholly: the press names
    /// the file, not the hunk, so it answers for all of it.
    pub fn toggle_file(&mut self, path: &str) -> Option<bool> {
        let entry = self.files.iter_mut().find(|f| f.path == path)?;
        let include = !entry.hunks.iter().all(|h| h.included);
        for hunk in &mut entry.hunks {
            hunk.included = include;
        }
        Some(include)
    }

    /// Flips one hunk of `path`, addressed by its position among the
    /// file's hunks. `None` when the path or the position names nothing.
    pub fn toggle_hunk(&mut self, path: &str, hunk: usize) -> Option<bool> {
        let entry = self.files.iter_mut().find(|f| f.path == path)?;
        let picked = entry.hunks.get_mut(hunk)?;
        picked.included = !picked.included;
        Some(picked.included)
    }

    /// Drops `path` whole. `true` when something was held.
    pub fn drop_file(&mut self, path: &str) -> bool {
        let before = self.files.len();
        self.files.retain(|f| f.path != path);
        self.files.len() != before
    }

    /// Empties the clipboard. Applying never clears: the same patch is
    /// often wanted on a second target, and only an explicit clear takes
    /// it away.
    pub fn clear(&mut self) {
        self.files.clear();
    }

    /// The included hunks of every held file, each file's patch beside its
    /// path — what an apply aims, in pick order. Files with nothing
    /// included ride along as empty patches, so the caller can name them
    /// in its refusal instead of silently skipping them.
    pub fn emit(&self) -> Vec<(String, Vec<u8>)> {
        self.files
            .iter()
            .map(|f| {
                let chosen: Vec<&Hunk> = f
                    .hunks
                    .iter()
                    .filter(|h| h.included)
                    .map(|h| &h.hunk)
                    .collect();
                (f.path.clone(), patch::emit(&f.path, &chosen))
            })
            .collect()
    }

    /// The status line's word for what is held: files and included hunks,
    /// and the empty clipboard said plainly rather than as a zero.
    pub fn status(&self) -> String {
        if self.files.is_empty() {
            return "the patch clipboard is empty".into();
        }
        let files = self.files.len();
        let hunks = self.included_hunks();
        format!(
            "patch: {files} file{} · {hunks} hunk{} included",
            plural(files),
            plural(hunks)
        )
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DiffLine;
    use crate::LineKind::{Added, Context};

    fn line(kind: crate::LineKind, text: &str, old: u32, new: u32) -> DiffLine {
        DiffLine {
            kind,
            text: text.into(),
            old_no: (old != 0).then_some(old),
            new_no: (new != 0).then_some(new),
            moved: false,
        }
    }

    fn hunk(tag: &str) -> Hunk {
        Hunk {
            header: format!("@@ {tag} @@"),
            lines: vec![line(Context, "same", 1, 1), line(Added, tag, 0, 2)],
        }
    }

    fn anchor() -> Anchor {
        Anchor::Unstaged
    }

    #[test]
    fn an_empty_clipboard_says_so() {
        let clip = PatchClipboard::new();
        assert!(clip.is_empty());
        assert_eq!(clip.included_hunks(), 0);
        assert_eq!(clip.status(), "the patch clipboard is empty");
        assert!(clip.emit().is_empty());
    }

    #[test]
    fn a_pick_holds_its_hunks_included() {
        let mut clip = PatchClipboard::new();
        let added = clip.pick(
            "a.txt".into(),
            anchor(),
            Some("i1".into()),
            Some("h1".into()),
            vec![hunk("one"), hunk("two")],
        );
        assert_eq!(added, 2);
        assert_eq!(clip.included_hunks(), 2);
        assert_eq!(clip.status(), "patch: 1 file · 2 hunks included");
    }

    #[test]
    fn a_second_pick_of_the_same_path_folds_in_without_reordering() {
        let mut clip = PatchClipboard::new();
        clip.pick("a.txt".into(), anchor(), None, None, vec![hunk("one")]);
        clip.toggle_hunk("a.txt", 0);
        let added = clip.pick(
            "a.txt".into(),
            anchor(),
            None,
            None,
            vec![hunk("one"), hunk("two")],
        );
        assert_eq!(added, 1, "the held hunk is not picked twice");
        let file = &clip.files()[0];
        assert_eq!(file.hunks.len(), 2);
        assert!(!file.hunks[0].included, "the toggle survived the re-pick");
        assert!(file.hunks[1].included);
    }

    #[test]
    fn toggling_a_file_answers_for_all_of_it() {
        let mut clip = PatchClipboard::new();
        clip.pick(
            "a.txt".into(),
            anchor(),
            None,
            None,
            vec![hunk("one"), hunk("two")],
        );
        assert_eq!(clip.toggle_file("a.txt"), Some(false));
        assert_eq!(clip.included_hunks(), 0);
        assert_eq!(clip.toggle_file("a.txt"), Some(true));
        assert_eq!(clip.included_hunks(), 2);
        assert_eq!(clip.toggle_file("gone.txt"), None);
    }

    #[test]
    fn dropping_a_file_leaves_the_rest() {
        let mut clip = PatchClipboard::new();
        clip.pick("a.txt".into(), anchor(), None, None, vec![hunk("one")]);
        clip.pick("b.txt".into(), anchor(), None, None, vec![hunk("two")]);
        assert!(clip.drop_file("a.txt"));
        assert!(!clip.drop_file("a.txt"));
        assert_eq!(clip.files().len(), 1);
        clip.clear();
        assert!(clip.is_empty());
    }

    #[test]
    fn emit_carries_each_files_included_hunks_beside_its_path() {
        let mut clip = PatchClipboard::new();
        clip.pick(
            "a.txt".into(),
            anchor(),
            None,
            None,
            vec![hunk("one"), hunk("two")],
        );
        clip.toggle_hunk("a.txt", 0);
        let out = clip.emit();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, "a.txt");
        let text = String::from_utf8(out[0].1.clone()).expect("utf-8");
        assert!(text.contains("two"), "the included hunk rides: {text}");
        assert!(!text.contains("one"), "the excluded hunk does not: {text}");
    }

    #[test]
    fn only_a_worktree_read_can_move() {
        assert!(Anchor::Staged.is_worktree());
        assert!(Anchor::Unstaged.is_worktree());
        assert!(Anchor::Untracked.is_worktree());
        assert!(!Anchor::Commit { sha: "abc".into() }.is_worktree());
        assert!(!Anchor::Stash {
            commit: "abc".into()
        }
        .is_worktree());
    }
}
