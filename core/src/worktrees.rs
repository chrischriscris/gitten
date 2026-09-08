//! Git worktrees, as `git worktree list --porcelain` reports them.
//!
//! A worktree is a second checkout of the same repository: its own working
//! tree, its own HEAD, one shared object store. What a client needs is the
//! list — where each checkout lives, what it holds, and which ones git
//! would refuse to touch — and nothing more: creating and removing go
//! through the acquisition layer, and switching to one is opening its path
//! as a repository, which the project machinery already knows.
//!
//! Paths stay raw bytes throughout. A checkout's path is shaped by whoever
//! created it, and a lossy read here would aim a removal at a directory
//! whose name survived only approximately — display decodes, verbs never do.

/// One checkout of the repository, as porcelain reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    /// Where the checkout lives, exactly as porcelain spelled it — absolute
    /// and symlink-resolved, which is why a caller compares it against a
    /// canonicalized root rather than the path the repository was opened by.
    pub path: Vec<u8>,
    /// The commit its HEAD names, full object id.
    pub head: String,
    /// The branch it holds, without the `refs/heads/` prefix — `None` for
    /// a detached checkout and for a bare repository, which hold no branch.
    pub branch: Option<Vec<u8>>,
    /// Whether this is the bare repository itself rather than a checkout.
    pub bare: bool,
    /// The lock reason, when the entry is locked — `Some("")` for the bare
    /// `locked` line, which locks without naming a reason. A locked
    /// worktree is one `remove` refuses without `--force`.
    pub lock: Option<String>,
    /// The prune reason, when git considers the entry gone — a path that no
    /// longer exists, or metadata whose directory vanished. A prunable
    /// entry is one `remove` deletes the metadata of rather than a tree.
    pub prunable: Option<String>,
}

impl Worktree {
    /// Whether git would refuse a plain `remove` of this entry: locked and
    /// dirty alike need the force spelling, and the question the second
    /// press answers is which of the two it is upgrading past.
    pub fn needs_force(&self) -> bool {
        self.lock.is_some() || self.prunable.is_some()
    }
}

/// Parses `git worktree list --porcelain`: records of a `worktree` line, a
/// `HEAD` line, a `branch`/`detached`/`bare` line, then zero or more
/// `locked`/`prunable` lines. Anything else in a record is ignored, so a
/// newer git's extra lines degrade to an unread garnish rather than a
/// failed list — the whole list must never die over one line's novelty.
pub fn parse_worktrees(raw: &[u8]) -> Vec<Worktree> {
    let mut out = Vec::new();
    let mut current: Option<Worktree> = None;
    let flush = |current: &mut Option<Worktree>, out: &mut Vec<Worktree>| {
        if let Some(w) = current.take() {
            // A record without a path is not a record — porcelain always
            // opens with one, so its absence means a truncated read.
            if !w.path.is_empty() {
                out.push(w);
            }
        }
    };
    for line in raw.split(|&b| b == b'\n') {
        if let Some(path) = line.strip_prefix(b"worktree ") {
            flush(&mut current, &mut out);
            current = Some(Worktree {
                path: path.to_vec(),
                head: String::new(),
                branch: None,
                bare: false,
                lock: None,
                prunable: None,
            });
        } else if let Some(head) = line.strip_prefix(b"HEAD ") {
            if let Some(w) = current.as_mut() {
                w.head = String::from_utf8_lossy(head).into_owned();
            }
        } else if let Some(branch) = line.strip_prefix(b"branch ") {
            if let Some(w) = current.as_mut() {
                // Porcelain spells the full ref; the row wants the name.
                let name = branch
                    .strip_prefix(b"refs/heads/")
                    .unwrap_or(branch)
                    .to_vec();
                w.branch = Some(name);
            }
        } else if line == b"detached" {
            if let Some(w) = current.as_mut() {
                w.branch = None;
            }
        } else if line == b"bare" {
            if let Some(w) = current.as_mut() {
                w.bare = true;
                w.branch = None;
            }
        } else if let Some(reason) = line.strip_prefix(b"locked ") {
            if let Some(w) = current.as_mut() {
                w.lock = Some(String::from_utf8_lossy(reason).into_owned());
            }
        } else if line == b"locked" {
            if let Some(w) = current.as_mut() {
                w.lock = Some(String::new());
            }
        } else if let Some(reason) = line.strip_prefix(b"prunable ") {
            if let Some(w) = current.as_mut() {
                w.prunable = Some(String::from_utf8_lossy(reason).into_owned());
            }
        }
    }
    flush(&mut current, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const TWO: &[u8] = b"worktree /repo\nHEAD abcdef0123456789\nbranch refs/heads/main\n\nworktree /repo/feature\nHEAD 1234567890abcdef\ndetached\n";

    #[test]
    fn a_list_parses_branches_detached_and_bare() {
        let list = parse_worktrees(TWO);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].path, b"/repo");
        assert_eq!(list[0].head, "abcdef0123456789");
        assert_eq!(list[0].branch, Some(b"main".to_vec()));
        assert!(!list[0].bare);
        assert_eq!(list[1].branch, None);
        assert!(!list[1].bare);

        let bare = parse_worktrees(b"worktree /srv/repo.git\nHEAD abcdef0123456789\nbare\n");
        assert_eq!(bare.len(), 1);
        assert!(bare[0].bare);
        assert_eq!(bare[0].branch, None);
    }

    #[test]
    fn locks_and_prunes_ride_along_and_force_follows_them() {
        let list = parse_worktrees(
            b"worktree /repo/held\nHEAD abcdef0123456789\nbranch refs/heads/held\nlocked better not\n\nworktree /repo/gone\nHEAD abcdef0123456789\ndetached\nprunable gitdir file points nowhere\n",
        );
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].lock.as_deref(), Some("better not"));
        assert_eq!(list[0].prunable, None);
        assert!(list[0].needs_force());
        assert_eq!(
            list[1].prunable.as_deref(),
            Some("gitdir file points nowhere")
        );
        assert!(list[1].needs_force());
        assert!(!parse_worktrees(TWO)[0].needs_force());
    }

    #[test]
    fn a_bare_locked_line_locks_without_a_reason_and_junk_degrades() {
        let list = parse_worktrees(
            b"worktree /repo/x\nHEAD abcdef0123456789\nbranch refs/heads/x\nlocked\nnote something future\n",
        );
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].lock.as_deref(), Some(""));
        assert!(list[0].needs_force());

        assert!(parse_worktrees(b"").is_empty());
        assert!(parse_worktrees(b"HEAD abcdef\n").is_empty());
    }
}
