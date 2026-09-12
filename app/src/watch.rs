//! Watching a repository nobody here is writing to — a `git` run in the
//! user's own terminal, a save in their editor — so a client can refresh
//! before anyone reaches for the key.
//!
//! The mechanism is shared for the reason [`crate::config::watch`] is:
//! holding a filesystem watch is I/O, which is this crate's job. What a firing
//! event *means* — how long to let a burst settle, which generation to bump,
//! which views to re-read — is the client's, so this module hands back a
//! handle and a callback and the decision stays where the rows are.

use notify::{EventKind, RecursiveMode, Watcher};
use std::path::PathBuf;

/// A live repository watch.
///
/// Opaque, because `notify` is this crate's dependency to carry and no
/// client's to name. The value must be held: dropping it stops the watching,
/// silently, which is a good way to lose an afternoon.
pub struct RepoWatch {
    _watcher: Box<dyn Watcher + Send>,
}

/// Watches the repository at `root` and calls `on_change` whenever anything
/// the panes could be showing moves — a worktree file saved, the index
/// written, HEAD or a ref moved.
///
/// Three roots, resolved by [`gitten_git::watch_targets`]: the worktree
/// (absent when the repository is bare), this checkout's own gitdir, and the
/// common dir a linked worktree shares with its siblings — a commit made in
/// another worktree lands in the common refs, never in this one's gitdir.
///
/// Two silences inside the gitdirs, where churn outruns meaning: `objects/`,
/// because a fetch writes thousands of files whose only readable outcome is
/// the ref move beside them, and `*.lock`, because a lock is a write in
/// flight and the rename over it fires an event of its own. Everything else
/// — `index`, `HEAD`, `MERGE_HEAD`, `packed-refs`, a `rebase-merge/`
/// directory appearing — is somebody's answer to "did the repository move".
///
/// What is *not* filtered is ignored worktree churn — `target/`,
/// `node_modules/` — because gitignore rules are git's to answer, not a path
/// filter's to guess. The client-side debounce and rate floor carry that
/// cost; a missed `target/` event would be a feature, a missed source file a
/// bug, and the filter cannot tell them apart.
///
/// `on_change` runs on notify's own thread: stamp a flag and return, exactly
/// as [`crate::config::watch`]'s callback does.
pub fn repo(
    root: &std::path::Path,
    mut on_change: impl FnMut() + Send + 'static,
) -> notify::Result<RepoWatch> {
    let targets = gitten_git::watch_targets(root)
        .ok_or_else(|| notify::Error::generic("not a repository"))?;
    // Canonicalized, because notify reports real paths: a repository opened
    // through a symlink would otherwise fail every `starts_with` below and
    // gitdir churn would read as worktree events.
    let real = |p: PathBuf| std::fs::canonicalize(&p).unwrap_or(p);
    // The state roots, deduplicated: a single-worktree repository's commondir
    // *is* its gitdir.
    let mut state = vec![real(targets.gitdir)];
    let commondir = real(targets.commondir);
    if commondir != state[0] {
        state.push(commondir);
    }
    let worktree = targets.worktree.map(real);
    let watched_state = state.clone();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let Ok(event) = res else { return };
        // Everything moves a repository except a read — and anything that
        // does not map to a kind (a rescan, a root remount) is a reason to
        // look again, not to stay quiet.
        if matches!(event.kind, EventKind::Access(_)) {
            return;
        }
        let moved = event.paths.iter().any(|p| {
            if watched_state.iter().any(|d| p.starts_with(d)) {
                !p.components().any(|c| c.as_os_str() == "objects")
                    && p.extension() != Some(std::ffi::OsStr::new("lock"))
            } else {
                // The only other root being watched is the worktree, whose
                // every change is a status change.
                true
            }
        });
        if moved {
            on_change();
        }
    })?;
    for dir in &state {
        watcher.watch(dir, RecursiveMode::Recursive)?;
    }
    if let Some(worktree) = worktree {
        watcher.watch(&worktree, RecursiveMode::Recursive)?;
    }
    Ok(RepoWatch {
        _watcher: Box::new(watcher),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    /// A real repository — `watch_targets` asks git — initialized to nothing
    /// but a gitdir, because the events under test are filesystem ones.
    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("gitten-app-watch-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a temp dir");
        let out = std::process::Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .arg(&dir)
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git init: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        dir
    }

    /// The callback's count, polled rather than awaited: an event rides the
    /// filesystem, notify's thread and a coalescing window, none of which
    /// take a future.
    fn fired(counter: &Arc<AtomicUsize>, within: Duration) -> usize {
        let deadline = Instant::now() + within;
        while Instant::now() < deadline {
            let n = counter.load(Ordering::Relaxed);
            if n > 0 {
                return n;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        0
    }

    #[test]
    fn a_save_and_an_index_write_speak_but_churn_does_not() {
        let root = scratch("filter");
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = calls.clone();
        let _watch = repo(&root, move || {
            counted.fetch_add(1, Ordering::Relaxed);
        })
        .expect("a repository watches");
        // Settle the watcher's own birth before measuring: setup can report
        // the files `git init` wrote moments ago.
        std::thread::sleep(Duration::from_millis(400));

        calls.store(0, Ordering::Relaxed);
        std::fs::write(root.join("new.txt"), b"saved in an editor\n").expect("a save");
        assert!(
            fired(&calls, Duration::from_secs(5)) > 0,
            "a worktree save fired nothing"
        );

        calls.store(0, Ordering::Relaxed);
        std::fs::write(root.join(".git").join("index"), b"staged\n").expect("an index write");
        assert!(
            fired(&calls, Duration::from_secs(5)) > 0,
            "an index write fired nothing"
        );

        // The two silences: a loose object is churn whose answer is a ref,
        // and a lock is a write in flight whose answer is the rename.
        calls.store(0, Ordering::Relaxed);
        let objects = root.join(".git").join("objects").join("ab");
        std::fs::create_dir_all(&objects).expect("an objects dir");
        std::fs::write(objects.join("c".repeat(38)), b"x").expect("a loose object");
        std::fs::write(root.join(".git").join("index.lock"), b"x").expect("a lock");
        std::thread::sleep(Duration::from_millis(600));
        assert_eq!(
            calls.load(Ordering::Relaxed),
            0,
            "objects churn or a lock fired the callback"
        );

        let _ = std::fs::remove_dir_all(root);
    }
}
