//! The cherry-pick clipboard: commits kept for a later paste.
//!
//! Pure state — full shas in, the same shas out, no repository, no I/O —
//! which is why it lives here rather than in a client: every frontend pastes
//! the same order, and the order is the whole contract. A paste replays the
//! clipboard front to back in one `git cherry-pick`, so the order below is
//! the order history grows in, and a conflict stops the sequence exactly
//! where git stopped it.

/// Commits kept for `commits.paste`, as full shas — stable IDs, never row
/// indices, so a refresh, a filter or a reorder between the copy and the
/// paste names the same commits rather than whatever slid into their rows.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CherryClipboard {
    shas: Vec<Vec<u8>>,
}

impl CherryClipboard {
    pub fn new() -> Self {
        Self::default()
    }

    /// What a paste replays, front to back: the copy order, oldest-first
    /// within any one marked range (a range reads as a unit to replay
    /// bottom-up, ancestors before descendants) and press order across
    /// separate copies. One slice so one `git cherry-pick` invocation takes
    /// it whole — a conflict then stops mid-sequence with the lifecycle's
    /// question standing, instead of scattering half a paste across jobs.
    pub fn ordered(&self) -> &[Vec<u8>] {
        &self.shas
    }

    pub fn len(&self) -> usize {
        self.shas.len()
    }

    pub fn is_empty(&self) -> bool {
        self.shas.is_empty()
    }

    pub fn contains(&self, sha: &[u8]) -> bool {
        self.shas.iter().any(|s| s == sha)
    }

    /// Copies `shas`, given newest-first as a commits list reads, onto the
    /// back of the clipboard oldest-first. Returns how many were new: a
    /// sha already kept stays where it was put — re-copying never
    /// reorders, because a clipboard that moves under a second press is a
    /// paste order nobody can predict.
    pub fn copy_newest_first(&mut self, shas: &[Vec<u8>]) -> usize {
        let mut added = 0;
        for sha in shas.iter().rev() {
            if !self.contains(sha) {
                self.shas.push(sha.clone());
                added += 1;
            }
        }
        added
    }

    /// Empties the clipboard. Pasting never clears: the same set can be
    /// replayed onto another branch, and only an explicit clear takes it
    /// away — which is also why a clear of an empty clipboard is worth
    /// saying rather than silently accepting.
    pub fn clear(&mut self) {
        self.shas.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sha(s: &str) -> Vec<u8> {
        s.as_bytes().to_vec()
    }

    #[test]
    fn a_marked_range_lands_oldest_first() {
        // The list reads newest-first; the paste must replay ancestors
        // before descendants, so one copy reverses.
        let mut clip = CherryClipboard::new();
        let added = clip.copy_newest_first(&[sha("new"), sha("mid"), sha("old")]);
        assert_eq!(added, 3);
        assert_eq!(clip.ordered(), &[sha("old"), sha("mid"), sha("new")]);
    }

    #[test]
    fn separate_copies_append_in_press_order() {
        let mut clip = CherryClipboard::new();
        clip.copy_newest_first(&[sha("new")]);
        clip.copy_newest_first(&[sha("old")]);
        assert_eq!(clip.ordered(), &[sha("new"), sha("old")]);
    }

    #[test]
    fn re_copying_never_reorders() {
        let mut clip = CherryClipboard::new();
        clip.copy_newest_first(&[sha("a"), sha("b")]);
        let added = clip.copy_newest_first(&[sha("a")]);
        assert_eq!(added, 0, "nothing new arrived");
        assert_eq!(clip.ordered(), &[sha("b"), sha("a")]);
    }

    #[test]
    fn paste_keeps_the_set_until_an_explicit_clear() {
        let mut clip = CherryClipboard::new();
        clip.copy_newest_first(&[sha("a")]);
        assert!(!clip.is_empty());
        clip.clear();
        assert!(clip.is_empty());
        assert!(clip.ordered().is_empty());
    }

    #[test]
    fn full_shas_survive_a_reorder() {
        // The point of stable IDs: the clipboard holds whole shas, so two
        // commits whose rows swapped still paste as themselves.
        let mut clip = CherryClipboard::new();
        clip.copy_newest_first(&[sha("abc123"), sha("def456")]);
        assert!(clip.contains(b"abc123"));
        assert!(!clip.contains(b"abc1234"), "no prefix matching");
    }
}
