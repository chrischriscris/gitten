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
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Session {
    /// The command that produced the view — see the note above about matching.
    pub key: String,
    /// First visible row.
    pub top: usize,
    /// The rail widths the pointer last dragged to, remembered with the
    /// position they were taken beside — `None` is the spec's rung for the
    /// window at hand. Sidebar, inspector, timeline; see `views::workspace`.
    pub sidebar_w: Option<f32>,
    pub inspector_w: Option<f32>,
    pub timeline_w: Option<f32>,
    /// The side-by-side rule's share of the row — a fraction rather than a
    /// width, because the share is what the pointer chose and a width taken
    /// on one window is the wrong answer on another.
    pub split: Option<f32>,
}

// The key for one invocation is `gitten_app::cli::Source::key`: it is everything
// that changes what is on screen, and every client has to agree about it or a
// position saved by one is restored by another into a different diff.

/// Under `target/`, because that is already git-ignored and already what you
/// delete for a clean slate. Overridable so a test never writes to a real one.
pub fn path() -> PathBuf {
    std::env::var_os("GITTEN_SESSION")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/gitten-session"))
}

/// Three lines: the key, then the row, then the rail widths as one
/// comma-joined field — an empty slot is the spec's rung. Hand-rolled rather
/// than TOML because this is a scratch file written every few hundred
/// milliseconds and read once, and a format nobody hand-edits does not need
/// a parser.
pub fn encode(s: &Session) -> String {
    let w = |w: Option<f32>| w.map(|w| w.to_string()).unwrap_or_default();
    format!(
        "{}\n{}\n{},{},{},{}\n",
        s.key,
        s.top,
        w(s.sidebar_w),
        w(s.inspector_w),
        w(s.timeline_w),
        w(s.split)
    )
}

/// `None` for anything unexpected. This file is a convenience — a corrupt or
/// half-written one must be ignored, never an error, and never a panic. The
/// widths line is younger than the other two, so it is also allowed to be
/// missing or mangled — a bad width decays to the spec, not to a lost
/// position.
pub fn decode(text: &str) -> Option<Session> {
    let mut lines = text.lines();
    let key = lines.next()?.to_string();
    let top = lines.next()?.trim().parse().ok()?;
    let width = |s: Option<&str>| {
        s.and_then(|s| s.parse::<f32>().ok())
            .filter(|w| w.is_finite() && *w > 0.0)
    };
    let mut widths = lines.next().unwrap_or("").split(',');
    let (sidebar_w, inspector_w, timeline_w) = (
        width(widths.next()),
        width(widths.next()),
        width(widths.next()),
    );
    // The rule's share is a fraction, not a width: `0 < f < 1` is the whole
    // of what makes one valid.
    let split = width(widths.next()).filter(|f| *f < 1.0);
    (!key.is_empty()).then_some(Session {
        key,
        top,
        sidebar_w,
        inspector_w,
        timeline_w,
        split,
    })
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

    use gitten_app::cli::{Source, View};
    use std::path::PathBuf;

    fn key(view: View, repo: &str, arg: &str) -> String {
        Source::Repo {
            path: PathBuf::from(repo),
            arg: arg.into(),
        }
        .key(view)
    }

    fn session() -> Session {
        Session {
            key: key(View::Diff, ".", "HEAD~2..HEAD"),
            top: 431,
            sidebar_w: Some(302.5),
            inspector_w: None,
            timeline_w: Some(240.0),
            split: Some(0.62),
        }
    }

    #[test]
    fn a_session_survives_a_round_trip() {
        let s = session();
        assert_eq!(decode(&encode(&s)), Some(s));
    }

    #[test]
    fn a_session_without_widths_is_older_not_corrupt() {
        // The widths line is younger than the file: a two-line session is a
        // position from before rails resized, and it decodes with the spec's
        // rungs rather than failing.
        assert_eq!(
            decode("key\n431\n"),
            Some(Session {
                key: "key".into(),
                top: 431,
                ..Session::default()
            })
        );
    }

    #[test]
    fn a_mangled_width_decays_to_the_spec_not_to_a_lost_row() {
        // A kill mid-flush can leave half the line; each field decays on its
        // own, and the row itself still comes back.
        let s = decode("key\n431\n302.5,oops,\n").unwrap();
        assert_eq!(s.top, 431);
        assert_eq!(s.sidebar_w, Some(302.5));
        assert_eq!(s.inspector_w, None);
        assert_eq!(s.timeline_w, None);
        for bad in ["key\n1\nNaN,,-4\n", "key\n1\n0,inf,\n"] {
            let s = decode(bad).unwrap();
            assert_eq!(
                (s.sidebar_w, s.inspector_w, s.timeline_w),
                (None, None, None),
                "{bad:?} kept a width"
            );
        }
        // The rule's share is a fraction: outside `0 < f < 1` it is not a
        // share at all, and a missing slot is a three-width file — still a
        // session, just an older one.
        let s = decode("key\n431\n,320,,1.5\n").unwrap();
        assert_eq!((s.inspector_w, s.split), (Some(320.0), None));
        assert_eq!(decode("key\n431\n,320,,0.62\n").unwrap().split, Some(0.62));
    }

    #[test]
    fn a_position_is_only_restored_for_the_command_that_took_it() {
        // The whole safety property: relaunching with a different revspec must
        // not drop you at row 431 of an unrelated diff.
        let dir = std::env::temp_dir().join("gitten-session-test-key");
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
        assert_eq!(
            restore("anything", Path::new("/nonexistent/gitten-session")),
            None
        );
    }

    #[test]
    fn a_corrupt_file_is_ignored_rather_than_fatal() {
        // It is written every few hundred milliseconds and can be caught
        // half-flushed by a kill; every one of these must be a quiet `None`.
        for text in [
            "",
            "\n",
            "only-a-key\n",
            "key\nnot-a-number\n",
            "key\n-1\n",
            "\n\n",
        ] {
            assert_eq!(decode(text), None, "{text:?} decoded to something");
        }
    }

    #[test]
    fn a_huge_row_number_survives() {
        // 714k-row diffs are a real fixture; the deletion one is bigger than any
        // sane default and must not overflow anything.
        let s = Session {
            key: "k".into(),
            top: 713_995,
            ..Session::default()
        };
        assert_eq!(decode(&encode(&s)), Some(s));
    }

    #[test]
    fn the_key_distinguishes_everything_that_changes_the_view() {
        // The key is `gitten_app`'s, shared with every client, and this is the
        // property the shell depends on: a position taken in one diff is never
        // restored into another.
        let a = key(View::Diff, ".", "HEAD~1");
        assert_ne!(a, key(View::Commits, ".", "HEAD~1"), "verb ignored");
        assert_ne!(a, key(View::Diff, "/other", "HEAD~1"), "repo ignored");
        assert_ne!(a, key(View::Diff, ".", "HEAD~2"), "revspec ignored");
    }
}
