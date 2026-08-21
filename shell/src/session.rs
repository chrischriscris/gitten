//! Where you were, so a restart can put you back.
//!
//! Changing code costs a rebuild — three to five seconds, which is tolerable. What
//! is not tolerable is the part either side of it: quit, retype the command,
//! scroll back to the file you were reading. This is what removes that, so
//! `./dev.sh` can rebuild and relaunch and land you on the same row.
//!
//! It is deliberately not a general "restore my workspace" feature. One number
//! and one key, in a file under `target/`, which is already ignored by git and
//! already the thing you delete when you want a clean slate.
//!
//! # The key is what makes it safe
//!
//! A saved position is only meaningful for the diff it was taken in. The key is
//! the command that produced the view — verb, repository, revspec — and a restore
//! only happens when it matches exactly. Relaunch with a different revspec and the
//! saved row is ignored rather than dropping you somewhere arbitrary in an
//! unrelated diff.
//!
//! # Why a row index and not a scroll offset
//!
//! A pixel offset means nothing if the font size changed, and the font is now
//! configurable and hot-reloaded. A row index survives that, survives a window
//! resize, and clamps harmlessly if the diff itself got shorter.

use std::path::{Path, PathBuf};

/// A place in a view, and the command it belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Session {
    /// The command that produced the view — see the note above about matching.
    pub key: String,
    /// First visible row.
    pub top: usize,
}

// The key for one invocation is `plait_app::cli::Source::key`: it is everything
// that changes what is on screen, and every client has to agree about it or a
// position saved by one is restored by another into a different diff.

/// Under `target/`, because that is already git-ignored and already what you
/// delete for a clean slate. Overridable so a test never writes to a real one.
pub fn path() -> PathBuf {
    std::env::var_os("PLAIT_SESSION")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/plait-session"))
}

/// Two lines: the key, then the row. Hand-rolled rather than TOML because this is
/// a scratch file written every few hundred milliseconds and read once, and a
/// format nobody hand-edits does not need a parser.
pub fn encode(s: &Session) -> String {
    format!("{}\n{}\n", s.key, s.top)
}

/// `None` for anything unexpected. This file is a convenience — a corrupt or
/// half-written one must be ignored, never an error, and never a panic.
pub fn decode(text: &str) -> Option<Session> {
    let mut lines = text.lines();
    let key = lines.next()?.to_string();
    let top = lines.next()?.trim().parse().ok()?;
    (!key.is_empty()).then_some(Session { key, top })
}

/// The saved position, if there is one *and* it belongs to this command.
pub fn restore(key: &str, path: &Path) -> Option<Session> {
    let text = std::fs::read_to_string(path).ok()?;
    decode(&text).filter(|s| s.key == key)
}

/// Best effort, and silent. Failing to record where you were is not worth a
/// message on a loop that runs every few hundred milliseconds.
pub fn save(s: &Session, path: &Path) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, encode(s));
}

#[cfg(test)]
mod tests {
    use super::*;

    use plait_app::cli::{Source, View};
    use std::path::PathBuf;

    fn key(view: View, repo: &str, arg: &str) -> String {
        Source::Repo { path: PathBuf::from(repo), arg: arg.into() }.key(view)
    }

    fn session() -> Session {
        Session { key: key(View::Diff, ".", "HEAD~2..HEAD"), top: 431 }
    }

    #[test]
    fn a_session_survives_a_round_trip() {
        let s = session();
        assert_eq!(decode(&encode(&s)), Some(s));
    }

    #[test]
    fn a_position_is_only_restored_for_the_command_that_took_it() {
        // The whole safety property: relaunching with a different revspec must
        // not drop you at row 431 of an unrelated diff.
        let dir = std::env::temp_dir().join("plait-session-test-key");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("state");
        let s = session();
        save(&s, &path);

        assert_eq!(restore(&s.key, &path).map(|r| r.top), Some(431));
        assert_eq!(restore(&key(View::Diff, ".", "main..feature"), &path), None);
        assert_eq!(restore(&key(View::Commits, ".", ""), &path), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nothing_saved_means_nothing_restored() {
        assert_eq!(restore("anything", Path::new("/nonexistent/plait-session")), None);
    }

    #[test]
    fn a_corrupt_file_is_ignored_rather_than_fatal() {
        // It is written every few hundred milliseconds and can be caught
        // half-flushed by a kill; every one of these must be a quiet `None`.
        for text in ["", "\n", "only-a-key\n", "key\nnot-a-number\n", "key\n-1\n", "\n\n"] {
            assert_eq!(decode(text), None, "{text:?} decoded to something");
        }
    }

    #[test]
    fn a_huge_row_number_survives() {
        // 714k-row diffs are a real fixture; the deletion one is bigger than any
        // sane default and must not overflow anything.
        let s = Session { key: "k".into(), top: 713_995 };
        assert_eq!(decode(&encode(&s)), Some(s));
    }

    #[test]
    fn the_key_distinguishes_everything_that_changes_the_view() {
        // The key is `plait_app`'s, shared with every client, and this is the
        // property the shell depends on: a position taken in one diff is never
        // restored into another.
        let a = key(View::Diff, ".", "HEAD~1");
        assert_ne!(a, key(View::Commits, ".", "HEAD~1"), "verb ignored");
        assert_ne!(a, key(View::Diff, "/other", "HEAD~1"), "repo ignored");
        assert_ne!(a, key(View::Diff, ".", "HEAD~2"), "revspec ignored");
    }
}
